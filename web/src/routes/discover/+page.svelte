<script lang="ts">
	import { apiGet } from '$lib/api';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import type { Clip } from '$lib/types';

	let clips = $state<Clip[]>([]);
	let tags = $state<{ tag: string; uses: number }[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	$effect(() => {
		void (async () => {
			try {
				const [feed, tagRes] = await Promise.all([
					apiGet<{ items?: Clip[] }>('/feed/trending'),
					apiGet<{ items?: { tag: string; uses: number }[] }>('/tags/trending')
				]);
				clips = feed.items ?? [];
				tags = tagRes.items ?? [];
			} catch (err) {
				loadError = err instanceof Error ? err.message : 'Could not load Discover.';
			} finally {
				loading = false;
			}
		})();
	});

	function fmt(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
		return String(n);
	}
</script>

<svelte:head>
	<title>Discover · TootTok</title>
</svelte:head>

<div class="page safe-area">
	<header class="head">
		<h1>Discover</h1>
		<a href="/search" class="search-link">Search</a>
	</header>

	{#if loading}
		<div class="center"><span class="spinner"></span></div>
	{:else if loadError}
		<EmptyState heading="Couldn't load Discover" message={loadError} />
	{:else}
		{#if tags.length > 0}
			<section aria-label="Trending tags">
				<h2>Trending</h2>
				<div class="tag-row">
					{#each tags as t (t.tag)}
						<a class="chip" href={`/tag/${encodeURIComponent(t.tag)}`}>
							#{t.tag}<span class="uses">{fmt(t.uses)}</span>
						</a>
					{/each}
				</div>
			</section>
		{/if}

		<section aria-label="Trending clips">
			<h2>Hot clips</h2>
			<div class="grid">
				{#each clips as clip (clip.id)}
					<a class="thumb" href={`/clips/${clip.id}`}>
						{#if clip.poster_url}
							<img src={clip.poster_url} alt="" loading="lazy" />
						{:else}
							<span class="ph">
								<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
									<path d="M8 5.14v13.72a1 1 0 0 0 1.5.86l11-6.86a1 1 0 0 0 0-1.72l-11-6.86a1 1 0 0 0-1.5.86z" />
								</svg>
							</span>
						{/if}
						<span class="meta">
							@{clip.author.username} · {fmt(clip.like_count)} ♥
						</span>
					</a>
				{:else}
					<p class="none">Nothing trending yet — upload something.</p>
				{/each}
			</div>
		</section>
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
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 1rem;
	}

	h1 {
		margin: 0;
		font-size: 1.4rem;
	}

	h2 {
		margin: 1.25rem 0 0.6rem;
		font-size: 0.95rem;
		color: #9a9aa5;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.search-link {
		color: var(--accent);
		font-weight: 600;
		font-size: 0.9rem;
		text-decoration: none;
	}

	.center {
		display: grid;
		place-items: center;
		padding: 4rem 0;
	}

	.tag-row {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
	}

	.chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		background: rgba(255, 255, 255, 0.07);
		border-radius: 999px;
		padding: 0.35rem 0.8rem;
		color: var(--text);
		font-weight: 600;
		font-size: 0.85rem;
		text-decoration: none;
	}

	.chip .uses {
		color: #9a9aa5;
		font-size: 0.75rem;
		font-weight: 500;
	}

	.grid {
		display: grid;
		grid-template-columns: repeat(3, 1fr);
		gap: 3px;
	}

	.thumb {
		position: relative;
		aspect-ratio: 9 / 16;
		border-radius: 6px;
		overflow: hidden;
		background: #101017;
		display: block;
	}

	.thumb img {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.ph {
		position: absolute;
		inset: 0;
		display: grid;
		place-items: center;
		color: #3a3a44;
	}

	.ph svg {
		width: 28px;
		height: 28px;
	}

	.meta {
		position: absolute;
		left: 0;
		right: 0;
		bottom: 0;
		padding: 14px 6px 5px;
		background: linear-gradient(transparent, rgba(0, 0, 0, 0.75));
		color: #fff;
		font-size: 0.68rem;
		font-weight: 600;
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
	}

	.none {
		grid-column: 1 / -1;
		text-align: center;
		color: #9a9aa5;
		padding: 2rem 0;
	}
</style>
