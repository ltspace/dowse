<script lang="ts">
	// 索引规则面板：Ctrl+, 打开，Esc/点击遮罩关闭。跟主搜索面板同一视觉语言
	// （--glass-tint / --panel-border / --radius-row），玻璃卡片，见
	// ShortcutOverlay 同款做法。
	//
	// 跟 ShortcutOverlay 的关键差异：那个浮层没有可交互内容，整块靠父组件
	// （+page.svelte 的 handleKeydown）拦截键盘、搜索框全程持有 DOM 焦点。
	// 这里恰恰相反——面板里是真实的文本框/数字框，用户要能实际打字，所以
	// DOM 焦点必须真的移进面板（见 onMount 里的 focus）。这带来一个好处：
	// 焦点一旦离开搜索框，绑在 `.search-input` 上的全局快捷键处理器
	// （Ctrl+P/Ctrl+S/Ctrl+D/↑↓/Enter 等）天然就不会再触发——"面板打开期间
	// 吞掉其余全局快捷键"不需要额外拦截逻辑，是焦点转移的自然结果。这里
	// 只需要自己处理 Esc（面板内任意字段按下都要能关闭），因为 Esc 不会
	// 冒泡到已经失焦的搜索框上。

	import { onMount } from 'svelte';
	import * as api from '../api';
	import type { IndexRules } from '../types';
	import { t } from '../i18n';

	let {
		roots,
		onclose,
		onrebuild
	}: {
		/** 已注册的全部索引根（display_path 清洗过）。"立即重建"只在恰好一个
		 * 根时可用——`rebuild_index` 命令用单个目标目录整体替换索引，多根
		 * 场景下这么做会把其它根一并冲掉，而多根索引目前没有暴露"重建全部
		 * 根"的前端命令（只有托盘每根子菜单的单根重建）。 */
		roots: string[];
		onclose: () => void;
		/** 保存成功后触发一次重建——父组件负责实际调用 rebuild_index 并接管
		 * 引导层展示，这里只管发出"该重建了"的意图。 */
		onrebuild: () => void;
	} = $props();

	/// 把逗号/换行分隔的文本拆成去空白、去空项的列表。大小写/去点/去重交给
	/// Rust 侧的 `IndexRules::normalize`（跟 CLI `dowse rules set` 同一份
	/// 逻辑），这里不重复实现一遍，保存成功后用服务端返回值回填表单即可
	/// 看到规范化之后的最终形态。
	function splitList(text: string): string[] {
		return text
			.split(/[,\n]/)
			.map((s) => s.trim())
			.filter(Boolean);
	}

	let excludeDirsText = $state('');
	let extraExtsText = $state('');
	let maxFileMbText = $state('20');

	let loading = $state(true);
	let loadError = $state('');
	let saving = $state(false);
	let saved = $state(false);
	let saveError = $state('');

	let canRebuild = $derived(roots.length === 1);

	function applyRules(rules: IndexRules) {
		excludeDirsText = rules.exclude_dirs.join('\n');
		extraExtsText = rules.extra_text_exts.join(', ');
		maxFileMbText = String(rules.max_file_mb);
	}

	/// 保存当前表单值，成功返回 true。失败时 `saveError` 承载错误文案，
	/// 不抛出——调用方（handleSave/handleSaveAndRebuild）据此决定要不要
	/// 接着触发重建。
	async function save(): Promise<boolean> {
		saving = true;
		saved = false;
		saveError = '';
		try {
			// 归一成 >=1 的整数再传：Rust 侧参数是 u64，负数/小数在 serde
			// 反序列化阶段就会生硬报错，到不了那边的 .max(1) 兜底——与其把
			// 反序列化错误原样甩给用户，不如前端先收敛到合法域。
			const raw = Number(maxFileMbText);
			const mb = Number.isFinite(raw) ? Math.max(1, Math.floor(raw)) : 1;
			const rules = await api.setRules(splitList(excludeDirsText), splitList(extraExtsText), mb);
			applyRules(rules);
			saved = true;
			return true;
		} catch (err) {
			saveError = t.rpSaveFailed(String(err));
			return false;
		} finally {
			saving = false;
		}
	}

	function handleSave() {
		save();
	}

	/// "立即重建"：先保存表单当前值再触发重建——不然点下去重建的还是
	/// 上一次保存的旧规则，跟按钮字面意思（重建"现在看到的"这份规则）不符。
	async function handleSaveAndRebuild() {
		if (!canRebuild) return;
		const ok = await save();
		if (ok) onrebuild();
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			onclose();
		}
	}

	let excludeDirsEl: HTMLTextAreaElement | undefined = $state();
	let cardEl: HTMLDivElement | undefined = $state();

	onMount(() => {
		// 数据还没拉回来之前先把焦点占住。注意此刻不能聚焦文本框——字段在
		// loading 期间是 disabled 的，对 disabled 元素调 .focus() 是浏览器
		// 规范下的 no-op，焦点会滞留在背后的搜索框上，导致"焦点进面板吞掉
		// 全局快捷键"的前提整个落空（Esc 甚至会把整个窗口藏掉）。所以先聚焦
		// 卡片本身（tabindex="-1"，任何时候都可聚焦），等数据回来、字段解禁
		// 后再由下面的 $effect 把焦点交给第一个文本框。
		cardEl?.focus();
		api
			.getRules()
			.then(applyRules)
			.catch(() => {
				loadError = t.rpLoadFailed;
			})
			.finally(() => {
				loading = false;
			});
	});

	$effect(() => {
		// loading 只会 true→false 翻转一次：字段解禁的那一刻把焦点从卡片
		// 移进第一个文本框，用户即可直接打字。
		if (!loading) excludeDirsEl?.focus();
	});
