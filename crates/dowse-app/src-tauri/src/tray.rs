use tauri::image::Image;
use tauri::menu::{
    CheckMenuItem, CheckMenuItemBuilder, Menu, MenuItem, MenuItemBuilder, PredefinedMenuItem,
    Submenu,
};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::config::ConfigState;
use crate::indexing_status::{IndexingPhase, IndexingStatus};
use crate::rebuild::{RebuildGuard, format_count};
use crate::state::SearchState;
use crate::window_fx::{self, EffectLevelState, TransparencyTier};

/// 任务栏是浅色还是深色，决定托盘剪影用哪一版（见图标说明第 4 节）。只在
/// 托盘图标构建时读一次，不做动态监听——运行中途切系统主题需要重启应用
/// 才能换剪影，这是本轮明确的取舍（任务书原话："动态监听不做"）。
#[cfg(target_os = "windows")]
fn taskbar_uses_light_theme() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;

    let mut value: u32 = 0;
    let mut size: u32 = std::mem::size_of::<u32>() as u32;
    // 任务栏（不是应用）的明暗跟的是 SystemUsesLightTheme，跟应用整体明暗的
    // AppsUseLightTheme 是两个独立的键——Win11 允许任务栏和应用窗口分别设置。
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!("Software\\Microsoft\\Windows\\CurrentVersion\\Themes\\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some(&mut value as *mut u32 as *mut core::ffi::c_void),
            Some(&mut size),
        )
    };
    // 键不存在（老版本 Windows）就落到"默认用 tray-dark"这条规则，
    // 跟第 4 节文档写的默认一致：Win11 开箱默认任务栏是深色。
    status == ERROR_SUCCESS && value != 0
}

#[cfg(not(target_os = "windows"))]
fn taskbar_uses_light_theme() -> bool {
    false
}

/// 剪影版没有底板，纯图形 + 透明背景。浅色任务栏配深色剪影（tray-light.png），
/// 深色任务栏配浅色剪影（tray-dark.png）——命名以"配哪种任务栏"而不是"剪影本身
/// 的颜色"为准，跟设计稿 icon-usage.md 第 4 节的文件命名保持一致。
///
/// `tauri::image::Image` 只接受已解码的 RGBA 像素，没有直接吃 PNG 字节的构造
/// 函数，这里用 `png` crate 手动解码内嵌的托盘图。
fn tray_icon_image() -> Image<'static> {
    let bytes: &[u8] = if taskbar_uses_light_theme() {
        include_bytes!("../icons/tray-light.png")
    } else {
        include_bytes!("../icons/tray-dark.png")
    };
    let (rgba, width, height) = decode_rgba_png(bytes);
    Image::new_owned(rgba, width, height)
}

/// 解码内嵌 PNG 为 RGBA8 像素。这两张图是打包进二进制的固定资源（不是运行时
/// 用户输入），解码失败说明打包本身出了问题，直接 panic 比悄悄显示空托盘图标
/// 更容易在开发阶段暴露——跟这个文件里其它内嵌资源的 `.expect()` 风格一致。
fn decode_rgba_png(bytes: &[u8]) -> (Vec<u8>, u32, u32) {
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().expect("内嵌托盘 PNG 应该是合法文件");
    let mut buf = vec![
        0u8;
        reader
            .output_buffer_size()
            .expect("PNG 应该带有明确的帧尺寸")
    ];
    let info = reader
        .next_frame(&mut buf)
        .expect("内嵌托盘 PNG 应该能正常解码首帧");
    buf.truncate(info.buffer_size());

    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => panic!("内嵌托盘 PNG 颜色类型不受支持: {other:?}（导出时应固定为 RGBA）"),
    };
    (rgba, info.width, info.height)
}

const MENU_SHOW: &str = "show";
const MENU_REBUILD: &str = "rebuild";
const MENU_CHANGE_FOLDER: &str = "change_folder";
const MENU_INDEX_INFO: &str = "index_info";
const MENU_AUTOSTART: &str = "autostart";
const MENU_TRANSPARENCY: &str = "transparency";
const MENU_TRANSPARENCY_LOW: &str = "transparency_low";
const MENU_TRANSPARENCY_MID: &str = "transparency_mid";
const MENU_TRANSPARENCY_HIGH: &str = "transparency_high";
const MENU_QUIT: &str = "quit";

