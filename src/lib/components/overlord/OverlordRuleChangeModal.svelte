<script lang="ts">
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import type { OverlordRuleChange } from '$lib/stores/overlord.svelte';
  import Button from '$lib/components/ui/Button.svelte';

  /** Human approval prompt for a proposeRuleChanges batch (docs/overlord.md §10):
   *  per-change multiSelect (one bad change mustn't force rejecting four good ones)
   *  and VERBATIM directive text — the text is the payload; a summary of it is not
   *  something anyone can approve blind. */

  const batch = $derived(overlordStore.pendingRuleChanges);
  let unchecked = $state<Set<number>>(new Set());

  $effect(() => {
    if (batch) unchecked = new Set();
  });

  function toggle(i: number) {
    const next = new Set(unchecked);
    if (next.has(i)) next.delete(i);
    else next.add(i);
    unchecked = next;
  }

  function ruleName(key: string | undefined): string {
    if (!key) return '?';
    const r = preferencesStore.overlordRules.find((r) => r.id === key || r.default_id === key);
    return r ? r.name : key;
  }

  /** Verbatim payload lines for a change — the directive texts, never a paraphrase. */
  function payloadLines(c: OverlordRuleChange): string[] {
    const lines: string[] = [];
    if (c.op === 'create' && c.rule) {
      if (c.rule.when) lines.push(`when: ${JSON.stringify(c.rule.when)}`);
      for (const s of c.rule.sequence ?? []) lines.push(`${s.kind}: ${s.text}`);
      if (c.rule.cooldown !== undefined) lines.push(`cooldown: ${c.rule.cooldown}s`);
    } else if (c.op === 'update' && c.patch) {
      const { sequence, ...rest } = c.patch;
      for (const s of sequence ?? []) lines.push(`${s.kind}: ${s.text}`);
      for (const [k, v] of Object.entries(rest)) {
        if (k === 'guards') continue; // stripped on apply — never shown as approvable
        lines.push(`${k}: ${JSON.stringify(v)}`);
      }
    } else if (c.op === 'rescope') {
      lines.push(`scope: ${c.workspaces?.length ? c.workspaces.join(', ') : 'global'}`);
    }
    return lines;
  }

  function opLabel(c: OverlordRuleChange): string {
    switch (c.op) {
      case 'create': return `Create rule “${c.rule?.name ?? 'unnamed'}”`;
      case 'update': return `Update “${ruleName(c.rule_id)}”`;
      case 'rescope': return `Rescope “${ruleName(c.rule_id)}”`;
      case 'enable': return `Enable “${ruleName(c.rule_id)}”`;
      case 'disable': return `Disable “${ruleName(c.rule_id)}”`;
      case 'delete': return `Delete “${ruleName(c.rule_id)}”`;
    }
  }

  function applySelected() {
    if (!batch) return;
    const approved = batch.changes.map((_, i) => i).filter((i) => !unchecked.has(i));
    overlordStore.resolveRuleChanges(batch.id, approved);
  }

  function rejectAll() {
    if (!batch) return;
    overlordStore.resolveRuleChanges(batch.id, []);
  }
</script>

{#if batch}
  <div class="modal-backdrop">
    <div class="modal">
      <h2>♔ Overlord proposes rule changes</h2>
      <p class="rationale">{batch.rationale}</p>
      <div class="changes">
        {#each batch.changes as change, i (i)}
          <label class="change" class:rejected={unchecked.has(i)}>
            <input type="checkbox" checked={!unchecked.has(i)} onchange={() => toggle(i)} />
            <div class="change-body">
              <div class="change-op">{opLabel(change)}</div>
              {#each payloadLines(change) as line (line)}
                <div class="change-line">{line}</div>
              {/each}
            </div>
          </label>
        {/each}
      </div>
      <p class="note">Unchecked changes are rejected — Overlord won't re-propose them. Guards are never changed by proposals.</p>
      <div class="actions">
        <Button variant="secondary" onclick={rejectAll}>Reject all</Button>
        <Button variant="primary" onclick={applySelected}>Apply selected</Button>
      </div>
    </div>
  </div>
{/if}

<style>
  .modal-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    width: min(640px, 90vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    padding: 20px;
    color: var(--fg);
  }

  h2 { margin: 0 0 8px; font-size: 1.077rem; }

  .rationale {
    color: var(--fg-dim);
    font-size: 0.923rem;
    margin: 0 0 12px;
  }

  .changes { overflow-y: auto; flex: 1; }

  .change {
    display: flex;
    gap: 10px;
    padding: 8px 10px;
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    margin-bottom: 8px;
    cursor: pointer;
    align-items: flex-start;
  }
  .change.rejected { opacity: 0.5; }
  .change input { margin-top: 3px; }

  .change-op { font-weight: 600; font-size: 0.923rem; margin-bottom: 4px; }

  .change-line {
    font-family: Menlo, monospace;
    font-size: 0.846rem;
    color: var(--fg-dim);
    background: var(--bg-dark);
    border-radius: 4px;
    padding: 2px 8px;
    margin: 2px 0;
    white-space: pre-wrap;
    word-break: break-word;
  }

  .note { color: var(--fg-dim); font-size: 0.769rem; margin: 8px 0 12px; }

  .actions { display: flex; justify-content: flex-end; gap: 8px; }
</style>
