<script lang="ts">
  import { slide } from 'svelte/transition';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { DEFAULT_OVERLORD_RULES, seedDefaultOverlordRules } from '$lib/overlord/defaults';
  import * as commands from '$lib/tauri/commands';
  import Icon from '$lib/components/Icon.svelte';
  import {
    conditionChip, conditionSource, describeCondition, describeGate,
    fmtSeconds, ruleWarnings, scopeLabel, sequenceSummary,
  } from '$lib/overlord/format';
  import '$lib/overlord/deck.css';
  import type {
    OverlordCondition, OverlordGate, OverlordRule, OverlordStep, OverlordAgentStateName,
  } from '$lib/tauri/types';

  /**
   * Overlord rules editor (docs/overlord.md §5).
   *
   * A rule types into a live terminal with the human's own authority, so the editor's
   * first job is COMPREHENSION: every rule states itself as a sentence before you ever
   * open it, the directive text is presented verbatim as the payload it is, and guard
   * combinations that would make a rule silently dead are called out inline.
   *
   * The guards block lives here and only here — it is unreachable from Overlord's
   * proposeRuleChanges MCP surface (§10 field tiers), which is what makes envelope-free
   * injection safe. That's stated in the UI, not just in the docs.
   */

  let expandedId = $state<string | null>(null);
  let confirmDeleteId = $state<string | null>(null);

  // All workspaces across windows, for scope chips (preferences is its own window, so
  // the per-window workspacesStore is empty here).
  let allWorkspaces = $state<{ id: string; name: string }[]>([]);
  commands.getAppData().then((data) => {
    allWorkspaces = data.windows.flatMap((w) =>
      w.workspaces.filter((ws) => !ws.overlord).map((ws) => ({ id: ws.id, name: ws.name })),
    );
  }).catch(() => {});

  const rules = $derived(preferencesStore.overlordRules);
  const activeCount = $derived(rules.filter((r) => r.enabled).length);
  const wsName = (id: string) => allWorkspaces.find((w) => w.id === id)?.name ?? id.slice(0, 8);

  function setRules(next: OverlordRule[]) { preferencesStore.setOverlordRules(next); }

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
      sequence: [{ kind: 'process', text: '', await: { until: 'turn_end' }, timeout_seconds: 600, on_timeout: 'abort' }],
    };
    setRules([rule, ...rules]);
    expandedId = rule.id;
  }

  function deleteRule(rule: OverlordRule) {
    if (confirmDeleteId !== rule.id) { confirmDeleteId = rule.id; return; }
    confirmDeleteId = null;
    if (rule.default_id) {
      preferencesStore.setHiddenDefaultOverlordRules([
        ...preferencesStore.hiddenDefaultOverlordRules, rule.default_id,
      ]);
    }
    setRules(rules.filter((r) => r.id !== rule.id));
  }

  function restoreDefault(rule: OverlordRule) {
    if (!rule.default_id) return;
    const tmpl = DEFAULT_OVERLORD_RULES[rule.default_id];
    if (!tmpl) return;
    updateRule(rule.id, { ...structuredClone(tmpl), user_modified: false } as Partial<OverlordRule>, false);
  }

  function restoreAllDefaults() {
    preferencesStore.setHiddenDefaultOverlordRules([]);
    const seeded = seedDefaultOverlordRules($state.snapshot(rules) as OverlordRule[], []);
    if (seeded) setRules(seeded);
  }

  // ── Condition ───────────────────────────────────────────────────────────────
  const CONDITION_EVENTS = [
    { value: 'context_pct', label: 'Context reaches', unit: '%', param: 'at_or_above' },
    { value: 'commit', label: 'A commit lands', unit: '', param: null },
    { value: 'turn_end', label: 'Every turn ends', unit: '', param: null },
    { value: 'tab_idle', label: 'Tab goes idle for', unit: 'min', param: 'minutes' },
    { value: 'no_todo_list', label: 'Work has no todo list', unit: '', param: null },
    { value: 'task_stale', label: 'Board task stale for', unit: 'days', param: 'days' },
    { value: 'agent_unready', label: 'Agent stops running', unit: '', param: null },
    { value: 'permission_pending', label: 'Permission waits for', unit: 'min', param: 'minutes' },
    { value: 'directive_unacked', label: 'Directive unacked for', unit: 'min', param: 'minutes' },
  ] as const;

  function conditionUnit(when: OverlordCondition): string {
    return CONDITION_EVENTS.find((c) => c.value === when.event)?.unit ?? '';
  }
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
    // picking a condition never silently produces a rule that can't fire.
    let guards = rule.guards;
    if (event === 'agent_unready') guards = { ...guards, require_live_repl: false };
    else if (event === 'permission_pending') {
      const st = guards.agent_state ?? ['idle'];
      if (!st.includes('permission')) guards = { ...guards, agent_state: [...st, 'permission'] };
    } else if (event === 'directive_unacked') guards = { ...guards, only_if_no_outstanding: false };
    updateRule(rule.id, { when, guards });
  }

  function setConditionParam(rule: OverlordRule, value: number) {
    const w = { ...rule.when } as Record<string, unknown>;
    if ('at_or_above' in w) w.at_or_above = value;
    else if ('minutes' in w) w.minutes = value;
    else if ('days' in w) w.days = value;
    updateRule(rule.id, { when: w as unknown as OverlordCondition });
  }

  // ── Steps ───────────────────────────────────────────────────────────────────
  const GATES = [
    { value: 'none', label: "Don't wait" },
    { value: 'turn_end', label: 'Wait for turn end' },
    { value: 'ack', label: 'Wait for ack' },
    { value: 'context_below', label: 'Wait for context under' },
    { value: 'idle_ms', label: 'Wait for quiet' },
  ] as const;
  const RUNTIMES = ['claude', 'codex', 'gemini'] as const;

  const gateValue = (s: OverlordStep) => s.await?.until ?? 'none';
  function gateParam(s: OverlordStep): number | null {
    if (s.await?.until === 'context_below') return s.await.pct;
    if (s.await?.until === 'idle_ms') return s.await.ms;
    return null;
  }
  const gateUnit = (s: OverlordStep) =>
    s.await?.until === 'context_below' ? '%' : s.await?.until === 'idle_ms' ? 'ms' : '';

  function updateStep(rule: OverlordRule, idx: number, patch: Partial<OverlordStep>) {
    updateRule(rule.id, { sequence: rule.sequence.map((s, i) => (i === idx ? { ...s, ...patch } : s)) });
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
    if (step.await?.until === 'context_below') updateStep(rule, idx, { await: { until: 'context_below', pct: value } });
    else if (step.await?.until === 'idle_ms') updateStep(rule, idx, { await: { until: 'idle_ms', ms: value } });
  }

  function addStep(rule: OverlordRule) {
    updateRule(rule.id, {
      sequence: [...rule.sequence, { kind: 'process', text: '', await: { until: 'turn_end' }, timeout_seconds: 600, on_timeout: 'abort' }],
    });
  }
  function removeStep(rule: OverlordRule, idx: number) {
    if (rule.sequence.length <= 1) return;
    updateRule(rule.id, { sequence: rule.sequence.filter((_, i) => i !== idx) });
  }

  // ── Scope + guards ──────────────────────────────────────────────────────────
  const AGENT_STATES: OverlordAgentStateName[] = ['idle', 'active', 'permission'];

  function toggleGuardState(rule: OverlordRule, st: OverlordAgentStateName) {
    const cur = rule.guards.agent_state ?? ['idle'];
    const next = cur.includes(st) ? cur.filter((s) => s !== st) : [...cur, st];
    updateRule(rule.id, { guards: { ...rule.guards, agent_state: next.length ? next : ['idle'] } });
  }
  function toggleWorkspace(rule: OverlordRule, wsId: string) {
    const next = rule.workspaces.includes(wsId)
      ? rule.workspaces.filter((id) => id !== wsId)
      : [...rule.workspaces, wsId];
    updateRule(rule.id, { workspaces: next });
  }
  function toggleStepRuntime(rule: OverlordRule, idx: number, rt: (typeof RUNTIMES)[number]) {
    const step = rule.sequence[idx];
    const cur = step.runtimes ?? [...RUNTIMES];
    const next = cur.includes(rt) ? cur.filter((r) => r !== rt) : [...cur, rt];
    updateStep(rule, idx, { runtimes: next.length === RUNTIMES.length ? undefined : next });
  }
