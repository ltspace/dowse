use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

use tauri::image::Image;
use tauri::menu::{
    CheckMenuItemBuilder, IsMenuItem, Menu, MenuItemBuilder, PredefinedMenuItem, Submenu,
};
use tauri::tray::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager};
use tauri_plugin_autostart::ManagerExt as AutostartManagerExt;
use tauri_plugin_dialog::DialogExt;

use crate::config::ConfigState;
use crate::indexing_status::{IndexingPhase, IndexingStatus};
use crate::rebuild::RebuildGuard;
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
const MENU_FOLDERS_ADD: &str = "folders_add";
const MENU_AUTOSTART: &str = "autostart";
const MENU_TRANSPARENCY: &str = "transparency";
const MENU_TRANSPARENCY_LOW: &str = "transparency_low";
const MENU_TRANSPARENCY_MID: &str = "transparency_mid";
const MENU_TRANSPARENCY_HIGH: &str = "transparency_high";
const MENU_QUIT: &str = "quit";
/// 每根一个动态子菜单，"重建"/"移除"两个动作项的 id 按 `{前缀}{根在
/// registered_roots() 里的下标}` 拼——菜单每次状态变化都整个重建（见
/// `refresh_menu`），下标只在"这次构建出来的菜单还没被下一次重建替换掉"
/// 这段时间内有效，点击时会重新读一次 `registered_roots()` 按下标取值，
/// 取不到（极端情况下菜单显示的瞬间根列表恰好变了）就提示重新打开菜单，
/// 不会误删/误重建到别的根。
const FOLDER_REBUILD_PREFIX: &str = "folder_rebuild::";
const FOLDER_REMOVE_PREFIX: &str = "folder_remove::";

/// 托盘图标句柄。多根索引（里程碑 7）之后菜单本身不再长期持有单个菜单项的
/// 句柄去做局部 `.set_text()`/`.set_checked()`——根的数量、每根的文档数、
/// 忙碌态都会变，改成"每次状态变化就整份重建 Menu，`set_menu` 整体换上去"
/// （`refresh_menu`），简单直接，不用为"这一项该不该跟着这次变化更新"操心。
pub struct TrayHandles {
    icon: TrayIcon<tauri::Wry>,
}

/// 是否有一次索引操作（全量重建/添加根/移除根/重建单根）正在进行——托盘
/// "索引文件夹"子菜单里跟根相关的动作项在忙碌期间整体置灰，防止重入。
pub struct TrayBusy(AtomicBool);

impl TrayBusy {
    pub fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn get(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }

    fn set(&self, busy: bool) {
        self.0.store(busy, Ordering::Release);
    }
}

/// 托盘图标 + 右键菜单：呼出 / "索引文件夹"子菜单（每根一项 + 添加文件夹…）
/// / 开机自启开关 / 透明效果开关 / 透明度三档子菜单 / 退出。进程常驻，浮窗
/// 只是 show/hide——托盘是用户确认"它还活着"、看一眼索引状态、做少数配置的
/// 入口。
pub fn build(app: &AppHandle) -> tauri::Result<()> {
    app.manage(TrayBusy::new());

    let menu = build_menu(app, false)?;
    let icon = tray_icon_image();

    let tray_icon = TrayIconBuilder::new()
        .icon(icon)
        .tooltip(crate::i18n::strings().idle_tooltip)
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

    app.manage(TrayHandles { icon: tray_icon });

    Ok(())
}

/// 从当前状态（配置/自启/根列表/忙碌态）整份重建菜单树，换到托盘图标上。
/// 添加/移除/重建根完成后、忙碌态切换、透明度/自启配置变化时都调这个——
/// 单一入口，不用惦记"这次变化该更新菜单里的哪几项"。
pub fn refresh_menu(app: &AppHandle) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let busy = app
        .try_state::<TrayBusy>()
        .map(|b| b.get())
        .unwrap_or(false);
    match build_menu(app, busy) {
        Ok(menu) => {
            let _ = handles.icon.set_menu(Some(menu));
        }
        Err(err) => eprintln!("重建托盘菜单失败: {err}"),
    }
}

