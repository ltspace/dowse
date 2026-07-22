mod mcp;

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dowse::{
    DEFAULT_WORKERS, IndexRules, IndexUpdater, OcrPipeline, Searcher, SortMode, WatchProgress,
    display_path, drain_ocr_queue, index_root_incremental_with_progress, index_status, load_rules,
    rebuild_index_with_progress, registered_roots, save_rules, watch_roots_auto,
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
    /// 增量补扫：往已有索引里再加一个根目录，只索引这个新目录，不动其它已注册的根
    /// （对比 `index` 会删掉整个索引从头重建）
    Add {
        /// 要新增并索引的目录
        dir: PathBuf,
    },
    /// 搜索已建好的索引
    Search {
        /// 查询词，支持多个词（AND）和 "短语"。还支持内联操作符：path:关键词（按
        /// 路径）、mtime:>2026-01-01 / mtime:<=2026-07（按修改日期，比较符 > >= < <=，
        /// 日期 YYYY-MM-DD 或 YYYY-MM）、size:>10mb / size:<500kb（按体积，单位
        /// kb/mb/gb）、大写 OR 分组（组内空格为 AND）、-词 或 NOT 词 排除；带空格
        /// 的操作数加引号，如 path:"我的 文档"
        query: Vec<String>,
        /// 最多返回几条
        #[arg(short = 'n', long, default_value_t = 10)]
        limit: usize,
        /// 只保留这些扩展名的结果（不含点，逗号分隔，如 md,pdf,txt）
        #[arg(long, value_delimiter = ',')]
        ext: Vec<String>,
        /// 排序方式：relevance（默认）/ mtime_desc / mtime_asc / size_desc
        #[arg(long)]
        sort: Option<String>,
    },
    /// 查看索引概况：位置、文档总数、落盘体积、已注册根目录、最近更新时间、当前规则
    Status,
    /// 查看或修改索引规则（排除目录 / 追加文本扩展名 / 单文件体积上限）
    Rules {
        #[command(subcommand)]
        action: RulesAction,
    },
    /// 前台运行文件监听，实时打印收到的事件和防抖后提交的批次（调试用），Ctrl+C 退出
    Watch {
        /// 要监听的目录；不给就用上次建索引时注册的根目录
        dir: Option<PathBuf>,
    },
    /// 启动只读 MCP server（stdio 传输），把本地索引暴露给 AI agent
    ///
    /// 例：`claude mcp add --scope user dowse -- dowse mcp`
    Mcp,
}

/// `dowse rules` 的子动作：查看或修改索引规则。
#[derive(Subcommand)]
enum RulesAction {
    /// 打印当前生效的索引规则
    Show,
    /// 修改索引规则（改完需要重建索引才完全生效）。只给出的项被改动，未给出的
    /// 项保持原值；列表类选项按"整体替换"语义处理。
    Set {
        /// 排除目录名列表（逗号分隔），整体替换现有列表
        #[arg(long, value_delimiter = ',')]
        exclude: Option<Vec<String>>,
        /// 追加的文本扩展名列表（逗号分隔，不含点），整体替换现有列表
        #[arg(long = "add-ext", value_delimiter = ',')]
        add_ext: Option<Vec<String>>,
        /// 单文件体积上限（MB）
        #[arg(long = "max-file-mb")]
        max_file_mb: Option<u64>,
    },
}

