<script lang="ts" module>
	import * as api from '$lib/api';
	import { extOf, kindOf } from '$lib/fileKind';

	// Rust 侧 FileIconCache 已经按扩展名缓存了系统图标本身；这层只是省掉
	// "一屏结果里一堆同扩展名文件"时的重复 IPC 往返，不重复维护过期逻辑——
	// 进程生命周期内一个扩展名最多真正问系统一次。
	const inflight = new Map<string, Promise<string | null>>();
	function loadSystemIcon(ext: string): Promise<string | null> {
		let pending = inflight.get(ext);
		if (!pending) {
			pending = api.fileIcon(ext).catch(() => null);
			inflight.set(ext, pending);
		}
		return pending;
	}
</script>

<script lang="ts">
	// 结果行图标优先用系统关联图标（真实反映用户装了什么软件、什么图标主题）；
	// 取不到时（非 Windows、系统查询失败）回落到这套手绘的极简分类图标——
	// 用色只取中性前景色，图标不是"活跃元素"，水蓝强调色不往这上面用。
	let { path }: { path: string } = $props();

	let ext = $derived(extOf(path));
	let kind = $derived(kindOf(path));

	// 扩展名变了（换了一行/换了预览的文件）就重新问一次系统图标；先清空
	// 避免旧图标闪一下才切到新的。取不到就停留在 null，交给下面回落到手绘版。
	let systemIconSrc = $state<string | null>(null);
	$effect(() => {
		const currentExt = ext;
		systemIconSrc = null;
		loadSystemIcon(currentExt).then((src) => {
			if (currentExt === ext) systemIconSrc = src;
		});
	});
</script>

{#if systemIconSrc}
	<img class="icon real" src={systemIconSrc} width="16" height="16" alt="" />
{:else}
	<svg class="icon" width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
		<path
			d="M4 1.5h6.5L14 5v11a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1V2.5a1 1 0 0 1 1-1Z"
			stroke="currentColor"
			stroke-width="1.15"
			stroke-linejoin="round"
			opacity="0.85"
		/>
		<path d="M10.4 1.6V5h3.5" stroke="currentColor" stroke-width="1.15" stroke-linejoin="round" opacity="0.55" />

		{#if kind === 'code'}
			<path
				d="M6.6 8.4 4.9 10l1.7 1.6M9.9 8.4l1.7 1.6-1.7 1.6"
				stroke="currentColor"
				stroke-width="1.15"
				stroke-linecap="round"
				stroke-linejoin="round"
				opacity="0.8"
			/>
		{:else if kind === 'doc'}
			<path
				d="M5 8.6h6M5 10.6h6M5 12.6h4"
				stroke="currentColor"
				stroke-width="1.1"
				stroke-linecap="round"
				opacity="0.7"
			/>
		{:else if kind === 'pdf'}
			<text x="4.4" y="12.4" font-size="4.6" font-weight="700" fill="currentColor" opacity="0.85"
				>PDF</text
			>
		{/if}
	</svg>
{/if}

<style>
	.icon {
		flex-shrink: 0;
	}

	svg.icon {
		color: var(--fg-secondary);
	}

	img.icon.real {
		width: 16px;
		height: 16px;
		object-fit: contain;
	}
</style>
