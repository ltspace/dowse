<script lang="ts">
	// 手绘的极简文件类型图标——不引图标库，就几个扩展名分类，用色只取
	// 中性前景色（图标不是"活跃元素"，水蓝强调色不往这上面用）。
	let { path }: { path: string } = $props();

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

	function extOf(p: string): string {
		const dot = p.lastIndexOf('.');
		const slash = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
		if (dot <= slash) return '';
		return p.slice(dot + 1).toLowerCase();
	}

	let ext = $derived(extOf(path));
	let kind = $derived(
		ext === 'pdf' ? 'pdf' : CODE_EXTS.has(ext) ? 'code' : DOC_EXTS.has(ext) ? 'doc' : 'file'
	);
</script>

<svg class="icon" width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
	<path
		d="M4 1.5h6.5L14 5v11a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V2.5a1 1 0 0 1 1-1Z"
		stroke="currentColor"
		stroke-width="1.15"
		stroke-linejoin="round"
		opacity="0.85"
	/>
	<path d="M10.4 1.6V5h3.5" stroke="currentColor" stroke-width="1.15" stroke-linejoin="round" opacity="0.55" />

	{#if kind === 'code'}
		<path
			d="M6.6 8.4 4.9 10l1.7 1.6M9.9 8.4l1.7 1.6-1.7 1.6"
			stroke="currentColor"
			stroke-width="1.15"
			stroke-linecap="round"
			stroke-linejoin="round"
			opacity="0.8"
		/>
	{:else if kind === 'doc'}
		<path
			d="M5 8.6h6M5 10.6h6M5 12.6h4"
			stroke="currentColor"
			stroke-width="1.1"
			stroke-linecap="round"
			opacity="0.7"
		/>
	{:else if kind === 'pdf'}
		<text x="4.4" y="12.4" font-size="4.6" font-weight="700" fill="currentColor" opacity="0.85"
			>PDF</text
		>
	{/if}
</svg>

<style>
	.icon {
		color: var(--fg-secondary);
		flex-shrink: 0;
	}
</style>
