<script lang="ts">
	// A deliberately spare, original gear mark: two concentric rings plus eight
	// rounded teeth. It stays in the same 24 px ghost-button family as PinButton
	// without importing a third-party icon whose silhouette may feel out of place.
	import { t } from '../i18n';

	let { onclick }: { onclick: () => void } = $props();
	const label = `${t.soSettings} · Ctrl+,`;
</script>

<button type="button" class="settings" aria-label={label} {onclick}>
	<svg class="icon" width="15" height="15" viewBox="0 0 20 20" fill="none" aria-hidden="true">
		<circle cx="10" cy="10" r="4.35" stroke="currentColor" stroke-width="1.35" />
		<circle cx="10" cy="10" r="1.55" stroke="currentColor" stroke-width="1.35" />
		<path
			d="M10 2.15v2M10 15.85v2M2.15 10h2M15.85 10h2M4.45 4.45l1.42 1.42M14.13 14.13l1.42 1.42M15.55 4.45l-1.42 1.42M5.87 14.13l-1.42 1.42"
			stroke="currentColor"
			stroke-width="1.35"
			stroke-linecap="round"
		/>
	</svg>
	<span class="tooltip" aria-hidden="true">{label}</span>
</button>

<style>
	.settings {
		position: relative;
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 24px;
		height: 24px;
		border: none;
		background: transparent;
		border-radius: var(--radius-chip);
		color: var(--fg-tertiary);
		cursor: default;
		transition:
			color 0.12s ease-out,
			background-color 0.12s ease-out;
	}

	.settings:hover,
	.settings:focus-visible {
		color: var(--fg-secondary);
		background: var(--row-hover);
	}

	.settings:focus-visible {
		outline: 1px solid var(--accent-border);
		outline-offset: 1px;
	}

	.icon {
		transition: transform 0.16s ease-out;
	}

	.settings:hover .icon {
		transform: rotate(12deg);
	}

	.tooltip {
		position: absolute;
		top: calc(100% + 7px);
		right: 0;
		z-index: 40;
		padding: 5px 8px;
		border: 1px solid var(--divider);
		border-radius: var(--radius-chip);
		background: var(--solid-bg);
		box-shadow: var(--panel-shadow);
		color: var(--fg-secondary);
		font-size: 11px;
		font-weight: 500;
		line-height: 1;
		white-space: nowrap;
		opacity: 0;
		transform: translateY(-2px);
		pointer-events: none;
		transition:
			opacity 0.12s ease-out,
			transform 0.12s ease-out;
	}

	.settings:hover .tooltip,
	.settings:focus-visible .tooltip {
		opacity: 1;
		transform: translateY(0);
	}
</style>
