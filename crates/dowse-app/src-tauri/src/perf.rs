//! 性能埋点：README 的设计目标"热键呼出到窗口可见 <50ms"、"击键到结果
//! 渲染 <80ms"至今从未被实际测量过——这个模块只负责让"呼出到可见"这一项
//! 可测（落一行日志），不加配置开关、不加 UI。"击键到渲染"那一项计时点都
//! 在前端（performance.now() + tick() + requestAnimationFrame），量出来之后
//! 直接调 `commands::report_search_perf` 打日志，不需要 Rust 侧存状态，
//! 所以这个模块只有呼出延迟这一半。
//!
//! 失败安全是硬要求：呼出延迟的起止两端分别在 Rust 热键回调和前端事件
//! 回调里，中间隔着一次 IPC，任何一步（事件没送到、前端来不及监听、
//! mutex poisoned）出岔子都只应该"这次没记上"，不能影响呼出本身。

use std::sync::Mutex;
use std::time::Instant;

/// 全局热键回调进入的时刻，只在"即将显示窗口"这条路径上写入
/// （见 lib.rs 的快捷键回调）——热键同时承担 show/hide 两种职责（toggle），
/// 呼出延迟只关心显示这条路径；托盘点击呼出（tray.rs）完全不经过这个
/// 模块，不会被误记成热键延迟。
///
/// 前端确认首帧真正绘制完成后调用 `report_shown_perf` 命令把这个值取走
/// （`take`，单次消费）：一是避免同一次呼出被重复上报，二是避免"热键按下
/// 但窗口这次没显示成功"之类的异常场景让下一次呼出继承一个过期的起始
/// 时刻、算出离谱的耗时。
#[derive(Default)]
pub struct HotkeyPerfState(Mutex<Option<Instant>>);

impl HotkeyPerfState {
    pub fn new() -> Self {
        Self::default()
    }

    /// 标记这次显示由全局热键触发，记下回调进入的单调时钟。
    pub fn mark_hotkey_show(&self) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = Some(Instant::now());
        }
    }

    /// 取出并清空标记的起始时刻。mutex poisoned 或压根没标记过（本次显示
    /// 不是热键触发的）都返回 `None`，调用方据此静默放弃这次记录。
    pub fn take(&self) -> Option<Instant> {
        self.0.lock().ok().and_then(|mut guard| guard.take())
    }
}
