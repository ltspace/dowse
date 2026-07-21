// 界面文案的中英双语字典。判定只做一次（模块首次求值时），没有运行时切换——
// t 是一个"启动时定死"的同步 const。只收界面可见文案（按钮、占位符、下拉项、
// toast、空态、快捷键提示、tooltip、aria-label 等）；注释和开发者面向的日志
// 不在此列，仍按仓库惯例保留中文。
//
// 语言来源（0.9.0 起）：设置面板可以把界面语言钉死为中/英，或 "auto" 跟随
// 系统。权威存储在 Rust 侧 config.lang（托盘 i18n 也读它）。但这里的 t 是同步
// const、等不了异步 IPC，所以用 localStorage 存一份"上次已知的语言选择"作
// **同步启动镜像**：模块首次求值时同步读它决定语言；app 挂载后再异步从 config
// 拉一次写回镜像（见 +page.svelte 的启动同步、以及设置面板改语言时的即时写入）。
// 配合"改语言重启后生效"的交互，语言本来就只在下次启动生效，这份一拍延迟正好
// 落在可接受范围内。镜像缺失/为 "auto" 时回落到 navigator.language，跟 0.7.0
// 起的纯跟随系统行为完全一致。
export const LANG_OVERRIDE_KEY = 'dowse.lang-override';

function resolveIsZh(): boolean {
	let override: string | null = null;
	try {
		override = localStorage.getItem(LANG_OVERRIDE_KEY);
	} catch {
		// localStorage 不可用（隐私模式等）就当没有覆盖，回落系统语言。
	}
	if (override === 'zh') return true;
	if (override === 'en') return false;
	return navigator.language.toLowerCase().startsWith('zh');
}

export const isZh = resolveIsZh();

interface Strings {
	// 类型筛选下拉
	filterAll: string;
	filterDoc: string;
	filterCode: string;
	filterImage: string;
	filterIdleLabel: string;
	// 排序下拉
	sortRelevance: string;
	sortNewest: string;
	sortOldest: string;
	sortLargest: string;
	sortIdleLabel: string;
	// 搜索框
	searchPlaceholder: string;
	// 结果计数分成前后缀，中间夹一个带滚动动画的数字组件
	resultsPrefix: string;
	resultsSuffix: string;
	// 目录选择对话框标题
	dialogPickIndexFolder: string;
	dialogAddFolder: string;
	// toast
	toastOpenFailed: (err: string) => string;
	toastRevealFailed: (err: string) => string;
	toastPathCopied: string;
	toastCopyFailed: string;
	toastRebuildDone: (n: number) => string;
	toastRebuildFailed: (err: string) => string;
	toastFolderRemoved: (n: number) => string;
	// OCR 进度条
	ocrProgressLabel: string;
	// 预览区
	previewSelectHint: string;
	ocrLoading: string;
	ocrCaption: string;
	ocrEmpty: string;
	previewLoading: string;
	previewEmpty: string;
	// 结果列表 aria
	resultListLabel: string;
	// 图钉
	pinUnpin: string;
	pin: string;
	// 底部快捷键条
	scHide: string;
	scCopyPath: string;
	scReveal: string;
	scOpen: string;
	// 空态
	esTypeToSearch: string;
	esSearchHelp: string;
	esAddFolder: string;
	esNoIndexTitle: string;
	esNoIndexSub: string;
	esPickAndIndex: string;
	esCountUnit: string;
	esErrorTitle: string;
	esUnknownError: string;
	esRepick: string;
	esNoMatch: (numDocs: number) => string;
	esNoMatchSub: string;
	// 建索引报告里"因体积超限跳过"的补充说明（有才展示）
	esSkippedOversize: (n: number) => string;
	// 搜索历史（空态区域上方，输入框为空时展示）
	historyTitle: string;
	historyClear: string;
	historyLabel: string;
	// 建索引完成冷报告 + 秒数格式
	formatSeconds: (seconds: number) => string;
	indexReport: (indexed: string, seconds: string) => string;
	// 快捷键速查浮层
	soCardTitle: string;
	soScrimLabel: string;
	soDialogLabel: string;
	soShow: string;
	soHide: string;
	soNavigate: string;
	soOpen: string;
	soRevealFolder: string;
	soCopyPath: string;
	soFilterType: string;
	soSort: string;
	soPin: string;
	soCheatSheet: string;
	soSettings: string;
	// 设置面板（Ctrl+, 打开）：分区标题 + 通用区各项
	setTitle: string;
	setCloseLabel: string;
	setTabGeneral: string;
	setTabRules: string;
	// 通用区 - 呼出快捷键改键
	setHotkeyLabel: string;
	setHotkeyHint: string;
	setHotkeyChange: string;
	setHotkeyCapturing: string;
	setHotkeyConfirm: string;
	setHotkeyCancel: string;
	setHotkeyNeedModifier: string;
	setHotkeySaved: string;
	setHotkeyFailed: (err: string) => string;
	// 通用区 - 透明效果 + 三档
	setTransparencyLabel: string;
	setTierLabel: string;
	setTierLow: string;
	setTierMid: string;
	setTierHigh: string;
	// 通用区 - 开机自启
	setAutostartLabel: string;
	// 通用区 - 界面语言
	setLangLabel: string;
	setLangAuto: string;
	setLangZh: string;
	setLangEn: string;
	setLangRestartHint: string;
	// 通用区 - 开/关两态（透明/自启共用）
	setOn: string;
	setOff: string;
	// 索引规则面板（Ctrl+, 打开）
	rpTitle: string;
	rpCloseLabel: string;
	rpExcludeDirsLabel: string;
	rpExcludeDirsHint: string;
	rpExtraExtsLabel: string;
	rpExtraExtsHint: string;
	rpMaxFileMbLabel: string;
	rpSave: string;
	rpSaving: string;
	rpSaved: string;
	rpSaveFailed: (err: string) => string;
	rpLoadFailed: string;
	rpRebuildNow: string;
	rpRebuildMultiRootHint: string;
	rpRebuildNoIndexHint: string;
}

