use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{Manager, State};
use tauri_plugin_opener::OpenerExt;

use crate::autohide::AutoHideSuppressor;
use crate::config::ConfigState;
use crate::file_icons::FileIconCache;
use crate::highlight::{TextSegment, highlight_name, segments_from_ranges};
use crate::indexing_status::{IndexingSnapshot, IndexingStatus};
use crate::logging;
use crate::perf::HotkeyPerfState;
use crate::rebuild::{IndexStatsDto, RebuildGuard};
use crate::state::SearchState;
use crate::window_fx::{self, EffectLevel, EffectLevelState, GlassAlpha};

#[derive(Serialize)]
pub struct SearchHitDto {
    /// 打开文件、在资源管理器定位用这个原始值——canonicalize 出来的
    /// `\\?\` 前缀不能剥，剥了长路径场景会重新撞上 Win32 的 MAX_PATH 限制。
    pub path: String,
    /// 结果行、预览区渲染路径文本专用，`\\?\`/`\\?\UNC\` 前缀已经剥掉。
    /// 只用于展示，不要拿去做文件操作。
    pub display_path: String,
    /// 拆出来单独给前端渲染文件名那一行，省得前端再解析一遍路径。
    pub name: String,
    pub name_segments: Vec<TextSegment>,
    pub snippet_segments: Vec<TextSegment>,
    pub score: f32,
}

#[derive(Serialize)]
pub struct PreviewDto {
    pub segments: Vec<TextSegment>,
}

#[derive(Serialize)]
pub struct IndexStatusDto {
    pub has_index: bool,
    pub num_docs: u64,
    /// 已注册的全部索引根，已经过 `dowse::display_path` 清洗（剥掉
    /// Windows 扩展长度路径的 `\\?\`/`\\?\UNC\` 前缀）——这是"给人看的路径
    /// 一律过 display_path"这条规矩在多根场景下唯一的出口，前端拿到手就是
    /// 可以直接渲染的文本，不用（也不应该）自己再处理一遍。空态浮窗据此列出
    /// 全部根（症状 5：选完目录之后要能看得见）。
    pub roots: Vec<String>,
}

fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// 前端启动时查一次当前生效的材质级别（Acrylic/Mica/纯色），
/// 决定要不要自己叠一层纯色背景兜底。托盘切换透明开关之后的更新走
/// `dowse://effect-level` 事件，这个 command 只覆盖启动时的初值。
#[tauri::command]
pub fn get_effect_level(state: State<EffectLevelState>) -> EffectLevel {
    state.get()
}

/// 前端启动时查一次当前透明度挡位对应的 CSS alpha（明/暗两套主题各一个
/// 数），用来给 `--glass-alpha-light`/`--glass-alpha-dark` 赋初值。托盘切
/// 挡位之后的更新走 `dowse://glass-alpha` 事件，这个 command 只覆盖启动时
/// 的初值——跟 `get_effect_level` 是同一套"启动查询 + 事件更新"分工。
#[tauri::command]
pub fn get_glass_alpha(config: State<ConfigState>) -> GlassAlpha {
    config.get().transparency_tier.glass_alpha()
}

/// 快捷键速查浮层（Ctrl+/）要显示"呼出"这一行实际绑定的全局快捷键，而不是
/// 硬编码默认值——`hotkey` 目前只能手改配置文件，真改过的话浮层不该继续
/// 显示旧的默认值。原样返回 `tauri-plugin-global-shortcut` 认的格式化字符串
/// （如 "Alt+Backquote"），人类可读的转换（Backquote -> `）交给前端做，
/// 跟其它 DTO 一样"Rust 只管传值，展示格式前端定"。
#[tauri::command]
pub fn get_hotkey(config: State<ConfigState>) -> String {
    config.get().hotkey
}

