<script lang="ts">
	import { goto } from '$app/navigation';
	import { apiPost, setCsrf } from '$lib/api';
	import { sessionUser } from '$lib/stores';

	interface AuthResponse {
		username?: string;
		user?: { username?: string };
		csrf_token?: string;
	}

	const USERNAME_RE = /^[a-z0-9_]{3,30}$/;

	let username = $state('');
	let email = $state('');
	let password = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);

	const usernameValid = $derived(USERNAME_RE.test(username));
	const passwordValid = $derived(password.length >= 10);
	const formValid = $derived(usernameValid && passwordValid);

	async function submit(event: SubmitEvent): Promise<void> {
		event.preventDefault();
		if (busy || !formValid) return;
		busy = true;
		error = null;
		try {
			const trimmedEmail = email.trim();
			await apiPost<unknown>('/auth/register', {
				username: username.trim(),
				password,
				...(trimmedEmail ? { email: trimmedEmail } : {})
			});

			// auto-login after successful registration
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
			error = err instanceof Error ? err.message : 'Could not create your account.';
		} finally {
			busy = false;
		}
	}
</script>

<svelte:head>
	<title>Create account · TootTok</title>
</svelte:head>

<div class="page safe-area">
	<form class="form-card" onsubmit={submit}>
		<h1>Create account</h1>
		<p class="sub">Join TootTok.</p>

		{#if error}
			<div class="banner error" role="alert">{error}</div>
		{/if}

		<div>
			<label class="label" for="username">Username</label>
			<input id="username" class="input" bind:value={username} autocomplete="username" required />
			{#if username && !usernameValid}
				<p class="hint">3–30 characters: lowercase letters, numbers and underscores only.</p>
			{/if}
		</div>

		<div>
			<label class="label" for="email">Email <span class="optional">(optional)</span></label>
			<input
				id="email"
				class="input"
				type="email"
				bind:value={email}
				autocomplete="email"
				placeholder="you@example.com"
			/>
		</div>

		<div>
			<label class="label" for="password">Password</label>
			<input
				id="password"
				class="input"
				type="password"
				bind:value={password}
				autocomplete="new-password"
				required
			/>
			{#if password && !passwordValid}
				<p class="hint">Use at least 10 characters.</p>
			{/if}
		</div>

		<button class="btn" type="submit" disabled={busy || !formValid}>
			{busy ? 'Creating account…' : 'Sign up'}
		</button>

		<p class="alt">Already have an account? <a href="/login">Log in</a></p>
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

	.optional {
		font-weight: 400;
		color: #77777f;
	}

	.hint {
		margin: 0.35rem 0 0;
		font-size: 0.78rem;
		color: #ffb3c0;
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
