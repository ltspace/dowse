use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use anyhow::{Context, Result};
use notify::event::{ModifyKind, RemoveKind, RenameMode};
use notify::{Event, EventKind, RecursiveMode, Watcher};

use crate::events::WatchEvent;
use crate::indexer::walk_index_files;

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

/// 一个路径的删除。notify 能分清文件/目录时精确处理；分不清（Any/Other，
/// 而且路径已删没法 stat）就两条都发——删单文件文档 + 前缀圈选删子树，
/// 都是幂等的，删不存在的 term 是空操作。
fn emit_remove(kind: RemoveKind, path: &Path, tx: &Sender<WatchEvent>) {
    match kind {
        RemoveKind::Folder => {
            let _ = tx.send(WatchEvent::RemoveDir(path.to_path_buf()));
        }
        RemoveKind::File => {
            let _ = tx.send(WatchEvent::Remove(path.to_path_buf()));
        }
        RemoveKind::Any | RemoveKind::Other => {
            let _ = tx.send(WatchEvent::Remove(path.to_path_buf()));
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
        // 只有旧名（移出监听范围/改名的前半）：当删除。文件/目录分不清就两条都发。
        RenameMode::From => {
            for p in paths {
                let _ = tx.send(WatchEvent::Remove(p.to_path_buf()));
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
        let _ = tx.send(WatchEvent::Remove(path.to_path_buf()));
        let _ = tx.send(WatchEvent::RemoveDir(path.to_path_buf()));
    }
}
