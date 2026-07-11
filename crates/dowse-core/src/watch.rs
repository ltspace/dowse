use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::event::{ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::events::{Debouncer, QUIET_WINDOW_MS, WatchEvent};
use crate::updater::{BatchOutcome, IndexUpdater};

/// 文件系统事件源抽象。里程碑 3 用 notify 做第一个实现；里程碑 6 的 NTFS
/// USN Journal 快速路径只要再实现这个 trait，就能接进同一条"防抖 → 更新"流水线，
/// 上层的 run_watch 完全不用改。
pub trait EventSource {
    /// 开始监听给定的根目录，把归一后的 WatchEvent 源源不断塞进 tx。
    /// 返回一个 guard——drop 掉它就停止监听（notify 的 watcher 正是这个语义）。
    fn watch(&self, roots: &[PathBuf], tx: Sender<WatchEvent>) -> Result<Box<dyn WatchGuard>>;
}

/// 监听句柄：留住它监听就继续，drop 掉就停。
pub trait WatchGuard: Send {}

/// 基于 notify 库的跨平台事件源。把各平台原生事件翻译成 WatchEvent。
pub struct NotifyEventSource;

impl EventSource for NotifyEventSource {
    fn watch(&self, roots: &[PathBuf], tx: Sender<WatchEvent>) -> Result<Box<dyn WatchGuard>> {
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<Event>| match res {
                Ok(event) => translate_event(&event, &tx),
                // 单个监听错误（某个子目录权限变化等）不该掀翻整条流水线，记日志继续。
                Err(err) => eprintln!("文件监听回调出错: {err}"),
            })
            .context("创建文件监听器失败")?;

        for root in roots {
            watcher
                .watch(root, RecursiveMode::Recursive)
                .with_context(|| format!("监听目录失败: {}", root.display()))?;
        }

        Ok(Box::new(NotifyGuard { _watcher: watcher }))
    }
}

/// 持有 notify watcher；drop 时 watcher 一起 drop，监听自动停止。
struct NotifyGuard {
    _watcher: notify::RecommendedWatcher,
}

impl WatchGuard for NotifyGuard {}

/// 把一条 notify 事件翻译成若干 WatchEvent 发进队列。
/// 这里做文件系统 IO（判断路径是文件还是目录、目录整体移入时展开子文件），
/// 属于事件源适配层；纯粹的防抖/合并逻辑在 events.rs 里，不碰 IO。
fn translate_event(event: &Event, tx: &Sender<WatchEvent>) {
    match &event.kind {
        EventKind::Create(_) => {
            for p in &event.paths {
                emit_upsert(p, tx);
            }
        }
        EventKind::Modify(ModifyKind::Name(mode)) => {
            translate_rename(*mode, &event.paths, tx);
        }
        // 内容/元数据修改：当作 upsert（先删后加）。
        EventKind::Modify(_) => {
            for p in &event.paths {
                emit_upsert(p, tx);
            }
        }
        EventKind::Remove(kind) => {
            for p in &event.paths {
                emit_remove(*kind, p, tx);
            }
        }
        // Any 是 notify 的"不精确"兜底：尽力而为——还在就当改，没了就当删。
        EventKind::Any => {
            for p in &event.paths {
                emit_best_effort(p, tx);
            }
        }
        // Access（打开/关闭/执行）与索引无关；Other 是无法表达的已知类型，
        // 两者都没有可靠的语义，忽略。
        EventKind::Access(_) | EventKind::Other => {}
    }
}

/// 一个路径的新增/修改：目录就发一个 UpsertDir 标记，文件就直接 upsert。
/// 目录整体被移入或复制进监听范围时，notify 只报一个目录事件，得自己下钻——
/// 但下钻的完整 walk **不能**在这里做：这里是 notify 的回调线程，大目录的
/// walk 阻塞到秒级会让 OS 的目录变更缓冲（Windows 上是 ReadDirectoryChangesW
/// 的内核缓冲区，容量有限）来不及被消费而溢出丢事件。真正的展开挪到消费侧
/// 线程（`IndexUpdater::apply` 处理 `PendingOp::UpsertTree` 时）去做。
fn emit_upsert(path: &Path, tx: &Sender<WatchEvent>) {
    if path.is_dir() {
        let _ = tx.send(WatchEvent::UpsertDir(path.to_path_buf()));
    } else {
        // 文件已不在（建了又秒删）也照发 Upsert：更新器会先删后发现无内容可加，
        // 等价于一次删除，幂等无害。
        let _ = tx.send(WatchEvent::Upsert(path.to_path_buf()));
    }
}

