import assert from 'node:assert/strict';
import test from 'node:test';

import { idleIndexingView, reduceIndexingView } from '../src/lib/indexingState.js';

test('终态快照覆盖 invoke 返回后迟到的文本进度', () => {
	// invoke 已经返回、前端本来准备展示搜索结果。
	let state = idleIndexingView;

	// 旧实现会停在这里：最后一条 progress 跨 IPC 通道迟到，把引导层重新打开。
	state = reduceIndexingView(state, {
		type: 'text-progress',
		progress: { processed: 1137, path: 'C:\\Users\\jackyliu\\Documents\\note.txt' }
	});
	assert.equal(state.phase, 'text');

	// 新增的同事件通道终态保证排在 progress 后面，让结果列表重新可见。
	state = reduceIndexingView(state, {
		type: 'snapshot',
		snapshot: {
			phase: 'idle',
			text_processed: 1137,
			text_current_file: 'C:\\Users\\jackyliu\\Documents\\note.txt',
			ocr_processed: 0,
			ocr_total: 0
		}
	});

	assert.deepEqual(state, idleIndexingView);
});

test('终态快照可以从文本阶段交接到 OCR 进度条', () => {
	const state = reduceIndexingView(
		{
			...idleIndexingView,
			phase: 'text',
			textProcessed: 42,
			textCurrentFile: 'last.png'
		},
		{
			type: 'snapshot',
			snapshot: {
				phase: 'ocr',
				text_processed: 42,
				text_current_file: 'last.png',
				ocr_processed: 3,
				ocr_total: 9
			}
		}
	);

	assert.deepEqual(state, {
		phase: 'ocr',
		textProcessed: 0,
		textCurrentFile: '',
		ocrProcessed: 3,
		ocrTotal: 9
	});
});
