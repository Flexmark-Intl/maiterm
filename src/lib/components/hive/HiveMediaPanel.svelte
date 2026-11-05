<!--
  "What it can make" (docs/hive-media.md §7): what this Mac already covers for each media
  capability, and for one nothing ready covers, the provider to set up and the steps only the
  human can do. Each card's "Set up <provider>" (and each alternative's "Set up") opens
  HiveMediaSetupSheet, which does the setup; the panel fetches again when the sheet says
  something changed, and tells its parent only through `onchanged`.

  Placed by the Create sheet's Tools step, Hive settings and Preferences → Hive. Props:
    workspaceId  the Hive's workspace (the Create sheet's, about to become one, or Hive settings'),
                 null in Preferences; an MCP proxy server scoped away from it is "not enabled for
                 this workspace". The setup sheet scopes a server it adds to it, and never opens an
                 install tab in it.
    startOpen    the caller's `shouldStartOpen()`; read ONCE at mount. After that the human's
                 choice wins, whatever the parent does.
    requested    optional: capability ids a planning agent's proposal asked for (docs/hive-media.md
                 §8). Read ONCE at mount. Each such row gets an "Asked for" tag, and one that lives
                 under "More" is listed in the main list instead, so More never opens by itself.
                 No card opens: this only shows which rows were asked for.
    onseeded     optional: called once at mount, after `requested` was read. The caller drops its
                 seed there, so a remount (Back, then Next) shows no tags and re-opens nothing.
    onchanged   optional: something a setup sheet did may have changed this Mac. It carries no
                 ids: the panel decides no ticks. The Create sheet and Hive settings read the status
                 themselves and detect their tools again on it (event-driven: never a poll).
    onstarted    optional, passed to the setup sheet: the install tab or server a step started.
                 The parent that outlives the panel watches for its end (createSetupWatch); the
                 panel listens for nothing app-wide itself.
    setup        optional: an approved media ask's sheet to open (docs/hive-media.md §8, Hive
                 settings only). Read ONCE at mount; spent by the first fetch that answers, which
                 opens that provider's sheet as its "Set up" button would (same baseline, same
                 `onstarted`), with the install Rust already started from the phone, if any.

  Ordering: every fetch takes a `createLatest()` ticket and checks it after each await, so a slow
  first answer can't overwrite a newer one and nothing is set after unmount. All joining and
  wording is lib/hive/media.ts.
