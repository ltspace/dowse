import test from 'node:test';
import assert from 'node:assert/strict';

import {
	SEARCH_PAGE_SIZE,
	clampPageIndex,
	pageCount,
	pageOffset
} from '../src/lib/pagination.js';

test('pagination stays hidden for a single result page', () => {
	assert.equal(pageCount(0), 0);
	assert.equal(pageCount(SEARCH_PAGE_SIZE), 1);
});

test('pagination includes a partial final page', () => {
	assert.equal(pageCount(SEARCH_PAGE_SIZE + 1), 2);
	assert.equal(pageCount(327), 7);
});

test('page offsets advance by the fixed search page size', () => {
	assert.equal(pageOffset(0), 0);
	assert.equal(pageOffset(3), 150);
});

test('page indices are clamped after the result set shrinks', () => {
	assert.equal(clampPageIndex(-1, 7), 0);
	assert.equal(clampPageIndex(9, 7), 6);
	assert.equal(clampPageIndex(4, 0), 0);
});
