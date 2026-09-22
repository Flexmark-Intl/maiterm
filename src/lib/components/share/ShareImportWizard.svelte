<script lang="ts">
  // Import a shared workspace (docs/workspace-share.md §4): map every root to a directory here,
  // probe access, clone what's missing in visible tabs, then build the workspace.
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import Button from '$lib/components/ui/Button.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { dispatch } from '$lib/stores/notificationDispatch';
  import {
    getPtyForegroundJob, writeTerminal, shareCheckDirs, shareCloneCommand, shareImportBuild, shareProbe, shareReadFile,
    type ShareDirVerdict, type ShareImportPreview, type ShareProbe, type SharedRoot,
  } from '$lib/tauri/commands';
  import { launchContextFor } from '$lib/share/share';

  interface Props {
    path: string;
    onclose: () => void;
  }
  let { path, onclose }: Props = $props();

  type CloneState = 'idle' | 'running' | 'done' | 'failed' | 'manual';
  interface RootRow {
    root: SharedRoot;
    repoName: string | null;
    /** null = skipped: its tabs open in the home directory */
    dest: string | null;
    verdict: ShareDirVerdict | null;
    /** the recorded path was rejected — shown so the user knows why they're being asked */
    recordedRejected: string | null;
    probe: ShareProbe | null;
    clone: CloneState;
    cloneTab: { workspaceId: string; paneId: string; tabId: string } | null;
    cloneError: string | null;
    skipped: boolean;
  }

  let preview = $state<ShareImportPreview | null>(null);
  let loadError = $state<string | null>(null);
  let rows = $state<RootRow[]>([]);
  let phase = $state<'map' | 'cloning' | 'building' | 'done'>('map');
  let includeNotes = $state(false);
  let includeTasks = $state(false);
  let serviceOn = $state<boolean[]>([]);
  /** service index → var → value, for values the sender didn't send */
  let envInputs = $state<Record<number, Record<string, string>>>({});
  let notices = $state<string[]>([]);
  let modalEl = $state<HTMLDivElement | undefined>();
  let createAllMessage = $state<string | null>(null);

  $effect(() => {
    requestAnimationFrame(() => modalEl?.focus());
  });

  (async () => {
    try {
      const p = await shareReadFile(path);
      rows = p.roots.map((c) => {
        const root = p.file.roots.find(r => r.id === c.root_id)!;
        const rejected = c.verdict === 'reject' ? c.reason : null;
        return {
          root,
          repoName: c.repo_name,
          dest: rejected ? null : c.local_path,
          verdict: rejected ? null : (c.verdict === 'use' ? { verdict: 'use' } : { verdict: 'clone' }),
          recordedRejected: rejected ? `${c.local_path}: ${rejected}` : null,
          probe: null,
          clone: 'idle',
          cloneTab: null,
          cloneError: null,
          skipped: false,
        } satisfies RootRow;
      });
      serviceOn = p.file.services.map(() => true);
      envInputs = Object.fromEntries(p.file.services.map((s, i) => [i, Object.fromEntries(s.env.filter(e => e.value == null).map(e => [e.name, '']))]));
      preview = p;
      probeAll();
    } catch (e) {
      loadError = String(e);
      logError(`share: reading ${path} failed: ${e}`);
    }
  })();

  async function probeAll() {
    const targets = rows.map((r, i) => ({ r, i })).filter(({ r }) => r.root.kind === 'git');
    if (targets.length === 0) return;
    const url = (r: SharedRoot) => r.remotes.origin ?? Object.values(r.remotes)[0];
    try {
      const results = await shareProbe(targets.map(({ r }) => ({ url: url(r.root), branch: r.root.branch ?? null })));
      results.forEach((p, k) => { rows[targets[k].i].probe = p; });
    } catch (e) {
      logError(`share: probe failed: ${e}`);
    }
  }

  function cloneUrl(r: SharedRoot): string {
    return r.remotes.origin ?? Object.values(r.remotes)[0];
  }

  function joinPath(parent: string, name: string): string {
    const sep = parent.includes('\\') && !parent.includes('/') ? '\\' : '/';
    return parent.replace(/[\\/]+$/, '') + sep + name;
  }

  async function checkOne(row: RootRow, dir: string): Promise<ShareDirVerdict> {
    const [v] = await shareCheckDirs([{ root: row.root, dir }]);
    return v;
  }

  async function pickFor(i: number) {
    const dir = await dialogOpen({ directory: true, multiple: false, title: `Where should ${rows[i].repoName ?? rows[i].root.path} go?` });
    if (typeof dir !== 'string') return;
    const v = await checkOne(rows[i], dir);
    if (v.verdict === 'reject') {
      rows[i].recordedRejected = `${dir}: ${v.reason}`;
      return;
    }
    rows[i].dest = dir;
    rows[i].verdict = v;
    rows[i].recordedRejected = null;
    rows[i].skipped = false;
  }

  function skip(i: number) {
    rows[i].dest = null;
    rows[i].verdict = null;
    rows[i].recordedRejected = null;
    rows[i].skipped = true;
  }

  const unresolvedGit = $derived(rows.filter(r => r.root.kind === 'git' && !r.verdict && !r.skipped));

  async function createAllIn() {
    const parent = await dialogOpen({ directory: true, multiple: false, title: 'Create the repositories in…' });
    if (typeof parent !== 'string') return;
    const idx = rows.map((r, i) => ({ r, i })).filter(({ r }) => r.root.kind === 'git' && !r.verdict && !r.skipped);
    const checks = idx.map(({ r }) => ({ root: r.root, dir: joinPath(parent, r.repoName ?? 'repo') }));
    const verdicts = await shareCheckDirs(checks);
    let rejected = 0;
    verdicts.forEach((v, k) => {
      const i = idx[k].i;
      if (v.verdict === 'reject') {
        rejected++;
        rows[i].recordedRejected = `${checks[k].dir}: ${v.reason}`;
      } else {
        rows[i].dest = checks[k].dir;
        rows[i].verdict = v;
        rows[i].recordedRejected = null;
      }
    });
    createAllMessage = rejected ? `${rejected} couldn't go there — choose a directory for ${rejected === 1 ? 'it' : 'each'} below.` : null;
  }

  function resolved(r: RootRow): boolean {
    return !!r.skipped || !!r.verdict;
  }
  const denied = $derived(rows.filter(r => r.verdict?.verdict === 'clone' && r.probe?.outcome === 'denied'));
  const canStart = $derived(!!preview && phase === 'map' && rows.every(resolved) && denied.length === 0);

  // ── Clone (§4 step 2) — visible tabs, one per repo ─────────────────────────────

  async function startImport() {
    phase = 'cloning';
    const toClone = rows.filter(r => r.verdict?.verdict === 'clone' && r.dest);
    for (const row of toClone) await startClone(row, true);
    maybeBuild();
  }

  async function startClone(row: RootRow, withBranch: boolean) {
    const wsId = workspacesStore.activeWorkspaceId;
    const ws = workspacesStore.workspaces.find(w => w.id === wsId);
    const paneId = ws?.active_pane_id ?? ws?.panes[0]?.id;
    if (!ws || !paneId || !row.dest) {
      row.clone = 'failed';
      row.cloneError = 'No workspace to open the clone tab in';
      return;
    }
    const branchMissing = row.probe?.outcome === 'reachable' && row.probe.branch_exists === false;
    const branch = withBranch && !branchMissing ? row.root.branch ?? null : null;
    const cmd = await shareCloneCommand(cloneUrl(row.root), row.dest, branch);
    row.clone = 'running';
    row.cloneError = null;
    if (row.cloneTab) {
      const inst = terminalsStore.get(row.cloneTab.tabId);
      if (inst) {
        await writeTerminal(inst.ptyId, Array.from(new TextEncoder().encode(cmd + '\n')));
        watchClone(row);
        return;
      }
    }
    const tab = await workspacesStore.createTab(ws.id, paneId, `clone ${row.repoName ?? ''}`.trim(), {
      background: true,
      context: { cwd: null, sshCommand: null, remoteCwd: null, launchCommand: cmd },
    });
    row.cloneTab = { workspaceId: ws.id, paneId, tabId: tab.id };
    // A background tab has no TerminalPane until something mounts it (+page's activatedTabIds),
    // and no PTY to type the clone into until then.
    window.dispatchEvent(new CustomEvent('activate-tab', { detail: tab.id }));
    watchClone(row);
  }

  /** Done = the shell is back at its prompt after having been busy, and the directory is now a
   *  checkout of this repo. A partial clone already has `.git` and the remote, so the verdict
   *  alone would call a clone done the moment it started. */
  async function watchClone(row: RootRow) {
    const started = Date.now();
    let sawBusy = false;
    let unknownPolls = 0;
    while (row.clone === 'running') {
      await new Promise(r => setTimeout(r, 1000));
      const inst = row.cloneTab ? terminalsStore.get(row.cloneTab.tabId) : undefined;
      if (!inst) {
        if (Date.now() - started > 15_000) { row.clone = 'failed'; row.cloneError = 'The clone tab was closed'; }
        continue;
      }
      let atPrompt: boolean | null = null;
      try { atPrompt = (await getPtyForegroundJob(inst.ptyId)).shell_at_prompt; } catch { continue; }
      if (atPrompt === null) {
        // This platform can't tell (docs/stack.md) — ask the human rather than guess.
        if (++unknownPolls > 3) row.clone = 'manual';
        continue;
      }
      if (!atPrompt) { sawBusy = true; continue; }
      if (!sawBusy && Date.now() - started < 6_000) continue;
      await finishClone(row);
    }
    maybeBuild();
  }

  async function finishClone(row: RootRow) {
    const v = await checkOne(row, row.dest!);
    if (v.verdict === 'use') {
      row.clone = 'done';
      logInfo(`share: cloned ${cloneUrl(row.root)} into ${row.dest}`);
      const t = row.cloneTab;
      if (t) workspacesStore.closeTabOrPane(t.workspaceId, t.paneId, t.tabId).catch(() => {});
      row.cloneTab = null;
    } else {
      row.clone = 'failed';
      row.cloneError = 'git clone didn\'t finish — its tab shows why';
    }
  }

  async function markManualDone(row: RootRow) {
    row.clone = 'running';
    await finishClone(row);
    maybeBuild();
  }

  async function retry(row: RootRow) {
    const v = await checkOne(row, row.dest!);
    if (v.verdict === 'reject') {
      row.cloneError = `${row.dest}: ${v.reason}. Remove it, or cancel and choose another directory.`;
      return;
    }
    // A missing branch is the likeliest reason a clone with --branch failed: retry without it.
    await startClone(row, false);
  }

  // ── Build (§4 step 3) ──────────────────────────────────────────────────────────

  function maybeBuild() {
    if (phase !== 'cloning') return;
    const pending = rows.filter(r => r.verdict?.verdict === 'clone' && r.clone !== 'done');
    if (pending.length === 0) build();
  }

  async function build() {
    if (!preview) return;
    phase = 'building';
    try {
      const mapping: Record<string, string> = {};
      for (const r of rows) if (r.dest && r.verdict) mapping[r.root.id] = r.dest;
      const result = await shareImportBuild(path, {
        mapping,
        include_notes: includeNotes,
        include_tasks: includeTasks,
        services: serviceOn.map((on, i) => (on ? i : -1)).filter(i => i >= 0),
        env_values: envInputs,
      });
      // Contexts first: adopting the workspace is what mounts its tabs (§5).
      for (const l of result.launches) terminalsStore.setSplitContext(l.tab_id, launchContextFor(l));
      await workspacesStore.adoptImportedWorkspace(result.workspace.id);
      // Only a pane's active tab mounts on its own; an agent tab behind it would sit unlaunched
      // until clicked, holding its launch context in memory that a restart would lose.
      for (const l of result.launches) window.dispatchEvent(new CustomEvent('activate-tab', { detail: l.tab_id }));
      notices = result.notices;
      phase = 'done';
      dispatch('Workspace imported', result.workspace.name, 'info');
      if (notices.length === 0) onclose();
    } catch (e) {
      logError(`share: import failed: ${e}`);
      dispatch('Import failed', String(e), 'error');
      phase = 'map';
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && phase !== 'building') { e.stopPropagation(); onclose(); }
  }

  const agentTabs = $derived((preview?.file.workspace.panes ?? []).flatMap(p => p.tabs).filter(t => t.agent).length);
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget && phase === 'map') onclose(); }}>
  <div class="modal" role="dialog" aria-modal="true" tabindex="-1" bind:this={modalEl} onkeydown={handleKeydown}>
    <div class="header">
      <h2>Import “{preview?.file.workspace.name ?? path.split(/[\\/]/).pop()}”</h2>
      <IconButton tooltip="Close" style="font-size: 1.538rem;padding:4px 8px;width:auto;height:auto" onclick={onclose} disabled={phase === 'building'}>&times;</IconButton>
    </div>

    <div class="content">
      {#if loadError}
        <p class="error">{loadError}</p>
      {:else if !preview}
        <p class="dim">Reading the file…</p>
      {:else if phase === 'done'}
        <p>The workspace is ready. A few things didn't land exactly where they were:</p>
        <ul class="notices">{#each notices as n}<li>{n}</li>{/each}</ul>
      {:else}
        <p class="lede">
          Shared from maiTerm {preview.file.maiterm_version} on {preview.file.exported_at.slice(0, 10)}.
          {#if preview.file.workspace.mesh}It's a mesh workspace: its agents will be bridged to each other.{/if}
          {#if agentTabs > 0}{agentTabs} tab{agentTabs === 1 ? '' : 's'} will start an agent.{/if}
        </p>

        {#if rows.length > 0}
          <h3>Directories</h3>
          {#if unresolvedGit.length > 0 && phase === 'map'}
            <div class="create-all">
              <Button variant="secondary" onclick={createAllIn}>Create all in…</Button>
              <span class="dim">Clones each missing repository into a folder named after it.</span>
            </div>
            {#if createAllMessage}<p class="warn">{createAllMessage}</p>{/if}
          {/if}
          <div class="list">
            {#each rows as r, i (r.root.id)}
              <div class="row">
                <div class="main">
                  <span class="name">{r.repoName ?? r.root.path}</span>
                  {#if r.root.kind === 'git'}<span class="mono dim">{cloneUrl(r.root)}{#if r.root.branch} · {r.root.branch}{/if}</span>{/if}
                </div>
                <div class="sub">
                  {#if r.skipped}
                    <span class="dim">Skipped — its tabs open in your home directory.</span>
                  {:else if r.verdict?.verdict === 'use'}
                    <span class="ok">Uses</span> <span class="mono">{r.dest}</span>
                  {:else if r.verdict?.verdict === 'clone'}
                    <span class="ok">Will clone into</span> <span class="mono">{r.dest}</span>
                  {:else if r.root.kind === 'plain'}
                    <span class="warn">Choose a directory for this — it isn't a repository maiTerm can clone.</span>
                  {:else}
                    <span class="warn">Choose where to clone this.</span>
                  {/if}
                </div>
                {#if r.recordedRejected}<div class="sub dim">Not used: {r.recordedRejected}</div>{/if}
                {#if r.verdict?.verdict === 'clone' && r.probe}
                  {#if r.probe.outcome === 'reachable'}
                    {#if r.probe.branch_exists === false}<div class="sub warn">Branch {r.root.branch} isn't on the remote — the default branch will be cloned.</div>{/if}
                  {:else if r.probe.outcome === 'denied'}
                    <div class="sub error">You don't have access to this repository: {r.probe.message}</div>
                  {:else}
                    <div class="sub dim">Couldn't check access ({r.probe.message}) — the clone will ask for anything it needs.</div>
                  {/if}
                {/if}
                {#if phase === 'cloning' && r.verdict?.verdict === 'clone'}
                  <div class="sub">
                    {#if r.clone === 'running'}<span class="dim">Cloning… (see its tab)</span>
                    {:else if r.clone === 'done'}<span class="ok">Cloned</span>
                    {:else if r.clone === 'manual'}
                      <span class="dim">maiTerm can't tell when the clone finishes here.</span>
                      <button class="link" onclick={() => markManualDone(r)}>It's finished</button>
                    {:else if r.clone === 'failed'}
                      <span class="error">{r.cloneError}</span> <button class="link" onclick={() => retry(r)}>Retry</button>
                    {/if}
                  </div>
                {/if}
                {#if phase === 'map'}
                  <div class="actions">
                    <button class="link" onclick={() => pickFor(i)}>{r.verdict ? 'Change…' : 'Choose…'}</button>
                    {#if !r.skipped}<button class="link" onclick={() => skip(i)}>Skip</button>{/if}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        {#if preview.file.services.length > 0 && phase === 'map'}
          <h3>Stack services</h3>
          <div class="list">
            {#each preview.file.services as s, i}
              <div class="row" class:off={!serviceOn[i]}>
                <label class="check"><input type="checkbox" bind:checked={serviceOn[i]} /> <span class="name">{s.name}</span> <span class="mono dim">{s.command}</span></label>
                {#if serviceOn[i] && Object.keys(envInputs[i] ?? {}).length > 0}
                  <div class="env">
                    <span class="dim">The sender left these for you to fill in:</span>
                    {#each Object.keys(envInputs[i]) as name (name)}
                      <label class="env-row"><span class="mono">{name}</span><input type="text" bind:value={envInputs[i][name]} spellcheck="false" autocomplete="off" /></label>
                    {/each}
                  </div>
                {/if}
              </div>
            {/each}
          </div>
        {/if}

        {#if (preview.file.notes || preview.file.tasks) && phase === 'map'}
          <h3>Also import</h3>
          {#if preview.file.notes}<label class="check"><input type="checkbox" bind:checked={includeNotes} /> Notes</label>{/if}
          {#if preview.file.tasks}<label class="check"><input type="checkbox" bind:checked={includeTasks} /> Task board ({preview.file.tasks.tasks.length})</label>{/if}
        {/if}

        {#if denied.length > 0}
          <p class="error block">Fix access to {denied.length === 1 ? 'that repository' : 'those repositories'}, or point {denied.length === 1 ? 'it' : 'them'} at a checkout you already have, to continue.</p>
        {/if}
      {/if}
    </div>

    <div class="footer">
      {#if phase === 'done'}
        <Button variant="primary" onclick={onclose}>Done</Button>
      {:else}
        <Button variant="secondary" onclick={onclose} disabled={phase === 'building'}>Cancel</Button>
        <Button variant="primary" onclick={startImport} disabled={!canStart}>
          {phase === 'cloning' ? 'Cloning…' : phase === 'building' ? 'Setting up…' : 'Set up workspace'}
        </Button>
      {/if}
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.6); display: flex; align-items: center; justify-content: center; z-index: 1000; }
  .modal { background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 10px; width: 620px; max-width: calc(100vw - 32px); max-height: 85vh; display: flex; flex-direction: column; box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4); outline: none; }
  .header { display: flex; align-items: center; justify-content: space-between; padding: 16px 20px 12px; border-bottom: 1px solid var(--bg-light); }
  .header h2 { font-size: 1.154rem; font-weight: 600; color: var(--fg); margin: 0; overflow-wrap: anywhere; }
  .content { padding: 14px 20px; overflow-y: auto; flex: 1; min-height: 0; color: var(--fg); font-size: 0.923rem; }
  .lede { color: var(--fg-dim); margin: 0 0 8px; line-height: 1.45; }
  h3 { font-size: 0.846rem; font-weight: 600; color: var(--fg-dim); text-transform: uppercase; letter-spacing: 0.5px; margin: 16px 0 6px; }
  .list { display: flex; flex-direction: column; gap: 4px; }
  .row { border: 1px solid var(--bg-light); border-radius: 6px; padding: 6px 10px; }
  .row.off { opacity: 0.55; }
  .main { display: flex; align-items: baseline; gap: 8px; flex-wrap: wrap; }
  .sub { margin-top: 3px; font-size: 0.846rem; overflow-wrap: anywhere; }
  .actions { margin-top: 4px; display: flex; gap: 12px; }
  .name { overflow-wrap: anywhere; }
  .mono { font-family: monospace; font-size: 0.846rem; overflow-wrap: anywhere; }
  .dim { color: var(--fg-dim); }
  .ok { color: var(--green); }
  .warn { color: var(--yellow); overflow-wrap: anywhere; }
  .error { color: var(--red); overflow-wrap: anywhere; }
  .block { margin-top: 14px; }
  .create-all { display: flex; align-items: center; gap: 10px; margin-bottom: 6px; flex-wrap: wrap; }
  .check { display: flex; align-items: center; gap: 6px; cursor: pointer; flex-wrap: wrap; }
  .check input { accent-color: var(--accent); }
  .env { margin: 6px 0 2px 22px; display: flex; flex-direction: column; gap: 4px; }
  .env-row { display: flex; gap: 8px; align-items: center; }
  .env-row input { flex: 1; min-width: 0; background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 4px; color: var(--fg); padding: 3px 6px; font-family: monospace; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font-size: 0.846rem; }
  .notices { margin: 8px 0 0; padding-left: 18px; color: var(--fg-dim); }
  .footer { display: flex; justify-content: flex-end; gap: 8px; padding: 12px 20px; border-top: 1px solid var(--bg-light); }
</style>
