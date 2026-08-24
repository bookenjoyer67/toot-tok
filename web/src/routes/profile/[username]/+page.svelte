<script lang="ts">
	import { page } from '$app/state';
	import { apiGet } from '$lib/api';
	import FollowButton from '$lib/components/FollowButton.svelte';
	import { followingMap, sessionUser } from '$lib/stores';
	import type { Clip, FeedResponse, Profile, ProfileResponse } from '$lib/types';

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
				// API nests the actor: { actor: {...}, clips: [...], counts... }
				const nested = data as ProfileResponse & { actor?: Profile };
				profile = nested.actor
					? {
							...nested.actor,
							clips: data.clips ?? [],
							follower_count: data.follower_count,
							following_count: data.following_count,
							likes_received: data.likes_received,
							is_following: data.is_following
							}
							: data;
							loading = false;
							// Trust the server's is_following over a possibly-stale client map.
							const actor = nested.actor;
							if ($sessionUser && actor) {
								const name = actor.username;
								followingMap.update((m) => {
									if (!data.is_following && !m.has(name)) return m;
									const next = new Map(m);
									if (data.is_following) {
										next.set(name, { actorId: actor.actor_id ?? 0, domain: null });
									} else {
										next.delete(name);
									}
									return next;
								});
							}
							})
			.catch((err: unknown) => {
				loadError = err instanceof Error ? err.message : 'Could not load this profile.';
				loading = false;
			});
	});

	const isMe = $derived(
		Boolean(
			$sessionUser &&
				$sessionUser.username === username &&
				!(profile?.domain && profile.domain.length > 0)
		)
	);

	type Tab = 'clips' | 'saved';
	let tab = $state<Tab>('clips');
	let savedClips = $state<Clip[] | null>(null);
	let savedLoading = $state(false);

	type ListKind = 'followers' | 'following';
	let listOpen = $state<ListKind | null>(null);
	let listItems = $state<{ actor_id: number; username: string }[]>([]);
	let listLoading = $state(false);

	async function openList(kind: ListKind): Promise<void> {
		listOpen = kind;
		listLoading = true;
		listItems = [];
		try {
			const data = await apiGet<{ items?: { actor_id: number; username: string }[] }>(
				`/profiles/${encodeURIComponent(username)}/${kind}`
			);
			listItems = data.items ?? [];
		} catch {
			listItems = [];
		} finally {
			listLoading = false;
		}
	}

	async function loadSaved(): Promise<void> {
		if (savedClips || savedLoading) return;
		savedLoading = true;
		try {
			const data = await apiGet<FeedResponse>('/bookmarks');
			savedClips = data.items ?? [];
		} catch {
			savedClips = [];
		} finally {
			savedLoading = false;
		}
	}

	function switchTab(t: Tab): void {
		tab = t;
		if (t === 'saved' && isMe) void loadSaved();
	}

	const gridClips = $derived(tab === 'clips' ? (profile?.clips ?? []) : (savedClips ?? []));

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
				<div>
					<button class="stat-btn" onclick={() => openList('following')}>
						<dt>Following</dt><dd>{fmt(profile.following_count ?? 0)}</dd>
					</button>
				</div>
				<div>
					<button class="stat-btn" onclick={() => openList('followers')}>
						<dt>Followers</dt><dd>{fmt(profile.follower_count ?? 0)}</dd>
					</button>
				</div>
				<div><dt>Likes</dt><dd>{fmt(profile.likes_received ?? 0)}</dd></div>
			</dl>
			{#if profile.summary}
				<!-- summary is escaped plain text from the backend; render as TEXT -->
				<p class="bio">{profile.summary}</p>
			{/if}
			{#if !isMe && !profile.domain}
				<FollowButton
					username={profile.username}
					actorId={profile.actor_id}
					size="lg"
				/>
			{/if}
		</header>

		<nav class="ptabs" aria-label="Profile sections">
			<button class:active={tab === 'clips'} onclick={() => switchTab('clips')}>Clips</button>
			{#if isMe}
				<button class:active={tab === 'saved'} onclick={() => switchTab('saved')}>Saved</button>
			{/if}
		</nav>

	<section class="grid" aria-label="Clips by @{profile.username}">
			{#each gridClips as clip (clip.id)}
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
				<p class="none">{tab === 'saved' ? 'Nothing saved yet.' : 'No clips yet.'}</p>
			{/each}
	</section>
	{/if}
</div>

{#if listOpen}
	<div
		class="lback"
		role="presentation"
		onclick={(e) => e.target === e.currentTarget && (listOpen = null)}
	>
		<div class="lsheet" role="dialog" aria-label={`${listOpen} of @${username}`}>
			<p class="lhead">{listOpen === 'followers' ? 'Followers' : 'Following'}</p>
			{#if listLoading}
				<div class="lcenter"><span class="spinner"></span></div>
			{:else if listItems.length === 0}
				<p class="lnone">Nobody here yet.</p>
			{:else}
				<ul class="llist">
					{#each listItems as u (u.actor_id)}
						<li>
							<a href={`/profile/${encodeURIComponent(u.username)}`} onclick={() => (listOpen = null)}>
								<span class="lavatar">{(u.username || '?').charAt(0).toUpperCase()}</span>
								<span class="lname">@{u.username}</span>
							</a>
						</li>
					{/each}
				</ul>
			{/if}
			<button class="lclose" onclick={() => (listOpen = null)}>Close</button>
		</div>
	</div>
{/if}

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

	.ptabs {
		display: flex;
		gap: 4px;
		margin-bottom: 1rem;
		border-bottom: 1px solid rgba(255, 255, 255, 0.08);
	}

	.ptabs button {
		flex: 1;
		border: none;
		background: transparent;
		color: #8f8f9a;
		font: inherit;
		font-weight: 600;
		font-size: 0.9rem;
		padding: 0.6rem 0;
		cursor: pointer;
		border-bottom: 2px solid transparent;
	}

	.ptabs button.active {
		color: var(--text);
		border-bottom-color: var(--text);
	}

	.stat-btn {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
		border: none;
		background: transparent;
		color: inherit;
		font: inherit;
		cursor: pointer;
		padding: 0;
	}

	.stat-btn:hover dd {
		color: var(--accent);
	}

	.lback {
		position: fixed;
		inset: 0;
		z-index: 90;
		background: rgba(0, 0, 0, 0.55);
		display: flex;
		align-items: flex-end;
		justify-content: center;
	}

	.lsheet {
		width: min(100%, 420px);
		max-height: 65dvh;
		display: flex;
		flex-direction: column;
		background: var(--surface, #16161d);
		border-radius: 16px 16px 0 0;
		padding: 14px 12px calc(var(--safe-bottom, 0px) + 12px);
	}

	.lhead {
		margin: 0 0 8px;
		text-align: center;
		font-weight: 700;
	}

	.llist {
		list-style: none;
		margin: 0;
		padding: 0;
		overflow-y: auto;
		flex: 1;
	}

	.llist a {
		display: flex;
		align-items: center;
		gap: 10px;
		padding: 0.5rem 0.4rem;
		border-radius: 10px;
		color: var(--text, #eee);
		text-decoration: none;
	}

	.llist a:hover {
		background: rgba(255, 255, 255, 0.06);
	}

	.lavatar {
		display: grid;
		place-items: center;
		width: 34px;
		height: 34px;
		border-radius: 50%;
		background: #2a2a33;
		color: #fff;
		font-weight: 700;
	}

	.lname {
		font-size: 0.92rem;
	}

	.lnone,
	.lcenter {
		padding: 1.5rem 0;
		text-align: center;
		color: #9a9aa5;
	}

	.lclose {
		margin-top: 8px;
		border: none;
		border-top: 1px solid rgba(255, 255, 255, 0.08);
		background: transparent;
		color: #9a9aa5;
		font: inherit;
		padding: 0.7rem 0 0;
		cursor: pointer;
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
