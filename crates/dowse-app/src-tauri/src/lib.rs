mod autohide;
mod commands;
mod config;
mod context_menu;
mod file_icons;
mod highlight;
mod indexing_status;
mod logging;
mod rebuild;
mod state;
mod tray;
mod watcher;
mod window_fx;

use tauri::{Manager, WindowEvent};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};

use autohide::AutoHideSuppressor;
use config::ConfigState;
use context_menu::ContextMenuTarget;
use file_icons::FileIconCache;
use indexing_status::IndexingStatus;
use rebuild::RebuildGuard;
use state::SearchState;
use watcher::WatchController;
use window_fx::EffectLevelState;

/// 默认全局呼出快捷键：Alt+`（反引号）。原先是 Alt+Space，跟部分用户机器上的
/// PowerToys Run 冲突，改成配置项后这个只是兜底默认值和解析失败时的回退。
fn default_shortcut() -> Shortcut {
    Shortcut::new(Some(Modifiers::ALT), Code::Backquote)
}

/// 从配置里的字符串解析快捷键，解析失败（比如手改配置文件写错了格式）
/// 就回退到默认值，不能让整个应用起不来。
fn parse_shortcut(hotkey: &str) -> Shortcut {
    hotkey.parse().unwrap_or_else(|err| {
        eprintln!("解析快捷键配置 \"{hotkey}\" 失败，回退到默认值: {err}");
        default_shortcut()
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // 越早越好：这一步之后所有的 eprintln!/println!（包括 dowse-core 内部
    // 各处排障日志）才会落进 `%LOCALAPPDATA%\dowse\logs\dowse.log`，
    // 而不是消失在 GUI 子系统没有控制台的黑洞里（见 logging.rs 的文档）。
    logging::init();

    let toggle = parse_shortcut(&config::load().hotkey);

    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_clipboard_manager::init())
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
        .manage(WatchController::new())
        .manage(FileIconCache::new())
        .manage(AutoHideSuppressor::new())
        .manage(ContextMenuTarget::new())
        .manage(IndexingStatus::new())
        .manage(RebuildGuard::new())
        .invoke_handler(tauri::generate_handler![
            commands::index_status,
            commands::indexing_status,
            commands::search,
            commands::preview,
            commands::open_file,
            commands::reveal_in_folder,
            commands::rebuild_index,
            commands::get_effect_level,
            commands::get_glass_alpha,
            commands::get_hotkey,
            commands::file_icon,
            commands::set_pinned,
            context_menu::show_result_context_menu,
        ])
        .setup(move |app| {
            // 快捷键抢注册失败（常见原因：被输入法或别的常驻工具占用了）
            // 不该让整个应用起不来——托盘的"呼出"菜单项还能用，把错误打到日志就行。
            match app.global_shortcut().register(toggle) {
                Ok(()) => logging::log_line("startup", &format!("已注册全局呼出快捷键: {toggle}")),
                Err(err) => logging::log_line(
                    "startup",
                    &format!("注册 {toggle} 全局快捷键失败，可能被别的程序占用了: {err}"),
                ),
            }

            let window = app
                .get_webview_window("main")
                .expect("tauri.conf.json 里定义的 main 窗口应该存在");

            // 结果行右键菜单（context_menu::show_result_context_menu）在这个窗口上
            // popup，选中项通过这里回调；托盘菜单是另一套独立的事件注册，见 tray.rs。
            window.on_menu_event(context_menu::handle_context_menu_event);

            let cfg = app.state::<ConfigState>().get();
            let level = window_fx::apply_with_fallback(
                &window,
                cfg.transparency_enabled,
                cfg.transparency_tier,
            );
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

            // 常驻监听：读索引里注册的根，先对账补齐停机期间的变更、再挂实时监听。
            // 索引不存在或 schema 需重建时读不到根，直接跳过——等用户重建后由
            // rebuild 流程把监听挂上。
            if let Ok(index_dir) = config::index_dir()
                && let Ok(roots) = dowse_core::registered_roots(&index_dir)
            {
                app.state::<WatchController>()
                    .start(app.handle().clone(), index_dir, roots);
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // 进程常驻，浮窗只是 show/hide：失焦即隐藏，符合 Spotlight/Raycast 的习惯，
            // 也避免用户切到别的窗口后浮窗还悬在最上层碍事。
            //
            // v0.5.0 加了"抑制自动隐藏"的豁免（见 autohide.rs）：结果行右键弹出
            // 原生菜单期间、以及用户点了图钉固定期间，这次失焦不该触发隐藏。
            // 注意这里只影响这一条自动隐藏路径——Esc（前端直接调
            // `getCurrentWindow().hide()`）和全局呼出快捷键的 `hide_window()`
            // 都不经过这里，固定状态不会拦住用户主动收起浮窗。
            if let WindowEvent::Focused(false) = event {
                if window.state::<AutoHideSuppressor>().is_suppressed() {
                    return;
                }
                let _ = window.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
