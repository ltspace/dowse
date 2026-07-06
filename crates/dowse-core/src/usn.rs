//! USN 事件源：运行期文件监听从 notify 换成读 USN Journal（设计文档第三节），
//! 外加游标补账（第四节）。只在 Windows 上编译（`mod usn;` 本身已经
//! `#[cfg(windows)]` 限定，理由同 mft.rs，这里不重复标注）。

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::IO::DeviceIoControl;
use windows::Win32::System::Ioctl::{
    CREATE_USN_JOURNAL_DATA, FSCTL_CREATE_USN_JOURNAL, FSCTL_QUERY_USN_JOURNAL,
    FSCTL_READ_USN_JOURNAL, READ_USN_JOURNAL_DATA_V0, USN_JOURNAL_DATA_V0,
};

use crate::cursor::{CursorSync, UsnCursor, VolumeKey};
use crate::events::{Debouncer, WatchEvent};
use crate::frn_table::FrnTable;
use crate::indexer::walk_index_files;
use crate::updater::IndexUpdater;
use crate::usn_translate::{UsnOutcome, UsnRecord, UsnTranslator, parse_usn_record_v2_bytes};
use crate::volume;
use crate::watch::{EventSource, WatchGuard};

/// FSCTL_READ_USN_JOURNAL 单次调用的输出缓冲大小，跟 mft.rs 的 MFT 枚举缓冲
/// 同一个量级——够装几百条记录，调用次数和内存占用的平衡点。
const READ_BUFFER_SIZE: usize = 64 * 1024;
/// 新建 Journal（这个卷之前从没建过）时的初始大小/扩容步长。多数机器上卷早就
/// 有 Journal 在跑（Windows 索引服务/杀软都会用到），这两个值只在从没建过时
/// 生效，不追求精确，够用就行。
const DEFAULT_JOURNAL_MAX_SIZE: u64 = 32 * 1024 * 1024;
const DEFAULT_JOURNAL_ALLOCATION_DELTA: u64 = 4 * 1024 * 1024;
/// 实时监听时 FSCTL_READ_USN_JOURNAL 的阻塞超时（秒）。没有新变更时最多等
/// 这么久就返回一次，好让读取线程有机会看一眼 stop 标志、及时退出。
const LIVE_READ_TIMEOUT_SECS: u64 = 1;

/// Win32 HANDLE 本质是进程句柄表里的一个不透明整数，不是需要 Rust 借用检查器
/// 关心的真实内存指针——跨线程传递、从另一个线程发起 Win32 调用是微软自己
/// 文档化支持的用法。windows crate 没有默认让 HANDLE 实现 Send（它的定义只是
/// 包了一个裸指针），这里手动补上。
struct SendHandle(HANDLE);
unsafe impl Send for SendHandle {}

/// 查询一个已打开的卷句柄对应的 Journal 状态；这个卷还没建过 Journal
/// （ERROR_JOURNAL_NOT_ACTIVE）就先建一个再查一次。
fn query_or_create_journal(handle: HANDLE) -> Result<USN_JOURNAL_DATA_V0> {
    match query_journal(handle) {
        Ok(data) => Ok(data),
        Err(err) if is_journal_not_active(&err) => {
            create_journal(handle).context("创建 USN Journal 失败")?;
            query_journal(handle).context("查询 USN Journal 失败")
        }
        Err(err) => Err(err).context("查询 USN Journal 失败"),
    }
}

fn is_journal_not_active(err: &windows::core::Error) -> bool {
    err.code() == windows::Win32::Foundation::ERROR_JOURNAL_NOT_ACTIVE.to_hresult()
}

fn query_journal(handle: HANDLE) -> windows::core::Result<USN_JOURNAL_DATA_V0> {
    let mut data = USN_JOURNAL_DATA_V0::default();
    let mut bytes_returned = 0u32;
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_QUERY_USN_JOURNAL,
            None,
            0,
            Some(std::ptr::from_mut(&mut data).cast::<core::ffi::c_void>()),
            std::mem::size_of::<USN_JOURNAL_DATA_V0>() as u32,
            Some(&mut bytes_returned),
            None,
        )
    }?;
    Ok(data)
}

fn create_journal(handle: HANDLE) -> windows::core::Result<()> {
    let input = CREATE_USN_JOURNAL_DATA {
        MaximumSize: DEFAULT_JOURNAL_MAX_SIZE,
        AllocationDelta: DEFAULT_JOURNAL_ALLOCATION_DELTA,
    };
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_CREATE_USN_JOURNAL,
            Some(std::ptr::from_ref(&input).cast::<core::ffi::c_void>()),
            std::mem::size_of::<CREATE_USN_JOURNAL_DATA>() as u32,
            None,
            0,
            None,
            None,
        )
    }
}