/// 前端打开浮窗/挂载时调用一次，用来决定空输入/无索引/有索引三种引导状态。
/// 根列表直接读索引的 meta（`dowse::registered_roots`），不是走
/// `ConfigState::target_dir`——多根索引之后 meta 里的 roots 才是唯一可信的
/// 来源（`ConfigState::target_dir` 只在"从没建过索引"的引导流程里还有用）。
#[tauri::command]
pub fn index_status(search: State<SearchState>) -> IndexStatusDto {
    let guard = search.0.lock().expect("search state mutex poisoned");
    let (has_index, num_docs) = match guard.as_ref() {
        Some(s) => (true, s.num_docs()),
        None => (false, 0),
    };
    let roots = crate::config::index_dir()
        .ok()
        .and_then(|dir| dowse::registered_roots(&dir).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|p| dowse::display_path(&p.to_string_lossy()))
        .collect();
    IndexStatusDto {
        has_index,
        num_docs,
        roots,
    }
}

/// 建索引进度的当前快照——窗口每次呼出都应该拉一次这个，再接续事件流
/// （`dowse://rebuild-progress`/`dowse://ocr-progress`）：窗口隐藏期间事件照样
/// 会发，但前端没监听、没地方存，重新唤出时必须能补一次，不能是一片空白
/// 或者停在呼出前那一刻的旧快照。
#[tauri::command]
pub fn indexing_status(status: State<IndexingStatus>) -> IndexingSnapshot {
    status.snapshot()
}

/// 浮窗"类型/排序"两个幽灵态下拉的取值透传到这里，翻译成 dowse 的
/// 分组常量/`SortMode`——语义（哪个字符串对应哪组扩展名、哪种排序）由
/// dowse 一处定义，Tauri 这层只是原样转发字符串，不在这里重复一份映射表。
/// `ext_group`/`sort` 都是可选参数：不传、传 "all"/未知字符串都表示不筛选/
/// 用默认相关性排序，前端传坏了也不会让搜索报错。
#[tauri::command]
pub fn search(
    search: State<SearchState>,
    query: String,
    limit: usize,
    ext_group: Option<String>,
    sort: Option<String>,
) -> Result<Vec<SearchHitDto>, String> {
    let guard = search.0.lock().map_err(|_| "搜索状态异常".to_string())?;
    let Some(searcher) = guard.as_ref() else {
        return Ok(Vec::new());
    };

    let group = dowse::ext_group_by_name(ext_group.as_deref());
    let sort_mode = dowse::SortMode::parse(sort.as_deref());

    let hits = searcher
        .search_advanced(&query, limit, group, sort_mode)
        .map_err(|e| e.to_string())?;
    Ok(hits
        .into_iter()
        .map(|hit| {
            let name = file_name_of(&hit.path);
            let name_segments = highlight_name(&name, &query);
            let snippet_segments = segments_from_ranges(&hit.snippet, &hit.highlighted);
            SearchHitDto {
                display_path: dowse::display_path(&hit.path),
                path: hit.path,
                name,
                name_segments,
                snippet_segments,
                score: hit.score,
            }
        })
        .collect())
}

/// 结果列表选中一行后，取更长的命中上下文给预览区。
#[tauri::command]
pub fn preview(
    search: State<SearchState>,
    path: String,
    query: String,
) -> Result<Option<PreviewDto>, String> {
    let guard = search.0.lock().map_err(|_| "搜索状态异常".to_string())?;
    let Some(searcher) = guard.as_ref() else {
        return Ok(None);
    };
    let Some(hit) = searcher.preview(&path, &query).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    Ok(Some(PreviewDto {
        segments: segments_from_ranges(&hit.snippet, &hit.highlighted),
    }))
}

/// 按扩展名取系统关联图标（PNG base64 data URI），取不到返回 `None`——前端据此
/// 回落到手绘的通用图标，不把"系统没有这个图标"当错误处理。`ext` 不带点，
/// 空字符串代表无扩展名文件。结果按扩展名缓存在 `FileIconCache` 里，
/// 一屏结果里一堆同后缀的文件只问系统一次。
#[tauri::command]
pub fn file_icon(cache: State<FileIconCache>, ext: String) -> Option<String> {
    cache.get(&ext)
}

/// 用系统默认程序打开文件。
#[tauri::command]
pub fn open_file(app: tauri::AppHandle, path: String) -> Result<(), String> {
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| e.to_string())
}

