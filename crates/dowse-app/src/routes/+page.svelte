<script lang="ts">
	import { onMount } from 'svelte';
	import { listen } from '@tauri-apps/api/event';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { open } from '@tauri-apps/plugin-dialog';
	import { animate } from 'motion';

	import * as api from '$lib/api';
	import type { EffectLevel, SearchHit, TextSegment } from '$lib/types';
	import ResultList from '$lib/components/ResultList.svelte';
	import PreviewPane from '$lib/components/PreviewPane.svelte';
	import ShortcutBar from '$lib/components/ShortcutBar.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';

	let query = $state('');
	let hits = $state<SearchHit[]>([]);
	let selectedIndex = $state(0);
	let hasIndex = $state<boolean | null>(null);
	let numDocs = $state(0);
	let previewSegments = $state<TextSegment[] | null>(null);
	let previewLoading = $state(false);
	let rebuildState = $state<'idle' | 'rebuilding' | 'error'>('idle');
	let rebuildError = $state('');
	let toast = $state('');

	let inputEl: HTMLInputElement | undefined = $state();
	let panelEl: HTMLDivElement | undefined = $state();
	let caretFlourishEl: HTMLSpanElement | undefined = $state();

	let selectedHit = $derived(hits[selectedIndex] ?? null);

	let guidanceKind = $derived.by((): 'idle' | 'no-index' | 'no-results' | 'rebuilding' | 'error' => {
		if (rebuildState === 'rebuilding') return 'rebuilding';
		if (rebuildState === 'error') return 'error';
		if (query.trim().length === 0) return 'idle';
		if (hasIndex === false) return 'no-index';
		if (hits.length === 0) return 'no-results';
		return 'idle';
	});
	let showGuidance = $derived(guidanceKind !== 'idle' || query.trim().length === 0);

	async function refreshIndexStatus() {
		try {
			const status = await api.indexStatus();
			hasIndex = status.has_index;
			numDocs = status.num_docs;
		} catch {
			hasIndex = false;
			numDocs = 0;
		}
	}

	// 键入 30ms 防抖即搜——查询词变了就重新发起，过期响应用 token 挡掉。
	let searchToken = 0;
	$effect(() => {
		const q = query;
		const token = ++searchToken;
		if (q.trim().length === 0) {
			hits = [];
			selectedIndex = 0;
			return;
		}
		const timer = setTimeout(async () => {
			try {
				const results = await api.search(q, 50);
				if (token !== searchToken) return;
				hits = results;
				selectedIndex = 0;
			} catch (err) {
				if (token !== searchToken) return;
				hits = [];
				console.error('search failed', err);
			}
		}, 30);
		return () => clearTimeout(timer);
	});

	// 选中行变了就换一份更长的预览上下文；轻微防抖避免按住方向键连续刷。
	let previewToken = 0;
	$effect(() => {
		const hit = selectedHit;
		const q = query;
		if (!hit) {
			previewSegments = null;
			previewLoading = false;
			return;
		}
		previewLoading = true;
		const token = ++previewToken;
		const timer = setTimeout(async () => {
			try {
				const result = await api.preview(hit.path, q);
				if (token !== previewToken) return;
				previewSegments = result?.segments ?? null;
			} catch (err) {
				if (token !== previewToken) return;
				previewSegments = null;
				console.error('preview failed', err);
			} finally {
				if (token === previewToken) previewLoading = false;
			}
		}, 40);
		return () => clearTimeout(timer);
	});

	function showToast(msg: string) {
		toast = msg;
		setTimeout(() => {
			if (toast === msg) toast = '';
		}, 2400);
	}

	async function pickDirectoryAndRebuild() {
		const dir = await open({ directory: true, multiple: false, title: '选择要索引的目录' });
		if (!dir || Array.isArray(dir)) return;

		rebuildState = 'rebuilding';
		try {
			const stats = await api.rebuildIndex(dir);
			rebuildState = 'idle';
			hasIndex = true;
			showToast(`索引建立完成，收录 ${stats.indexed} 个文件。`);
			refreshIndexStatus();
		} catch (err) {
			rebuildState = 'error';
			rebuildError = String(err);
		}
	}

	function openSelected() {
		if (!selectedHit) return;
		api.openFile(selectedHit.path).catch((err) => showToast(`文件打开失败：${err}`));
	}

	function revealSelected() {
		if (!selectedHit) return;
		api.revealInFolder(selectedHit.path).catch((err) => showToast(`定位文件夹失败：${err}`));
	}

	function copySelectedPath() {
		if (!selectedHit) return;
		navigator.clipboard
			.writeText(selectedHit.path)
			.then(() => showToast('路径已复制。'))
			.catch(() => showToast('复制失败。'));
	}

	function handleKeydown(e: KeyboardEvent) {
		if (e.key === 'Escape') {
			e.preventDefault();
			getCurrentWindow().hide();
			return;
		}
		if (e.key === 'ArrowDown') {
			e.preventDefault();
			if (hits.length > 0) selectedIndex = Math.min(selectedIndex + 1, hits.length - 1);
			return;
		}
		if (e.key === 'ArrowUp') {
			e.preventDefault();
			if (hits.length > 0) selectedIndex = Math.max(selectedIndex - 1, 0);
			return;
		}
		if (e.key === 'Enter') {
			e.preventDefault();
			if (e.ctrlKey) revealSelected();
			else openSelected();
			return;
		}
		if (e.key.toLowerCase() === 'c' && e.ctrlKey) {
			// 输入框里有正在编辑的文本选区时，让浏览器按正常复制处理，
			// 不要抢用户的编辑操作。
			const hasTextSelection =
				inputEl && inputEl.selectionStart !== null && inputEl.selectionStart !== inputEl.selectionEnd;
			if (!hasTextSelection && selectedHit) {
				e.preventDefault();
				copySelectedPath();
			}
		}
	}

	function focusAndSelectAll() {
		inputEl?.focus();
		inputEl?.select();
	}

	// 呼出的手感：轻微放大 + 淡入，全程压在 120ms 以内的弹簧物理，不是缓动曲线。
	// 用显式 keyframe（而不是读当前样式）保证每次呼出都从同一个起点播，
	// 不会因为上一次动画没播完就被打断而出现错位。
	function playShowAnimation() {
		if (!panelEl) return;
		animate(
			panelEl,
			{ opacity: [0, 1], scale: [0.98, 1] },
			{ type: 'spring', bounce: 0.2, duration: 0.12 }
		);
		playCaretFlourish();
	}

	// 呼出瞬间的光标手感：一根装饰性的竖条从 0 高度弹到全高，跟输入框呼出
	// 动画同一时刻起播。呼出时上次查询词会被全选（focusAndSelectAll），
	// 原生光标本来就被选区盖住看不见，这根竖条负责传达"已经就绪、可以打字
	// 了"的瞬时反馈；短暂停留后自己收回去，交回给原生光标，不留一根常驻的
	// 假光标在那里。
	function playCaretFlourish() {
		if (!caretFlourishEl) return;
		animate(
			caretFlourishEl,
			{ height: ['0px', '17px'] },
			{ type: 'spring', bounce: 0.15, duration: 0.1 }
		).then(() => {
			if (!caretFlourishEl) return;
			animate(caretFlourishEl, { height: '0px' }, { duration: 0.08, ease: 'easeIn', delay: 0.1 });
		});
	}

	onMount(() => {
		refreshIndexStatus();
		focusAndSelectAll();

		api.getEffectLevel().then((level: EffectLevel) => {
			document.documentElement.dataset.effect = level;
		});

		const unlistenShown = listen('dowse://shown', () => {
			refreshIndexStatus();
			focusAndSelectAll();
			playShowAnimation();
		});
		const unlistenEffect = listen<EffectLevel>('dowse://effect-level', (evt) => {
			document.documentElement.dataset.effect = evt.payload;
		});
		const unlistenRebuildDone = listen<number>('dowse://rebuild-done', (evt) => {
			refreshIndexStatus();
			showToast(`索引重建完成，收录 ${evt.payload} 个文件。`);
		});
		const unlistenRebuildError = listen<string>('dowse://rebuild-error', (evt) => {
			showToast(`索引重建失败：${evt.payload}`);
		});

		return () => {
			unlistenShown.then((f) => f());
			unlistenEffect.then((f) => f());
			unlistenRebuildDone.then((f) => f());
			unlistenRebuildError.then((f) => f());
		};
	});
