<script lang="ts">
	import { apiPost } from '$lib/api';
	import { followingMap, sessionUser } from '$lib/stores';

	let {
		username,
		actorId,
		domain = null,
		size = 'sm'
	}: {
		username: string;
		/** Author actor id when known (feed payloads) — avoids the state lookup. */
		actorId?: number;
		domain?: string | null;
		size?: 'sm' | 'lg';
	} = $props();

	let busy = $state(false);
	const me = $derived($sessionUser);
	const isSelf = $derived(me?.username === username && !domain);
	const mapEntry = $derived($followingMap.get(username));
	const isFollowing = $derived(mapEntry !== undefined);

	async function follow(): Promise<void> {
		if (!me) return;
		// Preferred: the actor row id from feed payloads — exact, no guessing.
		// Fallback (search results etc): construct a handle URI.
		const body =
			actorId && actorId > 0
				? { target_actor_id: actorId }
				: {
						actor_uri:
							domain && domain !== 'local'
								? `https://${domain}/ap/users/${encodeURIComponent(username)}`
								: `${location.origin}/users/${encodeURIComponent(username)}`
					};
		const res = await apiPost<{ target_actor_id?: number }>('/follows', body);
		const id = res.target_actor_id ?? actorId ?? 0;
		followingMap.update((m) => {
			const next = new Map(m);
			next.set(username, { actorId: id, domain });
			return next;
		});
	}

	async function unfollow(): Promise<void> {
		const id = mapEntry?.actorId ?? actorId;
		if (!id) return;
		await apiPost(`/follows/${id}/unfollow`);
		followingMap.update((m) => {
			const next = new Map(m);
			next.delete(username);
			return next;
		});
	}

	async function toggle(): Promise<void> {
		if (busy || isSelf || !me) return;
		busy = true;
		try {
			if (isFollowing) await unfollow();
			else await follow();
		} catch {
			// revert on failure: refetch authoritative list
			await hydrateFollowingSafe();
		} finally {
			busy = false;
		}
	}

	async function hydrateFollowingSafe(): Promise<void> {
		const mod = await import('$lib/stores');
		await mod.hydrateFollowing();
	}
</script>

{#if !isSelf}
	<button
		class="follow"
		class:on={isFollowing}
		class:lg={size === 'lg'}
		onclick={toggle}
		disabled={busy}
	>
		{#if size === 'lg'}
			{isFollowing ? 'Following' : 'Follow'}
		{:else if isFollowing}
			✓
		{:else}
			+
		{/if}
	</button>
{/if}

<style>
	.follow {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		border: none;
		border-radius: 999px;
		background: var(--accent);
		color: #fff;
		font-weight: 700;
		font-size: 0.8rem;
		padding: 0.3rem 0.9rem;
		cursor: pointer;
		transition:
			background 0.15s ease,
			transform 0.12s ease;
	}

	.follow:active {
		transform: scale(0.94);
	}

	.follow.on {
		background: rgba(255, 255, 255, 0.22);
		color: #fff;
	}

	.follow.lg {
		font-size: 0.95rem;
		padding: 0.55rem 2rem;
		min-width: 130px;
	}

	.follow:disabled {
		opacity: 0.6;
		cursor: default;
	}
</style>
