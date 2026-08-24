import { writable } from 'svelte/store';
import { apiGet } from './api';
import type { Clip } from './types';

export interface SessionUser {
	username: string;
	csrf: string;
	isAdmin?: boolean;
}

export interface FollowedHandle {
	/** Numeric actor id — needed by the unfollow endpoint. */
	actorId: number;
	domain: string | null;
}

export interface MePayload {
	username: string;
	// Backend /accounts/me returns the token under `csrf_token`.
	csrf_token?: string;
	is_admin?: boolean;
}

export function applyMe(me: MePayload): void {
	sessionUser.set({
		username: me.username,
		csrf: me.csrf_token ?? '',
		isAdmin: me.is_admin ?? false
	});
}

export type FeedKind = 'following' | 'discover';

export interface FeedState {
	items: Clip[];
	nextCursor: string | null;
	loaded: boolean;
}

function emptyFeed(): FeedState {
	return { items: [], nextCursor: null, loaded: false };
}

export const sessionUser = writable<SessionUser | null>(null);

export const unreadCount = writable(0);

/** username → { actorId, domain } for every accepted follow of the logged-in user. */
export const followingMap = writable<Map<string, FollowedHandle>>(new Map());

/** Hydrate the follow map once per login. Best-effort: failure leaves it empty. */
export async function hydrateFollowing(): Promise<void> {
	try {
		const data = await apiGet<{ following?: { actor_id: number; username: string; domain: string | null }[] }>(
			'/follows/mine'
		);
		const map = new Map<string, FollowedHandle>();
		for (const row of data.following ?? []) {
			map.set(row.username, { actorId: row.actor_id, domain: row.domain ?? null });
		}
		followingMap.set(map);
	} catch {
		followingMap.set(new Map());
	}
}

export function clearFollowing(): void {
	followingMap.set(new Map());
}

// ── playback preferences (localStorage-backed) ──────────────────────────────
export interface Prefs {
	autoplay: boolean;
	defaultMuted: boolean;
	dataSaver: boolean;
}

const PREFS_KEY = 'toottok-prefs';
const PREFS_DEFAULTS: Prefs = { autoplay: true, defaultMuted: true, dataSaver: false };

function loadPrefs(): Prefs {
	try {
		const raw = localStorage.getItem(PREFS_KEY);
		if (raw) return { ...PREFS_DEFAULTS, ...(JSON.parse(raw) as Partial<Prefs>) };
	} catch {
		/* fresh install */
	}
	return { ...PREFS_DEFAULTS };
}

export const prefs = writable<Prefs>(loadPrefs());

export function savePrefs(next: Prefs): void {
	prefs.set(next);
	try {
		localStorage.setItem(PREFS_KEY, JSON.stringify(next));
	} catch {
		/* storage full/blocked — keep in-memory */
	}
}

export const feedCache = writable<Record<FeedKind, FeedState>>({
	following: emptyFeed(),
	discover: emptyFeed()
});

export function appendFeed(kind: FeedKind, items: Clip[], nextCursor: string | null): void {
	feedCache.update((cache) => ({
		...cache,
		[kind]: {
			items: [...cache[kind].items, ...items],
			nextCursor,
			loaded: true
		}
	}));
}

export function resetFeeds(): void {
	feedCache.set({ following: emptyFeed(), discover: emptyFeed() });
}

export function bumpCommentCount(clipId: string): void {
	feedCache.update((cache) => {
		const next = { ...cache };
		for (const kind of Object.keys(next) as FeedKind[]) {
			if (next[kind].items.some((clip) => clip.id === clipId)) {
				next[kind] = {
					...next[kind],
					items: next[kind].items.map((clip) =>
						clip.id === clipId ? { ...clip, comment_count: clip.comment_count + 1 } : clip
					)
				};
			}
		}
		return next;
	});
}
