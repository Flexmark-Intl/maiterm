<script lang="ts">
  import Tooltip from '$lib/components/Tooltip.svelte';

  interface Props {
    color?: 'accent' | 'green' | 'red' | 'yellow' | 'dim';
    tooltip?: string;
    pulse?: boolean;
    hollow?: boolean;
  }

  let { color = 'accent', tooltip, pulse = false, hollow = false }: Props = $props();
</script>

{#if tooltip}
  <Tooltip text={tooltip}>
    <span class="status-dot {color}" class:pulse class:hollow></span>
  </Tooltip>
{:else}
  <span class="status-dot {color}" class:pulse class:hollow></span>
{/if}

<style>
  .status-dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex-shrink: 0;
    box-sizing: border-box;
  }

  .accent { background: var(--accent); }
  .green { background: var(--green); }
  .red { background: var(--red); }
  .yellow { background: var(--yellow); }
  .dim { background: var(--fg-dim); }

  /* Hollow: ring instead of filled dot (e.g. a "done & seen" agent). */
  .hollow { background: transparent; border-width: 1.5px; border-style: solid; }
  .hollow.accent { border-color: var(--accent); }
  .hollow.green { border-color: var(--green); }
  .hollow.red { border-color: var(--red); }
  .hollow.yellow { border-color: var(--yellow); }
  .hollow.dim { border-color: var(--fg-dim); }

  .pulse {
    animation: status-dot-pulse 1.5s ease-in-out infinite;
  }

  @keyframes status-dot-pulse {
    0%, 100% { opacity: 1; transform: scale(1); }
    50% { opacity: 0.3; transform: scale(0.7); }
  }
</style>
