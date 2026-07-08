use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{Emitter, State};
use tauri_plugin_opener::OpenerExt;

use crate::autohide::AutoHideSuppressor;
use crate::config::ConfigState;
use crate::file_icons::FileIconCache;
use crate::highlight::{TextSegment, highlight_name, segments_from_ranges};
use crate::state::SearchState;
use crate::watcher::WatchController;
use crate::window_fx::{EffectLevel, EffectLevelState, GlassAlpha};

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
pub struct IndexStatsDto {
    pub indexed: usize,
    pub skipped: usize,
    pub seconds: f64,
    /// 建索引期间发现、还没识别完的图片数——OCR 是独立的后台低优先级管线，
    /// 全量重建结束时这些图片大概率还在排队，浮窗拿这个数补一行"另有 N 张
    /// 图片在后台识别"的小字，不让用户误以为索引没做完。
    pub ocr_pending: usize,
}

/// 全量重建索引期间的一次进度汇报，经 `dowse://rebuild-progress` 事件推给前端。
/// 对应 dowse-core 的 `IndexProgress`，path 这里已经转成剥过 `\\?\` 前缀的
/// 展示用字符串——前端只拿它当一行流过的文字，不会拿去做文件操作。
#[derive(Serialize, Clone)]
pub struct IndexProgressDto {
    pub processed: usize,
    pub path: String,
}

#[derive(Serialize)]
pub struct IndexStatusDto {
    pub has_index: bool,
    pub num_docs: u64,
    /// 上次成功建索引的目录，回显在"重建索引"引导上。
    pub last_target_dir: Option<String>,
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
#[tauri::command]
pub fn index_status(search: State<SearchState>, config: State<ConfigState>) -> IndexStatusDto {
    let guard = search.0.lock().expect("search state mutex poisoned");
    let (has_index, num_docs) = match guard.as_ref() {
        Some(s) => (true, s.num_docs()),
        None => (false, 0),
    };
    let last_target_dir = config
        .get()
        .target_dir
        .map(|p| p.to_string_lossy().into_owned());
    IndexStatusDto {
        has_index,
        num_docs,
        last_target_dir,
    }
}

/// 浮窗"类型/排序"两个幽灵态下拉的取值透传到这里，翻译成 dowse-core 的
/// 分组常量/`SortMode`——语义（哪个字符串对应哪组扩展名、哪种排序）由
/// dowse-core 一处定义，Tauri 这层只是原样转发字符串，不在这里重复一份映射表。
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

    let group = dowse_core::ext_group_by_name(ext_group.as_deref());
    let sort_mode = dowse_core::SortMode::parse(sort.as_deref());

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
                display_path: dowse_core::display_path(&hit.path),
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

/// 全量重建索引。目标目录来自前端的目录选择器（或托盘"重建索引"复用上次的目录）。
/// 成功后把新的 Searcher 换进常驻状态，并把目录记进配置，供托盘复用。
///
/// 建索引期间通过 `dowse://rebuild-progress` 事件把进度实时推给前端（浮窗的
/// "实时直播"效果），频率由 dowse-core 的 `PROGRESS_INTERVAL` 控制，这里
/// 原样转发，不再额外节流。
#[tauri::command]
pub fn rebuild_index(
    app: tauri::AppHandle,
    search: State<SearchState>,
    config: State<ConfigState>,
    watch: State<WatchController>,
    dir: String,
) -> Result<IndexStatsDto, String> {
    let target = PathBuf::from(&dir);
    let target = target
        .canonicalize()
        .map_err(|_| "目录不存在".to_string())?;

    let index_dir = crate::config::index_dir().map_err(|e| e.to_string())?;

    // 重建前先停监听：放掉旧索引的写锁和文件句柄，否则 Windows 删不掉旧索引目录。
    watch.stop();

    let stats = dowse_core::rebuild_index_with_progress(&index_dir, &target, |progress| {
        let _ = app.emit(
            "dowse://rebuild-progress",
            IndexProgressDto {
                processed: progress.processed,
                path: dowse_core::display_path(&progress.path.to_string_lossy()),
            },
        );
    })
    .map_err(|e| e.to_string())?;

    // 在 watch.start 挪走 index_dir 之前先问一次 OCR 队列——两者用的是同一个
    // index_dir，问完这次调用就不再需要它了。
    let ocr_pending = dowse_core::OcrQueue::for_index_dir(&index_dir).pending_len();

    let new_searcher = dowse_core::Searcher::open(&index_dir).map_err(|e| e.to_string())?;
    search.replace(new_searcher);
    let _ = config.set_target_dir(target.clone());

    // 重建完盯住新索引根，重新挂上"对账 + 实时监听"。
    watch.start(index_dir, vec![target]);

    Ok(IndexStatsDto {
        indexed: stats.indexed,
        skipped: stats.skipped,
        seconds: stats.seconds,
        ocr_pending,
    })
}

/// 图钉固定开关：前端点了图钉按钮就调这个命令，把会话级的"抑制失焦自动
/// 隐藏"状态同步到 Rust 侧（见 autohide.rs 的 `AutoHideSuppressor`）。
/// 不落盘——重启应用后前端按钮状态和这里的计数器都回到初始值。
#[tauri::command]
pub fn set_pinned(suppressor: State<AutoHideSuppressor>, pinned: bool) {
    suppressor.set_pinned(pinned);
}