/// 在文件资源管理器里定位并选中该文件——`explorer /select,"path"`。
/// 用 raw_arg 拼命令行而不是 .arg()：explorer 要求 `/select,"path"` 是
/// 一个整体 token，Rust 默认的参数转义会把 `/select,` 和路径分开加引号，
/// explorer 识别不出来。
///
/// `path` 直接拼进 raw_arg，评审确认过双引号内没法逃逸出去构造额外参数
/// （explorer 把 `/select,"..."` 当一个整体 token 解析，不存在 shell 那种
/// 分词/命令拼接的注入面）。不过 spawn 前照样校验一下路径存在——防的不是
/// 注入，是把这条命令喂给一个根本不存在的路径时 explorer 的行为不可控
/// （可能弹出无关的默认窗口），加固成本很低，顺手做。
#[cfg(target_os = "windows")]
#[tauri::command]
pub fn reveal_in_folder(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    if !Path::new(&path).exists() {
        return Err("目标路径不存在".to_string());
    }
    let arg = format!("/select,\"{path}\"");
    std::process::Command::new("explorer")
        .raw_arg(arg)
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(not(target_os = "windows"))]
#[tauri::command]
pub fn reveal_in_folder(_path: String) -> Result<(), String> {
    Err("目前只支持 Windows".to_string())
}

/// 全量重建索引。目标目录来自前端的目录选择器（或托盘"重建索引"/"更改索引
/// 文件夹…"复用/新选的目录）。实际工作全部在 `rebuild::perform_rebuild`
/// 里——浮窗按钮、托盘两个菜单项三个入口共用同一份实现，行为保证一致。
///
/// 建索引期间通过 `dowse://rebuild-progress` 事件把进度实时推给前端（浮窗的
/// "实时直播"效果），频率由 dowse 的 `PROGRESS_INTERVAL` 控制。
/// `RebuildGuard` 防止这个命令和托盘的重建入口并发触发——已经有一次在跑
/// 就直接报错，不会互相踩踏。
#[tauri::command]
pub fn rebuild_index(app: tauri::AppHandle, dir: String) -> Result<IndexStatsDto, String> {
    if !app.state::<RebuildGuard>().try_begin() {
        return Err("已有一次建索引正在进行中，请稍候".to_string());
    }
    let target = PathBuf::from(&dir).canonicalize().map_err(|_| {
        app.state::<RebuildGuard>().end();
        "目录不存在".to_string()
    })?;

    let result = crate::rebuild::perform_rebuild(&app, target);
    app.state::<RebuildGuard>().end();
    result
}

/// 添加一个索引根（多根索引，里程碑 7）。浮窗空态"添加文件夹"链接走这个
/// 命令——跟 `rebuild_index` 是姊妹命令：都由 `rebuild::perform_*` 实现、
/// 都用 `RebuildGuard` 防并发、都靠 `dowse://rebuild-progress` 事件直播进度，
/// 唯一区别是这个不动现有索引内容，只对新根做一次目录树 upsert。
#[tauri::command]
pub fn add_root(app: tauri::AppHandle, dir: String) -> Result<IndexStatsDto, String> {
    if !app.state::<RebuildGuard>().try_begin() {
        return Err("已有一次建索引正在进行中，请稍候".to_string());
    }
    let target = PathBuf::from(&dir);

    let result = crate::rebuild::perform_add_root(&app, target);
    app.state::<RebuildGuard>().end();
    result
}

/// 图钉固定开关：前端点了图钉按钮就调这个命令，把会话级的"抑制失焦自动
/// 隐藏"状态同步到 Rust 侧（见 autohide.rs 的 `AutoHideSuppressor`）。
/// 不落盘——重启应用后前端按钮状态和这里的计数器都回到初始值。
#[tauri::command]
pub fn set_pinned(suppressor: State<AutoHideSuppressor>, pinned: bool) {
    suppressor.set_pinned(pinned);
}

/// 索引规则面板的读取入口：取当前索引目录旁 rules.json 里的规则（没配过就
/// 是逐字节等于老硬编码行为的默认值，见 `dowse::rules` 模块文档）。面板
/// Ctrl+, 打开时调一次拿表单初值。索引目录跟 `rebuild_index`/`add_root`
/// 用的是同一个（`crate::config::index_dir()`，固定在
/// `%LOCALAPPDATA%\dowse\index`），不需要额外传参区分。
#[tauri::command]
pub fn get_rules() -> Result<dowse::IndexRules, String> {
    let dir = crate::config::index_dir().map_err(|e| e.to_string())?;
    Ok(dowse::load_rules(&dir))
}

