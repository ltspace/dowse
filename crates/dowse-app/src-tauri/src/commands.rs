use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;
use tauri_plugin_opener::OpenerExt;

use crate::config::ConfigState;
use crate::file_icons::FileIconCache;
use crate::highlight::{TextSegment, highlight_name, segments_from_ranges};
use crate::state::SearchState;
use crate::watcher::WatchController;
use crate::window_fx::{EffectLevel, EffectLevelState};

#[derive(Serialize)]
pub struct SearchHitDto {
    pub path: String,
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

#[tauri::command]
pub fn search(
    search: State<SearchState>,
    query: String,
    limit: usize,
) -> Result<Vec<SearchHitDto>, String> {
    let guard = search.0.lock().map_err(|_| "搜索状态异常".to_string())?;
    let Some(searcher) = guard.as_ref() else {
        return Ok(Vec::new());
    };

    let hits = searcher.search(&query, limit).map_err(|e| e.to_string())?;
    Ok(hits
        .into_iter()
        .map(|hit| {
            let name = file_name_of(&hit.path);
            let name_segments = highlight_name(&name, &query);
            let snippet_segments = segments_from_ranges(&hit.snippet, &hit.highlighted);
            SearchHitDto {
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
#[cfg(target_os = "windows")]
#[tauri::command]
pub fn reveal_in_folder(path: String) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
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
#[tauri::command]
pub fn rebuild_index(
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

    let stats = dowse_core::rebuild_index(&index_dir, &target).map_err(|e| e.to_string())?;

    let new_searcher = dowse_core::Searcher::open(&index_dir).map_err(|e| e.to_string())?;
    search.replace(new_searcher);
    let _ = config.set_target_dir(target.clone());

    // 重建完盯住新索引根，重新挂上"对账 + 实时监听"。
    watch.start(index_dir, vec![target]);

    Ok(IndexStatsDto {
        indexed: stats.indexed,
        skipped: stats.skipped,
        seconds: stats.seconds,
    })
}
