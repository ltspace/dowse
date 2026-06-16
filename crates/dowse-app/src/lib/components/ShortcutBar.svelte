<script lang="ts">
	let { hasSelection }: { hasSelection: boolean } = $props();
</script>

<div class="bar">
	<span class="hint" class:dim={!hasSelection}>
		<kbd>↵</kbd> 打开
	</span>
	<span class="hint" class:dim={!hasSelection}>
		<kbd>Ctrl</kbd><kbd>↵</kbd> 打开所在文件夹
	</span>
	<span class="hint" class:dim={!hasSelection}>
		<kbd>Ctrl</kbd><kbd>C</kbd> 复制路径
	</span>
	<span class="spacer"></span>
	<span class="hint dim"><kbd>Esc</kbd> 隐藏</span>
</div>

<style>
	.bar {
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 0 14px;
		height: 32px;
		border-top: 1px solid var(--divider);
		font-size: 11px;
		color: var(--fg-secondary);
		flex-shrink: 0;
	}

	.hint {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		transition: opacity 0.15s ease;
	}

	.hint.dim {
		opacity: 0.55;
	}

	.spacer {
		flex: 1;
	}

	kbd {
		font-family: inherit;
		font-size: 10.5px;
		line-height: 1;
		padding: 3px 5px;
		border-radius: var(--radius-chip);
		background: var(--shortcut-chip-bg);
		color: var(--shortcut-chip-fg);
	}
</style>
