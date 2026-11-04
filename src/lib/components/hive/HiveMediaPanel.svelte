<!--
  "What it can make" (docs/hive-media.md §7): what this Mac already covers for each media
  capability, and for one nothing ready covers, the provider to set up and the steps only the
  human can do. Read-only in this phase: no setup buttons, and it emits nothing.

  Placed by the Create sheet's Tools step, Hive settings and Preferences → Hive. Props:
    workspaceId  the Hive's workspace (null in Preferences and before Create); an MCP proxy server
                 scoped away from it is "not enabled for this workspace".
    startOpen    the caller's `shouldStartOpen()`; read ONCE at mount. After that the human's
                 choice wins, whatever the parent does.

  Ordering: every fetch takes a `createLatest()` ticket and checks it after each await, so a slow
  first answer can't overwrite a newer one and nothing is set after unmount. All joining and
  wording is lib/hive/media.ts.
-->
<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import { warn as logWarn } from '@tauri-apps/plugin-log';
  import { hiveMediaCatalogue, hiveMediaStatus } from '$lib/tauri/commands';
  import type { HiveMediaCatalogue, HiveMediaRow } from '$lib/tauri/types';
  import { createLatest, externalUrl, groupRows, stateTone, recommendedSetupNote, subscriberProviders, summaryLine, toggleId, unverifiedNote } from '$lib/hive/media';
  import type { PanelRow } from '$lib/hive/media';

  interface Props {
    workspaceId: string | null;
    startOpen: boolean;
  }
  let { workspaceId, startOpen }: Props = $props();

  const uid = $props.id();
  const bodyId = `${uid}-body`;
  const moreId = `${uid}-more`;

  let open = $state(untrack(() => startOpen));
  let moreOpen = $state(false);
  let rows = $state<HiveMediaRow[] | null>(null);
  let catalogue = $state<HiveMediaCatalogue | null>(null);
  let loading = $state(true);
  let failure = $state<string | null>(null);
  /** Subscriber provider ids the human ticked. Component state only in this phase. */
  let pay = $state<string[]>([]);

  const latest = createLatest();

  const groups = $derived(rows && catalogue ? groupRows(rows, catalogue) : null);
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
    } catch (e) {
      if (stale()) return;
      failure = String(e);
    }
    if (!stale()) loading = false;
  }

  onMount(() => {
    void load();
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
  <section class="cap" aria-labelledby="{uid}-{r.capability}">
    <div class="cap-head">
      <h4 id="{uid}-{r.capability}">{r.label}</h4>
      {#if r.means}<span class="means">{r.means}</span>{/if}
    </div>

    {#if r.covers.length}
      <ul class="covers">
        {#each r.covers as c (c.toolId)}
          <li>
            <span class="tool">{c.toolLabel}</span>
            <span class="by">{c.providerName}</span>
            <span class="state {stateTone(c.state)}">{c.stateLabel}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if !r.ready}
      {#if r.recommended}
        {@const p = r.recommended}
        {@const note = unverifiedNote(p, r.capability)}
        {@const done = recommendedSetupNote(r)}
        <div class="card">
          <div class="card-head">
            <b class="pname">{p.name}</b>
            {#if done}<span class="tag warn">{done}</span>{:else}<span class="tag">Recommended</span>{/if}
          </div>
          {#if p.models_note}<p class="dim">{p.models_note}</p>{/if}
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
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
        </div>
      {:else if !r.covers.length}
        <p class="dim">Nothing on this Mac covers this yet.</p>
      {/if}
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

        {#each groups.common as r (r.capability)}
          {@render capRow(r)}
        {/each}

        {#if groups.more.length}
          <button type="button" class="more" aria-expanded={moreOpen} aria-controls={moreOpen ? moreId : undefined} onclick={() => (moreOpen = !moreOpen)}>
            <svg class="chev" class:open={moreOpen} width="10" height="10" viewBox="0 0 10 10" aria-hidden="true"><path d="M3 1.5 7 5 3 8.5" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" /></svg>
            More ({groups.more.length})
          </button>
          {#if moreOpen}
            <div class="more-body" id={moreId}>
              {#each groups.more as r (r.capability)}
                {@render capRow(r)}
              {/each}
            </div>
          {/if}
        {/if}
      {/if}
    </div>
  {/if}
</div>

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

  .body { display: flex; flex-direction: column; gap: 10px; min-width: 0; }
  .dim { margin: 0; font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .error { display: flex; flex-direction: column; align-items: flex-start; gap: 4px; font-size: 0.923rem; color: var(--fg); }
  .error p { margin: 0; overflow-wrap: anywhere; }
  .detail { font-size: 0.808rem; }

  .pay { margin: 0; padding: 6px 8px; display: flex; flex-wrap: wrap; gap: 4px 14px; border: 1px solid var(--bg-light); border-radius: 5px; min-width: 0; }
  .pay legend { padding: 0 4px; font-size: 0.808rem; color: var(--fg-dim); }
  .tickrow { display: inline-flex; align-items: center; gap: 6px; font-size: 0.923rem; color: var(--fg); cursor: pointer; }
  .tickrow input { accent-color: var(--accent); }

  .cap { display: flex; flex-direction: column; gap: 4px; min-width: 0; padding-top: 8px; border-top: 1px solid var(--bg-light); }
  .cap-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 8px; }
  .cap h4 { margin: 0; font-size: 0.923rem; font-weight: 600; color: var(--fg); overflow-wrap: anywhere; }
  .means { font-size: 0.808rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .covers { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; }
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
  .alts { display: flex; flex-direction: column; gap: 2px; padding-top: 4px; border-top: 1px dashed var(--bg-light); }
  .alts-label { font-size: 0.769rem; color: var(--fg-dim); }
  .alts ul { list-style: none; margin: 0; padding: 0; display: flex; flex-direction: column; gap: 2px; }
  .alts li { font-size: 0.846rem; overflow-wrap: anywhere; }
  .alts .pname { font-weight: 500; }

  .more { display: flex; align-items: baseline; gap: 6px; align-self: flex-start; padding: 2px 0; background: none; border: none; color: var(--fg-dim); font: inherit; font-size: 0.846rem; cursor: pointer; }
  .more:hover { color: var(--fg); }
  .more-body { display: flex; flex-direction: column; gap: 10px; min-width: 0; }
</style>
