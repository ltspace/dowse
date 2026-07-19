//! 全量重建索引（[`rebuild_index`]/[`rebuild_index_with_progress`]）：删掉旧
//! 索引目录、按卷能力选文件枚举方式（MFT 快速枚举或 walkdir 遍历）、逐个
//! 文件抽取内容建文档。也提供文本/图片文档的建文档逻辑（[`add_file_document`]
//! 等），供增量更新（`updater.rs`）复用，保证全量重建和增量更新写进索引的
//! 字段完全一致。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Instant, UNIX_EPOCH};

use anyhow::{Context, Result};
use tantivy::{Index, IndexWriter, doc};
use walkdir::WalkDir;

use crate::cursor::{UsnCursor, VolumeKey};
use crate::extract::{extract_text, is_extractable};
use crate::meta::{IndexMeta, SCHEMA_VERSION, save_meta};
use crate::ocr;
use crate::ocr_queue::OcrQueue;
use crate::volume::{self, RootCapability};
use crate::{Fields, build_schema, register_tokenizers};

/// 一次重建索引的统计结果，CLI 拿去打报告。
pub struct IndexStats {
    /// 成功抽取内容并写入索引的文件数。
    pub indexed: usize,
    /// 被跳过的文件数（无法抽取、读取失败、或不在收录范围内）。含下面
    /// `skipped_oversize` 那部分——超限跳过是"被跳过"的一个子类，不另算。
    pub skipped: usize,
    /// `skipped` 里因**单文件体积超过规则上限**而被跳过的文件数，单独拎出来
    /// 计数好让 CLI 报告/`dowse status` 把"这些文件是被体积上限挡住的、不是
    /// 内容有问题"这件事显式讲清楚（规则上限见 `rules` 模块）。
    pub skipped_oversize: usize,
    /// 本次重建的总耗时（秒）。
    pub seconds: f64,
}

/// [`add_file_document`] 的结果：收录、跳过（无可索引文本）、或因体积超限跳过。
/// 把"因体积超限"从普通跳过里拆出来，好让上层单独计数（[`IndexStats::skipped_oversize`]）。
pub(crate) enum AddOutcome {
    /// 抽到内容并写入了索引。
    Indexed,
    /// 没有可索引文本（不支持的格式/损坏/空文件等），跳过。
    Skipped,
    /// 本应可抽取，但单文件体积超过规则上限，跳过。
    SkippedOversize,
}

/// 全量重建索引期间的进度汇报节奏：每处理这么多个文件（收录 + 跳过一起算）
/// 才回调一次，不是逐文件都报——浮窗那头要把这个数字实时刷到界面上，逐文件
/// 报在几十万文件的目录上会把 Tauri 的事件 IPC 打爆。CLI 在这个基础上再自己
/// 降频到每千个文件打一行（是 PROGRESS_INTERVAL 的整数倍），两端共用同一处
/// 回调，只是各自选了不同的上报/打印间隔，逻辑不重复一份。
pub(crate) const PROGRESS_INTERVAL: usize = 50;

/// 单次进度汇报：累计处理数（收录 + 跳过），和刚处理完的那个文件路径。
#[derive(Debug, Clone)]
pub struct IndexProgress {
    /// 到目前为止累计处理的文件数（收录 + 跳过一起算）。
    pub processed: usize,
    /// 刚处理完的那个文件路径。
    pub path: PathBuf,
}

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

/// 遍历 root 下所有该收录的文件路径，统一应用跳过规则（排除目录、隐藏目录）。
/// 全量重建、启动对账、监听时目录整体移入都共用这一处遍历逻辑，保证三条路径
/// "哪些文件算数"的判断完全一致。排除目录列表取自进程级当前生效规则
/// （见 `rules` 模块）。
pub(crate) fn walk_index_files(root: &Path) -> impl Iterator<Item = PathBuf> {
    walk_index_files_with(root, crate::rules::active_rules())
}

