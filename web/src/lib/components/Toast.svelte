<script lang="ts">
	import { toast } from '$lib/toast';
</script>

{#if $toast}
	<div class="toast" role="status" aria-live="polite">{$toast}</div>
{/if}

<style>
	.toast {
		position: fixed;
		left: 50%;
		bottom: calc(var(--safe-bottom) + 76px);
		transform: translateX(-50%);
		z-index: 90;
		max-width: min(90vw, 320px);
		padding: 0.55rem 1.05rem;
		border-radius: 999px;
		background: rgba(22, 22, 29, 0.94);
		border: 1px solid rgba(255, 255, 255, 0.09);
		box-shadow: 0 10px 28px rgba(0, 0, 0, 0.5);
		color: var(--text);
		font-size: 0.85rem;
		font-weight: 600;
		text-align: center;
		pointer-events: none;
		backdrop-filter: blur(10px);
		-webkit-backdrop-filter: blur(10px);
		animation: toast-in 0.2s ease-out;
	}

	@keyframes toast-in {
		from {
			opacity: 0;
			transform: translate(-50%, 8px);
		}
		to {
			opacity: 1;
			transform: translate(-50%, 0);
		}
	}
</style>
