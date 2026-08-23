<script lang="ts">
	import { onDestroy } from 'svelte';
	import { toast } from '$lib/toast';

	interface Props {
		onRecorded: (file: File, durationMs: number) => void;
	}

	let { onRecorded }: Props = $props();

	type Phase = 'idle' | 'live' | 'recording' | 'review';
	let phase = $state<Phase>('idle');
	let error = $state<string | null>(null);

	let videoEl: HTMLVideoElement | null = $state(null);
	let stream: MediaStream | null = null;
	let recorder: MediaRecorder | null = null;
	let chunks: Blob[] = [];
	let recordedBlob = $state<Blob | null>(null);
	let recordedDurationMs = 0;
	let recStart = 0;
	let timerId: number | null = null;
	let elapsed = $state(0);
	let mediaSupported = $state(false);

	// Capture fallback: when getUserMedia is unavailable (insecure context on
	// LAN http), fall back to a native camera file input — opens the camera app.
	let captureInput: HTMLInputElement | null = $state(null);

	function probeSupport() {
		mediaSupported = typeof navigator !== 'undefined' && !!navigator.mediaDevices?.getUserMedia;
	}

	async function startCamera() {
		error = null;
		if (!mediaSupported) {
			captureInput?.click();
			return;
		}
		try {
			stream = await navigator.mediaDevices.getUserMedia({
				video: {
					facingMode: { ideal: 'environment' },
					width: { ideal: 720 },
					height: { ideal: 1280 },
					frameRate: { ideal: 30 }
				},
				audio: false
			});
			phase = 'live';
			if (videoEl) {
				videoEl.srcObject = stream;
				await videoEl.play().catch(() => {});
			}
		} catch (e) {
			error =
				e instanceof DOMException && e.name === 'NotAllowedError'
					? 'Camera permission denied — allow it, or pick from library instead.'
					: 'Camera unavailable here — try picking from library.';
			phase = 'idle';
		}
	}

	function startRecording() {
		if (!stream) return;
		const mime = MediaRecorder.isTypeSupported('video/mp4')
			? 'video/mp4'
			: MediaRecorder.isTypeSupported('video/webm;codecs=vp9')
				? 'video/webm;codecs=vp9'
				: 'video/webm';
		try {
			recorder = new MediaRecorder(stream, { mimeType: mime, videoBitsPerSecond: 4_000_000 });
		} catch {
			recorder = new MediaRecorder(stream);
		}
		chunks = [];
		recordedBlob = null;
		recorder.ondataavailable = (e) => {
			if (e.data.size > 0) chunks.push(e.data);
		};
		recorder.onstop = () => {
			recordedBlob = new Blob(chunks, { type: recorder?.mimeType || 'video/webm' });
			phase = 'review';
			stopTimer();
		};
		recorder.start();
		recStart = performance.now();
		phase = 'recording';
		elapsed = 0;
		timerId = window.setInterval(() => {
			elapsed = Math.floor((performance.now() - recStart) / 1000);
			// Hard cap at 60s — short-form law.
			if (elapsed >= 60) stopRecording();
		}, 250);
	}

	function stopRecording() {
		if (recorder && recorder.state !== 'inactive') {
			recordedDurationMs = performance.now() - recStart;
			recorder.stop();
		}
	}

	function stopTimer() {
		if (timerId !== null) {
			window.clearInterval(timerId);
			timerId = null;
		}
	}

	function retake() {
		recordedBlob = null;
		phase = 'live';
	}

	function useClip() {
		if (!recordedBlob) return;
		const ext = recordedBlob.type.includes('mp4') ? 'mp4' : 'webm';
		const file = new File([recordedBlob], `clip-${Date.now()}.${ext}`, { type: recordedBlob.type });
		onRecorded(file, recordedDurationMs);
	}

	function onNativeCapture(e: Event) {
		const input = e.target as HTMLInputElement;
		const f = input.files?.[0];
		if (f) onRecorded(f, 0);
		input.value = '';
	}

	probeSupport();

	onDestroy(() => {
		stopTimer();
		stream?.getTracks().forEach((t) => t.stop());
		recorder?.stop();
	});
</script>

