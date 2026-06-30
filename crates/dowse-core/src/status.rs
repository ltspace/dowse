use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Result;
use walkdir::WalkDir;

use crate::registered_roots;

/// 索引的只读状态快照，给 CLI/MCP 的 `index_status` 一类查询用。
pub struct IndexStatus {
    /// 索引里的文档总数。
    pub num_docs: u64,
    /// 已注册的索引根目录。
    pub roots: Vec<PathBuf>,
    /// 索引目录落盘体积（字节），递归求和索引目录下所有文件大小。
    pub disk_size_bytes: u64,
    /// 索引目录下所有文件里最新的 mtime——近似"最近一次更新时间"。
    /// 目录为空（理论上不该发生，schema 校验已保证至少有 meta 文件）时是 None。
    pub last_updated: Option<SystemTime>,
}

/// 读索引的只读状态：文档数、已注册根、落盘体积、最近更新时间。
///
/// 先走 `registered_roots` 做存在性 + schema 版本校验——索引不存在或版本不对时
/// 复用它已有的报错文案（提示重建），不在这里另造一套错误信息。
pub fn index_status(index_dir: &Path) -> Result<IndexStatus> {
    let roots = registered_roots(index_dir)?;

    let mut disk_size_bytes = 0u64;
    let mut last_updated: Option<SystemTime> = None;
    for entry in WalkDir::new(index_dir).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        disk_size_bytes += meta.len();
        if let Ok(modified) = meta.modified() {
            last_updated = Some(match last_updated {
                Some(prev) if prev >= modified => prev,
                _ => modified,
            });
        }
    }

    // 只读打开一次 Searcher 拿文档总数：这是唯一需要真正读 tantivy 索引的部分,
    // 复用 Searcher::open 而不是自己再开一遍 Index，保证 schema 校验、分词器
    // 注册这些细节只有一处实现。
    let num_docs = crate::Searcher::open(index_dir)?.num_docs();

    Ok(IndexStatus {
        num_docs,
        roots,
        disk_size_bytes,
        last_updated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_status_on_missing_index_errors() {
        let index_dir = tempfile::tempdir().unwrap();
        // 目录存在但从没建过索引：没有 meta.json，应该报错而不是返回空状态。
        let missing = index_dir.path().join("no-such-index");
        assert!(index_status(&missing).is_err());
    }

    #[test]
    fn index_status_reports_docs_roots_and_nonzero_disk_size() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::write(target_dir.path().join("note.md"), "内容")?;

        crate::rebuild_index(index_dir.path(), target_dir.path())?;

        let status = index_status(index_dir.path())?;
        assert_eq!(status.num_docs, 1);
        assert_eq!(status.roots, vec![target_dir.path().to_path_buf()]);
        assert!(status.disk_size_bytes > 0, "刚建的索引落盘体积不应为 0");
        assert!(status.last_updated.is_some());
        Ok(())
    }
}
