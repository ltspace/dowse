import { invoke } from '@tauri-apps/api/core';
import type {
	EffectLevel,
	ExtGroup,
	GlassAlpha,
	IndexingSnapshot,
	IndexRules,
	IndexStats,
	IndexStatus,
	PreviewResult,
	SearchHit,
	SortOption
} from './types';

export function indexStatus(): Promise<IndexStatus> {
	return invoke('index_status');
}

/// 建索引进度的当前快照——窗口每次呼出都应该拉一次，跟事件流（
/// `dowse://rebuild-progress`/`dowse://ocr-progress`）接续起来，见 +page.svelte
/// 的 `dowse://shown` 处理。
export function indexingStatus(): Promise<IndexingSnapshot> {
	return invoke('indexing_status');
}

export function search(
	query: string,
	limit = 30,
	extGroup: ExtGroup = 'all',
	sort: SortOption = 'relevance'
): Promise<SearchHit[]> {
	return invoke('search', { query, limit, extGroup, sort });
}

export function preview(path: string, query: string): Promise<PreviewResult | null> {
	return invoke('preview', { path, query });
}

export function openFile(path: string): Promise<void> {
	return invoke('open_file', { path });
}

export function revealInFolder(path: string): Promise<void> {
	return invoke('reveal_in_folder', { path });
}

export function rebuildIndex(dir: string): Promise<IndexStats> {
	return invoke('rebuild_index', { dir });
}

/// 添加一个索引根（多根索引）：不动现有内容，只对新目录做一次收录。
/// 空态"添加文件夹"链接走这个，跟 rebuildIndex 是姊妹命令，返回同一套统计。
export function addRoot(dir: string): Promise<IndexStats> {
	return invoke('add_root', { dir });
}

export function getEffectLevel(): Promise<EffectLevel> {
	return invoke('get_effect_level');
}

export function getGlassAlpha(): Promise<GlassAlpha> {
	return invoke('get_glass_alpha');
}

/// 当前生效的全局呼出快捷键，`tauri-plugin-global-shortcut` 的原始格式
/// （如 "Alt+Backquote"）——快捷键速查浮层拿去做人类可读的转换再显示。
export function getHotkey(): Promise<string> {
	return invoke('get_hotkey');
}

/// 按扩展名（不带点，小写与否都行）取系统关联图标的 PNG data URI，
/// 取不到返回 null——由调用方（FileIcon 组件）回落到手绘图标。
export function fileIcon(ext: string): Promise<string | null> {
	return invoke('file_icon', { ext });
}

/// 图钉固定开关：会话级，不落盘。固定期间失焦不再自动隐藏浮窗
/// （见 Rust 侧 autohide.rs 的 AutoHideSuppressor）。
export function setPinned(pinned: boolean): Promise<void> {
	return invoke('set_pinned', { pinned });
}

/// 结果行右键：在给定路径上弹出 Win32 原生上下文菜单（打开/打开所在
/// 文件夹/复制路径/复制文件名），菜单本身由 Rust 侧构造和处理，这里只是
/// 触发弹出，不需要等待用户选了哪一项。
export function showResultContextMenu(path: string): Promise<void> {
	return invoke('show_result_context_menu', { path });
}

/// 呼出延迟性能埋点：窗口 `dowse://shown` 之后确认首帧真正绘制完成（双重
/// requestAnimationFrame）才调这个，让 Rust 侧拿热键回调进入的单调时钟算
/// 差值打日志。非热键触发的显示（比如托盘点击）Rust 侧没有起始时刻，
/// 命令内部会静默跳过，前端不需要关心这个区分。
export function reportShownPerf(): Promise<void> {
	return invoke('report_shown_perf');
}

/// 击键到渲染性能埋点：搜索防抖触发、拿到结果、DOM 渲染完成后调一次。
/// `e2eMs` 含防抖等待（触发输入事件到渲染完成），`netMs` 不含（发起后端
/// 调用到渲染完成），`debounceMs` 是当前防抖窗口，一并打进日志避免端到端
/// 数字被误读。
export function reportSearchPerf(e2eMs: number, netMs: number, debounceMs: number): Promise<void> {
	return invoke('report_search_perf', { e2eMs, netMs, debounceMs });
}

/// Esc 收起浮窗。不用 `@tauri-apps/api/window` 的 `getCurrentWindow().hide()`——
/// 那走的是 Tauri core 插件的 `window|hide` 权限点，默认 capability 没放开，
/// 真机上会被 ACL 拒绝。这里走自定义命令，复用全局呼出快捷键同一条隐藏路径，
/// 自定义命令不受 ACL 权限点约束。
export function hideWindow(): Promise<void> {
	return invoke('hide_window');
}

/// 索引规则面板 Ctrl+, 打开时拉一次当前规则填表单初值。
export function getRules(): Promise<IndexRules> {
	return invoke('get_rules');
}

/// 索引规则面板"保存"：`maxFileMb` 必须是非负整数（Rust 侧是 u64，负数/小数
/// 会在反序列化阶段直接报错，调用方要先归一；0 会被 Rust 侧兜底成 1）；
/// 列表项的 trim/去空/大小写/去重由 Rust 侧统一处理，前端不用重复一遍
/// 规范化逻辑。返回规范化之后的最终值，用来回填表单，让展示的就是落盘的样子。
export function setRules(
	excludeDirs: string[],
	extraTextExts: string[],
	maxFileMb: number
): Promise<IndexRules> {
	return invoke('set_rules', {
		excludeDirs,
		extraTextExts,
		maxFileMb
	});
}
