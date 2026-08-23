<script lang="ts">
	import { onMount } from 'svelte';
	import { apiGet, apiPost, apiPut, setCsrf } from '$lib/api';
	import { applyMe, sessionUser } from '$lib/stores';
	import { timeAgo } from '$lib/time';
	import EmptyState from '$lib/components/EmptyState.svelte';

	interface MeResponse {
		username: string;
		is_admin?: boolean;
		csrf_token?: string;
	}

	interface AdminReport {
		id: string;
		state?: string;
		category?: string;
		body?: string | null;
		created_at?: string;
		reporter?: { username?: string } | string | null;
		target_summary?: string | null;
		target?: { summary?: string; username?: string } | null;
	}

	interface AdminUser {
		id: string;
		username: string;
		status?: string;
		suspended_at?: string | null;
	}

	type SettingKind = 'bool' | 'num' | 'text';

	interface SettingField {
		key: string;
		label: string;
		kind: SettingKind;
	}

	interface SettingGroup {
		name: string;
		label: string;
		fields: SettingField[];
	}

	type TabId = 'reports' | 'users' | 'settings';

	const TABS: { id: TabId; label: string }[] = [
		{ id: 'reports', label: 'Reports' },
		{ id: 'users', label: 'Users' },
		{ id: 'settings', label: 'Settings' }
	];

	const REPORT_STATES = ['open', 'resolved', 'all'] as const;

	let booted = $state(false);
	let tab = $state<TabId>('reports');

	let reports = $state<AdminReport[]>([]);
	let reportState = $state<(typeof REPORT_STATES)[number]>('open');
	let reportsLoading = $state(false);
	let reportsLoadedFor = $state<string | null>(null);
	let reportsError = $state<string | null>(null);
	let resolvingId = $state<string | null>(null);

	let users = $state<AdminUser[]>([]);
	let usersLoading = $state(false);
	let usersLoaded = $state(false);
	let usersError = $state<string | null>(null);
	let actingUserId = $state<string | null>(null);

	let settingGroups = $state<SettingGroup[]>([]);
	let textDraft = $state<Record<string, string>>({});
	let boolDraft = $state<Record<string, boolean>>({});
	let settingsLoading = $state(false);
	let settingsError = $state<string | null>(null);
	let savingGroup = $state('');
	let savedGroup = $state('');

	const user = $derived($sessionUser);
	const isAdmin = $derived(user?.isAdmin === true);

	onMount(() => {
		void boot();
	});

	async function boot(): Promise<void> {
		try {
			const me = await apiGet<MeResponse>('/accounts/me');
			if (me?.username) {
				if (me.csrf_token) setCsrf(me.csrf_token);
				applyMe(me);
			} else {
				sessionUser.set(null);
			}
		} catch {
			sessionUser.set(null);
		}
		booted = true;
		if ($sessionUser?.isAdmin) void ensureTab();
	}

	function ensureTab(): void {
		if (!$sessionUser?.isAdmin) return;
		if (tab === 'reports') void loadReports();
		else if (tab === 'users') void loadUsers();
		else void loadSettings();
	}

	function listOf<T>(data: unknown): T[] {
		if (Array.isArray(data)) return data as T[];
		if (data && typeof data === 'object') return ((data as Record<string, unknown>).items ?? []) as T[];
		return [];
	}

	async function loadReports(): Promise<void> {
		if (reportsLoadedFor === reportState && !reportsError) return;
		reportsLoading = true;
		reportsError = null;
		try {
			const data = await apiGet<unknown>(`/admin/reports?state=${reportState}`);
			reports = listOf<AdminReport>(data);
			reportsLoadedFor = reportState;
		} catch (err) {
			reportsError = err instanceof Error ? err.message : 'Could not load reports.';
		} finally {
			reportsLoading = false;
		}
	}

	function changeReportState(state: string): void {
		reportState = state as (typeof REPORT_STATES)[number];
		void loadReports();
	}

	function reporterName(report: AdminReport): string {
		const reporter = report.reporter;
		if (typeof reporter === 'string') return reporter;
		return reporter?.username ?? 'unknown';
	}

	function targetSummary(report: AdminReport): string {
		if (report.target_summary) return report.target_summary;
		const target = report.target;
		if (typeof target === 'string') return target;
		if (target?.summary) return target.summary;
		if (target?.username) return `@${target.username}`;
		return '(target unavailable)';
	}

	async function resolveReport(report: AdminReport): Promise<void> {
		const note = window.prompt(`Resolve this ${report.category ?? 'report'} — action note (optional):`);
		if (note === null) return;
		resolvingId = report.id;
		reportsError = null;
		try {
			await apiPost(`/admin/reports/${encodeURIComponent(report.id)}/resolve`, { action_note: note });
			if (reportState === 'open') {
				reports = reports.filter((r) => r.id !== report.id);
			} else {
				reports = reports.map((r) => (r.id === report.id ? { ...r, state: 'resolved' } : r));
			}
		} catch (err) {
			reportsError = err instanceof Error ? err.message : 'Could not resolve report.';
		} finally {
			resolvingId = null;
		}
	}

	async function loadUsers(): Promise<void> {
		if (usersLoaded && !usersError) return;
		usersLoading = true;
		usersError = null;
		try {
			const data = await apiGet<unknown>('/admin/users?state=all');
			users = listOf<AdminUser>(data);
			usersLoaded = true;
		} catch (err) {
			usersError = err instanceof Error ? err.message : 'Could not load users.';
		} finally {
			usersLoading = false;
		}
	}

	async function setUserStatus(target: AdminUser, action: 'suspend' | 'approve'): Promise<void> {
		const verb = action === 'suspend' ? 'Suspend' : 'Approve';
		if (!window.confirm(`${verb} @${target.username}?`)) return;
		actingUserId = target.id;
		usersError = null;
		try {
			await apiPost(`/admin/users/${encodeURIComponent(target.id)}/${action}`, {});
			users = users.map((u) =>
				u.id === target.id
					? {
							...u,
							status: action === 'suspend' ? 'suspended' : 'active',
							suspended_at: action === 'suspend' ? new Date().toISOString() : null
						}
					: u
			);
		} catch (err) {
			usersError = err instanceof Error ? err.message : `Could not ${action} user.`;
		} finally {
			actingUserId = null;
		}
	}

	async function loadSettings(): Promise<void> {
		if (settingGroups.length > 0 && !settingsError) return;
		settingsLoading = true;
		settingsError = null;
		try {
			const data = await apiGet<Record<string, unknown>>('/admin/settings');
			settingGroups = groupSettings(data ?? {});
		} catch (err) {
			settingsError = err instanceof Error ? err.message : 'Could not load settings.';
		} finally {
			settingsLoading = false;
		}
	}

	function groupSettings(raw: Record<string, unknown>): SettingGroup[] {
		const map = new Map<string, SettingField[]>();
		for (const [key, value] of Object.entries(raw)) {
			if (typeof value === 'object' && value !== null) continue;
			const kind: SettingKind =
				typeof value === 'boolean' ? 'bool' : typeof value === 'number' ? 'num' : 'text';
			textDraft[key] = value === null || value === undefined ? '' : String(value);
			if (kind === 'bool') boolDraft[key] = value === true;
			const dot = key.indexOf('.');
			const groupName = dot > 0 ? key.slice(0, dot) : 'general';
			const shortLabel = dot > 0 ? key.slice(dot + 1) : key;
			const field: SettingField = { key, label: humanize(shortLabel), kind };
			const existing = map.get(groupName);
			if (existing) existing.push(field);
			else map.set(groupName, [field]);
		}
		return Array.from(map.entries(), ([name, fields]) => ({ name, label: humanize(name), fields }));
	}

	function humanize(key: string): string {
		const cleaned = key.replace(/[_.-]+/g, ' ').trim();
		return cleaned.charAt(0).toUpperCase() + cleaned.slice(1);
	}

	async function saveGroup(group: SettingGroup): Promise<void> {
		savingGroup = group.name;
		settingsError = null;
		try {
			const payload: Record<string, string | number | boolean> = {};
			for (const field of group.fields) {
				if (field.kind === 'bool') {
					payload[field.key] = boolDraft[field.key] === true;
				} else if (field.kind === 'num') {
					const parsed = Number(textDraft[field.key]);
					payload[field.key] = Number.isNaN(parsed) ? textDraft[field.key] : parsed;
				} else {
					payload[field.key] = textDraft[field.key] ?? '';
				}
			}
			await apiPut('/admin/settings', payload);
			savedGroup = group.name;
			window.setTimeout(() => {
				if (savedGroup === group.name) savedGroup = '';
			}, 1800);
		} catch (err) {
			settingsError = err instanceof Error ? err.message : 'Could not save settings.';
		} finally {
			savingGroup = '';
		}
	}

	function switchTab(next: TabId): void {
		tab = next;
		void ensureTab();
	}
