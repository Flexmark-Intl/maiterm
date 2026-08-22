<script lang="ts">
  import { slide } from 'svelte/transition';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { DEFAULT_OVERLORD_RULES, seedDefaultOverlordRules } from '$lib/overlord/defaults';
  import * as commands from '$lib/tauri/commands';
  import Icon from '$lib/components/Icon.svelte';
  import type {
    OverlordCondition,
    OverlordGate,
    OverlordRule,
    OverlordStep,
    OverlordAgentStateName,
  } from '$lib/tauri/types';

  /** Overlord rules editor (docs/overlord.md §5) — the preferences surface for the
   *  ruleset. The guards block is deliberately part of THIS UI and unreachable from
   *  Overlord's proposeRuleChanges MCP surface (§10 field tiers). */

  let expandedId = $state<string | null>(null);
  let confirmDeleteId = $state<string | null>(null);

  // All workspaces across windows, for scope checkboxes (prefs is its own window,
  // so the per-window workspacesStore is empty here).
  let allWorkspaces = $state<{ id: string; name: string }[]>([]);
  commands.getAppData().then((data) => {
    allWorkspaces = data.windows.flatMap((w) =>
      w.workspaces.filter((ws) => !ws.overlord).map((ws) => ({ id: ws.id, name: ws.name })),
    );
  }).catch(() => {});

  const rules = $derived(preferencesStore.overlordRules);

  function setRules(next: OverlordRule[]) {
    preferencesStore.setOverlordRules(next);
  }

  function updateRule(id: string, patch: Partial<OverlordRule>, markModified = true) {
    setRules(rules.map((r) => {
      if (r.id !== id) return r;
      const next = { ...r, ...patch };
      if (markModified && r.default_id) next.user_modified = true;
      return next;
    }));
  }

  function addRule() {
    const rule: OverlordRule = {
      id: crypto.randomUUID(),
      name: 'New rule',
      description: null,
      enabled: false,
      workspaces: [],
      cooldown: 1800,
      origin: 'user',
      when: { event: 'context_pct', at_or_above: 55 },
      guards: {
        agent_state: ['idle'],
        min_quiet_ms: 3000,
        require_live_repl: true,
        max_per_hour: 1,
        only_if_no_outstanding: true,
      },
      sequence: [
        { kind: 'process', text: '', await: { until: 'turn_end' }, timeout_seconds: 600, on_timeout: 'abort' },
      ],
    };
    setRules([rule, ...rules]);
    expandedId = rule.id;
  }

  function deleteRule(rule: OverlordRule) {
    if (confirmDeleteId !== rule.id) {
      confirmDeleteId = rule.id;
      return;
    }
    confirmDeleteId = null;
    if (rule.default_id) {
      preferencesStore.setHiddenDefaultOverlordRules([
        ...preferencesStore.hiddenDefaultOverlordRules,
        rule.default_id,
      ]);
    }
    setRules(rules.filter((r) => r.id !== rule.id));
  }

  function restoreDefault(rule: OverlordRule) {
    if (!rule.default_id) return;
    const tmpl = DEFAULT_OVERLORD_RULES[rule.default_id];
    if (!tmpl) return;
    updateRule(rule.id, {
      ...structuredClone(tmpl),
      user_modified: false,
    } as Partial<OverlordRule>, false);
  }

  function restoreAllDefaults() {
    preferencesStore.setHiddenDefaultOverlordRules([]);
    const seeded = seedDefaultOverlordRules(
      $state.snapshot(rules) as OverlordRule[],
      [],
    );
    if (seeded) setRules(seeded);
  }

  // ── Condition editing ────────────────────────────────────────────────────────

  const CONDITION_EVENTS = [
    { value: 'context_pct', label: 'Context reaches %', param: 'at_or_above' },
    { value: 'turn_end', label: 'Every turn end', param: null },
    { value: 'commit', label: 'After a git commit', param: null },
    { value: 'tab_idle', label: 'Tab idle for minutes', param: 'minutes' },
    { value: 'task_stale', label: 'Board task stale for days', param: 'days' },
    { value: 'agent_unready', label: 'Agent not running', param: null },
    { value: 'no_todo_list', label: 'Working with no todo list', param: null },
    { value: 'permission_pending', label: 'Permission pending for minutes', param: 'minutes' },
    { value: 'directive_unacked', label: 'Directive unacked for minutes', param: 'minutes' },
  ] as const;

  function conditionParam(when: OverlordCondition): number | null {
    if ('at_or_above' in when) return when.at_or_above;
    if ('minutes' in when) return when.minutes;
    if ('days' in when) return when.days;
    return null;
  }

  function setConditionEvent(rule: OverlordRule, event: string) {
    const def = CONDITION_EVENTS.find((c) => c.value === event);
    if (!def) return;
    let when: OverlordCondition;
    if (def.param === 'at_or_above') when = { event: 'context_pct', at_or_above: 55 };
    else if (def.param === 'minutes') when = { event, minutes: 10 } as OverlordCondition;
    else if (def.param === 'days') when = { event: 'task_stale', days: 3 };
    else when = { event } as OverlordCondition;
    // Some conditions are dead under the default guards — adjust the coupled guard so
    // picking the condition doesn't silently produce a rule that can never fire.
    let guards = rule.guards;
    if (event === 'agent_unready') {
      // No live agent is the point; a live-REPL requirement contradicts it.
      guards = { ...guards, require_live_repl: false };
    } else if (event === 'permission_pending') {
      const st = guards.agent_state ?? ['idle'];
      if (!st.includes('permission')) guards = { ...guards, agent_state: [...st, 'permission'] };
    } else if (event === 'directive_unacked') {
      // The condition requires an outstanding directive to exist.
      guards = { ...guards, only_if_no_outstanding: false };
    }
    updateRule(rule.id, { when, guards });
  }

  function setConditionParam(rule: OverlordRule, value: number) {
    const w = { ...rule.when } as Record<string, unknown>;
    if ('at_or_above' in w) w.at_or_above = value;
    else if ('minutes' in w) w.minutes = value;
    else if ('days' in w) w.days = value;
    updateRule(rule.id, { when: w as unknown as OverlordCondition });
  }

  // ── Steps ────────────────────────────────────────────────────────────────────

  const GATES = [
    { value: 'none', label: 'Fire and forget' },
    { value: 'turn_end', label: 'Wait for turn end' },
    { value: 'ack', label: 'Wait for agent ack' },
    { value: 'context_below', label: 'Wait for context below %' },
    { value: 'idle_ms', label: 'Wait for output quiet (ms)' },
  ] as const;

  function gateValue(step: OverlordStep): string {
    return step.await?.until ?? 'none';
  }

  function gateParam(step: OverlordStep): number | null {
    if (!step.await) return null;
    if (step.await.until === 'context_below') return step.await.pct;
    if (step.await.until === 'idle_ms') return step.await.ms;
    return null;
  }

  function updateStep(rule: OverlordRule, idx: number, patch: Partial<OverlordStep>) {
    const sequence = rule.sequence.map((s, i) => (i === idx ? { ...s, ...patch } : s));
    updateRule(rule.id, { sequence });
  }

  function setStepGate(rule: OverlordRule, idx: number, until: string) {
    let gate: OverlordGate | null = null;
    if (until === 'turn_end') gate = { until: 'turn_end' };
    else if (until === 'ack') gate = { until: 'ack' };
    else if (until === 'context_below') gate = { until: 'context_below', pct: 30 };
    else if (until === 'idle_ms') gate = { until: 'idle_ms', ms: 5000 };
    updateStep(rule, idx, { await: gate });
  }

  function setStepGateParam(rule: OverlordRule, idx: number, value: number) {
    const step = rule.sequence[idx];
    if (!step.await) return;
    if (step.await.until === 'context_below') updateStep(rule, idx, { await: { until: 'context_below', pct: value } });
    else if (step.await.until === 'idle_ms') updateStep(rule, idx, { await: { until: 'idle_ms', ms: value } });
  }

  function addStep(rule: OverlordRule) {
    updateRule(rule.id, {
      sequence: [
        ...rule.sequence,
        { kind: 'process', text: '', await: { until: 'turn_end' }, timeout_seconds: 600, on_timeout: 'abort' },
      ],
    });
  }

  function removeStep(rule: OverlordRule, idx: number) {
    if (rule.sequence.length <= 1) return;
    updateRule(rule.id, { sequence: rule.sequence.filter((_, i) => i !== idx) });
  }

  // ── Guards + scope ───────────────────────────────────────────────────────────

  const AGENT_STATES: OverlordAgentStateName[] = ['idle', 'active', 'permission'];
  const RUNTIMES = ['claude', 'codex', 'gemini'] as const;

  function toggleGuardState(rule: OverlordRule, st: OverlordAgentStateName) {
    const cur = rule.guards.agent_state ?? ['idle'];
    const next = cur.includes(st) ? cur.filter((s) => s !== st) : [...cur, st];
    updateRule(rule.id, { guards: { ...rule.guards, agent_state: next } });
  }

  function toggleWorkspace(rule: OverlordRule, wsId: string) {
    const next = rule.workspaces.includes(wsId)
      ? rule.workspaces.filter((id) => id !== wsId)
      : [...rule.workspaces, wsId];
    updateRule(rule.id, { workspaces: next });
  }

  function toggleStepRuntime(rule: OverlordRule, idx: number, rt: 'claude' | 'codex' | 'gemini') {
    const step = rule.sequence[idx];
    const cur = step.runtimes ?? [...RUNTIMES];
    const next = cur.includes(rt) ? cur.filter((r) => r !== rt) : [...cur, rt];
    updateStep(rule, idx, { runtimes: next.length === RUNTIMES.length ? undefined : next });
  }