/// 索引统一放在 %LOCALAPPDATA%\dowse\index，跟被索引的目录无关。
///
/// `DOWSE_INDEX_DIR` 环境变量可以覆盖这个位置：集成测试（tests/mcp_integration.rs）
/// 靠它把子进程指向一个临时索引，不碰用户机器上真的那份。这是只给测试/CI 基础设施
/// 用的内部逃生舱，不是产品对外配置项——正常使用不该设它，也因此刻意不写进 `--help`，
/// 免得被当成稳定接口来承诺。
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
                if progress.processed % 1000 == 0 {
                    println!("  已处理 {} 个文件…", progress.processed);
                }
            })?;
            if stats.skipped_oversize > 0 {
                println!(
                    "完成: 收录 {} 个文件, 跳过 {} 个（其中 {} 个因体积超限）, 用时 {:.1}s",
                    stats.indexed, stats.skipped, stats.skipped_oversize, stats.seconds
                );
            } else {
                println!(
                    "完成: 收录 {} 个文件, 跳过 {} 个, 用时 {:.1}s",
                    stats.indexed, stats.skipped, stats.seconds
                );
            }

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
        Command::Search {
            query,
            limit,
            ext,
            sort,
        } => {
            let query_str = query.join(" ");
            if query_str.trim().is_empty() {
                anyhow::bail!("查询词不能为空，用法：dowse search <词> [更多词…]");
            }
            let searcher = Searcher::open(&index_dir()?)?;
            let sort_mode = SortMode::parse(sort.as_deref());
            // SortMode::parse 对未知值静默回退到相关性排序（其契约被浮窗等前端共用，
            // 不能改）。CLI 这层补一句提示：用户显式给了 --sort、却落回了默认档，且给
            // 的又不是 "relevance" 本身，说明这个值没被识别，别让用户以为标志生效了。
            if let Some(raw) = sort.as_deref()
                && sort_mode == SortMode::Relevance
                && raw != "relevance"
            {
                eprintln!("未知的排序方式 \"{raw}\"，已回退到相关性排序。");
            }
            // clap 的 value_delimiter 已经按逗号拆好；清洗规则（剔空串/trim/小写）
            // 抽到 clean_ext_tokens 里跟 MCP 那条入口共用，见其文档说明。空列表表示不过滤。
            let ext_tokens = clean_ext_tokens(ext.iter().map(String::as_str));
            let ext_refs: Vec<&str> = ext_tokens.iter().map(String::as_str).collect();
            let ext_group = (!ext_refs.is_empty()).then_some(ext_refs.as_slice());
            let hits = searcher.search_advanced(&query_str, limit, ext_group, sort_mode)?;

            if hits.is_empty() {
                println!("没搜到。索引里共 {} 篇文档。", searcher.num_docs());
                return Ok(());
            }
            for hit in &hits {
                // 只有相关性排序下 BM25 分数才有意义；按 mtime/size 排时分数固定为 0，
                // 展示它会误导，所以这两档不打分数。
                if sort_mode == SortMode::Relevance {
                    println!("\x1b[36m{}\x1b[0m  (score {:.2})", hit.path, hit.score);
                } else {
                    println!("\x1b[36m{}\x1b[0m", hit.path);
                }
                println!("  {}", render_snippet(&hit.snippet, &hit.highlighted));
            }
        }
        Command::Add { dir } => add_root_cmd(dir)?,
        Command::Status => status()?,
        Command::Rules { action } => rules_cmd(action)?,
        Command::Watch { dir } => watch(dir)?,
        Command::Mcp => run_mcp()?,
    }
    Ok(())
}

