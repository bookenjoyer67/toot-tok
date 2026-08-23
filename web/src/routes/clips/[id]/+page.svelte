<script lang="ts">
	import { page } from '$app/state';
	import { apiGet } from '$lib/api';
	import type { Author } from '$lib/types';

	interface ClipAsset {
		kind: string;
		rendition: string;
		url: string;
	}

	interface ClipDetail {
		id: string;
		status: string;
		duration_s?: number | null;
		width?: number | null;
		height?: number | null;
		like_count: number;
		comment_count: number;
		share_count?: number;
		view_count?: number;
		caption_html?: string | null;
		created_at: string;
		assets: ClipAsset[];
		// The detail endpoint omits these today; tolerate a feed-style payload
		// if a later API revision includes it.
		asset_url?: string;
		poster_url?: string;
		author?: Author | null;
	}

	const clipId = $derived(page.params.id ?? '');

	let clip = $state<ClipDetail | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	const assetUrl = $derived(
		clip?.asset_url ||
			clip?.assets.find((a) => a.kind === 'video_mp4')?.url ||
			clip?.assets.find((a) => a.kind?.startsWith('video'))?.url ||
			null
	);
	const posterUrl = $derived(
		clip?.poster_url || clip?.assets.find((a) => a.kind === 'poster')?.url || null
	);

	$effect(() => {
		const id = clipId;
		clip = null;
		loadError = null;
		loading = true;
		if (!id) {
			loading = false;
			return;
		}
		apiGet<ClipDetail>(`/clips/${encodeURIComponent(id)}`)
			.then((data) => {
				clip = data;
				loading = false;
			})
			.catch((err: unknown) => {
				loadError = err instanceof Error ? err.message : 'Could not load this clip.';
				loading = false;
			});
	});

	function fmt(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
		return String(n);
	}

	function timeAgo(iso: string): string {
		const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
		if (s < 60) return 'just now';
		if (s < 3600) return `${Math.floor(s / 60)}m`;
		if (s < 86_400) return `${Math.floor(s / 3600)}h`;
		if (s < 604_800) return `${Math.floor(s / 86_400)}d`;
		return `${Math.floor(s / 604_800)}w`;
	}
</script>

<svelte:head>
	<title>Clip · TootTok</title>
</svelte:head>

<div class="page">
	{#if loading}
		<div class="center"><span class="spinner"></span></div>
	{:else if loadError}
		<div class="center error">{loadError}</div>
	{:else if clip}
		{#if assetUrl}
			<!-- svelte-ignore a11y_media_has_caption -->
			<video class="stage" src={assetUrl} poster={posterUrl ?? undefined} controls autoplay muted loop playsinline></video>
		{:else}
			<div class="stage placeholder"></div>
		{/if}

		<div class="body safe-area">
			<a class="back" href="/">← Back to feed</a>

			{#if clip.caption_html}
				<p class="caption">{clip.caption_html ?? ''}</p>
			{/if}

			<div class="meta">
				{#if clip.author?.username}
					<a class="author" href="/profile/{clip.author.username}">
						@{clip.author.username}{#if clip.author.domain}@{clip.author.domain}{/if}
					</a>
				{/if}
				<span class="when">{timeAgo(clip.created_at)}</span>
			</div>

			<div class="stats">
				<div class="stat">
					<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
					</svg>
					<span>{fmt(clip.like_count)}</span>
				</div>
				<div class="stat">
					<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
						<path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z" />
					</svg>
					<span>{fmt(clip.comment_count)}</span>
				</div>
			</div>
		</div>
	{/if}
</div>

<style>
	.page {
		min-height: 100dvh;
		background: #000;
		display: flex;
		flex-direction: column;
	}

	.stage {
		width: 100%;
		aspect-ratio: 9 / 16;
		max-height: 62dvh;
		object-fit: cover;
		background: #000;
		display: block;
	}

	.stage.placeholder {
		background: #0b0b0f;
	}

	.body {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
		padding-top: 1rem;
		padding-bottom: calc(var(--safe-bottom) + 5rem);
	}

	.back {
		color: var(--accent, #ff4d6d);
		text-decoration: none;
		font-weight: 600;
		font-size: 0.9rem;
		align-self: flex-start;
	}

	.caption {
		margin: 0;
		font-size: 1rem;
		line-height: 1.5;
		overflow-wrap: anywhere;
	}

	.meta {
		display: flex;
		align-items: center;
		gap: 0.6rem;
	}

	.author {
		color: #fff;
		font-weight: 700;
		font-size: 0.95rem;
		text-decoration: none;
	}

	.when {
		color: #8f8f9a;
		font-size: 0.82rem;
	}

	.stats {
		display: flex;
		gap: 1.25rem;
		margin-top: 0.25rem;
	}

	.stat {
		display: inline-flex;
		align-items: center;
		gap: 0.35rem;
		color: #d8d8de;
		font-weight: 600;
		font-size: 0.85rem;
		font-variant-numeric: tabular-nums;
	}

	.stat svg {
		width: 18px;
		height: 18px;
		color: var(--accent, #ff4d6d);
	}

	.center {
		display: grid;
		place-items: center;
		padding: 4rem 0;
	}

	.error {
		color: #ff8298;
		font-size: 0.9rem;
		text-align: center;
	}
</style>
