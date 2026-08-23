<script lang="ts">
	import { onMount, onDestroy } from 'svelte';
	import type { EditManifest, EditSegment, FilterId, TextOverlay } from '$lib/editor';
	import { manifestDuration, resolveTime, cssFilter, FILTERS, SPEEDS } from '$lib/editor';

	interface Props {
		segments: EditSegment[];
		onPost: (manifest: EditManifest) => void;
		onBack: () => void;
	}

	let { segments, onPost, onBack }: Props = $props();

	// ── preview / timeline ────────────────────────────────────────────────
	let videoEl: HTMLVideoElement | null = $state(null);
	let currentTime = $state(0);
	let playing = $state(false);
	let total = $derived(manifestDuration({ segments, overlays: [], filter: 'none', voiceover: null, music: null, coverTime: 0, caption: '', cwText: null } as EditManifest));
	let activeIndex = $state(0);

	// ── edits ─────────────────────────────────────────────────────────────
	let filter = $state<FilterId>('none');
	let overlays = $state<TextOverlay[]>([]);
	let draftOverlay = $state('');
	let caption = $state('');
	let cwEnabled = $state(false);
	let cwText = $state('');
	let busy = $state(false);
	let error = $state<string | null>(null);
	let tab: 'edit' | 'text' | 'sound' | 'post' = $state('edit');

	// ── voiceover / sound ────────────────────────────────────────────────
	let voBlob: Blob | null = null;
	let voUrl = $state<string | null>(null);
	let voVolume = $state(1);
	let voRecorder: MediaRecorder | null = null;
	let voChunks: Blob[] = [];
	let voRecording = $state(false);
	let musicUrl = $state<string | null>(null);
	let musicVolume = $state(1);
	let musicInput: HTMLInputElement | null = $state(null);

	// ── dictation ────────────────────────────────────────────────────────
	let dictating = $state(false);
	let speechRec: SpeechRecognitionLike | null = null;

	function syncVideo(): void {
		if (!videoEl || !segments.length) return;
		const r = resolveTime(
			{ segments, overlays: [], filter: 'none', voiceover: null, music: null, coverTime: 0, caption: '', cwText: null } as EditManifest,
			currentTime
		);
		if (r.segmentIndex !== activeIndex) {
			activeIndex = r.segmentIndex;
			const seg = segments[r.segmentIndex];
			videoEl.src = seg.src;
			videoEl.playbackRate = seg.speed;
			videoEl.currentTime = Math.min(r.sourceTime, Math.max(0, seg.trimEnd - 0.05));
			if (playing) void videoEl.play().catch(() => {});
		} else if (videoEl.src !== segments[r.segmentIndex].src) {
			videoEl.src = segments[r.segmentIndex].src;
		}
	}

	function onTimeUpdate(): void {
		if (!videoEl || !segments.length) return;
		const seg = segments[activeIndex];
		// Map current playback position into final timeline: previous segments
		// duration + local progress scaled by speed.
		let prev = 0;
		for (let i = 0; i < activeIndex; i++) prev += (segments[i].trimEnd - segments[i].trimStart) / segments[i].speed;
		currentTime = prev + Math.max(0, (videoEl.currentTime - seg.trimStart) / seg.speed);
		if (videoEl.currentTime >= seg.trimEnd - 0.05) {
			if (activeIndex < segments.length - 1) {
				activeIndex += 1;
				const n = segments[activeIndex];
				videoEl.src = n.src;
				videoEl.playbackRate = n.speed;
				videoEl.currentTime = n.trimStart;
				if (playing) void videoEl.play().catch(() => {});
			} else {
				videoEl.pause();
				playing = false;
				currentTime = total;
			}
		}
	}

	function seekTo(t: number): void {
		currentTime = Math.max(0, Math.min(total, t));
		syncVideo();
	}

	function togglePlay(): void {
		if (!videoEl) return;
		if (playing) {
			videoEl.pause();
			playing = false;
		} else {
			if (currentTime >= total) {
				currentTime = 0;
				activeIndex = 0;
				videoEl.src = segments[0].src;
				videoEl.currentTime = segments[0].trimStart;
			}
			videoEl.playbackRate = segments[activeIndex]?.speed ?? 1;
			void videoEl.play().catch(() => {});
			playing = true;
		}
	}

	function setTrim(seg: EditSegment, edge: 'start' | 'end', value: number): void {
		// value in SOURCE seconds, clamped into the segment.
		const min = 0;
		const max = seg.sourceDuration;
		if (edge === 'start') seg.trimStart = Math.min(Math.max(min, value), seg.trimEnd - 0.1);
		else seg.trimEnd = Math.max(Math.min(max, value), seg.trimStart + 0.1);
		segments = [...segments];
		syncVideo();
	}

	function setSpeed(seg: EditSegment, speed: number): void {
		seg.speed = speed;
		segments = [...segments];
	}

	function removeSegment(i: number): void {
		segments = segments.filter((_, idx) => idx !== i);
		if (activeIndex >= segments.length) activeIndex = Math.max(0, segments.length - 1);
		syncVideo();
	}

	function addOverlay(): void {
		const text = draftOverlay.trim();
		if (!text) return;
		overlays = [
			...overlays,
			{
				text,
				x: 0.5,
				y: 0.75,
				scale: 1,
				start: currentTime,
				end: Math.min(total, currentTime + 3),
				color: '#ffffff',
				align: 'center'
			}
		];
		draftOverlay = '';
	}

	function removeOverlay(i: number): void {
		overlays = overlays.filter((_, idx) => idx !== i);
	}

	function formatTime(t: number): string {
		const s = Math.floor(t);
		return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`;
	}

	// ── voiceover ─────────────────────────────────────────────────────────
	async function startVO(): Promise<void> {
		if (!navigator.mediaDevices?.getUserMedia) return;
		try {
			const s = await navigator.mediaDevices.getUserMedia({ audio: true });
			const mime = MediaRecorder.isTypeSupported('audio/webm') ? 'audio/webm' : 'audio/mp4';
			voRecorder = new MediaRecorder(s, { mimeType: mime });
			voChunks = [];
			voRecorder.ondataavailable = (e) => {
				if (e.data.size > 0) voChunks.push(e.data);
			};
			voRecorder.onstop = () => {
				s.getTracks().forEach((t) => t.stop());
				voBlob = new Blob(voChunks, { type: voRecorder?.mimeType || 'audio/webm' });
				if (voUrl) URL.revokeObjectURL(voUrl);
				voUrl = URL.createObjectURL(voBlob);
			};
			voRecorder.start();
			voRecording = true;
		} catch {
			error = 'Microphone unavailable.';
		}
	}

	function stopVO(): void {
		if (voRecorder && voRecorder.state !== 'inactive') voRecorder.stop();
		voRecording = false;
	}

	function removeVO(): void {
		voBlob = null;
		if (voUrl) URL.revokeObjectURL(voUrl);
		voUrl = null;
	}

	function onMusic(e: Event): void {
		const f = (e.target as HTMLInputElement).files?.[0];
		if (!f) return;
		if (musicUrl) URL.revokeObjectURL(musicUrl);
		musicUrl = URL.createObjectURL(f);
	}

	// ── dictation ─────────────────────────────────────────────────────────
	function startDictation(): void {
		const SR = window.SpeechRecognition || window.webkitSpeechRecognition;
		if (!SR) {
			error = 'Dictation not supported on this browser — type instead.';
			return;
		}
		speechRec = new SR();
		speechRec.lang = navigator.language || 'en-US';
		speechRec.interimResults = true;
		speechRec.continuous = true;
		speechRec.onresult = (e) => {
			for (let i = e.resultIndex; i < e.results.length; i++) {
				const r = e.results[i];
				if (r.isFinal) caption = (caption + ' ' + r[0].transcript).trim();
			}
		};
		speechRec.onend = () => (dictating = false);
		speechRec.onerror = () => (dictating = false);
		speechRec.start();
		dictating = true;
	}

	function stopDictation(): void {
		speechRec?.stop();
		dictating = false;
	}

	function post(): void {
		if (busy) return;
		if (segments.length === 0) {
			error = 'Nothing to post.';
			return;
		}
		busy = true;
		error = null;
		const manifest: EditManifest = {
			segments,
			overlays,
			filter,
			voiceover: voBlob && voUrl ? { src: voUrl, volume: voVolume } : null,
			music: musicUrl ? { src: musicUrl, volume: musicVolume } : null,
			coverTime: currentTime,
			caption,
			cwText: cwEnabled && cwText.trim() ? cwText.trim() : null
		};
		onPost(manifest);
	}

	onMount(() => {
		if (segments.length) {
			videoEl!.src = segments[0].src;
			videoEl!.playbackRate = segments[0].speed;
			videoEl!.currentTime = segments[0].trimStart;
		}
	});

	onDestroy(() => {
		speechRec?.abort();
		if (voRecorder && voRecorder.state !== 'inactive') voRecorder.stop();
		if (voUrl) URL.revokeObjectURL(voUrl);
		if (musicUrl) URL.revokeObjectURL(musicUrl);
	});
</script>

<div class="editor">
	<!-- preview -->
	<div
		class="preview"
		onclick={togglePlay}
		onkeydown={(e) => {
			if (e.key === 'Enter' || e.key === ' ') {
				e.preventDefault();
				togglePlay();
			}
		}}
		role="button"
		tabindex="0"
		aria-label="Play / pause preview"
	>
		<video
			bind:this={videoEl}
			muted
			playsinline
			class="pv"
			style:filter={cssFilter(filter)}
			ontimeupdate={onTimeUpdate}
			onended={() => (playing = false)}
		></video>
		{#each overlays as ov, i}
			{#if currentTime >= ov.start && currentTime <= ov.end}
				<button
					type="button"
					class="ov"
					style:left={`${ov.x * 100}%`}
					style:top={`${ov.y * 100}%`}
					style:font-size={`${ov.scale}rem`}
					style:color={ov.color}
					style:text-align={ov.align}
					onclick={(e) => {
						e.stopPropagation();
						removeOverlay(i);
					}}
					title="Tap to remove"
					aria-label={`Remove text: ${ov.text}`}
				>{ov.text}</button>
			{/if}
		{/each}
		{#if playing}
			<span class="pause-hint" aria-hidden="true">⏸</span>
		{/if}
	</div>

	<!-- seek -->
	<div class="seekrow">
		<input
			type="range"
			min="0"
			max={total || 1}
			step="0.05"
			value={currentTime}
			oninput={(e) => seekTo(Number((e.target as HTMLInputElement).value))}
			aria-label="Seek"
		/>
		<span class="timecode">{formatTime(currentTime)} / {formatTime(total)}</span>
	</div>

	<!-- tabs -->
	<div class="tabs" role="tablist">
		<button type="button" class:active={tab === 'edit'} onclick={() => (tab = 'edit')} role="tab">Edit</button>
		<button type="button" class:active={tab === 'text'} onclick={() => (tab = 'text')} role="tab">Text</button>
		<button type="button" class:active={tab === 'sound'} onclick={() => (tab = 'sound')} role="tab">Sound</button>
		<button type="button" class:active={tab === 'post'} onclick={() => (tab = 'post')} role="tab">Post</button>
	</div>

	<!-- edit tab -->
	{#if tab === 'edit'}
		<div class="panel">
			<div class="timeline" aria-label="Segments">
				{#each segments as seg, i}
					<div class="seg-card">
						<span class="seg-idx">{i + 1}</span>
						<video class="seg-thumb" src={seg.src} muted playsinline></video>
						<div class="seg-controls">
							<label>Trim in
								<input
									type="range"
									min="0"
									max={seg.sourceDuration}
									step="0.05"
									value={seg.trimStart}
									oninput={(e) => setTrim(seg, 'start', Number((e.target as HTMLInputElement).value))}
									aria-label={`Trim start of segment ${i + 1}`}
								/>
								{formatTime(seg.trimStart)}
							</label>
							<label>Trim out
								<input
									type="range"
									min="0"
									max={seg.sourceDuration}
									step="0.05"
									value={seg.trimEnd}
									oninput={(e) => setTrim(seg, 'end', Number((e.target as HTMLInputElement).value))}
									aria-label={`Trim end of segment ${i + 1}`}
								/>
								{formatTime(seg.trimEnd)}
							</label>
							<label>Speed
								<select
									value={seg.speed}
									onchange={(e) => setSpeed(seg, Number((e.target as HTMLSelectElement).value))}
									aria-label={`Playback speed for segment ${i + 1}`}
								>
									{#each SPEEDS as sp}
										<option value={sp}>{sp}×</option>
									{/each}
								</select>
							</label>
							<button type="button" class="btn ghost small" onclick={() => removeSegment(i)}>Remove</button>
						</div>
					</div>
				{/each}
			</div>

			<div class="filters" aria-label="Filters">
				{#each FILTERS as f}
					<button
						type="button"
						class="filter-chip"
						class:active={filter === f.id}
						onclick={() => (filter = f.id)}
						aria-pressed={filter === f.id}
					>{f.label}</button>
				{/each}
			</div>
		</div>

	<!-- text tab -->
	{:else if tab === 'text'}
		<div class="panel">
			<div class="overlay-form">
				<input class="input" bind:value={draftOverlay} placeholder="Text to show…" maxlength="80" />
				<button type="button" class="btn accent" onclick={addOverlay}>Add</button>
			</div>
			{#if overlays.length === 0}
				<p class="hint">Add text that appears over your clip between the current time and +3s. Tap a text bubble in the preview to remove it.</p>
			{:else}
				<ul class="overlay-list">
					{#each overlays as ov, i}
						<li>
							<span>{ov.text}</span>
							<button type="button" class="btn ghost small" onclick={() => removeOverlay(i)}>Remove</button>
						</li>
					{/each}
				</ul>
			{/if}
		</div>

	<!-- sound tab -->
	{:else if tab === 'sound'}
		<div class="panel">
			<div class="sound-row">
				<span>Voiceover</span>
				{#if voUrl}
					<audio controls src={voUrl}></audio>
					<button type="button" class="btn ghost small" onclick={removeVO}>Remove</button>
				{:else}
					<button type="button" class="btn accent" onclick={voRecording ? stopVO : startVO}>
						{voRecording ? 'Stop' : 'Record voiceover'}
					</button>
				{/if}
				{#if voUrl}
					<label>Volume
						<input type="range" min="0" max="1" step="0.05" value={voVolume} oninput={(e) => (voVolume = Number((e.target as HTMLInputElement).value))} />
					</label>
				{/if}
			</div>
			<div class="sound-row">
				<span>Music / audio from device</span>
				<button type="button" class="btn ghost" onclick={() => musicInput?.click()}>{musicUrl ? 'Change audio' : 'Choose audio'}</button>
				<input bind:this={musicInput} type="file" accept="audio/*" hidden onchange={onMusic} />
				{#if musicUrl}
					<audio controls src={musicUrl}></audio>
					<label>Volume
						<input type="range" min="0" max="1" step="0.05" value={musicVolume} oninput={(e) => (musicVolume = Number((e.target as HTMLInputElement).value))} />
					</label>
				{/if}
			</div>
		</div>

	<!-- post tab -->
	{:else}
		<div class="panel">
			<label class="label" for="caption">Caption</label>
			<div class="caption-row">
				<textarea
					id="caption"
					class="input"
					rows="3"
					maxlength="500"
					bind:value={caption}
					placeholder="Say something…"
				></textarea>
				<button
					type="button"
					class="mic-btn"
					class:active={dictating}
					onclick={dictating ? stopDictation : startDictation}
					aria-label={dictating ? 'Stop dictation' : 'Dictate caption'}
				>
					🎙️
				</button>
			</div>
			<span class="counter">{caption.length}/500</span>

			<label class="check">
				<input type="checkbox" bind:checked={cwEnabled} />
				<span>Content warning</span>
			</label>
			{#if cwEnabled}
				<input class="input" bind:value={cwText} maxlength="200" placeholder="e.g. flashing lights" aria-label="Content warning text" />
			{/if}

			{#if error}
				<p class="err" role="alert">{error}</p>
			{/if}

			<div class="post-actions">
				<button
					type="button"
					class="btn ghost"
					onclick={() => {
						if (confirm('Discard your edits and go back?')) onBack();
					}}
				>‹ Back</button>
				<button type="button" class="btn accent big" disabled={busy} onclick={post}>{busy ? 'Posting…' : 'Post'}</button>
			</div>
		</div>
	{/if}
</div>

<style>
	.editor {
		position: fixed;
		inset: 0;
		background: var(--bg, #0b0b0f);
		display: flex;
		flex-direction: column;
		z-index: 25;
		color: var(--text, #f2f2f7);
	}
	.preview {
		position: relative;
		flex: 1;
		min-height: 0;
		background: #000;
		display: flex;
		align-items: center;
		justify-content: center;
		overflow: hidden;
		cursor: pointer;
	}
	.pv {
		width: 100%;
		height: 100%;
		object-fit: contain;
	}
	.ov {
		position: absolute;
		transform: translate(-50%, -50%);
		font-weight: 800;
		text-shadow: 0 1px 6px rgba(0, 0, 0, 0.9);
		pointer-events: auto;
		cursor: pointer;
		user-select: none;
		background: none;
		border: none;
		padding: 0;
		line-height: 1.1;
	}
	.pause-hint {
		position: absolute;
		font-size: 3rem;
		opacity: 0.7;
	}
	.seekrow {
		display: flex;
		align-items: center;
		gap: 0.6rem;
		padding: 0.4rem 0.9rem;
	}
	.seekrow input {
		flex: 1;
	}
	.timecode {
		font-size: 0.75rem;
		color: #8f8f9a;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}
	.tabs {
		display: flex;
		gap: 0.25rem;
		padding: 0 0.9rem;
		border-bottom: 1px solid var(--border, #2a2a34);
	}
	.tabs button {
		flex: 1;
		padding: 0.6rem 0;
		border: none;
		background: none;
		color: #8f8f9a;
		font-weight: 700;
		font-size: 0.9rem;
		border-bottom: 2px solid transparent;
		cursor: pointer;
		min-height: 44px;
	}
	.tabs button.active {
		color: var(--accent, #ff4d6d);
		border-bottom-color: var(--accent, #ff4d6d);
	}
	.panel {
		padding: 0.9rem;
		overflow-y: auto;
		max-height: 42vh;
	}
	.timeline {
		display: flex;
		flex-direction: column;
		gap: 0.6rem;
	}
	.seg-card {
		display: flex;
		gap: 0.6rem;
		background: var(--surface, #15151c);
		border: 1px solid var(--border, #2a2a34);
		border-radius: 12px;
		padding: 0.6rem;
		align-items: flex-start;
	}
	.seg-idx {
		background: var(--accent, #ff4d6d);
		border-radius: 50%;
		width: 22px;
		height: 22px;
		display: flex;
		align-items: center;
		justify-content: center;
		font-size: 0.72rem;
		font-weight: 700;
		flex-shrink: 0;
	}
	.seg-thumb {
		width: 64px;
		height: 96px;
		object-fit: cover;
		border-radius: 8px;
		flex-shrink: 0;
	}
	.seg-controls {
		flex: 1;
		display: flex;
		flex-direction: column;
		gap: 0.3rem;
		font-size: 0.78rem;
	}
	.seg-controls label {
		display: flex;
		align-items: center;
		gap: 0.4rem;
	}
	.seg-controls input[type='range'] {
		flex: 1;
		min-height: 44px;
	}
	.seg-controls select {
		min-height: 44px;
	}
	.filters {
		display: flex;
		flex-wrap: wrap;
		gap: 0.4rem;
		margin-top: 0.8rem;
	}
	.filter-chip {
		padding: 0.45rem 0.8rem;
		border-radius: 999px;
		border: 1px solid var(--border, #2a2a34);
		background: transparent;
		color: var(--text, #f2f2f7);
		cursor: pointer;
		font-size: 0.85rem;
		min-height: 44px;
	}
	.filter-chip.active {
		background: var(--accent, #ff4d6d);
		border-color: var(--accent, #ff4d6d);
		color: #fff;
	}
	.overlay-form {
		display: flex;
		gap: 0.5rem;
	}
	.overlay-form .input {
		flex: 1;
	}
	.hint {
		color: #8f8f9a;
		font-size: 0.85rem;
	}
	.overlay-list li {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0.4rem 0;
		border-bottom: 1px solid var(--border, #2a2a34);
	}
	.sound-row {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 0.7rem 0;
		border-bottom: 1px solid var(--border, #2a2a34);
	}
	.caption-row {
		display: flex;
		gap: 0.5rem;
		align-items: flex-end;
	}
	.caption-row textarea {
		flex: 1;
	}
	.mic-btn {
		width: 48px;
		height: 48px;
		border-radius: 50%;
		border: 2px solid var(--border, #2a2a34);
		background: var(--surface, #15151c);
		font-size: 1.2rem;
		cursor: pointer;
	}
	.mic-btn.active {
		border-color: var(--accent, #ff4d6d);
		background: #3a1218;
		animation: pulse 1s infinite;
	}
	@keyframes pulse {
		0%, 100% { box-shadow: 0 0 0 0 rgba(255, 77, 109, 0.5); }
		50% { box-shadow: 0 0 0 10px rgba(255, 77, 109, 0); }
	}
	.counter {
		font-size: 0.72rem;
		color: #8f8f9a;
	}
	.err {
		color: #ffb3bf;
		background: #3a1218;
		padding: 0.5rem 0.7rem;
		border-radius: 10px;
		font-size: 0.85rem;
	}
	.post-actions {
		display: flex;
		gap: 0.6rem;
		margin-top: 1rem;
	}
	.post-actions .btn {
		flex: 1;
	}
	.btn.ghost {
		background: var(--surface, #15151c);
		color: var(--text, #f2f2f7);
		border: 1px solid var(--border, #2a2a34);
	}
	.btn.accent {
		background: var(--accent, #ff4d6d);
		color: #fff;
		border: none;
	}
	.btn.accent.big {
		padding: 0.8rem 1rem;
		font-weight: 800;
		font-size: 1rem;
	}
	.btn:disabled {
		opacity: 0.5;
	}
	.check {
		display: flex;
		gap: 0.5rem;
		align-items: center;
		margin: 0.7rem 0 0.3rem;
	}
	.label {
		display: block;
		margin-bottom: 0.3rem;
		font-size: 0.85rem;
		font-weight: 600;
	}
</style>