</script>

<!-- 遮罩用 div 而不是 ShortcutOverlay 那种 <button> 包一切——面板里是真实的
     表单控件，不能被一个外层按钮抢走点击事件。遮罩本身点击关闭，卡片上
     stopPropagation 挡住冒泡，交互上等价于"点卡片外面才关"。 -->
<div
	class="scrim"
	role="presentation"
	onclick={onclose}
	onkeydown={handleKeydown}
>
	<div
		bind:this={cardEl}
		class="card"
		role="dialog"
		aria-modal="true"
		aria-label={t.rpTitle}
		tabindex="-1"
		onclick={(e) => e.stopPropagation()}
		onkeydown={handleKeydown}
	>
		<div class="head">
			<p class="card-title">{t.rpTitle}</p>
			<button type="button" class="close" onclick={onclose} aria-label={t.rpCloseLabel}>
				<svg width="12" height="12" viewBox="0 0 12 12" aria-hidden="true">
					<path
						d="M1.5 1.5l9 9M10.5 1.5l-9 9"
						stroke="currentColor"
						stroke-width="1.4"
						stroke-linecap="round"
					/>
				</svg>
			</button>
		</div>

		<div class="field">
			<label class="field-label" for="rp-exclude-dirs">{t.rpExcludeDirsLabel}</label>
			<textarea
				bind:this={excludeDirsEl}
				id="rp-exclude-dirs"
				rows="3"
				bind:value={excludeDirsText}
				disabled={loading}
			></textarea>
			<p class="field-hint">{t.rpExcludeDirsHint}</p>
		</div>

		<div class="field">
			<label class="field-label" for="rp-extra-exts">{t.rpExtraExtsLabel}</label>
			<input id="rp-extra-exts" type="text" bind:value={extraExtsText} disabled={loading} />
			<p class="field-hint">{t.rpExtraExtsHint}</p>
		</div>

		<div class="field">
			<label class="field-label" for="rp-max-file-mb">{t.rpMaxFileMbLabel}</label>
			<input
				id="rp-max-file-mb"
				class="mb-input"
				type="number"
				min="1"
				step="1"
				bind:value={maxFileMbText}
				disabled={loading}
			/>
		</div>

		{#if loadError}
			<p class="status error">{loadError}</p>
		{/if}

		<div class="actions">
			<button type="button" class="save" onclick={handleSave} disabled={loading || saving}>
				{saving ? t.rpSaving : t.rpSave}
			</button>
			<button
				type="button"
				class="rebuild"
				onclick={handleSaveAndRebuild}
				disabled={loading || saving || !canRebuild}
			>
				{t.rpRebuildNow}
			</button>
		</div>

		{#if saved}
			<p class="status success">{t.rpSaved}</p>
		{/if}
		{#if saveError}
			<p class="status error">{saveError}</p>
		{/if}
		{#if roots.length === 0}
			<p class="field-hint">{t.rpRebuildNoIndexHint}</p>
		{:else if !canRebuild}
			<p class="field-hint">{t.rpRebuildMultiRootHint}</p>
		{/if}
	</div>
</div>

<style>
	.scrim {
		position: absolute;
		inset: 0;
		z-index: 50;
		display: flex;
		align-items: center;
		justify-content: center;
		background: color-mix(in srgb, var(--solid-bg) 45%, transparent);
		backdrop-filter: blur(6px);
		-webkit-backdrop-filter: blur(6px);
		animation: scrim-in 0.1s ease-out;
	}

	.card {
		display: flex;
		flex-direction: column;
		gap: 14px;
		width: min(400px, calc(100% - 48px));
		padding: 20px 22px;
		text-align: left;
		background: var(--glass-tint);
		border: 1px solid var(--panel-border);
		border-radius: var(--radius-row);
		box-shadow: var(--panel-shadow);
		animation: card-in 0.12s ease-out;
	}

	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
	}

	.card-title {
		margin: 0;
		font-size: 11px;
		letter-spacing: 0.04em;
		color: var(--fg-tertiary);
	}

	/* 关闭按钮：跟卡片标题同一档淡，只有 hover 才提亮——不是主操作，
	   Esc/点遮罩才是关闭这个面板的主要方式，这颗按钮只为鼠标用户兜底。 */
	.close {
		display: flex;
		align-items: center;
		justify-content: center;
		width: 20px;
		height: 20px;
		border: none;
		background: transparent;
		border-radius: var(--radius-chip);
		color: var(--fg-tertiary);
		cursor: default;
	}

	.close:hover {
		color: var(--fg-primary);
		background: var(--row-hover);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 5px;
	}

	.field-label {
		font-size: 11px;
		letter-spacing: 0.02em;
		color: var(--fg-secondary);
	}

	.field-hint {
		margin: 0;
		font-size: 10.5px;
		color: var(--fg-tertiary);
		opacity: 0.8;
		line-height: 1.5;
	}

	textarea,
	input[type='text'],
	input[type='number'] {
		font: inherit;
		font-size: 12.5px;
		color: var(--fg-primary);
		background: var(--row-hover);
		border: 1px solid var(--panel-border);
		border-radius: var(--radius-chip);
		padding: 7px 9px;
		outline: none;
		resize: none;
	}

	textarea:focus,
	input:focus {
		border-color: var(--accent-border);
	}

	textarea:disabled,
	input:disabled {
		opacity: 0.6;
	}

	.mb-input {
		width: 88px;
	}

	.actions {
		display: flex;
		gap: 8px;
	}

	.save,
	.rebuild {
		font: inherit;
		font-size: 12px;
		font-weight: 500;
		padding: 7px 14px;
		border-radius: var(--radius-chip);
		cursor: default;
	}

	.save {
		border: 1px solid var(--accent-border);
		background: var(--accent-soft);
		color: var(--accent-strong);
	}

	.save:hover:not(:disabled) {
		filter: brightness(1.05);
	}

	.rebuild {
		border: 1px solid var(--panel-border);
		background: transparent;
		color: var(--fg-secondary);
	}

	.rebuild:hover:not(:disabled) {
		background: var(--row-hover);
		color: var(--fg-primary);
	}

	.save:disabled,
	.rebuild:disabled {
		opacity: 0.5;
	}

	.status {
		margin: 0;
		font-size: 11px;
	}

	.status.success {
		color: var(--accent-strong);
	}

	.status.error {
		color: var(--fg-secondary);
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