const zh: Strings = {
	filterAll: '全部',
	filterDoc: '文档',
	filterCode: '代码',
	filterImage: '图片',
	filterIdleLabel: '全部类型',
	sortRelevance: '相关性',
	sortNewest: '最新优先',
	sortOldest: '最旧优先',
	sortLargest: '最大优先',
	sortIdleLabel: '相关性',
	searchPlaceholder: '搜文件名或内容…',
	resultsPrefix: '结果 · ',
	resultsSuffix: ' 条',
	dialogPickIndexFolder: '选择要索引的目录',
	dialogAddFolder: '选择要添加的文件夹',
	toastOpenFailed: (err) => `文件打开失败：${err}`,
	toastRevealFailed: (err) => `定位文件夹失败：${err}`,
	toastPathCopied: '路径已复制。',
	toastCopyFailed: '复制失败。',
	toastRebuildDone: (n) => `索引重建完成，收录 ${n} 个文件。`,
	toastRebuildFailed: (err) => `索引重建失败：${err}`,
	toastFolderRemoved: (n) => `已移除该文件夹，删除 ${n} 篇文档。`,
	ocrProgressLabel: '图片识别',
	previewSelectHint: '选中结果后在此查看预览。',
	ocrLoading: '识别文字加载中…',
	ocrCaption: '图中文字（OCR 识别）',
	ocrEmpty: '没有识别到文字，或者还在后台排队处理。',
	previewLoading: '加载中…',
	previewEmpty: '没有可预览的文本内容。',
	resultListLabel: '搜索结果',
	pinUnpin: '取消固定（恢复失焦自动隐藏）',
	pin: '固定（失焦不再自动隐藏）',
	scHide: '隐藏',
	scCopyPath: '复制路径',
	scReveal: '打开所在文件夹',
	scOpen: '打开',
	esTypeToSearch: '键入即搜。',
	esSearchHelp: '文件名、文档正文都能搜，多个词默认取交集，"引号内"作短语查询。',
	esAddFolder: '添加文件夹',
	esNoIndexTitle: '尚未建立索引。',
	esNoIndexSub: '选择一个目录开始建索引，之后可在托盘菜单重建。',
	esPickAndIndex: '选择目录并建索引',
	esCountUnit: '篇',
	esErrorTitle: '索引操作失败。',
	esUnknownError: '未知错误。',
	esRepick: '重新选择目录',
	esNoMatch: (numDocs) => `没有匹配的结果。索引包含 ${numDocs} 篇文档。`,
	esNoMatchSub: '换一个查询词，或确认文件在已建索引的目录中。',
	esSkippedOversize: (n) => `其中 ${n} 个因体积超限跳过。`,
	historyTitle: '最近搜索',
	historyClear: '清空',
	historyLabel: '最近搜索列表',
	formatSeconds: (seconds) => (seconds < 10 ? `${seconds.toFixed(1)} 秒` : `${Math.round(seconds)} 秒`),
	indexReport: (indexed, seconds) => `${indexed} 篇，${seconds}。`,
	soCardTitle: '快捷键',
	soScrimLabel: '关闭快捷键速查（按任意键或点击）',
	soDialogLabel: '快捷键速查',
	soShow: '呼出',
	soHide: '隐藏',
	soNavigate: '导航',
	soOpen: '打开',
	soRevealFolder: '跳转文件夹',
	soCopyPath: '复制路径',
	soFilterType: '筛选类型',
	soSort: '排序',
	soPin: '固定',
	soCheatSheet: '速查',
	soSettings: '设置',
	setTitle: '设置',
	setCloseLabel: '关闭设置面板',
	setTabGeneral: '通用',
	setTabRules: '索引规则',
	setHotkeyLabel: '呼出快捷键',
	setHotkeyHint: '需包含至少一个修饰键（Ctrl / Alt / Shift / Win）加一个主键。',
	setHotkeyChange: '改键',
	setHotkeyCapturing: '按下新的组合键…（Esc 取消）',
	setHotkeyConfirm: '确认',
	setHotkeyCancel: '取消',
	setHotkeyNeedModifier: '至少需要一个修饰键（Ctrl / Alt / Shift / Win）。',
	setHotkeySaved: '快捷键已更新。',
	setHotkeyFailed: (err) => `改键失败：${err}`,
	setTransparencyLabel: '透明效果',
	setTierLabel: '透明度',
	setTierLow: '低',
	setTierMid: '中',
	setTierHigh: '高',
	setAutostartLabel: '开机自启',
	setLangLabel: '界面语言',
	setLangAuto: '跟随系统',
	setLangZh: '中文',
	setLangEn: 'English',
	setLangRestartHint: '重启后生效。',
	setOn: '开',
	setOff: '关',
	rpTitle: '索引规则',
	rpCloseLabel: '关闭索引规则面板',
	rpExcludeDirsLabel: '排除目录',
	rpExcludeDirsHint: '目录名，逗号或换行分隔（如 node_modules, dist）。以 . 开头的目录始终排除，不用列出来。',
	rpExtraExtsLabel: '追加文本扩展名',
	rpExtraExtsHint: '不含点，逗号分隔（如 rst, adoc）。在内建白名单之外追加，不是覆盖。',
	rpMaxFileMbLabel: '单文件体积上限（MB）',
	rpSave: '保存',
	rpSaving: '保存中…',
	rpSaved: '已保存，重建索引后完全生效。',
	rpSaveFailed: (err) => `保存失败：${err}`,
	rpLoadFailed: '规则加载失败，已显示默认值。',
	rpRebuildNow: '立即重建',
	rpRebuildMultiRootHint: '已注册多个索引目录，请到托盘菜单里逐个重建。',
	rpRebuildNoIndexHint: '还没有索引目录，先在空态或托盘菜单里添加一个文件夹。'
};