fn build_menu(app: &AppHandle, busy: bool) -> tauri::Result<Menu<tauri::Wry>> {
    let cfg = app.state::<ConfigState>().get();
    let autostart_enabled = app.autolaunch().is_enabled().unwrap_or(false);
    let s = crate::i18n::strings();

    let show_item = MenuItemBuilder::with_id(MENU_SHOW, s.menu_show).build(app)?;
    let folders_submenu = build_folders_submenu(app, busy)?;

    let autostart_item = CheckMenuItemBuilder::with_id(MENU_AUTOSTART, s.menu_autostart)
        .checked(autostart_enabled)
        .build(app)?;
    let transparency_item =
        CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY, s.menu_transparency_off)
            .checked(!cfg.transparency_enabled)
            .build(app)?;

    let tier = cfg.transparency_tier;
    let tier_low = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY_LOW, s.tier_low)
        .checked(tier == TransparencyTier::Low)
        .build(app)?;
    let tier_mid = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY_MID, s.tier_mid)
        .checked(tier == TransparencyTier::Mid)
        .build(app)?;
    let tier_high = CheckMenuItemBuilder::with_id(MENU_TRANSPARENCY_HIGH, s.tier_high)
        .checked(tier == TransparencyTier::High)
        .build(app)?;
    let tier_submenu = Submenu::with_items(
        app,
        s.tier_submenu,
        true,
        &[&tier_low, &tier_mid, &tier_high],
    )?;

    let quit_item = MenuItemBuilder::with_id(MENU_QUIT, s.menu_quit).build(app)?;

    Menu::with_items(
        app,
        &[
            &show_item,
            &folders_submenu,
            &PredefinedMenuItem::separator(app)?,
            &autostart_item,
            &transparency_item,
            &tier_submenu,
            &PredefinedMenuItem::separator(app)?,
            &quit_item,
        ],
    )
}

/// "索引文件夹"子菜单：每个已注册根一项（本身是个嵌套子菜单，标题是
/// "路径 · N 篇"，子项"重建"/"移除"），末尾"添加文件夹…"。索引还没建过、
/// 或者 schema 需要重建（`registered_roots` 读不出来）时没有根可列，
/// 子菜单只剩"添加文件夹…"一项——点它会走全量重建的引导流程（等价于
/// 浮窗空态"选择目录并建索引"），这也是升级后"旧索引读不到根"时唯一
/// 需要用户手动干预的路径。
fn build_folders_submenu(app: &AppHandle, busy: bool) -> tauri::Result<Submenu<tauri::Wry>> {
    let roots = crate::config::index_dir()
        .ok()
        .and_then(|dir| dowse::registered_roots(&dir).ok())
        .unwrap_or_default();

    let s = crate::i18n::strings();
    let mut items: Vec<Box<dyn IsMenuItem<tauri::Wry>>> = Vec::new();
    for (idx, root) in roots.iter().enumerate() {
        let docs = root_doc_count(app, root);
        let label = format!(
            "{} · {} {}",
            dowse::display_path(&root.to_string_lossy()),
            crate::rebuild::format_count(docs),
            s.root_docs_unit
        );
        let rebuild_item =
            MenuItemBuilder::with_id(format!("{FOLDER_REBUILD_PREFIX}{idx}"), s.rebuild_item)
                .enabled(!busy)
                .build(app)?;
        let remove_item =
            MenuItemBuilder::with_id(format!("{FOLDER_REMOVE_PREFIX}{idx}"), s.remove_item)
                .enabled(!busy)
                .build(app)?;
        let root_submenu = Submenu::with_items(app, label, true, &[&rebuild_item, &remove_item])?;
        items.push(Box::new(root_submenu));
    }

    if !roots.is_empty() {
        items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    let add_item = MenuItemBuilder::with_id(MENU_FOLDERS_ADD, s.add_folder_item)
        .enabled(!busy)
        .build(app)?;
    items.push(Box::new(add_item));

    let refs: Vec<&dyn IsMenuItem<tauri::Wry>> = items.iter().map(|b| b.as_ref()).collect();
    Submenu::with_items(app, s.folders_submenu, true, &refs)
}

fn root_doc_count(app: &AppHandle, root: &std::path::Path) -> u64 {
    app.state::<SearchState>()
        .0
        .lock()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(|s| s.count_under(root).ok()))
        .unwrap_or(0)
}

