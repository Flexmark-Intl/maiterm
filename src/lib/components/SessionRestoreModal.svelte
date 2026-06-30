<script lang="ts">
  import Button from '$lib/components/ui/Button.svelte';

  interface Props {
    /** Total number of tabs queued for restore. */
    total: number;
    /** How many tabs have finished restoring so far. */
    done: number;
    /** Human-readable label for the tab currently being restored ("workspace › tab"). */
    currentLabel: string;
    /** True once the user has asked to stop — disables the button and shows "Stopping…". */
    cancelling?: boolean;
    /** Stop the restore. Already-restored tabs stay live; the rest resume on click. */
    oncancel: () => void;
  }

  let { total, done, currentLabel, cancelling = false, oncancel }: Props = $props();

  const pct = $derived(total > 0 ? Math.min(100, Math.round((done / total) * 100)) : 0);

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      if (!cancelling) oncancel();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop">
  <div class="modal" role="dialog" aria-modal="true" aria-label="Restoring session">
    <div class="header">
      <span class="spinner" aria-hidden="true"></span>
      <h2>Restoring session</h2>
    </div>

    <p class="sub">Bringing your terminals back, one at a time.</p>

    <div class="progress">
      <div class="track">
        <div class="fill" style="width: {pct}%"></div>
      </div>
      <div class="counts">{done} / {total} tabs</div>
    </div>

    <p class="current" title={currentLabel}>{currentLabel || '…'}</p>

    <div class="footer">
      <Button variant="secondary" onclick={oncancel} disabled={cancelling}>
        {cancelling ? 'Stopping…' : 'Cancel'}
      </Button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1100;
  }

  .modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    width: 420px;
    max-width: calc(100vw - 48px);
    padding: 20px 22px 16px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    animation: pop 0.12s ease-out;
  }

  @keyframes pop {
    from { transform: scale(0.97); opacity: 0; }
    to   { transform: scale(1); opacity: 1; }
  }

  .header {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .header h2 {
    font-size: 1.154rem;
    font-weight: 600;
    color: var(--fg);
    margin: 0;
  }

  .spinner {
    width: 16px;
    height: 16px;
    border-radius: 50%;
    border: 2px solid var(--bg-light);
    border-top-color: var(--accent);
    animation: spin 0.7s linear infinite;
    flex-shrink: 0;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .sub {
    margin: 6px 0 16px;
    font-size: 0.846rem;
    color: var(--fg-dim);
  }

  .progress {
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .track {
    flex: 1;
    height: 6px;
    background: var(--bg-dark);
    border-radius: 3px;
    overflow: hidden;
  }

  .fill {
    height: 100%;
    background: var(--accent);
    border-radius: 3px;
    transition: width 0.2s ease;
  }

  .counts {
    font-size: 0.846rem;
    color: var(--fg-dim);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }

  .current {
    margin: 10px 0 16px;
    font-size: 0.846rem;
    font-family: monospace;
    color: var(--fg-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    min-height: 1.2em;
  }

  .footer {
    display: flex;
    justify-content: flex-end;
  }
</style>
