<script lang="ts">
	import { onMount } from 'svelte';
	import { goto } from '$app/navigation';
	import { apiGet, csrf, setCsrf } from '$lib/api';
	import { sessionUser } from '$lib/stores';
	import { showToast } from '$lib/toast';

	interface MeResponse {
		username: string;
		is_admin?: boolean;
		csrf?: string;
	}

	interface UploadResponse {
		clip_id?: string;
		id?: string;
		detail?: string;
	}

	const ACCEPTED_TYPES = ['video/mp4', 'video/webm', 'video/quicktime'];

	let booted = $state(false);

	let file = $state<File | null>(null);
	let dragOver = $state(false);
	let caption = $state('');
	let cwEnabled = $state(false);
	let cwText = $state('');
	let vttFile = $state<File | null>(null);
	let videoInput: HTMLInputElement | undefined = $state();

	let busy = $state(false);
	let progress = $state(0);
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
					if (me.csrf) setCsrf(me.csrf);
					sessionUser.set({
						username: me.username,
						csrf: me.csrf ?? '',
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

	function pick(f: File | null | undefined): void {
		error = null;
		if (!f) return;
		if (!ACCEPTED_TYPES.includes(f.type)) {
			fail('Unsupported format — use MP4, WebM or MOV.');
			return;
		}
		file = f;
	}

	function fail(message: string): void {
		error = message;
		showToast(message);
	}

	function clearFile(event: MouseEvent): void {
		event.stopPropagation();
		file = null;
		error = null;
		if (videoInput) videoInput.value = '';
	}

	function onDrop(event: DragEvent): void {
		dragOver = false;
		pick(event.dataTransfer?.files?.[0]);
	}

	function onKeydown(event: KeyboardEvent): void {
		if (event.key === 'Enter' || event.key === ' ') {
			event.preventDefault();
			videoInput?.click();
		}
	}

	function submit(event: SubmitEvent): void {
		event.preventDefault();
		if (!file || busy) return;

		const form = new FormData();
		form.append('file', file);
		const cap = caption.trim();
		if (cap) form.append('caption_html', cap);
		if (cwEnabled && cwText.trim()) form.append('cw_text', cwText.trim());
		if (vttFile) form.append('captions.vtt', vttFile, vttFile.name);

		busy = true;
		error = null;
		progress = 0;

		const xhr = new XMLHttpRequest();
		xhr.open('POST', '/api/v1/clips/upload');
		xhr.responseType = 'text';
		xhr.setRequestHeader('Accept', 'application/json');
		if (csrf.token) xhr.setRequestHeader('X-Toottok-CSRF', csrf.token);

		xhr.upload.onprogress = (ev) => {
			if (ev.lengthComputable) progress = Math.round((ev.loaded / ev.total) * 100);
		};

		xhr.onload = () => {
			busy = false;
			let body: UploadResponse | null = null;
			try {
				body = JSON.parse(xhr.responseText) as UploadResponse;
			} catch {
				body = null;
			}
			if (xhr.status >= 200 && xhr.status < 300) {
				clipId = body?.clip_id ?? body?.id ?? '';
				file = null;
				vttFile = null;
				caption = '';
				cwEnabled = false;
				cwText = '';
				if (videoInput) videoInput.value = '';
				showToast('Uploaded! Processing…');
			} else {
				fail(body?.detail ?? `Upload failed (${xhr.status} ${xhr.statusText}).`);
			}
		};

		xhr.onerror = () => {
			busy = false;
			fail('Network error during upload.');
		};

		xhr.send(form);
	}
</script>

<svelte:head>
	<title>Upload · TootTok</title>
</svelte:head>

{#if booted && $sessionUser}
	<div class="page safe-area">
		<h1>Upload a clip</h1>

		{#if clipId !== null}
			<section class="done" aria-live="polite">
				<h2>Upload complete</h2>
				<p class="clip-id">clip <code>{clipId || 'queued'}</code></p>
				<p class="processing"><span class="spinner"></span> processing…</p>
				<a class="btn ghost" href="/">Back to feed</a>
			</section>
		{:else}
			<form onsubmit={submit}>
				<input
					bind:this={videoInput}
					type="file"
					accept="video/mp4,video/webm,video/quicktime"
					hidden
					onchange={(e) => pick(e.currentTarget.files?.[0])}
				/>

				<div
					class="dropzone"
					class:over={dragOver}
					class:filled={!!file}
					role="button"
					tabindex="0"
					aria-label="Choose or drop a video file"
					onclick={() => videoInput?.click()}
					onkeydown={onKeydown}
					ondragover={(e) => {
						e.preventDefault();
						dragOver = true;
					}}
					ondragleave={() => (dragOver = false)}
					ondrop={(e) => {
						e.preventDefault();
						onDrop(e);
					}}
				>
					{#if file}
						<p class="fname">{file.name}</p>
						<p class="fsize">{(file.size / 1_048_576).toFixed(1)} MB</p>
						<button type="button" class="btn ghost small" onclick={clearFile}>Choose another</button>
					{:else}
						<svg
							viewBox="0 0 24 24"
							fill="none"
							stroke="currentColor"
							stroke-width="1.6"
							stroke-linecap="round"
							stroke-linejoin="round"
							aria-hidden="true"
						>
							<rect x="2" y="4" width="15" height="16" rx="2" />
							<path d="m22 8-5 4 5 4z" />
						</svg>
						<p class="drop-title">Drag &amp; drop your video</p>
						<p class="drop-sub">or tap to browse · MP4, WebM, MOV</p>
					{/if}
				</div>

				<div class="field">
					<label class="label" for="caption">Caption</label>
					<textarea
						id="caption"
						class="input"
						rows="3"
						maxlength="500"
						bind:value={caption}
						placeholder="Say something about your clip…"
					></textarea>
					<span class="counter" class:maxed={caption.length >= 500}>{caption.length}/500</span>
				</div>

				<div class="field">
					<label class="check">
						<input type="checkbox" bind:checked={cwEnabled} />
						<span>Content warning</span>
					</label>
					{#if cwEnabled}
						<input
							class="input"
							bind:value={cwText}
							maxlength="200"
							placeholder="e.g. flashing lights, violence"
							aria-label="Content warning text"
						/>
					{/if}
				</div>

				<div class="field">
					<label class="label" for="vtt">Captions file · WebVTT <span class="optional">(optional)</span></label>
					<input
						id="vtt"
						class="input file-input"
						type="file"
						accept=".vtt,text/vtt"
						onchange={(e) => {
							const f = e.currentTarget.files?.[0] ?? null;
							if (!f || f.name.toLowerCase().endsWith('.vtt') || f.type === 'text/vtt') {
								vttFile = f;
								error = null;
							} else {
								vttFile = null;
								e.currentTarget.value = '';
								fail('Captions file must be WebVTT (.vtt).');
							}
						}}
					/>
					{#if vttFile}
						<p class="fname small-fname">{vttFile.name}</p>
					{/if}
				</div>

				{#if error}
					<div class="banner error" role="alert">{error}</div>
				{/if}

				{#if busy}
					<div class="progress-wrap" aria-live="polite">
						<div class="progress"><div class="bar" style="width: {progress}%"></div></div>
						<span class="pct">{progress}%</span>
					</div>
				{/if}

				<button class="btn submit" type="submit" disabled={!file || busy}>
					{busy ? 'Uploading…' : 'Upload'}
				</button>
			</form>
		{/if}
	</div>
{:else}
	<div class="page safe-area gate">
		<span class="spinner"></span>
	</div>
{/if}

<style>
	.page {
		min-height: 100dvh;
		max-width: 520px;
		margin-inline: auto;
		padding: calc(var(--safe-top) + 1.25rem) 1rem calc(var(--safe-bottom) + 5.5rem);
	}

	h1 {
		margin: 0 0 1.25rem;
		font-size: 1.3rem;
	}

	form {
		display: flex;
		flex-direction: column;
		gap: 1.1rem;
	}

	.dropzone {
		display: flex;
		flex-direction: column;
		align-items: center;
		justify-content: center;
		gap: 0.4rem;
		min-height: 190px;
		padding: 1.25rem;
		text-align: center;
		border: 2px dashed rgba(255, 255, 255, 0.18);
		border-radius: 16px;
		background: var(--surface);
		cursor: pointer;
		transition:
			border-color 0.15s ease,
			background 0.15s ease;
	}

	.dropzone.over,
	.dropzone:focus-visible {
		border-color: var(--accent);
		background: rgba(255, 77, 109, 0.06);
		outline: none;
	}

	.dropzone.filled {
		border-style: solid;
		border-color: rgba(255, 255, 255, 0.12);
	}

	.dropzone svg {
		width: 42px;
		height: 42px;
		color: var(--accent);
		margin-bottom: 0.35rem;
	}

	.drop-title {
		margin: 0;
		font-weight: 600;
	}

	.drop-sub {
		margin: 0;
		font-size: 0.82rem;
		color: #8f8f9a;
	}

	.fname {
		margin: 0;
		font-weight: 600;
		overflow-wrap: anywhere;
	}

	.fsize {
		margin: 0;
		font-size: 0.82rem;
		color: #8f8f9a;
	}

	.small {
		padding: 0.35rem 0.9rem;
		font-size: 0.82rem;
		margin-top: 0.4rem;
	}

	.field {
		position: relative;
	}

	.check {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		font-size: 0.9rem;
		cursor: pointer;
	}

	.check input {
		width: 17px;
		height: 17px;
		accent-color: var(--accent);
	}

	.optional {
		font-weight: 400;
		color: #77777f;
	}

	.file-input {
		padding: 0.45rem 0.6rem;
		font-size: 0.85rem;
	}

	.file-input::file-selector-button {
		border: none;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.12);
		color: var(--text);
		font: inherit;
		font-size: 0.8rem;
		font-weight: 600;
		padding: 0.35rem 0.8rem;
		margin-right: 0.75rem;
		cursor: pointer;
	}

	.small-fname {
		margin: 0.4rem 0 0;
		font-size: 0.8rem;
	}

	.counter {
		position: absolute;
		right: 2px;
		top: 0;
		font-size: 0.72rem;
		color: #8f8f9a;
	}

	.counter.maxed {
		color: #ff8298;
	}

	.progress-wrap {
		display: flex;
		align-items: center;
		gap: 0.75rem;
	}

	.progress {
		flex: 1;
		height: 8px;
		border-radius: 999px;
		background: rgba(255, 255, 255, 0.1);
		overflow: hidden;
	}

	.bar {
		height: 100%;
		border-radius: 999px;
		background: var(--accent);
		transition: width 0.15s ease;
	}

	.pct {
		font-variant-numeric: tabular-nums;
		font-size: 0.82rem;
		color: #b9b9c3;
		min-width: 3ch;
		text-align: right;
	}

	.submit {
		align-self: stretch;
		justify-content: center;
	}

	.done {
		display: flex;
		flex-direction: column;
		align-items: center;
		gap: 0.6rem;
		text-align: center;
		background: var(--surface);
		border-radius: 16px;
		padding: 2rem 1.25rem;
	}

	.done h2 {
		margin: 0;
		font-size: 1.15rem;
	}

	.clip-id {
		margin: 0;
		color: #b9b9c3;
		font-size: 0.88rem;
	}

	.clip-id code {
		background: rgba(255, 255, 255, 0.08);
		border-radius: 6px;
		padding: 0.15rem 0.45rem;
	}

	.processing {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		margin: 0;
		color: #8f8f9a;
		font-size: 0.88rem;
	}

	.gate {
		display: grid;
		place-items: center;
	}
</style>