/// 把托盘 tooltip 同步成 `IndexingStatus` 当前快照对应的文案——空闲时是
/// 默认的呼出提示，文本/OCR 两个阶段各自换成"索引中 N 篇"/"图片识别 N / M"，
/// 窗口不用开着也能瞟一眼进度（症状 2/3 的验收场景之一）。
pub fn refresh_tooltip(app: &AppHandle) {
    let Some(handles) = app.try_state::<TrayHandles>() else {
        return;
    };
    let snapshot = app.state::<IndexingStatus>().snapshot();
    let s = crate::i18n::strings();
    let tooltip = match snapshot.phase {
        IndexingPhase::Idle => s.idle_tooltip.to_string(),
        IndexingPhase::Text => format!(
            "{}{}{}",
            s.tooltip_indexing_prefix,
            crate::rebuild::format_count(snapshot.text_processed as u64),
            s.tooltip_indexing_suffix
        ),
        IndexingPhase::Ocr => format!(
            "{}{} / {}",
            s.tooltip_ocr_prefix,
            crate::rebuild::format_count(snapshot.ocr_processed as u64),
            crate::rebuild::format_count(snapshot.ocr_total as u64)
        ),
    };
    let _ = handles.icon.set_tooltip(Some(tooltip));
}

/// 索引操作（全量重建/添加根/移除根/重建单根）期间把忙碌态置上/置下，
/// 顺带整份重建一次菜单——`build_folders_submenu` 据此把根相关的动作项
/// 整体置灰/恢复，防止重入。
pub fn set_busy(app: &AppHandle, busy: bool) {
    if let Some(state) = app.try_state::<TrayBusy>() {
        state.set(busy);
    }
    refresh_menu(app);
}

/// 透明效果开关的单一实现：托盘"关闭透明效果"菜单项和设置面板的
/// `set_transparency_enabled` 命令都走这里，`refresh_menu` 保证托盘勾选态
/// 跟面板永远同步，`dowse://effect-level` 事件让前端 CSS 兜底层跟上。
/// 传入的是目标状态（不是 toggle），托盘那边自己先算出"取反"再调进来。
pub fn apply_transparency_enabled(app: &AppHandle, enabled: bool) {
    let config = app.state::<ConfigState>();
    let _ = config.set_transparency_enabled(enabled);
    let tier = config.get().transparency_tier;
    if let Some(window) = app.get_webview_window("main") {
        let level = window_fx::apply_with_fallback(&window, enabled, tier);
        app.state::<EffectLevelState>().set(level);
        let _ = window.emit("dowse://effect-level", level);
    }
    refresh_menu(app);
}

