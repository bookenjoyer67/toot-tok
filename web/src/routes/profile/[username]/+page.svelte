<script lang="ts">
	import { page } from '$app/state';
	import { apiGet } from '$lib/api';
	import type { Clip, Profile } from '$lib/types';

	type ProfileResponse = Profile & { clips?: Clip[] };

	const username = $derived(page.params.username ?? '');

	let profile = $state<ProfileResponse | null>(null);
	let loading = $state(true);
	let loadError = $state<string | null>(null);

	$effect(() => {
		const u = username;
		profile = null;
		loadError = null;
		loading = true;
		apiGet<ProfileResponse>(`/profiles/${encodeURIComponent(u)}`)
			.then((data) => {
				// API nests the actor: { actor: {...}, clips: [...] } — flatten it
				const nested = data as ProfileResponse & { actor?: Profile };
				profile = nested.actor
					? { ...nested.actor, clips: data.clips ?? [] }
					: data;
				loading = false;
			})
			.catch((err: unknown) => {
				loadError = err instanceof Error ? err.message : 'Could not load this profile.';
				loading = false;
			});
	});

	function fmt(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
		return String(n);
	}
</script>

<svelte:head>
	<title>@{username} · TootTok</title>
</svelte:head>

<div class="page safe-area">
	{#if loading}
		<div class="center"><span class="spinner"></span></div>
	{:else if loadError}
		<div class="center error">{loadError}</div>
	{:else if profile}
		<header class="actor">
			{#if profile.avatar_path}
				<img class="avatar" src={profile.avatar_path} alt="" />
			{:else}
				<span class="avatar fallback">
					{(profile.display_name || profile.username || '?').charAt(0).toUpperCase()}
				</span>
			{/if}
			<div class="who">
				<h1>{profile.display_name || profile.username || '?'}</h1>
				<p class="handle">@{profile.username}{#if profile.domain}@{profile.domain}{/if}</p>
			</div>
			<dl class="stats">
				<!-- Backend sends only live clip rows; follower/following counts
				     arrive in a later API revision — show clips we can count. -->
				<div><dt>Clips</dt><dd>{fmt(profile.clips?.length ?? 0)}</dd></div>
			</dl>
			{#if profile.summary}
				<!-- summary is escaped plain text from the backend; render as TEXT -->
				<p class="bio">{profile.summary}</p>
			{/if}
		</header>

		<section class="grid" aria-label="Clips by @{profile.username}">
			{#each profile.clips ?? [] as clip (clip.id)}
				<div class="thumb">
					{#if clip.poster_url}
						<img src={clip.poster_url} alt="" loading="lazy" />
					{:else}
						<div class="placeholder">
							<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
								<path d="M8 5.14v13.72a1 1 0 0 0 1.5.86l11-6.86a1 1 0 0 0 0-1.72l-11-6.86a1 1 0 0 0-1.5.86z" />
							</svg>
						</div>
					{/if}
					<span class="thumb-meta">
						<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
							<path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
						</svg>
						{fmt(clip.like_count)}
					</span>
				</div>
			{:else}
				<p class="none">No clips yet.</p>
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

	.error {
		color: #ff8298;
		font-size: 0.9rem;
		text-align: center;
	}

	.actor {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.75rem;
		text-align: center;
		padding-bottom: 1.5rem;
		border-bottom: 1px solid rgba(255, 255, 255, 0.08);
		margin-bottom: 1rem;
	}

	.avatar {
		width: 84px;
		height: 84px;
		border-radius: 50%;
		object-fit: cover;
	}

	.fallback {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		background: var(--accent);
		color: #fff;
		font-weight: 700;
		font-size: 2rem;
	}

	h1 {
		margin: 0;
		font-size: 1.25rem;
	}

	.handle {
		margin: 0.15rem 0 0;
		color: #8f8f9a;
		font-size: 0.88rem;
	}

	.stats {
		display: flex;
		gap: 1.75rem;
		margin: 0.35rem 0 0;
	}

	.stats div {
		display: flex;
		flex-direction: column-reverse;
		gap: 1px;
	}

	dd {
		margin: 0;
		font-weight: 700;
		font-variant-numeric: tabular-nums;
	}

	dt {
		font-size: 0.72rem;
		color: #8f8f9a;
	}

	.bio {
		margin: 0.25rem 0 0;
		max-width: 42ch;
		color: #d8d8de;
		font-size: 0.9rem;
		line-height: 1.5;
		overflow-wrap: anywhere;
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

	.none {
		grid-column: 1 / -1;
		text-align: center;
		color: #8f8f9a;
		padding: 2.5rem 0;
	}
</style>
