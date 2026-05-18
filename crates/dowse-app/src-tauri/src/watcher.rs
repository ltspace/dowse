use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use dowse_core::{IndexUpdater, NotifyEventSource, reconcile, run_watch};

/// 常驻文件监听的启停控制器。托盘常驻程序启动时先对账、再挂实时监听。
///
/// 重建索引前**必须**先 `stop()`：它会置停止位并 join 后台线程，等 IndexWriter
/// 连同索引目录的文件句柄一起放掉——否则 Windows 上 `remove_dir_all` 删不掉正被
/// 占用的旧索引目录。重建完再 `start()` 盯住新索引根。
#[derive(Default)]
pub struct WatchController {
    inner: Mutex<Option<Running>>,
}

struct Running {
    stop: Arc<AtomicBool>,
    handle: JoinHandle<()>,
}

impl WatchController {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(None),
        }
    }

    /// 启动"对账 + 实时监听"后台线程。已在跑就先停旧的（幂等）；roots 为空则什么都不做。
    pub fn start(&self, index_dir: PathBuf, roots: Vec<PathBuf>) {
        self.stop();
        if roots.is_empty() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let stop_for_thread = stop.clone();
        let handle = std::thread::spawn(move || {
            watch_thread(index_dir, roots, stop_for_thread);
        });
        *self.inner.lock().expect("watch controller mutex poisoned") =
            Some(Running { stop, handle });
    }

    /// 停止监听并等后台线程退出，确保之后能安全删除/重建索引目录。幂等：没在跑就是空操作。
    pub fn stop(&self) {
        let running = self
            .inner
            .lock()
            .expect("watch controller mutex poisoned")
            .take();
        if let Some(Running { stop, handle }) = running {
            stop.store(true, Ordering::Relaxed);
            let _ = handle.join();
        }
    }
}

/// 后台线程主体：开写入端 → 先对账 → 再挂实时监听（阻塞到 stop）。
fn watch_thread(index_dir: PathBuf, roots: Vec<PathBuf>, stop: Arc<AtomicBool>) {
    let updater = match IndexUpdater::open(&index_dir) {
        Ok(updater) => Arc::new(Mutex::new(updater)),
        Err(err) => {
            // 索引不存在/schema 需重建时开不了写入端：不启动监听，等用户重建后
            // 由 rebuild 流程再挂上。前端此时也会因 Searcher 打不开而引导重建。
            eprintln!("打开索引写入端失败，未启动监听（索引可能需要重建）: {err}");
            return;
        }
    };

    // 1) 先对账，补齐程序没开着时错过的增删改。就在这个后台线程里跑（低优先级语义）；
    //    搜索侧是独立 reader，对账进行时索引照常可搜、不被锁死。
    for root in &roots {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut guard = updater.lock().expect("updater mutex poisoned");
        match reconcile(root, &mut guard) {
            Ok(stats) => eprintln!(
                "启动对账 {}：新增 {} / 修改 {} / 删除 {}",
                root.display(),
                stats.added,
                stats.modified,
                stats.removed
            ),
            Err(err) => eprintln!("启动对账 {} 失败: {err}", root.display()),
        }
    }

    // 2) 挂实时监听，阻塞到 stop 置位。提交后 Searcher 的 reader 自动重载，
    //    所以不用往前端推事件，搜索结果自然就追上了。
    if let Err(err) = run_watch(NotifyEventSource, &roots, updater, stop, |_progress| {}) {
        eprintln!("文件监听退出: {err}");
    }
}
