use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

/// 统一的"抑制自动隐藏"状态。
///
/// `lib.rs` 的 `WindowEvent::Focused(false)` 处理原本是"失焦即隐藏"（Spotlight/
/// Raycast 习惯，见 lib.rs 上的说明）；v0.5.0 加了两个会让窗口临时/长期失焦、
/// 但不该触发这条自动隐藏的场景：
///
/// - 结果行右键弹出的原生菜单（Win32 `TrackPopupMenu`）——菜单显示期间 WebView2
///   的渲染表面会失去输入焦点，实测确实会连带触发窗口的 Focused(false)，
///   不加这层豁免会出现"右键刚弹出菜单，窗口自己先隐藏了"的坏体验。
/// - 用户点了图钉手动固定——固定期间点窗口外/切到别的应用都不该收起浮窗，
///   直到用户自己再点一次图钉或按 Esc / 全局快捷键收起。
///
/// 两个贡献者共用同一个计数器，而不是"菜单一套判断、图钉一套判断"分头写：
/// 计数器 > 0 就跳过这次自动隐藏，减到 0 才恢复。Esc（`getCurrentWindow().hide()`）
/// 和全局呼出快捷键的隐藏调用都是直接 `.hide()`，不经过这个计数器，
/// 所以"固定"只豁免自动消失、不豁免用户主动收起，这条规则天然成立。
#[derive(Default)]
pub struct AutoHideSuppressor {
    count: AtomicUsize,
    pinned: AtomicBool,
}

impl AutoHideSuppressor {
    pub fn new() -> Self {
        Self::default()
    }

    /// 当前是否应该跳过失焦自动隐藏。
    pub fn is_suppressed(&self) -> bool {
        self.count.load(Ordering::SeqCst) > 0
    }

    /// 图钉切换：会话级状态，不落盘——重启应用回到未固定（见前端 PinButton）。
    /// 只在真的发生翻转时才动计数器，重复设成同一个值是空操作，避免计数器
    /// 因为前端重复调用而失衡。
    pub fn set_pinned(&self, pinned: bool) {
        let was_pinned = self.pinned.swap(pinned, Ordering::SeqCst);
        if pinned && !was_pinned {
            self.count.fetch_add(1, Ordering::SeqCst);
        } else if !pinned && was_pinned {
            self.count.fetch_sub(1, Ordering::SeqCst);
        }
    }

    /// 右键原生菜单弹出期间的临时抑制：RAII guard，menu 命令函数返回（包括
    /// 弹出失败/被用户直接取消菜单的路径）时自动释放，不会因为提前 return
    /// 忘记配对的 -1。
    #[must_use]
    pub fn suppress_for_menu(&self) -> MenuSuppressGuard<'_> {
        self.count.fetch_add(1, Ordering::SeqCst);
        MenuSuppressGuard { owner: self }
    }
}

pub struct MenuSuppressGuard<'a> {
    owner: &'a AutoHideSuppressor,
}

impl Drop for MenuSuppressGuard<'_> {
    fn drop(&mut self) {
        self.owner.count.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pin_toggle_suppresses_until_unpinned() {
        let s = AutoHideSuppressor::new();
        assert!(!s.is_suppressed());
        s.set_pinned(true);
        assert!(s.is_suppressed());
        s.set_pinned(false);
        assert!(!s.is_suppressed());
    }

    #[test]
    fn repeated_set_pinned_same_value_does_not_unbalance_counter() {
        let s = AutoHideSuppressor::new();
        s.set_pinned(true);
        s.set_pinned(true);
        s.set_pinned(true);
        assert!(s.is_suppressed());
        s.set_pinned(false);
        assert!(!s.is_suppressed(), "重复设 true 不应该让计数器多加超过一次");
    }

    #[test]
    fn menu_guard_suppresses_while_held_and_releases_on_drop() {
        let s = AutoHideSuppressor::new();
        assert!(!s.is_suppressed());
        {
            let _guard = s.suppress_for_menu();
            assert!(s.is_suppressed());
        }
        assert!(!s.is_suppressed());
    }

    #[test]
    fn menu_guard_during_pin_leaves_pin_suppression_active_after_drop() {
        let s = AutoHideSuppressor::new();
        s.set_pinned(true);
        {
            let _guard = s.suppress_for_menu();
            assert!(s.is_suppressed());
        }
        // 菜单 guard 释放后，图钉的常驻抑制应该还在——两个贡献者互不干扰。
        assert!(s.is_suppressed());
    }
}
