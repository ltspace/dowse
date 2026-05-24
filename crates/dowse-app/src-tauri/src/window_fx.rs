use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::window::{Color, Effect, EffectsBuilder};
use tauri::{Emitter, PhysicalPosition, Theme, WebviewWindow};

/// 材质降级链最终落到哪一级，前端用它决定要不要自己叠一层纯色背景兜底。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum EffectLevel {
    Acrylic,
    Mica,
    Solid,
}

/// 透明度三档，托盘"透明度"子菜单的三个选项。挡位名字说的是"透明度的
/// 高低"，不是"不透明度的高低"——`Low`（低透明度）最不透明，`High`
/// （高透明度）最通透，`Mid` 是默认值。
///
/// v0.4.0 自测暴露的问题：面板背后其实叠着两层不透明度贡献者——DWM 侧
/// `neutral_acrylic_tint` 送给系统合成器的 tint alpha，和 CSS 侧
/// `--glass-tint` 的 alpha。v0.4.0 只做了 CSS 那一层的调参，DWM 那层常年
/// 焊死在 40，用户把 CSS alpha 从 0.55 调到 0.32 却"观感丝毫没变"，就是
/// 因为焊死的 DWM 层把变化盖过去了。这张表把两层的 alpha 绑定到同一个
/// 挡位上，确保它们永远同向同步移动，用户拨一下托盘菜单，两层一起动。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TransparencyTier {
    /// 低透明度：最不透明，玻璃感最弱、可读性最强。
    Low,
    /// 中透明度：默认档，跟 v0.4.0 发布时的观感基本一致。
    #[default]
    Mid,
    /// 高透明度：最通透，玻璃感最强。
    High,
}

/// 面板可视不透明度收拢后的单一控制点：一个挡位 -> 两层 alpha 一起定。
/// 数值构成（暗色主题）：
///
/// | 挡位 | DWM tint alpha (0~255) | CSS --glass-tint alpha（暗） | CSS --glass-tint alpha（亮，暗+0.12） |
/// |------|------------------------|-------------------------------|------------------------------------------|
/// | 低   | 60                     | 0.45                          | 0.57                                      |
/// | 中   | 40                     | 0.28                          | 0.40                                      |
/// | 高   | 16                     | 0.12                          | 0.24                                      |
///
/// 亮色主题统一比暗色主题的 CSS alpha 高 0.12——亮底色透光感天然弱一档，
/// 需要多留一点不透明度才能撑住前景对比度，跟 app.css 里原有的明暗两套
/// tint 基色（#f6f6f8 亮 / #121214 暗）的取舍逻辑一致。
///
/// 纯色降级档（`EffectLevel::Solid`）完全不受这张表影响——纯色档在
/// app.css 里用 `:root[data-effect='solid']` 整体覆盖 `--glass-tint`
/// 为不透明色，跟这里的挡位无关。
impl TransparencyTier {
    /// DWM 侧 Acrylic tint 的 alpha 通道，0~255，喂给
    /// `EffectsBuilder::color`（Windows `SetWindowCompositionAttribute` 的
    /// `GradientColor`）。
    pub fn dwm_alpha(self) -> u8 {
        match self {
            TransparencyTier::Low => 60,
            TransparencyTier::Mid => 40,
            TransparencyTier::High => 16,
        }
    }

    /// CSS `--glass-tint` 暗色主题 alpha，0.0~1.0。
    pub fn css_alpha_dark(self) -> f32 {
        match self {
            TransparencyTier::Low => 0.45,
            TransparencyTier::Mid => 0.28,
            TransparencyTier::High => 0.12,
        }
    }

    /// CSS `--glass-tint` 亮色主题 alpha——统一比暗色主题高 0.12。
    pub fn css_alpha_light(self) -> f32 {
        self.css_alpha_dark() + 0.12
    }

    /// 打包成前端要的载荷，一次 emit/查询把两套主题的 alpha 都带过去。
    pub fn glass_alpha(self) -> GlassAlpha {
        GlassAlpha {
            light: self.css_alpha_light(),
            dark: self.css_alpha_dark(),
        }
    }
}