</script>

<div class="ov-rules">
  <!-- ══ Masthead ═══════════════════════════════════════════════════════════ -->
  <div class="masthead">
    <span class="masthead-mark">♔</span>
    <div class="masthead-copy">
      <p class="ov-label-lead">Overlord</p>
      <p class="lede">
        A per-window supervisor. It watches context pressure, commits, idleness and todo
        hygiene across your agent tabs, and types directives into them <em>with your
        authority</em> — the agent on the other end can't tell Overlord from you. Every
        injection is recorded verbatim in the ledger on the Overlord board.
      </p>
    </div>
  </div>

  <div class="switches">
    <button class="switch" class:on={preferencesStore.overlordEnabled}
            onclick={() => preferencesStore.setOverlordEnabled(!preferencesStore.overlordEnabled)}>
      <span class="switch-led"></span>
      <span class="switch-body">
        <span class="ov-label">Engine</span>
        <span class="switch-state">{preferencesStore.overlordEnabled ? 'Running' : 'Standby'}</span>
      </span>
    </button>

    <button class="switch" class:on={preferencesStore.overlordProposeMode}
            onclick={() => preferencesStore.setOverlordProposeMode(!preferencesStore.overlordProposeMode)}>
      <span class="switch-led"></span>
      <span class="switch-body">
        <span class="ov-label">Propose first</span>
        <span class="switch-state">{preferencesStore.overlordProposeMode ? 'You approve each directive' : 'Fires on its own'}</span>
      </span>
    </button>

    <div class="switch switch-static">
      <span class="ov-mono switch-count">{activeCount}<span class="of">/{rules.length}</span></span>
      <span class="switch-body">
        <span class="ov-label">Rules active</span>
        <span class="switch-state">{preferencesStore.overlordEnabled ? 'Evaluated every 5s' : 'Not evaluated'}</span>
      </span>
    </div>
  </div>

  <div class="ov-tickrule rules-rule"></div>

  <div class="rules-bar">
    <span class="ov-label">Ruleset</span>
    <span class="rules-bar-line"></span>
    <button class="ov-btn" onclick={addRule}>+ New rule</button>
    {#if preferencesStore.hiddenDefaultOverlordRules.length > 0}
      <button class="ov-btn" onclick={restoreAllDefaults}>Restore defaults</button>
    {/if}
  </div>

  <!-- ══ Rules ══════════════════════════════════════════════════════════════ -->
  {#each rules as rule, i (rule.id)}
    {@const open = expandedId === rule.id}
    {@const warnings = ruleWarnings(rule)}
    <article class="rule ov-panel ov-in" class:open class:off={!rule.enabled} style:--i={Math.min(i, 8)}>

      <div class="rule-head">
        <button class="pill" class:on={rule.enabled}
                onclick={() => updateRule(rule.id, { enabled: !rule.enabled }, false)}
                aria-pressed={rule.enabled} aria-label="Toggle rule">
          <span class="pill-knob"></span>
        </button>

        <button class="rule-summary" onclick={() => (expandedId = open ? null : rule.id)}>
          <span class="rule-name">
            {rule.name || 'Unnamed'}
            {#if rule.origin === 'proposed'}<span class="ov-chip tag">proposed</span>{/if}
            {#if rule.user_modified}<span class="ov-chip tag">edited</span>{/if}
          </span>
          <span class="rule-sentence">
            When {describeCondition(rule.when)} → {sequenceSummary(rule.sequence)}
            <span class="sep">·</span> {scopeLabel(rule, wsName)}
            <span class="sep">·</span> {fmtSeconds(rule.cooldown)} cooldown
          </span>
        </button>

        <span class="rule-chips">
          <span class="ov-chip ov-chip-tone" style:--tone="var(--ov-live)">{conditionChip(rule.when)}</span>
        </span>

        {#if rule.default_id}
          <button class="ov-btn rule-reset" disabled={!rule.user_modified}
                  onclick={() => restoreDefault(rule)} title="Restore this default's original wording">Reset</button>
        {/if}

        {#if confirmDeleteId === rule.id}
          <span class="confirm">
            <span class="ov-label">Delete?</span>
            <button class="ov-btn ov-btn-danger" onclick={() => deleteRule(rule)}>Yes</button>
            <button class="ov-btn" onclick={() => (confirmDeleteId = null)}>No</button>
          </span>
        {:else}
          <button class="icon-btn" onclick={() => deleteRule(rule)} title="Delete rule"><Icon name="trash" /></button>
        {/if}
      </div>

      {#if warnings.length}
        <div class="warnings">
          {#each warnings as w (w)}
            <p class="warning"><span class="warning-mark">!</span>{w}</p>
          {/each}
        </div>
      {/if}

      {#if open}
        <div class="rule-body" transition:slide={{ duration: 160 }}>

          <div class="field-grid">
            <label class="field">
              <span class="ov-label">Name</span>
              <input class="ov-input" type="text" value={rule.name}
                     onchange={(e) => updateRule(rule.id, { name: e.currentTarget.value })} />
            </label>
            <label class="field">
              <span class="ov-label">What it's for</span>
              <input class="ov-input" type="text" value={rule.description ?? ''} placeholder="Optional note to your future self"
                     onchange={(e) => updateRule(rule.id, { description: e.currentTarget.value || null })} />
            </label>
          </div>

          <!-- 01 WHEN -->
          <section class="stage">
            <div class="stage-head"><span class="stage-num ov-mono">01</span><span class="ov-label">When</span><span class="stage-rule"></span></div>
            <div class="row">
              <select class="ov-select" value={rule.when.event} onchange={(e) => setConditionEvent(rule, e.currentTarget.value)}>
                {#each CONDITION_EVENTS as c (c.value)}<option value={c.value}>{c.label}</option>{/each}
              </select>
              {#if conditionParam(rule.when) !== null}
                <input class="ov-input ov-num" type="number" min="1" value={conditionParam(rule.when)}
                       onchange={(e) => setConditionParam(rule, Number(e.currentTarget.value))} />
                <span class="unit">{conditionUnit(rule.when)}</span>
              {/if}
              <span class="spacer"></span>
              <span class="ov-label">then rest</span>
              <input class="ov-input ov-num" type="number" min="0" value={rule.cooldown}
                     onchange={(e) => updateRule(rule.id, { cooldown: Number(e.currentTarget.value) })} />
              <span class="unit">s per tab</span>
            </div>
            <p class="hint">Read from {conditionSource(rule.when)}.</p>
          </section>

          <!-- 02 DO -->
          <section class="stage">
            <div class="stage-head"><span class="stage-num ov-mono">02</span><span class="ov-label">Then say</span><span class="stage-rule"></span></div>

            <div class="sequence">
              {#each rule.sequence as step, idx (idx)}
                <div class="step">
                  <div class="step-rail"><span class="step-num ov-mono">{idx + 1}</span></div>
                  <div class="step-body">
                    <div class="row">
                      <div class="kinds">
                        <button class="kind" class:on={step.kind === 'process'} onclick={() => updateStep(rule, idx, { kind: 'process' })}>Directive</button>
                        <button class="kind" class:on={step.kind === 'slash'} onclick={() => updateStep(rule, idx, { kind: 'slash' })}>Slash command</button>
                      </div>
                      {#if step.kind === 'slash'}
                        <span class="runtimes">
                          <span class="ov-label">valid for</span>
                          {#each RUNTIMES as rt (rt)}
                            <button class="rt" class:on={(step.runtimes ?? [...RUNTIMES]).includes(rt)}
                                    onclick={() => toggleStepRuntime(rule, idx, rt)}>{rt}</button>
                          {/each}
                        </span>
                      {/if}
                      {#if rule.sequence.length > 1}
                        <button class="icon-btn step-del" onclick={() => removeStep(rule, idx)} title="Remove step"><Icon name="trash" /></button>
                      {/if}
                    </div>

                    <textarea class="ov-textarea" rows="2" value={step.text}
                              placeholder={step.kind === 'slash' ? '/compact' : 'Typed into the tab exactly as written — write it the way you would.'}
                              onchange={(e) => updateStep(rule, idx, { text: e.currentTarget.value })}></textarea>

                    <div class="row gate-row">
                      <select class="ov-select" value={gateValue(step)} onchange={(e) => setStepGate(rule, idx, e.currentTarget.value)}>
                        {#each GATES as g (g.value)}<option value={g.value}>{g.label}</option>{/each}
                      </select>
                      {#if gateParam(step) !== null}
                        <input class="ov-input ov-num" type="number" min="0" value={gateParam(step)}
                               onchange={(e) => setStepGateParam(rule, idx, Number(e.currentTarget.value))} />
                        <span class="unit">{gateUnit(step)}</span>
                      {/if}
                      {#if step.await}
                        <span class="ov-label">give up after</span>
                        <input class="ov-input ov-num" type="number" min="10" value={step.timeout_seconds ?? 600}
                               onchange={(e) => updateStep(rule, idx, { timeout_seconds: Number(e.currentTarget.value) })} />
                        <span class="unit">s, then</span>
                        <select class="ov-select" value={step.on_timeout ?? 'abort'}
                                onchange={(e) => updateStep(rule, idx, { on_timeout: e.currentTarget.value as OverlordStep['on_timeout'] })}>
                          <option value="abort">stop the whole sequence</option>
                          <option value="continue">carry on regardless</option>
                          <option value="notify_human">notify me</option>
                          <option value="escalate_to_overlord">escalate to Overlord</option>
                        </select>
                      {/if}
                    </div>
                    <p class="hint">Sends, then {describeGate(step.await)}.</p>
                  </div>
                </div>
              {/each}
            </div>
            <button class="ov-btn add-step" onclick={() => addStep(rule)}>+ Add step</button>
          </section>

          <!-- 03 WHERE -->
          <section class="stage">
            <div class="stage-head"><span class="stage-num ov-mono">03</span><span class="ov-label">Where</span><span class="stage-rule"></span></div>
            <div class="scope">
              <button class="scope-chip" class:on={rule.workspaces.length === 0}
                      onclick={() => updateRule(rule.id, { workspaces: [] })}>Everywhere</button>
              {#each allWorkspaces as ws (ws.id)}
                <button class="scope-chip" class:on={rule.workspaces.includes(ws.id)}
                        onclick={() => toggleWorkspace(rule, ws.id)}>{ws.name}</button>
              {/each}
            </div>
          </section>

          <!-- 04 GUARDS -->
          <section class="stage guards">
            <div class="stage-head">
              <span class="stage-num ov-mono">04</span><span class="ov-label">Guards</span>
              <span class="ov-chip sealed">sealed · human only</span>
              <span class="stage-rule"></span>
            </div>
            <p class="hint guards-note">
              Because directives are indistinguishable from you typing, these are the mechanical
              floor that keeps it safe. Overlord can propose changes to anything else in this rule —
              never to these.
            </p>

            <div class="row">
              <span class="ov-label">Inject only while the agent is</span>
              {#each AGENT_STATES as st (st)}
                <button class="scope-chip small" class:on={(rule.guards.agent_state ?? ['idle']).includes(st)}
                        onclick={() => toggleGuardState(rule, st)}>{st}</button>
              {/each}
            </div>

            <label class="ov-check guard-line">
              <input type="checkbox" checked={rule.guards.require_live_repl}
                     onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, require_live_repl: e.currentTarget.checked } })} />
              Require a live agent REPL
              <span class="hint inline">otherwise a directive can land in a bash shell</span>
            </label>

            <label class="ov-check guard-line">
              <input type="checkbox" checked={rule.guards.only_if_no_outstanding}
                     onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, only_if_no_outstanding: e.currentTarget.checked } })} />
              Serialize — one directive per tab at a time
            </label>

            <div class="row">
              <span class="ov-label">Wait for</span>
              <input class="ov-input ov-num" type="number" min="0" value={rule.guards.min_quiet_ms ?? 3000}
                     onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, min_quiet_ms: Number(e.currentTarget.value) } })} />
              <span class="unit">ms of silence · at most</span>
              <input class="ov-input ov-num" type="number" min="1" value={rule.guards.max_per_hour ?? 1}
                     onchange={(e) => updateRule(rule.id, { guards: { ...rule.guards, max_per_hour: Number(e.currentTarget.value) } })} />
              <span class="unit">fires per hour, per tab</span>
            </div>
          </section>
        </div>
      {/if}
    </article>
  {/each}

  {#if rules.length === 0}
    <div class="empty ov-panel ov-bracket">
      <p class="ov-label">No rules</p>
      <p class="hint">Add one, or restore the defaults to start from the checkpoint, review-after-commit and todo-hygiene set.</p>
    </div>
  {/if}
</div>

<style>
  .ov-rules { padding-bottom: 24px; }

  /* ── Masthead ─────────────────────────────────────────────────────────── */
  .masthead { display: flex; gap: 14px; align-items: flex-start; margin-bottom: 18px; }
  .masthead-mark {
    font-size: 1.6rem;
    line-height: 1.1;
    color: var(--ov-live);
    text-shadow: 0 0 16px color-mix(in srgb, var(--ov-live) 45%, transparent);
  }
  .masthead-copy { flex: 1; }
  .lede {
    color: var(--ov-ink-dim);
    font-size: 0.92rem;
    line-height: 1.65;
    max-width: 74ch;
    margin-top: 7px;
  }
  .lede em { color: var(--ov-ink-mid); font-style: italic; }

  /* ── Switches ─────────────────────────────────────────────────────────── */
  .switches { display: flex; gap: 8px; flex-wrap: wrap; }

  .switch {
    display: flex;
    align-items: center;
    gap: 11px;
    flex: 1;
    min-width: 190px;
    padding: 11px 13px;
    text-align: left;
    background: var(--ov-panel);
    border: 1px solid var(--ov-hair);
    border-radius: var(--ov-radius);
    cursor: pointer;
    transition: border-color 0.16s ease, background 0.16s ease;
  }
  .switch:hover { border-color: color-mix(in srgb, var(--ov-live) 45%, transparent); }
  .switch.on { border-color: color-mix(in srgb, var(--ov-live) 55%, transparent); }
  .switch-static { cursor: default; }
  .switch-static:hover { border-color: var(--ov-hair); }

  .switch-led {
    width: 9px; height: 9px;
    border-radius: 50%;
    flex-shrink: 0;
    background: var(--ov-ink-dim);
    box-shadow: none;
    transition: background 0.2s ease, box-shadow 0.2s ease;
  }
  .switch.on .switch-led {
    background: var(--ov-ok);
    box-shadow: 0 0 10px color-mix(in srgb, var(--ov-ok) 75%, transparent);
  }

  .switch-body { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .switch-state { font-size: 0.85rem; color: var(--ov-ink-mid); }
  .switch-count { font-size: 1.25rem; line-height: 1; color: var(--ov-ink); }
  .switch-count .of { font-size: 0.8rem; color: var(--ov-ink-dim); }

  .rules-rule { margin: 20px 0 14px; }

  .rules-bar { display: flex; align-items: center; gap: 12px; margin-bottom: 12px; }
  .rules-bar-line { flex: 1; height: 1px; background: var(--ov-hair); }

  /* ── Rule card ────────────────────────────────────────────────────────── */
  .rule { margin-bottom: 8px; transition: border-color 0.16s ease, opacity 0.16s ease; }
  .rule.off { opacity: 0.62; }
  .rule.open { border-color: color-mix(in srgb, var(--ov-live) 45%, transparent); }
  .rule:hover { border-color: color-mix(in srgb, var(--ov-live) 30%, var(--ov-hair)); }

  .rule-head { display: flex; align-items: center; gap: 11px; padding: 10px 12px; }

  .pill {
    position: relative;
    width: 32px; height: 17px;
    border-radius: 9px;
    background: var(--ov-hair-strong);
    flex-shrink: 0;
    transition: background 0.18s ease;
  }
  .pill.on { background: color-mix(in srgb, var(--ov-ok) 75%, transparent); }
  .pill-knob {
    position: absolute;
    top: 2px; left: 2px;
    width: 13px; height: 13px;
    border-radius: 50%;
    background: var(--bg-dark);
    transition: transform 0.18s cubic-bezier(0.3, 0.8, 0.3, 1);
  }
  .pill.on .pill-knob { transform: translateX(15px); }

  .rule-summary { flex: 1; min-width: 0; display: flex; flex-direction: column; gap: 4px; text-align: left; }
  .rule-name {
    display: flex;
    align-items: center;
    gap: 7px;
    font-family: var(--ov-face);
    font-size: 1.02rem;
    font-weight: 600;
    letter-spacing: 0.04em;
    color: var(--ov-ink);
  }
  .rule-summary:hover .rule-name { color: var(--ov-live); }
  .tag { height: 15px; font-size: 0.65rem; }

  .rule-sentence {
    font-size: 0.85rem;
    color: var(--ov-ink-dim);
    line-height: 1.45;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sep { opacity: 0.45; margin: 0 3px; }

  .rule-chips { display: flex; gap: 5px; flex-shrink: 0; }
  .rule-reset { flex-shrink: 0; }
  .confirm { display: flex; align-items: center; gap: 6px; flex-shrink: 0; }

  .icon-btn {
    color: var(--ov-ink-dim);
    padding: 3px;
    border-radius: 2px;
    flex-shrink: 0;
    transition: color 0.14s ease;
  }
  .icon-btn:hover { color: var(--ov-critical); }

  /* ── Warnings ─────────────────────────────────────────────────────────── */
  .warnings { padding: 0 12px 10px 55px; display: flex; flex-direction: column; gap: 5px; }
  .warning {
    display: flex;
    align-items: baseline;
    gap: 8px;
    font-size: 0.84rem;
    line-height: 1.5;
    color: color-mix(in srgb, var(--ov-warn) 80%, var(--fg));
  }
  .warning-mark {
    font-family: var(--ov-mono);
    font-weight: 700;
    color: var(--ov-warn);
    flex-shrink: 0;
  }

  /* ── Body ─────────────────────────────────────────────────────────────── */
  .rule-body { padding: 4px 12px 14px 55px; border-top: 1px solid var(--ov-hair); }

  .field-grid { display: grid; grid-template-columns: 1fr 1.6fr; gap: 10px; margin: 12px 0 4px; }
  .field { display: flex; flex-direction: column; gap: 5px; }

  .stage { margin-top: 18px; }
  .stage-head { display: flex; align-items: center; gap: 9px; margin-bottom: 9px; }
  .stage-num {
    font-size: 0.72rem;
    color: var(--ov-live);
    opacity: 0.8;
  }
  .stage-rule { flex: 1; height: 1px; background: var(--ov-hair); }

  .row { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; margin: 7px 0; }
  .spacer { flex: 1; min-width: 8px; }
  .unit { font-size: 0.82rem; color: var(--ov-ink-dim); }

  .hint { font-size: 0.79rem; color: var(--ov-ink-dim); line-height: 1.5; }
  .hint.inline { margin-left: 4px; opacity: 0.8; }

  /* ── Sequence ─────────────────────────────────────────────────────────── */
  .sequence { display: flex; flex-direction: column; }

  .step { display: flex; gap: 11px; }
  .step-rail {
    position: relative;
    width: 20px;
    flex-shrink: 0;
    display: flex;
    justify-content: center;
  }
  /* Connector line running down the sequence rail. */
  .step-rail::before {
    content: '';
    position: absolute;
    top: 22px; bottom: -2px;
    width: 1px;
    background: var(--ov-hair);
  }
  .step:last-child .step-rail::before { display: none; }
  .step-num {
    width: 20px; height: 20px;
    border-radius: 50%;
    border: 1px solid var(--ov-hair-strong);
    background: var(--bg-dark);
    color: var(--ov-ink-dim);
    font-size: 0.7rem;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1;
  }
  .step-body { flex: 1; min-width: 0; padding-bottom: 14px; }

  .kinds { display: flex; border: 1px solid var(--ov-hair); border-radius: 2px; overflow: hidden; }
  .kind {
    font-family: var(--ov-face);
    font-size: 0.76rem;
    font-weight: 600;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    padding: 4px 11px;
    color: var(--ov-ink-dim);
    transition: color 0.14s ease, background 0.14s ease;
  }
  .kind:hover { color: var(--ov-ink); }
  .kind.on { color: var(--ov-ink); background: color-mix(in srgb, var(--ov-live) 20%, transparent); }

  .runtimes { display: flex; align-items: center; gap: 5px; }
  .rt {
    font-family: var(--ov-mono);
    font-size: 0.74rem;
    padding: 2px 7px;
    border-radius: 2px;
    border: 1px solid var(--ov-hair);
    color: var(--ov-ink-dim);
    transition: all 0.14s ease;
  }
  .rt.on { color: var(--ov-live); border-color: color-mix(in srgb, var(--ov-live) 50%, transparent); }

  .step-del { margin-left: auto; }
  .gate-row { margin-top: 6px; }
  .add-step { margin-left: 31px; }

  /* ── Scope chips ──────────────────────────────────────────────────────── */
  .scope { display: flex; flex-wrap: wrap; gap: 6px; }
  .scope-chip {
    font-family: var(--ov-face);
    font-size: 0.8rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    padding: 4px 11px;
    border-radius: 2px;
    border: 1px solid var(--ov-hair);
    color: var(--ov-ink-dim);
    transition: all 0.14s ease;
  }
  .scope-chip:hover { color: var(--ov-ink); border-color: var(--ov-hair-strong); }
  .scope-chip.on {
    color: var(--ov-live);
    border-color: color-mix(in srgb, var(--ov-live) 55%, transparent);
    background: color-mix(in srgb, var(--ov-live) 12%, transparent);
  }
  .scope-chip.small { padding: 3px 9px; font-size: 0.76rem; text-transform: uppercase; letter-spacing: 0.1em; }

  /* ── Guards: visibly sealed ───────────────────────────────────────────── */
  .guards {
    border: 1px solid color-mix(in srgb, var(--ov-warn) 22%, var(--ov-hair));
    border-radius: var(--ov-radius);
    padding: 12px 14px 14px;
    background:
      repeating-linear-gradient(135deg,
        color-mix(in srgb, var(--ov-warn) 4%, transparent) 0 6px,
        transparent 6px 12px);
  }
  .sealed {
    border-color: color-mix(in srgb, var(--ov-warn) 45%, transparent);
    color: color-mix(in srgb, var(--ov-warn) 85%, var(--fg));
    background: color-mix(in srgb, var(--ov-warn) 10%, transparent);
  }
  .guards-note { max-width: 76ch; margin-bottom: 6px; }
  .guard-line { margin: 7px 0; }

  /* ── Empty ────────────────────────────────────────────────────────────── */
  .empty { padding: 26px; text-align: center; }
  .empty .hint { margin-top: 6px; }
</style>
