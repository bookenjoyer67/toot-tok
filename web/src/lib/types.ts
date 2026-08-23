export interface Author {
	username: string;
	display_name: string;
	avatar_path: string | null;
	domain: string;
	url?: string | null;
}

export interface Clip {
	id: string;
	ap_id: string;
	caption_html: string | null;
	duration_s: number;
	width: number;
	height: number;
	like_count: number;
	comment_count: number;
	created_at: string;
	author: Author;
	asset_url?: string;
	poster_url?: string;
}

export interface FeedResponse {
	items: Clip[];
	next_cursor?: string;
}

export interface Profile {
	// Backend /profiles/{username} actor payload: username, display_name,
	// domain, avatar_path, summary (plain escaped text — render as TEXT).
	username: string;
	display_name: string;
	avatar_path: string | null;
	domain: string | null;
	summary?: string | null;
}

export interface CommentT {
	id: string;
	clip_id: string;
	author: Author;
	body: string;
	body_html?: string | null;
	created_at: string;
}

export type NotificationType = 'like' | 'comment' | 'follow' | 'mention' | 'boost';

export interface NotificationT {
	id: string;
	type: NotificationType;
	actor: Author;
	clip_id: string | null;
	clip_poster_path?: string | null;
	body?: string | null;
	created_at: string;
	read: boolean;
}
