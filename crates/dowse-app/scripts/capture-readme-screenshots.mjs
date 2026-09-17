import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { mkdtemp, readFile, rm, stat, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { extname, join, resolve } from 'node:path';

const APP_WIDTH = 860;
const APP_HEIGHT = 560;
const SCALE = 2;
const appDir = resolve(import.meta.dirname, '..');
const buildDir = join(appDir, 'build');
const outputDir = resolve(appDir, '..', '..', 'docs', 'screenshots');

const chromeCandidates = [
	'C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe',
	'C:\\Program Files (x86)\\Microsoft\\Edge\\Application\\msedge.exe',
	'C:\\Program Files\\Microsoft\\Edge\\Application\\msedge.exe'
];

const contentTypes = new Map([
	['.css', 'text/css; charset=utf-8'],
	['.html', 'text/html; charset=utf-8'],
	['.js', 'text/javascript; charset=utf-8'],
	['.json', 'application/json; charset=utf-8'],
	['.png', 'image/png'],
	['.svg', 'image/svg+xml; charset=utf-8'],
	['.woff2', 'font/woff2']
]);

async function firstExisting(paths) {
	for (const path of paths) {
		try {
			await stat(path);
			return path;
		} catch {
			// Try the next installed browser.
		}
	}
	throw new Error('Chrome or Microsoft Edge is required to capture README screenshots.');
}

function serveBuild() {
	const server = createServer(async (request, response) => {
		try {
			const url = new URL(request.url ?? '/', 'http://127.0.0.1');
			const relative = url.pathname === '/' ? 'index.html' : url.pathname.slice(1);
			const candidate = resolve(buildDir, relative);
			const root = `${resolve(buildDir)}\\`;
			const file = candidate.startsWith(root) ? candidate : join(buildDir, 'index.html');
			let body;
			try {
				body = await readFile(file);
			} catch {
				body = await readFile(join(buildDir, 'index.html'));
			}
			response.writeHead(200, {
				'Content-Type': contentTypes.get(extname(file)) ?? 'application/octet-stream',
				'Cache-Control': 'no-store'
			});
			response.end(body);
		} catch (error) {
			response.writeHead(500, { 'Content-Type': 'text/plain; charset=utf-8' });
			response.end(String(error));
		}
	});
	return new Promise((resolvePromise) => {
		server.listen(0, '127.0.0.1', () => {
			const address = server.address();
			if (!address || typeof address === 'string') throw new Error('Could not bind screenshot server.');
			resolvePromise({ server, port: address.port });
		});
	});
}

class CdpClient {
	constructor(url) {
		this.nextId = 1;
		this.pending = new Map();
		this.socket = new WebSocket(url);
		this.ready = new Promise((resolvePromise, reject) => {
			this.socket.addEventListener('open', resolvePromise, { once: true });
			this.socket.addEventListener('error', reject, { once: true });
		});
		this.socket.addEventListener('message', (event) => {
			const message = JSON.parse(String(event.data));
			if (!message.id) return;
			const handler = this.pending.get(message.id);
			if (!handler) return;
			this.pending.delete(message.id);
			if (message.error) handler.reject(new Error(message.error.message));
			else handler.resolve(message.result);
		});
	}

	async call(method, params = {}) {
		await this.ready;
		const id = this.nextId++;
		return new Promise((resolvePromise, reject) => {
			this.pending.set(id, { resolve: resolvePromise, reject });
			this.socket.send(JSON.stringify({ id, method, params }));
		});
	}

	close() {
		this.socket.close();
	}
}

async function waitForDebugger(port) {
	for (let attempt = 0; attempt < 80; attempt += 1) {
		try {
			const response = await fetch(`http://127.0.0.1:${port}/json/list`);
			const pages = await response.json();
			const page = pages.find((entry) => entry.type === 'page');
			if (page?.webSocketDebuggerUrl) return page.webSocketDebuggerUrl;
		} catch {
			// Browser startup is still in progress.
		}
		await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
	}
	throw new Error('Timed out waiting for the browser debugger.');
}

function installMockRuntime(lang, mode) {
	localStorage.clear();
	localStorage.setItem('dowse.lang-override', lang);
	window.__screenshotErrors = [];
	window.__screenshotCommands = [];
	const originalConsoleError = console.error;
	console.error = (...args) => {
		window.__screenshotErrors.push(args.map(String).join(' '));
		originalConsoleError(...args);
	};
	const callbacks = new Map();
	let nextCallbackId = 1;
	let nextListenerId = 1;
	const zh = lang === 'zh';
	const highlight = (text, term) => {
		const index = text.toLowerCase().indexOf(term.toLowerCase());
		if (index < 0) return [{ text, highlighted: false }];
		return [
			{ text: text.slice(0, index), highlighted: false },
			{ text: text.slice(index, index + term.length), highlighted: true },
			{ text: text.slice(index + term.length), highlighted: false }
		].filter((part) => part.text.length > 0);
	};
	const names = zh
		? ['北极星计划_一页简报.md', '北极星体验看板.png', '搜索质量.csv', '00_演示资料说明.md', '发布检查清单.md', '周会纪要_2026-09-15.md']
		: ['Northstar-plan-brief.md', 'Northstar-experience-dashboard.png', 'search-quality.csv', '00-demo-notes.md', 'release-checklist.md', 'weekly-notes-2026-09-15.md'];
	const term = zh ? '北极星' : 'Northstar';
	const imageSvg = `<svg xmlns="http://www.w3.org/2000/svg" width="960" height="460" viewBox="0 0 960 460"><rect width="960" height="460" rx="24" fill="#eef7fb"/><rect x="34" y="34" width="892" height="74" rx="16" fill="#d5edf6"/><text x="66" y="81" font-family="Segoe UI, sans-serif" font-size="30" font-weight="600" fill="#176f93">${zh ? '北极星体验看板' : 'Northstar Experience Dashboard'}</text><g fill="#fff" stroke="#c6e1ec"><rect x="34" y="136" width="274" height="136" rx="16"/><rect x="343" y="136" width="274" height="136" rx="16"/><rect x="652" y="136" width="274" height="136" rx="16"/></g><g font-family="Segoe UI, sans-serif" fill="#60717a"><text x="58" y="174" font-size="18">${zh ? '召回率' : 'Recall'}</text><text x="367" y="174" font-size="18">${zh ? '响应时间' : 'Latency'}</text><text x="676" y="174" font-size="18">${zh ? '已索引文件' : 'Indexed files'}</text></g><g font-family="Segoe UI, sans-serif" font-size="45" font-weight="600" fill="#172c35"><text x="58" y="235">96.8%</text><text x="367" y="235">42 ms</text><text x="676" y="235">15,100</text></g><rect x="34" y="302" width="892" height="124" rx="16" fill="#fff" stroke="#c6e1ec"/><polyline points="68,388 168,365 268,372 368,330 468,348 568,316 668,326 768,287 892,304" fill="none" stroke="#39a3ce" stroke-width="6" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
	const imageData = `data:image/svg+xml;charset=utf-8,${encodeURIComponent(imageSvg)}`;

	function makeHit(index, forceImage = false) {
		const rawName = forceImage
			? zh
				? '北极星体验总览.png'
				: 'Northstar-experience-overview.png'
			: names[index % names.length];
		const extension = rawName.split('.').pop();
		const name = index < names.length ? rawName : rawName.replace(`.${extension}`, `-${String(index + 1).padStart(3, '0')}.${extension}`);
		const path = `C:\\Users\\demo\\Documents\\Northstar\\${name}`;
		const snippet = zh
			? `# ${term}计划 · 本地搜索、预览与发布质量记录。`
			: `# ${term} plan · local search, preview, and release-quality notes.`;
		return {
			path,
			display_path: path,
			name,
			name_segments: highlight(name, term),
			snippet_segments: highlight(snippet, term),
			score: 12 - index * 0.01
		};
	}

	window.__TAURI_EVENT_PLUGIN_INTERNALS__ = {
		unregisterListener(_event, id) {
			callbacks.delete(id);
		}
	};
	window.__TAURI_INTERNALS__ = {
		metadata: {
			currentWindow: { label: 'main' },
			currentWebview: { windowLabel: 'main', label: 'main' }
		},
		transformCallback(callback, once = false) {
			const id = nextCallbackId++;
			callbacks.set(id, (data) => {
				if (once) callbacks.delete(id);
				return callback?.(data);
			});
			return id;
		},
		unregisterCallback(id) {
			callbacks.delete(id);
		},
		runCallback(id, data) {
			callbacks.get(id)?.(data);
		},
		convertFileSrc() {
			return imageData;
		},
		async invoke(command, args = {}) {
			window.__screenshotCommands.push({ command, args });
			switch (command) {
				case 'plugin:event|listen':
					return nextListenerId++;
				case 'plugin:event|unlisten':
				case 'report_shown_perf':
				case 'report_search_perf':
				case 'set_pinned':
					return null;
				case 'index_status':
					return { has_index: true, num_docs: 15100, roots: ['C:\\Users\\demo\\Documents\\Northstar'] };
				case 'indexing_status':
					return { phase: 'idle', text_processed: 0, text_current_file: '', ocr_processed: 0, ocr_total: 0 };
				case 'get_effect_level':
					return 'solid';
				case 'get_glass_alpha':
					return { light: 0.4, dark: 0.28 };
				case 'get_hotkey':
					return 'Alt+Backquote';
				case 'get_config':
					return { hotkey: 'Alt+Backquote', transparency_enabled: false, transparency_tier: 'mid', autostart_enabled: true, lang };
				case 'get_rules':
					return { exclude_dirs: ['node_modules', 'target', '.git'], extra_text_exts: ['rst', 'adoc'], max_file_mb: 20 };
				case 'file_icon':
					return null;
				case 'search': {
					await new Promise((resolvePromise) => setTimeout(resolvePromise, 28));
					const offset = Number(args.offset ?? 0);
					const limit = Number(args.limit ?? 50);
					const total = 327;
					const hits = Array.from({ length: Math.min(limit, total - offset) }, (_, item) =>
						makeHit(offset + item, mode === 'ocr' && offset + item === 0)
					);
					return { hits, total };
				}
				case 'preview':
					return {
						segments: highlight(
							mode === 'ocr'
								? zh
									? '北极星体验看板：召回率 96.8%，响应时间 42ms，已索引文件 15,100。'
									: 'Northstar Experience Dashboard: 96.8% recall, 42ms latency, 15,100 indexed files.'
								: zh
									? '北极星计划要解决的不是再做一个搜索框，而是让散落在文档、代码、会议纪要与截图里的知识重新变得可抵达。所有内容只在本机建立索引，输入即搜，并在右侧直接展示更长的上下文。'
									: 'The Northstar plan is not another search box. It makes knowledge scattered across documents, code, meeting notes, and screenshots reachable again. Everything is indexed locally, searched as you type, and previewed in context.',
							term
						)
					};
				default:
					return null;
			}
		}
	};
}