</script>

<p class="section-desc">
  Overlord supervises this window's agent tabs: it watches context pressure, commits, idleness and
  todo hygiene, and types directives into tabs with your full authority. Rules fire deterministically —
  no model in the loop — and every injection lands in the verbatim ledger on the Overlord board.
</p>

<div class="setting-row">
  <div>
    <strong>Enable Overlord</strong>
    <p class="hint">Runs the per-window engine and shows the ♔ accessor row in the sidebar.</p>
  </div>
  <button
    class="toggle"
    class:active={preferencesStore.overlordEnabled}
    onclick={() => preferencesStore.setOverlordEnabled(!preferencesStore.overlordEnabled)}
    aria-pressed={preferencesStore.overlordEnabled}
    aria-label="Toggle Overlord"
  ><span class="toggle-knob"></span></button>
</div>

<div class="setting-row">
  <div>
    <strong>Propose mode</strong>
    <p class="hint">Rules land on the board as proposed directives you click to send, instead of firing autonomously. Recommended until you trust the ruleset.</p>
  </div>
  <button
    class="toggle"
    class:active={preferencesStore.overlordProposeMode}
    onclick={() => preferencesStore.setOverlordProposeMode(!preferencesStore.overlordProposeMode)}
    aria-pressed={preferencesStore.overlordProposeMode}
    aria-label="Toggle propose mode"
  ><span class="toggle-knob"></span></button>
