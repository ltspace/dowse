import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import ts from 'typescript';

test('startup config overrides stale cache even when storage is unavailable', async () => {
	const source = readFileSync(new URL('../src/lib/i18n.ts', import.meta.url), 'utf8');
	const { outputText } = ts.transpileModule(source, {
		compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2022 }
	});
	const savedNavigator = Object.getOwnPropertyDescriptor(globalThis, 'navigator');
	const savedStorage = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
	let cached = 'zh';
	let blocked = false;
	Object.defineProperty(globalThis, 'navigator', { configurable: true, value: { language: 'zh-CN' } });
	Object.defineProperty(globalThis, 'localStorage', { configurable: true, value: {
		getItem() { if (blocked) throw new Error('blocked'); return cached; },
		setItem(_key, value) { if (blocked) throw new Error('blocked'); cached = value; }
	} });
	try {
		const dictionary = await import(`data:text/javascript;base64,${Buffer.from(outputText).toString('base64')}`);
		assert.equal(dictionary.t.setTabGeneral, '通用');
		dictionary.initializeLanguage('en');
		assert.equal(dictionary.t.setTabGeneral, 'General');
		assert.equal(dictionary.t.scHide, 'Hide');
		assert.equal(cached, 'en');
		dictionary.initializeLanguage('zh');
		assert.equal(dictionary.t.setTabGeneral, '通用');
		blocked = true;
		dictionary.initializeLanguage('en');
		assert.equal(dictionary.t.setTabGeneral, 'General');
		dictionary.initializeLanguage('auto');
		assert.equal(dictionary.t.setTabGeneral, '通用');
		globalThis.navigator.language = 'en-US';
		dictionary.initializeLanguage('auto');
		assert.equal(dictionary.t.setTabGeneral, 'General');
	} finally {
		if (savedNavigator) Object.defineProperty(globalThis, 'navigator', savedNavigator);
		else delete globalThis.navigator;
		if (savedStorage) Object.defineProperty(globalThis, 'localStorage', savedStorage);
		else delete globalThis.localStorage;
	}
});
