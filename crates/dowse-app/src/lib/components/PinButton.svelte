<script lang="ts">
	// 图钉固定按钮——跟两个幽灵态下拉同一视觉语言：默认（未固定）态几乎不存在，
	// 只有 hover 才微亮；固定后图钉换成水蓝实心 + 微倾斜（"钉进去了"的手感）。
	// 状态是会话级的（由父组件持有 $state，见 +page.svelte），这里只负责呈现
	// 和点击回调，不落盘。

	let { pinned, onclick }: { pinned: boolean; onclick: () => void } = $props();
</script>

<button
	type="button"
	class="pin"
	class:pinned
	title={pinned ? '取消固定（恢复失焦自动隐藏）' : '固定（失焦不再自动隐藏）'}
	{onclick}
>
	<svg class="icon" width="14" height="14" viewBox="0 0 24 24" fill="none" aria-hidden="true">
		{#if pinned}
			<path
				d="M12 2.5c-2.9 0-5.2 2.3-5.2 5.2 0 2.1 1.3 4 3.2 4.8L9 17.5H6.5v1.6h11V17.5H15l-1-5c1.9-.8 3.2-2.7 3.2-4.8 0-2.9-2.3-5.2-5.2-5.2z"
				fill="currentColor"
			/>
			<path d="M12 19.1v3.4" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" />
		{:else}
			<path
				d="M12 2.5c-2.9 0-5.2 2.3-5.2 5.2 0 2.1 1.3 4 3.2 4.8L9 17.5H6.5v1.6h11V17.5H15l-1-5c1.9-.8 3.2-2.7 3.2-4.8 0-2.9-2.3-5.2-5.2-5.2z"
				stroke="currentColor"
				stroke-width="1.4"
				stroke-linejoin="round"
			/>
			<path d="M12 19.1v3.4" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
		{/if}
	</svg>
</button>

<style>
	.pin {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 24px;
		height: 24px;
		border: none;
		background: transparent;
		border-radius: var(--radius-chip);
		color: var(--fg-tertiary);
		cursor: default;
		transition:
			color 0.12s ease-out,
			background-color 0.12s ease-out;
	}

	.pin:hover {
		color: var(--fg-secondary);
		background: var(--row-hover);
	}

	.pin.pinned {
		color: var(--accent-strong);
	}

	.icon {
		transition: transform 0.15s ease-out;
	}

	.pin.pinned .icon {
		transform: rotate(-16deg);
	}
</style>
