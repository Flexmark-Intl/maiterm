<!--
  Hive settings → Tools (docs/hive.md §3.4, docs/hive-media.md §7): the Hive's tools as one line,
  and behind Change the Create sheet's grouped picker, the hosts the newly added tools need (offered
  ticked, as Re-detect offers them) and Save behind an inline confirm. Rust checks every id again
  against a fresh detection (hive/tool_change.rs) and tells the coordinator; workers get the change
  at their next launch. The pure half is lib/hive/toolsChange.ts.
-->
<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { error as logError } from '@tauri-apps/plugin-log';
  import type { Hive, HiveDetectedTool, HiveToolsPreview } from '$lib/tauri/types';
  import { hiveDetectTools, hiveSetTools, hiveToolsChangePreview } from '$lib/tauri/commands';
  import { pickerTools } from '$lib/hive/media';
  import { confirmLine, labelOf, nextTools, seedPicked, tickedHosts, toolsChanged, undetected } from '$lib/hive/toolsChange';
  import HiveToolPicker from './HiveToolPicker.svelte';

  interface Props {
    hive: Hive;
    workspaceId: string;
  }
  let { hive, workspaceId }: Props = $props();

  let alive = true;
  onDestroy(() => {
    alive = false;
  });

  /** This Mac's tools, without setup plumbing; null until detection answers. */
  let detected = $state<HiveDetectedTool[] | null>(null);
  let detectFailed = $state(false);
  /** Detection's newest request: an older answer never writes over a newer list. */
  let detectSeq = 0;
  function detect() {
    const mine = ++detectSeq;
    hiveDetectTools()
      .then((found) => {
        if (!alive || mine !== detectSeq) return;
        detected = pickerTools(found);
        detectFailed = false;
      })
      .catch((e) => {
        if (!alive || mine !== detectSeq) return;
        logError(`hive settings: tool detection failed: ${e}`);
        // A failed re-detection keeps the list it had; only a first failure leaves none.
        if (detected !== null) return;
        detected = [];
        detectFailed = true;
      });
  }
  detect();

  /** Detect again: the media panel above set something up (docs/hive-media.md §6). A tool that
   *  became ready appears in the picker as one the human can add; nothing is added to the Hive's
   *  tools, and the picks are left as they are: Save is the human's. */
  export function redetect() {
    detect();
  }

  /** Seeded once, from the Hive as the sheet opened; Cancel and Save seed it again. */
  let picked = $state(seedPicked(untrack(() => hive.tools)));
  let editing = $state(false);
  /** Offered hosts the human unticked. */
  let unticked = $state(new Set<string>());
  let confirming = $state(false);
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  let savedNote = $state<string | null>(null);

  /** The controls focus moves to when the one just used goes away: a keyboard user stays in the
   *  sheet, where Tab and the backdrop's Escape still work. */
  let changeBtn = $state<HTMLButtonElement | null>(null);
  let editorEl = $state<HTMLDivElement | null>(null);
  let saveMoreBtn = $state<HTMLButtonElement | null>(null);
  let confirmBtn = $state<HTMLButtonElement | null>(null);
  function focusAfter(get: () => HTMLElement | null | undefined) {
    void tick().then(() => {
      if (alive) get()?.focus();
    });
  }

  const known = $derived(detected ?? []);
  const label = (id: string) => labelOf(id, known);
  const summary = $derived(hive.tools.length ? hive.tools.map(label).join(', ') : 'none');
  const missing = $derived(detected ? undetected(hive.tools, detected) : []);
  const next = $derived(detected ? nextTools(hive.tools, picked, detected) : []);
  const changed = $derived(detected !== null && toolsChanged(hive.tools, next));
  /** What the preview is asked for: empty while nothing would change. */
  const nextKey = $derived(changed ? next.join('\n') : '');

  let preview = $state<HiveToolsPreview | null>(null);
  /** The `nextKey` the preview answers: Save waits until it is the current one. */
  let previewFor = $state('');
  let previewError = $state<string | null>(null);
  let seq = 0;

  $effect(() => {
    const key = nextKey;
    const ws = workspaceId;
    const tools = untrack(() => next);
    const mine = ++seq;
    if (!key) {
      preview = null;
      previewFor = '';
      previewError = null;
      return;
    }
    // A short wait so a run of ticks asks once.
    const t = setTimeout(() => {
      hiveToolsChangePreview(ws, tools)
        .then((p) => {
          if (!alive || mine !== seq) return;
          preview = p;
          previewFor = key;
          previewError = null;
        })
        .catch((e) => {
          if (!alive || mine !== seq) return;
          preview = null;
          previewFor = '';
          previewError = `Couldn't check the change: ${String(e).replace(/^Error: /, '')}`;
        });
    }, 200);
    return () => clearTimeout(t);
  });

  const ready = $derived(preview !== null && previewFor === nextKey && nextKey !== '');
  const ticked = $derived(preview ? tickedHosts(preview.hosts_offered, unticked) : []);

  function setPicked(nextPicked: Set<string>) {
    picked = nextPicked;
    confirming = false;
    savedNote = null;
  }
  function flipMissing(id: string) {
    const n = new Set(picked);
    if (n.has(id)) n.delete(id);
    else n.add(id);
    setPicked(n);
  }
  function flipHost(h: string) {
    const n = new Set(unticked);
    if (n.has(h)) n.delete(h);
    else n.add(h);
    unticked = n;
  }
  function start() {
    picked = seedPicked(hive.tools);
    unticked = new Set();
    saveError = null;
    savedNote = null;
    editing = true;
    focusAfter(() => editorEl?.querySelector<HTMLElement>('input, button'));
  }
  function cancel() {
    picked = seedPicked(hive.tools);
    unticked = new Set();
    confirming = false;
    saveError = null;
    editing = false;
    focusAfter(() => changeBtn);
  }
  function askConfirm() {
    confirming = true;
    focusAfter(() => confirmBtn);
  }
  function back() {
    confirming = false;
    focusAfter(() => saveMoreBtn);
  }

  async function save() {
    if (!ready || saving) return;
    const ws = workspaceId;
    const tools = [...next];
    const hosts = [...ticked];
    saving = true;
    saveError = null;
    try {
      const r = await hiveSetTools(ws, tools, hosts);
      if (!alive) return;
      picked = seedPicked(r.hive.tools);
      unticked = new Set();
      confirming = false;
      editing = false;
      savedNote = r.coordinator_told
        ? 'Saved. The queen was told; workers get the new tools at their next launch.'
        : 'Saved. Workers get the new tools at their next launch. The queen couldn’t be told now: tell her yourself.';
      focusAfter(() => changeBtn);
    } catch (e) {
      if (!alive) return;
      saveError = `Couldn't save the tools: ${String(e).replace(/^Error: /, '')}`;
      logError(`hive settings: tools change failed: ${e}`);
      focusAfter(() => confirmBtn);
    } finally {
      if (alive) saving = false;
    }
  }