/// `dowse add`：增量补扫，往已有索引里再加一个根目录。只索引这个新目录、把它
/// 追加进已注册根列表，索引里其它根的文档原样保留（对比 `dowse index` 会删掉整个
/// 索引从头重建）。报告风格跟 `dowse index` 保持一致。
fn add_root_cmd(dir: PathBuf) -> Result<()> {
    let index = index_dir()?;
    let dir = dir.canonicalize().context("目标目录不存在")?;
    println!("增量补扫新根: {}", display_path(&dir.to_string_lossy()));

    // 补扫复用一个现开的写入端；下面 drain_ocr_queue 会自己开一个写入端，一个索引
    // 同一时刻只能有一个 IndexWriter，所以补扫用完必须先 drop 掉再去跑 OCR。
    let mut updater = IndexUpdater::open(&index)
        .context("打不开索引写入端，先跑 `dowse index <目录>` 建一次索引")?;
    // 核心层回调已降频到每 PROGRESS_INTERVAL(50) 个文件一次，这里再降到每千个
    // （50 的整数倍）打一行，跟 `dowse index` 一样避免大目录刷屏。
    let stats = index_root_incremental_with_progress(&index, &dir, &mut updater, |progress| {
        if progress.processed % 1000 == 0 {
            println!("  已处理 {} 个文件…", progress.processed);
        }
    })?;
    drop(updater);

    if stats.skipped_oversize > 0 {
        println!(
            "完成: 新增收录 {} 个文件, 跳过 {} 个（其中 {} 个因体积超限）, 用时 {:.1}s",
            stats.indexed, stats.skipped, stats.skipped_oversize, stats.seconds
        );
    } else {
        println!(
            "完成: 新增收录 {} 个文件, 跳过 {} 个, 用时 {:.1}s",
            stats.indexed, stats.skipped, stats.seconds
        );
    }

    // 跟 `dowse index` 一样，把新根名下图片的 OCR 同步跑完，让命令退出时索引就是
    // 完整状态（常驻托盘程序才把图片交给后台 worker 池慢慢消化）。
    let ocr_stats = drain_ocr_queue(&index, DEFAULT_WORKERS)?;
    if !ocr_stats.available {
        println!("未检测到可用的 OCR 语言包，跳过图片文字识别（截图/图片文字不会被索引）。");
    } else if ocr_stats.processed > 0 {
        println!("OCR 完成: 识别 {} 张图片", ocr_stats.processed);
    }
    Ok(())
}

/// `dowse mcp`：只读 MCP server 是这个二进制里唯一需要异步运行时的子命令，
/// 所以只在这一条分支上起 tokio runtime，其它子命令继续走同步路径——
/// 没必要给整个 main 套 #[tokio::main]，那样会让所有子命令都背上 tokio 的初始化成本。
/// 用多线程 runtime：工具处理函数里调的是 dowse 的同步阻塞 I/O，
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

/// `dowse status`：读一份只读索引概况打给用户——索引在哪、收了多少篇、
/// 占多大盘、注册了哪些根、最近一次更新在什么时候。
fn status() -> Result<()> {
    let dir = index_dir()?;
    let status =
        index_status(&dir).context("读不到索引状态，先跑 `dowse index <目录>` 建一次索引")?;

    println!("索引位置: {}", dir.display());
    println!("文档总数: {}", status.num_docs);
    println!("落盘体积: {}", human_bytes(status.disk_size_bytes));
    if let Some(t) = status.last_updated {
        // elapsed() 只在系统时钟回拨这种罕见情况会 Err，退化成"刚刚"即可。
        let ago = t
            .elapsed()
            .map(human_ago)
            .unwrap_or_else(|_| "刚刚".to_owned());
        println!("最近更新: {ago}");
    }
    if status.roots.is_empty() {
        println!("已注册根目录: (无)");
    } else {
        println!("已注册根目录:");
        for r in &status.roots {
            println!("  {}", display_path(&r.to_string_lossy()));
        }
    }
    println!("索引规则:");
    print_rules(&status.rules);
    Ok(())
}

/// `dowse rules show`/`set`：查看或修改索引目录旁的 rules.json。修改后提示需要
/// 重建索引才完全生效——规则只影响此后的抽取/遍历判定，已经在索引里的文件不会
/// 被自动重新评估。
fn rules_cmd(action: RulesAction) -> Result<()> {
    let dir = index_dir()?;
    match action {
        RulesAction::Show => {
            let rules = load_rules(&dir);
            println!("当前索引规则:");
            print_rules(&rules);
        }
        RulesAction::Set {
            exclude,
            add_ext,
            max_file_mb,
        } => {
            // 在现有规则的基础上改：只覆盖显式给出的项，未给出的保持原值。
            let mut rules = load_rules(&dir);
            if let Some(exclude) = exclude {
                rules.exclude_dirs = exclude;
            }
            if let Some(add_ext) = add_ext {
                rules.extra_text_exts = add_ext;
            }
            if let Some(max_file_mb) = max_file_mb {
                rules.max_file_mb = max_file_mb;
            }
            // save_rules 内部会 normalize；这里也 normalize 一份用于即时回显，
            // 让打印出来的就是最终落盘的样子（去空白/去点/小写/去重）。
            rules.normalize();
            save_rules(&dir, &rules)?;
            println!("规则已更新:");
            print_rules(&rules);
            println!(
                "\n提示: 规则修改后需要重建索引（dowse index <目录>）才能完全生效——\
                 已在索引里的文件不会被自动重新评估。"
            );
        }
    }
    Ok(())
}