/// 给一个卷拍一张"现在的 Journal 长什么样"快照：Journal ID + 下一个待写入的
/// USN 位置。MFT 快速枚举刚建完索引时调用这个，把返回值当基线游标存进
/// meta.json——后面每次启动就能从这个基线开始回放追平（设计文档第四节）。
pub(crate) fn snapshot_journal(volume: &VolumeKey) -> Result<UsnCursor> {
    let handle =
        volume::open_volume_handle(volume).map_err(|e| anyhow::anyhow!("打开卷句柄失败: {e}"))?;
    let _guard = scopeguard(handle);
    let data = query_or_create_journal(handle).context("查询/创建 USN Journal 失败")?;
    Ok(UsnCursor {
        journal_id: data.UsnJournalID,
        next_usn: data.NextUsn,
    })
}

fn scopeguard(handle: HANDLE) -> impl Drop {
    struct Guard(HANDLE);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }
    Guard(handle)
}

/// 一次 FSCTL_READ_USN_JOURNAL 调用的结果。
struct JournalReadBatch {
    records: Vec<UsnRecord>,
    next_usn: i64,
}

/// 读一批 Journal 记录。`block` 为 false 时非阻塞立即返回（游标补账用，追到
/// 目标 usn 就停）；为 true 时最多阻塞 `LIVE_READ_TIMEOUT_SECS` 秒等新数据
/// （实时监听用）。
fn read_journal_batch(
    handle: HANDLE,
    journal_id: u64,
    start_usn: i64,
    block: bool,
    buf: &mut [u8],
) -> Result<JournalReadBatch> {
    let input = READ_USN_JOURNAL_DATA_V0 {
        StartUsn: start_usn,
        ReasonMask: u32::MAX,
        ReturnOnlyOnClose: 0,
        Timeout: if block { LIVE_READ_TIMEOUT_SECS } else { 0 },
        BytesToWaitFor: if block { 1 } else { 0 },
        UsnJournalID: journal_id,
    };
    let mut bytes_returned: u32 = 0;
    unsafe {
        DeviceIoControl(
            handle,
            FSCTL_READ_USN_JOURNAL,
            Some(std::ptr::from_ref(&input).cast::<core::ffi::c_void>()),
            std::mem::size_of::<READ_USN_JOURNAL_DATA_V0>() as u32,
            Some(buf.as_mut_ptr().cast::<core::ffi::c_void>()),
            buf.len() as u32,
            Some(&mut bytes_returned),
            None,
        )
    }
    .map_err(|e| anyhow::anyhow!("FSCTL_READ_USN_JOURNAL 失败: {e}"))?;

    if (bytes_returned as usize) < 8 {
        return Ok(JournalReadBatch {
            records: Vec::new(),
            next_usn: start_usn,
        });
    }
    let next_usn = i64::from_ne_bytes(buf[0..8].try_into().expect("8 字节切片"));
    let mut records = Vec::new();
    let mut offset = 8usize;
    let end = bytes_returned as usize;
    while offset + 4 <= end {
        let record_length =
            u32::from_ne_bytes(buf[offset..offset + 4].try_into().expect("4 字节切片")) as usize;
        if record_length == 0 || offset + record_length > end {
            break;
        }
        if let Some(record) = parse_usn_record_v2_bytes(&buf[offset..offset + record_length]) {
            records.push(record);
        }
        offset += record_length;
    }
    Ok(JournalReadBatch { records, next_usn })
}

/// 把翻译层的结果展开成一批 [`WatchEvent`]。是不是目录记录里自带，不用像
/// notify 那样临时 stat；但"目录整体移入监听范围"这种情形，子文件不会各自
/// 产生 USN 记录（它们自己的父 FRN 没变），需要真的下钻一次磁盘——跟
/// watch.rs::translate_rename 对目录改名的处理是同一套道理，行为对齐。
fn outcome_to_events(outcome: UsnOutcome) -> Vec<WatchEvent> {
    match outcome {
        UsnOutcome::None => Vec::new(),
        UsnOutcome::Upsert { path, is_dir } => {
            if is_dir {
                walk_index_files(&path).map(WatchEvent::Upsert).collect()
            } else {
                vec![WatchEvent::Upsert(path)]
            }
        }
        UsnOutcome::Remove { path, is_dir } => {
            if is_dir {
                vec![WatchEvent::RemoveDir(path)]
            } else {
                vec![WatchEvent::Remove(path)]
            }
        }
        UsnOutcome::Rename {
            from,
            to,
            to_is_dir,
        } => {
            if to_is_dir {
                let mut events = vec![WatchEvent::RemoveDir(from)];
                events.extend(walk_index_files(&to).map(WatchEvent::Upsert));
                events
            } else {
                vec![WatchEvent::Rename { from, to }]
            }
        }
    }
}