/// 广播/查询给前端的 CSS alpha 载荷——前端拿到后直接
/// `style.setProperty('--glass-alpha-light'/'--glass-alpha-dark', ...)`，
/// 具体套用哪一个由 CSS 的 `prefers-color-scheme` 媒体查询决定，不需要
/// 前端自己判断当前是明是暗。
#[derive(Debug, Clone, Copy, Serialize)]
pub struct GlassAlpha {
    pub light: f32,
    pub dark: f32,
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

/// Acrylic 材质上叠加的中性 tint——只影响 Windows 10 v1903+（Win11 上 DWM
/// 会忽略这个颜色，官方文档写明"Doesn't have any effect on ... Windows 11"）。
/// 加上它纯粹是给还在用 Win10 的用户一个体面的兜底，成本很低。
/// 暗色主题偏黑、亮色主题偏白；alpha 不再写死，由 `tier.dwm_alpha()` 给，
/// 跟 CSS 侧 `--glass-tint` 的 alpha 绑定在同一个挡位上同步移动——两层
/// 各管各的调，就是 v0.4.0 那个"调了不管用"问题的根源，见 `TransparencyTier`
/// 上的文档。
fn neutral_acrylic_tint(window: &WebviewWindow, tier: TransparencyTier) -> Color {
    let alpha = tier.dwm_alpha();
    match window.theme() {
        Ok(Theme::Dark) => Color(18, 18, 20, alpha),
        _ => Color(246, 246, 248, alpha),
    }
}

/// Win11 原生窗口圆角裁切：`DwmSetWindowAttribute` 设置
/// `DWMWA_WINDOW_CORNER_PREFERENCE`（33）= `DWMWCP_ROUND`（2），让 DWM 把整个
/// 窗口（连同 Acrylic/Mica 玻璃）按系统圆角裁掉。不这么做的话，面板本体的
/// CSS 圆角只裁了内容，窗口本体和它背后的玻璃依然是直角矩形，四角会露出
/// "玻璃直角 - 面板圆角" 的三角形瑕疵。
///
/// 圆角是纯视觉裁切，跟材质降级链（Acrylic/Mica/纯色）无关，纯色兜底和
/// 远程会话也一样受益，所以放在 `apply_with_fallback` 最前面无条件调用一次，
/// 不需要在每条降级分支里各设一遍。
#[cfg(target_os = "windows")]
fn apply_rounded_corners(window: &WebviewWindow) {
    const DWMWA_WINDOW_CORNER_PREFERENCE: u32 = 33;
    const DWMWCP_ROUND: i32 = 2;

    #[link(name = "dwmapi")]
    unsafe extern "system" {
        fn DwmSetWindowAttribute(
            hwnd: *mut core::ffi::c_void,
            dw_attribute: u32,
            pv_attribute: *const core::ffi::c_void,
            cb_attribute: u32,
        ) -> i32;
    }

    let Ok(hwnd) = window.hwnd() else {
        eprintln!("圆角裁切：拿不到 HWND，跳过");
        return;
    };

    let preference: i32 = DWMWCP_ROUND;
    let hr = unsafe {
        DwmSetWindowAttribute(
            hwnd.0,
            DWMWA_WINDOW_CORNER_PREFERENCE,
            &preference as *const i32 as *const core::ffi::c_void,
            std::mem::size_of::<i32>() as u32,
        )
    };
    if hr != 0 {
        eprintln!(
            "圆角裁切：DwmSetWindowAttribute 返回 HRESULT 0x{hr:x}，可能是系统版本太老（Win10 v20H1 以前没有这个属性）"
        );
    }
}

/// 材质降级链：Acrylic → Mica → 纯色。玻璃效果是锦上添花，
/// 拿不到就退一级，绝不能让"要不到 Acrylic"变成窗口显示不出来。
/// `transparency_enabled = false` 时（用户在托盘关了透明效果）直接落纯色。
///
/// 注意：`window.set_effects(..).is_ok()` 只反映"把请求发给了 DWM"，不反映
/// 材质是否真的生效——Tauri 内部把 `DwmSetWindowAttribute` 的 HRESULT 直接丢掉了，
/// 永远返回 `Ok`。所以这条链天然测不出"申请到了但显示成纯色"这种情况，
/// 远程会话就是最典型的例子，得单独识别、单独处理。
pub fn apply_with_fallback(
    window: &WebviewWindow,
    transparency_enabled: bool,
    tier: TransparencyTier,
) -> EffectLevel {
    // 圆角裁切跟材质降级链无关，纯色兜底和远程会话也一样受益，最前面无条件做一次。
    #[cfg(target_os = "windows")]
    apply_rounded_corners(window);

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
        let acrylic = EffectsBuilder::new()
            .effect(Effect::Acrylic)
            .color(neutral_acrylic_tint(window, tier))
            .build();
        let _ = window.set_effects(acrylic);
        eprintln!(
            "材质降级：检测到远程会话（SESSIONNAME={}），Acrylic/Mica 在此场景下会被 DWM 渲染成不透明纯色，直接落纯色",
            std::env::var("SESSIONNAME").unwrap_or_default()
        );
        return EffectLevel::Solid;
    }

    let acrylic = EffectsBuilder::new()
        .effect(Effect::Acrylic)
        .color(neutral_acrylic_tint(window, tier))
        .build();
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
///
/// v0.4.1 把窗口从 750×500 放大到 860×560 时复核过这条逻辑：这里用的是
/// `window.outer_size()`（整个窗口，含 app.css 里给阴影留白的
/// `--panel-margin`），横向居中和纵向 22% 起摆都是相对屏幕尺寸的比例计算，
/// 不依赖任何写死的像素基准，换了窗口尺寸不需要跟着改数。
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
