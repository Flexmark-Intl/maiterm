<!--
  Make this a Hive… (docs/hive.md §3): the fields that make a workspace a Hive, checked as the
  human types (lib/hive/model.ts) and again by Rust on Create. Folders are detected as they're
  added (git or plain, Base, trust); an untrusted repo is a warning, never a refusal (§6.6).
  The Stack step (§3 step 3) imports: infrastructure another workspace already runs as a
  `shared_from` reference (one that is the repo's own code is offered unticked: it has to run
  per branch), standing in for the suggestion it repeats, and the folders' suggested services as definitions — one whose fixed
  port another workspace is using is refused and named, one with a free fixed port is imported
  as one-copy and flagged (`importVerdict`). Rust re-checks every row (`import_stack`).
-->
<script lang="ts">
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Button from '$lib/components/ui/Button.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { navHistoryStore } from '$lib/stores/navHistory.svelte';
  import { stackStore } from '$lib/stores/stack.svelte';
  import { importVerdict, sameService, sharesByDefault, type RunningElsewhere, type StackOffer } from '$lib/stack/model';
  import { normalizeTitle } from '$lib/tasks/model';
  import { hiveDetectFolder, hiveDetectTools, hivePreflight, suggestStack, type StackSuggestion } from '$lib/tauri/commands';
  import type { HiveDetectedTool, HivePreflight, HiveSpec, Service } from '$lib/tauri/types';
  import { charCount, folderName, MAX_INSTRUCTIONS, slugify, validateSpec } from '$lib/hive/model';
  import { createHive } from '$lib/hive/create';

  interface Props {
    workspaceId: string;
    onclose: () => void;
  }
  let { workspaceId, onclose }: Props = $props();

  interface Folder {
    key: number;
    name: string;
    path: string;
    /** "git" | "plain"; a detected repo may be shared as plain, never the reverse. */
    kind: string;
    detectedKind: string;
    base: string;
    detectedBase: string | null;
    setup: string;
    land_check: string;
    primary: boolean;
    trusted: boolean;
    repoTop: string | null;
  }

  const MODELS = ['opus', 'sonnet', 'haiku'];
  const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'];

  const ws = $derived(workspacesStore.workspaces.find((w) => w.id === workspaceId));
  let preflight = $state<HivePreflight | null>(null);
  /** Until the preflight (and its default Primary's detection) answers: the ceiling and the
   *  folder list show a loading line, never a ceiling of 1 or an empty list. */
  let primaryPending = $state(true);
  let loadError = $state<string | null>(null);
  let tools = $state<HiveDetectedTool[] | null>(null);

  let name = $state(workspacesStore.workspaces.find((w) => w.id === workspaceId)?.name ?? '');
  let folders = $state<Folder[]>([]);
  let folderKey = 0;
  let folderNote = $state<string | null>(null);
  let goal = $state('');
  let instructions = $state('');
  let cap = $state(4);
  let capConfirmed = $state(false);
  let landing = $state<'base' | 'integration'>('base');
  let coordModel = $state('opus');
  let coordEffort = $state('medium');
  let workerModel = $state('sonnet');
  let workerEffort = $state('high');
  let picked = $state(new Set<string>());
  let budgetDollars = $state(10);
  let pushAfterLand = $state(false);
  /** Every Hive tab made off the phone: Create launches the coordinator at once, so there's no
   *  later moment to exclude it before its first turn. */
  let keepOffPhone = $state(false);
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let suggestions = $state<Record<string, StackSuggestion[] | null>>({});

  let modalEl = $state<HTMLDivElement | undefined>();
  let nameEl = $state<HTMLInputElement | undefined>();
  $effect(() => {
    // Svelte's autofocus is not focus (CLAUDE.md): explicitly, so typing and Escape work.
    requestAnimationFrame(() => (nameEl ?? modalEl)?.focus());
  });

  (async () => {
    // The window's active tab gives the default Primary, even when it's the shell this Hive
    // replaces; failing a folder there, the tab looked at before it (§3). Rust takes the first
    // that has a local folder a Hive may use.
    const active = workspacesStore.activeTab?.id ?? null;
    const before = navHistoryStore.peekMostRecent((e) => e.tabId !== active)?.tabId ?? null;
    const ids = [active, before].filter((id): id is string => !!id);
    // Each terminal's OSC 7 folder: Rust's saved one lags a `cd` made just before opening this.
    const live = ids.map((id) => {
      const osc = terminalsStore.getOsc(id);
      return { tab_id: id, cwd: osc?.cwd ?? null, host: osc?.cwdHost ?? null };
    });
    try {
      const p = await hivePreflight(workspaceId, ids, live);
      preflight = p;
      cap = Math.min(p.default_cap, Math.max(p.ceiling, 1));
      if (p.default_primary && folders.length === 0) await addFolder(p.default_primary);
    } catch (e) {
      loadError = String(e);
      logError(`hive: preflight failed: ${e}`);
    } finally {
      primaryPending = false;
    }
  })();
  hiveDetectTools().then((t) => (tools = t)).catch((e) => { tools = []; logError(`hive: tool detection failed: ${e}`); });

  async function addFolder(path: string, at?: number) {
    folderNote = null;
    try {
      const info = await hiveDetectFolder(path);
      if (folders.some((f, i) => f.path === info.path && i !== at)) {
        folderNote = `${info.path} is already in the list.`;
        return;
      }
      const taken = folders.filter((_, i) => i !== at).map((f) => f.name);
      const f: Folder = {
        key: ++folderKey,
        name: folderName(info.path, taken),
        path: info.path,
        kind: info.kind,
        detectedKind: info.kind,
        base: info.base ?? '',
        detectedBase: info.base,
        setup: '',
        land_check: '',
        primary: at !== undefined ? folders[at].primary : !folders.some((x) => x.primary),
        trusted: info.trusted,
        repoTop: info.repo_top,
      };
      folders = at !== undefined ? folders.map((x, i) => (i === at ? f : x)) : [...folders, f];
      if (!(info.path in suggestions)) {
        suggestions = { ...suggestions, [info.path]: null };
        suggestStack(info.path)
          .then((s) => (suggestions = { ...suggestions, [info.path]: s.filter((x) => x.recommended) }))
          .catch(() => (suggestions = { ...suggestions, [info.path]: [] }));
      }
    } catch (e) {
      folderNote = String(e);
    }
  }

  async function pickFolder() {
    const path = await dialogOpen({ directory: true, multiple: false, title: 'Add a folder to the Hive' }).catch(() => null);
    if (typeof path === 'string' && path) await addFolder(path);
  }

  function removeFolder(key: number) {
    const gone = folders.find((f) => f.key === key);
    folders = folders.filter((f) => f.key !== key);
    if (gone?.primary && folders.length) folders = folders.map((f, i) => ({ ...f, primary: i === 0 }));
  }

  function setPrimary(key: number) {
    folders = folders.map((f) => ({ ...f, primary: f.key === key }));
  }

  function patch(key: number, p: Partial<Folder>) {
    folders = folders.map((f) => (f.key === key ? { ...f, ...p } : f));
  }

  /** Every service up in another workspace of this window, on its own folder. */
  const upElsewhere = $derived(
    workspacesStore.workspaces
      .filter((w) => w.id !== workspaceId)
      .flatMap((w) =>
        stackStore.services(w.id)
          .filter((s) => !s.shared_from && ['running', 'ready', 'starting'].includes(stackStore.status(s.id)))
          .map((s) => ({ s, ws: w })),
      ),
  );
  /** Services the human already runs, in another workspace, from inside one of these folders:
   *  offered as `shared_from` references, so the Hive never starts a second one. */
  const runningElsewhere = $derived(
    upElsewhere
      .filter(({ s }) => folders.some((f) => s.cwd === f.path || s.cwd.startsWith(f.path + '/')))
      .map(({ s, ws }) => ({ id: s.id, name: s.name, workspace: ws.name, workspaceId: ws.id, service: s })),
  );
  const clashes = $derived<RunningElsewhere[]>(
    upElsewhere.map(({ s, ws }) => ({ name: s.name, workspace: ws.name, command: s.command, cwd: s.cwd, port: s.port ?? null })),
  );

  /** The step's choices: shared sources by service id (default on for infrastructure only,
   *  `sharesByDefault`), suggestions by `path|name` (default on for the recommended ones), with
   *  the command as edited. */
  let shareChoice = $state<Record<string, boolean>>({});
  const offers = $derived<StackOffer[]>(folders.flatMap((f) => suggestions[f.path] ?? []));
  function shareOn(r: (typeof runningElsewhere)[number]): boolean {
    return shareChoice[r.id] ?? sharesByDefault(r.service, offers);
  }
  /** The ticked reference a suggestion repeats, which stands in for it: never imported beside
   *  it, so the sheet can't block its own default with a name clash. */
  function coveredBy(s: StackSuggestion, command: string) {
    return runningElsewhere.find((r) => shareOn(r) && sameService(r.service, { name: s.name, command, cwd: s.cwd }));
  }
  let picks = $state<Record<string, { on: boolean; command: string }>>({});
  function pickOf(path: string, s: StackSuggestion) {
    return picks[`${path}|${s.name}`] ?? { on: true, command: s.command };
  }
  function setPick(path: string, s: StackSuggestion, p: Partial<{ on: boolean; command: string }>) {
    picks = { ...picks, [`${path}|${s.name}`]: { ...pickOf(path, s), ...p } };
  }

  /** What Create sends as `stack`. A refused suggestion is never sent. */
  const stackRows = $derived.by(() => {
    const now = new Date().toISOString();
    const row = (over: Partial<Service> & Pick<Service, 'name' | 'command' | 'cwd'>): Service => ({
      id: '', normalized_name: '', env: [], ssh_command: null, restart: 'on_crash', ready_pattern: null,
      port: null, url: null, origin: 'suggested',
      // A Hive's services start through its run queue, not with the workspace.
      auto_start: false,
      created_at: now, updated_at: now, ...over,
    });
    const out: Service[] = [];
    for (const r of runningElsewhere) {
      if (!shareOn(r)) continue;
      out.push(row({ name: r.name, command: r.service.command, cwd: r.service.cwd, origin: 'human', shared_from: { workspace_id: r.workspaceId, service_id: r.id } }));
    }
    for (const f of folders) {
      for (const s of suggestions[f.path] ?? []) {
        const p = pickOf(f.path, s);
        if (!p.on || coveredBy(s, p.command) || importVerdict({ command: p.command, cwd: s.cwd }, clashes).kind === 'refused') continue;
        out.push(row({ name: s.name, command: p.command.trim(), cwd: s.cwd }));
      }
    }
    return out;
  });
  const stackProblem = $derived.by(() => {
    const seen = new Set<string>();
    for (const r of stackRows) {
      const k = normalizeTitle(r.name);
      if (seen.has(k)) return `Two stack services are called ${r.name}: untick one.`;
      seen.add(k);
    }
    return null;
  });

  const slug = $derived(slugify(name));
  const instructionCount = $derived(charCount(instructions));
  const ceiling = $derived(preflight?.ceiling ?? 1);
  const problem = $derived(
    validateSpec({ name, goal, instructions, folders, cap: Number(cap), cap_confirmed: capConfirmed }, ceiling),
  );
  const blocked = $derived(
    !preflight ? 'Checking the workspace…'
      : !preflight.eligible ? (preflight.reason ?? "This workspace can't become a Hive.")
      : !preflight.platform_ok ? "A Hive needs Claude's sandbox, which native Windows doesn't have."
      : !preflight.claude_ok ? (preflight.claude_detail ?? 'Claude Code 2.1.295 or newer is needed.')
      : null,
  );
  const canCreate = $derived(!creating && !blocked && !problem && !stackProblem);

  async function create() {
    if (!canCreate) return;
    creating = true;
    createError = null;
    const spec: HiveSpec = {
      name: name.trim(),
      goal: goal.trim(),
      instructions,
      folders: folders.map((f) => ({
        name: f.name.trim(),
        path: f.path,
        kind: f.kind,
        base: f.kind === 'git' ? (f.base.trim() || null) : null,
        setup: f.kind === 'git' ? (f.setup.trim() || null) : null,
        land_check: f.kind === 'git' ? (f.land_check.trim() || null) : null,
        primary: f.primary,
        local_remotes: [],
      })),
      cap: Number(cap),
      cap_confirmed: capConfirmed,
      landing_mode: landing,
      models: {
        coordinator: { runtime: 'claude', model: coordModel, effort: coordEffort },
        worker: { runtime: 'claude', model: workerModel, effort: workerEffort },
      },
      tools: [...picked],
      budget_cents: Math.max(0, Math.round(Number(budgetDollars) * 100)),
      push_after_land: pushAfterLand,
      stack: $state.snapshot(stackRows) as Service[],
      keep_off_phone: keepOffPhone,
    };
    try {
      await createHive(workspaceId, spec);
      onclose();
    } catch (e) {
      createError = String(e);
      logError(`hive: create failed: ${e}`);
      creating = false;
    }
  }

  function close() {
    if (!creating) onclose();
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') { e.stopPropagation(); close(); }
  }

  const KIND_LABEL: Record<string, string> = { mcp: 'MCP server', cli: 'Command-line tool', skill: 'Skill', comfyui: 'Local generator' };
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) close(); }}>
  <div class="modal" role="dialog" aria-modal="true" aria-labelledby="hive-sheet-title" tabindex="-1" bind:this={modalEl} onkeydown={handleKeydown}>
    <div class="header">
      <h2 id="hive-sheet-title">Make “{ws?.name ?? ''}” a Hive</h2>
      <IconButton tooltip="Close" style="font-size: 1.538rem;padding:4px 8px;width:auto;height:auto" onclick={close}>&times;</IconButton>
    </div>

    <div class="content">
      <p class="lede">
        A coordinator agent works toward the goal, starting worker agents on the Hive's folders and
        landing what they finish. It reads code in its own copies, never in your checkouts.
      </p>

      {#if loadError}
        <p class="error">{loadError}</p>
      {:else if blocked && preflight}
        <p class="error">{blocked}</p>
      {/if}
      {#if preflight?.login_warning}
        <!-- A warning, never a block: a login Claude can't vouch for (a token) isn't reported. -->
        <p class="warn login" role="status">{preflight.login_warning}</p>
      {/if}

      <label class="field">
        <span class="label">Name</span>
        <input bind:this={nameEl} bind:value={name} placeholder="Session store rewrite" />
        <span class="hint">The workspace's name, and the workstream's on the task board.{#if landing === 'integration' && slug} Branch: <code>hive/{slug}</code>{/if}</span>
      </label>

      <div class="field">
        <span class="label">Folders</span>
        {#if folders.length === 0 && primaryPending}
          <p class="hint">Finding the folder you were working in…</p>
        {:else if folders.length === 0}
          <p class="hint">Add the Primary folder: where workers work unless a task says otherwise.</p>
        {/if}
        <div class="list">
          {#each folders as f, i (f.key)}
            <div class="row">
              <div class="row-top">
                <label class="radio">
                  <input type="radio" name="hive-primary" checked={f.primary} onchange={() => setPrimary(f.key)} />
                  Primary
                </label>
                <input class="fname" aria-label="Folder name" value={f.name} oninput={(e) => patch(f.key, { name: (e.currentTarget as HTMLInputElement).value })} />
                {#if f.detectedKind === 'git'}
                  <select aria-label="Folder kind" value={f.kind} onchange={(e) => patch(f.key, { kind: (e.currentTarget as HTMLSelectElement).value })}>
                    <option value="git">git repo</option>
                    <option value="plain">plain, read-only</option>
                  </select>
                {:else}
                  <span class="kind">plain, read-only</span>
                {/if}
                <IconButton tooltip="Remove this folder" onclick={() => removeFolder(f.key)} aria-label="Remove {f.name}">&times;</IconButton>
              </div>
              <div class="mono path">{f.path}</div>
              {#if f.repoTop}
                <div class="note-line">
                  Inside the repo at <span class="mono">{f.repoTop}</span>. As a git folder it has to be the whole repo.
                  <button class="link" onclick={() => addFolder(f.repoTop!, i)}>Use the whole repo</button>
                </div>
              {/if}
              {#if f.kind === 'git'}
                <div class="grid">
                  <label><span>Base</span><input value={f.base} placeholder={f.detectedBase ?? 'branch'} oninput={(e) => patch(f.key, { base: (e.currentTarget as HTMLInputElement).value })} /></label>
                  <label><span>Setup</span><input value={f.setup} placeholder="npm ci" oninput={(e) => patch(f.key, { setup: (e.currentTarget as HTMLInputElement).value })} /></label>
                  <label><span>Land check</span><input value={f.land_check} placeholder="npm run check" oninput={(e) => patch(f.key, { land_check: (e.currentTarget as HTMLInputElement).value })} /></label>
                </div>
                {#if !f.trusted}
                  <div class="warn">
                    <Tooltip text="Claude asks whether to trust a folder the first time it runs there, and nothing in maiTerm may answer that for you. Trusting the repo also covers every copy the Hive makes of it.">
                      <span>Claude hasn't been told to trust this repo.</span>
                    </Tooltip>
                    Run <code>claude</code> in it once and accept the question.
                  </div>
                {/if}
              {/if}
            </div>
          {/each}
        </div>
        <div class="actions-row">
          <Button variant="secondary" onclick={pickFolder}>Add folder…</Button>
          {#if folderNote}<span class="warn">{folderNote}</span>{/if}
        </div>
      </div>

      <label class="field">
        <span class="label">Goal</span>
        <input bind:value={goal} placeholder="One line: what done looks like" />
        <span class="hint">Shown on the Hive's header, and the first thing the coordinator is told.</span>
      </label>

      <label class="field">
        <span class="label">Instructions</span>
        <textarea rows="5" bind:value={instructions} placeholder="Standing rules every agent in the Hive starts from"></textarea>
        <span class="hint count" class:over={instructionCount > MAX_INSTRUCTIONS}>{instructionCount.toLocaleString()} / 16,000 characters</span>
      </label>

      <div class="field">
        <span class="label">Workers at once</span>
        <div class="inline">
          <input class="num" type="number" min="1" bind:value={cap} onchange={() => (capConfirmed = false)} />
          {#if preflight}
            <Tooltip text="About one worker per 16 GB of memory above 32 GB, and no more than a quarter of the CPU cores. Each worker is a Claude session plus its tools.">
              <span class="hint">This computer's ceiling: {ceiling}</span>
            </Tooltip>
          {:else if !loadError}
            <span class="hint">Measuring this computer…</span>
          {/if}
        </div>
        {#if preflight && Number(cap) > ceiling}
          <label class="check warn">
            <input type="checkbox" bind:checked={capConfirmed} />
            Use {cap} anyway: more than this computer is sized for
          </label>
        {/if}
      </div>

      <fieldset class="field">
        <legend class="label">Landing <span class="fixed">fixed once created</span></legend>
        <label class="check"><input type="radio" name="hive-landing" value="base" bind:group={landing} /> Land to Base: the coordinator merges each finished worker into its repos' Base</label>
        <label class="check"><input type="radio" name="hive-landing" value="integration" bind:group={landing} /> Integration branch, I approve each land</label>
      </fieldset>

      <div class="field">
        <span class="label">Models</span>
        <div class="models">
          <span>Coordinator</span>
          <select aria-label="Coordinator model" bind:value={coordModel}>{#each MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}</select>
          <select aria-label="Coordinator effort" bind:value={coordEffort}>{#each EFFORTS as e (e)}<option value={e}>{e} effort</option>{/each}</select>
          <span>Workers</span>
          <select aria-label="Worker model" bind:value={workerModel}>{#each MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}</select>
          <select aria-label="Worker effort" bind:value={workerEffort}>{#each EFFORTS as e (e)}<option value={e}>{e} effort</option>{/each}</select>
        </div>
      </div>

      <div class="field">
        <span class="label">Tools</span>
        {#if tools === null}
          <p class="hint">Looking for tools…</p>
        {:else if tools.length === 0}
          <p class="hint">Nothing detected: no MCP servers, ffmpeg, ImageMagick, SoX, Blender, skills or ComfyUI.</p>
        {:else}
          <div class="tools">
            {#each tools as t (t.id)}
              <label class="check">
                <input type="checkbox" checked={picked.has(t.id)} onchange={() => { const n = new Set(picked); if (n.has(t.id)) n.delete(t.id); else n.add(t.id); picked = n; }} />
                <span>{t.label}</span>
                <span class="hint">{KIND_LABEL[t.kind] ?? t.kind}{#if t.host} · {t.host}{/if}</span>
              </label>
            {/each}
          </div>
          <span class="hint">Tells the coordinator what it can really make things with.</span>
        {/if}
      </div>

      <div class="field">
        <span class="label">Budget</span>
        <div class="inline">
          <span>$</span><input class="num" type="number" min="0" step="1" bind:value={budgetDollars} />
          <span class="hint">for paid generation. A rule agents follow and a total you watch, not a wall: spends already under way can carry the total past it, and the only hard limit is your provider account's own credit.</span>
        </div>
      </div>

      <label class="check field">
        <input type="checkbox" bind:checked={pushAfterLand} />
        Push after land <span class="hint">each land to Base also pushes that repo's Base</span>
      </label>

      <label class="check field">
        <input type="checkbox" bind:checked={keepOffPhone} />
        Keep this Hive's agents off the phone <span class="hint">no agent of this Hive, the coordinator included, is a chat in maiLink</span>
      </label>

      <div class="field">
        <span class="label">Stack</span>
        <span class="hint">What the Hive's work runs. Something you already run in another workspace is used from there, never started twice; a service that takes <code>{'{port}'}</code> in its command runs a copy per branch.</span>
        {#if folders.length === 0}
          <p class="hint">Add a folder to see its services.</p>
        {/if}
        {#each runningElsewhere as s (s.id)}
          {@const on = shareOn(s)}
          {@const code = !sharesByDefault(s.service, offers)}
          <label class="check stack-row">
            <input type="checkbox" checked={on}
              onchange={() => (shareChoice = { ...shareChoice, [s.id]: !on })} />
            <span class="mono">{s.name}</span>
            {#if code && !on}
              <span class="hint">running in {s.workspace}, but it's this repo's code: the Hive runs it per branch — add <code>{'{port}'}</code> to its suggestion below</span>
            {:else if code}
              <span class="warn">use the one running in {s.workspace}: no branch can run it</span>
            {:else}
              <span class="hint">use the one running in {s.workspace}</span>
            {/if}
          </label>
        {/each}
        {#each folders as f (f.key)}
          {@const list = suggestions[f.path]}
          {#if list === undefined || list === null}
            <div class="stack-row"><span class="mono">{f.name}</span><span class="hint">reading…</span></div>
          {:else if list.length === 0}
            <div class="stack-row"><span class="mono">{f.name}</span><span class="hint">no services suggested</span></div>
          {:else}
            {#each list as s (s.name)}
              {@const p = pickOf(f.path, s)}
              {@const v = importVerdict({ command: p.command, cwd: s.cwd }, clashes)}
              {@const cover = coveredBy(s, p.command)}
              <div class="stack-pick">
                <label class="check">
                  <input type="checkbox" checked={p.on && !cover && v.kind !== 'refused'} disabled={!!cover || v.kind === 'refused'}
                    onchange={() => setPick(f.path, s, { on: !p.on })} />
                  <span class="mono">{s.name}</span>
                  <span class="hint">{f.name} · {s.source}</span>
                </label>
                <input class="mono cmd" aria-label="Command for {s.name}" value={p.command}
                  oninput={(e) => setPick(f.path, s, { command: (e.currentTarget as HTMLInputElement).value })} />
                {#if cover}
                  <span class="hint">the {cover.name} running in {cover.workspace} is used instead; untick it above to import this</span>
                {:else}
                  <span class={v.kind === 'copies' ? 'hint' : v.kind === 'fixed' ? 'warn' : 'error'}>{v.detail}</span>
                {/if}
              </div>
            {/each}
          {/if}
        {/each}
        {#if stackProblem}<span class="error">{stackProblem}</span>{/if}
      </div>
    </div>

    <div class="footer">
      {#if createError}
        <span class="error foot">{createError}</span>
      {:else if problem && !blocked}
        <span class="hint foot">{problem}</span>
      {/if}
      <Button variant="secondary" onclick={close} disabled={creating}>Cancel</Button>
      <Button variant="primary" onclick={create} disabled={!canCreate}>{creating ? 'Creating…' : 'Create Hive'}</Button>
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
    width: 660px;
    max-width: calc(100vw - 32px);
    max-height: 88vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    outline: none;
  }
  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--bg-light);
  }
  .header h2 { font-size: 1.154rem; font-weight: 600; color: var(--fg); margin: 0; overflow-wrap: anywhere; }
  .content { padding: 14px 20px; overflow-y: auto; flex: 1; min-height: 0; display: flex; flex-direction: column; gap: 14px; }
  .lede { font-size: 0.923rem; color: var(--fg-dim); margin: 0; line-height: 1.45; }
  .field { display: flex; flex-direction: column; gap: 5px; margin: 0; padding: 0; border: 0; min-width: 0; }
  .label { font-size: 0.846rem; font-weight: 600; color: var(--fg-dim); text-transform: uppercase; letter-spacing: 0.5px; padding: 0; }
  .fixed { text-transform: none; letter-spacing: 0; font-weight: 400; margin-left: 6px; color: var(--fg-dim); }
  input:not([type='checkbox']):not([type='radio']), textarea, select {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    color: var(--fg);
    font: inherit;
    font-size: 0.923rem;
    padding: 6px 8px;
    min-width: 0;
  }
  input:focus, textarea:focus, select:focus { outline: none; border-color: var(--accent); }
  textarea { resize: vertical; line-height: 1.45; }
  input[type='checkbox'], input[type='radio'] { accent-color: var(--accent); }
  .hint { font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .count { align-self: flex-end; font-variant-numeric: tabular-nums; }
  .count.over { color: var(--red, #f7768e); }
  .list { display: flex; flex-direction: column; gap: 6px; }
  .row { border: 1px solid var(--bg-light); border-radius: 6px; padding: 8px 10px; display: flex; flex-direction: column; gap: 6px; }
  .row-top { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .fname { flex: 1; min-width: 120px; }
  .kind { font-size: 0.846rem; color: var(--fg-dim); }
  .path { color: var(--fg-dim); }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 6px; }
  .grid label { display: flex; flex-direction: column; gap: 3px; font-size: 0.846rem; color: var(--fg-dim); }
  .radio, .check { display: flex; align-items: center; gap: 6px; font-size: 0.923rem; color: var(--fg); cursor: pointer; flex-wrap: wrap; }
  .actions-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .inline { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .num { width: 80px; }
  .models { display: grid; grid-template-columns: auto 1fr 1fr; gap: 6px 8px; align-items: center; font-size: 0.923rem; color: var(--fg); }
  .tools { display: flex; flex-direction: column; gap: 4px; }
  .stack-row { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
  .stack-pick { display: flex; flex-direction: column; gap: 4px; padding: 4px 0; }
  .cmd { width: 100%; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.846rem; color: var(--fg); overflow-wrap: anywhere; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; background: var(--bg-dark); padding: 1px 4px; border-radius: 3px; }
  .note-line { font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .warn { font-size: 0.846rem; color: var(--yellow, #e0af68); overflow-wrap: anywhere; }
  .error { color: var(--red, #f7768e); font-size: 0.923rem; overflow-wrap: anywhere; margin: 0; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.846rem; }
  .footer { display: flex; justify-content: flex-end; align-items: center; gap: 8px; padding: 12px 20px; border-top: 1px solid var(--bg-light); flex-wrap: wrap; }
  .foot { margin-right: auto; flex: 1 1 200px; }
</style>
