<script lang="ts">
	import { page } from '$app/state';
	import { apiGet } from '$lib/api';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import type { Clip, FeedResponse } from '$lib/types';

	const tag = $derived(decodeURIComponent(page.params.tag ?? ''));

	let items = $state<Clip[]>([]);
	let nextCursor = $state<string | null>(null);
	let loading = $state(true);
	let loadingMore = $state(false);
	let loadError = $state<string | null>(null);

	$effect(() => {
		const t = tag;
		items = [];
		nextCursor = null;
		loadError = null;
		loading = true;
		void fetchPage(t, null, true);
	});

	async function fetchPage(t: string, cursor: string | null, initial: boolean): Promise<void> {
		if (!t) return;
		if (initial) loading = true;
		else loadingMore = true;
		try {
			const qs = cursor ? `?cursor=${encodeURIComponent(cursor)}` : '';
			const res = await apiGet<FeedResponse>(`/tags/${encodeURIComponent(t)}/clips${qs}`);
			const fresh = res.items ?? [];
			items = cursor ? [...items, ...fresh] : fresh;
			nextCursor = res.next_cursor ?? null;
			loadError = null;
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Could not load clips for this tag.';
		} finally {
			loading = false;
			loadingMore = false;
		}
	}

	function fmt(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
		return String(n);
	}
</script>

<svelte:head>
	<title>#{tag} · TootTok</title>
</svelte:head>

<div class="page safe-area">
	<header class="head">
		<h1>#{tag}</h1>
	</header>

	{#if loading}
		<div class="center"><span class="spinner"></span></div>
	{:else if loadError && items.length === 0}
		<div class="center error">{loadError}</div>
	{:else if items.length === 0}
		<EmptyState heading="No clips yet" message={`Nothing has been tagged #${tag} so far.`} />
	{:else}
		<section class="grid" aria-label="Clips tagged #{tag}">
			{#each items as clip (clip.id)}
				<a class="thumb" href="/profile/{clip.author.username}" aria-label={clip.caption_html?.replace(/<[^>]*>/g, '') || 'Clip'}>
					{#if clip.poster_url}
						<img src={clip.poster_url} alt="" loading="lazy" />
					{:else}
						<span class="placeholder">
							<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
								<path d="M8 5.14v13.72a1 1 0 0 0 1.5.86l11-6.86a1 1 0 0 0 0-1.72l-11-6.86a1 1 0 0 0-1.5.86z" />
							</svg>
						</span>
					{/if}
					<span class="thumb-meta">
						<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
							<path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
						</svg>
						{fmt(clip.like_count)}
					</span>
				</a>
			{/each}
		</section>

		{#if nextCursor}
			<div class="more">
				<button class="btn ghost" onclick={() => void fetchPage(tag, nextCursor, false)} disabled={loadingMore}>
					{loadingMore ? 'Loading…' : 'Load more'}
				</button>
			</div>
		{/if}

		{#if loadError}
			<p class="error">{loadError}</p>
		{/if}
	{/if}
</div>

<style>
	.page {
		min-height: 100dvh;
		max-width: 560px;
		margin-inline: auto;
		padding: calc(var(--safe-top) + 1.25rem) 1rem calc(var(--safe-bottom) + 5.5rem);
	}

	.head {
		margin-bottom: 0.9rem;
	}

	h1 {
		margin: 0;
		font-size: 1.25rem;
		color: var(--accent);
	}

	.center {
		display: grid;
		place-items: center;
		padding: 4rem 0;
	}

	.error {
		text-align: center;
		color: #ff8298;
		font-size: 0.85rem;
		padding: 0.75rem 0 0;
		margin: 0;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 3px;
	}

	.thumb {
		position: relative;
		display: block;
		aspect-ratio: 9 / 16;
		border-radius: 6px;
		overflow: hidden;
		background: rgba(255, 255, 255, 0.04);
	}

	.thumb img {
		width: 100%;
		height: 100%;
		object-fit: cover;
		display: block;
	}

	.placeholder {
		width: 100%;
		height: 100%;
		display: grid;
		place-items: center;
		background: linear-gradient(160deg, #1c1c26, #12121a);
		color: rgba(255, 255, 255, 0.28);
	}

	.placeholder svg {
		width: 30%;
		height: auto;
	}

	.thumb-meta {
		position: absolute;
		left: 6px;
		bottom: 5px;
		display: inline-flex;
		align-items: center;
		gap: 4px;
		font-size: 0.72rem;
		font-weight: 600;
		color: #fff;
		text-shadow: 0 1px 3px rgba(0, 0, 0, 0.8);
	}

	.thumb-meta svg {
		width: 12px;
		height: 12px;
		color: var(--accent);
	}

	.more {
		display: flex;
		justify-content: center;
		padding: 1rem 0 0.25rem;
	}
</style>