/// 透明度三档的单一实现：托盘子菜单和设置面板的 `set_transparency_tier`
/// 命令共用。挡位无论透明效果当前开没开都先落盘；只有开着时才立刻重新申请
/// Acrylic 把新 alpha 送进 DWM 层。CSS 层的 alpha（`dowse://glass-alpha`）
/// 不管开没开都广播——纯色档下前端会被 `data-effect='solid'` 覆盖，更新
/// 变量无副作用，还能保证下次切回玻璃档时前端已是最新值。`refresh_menu`
/// 让托盘三档勾选态跟面板同步。
pub fn apply_transparency_tier(app: &AppHandle, tier: TransparencyTier) {
    let config = app.state::<ConfigState>();
    let _ = config.set_transparency_tier(tier);

    let transparency_enabled = config.get().transparency_enabled;
    if transparency_enabled && let Some(window) = app.get_webview_window("main") {
        let level = window_fx::apply_with_fallback(&window, true, tier);
        app.state::<EffectLevelState>().set(level);
        let _ = window.emit("dowse://effect-level", level);
    }

    let _ = app.emit("dowse://glass-alpha", tier.glass_alpha());
    refresh_menu(app);
}

/// 开机自启的单一实现：托盘"开机自启"菜单项和设置面板的 `set_autostart`
/// 命令共用。传入目标状态（托盘那边先读当前态再取反调进来）。`enable` 落地
/// 后同步记下 `autostart_user_disabled`——下次启动"默认开"逻辑只在"用户没
/// 主动关过"时才生效，不能覆盖用户选择。成功/失败都 `refresh_menu` 让托盘
/// 勾选态跟真实状态一致；失败把错误返回给调用方（命令据此报给前端）。
pub fn apply_autostart(app: &AppHandle, enable: bool) -> Result<(), String> {
    let mgr = app.autolaunch();
    let res = if enable { mgr.enable() } else { mgr.disable() };
    let out = match res {
        Ok(()) => {
            let _ = app
                .state::<ConfigState>()
                .set_autostart_user_disabled(!enable);
            Ok(())
        }
        Err(err) => {
            eprintln!("切换开机自启失败: {err}");
            Err(format!("{err}"))
        }
    };
    refresh_menu(app);
    out
}

fn handle_menu_event(app: &AppHandle, event: tauri::menu::MenuEvent) {
    let id = event.id().as_ref();
    match id {
        MENU_SHOW => {
            if let Some(window) = app.get_webview_window("main") {
                window_fx::show_window(&window);
            }
        }
        MENU_FOLDERS_ADD => add_folder(app),
        MENU_AUTOSTART => {
            // 托盘是 toggle 语义：读当前态、取反，交给共用实现落地。
            let enabled = app.autolaunch().is_enabled().unwrap_or(false);
            let _ = apply_autostart(app, !enabled);
        }
        MENU_TRANSPARENCY => {
            let now_enabled = !app.state::<ConfigState>().get().transparency_enabled;
            apply_transparency_enabled(app, now_enabled);
        }
        MENU_TRANSPARENCY_LOW | MENU_TRANSPARENCY_MID | MENU_TRANSPARENCY_HIGH => {
            let tier = match id {
                MENU_TRANSPARENCY_LOW => TransparencyTier::Low,
                MENU_TRANSPARENCY_MID => TransparencyTier::Mid,
                _ => TransparencyTier::High,
            };
            apply_transparency_tier(app, tier);
        }
        MENU_QUIT => app.exit(0),
        _ if id.starts_with(FOLDER_REBUILD_PREFIX) => {
            if let Ok(idx) = id[FOLDER_REBUILD_PREFIX.len()..].parse::<usize>() {
                rebuild_folder(app, idx);
            }
        }
        _ if id.starts_with(FOLDER_REMOVE_PREFIX) => {
            if let Ok(idx) = id[FOLDER_REMOVE_PREFIX.len()..].parse::<usize>() {
                remove_folder(app, idx);
            }
        }
        _ => {}
    }
}

/// 按菜单构建时的下标重新读一次当前根列表取值——菜单从"这次重建"到"用户
/// 点击"之间理论上可能又发生了一次根变化（比如恰好另一次操作也完成了），
/// 下标失配（越界）时返回 `None`，调用方据此提示用户重新打开菜单，不会
/// 误伤到下标对应的、实际上是另一个根。
fn resolve_root_by_index(idx: usize) -> Option<PathBuf> {
    let index_dir = crate::config::index_dir().ok()?;
    let roots = dowse::registered_roots(&index_dir).ok()?;
    roots.into_iter().nth(idx)
}

