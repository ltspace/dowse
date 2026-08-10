import assert from 'node:assert/strict';
import test from 'node:test';

import { idleIndexingView, reduceIndexingView } from '../src/lib/indexingState.js';

test('终态快照覆盖 invoke 返回后迟到的文本进度', () => {
	let state = idleIndexingView;

	state = reduceIndexingView(state, {
		type: 'text-progress',
		progress: { processed: 1137, path: 'C:\\Users\\jackyliu\\Documents\\note.txt' }
	});
	assert.equal(state.phase, 'text');

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