-->
<script lang="ts">
  import { onMount, tick as nextTick, untrack } from 'svelte';
  import HiveMediaSetupSheet from './HiveMediaSetupSheet.svelte';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import { warn as logWarn } from '@tauri-apps/plugin-log';
  import { hiveMediaCatalogue, hiveMediaStatus } from '$lib/tauri/commands';
  import type { HiveMediaCatalogue, HiveMediaInstallFinished, HiveMediaInstallStarted, HiveMediaRow } from '$lib/tauri/types';
  import { seededSetup, type SetupSeed } from '$lib/hive/toolSetup';
  import { compactLine, createLatest, placeRequested, readyToolIds, requestedCapabilities, externalUrl, coverNames, groupRows, modelsLine, stateTone, recommendedSetupNote, subscriberProviders, summaryLine, toggleId, unverifiedNote } from '$lib/hive/media';
  import type { PanelProvider, PanelRow, SetupStarted } from '$lib/hive/media';

  interface Props {
    workspaceId: string | null;
    startOpen: boolean;
    requested?: string[];
    onseeded?: () => void;
    onchanged?: () => void;
    onstarted?: (s: SetupStarted) => void;
    setup?: SetupSeed | null;
  }
  let { workspaceId, startOpen, requested, onseeded, onchanged, onstarted, setup: setupSeed = null }: Props = $props();

  const uid = $props.id();
  const bodyId = `${uid}-body`;
  const moreId = `${uid}-more`;

  let open = $state(untrack(() => startOpen));
  let moreOpen = $state(false);
  /** The capabilities a proposal asked for, read once: tagged, and lifted out of More. */
  const asked = untrack(() => requestedCapabilities(requested));
  /** Capability ids whose full card is open. Local to the panel, never persisted. */
  let cardsOpen = $state<string[]>([]);
  let rows = $state<HiveMediaRow[] | null>(null);
  let catalogue = $state<HiveMediaCatalogue | null>(null);
  let loading = $state(true);
  let failure = $state<string | null>(null);
  /** Subscriber provider ids the human ticked. Component state only in this phase. */
  let pay = $state<string[]>([]);

  const latest = createLatest();

  /** The open setup sheet, a new object per opening (the sheet is keyed by it, so each opening
   *  mounts a fresh one with its own seeds), and the button that opened it, for focus on close. */
  let setup = $state<{ provider: PanelProvider; capability: string; readyBefore: string[]; install?: HiveMediaInstallStarted | null; finished?: HiveMediaInstallFinished | null } | null>(null);
  let setupOpener: HTMLElement | null = null;
  /** The approved ask's sheet, until the first fetch opens it. */
  let seed = untrack(() => setupSeed);
  /** Why the approved ask's sheet couldn't open, shown above the rows. */
  let seedError = $state<string | null>(null);

  const groups = $derived(rows && catalogue ? groupRows(rows, catalogue) : null);
  const placed = $derived(groups ? placeRequested(groups, asked) : { main: [] as PanelRow[], more: [] as PanelRow[] });
  const subscribers = $derived(catalogue ? subscriberProviders(catalogue) : []);
  const line = $derived(failure ? "Media: couldn't check" : groups ? summaryLine(groups) : 'Media: checking…');
  const empty = $derived(!!groups && groups.common.length + groups.more.length === 0);

  async function load() {
    const stale = latest.next();
    // Props and state are read here, before the first await: nothing reads them after unmount.
    const ws = workspaceId;
    const paying = [...pay];
    const known = catalogue;
    loading = true;
    failure = null;
    try {
      const [cat, status] = await Promise.all([known ?? hiveMediaCatalogue(), hiveMediaStatus(ws, paying)]);
      if (stale()) return;
      catalogue = cat;
      rows = status;
      if (seed) openSeeded(seed, status, cat);
    } catch (e) {
      if (stale()) return;
      failure = String(e);
    }
    if (!stale()) loading = false;
  }

  onMount(() => {
    void load();
    onseeded?.();
    return () => latest.cancel();
  });

  let redetectBtn = $state<HTMLButtonElement | null>(null);

  /** Re-detect stays focusable while a fetch runs (aria-disabled, not disabled), so a keyboard user
   *  isn't dropped out of the page mid-fetch; a click then does nothing. */
  function redetect() {
    if (!loading) void load();
  }

  /** Retry sits in the error block, which a new fetch replaces, so a focused Retry would vanish and
   *  focus would fall to the page. Focus moves to the head's Re-detect button, which stays mounted,
   *  before the fetch starts. */
  function retry() {
    redetectBtn?.focus();
    redetect();
  }

  function tick(id: string) {
    pay = toggleId(pay, id);
    void load();
  }

  /** The approved ask's sheet, as its row's "Set up" would open it: its card shown, More opened
   *  when the row lives there, the same baseline. Spent once, whatever the outcome. */
  function openSeeded(s: SetupSeed, status: HiveMediaRow[], cat: HiveMediaCatalogue) {
    seed = null;
    const got = seededSetup(status, cat, s);
    if ('error' in got) {
      seedError = got.error;
      return;
    }
    open = true;
    if (!cardsOpen.includes(got.capability)) cardsOpen = [...cardsOpen, got.capability];
    // `placed` reads the rows just set.
    if (!placed.main.some((r) => r.capability === got.capability)) moreOpen = true;
    setup = { provider: got.provider, capability: got.label, readyBefore: got.readyBefore, install: s.install, finished: s.finished };
  }

  function openSetup(e: MouseEvent, provider: PanelProvider, capability: string) {
    setupOpener = e.currentTarget as HTMLElement;
    // The baseline the setup is judged against: what was ready when the human opened it.
    setup = { provider, capability, readyBefore: readyToolIds(rows) };
  }

  /** Focus goes back to the button that opened the sheet, or to Re-detect when a re-fetch took that
   *  button away (the capability became ready). Closing fetches again and tells the parent: a step
   *  in flight at the close (an add, a key save) may have landed with nobody left to say so. */
  function closeSetup() {
    setup = null;
    setupChanged();
    const opener = setupOpener;
    setupOpener = null;
    void nextTick().then(() => requestAnimationFrame(() => (opener?.isConnected ? opener : redetectBtn)?.focus()));
  }

  /** The sheet may have changed something on this Mac: fetch again for the display, and tell the
   *  parent at once. The parent reads the status itself and decides the ticks
   *  (`setupTicks`), so nothing here waits on this fetch, and an unmount mid-fetch loses nothing. */
  function setupChanged() {
    void load();
    onchanged?.();
  }

  /** The parent heard the end of a step this panel's sheet started: fetch again so the rows show
   *  it. Display only: it tells nobody (the parent already owns that end). */
  export function refresh() {
    void load();
  }

  async function go(url: string) {
    try {
      await shellOpen(url);
    } catch (e) {
      void logWarn(`[HiveMediaPanel] could not open ${url}: ${String(e)}`);
    }
  }
  function follow(e: MouseEvent, url: string) {
    e.preventDefault();
    void go(url);
  }
