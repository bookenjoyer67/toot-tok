<script lang="ts">
	import { page } from '$app/state';
	import { goto } from '$app/navigation';
	import { apiGet, apiPost, csrf } from '$lib/api';
	import { resetFeeds, sessionUser, unreadCount } from '$lib/stores';
	import type { NotificationT } from '$lib/types';

	interface NotificationsResponse {
		items?: NotificationT[];
	}

	let menuOpen = $state(false);

	const pathname: string = $derived(page.url.pathname);
	const hidden = $derived(
		pathname === '/login' || pathname === '/register' || pathname.startsWith('/upload')
	);
	const user = $derived($sessionUser);
	const unread = $derived($unreadCount);

	const profileHref = $derived(user ? `/profile/${user.username}` : '/login');
	const onFeed = $derived(pathname === '/');
	const onUpload = $derived(pathname.startsWith('/upload'));
	const onNotifs = $derived(pathname.startsWith('/notifications'));
	const onProfile = $derived(pathname.startsWith('/profile'));

	$effect(() => {
		if (!user) {
			unreadCount.set(0);
			return;
		}
		void refreshUnread();
		window.addEventListener('focus', refreshUnread);
		document.addEventListener('visibilitychange', refreshUnread);
		const interval = window.setInterval(refreshUnread, 60000);
		return () => {
			window.removeEventListener('focus', refreshUnread);
			document.removeEventListener('visibilitychange', refreshUnread);
			window.clearInterval(interval);
		};
	});

	async function refreshUnread(): Promise<void> {
		try {
			const data = await apiGet<NotificationT[] | NotificationsResponse>('/notifications');
			const items = Array.isArray(data) ? data : (data.items ?? []);
			unreadCount.set(items.filter((n) => !n.read).length);
		} catch {
			/* badge is best-effort */
		}
	}

	function toggleMenu(): void {
		menuOpen = !menuOpen;
	}

	function closeMenu(): void {
		menuOpen = false;
	}

	function onWindowClick(event: MouseEvent): void {
		if (!menuOpen) return;
		const target = event.target as Element | null;
		if (target?.closest('.menu-wrap')) return;
		menuOpen = false;
	}

	async function logout(): Promise<void> {
		menuOpen = false;
		if (!confirm('Log out?')) return;
		try {
			await apiPost('/auth/logout');
		} catch {
			/* clear local state regardless */
		}
		csrf.token = null;
		sessionUser.set(null);
		resetFeeds();
		unreadCount.set(0);
		await goto('/login');
	}
</script>

<svelte:window onclick={onWindowClick} />

