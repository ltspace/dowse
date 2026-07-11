use tauri::menu::{CheckMenuItemBuilder, Menu, MenuItemBuilder, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;

use crate::config::ConfigState;
use crate::state::SearchState;
use crate::window_fx::{self, EffectLevelState};

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

    let icon = app
        .default_window_icon()
        .cloned()
        .expect("打包时应该内嵌了默认图标");

    TrayIconBuilder::new()
        .icon(icon)
        .tooltip("dowse — Alt+Space 呼出")
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
        match dowse_core::rebuild_index(&index_dir, &target_dir) {
            Ok(stats) => {
                if let Ok(searcher) = dowse_core::Searcher::open(&index_dir) {
                    app.state::<SearchState>().replace(searcher);
                }
                let _ = app.emit("dowse://rebuild-done", stats.indexed);
            }
            Err(err) => {
                let _ = app.emit("dowse://rebuild-error", err.to_string());
            }
        }
    });
}