</script>

<div class="panel" bind:this={panelEl}>
	<div class="search-row">
		<svg class="search-icon" width="18" height="18" viewBox="0 0 18 18" fill="none" aria-hidden="true">
			<circle cx="8" cy="8" r="5.4" stroke="currentColor" stroke-width="1.4" />
			<path d="M12.2 12.2 16 16" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" />
		</svg>
		<div class="input-wrap">
			<input
				bind:this={inputEl}
				type="text"
				class="search-input"
				placeholder="搜文件名或内容…"
				bind:value={query}
				onkeydown={handleKeydown}
				autocomplete="off"
				spellcheck="false"
			/>
			<span class="caret-flourish" bind:this={caretFlourishEl} aria-hidden="true"></span>
		</div>
	</div>

	<div class="body">
		{#if showGuidance}
			<EmptyState
				kind={guidanceKind}
				{query}
				{numDocs}
				errorMessage={rebuildError}
				onpick={pickDirectoryAndRebuild}
			/>
		{:else}
			<div class="results">
				<ResultList
					{hits}
					{selectedIndex}
					onhover={(i) => (selectedIndex = i)}
					onselect={(i) => {
						selectedIndex = i;
						openSelected();
					}}
				/>
			</div>
			<div class="divider-v"></div>
			<div class="preview-col">
				<PreviewPane hit={selectedHit} segments={previewSegments} loading={previewLoading} />
			</div>
		{/if}
	</div>

	<ShortcutBar hasSelection={selectedHit !== null} />

	{#if toast}
		<div class="toast">{toast}</div>
	{/if}
</div>

<style>
	.panel {
		width: 100vw;
		height: 100vh;
		display: flex;
		flex-direction: column;
		background: var(--glass-tint);
		border-radius: var(--radius-window);
		border: 1px solid var(--divider);
		overflow: hidden;
		position: relative;
	}

	.search-row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 14px 20px;
		border-bottom: 1px solid var(--divider);
		flex-shrink: 0;
	}

	.search-icon {
		color: var(--fg-tertiary);
		flex-shrink: 0;
	}

	.input-wrap {
		position: relative;
		flex: 1;
		display: flex;
		align-items: center;
	}

	.search-input {
		flex: 1;
		border: none;
		outline: none;
		background: transparent;
		font-size: 19px;
		font-weight: 400;
		caret-color: var(--accent-caret);
	}

	.search-input::placeholder {
		color: var(--fg-tertiary);
	}

	/* 装饰性光标：呼出瞬间从 0 高度弹到全高的那一下手感，见 playCaretFlourish。
	   平时高度是 0（不可见），不跟原生光标打架。 */
	.caret-flourish {
		position: absolute;
		left: 0;
		top: 50%;
		width: 2px;
		height: 0px;
		transform: translateY(-50%);
		background: var(--accent-caret);
		border-radius: 1px;
		pointer-events: none;
	}

	.body {
		flex: 1;
		display: flex;
		min-height: 0;
	}

	.results {
		flex: 0 0 58%;
		min-width: 0;
		overflow: hidden;
	}

	.divider-v {
		width: 1px;
		background: var(--divider);
		flex-shrink: 0;
	}

	.preview-col {
		flex: 1;
		min-width: 0;
		overflow: hidden;
	}

	.toast {
		position: absolute;
		bottom: 42px;
		left: 50%;
		transform: translateX(-50%);
		background: var(--toast-bg);
		color: var(--toast-fg);
		font-size: 12px;
		padding: 7px 14px;
		border-radius: 8px;
		pointer-events: none;
		animation: toast-in 0.12s ease-out;
	}

	@keyframes toast-in {
		from {
			opacity: 0;
			transform: translate(-50%, 4px);
		}
		to {
			opacity: 1;
			transform: translate(-50%, 0);
		}
	}
</style>