</script>

<div class="tools">
  <p class="line">
    <span class="why">This Hive’s tools:</span>
    <span>{summary}</span>
    {#if !editing}
      <button type="button" class="link" bind:this={changeBtn} disabled={detected === null || detectFailed} onclick={start}>Change…</button>
    {/if}
  </p>
  {#if detectFailed}
    <p class="why">Couldn’t detect this Mac’s tools, so they can’t change now.</p>
  {/if}
  {#if savedNote}<p class="ok" role="status">{savedNote}</p>{/if}

  {#if editing && detected}
    <div class="editor" bind:this={editorEl}>
    <p class="why">Workers get what’s ticked at their next launch; a running worker keeps what it started with. Skills reach the queen only.</p>
    <HiveToolPicker tools={detected} {picked} onchange={setPicked} />

    {#if missing.length}
      <div class="block">
        <span class="sub">Not found on this Mac</span>
        <span class="why">Kept unless you untick it; a launch leaves out what it can’t find.</span>
        {#each missing as id (id)}
          <label class="row">
            <input type="checkbox" checked={picked.has(id)} onchange={() => flipMissing(id)} />
            <span class="name">{label(id)}</span>
          </label>
        {/each}
      </div>
    {/if}

    {#if previewError}<p class="err" role="alert">{previewError}</p>{/if}

    {#if ready && preview && preview.hosts_offered.length}
      <div class="block">
        <span class="sub">Hosts the added tools need</span>
        <span class="why">Media workers may reach the ticked ones, from their next launch. Other hosts are Budget and generation’s.</span>
        {#each preview.hosts_offered as h (h)}
          <label class="row">
            <input type="checkbox" checked={!unticked.has(h)} onchange={() => flipHost(h)} />
            <code>{h}</code>
          </label>
        {/each}
      </div>
    {/if}

    {#if saveError}<p class="err" role="alert">{saveError}</p>{/if}

    {#if confirming && ready && preview}
      <div class="confirm" role="group" aria-label="Confirm the tools change">
        <span>{confirmLine(preview.added, preview.removed, ticked.length, label)} The queen is told.</span>
        {#if preview.removed.length}<span class="why">Hosts stay until you remove them in Budget and generation.</span>{/if}
        <div class="actions">
          <button type="button" class="btn primary" bind:this={confirmBtn} disabled={saving} onclick={save}>{saving ? 'Saving…' : 'Save tools'}</button>
          <button type="button" class="btn" disabled={saving} onclick={back}>Back</button>
        </div>
      </div>
    {:else}
      <div class="actions">
        <button type="button" class="btn primary" bind:this={saveMoreBtn} disabled={!ready} onclick={askConfirm}>Save…</button>
        <button type="button" class="btn" onclick={cancel}>Cancel</button>
        {#if !changed}<span class="why">Nothing changed yet.</span>{/if}
      </div>
    {/if}
    </div>
  {/if}
</div>

<style>
  .tools, .editor { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .line { margin: 0; color: var(--fg); font-size: 0.8rem; line-height: 1.4; overflow-wrap: anywhere; display: flex; flex-wrap: wrap; gap: 4px 6px; align-items: baseline; }
  .why { color: var(--fg-dim); font-size: 0.75rem; overflow-wrap: anywhere; margin: 0; }
  .ok { color: var(--fg); font-size: 0.75rem; margin: 0; overflow-wrap: anywhere; }
  .err { color: var(--red); font-size: 0.75rem; margin: 0; overflow-wrap: anywhere; }
  .block { display: flex; flex-direction: column; gap: 3px; min-width: 0; }
  .sub { color: var(--fg); font-size: 0.8rem; font-weight: 600; }
  .row { display: flex; align-items: baseline; gap: 6px; font-size: 0.8rem; color: var(--fg); cursor: pointer; min-width: 0; flex-wrap: wrap; }
  .row input { accent-color: var(--accent); align-self: center; }
  .name { overflow-wrap: anywhere; min-width: 0; }
  code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.75rem; color: var(--fg); overflow-wrap: anywhere; }
  .confirm { display: flex; flex-direction: column; gap: 6px; padding: 8px; border: 1px solid var(--bg-light); border-radius: 6px; background: var(--bg-dark); color: var(--fg); font-size: 0.8rem; overflow-wrap: anywhere; }
  .actions { display: flex; flex-wrap: wrap; gap: 6px; align-items: baseline; }
  .link { background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.8rem; }
  .link:hover:not(:disabled) { text-decoration: underline; }
  .link:disabled { color: var(--fg-dim); cursor: default; }
  .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .btn {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.8rem;
    padding: 5px 10px;
  }
  .btn.primary { border-color: var(--accent); }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn:disabled { opacity: 0.5; cursor: default; }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
</style>
