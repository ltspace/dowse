<script lang="ts">
	// 数字滚动过渡：值变化时旧数字上移淡出、新数字下移淡入，~80ms，克制到
	// 几乎不打断阅读。用 Svelte 内建的 {#key}+fly 而不是 motion 库——这里只是
	// 一次性的进出转场，不需要弹簧物理。两个数字用 CSS Grid 叠在同一格里
	// （不是绝对定位），过渡期间新旧数字重叠但不影响外部布局宽度。
	//
	// 建索引"实时直播"的计数（v0.5.1 首次引入这个组件）和结果条数共用同一份
	// 实现——同一种"数字在变"的场景，没有理由用两套动画语言。毫秒数（页脚
	// 耗时）刻意不用这个组件：那个数字每次搜索都变，滚一下会晃眼，直接文本
	// 替换就好。

	import { fly } from 'svelte/transition';

	let { value }: { value: number } = $props();

	let formatted = $derived(value.toLocaleString('en-US'));
</script>

<span class="stack">
	{#key formatted}
		<span class="digits" in:fly={{ y: 8, duration: 80 }} out:fly={{ y: -8, duration: 80 }}>
			{formatted}
		</span>
	{/key}
</span>

<style>
	.stack {
		display: inline-grid;
	}

	.digits {
		grid-area: 1 / 1;
	}
</style>
