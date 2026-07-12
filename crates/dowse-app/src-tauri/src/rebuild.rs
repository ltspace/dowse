//! 全量重建的共享实现：浮窗"选目录建索引"按钮、托盘"重建索引"、托盘
//! "更改索引文件夹…" 三个入口都走 `perform_rebuild`，保证进度事件/状态更新/
//! 托盘 tooltip/搜索状态切换的行为完全一致，不会出现"这个入口忘了更新
//! 某一处状态"的偏差（症状 5：选完目录之后要能看得见、改得了）。

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

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
    crate::tray::set_rebuilding(app, true);
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
            crate::tray::set_rebuilding(app, false);
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
            crate::tray::set_rebuilding(app, false);
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
    crate::tray::set_rebuilding(app, false);
    crate::tray::refresh_index_info(app);
    crate::tray::refresh_tooltip(app);

    Ok(IndexStatsDto {
        indexed: stats.indexed,
        skipped: stats.skipped,
        seconds: stats.seconds,
        ocr_pending,
    })
}
