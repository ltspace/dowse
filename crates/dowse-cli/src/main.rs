use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dowse_core::{rebuild_index, Searcher};

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
}

/// 索引统一放在 %LOCALAPPDATA%\dowse\index，跟被索引的目录无关
fn index_dir() -> Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "dowse")
        .context("拿不到用户数据目录")?;
    Ok(dirs.data_local_dir().join("index"))
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Index { dir } => {
            let dir = dir.canonicalize().context("目标目录不存在")?;
            println!("索引目标: {}", dir.display());
            let stats = rebuild_index(&index_dir()?, &dir)?;
            println!(
                "完成: 收录 {} 个文件, 跳过 {} 个, 用时 {:.1}s",
                stats.indexed, stats.skipped, stats.seconds
            );
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
    }
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
