<script lang="ts">
	import { convertFileSrc } from '@tauri-apps/api/core';
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
	let isImage = $derived(hit !== null && kindOf(hit.path) === 'image');

	// 图片走 Tauri 的 asset 协议直接读本地文件（tauri.conf.json 里
	// app.security.assetProtocol 开了 scope: ["**"]——dowse 索引的目录是用户
	// 任意选的，没法预先收窄成一个固定范围）。同一路径每次 $derived 都会重新
	// 转换一次 URL，代价很小，不用额外缓存。
	let imageSrc = $derived(hit && isImage ? convertFileSrc(hit.path) : null);
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
			{#if isImage}
				{#if imageSrc}
					<img class="image-preview" src={imageSrc} alt={hit.name} />
				{/if}
				{#if loading}
					<p class="hint">识别文字加载中…</p>
				{:else if segments && segments.length > 0}
					<p class="context ocr-caption">图中文字（OCR 识别）：</p>
					<p class="context"><Segments {segments} /></p>
				{:else}
					<p class="hint">没有识别到文字，或者还在后台排队处理。</p>
				{/if}
			{:else if loading}
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
		padding: 20px;
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
		font-weight: 500;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.path {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-tertiary);
		word-break: break-all;
		padding-bottom: 8px;
		border-bottom: 1px solid var(--divider);
	}

	.body {
		flex: 1;
		overflow-y: auto;
	}

	.context {
		margin: 0;
		font-size: 12.5px;
		line-height: 2;
		color: var(--fg-secondary);
		white-space: pre-wrap;
		word-break: break-word;
	}

	.context.mono {
		font-family: var(--font-mono);
		font-size: 12px;
	}

	.image-preview {
		display: block;
		max-width: 100%;
		max-height: 260px;
		object-fit: contain;
		border-radius: 6px;
		border: 1px solid var(--divider);
		margin-bottom: 10px;
	}

	.ocr-caption {
		color: var(--fg-tertiary);
		margin-bottom: 2px;
	}

	.hint {
		margin: 0;
		font-size: 12.5px;
		color: var(--fg-tertiary);
	}
</style>
