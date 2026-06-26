<script lang="ts">
	import type { SearchHit } from '../types';
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
</button>

<style>
	.row {
		display: flex;
		align-items: flex-start;
		gap: 10px;
		width: 100%;
		padding: 9px 12px;
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

	.row.selected {
		background: var(--accent-soft);
		border-color: var(--accent-border);
	}

	.row-icon {
		display: flex;
		align-items: center;
		height: 20px;
		margin-top: 1px;
	}

	.row-text {
		min-width: 0;
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.row-name {
		font-size: 13.5px;
		font-weight: 600;
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
</style>
