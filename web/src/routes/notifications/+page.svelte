<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { apiGet, apiPut, setCsrf } from '$lib/api';
	import { sessionUser, unreadCount } from '$lib/stores';
	import { timeAgo } from '$lib/time';
	import EmptyState from '$lib/components/EmptyState.svelte';
	import type { NotificationT } from '$lib/types';

	interface MeResponse {
		username: string;
		is_admin?: boolean;
		csrf?: string;
	}

	let booted = $state(false);
	let items = $state<NotificationT[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let marking = $state(false);

	const unread = $derived(items.filter((n) => !n.read).length);

	onMount(() => {
		void boot();
	});

	async function boot(): Promise<void> {
		if (!$sessionUser) {
			try {
				const me = await apiGet<MeResponse>('/accounts/me');
				if (me?.username) {
					if (me.csrf) setCsrf(me.csrf);
					sessionUser.set({
						username: me.username,
						csrf: me.csrf ?? '',
						isAdmin: me.is_admin ?? false
					});
				}
			} catch {
				sessionUser.set(null);
			}
		}
		booted = true;
		if ($sessionUser) await load();
		else loading = false;
	}

	$effect(() => {
		if (booted && !$sessionUser) void goto('/login');
	});

	async function load(): Promise<void> {
		loading = true;
		loadError = null;
		try {
			const data = await apiGet<NotificationT[] | { items: NotificationT[] }>('/notifications');
			items = Array.isArray(data) ? data : (data.items ?? []);
			unreadCount.set(items.filter((n) => !n.read).length);
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Could not load notifications.';
		} finally {
			loading = false;
		}
	}

	async function markAllRead(): Promise<void> {
		if (marking || unread === 0) return;
		marking = true;
		try {
			await apiPut('/notifications/read', {});
			items = items.map((n) => ({ ...n, read: true }));
			unreadCount.set(0);
		} catch {
			/* leave state as-is */
		} finally {
			marking = false;
		}
	}
</script>

<svelte:head>
	<title>Notifications · TootTok</title>
</svelte:head>

{#if $sessionUser}
	<div class="page safe-area">
		<header class="head">
			<h1>Notifications</h1>
			<button
				class="mark"
				onclick={markAllRead}
				disabled={marking || unread === 0 || loading}
			>
				{marking ? '…' : 'Mark all read'}
			</button>
		</header>

		{#if loading}
			<div class="center"><span class="spinner"></span></div>
		{:else if loadError}
			<div class="center error">{loadError}</div>
		{:else if items.length === 0}
			<EmptyState heading="Nothing here yet" message="Likes, comments and boosts will show up here." />
		{:else}
			<ul class="list">
				{#each items as n (n.id)}
					<li class="item" class:unread={!n.read}>
						<span class="icon" class:accent={n.type === 'like'}>
							{#if n.type === 'like'}
								<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
									<path d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z" />
								</svg>
							{:else if n.type === 'comment' || n.type === 'mention'}
								<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
									<path d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z" />
								</svg>
							{:else if n.type === 'boost'}
								<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
									<polyline points="17 1 21 5 17 9" />
									<path d="M3 11V9a4 4 0 0 1 4-4h14" />
									<polyline points="7 23 3 19 7 15" />
									<path d="M21 13v2a4 4 0 0 1-4 4H3" />
								</svg>
							{:else}
								<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
									<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
									<circle cx="12" cy="7" r="4" />
								</svg>
							{/if}
						</span>
						<div class="body">
							<p class="line">
								<strong>@{n.actor.username}</strong>
								{#if n.type === 'like'}liked your clip{:else if n.type === 'comment'}commented on your clip{:else if n.type === 'boost'}boosted your clip{:else if n.type === 'follow'}followed you{:else}mentioned you{/if}
							</p>
							{#if n.body}
								<p class="preview">{n.body}</p>
							{/if}
						</div>
						<span class="when">{timeAgo(n.created_at)}</span>
					</li>
				{/each}
			</ul>
		{/if}
	</div>
{:else if booted}
	<div class="page safe-area gate">
		<EmptyState heading="You're not logged in" message="Log in to see your notifications.">
			<a href="/login" class="btn">Log in</a>
		</EmptyState>
	</div>
{:else}
	<div class="page safe-area gate">
		<span class="spinner"></span>
	</div>
{/if}

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

	.mark {
		border: none;
		background: transparent;
		color: var(--accent);
		font: inherit;
		font-size: 0.85rem;
		font-weight: 600;
		cursor: pointer;
		padding: 0.35rem 0.5rem;
		border-radius: 8px;
	}

	.mark:hover:not(:disabled) {
		background: rgba(255, 77, 109, 0.1);
	}

	.mark:disabled {
		color: #55555e;
		cursor: default;
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
		display: flex;
		align-items: flex-start;
		gap: 12px;
		padding: 12px 10px;
		border-radius: 12px;
	}

	.item.unread {
		background: rgba(255, 77, 109, 0.07);
	}

	.icon {
		display: grid;
		place-items: center;
		width: 36px;
		height: 36px;
		flex-shrink: 0;
		border-radius: 50%;
		background: rgba(255, 255, 255, 0.08);
		color: #cfcfd6;
	}

	.icon.accent {
		color: var(--accent);
	}

	.icon svg {
		width: 18px;
		height: 18px;
	}

	.body {
		flex: 1;
		min-width: 0;
	}

	.line {
		margin: 0;
		font-size: 0.88rem;
		line-height: 1.4;
	}

	.preview {
		margin: 2px 0 0;
		font-size: 0.82rem;
		color: #b9b9c3;
		overflow-wrap: anywhere;
	}

	.when {
		color: #8f8f9a;
		font-size: 0.75rem;
		flex-shrink: 0;
		padding-top: 2px;
	}

	.gate {
		display: grid;
		place-items: center;
	}
</style>
