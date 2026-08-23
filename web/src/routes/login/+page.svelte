<script lang="ts">
	import { goto } from '$app/navigation';
	import { apiPost, setCsrf } from '$lib/api';
	import { sessionUser } from '$lib/stores';

	interface AuthResponse {
		username?: string;
		user?: { username?: string };
		csrf_token?: string;
	}

	let username = $state('');
	let password = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (busy) return;
		busy = true;
		error = null;
		try {
			const res = await apiPost<AuthResponse>('/auth/login', {
				username_or_email: username.trim(),
				password
			});
			const token = res.csrf_token ?? '';
			if (token) setCsrf(token);
			sessionUser.set({
				username: res.username ?? res.user?.username ?? username.trim(),
				csrf: token
			});
			await goto('/');
		} catch (err) {
			error = err instanceof Error ? err.message : 'Could not log you in.';
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head>
	<title>Log in · TootTok</title>
</svelte:head>

<div class="page safe-area">
	<form class="form-card" onsubmit={submit}>
		<h1>Welcome back</h1>
		<p class="sub">Log in to TootTok.</p>

		{#if error}
			<div class="banner error" role="alert">{error}</div>
		{/if}

		<div>
			<label class="label" for="username">Username or email</label>
			<input id="username" class="input" bind:value={username} autocomplete="username" required />
		</div>

		<div>
			<label class="label" for="password">Password</label>
			<input
				id="password"
				class="input"
				type="password"
				bind:value={password}
				autocomplete="current-password"
				required
			/>
		</div>

		<button class="btn" type="submit" disabled={busy}>
			{busy ? 'Logging in…' : 'Log in'}
		</button>

		<p class="alt">Need an account? <a href="/register">Register</a></p>
	</form>
</div>

<style>
	.page {
		min-height: 100dvh;
		display: grid;
		place-items: center;
		padding: 1rem;
	}

	h1 {
		margin: 0;
		font-size: 1.35rem;
	}

	.sub {
		margin: -0.5rem 0 0;
		color: #8f8f9a;
		font-size: 0.88rem;
	}

	.alt {
		margin: 0;
		text-align: center;
		font-size: 0.85rem;
		color: #8f8f9a;
	}

	.alt a {
		color: var(--accent);
		font-weight: 600;
		text-decoration: none;
	}
</style>
