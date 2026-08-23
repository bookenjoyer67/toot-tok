<script lang="ts">
	import { apiGet, apiPost } from '$lib/api';
	import { sessionUser } from '$lib/stores';
	import type { Clip, CommentT } from '$lib/types';

	let { clip, onclose, onposted }: { clip: Clip; onclose: () => void; onposted?: () => void } =
		$props();

	let comments = $state<CommentT[]>([]);
	let loading = $state(true);
	let loadError = $state<string | null>(null);
	let draft = $state('');
	let posting = $state(false);
	let postError = $state<string | null>(null);
	let inputEl: HTMLInputElement | null = $state(null);
	let sheetEl: HTMLDivElement | null = $state(null);

	$effect(() => {
		const id = clip.id;
		comments = [];
		loadError = null;
		postError = null;
		void load(id);
	});

	$effect(() => {
		// Modal hygiene: focus lands INSIDE the dialog for keyboard/screen-reader
		// users even when logged out (no comment input exists then) — sheet
		// itself is the focus target; input focus stays a bonus when present.
		sheetEl?.focus();
	});

	$effect(() => {
		if ($sessionUser && inputEl) {
			const t = window.requestAnimationFrame(() => inputEl?.focus());
			return () => window.cancelAnimationFrame(t);
		}
	});

	$effect(() => {
		const previouslyFocused = document.activeElement;
		return () => {
			if (previouslyFocused instanceof HTMLElement) previouslyFocused.focus();
		};
	});

	async function load(id: string): Promise<void> {
		loading = true;
		try {
			const data = await apiGet<CommentT[] | { items: CommentT[] }>(`/clips/${id}/comments`);
			comments = Array.isArray(data) ? data : (data.items ?? []);
		} catch (err) {
			loadError = err instanceof Error ? err.message : 'Could not load comments.';
		} finally {
			loading = false;
		}
	}

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		const body = draft.trim();
		if (!body || posting) return;
		posting = true;
		postError = null;
		try {
			const created = await apiPost<CommentT>(`/clips/${clip.id}/comments`, { body });
			comments = [created, ...comments];
			draft = '';
			onposted?.();
		} catch (err) {
			postError = err instanceof Error ? err.message : 'Could not post comment.';
		} finally {
			posting = false;
		}
	}

	function timeAgo(iso: string): string {
		const s = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
		if (s < 60) return 'now';
		if (s < 3600) return `${Math.floor(s / 60)}m`;
		if (s < 86_400) return `${Math.floor(s / 3600)}h`;
		if (s < 604_800) return `${Math.floor(s / 86_400)}d`;
		return `${Math.floor(s / 604_800)}w`;
	}

	function onkeydown(event: KeyboardEvent): void {
		if (event.key === 'Escape') {
			onclose();
			return;
		}
		if (event.key !== 'Tab' || !sheetEl) return;

		const focusables = Array.from(
			sheetEl.querySelectorAll<HTMLElement>(
				'a[href], button:not([disabled]), input:not([disabled]), textarea:not([disabled]), select:not([disabled]), [tabindex]:not([tabindex="-1"])'
			)
		);
		if (focusables.length === 0) {
			event.preventDefault();
			return;
		}

		const first = focusables[0];
		const last = focusables[focusables.length - 1];
		const current = document.activeElement;
		const inside = current instanceof Node && sheetEl.contains(current);

		if (event.shiftKey) {
			if (current === first || !inside) {
				event.preventDefault();
				last.focus();
			}
		} else if (current === last || !inside) {
			event.preventDefault();
			first.focus();
		}
	}
</script>

<svelte:window onkeydown={onkeydown} />

<button class="backdrop" aria-label="Close comments" onclick={onclose}></button>

