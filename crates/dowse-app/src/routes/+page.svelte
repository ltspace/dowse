<script lang="ts">
	import { onMount } from 'svelte';
	import { listen } from '@tauri-apps/api/event';
	import { getCurrentWindow } from '@tauri-apps/api/window';
	import { open } from '@tauri-apps/plugin-dialog';
	import { animate } from 'motion';

	import * as api from '$lib/api';
	import type {
		EffectLevel,
		ExtGroup,
		GlassAlpha,
		IndexingPhase,
		IndexProgress,
		SearchHit,
		SortOption,
		TextSegment
	} from '$lib/types';
	import ResultList from '$lib/components/ResultList.svelte';
	import PreviewPane from '$lib/components/PreviewPane.svelte';
	import ShortcutBar from '$lib/components/ShortcutBar.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import GhostDropdown from '$lib/components/GhostDropdown.svelte';
	import PinButton from '$lib/components/PinButton.svelte';
	import AnimatedNumber from '$lib/components/AnimatedNumber.svelte';
	import ShortcutOverlay from '$lib/components/ShortcutOverlay.svelte';
	import IndexingStrip from '$lib/components/IndexingStrip.svelte';
	import { formatHotkey } from '$lib/hotkey';

	// 类型筛选 / 排序器两个幽灵态下拉的选项表——顺序即菜单里的显示顺序，
	// 第一项永远是"默认值"（对应 GhostDropdown 的 defaultValue）。
	const TYPE_OPTIONS = [
		{ value: 'all', label: '全部' },
		{ value: 'doc', label: '文档' },
		{ value: 'code', label: '代码' },
		{ value: 'image', label: '图片' }
	] as const;
	const SORT_OPTIONS = [
		{ value: 'relevance', label: '相关性' },
		{ value: 'mtime_desc', label: '最新优先' },
		{ value: 'mtime_asc', label: '最旧优先' },
		{ value: 'size_desc', label: '最大优先' }
	] as const;

	let query = $state('');
	let hits = $state<SearchHit[]>([]);
	let selectedIndex = $state(0);
	let hasIndex = $state<boolean | null>(null);
	let numDocs = $state(0);
	/** 已注册的全部索引根（已过 display_path 清洗），空态逐行列出。 */
	let roots = $state<string[]>([]);
	let previewSegments = $state<TextSegment[] | null>(null);
	let previewLoading = $state(false);
	let rebuildState = $state<'idle' | 'rebuilding' | 'error'>('idle');
	let rebuildError = $state('');
	let toast = $state('');

	// 建索引"实时直播"：processed/currentFile 在整个重建期间随
	// dowse://rebuild-progress 事件滚动更新；report 只在重建成功的那一刻
	// 赋值，浮窗拿它替换掉实时计数、停留片刻后再收回引导层（见
	// pickDirectoryAndRebuild）。
	let indexingProcessed = $state(0);
	let indexingCurrentFile = $state('');
	let indexingReport = $state<{ indexed: number; seconds: number } | null>(null);

	// 建索引进度的"活的"状态：`indexingPhase` 是单一事实来源，决定要不要展示
	// 文本阶段的全屏引导层（'text'）、OCR 阶段的常驻进度条（'ocr'，不遮挡
	// 搜索结果）。事件流只在窗口开着时有意义，`refreshIndexingStatus` 在
	// 窗口每次呼出/挂载时拉一次快照续播，避免窗口隐藏期间错过事件后留下
	// 一片空白（症状 2/3 的验收场景：反复唤出/隐藏窗口，进度视图每次都活着）。
	let indexingPhase = $state<IndexingPhase>('idle');
	let indexingOcrProcessed = $state(0);
	let indexingOcrTotal = $state(0);

	// 本次搜索耗时（发起请求到结果上屏），页脚小字用；null 表示还没有可展示
	// 的一次搜索（空查询/首次挂载）。刻意不做滚动动画——每次搜索都变，
	// 动画反而晃眼。
	let lastSearchMs = $state<number | null>(null);

	let extGroup = $state<ExtGroup>('all');
	let sortOption = $state<SortOption>('relevance');
	let typeMenuOpen = $state(false);
	let sortMenuOpen = $state(false);
	let typeMenuIndex = $state(0);
	let sortMenuIndex = $state(0);
	let pinned = $state(false);
	let shortcutOverlayOpen = $state(false);
	let hotkeyLabel = $state('Alt+`');

	let inputEl: HTMLInputElement | undefined = $state();
	let panelEl: HTMLDivElement | undefined = $state();
	let caretFlourishEl: HTMLSpanElement | undefined = $state();
	let controlsEl: HTMLDivElement | undefined = $state();

	let selectedHit = $derived(hits[selectedIndex] ?? null);

	// 文本阶段（'text'）无论触发源（浮窗按钮/托盘"重建索引"/托盘"更改索引
	// 文件夹…"）都要接管成全屏引导层——`indexingPhase` 是跨触发源统一的
	// 事实来源；`rebuildState === 'rebuilding'` 仍然保留，覆盖"点击到第一次
	// 状态同步之间"那一小段间隙，避免按钮点下去的瞬间还没转譯成
	// indexingPhase 更新时闪一下结果视图。OCR 阶段（'ocr'）不在这里处理——
	// 它不遮挡搜索结果，见下面的 <IndexingStrip>。
	let guidanceKind = $derived.by((): 'idle' | 'no-index' | 'no-results' | 'rebuilding' | 'error' => {
		if (rebuildState === 'rebuilding' || indexingPhase === 'text') return 'rebuilding';
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
			roots = status.roots;
		} catch {
			hasIndex = false;
			numDocs = 0;
			roots = [];
		}
	}

	/// OCR 队列进度的归一化更新：`pending` 是队列里还剩多少张待处理，跟
	/// Rust 侧 `indexing_status.rs::set_ocr_pending` 同一套语义（常驻监听
	/// 期间又发现新图片时顺势抬高 total，保证 processed 不会算出负数）。
	function applyOcrPending(pending: number) {
		if (pending <= 0) {
			indexingPhase = 'idle';
			indexingOcrProcessed = 0;
			indexingOcrTotal = 0;
			return;
		}
		indexingPhase = 'ocr';
		if (pending > indexingOcrTotal) indexingOcrTotal = pending;
		indexingOcrProcessed = Math.max(0, indexingOcrTotal - pending);
	}

	/// 拉一次后端的"当前建索引进度"快照，跟事件流接续起来——窗口每次呼出
	/// 都要调用一次：事件在窗口隐藏期间照样会发，但前端没监听、没地方存，
	/// 重新唤出时必须能补一次，不能是一片空白或者停在呼出前那一刻的旧状态。
	async function refreshIndexingStatus() {
		try {
			const snap = await api.indexingStatus();
			indexingPhase = snap.phase;
			if (snap.phase === 'text') {
				indexingProcessed = snap.text_processed;
				indexingCurrentFile = snap.text_current_file;
			} else if (snap.phase === 'ocr') {
				indexingOcrTotal = snap.ocr_total;
				indexingOcrProcessed = snap.ocr_processed;
			}
		} catch {
			// 拉取失败（比如 Tauri IPC 一次性抖动）保留上一次已知状态，
			// 不主动清空——清空反而会让活着的进度视图短暂"消失"一下。
		}
	}

	// 键入 30ms 防抖即搜——查询词变了就重新发起，过期响应用 token 挡掉。
	// 类型筛选 / 排序器的选择跟查询词一起参与防抖：选中即重搜，不需要额外
	// 的"应用"按钮。
	let searchToken = 0;
	$effect(() => {
		const q = query;
		const group = extGroup;
		const sort = sortOption;
		const token = ++searchToken;
		if (q.trim().length === 0) {
			hits = [];
			selectedIndex = 0;
			lastSearchMs = null;
			return;
		}
		const timer = setTimeout(async () => {
			// 计时窗口：从这里"发起请求"到下面结果赋值"上屏"，不含防抖等待——
			// 页脚毫秒数是给用户看引擎有多快，不是给他们看输了多久的字。
			const startedAt = performance.now();
			try {
				const results = await api.search(q, 50, group, sort);
				if (token !== searchToken) return;
				hits = results;
				selectedIndex = 0;
				lastSearchMs = performance.now() - startedAt;
			} catch (err) {
				if (token !== searchToken) return;
				hits = [];
				lastSearchMs = null;
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
		indexingPhase = 'text';
		indexingProcessed = 0;
		indexingCurrentFile = '';
		indexingReport = null;
		try {
			const stats = await api.rebuildIndex(dir);
			hasIndex = true;
			refreshIndexStatus();
			// 文本阶段结束，交接给 OCR 阶段（如果还有图片没识别完）——不等
			// 第一个 `dowse://ocr-progress` 事件，直接用这次 invoke 返回的
			// 初始值起播，衔接文本直播 → 完成报告 → 图片余量递减的完整时间线。
			applyOcrPending(stats.ocr_pending);
			// 冷报告替换掉实时计数，停留片刻后收回整个引导层——用户看得见
			// "完成了"，不需要再点一下才能回到搜索态。
			const report = { indexed: stats.indexed, seconds: stats.seconds };
			indexingReport = report;
			setTimeout(() => {
				if (indexingReport !== report) return;
				rebuildState = 'idle';
				indexingReport = null;
			}, 1800);
		} catch (err) {
			rebuildState = 'error';
			rebuildError = String(err);
			indexingPhase = 'idle';
		}
	}

	/// 空态"添加文件夹"链接：多根索引场景下追加一个根，不动现有内容——跟
	/// pickDirectoryAndRebuild 走的是同一套"实时直播 + 冷报告"UI 节奏
	/// （同一批 dowse://rebuild-progress 事件），唯一区别是调用的命令
	/// （add_root 而不是 rebuild_index）和完成后不整体替换 hasIndex/roots，
	/// 而是照常刷新（roots 会多出这一项）。
	async function pickDirectoryAndAddRoot() {
		const dir = await open({ directory: true, multiple: false, title: '选择要添加的文件夹' });
		if (!dir || Array.isArray(dir)) return;

		rebuildState = 'rebuilding';
		indexingPhase = 'text';
		indexingProcessed = 0;
		indexingCurrentFile = '';
		indexingReport = null;
		try {
			const stats = await api.addRoot(dir);
			refreshIndexStatus();
			applyOcrPending(stats.ocr_pending);
			const report = { indexed: stats.indexed, seconds: stats.seconds };
			indexingReport = report;
			setTimeout(() => {
				if (indexingReport !== report) return;
				rebuildState = 'idle';
				indexingReport = null;
			}, 1800);
		} catch (err) {
			rebuildState = 'error';
			rebuildError = String(err);
			indexingPhase = 'idle';
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

	function togglePinned() {
		pinned = !pinned;
		api.setPinned(pinned).catch((err) => console.error('setPinned failed', err));
	}

	function showContextMenu(i: number) {
		selectedIndex = i;
		const hit = hits[i];
		if (!hit) return;
		api.showResultContextMenu(hit.path).catch((err) => console.error('showResultContextMenu failed', err));
	}

	function closeMenus() {
		typeMenuOpen = false;
		sortMenuOpen = false;
	}

	function openTypeMenu() {
		sortMenuOpen = false;
		typeMenuOpen = !typeMenuOpen;
		typeMenuIndex = Math.max(
			0,
			TYPE_OPTIONS.findIndex((o) => o.value === extGroup)
		);
	}

	function openSortMenu() {
		typeMenuOpen = false;
		sortMenuOpen = !sortMenuOpen;
		sortMenuIndex = Math.max(
			0,
			SORT_OPTIONS.findIndex((o) => o.value === sortOption)
		);
	}

	function handleKeydown(e: KeyboardEvent) {
		// 速查浮层打开期间，主输入完全不响应——任意键都只用来关掉浮层，
		// 不会漏进搜索框变成一个字符，也不会触发下面任何快捷键分支。
		if (shortcutOverlayOpen) {
			e.preventDefault();
			shortcutOverlayOpen = false;
			return;
		}
		if (e.ctrlKey && e.key === '/') {
			e.preventDefault();
			closeMenus();
			shortcutOverlayOpen = true;
			return;
		}

		// Ctrl+P / Ctrl+S 开关类型/排序菜单，Ctrl+D 切换图钉——都不进底部快捷键
		// 提示条（保持底部简洁），只在这份速查浮层里能查到。两个下拉菜单互斥：
		// 开一个就关另一个。
		if (e.ctrlKey && e.key.toLowerCase() === 'p') {
			e.preventDefault();
			openTypeMenu();
			return;
		}
		if (e.ctrlKey && e.key.toLowerCase() === 's') {
			e.preventDefault();
			openSortMenu();
			return;
		}
		if (e.ctrlKey && e.key.toLowerCase() === 'd') {
			e.preventDefault();
			togglePinned();
			return;
		}

		// 菜单打开时，↑↓/Enter/Esc 转去控制菜单本身，不透传给下面的结果列表
		// 导航——避免同一次按键既翻结果又翻菜单项。
		if (typeMenuOpen || sortMenuOpen) {
			const options = typeMenuOpen ? TYPE_OPTIONS : SORT_OPTIONS;
			let index = typeMenuOpen ? typeMenuIndex : sortMenuIndex;
			if (e.key === 'ArrowDown') {
				e.preventDefault();
				index = Math.min(index + 1, options.length - 1);
			} else if (e.key === 'ArrowUp') {
				e.preventDefault();
				index = Math.max(index - 1, 0);
			} else if (e.key === 'Enter') {
				e.preventDefault();
				const picked = options[index].value;
				if (typeMenuOpen) extGroup = picked as ExtGroup;
				else sortOption = picked as SortOption;
				closeMenus();
				return;
			} else if (e.key === 'Escape') {
				e.preventDefault();
				closeMenus();
				return;
			} else {
				return;
			}
			if (typeMenuOpen) typeMenuIndex = index;
			else sortMenuIndex = index;
			return;
		}

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

	// 面板可视不透明度收拢到这一个入口：两个数字（明/暗主题各一个 alpha）
	// 直接写进 CSS 变量，具体哪个生效由 app.css 的 prefers-color-scheme
	// 媒体查询决定，这里不用猜当前是明是暗。托盘切透明度档位时走
	// dowse://glass-alpha 事件复用同一个函数。
	function applyGlassAlpha(alpha: GlassAlpha) {
		document.documentElement.style.setProperty('--glass-alpha-light', String(alpha.light));
		document.documentElement.style.setProperty('--glass-alpha-dark', String(alpha.dark));
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
			{ height: ['0px', '20px'] },
			{ type: 'spring', bounce: 0.15, duration: 0.1 }
		).then(() => {
			if (!caretFlourishEl) return;
			animate(caretFlourishEl, { height: '0px' }, { duration: 0.08, ease: 'easeIn', delay: 0.1 });
		});
	}

	// 鼠标点了 .controls（两个下拉 + 图钉）以外的地方就收起菜单——两个下拉
	// 平时几乎不占视觉存在感，不能指望用户记得回来点一下 trigger 才能关掉。
	function handleDocumentClick(e: MouseEvent) {
		if (!typeMenuOpen && !sortMenuOpen) return;
		if (controlsEl && e.target instanceof Node && controlsEl.contains(e.target)) return;
		closeMenus();
	}

	onMount(() => {
		refreshIndexStatus();
		// 窗口挂载时也拉一次进度快照——覆盖"托盘触发的建索引正在跑，浮窗这时
		// 才第一次打开"的场景，不用等下一条事件才能看到活的进度。
		refreshIndexingStatus();
		focusAndSelectAll();

		api.getEffectLevel().then((level: EffectLevel) => {
			document.documentElement.dataset.effect = level;
		});
		api.getGlassAlpha().then(applyGlassAlpha);
		api.getHotkey().then((raw) => {
			hotkeyLabel = formatHotkey(raw);
		});

		document.addEventListener('click', handleDocumentClick);

		const unlistenShown = listen('dowse://shown', () => {
			refreshIndexStatus();
			// 窗口重新唤出时必须能看到活的进度，不能是呼出前那一刻的旧快照，
			// 更不能是空白——建索引期间反复隐藏/唤出窗口是这套进度视图的
			// 核心验收场景（症状 2/3）。
			refreshIndexingStatus();
			focusAndSelectAll();
			playShowAnimation();
			closeMenus();
		});
		const unlistenEffect = listen<EffectLevel>('dowse://effect-level', (evt) => {
			document.documentElement.dataset.effect = evt.payload;
		});
		const unlistenGlassAlpha = listen<GlassAlpha>('dowse://glass-alpha', (evt) => {
			applyGlassAlpha(evt.payload);
		});
		const unlistenRebuildDone = listen<number>('dowse://rebuild-done', (evt) => {
			refreshIndexStatus();
			// 托盘触发的重建（"重建索引"/"更改索引文件夹…"）没有走本地的
			// pickDirectoryAndRebuild，指望这里补一次快照拉取，好在窗口这时
			// 恰好开着的话立刻接上 OCR 阶段的进度条，不用等下一条事件。
			refreshIndexingStatus();
			showToast(`索引重建完成，收录 ${evt.payload} 个文件。`);
		});
		const unlistenRebuildError = listen<string>('dowse://rebuild-error', (evt) => {
			refreshIndexingStatus();
			showToast(`索引重建失败：${evt.payload}`);
		});
		// 托盘"移除"文件夹的成功回执——移除没有"收录数"可言，走独立事件而不是
		// 复用 dowse://rebuild-done（那个的 toast 文案是"收录 N 个文件"，套用
		// 到移除操作上语义是反的）。
		const unlistenRootRemoved = listen<number>('dowse://root-removed', (evt) => {
			refreshIndexStatus();
			showToast(`已移除该文件夹，删除 ${evt.payload} 篇文档。`);
		});
		// 建索引"实时直播"：全程监听，不只在 rebuildState === 'rebuilding' 时才挂——
		// 事件只在 rebuild_index 命令执行期间才会发出，不重建时这个监听器闲置无害。
		const unlistenRebuildProgress = listen<IndexProgress>('dowse://rebuild-progress', (evt) => {
			indexingPhase = 'text';
			indexingProcessed = evt.payload.processed;
			indexingCurrentFile = evt.payload.path;
		});
		// OCR 队列消化进度：payload 是这一刻队列里还剩多少张待处理——
		// 症状 3 的修复核心，v0.6.1 之前这行字是重建完成那一刻的静态快照，
		// 从不刷新；现在跟着 OCR worker 每个 flush 批次持续推送、持续下降。
		const unlistenOcrProgress = listen<number>('dowse://ocr-progress', (evt) => {
			applyOcrPending(evt.payload);
		});

		return () => {
			document.removeEventListener('click', handleDocumentClick);
			unlistenShown.then((f) => f());
			unlistenEffect.then((f) => f());
			unlistenGlassAlpha.then((f) => f());
			unlistenRebuildDone.then((f) => f());
			unlistenRebuildError.then((f) => f());
			unlistenRootRemoved.then((f) => f());
			unlistenRebuildProgress.then((f) => f());
			unlistenOcrProgress.then((f) => f());
		};
	});
</script>

<div class="panel" bind:this={panelEl}>
	<div class="search-row">
		<svg class="search-icon" width="20" height="20" viewBox="0 0 18 18" fill="none" aria-hidden="true">
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
		<div class="controls" bind:this={controlsEl}>
			<GhostDropdown
				idleLabel="全部类型"
				options={TYPE_OPTIONS}
				value={extGroup}
				defaultValue="all"
				open={typeMenuOpen}
				bind:activeIndex={typeMenuIndex}
				onselect={(v) => {
					extGroup = v as ExtGroup;
					closeMenus();
				}}
				ontoggle={openTypeMenu}
			/>
			<GhostDropdown
				idleLabel="相关性"
				options={SORT_OPTIONS}
				value={sortOption}
				defaultValue="relevance"
				open={sortMenuOpen}
				bind:activeIndex={sortMenuIndex}
				onselect={(v) => {
					sortOption = v as SortOption;
					closeMenus();
				}}
				ontoggle={openSortMenu}
			/>
			<PinButton {pinned} onclick={togglePinned} />
		</div>
	</div>

	<div class="body">
		{#if showGuidance}
			<EmptyState
				kind={guidanceKind}
				{query}
				{numDocs}
				errorMessage={rebuildError}
				{indexingProcessed}
				{indexingCurrentFile}
				{indexingReport}
				{roots}
				onpick={pickDirectoryAndRebuild}
				onaddfolder={pickDirectoryAndAddRoot}
			/>
		{:else}
			<div class="results">
				<div class="results-heading">
					<span>结果 · <AnimatedNumber value={hits.length} /> 条</span>
					{#if lastSearchMs !== null}
						<span class="search-ms">{Math.round(lastSearchMs)}ms</span>
					{/if}
				</div>
				<ResultList
					{hits}
					{selectedIndex}
					onhover={(i) => (selectedIndex = i)}
					onselect={(i) => {
						selectedIndex = i;
						openSelected();
					}}
					oncontextmenu={showContextMenu}
				/>
			</div>
			<div class="divider-v"></div>
			<div class="preview-col">
				<PreviewPane hit={selectedHit} segments={previewSegments} loading={previewLoading} />
			</div>
		{/if}
	</div>

	<!-- OCR 回填阶段的常驻进度条：跟 showGuidance 的分支是平级关系，不遮挡
	     搜索结果——文本已经 commit 的内容立刻可搜，图片在后台慢慢识别，
	     这条状态是独立叠加在结果视图之上的，不是替换（症状 4 的验收场景：
	     建索引期间反复搜索必须可用）。 -->
	{#if indexingPhase === 'ocr'}
		<IndexingStrip processed={indexingOcrProcessed} total={indexingOcrTotal} />
	{/if}

	<ShortcutBar hasSelection={selectedHit !== null} />

	{#if toast}
		<div class="toast">{toast}</div>
	{/if}

	{#if shortcutOverlayOpen}
		<ShortcutOverlay hotkey={hotkeyLabel} onclose={() => (shortcutOverlayOpen = false)} />
	{/if}
</div>

<style>
	/* v0.4.1 曾经让 .panel 内缩 16px（inset: var(--panel-margin)）给
	   box-shadow 留渲染空间——结论错了，撤销，别再试：DWM 的 Acrylic/Mica
	   是整个窗口生效的合成效果，不认 CSS 布局留出来的"空白"，缩出来的这一
	   圈边距照样被渲染成玻璃，视觉上就成了"外面一圈裸玻璃画框、里面一个
	   .panel 边框"的双框。整窗玻璃和"用内缩+CSS阴影模拟悬浮"这个方案在
	   物理上不兼容，不是哪个数值没调对，任何再往这个方向调 inset 数值的
	   尝试都会复现同一个问题。
	   悬浮感的代价就此放弃——.panel 满铺整个窗口（inset: 0），只留一圈
	   1px 半透明描边勾出边界，不再画阴影。position: absolute 以窗口
	   （初始包含块）为参照，而不是随便找一个祖先元素——html/body 都没有
	   设 position，天然就是这个参照系。 */
	.panel {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		background: var(--glass-tint);
		border-radius: var(--radius-window);
		border: 1px solid var(--panel-border);
		overflow: hidden;
	}

	/* 输入区刻意不做"框"——没有边框、没有底色块，大字号裸排；下面这条
	   1px 低对比发丝线才是分隔输入区和结果区的唯一视觉边界，Raycast 同款。 */
	.search-row {
		display: flex;
		align-items: center;
		gap: 12px;
		padding: 16px 24px;
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
		font-size: 22px;
		font-weight: 400;
		caret-color: var(--accent-caret);
	}

	/* 类型筛选 / 排序器 / 图钉——三个都是默认态"几乎不存在"的幽灵控件，
	   紧贴输入区右侧，跟输入框之间留一点呼吸但不占视觉重量。 */
	.controls {
		display: flex;
		align-items: center;
		gap: 2px;
		flex-shrink: 0;
	}

	.search-input::placeholder {
		color: var(--fg-placeholder);
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
		display: flex;
		flex-direction: column;
		overflow: hidden;
	}

	/* 区分小标题：只在真的有结果时出现（showGuidance 为假意味着 hits 非空，
	   见 +page.svelte 顶部的 guidanceKind 推导），空态/建索引态走 EmptyState
	   分支，不会渲染到这里。 */
	.results-heading {
		flex-shrink: 0;
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: 8px;
		padding: 12px 16px 8px;
		font-size: 11px;
		letter-spacing: 0.04em;
		color: var(--fg-tertiary);
	}

	/* 页脚毫秒数：不解释、不加图标，比小标题本身再淡一档，等宽数字继承
	   全局 body 的 tabular-nums。刻意不做滚动动画（AnimatedNumber）——
	   每次搜索都变，滚一下反而晃眼，见 +page.svelte 顶部 lastSearchMs 的注释。 */
	.search-ms {
		flex-shrink: 0;
		opacity: 0.7;
		letter-spacing: 0;
	}

	.divider-v {
		width: 1px;
		background: var(--divider);
		flex-shrink: 0;
	}

	.preview-col {
		flex: 1;
		min-width: 240px;
		overflow: hidden;
	}

	.toast {
		position: absolute;
		bottom: 48px;
		left: 50%;
		transform: translateX(-50%);
		background: var(--toast-bg);
		color: var(--toast-fg);
		font-size: 12px;
		padding: 8px 16px;
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
