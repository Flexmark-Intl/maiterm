<script lang="ts">
  /** Preferences → Agent Defaults (docs/agent-defaults.md).
   *
   *  The values each agent's new sessions start with, written into that agent's OWN settings
   *  file. Rust (`agent_defaults`) owns all of it: this pane draws what is on disk and asks for
   *  one change at a time. "Agent's default" means the key is absent. Every answer is the
   *  file's state after the write, so the pane never shows what it only hoped happened.
   *
   *  Text values (days, a Codex model) are committed on Enter or leaving the field, never per
   *  keystroke: typing "500" must not save "5" on the way, and a 5-day retention sweeps the
   *  transcripts on the next Claude start. */
  import { onMount } from 'svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AgentDefaultsRuntime, AgentDefaultsStatus } from '$lib/tauri/commands';

  type Option = { value: string; label: string };

  /** The select value for "absent". Never a real value: Antigravity's agent mode has "" as one. */
  const ABSENT = '\u0000absent';

  const CLAUDE_ADVISORS: Option[] = [
    { value: 'fable', label: 'Fable' },
    { value: 'opus', label: 'Opus' },
    { value: 'sonnet', label: 'Sonnet' },
  ];
  const CLAUDE_MODELS: Option[] = [
    { value: 'opus', label: 'Opus' },
    { value: 'sonnet', label: 'Sonnet' },
    { value: 'fable', label: 'Fable' },
    { value: 'haiku', label: 'Haiku' },
  ];
  const CLAUDE_EFFORTS: Option[] = ['low', 'medium', 'high', 'xhigh'].map((v) => ({ value: v, label: v }));
  const CODEX_EFFORTS: Option[] = ['minimal', 'low', 'medium', 'high', 'xhigh', 'max'].map((v) => ({ value: v, label: v }));
  const CODEX_REVIEWERS: Option[] = [
    { value: 'user', label: 'You (user)' },
    { value: 'auto_review', label: 'Automatic review (auto_review)' },
  ];
  const AGY_MODES: Option[] = [
    { value: '', label: 'Default' },
    { value: 'accept-edits', label: 'Accept edits' },
    { value: 'plan', label: 'Plan' },
    { value: 'auto', label: 'Auto' },
  ];
  const AGY_THINKING: Option[] = [
    { value: 'true', label: 'Shown' },
    { value: 'false', label: 'Hidden' },
  ];
  const AGY_QUEUED: Option[] = [
    { value: 'queue', label: 'Queue until the turn ends' },
    { value: 'send-immediately', label: 'Send immediately' },
  ];

  let status = $state<AgentDefaultsStatus | null>(null);
  let busy = $state<string | null>(null);
  /** One message at a time, on the part it is about. */
  let feedback = $state<{ runtime: AgentDefaultsRuntime | null; text: string } | null>(null);
  /** Parts the human opened; a part whose agent is installed starts open. */
  let opened = $state<Record<AgentDefaultsRuntime, boolean | null>>({ claude: null, codex: null, agy: null });

  async function load() {
    try {
      status = await commands.agentDefaultsStatus();
      feedback = null;
    } catch (e) {
      feedback = { runtime: null, text: String(e) };
    }
  }

  onMount(() => {
    void load();
  });

  function isOpen(rt: AgentDefaultsRuntime, installed: boolean): boolean {
    return opened[rt] ?? installed;
  }

  /** `el`/`was`: the control and what it showed before, put back when the write is refused, so
   *  a control never shows a value the file doesn't hold. */
  async function change(runtime: AgentDefaultsRuntime, key: string, value: string | null, el: HTMLInputElement | HTMLSelectElement, was: string) {
    if (busy) {
      el.value = was;
      return;
    }
    busy = `${runtime}:${key}`;
    feedback = null;
    try {
      status = await commands.agentDefaultsSet(runtime, key, value);
    } catch (e) {
      el.value = was;
      feedback = { runtime, text: String(e) };
      await load().catch(() => {});
      // load() clears feedback on success; keep the refusal visible.
      feedback = { runtime, text: String(e) };
    } finally {
      busy = null;
    }
  }

  /** The options, plus the on-disk value when it isn't one of them: a value set by hand or
   *  by a newer CLI is shown as it is, never as a blank that could be saved over. */
  function withCurrent(options: Option[], current: string | null): Option[] {
    if (current === null || options.some((o) => o.value === current)) return options;
    return [...options, { value: current, label: `${current} (set elsewhere)` }];
  }

  function onSelect(runtime: AgentDefaultsRuntime, key: string, current: string | null, e: Event) {
    const el = e.currentTarget as HTMLSelectElement;
    const v = el.value;
    void change(runtime, key, v === ABSENT ? null : v, el, current ?? ABSENT);
  }

  /** Commit a text field: empty is "the agent's default"; unchanged writes nothing. */
  function commitText(runtime: AgentDefaultsRuntime, key: string, current: string | null, input: HTMLInputElement) {
    const v = input.value.trim();
    if ((v === '' ? null : v) === current) {
      input.value = current ?? '';
      return;
    }
    void change(runtime, key, v === '' ? null : v, input, current ?? '');
  }

  function textKeys(e: KeyboardEvent, current: string | null) {
    const input = e.currentTarget as HTMLInputElement;
    if (e.key === 'Enter') input.blur();
    if (e.key === 'Escape') {
      input.value = current ?? '';
      input.blur();
    }
  }

  const advisorStuck = $derived(
    !!status?.claude.advisor && status.claude.flags_off && !status.claude.advisor_switch,
  );