</div>

<div style="display: flex; gap: 8px; margin: 12px 0;">
  <button class="add-btn" onclick={addRule}>+ Add Rule</button>
  {#if preferencesStore.hiddenDefaultOverlordRules.length > 0}
    <button class="add-btn" onclick={restoreAllDefaults}>Restore Defaults</button>
  {/if}
</div>

{#each rules as rule (rule.id)}
  <div class="rule-card">
    <div class="rule-header">
      <button
        class="toggle small"
        class:active={rule.enabled}
        onclick={() => updateRule(rule.id, { enabled: !rule.enabled }, false)}
        aria-pressed={rule.enabled}
        aria-label="Toggle rule"
      ><span class="toggle-knob"></span></button>
      <button class="rule-name-btn" onclick={() => (expandedId = expandedId === rule.id ? null : rule.id)}>
        <svg class="chevron" class:expanded={expandedId === rule.id} width="12" height="12" viewBox="0 0 16 16" fill="currentColor"><path d="M6 3l5 5-5 5z"/></svg>
        {rule.name || 'Unnamed'}
        {#if rule.origin === 'proposed'}<span class="origin-tag">proposed</span>{/if}
        {#if rule.workspaces.length}<span class="scope-tag">{rule.workspaces.length} ws</span>{/if}
      </button>
      {#if rule.default_id}
        <button class="reset-btn" disabled={!rule.user_modified} onclick={() => restoreDefault(rule)} title="Restore to default">Reset</button>
      {/if}
      {#if confirmDeleteId === rule.id}
        <span class="confirm-delete">
          Delete?
          <button class="confirm-btn yes" onclick={() => deleteRule(rule)}>Yes</button>
          <button class="confirm-btn" onclick={() => (confirmDeleteId = null)}>No</button>
        </span>
      {:else}
        <button class="delete-btn" onclick={() => deleteRule(rule)} title="Delete rule"><Icon name="trash" /></button>
      {/if}
    </div>

    {#if expandedId === rule.id}
      <div class="rule-body" transition:slide={{ duration: 150 }}>
        <div class="field">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label>Name</label>
          <input type="text" value={rule.name} onchange={(e) => updateRule(rule.id, { name: e.currentTarget.value })} />
        </div>
        <div class="field">
          <!-- svelte-ignore a11y_label_has_associated_control -->
          <label>Description</label>
          <input type="text" value={rule.description ?? ''} onchange={(e) => updateRule(rule.id, { description: e.currentTarget.value || null })} />
        </div>

        <h4 class="sub-heading">When</h4>
        <div class="field-row">
          <select value={rule.when.event} onchange={(e) => setConditionEvent(rule, e.currentTarget.value)}>
            {#each CONDITION_EVENTS as c (c.value)}
              <option value={c.value}>{c.label}</option>
            {/each}
          </select>
          {#if conditionParam(rule.when) !== null}
            <input class="num" type="number" min="1" value={conditionParam(rule.when)} onchange={(e) => setConditionParam(rule, Number(e.currentTarget.value))} />
          {/if}
          <span class="inline-label">cooldown</span>
          <input class="num" type="number" min="0" value={rule.cooldown} onchange={(e) => updateRule(rule.id, { cooldown: Number(e.currentTarget.value) })} title="Seconds, per tab" />
          <span class="inline-label">s</span>
        </div>

        <h4 class="sub-heading">Scope</h4>
        <div class="scope-list">
          <span class="hint">{rule.workspaces.length === 0 ? 'Global — every workspace in every window.' : 'Only in the checked workspaces:'}</span>
          <div class="checks">
            {#each allWorkspaces as ws (ws.id)}
              <label class="check"><input type="checkbox" checked={rule.workspaces.includes(ws.id)} onchange={() => toggleWorkspace(rule, ws.id)} />{ws.name}</label>
            {/each}
          </div>
        </div>

        <h4 class="sub-heading">Directive sequence</h4>
        {#each rule.sequence as step, idx (idx)}
          <div class="step">
            <div class="step-row">
              <span class="step-num">{idx + 1}</span>
              <select value={step.kind} onchange={(e) => updateStep(rule, idx, { kind: e.currentTarget.value as 'process' | 'slash' })}>
                <option value="process">directive</option>
                <option value="slash">slash command</option>
              </select>
              {#if step.kind === 'slash'}
                <span class="checks inline">
                  {#each RUNTIMES as rt (rt)}
                    <label class="check"><input type="checkbox" checked={(step.runtimes ?? [...RUNTIMES]).includes(rt)} onchange={() => toggleStepRuntime(rule, idx, rt)} />{rt}</label>
                  {/each}
                </span>
              {/if}
              {#if rule.sequence.length > 1}
                <button class="delete-btn step-del" onclick={() => removeStep(rule, idx)} title="Remove step"><Icon name="trash" /></button>
              {/if}
            </div>
            <textarea rows="2" value={step.text} placeholder={step.kind === 'slash' ? '/compact' : 'The directive text — typed into the tab verbatim.'} onchange={(e) => updateStep(rule, idx, { text: e.currentTarget.value })}></textarea>
            <div class="step-row">
              <select value={gateValue(step)} onchange={(e) => setStepGate(rule, idx, e.currentTarget.value)}>
                {#each GATES as g (g.value)}
                  <option value={g.value}>{g.label}</option>
                {/each}
              </select>
              {#if gateParam(step) !== null}
                <input class="num" type="number" min="0" value={gateParam(step)} onchange={(e) => setStepGateParam(rule, idx, Number(e.currentTarget.value))} />
              {/if}
              {#if step.await}
                <span class="inline-label">timeout</span>
                <input class="num" type="number" min="10" value={step.timeout_seconds ?? 600} onchange={(e) => updateStep(rule, idx, { timeout_seconds: Number(e.currentTarget.value) })} />
                <span class="inline-label">s, then</span>
                <select value={step.on_timeout ?? 'abort'} onchange={(e) => updateStep(rule, idx, { on_timeout: e.currentTarget.value as OverlordStep['on_timeout'] })}>
                  <option value="abort">abort ritual</option>
                  <option value="continue">continue anyway</option>
                  <option value="notify_human">notify me</option>
                  <option value="escalate_to_overlord">escalate to Overlord</option>
                </select>
              {/if}
            </div>
          </div>
        {/each}
        <button class="add-btn" onclick={() => addStep(rule)}>+ Add Step</button>

        <h4 class="sub-heading">Guards <span class="hint">(mechanical safety floor — never editable by the Overlord agent)</span></h4>
        <div class="field-row">
          <span class="inline-label">inject only while agent is</span>
          <span class="checks inline">
            {#each AGENT_STATES as st (st)}
              <label class="check"><input type="checkbox" checked={(rule.guards.agent_state ?? ['idle']).includes(st)} onchange={() => toggleGuardState(rule, st)} />{st}</label>
            {/each}
          </span>
        </div>
        <div class="field-row">
          <label class="check"><input type="checkbox" checked={rule.guards.require_live_repl} onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, require_live_repl: e.currentTarget.checked } })} />require live agent REPL <span class="hint">(without it a directive can land in a bash shell)</span></label>
        </div>
        <div class="field-row">
          <label class="check"><input type="checkbox" checked={rule.guards.only_if_no_outstanding} onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, only_if_no_outstanding: e.currentTarget.checked } })} />serialize — only when no directive is outstanding on the tab</label>
        </div>
        <div class="field-row">
          <span class="inline-label">quiet for</span>
          <input class="num" type="number" min="0" value={rule.guards.min_quiet_ms ?? 3000} onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, min_quiet_ms: Number(e.currentTarget.value) } })} />
          <span class="inline-label">ms before injecting · at most</span>
          <input class="num" type="number" min="1" value={rule.guards.max_per_hour ?? 1} onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, max_per_hour: Number(e.currentTarget.value) } })} />
          <span class="inline-label">fires/hour per tab</span>
        </div>
      </div>
    {/if}
  </div>
{/each}

<style>
  .section-desc { color: var(--fg-dim); font-size: 0.923rem; margin-bottom: 16px; }
  .hint { color: var(--fg-dim); font-size: 0.846rem; margin: 2px 0 0; font-weight: 400; }

  .setting-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    padding: 10px 0;
    border-bottom: 1px solid var(--bg-medium);
  }

  .toggle {
    position: relative;
    width: 40px;
    height: 22px;
    border-radius: 11px;
    background: var(--bg-light);
    border: none;
    cursor: pointer;
    flex-shrink: 0;
    transition: background 0.15s;
  }
  .toggle.active { background: var(--accent); }
  .toggle.small { width: 32px; height: 18px; border-radius: 9px; }
  .toggle-knob {
    position: absolute;
    top: 2px; left: 2px;
    width: 18px; height: 18px;
    border-radius: 50%;
    background: var(--fg);
    transition: transform 0.15s;
  }
  .toggle.small .toggle-knob { width: 14px; height: 14px; }
  .toggle.active .toggle-knob { transform: translateX(18px); }
  .toggle.small.active .toggle-knob { transform: translateX(14px); }

  .add-btn {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    padding: 4px 12px;
    font-size: 0.846rem;
    cursor: pointer;
  }
  .add-btn:hover { border-color: var(--accent); }

  .rule-card {
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    margin-bottom: 8px;
    background: var(--bg-medium);
  }

  .rule-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
  }

  .rule-name-btn {
    flex: 1;
    display: flex;
    align-items: center;
    gap: 6px;
    background: none;
    border: none;
    color: var(--fg);
    font-size: 0.923rem;
    font-weight: 600;
    cursor: pointer;
    text-align: left;
  }

  .chevron { transition: transform 0.15s; }
  .chevron.expanded { transform: rotate(90deg); }

  .origin-tag, .scope-tag {
    font-size: 0.692rem;
    background: var(--bg-light);
    color: var(--fg-dim);
    border-radius: 8px;
    padding: 0 6px;
    font-weight: 400;
  }

  .reset-btn {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    font-size: 0.769rem;
    padding: 2px 8px;
    cursor: pointer;
  }
  .reset-btn:disabled { opacity: 0.4; cursor: default; }

  .delete-btn {
    background: none;
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    padding: 2px;
  }
  .delete-btn:hover { color: #f7768e; }

  .confirm-delete { display: flex; align-items: center; gap: 6px; font-size: 0.846rem; color: var(--fg-dim); }
  .confirm-btn {
    background: var(--bg-light);
    border: none;
    border-radius: 4px;
    color: var(--fg);
    padding: 2px 8px;
    font-size: 0.769rem;
    cursor: pointer;
  }
  .confirm-btn.yes { background: #f7768e; color: var(--bg-dark); }

  .rule-body { padding: 4px 12px 12px; border-top: 1px solid var(--bg-light); }

  .field { margin: 8px 0; }
  .field label { display: block; font-size: 0.846rem; color: var(--fg-dim); margin-bottom: 3px; }
  .field input[type='text'] {
    width: 100%;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    padding: 5px 8px;
    font-size: 0.846rem;
  }

  .sub-heading { font-size: 0.846rem; margin: 14px 0 6px; color: var(--fg); }
  .sub-heading .hint { font-size: 0.769rem; }

  .field-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 6px 0; }
  .inline-label { color: var(--fg-dim); font-size: 0.846rem; }

  select {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    padding: 4px 6px;
    font-size: 0.846rem;
  }

  input.num {
    width: 70px;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    padding: 4px 6px;
    font-size: 0.846rem;
  }

  input:focus, select:focus, textarea:focus { outline: none; border-color: var(--accent); }

  .scope-list .checks { margin-top: 6px; }
  .checks { display: flex; flex-wrap: wrap; gap: 6px 14px; }
  .checks.inline { display: inline-flex; }
  .check { display: flex; align-items: center; gap: 5px; font-size: 0.846rem; color: var(--fg); cursor: pointer; }

  .step {
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 8px;
    margin-bottom: 8px;
    background: var(--bg-dark);
  }
  .step-row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin-bottom: 6px; }
  .step-row:last-child { margin-bottom: 0; }
  .step-num {
    background: var(--bg-light);
    border-radius: 50%;
    width: 18px; height: 18px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    font-size: 0.692rem;
    color: var(--fg-dim);
    flex-shrink: 0;
  }
  .step-del { margin-left: auto; }
  .step textarea {
    width: 100%;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    padding: 5px 8px;
    font-size: 0.846rem;
    font-family: inherit;
    resize: vertical;
    margin-bottom: 6px;
  }
</style>
