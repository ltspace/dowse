// 跟 Rust 侧 crates/dowse-app/src-tauri/src/commands.rs 与 window_fx.rs 的
// DTO 一一对应。命中区间在 Rust 侧已经切成 TextSegment 数组——这里不做任何
// 字节偏移换算，前端只管按顺序渲染 segments。

export interface TextSegment {
	text: string;
	highlighted: boolean;
}

export interface SearchHit {
	path: string;
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

export type EffectLevel = 'acrylic' | 'mica' | 'solid';
