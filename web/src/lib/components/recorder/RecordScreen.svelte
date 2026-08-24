<script lang="ts">
	import { onDestroy } from 'svelte';
	import { MAX_CLIP_SECONDS } from '$lib/editor';
	import type { EditSegment } from '$lib/editor';
	import { showToast } from '$lib/toast';

	interface Props {
		onDone: (segments: EditSegment[]) => void;
	}

	let { onDone }: Props = $props();

	// ── camera state ──────────────────────────────────────────────────────
	let stream = $state<MediaStream | null>(null);
	let recorder: MediaRecorder | null = null;
	let chunks: Blob[] = [];
	let facing = $state<'environment' | 'user'>('environment');
	let flashOn = $state(false);
	let camError = $state<string | null>(null);
	let starting = $state(true);

	// ── recording state ───────────────────────────────────────────────────
	let segments = $state<EditSegment[]>([]);
	let recording = $state(false);
	let recordedTotal = $state(0);
	let segStart = 0;
	let timerId: number | null = null;

	let videoEl: HTMLVideoElement | null = $state(null);
	let galleryInput: HTMLInputElement | null = $state(null);

	async function startCamera(): Promise<void> {
		starting = true;
		camError = null;
		try {
			stream?.getTracks().forEach((t) => t.stop());
			// No width/height ideals: requesting 1080x1920 makes Android hand back
			// a portrait-cropped stream that object-fit:cover over-zooms. Take the
			// camera's native frame and let CSS fit it.
			stream = await navigator.mediaDevices.getUserMedia({
				video: { facingMode: { ideal: facing }, frameRate: { ideal: 30 } },
				audio: false
			});
			if (videoEl && videoEl.srcObject !== stream) {
				videoEl.srcObject = stream;
				await videoEl.play().catch(() => {});
			}
			applyFlash();
		} catch (e) {
			camError =
				e instanceof DOMException && e.name === 'NotAllowedError'
					? 'Camera permission denied — allow it in your browser settings.'
					: 'Camera unavailable — use the gallery instead.';
			// Fall back to the gallery picker.
		} finally {
			starting = false;
		}
	}

	function applyFlash(): void {
		// Flash is a per-track constraint when supported (mobile browsers).
		const track = stream?.getVideoTracks()[0];
		try {
			// @ts-expect-error non-standard but widely supported
			track?.applyConstraints?.({ advanced: [{ torch: flashOn }] });
		} catch {
			// unsupported — ignore
		}
	}

	function flipCamera(): void {
		facing = facing === 'environment' ? 'user' : 'environment';
		void startCamera();
	}

	function toggleFlash(): void {
		flashOn = !flashOn;
		applyFlash();
	}

	// ── segment recording ─────────────────────────────────────────────────
	function startSegment(): void {
		if (!stream || recordedTotal >= MAX_CLIP_SECONDS) return;
		const mime = MediaRecorder.isTypeSupported('video/mp4')
			? 'video/mp4'
			: MediaRecorder.isTypeSupported('video/webm;codecs=vp9')
				? 'video/webm;codecs=vp9'
				: 'video/webm';
		try {
			recorder = new MediaRecorder(stream, { mimeType: mime, videoBitsPerSecond: 6_000_000 });
		} catch {
			recorder = new MediaRecorder(stream);
		}
		chunks = [];
		recorder.ondataavailable = (e) => {
			if (e.data.size > 0) chunks.push(e.data);
		};
		recorder.onstop = () => {
			if (chunks.length === 0) return;
			const blob = new Blob(chunks, { type: recorder?.mimeType || 'video/webm' });
			const src = URL.createObjectURL(blob);
			const el = document.createElement('video');
			el.preload = 'metadata';
			el.src = src;
			el.onloadedmetadata = () => {
				const dur = isFinite(el.duration) ? el.duration : 0;
				segments = [
					...segments,
					{ src, sourceDuration: dur, trimStart: 0, trimEnd: dur, speed: 1, volume: 1 }
				];
				// Keep the object URL alive: EditScreen + upload read it later.
			};
		};
		recorder.start();
		recording = true;
		segStart = performance.now();
		timerId = window.setInterval(() => {
			const now = performance.now();
			recordedTotal = segments.reduce((a, s) => a + (s.trimEnd - s.trimStart), 0) + (now - segStart) / 1000;
			if (recordedTotal >= MAX_CLIP_SECONDS) stopSegment(true);
		}, 200);
	}

	function stopSegment(auto = false): void {
		if (recorder && recorder.state !== 'inactive') {
			recorder.stop();
		}
		recording = false;
		if (timerId !== null) {
			window.clearInterval(timerId);
			timerId = null;
		}
		if (auto) showToast('Max clip length reached');
	}

	function toggleRecord(): void {
		if (recording) stopSegment();
		else startSegment();
	}

	function undoLast(): void {
		if (segments.length === 0) return;
		if (!confirm('Remove the last segment?')) return;
		const last = segments[segments.length - 1];
		if (last) URL.revokeObjectURL(last.src);
		segments = segments.slice(0, -1);
		recordedTotal = segments.reduce((a, s) => a + (s.trimEnd - s.trimStart), 0);
	}

	function removeSegmentAt(i: number): void {
		const seg = segments[i];
		if (!seg) return;
		if (!confirm(`Remove segment ${i + 1}?`)) return;
		URL.revokeObjectURL(seg.src);
		segments = segments.filter((_, idx) => idx !== i);
		recordedTotal = segments.reduce((a, s) => a + (s.trimEnd - s.trimStart), 0);
	}

	function onGallery(e: Event): void {
		const input = e.target as HTMLInputElement;
		const f = input.files?.[0];
		if (!f) return;
		const src = URL.createObjectURL(f);
		const el = document.createElement('video');
		el.preload = 'metadata';
		el.src = src;
		el.onloadedmetadata = () => {
			const dur = isFinite(el.duration) ? el.duration : 0;
			segments = [
				...segments,
				{ src, sourceDuration: dur, trimStart: 0, trimEnd: dur, speed: 1, volume: 1 }
			];
		};
		input.value = '';
	}

	function done(): void {
		stopSegment();
		if (segments.length === 0) {
			showToast('Record something first');
			return;
		}
		stream?.getTracks().forEach((t) => t.stop());
		onDone(segments);
	}

	// Always attempt; startCamera falls back to gallery when unsupported.
	void startCamera();

	onDestroy(() => {
		stopSegment();
		stream?.getTracks().forEach((t) => t.stop());
		// Do NOT revoke segment URLs here: segments are handed to the page
		// (edit screen plays them). The page owns cleanup (onBack / upload
		// finally / page onDestroy). Only undo/removeSegmentAt (which drop a
		// segment) revoke.
	});
