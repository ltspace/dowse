import test from 'node:test';
import assert from 'node:assert/strict';

import {
	DEFAULT_SPLIT_RATIO,
	MIN_PREVIEW_WIDTH,
	MIN_RESULTS_WIDTH,
	SPLIT_DIVIDER_WIDTH,
	clampSplitWidth,
	normalizeSplitRatio,
	splitRatioFromWidth,
	splitWidthFromRatio
} from '../src/lib/splitPane.js';

test('默认分栏比例在正常窗口中转换为稳定宽度', () => {
	const containerWidth = 860;
	const available = containerWidth - SPLIT_DIVIDER_WIDTH;
	assert.equal(splitWidthFromRatio(containerWidth, DEFAULT_SPLIT_RATIO), available * DEFAULT_SPLIT_RATIO);
});

test('拖动分隔线不会把任意一栏压过最小宽度', () => {
	const containerWidth = 860;
	assert.equal(clampSplitWidth(containerWidth, 0), MIN_RESULTS_WIDTH);
	assert.equal(
		clampSplitWidth(containerWidth, containerWidth),
		containerWidth - SPLIT_DIVIDER_WIDTH - MIN_PREVIEW_WIDTH
	);
});

test('保存的非法比例回落到默认值', () => {
	assert.equal(normalizeSplitRatio('not-a-number'), DEFAULT_SPLIT_RATIO);
	assert.equal(normalizeSplitRatio(0), DEFAULT_SPLIT_RATIO);
	assert.equal(normalizeSplitRatio(1), DEFAULT_SPLIT_RATIO);
});

test('宽度和比例可以往返，并在边界处保持夹紧', () => {
	const containerWidth = 860;
	const width = splitWidthFromRatio(containerWidth, 0.7);
	assert.equal(splitRatioFromWidth(containerWidth, width), width / (containerWidth - SPLIT_DIVIDER_WIDTH));
	assert.equal(splitRatioFromWidth(containerWidth, -100), MIN_RESULTS_WIDTH / (containerWidth - SPLIT_DIVIDER_WIDTH));
});
