<script lang="ts">
	import { apiPost } from '$lib/api';
	import { showToast } from '$lib/toast';
	import type { Clip } from '$lib/types';

	let {
		clip,
		onclose
	}: {
		clip: Clip;
		onclose: () => void;
	} = $props();

	let reporting = $state(false);
	let reportBody = $state('');
	const permalink = $derived(clip.ap_id || `${location.origin}/clips/${clip.id}`);

	async function nativeShare(): Promise<void> {
		if (!navigator.share) return;
		try {
			await navigator.share({ title: `Clip by @${clip.author.username}`, url: permalink });
			onclose();
		} catch {
			/* user dismissed */
		}
	}

	async function copyLink(): Promise<void> {
		try {
			await navigator.clipboard.writeText(permalink);
			showToast('Copied!');
			onclose();
		} catch {
			showToast('Could not copy link');
		}
	}

	async function submitReport(): Promise<void> {
		try {
			await apiPost('/reports', {
				target_type: 'clip',
				target_id: Number(clip.id),
				category: 'clip',
				body: reportBody || null
			});
			showToast('Report sent');
			reporting = false;
			onclose();
		} catch (err) {
			showToast(err instanceof Error ? err.message : 'Report failed');
		}
	}
</script>

<svelte:window onkeydown={(e) => e.key === 'Escape' && onclose()} />

<div
	class="backdrop"
	role="presentation"
	onclick={(e) => e.target === e.currentTarget && onclose()}
>
	<div class="sheet" role="dialog" aria-label={`Share clip by @${clip.author.username}`}>
		{#if !reporting}
			<button class="row" onclick={() => void nativeShare()} disabled={!navigator.share}>
				<span class="emoji">📤</span> Share…
			</button>
			<button class="row" onclick={() => void copyLink()}>
				<span class="emoji">🔗</span> Copy link
			</button>
			<a class="row" href="/clips/{clip.id}">
				<span class="emoji">🎬</span> Open clip page
			</a>
			<button class="row danger" onclick={() => (reporting = true)}>
				<span class="emoji">🚩</span> Report clip
			</button>
			<button class="row cancel" onclick={onclose}>Cancel</button>
		{:else}
			<p class="rep-head">Why are you reporting this clip?</p>
			<textarea
				bind:value={reportBody}
				rows="3"
				maxlength="2000"
				placeholder="Optional details…"
			></textarea>
			<button class="row danger" onclick={() => void submitReport()}>Send report</button>
			<button class="row cancel" onclick={() => (reporting = false)}>Back</button>
		{/if}
	</div>
</div>

<style>
	.backdrop {
		position: fixed;
		inset: 0;
		z-index: 90;
		background: rgba(0, 0, 0, 0.55);
		display: flex;
		align-items: flex-end;
		justify-content: center;
	}

	.sheet {
		width: min(100%, 420px);
		background: var(--surface, #16161d);
		border-radius: 16px 16px 0 0;
		padding: 10px calc(12px + var(--safe-right, 0px)) calc(var(--safe-bottom, 0px) + 12px) 12px;
		display: flex;
		flex-direction: column;
		gap: 4px;
		box-shadow: 0 -8px 40px rgba(0, 0, 0, 0.5);
	}

	.row {
		display: flex;
		align-items: center;
		gap: 10px;
		width: 100%;
		text-align: left;
		border: none;
		background: transparent;
		color: var(--text, #eee);
		font: inherit;
		font-size: 0.95rem;
		padding: 0.75rem 0.6rem;
		border-radius: 10px;
		cursor: pointer;
		text-decoration: none;
	}

	.row:hover:not(:disabled) {
		background: rgba(255, 255, 255, 0.07);
	}

	.row:disabled {
		opacity: 0.45;
		cursor: default;
	}

	.emoji {
		width: 1.4em;
		text-align: center;
	}

	.danger {
		color: #ff8298;
	}

	.cancel {
		border-top: 1px solid rgba(255, 255, 255, 0.08);
		margin-top: 4px;
		justify-content: center;
		color: #9a9aa5;
	}

	.rep-head {
		margin: 0.5rem 0 0.25rem;
		font-weight: 700;
		font-size: 0.95rem;
	}

	textarea {
		resize: vertical;
		background: rgba(255, 255, 255, 0.05);
		border: 1px solid rgba(255, 255, 255, 0.12);
		border-radius: 10px;
		color: inherit;
		font: inherit;
		padding: 0.6rem;
		margin-bottom: 4px;
	}
</style>
