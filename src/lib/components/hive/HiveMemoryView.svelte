<!--
  A Hive's Memory view (docs/hive.md §9), beside the coordinator in place of the board: every file
  in the shared memory folder with who wrote it (Rust's record, never the file's frontmatter), a
  reader and editor, the kept block, and the lines found in MEMORY.md that link no file.

  Every write goes through Rust's one writer, which the agents' rebuilds also go through, so an
  edit here never races one. The human's own edits are `human`. It refetches when its window hears
  `hive-memory-changed` (`emit_to`, never a broadcast) and never polls. Tab Files; Decisions and
  Precedents arrive with step 7d.
-->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { hiveMemoryDelete, hiveMemoryKept, hiveMemoryList, hiveMemoryPin, hiveMemoryRead, hiveMemoryUnfiled, hiveMemoryWrite } from '$lib/tauri/commands';
  import type { HiveMemoryFile, HiveMemoryView } from '$lib/tauri/types';
  import { countedListen } from '$lib/utils/listenCounter';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { budgetText, fileTags, keptError, keptLines, newFileName, provenance, TAG_TIP } from '$lib/hive/memory';

  interface Props {
    workspaceId: string;
  }
  let { workspaceId }: Props = $props();

  let mem = $state<HiveMemoryView | null>(null);
  let readError = $state<string | null>(null);
  /** The last refusal or failure of an action, shown where the human acted. */
  let actionError = $state<string | null>(null);
  let busy = $state(false);

  const clean = (e: unknown) => String(e).replace(/^Error: /, '');

  let seq = 0;
  function load() {
    const id = workspaceId;
    const mine = ++seq;
    hiveMemoryList(id)
      .then((v) => { if (mine === seq && id === workspaceId) { mem = v; readError = null; } })
      .catch((e) => { if (mine === seq) readError = clean(e); });
  }
  $effect(() => {
    void workspaceId;
    untrack(load);
  });

  /** Runs one command; its answer is the new view. */
  async function act(what: string, run: () => Promise<HiveMemoryView>): Promise<boolean> {
    if (busy) return false;
    busy = true;
    actionError = null;
    try {
      mem = await run();
      return true;
    } catch (e) {
      actionError = clean(e);
      logError(`hive: memory ${what} refused: ${e}`);
      return false;
    } finally {
      busy = false;
    }
  }

  // ── The open file ──
  /** `sha` null: a new file not written yet. */
  let open = $state<{ name: string; text: string; sha: string | null; draft: string } | null>(null);
  let newName = $state<string | null>(null);
  let confirmDelete = $state(false);
  const openFile = $derived(open ? mem?.files.find((f) => f.name === open!.name) ?? null : null);
  const dirty = $derived(!!open && open.draft !== open.text);
  /** Archives are maiTerm's; a file in a subfolder can be read and deleted, not written. */
  const writable = $derived(!!open && !openFile?.archive && !openFile?.in_subfolder);

  async function select(f: HiveMemoryFile) {
    if (busy) return;
    actionError = null;
    confirmDelete = false;
    newName = null;
    try {
      const t = await hiveMemoryRead(workspaceId, f.name);
      open = { name: f.name, text: t.text, sha: t.sha256, draft: t.text };
    } catch (e) {
      actionError = clean(e);
    }
  }

  function startNew() {
    open = null;
    confirmDelete = false;
    actionError = null;
    newName = '';
  }

  function createNew() {
    const name = newFileName(newName ?? '');
    if (!name) {
      actionError = 'A memory file needs one name: no slashes, not starting with a dot, not MEMORY or archive-…';
      return;
    }
    if (mem?.files.some((f) => f.name === name)) {
      actionError = `${name} already exists: pick it in the list to edit it`;
      return;
    }
    const stem = name.replace(/\.md$/, '');
    const text = `---\nname: ${stem}\ndescription: \nmetadata:\n  type: project\n---\n\n`;
    open = { name, text: '', sha: null, draft: text };
    newName = null;
  }

  async function save() {
    if (!open || !writable || !dirty) return;
    const { name, draft, sha } = open;
    if (await act('write', () => hiveMemoryWrite(workspaceId, name, draft, sha))) {
      try {
        const t = await hiveMemoryRead(workspaceId, name);
        open = { name, text: t.text, sha: t.sha256, draft: t.text };
      } catch {
        open = null;
      }
    }
  }

  async function reopen() {
    if (!open) return;
    const f = mem?.files.find((x) => x.name === open!.name);
    if (f) await select(f);
    else open = null;
  }

  async function del() {
    if (!open?.sha) return;
    const { name, sha } = open;
    if (await act('delete', () => hiveMemoryDelete(workspaceId, name, sha))) {
      open = null;
      confirmDelete = false;
    }
  }

  // ── The kept block ──
  let keptDraft = $state('');
  let keptTouched = $state(false);
  /** The block the draft started from: Save sends it, and Rust refuses if the block moved on. */
  let keptBase = $state<string[]>([]);
  const keptShown = $derived(mem?.kept.join('\n') ?? '');
  $effect(() => {
    const k = keptShown;
    if (!untrack(() => keptTouched)) {
      keptDraft = k;
      keptBase = untrack(() => mem?.kept ?? []);
    }
  });
  const keptErr = $derived(mem && keptTouched ? keptError(keptLines(keptDraft), mem.budget) : null);
  const keptMovedOn = $derived(keptTouched && keptBase.join('\n') !== keptShown);

  async function saveKept() {
    if (!keptTouched || keptErr) return;
    if (await act('kept block', () => hiveMemoryKept(workspaceId, 'set', keptLines(keptDraft), keptBase))) keptTouched = false;
  }

  function discardKept() {
    keptTouched = false;
    keptDraft = keptShown;
    keptBase = mem?.kept ?? [];
  }

  onMount(() => {
    let unlisten: (() => void) | undefined;
    let gone = false;
    countedListen<{ workspace_id: string }>('hive-memory-changed', (e) => {
      if (e.payload.workspace_id === workspaceId) load();
    }, { target: getCurrentWindow().label })
      .then((u) => { if (gone) u(); else unlisten = u; })
      .catch((e) => logError(`hive: memory-changed listener: ${e}`));
    return () => {
      gone = true;
      unlisten?.();
    };
  });

  const when = (ms: number) => (ms ? new Date(ms).toLocaleString() : '');
  const topFiles = $derived(mem?.files.filter((f) => !f.in_subfolder) ?? []);
  const subFiles = $derived(mem?.files.filter((f) => f.in_subfolder) ?? []);
