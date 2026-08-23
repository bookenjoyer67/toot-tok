<script lang="ts">
	interface Props {
		value?: string;
		placeholder?: string;
		onquery?: (query: string) => void;
	}

	let { value = $bindable(''), placeholder = 'Search', onquery }: Props = $props();

	const DEBOUNCE_MS = 300;

	let timer: ReturnType<typeof setTimeout> | undefined;
	let inputEl: HTMLInputElement | undefined = $state();

	$effect(() => {
		return () => clearTimeout(timer);
	});

	function emit(query: string): void {
		onquery?.(query.trim());
	}

	function onInput(): void {
		clearTimeout(timer);
		timer = setTimeout(() => emit(value), DEBOUNCE_MS);
	}

	function onSubmit(event: SubmitEvent): void {
		event.preventDefault();
		clearTimeout(timer);
		emit(value);
		inputEl?.blur();
	}

	function clear(): void {
		clearTimeout(timer);
		value = '';
		emit('');
		inputEl?.focus();
	}
</script>

<form class="pill" role="search" onsubmit={onSubmit}>
	<svg
		class="glass"
		viewBox="0 0 24 24"
		fill="none"
		stroke="currentColor"
		stroke-width="2"
		stroke-linecap="round"
		stroke-linejoin="round"
		aria-hidden="true"
	>
		<circle cx="11" cy="11" r="8" />
		<line x1="21" y1="21" x2="16.65" y2="16.65" />
	</svg>
	<input
		bind:this={inputEl}
		bind:value
		oninput={onInput}
		type="search"
		{placeholder}
		aria-label="Search"
		autocomplete="off"
		spellcheck="false"
		enterkeyhint="search"
	/>
	{#if value}
		<button type="button" class="clear" onclick={clear} aria-label="Clear search">
			<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.4" stroke-linecap="round" aria-hidden="true">
				<line x1="6" y1="6" x2="18" y2="18" />
				<line x1="18" y1="6" x2="6" y2="18" />
			</svg>
		</button>
	{/if}
</form>

<style>
	.pill {
		display: flex;
		align-items: center;
		gap: 8px;
		width: 100%;
		background: rgba(255, 255, 255, 0.08);
		border: 1px solid transparent;
		border-radius: 999px;
		padding: 0.45rem 0.9rem;
		transition: border-color 0.15s ease;
	}

	.pill:focus-within {
		border-color: rgba(255, 255, 255, 0.28);
	}

	.glass {
		width: 18px;
		height: 18px;
		flex-shrink: 0;
		color: #8f8f9a;
	}

	input {
		flex: 1;
		min-width: 0;
		border: none;
		background: transparent;
		color: var(--text);
		font: inherit;
		font-size: 0.95rem;
		outline: none;
	}

	input::placeholder {
		color: #77777f;
	}

	input::-webkit-search-cancel-button {
		display: none;
	}

	.clear {
		display: grid;
		place-items: center;
		width: 22px;
		height: 22px;
		flex-shrink: 0;
		border: none;
		border-radius: 50%;
		background: rgba(255, 255, 255, 0.14);
		color: #cfcfd6;
		cursor: pointer;
		padding: 0;
	}

	.clear svg {
		width: 11px;
		height: 11px;
	}

	.clear:hover {
		background: rgba(255, 255, 255, 0.22);
	}
</style>
