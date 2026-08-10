export const DEFAULT_SPLIT_RATIO = 0.58;
export const SPLIT_DIVIDER_WIDTH = 9;
export const MIN_RESULTS_WIDTH = 280;
export const MIN_PREVIEW_WIDTH = 240;

/** @param {unknown} value */
export function normalizeSplitRatio(value) {
	const ratio = Number(value);
	return Number.isFinite(ratio) && ratio > 0 && ratio < 1 ? ratio : DEFAULT_SPLIT_RATIO;
}

/**
 * @param {number} containerWidth
 * @param {number} desiredWidth
 */
export function clampSplitWidth(containerWidth, desiredWidth) {
	const available = Math.max(0, containerWidth - SPLIT_DIVIDER_WIDTH);
	if (available === 0) return 0;

	// Tiny windows cannot satisfy both preferred minima. Keep both columns present
	// instead of letting one consume the entire grid.
	const minLeft = Math.min(MIN_RESULTS_WIDTH, available / 2);
	const maxLeft = Math.max(minLeft, available - Math.min(MIN_PREVIEW_WIDTH, available / 2));
	return Math.min(maxLeft, Math.max(minLeft, desiredWidth));
}

/**
 * @param {number} containerWidth
 * @param {unknown} ratio
 */
export function splitWidthFromRatio(containerWidth, ratio) {
	const available = Math.max(0, containerWidth - SPLIT_DIVIDER_WIDTH);
	return clampSplitWidth(containerWidth, available * normalizeSplitRatio(ratio));
}

/**
 * @param {number} containerWidth
 * @param {number} width
 */
export function splitRatioFromWidth(containerWidth, width) {
	const available = Math.max(0, containerWidth - SPLIT_DIVIDER_WIDTH);
	if (available === 0) return DEFAULT_SPLIT_RATIO;
	return clampSplitWidth(containerWidth, width) / available;
}
