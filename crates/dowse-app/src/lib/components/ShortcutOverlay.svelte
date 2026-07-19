<script lang="ts">
	// Ctrl+/ 呼出的快捷键速查——玻璃卡片，跟主面板同一视觉语言（--glass-tint /
	// --panel-border / --radius-row），kbd 键帽直接复用 ShortcutBar 的样式。
	// 按任意键或点击（包括点在卡片本身上）即散：键盘那半由父组件
	// （+page.svelte 的 handleKeydown，浮层打开期间拦截所有按键）负责，
	// 这里只处理鼠标点击。

	import { t } from '../i18n';

	type Row = { label: string; keys: string[] };

	let { hotkey, onclose }: { hotkey: string; onclose: () => void } = $props();

	let columns = $derived<[Row[], Row[]]>([
		[
			{ label: t.soShow, keys: [hotkey] },
			{ label: t.soHide, keys: ['Esc'] },
			{ label: t.soNavigate, keys: ['↑', '↓'] },
			{ label: t.soOpen, keys: ['↵'] },
			{ label: t.soRevealFolder, keys: ['Ctrl', '↵'] }
		],
		[
			{ label: t.soCopyPath, keys: ['Ctrl', 'C'] },
			{ label: t.soFilterType, keys: ['Ctrl', 'P'] },
			{ label: t.soSort, keys: ['Ctrl', 'S'] },
			{ label: t.soPin, keys: ['Ctrl', 'D'] },
			{ label: t.soCheatSheet, keys: ['Ctrl', '/'] },
			{ label: t.soRules, keys: ['Ctrl', ','] }
		]
	]);
</script>

<!-- 用 <button> 而不是带 onclick 的裸 div 包住整块——点哪里都散（含卡片本身），
     跟这个代码库其它可点击元素（GhostDropdown/PinButton/ResultRow）统一走
     真实按钮元素的惯例，不用另外补键盘事件处理器：键盘那半由父组件在浮层
     打开期间拦截输入框的所有按键完成（搜索框全程持有焦点，见 +page.svelte
     的 handleKeydown），这个按钮只负责鼠标点击这一半。 -->
<button type="button" class="scrim" onclick={onclose} aria-label={t.soScrimLabel}>
	<div class="card" role="dialog" aria-modal="true" aria-label={t.soDialogLabel}>
		<p class="card-title">{t.soCardTitle}</p>
		<div class="columns">
			{#each columns as col, i (i)}
				<ul class="col">
					{#each col as row (row.label)}
						<li class="row">
							<span class="label">{row.label}</span>
							<span class="keys">
								{#each row.keys as k (k)}<kbd>{k}</kbd>{/each}
							</span>
						</li>
					{/each}
				</ul>
			{/each}
		</div>
	</div>
</button>

<style>
	.scrim {
		position: absolute;
		inset: 0;
		z-index: 50;
		display: flex;
		align-items: center;
		justify-content: center;
		border: none;
		margin: 0;
		font: inherit;
		cursor: default;
		background: color-mix(in srgb, var(--solid-bg) 45%, transparent);
		backdrop-filter: blur(6px);
		-webkit-backdrop-filter: blur(6px);
		animation: scrim-in 0.1s ease-out;
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 14px;
		padding: 20px 26px;
		text-align: left;
		background: var(--glass-tint);
		border: 1px solid var(--panel-border);
		border-radius: var(--radius-row);
		box-shadow: var(--panel-shadow);
		animation: card-in 0.12s ease-out;
	}

	.card-title {
		margin: 0;
		font-size: 11px;
		letter-spacing: 0.04em;
		color: var(--fg-tertiary);
	}

	.columns {
		display: flex;
		gap: 36px;
	}

	.col {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 11px;
		min-width: 168px;
	}

	.row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 20px;
	}

	.label {
		font-size: 12px;
		color: var(--fg-secondary);
	}

	.keys {
		display: inline-flex;
		gap: 4px;
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

	@keyframes scrim-in {
		from {
			opacity: 0;
		}
		to {
			opacity: 1;
		}
	}

	@keyframes card-in {
		from {
			opacity: 0;
			transform: scale(0.98);
		}
		to {
			opacity: 1;
			transform: scale(1);
		}
	}
</style>
