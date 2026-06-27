//! 全量重建的共享实现：浮窗"选目录建索引"按钮、托盘"重建索引"、托盘
//! "更改索引文件夹…" 三个入口都走 `perform_rebuild`，保证进度事件/状态更新/
//! 托盘 tooltip/搜索状态切换的行为完全一致，不会出现"这个入口忘了更新
//! 某一处状态"的偏差（症状 5：选完目录之后要能看得见、改得了）。

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager};

use crate::config::ConfigState;
use crate::indexing_status::IndexingStatus;
use crate::state::SearchState;
use crate::watcher::WatchController;

/// 防止"重建索引"/"更改索引文件夹"/浮窗按钮三个入口并发触发重建——全量重建
/// 期间旧索引目录会被删掉重建，重叠执行会互相踩踏（Windows 删目录、tantivy
/// 写入端都不是可重入的）。
pub struct RebuildGuard(AtomicBool);

impl RebuildGuard {
    pub fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// 尝试拿到独占重建权，已经有一次在跑就返回 false（调用方据此提示用户
    /// "已有一次建索引在进行中"，而不是让两次重建互相踩踏）。
    pub fn try_begin(&self) -> bool {
        self.0
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn end(&self) {
        self.0.store(false, Ordering::Release);
    }
}

#[derive(Serialize, Clone)]
pub struct IndexProgressDto {
    pub processed: usize,
    pub path: String,
}

#[derive(Serialize)]
pub struct IndexStatsDto {
    pub indexed: usize,
    pub skipped: usize,
    pub seconds: f64,
    /// 建索引期间发现、还没识别完的图片数——OCR 是独立的后台低优先级管线，
    /// 全量重建结束时这些图片大概率还在排队。前端不再只拿它当一次性快照
    /// 展示：`dowse://ocr-progress` 事件 + `indexing_status` 查询命令会在
    /// 队列消化过程中持续刷新这个数字（v0.6.1 之前它是静态的，永远不变）。
    pub ocr_pending: usize,
}

/// 移除根的结果，托盘"移除"动作用；跟 `IndexStatsDto` 分开成一份独立的
/// DTO——移除没有"收录/跳过"这两个概念，硬凑共用字段会让字段名词不达意。
#[derive(Serialize, Clone, Copy)]
pub struct RemoveRootStatsDto {
    pub removed: usize,
}

/// 千分位分隔，托盘 tooltip/菜单文案里数字过万时更易读（"15,287"）。
pub fn format_count(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().rev().enumerate() {
        if i != 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

/// 全量重建的完整流程：停旧监听 → 建索引（文本阶段，进度实时推给前端/托盘）
/// → 换新 Searcher → 记住目标目录 → 重新挂监听（含 OCR 后台管线，OCR 阶段
/// 进度接续推送）。调用方负责 `RebuildGuard` 的独占权（本函数不重入判断），
/// 失败时把 `IndexingStatus`/托盘状态都清干净，不留半截进度。
pub fn perform_rebuild(app: &AppHandle, target: PathBuf) -> Result<IndexStatsDto, String> {
    let index_dir = crate::config::index_dir().map_err(|e| e.to_string())?;

    app.state::<WatchController>().stop();
    app.state::<IndexingStatus>().begin_text();
    crate::tray::set_busy(app, true);
    crate::tray::refresh_tooltip(app);

    let app_for_progress = app.clone();
    let rebuild_result =
        dowse_core::rebuild_index_with_progress(&index_dir, &target, move |progress| {
            let display_path = dowse_core::display_path(&progress.path.to_string_lossy());
            app_for_progress
                .state::<IndexingStatus>()
                .set_text_progress(progress.processed, display_path.clone());
            let _ = app_for_progress.emit(
                "dowse://rebuild-progress",
                IndexProgressDto {
                    processed: progress.processed,
                    path: display_path,
                },
            );
            crate::tray::refresh_tooltip(&app_for_progress);
        });

    let stats = match rebuild_result {
        Ok(stats) => stats,
        Err(err) => {
            app.state::<IndexingStatus>().reset_idle();
            crate::tray::set_busy(app, false);
            crate::tray::refresh_tooltip(app);
            return Err(err.to_string());
        }
    };

    // 在 watch.start 挪走 index_dir 之前先问一次 OCR 队列——两者用的是同一个
    // index_dir，问完这次调用就不再需要它了。
    let ocr_pending = dowse_core::OcrQueue::for_index_dir(&index_dir).pending_len();

    let new_searcher = match dowse_core::Searcher::open(&index_dir) {
        Ok(searcher) => searcher,
        Err(err) => {
            app.state::<IndexingStatus>().reset_idle();
            crate::tray::set_busy(app, false);
            crate::tray::refresh_tooltip(app);
            return Err(err.to_string());
        }
    };
    app.state::<SearchState>().replace(new_searcher);
    let _ = app.state::<ConfigState>().set_target_dir(target.clone());

    // 重建完盯住新索引根，重新挂上"对账 + 实时监听"（含 OCR 后台管线）。
    app.state::<WatchController>()
        .start(app.clone(), index_dir, vec![target]);

    app.state::<IndexingStatus>().begin_ocr(ocr_pending);
    crate::tray::set_busy(app, false);
    crate::tray::refresh_menu(app);
    crate::tray::refresh_tooltip(app);

    Ok(IndexStatsDto {
        indexed: stats.indexed,
        skipped: stats.skipped,
        seconds: stats.seconds,
        ocr_pending,
    })
}

/// 添加/移除根失败时的收尾：建索引状态回 idle、解除托盘忙碌态，用现有（没被
/// 这次失败操作动过）的 roots 把常驻监听接回去——不能因为一次失败（最常见
/// 就是嵌套校验没过）就让整个应用停摆监听。验收清单第 3 条"拒绝且提示清晰"
/// 隐含的要求是：拒绝之后应用其它一切照常，不止是弹个错误提示那么简单。
///
/// 跟 `perform_rebuild` 的失败分支不共用这个收尾：全量重建失败时旧索引目录
/// 可能已经被删掉、新索引还没建完，此时重新挂监听没有意义（`registered_roots`
/// 大概率也读不到）；而添加/移除根从不删除现有索引，失败时 meta 里的 roots
/// 还是最后一次成功状态，重新挂监听是安全且必要的。
fn restart_watch_after_root_op(app: &AppHandle, index_dir: &Path) {
    if let Ok(roots) = dowse_core::registered_roots(index_dir) {
        app.state::<WatchController>()
            .start(app.clone(), index_dir.to_path_buf(), roots);
    }
}

fn fail_root_op<T>(app: &AppHandle, index_dir: &Path, err: String) -> Result<T, String> {
    app.state::<IndexingStatus>().reset_idle();
    crate::tray::set_busy(app, false);
    crate::tray::refresh_tooltip(app);
    restart_watch_after_root_op(app, index_dir);
    Err(err)
}

/// 添加一个根：跟 `perform_rebuild` 共用"停旧监听 → 操作 → 换新 Searcher →
/// 重新挂监听"的节奏和进度事件（`dowse://rebuild-progress`）/状态机制
/// （`IndexingStatus`），但操作本身走 `dowse_core::add_root_with_progress`——
/// 不删现有索引，只对新根做一次目录树 upsert（设计文档"核心操作语义"）。
///
/// 现开一个 `IndexUpdater::open`：`WatchController::stop()` 已经 join 完常驻
/// 监听线程，它那份 `IndexUpdater` 连同索引写入端句柄一起释放了，这里开一份
/// 新的不会跟谁抢锁（跟 `perform_rebuild` 让 `rebuild_index_with_progress`
/// 内部自己开写入端是同一个前提条件）。
pub fn perform_add_root(app: &AppHandle, target: PathBuf) -> Result<IndexStatsDto, String> {
    let index_dir = crate::config::index_dir().map_err(|e| e.to_string())?;
    let start = Instant::now();

    app.state::<WatchController>().stop();
    app.state::<IndexingStatus>().begin_text();
    crate::tray::set_busy(app, true);
    crate::tray::refresh_tooltip(app);

    let mut updater = match dowse_core::IndexUpdater::open(&index_dir) {
        Ok(updater) => updater,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };

    let app_for_progress = app.clone();
    let add_result =
        dowse_core::add_root_with_progress(&index_dir, &target, &mut updater, move |progress| {
            let display_path = dowse_core::display_path(&progress.path.to_string_lossy());
            app_for_progress
                .state::<IndexingStatus>()
                .set_text_progress(progress.processed, display_path.clone());
            let _ = app_for_progress.emit(
                "dowse://rebuild-progress",
                IndexProgressDto {
                    processed: progress.processed,
                    path: display_path,
                },
            );
            crate::tray::refresh_tooltip(&app_for_progress);
        });
    // 写入端用完立刻放掉——下面开只读 Searcher/重新挂监听都要求索引目录
    // 没有活着的 IndexWriter 占着。
    drop(updater);

    let stats = match add_result {
        Ok(stats) => stats,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };

    let ocr_pending = dowse_core::OcrQueue::for_index_dir(&index_dir).pending_len();

    let new_searcher = match dowse_core::Searcher::open(&index_dir) {
        Ok(searcher) => searcher,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };
    app.state::<SearchState>().replace(new_searcher);

    restart_watch_after_root_op(app, &index_dir);

    app.state::<IndexingStatus>().begin_ocr(ocr_pending);
    crate::tray::set_busy(app, false);
    crate::tray::refresh_menu(app);
    crate::tray::refresh_tooltip(app);

    Ok(IndexStatsDto {
        indexed: stats.indexed,
        skipped: stats.skipped,
        seconds: start.elapsed().as_secs_f64(),
        ocr_pending,
    })
}

/// 移除一个根：前缀圈选删文档 + OCR 队列 compact + roots 移除（设计文档
/// "核心操作语义"）。跟添加根共用同一套停监听/重挂监听节奏，但这是一次
/// 批量删除，没有"逐文件进度"可直播，不接 `dowse://rebuild-progress`。
pub fn perform_remove_root(app: &AppHandle, root: PathBuf) -> Result<RemoveRootStatsDto, String> {
    let index_dir = crate::config::index_dir().map_err(|e| e.to_string())?;

    app.state::<WatchController>().stop();
    crate::tray::set_busy(app, true);
    crate::tray::refresh_tooltip(app);

    let mut updater = match dowse_core::IndexUpdater::open(&index_dir) {
        Ok(updater) => updater,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };
    let remove_result = dowse_core::remove_root(&index_dir, &root, &mut updater);
    drop(updater);

    let stats = match remove_result {
        Ok(stats) => stats,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };

    let new_searcher = match dowse_core::Searcher::open(&index_dir) {
        Ok(searcher) => searcher,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };
    app.state::<SearchState>().replace(new_searcher);

    restart_watch_after_root_op(app, &index_dir);
    crate::tray::set_busy(app, false);
    crate::tray::refresh_menu(app);
    crate::tray::refresh_tooltip(app);

    Ok(RemoveRootStatsDto {
        removed: stats.removed,
    })
}

/// 重建单根 = 移除根 + 添加根的组合（设计文档"核心操作语义"），托盘每根
/// 子菜单的"重建"动作用。跟 `perform_add_root` 几乎一样的节奏，唯一区别是
/// 操作本身换成 `dowse_core::rebuild_root_with_progress`。
pub fn perform_rebuild_root(app: &AppHandle, root: PathBuf) -> Result<IndexStatsDto, String> {
    let index_dir = crate::config::index_dir().map_err(|e| e.to_string())?;
    let start = Instant::now();

    app.state::<WatchController>().stop();
    app.state::<IndexingStatus>().begin_text();
    crate::tray::set_busy(app, true);
    crate::tray::refresh_tooltip(app);

    let mut updater = match dowse_core::IndexUpdater::open(&index_dir) {
        Ok(updater) => updater,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };

    let app_for_progress = app.clone();
    let rebuild_result =
        dowse_core::rebuild_root_with_progress(&index_dir, &root, &mut updater, move |progress| {
            let display_path = dowse_core::display_path(&progress.path.to_string_lossy());
            app_for_progress
                .state::<IndexingStatus>()
                .set_text_progress(progress.processed, display_path.clone());
            let _ = app_for_progress.emit(
                "dowse://rebuild-progress",
                IndexProgressDto {
                    processed: progress.processed,
                    path: display_path,
                },
            );
            crate::tray::refresh_tooltip(&app_for_progress);
        });
    drop(updater);

    let stats = match rebuild_result {
        Ok(stats) => stats,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };

    let ocr_pending = dowse_core::OcrQueue::for_index_dir(&index_dir).pending_len();

    let new_searcher = match dowse_core::Searcher::open(&index_dir) {
        Ok(searcher) => searcher,
        Err(err) => return fail_root_op(app, &index_dir, err.to_string()),
    };
    app.state::<SearchState>().replace(new_searcher);

    restart_watch_after_root_op(app, &index_dir);

    app.state::<IndexingStatus>().begin_ocr(ocr_pending);
    crate::tray::set_busy(app, false);
    crate::tray::refresh_menu(app);
    crate::tray::refresh_tooltip(app);

    Ok(IndexStatsDto {
        indexed: stats.indexed,
        skipped: stats.skipped,
        seconds: start.elapsed().as_secs_f64(),
        ocr_pending,
    })
}
