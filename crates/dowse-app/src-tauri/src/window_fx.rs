use std::sync::Mutex;

use serde::Serialize;
use tauri::window::{Effect, EffectsBuilder};
use tauri::{Emitter, PhysicalPosition, WebviewWindow};

/// 材质降级链最终落到哪一级，前端用它决定要不要自己叠一层纯色背景兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectLevel {
    Acrylic,
    Mica,
    Solid,
}

/// 当前生效的材质级别，进程内常驻一份供前端启动时查询
/// （启动阶段 emit 的事件前端不一定来得及监听到，State 查询更可靠）。
pub struct EffectLevelState(pub Mutex<EffectLevel>);

impl EffectLevelState {
    pub fn new(level: EffectLevel) -> Self {
        Self(Mutex::new(level))
    }

    pub fn get(&self) -> EffectLevel {
        *self.0.lock().expect("effect level mutex poisoned")
    }

    pub fn set(&self, level: EffectLevel) {
        *self.0.lock().expect("effect level mutex poisoned") = level;
    }
}

/// 材质降级链：Acrylic → Mica → 纯色。玻璃效果是锦上添花，
/// 拿不到就退一级，绝不能让"要不到 Acrylic"变成窗口显示不出来。
/// `transparency_enabled = false` 时（用户在托盘关了透明效果）直接落纯色。
pub fn apply_with_fallback(window: &WebviewWindow, transparency_enabled: bool) -> EffectLevel {
    if !transparency_enabled {
        let _ = window.set_effects(None);
        return EffectLevel::Solid;
    }

    let acrylic = EffectsBuilder::new().effect(Effect::Acrylic).build();
    if window.set_effects(acrylic).is_ok() {
        return EffectLevel::Acrylic;
    }

    let mica = EffectsBuilder::new().effect(Effect::Mica).build();
    if window.set_effects(mica).is_ok() {
        return EffectLevel::Mica;
    }

    let _ = window.set_effects(None);
    EffectLevel::Solid
}

/// 窗口居中偏上——参照 Spotlight/Raycast 的位置习惯，不是正中央。
/// 屏幕高度的约 22% 处起摆，比 50% 正中更符合"呼出即用"的视觉预期。
pub fn position_upper_center(window: &WebviewWindow) -> tauri::Result<()> {
    let Some(monitor) = window.current_monitor()? else {
        return Ok(());
    };
    let screen_size = *monitor.size();
    let screen_pos = *monitor.position();
    let win_size = window.outer_size()?;

    let x = screen_pos.x + (screen_size.width as i32 - win_size.width as i32) / 2;
    let y = screen_pos.y + (screen_size.height as f64 * 0.22) as i32;

    window.set_position(PhysicalPosition::new(x, y))
}

/// 呼出：定位到居中偏上、显示、抢焦点，再广播一个事件给前端——
/// 前端监听它来做"输入框自动聚焦、上次查询词全选"（设计文档的交互规则）。
pub fn show_window(window: &WebviewWindow) {
    let _ = position_upper_center(window);
    let _ = window.show();
    let _ = window.set_focus();
    let _ = window.emit("dowse://shown", ());
}

pub fn hide_window(window: &WebviewWindow) {
    let _ = window.hide();
}

pub fn toggle_window(window: &WebviewWindow) {
    if window.is_visible().unwrap_or(false) {
        hide_window(window);
    } else {
        show_window(window);
    }
}
