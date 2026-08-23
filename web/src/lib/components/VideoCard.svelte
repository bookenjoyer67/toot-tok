<script lang="ts">
	import { untrack } from 'svelte';
	import { apiDelete, apiPost } from '$lib/api';
	import { showToast } from '$lib/toast';
	import type { Clip } from '$lib/types';

	let { clip, active, onComments }: { clip: Clip; active: boolean; onComments?: (clip: Clip) => void } =
		$props();

	let video: HTMLVideoElement | null = $state(null);
	let muted = $state(true);
	let liked = $state(false);
	let likeCount = $state(untrack(() => clip.like_count));

	$effect(() => {
		const v = video;
		if (!v) return;
		if (active) {
			v.play().catch(() => {});
		} else {
			v.pause();
			v.currentTime = 0;
		}
	});

	$effect(() => {
		if (video) video.muted = muted;
	});

	function togglePlay(): void {
		const v = video;
		if (!v) return;
		if (v.paused) v.play().catch(() => {});
		else v.pause();
	}

	async function like(): Promise<void> {
		if (liked) {
			liked = false;
			likeCount -= 1;
			try {
				await apiDelete(`/clips/${clip.id}/like`);
			} catch {
				liked = true;
				likeCount += 1;
			}
			return;
		}
		liked = true;
		likeCount += 1;
		try {
			await apiPost(`/clips/${clip.id}/like`);
		} catch {
			liked = false;
			likeCount -= 1;
		}
	}

	async function share(): Promise<void> {
		const permalink = clip.ap_id || `${location.origin}/clips/${clip.id}`;
		try {
			await navigator.clipboard.writeText(permalink);
			showToast('Copied!');
		} catch {
			showToast('Could not copy link');
		}
	}

	function fmt(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
		return String(n);
	}
</script>

<div class="card">
	<!-- svelte-ignore a11y_media_has_caption -->
	<video
		bind:this={video}
		src={clip.asset_url}
		poster={clip.poster_url}
		loop
		muted
		playsinline
		preload="metadata"
	></video>

	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div class="tap-layer" onclick={togglePlay}></div>

	<div class="overlay">
		<p class="handle">@{clip.author.username}</p>
		{#if clip.caption_html}
			<p class="caption">{clip.caption_html ?? ''}</p>
		{/if}
	</div>

	<div class="rail">
		<button
			class="icon-btn rail-btn like"
			class:liked
			aria-label={liked ? 'Unlike' : 'Like'}
			aria-pressed={liked}
			onclick={() => void like()}
		>
			<svg
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path
					d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"
				/>
			</svg>
			<span class="count">{fmt(likeCount)}</span>
		</button>

		<button
			class="icon-btn rail-btn"
			aria-label="Comments"
			onclick={() => onComments?.(clip)}
		>
			<svg
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path
					d="M21 11.5a8.38 8.38 0 0 1-.9 3.8 8.5 8.5 0 0 1-7.6 4.7 8.38 8.38 0 0 1-3.8-.9L3 21l1.9-5.7a8.38 8.38 0 0 1-.9-3.8 8.5 8.5 0 0 1 4.7-7.6 8.38 8.38 0 0 1 3.8-.9h.5a8.48 8.48 0 0 1 8 8v.5z"
				/>
			</svg>
			<span class="count">{fmt(clip.comment_count)}</span>
		</button>

		<button class="icon-btn rail-btn" aria-label="Copy link" onclick={() => void share()}>
			<svg
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path d="M4 12v8a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2v-8" />
				<polyline points="16 6 12 2 8 6" />
				<line x1="12" y1="2" x2="12" y2="15" />
			</svg>
			<span class="count">Share</span>
		</button>

		<button
			class="icon-btn rail-btn mute"
			aria-label={muted ? 'Unmute' : 'Mute'}
			onclick={() => (muted = !muted)}
		>
			{#if muted}
				<svg
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
					<line x1="23" y1="9" x2="17" y2="15" />
					<line x1="17" y1="9" x2="23" y2="15" />
				</svg>
			{:else}
				<svg
					viewBox="0 0 24 24"
					fill="none"
					stroke="currentColor"
					stroke-width="2"
					stroke-linecap="round"
					stroke-linejoin="round"
				>
					<polygon points="11 5 6 9 2 9 2 15 6 15 11 19 11 5" />
					<path d="M15.54 8.46a5 5 0 0 1 0 7.07" />
					<path d="M19.07 4.93a10 10 0 0 1 0 14.14" />
				</svg>
			{/if}
		</button>
	</div>
</div>

<style>
	.card {
		position: relative;
		height: 100dvh;
		width: 100%;
		overflow: hidden;
		background: #000;
		user-select: none;
	}

	video {
		position: absolute;
		inset: 0;
		width: 100%;
		height: 100%;
		object-fit: cover;
		background: #000;
	}

	.tap-layer {
		position: absolute;
		inset: 0;
		z-index: 1;
		cursor: pointer;
	}

	.overlay {
		position: absolute;
		left: 0;
		right: 76px;
		bottom: calc(var(--safe-bottom) + 72px);
		z-index: 2;
		padding: 0 16px;
		color: #fff;
		pointer-events: none;
		text-shadow: 0 1px 3px rgba(0, 0, 0, 0.65);
	}

	.handle {
		margin: 0 0 4px;
		font-weight: 700;
		font-size: 0.98rem;
	}

	.caption {
		margin: 0;
		font-size: 0.88rem;
		line-height: 1.4;
		max-height: 3.6em;
		overflow: hidden;
	}

	.rail {
		position: absolute;
		right: 10px;
		bottom: calc(var(--safe-bottom) + 72px);
		z-index: 3;
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 14px;
	}

	.rail-btn {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 3px;
		width: 48px;
		height: auto;
		border-radius: 14px;
		padding: 6px 0;
		color: #fff;
		text-shadow: 0 1px 3px rgba(0, 0, 0, 0.6);
	}

	.rail-btn svg {
		width: 28px;
		height: 28px;
		filter: drop-shadow(0 1px 3px rgba(0, 0, 0, 0.6));
	}

	.count {
		font-size: 0.72rem;
		font-weight: 600;
	}

	.like.liked svg,
	.like.liked .count {
		color: var(--accent);
		fill: var(--accent);
	}

	.like.liked svg path {
		stroke: var(--accent);
	}
</style>
