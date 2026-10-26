<!--
  The Overlord agent's proposed rule changes, for the human to approve (docs/overlord.md §10):
  one card per batch, on the Overlord tab's corner and in the Loom's Decisions — never a modal
  over the window. A modal opened by itself, and Escape anywhere rejected the whole batch.

  Overlord may propose how it supervises; only the human may grant it. Two things this card
  must never do: hide a field that changes behaviour behind a summary, and force all-or-nothing
  on a batch (one bad change would make the human reject four good ones). So: per-change
  approval, and the payload rendered VERBATIM — the directive text IS the thing being
  authorized. No key decides anything here; only a click on Apply or Reject all.
-->
<script lang="ts">
  import { AGENT_RULE_GUARDS, overlordStore } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import type { OverlordRuleChange, PendingRuleChangeBatch } from '$lib/stores/overlord.svelte';
  import type { OverlordRule } from '$lib/tauri/types';
  import { describeCondition, describeGate, guardsForCondition, ruleWarnings } from '$lib/overlord/format';

  interface Props {
    batch: PendingRuleChangeBatch;
    /** Receives one line saying what happened, for the surface to show. */
    onnote?: (text: string) => void;
    /** When the list holding this card last changed (ms): anything above it moving can put a
     *  card under the pointer (ScriptApprovalCard's rule). */
    listChangedAt?: number;
  }
  let { batch, onnote, listChangedAt = 0 }: Props = $props();

  /** Nothing is decided in a card's first moments, counted from the latest of: the batch being
   *  proposed, this card appearing, and its list last changing. */
  const FRESH_MS = 1500;
  const shownAt = Date.now();
  const isFresh = () => {
    const age = Date.now() - overlordStore.pendingRuleChangesAt;
    const settled = Date.now() - Math.max(shownAt, listChangedAt);
    return (age >= 0 && age < FRESH_MS) || settled < FRESH_MS;
  };

  let unchecked = $state<Set<number>>(new Set());
  const approvedCount = $derived(batch.changes.length - unchecked.size);

  function toggle(i: number) {
    const next = new Set(unchecked);
    if (next.has(i)) next.delete(i); else next.add(i);
    unchecked = next;
  }

  function ruleName(key: string | undefined): string {
    if (!key) return 'unknown rule';
    const r = preferencesStore.overlordRules.find((r) => r.id === key || r.default_id === key);
    return r ? r.name : key;
  }

  function wsName(id: string): string {
    for (const w of workspacesStore.workspaces) if (w.id === id) return w.name;
    return id.slice(0, 8);
  }

  /**
   * What would be wrong with this rule ONCE APPLIED — not with the rule as pitched.
   *
   * The agent doesn't choose guards (§10 field tiers), so the rule the human approves is
   * the agent's condition and steps under maiTerm's guards, and that combination can be
   * dead on arrival. Rebuild the effective rule exactly as `applyRuleChange` would, then ask.
   */
  function warningsFor(c: OverlordRuleChange): string[] {
    if (c.op === 'create' && c.rule?.when && c.rule.sequence?.length) {
      return ruleWarnings({
        ...c.rule,
        guards: guardsForCondition(AGENT_RULE_GUARDS, c.rule.when.event),
      } as OverlordRule);
    }
    if (c.op === 'update' && c.patch) {
      const base = preferencesStore.overlordRules.find((r) => r.id === c.rule_id || r.default_id === c.rule_id);
      if (!base) return [];
      const { guards: _g, ...patch } = c.patch;
      const guards = patch.when ? guardsForCondition(base.guards, patch.when.event) : base.guards;
      return ruleWarnings({ ...base, ...patch, guards } as OverlordRule);
    }
    return [];
  }

  interface Line { kind: 'text' | 'meta' | 'danger'; label: string; value: string; }

  /** Verbatim payload lines. Anything that alters OTHER rules is marked danger so it
   *  can never be approved without being seen (a hidden `supersedes` silently disables
   *  the human's own rules). */
  function payloadLines(c: OverlordRuleChange): Line[] {
    const lines: Line[] = [];
    if (c.op === 'create' && c.rule) {
      if (c.rule.when) lines.push({ kind: 'meta', label: 'fires when', value: describeCondition(c.rule.when) });
      for (const s of c.rule.sequence ?? []) {
        lines.push({ kind: 'text', label: s.kind === 'slash' ? 'runs' : 'types', value: s.text });
        lines.push({ kind: 'meta', label: 'then', value: describeGate(s.await) });
      }
      if (c.rule.cooldown !== undefined) lines.push({ kind: 'meta', label: 'cooldown', value: `${c.rule.cooldown}s per tab` });
      lines.push({
        kind: 'meta',
        label: 'scope',
        value: c.rule.workspaces?.length ? c.rule.workspaces.map(wsName).join(', ') : 'every workspace',
      });
      if (c.rule.enabled === false) lines.push({ kind: 'meta', label: 'state', value: 'created disabled' });
      if (c.rule.supersedes?.length) {
        lines.push({
          kind: 'danger',
          label: 'disables',
          value: `${c.rule.supersedes.map(ruleName).join(', ')} — these stop running wherever this rule applies`,
        });
      }
    } else if (c.op === 'update' && c.patch) {
      const { sequence, guards: _g, ...rest } = c.patch;
      for (const s of sequence ?? []) lines.push({ kind: 'text', label: s.kind === 'slash' ? 'runs' : 'types', value: s.text });
      for (const [k, v] of Object.entries(rest)) {
        const val = k === 'when' && c.patch.when ? describeCondition(c.patch.when) : JSON.stringify(v);
        lines.push({ kind: k === 'supersedes' ? 'danger' : 'meta', label: k, value: val });
      }
    } else if (c.op === 'rescope') {
      lines.push({
        kind: 'meta',
        label: 'new scope',
        value: c.workspaces?.length ? c.workspaces.map(wsName).join(', ') : 'every workspace',
      });
    } else if (c.op === 'delete') {
      lines.push({ kind: 'danger', label: 'removes', value: 'this rule stops supervising anything, permanently' });
    } else if (c.op === 'playbook_add') {
      // Verbatim: this sentence is what the agent will answer tabs' decisions with, unasked.
      lines.push({ kind: 'text', label: 'standing answer', value: (c.text ?? '').trim() });
    } else if (c.op === 'playbook_remove') {
      lines.push({ kind: 'text', label: 'removes', value: (c.text ?? '').trim() });
    }
    return lines;
  }

  function opVerb(c: OverlordRuleChange): { verb: string; subject: string; danger: boolean } {
    switch (c.op) {
      case 'create': return { verb: 'Create', subject: c.rule?.name ?? 'unnamed rule', danger: false };
      case 'update': return { verb: 'Rewrite', subject: ruleName(c.rule_id), danger: false };
      case 'rescope': return { verb: 'Rescope', subject: ruleName(c.rule_id), danger: false };
      case 'enable': return { verb: 'Enable', subject: ruleName(c.rule_id), danger: false };
      case 'disable': return { verb: 'Disable', subject: ruleName(c.rule_id), danger: true };
      case 'delete': return { verb: 'Delete', subject: ruleName(c.rule_id), danger: true };
      case 'playbook_add': return { verb: 'Teach', subject: 'escalation playbook', danger: false };
      case 'playbook_remove': return { verb: 'Unlearn', subject: 'escalation playbook entry', danger: false };
    }
  }

  function decide(approvedIdx: number[], e: MouseEvent) {
    if (e.detail > 1) return;
    if (isFresh()) {
      onnote?.('This just appeared or moved. Read it, then choose.');
      return;
    }
    if (overlordStore.pendingRuleChanges?.id !== batch.id) {
      onnote?.('It was already answered.');
      return;
    }
    overlordStore.resolveRuleChanges(batch.id, approvedIdx);
    const n = approvedIdx.length;
    const total = batch.changes.length;
    onnote?.(
      n === 0
        ? 'Rejected. Overlord will not pitch these again this session.'
        : n === total
          ? `Applied ${n === 1 ? 'the change' : `all ${n} changes`}.`
          : `Applied ${n} of ${total}; the rest were rejected.`,
    );
  }