/// 把一份索引规则按统一缩进格式打给用户，`status` 和 `rules` 两处共用。
fn print_rules(rules: &IndexRules) {
    let fmt_list = |items: &[String]| {
        if items.is_empty() {
            "(无)".to_string()
        } else {
            items.join(", ")
        }
    };
    println!("  排除目录: {}", fmt_list(&rules.exclude_dirs));
    println!("  追加文本扩展名: {}", fmt_list(&rules.extra_text_exts));
    println!("  单文件体积上限: {} MB", rules.max_file_mb);
}

/// 把字节数格式化成人类可读的 B/KB/MB/GB（1024 进制）。
fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < UNITS.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[i])
    }
}

/// 把时长格式化成"X 秒/分钟/小时/天前"。
fn human_ago(d: std::time::Duration) -> String {
    let s = d.as_secs();
    if s < 60 {
        format!("{s} 秒前")
    } else if s < 3600 {
        format!("{} 分钟前", s / 60)
    } else if s < 86400 {
        format!("{} 小时前", s / 3600)
    } else {
        format!("{} 天前", s / 86400)
    }
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
    let ocr_pipeline =
        OcrPipeline::start(updater.clone(), index.clone(), DEFAULT_WORKERS, |pending| {
            println!("OCR 队列剩余 {pending} 张待识别");
        });

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

/// 把扩展名过滤参数清洗成一串可用的扩展名：剔除空串——`md,,txt` 会拆出 `""`，
/// 留着它会去匹配"没有扩展名"的文件，把结果悄悄放宽；再做 trim + 小写归一——
/// 索引里的 ext 字段是小写无空格的（见 extract.rs 的 `to_ascii_lowercase`），
/// `"md, PDF"` 这种带空格/大写的输入不归一就会静默匹配不到任何文件。CLI（clap
/// 按逗号拆好的 `Vec<String>`）和 MCP（单个逗号分隔字符串自己 `split(',')`）两条
/// 入口共用这套清洗规则，保证同样的输入在两处得到同样的解释，不用各写一份解析器。
/// 空列表表示不按扩展名过滤。
pub(crate) fn clean_ext_tokens<'a, I>(tokens: I) -> Vec<String>
where
    I: IntoIterator<Item = &'a str>,
{
    tokens
        .into_iter()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_ascii_lowercase)
        .collect()
}

/// 把命中区间用 `open`/`close` 包起来切片重组（终端染色和 MCP 的 «» 标记
/// 共用）。区间是字节偏移，依赖 `SearchHit.highlighted` 的契约：已按起点排序、
/// 互不重叠、落在 UTF-8 边界上（由 dowse::normalize_ranges 保证），
/// 所以这里游标只前进不回退。
pub(crate) fn wrap_highlight_ranges(
    fragment: &str,
    ranges: &[std::ops::Range<usize>],
    open: &str,
    close: &str,
) -> String {
    let mut out = String::with_capacity(fragment.len() + ranges.len() * (open.len() + close.len()));
    let mut cursor = 0;
    for r in ranges {
        out.push_str(&fragment[cursor..r.start]);
        out.push_str(open);
        out.push_str(&fragment[r.start..r.end]);
        out.push_str(close);
        cursor = r.end;
    }
    out.push_str(&fragment[cursor..]);
    out
}

/// 终端里把命中区间染成黄色，并把换行压成空格（结果行是单行展示）。
fn render_snippet(fragment: &str, ranges: &[std::ops::Range<usize>]) -> String {
    wrap_highlight_ranges(fragment, ranges, "\x1b[33;1m", "\x1b[0m").replace('\n', " ")
}
