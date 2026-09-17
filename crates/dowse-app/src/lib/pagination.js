export const SEARCH_PAGE_SIZE = 50;

/** @param {number} total @param {number} pageSize */
export function pageCount(total, pageSize = SEARCH_PAGE_SIZE) {
	if (!Number.isFinite(total) || total <= 0) return 0;
	if (!Number.isFinite(pageSize) || pageSize <= 0) return 0;
	return Math.ceil(total / pageSize);
}

/** @param {number} pageIndex @param {number} pageSize */
export function pageOffset(pageIndex, pageSize = SEARCH_PAGE_SIZE) {
	return Math.max(0, Math.trunc(pageIndex)) * pageSize;
}

/** @param {number} pageIndex @param {number} totalPages */
export function clampPageIndex(pageIndex, totalPages) {
	if (!Number.isFinite(totalPages) || totalPages <= 0) return 0;
	return Math.min(Math.max(0, Math.trunc(pageIndex)), Math.trunc(totalPages) - 1);
}