{#if !hidden}
	<header class="topbar">
		{#if user}
			<div class="menu-wrap">
				<button
					class="user-btn"
					class:open={menuOpen}
					aria-haspopup="menu"
					aria-expanded={menuOpen}
					onclick={toggleMenu}
				>
					@{user.username}
					<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5" stroke-linecap="round" aria-hidden="true">
						<polyline points="6 9 12 15 18 9" />
					</svg>
				</button>
				{#if menuOpen}
					<div class="menu" role="menu">
						<a role="menuitem" href={profileHref} onclick={closeMenu}>Profile</a>
						<button role="menuitem" onclick={logout}>Log out</button>
					</div>
				{/if}
			</div>
		{:else}
			<a class="btn login-btn" href="/login">Log in</a>
		{/if}
	</header>

	<nav class="tabbar" aria-label="Primary">
		<a href="/" class="tab" class:active={onFeed} aria-label="Feed" aria-current={onFeed ? 'page' : undefined}>
			<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
				<polyline points="9 22 9 12 15 12 15 22" />
			</svg>
			<span>Feed</span>
		</a>

		<a href="/upload" class="tab compose" class:active={onUpload} aria-label="Upload" aria-current={onUpload ? 'page' : undefined}>
			<span class="plus-circle">
				<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
					<line x1="12" y1="5" x2="12" y2="19" />
					<line x1="5" y1="12" x2="19" y2="12" />
				</svg>
			</span>
			<span>Upload</span>
		</a>

		<a href="/notifications" class="tab" class:active={onNotifs} aria-label="Notifications" aria-current={onNotifs ? 'page' : undefined}>
			<span class="bell">
				<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
					<path d="M18 8A6 6 0 0 0 6 8c0 7-3 9-3 9h18s-3-2-3-9" />
					<path d="M13.73 21a2 2 0 0 1-3.46 0" />
				</svg>
				{#if unread > 0}
					<span class="badge">{unread > 99 ? '99+' : unread}</span>
				{/if}
			</span>
			<span>Alerts</span>
		</a>

		<a href={profileHref} class="tab" class:active={onProfile} aria-label="Profile" aria-current={onProfile ? 'page' : undefined}>
			<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M20 21v-2a4 4 0 0 0-4-4H8a4 4 0 0 0-4 4v2" />
				<circle cx="12" cy="7" r="4" />
			</svg>
			<span>Profile</span>
		</a>
	</nav>
{/if}

<style>
	.topbar {
		position: fixed;
		top: calc(var(--safe-top) + 10px);
		right: calc(var(--safe-right) + 12px);
		z-index: 45;
		display: flex;
		justify-content: flex-end;
	}

	.login-btn {
		padding: 0.4rem 1rem;
		font-size: 0.85rem;
		box-shadow: 0 4px 16px rgba(0, 0, 0, 0.35);
	}

	.menu-wrap {
		position: relative;
	}

	.user-btn {
		display: inline-flex;
		align-items: center;
		gap: 4px;
		border: none;
		border-radius: 999px;
		background: rgba(15, 15, 20, 0.55);
		color: var(--text);
		font: inherit;
		font-weight: 600;
		font-size: 0.85rem;
		padding: 0.42rem 0.85rem;
		cursor: pointer;
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
	}

	.user-btn svg {
		width: 14px;
		height: 14px;
		transition: transform 0.15s ease;
	}

	.user-btn.open svg {
		transform: rotate(180deg);
	}

	.menu {
		position: absolute;
		top: calc(100% + 8px);
		right: 0;
		min-width: 150px;
		display: flex;
		flex-direction: column;
		background: var(--surface);
		border: 1px solid rgba(255, 255, 255, 0.08);
		border-radius: 12px;
		padding: 6px;
		box-shadow: 0 12px 32px rgba(0, 0, 0, 0.5);
	}

	.menu a,
	.menu button {
		display: block;
		width: 100%;
		text-align: left;
		border: none;
		background: transparent;
		color: var(--text);
		font: inherit;
		font-size: 0.88rem;
		padding: 0.55rem 0.7rem;
		border-radius: 8px;
		text-decoration: none;
		cursor: pointer;
	}

	.menu a:hover,
	.menu button:hover {
		background: rgba(255, 255, 255, 0.08);
	}

	.menu button {
		color: #ff8298;
	}

	.tabbar {
		position: fixed;
		left: 0;
		right: 0;
		bottom: 0;
		z-index: 55;
		display: flex;
		align-items: stretch;
		justify-content: space-around;
		background: rgba(13, 13, 18, 0.92);
		backdrop-filter: blur(12px);
		-webkit-backdrop-filter: blur(12px);
		border-top: 1px solid rgba(255, 255, 255, 0.07);
		padding-bottom: var(--safe-bottom);
	}

	.tab {
		flex: 1;
		max-width: 120px;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 2px;
		padding: 7px 0 6px;
		color: #8f8f9a;
		font-size: 0.68rem;
		font-weight: 600;
		text-decoration: none;
		transition: color 0.15s ease;
	}

	.tab.active {
		color: var(--text);
	}

	.tab svg {
		width: 23px;
		height: 23px;
		display: block;
	}

	.compose .plus-circle {
		display: grid;
		place-items: center;
		width: 44px;
		height: 30px;
		margin-top: -8px;
		border-radius: 12px;
		background: linear-gradient(135deg, var(--accent), #ff7a59);
		color: #fff;
		box-shadow: 0 4px 14px rgba(255, 77, 109, 0.45);
		transition: transform 0.15s ease;
	}

	.compose .plus-circle svg {
		width: 18px;
		height: 18px;
	}

	.compose.active .plus-circle {
		transform: scale(1.06);
	}

	.bell {
		position: relative;
		display: inline-flex;
	}

	.badge {
		position: absolute;
		top: -5px;
		right: -9px;
		min-width: 17px;
		height: 17px;
		display: grid;
		place-items: center;
		padding: 0 4px;
		border-radius: 999px;
		background: var(--accent);
		color: #fff;
		font-size: 0.62rem;
		font-weight: 700;
		line-height: 1;
	}
</style>