</script>

<div class="rules">
  <p class="q">Allow {batch.changes.length === 1 ? 'this rule change' : `these ${batch.changes.length} rule changes`}?</p>
  {#if batch.rationale}
    <!-- The agent's words, set apart from maiTerm's own description below. -->
    <blockquote class="reason">{batch.rationale}</blockquote>
  {/if}
  <p class="who">Proposed by <strong>Overlord</strong> · tick each one you allow</p>

  <div class="changes">
    {#each batch.changes as change, i (i)}
      {@const meta = opVerb(change)}
      {@const rejected = unchecked.has(i)}
      <div class="change" class:rejected>
        <label class="head">
          <input type="checkbox" checked={!rejected} onchange={() => toggle(i)} />
          <span class="verb" class:danger={meta.danger}>{meta.verb}</span>
          <span class="subject">{meta.subject}</span>
        </label>
        {#each payloadLines(change) as line, j (j)}
          {#if line.kind === 'text'}
            <div class="line">
              <span class="label">{line.label}</span>
              <div class="verbatim">{line.value}</div>
            </div>
          {:else}
            <div class="line meta" class:danger={line.kind === 'danger'}>
              <span class="label">{line.label}</span>
              <span class="value">{line.value}</span>
            </div>
          {/if}
        {/each}
        {#each warningsFor(change) as w (w)}
          <p class="warn">{w}</p>
        {/each}
      </div>
    {/each}
  </div>

  <p class="note">
    A rejected change is remembered: Overlord won't pitch it again this session. Guards are
    never proposable, so nothing here can widen what Overlord is allowed to do.
  </p>
  <div class="choices">
    <button class="allow" disabled={approvedCount === 0} onclick={(e) => decide(batch.changes.map((_, i) => i).filter((i) => !unchecked.has(i)), e)}>
      {approvedCount === batch.changes.length ? 'Apply' : `Apply ${approvedCount} of ${batch.changes.length}`}
    </button>
    <button class="deny" onclick={(e) => decide([], e)}>Reject all</button>
  </div>
</div>

<style>
  .rules {
    -webkit-user-select: text;
    user-select: text;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
    border-left: 3px solid var(--yellow, #e0af68);
    background: color-mix(in srgb, var(--yellow, #e0af68) 7%, transparent);
    border-radius: 0 8px 8px 0;
    font-size: 12.5px;
    color: var(--fg);
    min-width: 0;
  }
  .q { margin: 0; font-size: 15px; font-weight: 600; line-height: 1.3; }
  .who { margin: 0; color: var(--fg-dim); overflow-wrap: anywhere; }
  .who strong { color: var(--fg); font-weight: 600; }
  .reason {
    margin: 0;
    padding: 4px 10px;
    border-left: 2px solid var(--bg-light);
    color: var(--fg-dim);
    font-style: italic;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .changes { display: flex; flex-direction: column; gap: 10px; }
  .change {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding-top: 8px;
    border-top: 1px solid var(--bg-light);
    min-width: 0;
  }
  .change.rejected > :not(.head) { opacity: 0.45; }
  .head { display: flex; align-items: baseline; flex-wrap: wrap; gap: 6px; cursor: pointer; }
  .head input { align-self: center; margin: 0; accent-color: var(--accent); }
  .verb { font-size: 11px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; color: var(--accent); }
  .verb.danger { color: var(--red, #f7768e); }
  .subject { font-weight: 600; overflow-wrap: anywhere; }
  .line { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .line.meta { flex-direction: row; flex-wrap: wrap; align-items: baseline; gap: 2px 8px; }
  .label { font-size: 10.5px; letter-spacing: 0.06em; text-transform: uppercase; color: var(--fg-dim); flex-shrink: 0; }
  .value { color: var(--fg-dim); overflow-wrap: anywhere; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11.5px; }
  .line.danger .label,
  .line.danger .value { color: var(--red, #f7768e); }
  .verbatim {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11.5px;
    line-height: 1.5;
    background: color-mix(in srgb, var(--bg-dark) 65%, transparent);
    border-left: 2px solid var(--bg-light);
    padding: 4px 8px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }
  /* "This never fires": the one thing here the human cannot see for themselves, since the
     guards it contradicts are maiTerm's, not shown. */
  .warn {
    margin: 0;
    padding: 4px 8px;
    border-left: 2px solid var(--yellow, #e0af68);
    color: var(--yellow, #e0af68);
    font-size: 11.5px;
    line-height: 1.45;
  }
  .note { margin: 0; color: var(--fg-dim); font-size: 11.5px; line-height: 1.45; }
  .choices { display: flex; flex-wrap: wrap; gap: 6px; }
  .choices button {
    font: inherit;
    font-weight: 600;
    padding: 5px 14px;
    border-radius: 6px;
    cursor: pointer;
  }
  .allow { background: var(--accent); border: 1px solid var(--accent); color: var(--bg-dark); }
  .allow:hover:not(:disabled) { filter: brightness(1.1); }
  .deny { background: none; border: 1px solid var(--bg-light); color: var(--fg); }
  .deny:hover:not(:disabled) { background: var(--bg-light); }
  .choices button:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .choices button:disabled { opacity: 0.5; cursor: default; }
</style>