const IDLE_TOOLTIP: &str = "dowse — Alt+` 呼出";

/// 托盘图标句柄 + 几个需要运行时更新文案/启停的菜单项句柄。建完菜单/托盘之后
/// 存进 `app.manage()`，供重建流程、OCR 进度回调随时取出来刷新（tooltip、
/// "索引：<目录> · N 篇" 这行只读信息、"重建索引"/"更改索引文件夹…" 两个
/// 动作项的置灰）。
pub struct TrayHandles {
    icon: TrayIcon<tauri::Wry>,
    index_info_item: MenuItem<tauri::Wry>,
    rebuild_item: MenuItem<tauri::Wry>,
    change_folder_item: MenuItem<tauri::Wry>,
}

/// "透明度"子菜单里三个互斥的档位勾选项。Tauri/muda 没有原生的单选菜单项
/// 类型，这里用三个 `CheckMenuItem` 手动模拟单选组——选中一个就要把另外
/// 两个的勾去掉，这三个句柄需要在选中变化时能重新拿到手，所以存进
/// `app.manage()` 供 `handle_menu_event` 复用，而不是建完就丢。
struct TransparencyMenuItems {
    low: CheckMenuItem<tauri::Wry>,
    mid: CheckMenuItem<tauri::Wry>,
    high: CheckMenuItem<tauri::Wry>,
}

impl TransparencyMenuItems {
    /// 把三个勾选项的状态同步到给定档位——目标档位打勾，另外两个去勾。
    fn sync_checked(&self, tier: TransparencyTier) {
        let _ = self.low.set_checked(tier == TransparencyTier::Low);
        let _ = self.mid.set_checked(tier == TransparencyTier::Mid);
        let _ = self.high.set_checked(tier == TransparencyTier::High);
    }
}

/// 托盘图标 + 右键菜单：呼出 / 索引信息（只读） / 重建索引 / 更改索引文件夹…
/// / 开机自启开关 / 透明效果开关 / 透明度三档子菜单 / 退出。进程常驻，浮窗
/// 只是 show/hide——托盘是用户确认"它还活着"、看一眼索引状态、做少数配置的
/// 入口（症状 5：选定文件夹建索引后要能看得见当前根、能改）。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let cfg = app.state::<ConfigState>().get();
    let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);

    let show_item = MenuItemBuilder::with_id(MENU_SHOW, "呼出").build(app)?;
    let index_info_item = MenuItemBuilder::with_id(MENU_INDEX_INFO, index_info_text(app))
        .enabled(false)
        .build(app)?;
    let rebuild_item = MenuItemBuilder::with_id(MENU_REBUILD, "重建索引").build(app)?;
    let change_folder_item =
        MenuItemBuilder::with_id(MENU_CHANGE_FOLDER, "更改索引文件夹…").build(app)?;
    let autostart_item = CheckMenuItemBuilder::with_id(MENU_AUTOSTART, "开机自启")
        .checked(autostart_enabled)
        .build(app)?;
    let transparency_item = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY, "关闭透明效果")
        .checked(!cfg.transparency_enabled)
        .build(app)?;

    let tier = cfg.transparency_tier;
    let tier_low = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY_LOW, "低")
        .checked(tier == TransparencyTier::Low)
        .build(app)?;
    let tier_mid = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY_MID, "中")
        .checked(tier == TransparencyTier::Mid)
        .build(app)?;
    let tier_high = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY_HIGH, "高")
        .checked(tier == TransparencyTier::High)
        .build(app)?;
    let tier_submenu =
        Submenu::with_items(app, "透明度", true, &[&tier_low, &tier_mid, &tier_high])?;
    app.manage(TransparencyMenuItems {
        low: tier_low,
        mid: tier_mid,
        high: tier_high,
    });

    let quit_item = MenuItemBuilder::with_id(MENU_QUIT, "退出").build(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &index_info_item,
            &rebuild_item,
            &change_folder_item,
            &PredefinedMenuItem::separator(app)?,
            &autostart_item,
            &transparency_item,
            &tier_submenu,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    let icon = tray_icon_image();

    let tray_icon = TrayIconBuilder::new()
        .icon(icon)
        .tooltip(IDLE_TOOLTIP)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(handle_menu_event)
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
                && let Some(window) = tray.app_handle().get_webview_window("main")
            {
                window_fx::toggle_window(&window);
            }
        })
        .build(app)?;

    app.manage(TrayHandles {
        icon: tray_icon,
        index_info_item,
        rebuild_item,
        change_folder_item,
    });

    Ok(())
}

