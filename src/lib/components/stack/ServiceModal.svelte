<script lang="ts">
  /** Add or edit one stack service (docs/stack.md §7). A modal rather than inline rows:
   *  the sidebar is 200–320px and a command line, a cwd and an env block need room.
   *  Follows TaskAddModal — same backdrop/panel/field/btn vocabulary, same explicit rAF
   *  focus (Svelte's `autofocus` is not focus; see the note there). */
  import type { Service, ServiceRestart } from '$lib/tauri/types';
  import type { ServiceInput } from '$lib/stores/stack.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { checkoutInCommand } from '$lib/stack/model';
  import { modLabel } from '$lib/utils/platform';

  interface Props {
    /** Existing service to edit; null = add. */
    service: Service | null;
    /** The workspace the service is in: never offered as a source to share from. */
    workspaceId: string;
    /** Pre-filled cwd for a new service (the active tab's directory). */
    defaultCwd: string;
    onsubmit: (input: ServiceInput) => void;
    oncancel: () => void;
  }

  let { service, workspaceId, defaultCwd, onsubmit, oncancel }: Props = $props();

  /** "Use the running service from another workspace" (docs/hive.md §6.8): a reference this
   *  workspace never starts or stops; its port fills `{port:<name>}` here. Only a new service
   *  can become one; a shared one edits only its name. */
  const editingShared = (() => !!service?.shared_from)();
  let mode = $state<'command' | 'shared'>(editingShared ? 'shared' : 'command');
  const sources = $derived(
    workspacesStore.workspaces
      .filter((w) => w.id !== workspaceId)
      .flatMap((w) => (w.stack ?? []).filter((s) => !s.shared_from).map((s) => ({ key: `${w.id}/${s.id}`, ws: w, s }))),
  );
  let sourceKey = $state('');
  const source = $derived(sources.find((x) => x.key === sourceKey) ?? null);

  /* The form is a snapshot of the props taken when the modal opens — it is remounted per
     open, and a service edited underneath an open form should not have its fields yanked. */
  function seed() {
    return {
      name: service?.name ?? '',
      command: service?.command ?? '',
      cwd: service?.cwd ?? defaultCwd,
      envText: (service?.env ?? []).map(([k, v]) => `${k}=${v}`).join('\n'),
      autoStart: service?.auto_start ?? true,
      restart: (service?.restart ?? 'on_crash') as ServiceRestart,
      readyPattern: service?.ready_pattern ?? '',
    };
  }
  const s0 = seed();
  let name = $state(s0.name);
  let command = $state(s0.command);
  let cwd = $state(s0.cwd);
  let envText = $state(s0.envText);
  let autoStart = $state(s0.autoStart);
  let restart = $state<ServiceRestart>(s0.restart);
  let readyPattern = $state(s0.readyPattern);
  let nameEl = $state<HTMLInputElement | null>(null);

  $effect(() => {
    const id = requestAnimationFrame(() => nameEl?.focus());
    return () => cancelAnimationFrame(id);
  });

  /** Rust refuses `{checkout}` in a command too, at every start; saying so here saves a
   *  service that could never run. */
  const commandError = $derived(
    mode === 'command' && checkoutInCommand(command)
      ? '{checkout} is a folder path, which may have spaces in it: use it in an environment value (ROOT={checkout}), not in the command.'
      : null,
  );
  const canSave = $derived(
    name.trim().length > 0 && (mode === 'shared' ? editingShared || !!source : command.trim().length > 0 && !commandError),
  );

  /** `K=V` per line; blank lines and `#` comments ignored; the first `=` splits. */
  function parseEnv(text: string): [string, string][] {
    const out: [string, string][] = [];
    for (const raw of text.split('\n')) {
      const line = raw.trim();
      if (!line || line.startsWith('#')) continue;
      const eq = line.indexOf('=');
      if (eq <= 0) continue;
      out.push([line.slice(0, eq).trim(), line.slice(eq + 1).trim()]);
    }
    return out;
  }

  const readyPatternError = $derived.by(() => {
    if (!readyPattern.trim()) return null;
    try { new RegExp(readyPattern); return null; } catch (e) { return String(e); }
  });

  function save() {
    if (!canSave) return;
    if (mode === 'shared') {
      // The source's command and folder are kept for what they say, never run from here.
      const from = editingShared ? null : source;
      onsubmit({
        name: name.trim(),
        command: from?.s.command ?? service?.command ?? '',
        cwd: from?.s.cwd ?? service?.cwd ?? '',
        auto_start: false,
        shared_from: from ? { workspace_id: from.ws.id, service_id: from.s.id } : (service?.shared_from ?? null),
      });
      return;
    }
    if (readyPatternError) return;
    onsubmit({
      name: name.trim(),
      command: command.trim(),
      cwd: cwd.trim(),
      env: parseEnv(envText),
      auto_start: autoStart,
      restart,
      ready_pattern: readyPattern.trim() || null,
    });
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      oncancel();
      return;
    }
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      save();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) oncancel();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="backdrop"
  onclick={handleBackdropClick}
  onkeydown={handleKeydown}
  role="dialog"
  aria-modal="true"
  aria-label={service ? 'Edit service' : 'Add a service'}
  tabindex="-1"
