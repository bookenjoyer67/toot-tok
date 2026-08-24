<script lang="ts">
	import { apiGet } from '$lib/api';
	import { sessionUser } from '$lib/stores';
	import { timeAgo } from '$lib/time';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import SearchBar from '$lib/components/SearchBar.svelte';

	interface ActorHit {
		username: string;
		domain?: string | null;
		display_name?: string | null;
		avatar_path?: string | null;
	}

	interface TagHit {
		tag?: string;
		name?: string;
	}

	interface ClipHit {
		id?: string;
		caption_html?: string | null;
		caption_text?: string | null;
		caption?: string | null;
		author?: { username?: string; domain?: string | null };
		created_at?: string;
	}

	type SearchType = '' | 'actors' | 'tags' | 'clips';

	const TABS: { id: SearchType; label: string }[] = [
		{ id: '', label: 'All' },
		{ id: 'actors', label: 'People' },
		{ id: 'tags', label: 'Tags' },
		{ id: 'clips', label: 'Clips' }
	];

	let query = $state('');
	let activeType = $state<SearchType>('');
	let actors = $state<ActorHit[]>([]);
	let tags = $state<TagHit[]>([]);
	let clips = $state<ClipHit[]>([]);
	let searched = $state(false);
	let loading = $state(false);
	let loadError = $state<string | null>(null);

	let controller: AbortController | undefined;
	let lastRun = '';

	const hasResults = $derived(actors.length > 0 || tags.length > 0 || clips.length > 0);
	const loggedIn = $derived(Boolean($sessionUser));

	function bucketOf(data: unknown, key: string): unknown[] {
		if (Array.isArray(data)) return data;
		if (data && typeof data === 'object') {
			const value = (data as Record<string, unknown>)[key];
			if (Array.isArray(value)) return value;
			const items = (data as Record<string, unknown>).items;
			if (items && typeof items === 'object' && Array.isArray((items as Record<string, unknown>)[key])) {
				return (items as Record<string, unknown>)[key] as unknown[];
			}
		}
		return [];
	}

	async function run(rawQuery: string): Promise<void> {
		query = rawQuery;
		controller?.abort();
		if (!query) {
			searched = false;
			actors = [];
			tags = [];
			clips = [];
			loadError = null;
			loading = false;
			return;
		}
		lastRun = `${query}\u0000${activeType}`;
		const stamp = lastRun;
		controller = new AbortController();
		loading = true;
		loadError = null;
		try {
			const typePart = activeType ? `&type=${encodeURIComponent(activeType)}` : '';
			const data = await apiGet<unknown>(
				`/search?q=${encodeURIComponent(query)}${typePart}`,
				controller.signal
			);
			if (stamp !== lastRun) return;
			if (!activeType || activeType === 'actors') {
				actors = bucketOf(data, 'actors') as ActorHit[];
			} else {
				actors = [];
			}
			if (!activeType || activeType === 'tags') {
				tags = bucketOf(data, 'tags') as TagHit[];
			} else {
				tags = [];
			}
			if (!activeType || activeType === 'clips') {
				clips = bucketOf(data, 'clips') as ClipHit[];
			} else {
				clips = [];
			}
			searched = true;
		} catch (err) {
			if (err instanceof DOMException && err.name === 'AbortError') return;
			loadError = err instanceof Error ? err.message : 'Search failed.';
		} finally {
			if (stamp === lastRun) loading = false;
		}
	}

	function switchType(type: SearchType): void {
		if (type === activeType) return;
		activeType = type;
		if (query) void run(query);
	}

	function tagName(tag: TagHit): string {
		return tag.tag ?? tag.name ?? '';
	}

	function clipText(clip: ClipHit): string {
		if (clip.caption_text) return clip.caption_text;
		if (clip.caption) return clip.caption;
		return (clip.caption_html ?? '').replace(/<[^>]*>/g, '').trim();
	}

	function handle(author: { username?: string; domain?: string | null } | undefined): string {
		if (!author?.username) return '';
		const domain = author.domain && author.domain !== 'local' ? `@${author.domain}` : '';
		return `@${author.username}${domain}`;
	}

	const actorKey = (a: ActorHit, i: number) => `${a.username}@${a.domain ?? ''}-${i}`;
	const tagKey = (t: TagHit, i: number) => `${tagName(t)}-${i}`;
	const clipKey = (c: ClipHit, i: number) => `${c.id ?? 'clip'}-${i}`;
</script>

<svelte:head>
	<title>Search · TootTok</title>
</svelte:head>

