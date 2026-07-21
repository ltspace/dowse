<script lang="ts">
	// 设置面板：Ctrl+, 打开，Esc/点击遮罩关闭。跟主搜索面板同一视觉语言
	// （--glass-tint / --panel-border / --radius-row），玻璃卡片，见
	// ShortcutOverlay 同款做法。分两个分区：「通用」（默认停在这里）+「索引规则」
	// （原索引规则三项原样迁入）。
	//
	// 焦点/键盘约定（跟老索引规则面板一致，扩成设置面板后必须继续成立）：
	// 面板里有真实的表单控件（文本框/按钮），用户要能实际打字/按键，所以 DOM
	// 焦点必须真的移进面板——onMount 先聚焦卡片本身（tabindex="-1"，任何时候都
	// 可聚焦），焦点一旦离开搜索框，绑在 `.search-input` 上的全局快捷键处理器
	// 天然就不会再触发，"面板打开期间吞掉其余全局快捷键"是焦点转移的自然结果，
	// 不需要额外拦截逻辑。这里只需自己处理 Esc（面板内按下要能关闭），因为 Esc
	// 不会冒泡到已经失焦的搜索框上。
	//
	// 一个新增的例外是"改键捕获态"：捕获期间 Esc 只退出捕获、不关面板（捕获态
	// 优先），并且捕获处理器 stopPropagation，避免 Esc/组合键冒泡到卡片的关闭
	// 处理器上——详见下面 captureKeydown 的注释。
	//
	// +page.svelte 的 handleKeydown 还留了一条 settingsPanelOpen 保险丝分支：正常
	// 情况下焦点在面板内、那个处理器根本不触发，但万一焦点转移落空，保险丝保证
	// 按键不会漏进面板背后的搜索 UI。扩成设置面板后这套约定原样保留。

	import { onMount, tick } from 'svelte';
	import * as api from '../api';
	import type { IndexRules, LangOption, TransparencyTier } from '../types';
	import { t, LANG_OVERRIDE_KEY } from '../i18n';
	import { formatHotkey } from '../hotkey';

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

	// 当前分区，默认停在「通用」。
	let tab = $state<'general' | 'rules'>('general');

	let cardEl: HTMLDivElement | undefined = $state();

	// ── 通用区状态（初值从 get_config 拉一次） ──────────────────────────────
	let hotkeyLabel = $state(''); // 展示用（formatHotkey 之后）
	let transparencyEnabled = $state(true);
	let tier = $state<TransparencyTier>('mid');
	let autostartEnabled = $state(false);
	let lang = $state<LangOption>('auto');
	let langChanged = $state(false); // 改过语言 → 显示"重启后生效"

	// ── 改键捕获态 ──────────────────────────────────────────────────────────
	let capturing = $state(false); // 正在等用户按下新组合键
	let pendingCombo = $state<string | null>(null); // 捕到、待确认的组合键（Tauri 格式）
	let pendingLabel = $state(''); // 上面那个的展示形式
	let hotkeyMsg = $state(''); // 提示/错误文案（need-modifier / saved / failed）
	let hotkeyBtnEl: HTMLButtonElement | undefined = $state();
	let confirmBtnEl: HTMLButtonElement | undefined = $state();

	// 只按了修饰键（还没按主键）时的 e.code 集合——捕获时要跳过，继续等主键。
	const MODIFIER_CODES = new Set([
		'ControlLeft',
		'ControlRight',
		'AltLeft',
		'AltRight',
		'ShiftLeft',
		'ShiftRight',
		'MetaLeft',
		'MetaRight',
		'OSLeft',
		'OSRight'
	]);

	// 通用/透明/语言三组分段控件的选项表。
	const TIER_OPTIONS: { value: TransparencyTier; label: string }[] = [
		{ value: 'low', label: t.setTierLow },
		{ value: 'mid', label: t.setTierMid },
		{ value: 'high', label: t.setTierHigh }
	];
	const LANG_OPTIONS: { value: LangOption; label: string }[] = [
		{ value: 'auto', label: t.setLangAuto },
		{ value: 'zh', label: t.setLangZh },
		{ value: 'en', label: t.setLangEn }
	];
	const ONOFF_OPTIONS = [
		{ value: 'off', label: t.setOff },
		{ value: 'on', label: t.setOn }
	];

	// ── 索引规则区状态（原样迁入） ──────────────────────────────────────────
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
	let excludeDirsEl: HTMLTextAreaElement | undefined = $state();

	function applyRules(rules: IndexRules) {
		excludeDirsText = rules.exclude_dirs.join('\n');
		extraExtsText = rules.extra_text_exts.join(', ');
		maxFileMbText = String(rules.max_file_mb);
	}

	async function save(): Promise<boolean> {
		saving = true;
		saved = false;
		saveError = '';
		try {
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

	// ── 分区切换 ────────────────────────────────────────────────────────────
	function setTab(next: 'general' | 'rules') {
		tab = next;
		// 切到索引规则区、且字段已解禁时，把焦点交给第一个文本框，用户即可
		// 直接打字（loading 期间字段 disabled，对其 focus 是 no-op，跳过）。
		if (next === 'rules' && !loading) {
			tick().then(() => excludeDirsEl?.focus());
		} else {
			// 通用区没有主文本输入，焦点回卡片即可保证面板继续吞掉全局快捷键。
			tick().then(() => cardEl?.focus());
		}
	}

	// ── 改键 ────────────────────────────────────────────────────────────────
	function startCapture() {
		hotkeyMsg = '';
		pendingCombo = null;
		pendingLabel = '';
		capturing = true;
		// 聚焦捕获按钮，键盘事件落在它上面。切到捕获态后按钮元素会被替换成
		// 另一个（文案不同），所以等 DOM 更新后再抢焦点。
		tick().then(() => hotkeyBtnEl?.focus());
	}

	function captureKeydown(e: KeyboardEvent) {
		if (!capturing) return;
		e.preventDefault();
		// 关键：stopPropagation 别让 Esc/按键冒泡到卡片的 onkeydown（那个会关
		// 面板）。捕获态下 Esc 只退捕获、不关面板（捕获态优先）。
		e.stopPropagation();
		if (e.key === 'Escape') {
			capturing = false;
			tick().then(() => cardEl?.focus());
			return;
		}
		const code = e.code;
		// 只按了修饰键、或拿不到 code：继续等一个主键。
		if (!code || MODIFIER_CODES.has(code)) return;
		const mods: string[] = [];
		if (e.ctrlKey) mods.push('Ctrl');
		if (e.altKey) mods.push('Alt');
		if (e.shiftKey) mods.push('Shift');
		if (e.metaKey) mods.push('Super'); // Windows 键，Tauri 解析认 SUPER
		if (mods.length === 0) {
			// 主键没配修饰键——不接受，提示后继续捕获（任务要求"须含至少一个
			// 修饰键 + 一个主键"才预览）。
			hotkeyMsg = t.setHotkeyNeedModifier;
			return;
		}
		// 组合键用 e.code 名（KeyK / Backquote / Digit1 …）拼——既是 Tauri 的
		// Shortcut 解析认的 token，也是 formatHotkey 认的输入。进入"预览待确认"态。
		pendingCombo = [...mods, code].join('+');
		pendingLabel = formatHotkey(pendingCombo);
		hotkeyMsg = '';
		capturing = false;
		tick().then(() => confirmBtnEl?.focus());
	}

	function captureBlur() {
		// 焦点离开捕获按钮（还没捕到组合键）就退出捕获态，别留一个"还在捕获
		// 但用户已走开"的幽灵态。已经预览出 pendingCombo 的分支不受影响。
		if (capturing) capturing = false;
	}

	async function confirmHotkey() {
		if (!pendingCombo) return;
		const combo = pendingCombo;
		try {
			await api.setHotkey(combo);
			hotkeyLabel = formatHotkey(combo);
			hotkeyMsg = t.setHotkeySaved;
		} catch (err) {
			// 注册失败（多半被占用）——Rust 侧已回滚到旧键，这里保持旧的
			// hotkeyLabel 不动，只报错。
			hotkeyMsg = t.setHotkeyFailed(String(err));
		} finally {
			pendingCombo = null;
			pendingLabel = '';
			// 确认/取消的按钮消失后焦点会掉到 body，把它收回卡片，保证面板继续
			// 吞掉全局快捷键、Esc 仍能关面板。
			tick().then(() => cardEl?.focus());
		}
	}

	function cancelHotkey() {
		pendingCombo = null;
		pendingLabel = '';
		hotkeyMsg = '';
		capturing = false;
		tick().then(() => cardEl?.focus());
	}

	// ── 透明 / 自启 / 语言：即存即生效，无"保存"按钮 ────────────────────────
	function pickTransparency(on: boolean) {
		transparencyEnabled = on;
		api.setTransparencyEnabled(on).catch((e) => console.error('setTransparencyEnabled failed', e));
	}

	function pickTier(next: TransparencyTier) {
		tier = next;
		api.setTransparencyTier(next).catch((e) => console.error('setTransparencyTier failed', e));
	}

	function pickAutostart(on: boolean) {
		autostartEnabled = on;
		api.setAutostart(on).catch((e) => {
			// 系统拒绝写自启项等——回滚 UI 勾选态，别让面板显示成功了实则没生效。
			autostartEnabled = !on;
			console.error('setAutostart failed', e);
		});
	}

	function pickLang(next: LangOption) {
		if (next === lang) return;
		lang = next;
		langChanged = true; // 显示"重启后生效"
		api
			.setLang(next)
			.then(() => {
				// 同步前端 i18n 的启动镜像：下次启动 i18n.ts 同步读它决定语言
				// （见 i18n.ts 顶部说明）。写失败无所谓——+page.svelte 启动时还会
				// 从 config 兜底同步一次。
				try {
					localStorage.setItem(LANG_OVERRIDE_KEY, next);
				} catch {
					// localStorage 不可用就算了，config 已经落盘，重启读 config 兜底。
				}
			})
			.catch((e) => console.error('setLang failed', e));
	}

	// ── 键盘：Esc 关面板（捕获态例外，已在 captureKeydown 里先行拦截） ──────
	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			e.preventDefault();
			e.stopPropagation();
			onclose();
		}
	}

	onMount(() => {
		// 数据回来之前先把焦点占住（卡片 tabindex="-1" 任何时候都可聚焦）——
		// 不能先聚焦文本框：字段 loading 期间 disabled，对 disabled 元素 focus 是
		// no-op，焦点会滞留在背后搜索框上，导致"焦点进面板吞掉全局快捷键"的前提
		// 落空（Esc 甚至会把整个窗口藏掉）。默认停在通用区，本来也不聚焦文本框。
		cardEl?.focus();

		// 通用区初值：拿不到就用默认展示值，不阻塞面板可用。
		api
			.getConfig()
			.then((cfg) => {
				hotkeyLabel = formatHotkey(cfg.hotkey);
				transparencyEnabled = cfg.transparency_enabled;
				tier = cfg.transparency_tier;
				autostartEnabled = cfg.autostart_enabled;
				lang = cfg.lang;
			})
			.catch(() => {
				// 静默：通用区退回默认展示值即可。
			});

		// 索引规则初值（原逻辑）。
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
</script>

<!-- 分段控件片段：透明开关/透明度三档/开机自启/界面语言四处共用，避免为
     单处逻辑各写一遍，也不值得拆成独立组件。disabled 时整组置灰。 -->
{#snippet segmented(
	options: { value: string; label: string }[],
	current: string,
	onpick: (v: string) => void,
	disabled: boolean
)}
	<div class="seg" role="group">
		{#each options as opt (opt.value)}
			<button
				type="button"
				class="seg-btn"
				class:active={opt.value === current}
				{disabled}
				onclick={() => onpick(opt.value)}
			>
				{opt.label}
			</button>
		{/each}
	</div>
{/snippet}

<!-- 遮罩点击关闭，卡片上 stopPropagation 挡住冒泡，交互上等价于"点卡片外面
     才关"。遮罩用 div 而不是 <button> 包一切——面板里是真实表单控件，不能被
     外层按钮抢走点击事件。 -->
<div class="scrim" role="presentation" onclick={onclose} onkeydown={handleKeydown}>
	<div
		bind:this={cardEl}
		class="card"
		role="dialog"
		aria-modal="true"
		aria-label={t.setTitle}
		tabindex="-1"
		onclick={(e) => e.stopPropagation()}
		onkeydown={handleKeydown}
	>
		<div class="head">
			<div class="tabs" role="tablist">
				<button
					type="button"
					class="tab"
					class:active={tab === 'general'}
					role="tab"
					aria-selected={tab === 'general'}
					onclick={() => setTab('general')}
				>
					{t.setTabGeneral}
				</button>
				<button
					type="button"
					class="tab"
					class:active={tab === 'rules'}
					role="tab"
					aria-selected={tab === 'rules'}
					onclick={() => setTab('rules')}
				>
					{t.setTabRules}
				</button>
			</div>
			<button type="button" class="close" onclick={onclose} aria-label={t.setCloseLabel}>
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

		{#if tab === 'general'}
			<!-- 呼出快捷键改键 -->
			<div class="field">
				<span class="field-label">{t.setHotkeyLabel}</span>
				{#if capturing}
					<button
						bind:this={hotkeyBtnEl}
						type="button"
						class="hotkey-btn capturing"
						onkeydown={captureKeydown}
						onblur={captureBlur}
					>
						{t.setHotkeyCapturing}
					</button>
				{:else if pendingCombo}
					<div class="hotkey-row">
						<kbd class="hotkey-chip">{pendingLabel}</kbd>
						<button
							bind:this={confirmBtnEl}
							type="button"
							class="mini-btn primary"
							onclick={confirmHotkey}
						>
							{t.setHotkeyConfirm}
						</button>
						<button type="button" class="mini-btn" onclick={cancelHotkey}>
							{t.setHotkeyCancel}
						</button>
					</div>
				{:else}
					<div class="hotkey-row">
						<kbd class="hotkey-chip">{hotkeyLabel || '—'}</kbd>
						<button type="button" class="mini-btn" onclick={startCapture}>
							{t.setHotkeyChange}
						</button>
					</div>
				{/if}
				<p class="field-hint">{t.setHotkeyHint}</p>
				{#if hotkeyMsg}
					<p class="status">{hotkeyMsg}</p>
				{/if}
			</div>

			<!-- 透明效果 + 透明度三档 -->
			<div class="field">
				<div class="field-inline">
					<span class="field-label">{t.setTransparencyLabel}</span>
					{@render segmented(
						ONOFF_OPTIONS,
						transparencyEnabled ? 'on' : 'off',
						(v) => pickTransparency(v === 'on'),
						false
					)}
				</div>
				<div class="field-inline">
					<span class="field-label sub">{t.setTierLabel}</span>
					{@render segmented(TIER_OPTIONS, tier, (v) => pickTier(v as TransparencyTier), !transparencyEnabled)}
				</div>
			</div>

			<!-- 开机自启 -->
			<div class="field">
				<div class="field-inline">
					<span class="field-label">{t.setAutostartLabel}</span>
					{@render segmented(
						ONOFF_OPTIONS,
						autostartEnabled ? 'on' : 'off',
						(v) => pickAutostart(v === 'on'),
						false
					)}
				</div>
			</div>

			<!-- 界面语言 -->
			<div class="field">
				<div class="field-inline">
					<span class="field-label">{t.setLangLabel}</span>
					{@render segmented(LANG_OPTIONS, lang, (v) => pickLang(v as LangOption), false)}
				</div>
				{#if langChanged}
					<p class="field-hint restart">{t.setLangRestartHint}</p>
				{/if}
			</div>
		{:else}
			<!-- 索引规则（原三项原样迁入，保留 保存/立即重建 逻辑） -->
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
		width: min(440px, calc(100% - 48px));
		padding: 18px 22px 20px;
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

	/* 分区切换：两个克制的文字 tab，非当前项更淡；下方一条发丝线兜底分隔。
	   不做底色块或大按钮——跟主面板"幽灵控件"的克制一致。 */
	.tabs {
		display: flex;
		gap: 4px;
	}

	.tab {
		font: inherit;
		font-size: 12px;
		font-weight: 500;
		letter-spacing: 0.02em;
		padding: 4px 10px;
		border: none;
		background: transparent;
		border-radius: var(--radius-chip);
		color: var(--fg-tertiary);
		cursor: default;
	}

	.tab:hover {
		color: var(--fg-secondary);
	}

	.tab.active {
		color: var(--fg-primary);
		background: var(--row-hover);
	}

	/* 关闭按钮：跟标题同一档淡，只有 hover 才提亮——Esc/点遮罩才是关闭主路径，
	   这颗按钮只为鼠标用户兜底。 */
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
		gap: 6px;
	}

	/* 通用区多数项是"标签 + 右侧控件"的横排；纵排的（改键、索引规则文本框）
	   直接靠 .field 的 column 布局。 */
	.field-inline {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	.field-label {
		font-size: 11px;
		letter-spacing: 0.02em;
		color: var(--fg-secondary);
	}

	.field-label.sub {
		color: var(--fg-tertiary);
	}

	.field-hint {
		margin: 0;
		font-size: 10.5px;
		color: var(--fg-tertiary);
		opacity: 0.8;
		line-height: 1.5;
	}

	.field-hint.restart {
		color: var(--accent-strong);
		opacity: 1;
	}

	/* 分段控件：一圈描边、内部按钮平铺，当前项一个低调的填充。跟托盘三档
	   同一套心智模型，视觉上收在一颗 chip 的重量里。 */
	.seg {
		display: inline-flex;
		border: 1px solid var(--panel-border);
		border-radius: var(--radius-chip);
		overflow: hidden;
	}

	.seg-btn {
		font: inherit;
		font-size: 11.5px;
		padding: 4px 11px;
		border: none;
		background: transparent;
		color: var(--fg-secondary);
		cursor: default;
	}

	.seg-btn:not(:last-child) {
		border-right: 1px solid var(--panel-border);
	}

	.seg-btn:hover:not(:disabled):not(.active) {
		background: var(--row-hover);
		color: var(--fg-primary);
	}

	.seg-btn.active {
		background: var(--accent-soft);
		color: var(--accent-strong);
	}

	.seg-btn:disabled {
		opacity: 0.45;
	}

	/* 改键：当前键 chip + 改键/确认/取消 迷你按钮；捕获态是一颗占满的按钮。 */
	.hotkey-row {
		display: flex;
		align-items: center;
		gap: 8px;
	}

	.hotkey-chip {
		font-family: inherit;
		font-size: 11.5px;
		line-height: 1;
		padding: 5px 10px;
		border-radius: var(--radius-chip);
		background: var(--shortcut-chip-bg);
		color: var(--shortcut-chip-fg);
	}

	.hotkey-btn {
		font: inherit;
		font-size: 12px;
		text-align: left;
		padding: 7px 10px;
		border-radius: var(--radius-chip);
		border: 1px solid var(--accent-border);
		background: var(--accent-soft);
		color: var(--accent-strong);
		cursor: default;
	}

	.mini-btn {
		font: inherit;
		font-size: 11.5px;
		padding: 4px 11px;
		border-radius: var(--radius-chip);
		border: 1px solid var(--panel-border);
		background: transparent;
		color: var(--fg-secondary);
		cursor: default;
	}

	.mini-btn:hover {
		background: var(--row-hover);
		color: var(--fg-primary);
	}

	.mini-btn.primary {
		border-color: var(--accent-border);
		background: var(--accent-soft);
		color: var(--accent-strong);
	}

	.mini-btn.primary:hover {
		filter: brightness(1.05);
		background: var(--accent-soft);
		color: var(--accent-strong);
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
		color: var(--fg-secondary);
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
