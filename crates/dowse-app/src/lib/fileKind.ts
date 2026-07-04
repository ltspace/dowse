// 扩展名 -> 粗分类，供 FileIcon（手绘回落图标选形状）和 PreviewPane（代码类
// 文件正文用等宽字体）共用，避免两处各自维护一份扩展名集合走漂。

const CODE_EXTS = new Set([
	'rs',
	'py',
	'go',
	'js',
	'ts',
	'jsx',
	'tsx',
	'c',
	'h',
	'cpp',
	'hpp',
	'cs',
	'rb',
	'php',
	'lua',
	'vue',
	'java',
	'json',
	'toml',
	'yaml',
	'yml',
	'sh',
	'ps1',
	'bat',
	'sql',
	'html',
	'htm',
	'xml',
	'css'
]);
const DOC_EXTS = new Set(['md', 'markdown', 'txt', 'log', 'csv', 'tsv', 'ini', 'cfg', 'conf']);

// 跟 Rust 侧 dowse-core::ocr 的 IMAGE_EXTS 保持一致（png/jpg/jpeg/webp/bmp，
// 见 docs/DESIGN-M4-OCR管线.md 第三节"范围"）。前端没有单独的 command 去问
// "这是不是图片"，直接按扩展名分类，跟 kindOf 的其它分支同一套逻辑。
const IMAGE_EXTS = new Set(['png', 'jpg', 'jpeg', 'webp', 'bmp']);

export function extOf(path: string): string {
	const dot = path.lastIndexOf('.');
	const slash = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
	if (dot <= slash) return '';
	return path.slice(dot + 1).toLowerCase();
}

export type FileKind = 'pdf' | 'code' | 'doc' | 'image' | 'file';

export function kindOf(path: string): FileKind {
	const ext = extOf(path);
	if (ext === 'pdf') return 'pdf';
	if (IMAGE_EXTS.has(ext)) return 'image';
	if (CODE_EXTS.has(ext)) return 'code';
	if (DOC_EXTS.has(ext)) return 'doc';
	return 'file';
}
