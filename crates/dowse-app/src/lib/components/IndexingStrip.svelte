<script lang="ts">
	// OCR 回填阶段的常驻进度条——跟文本阶段的"建索引"引导层不同，这一条
	// **不遮挡搜索结果**：文本已经 commit 的内容立刻可搜，图片在后台慢慢
	// 识别，用户应该能一边搜一边看着这行数字往下走（症状 3/4 的验收场景）。
	//
	// 视觉红线：全程无 emoji/感叹号/spinner；除这条水蓝发丝进度条本身，
	// 其余都是灰阶；这是全流程唯一一处进度条——因为只有这里总量是真实已知
	// 的（文本阶段总量未知，装作知道进度是廉价感的重灾区，见 EmptyState）。
	import { fade } from 'svelte/transition';
	import AnimatedNumber from './AnimatedNumber.svelte';
	import { t } from '../i18n';

	let { processed, total }: { processed: number; total: number } = $props();

	let percent = $derived(total > 0 ? Math.min(100, Math.max(0, (processed / total) * 100)) : 0);
</script>

<div class="strip" transition:fade={{ duration: 200 }}>
	<p class="line mono">
		{t.ocrProgressLabel} <AnimatedNumber value={processed} /> / {total.toLocaleString('en-US')}
	</p>
	<div class="track">
		<div class="fill" style="width: {percent}%"></div>
	</div>
</div>

<style>
	.strip {
		flex-shrink: 0;
		padding: 8px 24px 10px;
	}

	.line {
		margin: 0 0 6px;
		font-size: 11px;
		color: var(--fg-tertiary);
	}

	.mono {
		font-variant-numeric: tabular-nums;
	}

	.track {
		height: 2px;
		border-radius: 1px;
		background: var(--divider);
		overflow: hidden;
	}

	.fill {
		height: 100%;
		border-radius: 1px;
		background: var(--accent-caret);
		transition: width 200ms ease;
	}
</style>
