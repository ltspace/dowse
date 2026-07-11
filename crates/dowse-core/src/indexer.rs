use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use tantivy::{Index, IndexWriter, doc};
use walkdir::WalkDir;

use crate::cursor::{UsnCursor, VolumeKey};
use crate::extract::extract_text;
use crate::meta::{IndexMeta, SCHEMA_VERSION, save_meta};
use crate::ocr;
use crate::ocr_queue::OcrQueue;
use crate::volume::{self, RootCapability};
use crate::{Fields, build_schema, register_tokenizers};

/// 一次重建索引的统计结果，CLI 拿去打报告。
pub struct IndexStats {
    pub indexed: usize,
    pub skipped: usize,
    pub seconds: f64,
}

/// 全量重建索引期间的进度汇报节奏：每处理这么多个文件（收录 + 跳过一起算）
/// 才回调一次，不是逐文件都报——浮窗那头要把这个数字实时刷到界面上，逐文件
/// 报在几十万文件的目录上会把 Tauri 的事件 IPC 打爆。CLI 在这个基础上再自己
/// 降频到每千个文件打一行（是 PROGRESS_INTERVAL 的整数倍），两端共用同一处
/// 回调，只是各自选了不同的上报/打印间隔，逻辑不重复一份。
pub const PROGRESS_INTERVAL: usize = 50;

/// 单次进度汇报：累计处理数（收录 + 跳过），和刚处理完的那个文件路径。
#[derive(Debug, Clone)]
pub struct IndexProgress {
    pub processed: usize,
    pub path: PathBuf,
}

/// 这些目录整棵跳过：要么是依赖/构建产物，要么是仓库内部数据。
const SKIP_DIRS: &[&str] = &["node_modules", "target", ".git", ".venv", "__pycache__"];

/// `remove_dir_all` 撞上 `PermissionDenied` 时的重试次数上限（首次尝试之外
/// 还会再试这么多次）。
const REMOVE_DIR_ALL_RETRIES: u32 = 4;
/// 重试的起始等待时长，按 2 倍指数退避递增（50ms、100ms、200ms、400ms）。
const REMOVE_DIR_ALL_RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_millis(50);

/// 删整个目录，撞上 `PermissionDenied` 时按指数退避重试几次再放弃。
///
/// Windows 下删索引目录是 flaky 测试和生产 rebuild 共同的根因：tantivy 的
/// mmap/合并线程、notify 的目录句柄、OCR worker 释放句柄都不一定跟"调用方
/// 认为已经用完了"这个时间点同步——句柄晚释放几十毫秒很常见。标准库的
/// `remove_dir_all` 只试一次，撞上未释放的句柄就直接报错；这里给它一点
/// 时间让句柄自然释放，仍然只在 `PermissionDenied` 上重试，别的错误
/// （目录本来就不存在等）照常直接透传，不掩盖真实故障。
pub fn remove_dir_all_retrying(path: &Path) -> std::io::Result<()> {
    let mut delay = REMOVE_DIR_ALL_RETRY_BASE_DELAY;
    for attempt in 1..=REMOVE_DIR_ALL_RETRIES {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
                eprintln!(
                    "删除目录 {} 遇到句柄未释放（第 {attempt}/{REMOVE_DIR_ALL_RETRIES} 次重试前等待 {delay:?}）: {err}",
                    path.display()
                );
                std::thread::sleep(delay);
                delay *= 2;
            }
            Err(err) => return Err(err),
        }
    }
    std::fs::remove_dir_all(path)
}

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

