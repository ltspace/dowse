/** @typedef {import('./types.js').IndexingPhase} IndexingPhase */
/** @typedef {import('./types.js').IndexingSnapshot} IndexingSnapshot */
/** @typedef {import('./types.js').IndexProgress} IndexProgress */

/**
 * @typedef {object} IndexingViewState
 * @property {IndexingPhase} phase
 * @property {number} textProcessed
 * @property {string} textCurrentFile
 * @property {number} ocrProcessed
 * @property {number} ocrTotal
 */

/**
 * @typedef {{ type: 'text-progress', progress: IndexProgress }
 *   | { type: 'snapshot', snapshot: IndexingSnapshot }} IndexingViewEvent
 */

/** @type {IndexingViewState} */
export const idleIndexingView = {
	phase: 'idle',
	textProcessed: 0,
	textCurrentFile: '',
	ocrProcessed: 0,
	ocrTotal: 0
};

/**
 * 把实时进度和后端权威快照归并成同一份前端状态。
 *
 * 重建命令的返回值和 `dowse://rebuild-progress` 不走同一条 IPC 通道，最后一条
 * progress 可能晚于 invoke 返回抵达。后端会在所有 progress 之后通过事件通道
 * 再发一份终态 snapshot；这个 reducer 让它完整覆盖旧的文本阶段，避免完成报告
 * 一直挡住已经搜到的结果。
 *
 * @param {IndexingViewState} state
 * @param {IndexingViewEvent} event
 * @returns {IndexingViewState}
 */
export function reduceIndexingView(state, event) {
	if (event.type === 'text-progress') {
		return {
			...state,
			phase: 'text',
			textProcessed: event.progress.processed,
			textCurrentFile: event.progress.path
		};
	}

	const snapshot = event.snapshot;
	return {
		phase: snapshot.phase,
		textProcessed: snapshot.phase === 'text' ? snapshot.text_processed : 0,
		textCurrentFile: snapshot.phase === 'text' ? snapshot.text_current_file : '',
		ocrProcessed: snapshot.phase === 'ocr' ? snapshot.ocr_processed : 0,
		ocrTotal: snapshot.phase === 'ocr' ? snapshot.ocr_total : 0
	};
}