</script>

<div class="recorder">
	{#if camError && segments.length === 0}
		<div class="cam-fallback">
			<p>{camError}</p>
			<button type="button" class="btn ghost" onclick={() => galleryInput?.click()}>Pick from gallery</button>
		</div>
	{:else}
		<div class="stage">
			<!-- element ALWAYS mounted; startCamera attaches the stream -->
			<video
				bind:this={videoEl}
				autoplay
				muted
				playsinline
				class="preview"
				class:mirror={facing === 'user'}
			></video>
			{#if !stream}
				<div class="preview placeholder" aria-hidden="true"></div>
			{/if}

			<!-- top bar -->
			<div class="topbar">
				<button type="button" class="chip" class:active={flashOn} onclick={toggleFlash} aria-label="Toggle flash">
					⚡
				</button>
				<span class="timer" aria-live="polite">{Math.ceil(recordedTotal)}s</span>
				<button type="button" class="chip" onclick={flipCamera} aria-label="Flip camera">
					🔄
				</button>
			</div>

			<!-- bottom controls -->
			<div class="controls">
				<div class="left">
					<button type="button" class="gallery-btn" onclick={() => galleryInput?.click()} aria-label="Pick from gallery">
						🎞️
					</button>
				<input
					bind:this={galleryInput}
					type="file"
					accept="video/mp4,video/webm,video/quicktime"
					hidden
					onchange={onGallery}
				/>
					{#if segments.length > 0}
						<button type="button" class="chip" onclick={undoLast} aria-label="Remove last segment">✕</button>
					{/if}
				</div>
				<button
					type="button"
					class="rec-btn"
					class:recording={recording}
					disabled={!stream}
					onclick={toggleRecord}
					aria-label={recording ? 'Stop recording' : 'Record'}
				>
					<span></span>
				</button>
				<div class="right">
					<button
						type="button"
						class="chip"
						class:enabled={segments.length > 0}
						disabled={segments.length === 0}
						onclick={done}
						aria-label="Continue to edit"
					>
						Next →
					</button>
				</div>
			</div>

			{#if segments.length > 0}
				<div class="segments" aria-label="Recorded segments">
					{#each segments as seg, i}
						<button
							type="button"
							class="seg"
							onclick={() => removeSegmentAt(i)}
							title="Remove segment {i + 1}"
							aria-label="Remove segment {i + 1}"
						>
							{i + 1}
						</button>
					{/each}
				</div>
			{/if}
		</div>
	{/if}
</div>

<style>
	.recorder {
		position: fixed;
		inset: 0;
		background: #000;
		z-index: 20;
	}
	.stage {
		position: relative;
		width: 100%;
		height: 100%;
	}
	.preview {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}
	/* Front camera previews are mirrored (selfie view) — text reads correctly.
	   Recording itself stays unmirrored data; only the live preview flips. */
	.preview.mirror {
		transform: scaleX(-1);
	}
	.preview.placeholder {
		background: #0b0b0f;
	}
	.topbar {
		position: absolute;
		top: 0;
		left: 0;
		right: 0;
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: calc(env(safe-area-inset-top, 0px) + 0.75rem) 1rem;
		background: linear-gradient(rgba(0, 0, 0, 0.55), transparent);
	}
	.timer {
		color: #fff;
		font-variant-numeric: tabular-nums;
		font-weight: 700;
		text-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
	}
	.chip {
		width: 46px;
		height: 46px;
		border-radius: 50%;
		border: none;
		background: rgba(255, 255, 255, 0.16);
		color: #fff;
		font-size: 1.15rem;
		cursor: pointer;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.chip.active {
		background: var(--accent, #ff4d6d);
	}
	.chip.enabled {
		width: auto;
		padding: 0 1rem;
		border-radius: 999px;
		background: var(--accent, #ff4d6d);
		font-size: 0.95rem;
		font-weight: 700;
	}
	.chip:disabled {
		opacity: 0.35;
	}
	.controls {
		position: absolute;
		bottom: 0;
		left: 0;
		right: 0;
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: 0 1.25rem calc(env(safe-area-inset-bottom, 0px) + 1.25rem);
		background: linear-gradient(transparent, rgba(0, 0, 0, 0.6));
	}
	.left,
	.right {
		display: flex;
		gap: 0.6rem;
		align-items: center;
		flex: 1;
	}
	.right {
		justify-content: flex-end;
	}
	.gallery-btn {
		width: 46px;
		height: 46px;
		border-radius: 10px;
		border: none;
		background: rgba(255, 255, 255, 0.16);
		font-size: 1.3rem;
		cursor: pointer;
	}
	.rec-btn {
		width: 76px;
		height: 76px;
		border-radius: 50%;
		border: 4px solid #fff;
		background: rgba(255, 255, 255, 0.12);
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
		flex-shrink: 0;
	}
	.rec-btn span {
		width: 60px;
		height: 60px;
		border-radius: 50%;
		background: #ff4d6d;
		transition: border-radius 0.15s ease, transform 0.15s ease;
	}
	.rec-btn:disabled {
		opacity: 0.35;
		cursor: not-allowed;
	}
	.rec-btn.recording span {
		border-radius: 14px;
		transform: scale(0.85);
	}
	.segments {
		position: absolute;
		bottom: calc(env(safe-area-inset-bottom, 0px) + 7.5rem);
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		gap: 0.3rem;
	}
	.seg {
		width: 14px;
		height: 14px;
		border-radius: 50%;
		border: none;
		padding: 0;
		background: #ff4d6d;
		color: #fff;
		font: inherit;
		font-size: 0.6rem;
		font-weight: 700;
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
	}
	.seg:active {
		transform: scale(0.85);
	}
	.cam-fallback {
		height: 100%;
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 1rem;
		padding: 2rem;
		color: #f2f2f7;
		text-align: center;
	}
	.btn.ghost {
		background: var(--surface, #15151c);
		color: var(--text, #f2f2f7);
		border: 1px solid var(--border, #2a2a34);
	}
</style>
