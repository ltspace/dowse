mod commands;
mod config;
mod highlight;
mod state;
mod tray;
mod window_fx;

use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use config::ConfigState;
use state::SearchState;
use window_fx::EffectLevelState;

/// 全局呼出快捷键：Alt+Space。已跟用户确认过，不做成可配置项（M2 范围内）。
fn toggle_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::ALT), Code::Space)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let toggle = toggle_shortcut();

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(move |app, shortcut, event| {
                    if *shortcut == toggle
                        && event.state() == ShortcutState::Pressed
                        && let Some(window) = app.get_webview_window("main")
                    {
                        window_fx::toggle_window(&window);
                    }
                })
                .build(),
        )
        .manage(ConfigState::new())
        .manage(SearchState::load_initial())
        .invoke_handler(tauri::generate_handler![
            commands::index_status,
            commands::search,
            commands::preview,
            commands::open_file,
            commands::reveal_in_folder,
            commands::rebuild_index,
            commands::get_effect_level,
        ])
        .setup(move |app| {
            // 快捷键抢注册失败（常见原因：被输入法或别的常驻工具占用了 Alt+Space）
            // 不该让整个应用起不来——托盘的"呼出"菜单项还能用，把错误打到日志就行。
            if let Err(err) = app.global_shortcut().register(toggle) {
                eprintln!("注册 Alt+Space 全局快捷键失败，可能被别的程序占用了: {err}");
            }

            let window = app
                .get_webview_window("main")
                .expect("tauri.conf.json 里定义的 main 窗口应该存在");

            let cfg = app.state::<ConfigState>().get();
            let level = window_fx::apply_with_fallback(&window, cfg.transparency_enabled);
            app.manage(EffectLevelState::new(level));
            let _ = window_fx::position_upper_center(&window);

            // 设计文档："开机自启（可在托盘菜单关掉）"——默认开。只在用户没有
            // 主动关过的前提下才去抢着开，不然每次启动都会把用户关掉的选项
            // 悄悄打开回去。
            if !cfg.autostart_user_disabled {
                let mgr = app.autolaunch();
                if !mgr.is_enabled().unwrap_or(true)
                    && let Err(err) = mgr.enable()
                {
                    eprintln!("默认开启开机自启失败: {err}");
                }
            }

            tray::build(app.handle())?;

            Ok(())
        })
        .on_window_event(|window, event| {
            // 进程常驻，浮窗只是 show/hide：失焦即隐藏，符合 Spotlight/Raycast 的习惯，
            // 也避免用户切到别的窗口后浮窗还悬在最上层碍事。
            if let WindowEvent::Focused(false) = event {
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
