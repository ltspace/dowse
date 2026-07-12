use std::path::PathBuf;
use std::sync::Mutex;

use tauri::menu::{Menu, MenuEvent, MenuItemBuilder};
use tauri::{AppHandle, Manager, State, WebviewWindow};
use tauri_plugin_clipboard_manager::ClipboardExt;

use crate::autohide::AutoHideSuppressor;

const ITEM_OPEN: &str = "ctxmenu-open";
const ITEM_REVEAL: &str = "ctxmenu-reveal";
const ITEM_COPY_PATH: &str = "ctxmenu-copy-path";
const ITEM_COPY_NAME: &str = "ctxmenu-copy-name";

/// 右键菜单弹出时圈定的目标文件路径。`popup_menu` 是阻塞调用（见
/// `show_result_context_menu`），同一时刻只会有一个原生菜单在显示，所以
/// 用一个 `Mutex<Option<PathBuf>>` 记"这次菜单是对着哪个文件弹出的"就够，
/// 不需要把路径编进菜单项 id 里去绕开 Windows 路径里的特殊字符。
#[derive(Default)]
pub struct ContextMenuTarget(Mutex<Option<PathBuf>>);

impl ContextMenuTarget {
    pub fn new() -> Self {
        Self::default()
    }

    fn set(&self, path: PathBuf) {
        *self.0.lock().expect("context menu target mutex poisoned") = Some(path);
    }

    /// 取走并清空，避免上一次菜单的目标路径残留到下一次不相关的菜单事件上。
    fn take(&self) -> Option<PathBuf> {
        self.0
            .lock()
            .expect("context menu target mutex poisoned")
            .take()
    }
}

/// 在结果行右键处弹出 Win32 原生上下文菜单：打开 / 打开所在文件夹 /
/// 复制完整路径 / 复制文件名。菜单本身是系统绘制的（Tauri 的 `Menu::popup`
/// 底层是 `TrackPopupMenu`），不是前端画的假菜单——这正是"原生风格"的来源。
///
/// `popup_menu` 会阻塞到菜单被选中或取消才返回（Win32 `TrackPopupMenu` 是
/// 模态调用），选中项通过 `window.on_menu_event`（见 lib.rs 的 setup、本文件
/// 的 `handle_context_menu_event`）异步回调，不是这里的返回值。
#[tauri::command]
pub fn show_result_context_menu(
    app: AppHandle,
    window: WebviewWindow,
    target: State<ContextMenuTarget>,
    suppressor: State<AutoHideSuppressor>,
    path: String,
) -> Result<(), String> {
    target.set(PathBuf::from(&path));

    let s = crate::i18n::strings();
    let open_item = MenuItemBuilder::with_id(ITEM_OPEN, s.ctx_open)
        .build(&app)
        .map_err(|e| e.to_string())?;
    let reveal_item = MenuItemBuilder::with_id(ITEM_REVEAL, s.ctx_reveal)
        .build(&app)
        .map_err(|e| e.to_string())?;
    let copy_path_item = MenuItemBuilder::with_id(ITEM_COPY_PATH, s.ctx_copy_path)
        .build(&app)
        .map_err(|e| e.to_string())?;
    let copy_name_item = MenuItemBuilder::with_id(ITEM_COPY_NAME, s.ctx_copy_name)
        .build(&app)
        .map_err(|e| e.to_string())?;

    let menu = Menu::with_items(
        &app,
        &[&open_item, &reveal_item, &copy_path_item, &copy_name_item],
    )
    .map_err(|e| e.to_string())?;

    // 原生菜单抢焦点期间临时抑制失焦自动隐藏；guard 在本函数返回时释放
    // （不管菜单是被选中还是直接取消），不会因为提前 return 漏掉配对的释放。
    let _guard = suppressor.suppress_for_menu();
    window.popup_menu(&menu).map_err(|e| e.to_string())?;
    Ok(())
}

/// 注册在 `main` 窗口上（见 lib.rs 的 `setup`），处理该窗口弹出的原生菜单的
/// 选中事件——目前只有结果行右键菜单这一处会在这个窗口上弹菜单。跟托盘菜单
/// 的事件回调（tray.rs 的 `handle_menu_event`）是两套独立的注册，互不影响。
///
/// 四个动作里"打开"/"打开所在文件夹"直接调用 commands.rs 里已经给前端用的
/// 同一个函数（`open_file`/`reveal_in_folder` 本身就是普通 Rust 函数，
/// `#[tauri::command]` 只是给它们额外挂了 invoke 分发，直接调用不绕 IPC）；
/// 复制到剪贴板走 Rust 侧的剪贴板插件——原生菜单的选中事件发生在 Rust 侧，
/// 没有 JS 上下文可以调用前端已有的 `navigator.clipboard.writeText`，
/// 所以剪贴板这一步是新增的最小实现。
pub fn handle_context_menu_event(window: &tauri::Window, event: MenuEvent) {
    let id = event.id().as_ref();
    if ![ITEM_OPEN, ITEM_REVEAL, ITEM_COPY_PATH, ITEM_COPY_NAME].contains(&id) {
        // 不是本模块关心的菜单项（比如未来窗口菜单栏加了别的东西），不消费目标路径。
        return;
    }

    let app = window.app_handle();
    let Some(path) = app.state::<ContextMenuTarget>().take() else {
        return;
    };
    let path = path.to_string_lossy().into_owned();

    match id {
        ITEM_OPEN => {
            if let Err(err) = crate::commands::open_file(app.clone(), path) {
                eprintln!("右键菜单打开文件失败: {err}");
            }
        }
        ITEM_REVEAL => {
            if let Err(err) = crate::commands::reveal_in_folder(path) {
                eprintln!("右键菜单定位文件夹失败: {err}");
            }
        }
        ITEM_COPY_PATH => {
            if let Err(err) = app.clipboard().write_text(path) {
                eprintln!("右键菜单复制路径失败: {err}");
            }
        }
        ITEM_COPY_NAME => {
            let name = std::path::Path::new(&path)
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or(path);
            if let Err(err) = app.clipboard().write_text(name) {
                eprintln!("右键菜单复制文件名失败: {err}");
            }
        }
        _ => unreachable!("已在函数开头按白名单过滤过"),
    }
}
