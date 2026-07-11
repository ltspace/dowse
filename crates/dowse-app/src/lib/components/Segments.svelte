<script lang="ts">
	import type { TextSegment } from '../types';

	let { segments }: { segments: TextSegment[] } = $props();
</script>

{#each segments as seg, i (i)}
	{#if seg.highlighted}
		<mark class="hl">{seg.text}</mark>
	{:else}
		{seg.text}
	{/if}
{/each}
