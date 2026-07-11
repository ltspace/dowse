use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use notify::event::{ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::events::{Debouncer, WatchEvent, QUIET_WINDOW_MS};
use crate::indexer::walk_index_files;
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
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| match res {
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

/// 一个路径的新增/修改：目录就展开成其下每个文件的 upsert，文件就直接 upsert。
/// 目录整体被移入或复制进监听范围时，notify 只报一个目录事件，得自己下钻。
fn emit_upsert(path: &Path, tx: &Sender<WatchEvent>) {
    if path.is_dir() {
        for file in walk_index_files(path) {
            let _ = tx.send(WatchEvent::Upsert(file));
        }
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

    let mut guard = updater.lock().expect("updater mutex poisoned");
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