/// "索引：<目录> · N 篇"这一行只读菜单项的文案；还没建过索引时给一句引导语。
fn index_info_text(app: &AppHandle) -> String {
    let target_dir = app.state::<ConfigState>().get().target_dir;
    let Some(dir) = target_dir else {
        return "尚未建立索引".to_string();
    };
    let num_docs = app
        .state::<SearchState>()
        .0
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().map(|s| s.num_docs()))
        .unwrap_or(0);
    format!(
        "索引：{} · {} 篇",
        dowse_core::display_path(&dir.to_string_lossy()),
        format_count(num_docs)
    )
}

/// 重建/切换完索引根之后调一次，把托盘那行只读信息刷新成最新的目录 + 篇数
/// （症状 5"可见"：托盘要能看到当前索引根，不用只靠猜）。
pub fn refresh_index_info(app: &AppHandle) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let _ = handles.index_info_item.set_text(index_info_text(app));
}

/// 把托盘 tooltip 同步成 `IndexingStatus` 当前快照对应的文案——空闲时是
/// 默认的呼出提示，文本/OCR 两个阶段各自换成"索引中 N 篇"/"图片识别 N / M"，
/// 窗口不用开着也能瞟一眼进度（症状 2/3 的验收场景之一）。
pub fn refresh_tooltip(app: &AppHandle) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let snapshot = app.state::<IndexingStatus>().snapshot();
    let tooltip = match snapshot.phase {
        IndexingPhase::Idle => IDLE_TOOLTIP.to_string(),
        IndexingPhase::Text => format!(
            "dowse — 索引中 {} 篇",
            format_count(snapshot.text_processed as u64)
        ),
        IndexingPhase::Ocr => format!(
            "dowse — 图片识别 {} / {}",
            format_count(snapshot.ocr_processed as u64),
            format_count(snapshot.ocr_total as u64)
        ),
    };
    let _ = handles.icon.set_tooltip(Some(tooltip));
}

/// 重建期间把"重建索引"/"更改索引文件夹…"两个动作项置灰，防止重入——全量
/// 重建会删掉旧索引目录重建，两次并发执行会互相踩踏。
pub fn set_rebuilding(app: &AppHandle, busy: bool) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let _ = handles.rebuild_item.set_enabled(!busy);
    let _ = handles.change_folder_item.set_enabled(!busy);
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => {
            if let Some(window) = app.get_webview_window("main") {
                window_fx::show_window(&window);
            }
        }
        MENU_REBUILD => rebuild_from_last_dir(app),
        MENU_CHANGE_FOLDER => change_index_folder(app),
        MENU_AUTOSTART => {
            let mgr = app.autolaunch();
            let enabled = mgr.is_enabled().unwrap_or(false);
            let toggled = if enabled { mgr.disable() } else { mgr.enable() };
            match toggled {
                Ok(()) => {
                    // 记下用户是主动关的还是主动开的——下次启动时的默认开逻辑
                    // 只在"用户没关过"的前提下生效，不能覆盖用户的选择。
                    let _ = app
                        .state::<ConfigState>()
                        .set_autostart_user_disabled(enabled);
                }
                Err(err) => eprintln!("切换开机自启失败: {err}"),
            }
        }
        MENU_TRANSPARENCY => {
            let config = app.state::<ConfigState>();
            let now_enabled = !config.get().transparency_enabled;
            let _ = config.set_transparency_enabled(now_enabled);
            let tier = config.get().transparency_tier;
            if let Some(window) = app.get_webview_window("main") {
                let level = window_fx::apply_with_fallback(&window, now_enabled, tier);
                app.state::<EffectLevelState>().set(level);
                let _ = window.emit("dowse://effect-level", level);
            }
        }
        MENU_TRANSPARENCY_LOW | MENU_TRANSPARENCY_MID | MENU_TRANSPARENCY_HIGH => {
            let tier = match event.id().as_ref() {
                MENU_TRANSPARENCY_LOW => TransparencyTier::Low,
                MENU_TRANSPARENCY_MID => TransparencyTier::Mid,
                _ => TransparencyTier::High,
            };
            let config = app.state::<ConfigState>();
            let _ = config.set_transparency_tier(tier);

            if let Some(items) = app.try_state::<TransparencyMenuItems>() {
                items.sync_checked(tier);
            }

            // 挡位无论透明效果当前开没开都先存下来；只有透明效果开着的时候
            // 才需要立刻重新申请 Acrylic 把新 alpha 应用到 DWM 那一层——
            // 关着的话已经是纯色，档位只是记下来等下次重新打开时生效。
            let transparency_enabled = config.get().transparency_enabled;
            if transparency_enabled && let Some(window) = app.get_webview_window("main") {
                let level = window_fx::apply_with_fallback(&window, true, tier);
                app.state::<EffectLevelState>().set(level);
                let _ = window.emit("dowse://effect-level", level);
            }

            // CSS 那一层的 alpha 不管透明效果开没开都要广播给前端——纯色档
            // 下 app.css 的 `data-effect='solid'` 规则会整体覆盖掉
            // `--glass-tint`，这里更新变量不会有副作用，且能保证下次切回
            // 玻璃档时前端已经是最新值，不用等一次额外的往返。
            let _ = app.emit("dowse://glass-alpha", tier.glass_alpha());
        }
        MENU_QUIT => app.exit(0),
        _ => {}
    }
}

