<script lang="ts">
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import type { OverlordRuleChange } from '$lib/stores/overlord.svelte';
  import { describeCondition, describeGate } from '$lib/overlord/format';
  import '$lib/overlord/deck.css';

  /**
   * Rule-change authorization (docs/overlord.md §10).
   *
   * Overlord may propose how it supervises; only the human may grant it. Two things this
   * screen must never do: hide a field that changes behaviour behind a summary, and force
   * all-or-nothing on a batch (one bad change would make the human reject four good ones).
   * So: per-change approval, and the payload rendered VERBATIM — the directive text IS the
   * thing being authorized, and "tightened the compaction wording" is not something anyone
   * can approve blind.
   */

  const batch = $derived(overlordStore.pendingRuleChanges);
  let unchecked = $state<Set<number>>(new Set());

  $effect(() => { if (batch) unchecked = new Set(); });

  const approvedCount = $derived(batch ? batch.changes.length - unchecked.size : 0);

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
    }
  }

  function applySelected() {
    if (!batch) return;
    overlordStore.resolveRuleChanges(batch.id, batch.changes.map((_, i) => i).filter((i) => !unchecked.has(i)));
  }

  function rejectAll() {
    if (!batch) return;
    overlordStore.resolveRuleChanges(batch.id, []);
  }

  // Escape rejects (the safe default — never let a stray key grant authority);
  // Cmd/Ctrl+Enter applies the current selection.
  function onKeydown(e: KeyboardEvent) {
    if (!batch) return;
    if (e.key === 'Escape') { e.preventDefault(); rejectAll(); }
    else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) { e.preventDefault(); applySelected(); }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if batch}
  <div class="scrim">
    <div class="slip ov-bracket ov-grain" role="dialog" aria-modal="true" aria-label="Overlord rule change authorization">

      <header class="slip-head">
        <div class="seal">♔</div>
        <div class="slip-title">
          <p class="ov-label">Authorization required</p>
          <p class="ov-label-lead">Overlord proposes {batch.changes.length} rule change{batch.changes.length === 1 ? '' : 's'}</p>
        </div>
        <span class="ov-mono slip-count">{approvedCount}<span class="of">/{batch.changes.length}</span></span>
      </header>

      <div class="ov-tickrule"></div>

      <p class="rationale">{batch.rationale}</p>

      <div class="changes">
        {#each batch.changes as change, i (i)}
          {@const meta = opVerb(change)}
          {@const rejected = unchecked.has(i)}
          <div class="change" class:rejected class:danger={meta.danger}>
            <button class="stamp" class:on={!rejected} onclick={() => toggle(i)}
                    aria-pressed={!rejected} aria-label="{rejected ? 'Approve' : 'Reject'} this change">
              <span class="stamp-mark">{rejected ? '✕' : '✓'}</span>
              <span class="ov-label stamp-word">{rejected ? 'reject' : 'approve'}</span>
            </button>

            <div class="change-body">
              <div class="change-head">
                <span class="ov-mono change-idx">{String(i + 1).padStart(2, '0')}</span>
                <span class="change-verb" class:danger={meta.danger}>{meta.verb}</span>
                <span class="change-subject">{meta.subject}</span>
              </div>

              {#each payloadLines(change) as line, j (j)}
                {#if line.kind === 'text'}
                  <div class="line-text">
                    <span class="ov-label line-label">{line.label}</span>
                    <div class="ov-verbatim">{line.value}</div>
                  </div>
                {:else}
                  <div class="line-meta" class:danger={line.kind === 'danger'}>
                    <span class="ov-label line-label">{line.label}</span>
                    <span class="ov-mono line-value">{line.value}</span>
                  </div>
                {/if}
              {/each}
            </div>
          </div>
        {/each}
      </div>

      <footer class="slip-foot">
        <p class="note">
          Rejected changes are remembered — Overlord won't pitch them again this session.
          <strong>Guards are never proposable</strong>, so nothing here can widen what Overlord
          is allowed to do.
        </p>
        <div class="actions">
          <button class="ov-btn ov-btn-danger" onclick={rejectAll}>Reject all</button>
          <button class="ov-btn ov-btn-primary" onclick={applySelected} disabled={approvedCount === 0}>
            Apply {approvedCount || ''}
          </button>
        </div>
      </footer>
    </div>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    z-index: 1000;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 28px;
    background: color-mix(in srgb, var(--bg-dark) 72%, transparent);
    backdrop-filter: blur(7px) saturate(0.85);
    animation: ovIn 0.2s ease both;
  }

  .slip {
    position: relative;
    width: min(680px, 100%);
    max-height: 84vh;
    display: flex;
    flex-direction: column;
    background:
      radial-gradient(120% 55% at 50% 0%,
        color-mix(in srgb, var(--accent) 10%, transparent) 0%, transparent 62%),
      var(--ov-panel);
    border: 1px solid color-mix(in srgb, var(--ov-live) 35%, var(--ov-hair));
    border-radius: var(--ov-radius);
    box-shadow: 0 24px 70px -12px color-mix(in srgb, var(--bg-dark) 92%, transparent);
    color: var(--ov-ink);
    animation: ovIn 0.34s cubic-bezier(0.16, 0.84, 0.32, 1) both;
  }

  .slip-head { display: flex; align-items: center; gap: 13px; padding: 16px 20px 13px; }
  .seal {
    width: 34px; height: 34px;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 1.1rem;
    color: var(--ov-live);
    border: 1px solid color-mix(in srgb, var(--ov-live) 45%, transparent);
    border-radius: 50%;
    flex-shrink: 0;
    text-shadow: 0 0 12px color-mix(in srgb, var(--ov-live) 55%, transparent);
  }
  .slip-title { flex: 1; display: flex; flex-direction: column; gap: 6px; }
  .slip-count { font-size: 1.3rem; line-height: 1; color: var(--ov-live); }
  .slip-count .of { font-size: 0.8rem; color: var(--ov-ink-dim); }

  .rationale {
    padding: 14px 20px 4px;
    color: var(--ov-ink-mid);
    font-size: 0.93rem;
    line-height: 1.65;
    font-style: italic;
  }

  .changes { overflow-y: auto; padding: 10px 20px 4px; flex: 1; }

  .change {
    display: flex;
    gap: 12px;
    padding: 12px 0;
    border-top: 1px solid var(--ov-hair);
    transition: opacity 0.16s ease;
  }
  .change:first-child { border-top: none; }
  .change.rejected { opacity: 0.42; }

  .stamp {
    width: 60px;
    flex-shrink: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 4px;
    padding: 7px 0;
    border: 1px solid var(--ov-hair-strong);
    border-radius: 2px;
    color: var(--ov-ink-dim);
    align-self: flex-start;
    transition: all 0.16s ease;
  }
  .stamp.on {
    color: var(--ov-ok);
    border-color: color-mix(in srgb, var(--ov-ok) 55%, transparent);
    background: color-mix(in srgb, var(--ov-ok) 12%, transparent);
  }
  .stamp-mark { font-size: 0.95rem; line-height: 1; }
  .stamp-word { font-size: 0.62rem; letter-spacing: 0.14em; color: inherit; }

  .change-body { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 7px; }
  .change-head { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .change-idx { font-size: 0.72rem; color: var(--ov-ink-dim); }
  .change-verb {
    font-family: var(--ov-face);
    font-size: 0.8rem;
    font-weight: 700;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--ov-live);
  }
  .change-verb.danger { color: var(--ov-critical); }
  .change-subject { font-weight: 600; font-size: 0.95rem; }

  .line-text { display: flex; flex-direction: column; gap: 4px; }
  .line-label { flex-shrink: 0; min-width: 62px; }
  .line-meta { display: flex; align-items: baseline; gap: 9px; font-size: 0.83rem; }
  .line-value { color: var(--ov-ink-dim); word-break: break-word; }
  .line-meta.danger .line-value { color: color-mix(in srgb, var(--ov-critical) 85%, var(--fg)); }
  .line-meta.danger .line-label { color: var(--ov-critical); }

  .slip-foot {
    display: flex;
    align-items: flex-end;
    gap: 18px;
    padding: 14px 20px 16px;
    border-top: 1px solid var(--ov-hair);
  }
  .note {
    flex: 1;
    font-size: 0.79rem;
    line-height: 1.55;
    color: var(--ov-ink-dim);
  }
  .note strong { color: var(--ov-ink-mid); font-weight: 600; }
  .actions { display: flex; gap: 7px; flex-shrink: 0; }
</style>