</script>

{#snippet link(label: string, url: string)}
  {@const safe = externalUrl(url)}
  {#if safe}
    <a class="ext" href={safe} rel="noopener noreferrer" onclick={(e) => follow(e, safe)} onauxclick={(e) => e.preventDefault()}>{label}</a>
  {/if}
{/snippet}

{#snippet capRow(r: PanelRow)}
  {@const c = compactLine(r)}
  {@const cardId = `${uid}-card-${r.capability}`}
  {@const showCard = c.details && cardsOpen.includes(r.capability)}
  <section class="cap" aria-labelledby="{uid}-{r.capability}">
    <div class="line">
      <h4 id="{uid}-{r.capability}">{r.label}</h4>
      <div class="sum">
        {#if asked.includes(r.capability)}<span class="tag asked">Asked for</span>{/if}
        {#if r.covers.length}
          <ul class="covers">
            {#each r.covers as cv (cv.toolId)}
              {@const names = coverNames(cv)}
              <li>
                <span class="tool">{names.tool}</span>
                {#if names.by}<span class="by">{names.by}</span>{/if}
                <span class="state {stateTone(cv.state)}">{cv.stateLabel}</span>
              </li>
            {/each}
          </ul>
        {:else}
          <span class="none">{c.status}</span>
          {#if c.suggested}<span class="by">{c.suggested}</span>{/if}
        {/if}
      </div>
      {#if c.details}
        <button type="button" class="disc" aria-expanded={showCard} aria-controls={showCard ? cardId : undefined} onclick={() => (cardsOpen = toggleId(cardsOpen, r.capability))}>
          <svg class="chev" class:open={showCard} width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3 1.5 7 5 3 8.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
          {c.detailsLabel}
        </button>
      {/if}
    </div>

    {#if showCard && r.recommended}
      {@const p = r.recommended}
      {@const note = unverifiedNote(p, r.capability)}
      {@const done = recommendedSetupNote(r)}
      <div class="card" id={cardId}>
        <div class="card-head">
          <b class="pname">{p.name}</b>
          {#if done}<span class="tag warn">{done}</span>{:else}<span class="tag">Recommended</span>{/if}
        </div>
        {#if modelsLine(p, r.capability)}<p class="dim">{modelsLine(p, r.capability)}</p>{/if}
        {#if p.creditsLine}<p class="credits">{p.creditsLine}</p>{/if}
        {#if note}<p class="unv">{note}</p>{/if}
        {#if p.guide.length}
          <ol class="guide">
            {#each p.guide as step, i (i)}<li>{step}</li>{/each}
          </ol>
        {/if}
        {#if externalUrl(p.signup_url) || externalUrl(p.billing_url)}
          <p class="links">
            {@render link('Sign up', p.signup_url)}
            {@render link('Billing', p.billing_url)}
          </p>
        {/if}
        <div class="setup-row">
          <button type="button" class="setup" onclick={(e) => openSetup(e, p, r.label)}>Set up {p.name}</button>
        </div>
        {#if r.alternatives.length}
          <div class="alts">
            <span class="alts-label">Other options</span>
            <ul>
              {#each r.alternatives as a (a.id)}
                {@const anote = unverifiedNote(a, r.capability)}
                <li>
                  <span class="pname">{a.name}</span>
                  {#if a.creditsLine}<span class="dim">· {a.creditsLine}</span>{/if}
                  {#if anote}<span class="unv">· {anote}</span>{/if}
                  <button type="button" class="link" aria-label="Set up {a.name}" onclick={(e) => openSetup(e, a, r.label)}>Set up</button>
                </li>
              {/each}
            </ul>
          </div>
        {/if}
      </div>
    {/if}
  </section>
{/snippet}

<div class="panel">
  <div class="head">
    <button type="button" class="toggle" aria-expanded={open} aria-controls={open ? bodyId : undefined} onclick={() => (open = !open)}>
      <svg class="chev" class:open width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3 1.5 7 5 3 8.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
      <span class="title">{open ? 'What it can make' : line}</span>
    </button>
    <button type="button" class="link" bind:this={redetectBtn} onclick={redetect} aria-disabled={loading} aria-label="Re-detect what this Mac can make">Re-detect</button>
  </div>

  {#if open}
    <div class="body" id={bodyId} aria-busy={loading}>
      {#if loading && !groups}
        <p class="dim" role="status">Checking what this Mac can make…</p>
      {:else if failure}
        <div class="error" role="alert">
          <p>Couldn't check what this Mac can make.</p>
          <p class="dim detail">{failure}</p>
          <button type="button" class="link" onclick={retry}>Retry</button>
        </div>
      {:else if empty}
        <p class="dim">No media capabilities to show.</p>
      {:else if groups}
        {#if loading}<p class="dim" role="status">Checking…</p>{/if}
        {#if seedError}<p class="seed-error" role="alert">{seedError} Set it up from its row below instead.</p>{/if}

        <p class="dim">What this Mac can already make. Opening a card doesn't add anything to the Hive.</p>

        {#each placed.main as r (r.capability)}
          {@render capRow(r)}
        {/each}

        {#if subscribers.length}
          <fieldset class="pay">
            <legend>I already pay for…</legend>
            {#each subscribers as p (p.id)}
              <label class="tickrow">
                <input type="checkbox" checked={pay.includes(p.id)} onchange={() => tick(p.id)} />
                <span class="pname">{p.name}</span>
              </label>
            {/each}
          </fieldset>
        {/if}

        {#if placed.more.length}
          <button type="button" class="more" aria-expanded={moreOpen} aria-controls={moreOpen ? moreId : undefined} onclick={() => (moreOpen = !moreOpen)}>
            <svg class="chev" class:open={moreOpen} width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3 1.5 7 5 3 8.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
            More ({placed.more.length})
          </button>
          {#if moreOpen}
            <div class="more-body" id={moreId}>
              {#each placed.more as r (r.capability)}
                {@render capRow(r)}
              {/each}
            </div>
          {/if}
        {/if}
      {/if}
    </div>
  {/if}
</div>

{#if setup}
  {#key setup}
    <HiveMediaSetupSheet provider={setup.provider} capabilityLabel={setup.capability} {workspaceId} onclose={closeSetup} onchanged={setupChanged} {onstarted} readyBefore={setup.readyBefore} install={setup.install ?? null} installFinished={setup.finished ?? null} />
  {/key}
{/if}

<style>
  .panel { display: flex; flex-direction: column; gap: 8px; min-width: 0; padding: 8px 10px; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 6px; }
  .head { display: flex; align-items: baseline; gap: 8px; min-width: 0; }
  .toggle { display: flex; align-items: baseline; gap: 6px; flex: 1; min-width: 0; padding: 0; background: none; border: none; color: var(--fg); font: inherit; font-size: 0.923rem; font-weight: 600; text-align: left; cursor: pointer; }
  .toggle:focus-visible, .more:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .title { overflow-wrap: anywhere; min-width: 0; }
  .chev { flex: none; align-self: center; color: var(--fg-dim); transition: transform 0.15s; }
  .chev.open { transform: rotate(90deg); }
  .link { flex: none; background: none; border: none; padding: 0; color: var(--accent); cursor: pointer; font: inherit; font-size: 0.846rem; }
  .link:hover:not([aria-disabled='true']) { text-decoration: underline; }
  .link[aria-disabled='true'] { color: var(--fg-dim); cursor: default; }
  .link:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }

  .body { display: flex; flex-direction: column; gap: 6px; min-width: 0; }
  .dim { margin: 0; font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .error { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; font-size: 0.923rem; color: var(--fg); }
  .error p { margin: 0; overflow-wrap: anywhere; }
  .detail { font-size: 0.808rem; }
  .seed-error { margin: 0; font-size: 0.846rem; color: var(--yellow); overflow-wrap: anywhere; }

  .pay { margin: 0; padding: 6px 8px; display: flex; flex-wrap: wrap; gap: 4px 14px; border: 1px solid var(--bg-light); border-radius: 5px; min-width: 0; }
  .pay legend { padding: 0 4px; font-size: 0.808rem; color: var(--fg-dim); }
  .tickrow { display: inline-flex; align-items: center; gap: 6px; font-size: 0.923rem; color: var(--fg); cursor: pointer; }
  .tickrow input { accent-color: var(--accent); }

  .cap { display: flex; flex-direction: column; gap: 6px; min-width: 0; padding-top: 6px; border-top: 1px solid var(--bg-light); }
  .line { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 12px; min-width: 0; }
  .cap h4 { flex: 0 0 5.5em; }
  .sum { flex: 1 1 12em; display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 8px; min-width: 0; font-size: 0.846rem; }
  .none { color: var(--fg-dim); }
  .disc { flex: none; display: inline-flex; align-items: baseline; gap: 5px; padding: 0; background: none; border: none; color: var(--accent); font: inherit; font-size: 0.846rem; cursor: pointer; text-align: left; }
  .disc:hover { text-decoration: underline; }
  .disc:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .cap h4 { margin: 0; font-size: 0.923rem; font-weight: 600; color: var(--fg); overflow-wrap: anywhere; }
  .covers { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; min-width: 0; }
  .covers li { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 8px; font-size: 0.846rem; min-width: 0; }
  .tool { color: var(--fg); overflow-wrap: anywhere; }
  .by { color: var(--fg-dim); overflow-wrap: anywhere; }
  .state { font-size: 0.769rem; padding: 0 6px; border-radius: 8px; border: 1px solid var(--bg-light); color: var(--fg-dim); overflow-wrap: anywhere; }
  .state.ok { color: var(--green); border-color: var(--green); }
  .state.warn { color: var(--yellow); border-color: var(--yellow); }

  .card { display: flex; flex-direction: column; gap: 4px; min-width: 0; padding: 8px 10px; background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; }
  .card p { margin: 0; }
  .card-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 8px; }
  .pname { font-weight: 600; color: var(--fg); overflow-wrap: anywhere; }
  .tag { font-size: 0.769rem; color: var(--accent); overflow-wrap: anywhere; }
  .tag.warn { color: var(--yellow); }
  .credits { font-size: 0.846rem; color: var(--fg); overflow-wrap: anywhere; }
  .unv { font-size: 0.769rem; font-style: italic; color: var(--fg-dim); overflow-wrap: anywhere; }
  .guide { margin: 2px 0; padding-left: 20px; font-size: 0.846rem; color: var(--fg); overflow-wrap: anywhere; }
  .guide li { padding: 1px 0; }
  .links { display: flex; flex-wrap: wrap; gap: 4px 14px; font-size: 0.846rem; }
  .ext { color: var(--accent); text-decoration: none; }
  .ext:hover { text-decoration: underline; }
  .ext:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .setup-row { display: flex; flex-wrap: wrap; gap: 6px; padding-top: 2px; }
  .setup { padding: 4px 12px; background: var(--accent); color: var(--bg-dark); border: none; border-radius: 5px; font: inherit; font-size: 0.846rem; font-weight: 600; cursor: pointer; text-align: left; overflow-wrap: anywhere; }
  .setup:hover { filter: brightness(1.1); }
  .setup:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .alts { display: flex; flex-direction: column; gap: 2px; padding-top: 4px; border-top: 1px dashed var(--bg-light); }
  .alts-label { font-size: 0.769rem; color: var(--fg-dim); }
  .alts ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; }
  .alts li { font-size: 0.846rem; overflow-wrap: anywhere; display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 6px; }
  .alts .pname { font-weight: 500; }

  .more { display: flex; align-items: baseline; gap: 6px; align-self: flex-start; padding: 2px 0; background: none; border: none; color: var(--fg-dim); font: inherit; font-size: 0.846rem; cursor: pointer; }
  .more:hover { color: var(--fg); }
  .more-body { display: flex; flex-direction: column; gap: 10px; min-width: 0; }
</style>