/// 一个路径的删除。notify 能分清文件/目录时精确处理；分不清（Any/Other，路径已删
/// 没法 stat）就发一条 RemoveDir——它在更新器里是"删这个 path 本身 + 前缀圈选删子树"，
/// 文件和目录两种情形一并覆盖。**不能**再补发一条精确 Remove：同一 path 的两条事件
/// 会在防抖队列里按 path 合并、后者覆盖前者，反而漏删。
fn emit_remove(kind: RemoveKind, path: &Path, tx: &Sender<WatchEvent>) {
    match kind {
        RemoveKind::File => {
            let _ = tx.send(WatchEvent::Remove(path.to_path_buf()));
        }
        // Folder / Any / Other 一律走 RemoveDir（= 删自身 + 删子树），一条搞定。
        _ => {
            let _ = tx.send(WatchEvent::RemoveDir(path.to_path_buf()));
        }
    }
}

/// 重命名的翻译。Both 一次给出 from/to；From/To 分两次给。
fn translate_rename(mode: RenameMode, paths: &[PathBuf], tx: &Sender<WatchEvent>) {
    match mode {
        RenameMode::Both if paths.len() == 2 => {
            let (from, to) = (&paths[0], &paths[1]);
            if to.is_dir() {
                // 目录改名：旧路径整棵前缀删，新路径下钻重新收录。
                let _ = tx.send(WatchEvent::RemoveDir(from.to_path_buf()));
                emit_upsert(to, tx);
            } else {
                let _ = tx.send(WatchEvent::Rename {
                    from: from.to_path_buf(),
                    to: to.to_path_buf(),
                });
            }
        }
        // 只有旧名（移出监听范围/改名的前半）：当删除。文件/目录分不清，发一条
        // RemoveDir（删自身 + 删子树）即可，别再补 Remove（同 path 会被合并覆盖）。
        RenameMode::From => {
            for p in paths {
                let _ = tx.send(WatchEvent::RemoveDir(p.to_path_buf()));
            }
        }
        // 只有新名（移入监听范围/改名的后半）：当新增。
        RenameMode::To => {
            for p in paths {
                emit_upsert(p, tx);
            }
        }
        // Both 但路径数不是 2，或 Any/Other：尽力而为。
        _ => {
            if paths.len() == 2 {
                translate_rename(RenameMode::Both, paths, tx);
            } else {
                for p in paths {
                    emit_best_effort(p, tx);
                }
            }
        }
    }
}

/// 分不清事件类型时的兜底：路径还在就当 upsert，没了就当删除（文件+子树都删）。
fn emit_best_effort(path: &Path, tx: &Sender<WatchEvent>) {
    if path.exists() {
        emit_upsert(path, tx);
    } else {
        // 一条 RemoveDir 覆盖文件+目录两种情形（见 emit_remove 的说明）。
        let _ = tx.send(WatchEvent::RemoveDir(path.to_path_buf()));
    }
}

/// 监听循环对外汇报的进度，给 CLI 打日志、给托盘端刷新状态用。
#[derive(Debug, Clone)]
pub enum WatchProgress {
    /// 收到一个原始事件（防抖前）。
    Received(WatchEvent),
    /// 一批防抖后的变更已提交入索引。
    Committed {
        batch_size: usize,
        outcome: BatchOutcome,
    },
    /// 提交失败，这批已退回队列、下个窗口重试。
    CommitFailed(String),
}

