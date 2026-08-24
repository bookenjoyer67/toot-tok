<script lang="ts">
	import { onMount } from 'svelte';
	import { apiGet, setCsrf } from '$lib/api';
	import {
		appendFeed,
		bumpCommentCount,
		feedCache,
		sessionUser,
		type FeedKind
	} from '$lib/stores';
	import CommentsSheet from '$lib/components/CommentsSheet.svelte';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import VideoCard from '$lib/components/VideoCard.svelte';
	import type { Clip, FeedResponse } from '$lib/types';

	interface MeResponse {
		username: string;
		display_name?: string;
		avatar_path?: string | null;
		is_admin?: boolean;
		csrf_token?: string;
	}

	let booted = $state(false);
	let activeKind = $state<FeedKind>('discover');
	let activeIndex = $state(0);
	let loadingMore = $state(false);
	let loadError = $state<string | null>(null);
	let scroller: HTMLDivElement | undefined = $state();
	let commentsClip: Clip | null = $state(null);
	const inflightByKind: Record<FeedKind, boolean> = { following: false, local: false, discover: false };

	const cache = $derived($feedCache);
	const items = $derived(cache[activeKind].items);
	const cursor = $derived(cache[activeKind].nextCursor);

	onMount(() => {
		void init();
	});

	async function init(): Promise<void> {
		try {
			const me = await apiGet<MeResponse>('/accounts/me');
			if (me?.username) {
				if (me.csrf_token) setCsrf(me.csrf_token);
				sessionUser.set({ username: me.username, csrf: me.csrf_token ?? '', isAdmin: me.is_admin ?? false });
				activeKind = 'following';
			}
		} catch {
			sessionUser.set(null);
		}
		booted = true;
		await ensure(activeKind);
	}

	async function ensure(kind: FeedKind): Promise<void> {
		if ($feedCache[kind].loaded) return;
		if (kind === 'following' && !$sessionUser) return;
		await fetchPage(kind, $feedCache[kind].nextCursor);
	}

	async function fetchPage(kind: FeedKind, cursorParam: string | null): Promise<void> {
		if (inflightByKind[kind]) return;
		inflightByKind[kind] = true;
		loadingMore = true;
		loadError = null;
		try {
			const base =
				kind === 'following'
					? '/feed/following'
					: kind === 'local'
						? '/feed/local'
						: '/feed/discover';
			const qs = cursorParam ? `?cursor=${encodeURIComponent(cursorParam)}` : '';
			const res = await apiGet<FeedResponse>(`${base}${qs}`);
			appendFeed(kind, res.items ?? [], res.next_cursor ?? null);
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Could not load this feed.';
		} finally {
			inflightByKind[kind] = false;
			loadingMore = false;
		}
	}

	function switchKind(kind: FeedKind): void {
		if (kind === activeKind) return;
		activeKind = kind;
		activeIndex = 0;
		loadError = null;
		scroller?.scrollTo({ top: 0, behavior: 'auto' });
		void ensure(kind);
	}

	$effect(() => {
		const root = scroller;
		if (!root) return;
		void items.length;
		const io = new IntersectionObserver(
			(entries) => {
				for (const entry of entries) {
					if (!entry.isIntersecting || entry.intersectionRatio < 0.6) continue;
					const idx = Number((entry.target as HTMLElement).dataset.index);
					if (!Number.isNaN(idx)) activeIndex = idx;
				}
			},
			{ root, threshold: [0.6] }
		);
		for (const child of Array.from(root.children)) io.observe(child);
		return () => io.disconnect();
	});

	$effect(() => {
		if (!cursor) return;
		if (activeIndex < items.length - 3) return;
		if (loadingMore || inflightByKind[activeKind] || loadError) return;
		void fetchPage(activeKind, cursor);
	});

	function goTo(index: number): void {
		const clamped = Math.max(0, Math.min(index, items.length - 1));
		activeIndex = clamped;
		scroller?.children[clamped]?.scrollIntoView({ behavior: 'smooth', block: 'start' });
	}

	function onKeydown(e: KeyboardEvent): void {
		const t = e.target as HTMLElement | null;
		if (t && (t.tagName === 'INPUT' || t.tagName === 'TEXTAREA' || t.isContentEditable)) return;
		switch (e.key) {
			case 'j':
			case 'ArrowDown':
				e.preventDefault();
				goTo(activeIndex + 1);
				break;
			case 'k':
			case 'ArrowUp':
				e.preventDefault();
				goTo(activeIndex - 1);
				break;
			case 'l':
				e.preventDefault();
				items[activeIndex] && document.dispatchEvent(new CustomEvent('toottok-like'));
				break;
			case 'm':
				e.preventDefault();
				document.dispatchEvent(new CustomEvent('toottok-mute'));
				break;
			case 'c':
				e.preventDefault();
				if (items[activeIndex]) commentsClip = items[activeIndex];
				break;
		}
	}
