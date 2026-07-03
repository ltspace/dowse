<script lang="ts">
	// 幽灵态下拉——Raycast 手法：默认态几乎不存在（无边框无底色、11~12px 灰字），
	// 只有 hover 才微亮；选中非默认值时换成所选项文字并用水蓝微标记。
	// 键盘导航（Ctrl+P/Ctrl+S 开关、↑↓ 选、Enter 定、Esc 关）由父组件
	// （+page.svelte 的 handleKeydown）统一处理——这里只负责呈现和鼠标交互，
	// 避免菜单打开时把 DOM 焦点从搜索输入框挪走导致打字体验被打断。

	let {
		idleLabel,
		options,
		value,
		defaultValue,
		open,
		activeIndex = $bindable(0),
		onselect,
		ontoggle
	}: {
		idleLabel: string;
		options: readonly { value: string; label: string }[];
		value: string;
		defaultValue: string;
		open: boolean;
		activeIndex?: number;
		onselect: (value: string) => void;
		ontoggle: () => void;
	} = $props();

	let isDefault = $derived(value === defaultValue);
	let selectedLabel = $derived(options.find((o) => o.value === value)?.label ?? idleLabel);
</script>

<div class="dropdown">
	<button type="button" class="trigger" class:marked={!isDefault} onclick={ontoggle}>
		<span class="trigger-text">{isDefault ? idleLabel : selectedLabel}</span>
		<svg class="chevron" width="10" height="10" viewBox="0 0 10 10" aria-hidden="true">
			<path
				d="M2.5 4l2.5 2.5L7.5 4"
				stroke="currentColor"
				stroke-width="1.3"
				fill="none"
				stroke-linecap="round"
				stroke-linejoin="round"
			/>
		</svg>
	</button>
	{#if open}
		<div class="menu" role="listbox">
			{#each options as opt, i (opt.value)}
				<button
					type="button"
					class="item"
					class:current={opt.value === value}
					class:hovered={i === activeIndex}
					onmouseenter={() => (activeIndex = i)}
					onclick={() => onselect(opt.value)}
				>
					{opt.label}
				</button>
			{/each}
		</div>
	{/if}
</div>

<style>
	.dropdown {
		position: relative;
	}

	.trigger {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		border: none;
		background: transparent;
		padding: 4px 6px;
		border-radius: var(--radius-chip);
		font: inherit;
		font-size: 11.5px;
		color: var(--fg-tertiary);
		cursor: default;
		transition:
			color 0.12s ease-out,
			background-color 0.12s ease-out;
	}

	.trigger:hover {
		color: var(--fg-secondary);
		background: var(--row-hover);
	}

	.trigger.marked {
		color: var(--accent-strong);
	}

	.trigger-text {
		white-space: nowrap;
	}

	.chevron {
		flex-shrink: 0;
		opacity: 0.75;
	}

	.menu {
		position: absolute;
		top: calc(100% + 6px);
		right: 0;
		z-index: 10;
		min-width: 108px;
		padding: 4px;
		display: flex;
		flex-direction: column;
		gap: 1px;
		background: var(--glass-tint);
		border: 1px solid var(--panel-border);
		border-radius: var(--radius-row);
		box-shadow: var(--panel-shadow);
	}

	.item {
		border: none;
		background: transparent;
		text-align: left;
		font: inherit;
		font-size: 12.5px;
		padding: 6px 8px;
		border-radius: 6px;
		color: var(--fg-primary);
		cursor: default;
	}

	.item.hovered {
		background: var(--row-hover);
	}

	.item.current {
		color: var(--accent-strong);
		font-weight: 500;
	}
</style>
