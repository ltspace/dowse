<script lang="ts">
	import { onMount, tick } from 'svelte';
	import { t } from '../i18n';
	let { src, name, onclose }: { src: string; name: string; onclose: () => void } = $props();
	let dialog: HTMLDialogElement;
	let viewport: HTMLDivElement;
	let width = $state(0), height = $state(0);
	let naturalWidth = $state(0), naturalHeight = $state(0);
	let mode = $state<'fit' | 'width' | 'manual'>('fit');
	let manualScale = $state(1);
	let failed = $state(false);
	let dragging = $state(false);
	let lastX = 0, lastY = 0;
	let scale = $derived(naturalWidth && naturalHeight
		? mode === 'fit' ? Math.min(width / naturalWidth, height / naturalHeight)
			: mode === 'width' ? width / naturalWidth : manualScale
		: 1);

	onMount(() => {
		const previous = document.activeElement;
		dialog.showModal();
		return () => {
			dialog.close();
			if (previous instanceof HTMLElement && previous.isConnected) previous.focus({ preventScroll: true });
		};
	});

	async function setMode(next: 'fit' | 'width' | 'manual') {
		mode = next;
		manualScale = 1;
		await tick();
		viewport.scrollTo(0, 0);
	}

	async function zoom(factor: number) {
		if (!naturalWidth || failed) return;
		const old = scale;
		const x = (viewport.scrollLeft + width / 2 - Math.max(0, (width - naturalWidth * old) / 2)) / old;
		const y = (viewport.scrollTop + height / 2 - Math.max(0, (height - naturalHeight * old) / 2)) / old;
		manualScale = Math.max(0.01, Math.min(8, old * factor));
		mode = 'manual';
		await tick();
		viewport.scrollTo(x * scale - width / 2, y * scale - height / 2);
	}

	function startPan(e: PointerEvent) {
		if (e.button !== 0 || !(e.target instanceof HTMLImageElement)) return;
		e.preventDefault();
		viewport.focus({ preventScroll: true });
		viewport.setPointerCapture(e.pointerId);
		dragging = true;
		lastX = e.clientX; lastY = e.clientY;
	}
	function pan(e: PointerEvent) {
		if (!dragging) return;
		viewport.scrollLeft -= e.clientX - lastX;
		viewport.scrollTop -= e.clientY - lastY;
		lastX = e.clientX; lastY = e.clientY;
	}
	function endPan(e: PointerEvent) {
		dragging = false;
		if (viewport.hasPointerCapture(e.pointerId)) viewport.releasePointerCapture(e.pointerId);
	}
</script>

<!-- Native modal keeps background search inert and traps focus. -->
<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<dialog bind:this={dialog} aria-label={t.imageViewer} oncancel={(e) => { e.preventDefault(); onclose(); }}
	onkeydown={(e) => { e.stopPropagation(); }}>
	<header>
		<span class="name" title={name}>{name}</span>
		<button onclick={onclose} aria-label={t.closeImageViewer}>{t.closeImageViewer} · Esc</button>
	</header>
	<div class="toolbar">
		<button class:active={mode === 'fit'} onclick={() => setMode('fit')}>{t.imageFit}</button>
		<button class:active={mode === 'width'} onclick={() => setMode('width')}>{t.imageFitWidth}</button>
		<button class:active={mode === 'manual' && scale === 1} onclick={() => setMode('manual')}>100%</button>
		<span class="zoom">
			<button disabled={!naturalWidth || failed || scale <= 0.01} aria-label={t.imageZoomOut} onclick={() => zoom(1 / 1.25)}>−</button>
			<output>{naturalWidth ? Math.round(scale * 100) : 100}%</output>
			<button disabled={!naturalWidth || failed || scale >= 8} aria-label={t.imageZoomIn} onclick={() => zoom(1.25)}>+</button>
		</span>
	</div>
	<!-- svelte-ignore a11y_no_noninteractive_element_interactions, a11y_no_noninteractive_tabindex (scroll region must be keyboard accessible) -->
	<div class="viewport" class:dragging bind:this={viewport} bind:clientWidth={width} bind:clientHeight={height}
		role="region" aria-label={t.imageViewer} tabindex="0"
		onpointerdown={startPan} onpointermove={pan} onpointerup={endPan} onpointercancel={endPan}
		onlostpointercapture={() => dragging = false}>
		{#if failed}<p role="status">{t.imageLoadFailed}</p>{/if}
		<div class="canvas">
			<img {src} alt={name} draggable="false" hidden={failed}
				style:width={naturalWidth ? `${naturalWidth * scale}px` : 'auto'}
				style:height={naturalHeight ? `${naturalHeight * scale}px` : 'auto'}
				onload={(e) => { const img = e.currentTarget as HTMLImageElement; naturalWidth = img.naturalWidth; naturalHeight = img.naturalHeight; }}
				onerror={() => failed = true} />
		</div>
	</div>
</dialog>

<style>
	dialog {
		position: fixed;
		inset: 0;
		width: 100%;
		height: 100%;
		max-width: none;
		max-height: none;
		margin: 0;
		padding: 12px;
		border: 1px solid var(--divider);
		box-sizing: border-box;
		background: var(--solid-bg);
		color: var(--fg-primary);
	}
	dialog[open] {
		display: flex;
		flex-direction: column;
		gap: 10px;
	}
	dialog::backdrop {
		background: var(--solid-bg);
	}
	header, .toolbar, .zoom {
		display: flex;
		align-items: center;
		gap: 8px;
	}
	header {
		min-width: 0;
	}
	.name {
		flex: 1;
		min-width: 0;
		overflow: hidden;
		white-space: nowrap;
		text-overflow: ellipsis;
		font-size: 13px;
	}
	.toolbar {
		flex-wrap: wrap;
	}
	.zoom {
		margin-left: auto;
	}
	output {
		min-width: 48px;
		text-align: center;
		font-size: 12px;
		font-variant-numeric: tabular-nums;
	}
	button {
		border: 1px solid var(--divider);
		border-radius: 6px;
		background: transparent;
		color: inherit;
		padding: 6px 10px;
		cursor: pointer;
		font: inherit;
		font-size: 12px;
	}
	button:hover, button.active {
		background: var(--accent-soft);
	}
	button:disabled {
		opacity: .4;
		cursor: default;
	}
	.viewport {
		flex: 1;
		min-height: 0;
		min-width: 0;
		overflow: auto;
		overscroll-behavior: contain;
		touch-action: none;
		background: var(--row-hover);
		border-radius: 6px;
	}
	.canvas {
		display: flex;
		width: max-content;
		height: max-content;
		min-width: 100%;
		min-height: 100%;
	}
	img {
		flex: none;
		margin: auto;
		max-width: none;
		user-select: none;
		cursor: grab;
	}
	.dragging, .dragging img {
		cursor: grabbing;
	}
	p {
		padding: 16px;
		color: var(--fg-secondary);
	}
</style>
