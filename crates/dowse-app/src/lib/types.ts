// 跟 Rust 侧 crates/dowse-app/src-tauri/src/commands.rs 与 window_fx.rs 的
// DTO 一一对应。命中区间在 Rust 侧已经切成 TextSegment 数组——这里不做任何
// 字节偏移换算，前端只管按顺序渲染 segments。

export interface TextSegment {
	text: string;
	highlighted: boolean;
}

export interface SearchHit {
	/** 打开文件、跳转文件夹用这个——可能带 Windows 扩展长度路径的 `\\?\` 前缀，别拿去展示。 */
	path: string;
	/** 结果行、预览区展示路径文本用这个——`\\?\` 前缀已经剥掉。 */
	display_path: string;
	name: string;
	name_segments: TextSegment[];
	snippet_segments: TextSegment[];
	score: number;
}

export interface PreviewResult {
	segments: TextSegment[];
}

export interface IndexStats {
	indexed: number;
	skipped: number;
	seconds: number;
}

export interface IndexStatus {
	has_index: boolean;
	num_docs: number;
	last_target_dir: string | null;
}

/// 类型筛选下拉的取值，跟 Rust 侧 dowse-core::ext_groups::by_name 认的字符串一一对应。
export type ExtGroup = 'all' | 'doc' | 'code' | 'image';

/// 排序下拉的取值，跟 Rust 侧 dowse-core::SortMode::parse 认的字符串一一对应。
export type SortOption = 'relevance' | 'mtime_desc' | 'mtime_asc' | 'size_desc';

export type EffectLevel = 'acrylic' | 'mica' | 'solid';

/// 面板可视不透明度的明/暗两套 CSS alpha（0~1），跟托盘"透明度"三档挂钩，
/// 见 Rust 侧 window_fx.rs 的 `TransparencyTier`/`GlassAlpha`。
export interface GlassAlpha {
	light: number;
	dark: number;
}