/// 一次性游标补账的统计。
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct CatchupStats {
    pub records_read: usize,
    pub upserted: usize,
    pub removed: usize,
    pub skipped: usize,
}

/// 游标补账：从持久化的 `cursor` 回放到调用时刻的 Journal 位置，秒级追平
/// （设计文档第四节"启动时"）。回放期间产出的变更先过防抖队列合并去重
/// （同一路径来回改多次只留最终态），再一次性 apply——不是给 updater 逐条
/// commit，避免上千条历史记录拖慢启动。
///
/// 返回新的游标位置——调用方负责在这批 apply 成功之后才持久化它（"先
/// commit 索引、后写游标"，见设计文档第四节；这个函数不落盘，落盘是调用方
/// 的事，保持这层职责单一)。
pub(crate) fn catchup(
    volume: &VolumeKey,
    roots: &[PathBuf],
    table: &mut FrnTable,
    cursor: UsnCursor,
    updater: &mut IndexUpdater,
) -> Result<(CatchupStats, UsnCursor)> {
    let handle = crate::volume::open_volume_handle(volume)
        .map_err(|e| anyhow::anyhow!("打开卷句柄失败: {e}"))?;
    let _guard = scopeguard(handle);

    let live = query_journal(handle).context("查询 USN Journal 失败")?;
    let target_usn = live.NextUsn;

    let mut translator = UsnTranslator::new(std::mem::take(table));
    let mut debouncer = Debouncer::new(roots.to_vec());
    let mut buf = vec![0u8; READ_BUFFER_SIZE];
    let mut stats = CatchupStats::default();
    let mut current_usn = cursor.next_usn;

    while current_usn < target_usn {
        let batch = read_journal_batch(handle, cursor.journal_id, current_usn, false, &mut buf)?;
        if batch.records.is_empty() && batch.next_usn <= current_usn {
            break; // 没有更多可读的了（正常收尾，不算错误）
        }
        for record in batch.records {
            stats.records_read += 1;
            let outcome = translator.translate(record);
            for event in outcome_to_events(outcome) {
                debouncer.push(event);
            }
        }
        current_usn = batch.next_usn;
    }

    *table = translator.into_table();

    if !debouncer.is_empty() {
        let changes = debouncer.drain();
        let outcome = updater.apply(&changes).context("游标补账提交失败")?;
        stats.upserted = outcome.upserted;
        stats.removed = outcome.removed;
        stats.skipped = outcome.skipped;
    }

    Ok((
        stats,
        UsnCursor {
            journal_id: cursor.journal_id,
            next_usn: current_usn,
        },
    ))
}

/// 一个卷做 live 监听前的起始状态：从哪个 usn 开始读、这个 Journal 的 ID、
/// 以及接力过来的 FrnTable（MFT 枚举或游标补账建好的，不重新枚举一遍）。
pub(crate) struct VolumeStart {
    pub table: FrnTable,
    pub journal_id: u64,
    pub start_usn: i64,
}

