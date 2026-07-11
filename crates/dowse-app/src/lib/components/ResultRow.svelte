<script lang="ts">
	import type { SearchHit } from '../types';
	import { extOf } from '../fileKind';
	import FileIcon from './FileIcon.svelte';
	import Segments from './Segments.svelte';

	let {
		hit,
		selected,
		onhover,
		onselect
	}: {
		hit: SearchHit;
		selected: boolean;
		onhover: () => void;
		onselect: () => void;
	} = $props();

	// 目录部分给路径行用：文件名已经单独占一行了，路径行只需要目录。
	let dirOf = $derived.by(() => {
		const p = hit.path;
		const slash = Math.max(p.lastIndexOf('/'), p.lastIndexOf('\\'));
		return slash >= 0 ? p.slice(0, slash + 1) : p;
	});

	// 行右侧的类型提示——Raycast 每行右边都挂一个与图标同层级的分类小字；
	// 这里用扩展名（没有就不渲染，不留空占位）。
	let typeLabel = $derived(extOf(hit.path).toUpperCase());
</script>

<button
	type="button"
	class="row"
	class:selected
	onmouseenter={onhover}
	onclick={onselect}
	ondblclick={onselect}
>
	<span class="row-icon"><FileIcon path={hit.path} /></span>
	<span class="row-text">
		<span class="row-name"><Segments segments={hit.name_segments} /></span>
		<span class="row-path">{dirOf}</span>
		{#if hit.snippet_segments.length > 0}
			<span class="row-snippet"><Segments segments={hit.snippet_segments} /></span>
		{/if}
	</span>
	{#if typeLabel}
		<span class="row-type">{typeLabel}</span>
	{/if}
</button>

<style>
	.row {
		display: flex;
		align-items: flex-start;
		gap: 12px;
		width: 100%;
		padding: 8px 12px;
		border: 1px solid transparent;
		border-radius: var(--radius-row);
		background: transparent;
		font: inherit;
		text-align: left;
		cursor: default;
		color: var(--fg-primary);
		transition:
			background-color 0.09s ease-out,
			border-color 0.09s ease-out;
	}

	.row:hover {
		background: var(--row-hover);
	}

	/* 选中态只用纯色块，不加描边——border 仍占位为 transparent 是为了不
	   在切换选中时抖动 1px 布局，视觉上跟未选中时完全一样"没有边框"。 */
	.row.selected {
		background: var(--accent-soft);
	}

	.row-icon {
		display: flex;
		align-items: center;
		height: 20px;
		margin-top: 1px;
	}

	.row-text {
		min-width: 0;
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.row-name {
		font-size: 13.5px;
		font-weight: 500;
		line-height: 1.3;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.row-path {
		font-family: var(--font-mono);
		font-size: 11px;
		color: var(--fg-tertiary);
		line-height: 1.3;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.row-snippet {
		font-size: 12px;
		color: var(--fg-secondary);
		line-height: 1.4;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	/* 右侧类型提示：跟图标同一行水平对齐，独立一列不挤标题——
	   Raycast 每行右边都留一个这样的分类小字，颜色最淡、字号最小。 */
	.row-type {
		flex-shrink: 0;
		align-self: flex-start;
		margin-top: 1px;
		font-size: 10.5px;
		font-weight: 500;
		letter-spacing: 0.02em;
		color: var(--fg-tertiary);
		opacity: 0.75;
	}
</style>
