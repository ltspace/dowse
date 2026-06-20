use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use tantivy::{doc, Index, IndexWriter};
use walkdir::WalkDir;

use crate::extract::extract_text;
use crate::meta::{save_meta, IndexMeta, SCHEMA_VERSION};
use crate::{build_schema, register_tokenizers, Fields};

/// 一次重建索引的统计结果，CLI 拿去打报告。
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub seconds: f64,
}

/// 这些目录整棵跳过：要么是依赖/构建产物，要么是仓库内部数据。
const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".venv", "__pycache__"];

/// 遍历 root 下所有该收录的文件路径，统一应用跳过规则（依赖/构建产物目录、
/// 隐藏目录）。全量重建、启动对账、监听时目录整体移入都共用这一处遍历逻辑，
/// 保证三条路径"哪些文件算数"的判断完全一致。
pub(crate) fn walk_index_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| {
            // 根目录是显式指定的扫描起点，跳过规则不适用于它——否则 filter_entry
            // 会让 walkdir 连根都不下钻，整棵树静默扫出 0 个文件。
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_dir()
                && (SKIP_DIRS.contains(&name.as_ref()) || name.starts_with('.')))
        })
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| e.path().to_path_buf())
}

/// 读文件的 (mtime 毫秒, size 字节)，喂给 schema 的 mtime/size 字段和启动对账。
/// 取毫秒而不是秒：同一秒内内容变了但字节数没变的编辑，秒级 mtime 会漏掉。
/// 拿不到元数据（文件刚被删等）返回 None，调用方自己决定当 (0,0) 还是跳过。
pub(crate) fn file_stat(path: &Path) -> Option<(i64, u64)> {
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    Some((mtime, meta.len()))
}

/// 抽取一个文件并写进索引（不 commit）。返回 true=收录、false=没有可索引文本被跳过。
/// 全量重建和增量更新共用这一处建文档逻辑，保证两条路径写进去的字段完全一致。
pub(crate) fn add_file_document(writer: &IndexWriter, fields: &Fields, path: &Path) -> Result<bool> {
    let Some(content) = extract_text(path) else {
        return Ok(false);
    };
    let (mtime, size) = file_stat(path).unwrap_or((0, 0));

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
        fields.mtime => mtime,
        fields.size => size,
    ))?;
    Ok(true)
}

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

    for path in walk_index_files(target_dir) {
        if add_file_document(&writer, &fields, &path)? {
            indexed += 1;
        } else {
            skipped += 1;
        }
    }

    // commit 才是真正落盘的时刻；之前 add_document 都只进内存缓冲
    writer.commit().context("索引提交失败")?;

    // 全量重建后重写 meta.json：记下当前 schema 版本和这次索引的根目录。
    // 索引根列表是启动对账和监听要监视哪些目录的依据。
    save_meta(
        index_dir,
        &IndexMeta {
            schema_version: SCHEMA_VERSION,
            roots: vec![target_dir.to_path_buf()],
        },
    )?;

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