/// 给快车道的每个卷建立监听前的起始状态：有可用游标就 MFT 重建路径表 +
/// 游标补账精确追平；没有可用游标（第一次跑，或者 Journal 被重建/回绕过）
/// 就退回 mtime 全扫对账，同时趁机拍一个新的游标基线（设计文档第四节）。
///
/// 每卷一次 MFT 枚举——不管走哪条子路径都需要新鲜的 FrnTable（进程重启后
/// 内存里的旧表已经没了，没有更省事的办法接力），但枚举本身很快（性能预算
/// 100 万条 < 5s），代价可以接受；真正被游标省下来的是"要不要把这个卷的
/// 每个文件都拿去跟索引比对 mtime/size"这一步。
pub(crate) fn bootstrap_fast_roots(
    index_dir: &std::path::Path,
    fast_roots: &[PathBuf],
    updater: &Arc<Mutex<IndexUpdater>>,
) -> Result<HashMap<VolumeKey, VolumeStart>> {
    let mut by_volume: HashMap<VolumeKey, Vec<PathBuf>> = HashMap::new();
    for root in fast_roots {
        if let Some(vol) = volume::volume_key(root) {
            by_volume.entry(vol).or_default().push(root.clone());
        }
    }

    let persisted = crate::meta::load_usn_cursors(index_dir);
    let mut starts = HashMap::new();

    for (vol, vol_roots) in by_volume {
        let handle = volume::open_volume_handle(&vol)
            .map_err(|e| anyhow::anyhow!("打开卷句柄失败 {vol}: {e}"))?;
        let live = query_journal(handle).context("查询 USN Journal 失败")?;
        unsafe {
            let _ = CloseHandle(handle);
        }

        let usable_cursor = persisted.get(&vol).copied().filter(|c| {
            crate::cursor::cursor_is_usable(*c, live.UsnJournalID, live.LowestValidUsn)
        });

        let start = match usable_cursor {
            Some(cursor) => {
                let (mut table, _files, stats) = crate::mft::enumerate(&vol, &vol_roots)
                    .with_context(|| format!("游标补账前的 MFT 枚举失败: {vol}"))?;
                eprintln!(
                    "{vol}: 游标有效，MFT 重建路径表（{} 条）后按游标回放补账",
                    stats.matched
                );
                let (catchup_stats, new_cursor) = {
                    let mut guard = updater.lock().expect("updater mutex poisoned");
                    catchup(&vol, &vol_roots, &mut table, cursor, &mut guard)?
                };
                eprintln!(
                    "{vol}: 游标补账完成，读 {} 条记录，收录 {} / 删除 {} / 跳过 {}",
                    catchup_stats.records_read,
                    catchup_stats.upserted,
                    catchup_stats.removed,
                    catchup_stats.skipped
                );
                crate::meta::save_usn_cursor(index_dir, &vol, new_cursor);
                VolumeStart {
                    table,
                    journal_id: new_cursor.journal_id,
                    start_usn: new_cursor.next_usn,
                }
            }
            None => {
                eprintln!("{vol}: 无可用游标（首次启动或已过期），退回 mtime 全扫对账");
                for root in &vol_roots {
                    let mut guard = updater.lock().expect("updater mutex poisoned");
                    if let Err(err) = crate::reconcile::reconcile(root, &mut guard) {
                        eprintln!("启动对账 {} 失败: {err}", root.display());
                    }
                }
                let (table, _files, stats) = crate::mft::enumerate(&vol, &vol_roots)
                    .with_context(|| format!("建立快速路径基线的 MFT 枚举失败: {vol}"))?;
                let fresh_cursor = UsnCursor {
                    journal_id: live.UsnJournalID,
                    next_usn: live.NextUsn,
                };
                eprintln!(
                    "{vol}: 已建立新的 USN 游标基线（MFT 重建路径表 {} 条）",
                    stats.matched
                );
                crate::meta::save_usn_cursor(index_dir, &vol, fresh_cursor);
                VolumeStart {
                    table,
                    journal_id: fresh_cursor.journal_id,
                    start_usn: fresh_cursor.next_usn,
                }
            }
        };
        starts.insert(vol, start);
    }

    Ok(starts)
}

/// 快车道的 live 监听：`bootstrap_fast_roots` 建好起始状态后调用。把
/// USN 游标持久化钩进 `on_progress`——每批 commit 成功后才把游标往前推
/// （"先 commit 索引、后写游标"，设计文档第四节），钩子本身很薄，实际的
/// 先后顺序保证来自 [`CursorSync`] 的设计（见 cursor.rs 的文档）。
pub(crate) fn run_fast_lane(
    index_dir: &std::path::Path,
    fast_roots: &[PathBuf],
    volume_starts: HashMap<VolumeKey, VolumeStart>,
    updater: Arc<Mutex<IndexUpdater>>,
    stop: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(crate::watch::WatchProgress) + Send + Sync>,
) -> Result<()> {
    let journal_ids: HashMap<VolumeKey, u64> = volume_starts
        .iter()
        .map(|(vol, start)| (vol.clone(), start.journal_id))
        .collect();

    let cursor_sync = Arc::new(CursorSync::new());
    let source = UsnEventSource::new(volume_starts, cursor_sync.clone());
    let index_dir = index_dir.to_path_buf();

    crate::watch::run_watch(source, fast_roots, updater, stop, move |progress| {
        match &progress {
            crate::watch::WatchProgress::Received(_) => cursor_sync.on_received(),
            crate::watch::WatchProgress::Committed { .. } => {
                for (volume, next_usn) in cursor_sync.on_committed() {
                    if let Some(&journal_id) = journal_ids.get(&volume) {
                        crate::meta::save_usn_cursor(
                            &index_dir,
                            &volume,
                            UsnCursor {
                                journal_id,
                                next_usn,
                            },
                        );
                    }
                }
            }
            _ => {}
        }
        on_progress(progress);
    })
}

