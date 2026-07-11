mod mcp;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dowse_core::{
    DEFAULT_WORKERS, IndexUpdater, OcrPipeline, Searcher, WatchProgress, drain_ocr_queue,
    rebuild_index_with_progress, registered_roots, watch_roots_auto,
};
use rmcp::ServiceExt;
use rmcp::transport::stdio;

#[derive(Parser)]
#[command(name = "dowse", about = "探水杖：Windows 本地全盘搜索", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// 全量重建索引
    Index {
        /// 要索引的目录
        dir: PathBuf,
    },
    /// 搜索已建好的索引
    Search {
        /// 查询词，支持多个词（AND）和 "短语"
        query: Vec<String>,
        /// 最多返回几条
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
    },
    /// 前台运行文件监听，实时打印收到的事件和防抖后提交的批次（调试用），Ctrl+C 退出
    Watch {
        /// 要监听的目录；不给就用上次建索引时注册的根目录
        dir: Option<PathBuf>,
    },
    /// 启动只读 MCP server（stdio 传输），把本地索引暴露给 AI agent
    ///
    /// 例：`claude mcp add dowse -- dowse mcp`
    Mcp,
}

/// 索引统一放在 %LOCALAPPDATA%\dowse\index，跟被索引的目录无关。
///
/// `DOWSE_INDEX_DIR` 环境变量可以覆盖这个位置——只给集成测试用，好让子进程指向
/// 一个临时索引而不是碰用户机器上真的那份；正常使用不应该设这个变量。
fn index_dir() -> Result<PathBuf> {
    if let Ok(dir) = std::env::var("DOWSE_INDEX_DIR") {
        return Ok(PathBuf::from(dir));
    }
    let dirs = directories::ProjectDirs::from("", "", "dowse").context("拿不到用户数据目录")?;
    Ok(dirs.data_local_dir().join("index"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Index { dir } => {
            let dir = dir.canonicalize().context("目标目录不存在")?;
            println!("索引目标: {}", dir.display());
            // 核心层已经把回调频率降到每 PROGRESS_INTERVAL(50) 个文件一次，
            // 这里再降频到每千个文件（是 50 的整数倍）打一行，避免大目录把
            // 终端刷屏。
            let stats = rebuild_index_with_progress(&index_dir()?, &dir, |progress| {
                if progress.processed.is_multiple_of(1000) {
                    println!("  已处理 {} 个文件…", progress.processed);
                }
            })?;
            println!(
                "完成: 收录 {} 个文件, 跳过 {} 个, 用时 {:.1}s",
                stats.indexed, stats.skipped, stats.seconds
            );

            // 文本先行可搜之后，紧接着把 OCR 队列同步跑完——`dowse index` 是一次性
            // 命令，用户期望它退出时索引就是完整状态，而不是留一堆图片"回头再说"
            // （常驻的托盘程序不需要这一步，图片交给后台 worker 池慢慢消化）。
            let ocr_stats = drain_ocr_queue(&index_dir()?, DEFAULT_WORKERS)?;
            if !ocr_stats.available {
                println!(
                    "未检测到可用的 OCR 语言包，跳过图片文字识别（截图/图片文字不会被索引）。"
                );
            } else if ocr_stats.processed > 0 {
                println!("OCR 完成: 识别 {} 张图片", ocr_stats.processed);
            }
        }
        Command::Search { query, limit } => {
            let query_str = query.join(" ");
            let searcher = Searcher::open(&index_dir()?)?;
            let hits = searcher.search(&query_str, limit)?;

            if hits.is_empty() {
                println!("没搜到。索引里共 {} 篇文档。", searcher.num_docs());
                return Ok(());
            }
            for hit in &hits {
                println!("\x1b[36m{}\x1b[0m  (score {:.2})", hit.path, hit.score);
                println!("  {}", render_snippet(&hit.snippet, &hit.highlighted));
            }
        }
        Command::Watch { dir } => watch(dir)?,
        Command::Mcp => run_mcp()?,
    }
    Ok(())
}

/// `dowse mcp`：只读 MCP server 是这个二进制里唯一需要异步运行时的子命令，
/// 所以只在这一条分支上起 tokio runtime，其它子命令继续走同步路径——
/// 没必要给整个 main 套 #[tokio::main]，那样会让所有子命令都背上 tokio 的初始化成本。
/// 用多线程 runtime：工具处理函数里调的是 dowse-core 的同步阻塞 I/O，
/// 单线程 runtime 下阻塞调用会卡住整个 server（stdio 收发都跑不动）。
fn run_mcp() -> Result<()> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("起 tokio runtime 失败")?
        .block_on(run_mcp_async())
}

