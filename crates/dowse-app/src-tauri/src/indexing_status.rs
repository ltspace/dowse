use std::sync::Mutex;

use serde::Serialize;

/// 建索引流程当前处在哪个阶段。`Idle` 也是"没有在建索引"的常态。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum IndexingPhase {
    #[default]
    Idle,
    /// 全量重建的文本阶段：总量未知（走到哪算哪），只有"已处理数 + 当前文件"。
    Text,
    /// OCR 回填阶段：总量已知（文本阶段结束那一刻的图片队列长度，期间如果
    /// 常驻监听又发现新图片会顺势抬高），可以算出真实的完成比例。
    Ocr,
}

/// 建索引进度的一份快照，直接序列化给前端。窗口每次呼出都可以主动拉一次
/// 这份快照（见 `commands::indexing_status`），不用只靠事件流——事件在窗口
/// 隐藏期间照样会发，但前端没监听、也没地方存，重新唤出时必须能补一次。
#[derive(Debug, Clone, Serialize, Default)]
pub struct IndexingSnapshot {
    pub phase: IndexingPhase,
    pub text_processed: usize,
    pub text_current_file: String,
    pub ocr_processed: usize,
    pub ocr_total: usize,
}

/// 进程内常驻的建索引进度状态。写端是 `commands::rebuild_index`（文本阶段）
/// 和 OCR worker 池的进度回调（`watcher.rs`，OCR 阶段）；读端是
/// `commands::indexing_status`（前端窗口每次呼出时拉一次）和事件推送
/// （两条写路径各自顺手 `app.emit` 一次，供窗口开着时的实时刷新）。
pub struct IndexingStatus(Mutex<IndexingSnapshot>);

impl IndexingStatus {
    pub fn new() -> Self {
        Self(Mutex::new(IndexingSnapshot::default()))
    }

    pub fn snapshot(&self) -> IndexingSnapshot {
        self.0
            .lock()
            .expect("indexing status mutex poisoned")
            .clone()
    }

    /// 全量重建开始：清空上一轮的状态，进入文本阶段。
    pub fn begin_text(&self) {
        let mut guard = self.0.lock().expect("indexing status mutex poisoned");
        *guard = IndexingSnapshot {
            phase: IndexingPhase::Text,
            ..Default::default()
        };
    }

    /// 文本阶段的一次进度汇报：累计处理数 + 当前文件，节奏跟
    /// `dowse://rebuild-progress` 事件完全一致（同一处回调顺手更新两边）。
    pub fn set_text_progress(&self, processed: usize, current_file: String) {
        let mut guard = self.0.lock().expect("indexing status mutex poisoned");
        guard.phase = IndexingPhase::Text;
        guard.text_processed = processed;
        guard.text_current_file = current_file;
    }

    /// 文本阶段结束、准备进入 OCR 阶段：`total` 是这一刻 OCR 队列里还有多少张
    /// 图片没处理。0 张的话没有 OCR 阶段可言，直接回到 idle。
    pub fn begin_ocr(&self, total: usize) {
        let mut guard = self.0.lock().expect("indexing status mutex poisoned");
        if total == 0 {
            *guard = IndexingSnapshot::default();
            return;
        }
        guard.phase = IndexingPhase::Ocr;
        guard.ocr_total = total;
        guard.ocr_processed = 0;
    }

    /// OCR worker 每 flush 一批就调一次：`pending` 是那一刻队列里还剩多少张。
    /// 剩 0 张直接回到 idle（前端据此让"图片识别"那行淡出）。
    ///
    /// 常驻监听期间可能在 OCR 阶段途中又发现新图片（`pending` 超过当初
    /// `begin_ocr` 记的 `ocr_total`）——这种情况下把 `ocr_total` 顺势抬高，
    /// 保证 `ocr_processed = ocr_total - pending` 不会算出负数。
    pub fn set_ocr_pending(&self, pending: usize) {
        let mut guard = self.0.lock().expect("indexing status mutex poisoned");
        if pending == 0 {
            *guard = IndexingSnapshot::default();
            return;
        }
        guard.phase = IndexingPhase::Ocr;
        if pending > guard.ocr_total {
            guard.ocr_total = pending;
        }
        guard.ocr_processed = guard.ocr_total.saturating_sub(pending);
    }

    /// 重建失败等场景下强制回到 idle，不留半截进度。
    pub fn reset_idle(&self) {
        let mut guard = self.0.lock().expect("indexing status mutex poisoned");
        *guard = IndexingSnapshot::default();
    }
}
