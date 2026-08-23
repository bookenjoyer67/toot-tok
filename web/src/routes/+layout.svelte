<script lang="ts">
	import '../app.css';
	import { onMount } from 'svelte';
	import type { Snippet } from 'svelte';
	import NavBar from '$lib/components/NavBar.svelte';
	import Toast from '$lib/components/Toast.svelte';
	import { showToast } from '$lib/toast';

	let { children }: { children: Snippet } = $props();

	onMount(() => {
		if (!import.meta.env.PROD) return;
		if (!('serviceWorker' in navigator)) return;

		navigator.serviceWorker.register('/sw.js').catch(() => {});

		const onMessage = (event: MessageEvent): void => {
			if ((event.data as { type?: string } | null)?.type === 'OFFLINE') {
				showToast('You are offline');
			}
		};
		navigator.serviceWorker.addEventListener('message', onMessage);
		return () => navigator.serviceWorker.removeEventListener('message', onMessage);
	});
</script>

<NavBar />

{@render children()}

<Toast />
