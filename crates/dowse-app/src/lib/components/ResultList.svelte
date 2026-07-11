<script lang="ts">
	import type { SearchHit } from '../types';
	import ResultRow from './ResultRow.svelte';

	let {
		hits,
		selectedIndex,
		onhover,
		onselect
	}: {
		hits: SearchHit[];
		selectedIndex: number;
		onhover: (i: number) => void;
		onselect: (i: number) => void;
	} = $props();

	let listEl: HTMLDivElement | undefined = $state();

	// 选中项变了就把它滚进可视区——键盘走天下的场景下，用户不该需要手动滚动
	// 去找选中行在哪。
	$effect(() => {
		selectedIndex;
		if (!listEl) return;
		const row = listEl.querySelector<HTMLElement>(`[data-idx="${selectedIndex}"]`);
		row?.scrollIntoView({ block: 'nearest' });
	});
</script>

<div class="list" bind:this={listEl} role="listbox" aria-label="搜索结果">
	{#each hits as hit, i (hit.path)}
		<div data-idx={i}>
			<ResultRow {hit} selected={i === selectedIndex} onhover={() => onhover(i)} onselect={() => onselect(i)} />
		</div>
	{/each}
</div>

<style>
	.list {
		height: 100%;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 1px;
		padding: 6px;
	}
</style>