/// 按卷能力探测选文件清单的产出方式：NTFS + 管理员权限就用 MFT 快速枚举，
/// 拿不到就退回 [`walk_index_files`]（设计文档第一节的降级判定）。
///
/// MFT 枚举失败（探测通过了，但真枚举时出于某种原因失败——比如探测和枚举
/// 之间权限被收回）时也静默退回 walkdir，不让一次枚举失败拖垮整个建索引
/// 流程——诚实降级不只是"没权限就降级"，"权限看着有、用起来失败"也要兜底。
fn collect_index_files(target_dir: &Path) -> (Vec<PathBuf>, HashMap<VolumeKey, UsnCursor>) {
    if let RootCapability::Fast { volume } = volume::probe_root_capability(target_dir)
        && let Some(result) = collect_via_mft(&volume, target_dir)
    {
        return result;
    }
    (walk_index_files(target_dir).collect(), HashMap::new())
}

#[cfg(windows)]
fn collect_via_mft(
    volume: &VolumeKey,
    target_dir: &Path,
) -> Option<(Vec<PathBuf>, HashMap<VolumeKey, UsnCursor>)> {
    // 先拍游标快照、再枚举——顺序很关键：枚举期间如果有并发改动写进了
    // USN Journal，只要它们的 usn >= 这个快照的 next_usn，后续补账/live
    // 监听就一定会重放到；反过来"枚举完再拍快照"会漏掉枚举过程中发生的变更。
    let cursor = match crate::usn::snapshot_journal(volume) {
        Ok(cursor) => cursor,
        Err(err) => {
            eprintln!("拍 USN 游标快照失败（{volume}），MFT 快速路径整体回退: {err}");
            return None;
        }
    };
    let (_table, files, stats) =
        match crate::mft::enumerate(volume, std::slice::from_ref(&target_dir.to_path_buf())) {
            Ok(result) => result,
            Err(err) => {
                eprintln!("MFT 快速枚举失败（{volume}），回退到目录遍历: {err}");
                return None;
            }
        };
    eprintln!(
        "MFT 快速枚举（{volume}）：整卷扫到 {} 条记录，落在监听根内 {} 条",
        stats.scanned, stats.matched
    );
    let mut cursors = HashMap::new();
    cursors.insert(volume.clone(), cursor);
    Some((files, cursors))
}

#[cfg(not(windows))]
fn collect_via_mft(
    _volume: &VolumeKey,
    _target_dir: &Path,
) -> Option<(Vec<PathBuf>, HashMap<VolumeKey, UsnCursor>)> {
    None
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
        // 文本抽取管线产出 "text"；图片走 add_image_document_with_content，写 "image"。
        fields.kind => "text",
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
        fields.kind => "image",
        fields.content => content.to_string(),
        fields.mtime => mtime,
        fields.size => size,
    ))?;
    Ok(())
}

/// 索引提交尾部的顺序不变量：**先**把 OCR 队列状态落盘，**后**提交 tantivy 的
/// 索引写入。全量重建（本文件）和增量更新批处理尾部（`updater.rs::apply`）
/// 都通过这一个函数收尾，顺序只需要在这一处锁死——两个调用方各自决定"要不要
/// 存 OCR 队列"（`save_ocr_queue` 传空操作即可跳过），但只要传了非空操作，
/// 它必须先于 `commit_writer` 执行完。
///
/// 为什么方向不能反：图片文档一入索引就带着正确的 (mtime,size)（哪怕内容还是
/// 占位的空字符串），如果先 commit 再存队列，进程恰好在两步之间崩溃时，索引
/// 里已经落地的占位文档会让下次启动对账判定"这张图片没有变化"，而队列里记着
/// 的"这张图片还没识别"却没能持久化——这张图片的文字就永远进不了索引。反过来，
/// 队列先落盘、索引后提交：崩溃后最坏情况是重复识别一张已经处理过的 pending
/// 图片，幂等无害。
pub(crate) fn commit_index_tail(
    save_ocr_queue: impl FnOnce() -> Result<()>,
    commit_writer: impl FnOnce() -> Result<()>,
) -> Result<()> {
    save_ocr_queue()?;
    commit_writer()
}

