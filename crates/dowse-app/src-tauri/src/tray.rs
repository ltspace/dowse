use tauri::image::Image;
use tauri::menu::{CheckMenuItemBuilder, Menu, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;

use crate::config::ConfigState;
use crate::state::SearchState;
use crate::watcher::WatchController;
use crate::window_fx::{self, EffectLevelState};

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
const MENU_AUTOSTART: &str = "autostart";
const MENU_TRANSPARENCY: &str = "transparency";
const MENU_QUIT: &str = "quit";

/// 托盘图标 + 右键菜单：呼出 / 重建索引 / 开机自启开关 / 透明效果开关 / 退出。
/// 进程常驻，浮窗只是 show/hide——托盘是用户确认"它还活着"和做少数配置的入口。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    let cfg = app.state::<ConfigState>().get();
    let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);

    let show_item = MenuItemBuilder::with_id(MENU_SHOW, "呼出").build(app)?;
    let rebuild_item = MenuItemBuilder::with_id(MENU_REBUILD, "重建索引").build(app)?;
    let autostart_item = CheckMenuItemBuilder::with_id(MENU_AUTOSTART, "开机自启")
        .checked(autostart_enabled)
        .build(app)?;
    let transparency_item = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY, "关闭透明效果")
        .checked(!cfg.transparency_enabled)
        .build(app)?;
    let quit_item = MenuItemBuilder::with_id(MENU_QUIT, "退出").build(app)?;

    let menu = Menu::with_items(
        app,
        &[
            &show_item,
            &rebuild_item,
            &PredefinedMenuItem::separator(app)?,
            &autostart_item,
            &transparency_item,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )?;

    let icon = tray_icon_image();

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("dowse — Alt+` 呼出")
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

    Ok(())
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    match event.id().as_ref() {
        MENU_SHOW => {
            if let Some(window) = app.get_webview_window("main") {
                window_fx::show_window(&window);
            }
        }
        MENU_REBUILD => rebuild_from_last_dir(app),
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
            if let Some(window) = app.get_webview_window("main") {
                let level = window_fx::apply_with_fallback(&window, now_enabled);
                app.state::<EffectLevelState>().set(level);
                let _ = window.emit("dowse://effect-level", level);
            }
        }
        MENU_QUIT => app.exit(0),
        _ => {}
    }
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

    let app = app.clone();
    std::thread::spawn(move || {
        let Ok(index_dir) = crate::config::index_dir() else {
            return;
        };
        // 重建前先停监听，放掉旧索引的写锁/文件句柄（Windows 删不掉被占用的目录）。
        app.state::<WatchController>().stop();
        match dowse_core::rebuild_index(&index_dir, &target_dir) {
            Ok(stats) => {
                if let Ok(searcher) = dowse_core::Searcher::open(&index_dir) {
                    app.state::<SearchState>().replace(searcher);
                }
                // 重建完盯住新索引根，重新挂上对账 + 实时监听。
                app.state::<WatchController>()
                    .start(index_dir, vec![target_dir]);
                let _ = app.emit("dowse://rebuild-done", stats.indexed);
            }
            Err(err) => {
                let _ = app.emit("dowse://rebuild-error", err.to_string());
            }
        }
    });
}