</script>

{#snippet choice(runtime: AgentDefaultsRuntime, key: string, current: string | null, options: Option[], disabled: boolean, label: string)}
  <select
    aria-label={label}
    value={current ?? ABSENT}
    disabled={disabled || busy !== null}
    onchange={(e) => onSelect(runtime, key, current, e)}
  >
    <option value={ABSENT}>Agent's default</option>
    {#each withCurrent(options, current) as o (o.value)}
      <option value={o.value}>{o.label}</option>
    {/each}
  </select>
{/snippet}

{#snippet header(runtime: AgentDefaultsRuntime, name: string, file: string, installed: boolean)}
  <button
    class="part-head"
    onclick={() => (opened[runtime] = !isOpen(runtime, installed))}
    aria-expanded={isOpen(runtime, installed)}
  >
    <svg class="chevron" class:open={isOpen(runtime, installed)} width="12" height="12" viewBox="0 0 16 16" fill="currentColor"><path d="M6 3l5 5-5 5z" /></svg>
    <span class="part-name">{name}</span>
    <code class="part-file">{file}</code>
    {#if !installed}<span class="part-note">not installed</span>{/if}
  </button>
{/snippet}

{#snippet message(runtime: AgentDefaultsRuntime, error: string | null)}
  {#if error}
    <p class="error">{error}</p>
  {/if}
  {#if feedback && feedback.runtime === runtime}
    <p class="error">{feedback.text}</p>
  {/if}
{/snippet}

<div class="pane">
  <h3>Agent Defaults</h3>
  <p class="hint">
    The model, effort and other values each agent's new sessions start with, written into that
    agent's own settings file, so they hold in every terminal, not just maiTerm's. Each value is
    read from the file: change one with the agent's own <code>/config</code> and it shows here.
    "Agent's default" leaves the setting out, so the agent's own default applies. A session's own
    <code>/model</code>, <code>/effort</code> or launch flag still wins for that session. Changes
    apply to new sessions.
  </p>

  {#if feedback && feedback.runtime === null}
    <p class="error">{feedback.text}</p>
  {/if}

  {#if !status}
    <p class="hint">Reading…</p>
  {:else}
    {@const c = status.claude}
    <section class="part">
      {@render header('claude', 'Claude Code', '~/.claude/settings.json', c.installed)}
      {#if isOpen('claude', c.installed)}
        <div class="rows">
          {@render message('claude', c.error)}

          <div class="row">
            <div class="text">
              <span class="label">Advisor</span>
              <p class="hint">
                A stronger model Claude consults before and after substantial work. It must be at
                least as capable as the model it advises, or Claude skips it. Hive workers consult
                it too, so its cost and time are multiplied by the number of workers. Some advisor
                models bill to usage credits. Running sessions pick it up after <code>/clear</code>
                or <code>/compact</code>.
              </p>
              {#if advisorStuck}
                <p class="warn">
                  Not running: telemetry is off in Claude's settings, and the advisor then needs
                  a switch that isn't set. Choose the advisor again here to set it.
                </p>
              {/if}
            </div>
            {@render choice('claude', 'advisor', c.advisor, CLAUDE_ADVISORS, !!c.error, 'Claude advisor')}
          </div>

          <div class="row">
            <div class="text">
              <span class="label">Default model</span>
            </div>
            {@render choice('claude', 'model', c.model, CLAUDE_MODELS, !!c.error, 'Claude default model')}
          </div>

          <div class="row column">
            <div class="text">
              <span class="label">Effort</span>
              <p class="hint">Claude keeps effort per model, so each model has its own.</p>
            </div>
            <div class="effort">
              {#each c.effort as row (row.model_id)}
                <div class="effort-row">
                  <span class="effort-model">{row.label ?? row.model_id}</span>
                  {#if row.label}
                    {@render choice('claude', `effort:${row.model_id}`, row.level, CLAUDE_EFFORTS, !!c.error, `${row.label} effort`)}
                  {:else}
                    <span class="readonly">{row.level}</span>
                  {/if}
                </div>
              {/each}
            </div>
          </div>

          <div class="row">
            <div class="text">
              <span class="label">Keep transcripts (days)</span>
              <p class="hint">
                Claude deletes chats older than this when a session starts: 30 days unless set.
                Resume, maiLink and the Loom read them. maiTerm sets 500 once where nothing is
                set. Empty for Claude's default.
              </p>
            </div>
            <input
              class="days"
              type="text"
              inputmode="numeric"
              aria-label="Days Claude keeps transcripts"
              placeholder="30"
              value={c.retention_days ?? ''}
              disabled={!!c.error || busy !== null}
              onkeydown={(e) => textKeys(e, c.retention_days)}
              onblur={(e) => commitText('claude', 'retention', c.retention_days, e.currentTarget)}
            />
          </div>
        </div>
      {/if}
    </section>

    {@const x = status.codex}
    <section class="part">
      {@render header('codex', 'Codex', '~/.codex/config.toml', x.installed)}
      {#if isOpen('codex', x.installed)}
        <div class="rows">
          {@render message('codex', x.error)}
          <div class="row">
            <div class="text">
              <span class="label">Default model</span>
              <p class="hint">A Codex model name, as <code>codex --model</code> takes it. Empty for Codex's default.</p>
            </div>
            <input
              class="model"
              type="text"
              spellcheck="false"
              aria-label="Codex default model"
              placeholder="Codex's default"
              value={x.model ?? ''}
              disabled={!!x.error || busy !== null}
              onkeydown={(e) => textKeys(e, x.model)}
              onblur={(e) => commitText('codex', 'model', x.model, e.currentTarget)}
            />
          </div>
          <div class="row">
            <div class="text"><span class="label">Reasoning effort</span></div>
            {@render choice('codex', 'effort', x.effort, CODEX_EFFORTS, !!x.error, 'Codex reasoning effort')}
          </div>
          <div class="row">
            <div class="text">
              <span class="label">Approvals reviewer</span>
              <p class="hint">Who answers Codex's approval requests.</p>
            </div>
            {@render choice('codex', 'reviewer', x.reviewer, CODEX_REVIEWERS, !!x.error, 'Codex approvals reviewer')}
          </div>
        </div>
      {/if}
    </section>

    {@const g = status.agy}
    <section class="part">
      {@render header('agy', 'Antigravity', '~/.gemini/antigravity-cli/settings.json', g.installed)}
      {#if isOpen('agy', g.installed)}
        <div class="rows">
          {@render message('agy', g.error)}
          <div class="row">
            <div class="text">
              <span class="label">Agent mode</span>
              <p class="hint">"Default" is Antigravity's own Default mode, saved as a choice.</p>
            </div>
            {@render choice('agy', 'agent_mode', g.agent_mode, AGY_MODES, !!g.error, 'Antigravity agent mode')}
          </div>
          <div class="row">
            <div class="text"><span class="label">Thinking</span></div>
            {@render choice('agy', 'show_thinking', g.show_thinking, AGY_THINKING, !!g.error, 'Antigravity thinking display')}
          </div>
          <div class="row">
            <div class="text">
              <span class="label">Messages sent while it works</span>
            </div>
            {@render choice('agy', 'queued_messages', g.queued_messages, AGY_QUEUED, !!g.error, 'Antigravity queued messages')}
          </div>
          <p class="hint">Antigravity's model and effort aren't kept in its settings file, so they aren't offered here.</p>
        </div>
      {/if}
    </section>
  {/if}
</div>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  h3 {
    color: var(--fg);
    font-size: 1rem;
    margin: 0;
  }

  .hint {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .error,
  .warn {
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .error {
    color: var(--red, #f7768e);
  }

  .warn {
    color: var(--yellow, #e0af68);
  }

  .part {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
  }

  .part-head {
    align-items: center;
    background: none;
    border: none;
    color: var(--fg);
    cursor: pointer;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    padding: 10px 14px;
    text-align: left;
    width: 100%;
  }

  .chevron {
    color: var(--fg-dim);
    flex: none;
    transition: transform 0.15s;
  }

  .chevron.open {
    transform: rotate(90deg);
  }

  .part-name {
    font-weight: 600;
  }

  .part-file {
    color: var(--fg-dim);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }

  .part-note {
    color: var(--fg-dim);
    font-size: 0.75rem;
    font-style: italic;
  }

  .rows {
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 12px 14px 14px;
  }

  .row {
    align-items: flex-start;
    display: flex;
    gap: 16px;
    justify-content: space-between;
  }

  .row.column {
    flex-direction: column;
    gap: 8px;
  }

  .text {
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .label {
    color: var(--fg);
    font-size: 0.9rem;
  }

  select,
  input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    flex: none;
    font-size: 0.9rem;
    padding: 5px 8px;
  }

  select {
    cursor: pointer;
    min-width: 150px;
  }

  select:hover:not(:disabled),
  input:focus {
    border-color: var(--accent);
    outline: none;
  }

  select:disabled,
  input:disabled {
    opacity: 0.6;
  }

  input.days {
    width: 80px;
  }

  input.model {
    width: 180px;
  }

  .effort {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-left: 12px;
  }

  .effort-row {
    align-items: center;
    display: flex;
    gap: 12px;
    justify-content: space-between;
  }

  .effort-model {
    color: var(--fg);
    font-size: 0.85rem;
    overflow-wrap: anywhere;
  }

  .readonly {
    color: var(--fg-dim);
    font-size: 0.85rem;
  }

  code {
    font-size: 0.75rem;
  }
</style>
