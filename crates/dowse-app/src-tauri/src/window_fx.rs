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
///
/// v0.4.2 自测又发现：v0.4.1 这个"两层绑同一挡位"的方案在 Windows 11
/// build ≥ 22523（22H2 起）上仍然失效——不是绑定关系错了，是 DWM 侧那条
/// 路径本身在这些系统版本上已经不接受自定义 alpha 了（`tauri::set_effects`
/// 底下的 `window-vibrancy` 在这个版本区间会自动切到
/// `DWMWA_SYSTEMBACKDROP_TYPE`，这个新 API 没有颜色/alpha 参数）。
/// `apply_with_fallback` 里已经绕开这条自动升级、直接调
/// `SetWindowCompositionAttribute` 保证 DWM 侧 alpha 真正生效，这张表和
/// `dwm_alpha()` 的数值本身不用变，改的只是"怎么把这个数字送进 DWM"，
/// 细节见 `swca` 模块的文档注释。
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
    /// DWM 侧 Acrylic tint 的 alpha 通道，0~255，最终喂给 `swca::apply_acrylic`
    /// 组装的 `GradientColor`（Windows `SetWindowCompositionAttribute` 的
    /// `ACCENT_POLICY`）。
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

/// 是不是在远程会话（RDP/AVD/VDI）里跑，只用来记日志、不再驱动降级决策
/// （v0.6.1 起，见 `apply_with_fallback` 里那段撤销说明）。老版本 Windows 的
/// DWM 在远程会话下确实会把 Acrylic/Mica 系统材质悄悄换成不透明纯色，但这
/// 不是当前所有 RDP 场景都成立的规律，而且 `SESSIONNAME` 探测本身依赖启动
/// 上下文——开机自启的进程如果先于 RDP 连接启动，这里测不出"现在其实是远程
/// 会话"，这条判据的可靠性只够留痕，不够拿来做非黑即白的降级决策。
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

/// v0.4.2 自测发现：托盘"透明度"三档在任何当前 Windows 11 机器上都是
/// 死代码，根因在 `tauri::WebviewWindow::set_effects` 这条调用链上——
/// `tauri` 的 `Effect::Acrylic` 内部转给 `window-vibrancy` 的
/// `apply_acrylic()`，那个函数在系统 build ≥ 22523（Win11 22H2 起，
/// 这台机器是 26100）时会改用 `DwmSetWindowAttribute(DWMWA_SYSTEMBACKDROP_TYPE,
/// DWMSBT_TRANSIENTWINDOW)`（Win11 原生系统背景材质），这条新路径**根本
/// 不接受自定义颜色/alpha 参数**——`neutral_acrylic_tint()` 算出来的
/// `Color`（也就是 `TransparencyTier::dwm_alpha()` 的 60/40/16）在这条路径
/// 上被直接丢弃，材质的不透明度完全由系统自己决定，托盘三档不管选哪个，
/// 送进去的都是同一个系统默认值，DWM 半层因此彻底失联。
///
/// 唯一还接受自定义 alpha 的官方渠道是更老的 `SetWindowCompositionAttribute`
/// （user32.dll 的未公开 API，`ACCENT_ENABLE_ACRYLICBLURBEHIND`），
/// `window-vibrancy` 只在 build < 22523 时才会走这条路——但那正是我们
/// 需要的那条。这里绕开 `window-vibrancy` 的自动升级判断，直接调用同一个
/// API，跟它内部实现用的是同一套结构体布局，代价是放弃 Win11 更新潮的
/// 原生背景材质观感，换来透明度三档在所有 Windows 版本上都真正生效。
#[cfg(target_os = "windows")]
mod swca {
    use std::sync::OnceLock;

    use tauri::window::Color;
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
    use windows::core::s;

    #[repr(C)]
    struct AccentPolicy {
        accent_state: u32,
        accent_flags: u32,
        gradient_color: u32,
        animation_id: u32,
    }

    #[repr(C)]
    struct WindowCompositionAttribData {
        attrib: u32,
        pv_data: *mut core::ffi::c_void,
        cb_data: usize,
    }

    const WCA_ACCENT_POLICY: u32 = 0x13;
    const ACCENT_DISABLED: u32 = 0;
    const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

    type SetWindowCompositionAttributeFn =
        unsafe extern "system" fn(*mut core::ffi::c_void, *mut WindowCompositionAttribData) -> i32;