/// 托盘"添加文件夹…"：弹系统目录选择器，选定后视索引现状决定走哪条路——
/// 索引还没建过（`registered_roots` 读不出来，包含"从没建过"和"schema
/// 需要重建"两种情形）就走全量重建的引导流程（等价于浮窗空态"选择目录并
/// 建索引"）；已经有索引就走 `add_root`（多根索引核心操作，不动现有内容）。
/// 两条路径统一在这里判断，托盘和浮窗（`commands::add_root`）复用同一个
/// 判断，不会出现"这个入口忘了判断该走哪条路"的偏差。
fn add_folder(app: &AppHandle) {
    if !app.state::<RebuildGuard>().try_begin() {
        return;
    }
    let app = app.clone();
    app.dialog()
        .file()
        .set_title(crate::i18n::strings().dialog_pick_folder)
        .pick_folder(move |folder| {
            let Some(folder) = folder else {
                app.state::<RebuildGuard>().end();
                return;
            };
            let Ok(target) = folder.into_path() else {
                app.state::<RebuildGuard>().end();
                return;
            };
            spawn_add_or_bootstrap(&app, target);
        });
}

fn has_existing_index() -> bool {
    crate::config::index_dir()
        .ok()
        .and_then(|dir| dowse::registered_roots(&dir).ok())
        .is_some()
}

fn spawn_add_or_bootstrap(app: &AppHandle, target: PathBuf) {
    let bootstrap = !has_existing_index();
    let app = app.clone();
    std::thread::spawn(move || {
        let result = if bootstrap {
            crate::rebuild::perform_rebuild(&app, target)
        } else {
            crate::rebuild::perform_add_root(&app, target)
        };
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

/// 托盘每根子菜单的"重建"动作：按下标解出根路径，走
/// `rebuild::perform_rebuild_root`（= 移除根 + 添加根），成功/失败复用
/// `dowse://rebuild-done`/`dowse://rebuild-error` 事件，浮窗开着的话会照常
/// 收到刷新（跟托盘触发全量重建的既有事件通道一致）。
fn rebuild_folder(app: &AppHandle, idx: usize) {
    if !app.state::<RebuildGuard>().try_begin() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let outcome = resolve_root_by_index(idx)
            .ok_or_else(|| crate::i18n::strings().stale_root_error.to_string())
            .and_then(|root| crate::rebuild::perform_rebuild_root(&app, root));
        app.state::<RebuildGuard>().end();
        match outcome {
            Ok(stats) => {
                let _ = app.emit("dowse://rebuild-done", stats.indexed);
            }
            Err(err) => {
                let _ = app.emit("dowse://rebuild-error", err);
            }
        }
    });
}

/// 托盘每根子菜单的"移除"动作：按下标解出根路径，走
/// `rebuild::perform_remove_root`。移除没有"收录数"可言，成功时走独立的
/// `dowse://root-removed` 事件（携带删除的文档数），失败复用
/// `dowse://rebuild-error`（错误文案本身已经足够说明是哪类操作失败）。
fn remove_folder(app: &AppHandle, idx: usize) {
    if !app.state::<RebuildGuard>().try_begin() {
        return;
    }
    let app = app.clone();
    std::thread::spawn(move || {
        let outcome = resolve_root_by_index(idx)
            .ok_or_else(|| crate::i18n::strings().stale_root_error.to_string())
            .and_then(|root| crate::rebuild::perform_remove_root(&app, root));
        app.state::<RebuildGuard>().end();
        match outcome {
            Ok(stats) => {
                let _ = app.emit("dowse://root-removed", stats.removed);
            }
            Err(err) => {
                let _ = app.emit("dowse://rebuild-error", err);
            }
        }
    });
}
