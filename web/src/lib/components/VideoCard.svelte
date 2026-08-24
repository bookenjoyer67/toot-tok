<script lang="ts">
	import { untrack } from 'svelte';
	import { apiDelete, apiPost, apiPut } from '$lib/api';
	import { showToast } from '$lib/toast';
	import FollowButton from '$lib/components/FollowButton.svelte';
	import ShareSheet from '$lib/components/ShareSheet.svelte';
	import { get } from 'svelte/store';
	import { prefs } from '$lib/stores';
	import type { Clip } from '$lib/types';

	let {
		clip,
		active,
		showFollow = true,
		onComments
	}: {
		clip: Clip;
		active: boolean;
		/** Hide the in-rail follow button (e.g. on your own profile page). */
		showFollow?: boolean;
		onComments?: (clip: Clip) => void;
	} = $props();

	let video: HTMLVideoElement | null = $state(null);
	let cardEl: HTMLDivElement | null = $state(null);
	let muted = $state(get(prefs).defaultMuted);
	let liked = $state(false);
	let likeCount = $state(untrack(() => clip.like_count));
	let boosted = $state(false);
	let boostCount = $state(untrack(() => clip.share_count ?? 0));
	let saved = $state(false);
	let shareOpen = $state(false);
	let progress = $state(0); // 0..1
	let scrubbing = $state(false);

	$effect(() => {
		if (!active) {
			progress = 0;
			scrubbing = false;
		}
	});

	$effect(() => {
		const v = video;
		if (!v) return;
		if (active && get(prefs).autoplay) {
			v.play().catch(() => {});
		} else {
			v.pause();
			v.currentTime = 0;
		}
	});

	$effect(() => {
		if (video) video.muted = muted;
	});

	function onDocLike(): void {
		if (active && !liked) void like();
	}

	function onDocMute(): void {
		if (active) muted = !muted;
	}

	$effect(() => {
		document.addEventListener('toottok-like', onDocLike);
		document.addEventListener('toottok-mute', onDocMute);
		return () => {
			document.removeEventListener('toottok-like', onDocLike);
			document.removeEventListener('toottok-mute', onDocMute);
		};
	});

	function togglePlay(): void {
		const v = video;
		if (!v) return;
		if (v.paused) v.play().catch(() => {});
		else v.pause();
	}

	// ── gestures: single-tap pause, double-tap like, hold = 2× speed ──────────
	let pressTimer: ReturnType<typeof setTimeout> | undefined;
	let tapTimer: ReturnType<typeof setTimeout> | undefined;
	let lastTapAt = 0;
	let hearts = $state<{ id: number; x: number; y: number }[]>([]);
	let heartSeq = 0;

	function clearPressTimer(): void {
		if (pressTimer !== undefined) {
			clearTimeout(pressTimer);
			pressTimer = undefined;
		}
	}

	function endTurbo(): void {
		if (video && video.playbackRate !== 1) video.playbackRate = 1;
	}

	function onLayerPointerDown(): void {
		clearPressTimer();
		pressTimer = setTimeout(() => {
			pressTimer = undefined;
			if (video) video.playbackRate = 2;
		}, 450);
	}

	function onLayerPointerCancel(): void {
		clearPressTimer();
		endTurbo();
	}

	function onLayerPointerLeave(): void {
		clearPressTimer();
		endTurbo();
	}

	function onLayerPointerUp(e: PointerEvent): void {
		clearPressTimer();
		if (video?.playbackRate === 2) {
			endTurbo();
			return; // hold-release consumes the tap
		}
		const now = Date.now();
		if (now - lastTapAt < 300) {
			lastTapAt = 0;
			if (tapTimer !== undefined) {
				clearTimeout(tapTimer);
				tapTimer = undefined;
			}
			doubleTapLike(e.clientX, e.clientY);
			return;
		}
		lastTapAt = now;
		tapTimer = setTimeout(() => {
			tapTimer = undefined;
			togglePlay();
		}, 310);
	}

	function doubleTapLike(clientX: number, clientY: number): void {
		if (!liked) void like();
		const rect = cardEl?.getBoundingClientRect();
		const id = ++heartSeq;
		hearts = [
			...hearts,
			{
				id,
				x: rect ? clientX - rect.left : clientX,
				y: rect ? clientY - rect.top : clientY
			}
		];
		setTimeout(() => {
			hearts = hearts.filter((h) => h.id !== id);
		}, 800);
	}

	async function toggleSave(): Promise<void> {
		saved = !saved;
		try {
			if (saved) await apiPut(`/clips/${clip.id}/bookmark`);
			else await apiDelete(`/clips/${clip.id}/bookmark`);
		} catch {
			saved = !saved;
		}
	}

	async function toggleBoost(): Promise<void> {
		if (boosted) {
			boosted = false;
			boostCount -= 1;
			try {
				await apiDelete(`/clips/${clip.id}/announce`);
			} catch {
				boosted = true;
				boostCount += 1;
			}
			return;
		}
		boosted = true;
		boostCount += 1;
		try {
			await apiPost(`/clips/${clip.id}/announce`);
			showToast('Boosted');
		} catch {
			boosted = false;
			boostCount -= 1;
		}
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
		shareOpen = true;
	}

	function fmt(n: number): string {
		if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1).replace(/\.0$/, '')}M`;
		if (n >= 1_000) return `${(n / 1_000).toFixed(1).replace(/\.0$/, '')}K`;
		return String(n);
	}
</script>

<div class="card" bind:this={cardEl}>
	<!-- svelte-ignore a11y_media_has_caption -->
	<video
		bind:this={video}
		src={clip.asset_url}
		poster={clip.poster_url}
		loop
		muted
		playsinline
		preload="metadata"
		ontimeupdate={() => {
			if (!scrubbing && video && video.duration > 0) {
				progress = video.currentTime / video.duration;
			}
		}}
	></video>

	<!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
	<div
		class="tap-layer"
		onpointerdown={onLayerPointerDown}
		onpointerup={onLayerPointerUp}
		onpointercancel={onLayerPointerCancel}
		onpointerleave={onLayerPointerLeave}
		oncontextmenu={(e) => e.preventDefault()}
	></div>

	{#each hearts as heart (heart.id)}
		<div class="burst" style="left: {heart.x}px; top: {heart.y}px">
			<svg viewBox="0 0 24 24" fill="#ff4d6d" stroke="#ff4d6d" stroke-width="1.5">
				<path
					d="M20.84 4.61a5.5 5.5 0 0 0-7.78 0L12 5.67l-1.06-1.06a5.5 5.5 0 0 0-7.78 7.78l1.06 1.06L12 21.23l7.78-7.78 1.06-1.06a5.5 5.5 0 0 0 0-7.78z"
				/>
			</svg>
		</div>
	{/each}

	<input
		class="seek"
		style="--progress: {progress}"
		type="range"
		min="0"
		max="1000"
		value={Math.round(progress * 1000)}
		aria-label="Seek video"
		onpointerdown={() => (scrubbing = true)}
		onpointerup={() => (scrubbing = false)}
		onpointercancel={() => (scrubbing = false)}
		oninput={(e) => {
			const v = video;
			if (!v) return;
			const frac = Number(e.currentTarget.value) / 1000;
			progress = frac;
			if (v.duration > 0) v.currentTime = frac * v.duration;
		}}
	/>

	<div class="overlay">
		<p class="handle">@{clip.author.username}</p>
		{#if clip.caption_html}
			<p class="caption">{clip.caption_html ?? ''}</p>
		{/if}
		{#if clip.sound}
			<a class="sound-chip" href={`/sound/${clip.sound.id}`}>
				<svg viewBox="0 0 24 24" fill="currentColor" aria-hidden="true">
					<path d="M12 3v10.55A4 4 0 1 0 14 17V7h4V3h-6z" />
				</svg>
				<span>{clip.sound.title}</span>
			</a>
		{/if}
	</div>

	<div class="rail">
		{#if showFollow}
			<div class="follow-wrap">
				<a class="avatar-link" href={`/profile/${encodeURIComponent(clip.author.username)}`}>
					{#if clip.author.avatar_path}
						<img class="avatar" src={clip.author.avatar_path} alt="" />
					{:else}
						<span class="avatar fallback">
							{(clip.author.display_name || clip.author.username || '?').charAt(0).toUpperCase()}
						</span>
					{/if}
				</a>
				<FollowButton
					username={clip.author.username}
					actorId={clip.author.actor_id}
					domain={clip.author.domain || null}
					size="sm"
				/>
			</div>
		{/if}

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
			class="icon-btn rail-btn boost"
			class:boosted
			aria-label={boosted ? 'Undo boost' : 'Boost'}
			aria-pressed={boosted}
			onclick={() => void toggleBoost()}
		>
			<svg
				viewBox="0 0 24 24"
				fill="none"
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<polyline points="17 1 21 5 17 9" />
				<path d="M3 11V9a4 4 0 0 1 4-4h14" />
				<polyline points="7 23 3 19 7 15" />
				<path d="M21 13v2a4 4 0 0 1-4 4H3" />
			</svg>
			<span class="count">{fmt(boostCount)}</span>
		</button>

		<button
			class="icon-btn rail-btn save"
			class:saved
			aria-label={saved ? 'Remove from saved' : 'Save'}
			aria-pressed={saved}
			onclick={() => void toggleSave()}
		>
			<svg
				viewBox="0 0 24 24"
				fill={saved ? 'currentColor' : 'none'}
				stroke="currentColor"
				stroke-width="2"
				stroke-linecap="round"
				stroke-linejoin="round"
			>
				<path d="M19 21l-7-5-7 5V5a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2z" />
			</svg>
			<span class="count">{'Saved'}</span>
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

{#if shareOpen}
	<ShareSheet {clip} onclose={() => (shareOpen = false)} />
{/if}

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

	.seek {
		position: absolute;
		left: 0;
		right: 0;
		bottom: calc(var(--safe-bottom) + 62px);
		z-index: 4;
		width: 100%;
		height: 18px;
		margin: 0;
		appearance: none;
		-webkit-appearance: none;
		background: transparent;
		cursor: pointer;
	}

	.seek::-webkit-slider-runnable-track {
		height: 3px;
		border-radius: 2px;
		background: linear-gradient(
			to right,
			var(--accent) calc(var(--progress, 0) * 100%),
			rgba(255, 255, 255, 0.35) calc(var(--progress, 0) * 100%)
		);
	}

	.seek::-moz-range-track {
		height: 3px;
		border-radius: 2px;
		background: rgba(255, 255, 255, 0.35);
	}

	.seek::-moz-range-progress {
		height: 3px;
		border-radius: 2px;
		background: var(--accent);
	}

	.seek::-webkit-slider-thumb {
		appearance: none;
		-webkit-appearance: none;
		width: 12px;
		height: 12px;
		border-radius: 50%;
		background: #fff;
		box-shadow: 0 1px 4px rgba(0, 0, 0, 0.5);
		margin-top: -4.5px;
	}

	.seek::-moz-range-thumb {
		width: 12px;
		height: 12px;
		border: none;
		border-radius: 50%;
		background: #fff;
	}

	.burst {
		position: absolute;
		z-index: 5;
		width: 0;
		height: 0;
		pointer-events: none;
		animation: heart-pop 0.8s ease-out forwards;
	}

	.burst svg {
		position: absolute;
		left: -36px;
		top: -36px;
		width: 72px;
		height: 72px;
		filter: drop-shadow(0 2px 8px rgba(0, 0, 0, 0.4));
	}

	@keyframes heart-pop {
		0% {
			transform: scale(0.2);
			opacity: 0;
		}
		25% {
			transform: scale(1.15);
			opacity: 1;
		}
		60% {
			transform: scale(1) translateY(-12px);
			opacity: 1;
		}
		100% {
			transform: scale(0.9) translateY(-56px);
			opacity: 0;
		}
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

	.sound-chip {
		display: inline-flex;
		align-items: center;
		gap: 6px;
		margin-top: 8px;
		max-width: 220px;
		color: #fff;
		font-size: 0.8rem;
		font-weight: 600;
		text-decoration: none;
		text-shadow: 0 1px 3px rgba(0, 0, 0, 0.65);
		pointer-events: auto;
	}

	.sound-chip svg {
		width: 14px;
		height: 14px;
		flex-shrink: 0;
	}

	.sound-chip span {
		white-space: nowrap;
		overflow: hidden;
		text-overflow: ellipsis;
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

	.follow-wrap {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 6px;
		margin-bottom: 2px;
	}

	.avatar-link {
		display: block;
		border-radius: 50%;
		border: 2px solid #fff;
		line-height: 0;
	}

	.avatar {
		width: 44px;
		height: 44px;
		border-radius: 50%;
		object-fit: cover;
		background: #222;
	}

	.avatar.fallback {
		display: grid;
		place-items: center;
		color: #fff;
		font-weight: 700;
		font-size: 1.1rem;
	}

	/* Pull the +/✓ pill up over the avatar edge, TikTok-style */
	.follow-wrap :global(button.follow) {
		margin-top: -14px;
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

	.boost.boosted svg,
	.boost.boosted .count {
		color: #4dd8ff;
	}

	.boost.boosted svg path,
	.boost.boosted svg polyline {
		stroke: #4dd8ff;
	}

	.save.saved {
		color: #ffd24d;
	}
</style>