    /// `SetWindowCompositionAttribute` 是未公开 API，没有出现在 Windows SDK
    /// 的 user32.lib 里——静态 `#[link(name = "user32")]` 链接会在链接期
    /// 报 `无法解析的外部符号 __imp_SetWindowCompositionAttribute`（本轮
    /// 修复时实测踩到过），只能运行时 `GetProcAddress` 动态取，跟
    /// window-vibrancy 内部的做法一致。只查一次，缓存下来。
    fn proc() -> Option<SetWindowCompositionAttributeFn> {
        static CACHED: OnceLock<Option<usize>> = OnceLock::new();
        let addr = *CACHED.get_or_init(|| unsafe {
            let module = LoadLibraryA(s!("user32.dll")).ok()?;
            let addr = GetProcAddress(module, s!("SetWindowCompositionAttribute"))?;
            Some(addr as usize)
        });
        addr.map(|addr| unsafe {
            std::mem::transmute::<usize, SetWindowCompositionAttributeFn>(addr)
        })
    }

    /// `accent_state` 只在这个模块内部传 `ACCENT_ENABLE_ACRYLICBLURBEHIND`
    /// 或 `ACCENT_DISABLED` 两种取值，见 `apply_acrylic`/`clear_acrylic`。
    fn set_accent(hwnd: *mut core::ffi::c_void, accent_state: u32, color: Option<Color>) -> bool {
        let Some(set_window_composition_attribute) = proc() else {
            eprintln!("SWCA：GetProcAddress 拿不到 SetWindowCompositionAttribute，跳过");
            return false;
        };

        let mut color = color.unwrap_or(Color(0, 0, 0, 0));
        let is_acrylic = accent_state == ACCENT_ENABLE_ACRYLICBLURBEHIND;
        // Acrylic 不接受 alpha=0（会整个不显示），跟 window-vibrancy 的处理一致。
        if is_acrylic && color.3 == 0 {
            color.3 = 1;
        }

        let mut policy = AccentPolicy {
            accent_state,
            accent_flags: if is_acrylic { 0 } else { 2 },
            gradient_color: (color.0 as u32)
                | ((color.1 as u32) << 8)
                | ((color.2 as u32) << 16)
                | ((color.3 as u32) << 24),
            animation_id: 0,
        };

        let mut data = WindowCompositionAttribData {
            attrib: WCA_ACCENT_POLICY,
            pv_data: &mut policy as *mut _ as *mut core::ffi::c_void,
            cb_data: std::mem::size_of::<AccentPolicy>(),
        };

        let ok = unsafe { set_window_composition_attribute(hwnd, &mut data as *mut _) };
        ok != 0
    }

    /// 申请 Acrylic Blur Behind，`color` 的 alpha 通道真正生效（跟
    /// `DWMWA_SYSTEMBACKDROP_TYPE` 不同）。
    pub fn apply_acrylic(hwnd: *mut core::ffi::c_void, color: Color) -> bool {
        set_accent(hwnd, ACCENT_ENABLE_ACRYLICBLURBEHIND, Some(color))
    }

