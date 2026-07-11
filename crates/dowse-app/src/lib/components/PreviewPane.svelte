<script lang="ts">
	import type { SearchHit, TextSegment } from '../types';
	import { kindOf } from '$lib/fileKind';
	import FileIcon from './FileIcon.svelte';
	import Segments from './Segments.svelte';

	let {
		hit,
		segments,
		loading
	}: {
		hit: SearchHit | null;
		segments: TextSegment[] | null;
		loading: boolean;
	} = $props();

	// 代码类文件的正文用等宽字体排——对齐缩进、行号视觉上更容易对上，
	// 文档类（md/txt 等）保持正文字体不变。
	let isCode = $derived(hit !== null && kindOf(hit.path) === 'code');
</script>

<div class="preview">
	{#if !hit}
		<p class="hint">选中结果后在此查看预览。</p>
	{:else}
		<div class="header">
			<FileIcon path={hit.path} />
			<span class="name"><Segments segments={hit.name_segments} /></span>
		</div>
		<div class="path">{hit.path}</div>
		<div class="body">
			{#if loading}
				<p class="hint">加载中…</p>
			{:else if segments && segments.length > 0}
				<p class="context" class:mono={isCode}><Segments {segments} /></p>
			{:else}
				<p class="hint">没有可预览的文本内容。</p>
			{/if}
		</div>
	{/if}
</div>

<style>
	.preview {
		height: 100%;
		display: flex;
		flex-direction: column;
		padding: 16px;
		gap: 8px;
		overflow: hidden;
	}

	.header {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.name {
		font-size: 13.5px;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.path {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-tertiary);
		word-break: break-all;
		padding-bottom: 6px;
		border-bottom: 1px solid var(--divider);
	}

	.body {
		flex: 1;
		overflow-y: auto;
	}

	.context {
		margin: 0;
		font-size: 12.5px;
		line-height: 1.75;
		color: var(--fg-secondary);
		white-space: pre-wrap;
		word-break: break-word;
	}

	.context.mono {
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.hint {
		margin: 0;
		font-size: 12.5px;
		color: var(--fg-tertiary);
	}
</style>
