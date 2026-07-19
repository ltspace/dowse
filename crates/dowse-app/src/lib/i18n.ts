// 界面文案的中英双语字典，跟随系统语言：navigator.language 以 "zh" 开头走
// 中文，其余一律英文。判定只做一次（模块首次求值时），没有运行时切换、没有
// 语言开关、没有设置项——纯跟随系统区域。只收界面可见文案（按钮、占位符、
// 下拉项、toast、空态、快捷键提示、tooltip、aria-label 等）；注释和开发者
// 面向的日志不在此列，仍按仓库惯例保留中文。

export const isZh = navigator.language.toLowerCase().startsWith('zh');

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
	soCheatSheet: '速查'
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
	soCheatSheet: 'Cheat sheet'
};

export const t: Strings = isZh ? zh : en;