</script>

<section class="memory" aria-label="Memory">
  <div class="tabs" role="tablist" aria-label="Memory">
    <button type="button" role="tab" class="tab active" aria-selected="true">Files</button>
  </div>

  {#if readError}
    <p class="error" role="alert">Couldn't read the memory folder: {readError}</p>
  {:else if !mem}
    <p class="note">Reading the memory folder…</p>
  {:else}
    {#if mem.status}
      <p class="warn" role="status">{mem.status}. Shorten the kept block below to bring MEMORY.md back.</p>
    {/if}
    {#if mem.kept_changed}
      <div class="warn banner" role="status">
        <span>The kept block was changed outside this view.</span>
        <Tooltip text="Take the block as it is now as yours">
          <button type="button" class="link" disabled={busy} onclick={() => act('accept', () => hiveMemoryKept(workspaceId, 'accept', null, mem?.kept ?? []))}>Keep as is</button>
        </Tooltip>
        <Tooltip text="Write back the kept block you last set here">
          <button type="button" class="link" disabled={busy} onclick={() => act('restore', () => hiveMemoryKept(workspaceId, 'restore', null, mem?.kept ?? []))}>Restore mine</button>
        </Tooltip>
      </div>
    {/if}
    {#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

    <div class="split">
      <div class="list">
        <div class="list-h">
          <b>Files</b>
          <button type="button" class="link" disabled={busy} onclick={startNew}>New file</button>
        </div>
        {#if newName !== null}
          <form class="new" onsubmit={(e) => { e.preventDefault(); createNew(); }}>
            <input type="text" placeholder="file-name.md" bind:value={newName} aria-label="New memory file name" />
            <button type="submit" class="btn">Create</button>
            <button type="button" class="link" onclick={() => (newName = null)}>Cancel</button>
          </form>
        {/if}
        {#if !topFiles.length && !subFiles.length}
          <p class="note">No memory files yet. Agents save them as they learn things; you can add your own.</p>
        {/if}
        <ul role="list">
          {#each [...topFiles, ...subFiles] as f (f.name)}
            {@const p = provenance(f)}
            <li>
              <button type="button" class="file" class:selected={open?.name === f.name} onclick={() => select(f)}>
                <span class="file-title">{f.title}</span>
                <span class="file-meta">
                  <span class="file-name">{f.name}</span>
                  <Tooltip text={p.tip}><span class="badge {p.tone}">{p.text}</span></Tooltip>
                  {#each fileTags(f) as tag (tag)}
                    <Tooltip text={TAG_TIP[tag] ?? tag}><span class="tag">{tag}</span></Tooltip>
                  {/each}
                </span>
                {#if f.description}<span class="file-desc">{f.description}</span>{/if}
              </button>
            </li>
          {/each}
        </ul>
        <p class="budget">{budgetText(mem.budget)}</p>
      </div>

      <div class="detail">
        {#if open}
          <div class="detail-h">
            <b>{open.name}</b>
            {#if openFile}
              <span class="dim">
                {provenance(openFile).text}{#if openFile.created_by && openFile.created_by !== openFile.by} · first written by {openFile.created_by}{/if}
                {#if openFile.modified_ms} · {when(openFile.modified_ms)}{/if}
              </span>
            {:else if open.sha === null}
              <span class="dim">New file</span>
            {/if}
          </div>
          <textarea
            class="editor"
            bind:value={open.draft}
            readonly={!writable}
            spellcheck="false"
            aria-label="Memory file {open.name}"
          ></textarea>
          <div class="actions">
            {#if writable}
              <button type="button" class="btn primary" disabled={busy || !dirty} onclick={save}>Save</button>
              {#if dirty}<button type="button" class="link" onclick={() => open && (open.draft = open.text)}>Discard</button>{/if}
            {/if}
            {#if open.sha && !openFile?.archive}
              {#if confirmDelete}
                <span class="confirm">Delete {open.name}? Agents stop seeing it at their next start.</span>
                <button type="button" class="btn danger" disabled={busy} onclick={del}>Delete</button>
                <button type="button" class="link" onclick={() => (confirmDelete = false)}>Cancel</button>
              {:else}
                <button type="button" class="link danger" disabled={busy} onclick={() => (confirmDelete = true)}>Delete…</button>
              {/if}
            {/if}
            {#if openFile && !openFile.in_subfolder && !openFile.archive && !openFile.pinned}
              <Tooltip text="Put this file's index line in the kept block, which maiTerm never rewrites">
                <button type="button" class="link" disabled={busy} onclick={() => act('pin', () => hiveMemoryPin(workspaceId, open!.name))}>Pin</button>
              </Tooltip>
            {/if}
            {#if actionError?.includes('since you opened it')}
              <button type="button" class="link" onclick={reopen}>Reopen</button>
            {/if}
          </div>
          {#if openFile?.in_subfolder}
            <p class="note">Files in subfolders are listed but never indexed; edit them where they are.</p>
          {:else if openFile?.archive}
            <p class="note">maiTerm's fold of older index lines: every file it lists is still in the folder.</p>
          {/if}
        {:else}
          <p class="note">Pick a file to read or edit it. maiTerm keeps MEMORY.md's index from each file's name and description.</p>
        {/if}

        <div class="kept">
          <div class="detail-h">
            <b>Kept block</b>
            <span class="dim">Your lines at the top of MEMORY.md. maiTerm never rewrites them.</span>
          </div>
          <textarea
            class="editor kept-editor"
            bind:value={keptDraft}
            oninput={() => (keptTouched = true)}
            spellcheck="false"
            aria-label="Kept block"
          ></textarea>
          {#if keptErr}<p class="error">{keptErr}</p>{/if}
          {#if keptMovedOn}<p class="warn" role="status">The kept block changed since you started editing. Discard to see it, then make your change again.</p>{/if}
          <div class="actions">
            <button type="button" class="btn primary" disabled={busy || !keptTouched || !!keptErr} onclick={saveKept}>Save kept block</button>
            {#if keptTouched}<button type="button" class="link" onclick={discardKept}>Discard</button>{/if}
          </div>
        </div>

        {#if mem.unfiled.length}
          <div class="unfiled">
            <div class="detail-h">
              <b>Unfiled lines</b>
              <span class="dim">Found in MEMORY.md, linking no memory file: moved to unfiled.md, never dropped.</span>
            </div>
            <ul role="list">
              {#each mem.unfiled as line, i (`${i}:${line}`)}
                <li class="uline">
                  <code>{line}</code>
                  <span class="uacts">
                    <Tooltip text="Leave it in unfiled.md and stop offering it here">
                      <button type="button" class="link" disabled={busy} onclick={() => act('keep', () => hiveMemoryUnfiled(workspaceId, line, 'keep'))}>Keep</button>
                    </Tooltip>
                    <Tooltip text="Move it into the kept block">
                      <button type="button" class="link" disabled={busy} onclick={() => act('pin', () => hiveMemoryUnfiled(workspaceId, line, 'pin'))}>Pin</button>
                    </Tooltip>
                    <Tooltip text="Remove it from unfiled.md">
                      <button type="button" class="link danger" disabled={busy} onclick={() => act('delete', () => hiveMemoryUnfiled(workspaceId, line, 'delete'))}>Delete</button>
                    </Tooltip>
                  </span>
                </li>
              {/each}
            </ul>
          </div>
        {/if}
      </div>
    </div>
  {/if}
</section>

<style>
  .memory { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; overflow: hidden; }
  .tabs { display: flex; gap: 4px; padding: 8px 14px 0; border-bottom: 1px solid var(--bg-light); }
  .tab { background: none; border: none; border-bottom: 2px solid transparent; padding: 4px 8px; color: var(--fg-dim); font: inherit; font-size: 12px; cursor: pointer; }
  .tab.active { color: var(--fg); border-bottom-color: var(--accent); }
  .split { flex: 1; min-height: 0; display: flex; flex-wrap: wrap; }
  .list { flex: 1 1 240px; max-width: 380px; min-width: 0; min-height: 0; overflow-y: auto; border-right: 1px solid var(--bg-light); padding: 10px; display: flex; flex-direction: column; gap: 6px; }
  .list-h, .detail-h { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 10px; font-size: 12px; }
  .list ul, .unfiled ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 4px; }
  .file { width: 100%; text-align: left; background: var(--bg-medium); border: 1px solid transparent; border-radius: 4px; padding: 6px 8px; color: var(--fg); font: inherit; cursor: pointer; display: flex; flex-direction: column; gap: 2px; }
  .file:hover, .file:focus-visible { border-color: var(--bg-light); outline: none; }
  .file.selected { border-color: var(--accent); }
  .file-title { font-size: 12.5px; overflow-wrap: anywhere; }
  .file-meta { display: flex; flex-wrap: wrap; align-items: center; gap: 4px 6px; font-size: 10.5px; color: var(--fg-dim); }
  .file-name { overflow-wrap: anywhere; }
  .file-desc { font-size: 11px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .badge { border-radius: 3px; padding: 0 4px; border: 1px solid var(--bg-light); }
  .badge.human { color: var(--green); border-color: color-mix(in srgb, var(--green) 45%, transparent); }
  .badge.coordinator { color: var(--accent); border-color: color-mix(in srgb, var(--accent) 45%, transparent); }
  .badge.worker { color: var(--fg); }
  .badge.unknown { color: var(--yellow); border-color: color-mix(in srgb, var(--yellow) 45%, transparent); }
  .tag { font-style: italic; }
  .budget { margin: auto 0 0; padding-top: 8px; font-size: 10.5px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .detail { flex: 2 1 320px; min-width: 0; min-height: 0; overflow-y: auto; padding: 10px 14px; display: flex; flex-direction: column; gap: 8px; }
  .editor { width: 100%; box-sizing: border-box; min-height: 180px; resize: vertical; background: var(--bg-medium); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 4px; padding: 8px; font-family: var(--font-mono, monospace); font-size: 12px; }
  .editor[readonly] { color: var(--fg-dim); }
  .kept-editor { min-height: 90px; }
  .kept, .unfiled { display: flex; flex-direction: column; gap: 6px; padding-top: 8px; border-top: 1px solid var(--bg-light); }
  .actions { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; font-size: 12px; }
  .new { display: flex; flex-wrap: wrap; gap: 6px; align-items: center; }
  .new input { flex: 1 1 140px; min-width: 0; background: var(--bg-medium); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 4px; padding: 3px 6px; font: inherit; font-size: 12px; }
  .uline { display: flex; flex-wrap: wrap; gap: 4px 10px; align-items: baseline; font-size: 12px; }
  .uline code { flex: 1 1 200px; min-width: 0; overflow-wrap: anywhere; white-space: pre-wrap; }
  .uacts { display: flex; gap: 8px; }
  .btn { background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 4px; padding: 3px 10px; color: var(--fg); font: inherit; font-size: 12px; cursor: pointer; }
  .btn.primary { border-color: var(--accent); color: var(--accent); }
  .btn.danger { border-color: var(--red); color: var(--red); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font: inherit; font-size: 12px; cursor: pointer; }
  .link:hover { text-decoration: underline; }
  .link:disabled { opacity: 0.5; cursor: default; text-decoration: none; }
  .link.danger { color: var(--fg-dim); }
  .link.danger:hover { color: var(--red); }
  .confirm { color: var(--fg); overflow-wrap: anywhere; }
  .dim, .note { color: var(--fg-dim); font-size: 12px; overflow-wrap: anywhere; }
  .note { margin: 0; }
  .warn { color: var(--yellow); font-size: 12px; margin: 6px 14px 0; overflow-wrap: anywhere; }
  .banner { display: flex; flex-wrap: wrap; gap: 4px 12px; align-items: baseline; }
  .error { color: var(--red); font-size: 12px; margin: 6px 14px 0; overflow-wrap: anywhere; }
  .detail .error { margin: 0; }
</style>