async function evaluate(client, expression) {
	const result = await client.call('Runtime.evaluate', {
		expression,
		awaitPromise: true,
		returnByValue: true
	});
	if (result.exceptionDetails) throw new Error(result.exceptionDetails.text);
	return result.result?.value;
}

async function capture(client, baseUrl, { file, lang, mode }) {
	await client.call('Page.navigate', { url: 'about:blank' });
	const preload = await client.call('Page.addScriptToEvaluateOnNewDocument', {
		source: `(${installMockRuntime.toString()})(${JSON.stringify(lang)}, ${JSON.stringify(mode)});`
	});
	await client.call('Page.navigate', { url: baseUrl });
	const rendered = await evaluate(
		client,
		`new Promise(async (resolve) => {
			while (!document.querySelector('.search-input')) await new Promise((r) => setTimeout(r, 25));
			await document.fonts.ready;
			const input = document.querySelector('.search-input');
			if (${JSON.stringify(mode)} === 'settings') {
				input.dispatchEvent(new KeyboardEvent('keydown', { key: ',', code: 'Comma', ctrlKey: true, bubbles: true }));
			} else {
				input.value = ${JSON.stringify(lang === 'zh' ? '北极星' : 'Northstar')};
				input.dispatchEvent(new InputEvent('input', { bubbles: true, inputType: 'insertText' }));
			}
			await new Promise((r) => setTimeout(r, 700));
			window.scrollTo(0, 0);
			resolve({ hasResult: Boolean(document.querySelector('.row')), errors: window.__screenshotErrors ?? [], commands: window.__screenshotCommands ?? [] });
		})`
	);
	if (mode !== 'settings' && !rendered.hasResult) {
		throw new Error(`Screenshot ${file} rendered no search results: ${rendered.errors.join(' | ')}; commands=${JSON.stringify(rendered.commands)}`);
	}
	const screenshot = await client.call('Page.captureScreenshot', {
		format: 'png',
		fromSurface: true,
		captureBeyondViewport: false
	});
	await writeFile(join(outputDir, file), Buffer.from(screenshot.data, 'base64'));
	await client.call('Page.removeScriptToEvaluateOnNewDocument', { identifier: preload.identifier });
}