/// 监听主循环：把事件源 → 防抖队列 → 增量更新器串起来，阻塞运行直到 stop 置位。
///
/// - 500ms 静默窗口由 `recv_timeout` 驱动：每来一个事件就重置等待，静默满 500ms
///   才刷一批；事件持续不断时靠水位（5000）强制刷批，不会无限攒着。
/// - 一批一次 commit（commit 是重操作）。提交失败就把这批退回队列下轮重试。
/// - stop 每个窗口 tick（≤500ms）检查一次，所以 Ctrl+C / 退出能及时生效。
///
/// updater 用 `Arc<Mutex<_>>` 传入，好和启动对账共用同一个 writer；搜索侧是独立
/// reader，监听运行时索引照常可搜。
pub fn run_watch(
    source: impl EventSource,
    roots: &[PathBuf],
    updater: Arc<Mutex<IndexUpdater>>,
    stop: Arc<AtomicBool>,
    mut on_progress: impl FnMut(WatchProgress),
) -> Result<()> {
    let (tx, rx) = std::sync::mpsc::channel::<WatchEvent>();
    // guard 留到函数结束：drop 掉它 notify 才停。
    let _guard = source.watch(roots, tx)?;

    let mut debouncer = Debouncer::new(roots.to_vec());
    let window = Duration::from_millis(QUIET_WINDOW_MS);

    while !stop.load(Ordering::Relaxed) {
        match rx.recv_timeout(window) {
            Ok(event) => {
                on_progress(WatchProgress::Received(event.clone()));
                // 水位到了就立刻刷，不等静默窗口
                if debouncer.push(event) {
                    flush_batch(&mut debouncer, &updater, &mut on_progress);
                }
            }
            // 静默窗口到期：有攒着的就刷一批
            Err(RecvTimeoutError::Timeout) => {
                flush_batch(&mut debouncer, &updater, &mut on_progress);
            }
            // 事件源侧关闭了（watcher 出错退出等）：收尾退出
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    // 退出前把残留的刷掉，别把最后一批丢了
    flush_batch(&mut debouncer, &updater, &mut on_progress);
    Ok(())
}

/// 排干防抖队列、提交一批。空批直接返回。提交失败把这批退回队列下轮重试。
fn flush_batch(
    debouncer: &mut Debouncer,
    updater: &Mutex<IndexUpdater>,
    on_progress: &mut dyn FnMut(WatchProgress),
) {
    if debouncer.is_empty() {
        return;
    }
    let batch = debouncer.drain();
    let batch_size = batch.len();

    let mut guard = updater.lock().unwrap_or_else(|e| e.into_inner());
    match guard.apply(&batch) {
        Ok(outcome) => {
            drop(guard);
            on_progress(WatchProgress::Committed {
                batch_size,
                outcome,
            });
        }
        Err(err) => {
            drop(guard);
            // 本批退回队列，下个窗口再试；期间新来的同 path 事件优先。
            debouncer.requeue(batch);
            on_progress(WatchProgress::CommitFailed(err.to_string()));
        }
    }
}

/// 按卷判定自动选监听路径的入口（里程碑 6）：每个根探测 NTFS + 管理员权限，
/// 拿得到就走 MFT/USN 快车道，拿不到就走现有的 walkdir + notify 慢车道
/// （设计文档第一节）。CLI 的 `dowse watch` 和托盘常驻程序的监听线程都应该
/// 调这个函数替代直接调 `run_watch(NotifyEventSource, ...)`——这是"上层
/// 完全不用感知快慢车道差别"在代码上的落地：调用方只需要把原来的
/// `run_watch(NotifyEventSource, &roots, ...)` 换成
/// `watch_roots_auto(index_dir, &roots, ...)`，其余逻辑不用动。
///
/// 一台机器可能同时有走得快的盘和走不了快车道的盘（比如 C 盘有管理员权限、
/// U 盘是 FAT32），两条车道会分别起监听、共用同一个 `updater`/`stop`。
/// 只有慢车道（最常见：非管理员运行）时完全复用里程碑 3 的老路径，
/// 不引入任何新线程/新开销。
///
/// `on_progress` 要求 `Fn`（不是 `FnMut`）+ `Send + Sync`：混合车道场景下
/// 快慢两条车道各自在自己的线程里调用它，需要能从多个线程并发调用。
pub fn watch_roots_auto(
    index_dir: &Path,
    roots: &[PathBuf],
    updater: Arc<Mutex<IndexUpdater>>,
    stop: Arc<AtomicBool>,
    on_progress: impl Fn(WatchProgress) + Send + Sync + 'static,
) -> Result<()> {
    // 孤儿文档清理（多根索引，里程碑 7）：先于按根对账跑一次，兜底"添加根"/
    // "移除根"中途崩溃残留的、不属于当前任何注册根的文档（见
    // `reconcile::reconcile_orphans` 的文档）。用完整的 roots 列表（不区分
    // 快慢车道）跑一次即可，孤儿判定跟"这个根走不走 MFT 快速路径"无关。
    {
        let mut guard = updater.lock().unwrap_or_else(|e| e.into_inner());
        if let Err(err) = crate::reconcile::reconcile_orphans(roots, &mut guard) {
            eprintln!("孤儿文档清理失败: {err}");
        }
    }

    let mut fast_roots = Vec::new();
    let mut slow_roots = Vec::new();
    for root in roots {
        match crate::volume::probe_root_capability(root) {
            crate::volume::RootCapability::Fast { .. } => fast_roots.push(root.clone()),
            crate::volume::RootCapability::Fallback { .. } => slow_roots.push(root.clone()),
        }
    }

    #[cfg(windows)]
    let volume_starts = if fast_roots.is_empty() {
        None
    } else {
        match crate::usn::bootstrap_fast_roots(index_dir, &fast_roots, &updater) {
            Ok(starts) => Some(starts),
            Err(err) => {
                // 诚实降级不只是"探测阶段没权限就走慢车道"：真正 bootstrap
                // 时才暴露的问题（比如探测和 bootstrap 之间权限被收回）
                // 也不该让整个监听起不来——把这些根并入慢车道，这次运行
                // 整体退回 walkdir + notify，下次启动再重新探测。
                eprintln!("快速路径初始化失败，本次运行整体退回 walkdir + notify: {err}");
                slow_roots.append(&mut fast_roots);
                None
            }
        }
    };
    #[cfg(not(windows))]
    let volume_starts: Option<()> = None;

    if fast_roots.is_empty() || volume_starts.is_none() {
        for root in &slow_roots {
            let mut guard = updater.lock().unwrap_or_else(|e| e.into_inner());
            if let Err(err) = crate::reconcile::reconcile(root, &mut guard) {
                eprintln!("启动对账 {} 失败: {err}", root.display());
            }
        }
        return run_watch(NotifyEventSource, &slow_roots, updater, stop, move |p| {
            on_progress(p)
        });
    }

    // 非 Windows 平台上 volume_starts 恒为 None（见上面的 #[cfg(not(windows))]
    // 分支），所以上面这个 if 恒真，一定会在到达这里之前 return——这条
    // unreachable! 纯粹是给编译器一个 `Result<()>` 类型的桩，让函数在非
    // Windows 平台上也能完成类型检查，不代表这条路径真的会被执行到。
    #[cfg(not(windows))]
    unreachable!("非 Windows 平台上面的 if 恒真，已在此之前 return");

    #[cfg(windows)]
    {
        let on_progress = Arc::new(on_progress);
        let slow_handle = if slow_roots.is_empty() {
            None
        } else {
            let slow_roots = slow_roots.clone();
            let updater = updater.clone();
            let stop = stop.clone();
            let on_progress = on_progress.clone();
            Some(std::thread::spawn(move || {
                for root in &slow_roots {
                    let mut guard = updater.lock().unwrap_or_else(|e| e.into_inner());
                    if let Err(err) = crate::reconcile::reconcile(root, &mut guard) {
                        eprintln!("启动对账 {} 失败: {err}", root.display());
                    }
                }
                let on_progress = on_progress.clone();
                if let Err(err) =
                    run_watch(NotifyEventSource, &slow_roots, updater, stop, move |p| {
                        on_progress(p)
                    })
                {
                    eprintln!("慢车道监听退出: {err}");
                }
            }))
        };

        // 留一份 fast_roots 自己的克隆：run_fast_lane 只在"连启动都失败"时才
        // 返回 Err（正常运行期间碰到的单卷读取错误在 usn.rs 内部自己 eprintln
        // 后收工，不会让这个 Result 变成 Err）——一旦真的启动失败，之前的写法
        // 是原样把这个 Err 返回给调用方，同时下面还要等 slow_handle join 完才
        // 真正返回：如果这次运行本来就没有慢车道根（slow_handle 是 None），
        // 调用方在此期间完全拿不到任何反馈，而这些快车道根从此在本次运行里
        // 彻底没人监听，直到外部调用方察觉到 Err、决定要不要重启整个监听——
        // 这段"错误静默 + 快车道卷失聪"的空窗期不该存在。
        let updater_for_fallback = updater.clone();
        let stop_for_fallback = stop.clone();
        let on_progress_for_fallback = on_progress.clone();
        let fast_roots_for_fallback = fast_roots.clone();

        let result = crate::usn::run_fast_lane(
            index_dir,
            &fast_roots,
            volume_starts.expect("上面已经判空"),
            updater,
            stop,
            on_progress,
        );

        let result = match result {
            Ok(()) => Ok(()),
            Err(err) => {
                // 立即日志 + 把这几个根并入慢车道继续跑本次运行，而不是把 Err
                // 一路扣到 slow_handle.join() 之后才让调用方知道——这段时间里
                // 这些根本来会完全没人监听。下次启动时仍然会重新探测，重新
                // 尝试快车道，这里只影响"这一次运行"。
                eprintln!("快车道启动失败，本次运行把这些根并入慢车道继续: {err}");
                for root in &fast_roots_for_fallback {
                    let mut guard = updater_for_fallback
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Err(err) = crate::reconcile::reconcile(root, &mut guard) {
                        eprintln!("启动对账 {} 失败: {err}", root.display());
                    }
                }
                run_watch(
                    NotifyEventSource,
                    &fast_roots_for_fallback,
                    updater_for_fallback,
                    stop_for_fallback,
                    move |p| on_progress_for_fallback(p),
                )
            }
        };

        if let Some(handle) = slow_handle {
            let _ = handle.join();
        }
        result
    }
}
