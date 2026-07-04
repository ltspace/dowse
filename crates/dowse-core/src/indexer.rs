use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use tantivy::{Index, IndexWriter, doc};
use walkdir::WalkDir;

use crate::extract::extract_text;
use crate::meta::{IndexMeta, SCHEMA_VERSION, save_meta};
use crate::ocr;
use crate::ocr_queue::OcrQueue;
use crate::{Fields, build_schema, register_tokenizers};

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
///
/// `index_dir` 只有图片分支会用到（去查/写 OCR 队列的持久化状态），文本文件的
/// 建文档逻辑跟里程碑 3 完全一样。
pub(crate) fn add_file_document(
    writer: &IndexWriter,
    fields: &Fields,
    path: &Path,
    index_dir: &Path,
) -> Result<bool> {
    if ocr::is_image(path) {
        return add_image_document(writer, fields, path, index_dir);
    }

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

/// 图片文档：内容来自 OCR，可能是缓存命中的旧识别结果（全量重建索引时最常见），
/// 也可能是刚发现、还没识别完的占位符（content 为空字符串，文件名先可搜，正文等
/// 后台 worker 池处理完再回填）。全量重建和增量更新（watch/reconcile 触发的
/// upsert）都走这一处，跟文本文档的建文档逻辑是同一层级的姊妹函数。
///
/// 单文件超过 OCR 体积上限时整篇跳过（不占位、不入 OCR 队列），跟文本文件超限
/// 时 `extract_text` 返回 None 被跳过是同一语义。
fn add_image_document(
    writer: &IndexWriter,
    fields: &Fields,
    path: &Path,
    index_dir: &Path,
) -> Result<bool> {
    let Some((mtime, size)) = file_stat(path) else {
        return Ok(false);
    };
    if size > ocr::MAX_IMAGE_BYTES {
        return Ok(false);
    }

    let queue = OcrQueue::for_index_dir(index_dir);
    let content = match queue.cached_content(path, mtime, size) {
        Some(cached) => cached,
        None => {
            queue.enqueue(path.to_path_buf(), mtime, size);
            String::new()
        }
    };

    add_image_document_with_content(writer, fields, path, mtime, size, &content)?;
    Ok(true)
}

/// 用已知内容（OCR worker 识别完的最终结果，或者上面缓存命中的旧结果）直接建一篇
/// 图片文档，不碰 OCR 队列——OcrPipeline 的 worker 写回结果时也是调这个。
pub(crate) fn add_image_document_with_content(
    writer: &IndexWriter,
    fields: &Fields,
    path: &Path,
    mtime: i64,
    size: u64,
    content: &str,
) -> Result<()> {
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
        // TODO(kind 字段接线点): schema v3 合并后这里要补 fields.kind => "image"，
        // 上面 add_file_document 里文本分支对应补 "text"。kind 字段本身由并行分支
        // 加进 build_schema()，此处只是标记接线位置，不在本里程碑改 schema。
        fields.content => content.to_string(),
        fields.mtime => mtime,
        fields.size => size,
    ))?;
    Ok(())
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
        if add_file_document(&writer, &fields, &path, index_dir)? {
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

    // 上面的 walk 里，每碰到一张新图片就往 OcrQueue 的内存态里塞了一条 pending，
    // 这里统一落一次盘——批量存盘，不是每碰到一张图片就写一次文件。
    OcrQueue::for_index_dir(index_dir)
        .save()
        .context("保存 OCR 队列状态失败")?;

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
