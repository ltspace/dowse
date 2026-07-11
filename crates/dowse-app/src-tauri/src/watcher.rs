use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use dowse_core::{DEFAULT_WORKERS, IndexUpdater, OcrPipeline, watch_roots_auto};

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

/// 后台线程主体：开写入端 → 挂"自动选路径"的监听（内部先做启动对账/游标
/// 补账，再转实时监听），阻塞到 stop。
///
/// 启动对账这一步（里程碑 3 是"每个根 mtime 全扫"，里程碑 6 起改成
/// "按卷探测，有游标就补账追平、没有才全扫"）现在整个下沉到
/// `watch_roots_auto` 内部——它对每个根按卷判定快慢车道，快车道走 MFT
/// 重建路径表 + 游标补账，慢车道还是老的 mtime 全扫，两条路径产出的效果
/// 对调用方是一样的，这个函数不用再关心"该对账还是该补账"这种细节。
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

    // 常驻程序不赶时间：图片交给独立的后台 worker 池慢慢消化，跟文本监听并行
    // 跑、互不阻塞。没有可用语言包时返回 None，只打一行日志，不影响文本监听。
    let ocr_pipeline = OcrPipeline::start(updater.clone(), index_dir.clone(), DEFAULT_WORKERS);

    // 挂实时监听，阻塞到 stop 置位。提交后 Searcher 的 reader 自动重载，
    // 所以不用往前端推事件，搜索结果自然就追上了。
    if let Err(err) = watch_roots_auto(&index_dir, &roots, updater, stop, |_progress| {}) {
        eprintln!("文件监听退出: {err}");
    }

    // 文本监听停了，OCR 管线也跟着收尾——两者共用同一个后台线程的生命周期，
    // WatchController::stop() 的 join() 会等到这里才返回，保证重建索引前
    // OCR worker 也已经放掉了索引写入端的句柄。
    if let Some(pipeline) = ocr_pipeline {
        pipeline.stop();
    }
}