    /// 清掉 SWCA 层的 Acrylic——跟 `tauri::WebviewWindow::set_effects(None)`
    /// 走的是不同的合成属性（那个清的是 `DWMWA_SYSTEMBACKDROP_TYPE`），
    /// 两边都要清，否则可能残留一层"看不见但还在合成"的旧 Acrylic。
    pub fn clear_acrylic(hwnd: *mut core::ffi::c_void) -> bool {
        set_accent(hwnd, ACCENT_DISABLED, None)
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

/// 申请 Acrylic，返回是否成功把请求发出去了。Windows 上直接走
/// `swca::apply_acrylic`——原因见上面 `swca` 模块的文档注释：只有这条路径
/// 的 alpha 参数真正生效，托盘三档才有视觉区别。拿不到 HWND 时（理论上
/// 不该发生）退回 tauri 自带的 `set_effects`兜底；非 Windows 平台也是
/// 一样，Acrylic 本来就是 Windows 独有的材质，那边的实现只是占位。
fn apply_acrylic_effect(window: &WebviewWindow, tint: Color) -> bool {
    #[cfg(target_os = "windows")]
    {
        if let Ok(hwnd) = window.hwnd() {
            return swca::apply_acrylic(hwnd.0, tint);
        }
    }
    #[allow(unreachable_code)]
    {
        let acrylic = EffectsBuilder::new()
            .effect(Effect::Acrylic)
            .color(tint)
            .build();
        window.set_effects(acrylic).is_ok()
    }
}

/// 清掉 SWCA 层的 Acrylic，再叠一次 tauri 自带的 `set_effects(None)`——
/// 两边各管一个不同的合成属性（见 `swca::clear_acrylic` 文档注释），都清
/// 才能保证不残留旧材质。
fn clear_acrylic_effect(window: &WebviewWindow) {
    #[cfg(target_os = "windows")]
    {
        if let Ok(hwnd) = window.hwnd() {
            let _ = swca::clear_acrylic(hwnd.0);
        }
    }
    let _ = window.set_effects(None);
}

/// 材质降级链：Acrylic → Mica → 纯色。玻璃效果是锦上添花，
/// 拿不到就退一级，绝不能让"要不到 Acrylic"变成窗口显示不出来。
/// `transparency_enabled = false` 时（用户在托盘关了透明效果）直接落纯色。
///
/// 注意：`apply_acrylic_effect`/`window.set_effects(..).is_ok()` 只反映
/// "把请求发给了 DWM"，不反映材质是否真的生效——Tauri 内部把
/// `DwmSetWindowAttribute` 的 HRESULT 直接丢掉了，永远返回 `Ok`。所以这条链
/// 天然测不出"申请到了但显示成纯色"这种情况，远程会话就是最典型的例子，
/// 得单独识别、单独处理。
pub fn apply_with_fallback(
    window: &WebviewWindow,
    transparency_enabled: bool,
    tier: TransparencyTier,
) -> EffectLevel {
    // 圆角裁切跟材质降级链无关，纯色兜底和远程会话也一样受益，最前面无条件做一次。
    #[cfg(target_os = "windows")]
    apply_rounded_corners(window);

    if !transparency_enabled {
        clear_acrylic_effect(window);
        eprintln!("材质降级：用户在托盘关闭了透明效果，直接落纯色");
        return EffectLevel::Solid;
    }

    if is_remote_session() {
        // v0.6.0 曾经在这里预判"远程会话一律强制报告 Solid"，v0.6.1 撤销了这个
        // 预判：用户实测反馈新版 Win11 的 RDP 图形管线能正常透传 DWM
        // Acrylic/Mica 特效，老版本 Windows 才有"远程会话被悄悄渲染成不透明
        // 纯色"的限制——一刀切强制降级反而会在能正常渲染的新机器上误伤真实
        // 效果。而且 `SESSIONNAME` 探测本身依赖启动上下文：开机自启的进程
        // 如果是先启动、用户后连的 RDP，这里根本测不出"现在是远程会话"，
        // 说明这条判据已经不可靠到不该再拿来做决策，只适合留痕。
        //
        // 改成只记日志、不改行为：材质请求照常发，`EffectLevel` 照常按下面
        // 的正常降级链上报，真渲染不出来玻璃效果时，用户手里还有托盘"关闭
        // 透明效果"开关和纯色档可以自救，不需要我们越俎代庖替他们决定。
        eprintln!(
            "检测到远程会话（SESSIONNAME={}），材质请求照常发出，不再强制降级为纯色——\
             新版 RDP 通常能正常渲染 Acrylic/Mica，渲染不出来时可用托盘的透明度开关自救",
            std::env::var("SESSIONNAME").unwrap_or_default()
        );
    }

    if apply_acrylic_effect(window, neutral_acrylic_tint(window, tier)) {
        eprintln!("材质降级：Acrylic 申请已发出（tier={tier:?}）");
        return EffectLevel::Acrylic;
    }

    let mica = EffectsBuilder::new().effect(Effect::Mica).build();
    if window.set_effects(mica).is_ok() {
        eprintln!("材质降级：Acrylic 申请失败，落到 Mica");
        return EffectLevel::Mica;
    }

    clear_acrylic_effect(window);
    eprintln!("材质降级：Acrylic 和 Mica 都申请失败，落到纯色");
    EffectLevel::Solid
}

/// 窗口居中偏上——参照 Spotlight/Raycast 的位置习惯，不是正中央。
/// 屏幕高度的约 22% 处起摆，比 50% 正中更符合"呼出即用"的视觉预期。
///
/// v0.4.1 把窗口从 750×500 放大到 860×560 时复核过这条逻辑：这里用的是
/// `window.outer_size()`（整个窗口——v0.4.2 起 .panel 满铺窗口，两者已经
/// 是同一个尺寸，见 app.css 里双框 bug 的修复说明），横向居中和纵向 22%
/// 起摆都是相对屏幕尺寸的比例计算，不依赖任何写死的像素基准，换了窗口
/// 尺寸不需要跟着改数。
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
