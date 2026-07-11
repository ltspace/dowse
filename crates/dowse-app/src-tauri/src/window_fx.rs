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

/// 是不是在远程会话（RDP/AVD/VDI）里跑。Windows 的 DWM 在远程会话下会把
/// Acrylic/Mica 系统材质悄悄换成一块不透明的纯色回退——`DwmSetWindowAttribute`
/// 调用本身仍然返回成功，应用层没有公开 API 能查到"其实没在磨砂"，
/// 所以没法靠 `set_effects` 的返回值判断。用 `SESSIONNAME` 环境变量识别：
/// 本机控制台会话固定是 "Console"，RDP 会话是 "RDP-Tcp#<id>"。
fn is_remote_session() -> bool {
    std::env::var("SESSIONNAME")
        .map(|name| !name.eq_ignore_ascii_case("console"))
        .unwrap_or(false)
}

/// 材质降级链：Acrylic → Mica → 纯色。玻璃效果是锦上添花，
/// 拿不到就退一级，绝不能让"要不到 Acrylic"变成窗口显示不出来。
/// `transparency_enabled = false` 时（用户在托盘关了透明效果）直接落纯色。
///
/// 注意：`window.set_effects(..).is_ok()` 只反映"把请求发给了 DWM"，不反映
/// 材质是否真的生效——Tauri 内部把 `DwmSetWindowAttribute` 的 HRESULT 直接丢掉了，
/// 永远返回 `Ok`。所以这条链天然测不出"申请到了但显示成纯色"这种情况，
/// 远程会话就是最典型的例子，得单独识别、单独处理。
pub fn apply_with_fallback(window: &WebviewWindow, transparency_enabled: bool) -> EffectLevel {
    if !transparency_enabled {
        let _ = window.set_effects(None);
        eprintln!("材质降级：用户在托盘关闭了透明效果，直接落纯色");
        return EffectLevel::Solid;
    }

    if is_remote_session() {
        // 材质请求照发（不影响功能，万一某些远程会话其实支持），但已知
        // DWM 在这种场景下大概率会把 Acrylic/Mica 悄悄渲染成不透明纯色——
        // 与其让前端顶着"acrylic"的名头显示一块来源不明的纯色，不如如实
        // 上报 Solid，让前端走我们自己设计过的深灰兜底，观感可控。
        let acrylic = EffectsBuilder::new().effect(Effect::Acrylic).build();
        let _ = window.set_effects(acrylic);
        eprintln!(
            "材质降级：检测到远程会话（SESSIONNAME={}），Acrylic/Mica 在此场景下会被 DWM 渲染成不透明纯色，直接落纯色",
            std::env::var("SESSIONNAME").unwrap_or_default()
        );
        return EffectLevel::Solid;
    }

    let acrylic = EffectsBuilder::new().effect(Effect::Acrylic).build();
    if window.set_effects(acrylic).is_ok() {
        eprintln!("材质降级：Acrylic 申请已发出");
        return EffectLevel::Acrylic;
    }

    let mica = EffectsBuilder::new().effect(Effect::Mica).build();
    if window.set_effects(mica).is_ok() {
        eprintln!("材质降级：Acrylic 申请失败，落到 Mica");
        return EffectLevel::Mica;
    }

    let _ = window.set_effects(None);
    eprintln!("材质降级：Acrylic 和 Mica 都申请失败，落到纯色");
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
