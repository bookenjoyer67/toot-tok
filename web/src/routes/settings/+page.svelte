<script lang="ts">
	import { goto } from '$app/navigation';
	import { apiPost, csrf } from '$lib/api';
	import { clearFollowing, prefs, resetFeeds, savePrefs, sessionUser } from '$lib/stores';

	let settings = $derived(prefs);

	async function logout(): Promise<void> {
		if (!confirm('Log out?')) return;
		try {
			await apiPost('/auth/logout');
		} catch {
			/* clear local state regardless */
		}
		csrf.token = null;
		sessionUser.set(null);
		clearFollowing();
		resetFeeds();
		await goto('/login');
	}
</script>

<svelte:head>
	<title>Settings · TootTok</title>
</svelte:head>

<div class="page safe-area">
	<h1>Settings</h1>

	<section aria-label="Playback">
		<h2>Playback</h2>

		<label class="row">
			<span>
				<strong>Autoplay</strong>
				<small>Start playing clips automatically in the feed</small>
			</span>
			<input
				type="checkbox"
				bind:checked={$settings.autoplay}
				onchange={() => savePrefs({ ...$settings, autoplay: !$settings.autoplay })}
			/>
		</label>

		<label class="row">
			<span>
				<strong>Start muted</strong>
				<small>New clips begin with sound off (required by browsers anyway)</small>
			</span>
			<input
				type="checkbox"
				bind:checked={$settings.defaultMuted}
				onchange={() => savePrefs({ ...$settings, defaultMuted: !$settings.defaultMuted })}
			/>
		</label>

		<label class="row">
			<span>
				<strong>Data saver</strong>
				<small>Show posters instead of playing video while scrolling</small>
			</span>
			<input
				type="checkbox"
				bind:checked={$settings.dataSaver}
				onchange={() => savePrefs({ ...$settings, dataSaver: !$settings.dataSaver })}
			/>
		</label>
	</section>

	<section aria-label="Account">
		<h2>Account</h2>
		<button class="danger" onclick={() => void logout()}>Log out</button>
	</section>

	<p class="note">Preferences are stored on this device only.</p>
</div>

<style>
	.page {
		min-height: 100dvh;
		max-width: 560px;
		margin-inline: auto;
		padding: calc(var(--safe-top) + 1.25rem) 1rem calc(var(--safe-bottom) + 5.5rem);
	}

	h1 {
		margin: 0 0 0.75rem;
		font-size: 1.4rem;
	}

	h2 {
		margin: 1.25rem 0 0.6rem;
		font-size: 0.95rem;
		color: #9a9aa5;
		text-transform: uppercase;
		letter-spacing: 0.04em;
	}

	.row {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 1rem;
		padding: 0.85rem 0;
		border-bottom: 1px solid rgba(255, 255, 255, 0.07);
		cursor: pointer;
	}

	.row span {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.row small {
		color: #9a9aa5;
		font-size: 0.78rem;
	}

	input[type='checkbox'] {
		width: 20px;
		height: 20px;
		accent-color: var(--accent);
		flex-shrink: 0;
	}

	.danger {
		width: 100%;
		border: none;
		border-radius: 12px;
		background: rgba(255, 130, 152, 0.12);
		color: #ff8298;
		font: inherit;
		font-weight: 700;
		padding: 0.8rem;
		cursor: pointer;
		margin-top: 0.25rem;
	}

	.note {
		margin-top: 1.5rem;
		color: #77777f;
		font-size: 0.78rem;
		text-align: center;
	}
</style>
