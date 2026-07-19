<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { listen } from '@tauri-apps/api/event';
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
	import IndexRulesPanel from '$lib/components/IndexRulesPanel.svelte';
	import { formatHotkey } from '$lib/hotkey';
	import { t } from '$lib/i18n';
	import { loadHistory, recordHistory, removeHistoryEntry, clearHistory } from '$lib/searchHistory';

	// 类型筛选 / 排序器两个幽灵态下拉的选项表——顺序即菜单里的显示顺序，
	// 第一项永远是"默认值"（对应 GhostDropdown 的 defaultValue）。文案跟随系统语言。
	const TYPE_OPTIONS = [
		{ value: 'all', label: t.filterAll },
		{ value: 'doc', label: t.filterDoc },
		{ value: 'code', label: t.filterCode },
		{ value: 'image', label: t.filterImage }
	];
	const SORT_OPTIONS = [
		{ value: 'relevance', label: t.sortRelevance },
		{ value: 'mtime_desc', label: t.sortNewest },
		{ value: 'mtime_asc', label: t.sortOldest },
		{ value: 'size_desc', label: t.sortLargest }
	];

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
	/** 索引规则面板（Ctrl+, 打开）。跟 shortcutOverlayOpen 不同，这个面板里
	 * 有真实表单字段，打开时 DOM 焦点会移进面板本身（见 IndexRulesPanel
	 * 组件顶部注释），不是靠 handleKeydown 拦截键盘。 */
	let rulesPanelOpen = $state(false);

	// 搜索历史：只在"打开了某条结果"的那一刻记（见 recordSearchHistory），
	// 不在击键/出结果时记。只在输入框为空时展示（historyMode），跟结果列表的
	// ↑↓/Enter 天然互斥——查询非空时 hits 才可能非空，两套导航不会抢键。
	let history = $state<string[]>([]);
	let historyIndex = $state(0);
	let historyMode = $derived(query.trim().length === 0 && history.length > 0);

	// 历史条目增删后夹紧选中下标，避免删空/删到末尾后指向一个不存在的位置。
	$effect(() => {
		if (historyIndex >= history.length) historyIndex = Math.max(0, history.length - 1);
	});

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
	// 的"应用"按钮。这个常量同时喂给下面的 setTimeout 和 reportSearchPerf
	// 的日志文案，避免两处各写一份 30 而将来改漏一处。
	const SEARCH_DEBOUNCE_MS = 30;
	let searchToken = 0;
	$effect(() => {
		const q = query;
		const group = extGroup;
		const sort = sortOption;
		const token = ++searchToken;
		// 击键到渲染性能埋点的起点：近似"触发搜索的输入事件"的时刻——
		// $effect 因 query/extGroup/sortOption 变化而重跑，跟用户敲键盘/
		// 切换筛选几乎同时发生。真正测的是下面 setTimeout 跑完之后（见
		// reportSearchPerf），中间的等待就是防抖窗口本身。
		const keystrokeAt = performance.now();
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
				reportSearchPerf(keystrokeAt, startedAt, token);
			} catch (err) {
				if (token !== searchToken) return;
				hits = [];
				lastSearchMs = null;
				console.error('search failed', err);
			}
		}, SEARCH_DEBOUNCE_MS);
		return () => clearTimeout(timer);
	});

	/// 击键到渲染性能埋点：`hits` 赋值只是把新数据排进 Svelte 的响应式
	/// 更新队列，不代表 DOM 已经画出来——`await tick()` 等 Svelte 把这次
	/// 更新刷进 DOM，再等一帧 `requestAnimationFrame` 确认浏览器完成绘制，
	/// 这时才是"结果渲染完成"的真实时刻。`token` 复核放在渲染之后——如果
	/// 这次搜索已经被后续输入取代（`token !== searchToken`），说明用户又
	/// 敲了字，这次上报的数字已经没有意义，静默丢弃。任何一步失败（理论上
	/// 不会，但 invoke 本身可能抖动）都吞掉，不影响搜索主流程。
	async function reportSearchPerf(keystrokeAt: number, invokedAt: number, token: number) {
		await tick();
		requestAnimationFrame(() => {
			if (token !== searchToken) return;
			const renderedAt = performance.now();
			const e2eMs = renderedAt - keystrokeAt;
			const netMs = renderedAt - invokedAt;
			api.reportSearchPerf(e2eMs, netMs, SEARCH_DEBOUNCE_MS).catch(() => {
				// 失败安全：埋点不能影响搜索主流程。
			});
		});
	}

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

	/// 全量重建的共同实现：`pickDirectoryAndRebuild`（先弹目录选择器）和
	/// `rebuildCurrentIndex`（索引规则面板"立即重建"，目标目录已知）都走
	/// 这一份，避免"实时直播 + 冷报告"这套 UI 节奏在两个入口各写一遍、
	/// 后续改动漏改一处。
	async function rebuildWithDir(dir: string) {
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
			const report = {
				indexed: stats.indexed,
				seconds: stats.seconds,
				skippedOversize: stats.skipped_oversize
			};
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

	async function pickDirectoryAndRebuild() {
		const dir = await open({ directory: true, multiple: false, title: t.dialogPickIndexFolder });
		if (!dir || Array.isArray(dir)) return;
		await rebuildWithDir(dir);
	}

	/// 索引规则面板"立即重建"：只在恰好一个索引根时可用（面板自己按
	/// `roots.length === 1` 禁用了按钮，这里的判断是防御性的，防止将来
	/// 谁绕过面板直接调这个函数）。`rebuild_index` 命令用单个目标目录整体
	/// 替换索引，多根场景下这么做会把其它根一并冲掉——多根索引目前没有
	/// 暴露"重建全部根"的前端命令，只有托盘每根子菜单的单根重建，所以
	/// 多根时不做任何事，交给面板上的提示文案引导用户去托盘操作。
	async function rebuildCurrentIndex() {
		if (roots.length !== 1) return;
		await rebuildWithDir(roots[0]);
	}

	function openRulesPanel() {
		typeMenuOpen = false;
		sortMenuOpen = false;
		rulesPanelOpen = true;
	}

	function closeRulesPanel() {
		rulesPanelOpen = false;
		// 面板关闭后把焦点交还给搜索框——面板打开期间焦点在表单字段里
		// （见 IndexRulesPanel 组件顶部注释），关闭动作不该让用户还要再点
		// 一下才能继续打字，跟呼出浮窗时 focusAndSelectAll 是同一个诉求。
		inputEl?.focus();
	}

	/// 空态"添加文件夹"链接：多根索引场景下追加一个根，不动现有内容——跟
	/// pickDirectoryAndRebuild 走的是同一套"实时直播 + 冷报告"UI 节奏
	/// （同一批 dowse://rebuild-progress 事件），唯一区别是调用的命令
	/// （add_root 而不是 rebuild_index）和完成后不整体替换 hasIndex/roots，
	/// 而是照常刷新（roots 会多出这一项）。
	async function pickDirectoryAndAddRoot() {
		const dir = await open({ directory: true, multiple: false, title: t.dialogAddFolder });
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
			const report = {
				indexed: stats.indexed,
				seconds: stats.seconds,
				skippedOversize: stats.skipped_oversize
			};
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

	/// 记一次搜索历史：只在用户明确要"打开"某条结果的时刻调用（openSelected/
	/// revealSelected/showContextMenu），不在击键或结果刷新时调用——那会把
	/// 历史刷满中间态。当前查询词为空时不记（没有词可记）。
	function recordSearchHistory() {
		if (query.trim().length === 0) return;
		history = recordHistory(query);
	}

	function openSelected() {
		if (!selectedHit) return;
		recordSearchHistory();
		api.openFile(selectedHit.path).catch((err) => showToast(t.toastOpenFailed(String(err))));
	}

	function revealSelected() {
		if (!selectedHit) return;
		recordSearchHistory();
		api.revealInFolder(selectedHit.path).catch((err) => showToast(t.toastRevealFailed(String(err))));
	}

	function copySelectedPath() {
		if (!selectedHit) return;
		navigator.clipboard
			.writeText(selectedHit.path)
			.then(() => showToast(t.toastPathCopied))
			.catch(() => showToast(t.toastCopyFailed));
	}

	function togglePinned() {
		pinned = !pinned;
		api.setPinned(pinned).catch((err) => console.error('setPinned failed', err));
	}

	function showContextMenu(i: number) {
		selectedIndex = i;
		const hit = hits[i];
		if (!hit) return;
		// 右键菜单的具体动作（打开/定位/复制路径）是 Rust 侧原生处理的
		// （context_menu.rs::handle_context_menu_event），前端拿不到"选中了
		// 哪一项"的回调；退而在弹出菜单的这一刻记录，近似"用户对这条结果
		// 表达了打开/定位的意图"——唯一的误差是"仅复制路径"也会计入历史，
		// 可接受（历史本身就是去重+faint 展示，不是精确审计）。
		recordSearchHistory();
		api.showResultContextMenu(hit.path).catch((err) => console.error('showResultContextMenu failed', err));
	}

	/// 历史条目被选中（Enter 或鼠标点击）：填入查询词并让已有的搜索 $effect
	/// 接管防抖+发起搜索，这里不用重复实现一遍。
	function selectHistoryEntry(q: string) {
		query = q;
		inputEl?.focus();
	}

	function removeHistoryAt(index: number) {
		const target = history[index];
		if (target === undefined) return;
		history = removeHistoryEntry(target);
	}

	function clearAllHistory() {
		history = clearHistory();
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
		// 规则面板打开期间的保险丝：正常情况下 DOM 焦点在面板内、本处理器
		// （绑在 .search-input 上）根本不会触发。但焦点转移可能落空——比如
		// 字段还在 loading disabled 态、或未来某个改动打断了聚焦时机——那时
		// 绝不能让按键漏进面板背后的搜索 UI：Esc 只关面板（不是藏整个窗口），
		// 其余按键一律吞掉。
		if (rulesPanelOpen) {
			e.preventDefault();
			if (e.key === 'Escape') {
				closeRulesPanel();
			}
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
		if (e.ctrlKey && e.key === ',') {
			e.preventDefault();
			closeMenus();
			openRulesPanel();
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

		// 历史条目导航：只在输入框为空且有历史时生效（historyMode）。查询非空
		// 时 hits 才可能非空，两套 ↑↓/Enter 天然互斥，不会抢键。Delete 删除
		// 选中条目——不影响 Backspace，输入框这时本来就是空的，Backspace
		// 依旧是正常的编辑行为。
		if (historyMode) {
			if (e.key === 'ArrowDown') {
				e.preventDefault();
				historyIndex = Math.min(historyIndex + 1, history.length - 1);
				return;
			}
			if (e.key === 'ArrowUp') {
				e.preventDefault();
				historyIndex = Math.max(historyIndex - 1, 0);
				return;
			}
			if (e.key === 'Enter') {
				e.preventDefault();
				selectHistoryEntry(history[historyIndex]);
				return;
			}
			if (e.key === 'Delete') {
				e.preventDefault();
				removeHistoryAt(historyIndex);
				return;
			}
		}

		if (e.key === 'Escape') {
			e.preventDefault();
			api.hideWindow().catch((err) => console.error('hideWindow failed', err));
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

	/// 呼出延迟性能埋点：`dowse://shown` 事件到这里只说明 Rust 侧调用过
	/// `window.show()`，不代表前端这一帧已经真正绘制上屏——用双重
	/// requestAnimationFrame 确认至少完成一次绘制之后再回报给 Rust 侧，
	/// Rust 那边拿热键回调进入的单调时钟算差值打日志（见 perf.rs）。非
	/// 热键触发的显示（托盘点击等）Rust 侧没有起始时刻可算，命令内部会
	/// 静默跳过，这里不需要关心那个区分。
	function reportShownPerf() {
		requestAnimationFrame(() => {
			requestAnimationFrame(() => {
				api.reportShownPerf().catch(() => {
					// 失败安全：埋点不能影响呼出主流程。
				});
			});
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
		history = loadHistory();
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
			reportShownPerf();
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
			showToast(t.toastRebuildDone(evt.payload));
		});
		const unlistenRebuildError = listen<string>('dowse://rebuild-error', (evt) => {
			refreshIndexingStatus();
			showToast(t.toastRebuildFailed(evt.payload));
		});
		// 托盘"移除"文件夹的成功回执——移除没有"收录数"可言，走独立事件而不是
		// 复用 dowse://rebuild-done（那个的 toast 文案是"收录 N 个文件"，套用
		// 到移除操作上语义是反的）。
		const unlistenRootRemoved = listen<number>('dowse://root-removed', (evt) => {
			refreshIndexStatus();
			showToast(t.toastFolderRemoved(evt.payload));
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
				placeholder={t.searchPlaceholder}
				bind:value={query}
				onkeydown={handleKeydown}
				autocomplete="off"
				spellcheck="false"
			/>
			<span class="caret-flourish" bind:this={caretFlourishEl} aria-hidden="true"></span>
		</div>
		<div class="controls" bind:this={controlsEl}>
			<GhostDropdown
				idleLabel={t.filterIdleLabel}
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
				idleLabel={t.sortIdleLabel}
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
				{history}
				{historyIndex}
				onpick={pickDirectoryAndRebuild}
				onaddfolder={pickDirectoryAndAddRoot}
				onselecthistory={selectHistoryEntry}
				onhoverhistory={(i) => (historyIndex = i)}
				onclearhistory={clearAllHistory}
			/>
		{:else}
			<div class="results">
				<div class="results-heading">
					<span>{t.resultsPrefix}<AnimatedNumber value={hits.length} />{t.resultsSuffix}</span>
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

	{#if rulesPanelOpen}
		<IndexRulesPanel
			{roots}
			onclose={closeRulesPanel}
			onrebuild={() => {
				closeRulesPanel();
				rebuildCurrentIndex();
			}}
		/>
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