/// 后台线程里跑一次 `rebuild::perform_rebuild`，成功/失败各广播一个事件——
/// 托盘触发的重建没有 Tauri invoke 的返回值可用（不像浮窗按钮那样直接拿到
/// `rebuild_index` 命令的 Result），前端靠监听 `dowse://rebuild-done`/
/// `dowse://rebuild-error` 得知结果（沿用 v0.6.0 就有的这套事件）。
/// `RebuildGuard` 的独占权由调用方（`rebuild_from_last_dir`/
/// `change_index_folder`）在拿到目标目录之后、真正开始重建之前获取。
fn spawn_rebuild(app: &AppHandle, target: std::path::PathBuf) {
    let app = app.clone();
    std::thread::spawn(move || {
        let result = crate::rebuild::perform_rebuild(&app, target);
        app.state::<RebuildGuard>().end();
        match result {
            Ok(stats) => {
                let _ = app.emit("dowse://rebuild-done", stats.indexed);
            }
            Err(err) => {
                let _ = app.emit("dowse://rebuild-error", err);
            }
        }
    });
}

/// 托盘"重建索引"复用上次成功建索引的目录；还没配置过就呼出窗口，
/// 让用户走前端"选个目录开始建索引"的引导，而不是在托盘里悄悄失败。
fn rebuild_from_last_dir(app: &AppHandle) {
    let Some(target_dir) = app.state::<ConfigState>().get().target_dir else {
        if let Some(window) = app.get_webview_window("main") {
            window_fx::show_window(&window);
        }
        return;
    };
    if !app.state::<RebuildGuard>().try_begin() {
        return;
    }
    spawn_rebuild(app, target_dir);
}

/// 托盘"更改索引文件夹…"：弹系统目录选择器，选定后对新目录整次重建（等价于
/// 换根，旧根内容整体被替换——症状 5"可改"）。用户取消选择就什么都不做。
/// `RebuildGuard` 在弹出选择器之前就拿到，防止用户在对话框还开着的时候
/// 又从别的入口（浮窗按钮/"重建索引"）触发第二次重建；取消/拿不到路径时
/// 要记得放回去，不然这个入口会永久卡死在"重建中"。
fn change_index_folder(app: &AppHandle) {
    if !app.state::<RebuildGuard>().try_begin() {
        return;
    }
    let app = app.clone();
    app.dialog()
        .file()
        .set_title("选择要索引的目录")
        .pick_folder(move |folder| {
            let Some(folder) = folder else {
                app.state::<RebuildGuard>().end();
                return;
            };
            let Ok(target) = folder.into_path() else {
                app.state::<RebuildGuard>().end();
                return;
            };
            // spawn_rebuild 内部会在重建结束后调用 RebuildGuard::end()，
            // 这里不用重复放一次——独占权从"拿到"到"重建线程结束"是同一段
            // 生命周期，中间途经选择器回调，不能提前释放。
            spawn_rebuild(&app, target);
        });
}
