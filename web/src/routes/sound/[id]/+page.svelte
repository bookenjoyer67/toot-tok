<script lang="ts">
	import { page } from '$app/state';
	import { apiGet } from '$lib/api';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import type { Clip } from '$lib/types';

	const soundId = $derived(Number(page.params.id ?? 0));

	interface SoundCard {
		id: number;
		title: string;
		author: string | null;
		clip_count: number;
	}

	let sound = $state<SoundCard | null>(null);
	let clips = $state<Clip[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	$effect(() => {
		if (!soundId) return;
		void (async () => {
			try {
				const [card, feed] = await Promise.all([
					apiGet<SoundCard>(`/sounds/${soundId}`),
					apiGet<{ items?: Clip[] }>(`/sounds/${soundId}/clips`)
				]);
				sound = card;
				clips = feed.items ?? [];
			} catch (err) {
				loadError = err instanceof Error ? err.message : 'Could not load this sound.';
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
	<title>{sound?.title ?? 'Sound'} · TootTok</title>
</svelte:head>

<div class="page safe-area">
	{#if loading}
		<div class="center"><span class="spinner"></span></div>
	{:else if loadError}
		<EmptyState heading="Couldn't load sound" message={loadError} />
	{:else if sound}
		<header class="card-head">
			<span class="disc" aria-hidden="true">
				<svg viewBox="0 0 24 24" fill="currentColor">
					<path d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z" />
				</svg>
			</span>
			<div class="meta">
				<h1>{sound.title}</h1>
				<p class="sub">
					{#if sound.author}@{sound.author} · {/if}{fmt(sound.clip_count)} clips
				</p>
			</div>
		</header>

		<section class="grid" aria-label="Clips using this sound">
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
					<span class="likes">{fmt(clip.like_count)} ♥</span>
				</a>
			{:else}
				<EmptyState message="No clips use this sound yet." />
			{/each}
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

	.center {
		display: grid;
		place-items: center;
		padding: 4rem 0;
	}

	.card-head {
		display: flex;
		align-items: center;
		gap: 14px;
		padding-bottom: 1.25rem;
		border-bottom: 1px solid rgba(255, 255, 255, 0.08);
		margin-bottom: 1rem;
	}

	.disc {
		display: grid;
		place-items: center;
		width: 72px;
		height: 72px;
		border-radius: 50%;
		background: linear-gradient(135deg, var(--accent), #ff7a59);
		color: #fff;
		flex-shrink: 0;
		animation: spin 8s linear infinite;
	}

	.disc svg {
		width: 30px;
		height: 30px;
	}

	@keyframes spin {
		to {
			transform: rotate(360deg);
		}
	}

	h1 {
		margin: 0 0 2px;
		font-size: 1.15rem;
		word-break: break-word;
	}

	.sub {
		margin: 0;
		color: #9a9aa5;
		font-size: 0.85rem;
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
		width: 26px;
		height: 26px;
	}

	.likes {
		position: absolute;
		left: 6px;
		bottom: 5px;
		color: #fff;
		font-size: 0.68rem;
		font-weight: 600;
		text-shadow: 0 1px 3px rgba(0, 0, 0, 0.7);
	}
</style>