async function removeTemporaryProfile(profileDir) {
	for (let attempt = 0; attempt < 30; attempt += 1) {
		try {
			await rm(profileDir, { recursive: true, force: true });
			return;
		} catch (error) {
			if (!['EBUSY', 'EPERM', 'ENOTEMPTY'].includes(error?.code)) throw error;
			await new Promise((resolvePromise) => setTimeout(resolvePromise, 100));
		}
	}
	throw new Error(`Could not remove temporary browser profile: ${profileDir}`);
}

async function main() {
	await stat(join(buildDir, 'index.html')).catch(() => {
		throw new Error('Run `npm run build` before capturing README screenshots.');
	});
	const browser = await firstExisting(chromeCandidates);
	const profileDir = await mkdtemp(join(tmpdir(), 'dowse-screenshots-'));
	const { server, port: appPort } = await serveBuild();
	const debugServer = createServer();
	await new Promise((resolvePromise) => debugServer.listen(0, '127.0.0.1', resolvePromise));
	const debugAddress = debugServer.address();
	if (!debugAddress || typeof debugAddress === 'string') throw new Error('Could not reserve debugger port.');
	const debugPort = debugAddress.port;
	await new Promise((resolvePromise) => debugServer.close(resolvePromise));
	const child = spawn(
		browser,
		[
			'--headless=new',
			`--remote-debugging-port=${debugPort}`,
			`--user-data-dir=${profileDir}`,
			`--window-size=${APP_WIDTH},${APP_HEIGHT}`,
			'--hide-scrollbars',
			'--disable-extensions',
			'--disable-sync',
			'--no-first-run',
			'about:blank'
		],
		{ stdio: 'ignore', windowsHide: true }
	);

	let client;
	try {
		const debuggerUrl = await waitForDebugger(debugPort);
		client = new CdpClient(debuggerUrl);
		await client.call('Page.enable');
		await client.call('Runtime.enable');
		await client.call('Emulation.setDeviceMetricsOverride', {
			width: APP_WIDTH,
			height: APP_HEIGHT,
			deviceScaleFactor: SCALE,
			mobile: false,
			screenWidth: APP_WIDTH,
			screenHeight: APP_HEIGHT
		});
		await client.call('Emulation.setDefaultBackgroundColorOverride', {
			color: { r: 0, g: 0, b: 0, a: 0 }
		});
		const baseUrl = `http://127.0.0.1:${appPort}/`;
		for (const target of [
			{ file: 'hero.png', lang: 'en', mode: 'hero' },
			{ file: 'hero.zh-CN.png', lang: 'zh', mode: 'hero' },
			{ file: 'settings.png', lang: 'en', mode: 'settings' },
			{ file: 'settings.zh-CN.png', lang: 'zh', mode: 'settings' },
			{ file: 'ocr-preview.png', lang: 'en', mode: 'ocr' },
			{ file: 'ocr-preview.zh-CN.png', lang: 'zh', mode: 'ocr' }
		]) {
			await capture(client, baseUrl, target);
		}
	} finally {
		client?.close();
		child.kill();
		await Promise.race([once(child, 'exit'), new Promise((resolvePromise) => setTimeout(resolvePromise, 3000))]);
		await new Promise((resolvePromise) => server.close(resolvePromise));
		if (profileDir.startsWith(join(tmpdir(), 'dowse-screenshots-'))) {
			await removeTemporaryProfile(profileDir);
		}
	}
}

await main();