<div class="sheet" role="dialog" aria-modal="true" aria-label="Comments for @{clip.author.username}" tabindex="-1" bind:this={sheetEl}>
	<header>
		<span class="grabber"></span>
		<h2>Comments</h2>
		<button class="icon-btn close" aria-label="Close" onclick={onclose}>
			<svg
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
			>
				<line x1="18" y1="6" x2="6" y2="18" />
				<line x1="6" y1="6" x2="18" y2="18" />
			</svg>
		</button>
	</header>

	<div class="list">
		{#if loading}
			<div class="center"><span class="spinner"></span></div>
		{:else if loadError}
			<div class="center error">{loadError}</div>
		{:else if comments.length === 0}
			<div class="center muted">No comments yet. Say something nice.</div>
		{:else}
			{#each comments as c (c.id)}
				<article class="comment">
					{#if c.author.avatar_path}
						<img class="avatar" src={c.author.avatar_path} alt="" />
					{:else}
						<span class="avatar fallback"
							>{(c.author.display_name || c.author.username).charAt(0).toUpperCase()}</span
						>
					{/if}
					<div class="meta">
						<p class="who">
							<strong>@{c.author.username}</strong>
							<span class="when">{timeAgo(c.created_at)}</span>
						</p>
						<p class="body">{c.body}</p>
					</div>
				</article>
			{/each}
		{/if}
	</div>

	<footer>
		{#if $sessionUser}
			<form onsubmit={(e) => void submit(e)}>
				<label class="sr-only" for="comment-body">Add a comment</label>
				<input
					id="comment-body"
					bind:this={inputEl}
					bind:value={draft}
					placeholder="Add a comment…"
					maxlength="500"
				/>
				<button class="btn" type="submit" disabled={posting || !draft.trim()}>
					{posting ? '…' : 'Post'}
				</button>
			</form>
			{#if postError}<p class="error">{postError}</p>{/if}
		{:else}
			<a class="btn ghost login" href="/login">Log in to comment</a>
		{/if}
	</footer>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		z-index: 50;
		border: none;
		padding: 0;
		background: rgba(0, 0, 0, 0.45);
		cursor: default;
	}

	header {
		position: relative;
		display: grid;
		place-items: center;
		padding: 10px 0 6px;
		border-bottom: 1px solid rgba(255, 255, 255, 0.08);
		flex-shrink: 0;
	}

	.grabber {
		position: absolute;
		top: 6px;
		left: 50%;
		transform: translateX(-50%);
		width: 40px;
		height: 4px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.25);
	}

	h2 {
		margin: 4px 0 0;
		font-size: 0.95rem;
	}

	.close {
		position: absolute;
		right: 8px;
		top: 6px;
		width: 36px;
		height: 36px;
		color: #cfcfd6;
	}

	.close svg {
		width: 22px;
		height: 22px;
	}

	.list {
		flex: 1;
		overflow-y: auto;
		padding: 10px 16px;
		min-height: 120px;
	}

	.center {
		padding: 28px 0;
		text-align: center;
		color: #b9b9c3;
		font-size: 0.9rem;
	}

	.comment {
		display: flex;
		gap: 10px;
		padding: 8px 0;
	}

	.avatar {
		width: 34px;
		height: 34px;
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
		font-size: 0.85rem;
	}

	.meta {
		min-width: 0;
	}

	.who {
		margin: 0;
		font-size: 0.82rem;
		display: flex;
		gap: 8px;
		align-items: baseline;
	}

	.when {
		color: #8f8f9a;
		font-size: 0.75rem;
	}

	.body {
		margin: 2px 0 0;
		font-size: 0.88rem;
		line-height: 1.4;
		color: #e6e6ec;
		overflow-wrap: anywhere;
	}

	footer {
		flex-shrink: 0;
		padding: 10px 14px calc(var(--safe-bottom) + 10px);
		border-top: 1px solid rgba(255, 255, 255, 0.08);
	}

	form {
		display: flex;
		gap: 8px;
	}

	input {
		flex: 1;
		min-width: 0;
		background: rgba(255, 255, 255, 0.08);
		border: 1px solid transparent;
		border-radius: 999px;
		color: var(--text);
		font: inherit;
		font-size: 0.9rem;
		padding: 0.55rem 1rem;
		outline: none;
	}

	input:focus {
		border-color: rgba(255, 255, 255, 0.25);
	}

	.login {
		width: 100%;
		justify-content: center;
		text-decoration: none;
	}

	.error {
		margin: 6px 0 0;
		color: #ff8298;
		font-size: 0.8rem;
	}
</style>
