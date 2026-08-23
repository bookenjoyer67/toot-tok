import { writable } from 'svelte/store';

export const toast = writable<string | null>(null);

let timer: ReturnType<typeof setTimeout> | undefined;

export function showToast(message: string, durationMs = 2500): void {
	toast.set(message);
	clearTimeout(timer);
	timer = setTimeout(() => toast.set(null), durationMs);
}
