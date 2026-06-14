use std::path::Path;
use std::time::Instant;

use anyhow::{Context, Result};
use tantivy::{doc, Index, IndexWriter};
use walkdir::WalkDir;

use crate::extract::extract_text;
use crate::{build_schema, register_tokenizers};

/// 一次重建索引的统计结果，CLI 拿去打报告。
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub seconds: f64,
}

/// 这些目录整棵跳过：要么是依赖/构建产物，要么是仓库内部数据。
const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".venv", "__pycache__"];

/// v0 策略：全量重建。删掉旧索引目录，从头扫一遍。
/// 增量更新是里程碑 3 的事，现在先把"能搜"跑通。
pub fn rebuild_index(index_dir: &Path, target_dir: &Path) -> Result<IndexStats> {
    let start = Instant::now();

    if index_dir.exists() {
        std::fs::remove_dir_all(index_dir).context("清理旧索引目录失败")?;
    }
    std::fs::create_dir_all(index_dir)?;

    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema)?;
    register_tokenizers(&index);

    // 200MB 的写入缓冲：攒满一批才刷盘，比逐篇写快一个量级
    let mut writer: IndexWriter = index.writer(200 * 1024 * 1024)?;

    let mut indexed = 0usize;
    let mut skipped = 0usize;

    let walker = WalkDir::new(target_dir).into_iter().filter_entry(|e| {
        // 根目录是用户显式指定的扫描起点，跳过规则不适用于它——
        // 否则 filter_entry 会让 walkdir 连根目录都不下钻，整棵树静默扫出 0 个文件。
        if e.depth() == 0 {
            return true;
        }
        let name = e.file_name().to_string_lossy();
        !(e.file_type().is_dir() && (SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.')))
    });

    for entry in walker {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();

        let Some(content) = extract_text(path) else {
            skipped += 1;
            continue;
        };

        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();

        writer.add_document(doc!(
            fields.path => path.to_string_lossy().into_owned(),
            fields.name => name,
            fields.ext => ext,
            fields.content => content,
        ))?;
        indexed += 1;
    }

    // commit 才是真正落盘的时刻；之前 add_document 都只进内存缓冲
    writer.commit().context("索引提交失败")?;

    Ok(IndexStats {
        indexed,
        skipped,
        seconds: start.elapsed().as_secs_f64(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rebuild_index_root_dot_prefixed_dir_is_not_skipped() -> Result<()> {
        // 根目录本身以 "." 开头时，不应触发 dot-prefix 跳过规则——
        // 用户显式指定的扫描起点必须被下钻，否则整棵树静默扫出 0 个文件。
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix(".dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("note.txt"), "hello dowse")?;

        let stats = rebuild_index(index_dir.path(), target_dir.path())?;

        assert_eq!(stats.indexed, 1);
        Ok(())
    }
}
