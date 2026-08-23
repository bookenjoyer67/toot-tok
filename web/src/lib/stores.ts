import { writable } from 'svelte/store';
import type { Clip } from './types';

export interface SessionUser {
	username: string;
	csrf: string;
	isAdmin?: boolean;
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