/// [`walk_index_files`] 的纯函数版：排除判定接收显式规则，不碰进程级全局，
/// 便于单测。规则用 `Arc` 传进来直接 move 进 `filter_entry` 闭包，让它随迭代器
/// 存活整个遍历过程。
pub(crate) fn walk_index_files_with(
    root: &Path,
    rules: std::sync::Arc<crate::rules::IndexRules>,
) -> impl Iterator<Item = PathBuf> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(move |e| {
            // 根目录是显式指定的扫描起点，跳过规则不适用于它——否则 filter_entry
            // 会让 walkdir 连根都不下钻，整棵树静默扫出 0 个文件。
            if e.depth() == 0 {
                return true;
            }
            let name = e.file_name().to_string_lossy();
            !(e.file_type().is_dir() && rules.is_dir_excluded(&name))
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
    // MFT 枚举本身不像 walkdir 那样能按目录下钻剪枝，只能拿到重建好的完整路径
    // 再逐条筛掉穿过排除目录的文件，让快车道跟 `walk_index_files` 的排除口径
    // 一致（否则 node_modules/target 等会在快车道照进索引）。
    let rules = crate::rules::active_rules();
    let files: Vec<PathBuf> = files
        .into_iter()
        .filter(|p| !rules.path_under_excluded_dir(p, target_dir))
        .collect();
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

/// 抽取一个文件并写进索引（不 commit）。返回 [`AddOutcome`]：收录 / 跳过 /
/// 因体积超限跳过。全量重建和增量更新共用这一处建文档逻辑，保证两条路径写进去
/// 的字段完全一致。
///
/// `index_dir` 只有图片分支会用到（去查/写 OCR 队列的持久化状态），文本文件的
/// 建文档逻辑跟里程碑 3 完全一样。
pub(crate) fn add_file_document(
    writer: &IndexWriter,
    fields: &Fields,
    path: &Path,
    index_dir: &Path,
) -> Result<AddOutcome> {
    if ocr::is_image(path) {
        // 图片走 OCR，体积上限是另一套（ocr::MAX_IMAGE_BYTES），超限按普通跳过
        // 计，不并进文本抽取的"因体积超限跳过"统计。
        return Ok(if add_image_document(writer, fields, path, index_dir)? {
            AddOutcome::Indexed
        } else {
            AddOutcome::Skipped
        });
    }

    let Some(content) = extract_text(path) else {
        // 抽不出文本可能只是不支持的格式/损坏，也可能是本应可抽取但体积超限。
        // 只对"本应可抽取的类型"再查一次体积，把超限跳过跟其它跳过区分开——
        // 普通不可抽取类型（.exe 等）不做这次多余的 stat。
        if is_extractable(path)
            && let Some((_, size)) = file_stat(path)
            && size > crate::rules::active_rules().max_file_bytes()
        {
            return Ok(AddOutcome::SkippedOversize);
        }
        return Ok(AddOutcome::Skipped);
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
    Ok(AddOutcome::Indexed)
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
///
/// # Examples
///
/// ```no_run
/// # fn main() -> anyhow::Result<()> {
/// use std::path::Path;
/// use dowse::rebuild_index;
///
/// let stats = rebuild_index(Path::new("./my-index"), Path::new("./my-documents"))?;
/// println!(
///     "收录 {} 个文件，跳过 {}，耗时 {:.1}s",
///     stats.indexed, stats.skipped, stats.seconds
/// );
/// # Ok(())
/// # }
/// ```
pub fn rebuild_index(index_dir: &Path, target_dir: &Path) -> Result<IndexStats> {
    rebuild_index_with_progress(index_dir, target_dir, |_| {})
}

/// 全量重建撞上 Windows 上实时扫描的杀毒/EDR 软件时，tantivy 的索引写入
/// 线程会在新建段文件的一瞬间被拒绝访问（`ERROR_ACCESS_DENIED`），进而把
/// 整个 `IndexWriter` 判定为"已死"，往外抛的是笼统的 "index writer was
/// killed"，看不到具体是哪个文件、什么原因——排障时强制在这条路径上追加
/// `commit()` 才挖出真实的底层错误是某个刚建的段文件 `OpenWriteError`/
/// `PermissionDenied`（Windows 错误码 5）。这跟 `remove_dir_all_retrying`
/// 处理的"目录没删干净"是两回事：这里索引目录是全新建的，问题出在扫描器
/// 和 tantivy 高频建/删文件之间的瞬时竞争，属于外部软件的一次性抖动。
///
/// 全量重建本来就是从头删/建索引目录，天然幂等——整次重试比在 tantivy
/// 内部找钩子补丁更可靠。仅在命中这个特征错误时重试，别的错误（磁盘满、
/// 目标目录不存在等）照常直接透传。
const REBUILD_RETRIES: u32 = 10;
/// 重试之间给扫描器一点喘息时间再动手，太快重试等于原地反复撞同一堵墙；
/// 按 2 倍指数退避递增（300ms、600ms……），跟 `remove_dir_all_retrying`
/// 同一套思路，封顶避免最坏情况下退避本身拖太久。
const REBUILD_RETRY_BASE_DELAY: std::time::Duration = std::time::Duration::from_millis(300);
const REBUILD_RETRY_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(3);

/// 每次尝试用几个 tantivy 写入线程：并发建/删文件的规模越大，越容易撞上
/// 扫描器（见 `rebuild_index_attempt` 里的解释）。第一次尝试按常规折中的
/// 4 线程跑（吞吐还过得去），连续失败就说明这台机器眼下的扫描器压力偏大，
/// 后面几次重试逐步收窄并发规模，用吞吐换成功率——第 8 次起降到只剩 1
/// 线程，跟单线程写入等价，实测这是最不容易再撞上的配置。
fn writer_threads_for_attempt(attempt: u32) -> usize {
    match attempt {
        1..=4 => 4,
        5..=7 => 2,
        _ => 1,
    }
}

/// 判定一次重建失败是不是上面说的"被扫描器打断的瞬时竞争"。这个根因在
/// tantivy 里会以两种不同的外壳冒出来，两种都要认：
///
/// - 建文档循环里调用 `add_document` 时撞上：某个 worker 线程早先已经
///   因为这个原因死了，写入端被判定"已死"，这时候拿到的是笼统的
///   `TantivyError::ErrorInThread`，看不到具体是哪个文件、什么原因
///   （tantivy 自己也没往外传）。
/// - 一路挺到 `commit()` 才撞上：这时候 tantivy 会把真正死掉的那个
///   worker 线程的原始错误直接吐出来，是 `TantivyError::OpenWriteError`
///   包着 Windows 错误码 5（`PermissionDenied`）——排障记录里就是靠这条
///   路径才挖到真实根因。只在两种外壳下都遇到过，所以只认这两种，别的
///   IoError（比如磁盘满、路径不存在）不在这个判据里，照常直接透传。
pub(crate) fn is_transient_writer_killed(err: &anyhow::Error) -> bool {
    err.chain().any(
        |cause| match cause.downcast_ref::<tantivy::TantivyError>() {
            Some(tantivy::TantivyError::ErrorInThread(_)) => true,
            Some(tantivy::TantivyError::OpenWriteError(
                tantivy::directory::error::OpenWriteError::IoError { io_error, .. },
            )) => io_error.kind() == std::io::ErrorKind::PermissionDenied,
            Some(tantivy::TantivyError::IoError(io_error)) => {
                io_error.kind() == std::io::ErrorKind::PermissionDenied
            }
            _ => false,
        },
    )
}

/// 同 `rebuild_index`，多一个进度回调：每处理 `PROGRESS_INTERVAL` 个文件就报一次
/// 累计处理数和当前文件路径。CLI 的 `dowse index` 和浮窗的"建索引"都走这个，
/// 一份实现两处消费，避免两端各自维护一份遍历+回调逻辑。
pub fn rebuild_index_with_progress(
    index_dir: &Path,
    target_dir: &Path,
    mut on_progress: impl FnMut(IndexProgress),
) -> Result<IndexStats> {
    // 开工前把这个索引目录旁的规则加载为进程级当前生效规则：底层的
    // walk_index_files/is_extractable/extract_text 都读全局，加载一次让本次
    // 重建整体尊重 rules.json（无规则文件时加载到的就是默认值，行为不变）。
    crate::rules::load_active_rules(index_dir);

    let start = Instant::now();
    let mut delay = REBUILD_RETRY_BASE_DELAY;

    for attempt in 1..=REBUILD_RETRIES {
        let writer_threads = writer_threads_for_attempt(attempt);
        match rebuild_index_attempt(index_dir, target_dir, writer_threads, &mut on_progress) {
            Ok((indexed, skipped, skipped_oversize, usn_cursors)) => {
                return finish_rebuild(
                    index_dir,
                    target_dir,
                    indexed,
                    skipped,
                    skipped_oversize,
                    usn_cursors,
                    start,
                );
            }
            Err(err) if attempt < REBUILD_RETRIES && is_transient_writer_killed(&err) => {
                eprintln!(
                    "全量重建撞上瞬时的索引写入中断（第 {attempt}/{REBUILD_RETRIES} 次，常见于杀毒/EDR \
                     软件实时扫描新建文件），等待 {delay:?} 后整次重试: {err}"
                );
                std::thread::sleep(delay);
                delay = (delay * 2).min(REBUILD_RETRY_MAX_DELAY);
                continue;
            }
            Err(err) => return Err(err),
        }
    }
    unreachable!("循环要么在 Ok 分支返回，要么在最后一次尝试的 Err 分支返回")
}

/// 一次重建尝试：删旧目录、建新索引、把 `files` 全部写进去，返回收录/跳过计数
/// 和 USN 游标基线，交给 `finish_rebuild` 收尾。失败时上层决定要不要整次重试。
fn rebuild_index_attempt(
    index_dir: &Path,
    target_dir: &Path,
    writer_threads: usize,
    on_progress: &mut impl FnMut(IndexProgress),
) -> Result<(usize, usize, usize, HashMap<VolumeKey, UsnCursor>)> {
    if index_dir.exists() {
        remove_dir_all_retrying(index_dir).context("清理旧索引目录失败")?;
    }
    std::fs::create_dir_all(index_dir)?;

    let (schema, fields) = build_schema();
    let index = Index::create_in_dir(index_dir, schema)?;
    register_tokenizers(&index);

    // 200MB 的写入缓冲：攒满一批才刷盘，比逐篇写快一个量级。
    //
    // 线程数不用 tantivy 默认的 `index.writer(..)`（按 CPU 核数封顶 8）：
    // 高核数机器默认会开满 8 个 worker 线程，在索引目录里高频并发建/删
    // 文件（tantivy 每建一个段文件都要重写一次 `.managed.json`），这种
    // "多线程短时间内在同一目录爆发式建删文件"的模式正好撞上杀毒/EDR
    // 软件（尤其是带行为检测的，比如反勒索模块）的敏感区，实测默认 8
    // 线程哪怕配上面的整次重试兜底也压不住。`writer_threads` 由上层的
    // `writer_threads_for_attempt` 按重试次数递减传进来，第一次尝试给
    // 4（吞吐折中），连续撞上就逐步收窄到 1。
    let mut writer: IndexWriter =
        index.writer_with_num_threads(writer_threads, 200 * 1024 * 1024)?;

    let mut indexed = 0usize;
    let mut skipped = 0usize;
    let mut skipped_oversize = 0usize;

    // 按卷判定走 MFT 快速枚举还是现有的 walkdir 遍历（设计文档第一节）。
    // 两条路径产出的文件清单语义一致——下面的收录循环完全不用关心是哪条路径
    // 来的，这正是"上层感知不到差别"要的效果。
    let (files, usn_cursors) = collect_index_files(target_dir);

    for path in files {
        match add_file_document(&writer, &fields, &path, index_dir)? {
            AddOutcome::Indexed => indexed += 1,
            // 超限跳过既计入总跳过数，也单独累加一份明细。
            AddOutcome::Skipped => skipped += 1,
            AddOutcome::SkippedOversize => {
                skipped += 1;
                skipped_oversize += 1;
            }
        }
        let processed = indexed + skipped;
        if processed % PROGRESS_INTERVAL == 0 {
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

    Ok((indexed, skipped, skipped_oversize, usn_cursors))
}

/// 重建成功之后的收尾：写 meta.json、拼 `IndexStats`。这一步不碰 tantivy
/// 的 `IndexWriter`，不会撞上 `rebuild_index_attempt` 那种瞬时竞争，
/// 不需要包进重试循环。
fn finish_rebuild(
    index_dir: &Path,
    target_dir: &Path,
    indexed: usize,
    skipped: usize,
    skipped_oversize: usize,
    usn_cursors: HashMap<VolumeKey, UsnCursor>,
    start: Instant,
) -> Result<IndexStats> {
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
        skipped_oversize,
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

    /// 默认规则（20MB 上限）下，一个超限文件应被跳过并单独计进 skipped_oversize，
    /// 正常文件照常收录。用 set_len 造稀疏文件，只撑大小不真写数据。
    /// 依赖进程级全局为默认规则——本 crate 里没有任何测试会把非默认规则灌进全局
    /// （非默认路径都走接收显式规则的 `*_with` 纯函数单测），所以这个断言稳定。
    #[test]
    fn rebuild_counts_oversize_skips_separately() -> Result<()> {
        let index_dir = tempfile::tempdir()?;
        let target_dir = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;

        std::fs::write(target_dir.path().join("normal.txt"), "正常内容 normal")?;
        let big = target_dir.path().join("huge.log");
        let f = std::fs::File::create(&big)?;
        f.set_len(crate::rules::IndexRules::default().max_file_bytes() + 1)?;
        drop(f);

        let stats = rebuild_index(index_dir.path(), target_dir.path())?;
        assert_eq!(stats.indexed, 1, "只有正常文件应被收录");
        assert_eq!(stats.skipped, 1, "超限文件计入总跳过数");
        assert_eq!(stats.skipped_oversize, 1, "超限文件单独计一份明细");
        Ok(())
    }

    /// `walk_index_files_with` 应按传入规则的排除列表整棵跳过目录；不在列表里的
    /// 目录（哪怕是抽出前的默认排除名）正常收录——列表是整体替换语义。
    #[test]
    fn walk_index_files_with_respects_custom_exclude() -> Result<()> {
        let root = tempfile::Builder::new().prefix("dowse-test-").tempdir()?;
        std::fs::create_dir_all(root.path().join("build"))?;
        std::fs::create_dir_all(root.path().join("node_modules"))?;
        std::fs::write(root.path().join("keep.txt"), "a")?;
        std::fs::write(root.path().join("build").join("skip.txt"), "b")?;
        std::fs::write(root.path().join("node_modules").join("kept.txt"), "c")?;

        // 自定义规则只排除 build；node_modules 不在列表里，应被收录。
        let rules = std::sync::Arc::new(crate::rules::IndexRules {
            exclude_dirs: vec!["build".into()],
            extra_text_exts: vec![],
            max_file_mb: 20,
        });
        let mut files: Vec<String> = walk_index_files_with(root.path(), rules)
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        files.sort();
        assert_eq!(files, vec!["keep.txt", "kept.txt"]);
        Ok(())
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
