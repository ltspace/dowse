// 搜索历史的读写层：只在用户"打开了某条结果"的那一刻记录当前查询词（见
// +page.svelte 的 recordSearchHistory），不在击键/出结果时记，避免历史被
// 中间态刷满。持久化用 localStorage，键名带 dowse 前缀，上限 10 条，
// 去重（同词提升到最前），最新在前。这里只负责存储读写，不碰任何 UI 状态。

const STORAGE_KEY = 'dowse:search-history';
const MAX_ENTRIES = 10;

function readRaw(): string[] {
	try {
		const raw = localStorage.getItem(STORAGE_KEY);
		if (!raw) return [];
		const parsed = JSON.parse(raw);
		if (!Array.isArray(parsed)) return [];
		return parsed.filter((x): x is string => typeof x === 'string' && x.length > 0);
	} catch {
		// localStorage 不可用（隐私/受限环境）或存的 JSON 损坏时退化成空历史，
		// 不抛错影响搜索主流程——历史只是锦上添花。
		return [];
	}
}

function writeRaw(entries: string[]) {
	try {
		localStorage.setItem(STORAGE_KEY, JSON.stringify(entries));
	} catch {
		// 同上，写入失败静默忽略。
	}
}

export function loadHistory(): string[] {
	return readRaw();
}

/// 记录一次"打开"：同词去重并提到最前，超出上限（10）的旧条目直接截掉。
/// 返回更新后的完整列表，调用方（+page.svelte）拿它直接赋给本地 state，
/// 不需要再单独读一次。
export function recordHistory(query: string): string[] {
	const trimmed = query.trim();
	if (!trimmed) return readRaw();
	const existing = readRaw().filter((q) => q !== trimmed);
	const next = [trimmed, ...existing].slice(0, MAX_ENTRIES);
	writeRaw(next);
	return next;
}

export function removeHistoryEntry(query: string): string[] {
	const next = readRaw().filter((q) => q !== query);
	writeRaw(next);
	return next;
}

export function clearHistory(): string[] {
	writeRaw([]);
	return [];
}
