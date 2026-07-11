<script lang="ts">
	import { fade } from 'svelte/transition';
	import AnimatedNumber from './AnimatedNumber.svelte';

	type Kind = 'idle' | 'no-index' | 'no-results' | 'rebuilding' | 'error';

	/// 建索引完成后的冷报告——出现片刻取代实时计数，再由父组件收起整个引导层。
	interface IndexReport {
		indexed: number;
		seconds: number;
		ocrPending: number;
	}

	let {
		kind,
		query,
		numDocs,
		errorMessage,
		indexingProcessed = 0,
		indexingCurrentFile = '',
		indexingReport = null,
		onpick
	}: {
		kind: Kind;
		query: string;
		numDocs: number;
		errorMessage?: string;
		indexingProcessed?: number;
		indexingCurrentFile?: string;
		indexingReport?: IndexReport | null;
		onpick: () => void;
	} = $props();

	// "37 秒" 这种整数量级用四舍五入；不到 10 秒时留一位小数——冒烟测试用的
	// 小目录经常一眨眼就建完，整数会显示成没有意义的 "0 秒"。
	function formatSeconds(seconds: number): string {
		if (seconds < 10) return `${seconds.toFixed(1)} 秒`;
		return `${Math.round(seconds)} 秒`;
	}
</script>

<div class="empty">
	{#if kind === 'idle'}
		<p class="title">键入即搜。</p>
		<p class="sub">文件名、文档正文都能搜，多个词默认取交集，"引号内"作短语查询。</p>
	{:else if kind === 'no-index'}
		<p class="title">尚未建立索引。</p>
		<p class="sub">选择一个目录开始建索引，之后可在托盘菜单重建。</p>
		<button type="button" class="pick" onclick={onpick}>选择目录并建索引</button>
	{:else if kind === 'rebuilding'}
		{#if indexingReport}
			<p class="title mono report">
				{indexingReport.indexed.toLocaleString('en-US')} 篇，{formatSeconds(
					indexingReport.seconds
				)}。
			</p>
			{#if indexingReport.ocrPending > 0}
				<p class="sub ocr-note">另有 {indexingReport.ocrPending} 张图片在后台识别。</p>
			{/if}
		{:else}
			<p class="title">正在建立索引。</p>
			<p class="sub counting">
				<span class="mono"><AnimatedNumber value={indexingProcessed} /></span> 篇已收录
			</p>
			<div class="current-file-slot">
				{#key indexingCurrentFile}
					{#if indexingCurrentFile}
						<p
							class="current-file mono"
							in:fade={{ duration: 100 }}
							out:fade={{ duration: 100 }}
						>
							{indexingCurrentFile}
						</p>
					{/if}
				{/key}
			</div>
		{/if}
	{:else if kind === 'error'}
		<p class="title">索引操作失败。</p>
		<p class="sub">{errorMessage ?? '未知错误。'}</p>
		<button type="button" class="pick" onclick={onpick}>重新选择目录</button>
	{:else}
		<p class="title">没有匹配的结果。索引包含 {numDocs} 篇文档。</p>
		<p class="sub">换一个查询词，或确认文件在已建索引的目录中。</p>
	{/if}
</div>

<style>
	.empty {
		height: 100%;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		text-align: center;
		gap: 8px;
		padding: 36px;
	}

	.title {
		margin: 0;
		font-size: 14px;
		font-weight: 500;
		color: var(--fg-primary);
	}

	.sub {
		margin: 0;
		font-size: 12px;
		color: var(--fg-tertiary);
		max-width: 320px;
		line-height: 1.6;
	}

	.mono {
		font-variant-numeric: tabular-nums;
	}

	/* 建索引完成时的冷报告：陈述句、等宽数字、句号收尾——跟计数用同一个
	   .title 位置，替换掉而不是新起一段，避免整块引导层跳一下高度。 */
	.title.report {
		font-weight: 400;
	}

	.counting {
		font-weight: 400;
	}

	.ocr-note {
		font-size: 11px;
		max-width: 320px;
	}

	/* 固定高度的槽位：文件名一行流过时用 fade 进出，占位高度不随有无内容
	   跳动——没有文件名时槽位仍在，只是空着。 */
	.current-file-slot {
		height: 15px;
		display: grid;
	}

	.current-file-slot > * {
		grid-area: 1 / 1;
	}

	.current-file {
		margin: 0;
		font-size: 11px;
		color: var(--fg-tertiary);
		opacity: 0.75;
		max-width: 360px;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.pick {
		margin-top: 12px;
		font: inherit;
		font-size: 12px;
		font-weight: 500;
		padding: 8px 16px;
		border-radius: 8px;
		border: 1px solid var(--accent-border);
		background: var(--accent-soft);
		color: var(--accent-strong);
		cursor: default;
	}

	.pick:hover {
		filter: brightness(1.05);
	}
</style>
