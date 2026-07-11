import { invoke } from '@tauri-apps/api/core';
import type {
	EffectLevel,
	ExtGroup,
	GlassAlpha,
	IndexStats,
	IndexStatus,
	PreviewResult,
	SearchHit,
	SortOption
} from './types';

export function indexStatus(): Promise<IndexStatus> {
	return invoke('index_status');
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
