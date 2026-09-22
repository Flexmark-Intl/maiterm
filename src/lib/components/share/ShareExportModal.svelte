<script lang="ts">
  // Share Workspace (docs/workspace-share.md §3): what will travel, the sender's choices, and
  // warnings for any repo whose receiver wouldn't get the sender's code.
  import { save as dialogSave } from '@tauri-apps/plugin-dialog';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Button from '$lib/components/ui/Button.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { dispatch } from '$lib/stores/notificationDispatch';
  import { shareExportPreview, shareExportWrite, type ShareExportPreview, type ShareTabContext } from '$lib/tauri/commands';
  import { gatherShareContexts, SHARE_EXTENSION } from '$lib/share/share';

  interface Props {
    workspaceId: string;
    onclose: () => void;
  }
  let { workspaceId, onclose }: Props = $props();

  let preview = $state<ShareExportPreview | null>(null);
  let contexts: ShareTabContext[] = [];
  let loadError = $state<string | null>(null);
  let saving = $state(false);
  let includeNotes = $state(false);
  let includeTasks = $state(false);
  let agentTabs = $state(new Set<string>());
  let services = $state(new Set<string>());
  /** service id → env var names whose VALUES travel */
  let envValues = $state<Record<string, Set<string>>>({});
  let modalEl = $state<HTMLDivElement | undefined>();

  const ws = $derived(workspacesStore.workspaces.find(w => w.id === workspaceId));

  $effect(() => {
    // Svelte's autofocus is not focus (CLAUDE.md) — focus explicitly so Escape works.
    requestAnimationFrame(() => modalEl?.focus());
  });

  (async () => {
    const w = workspacesStore.workspaces.find(x => x.id === workspaceId);
    if (!w) { loadError = 'Workspace not found'; return; }
    try {
      contexts = await gatherShareContexts(w);
      const p = await shareExportPreview(workspaceId, contexts);
      agentTabs = new Set(p.tabs.filter(t => t.agent).map(t => t.id));
      services = new Set(p.services.map(s => s.id));
      envValues = Object.fromEntries(p.services.map(s => [s.id, new Set<string>()]));
      preview = p;
    } catch (e) {
      loadError = String(e);
      logError(`share: preview failed: ${e}`);
    }
  })();

  const rootsById = $derived(new Map((preview?.roots ?? []).map(r => [r.id, r])));
  const warnings = $derived((preview?.roots ?? []).filter(r => r.warning));

  function toggle(set: Set<string>, id: string): Set<string> {
    const next = new Set(set);
    if (next.has(id)) next.delete(id); else next.add(id);
    return next;
  }

  function toggleEnv(serviceId: string, name: string) {
    envValues = { ...envValues, [serviceId]: toggle(envValues[serviceId] ?? new Set(), name) };
  }

  function setAllEnv(serviceId: string, names: string[], on: boolean) {
    envValues = { ...envValues, [serviceId]: new Set(on ? names : []) };
  }

  function rootLabel(id: string | null): string {
    if (!id) return '';
    return rootsById.get(id)?.path ?? '';
  }

  function remoteOf(r: { remotes: Record<string, string> }): string | null {
    return r.remotes.origin ?? Object.values(r.remotes)[0] ?? null;
  }

  const runtimeName: Record<string, string> = { claude: 'Claude', codex: 'Codex', gemini: 'Gemini' };

  async function handleSave() {
    if (!preview || saving) return;
    const safeName = preview.name.replace(/[\\/:*?"<>|]+/g, '-').trim() || 'workspace';
    const path = await dialogSave({
      defaultPath: `${safeName}.${SHARE_EXTENSION}`,
      filters: [{ name: 'maiTerm Workspace', extensions: [SHARE_EXTENSION] }],
    });
    if (!path) return;
    saving = true;
    try {
      await shareExportWrite(workspaceId, contexts, {
        include_notes: includeNotes,
        include_tasks: includeTasks,
        services: [...services].map(id => ({ id, env_values: [...(envValues[id] ?? [])] })),
        agent_tab_ids: [...agentTabs],
      }, path);
      dispatch('Workspace shared', `Saved ${path.split(/[\\/]/).pop()}`, 'info');
      onclose();
    } catch (e) {
      logError(`share: export failed: ${e}`);
      dispatch('Share failed', String(e), 'error');
      saving = false;
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') { e.stopPropagation(); onclose(); }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) onclose(); }}>
  <div class="modal" role="dialog" aria-modal="true" tabindex="-1" bind:this={modalEl} onkeydown={handleKeydown}>
    <div class="header">
      <h2>Share “{ws?.name ?? preview?.name ?? ''}”</h2>
      <IconButton tooltip="Close" style="font-size: 1.538rem;padding:4px 8px;width:auto;height:auto" onclick={onclose}>&times;</IconButton>
    </div>

    <div class="content">
      <p class="lede">
        Saves a file someone else can open to set this workspace up on their computer: the same
        tabs, in the same repositories, cloning any they don't have. Preferences, accounts,
        scrollback and your agents' sessions never travel.
      </p>

      {#if loadError}
        <p class="error">{loadError}</p>
      {:else if !preview}
        <p class="dim">Reading the workspace…</p>
      {:else}
        {#if ws?.bridge_all}
          <p class="note">Mesh workspace — the mesh and each tab's role travel; the receiver's agents join it as they start.</p>
        {/if}

        {#if preview.roots.length > 0}
          <h3>Directories</h3>
          <div class="list">
            {#each preview.roots as r (r.id)}
              <div class="row">
                <div class="main">
                  <span class="mono">{r.path}</span>
                  <span class="dim">{r.tab_count} tab{r.tab_count === 1 ? '' : 's'}</span>
                </div>
                {#if r.kind === 'git'}
                  <div class="sub mono">{remoteOf(r)}{#if r.branch} · {r.branch}{/if}</div>
                {:else}
                  <div class="sub">plain directory — nothing to clone</div>
                {/if}
                {#if r.warning}<div class="warn">{r.warning}</div>{/if}
              </div>
            {/each}
          </div>
        {/if}

        <h3>Tabs</h3>
        <div class="list">
          {#each preview.tabs as t (t.id)}
            <div class="row" class:dropped={t.kind === 'dropped'}>
              <div class="main">
                <span class="name">{t.name}</span>
                <span class="dim">
                  {#if t.kind === 'dropped'}not shared — {t.dropped_reason}
                  {:else if t.kind === 'remote' || t.kind === 'remote_editor'}{t.place}
                  {:else if t.root_id}{rootLabel(t.root_id)}
                  {:else}home directory{/if}
                </span>
              </div>
              {#if t.agent}
                <label class="check sub">
                  <input type="checkbox" checked={agentTabs.has(t.id)} onchange={() => (agentTabs = toggle(agentTabs, t.id))} />
                  Start {runtimeName[t.agent.runtime] ?? t.agent.runtime}
                  <span class="dim">— {t.agent.forkable ? 'forks this remote session' : 'a fresh session'} ({t.agent.reason})</span>
                </label>
              {/if}
            </div>
          {/each}
        </div>

        {#if preview.services.length > 0}
          <h3>Stack services</h3>
          <div class="list">
            {#each preview.services as s (s.id)}
              <div class="row" class:dropped={!services.has(s.id)}>
                <label class="check main">
                  <input type="checkbox" checked={services.has(s.id)} onchange={() => (services = toggle(services, s.id))} />
                  <span class="name">{s.name}</span>
                </label>
                {#if services.has(s.id) && s.env_names.length > 0}
                  <div class="env">
                    <div class="env-head">
                      <span class="dim">Send values for:</span>
                      <button class="link" onclick={() => setAllEnv(s.id, s.env_names, true)}>all</button>
                      <button class="link" onclick={() => setAllEnv(s.id, s.env_names, false)}>none</button>
                    </div>
                    {#each s.env_names as name (name)}
                      <label class="check">
                        <input type="checkbox" checked={envValues[s.id]?.has(name) ?? false} onchange={() => toggleEnv(s.id, name)} />
                        <span class="mono">{name}</span>
                      </label>
                    {/each}
                    <div class="dim small">Unticked values are left for the receiver to fill in.</div>
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        <h3>Also include</h3>
        <label class="check">
          <input type="checkbox" bind:checked={includeNotes} disabled={preview.note_count === 0} />
          Notes <span class="dim">({preview.note_count})</span>
        </label>
        <label class="check">
          <input type="checkbox" bind:checked={includeTasks} disabled={preview.task_count === 0} />
          Task board <span class="dim">({preview.task_count} task{preview.task_count === 1 ? '' : 's'})</span>
        </label>

        {#if warnings.length > 0}
          <p class="warn block">
            {warnings.length} director{warnings.length === 1 ? 'y' : 'ies'} above won't give the receiver exactly your
            code. You can still share; they'll see the same warning.
          </p>
        {/if}
      {/if}
    </div>

    <div class="footer">
      <Button variant="secondary" onclick={onclose} disabled={saving}>Cancel</Button>
      <Button variant="primary" onclick={handleSave} disabled={!preview || saving}>{saving ? 'Saving…' : 'Save…'}</Button>
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
    z-index: 1000;
  }
  .modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    width: 600px;
    max-width: calc(100vw - 32px);
    max-height: 85vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    outline: none;
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--bg-light);
  }
  .header h2 { font-size: 1.154rem; font-weight: 600; color: var(--fg); margin: 0; overflow-wrap: anywhere; }
  .content { padding: 14px 20px; overflow-y: auto; flex: 1; min-height: 0; }
  .lede { font-size: 0.923rem; color: var(--fg-dim); margin: 0 0 12px; line-height: 1.45; }
  h3 { font-size: 0.846rem; font-weight: 600; color: var(--fg-dim); text-transform: uppercase; letter-spacing: 0.5px; margin: 16px 0 6px; }
  .list { display: flex; flex-direction: column; gap: 4px; }
  .row { border: 1px solid var(--bg-light); border-radius: 6px; padding: 6px 10px; }
  .row.dropped { opacity: 0.55; }
  .main { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .sub { margin-top: 3px; font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .name { color: var(--fg); font-size: 0.923rem; overflow-wrap: anywhere; }
  .mono { font-family: monospace; font-size: 0.846rem; color: var(--fg); overflow-wrap: anywhere; }
  .dim { color: var(--fg-dim); font-size: 0.846rem; overflow-wrap: anywhere; }
  .small { font-size: 0.769rem; margin-top: 2px; }
  .warn { margin-top: 3px; font-size: 0.846rem; color: var(--yellow, #e0af68); overflow-wrap: anywhere; }
  .warn.block { margin-top: 14px; }
  .note { font-size: 0.923rem; color: var(--fg); background: var(--bg-dark); border-radius: 6px; padding: 8px 10px; margin: 0 0 8px; }
  .error { color: var(--red, #f7768e); font-size: 0.923rem; overflow-wrap: anywhere; }
  .check { display: flex; align-items: center; gap: 6px; font-size: 0.923rem; color: var(--fg); cursor: pointer; flex-wrap: wrap; }
  .check input { accent-color: var(--accent); }
  .env { margin: 6px 0 2px 22px; display: flex; flex-direction: column; gap: 3px; }
  .env-head { display: flex; gap: 8px; align-items: baseline; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font-size: 0.846rem; }
  .footer { display: flex; justify-content: flex-end; gap: 8px; padding: 12px 20px; border-top: 1px solid var(--bg-light); }
</style>
