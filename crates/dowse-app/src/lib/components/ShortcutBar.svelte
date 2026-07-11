<script lang="ts">
	let { hasSelection }: { hasSelection: boolean } = $props();
</script>

<div class="bar">
	<!-- 左侧品牌区：dowse 自己的错位圆图标（呼应"探水杖"意象的两圆叠合），
	     不是 Raycast 的雷标——素材是自己的，只有版式抄 Raycast 的"左品牌右操作"。 -->
	<span class="brand" aria-hidden="true">
		<svg class="brand-mark" width="14" height="14" viewBox="0 0 18 18" fill="none">
			<circle cx="7" cy="6.4" r="5" fill="var(--accent-strong)" />
			<circle cx="11" cy="11.6" r="5" fill="var(--accent-strong)" />
		</svg>
		<span class="brand-name">dowse</span>
	</span>

	<span class="spacer"></span>

	<span class="hint dim"><kbd>Esc</kbd> 隐藏</span>
	<span class="hint" class:dim={!hasSelection}>
		<kbd>Ctrl</kbd><kbd>C</kbd> 复制路径
	</span>
	<span class="hint" class:dim={!hasSelection}>
		<kbd>Ctrl</kbd><kbd>↵</kbd> 打开所在文件夹
	</span>

	<span class="divider-h" aria-hidden="true"></span>

	<span class="hint primary" class:dim={!hasSelection}>
		<kbd>↵</kbd> 打开
	</span>
</div>

<style>
	.bar {
		display: flex;
		align-items: center;
		gap: 16px;
		padding: 0 16px;
		height: 44px;
		border-top: 1px solid var(--divider);
		font-size: 11px;
		color: var(--fg-secondary);
		flex-shrink: 0;
	}

	.brand {
		display: inline-flex;
		align-items: center;
		gap: 8px;
		flex-shrink: 0;
		opacity: 0.85;
	}

	.brand-mark {
		flex-shrink: 0;
	}

	.brand-name {
		font-size: 11.5px;
		font-weight: 500;
		color: var(--fg-secondary);
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

	/* 主操作（打开）永远最靠右、最清楚——跟次要操作用一条竖分隔线拉开，
	   即使它本身也因未选中而变淡，也不跟左边那串挤在一起。 */
	.hint.primary {
		font-weight: 500;
	}

	.spacer {
		flex: 1;
	}

	.divider-h {
		width: 1px;
		height: 14px;
		background: var(--divider);
		flex-shrink: 0;
	}

	kbd {
		font-family: inherit;
		font-size: 10.5px;
		line-height: 1;
		padding: 4px 8px;
		border-radius: var(--radius-chip);
		background: var(--shortcut-chip-bg);
		color: var(--shortcut-chip-fg);
	}
</style>
