<script lang="ts">
	import { convertFileSrc } from '@tauri-apps/api/core';
	import type { SearchHit, TextSegment } from '../types';
	import { kindOf } from '$lib/fileKind';
	import { middleEllipsis } from '../pathTruncate';
	import FileIcon from './FileIcon.svelte';
	import Segments from './Segments.svelte';
	import { t } from '../i18n';

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
		<p class="hint">{t.previewSelectHint}</p>
	{:else}
		<div class="header">
			<FileIcon path={hit.path} />
			<span class="name"><Segments segments={hit.name_segments} /></span>
		</div>
		<div class="path">{middleEllipsis(hit.display_path)}</div>
		<div class="body">
			{#if isImage}
				{#if imageSrc}
					<img class="image-preview" src={imageSrc} alt={hit.name} />
				{/if}
				{#if loading}
					<p class="hint">{t.ocrLoading}</p>
				{:else if segments && segments.length > 0}
					<p class="ocr-caption">{t.ocrCaption}</p>
					<div class="ocr-text-wrap">
						<p class="context ocr-text"><Segments {segments} /></p>
					</div>
				{:else}
					<p class="hint">{t.ocrEmpty}</p>
				{/if}
			{:else if loading}
				<p class="hint">{t.previewLoading}</p>
			{:else if segments && segments.length > 0}
				<p class="context" class:mono={isCode}><Segments {segments} /></p>
			{:else}
				<p class="hint">{t.previewEmpty}</p>
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
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
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

	/* 分区小标题规格：跟 +page.svelte 的 .results-heading 同一档——11px、
	   微字距、三级灰，跟正文（.context）明确区分出"这是个标签，不是正文"。 */
	.ocr-caption {
		margin: 0 0 4px;
		font-size: 11px;
		letter-spacing: 0.04em;
		color: var(--fg-tertiary);
	}

	/* OCR 文字段限高约 6~8 行，超出内部滚动——手机截图的状态栏文字、大段
	   OCR 误识别是常态，不能让预览区被一张图的识别结果撑到没法看别的信息。 */
	.ocr-text-wrap {
		max-height: 200px;
		overflow-y: auto;
	}

	/* 对比度比普通正文提一档：OCR 识别本身就有噪声，用更高对比度的主文字色
	   帮助辨认，跟旁边的 .hint/.ocr-caption 三级灰形成清楚的层次。 */
	.ocr-text {
		color: var(--fg-primary);
	}

	.hint {
		margin: 0;
		font-size: 12.5px;
		color: var(--fg-tertiary);
	}
</style>