async fn run_mcp_async() -> Result<()> {
    let server = mcp::DowseMcpServer::new(index_dir()?);
    let service = server.serve(stdio()).await.context("MCP server 启动失败")?;
    service.waiting().await.context("MCP server 异常退出")?;
    Ok(())
}

/// `dowse watch`：挂上文件监听，把事件和提交批次实时打到终端，Ctrl+C 退出。
/// 纯调试用途——托盘常驻程序才是监听的正式宿主。
fn watch(dir: Option<PathBuf>) -> Result<()> {
    let index = index_dir()?;

    // 监听哪些根：显式给了目录就用它，否则用索引里注册的根。
    let roots: Vec<PathBuf> = match dir {
        Some(d) => vec![d.canonicalize().context("目标目录不存在")?],
        None => {
            let roots = registered_roots(&index)
                .context("读不到已注册的索引根，先跑 `dowse index <目录>` 建一次索引")?;
            if roots.is_empty() {
                anyhow::bail!("索引里没有已注册的根目录，先跑 `dowse index <目录>`");
            }
            roots
        }
    };

    println!("监听目录：");
    for r in &roots {
        println!("  {}", r.display());
    }
    println!("按 Ctrl+C 退出。\n");

    let updater = Arc::new(Mutex::new(
        IndexUpdater::open(&index).context("打不开索引写入端，先建一次索引")?,
    ));
    let stop = Arc::new(AtomicBool::new(false));

    // Ctrl+C：置停止位，run_watch 下个窗口 tick（≤500ms）看到后干净退出。
    {
        let stop = stop.clone();
        ctrlc::set_handler(move || {
            eprintln!("\n收到 Ctrl+C，正在停止监听…");
            stop.store(true, Ordering::Relaxed);
        })
        .context("安装 Ctrl+C 处理器失败")?;
    }

    // OCR 是独立的后台低优先级管线，跟文本监听并行跑，互不阻塞；没有可用语言包
    // 时 start() 返回 None，打印一行提示，watch 主流程照常继续（不因此报错退出）。
    let ocr_pipeline = OcrPipeline::start(updater.clone(), index.clone(), DEFAULT_WORKERS);

    watch_roots_auto(&index, &roots, updater, stop, |progress| match progress {
        WatchProgress::Received(ev) => println!("  事件  {ev:?}"),
        WatchProgress::Committed {
            batch_size,
            outcome,
        } => println!(
            "提交一批：{batch_size} 项 → 收录 {} / 删除 {} / 跳过 {}",
            outcome.upserted, outcome.removed, outcome.skipped
        ),
        WatchProgress::CommitFailed(err) => {
            eprintln!("提交失败（已退回队列，下个窗口重试）：{err}")
        }
    })?;

    if let Some(pipeline) = ocr_pipeline {
        pipeline.stop();
    }

    println!("监听已停止。");
    Ok(())
}

/// 把命中区间染成黄色。区间是字节偏移，tantivy 保证落在 UTF-8 边界上。
/// 依赖 `SearchHit.highlighted` 的契约：区间已按起点排序且互不重叠
/// （由 dowse-core::searcher::normalize_ranges 保证），这里游标只前进不回退，
/// 不重复处理有序不重叠的假设。
fn render_snippet(fragment: &str, ranges: &[std::ops::Range<usize>]) -> String {
    let mut out = String::with_capacity(fragment.len() + ranges.len() * 10);
    let mut cursor = 0;
    for r in ranges {
        out.push_str(&fragment[cursor..r.start]);
        out.push_str("\x1b[33;1m");
        out.push_str(&fragment[r.start..r.end]);
        out.push_str("\x1b[0m");
        cursor = r.end;
    }
    out.push_str(&fragment[cursor..]);
    out.replace('\n', " ")
}
