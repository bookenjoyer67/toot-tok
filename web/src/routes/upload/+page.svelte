<script lang="ts">
	import { onDestroy, onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { apiGet, csrf, setCsrf } from '$lib/api';
	import { sessionUser } from '$lib/stores';
	import { showToast } from '$lib/toast';
	import RecordScreen from '$lib/components/recorder/RecordScreen.svelte';
	import EditScreen from '$lib/components/recorder/EditScreen.svelte';
	import type { EditManifest, EditSegment } from '$lib/editor';

	interface MeResponse {
		username: string;
		is_admin?: boolean;
		csrf_token?: string;
	}

	type Stage = 'record' | 'edit' | 'uploading' | 'done';

	let booted = $state(false);
	let stage = $state<Stage>('record');
	let segments = $state<EditSegment[]>([]);
	let manifest = $state<EditManifest | null>(null);

	let progress = $state(0);
	let uploading = $state(false);
	let error = $state<string | null>(null);
	let clipId = $state<string | null>(null);

	onMount(() => {
		void boot();
	});

	async function boot(): Promise<void> {
		if (!$sessionUser) {
			try {
				const me = await apiGet<MeResponse>('/accounts/me');
				if (me?.username) {
					if (me.csrf_token) setCsrf(me.csrf_token);
					sessionUser.set({
						username: me.username,
						csrf: me.csrf_token ?? '',
						isAdmin: me.is_admin ?? false
					});
				}
			} catch {
				sessionUser.set(null);
			}
		}
		booted = true;
	}

	$effect(() => {
		if (booted && !$sessionUser) void goto('/login');
	});

	function onRecorded(segs: EditSegment[]): void {
		segments = segs;
		stage = 'edit';
	}

	function onBack(): void {
		segments.forEach((s) => URL.revokeObjectURL(s.src));
		manifest?.voiceover && URL.revokeObjectURL(manifest.voiceover.src);
		manifest?.music && URL.revokeObjectURL(manifest.music.src);
		segments = [];
		manifest = null;
		stage = 'record';
	}

	function onPost(m: EditManifest): void {
		manifest = m;
		void upload(m);
	}

	/** Resolve every segment blob, then POST them as separate multipart files
	 *  (file0, file1, … plus the first segment again as `file` for the v1
	 *  renderer) alongside the full edit_manifest JSON. Keeps XHR progress. */
	async function upload(m: EditManifest): Promise<void> {
		error = null;
		uploading = true;
		const consumed: string[] = [];
		const segBlobs: Blob[] = [];
		let voBlob: Blob | null = null;
		let musicBlob: Blob | null = null;
		try {
			// Read every blob while EditScreen (and its object URLs) is still
			// mounted — the URLs die once this page switches to 'uploading'.
			for (const seg of m.segments) {
				const blob = await fetch(seg.src).then((r) => r.blob());
				segBlobs.push(blob);
				consumed.push(seg.src);
			}
			if (m.voiceover) {
				voBlob = await fetch(m.voiceover.src).then((r) => r.blob());
				consumed.push(m.voiceover.src);
			}
			if (m.music) {
				musicBlob = await fetch(m.music.src).then((r) => r.blob());
				consumed.push(m.music.src);
			}

			stage = 'edit';
			progress = 0;

			const form = new FormData();
			m.segments.forEach((seg, i) => {
				const blob = segBlobs[i];
				const ext = blob.type.includes('mp4') ? 'mp4' : 'webm';
				const key = `file${i}`;
				form.append(key, new File([blob], `clip-${i + 1}-${Date.now()}.${ext}`, { type: blob.type }));
				if (i === 0) {
					form.append('file', new File([blob], `clip-${Date.now()}.${ext}`, { type: blob.type }));
				}
			});
			if (m.caption.trim()) form.append('caption_html', m.caption.trim());
			if (m.cwText) form.append('cw_text', m.cwText);
			if (m.soundTitle) form.append('sound_title', m.soundTitle);

			form.append(
				'edit_manifest',
				JSON.stringify({
					segments: m.segments.map((seg, i) => ({
						src: null,
						blobKey: `file${i}`,
						trimStart: seg.trimStart,
						trimEnd: seg.trimEnd,
						speed: seg.speed,
						volume: seg.volume
					})),
					overlays: m.overlays,
					filter: m.filter,
					voiceover: m.voiceover ? { blobKey: 'vo', volume: m.voiceover.volume } : null,
					music: m.music ? { blobKey: 'music', volume: m.music.volume } : null,
					coverTime: m.coverTime,
					caption: m.caption,
					cwText: m.cwText
				})
			);
			if (voBlob) form.append('vo', voBlob, `voiceover-${Date.now()}.webm`);
			if (musicBlob) {
				const ext = musicBlob.type.includes('mp3') ? 'mp3' : musicBlob.type.includes('mp4') ? 'm4a' : 'webm';
				form.append('music', musicBlob, `music-${Date.now()}.${ext}`);
			}

			await new Promise<void>((resolve) => {
				const xhr = new XMLHttpRequest();
				xhr.open('POST', '/api/v1/clips/upload');
				xhr.responseType = 'text';
				xhr.setRequestHeader('Accept', 'application/json');
				if (csrf.token) xhr.setRequestHeader('X-Toottok-CSRF', csrf.token);
				xhr.timeout = 60000;

				xhr.upload.onprogress = (ev) => {
					if (ev.lengthComputable) progress = Math.round((ev.loaded / ev.total) * 100);
				};
				xhr.onload = () => {
					let body: { clip_id?: string; id?: string; detail?: string } | null = null;
					try {
						body = JSON.parse(xhr.responseText) as { clip_id?: string; id?: string; detail?: string };
					} catch {
						body = null;
					}
					if (xhr.status >= 200 && xhr.status < 300) {
						clipId = body?.clip_id ?? body?.id ?? '';
						showToast('Uploaded! Processing…');
						// Bounce to the feed like TikTok — upload already done.
						window.setTimeout(() => void goto('/'), 1200);
					} else {
						error = body?.detail ?? `Upload failed (${xhr.status}).`;
						restoreSegments(segBlobs);
						stage = 'edit';
					}
					resolve();
				};
				xhr.onerror = () => {
					error = 'Network error during upload.';
					restoreSegments(segBlobs);
					stage = 'edit';
					resolve();
				};
				xhr.ontimeout = () => {
					error = 'Upload timed out.';
					restoreSegments(segBlobs);
					stage = 'edit';
					resolve();
				};
				xhr.send(form);
			});
		} catch {
			error = 'Could not read the recorded clip.';
			restoreSegments(segBlobs);
			stage = 'edit';
		} finally {
			uploading = false;
			// The blobs were consumed — release the object URLs now that the
			// upload finished (success OR error). URLs for segments whose blobs
			// never resolved stay alive so the editor can be retried.
			for (const src of consumed) URL.revokeObjectURL(src);
		}
	}

	/** Re-point `segments` at fresh object URLs from the resolved blobs so the
	 *  editor stays usable after a failed upload revoked the originals. */
	function restoreSegments(blobs: Blob[]): void {
		if (blobs.length !== segments.length) return;
		segments = segments.map((seg, i) => ({ ...seg, src: URL.createObjectURL(blobs[i]) }));
	}

	onDestroy(() => {
		segments.forEach((s) => URL.revokeObjectURL(s.src));
		if (manifest?.voiceover) URL.revokeObjectURL(manifest.voiceover.src);
		if (manifest?.music) URL.revokeObjectURL(manifest.music.src);
	});
</script>

<svelte:head>
	<title>Record · TootTok</title>
</svelte:head>

{#if booted && $sessionUser}
	{#if stage === 'record'}
		<RecordScreen onDone={onRecorded} />
	{:else if stage === 'edit' && segments.length}
		<EditScreen {segments} onPost={onPost} onBack={onBack} />
		{#if uploading}
			<div class="upload-pill" role="status" aria-live="polite">
				<span class="spinner"></span> Uploading… {progress}%
			</div>
		{/if}
		{#if error}
			<p class="upload-err" role="alert">{error}</p>
		{/if}
	{/if}
{/if}

<style>
	.upload-pill {
		position: fixed;
		bottom: calc(var(--safe-bottom, 0px) + 84px);
		left: 50%;
		transform: translateX(-50%);
		display: flex;
		align-items: center;
		gap: 0.5rem;
		padding: 0.6rem 1rem;
		border-radius: 999px;
		background: rgba(0, 0, 0, 0.85);
		color: #fff;
		font-size: 0.85rem;
		z-index: 90;
		pointer-events: none;
	}
	.upload-err {
		position: fixed;
		bottom: calc(var(--safe-bottom, 0px) + 140px);
		left: 50%;
		transform: translateX(-50%);
		background: #ff4d6d;
		color: #fff;
		padding: 0.6rem 1rem;
		border-radius: 12px;
		font-size: 0.85rem;
		z-index: 90;
		max-width: 86vw;
	}
</style>
