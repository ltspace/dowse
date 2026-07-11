use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::ocr;
use crate::ocr_queue::OcrQueue;
use crate::updater::IndexUpdater;

/// worker 池线程数范围。设计文档的性能预算按 4 worker 估算（约 30 张/秒吞吐），
/// 2 是下限——低于这个数就没有"池"的意义了。
pub const MIN_WORKERS: usize = 2;
pub const MAX_WORKERS: usize = 4;

/// CLI/托盘启动 OCR 管线时的默认线程数。
pub const DEFAULT_WORKERS: usize = MAX_WORKERS;

/// 队列空时的轮询间隔：避免 worker 忙等吃满一个核心。OCR 是低优先级后台任务，
/// 晚 200ms 发现新入队的图片完全不影响"一分钟内可搜到"的验收标准。
const IDLE_POLL: Duration = Duration::from_millis(200);

/// OCR 后台管线句柄：持有 worker 线程，stop() 时等待它们干净退出（当前正在跑的
/// 那一张 OCR 会跑完，不会被中途打断留下半张写坏的文档）。
pub struct OcrPipeline {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl OcrPipeline {
    /// 启动 worker 池。系统没有任何 OCR 语言包时返回 None（打印一行日志，管线
    /// 整体不启动、不崩溃）——调用方据此可以让托盘/浮窗提示用户，而不是硬报错
    /// （设计文档"降级与错误处理"一节）。
    pub fn start(
        updater: Arc<Mutex<IndexUpdater>>,
        index_dir: PathBuf,
        worker_count: usize,
    ) -> Option<Self> {
        if !ocr::is_available() {
            eprintln!("未检测到可用的 OCR 语言包，OCR 管线已停用（截图/图片文字不会被索引）");
            return None;
        }

        let queue = OcrQueue::for_index_dir(&index_dir);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_count = worker_count.clamp(MIN_WORKERS, MAX_WORKERS);

        let handles = (0..worker_count)
            .map(|_| {
                let updater = updater.clone();
                let queue = queue.clone();
                let stop = stop.clone();
                std::thread::spawn(move || worker_loop(queue, updater, stop))
            })
            .collect();

        Some(Self { stop, handles })
    }

    /// 通知所有 worker 停止、并等它们退出。每个 worker 手头正在跑的那一次识别
    /// 会跑完（不会被中途打断），退出前各自把队列进度落一次盘。
    pub fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles {
            let _ = handle.join();
        }
    }
}

/// 单个 worker 的主循环：独占创建一个 OcrEngine（绝不跨线程共享），不断从队列取
/// 图片、识别、清洗、写回索引，队列空了就小睡一下避免忙等。
fn worker_loop(queue: Arc<OcrQueue>, updater: Arc<Mutex<IndexUpdater>>, stop: Arc<AtomicBool>) {
    let engine = match ocr::create_engine() {
        Ok(engine) => engine,
        Err(err) => {
            // OcrPipeline::start 已经用 is_available() 探测过一次，这里理论上不该失败；
            // 真发生了（比如探测和创建之间语言包被卸载）就让这个 worker 直接退出，
            // 其余 worker 不受影响，不 panic 整个进程。
            eprintln!("OCR worker 创建引擎失败，该 worker 退出: {err}");
            return;
        }
    };

    loop {
        let Some((path, mtime, size)) = queue.pop_pending() else {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(IDLE_POLL);
            continue;
        };

        // 识别过程本身不持有 updater 的锁——这是"低优先级、不阻塞搜索"的关键：
        // 慢的是这一步（百毫秒级），写回索引只是一次快速的 delete+add+commit。
        let content = match ocr::recognize(&engine, &path) {
            Ok(raw) => ocr::dual_form_content(&raw),
            Err(err) => {
                eprintln!(
                    "OCR 识别出错，跳过并记为已处理（不重试）{}: {err}",
                    path.display()
                );
                String::new()
            }
        };

        {
            let mut guard = updater.lock().expect("updater mutex poisoned");
            if let Err(err) = guard.stage_and_commit_ocr_result(&path, mtime, size, &content) {
                eprintln!("OCR 结果写入索引失败 {}: {err}", path.display());
            }
        }
        queue.mark_processed(path, mtime, size, content);
        if let Err(err) = queue.save() {
            eprintln!("OCR 队列进度落盘失败: {err}");
        }

        if stop.load(Ordering::Relaxed) {
            return;
        }
    }
}

/// `drain_ocr_queue` 一次运行的统计结果。
#[derive(Debug, Clone, Copy, Default)]
pub struct OcrDrainStats {
    /// 系统是否有可用的 OCR 语言包；false 时下面的 processed 恒为 0，不算错误。
    pub available: bool,
    /// 本次实际处理掉的图片数。
    pub processed: usize,
}

/// 单次调用最多等这么久：避免一张损坏图片卡住识别、把 `dowse index` 挂死。
/// 超时后照样把已经处理完的部分落盘退出，剩下的留给下次 `dowse watch`/托盘常驻消化。
const DRAIN_TIMEOUT: Duration = Duration::from_secs(60);

/// 同步跑完 OCR 队列直到清空（`dowse index` 这类一次性命令用）：建完文本索引后
/// 紧接着启动 worker 池处理图片，直到队列见底再返回，让命令结束时索引就是完整
/// 可搜的状态，而不是留一堆图片在后台"回头再说"——常驻的托盘程序不需要这个，
/// 那边图片交给后台 worker 池慢慢消化就行（见设计文档"独立于文本管线"一节）。
pub fn drain_ocr_queue(index_dir: &std::path::Path, worker_count: usize) -> Result<OcrDrainStats> {
    if !ocr::is_available() {
        return Ok(OcrDrainStats {
            available: false,
            processed: 0,
        });
    }

    let queue = OcrQueue::for_index_dir(index_dir);
    let before = queue.pending_len();
    if before == 0 {
        return Ok(OcrDrainStats {
            available: true,
            processed: 0,
        });
    }

    let updater = Arc::new(Mutex::new(IndexUpdater::open(index_dir)?));
    let Some(pipeline) = OcrPipeline::start(updater, index_dir.to_path_buf(), worker_count) else {
        return Ok(OcrDrainStats {
            available: false,
            processed: 0,
        });
    };

    let start = Instant::now();
    while queue.pending_len() > 0 && start.elapsed() < DRAIN_TIMEOUT {
        std::thread::sleep(Duration::from_millis(50));
    }
    let remaining = queue.pending_len();
    pipeline.stop();

    Ok(OcrDrainStats {
        available: true,
        processed: before.saturating_sub(remaining),
    })
}