/// 索引规则面板的保存入口。`max_file_mb` 兜底至少 1——0 或负数（前端数字
/// 输入框理论上传不出负数，但 0 是合法的用户输入）会让所有文件都判定超限
/// 跳过，没有意义。列表项的 trim/去空/大小写/去重统一交给
/// `IndexRules::normalize`，跟 CLI `dowse rules set` 落盘前那一步是同一份
/// 逻辑，两个入口保存出来的规则文件形态一致。
///
/// 只落盘规则文件，不在这里顺带触发重建——规则改了但索引没重建之前，
/// 新规则不会立刻生效（见 `dowse::rules` 模块文档"改规则后需重建索引才
/// 完全生效"），要不要立刻重建交给用户在面板上点"立即重建"决定。
#[tauri::command]
pub fn set_rules(
    exclude_dirs: Vec<String>,
    extra_text_exts: Vec<String>,
    max_file_mb: u64,
) -> Result<dowse::IndexRules, String> {
    let dir = crate::config::index_dir().map_err(|e| e.to_string())?;
    let mut rules = dowse::IndexRules {
        exclude_dirs,
        extra_text_exts,
        max_file_mb: max_file_mb.max(1),
    };
    rules.normalize();
    dowse::save_rules(&dir, &rules).map_err(|e| e.to_string())?;
    Ok(rules)
}

/// Esc 收起浮窗。前端原先直接调 JS 侧 `getCurrentWindow().hide()`，那是
/// Tauri core 插件的 `window|hide` 权限点，默认 capability（`core:default`）
/// 不包含它，真机上被 ACL 拒绝、Esc 按了没反应。这里改成走自定义命令，
/// 复用全局呼出快捷键同一条 `window_fx::hide_window` 路径——自定义命令不受
/// ACL 权限点约束，比在 capabilities 里放开 `core:window:allow-hide`
/// 权限面更小。
#[tauri::command]
pub fn hide_window(window: tauri::WebviewWindow) {
    window_fx::hide_window(&window);
}

/// 呼出延迟性能埋点的落地端：前端在窗口 `dowse://shown` 之后确认首帧真正
/// 绘制完成（双重 `requestAnimationFrame`，见 +page.svelte 的
/// `reportShownPerf`）才调这个命令。`HotkeyPerfState` 只在全局热键触发
/// "显示"这条路径上被标记过（见 lib.rs 的快捷键回调）——取不到值就说明
/// 这次显示不是热键触发的（比如托盘点击），静默跳过，不是错误。
#[tauri::command]
pub fn report_shown_perf(perf: State<HotkeyPerfState>) {
    let Some(started_at) = perf.take() else {
        return;
    };
    logging::log_line(
        "perf",
        &format!("呼出到可见 {}ms", started_at.elapsed().as_millis()),
    );
}

/// 击键到渲染性能埋点的落地端：前端搜索防抖(30ms)触发、拿到结果、Svelte
/// 完成 DOM 渲染后调一次（见 +page.svelte 的 `reportSearchPerf`）。
/// `e2e_ms` 是从触发搜索的输入事件到渲染完成的端到端耗时（含防抖等待），
/// `net_ms` 是从发起后端搜索调用到渲染完成的净耗时——README"击键到结果
/// 渲染"的语义更贴近端到端，但防抖窗口本身会把数字拉高一截，一并把
/// `debounce_ms` 打进日志，避免看日志的人把端到端数字误读成引擎有多慢。
/// 每次防抖触发记一条，不做采样聚合（日志按体积轮转，见 logging.rs）。
#[tauri::command]
pub fn report_search_perf(e2e_ms: f64, net_ms: f64, debounce_ms: u32) {
    logging::log_line(
        "perf",
        &format!(
            "击键到渲染 {}ms (端到端, 含防抖 {debounce_ms}ms) / 净 {}ms",
            e2e_ms.round() as i64,
            net_ms.round() as i64
        ),
    );
}
