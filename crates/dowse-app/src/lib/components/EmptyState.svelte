<script lang="ts">
	type Kind = 'idle' | 'no-index' | 'no-results' | 'rebuilding' | 'error';

	let {
		kind,
		query,
		numDocs,
		errorMessage,
		onpick
	}: {
		kind: Kind;
		query: string;
		numDocs: number;
		errorMessage?: string;
		onpick: () => void;
	} = $props();
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
		<p class="title">正在建立索引。</p>
		<p class="sub">首次建索引耗时较长，建成后为常驻内存查询。</p>
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
		gap: 6px;
		padding: 32px;
	}

	.title {
		margin: 0;
		font-size: 14px;
		font-weight: 600;
		color: var(--fg-primary);
	}

	.sub {
		margin: 0;
		font-size: 12px;
		color: var(--fg-tertiary);
		max-width: 320px;
		line-height: 1.6;
	}

	.pick {
		margin-top: 10px;
		font: inherit;
		font-size: 12px;
		font-weight: 600;
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
