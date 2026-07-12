<script lang="ts">
	import { fade } from 'svelte/transition';
	import AnimatedNumber from './AnimatedNumber.svelte';
	import { middleEllipsis } from '../pathTruncate';
	import { t } from '../i18n';

	type Kind = 'idle' | 'no-index' | 'no-results' | 'rebuilding' | 'error';

	/// 建索引完成后的冷报告——出现片刻取代实时计数，再由父组件收起整个引导层。
	/// 不再带 ocrPending：图片后台识别的进度改由常驻的 `IndexingStrip` 组件
	/// 接管（症状 3：那行字要能持续刷新，不是重建完成那一刻的静态快照）。
	interface IndexReport {
		indexed: number;
		seconds: number;
	}

	let {
		kind,
		query,
		numDocs,
		errorMessage,
		indexingProcessed = 0,
		indexingCurrentFile = '',
		indexingReport = null,
		roots = [],
		onpick,
		onaddfolder
	}: {
		kind: Kind;
		query: string;
		numDocs: number;
		errorMessage?: string;
		indexingProcessed?: number;
		indexingCurrentFile?: string;
		indexingReport?: IndexReport | null;
		/** 已注册的全部索引根（已过 display_path 清洗），空态逐行列出。 */
		roots?: string[];
		onpick: () => void;
		onaddfolder?: () => void;
	} = $props();

	// "37 秒" 这种整数量级用四舍五入；不到 10 秒时留一位小数——冒烟测试用的
	// 小目录经常一眨眼就建完，整数会显示成没有意义的 "0 秒"。文案与单位收进 i18n。
	function formatSeconds(seconds: number): string {
		return t.formatSeconds(seconds);
	}
</script>

<div class="empty">
	{#if kind === 'idle'}
		<p class="title">{t.esTypeToSearch}</p>
		<p class="sub">{t.esSearchHelp}</p>
		{#if roots.length > 0}
			{#each roots as root (root)}
				<p class="sub root-path mono">{middleEllipsis(root)}</p>
			{/each}
			{#if onaddfolder}
				<button type="button" class="link" onclick={onaddfolder}>{t.esAddFolder}</button>
			{/if}
		{/if}
	{:else if kind === 'no-index'}
		<p class="title">{t.esNoIndexTitle}</p>
		<p class="sub">{t.esNoIndexSub}</p>
		<button type="button" class="pick" onclick={onpick}>{t.esPickAndIndex}</button>
	{:else if kind === 'rebuilding'}
		{#if indexingReport}
			<p class="title mono report">
				{t.indexReport(
					indexingReport.indexed.toLocaleString('en-US'),
					formatSeconds(indexingReport.seconds)
				)}
			</p>
		{:else}
			<!-- 阶段一：文本索引，总量未知。就是数字本身，不带"正在处理"之类的
			     废话前缀；不放进度条/百分比——总量未知时装作知道进度是廉价感的
			     重灾区，也不放转圈 spinner。 -->
			<p class="big-count mono"><AnimatedNumber value={indexingProcessed} /></p>
			<p class="count-unit">{t.esCountUnit}</p>
			<div class="current-file-slot">
				{#key indexingCurrentFile}
					{#if indexingCurrentFile}
						<p
							class="current-file mono"
							in:fade={{ duration: 90 }}
							out:fade={{ duration: 90 }}
						>
							{middleEllipsis(indexingCurrentFile)}
						</p>
					{/if}
				{/key}
			</div>
		{/if}
	{:else if kind === 'error'}
		<p class="title">{t.esErrorTitle}</p>
		<p class="sub">{errorMessage ?? t.esUnknownError}</p>
		<button type="button" class="pick" onclick={onpick}>{t.esRepick}</button>
	{:else}
		<p class="title">{t.esNoMatch(numDocs)}</p>
		<p class="sub">{t.esNoMatchSub}</p>
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

	/* 阶段一大号滚动计数：28~32px、细字重、等宽数字——就是数字本身，见上面
	   模板里"不带废话前缀"的注释。取中间值 30px。 */
	.big-count {
		margin: 0;
		font-size: 30px;
		font-weight: 300;
		color: var(--fg-primary);
		line-height: 1.2;
	}

	.count-unit {
		margin: 0 0 4px;
		font-size: 12px;
		color: var(--fg-tertiary);
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

	/* 空态"键入即搜"下面那行当前索引根路径，跟 .sub 同一档灰度，字号跟
	   current-file 对齐（都是"次要路径信息"），等宽字体方便中段省略号
	   跟路径分隔符区分开。 */
	.root-path {
		font-size: 11px;
		opacity: 0.75;
		max-width: 360px;
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

	/* "更改文件夹"是空态里的次要出口，链接级别，不能喧宾夺主——没有边框/
	   底色，比 .sub 再淡一档，只有 hover 时才提示可点。 */
	.link {
		margin-top: 2px;
		font: inherit;
		font-size: 11px;
		color: var(--accent-strong);
		opacity: 0.85;
		background: none;
		border: none;
		padding: 0;
		cursor: default;
	}

	.link:hover {
		opacity: 1;
		text-decoration: underline;
	}
</style>