</script>

<svelte:head>
	<title>Admin · TootTok</title>
</svelte:head>

{#if !booted}
	<div class="page safe-area gate"><span class="spinner"></span></div>
{:else if !user}
	<div class="page safe-area gate">
		<EmptyState heading="You're not logged in" message="Log in to continue to the admin console.">
			<a href="/login" class="btn">Log in</a>
		</EmptyState>
	</div>
{:else if !isAdmin}
	<div class="page safe-area gate">
		<EmptyState heading="Admin access only" message="This area is reserved for moderators and admins. Your account doesn't have access.">
			<a href="/" class="btn ghost">Back to feed</a>
		</EmptyState>
	</div>
{:else}
	<div class="page safe-area">
		<header class="head">
			<h1>Admin</h1>
			<span class="who">@{user.username}</span>
		</header>

		<div class="tabs" role="tablist" aria-label="Admin sections">
			{#each TABS as t (t.id)}
				<button
					role="tab"
					aria-selected={tab === t.id}
					class:active={tab === t.id}
					onclick={() => switchTab(t.id)}
				>
					{t.label}
				</button>
			{/each}
		</div>

		{#if tab === 'reports'}
			<section aria-label="Reports">
				<div class="toolbar">
					<label class="label" for="report-state">State</label>
					<select id="report-state" class="input select" onchange={(e) => changeReportState(e.currentTarget.value)}>
						{#each REPORT_STATES as s (s)}
							<option value={s} selected={reportState === s}>{s}</option>
						{/each}
					</select>
				</div>

				{#if reportsLoading}
					<div class="center"><span class="spinner"></span></div>
				{:else if reportsError}
					<div class="center error">{reportsError}</div>
				{:else if reports.length === 0}
					<EmptyState heading="No reports" message={`There are no ${reportState} reports right now.`} />
				{:else}
					<ul class="list">
						{#each reports as r (r.id)}
							<li class="card">
								<div class="card-top">
									<strong>@{reporterName(r)}</strong>
									{#if r.category}<span class="pill">{r.category}</span>{/if}
									{#if r.created_at}<span class="when">{timeAgo(r.created_at)}</span>{/if}
								</div>
								<p class="target">{targetSummary(r)}</p>
								{#if r.body}<p class="body">{r.body}</p>{/if}
								<div class="actions">
									<button class="btn small" onclick={() => void resolveReport(r)} disabled={resolvingId !== null}>
										{resolvingId === r.id ? 'Resolving…' : 'Resolve'}
									</button>
									{#if r.state}<span class="state">{r.state}</span>{/if}
								</div>
							</li>
						{/each}
					</ul>
				{/if}
			</section>
		{:else if tab === 'users'}
			<section aria-label="Users">
				{#if usersLoading}
					<div class="center"><span class="spinner"></span></div>
				{:else if usersError}
					<div class="center error">{usersError}</div>
				{:else if users.length === 0}
					<EmptyState heading="No users" message="No accounts matched." />
				{:else}
					<ul class="list">
						{#each users as u (u.id)}
							<li class="card row-card">
								<div class="row-info">
									<strong>@{u.username}</strong>
									<span class="status-line">
										<span class="status {u.status ?? 'unknown'}">{u.status ?? 'unknown'}</span>
										{#if u.suspended_at}<span class="when">since {timeAgo(u.suspended_at)}</span>{/if}
									</span>
								</div>
								<div class="actions">
									{#if u.status === 'suspended'}
										<button
											class="btn small ghost"
											onclick={() => void setUserStatus(u, 'approve')}
											disabled={actingUserId !== null}
										>
											{actingUserId === u.id ? '…' : 'Approve'}
										</button>
									{:else}
										<button
											class="btn small danger"
											onclick={() => void setUserStatus(u, 'suspend')}
											disabled={actingUserId !== null}
										>
											{actingUserId === u.id ? '…' : 'Suspend'}
										</button>
									{/if}
								</div>
							</li>
						{/each}
					</ul>
				{/if}
			</section>
		{:else}
			<section aria-label="Settings">
				{#if settingsLoading}
					<div class="center"><span class="spinner"></span></div>
				{:else if settingsError}
					<div class="center error">{settingsError}</div>
				{:else if settingGroups.length === 0}
					<EmptyState heading="No settings" message="There are no configurable settings exposed." />
				{:else}
					{#each settingGroups as group (group.name)}
						<form class="card group" onsubmit={(e) => { e.preventDefault(); void saveGroup(group); }}>
							<h2>{group.label}</h2>
							{#each group.fields as field (field.key)}
								<div class="field">
									<label class="label" for={field.key}>{field.label}</label>
									{#if field.kind === 'bool'}
										<input
											id={field.key}
											type="checkbox"
											class="toggle"
											checked={boolDraft[field.key] === true}
											onchange={(e) => (boolDraft[field.key] = e.currentTarget.checked)}
										/>
									{:else if field.kind === 'num'}
										<input
											id={field.key}
											class="input"
											type="number"
											step="any"
											bind:value={textDraft[field.key]}
										/>
									{:else}
										<input id={field.key} class="input" type="text" bind:value={textDraft[field.key]} />
									{/if}
								</div>
							{/each}
							<div class="group-foot">
								<button class="btn small" type="submit" disabled={savingGroup !== ''}>
									{savingGroup === group.name ? 'Saving…' : 'Save'}
								</button>
								{#if savedGroup === group.name}<span class="saved">Saved</span>{/if}
							</div>
						</form>
					{/each}
				{/if}
			</section>
		{/if}
	</div>
{/if}

<style>
	.page {
		min-height: 100dvh;
		max-width: 520px;
		margin-inline: auto;
		padding: calc(var(--safe-top) + 1.25rem) 1rem calc(var(--safe-bottom) + 5.5rem);
	}

	.gate {
		display: grid;
		place-items: center;
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

	.who {
		color: #8f8f9a;
		font-size: 0.85rem;
		font-weight: 600;
	}

	.tabs {
		display: flex;
		gap: 6px;
		margin-bottom: 0.9rem;
	}

	.tabs button {
		flex: 1;
		border: none;
		background: rgba(255, 255, 255, 0.08);
		color: #d8d8de;
		font: inherit;
		font-weight: 600;
		font-size: 0.85rem;
		padding: 0.45rem 0;
		border-radius: 999px;
		cursor: pointer;
		transition:
			background 0.15s ease,
			color 0.15s ease;
	}

	.tabs button.active {
		background: var(--accent);
		color: #fff;
	}

	.toolbar {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-bottom: 0.75rem;
	}

	.toolbar .label {
		margin: 0;
	}

	.select {
		width: auto;
		min-width: 140px;
		padding: 0.4rem 0.7rem;
	}

	.center {
		display: grid;
		place-items: center;
		padding: 3.5rem 0;
	}

	.error {
		color: #ff8298;
		font-size: 0.9rem;
		text-align: center;
		padding: 2.5rem 0;
	}

	.list {
		list-style: none;
		margin: 0;
		padding: 0;
		display: flex;
		flex-direction: column;
		gap: 10px;
	}

	.card {
		background: var(--surface);
		border: 1px solid rgba(255, 255, 255, 0.06);
		border-radius: 14px;
		padding: 0.85rem 0.95rem;
	}

	.row-card {
		display: flex;
		align-items: center;
		justify-content: space-between;
		gap: 12px;
	}

	.row-info {
		display: flex;
		flex-direction: column;
		gap: 3px;
		min-width: 0;
	}

	.status-line {
		display: inline-flex;
		align-items: center;
		gap: 8px;
	}

	.status {
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		color: #b9b9c3;
	}

	.status.suspended {
		color: #ff8298;
	}

	.status.active {
		color: #6ee7a0;
	}

	.when {
		color: #8f8f9a;
		font-size: 0.75rem;
	}

	.card-top {
		display: flex;
		align-items: center;
		gap: 8px;
		flex-wrap: wrap;
	}

	.pill {
		background: rgba(255, 77, 109, 0.14);
		color: #ff8fa3;
		font-size: 0.72rem;
		font-weight: 700;
		text-transform: uppercase;
		letter-spacing: 0.05em;
		padding: 0.14rem 0.55rem;
		border-radius: 999px;
	}

	.target {
		margin: 0.45rem 0 0;
		font-size: 0.88rem;
		color: #d8d8de;
		overflow-wrap: anywhere;
	}

	.body {
		margin: 0.3rem 0 0;
		font-size: 0.82rem;
		color: #b9b9c3;
		line-height: 1.45;
		overflow-wrap: anywhere;
	}

	.actions {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-top: 0.65rem;
	}

	.state {
		color: #8f8f9a;
		font-size: 0.78rem;
		text-transform: capitalize;
	}

	.btn.small {
		padding: 0.38rem 0.95rem;
		font-size: 0.82rem;
	}

	.btn.danger {
		background: rgba(255, 77, 109, 0.16);
		color: #ff8298;
	}

	.group {
		margin-bottom: 12px;
	}

	.group h2 {
		margin: 0 0 0.75rem;
		font-size: 1rem;
	}

	.field {
		margin-bottom: 0.7rem;
	}

	.toggle {
		width: 20px;
		height: 20px;
		accent-color: var(--accent);
		cursor: pointer;
	}

	.group-foot {
		display: flex;
		align-items: center;
		gap: 10px;
		margin-top: 0.35rem;
	}

	.saved {
		color: #6ee7a0;
		font-size: 0.82rem;
		font-weight: 600;
	}
</style>