>
  <div class="panel">
    <div class="header">
      <div class="title">{service ? 'Edit service' : 'New service'}</div>
      <div class="subtitle">
        {#if mode === 'shared'}
          Another workspace's service, used here as it runs there.
        {:else}
          Runs as its own tab in this workspace. The shell stays up; the command runs in front of it.
        {/if}
      </div>
    </div>

    <div class="body">
      {#if !service}
        <div class="modes" role="radiogroup" aria-label="What this service is">
          <label class="check"><input type="radio" name="svc-mode" value="command" bind:group={mode} /> Run a command</label>
          <label class="check"><input type="radio" name="svc-mode" value="shared" bind:group={mode} disabled={sources.length === 0} /> Use the running service from another workspace</label>
        </div>
      {/if}

      <div class="row">
        <label class="field grow">
          <span class="label">Name</span>
          <input class="text-input" bind:this={nameEl} bind:value={name} placeholder="web" spellcheck="false" />
        </label>
        {#if mode === 'command'}
          <label class="field">
            <span class="label">On crash</span>
            <select class="text-input" bind:value={restart}>
              <option value="on_crash">Restart</option>
              <option value="never">Leave it</option>
            </select>
          </label>
        {/if}
      </div>

      {#if mode === 'shared'}
        {#if editingShared}
          <p class="note">Shared from another workspace: it's started and stopped there, and its port fills <code>{'{port:'}{name.trim() || 'name'}{'}'}</code> in this workspace's commands.</p>
        {:else}
          <label class="field">
            <span class="label">Service</span>
            <select class="text-input" bind:value={sourceKey}
              onchange={() => { if (source && !name.trim()) name = source.s.name; }}>
              <option value="" disabled>Pick one…</option>
              {#each sources as x (x.key)}
                <option value={x.key}>{x.s.name} — {x.ws.name}</option>
              {/each}
            </select>
            <span class="hint-line">This workspace never starts, stops or moves it. Its port fills <code>{'{port:'}{name.trim() || 'name'}{'}'}</code> in this workspace's commands.</span>
          </label>
        {/if}
      {:else}
      <label class="field">
        <span class="label">Command <span class="opt">optional placeholders: {'{port}'} runs a copy per branch, {'{branch}'}, {'{worker}'}, {'{port:<service>}'}</span></span>
        <input class="text-input mono" bind:value={command} placeholder="npm run dev" spellcheck="false"
          onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); save(); } }} />
        {#if commandError}<span class="error">{commandError}</span>{/if}
      </label>

      <label class="field">
        <span class="label">Directory</span>
        <input class="text-input mono" bind:value={cwd} placeholder="/path/to/project" spellcheck="false" />
      </label>

      <label class="field">
        <span class="label">Environment <span class="opt">optional — one KEY=value per line</span></span>
        <textarea class="text-input mono" rows="3" bind:value={envText} placeholder="PORT=5173" spellcheck="false"></textarea>
      </label>

      <label class="field">
        <span class="label">Ready when output matches <span class="opt">optional — maiTerm recognises the usual shapes on its own; set this to override it</span></span>
        <input class="text-input mono" bind:value={readyPattern} placeholder="Local:\s+http://localhost:(?<port>\d+)" spellcheck="false" />
        {#if readyPatternError}<span class="error">{readyPatternError}</span>{/if}
      </label>

      <label class="check">
        <input type="checkbox" bind:checked={autoStart} />
        <span>Start with the workspace</span>
      </label>
      {/if}
    </div>

    <div class="footer">
      <span class="hint">{modLabel}+Enter to save</span>
      <div class="actions">
        <button type="button" class="btn" onclick={oncancel}>Cancel</button>
        <button type="button" class="btn primary" disabled={!canSave || (mode === 'command' && !!readyPatternError)} onclick={save}>
          {service ? 'Save' : 'Add service'}
        </button>
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 12vh;
    z-index: 1000;
  }

  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    max-height: 76vh;
    width: 500px;
  }

  .header {
    border-bottom: 1px solid var(--bg-light);
    padding: 12px 14px 10px;
  }

  .title {
    color: var(--fg);
    font-size: 1rem;
    font-weight: 600;
  }

  .subtitle {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.4;
    margin-top: 3px;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 12px;
    overflow-y: auto;
    padding: 14px;
  }

  .row { display: flex; gap: 10px; }
  .grow { flex: 1; }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
    min-width: 0;
  }

  .label {
    color: var(--fg-dim);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .opt {
    font-weight: 400;
    letter-spacing: 0;
    opacity: 0.7;
    text-transform: none;
  }

  .text-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 7px 9px;
    width: 100%;
  }
  .text-input.mono { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.8rem; }
  .text-input:focus {
    border-color: var(--accent);
    outline: none;
  }
  .text-input::placeholder { color: var(--fg-dim); }
  textarea.text-input { resize: vertical; }

  .error { color: var(--red); font-size: 0.72rem; }
  .modes { display: flex; flex-direction: column; gap: 6px; }
  .note { background: var(--bg-dark); border-radius: 5px; color: var(--fg); font-size: 0.8rem; line-height: 1.4; margin: 0; padding: 8px 10px; }
  .hint-line { color: var(--fg-dim); font-size: 0.72rem; line-height: 1.4; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; }

  .check {
    align-items: center;
    color: var(--fg);
    cursor: pointer;
    display: flex;
    font-size: 0.85rem;
    gap: 8px;
  }
  .check input { accent-color: var(--accent); }

  .footer {
    align-items: center;
    border-top: 1px solid var(--bg-light);
    display: flex;
    gap: 10px;
    justify-content: space-between;
    padding: 10px 14px;
  }

  .hint {
    color: var(--fg-dim);
    font-size: 0.72rem;
  }

  .actions { display: flex; gap: 7px; }

  .btn {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.8rem;
    padding: 6px 12px;
  }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg-dark);
    font-weight: 600;
  }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