const en: Strings = {
	filterAll: 'All',
	filterDoc: 'Docs',
	filterCode: 'Code',
	filterImage: 'Images',
	filterIdleLabel: 'All types',
	sortRelevance: 'Relevance',
	sortNewest: 'Newest',
	sortOldest: 'Oldest',
	sortLargest: 'Largest',
	sortIdleLabel: 'Relevance',
	searchPlaceholder: 'Search names or contents…',
	resultsPrefix: 'Results · ',
	resultsSuffix: '',
	dialogPickIndexFolder: 'Choose a folder to index',
	dialogAddFolder: 'Choose a folder to add',
	toastOpenFailed: (err) => `Could not open file: ${err}`,
	toastRevealFailed: (err) => `Could not reveal folder: ${err}`,
	toastPathCopied: 'Path copied.',
	toastCopyFailed: 'Copy failed.',
	toastRebuildDone: (n) => `Index rebuilt. ${n} files indexed.`,
	toastRebuildFailed: (err) => `Index rebuild failed: ${err}`,
	toastFolderRemoved: (n) => `Folder removed. ${n} documents dropped.`,
	ocrProgressLabel: 'Image OCR',
	previewSelectHint: 'Select a result to preview it here.',
	ocrLoading: 'Loading recognized text…',
	ocrCaption: 'Text in image (OCR)',
	ocrEmpty: 'No text recognized, or still queued in the background.',
	previewLoading: 'Loading…',
	previewEmpty: 'No previewable text content.',
	resultListLabel: 'Search results',
	pinUnpin: 'Unpin (resume auto-hide on blur)',
	pin: 'Pin (stop auto-hide on blur)',
	scHide: 'Hide',
	scCopyPath: 'Copy path',
	scReveal: 'Reveal in File Explorer',
	scOpen: 'Open',
	esTypeToSearch: 'Type to search.',
	esSearchHelp: 'Search file names and document contents. Multiple words are ANDed; "quoted" runs a phrase query.',
	esAddFolder: 'Add folder',
	esNoIndexTitle: 'No index yet.',
	esNoIndexSub: 'Pick a folder to start indexing. You can rebuild later from the tray menu.',
	esPickAndIndex: 'Pick a folder and index',
	esCountUnit: 'docs',
	esErrorTitle: 'Indexing failed.',
	esUnknownError: 'Unknown error.',
	esRepick: 'Pick another folder',
	esNoMatch: (numDocs) => `No matches. The index holds ${numDocs} documents.`,
	esNoMatchSub: 'Try another query, or check the file is in an indexed folder.',
	esSkippedOversize: (n) => `${n} skipped for exceeding the size limit.`,
	historyTitle: 'Recent searches',
	historyClear: 'Clear',
	historyLabel: 'Recent searches list',
	formatSeconds: (seconds) => (seconds < 10 ? `${seconds.toFixed(1)} seconds` : `${Math.round(seconds)} seconds`),
	indexReport: (indexed, seconds) => `${indexed} documents, ${seconds}.`,
	soCardTitle: 'Shortcuts',
	soScrimLabel: 'Close shortcut cheat sheet (press any key or click)',
	soDialogLabel: 'Shortcut cheat sheet',
	soShow: 'Show',
	soHide: 'Hide',
	soNavigate: 'Navigate',
	soOpen: 'Open',
	soRevealFolder: 'Reveal folder',
	soCopyPath: 'Copy path',
	soFilterType: 'Filter type',
	soSort: 'Sort',
	soPin: 'Pin',
	soCheatSheet: 'Cheat sheet',
	soSettings: 'Settings',
	setTitle: 'Settings',
	setCloseLabel: 'Close settings',
	setTabGeneral: 'General',
	setTabRules: 'Index rules',
	setHotkeyLabel: 'Toggle shortcut',
	setHotkeyHint: 'Needs at least one modifier (Ctrl / Alt / Shift / Win) plus a main key.',
	setHotkeyChange: 'Change',
	setHotkeyCapturing: 'Press the new combo… (Esc to cancel)',
	setHotkeyConfirm: 'Confirm',
	setHotkeyCancel: 'Cancel',
	setHotkeyNeedModifier: 'Needs at least one modifier (Ctrl / Alt / Shift / Win).',
	setHotkeySaved: 'Shortcut updated.',
	setHotkeyFailed: (err) => `Rebind failed: ${err}`,
	setTransparencyLabel: 'Transparency',
	setTierLabel: 'Opacity',
	setTierLow: 'Low',
	setTierMid: 'Medium',
	setTierHigh: 'High',
	setAutostartLabel: 'Launch at startup',
	setLangLabel: 'Language',
	setLangAuto: 'System',
	setLangZh: '中文',
	setLangEn: 'English',
	setLangRestartHint: 'Takes effect after restart.',
	setOn: 'On',
	setOff: 'Off',
	rpTitle: 'Index rules',
	rpCloseLabel: 'Close index rules panel',
	rpExcludeDirsLabel: 'Excluded folders',
	rpExcludeDirsHint:
		'Folder names, comma- or newline-separated (e.g. node_modules, dist). Dot-folders are always excluded, no need to list them.',
	rpExtraExtsLabel: 'Extra text extensions',
	rpExtraExtsHint: 'No leading dot, comma-separated (e.g. rst, adoc). Adds to the built-in list, does not replace it.',
	rpMaxFileMbLabel: 'Max file size (MB)',
	rpSave: 'Save',
	rpSaving: 'Saving…',
	rpSaved: 'Saved. Rebuild the index for this to fully take effect.',
	rpSaveFailed: (err) => `Save failed: ${err}`,
	rpLoadFailed: 'Could not load rules; showing defaults.',
	rpRebuildNow: 'Rebuild now',
	rpRebuildMultiRootHint: 'Multiple index folders are registered; rebuild each one from the tray menu.',
	rpRebuildNoIndexHint: 'No index folder yet — add one from the empty state or the tray menu first.'
};

export const t: Strings = isZh ? zh : en;
