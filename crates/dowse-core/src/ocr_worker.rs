//! OCR 后台 worker 池：[`OcrPipeline::start`] 启动几个线程持续从
//! [`crate::OcrQueue`] 取图片识别、批量写回索引（攒够一批或超过时间窗口才
//! commit 一次，避免逐张 commit 打爆磁盘 IO）。[`drain_ocr_queue`] 是一次性
//! 处理完当前排队图片的薄封装，给 CLI 等不需要常驻后台线程的场景用。

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
pub(crate) const MIN_WORKERS: usize = 2;
pub(crate) const MAX_WORKERS: usize = 4;

/// CLI/托盘启动 OCR 管线时的默认线程数。
pub const DEFAULT_WORKERS: usize = MAX_WORKERS;

/// 队列空时的轮询间隔：避免 worker 忙等吃满一个核心。OCR 是低优先级后台任务，
/// 晚 200ms 发现新入队的图片完全不影响"一分钟内可搜到"的验收标准。
const IDLE_POLL: Duration = Duration::from_millis(200);

/// OCR 结果写回索引的批次上限：攒够这么多张识别结果就提交一次，不用等到
/// 时间窗口。v0.6.1 之前是每识别完一张图就单独 commit 一次——15k 张图片
/// 对应 15k 次重量级 tantivy commit（每次都重写段元文件、可能触发合并），
/// 磁盘 IO 被打爆，现场表现为窗口唤起卡顿数秒、进程在高频建删文件时更容易
/// 撞上杀软实时扫描而崩溃。批量提交把 commit 次数从"每张一次"降到
/// "每 32 张（或 5 秒，先到者）一次"，量级上差两个数量级。
const OCR_BATCH_MAX: usize = 32;
/// 批次的时间窗口上限：哪怕图片识别得慢、迟迟凑不够 32 张，也不能让第一张
/// 识别完的结果在内存里等太久才落盘可搜——5 秒比设计文档"新增文件一分钟内
/// 可搜"的验收预算还有余量，同时足够把大多数正常吞吐下的一批 32 张
/// 图片自然凑满（约 30 张/秒的预算下一秒多就够一批）。
const OCR_BATCH_WINDOW: Duration = Duration::from_secs(5);

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
    ///
    /// `on_progress` 在每次一批识别结果成功写回索引后被调用一次，参数是调用
    /// 那一刻队列里还剩多少张待处理——前端"另有 N 张图片在后台识别"那行字
    /// 靠这个活起来（v0.6.1 之前这个数字是重建完成那一刻的静态快照，从不刷新）。
    /// 可能从多个 worker 线程并发调用，必须 `Send + Sync`；不需要进度回调的
    /// 调用方（`drain_ocr_queue`、CLI 的一次性场景）传 `|_| {}` 即可。
    pub fn start(
        updater: Arc<Mutex<IndexUpdater>>,
        index_dir: PathBuf,
        worker_count: usize,
        on_progress: impl Fn(usize) + Send + Sync + 'static,
    ) -> Option<Self> {
        if !ocr::is_available() {
            eprintln!("未检测到可用的 OCR 语言包，OCR 管线已停用（截图/图片文字不会被索引）");
            return None;
        }

        let queue = OcrQueue::for_index_dir(&index_dir);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_count = worker_count.clamp(MIN_WORKERS, MAX_WORKERS);
        let on_progress: Arc<dyn Fn(usize) + Send + Sync> = Arc::new(on_progress);

        let handles = (0..worker_count)
            .map(|_| {
                let updater = updater.clone();
                let queue = queue.clone();
                let stop = stop.clone();
                let on_progress = on_progress.clone();
                std::thread::spawn(move || worker_loop(queue, updater, stop, on_progress))
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
/// 图片、识别、清洗，识别结果先攒进本地批次，凑够 `OCR_BATCH_MAX` 张或者攒了
/// `OCR_BATCH_WINDOW` 才一次性写回索引（见批量提交的文档）。队列暂时空了就把
/// 手头的半批先落盘——不能让已经识别完的结果因为"凑不够一批"就在内存里等到
/// 下一张图片入队，那样队列长期偏空时最后几张图会迟迟搜不到。
fn worker_loop(
    queue: Arc<OcrQueue>,
    updater: Arc<Mutex<IndexUpdater>>,
    stop: Arc<AtomicBool>,
    on_progress: Arc<dyn Fn(usize) + Send + Sync>,
) {
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

    let mut batch: Vec<(PathBuf, i64, u64, String)> = Vec::with_capacity(OCR_BATCH_MAX);
    let mut batch_started: Option<Instant> = None;

    loop {
        match queue.pop_pending() {
            Some((path, mtime, size)) => {
                // 识别过程本身不持有 updater 的锁——这是"低优先级、不阻塞搜索"的
                // 关键：慢的是这一步（百毫秒级），写回索引只是攒批之后偶尔一次的
                // delete+add+commit。
                let content = match ocr::recognize(&engine, &path) {
                    Ok(raw) => ocr::dual_form_content(&raw),
                    Err(err) => {
                        eprintln!(
                            "OCR 识别出错，记为已处理（不重试，避免损坏图片反复卡住队列）{}: {err}",
                            path.display()
                        );
                        String::new()
                    }
                };

                if batch_started.is_none() {
                    batch_started = Some(Instant::now());
                }
                batch.push((path, mtime, size, content));

                let window_elapsed = batch_started
                    .map(|t| t.elapsed() >= OCR_BATCH_WINDOW)
                    .unwrap_or(false);
                if batch.len() >= OCR_BATCH_MAX || window_elapsed {
                    flush_batch(&mut batch, &queue, &updater, &on_progress);
                    batch_started = None;
                }
            }
            None => {
                // 队列暂时空了：先把手头的半批落盘，不要让已识别完的结果在内存里
                // 干等（可能一等就是很久，取决于下一张图片什么时候被发现入队）。
                if !batch.is_empty() {
                    flush_batch(&mut batch, &queue, &updater, &on_progress);
                    batch_started = None;
                }
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(IDLE_POLL);
                continue;
            }
        }

        if stop.load(Ordering::Relaxed) {
            // 停止前把手头还没提交的结果落盘，不要留着半批白白丢掉识别成果。
            if !batch.is_empty() {
                flush_batch(&mut batch, &queue, &updater, &on_progress);
            }
            return;
        }
    }
}

/// 把攒够的一批 OCR 结果写回索引：成功就把整批标记为已处理（连同内容缓存）；
/// 写入端撞上非瞬时错误、重试耗尽后仍然失败（`commit_with_retry` 已经在
/// `stage_and_commit_ocr_batch` 内部处理过瞬时的杀软扫描冲突），就把整批
/// 原样退回待处理队列，下次 worker 循环会重新识别——不能像旧实现那样
/// 无论提交成不成功都调用 `mark_processed`，那样一旦写入端失败，后续的每
/// 一张图片都会被静默标记"已处理"却从未真正写进索引，永久丢失识别结果而
/// 且没有任何报错信号。批次清空、进度回调调用一次，不管成功还是失败——
/// 失败时 `queue.pending_len()` 会因为整批退回而不降反升，如实反映现状。
fn flush_batch(
    batch: &mut Vec<(PathBuf, i64, u64, String)>,
    queue: &Arc<OcrQueue>,
    updater: &Mutex<IndexUpdater>,
    on_progress: &Arc<dyn Fn(usize) + Send + Sync>,
) {
    let items = std::mem::take(batch);
    if items.is_empty() {
        return;
    }

    let commit_result = {
        let mut guard = updater.lock().expect("updater mutex poisoned");
        guard.stage_and_commit_ocr_batch(&items)
    };

    match commit_result {
        Ok(()) => {
            for (path, mtime, size, content) in items {
                queue.mark_processed(path, mtime, size, content);
            }
        }
        Err(err) => {
            eprintln!(
                "OCR 批量结果写入索引失败，{} 张退回队列下次重试: {err}",
                items.len()
            );
            for (path, mtime, size, _content) in items {
                queue.enqueue(path, mtime, size);
            }
        }
    }

    if let Err(err) = queue.save() {
        eprintln!("OCR 队列进度落盘失败: {err}");
    }

    on_progress(queue.pending_len());
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
    let Some(pipeline) = OcrPipeline::start(updater, index_dir.to_path_buf(), worker_count, |_| {})
    else {
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