/// 基于 USN Journal 的事件源：实现 [`EventSource`]/[`WatchGuard`]，接进现有
/// `run_watch` 流水线（设计文档"与现有架构的关系"一节："事件源只需产出
/// WatchEvent，防抖、合并、水位、增量更新全部复用"）。
pub(crate) struct UsnEventSource {
    volumes: Mutex<HashMap<VolumeKey, VolumeStart>>,
    cursor_sync: Arc<CursorSync>,
}

impl UsnEventSource {
    pub fn new(volumes: HashMap<VolumeKey, VolumeStart>, cursor_sync: Arc<CursorSync>) -> Self {
        Self {
            volumes: Mutex::new(volumes),
            cursor_sync,
        }
    }
}

impl EventSource for UsnEventSource {
    fn watch(&self, roots: &[PathBuf], tx: Sender<WatchEvent>) -> Result<Box<dyn WatchGuard>> {
        let mut by_volume: HashMap<VolumeKey, Vec<PathBuf>> = HashMap::new();
        for root in roots {
            if let Some(vol) = volume::volume_key(root) {
                by_volume.entry(vol).or_default().push(root.clone());
            }
        }

        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::new();
        let mut starts = self.volumes.lock().expect("usn volumes mutex poisoned");

        for (vol, vol_roots) in by_volume {
            let Some(start) = starts.remove(&vol) else {
                eprintln!("USN 事件源：{vol} 没有可用的起始状态（未做过 MFT 枚举/游标补账），跳过");
                continue;
            };
            let handle = SendHandle(
                volume::open_volume_handle(&vol)
                    .map_err(|e| anyhow::anyhow!("打开卷句柄失败 {vol}: {e}"))?,
            );
            let tx = tx.clone();
            let stop = stop.clone();
            let cursor_sync = self.cursor_sync.clone();
            let join = std::thread::spawn(move || {
                run_reader_loop(vol, vol_roots, handle, start, tx, stop, cursor_sync);
            });
            handles.push(join);
        }
        drop(starts);

        Ok(Box::new(UsnGuard { stop, handles }))
    }
}

fn run_reader_loop(
    vol: VolumeKey,
    roots: Vec<PathBuf>,
    handle: SendHandle,
    start: VolumeStart,
    tx: Sender<WatchEvent>,
    stop: Arc<AtomicBool>,
    cursor_sync: Arc<CursorSync>,
) {
    let handle = handle.0;
    let mut translator = UsnTranslator::new(start.table);
    let mut current_usn = start.start_usn;
    let mut buf = vec![0u8; READ_BUFFER_SIZE];
    // roots 目前只用来在未来扩展根粒度的多卷路由时留个钩子；USN 记录本身
    // 已经通过 FrnTable 的锚点判定了是否落在监听范围内，这里不需要再用
    // roots 做二次过滤——保留字段是为了让读取线程知道自己服务于哪些根
    // （调试日志用），避免看起来像"收了参数却不用"的死代码。
    let _ = &roots;

    while !stop.load(Ordering::Relaxed) {
        let batch = match read_journal_batch(handle, start.journal_id, current_usn, true, &mut buf)
        {
            Ok(b) => b,
            Err(err) => {
                eprintln!(
                    "USN Journal 读取出错（{vol}）：{err}，该卷监听退出，靠下次启动补账/对账兜底"
                );
                break;
            }
        };
        for record in batch.records {
            let record_usn = record.usn;
            let outcome = translator.translate(record);
            for event in outcome_to_events(outcome) {
                if cursor_sync
                    .record_and_send(vol.clone(), record_usn, event, &tx)
                    .is_err()
                {
                    // 接收端（run_watch 主循环）已经退出，读取线程也该收工了。
                    unsafe {
                        let _ = CloseHandle(handle);
                    }
                    return;
                }
            }
        }
        current_usn = batch.next_usn;
    }

    unsafe {
        let _ = CloseHandle(handle);
    }
}

struct UsnGuard {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl WatchGuard for UsnGuard {}

impl Drop for UsnGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}