<div class="cam">
	{#if error}
		<div class="cam-error" role="alert">{error}</div>
	{/if}

	{#if phase === 'idle'}
		<button type="button" class="cam-cta" onclick={startCamera} aria-label="Start camera recording">
			<svg viewBox="0 0 24 24" width="34" height="34" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
				<path d="M23 7l-7 5 7 5V7z" />
				<rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
			</svg>
			<span>Record a clip</span>
			<small>{mediaSupported ? 'Camera opens in-app' : 'Opens your camera app'}</small>
		</button>
		<!-- Native camera fallback (works over plain http) -->
		<input
			bind:this={captureInput}
			type="file"
			accept="video/*"
			capture="environment"
			style="display:none"
			onchange={onNativeCapture}
		/>
	{:else if phase === 'live' || phase === 'recording'}
		<div class="cam-stage">
			<video bind:this={videoEl} autoplay muted playsinline class="cam-preview"></video>
			<div class="cam-overlay">
				{#if phase === 'recording'}
					<span class="rec-dot" aria-hidden="true"></span>
					<span class="rec-timer" aria-live="polite">{elapsed}s</span>
				{/if}
				<div class="cam-controls">
					{#if phase === 'recording'}
						<button type="button" class="rec-btn stop" onclick={stopRecording} aria-label="Stop recording">
							<span></span>
						</button>
					{:else}
						<button type="button" class="rec-btn" onclick={startRecording} aria-label="Start recording">
							<span></span>
						</button>
					{/if}
				</div>
			</div>
		</div>
	{:else if phase === 'review' && recordedBlob}
		<div class="cam-stage">
			<video class="cam-preview" controls playsinline src={URL.createObjectURL(recordedBlob)}>
				<track kind="captions" label="none" />
			</video>
			<div class="cam-actions">
				<button type="button" class="btn ghost" onclick={retake}>Retake</button>
				<button type="button" class="btn accent" onclick={useClip}>Use this clip</button>
			</div>
		</div>
	{/if}
</div>

<style>
	.cam {
		display: flex;
		flex-direction: column;
		gap: 0.75rem;
	}

	.cam-cta {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.35rem;
		padding: 2.5rem 1rem;
		border: 1.5px dashed var(--border, #2a2a34);
		border-radius: 16px;
		background: var(--surface, #15151c);
		color: var(--text, #f2f2f7);
		cursor: pointer;
		min-height: 180px;
	}
	.cam-cta:hover {
		border-color: var(--accent, #ff4d6d);
	}
	.cam-cta small {
		color: #8f8f9a;
	}

	.cam-error {
		background: #3a1218;
		color: #ffb3bf;
		padding: 0.6rem 0.8rem;
		border-radius: 10px;
		font-size: 0.85rem;
	}

	.cam-stage {
		position: relative;
		border-radius: 16px;
		overflow: hidden;
		background: #000;
		aspect-ratio: 9 / 16;
		max-height: 62vh;
		display: flex;
		align-items: center;
		justify-content: center;
	}
	.cam-preview {
		width: 100%;
		height: 100%;
		object-fit: cover;
	}

	.cam-overlay {
		position: absolute;
		inset: 0;
		display: flex;
		flex-direction: column;
		justify-content: space-between;
		align-items: center;
		padding: 1rem;
	}
	.rec-dot {
		width: 12px;
		height: 12px;
		border-radius: 50%;
		background: #ff4d6d;
		animation: pulse 1s infinite;
	}
	@keyframes pulse {
		0%, 100% { opacity: 1; }
		50% { opacity: 0.3; }
	}
	.rec-timer {
		color: #fff;
		font-variant-numeric: tabular-nums;
		text-shadow: 0 1px 4px rgba(0, 0, 0, 0.6);
	}

	.cam-controls {
		padding-bottom: env(safe-area-inset-bottom, 0px);
	}
	.rec-btn {
		width: 68px;
		height: 68px;
		border-radius: 50%;
		border: 4px solid #fff;
		background: rgba(255, 255, 255, 0.08);
		display: flex;
		align-items: center;
		justify-content: center;
		cursor: pointer;
	}
	.rec-btn span {
		width: 52px;
		height: 52px;
		border-radius: 50%;
		background: #ff4d6d;
		transition: border-radius 0.15s ease;
	}
	.rec-btn.stop span {
		border-radius: 10px;
		background: #ff4d6d;
	}

	.cam-actions {
		position: absolute;
		bottom: 0;
		left: 0;
		right: 0;
		display: flex;
		justify-content: center;
		gap: 0.75rem;
		padding: 1rem;
		padding-bottom: calc(1rem + env(safe-area-inset-bottom, 0px));
		background: linear-gradient(transparent, rgba(0, 0, 0, 0.7));
	}
	.btn.ghost {
		background: rgba(255, 255, 255, 0.15);
		color: #fff;
		border: none;
	}
	.btn.accent {
		background: var(--accent, #ff4d6d);
		color: #fff;
		border: none;
	}
</style>