</script>

<svelte:head>
	<title>TootTok</title>
	<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover" />
</svelte:head>

<svelte:window onkeydown={onKeydown} />

<div class="tabs">
	<button class:active={activeKind === 'following'} onclick={() => switchKind('following')}>
		Home
	</button>
	<button class:active={activeKind === 'local'} onclick={() => switchKind('local')}>
		Local
	</button>
	<button class:active={activeKind === 'discover'} onclick={() => switchKind('discover')}>
		Federated
	</button>
</div>

<a href="/search" class="search-fab" aria-label="Search">
	<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
		<circle cx="11" cy="11" r="8" />
		<line x1="21" y1="21" x2="16.65" y2="16.65" />
	</svg>
</a>

<div class="shell" bind:this={scroller}>
	{#each items as clip, i (clip.id)}
		<section class="slide" data-index={i}>
			<VideoCard {clip} active={i === activeIndex} onComments={(c) => (commentsClip = c)} />
		</section>
	{:else}
		<section class="slide notice">
			{#if !booted || (!cache[activeKind].loaded && !loadError)}
				<span class="spinner" aria-label="Loading feed"></span>
			{:else if activeKind === 'following' && !$sessionUser}
				<EmptyState heading="You're not logged in" message="Log in to see clips from people you follow.">
					<a href="/login" class="btn">Log in</a>
				</EmptyState>
			{:else if loadError}
				<EmptyState heading="Couldn't load this feed" message={loadError}>
					<button class="btn" onclick={() => void fetchPage(activeKind, cursor)}>Try again</button>
				</EmptyState>
			{:else}
				<EmptyState message="No clips yet — upload the first one">
					<a href="/upload" class="btn">Upload a clip</a>
				</EmptyState>
			{/if}
		</section>
	{/each}
</div>

{#if loadingMore && items.length > 0}
	<div class="more"><span class="spinner"></span></div>
{/if}

{#if commentsClip}
	<CommentsSheet clip={commentsClip} onclose={() => (commentsClip = null)} onposted={() =>
			bumpCommentCount(commentsClip?.id ?? '')} />
{/if}

<style>
	.shell {
		height: 100dvh;
		width: 100%;
		overflow-y: auto;
		scroll-snap-type: y mandatory;
		overscroll-behavior-y: contain;
		scrollbar-width: none;
		background: #000;
	}

	.shell::-webkit-scrollbar {
		display: none;
	}

	.slide {
		position: relative;
		height: 100dvh;
		scroll-snap-align: start;
		scroll-snap-stop: always;
	}

	.notice {
		display: grid;
		place-items: center;
		background: var(--bg);
		padding-top: var(--safe-top);
		padding-bottom: var(--safe-bottom);
	}

	.tabs {
		position: fixed;
		top: 0;
		left: 0;
		right: 0;
		z-index: 40;
		display: flex;
		justify-content: center;
		gap: 6px;
		padding-top: calc(var(--safe-top) + 12px);
		padding-left: 16px;
		padding-right: 92px;
		pointer-events: none;
	}

	.tabs button {
		pointer-events: auto;
		border: none;
		background: rgba(15, 15, 20, 0.55);
		color: #d8d8de;
		font-weight: 600;
		font-size: 0.95rem;
		padding: 0.45rem 1.05rem;
		border-radius: 999px;
		cursor: pointer;
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
		transition:
			background 0.15s ease,
			color 0.15s ease;
	}

	.tabs button.active {
		background: var(--accent);
		color: #fff;
	}

	.search-fab {
		position: fixed;
		top: calc(var(--safe-top) + 56px);
		right: calc(var(--safe-right) + 14px);
		z-index: 44;
		display: grid;
		place-items: center;
		width: 42px;
		height: 42px;
		border-radius: 50%;
		background: rgba(15, 15, 20, 0.55);
		color: #d8d8de;
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
		transition:
			color 0.15s ease,
			transform 0.15s ease;
	}

	.search-fab:hover {
		color: #fff;
	}

	.search-fab:active {
		transform: scale(0.9);
	}

	.search-fab svg {
		width: 20px;
		height: 20px;
	}

	.more {
		position: fixed;
		bottom: calc(var(--safe-bottom) + 72px);
		left: 50%;
		transform: translateX(-50%);
		z-index: 40;
		display: grid;
		place-items: center;
		width: 44px;
		height: 44px;
		border-radius: 50%;
		background: rgba(15, 15, 20, 0.55);
		backdrop-filter: blur(10px);
	}
</style>
