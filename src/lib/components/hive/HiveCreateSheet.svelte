<!--
  Make this a Hive… (docs/hive.md §3): a guided wizard, one question a step (Goal, Folders, Crew,
  Tools, Stack, Review). The step rail on the left is a running receipt: each step it has passed
  shows what was chosen there, so the human never loses the whole picture to a scroll. Every step
  has defaults but Goal, so Next is all most steps need. Checked as the human types
  (lib/hive/model.ts, a part per step) and again by Rust on Create. Folders are detected as they're
  added (git or plain, Base, trust); an untrusted repo is a warning, never a refusal, with the
  human's own Trust it beside it (§6.6), read from the root the agents launch under.
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
  import { hiveDetectFolder, hiveDetectTools, hivePreflight, hiveTrustRepo, suggestStack, type StackSuggestion } from '$lib/tauri/commands';
  import type { HiveDetectedTool, HivePreflight, HiveSpec, Service } from '$lib/tauri/types';
  import { charCount, folderName, MAX_BRIEF, MAX_BRIEF_FILES, MAX_INSTRUCTIONS, slugify, validateAbout, validateCap, validateFolders, validateSpec } from '$lib/hive/model';
  import { MAX_CAP } from '$lib/hive/change';
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
  /** Until the preflight (and its default Primary's detection) answers: the
   *  folder list shows a loading line, never an empty list. */
  let primaryPending = $state(true);
  let loadError = $state<string | null>(null);
  let tools = $state<HiveDetectedTool[] | null>(null);

  let name = $state(workspacesStore.workspaces.find((w) => w.id === workspaceId)?.name ?? '');
  let folders = $state<Folder[]>([]);
  let folderKey = 0;
  let folderNote = $state<string | null>(null);
  let goal = $state('');
  let instructions = $state('');
  // The project brief and its files: the queen's alone (docs/hive.md §3.5).
  let brief = $state('');
  let briefFiles = $state<string[]>([]);
  // Rules for every agent stay folded away until wanted: most Hives need none.
  let rulesOpen = $state(false);
  let cap = $state(4);
  let landing = $state<'base' | 'integration'>('base');
  let coordModel = $state('opus');
  let coordEffort = $state('medium');
  let workerModel = $state('sonnet');
  let workerEffort = $state('high');
  let picked = $state(new Set<string>());
  let budgetDollars = $state(10);
  let pushAfterLand = $state(false);
  /** The budget as Create sends it: an emptied or negative field is $0, and the receipt and the
   *  review say what is sent. */
  const budget = $derived(Math.max(0, Number(budgetDollars) || 0));
  /** Rust pushes only lands to Base: an integration branch is never pushed. */
  const pushes = $derived(pushAfterLand && landing === 'base');
  /** Who is a chat on the phone. "none" makes every Hive tab off it: Create launches the
   *  coordinator at once, so there's no later moment to exclude it before its first turn. */
  let phone = $state<'coordinator' | 'all' | 'none'>('coordinator');
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let suggestions = $state<Record<string, StackSuggestion[] | null>>({});

  let modalEl = $state<HTMLDivElement | undefined>();
  let bodyEl = $state<HTMLDivElement | undefined>();

  // ── The wizard's steps ──
  const STEPS = [
    { key: 'goal', title: 'Goal', ask: 'What should this Hive get done?' },
    { key: 'folders', title: 'Folders', ask: 'Where does the work happen?' },
    { key: 'crew', title: 'Crew', ask: 'Who does the work, and how does it land?' },
    { key: 'tools', title: 'Tools', ask: 'What can it make things with?' },
    { key: 'stack', title: 'Stack', ask: 'What does the work run?' },
    { key: 'review', title: 'Review', ask: 'Ready to start' },
  ] as const;
  type StepKey = (typeof STEPS)[number]['key'];
  let step = $state(0);
  /** The furthest step reached: the rail shows a receipt for each step before it. */
  let reached = $state(0);
  const current = $derived(STEPS[step]);
  const isLast = $derived(step === STEPS.length - 1);

  /** What stops leaving a step, in its own words. */
  function stepProblem(k: StepKey): string | null {
    if (k === 'goal') return validateAbout({ name, goal, instructions, brief, brief_files: briefFiles });
    if (k === 'folders') return primaryPending && !folders.length ? 'Finding your folder…' : validateFolders({ folders });
    if (k === 'crew') return validateCap({ cap: Number(cap) });
    if (k === 'stack') return stackProblem;
    return null;
  }
  const here = $derived(stepProblem(current.key));
  /** A step can be opened from the rail when every step before it is complete. */
  function canOpen(i: number): boolean {
    return STEPS.slice(0, i).every((s) => !stepProblem(s.key));
  }
  function go(i: number) {
    if (i < 0 || i >= STEPS.length || !canOpen(i)) return;
    step = i;
    reached = Math.max(reached, i);
  }
  function next() {
    if (isLast) void create();
    else if (!here) go(step + 1);
  }
  // The Goal step opens on its goal field, every other step on its question (a folder's name or
  // the budget is no place to land the caret). Svelte's autofocus is not focus (CLAUDE.md):
  // explicitly, which keeps typing and Escape working.
  $effect(() => {
    const k = current.key;
    requestAnimationFrame(() => {
      const el = k === 'goal' ? bodyEl?.querySelector<HTMLElement>('input') : bodyEl?.querySelector<HTMLElement>('h3');
      (el ?? modalEl)?.focus();
    });
  });
  /** Enter in a one-line field goes on (Cmd/Ctrl+Enter from anywhere, a textarea included). */
  function onBodyKey(e: KeyboardEvent) {
    if (e.key !== 'Enter' || e.isComposing) return;
    const t = e.target as HTMLElement;
    const lineField = t instanceof HTMLInputElement && !['checkbox', 'radio'].includes(t.type);
    if (!(e.metaKey || e.ctrlKey) && !lineField) return;
    e.preventDefault();
    next();
  }

  // ── The rail's receipts and the review, in words ──
  const cap1 = (w: string) => w.charAt(0).toUpperCase() + w.slice(1);
  const folderList = $derived(
    [...folders].sort((a, b) => Number(b.primary) - Number(a.primary)).map((f) => f.name.trim() || f.path),
  );
  const pickedTools = $derived((tools ?? []).filter((t) => picked.has(t.id)).map((t) => t.label));
  const PHONE_TEXT = { coordinator: 'The queen only', all: 'The queen and its workers', none: 'Nobody' } as const;
  function receipt(k: StepKey): string | null {
    switch (k) {
      case 'goal': return goal.trim() || null;
      case 'folders': return folderList.length ? folderList.join(', ') : null;
      case 'crew': return validateCap({ cap: Number(cap) }) ? null : `Up to ${cap} ${cap1(workerModel)} workers`;
      case 'tools': return `${pickedTools.length ? `${pickedTools.length} tool${pickedTools.length === 1 ? '' : 's'}` : 'No tools'}, $${budget} budget`;
      case 'stack': return stackRows.length ? `${stackRows.length} service${stackRows.length === 1 ? '' : 's'}` : 'Nothing to run';
      default: return null;
    }
  }

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
      cap = p.default_cap;
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
        // A fresh worktree has no dependencies, and a worker can't install them in its own
        // shell: the lockfile's install, shown and editable.
        setup: info.setup ?? '',
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

  /** Trust it (§6.6): the human's click is the human trusting their repo, written by Rust into
   *  the root the Hive's agents launch under. The other remedy, `claude` in this workspace's own
   *  shell, would use up the fresh shell the Hive replaces. */
  let trusting = $state<number | null>(null);
  let trustError = $state<{ key: number; text: string } | null>(null);
  async function trustRepo(f: Folder) {
    if (trusting !== null) return;
    trusting = f.key;
    trustError = null;
    try {
      const info = await hiveTrustRepo(f.path);
      patch(f.key, { trusted: info.trusted });
      if (!info.trusted) trustError = { key: f.key, text: 'Written, but Claude still reads it as untrusted.' };
    } catch (e) {
      trustError = { key: f.key, text: String(e).replace(/^Error: /, '') };
      logError(`hive: trusting ${f.path} failed: ${e}`);
    } finally {
      trusting = null;
    }
  }

  const fileName = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;

  async function attachBriefFiles() {
    const picked = await dialogOpen({ directory: false, multiple: true, title: 'Attach files to the project brief' }).catch(() => null);
    const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).filter((p): p is string => typeof p === 'string' && !!p);
    briefFiles = [...briefFiles, ...paths.filter((p) => !briefFiles.includes(p))];
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
  const briefCount = $derived(charCount(brief));
  const problem = $derived(validateSpec({ name, goal, instructions, brief, brief_files: briefFiles, folders, cap: Number(cap) }));
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
      brief,
      brief_files: briefFiles,
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
      landing_mode: landing,
      models: {
        coordinator: { runtime: 'claude', model: coordModel, effort: coordEffort },
        worker: { runtime: 'claude', model: workerModel, effort: workerEffort },
      },
      tools: [...picked],
      budget_cents: Math.round(budget * 100),
      push_after_land: pushes,
      stack: $state.snapshot(stackRows) as Service[],
      keep_off_phone: phone === 'none',
      workers_on_phone: phone === 'all',
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

    <div class="wizard">
      <nav class="rail" aria-label="Steps">
        <ol>
          {#each STEPS as s, i (s.key)}
            {@const done = i <= reached && i !== step && !stepProblem(s.key)}
            {@const r = i <= reached && i !== step ? receipt(s.key) : null}
            <li>
              <button
                type="button"
                class="stp"
                class:current={i === step}
                class:done
                disabled={!canOpen(i)}
                aria-current={i === step ? 'step' : undefined}
                onclick={() => go(i)}
              >
                <span class="dot" aria-hidden="true">{#if done}<svg viewBox="0 0 12 12" width="10" height="10"><path d="M2.5 6.2l2.3 2.3 4.7-4.9" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" /></svg>{:else}{i + 1}{/if}</span>
                <span class="stp-text">
                  <span class="stp-title">{s.title}</span>
                  {#if r}<span class="stp-receipt">{r}</span>{/if}
                </span>
              </button>
            </li>
          {/each}
        </ol>
      </nav>

      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="body" bind:this={bodyEl} onkeydown={onBodyKey}>
        <h3 tabindex="-1">{current.ask}</h3>

        {#if loadError}
          <p class="error">{loadError}</p>
        {:else if blocked && preflight}
          <p class="error">{blocked}</p>
        {/if}

        {#if current.key === 'goal'}
          <p class="lede">
            A queen agent works toward the goal, starting worker agents on the Hive's folders and
            landing what they finish. It reads code in its own copies, never in your checkouts.
          </p>
          {#if preflight?.login_warning}
            <!-- A warning, never a block: a login Claude can't vouch for (a token) isn't reported. -->
            <p class="warn login" role="status">{preflight.login_warning}</p>
          {/if}
          <label class="field">
            <span class="label">Goal</span>
            <input bind:value={goal} placeholder="One line: what done looks like" />
            <span class="hint">Shown on the Hive's header. The queen won't start workers on this line alone.</span>
          </label>
          <label class="field">
            <span class="label">Name</span>
            <input bind:value={name} placeholder="Session store rewrite" />
            <span class="hint">The workspace's name, and the workstream's on the task board.{#if landing === 'integration' && slug} Branch: <code>hive/{slug}</code>{/if}</span>
          </label>
          <label class="field">
            <span class="label">Project brief <span class="opt">optional</span></span>
            <textarea rows="7" bind:value={brief} placeholder="What the queen needs to start: the plan, who it's for, constraints, links. Or where to get them: “Bridge to the API tab for the specs.”"></textarea>
            <span class="hint">Only the queen reads this; workers get what she tells them.</span>
            <span class="hint count" class:over={briefCount > MAX_BRIEF}>{briefCount.toLocaleString()} / 32,000 characters</span>
          </label>
          <div class="brief-files">
            {#each briefFiles as f (f)}
              <div class="brief-file">
                <code>{fileName(f)}</code>
                <IconButton tooltip="Remove {fileName(f)}" onclick={() => (briefFiles = briefFiles.filter((x) => x !== f))} aria-label="Remove {fileName(f)}">&times;</IconButton>
              </div>
            {/each}
            {#if briefFiles.length < MAX_BRIEF_FILES}
              <Button variant="secondary" onclick={attachBriefFiles}>Attach files…</Button>
            {/if}
            <span class="hint">A plan, a spec, notes. Copied for the queen when the Hive is made.</span>
          </div>
          {#if rulesOpen || instructions.trim()}
            <label class="field">
              <span class="label">Rules for every agent <span class="opt">optional</span></span>
              <textarea rows="4" bind:value={instructions} placeholder="Rules for all work, whatever the task: “use pnpm”, “don't touch legacy/”, “British English”"></textarea>
              <span class="hint">Every worker gets these too, so keep them short.</span>
              <span class="hint count" class:over={instructionCount > MAX_INSTRUCTIONS}>{instructionCount.toLocaleString()} / 16,000 characters</span>
            </label>
          {:else}
            <button type="button" class="reveal" onclick={() => (rulesOpen = true)}>Add rules for every agent</button>
          {/if}

        {:else if current.key === 'folders'}
          <p class="lede">Workers get their own copies of each git folder, and land their work back into it. A plain folder is shared read-only.</p>
          {#if folders.length === 0 && primaryPending}
            <p class="hint">Finding the folder you were working in…</p>
          {:else if folders.length === 0}
            <p class="hint">Add the Primary folder: where workers work unless a task says otherwise.</p>
          {/if}
          <div class="list">
            {#each folders as f, i (f.key)}
              <div class="row" class:primary={f.primary}>
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
                      <Tooltip text="Claude asks whether to trust a folder the first time it runs there, and nothing in maiTerm may answer that for you. A trusted repo's project settings, hooks and MCP servers run without asking. Trusting the repo also covers every copy the Hive makes of it.">
                        <span>Claude hasn't been told to trust this repo{preflight?.trust_account
                          ? ` under "${preflight.trust_account}", the account this Hive's agents run as`
                          : preflight?.trust_root_dir
                            ? ` under the Claude config at ${preflight.trust_root_dir} (your CLAUDE_CONFIG_DIR), the one this Hive's agents run as`
                            : ''}.</span>
                      </Tooltip>
                      <Tooltip text="Records your trust of this repo where those agents read it, as Claude's own question would">
                        <button class="link" disabled={trusting === f.key} onclick={() => void trustRepo(f)}>{trusting === f.key ? 'Trusting…' : 'Trust it'}</button>
                      </Tooltip>
                      or run <code>claude</code> once in it in another tab under that account (not this workspace's shell: the Hive replaces it).
                      {#if trustError?.key === f.key}<span class="err">{trustError.text}</span>{/if}
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

        {:else if current.key === 'crew'}
          <div class="field">
            <span class="label">Workers at once</span>
            <div class="inline">
              <input class="num" type="number" min="1" max={MAX_CAP} bind:value={cap} />
              <Tooltip text="The queen starts only as many as the plan needs. If memory runs short, maiTerm holds new workers until it eases, and builds and tests take turns in the run queue.">
                <span class="hint">At most this many; the queen decides how many it needs</span>
              </Tooltip>
            </div>
          </div>
          <div class="field">
            <span class="label">Models</span>
            <div class="models">
              <span>Queen</span>
              <select aria-label="Queen model" bind:value={coordModel}>{#each MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}</select>
              <select aria-label="Queen effort" bind:value={coordEffort}>{#each EFFORTS as e (e)}<option value={e}>{e} effort</option>{/each}</select>
              <span>Workers</span>
              <select aria-label="Worker model" bind:value={workerModel}>{#each MODELS as m (m)}<option value={m}>Claude {m}</option>{/each}</select>
              <select aria-label="Worker effort" bind:value={workerEffort}>{#each EFFORTS as e (e)}<option value={e}>{e} effort</option>{/each}</select>
            </div>
            <span class="hint">Both can be changed later in the Hive's settings.</span>
          </div>
          <fieldset class="field">
            <legend class="label">How work lands <span class="opt">can't be changed later</span></legend>
            <label class="choice"><input type="radio" name="hive-landing" value="base" bind:group={landing} /><span><b>Straight into Base</b><span class="hint">The queen merges each finished worker into its repo's Base branch.</span></span></label>
            <label class="choice"><input type="radio" name="hive-landing" value="integration" bind:group={landing} /><span><b>Into an integration branch, each land approved by you</b><span class="hint">Base stays untouched until you merge the branch yourself.</span></span></label>
          </fieldset>
          {#if landing === 'base'}
            <label class="choice field">
              <input type="checkbox" bind:checked={pushAfterLand} />
              <span><b>Push after each land</b><span class="hint">Each land to Base also pushes that repo's Base.</span></span>
            </label>
          {/if}
          <fieldset class="field">
            <legend class="label">Chats on your phone</legend>
            <label class="choice"><input type="radio" name="hive-phone" value="coordinator" bind:group={phone} /><span><b>The queen only</b><span class="hint">It speaks for its workers.</span></span></label>
            <label class="choice"><input type="radio" name="hive-phone" value="all" bind:group={phone} /><span><b>The queen and its workers</b></span></label>
            <label class="choice"><input type="radio" name="hive-phone" value="none" bind:group={phone} /><span><b>Nobody</b><span class="hint">No agent of this Hive is a chat in maiLink. Can't be changed later.</span></span></label>
          </fieldset>

        {:else if current.key === 'tools'}
          <div class="field">
            <span class="label">Tools</span>
            {#if tools === null}
              <p class="hint">Looking for tools…</p>
            {:else if tools.length === 0}
              <p class="hint">Nothing detected: no MCP servers, ffmpeg, ImageMagick, SoX, Blender, skills or ComfyUI.</p>
            {:else}
              <span class="hint">Tells the queen what it can really make things with.</span>
              <div class="tools">
                {#each tools as t (t.id)}
                  <label class="check">
                    <input type="checkbox" checked={picked.has(t.id)} onchange={() => { const n = new Set(picked); if (n.has(t.id)) n.delete(t.id); else n.add(t.id); picked = n; }} />
                    <span>{t.label}</span>
                    <span class="hint">{KIND_LABEL[t.kind] ?? t.kind}{#if t.host} · {t.host}{/if}</span>
                  </label>
                {/each}
              </div>
            {/if}
          </div>
          <div class="field">
            <span class="label">Budget for paid generation</span>
            <div class="inline">
              <span>$</span><input class="num" type="number" min="0" step="1" bind:value={budgetDollars} />
            </div>
            <span class="hint">A rule agents follow and a total you watch, not a wall: spends already under way can carry the total past it, and the only hard limit is your provider account's own credit.</span>
          </div>

        {:else if current.key === 'stack'}
          <p class="lede">Something you already run in another workspace is used from there, never started twice. A service that takes <code>{'{port}'}</code> in its command runs a copy per branch.</p>
          {#if folders.length === 0}
            <p class="hint">Add a folder to see its services.</p>
          {:else if !runningElsewhere.length && folders.every((f) => suggestions[f.path]?.length === 0)}
            <p class="hint">Nothing to run was found in these folders. You can skip this step.</p>
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
            {:else if list.length}
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

        {:else}
          <div class="review">
            <p>
              A queen on <b>Claude {coordModel}</b> ({coordEffort} effort) works toward
              “{goal.trim()}”. It starts up to <b>{cap}</b> workers on <b>Claude {workerModel}</b> ({workerEffort} effort)
              in <b>{folderList.join(', ')}</b>, and lands each finished worker
              {landing === 'base' ? "straight into its repo's Base" : `into hive/${slug}, each land approved by you`}{pushes ? ', then pushes it' : ''}.
            </p>
            <p>
              {pickedTools.length ? `It can make things with ${pickedTools.join(', ')}` : 'No extra tools'}, with
              a ${budget} budget for paid generation.
              {stackRows.length ? `Its stack: ${stackRows.map((r) => r.name).join(', ')}.` : 'It has no stack services.'}
              On your phone: {PHONE_TEXT[phone].toLowerCase()}.
            </p>
            <p>
              {#if brief.trim() || briefFiles.length}
                It starts from your project brief{briefFiles.length ? ` and ${briefFiles.length} attached file${briefFiles.length === 1 ? '' : 's'}` : ''},
              {:else}
                With no project brief it asks you what it needs,
              {/if}
              and you confirm its completion criteria before any worker starts.
              {#if instructions.trim()}Every agent also follows your {instructionCount.toLocaleString()} characters of rules.{/if}
            </p>
            <p class="hint">Creating it replaces this workspace's shell with the queen, which starts at once.</p>
          </div>
        {/if}
      </div>
    </div>

    <div class="footer">
      {#if createError}
        <span class="error foot">{createError}</span>
      {:else if here && !blocked}
        <span class="hint foot">{here}</span>
      {:else if isLast && (problem ?? stackProblem) && !blocked}
        <span class="hint foot">{problem ?? stackProblem}</span>
      {:else}
        <span class="foot"></span>
      {/if}
      <Button variant="secondary" onclick={close} disabled={creating}>Cancel</Button>
      {#if step > 0}<Button variant="secondary" onclick={() => go(step - 1)} disabled={creating}>Back</Button>{/if}
      {#if isLast}
        <Button variant="primary" onclick={create} disabled={!canCreate}>{creating ? 'Creating…' : 'Create Hive'}</Button>
      {:else}
        <Button variant="primary" onclick={next} disabled={!!here}>Next: {STEPS[step + 1].title}</Button>
      {/if}
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
    width: 780px;
    max-width: calc(100vw - 32px);
    height: min(640px, 88vh);
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

  /* The wizard: the step rail beside the step's body. */
  .wizard { flex: 1; min-height: 0; display: flex; }
  .rail { flex: 0 0 196px; border-right: 1px solid var(--bg-light); padding: 14px 10px; overflow-y: auto; background: color-mix(in srgb, var(--bg-dark) 35%, transparent); }
  .rail ol { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; position: relative; }
  /* The thread the steps hang on. */
  .rail ol::before { content: ''; position: absolute; left: 17px; top: 14px; bottom: 14px; width: 1px; background: var(--bg-light); }
  .stp {
    position: relative;
    display: flex;
    align-items: flex-start;
    gap: 10px;
    width: 100%;
    text-align: left;
    background: none;
    border: none;
    border-radius: 6px;
    padding: 7px 6px;
    color: var(--fg-dim);
    font: inherit;
    cursor: pointer;
  }
  .stp:hover:not(:disabled) { background: var(--bg-medium); }
  .stp:disabled { cursor: default; opacity: 0.55; }
  .stp:focus-visible { outline: 2px solid var(--accent); outline-offset: -2px; }
  .dot {
    flex: 0 0 auto;
    width: 22px;
    height: 22px;
    border-radius: 50%;
    display: grid;
    place-items: center;
    font-size: 0.769rem;
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    color: var(--fg-dim);
    transition: background 0.15s, border-color 0.15s, color 0.15s;
  }
  .stp.done .dot { background: color-mix(in srgb, var(--green, #9ece6a) 18%, var(--bg-medium)); border-color: color-mix(in srgb, var(--green, #9ece6a) 55%, transparent); color: var(--green, #9ece6a); }
  .stp.current .dot { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); }
  .stp-text { display: flex; flex-direction: column; gap: 2px; min-width: 0; padding-top: 2px; }
  .stp-title { font-size: 0.923rem; color: inherit; }
  .stp.current .stp-title { color: var(--fg); font-weight: 600; }
  .stp.done .stp-title { color: var(--fg); }
  /* The receipt: what was chosen at that step, two lines at most. */
  .stp-receipt {
    font-size: 0.808rem;
    color: var(--fg-dim);
    line-height: 1.35;
    overflow-wrap: anywhere;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
    overflow: hidden;
  }

  .body { flex: 1; min-width: 0; padding: 18px 22px; overflow-y: auto; display: flex; flex-direction: column; gap: 16px; }
  .body h3 { margin: 0; font-size: 1.308rem; font-weight: 600; color: var(--fg); letter-spacing: -0.01em; outline: none; }
  .lede { font-size: 0.923rem; color: var(--fg-dim); margin: -6px 0 0; line-height: 1.5; max-width: 62ch; }
  .field { display: flex; flex-direction: column; gap: 5px; margin: 0; padding: 0; border: 0; min-width: 0; }
  .label { font-size: 0.923rem; font-weight: 600; color: var(--fg); padding: 0; }
  .opt { font-weight: 400; color: var(--fg-dim); margin-left: 6px; font-size: 0.846rem; }
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
  .hint { font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; line-height: 1.45; }
  .count { align-self: flex-end; font-variant-numeric: tabular-nums; }
  .count.over { color: var(--red, #f7768e); }
  .brief-files { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 8px; }
  .brief-files .hint { flex-basis: 100%; }
  .brief-file { display: inline-flex; align-items: center; gap: 2px; min-width: 0; max-width: 100%; border: 1px solid var(--bg-light); border-radius: 6px; padding-left: 8px; }
  .brief-file code { font-size: 0.846rem; overflow-wrap: anywhere; }
  .reveal { align-self: flex-start; background: none; border: 0; padding: 2px 0; color: var(--accent); font: inherit; font-size: 0.923rem; cursor: pointer; }
  .reveal:hover { text-decoration: underline; }
  .reveal:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .list { display: flex; flex-direction: column; gap: 8px; }
  .row { border: 1px solid var(--bg-light); border-radius: 8px; padding: 10px 12px; display: flex; flex-direction: column; gap: 6px; }
  .row.primary { border-color: color-mix(in srgb, var(--accent) 50%, var(--bg-light)); }
  .row-top { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .fname { flex: 1; min-width: 120px; }
  .kind { font-size: 0.846rem; color: var(--fg-dim); }
  .path { color: var(--fg-dim); }
  .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 6px; }
  .grid label { display: flex; flex-direction: column; gap: 3px; font-size: 0.846rem; color: var(--fg-dim); }
  .radio, .check { display: flex; align-items: center; gap: 6px; font-size: 0.923rem; color: var(--fg); cursor: pointer; flex-wrap: wrap; }
  /* A choice with a line under it saying what it does. */
  .choice { display: flex; align-items: flex-start; gap: 8px; font-size: 0.923rem; color: var(--fg); cursor: pointer; padding: 3px 0; }
  .choice input { margin-top: 3px; }
  .choice > span { display: flex; flex-direction: column; gap: 1px; min-width: 0; }
  .choice b { font-weight: 500; }
  fieldset.field { gap: 4px; }
  legend.label { margin-bottom: 4px; }
  .actions-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .inline { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; }
  .num { width: 80px; }
  .models { display: grid; grid-template-columns: auto 1fr 1fr; gap: 6px 8px; align-items: center; font-size: 0.923rem; color: var(--fg); }
  .tools { display: flex; flex-direction: column; gap: 4px; }
  .stack-row { display: flex; gap: 8px; align-items: baseline; flex-wrap: wrap; }
  .stack-pick { display: flex; flex-direction: column; gap: 4px; padding: 4px 0; }
  .cmd { width: 100%; }
  .review { display: flex; flex-direction: column; gap: 12px; max-width: 62ch; }
  .review p { margin: 0; font-size: 1rem; line-height: 1.6; color: var(--fg); }
  .review p.hint { font-size: 0.846rem; color: var(--fg-dim); }
  .review b { font-weight: 600; }
  .mono { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.846rem; color: var(--fg); overflow-wrap: anywhere; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; background: var(--bg-dark); padding: 1px 4px; border-radius: 3px; }
  .note-line { font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .warn { font-size: 0.846rem; color: var(--yellow, #e0af68); overflow-wrap: anywhere; }
  .login { margin: 0; }
  .error { color: var(--red, #f7768e); font-size: 0.923rem; overflow-wrap: anywhere; margin: 0; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.846rem; }
  .link:disabled { color: var(--fg-dim); cursor: default; }
  .warn .err { display: block; color: var(--red, #f7768e); }
  .footer { display: flex; justify-content: flex-end; align-items: center; gap: 8px; padding: 12px 20px; border-top: 1px solid var(--bg-light); flex-wrap: wrap; }
  .foot { margin-right: auto; flex: 1 1 200px; }

  /* Narrow: the rail becomes a row of numbered dots above the step. */
  @media (max-width: 620px) {
    .wizard { flex-direction: column; }
    .rail { flex: 0 0 auto; border-right: 0; border-bottom: 1px solid var(--bg-light); padding: 8px 10px; }
    .rail ol { flex-direction: row; justify-content: space-between; }
    .rail ol::before { left: 20px; right: 20px; top: 50%; bottom: auto; width: auto; height: 1px; }
    .stp { width: auto; }
    .stp-text { display: none; }
  }
  @media (prefers-reduced-motion: reduce) {
    .dot { transition: none; }
  }
</style>