<div class="page safe-area">
	<header class="head">
		<h1>Search</h1>
	</header>

	<div class="searchbox">
		<SearchBar bind:value={query} placeholder="Search people, tags, clips" onquery={(q) => void run(q)} />
	</div>

	<div class="types" role="tablist" aria-label="Result type">
		{#each TABS as t (t.id)}
			<button role="tab" aria-selected={activeType === t.id} class:active={activeType === t.id} onclick={() => switchType(t.id)}>
				{t.label}
			</button>
		{/each}
	</div>

	{#if loading && !hasResults}
		<div class="center"><span class="spinner"></span></div>
	{:else if loadError}
		<div class="center error">{loadError}</div>
	{:else if !searched}
		<EmptyState
			heading="Find things"
			message={loggedIn
				? 'Look up people, hashtags and clips across the network.'
				: 'Log in to search for people and clips.'}
		/>
	{:else if !hasResults}
		<EmptyState heading="Nothing found" message="No matches — try different keywords." />
	{:else}
		{#if actors.length > 0}
			<section aria-label="People">
				<h2>People</h2>
				<ul class="list">
					{#each actors as a, i (actorKey(a, i))}
						<li class="item actor">
							<a class="actor-link" href="/profile/{a.username}">
								{#if a.avatar_path}
									<img class="avatar" src={a.avatar_path} alt="" loading="lazy" />
								{:else}
									<span class="avatar fallback">{(a.display_name || a.username || '?').charAt(0).toUpperCase()}</span>
								{/if}
								<span class="who">
									<span class="name">{a.display_name || a.username}</span>
									<span class="handle">@{a.username}{#if a.domain && a.domain !== 'local'}@{a.domain}{/if}</span>
								</span>
							</a>
						</li>
					{/each}
				</ul>
			</section>
		{/if}

		{#if tags.length > 0}
			<section aria-label="Tags">
				<h2>Tags</h2>
				<div class="chips">
					{#each tags as t, i (tagKey(t, i))}
						{#if tagName(t)}
							<a class="chip" href="/tag/{tagName(t)}">#{tagName(t)}</a>
						{/if}
					{/each}
				</div>
			</section>
		{/if}

		{#if clips.length > 0}
			<section aria-label="Clips">
				<h2>Clips</h2>
				<ul class="list">
					{#each clips as c, i (clipKey(c, i))}
						<li class="item">
							{#if c.id}
								<a class="clip-link" href="/clips/{c.id}">
									<div class="clip-body">
										<p class="caption">{clipText(c) || '(no caption)'}</p>
										{#if c.author?.username || c.created_at}
											<p class="meta">
												{#if c.author?.username}<span>{handle(c.author)}</span>{/if}
												{#if c.created_at}<span>{timeAgo(c.created_at)}</span>{/if}
											</p>
										{/if}
									</div>
								</a>
							{:else}
								<div class="clip-body">
									<p class="caption">{clipText(c) || '(no caption)'}</p>
								</div>
							{/if}
						</li>
					{/each}
				</ul>
			</section>
		{/if}
	{/if}
</div>

<style>
	.page {
		min-height: 100dvh;
		max-width: 520px;
		margin-inline: auto;
		padding: calc(var(--safe-top) + 1.25rem) 1rem calc(var(--safe-bottom) + 5.5rem);
	}

	.head {
		display: flex;
		align-items: center;
		justify-content: space-between;
		margin-bottom: 0.75rem;
	}

	h1 {
		margin: 0;
		font-size: 1.25rem;
	}

	h2 {
		margin: 1.25rem 0 0.35rem;
		font-size: 0.75rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.08em;
		color: #8f8f9a;
	}

	.searchbox {
		margin-bottom: 0.7rem;
	}

	.types {
		display: flex;
		gap: 6px;
		margin-bottom: 0.5rem;
	}

	.types button {
		border: none;
		background: rgba(255, 255, 255, 0.08);
		color: #d8d8de;
		font: inherit;
		font-weight: 600;
		font-size: 0.8rem;
		padding: 0.32rem 0.85rem;
		border-radius: 999px;
		cursor: pointer;
		transition:
			background 0.15s ease,
			color 0.15s ease;
	}

	.types button.active {
		background: var(--accent);
		color: #fff;
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

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
	}

	.item {
		padding: 10px;
		border-radius: 12px;
	}

	.item:hover {
		background: rgba(255, 255, 255, 0.05);
	}

	.actor-link {
		display: flex;
		align-items: center;
		gap: 12px;
		text-decoration: none;
		color: inherit;
		min-width: 0;
	}

	.avatar {
		width: 42px;
		height: 42px;
		border-radius: 50%;
		object-fit: cover;
		flex-shrink: 0;
	}

	.fallback {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		background: var(--accent);
		color: #fff;
		font-weight: 700;
		font-size: 1.05rem;
	}

	.who {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.name {
		font-weight: 600;
		font-size: 0.92rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.handle {
		color: #8f8f9a;
		font-size: 0.82rem;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.chips {
		display: flex;
		flex-wrap: wrap;
		gap: 8px;
		padding-top: 2px;
	}

	.chip {
		display: inline-block;
		background: rgba(255, 77, 109, 0.12);
		color: #ff8fa3;
		font-weight: 600;
		font-size: 0.88rem;
		padding: 0.4rem 0.9rem;
		border-radius: 999px;
		text-decoration: none;
		transition: background 0.15s ease;
	}

	.chip:hover {
		background: rgba(255, 77, 109, 0.24);
	}

	.clip-body {
		min-width: 0;
	}

	.clip-link {
		display: block;
		text-decoration: none;
		color: inherit;
		min-width: 0;
	}

	.caption {
		margin: 0;
		font-size: 0.9rem;
		line-height: 1.45;
		overflow-wrap: anywhere;
	}

	.meta {
		display: flex;
		gap: 10px;
		margin: 3px 0 0;
		color: #8f8f9a;
		font-size: 0.78rem;
	}
</style>
