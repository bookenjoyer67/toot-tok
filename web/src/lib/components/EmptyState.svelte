<script lang="ts">
	import type { Snippet } from 'svelte';

	let {
		heading = '',
		message = '',
		children
	}: { heading?: string; message?: string; children?: Snippet } = $props();
</script>

<div class="empty">
	<svg
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		stroke-width="1.6"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<rect x="2" y="2" width="20" height="20" rx="2.18" ry="2.18" />
		<line x1="7" y1="2" x2="7" y2="22" />
		<line x1="17" y1="2" x2="17" y2="22" />
		<line x1="2" y1="12" x2="22" y2="12" />
		<line x1="2" y1="7" x2="7" y2="7" />
		<line x1="2" y1="17" x2="7" y2="17" />
		<line x1="17" y1="17" x2="22" y2="17" />
		<line x1="17" y1="7" x2="22" y2="7" />
	</svg>
	{#if heading}
		<h2>{heading}</h2>
	{/if}
	{#if message}
		<p>{message}</p>
	{/if}
	{@render children?.()}
</div>

<style>
	.empty {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 10px;
		text-align: center;
		padding: 2rem;
		max-width: 340px;
		margin-inline: auto;
	}

	.empty svg {
		width: 56px;
		height: 56px;
		color: var(--accent);
		opacity: 0.9;
	}

	h2 {
		margin: 0;
		font-size: 1.15rem;
	}

	p {
		margin: 0;
		color: #b9b9c3;
		line-height: 1.5;
	}

	.empty :global(a),
	.empty :global(button) {
		margin-top: 0.5rem;
	}
</style>