/// v0 策略：全量重建。删掉旧索引目录，从头扫一遍。
/// 增量更新是里程碑 3 的事，现在先把"能搜"跑通。
///
/// 不需要进度直播的调用方（测试、内部各处对账/重建路径）走这个薄封装，
/// 回调是空操作——真正的实现和进度上报都在 `rebuild_index_with_progress`。
pub fn rebuild_index(index_dir: &Path, target_dir: &Path) -> Result<IndexStats> {
    rebuild_index_with_progress(index_dir, target_dir, |_| {})
}

/// 同 `rebuild_index`，多一个进度回调：每处理 `PROGRESS_INTERVAL` 个文件就报一次
/// 累计处理数和当前文件路径。CLI 的 `dowse index` 和浮窗的"建索引"都走这个，
/// 一份实现两处消费，避免两端各自维护一份遍历+回调逻辑。
pub fn rebuild_index_with_progress(
    index_dir: &Path,
    target_dir: &Path,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexStats> {
    let start = Instant::now();

    if index_dir.exists() {
        remove_dir_all_retrying(index_dir).context("清理旧索引目录失败")?;
    }
    std::fs::create_dir_all(index_dir)?;

    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema)?;
    register_tokenizers(&index);

    // 200MB 的写入缓冲：攒满一批才刷盘，比逐篇写快一个量级
    let mut writer: IndexWriter = index.writer(200 * 1024 * 1024)?;

    let mut indexed = 0usize;
    let mut skipped = 0usize;

    // 按卷判定走 MFT 快速枚举还是现有的 walkdir 遍历（设计文档第一节）。
    // 两条路径产出的文件清单语义一致——下面的收录循环完全不用关心是哪条路径
    // 来的，这正是"上层感知不到差别"要的效果。
    let (files, usn_cursors) = collect_index_files(target_dir);

    for path in files {
        if add_file_document(&writer, &fields, &path, index_dir)? {
            indexed += 1;
        } else {
            skipped += 1;
        }
        let processed = indexed + skipped;
        if processed.is_multiple_of(PROGRESS_INTERVAL) {
            on_progress(IndexProgress {
                processed,
                path: path.clone(),
            });
        }
    }

    let ocr_queue = OcrQueue::for_index_dir(index_dir);

    // 全量重建的目标只有这一个根：把队列里不属于这个根、或者对应文件已经
    // 不在磁盘上的历史条目裁掉，再落盘——不然进程级单例把旧目标目录的陈年
    // pending/processed 全量存回，只增不减、永久堆积。
    ocr_queue.compact(&[target_dir.to_path_buf()]);

    // 索引提交尾部的耐久性顺序不变量：先把 OCR 队列落盘，再提交 tantivy 写入。
    // 顺序反过来的话，进程恰好在两步之间崩溃时，索引里已经落地的图片占位
    // 文档（mtime/size 已经写对）会让下次启动对账判定"没有变化"，而队列里
    // 记着的待处理状态却没能持久化——这张图片的文字就永远进不了索引。
    // 重复识别一张 pending 图片是幂等无害的，方向反过来才会丢数据，见
    // `commit_index_tail` 的文档。
    commit_index_tail(
        || ocr_queue.save().context("保存 OCR 队列状态失败"),
        || writer.commit().map(|_| ()).context("索引提交失败"),
    )?;

    // 合流线程的句柄要在这里 join 掉：commit 只是把变更写进段文件，后台
    // 合并线程可能还在跑，不等它们退出，段文件的 mmap 句柄不会释放——
    // Windows 下紧接着的 remove_dir_all（下次重建/测试收尾）就可能撞上
    // PermissionDenied（P1 审查项：flaky 根因之一）。
    writer
        .wait_merging_threads()
        .context("等待索引合并线程退出失败")?;

    // 全量重建后重写 meta.json：记下当前 schema 版本、这次索引的根目录，
    // 以及（如果走了快速路径）刚建好的 USN 游标基线——后面启动监听时读到它
    // 就能从这一刻开始回放追平，不用退回 mtime 全扫对账（设计文档第四节）。
    save_meta(
        index_dir,
        &IndexMeta {
            schema_version: SCHEMA_VERSION,
            roots: vec![target_dir.to_path_buf()],
            usn_cursors,
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

    /// 进度回调应该每 PROGRESS_INTERVAL 个文件触发一次，累计处理数正确，
    /// 不多不少——这是浮窗"实时直播"和 CLI 千行打印共用的节奏保证。
    #[test]
    fn rebuild_index_with_progress_reports_every_interval() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::tempdir()?;

        let total = PROGRESS_INTERVAL * 2 + 3;
        for i in 0..total {
            std::fs::write(target_dir.path().join(format!("f{i}.txt")), "内容")?;
        }

        let mut reports = Vec::new();
        let stats = rebuild_index_with_progress(index_dir.path(), target_dir.path(), |p| {
            reports.push(p.processed);
        })?;

        assert_eq!(stats.indexed, total);
        assert_eq!(
            reports,
            vec![PROGRESS_INTERVAL, PROGRESS_INTERVAL * 2],
            "总数不是间隔整数倍时，最后一段不足一个间隔的尾巴不应触发额外回调"
        );
        Ok(())
    }

    /// 文件总数不到一个 PROGRESS_INTERVAL 时，进度回调一次都不该触发——
    /// 完整结果由返回的 IndexStats 兜底，不依赖至少一次回调。
    #[test]
    fn rebuild_index_with_progress_below_interval_reports_nothing() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::tempdir()?;
        std::fs::write(target_dir.path().join("only.txt"), "内容")?;

        let mut calls = 0usize;
        rebuild_index_with_progress(index_dir.path(), target_dir.path(), |_| calls += 1)?;

        assert_eq!(calls, 0);
        Ok(())
    }

    /// 锁死 `commit_index_tail` 的顺序不变量：`save_ocr_queue` 必须先于
    /// `commit_writer` 跑完——这正是 rebuild/增量更新两处收尾共用的耐久性
    /// 保证，反过来就会复现"崩溃后图片文字永远进不了索引"的 bug。
    #[test]
    fn commit_index_tail_saves_ocr_queue_before_committing_writer() {
        let order = std::cell::RefCell::new(Vec::new());
        let result = commit_index_tail(
            || {
                order.borrow_mut().push("save_queue");
                Ok(())
            },
            || {
                order.borrow_mut().push("commit_writer");
                Ok(())
            },
        );
        assert!(result.is_ok());
        assert_eq!(order.into_inner(), vec!["save_queue", "commit_writer"]);
    }

    /// 队列保存失败时不该继续提交索引写入——保持"先记账、后落地"的语义，
    /// 不能让一次失败的队列保存之后仍然把索引提交出去。
    #[test]
    fn commit_index_tail_skips_commit_when_queue_save_fails() {
        let order = std::cell::RefCell::new(Vec::new());
        let result = commit_index_tail(
            || {
                order.borrow_mut().push("save_queue");
                anyhow::bail!("模拟保存失败")
            },
            || {
                order.borrow_mut().push("commit_writer");
                Ok(())
            },
        );
        assert!(result.is_err());
        assert_eq!(
            order.into_inner(),
            vec!["save_queue"],
            "队列保存失败时不该继续提交索引写入"
        );
    }

    #[test]
    fn remove_dir_all_retrying_removes_existing_directory() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let target = dir.path().join("victim");
        std::fs::create_dir_all(&target)?;
        std::fs::write(target.join("f.txt"), "内容")?;

        remove_dir_all_retrying(&target)?;
        assert!(!target.exists());
        Ok(())
    }

    #[test]
    fn remove_dir_all_retrying_propagates_non_permission_errors() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("does-not-exist");
        let err =
            remove_dir_all_retrying(&missing).expect_err("目录不存在应该报错，不应该重试掩盖");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }
}
