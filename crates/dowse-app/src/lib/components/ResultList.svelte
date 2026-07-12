<script lang="ts">
	import { tick } from 'svelte';
	import { animate, stagger } from 'motion';
	import type { SearchHit } from '../types';
	import ResultRow from './ResultRow.svelte';
	import { t } from '../i18n';

	let {
		hits,
		selectedIndex,
		onhover,
		onselect,
		oncontextmenu
	}: {
		hits: SearchHit[];
		selectedIndex: number;
		onhover: (i: number) => void;
		onselect: (i: number) => void;
		oncontextmenu: (i: number) => void;
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

	// 新的一批结果上屏时做极短的错峰滑入——只给前 10 行做，结果多时不能让
	// 用户等一串错峰跑完才看到东西；弹簧动画本身也要控制在 150ms 内。
	let lastRevealKey = '';
	$effect(() => {
		const key = hits.map((h) => h.path).join('|');
		if (key === lastRevealKey || !listEl) return;
		lastRevealKey = key;
		tick().then(() => {
			if (!listEl) return;
			const rows = Array.from(listEl.querySelectorAll<HTMLElement>('.row')).slice(0, 10);
			if (rows.length === 0) return;
			animate(
				rows,
				{ opacity: [0, 1], y: [6, 0] },
				{ type: 'spring', bounce: 0.15, duration: 0.12, delay: stagger(0.012) }
			);
		});
	});
</script>

<div class="list" bind:this={listEl} role="listbox" aria-label={t.resultListLabel}>
	{#each hits as hit, i (hit.path)}
		<div data-idx={i}>
			<ResultRow
				{hit}
				selected={i === selectedIndex}
				onhover={() => onhover(i)}
				onselect={() => onselect(i)}
				oncontextmenu={() => oncontextmenu(i)}
			/>
		</div>
	{/each}
</div>

<style>
	.list {
		flex: 1;
		min-height: 0;
		overflow-y: auto;
		display: flex;
		flex-direction: column;
		gap: 1px;
		padding: 8px;
	}
</style>
