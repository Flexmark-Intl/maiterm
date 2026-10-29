<!--
  One asset large (docs/hive.md §10.4), in the gallery's own area, never over the coordinator.
  Real players: `<video>` with frame steps, `<audio>` over its waveform, an image with click and
  wheel zoom and drag to pan, a document's first page with Reveal (paging is deferred, plan D9;
  nothing here launches a stored file, whose extension an agent chose).
  Then the recipe, who made it and for which task (each a link to that agent's column: every
  result is one click from the agent that made it), and its lineage. Curation (step 8e): Star,
  Pick / Final / Reject (the coordinator's recommendation shown beside them under Ask me), Add to
  collection, Iterate (onto the coordinator's composer, recipe quoted), Send to worker…,
  Export…, and the trash.
-->
<script lang="ts">
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { galleryGet, revealInFileManager } from '$lib/tauri/commands';
  import type { GalleryAssetDetail, GalleryChip, HiveWorkerCard } from '$lib/tauri/types';
  import { fmtBytes, fmtDuration, galleryUrl, madeByLabel, picture } from '$lib/hive/gallery';
  import type { CurateActions } from '$lib/hive/galleryCurate';
  import { isMac, isWindows } from '$lib/utils/platform';

  const revealLabel = isMac() ? 'Reveal in Finder' : isWindows() ? 'Show in File Explorer' : 'Open Containing Folder';

  interface Props {
    workspaceId: string;
    hiveId: string;
    id: string;
    /** Moves on every gallery change: the detail reads its asset again. */
    version: number;
    workers: HiveWorkerCard[];
    hasPrev: boolean;
    hasNext: boolean;
    onback: () => void;
    onnavigate: (id: string) => void;
    onstep: (dir: -1 | 1) => void;
    onopenworker: (taskId: string) => void;
    onopencoordinator: () => void;
    /** The coordinator's finals recommendation on it, waiting on the human ("" no status). */
    recommendation?: string;
    actions: CurateActions;
  }
  let { workspaceId, hiveId, id, version, workers, hasPrev, hasNext, onback, onnavigate, onstep, onopenworker, onopencoordinator, recommendation, actions }: Props = $props();

  let asset = $state<GalleryAssetDetail | null>(null);
  let failed = $state('');
  $effect(() => {
    const want = id;
    const ws = workspaceId;
    void version;
    let alive = true;
    galleryGet(ws, want)
      .then((a) => { if (alive) { asset = a; failed = ''; } })
      .catch((e) => {
        if (!alive) return;
        failed = String(e).replace(/^Error: /, '');
        logError(`hive: asset ${want} not read: ${e}`);
      });
    return () => { alive = false; };
  });
  // A different asset starts unzoomed, at the start of its media.
  $effect(() => {
    void id;
    zoom = 1;
    pan = { x: 0, y: 0 };
    note = '';
  });

  const shown = $derived(asset && asset.id === id ? asset : null);
  const pic = $derived(shown ? picture(shown) : null);
  const gone = $derived(!!shown && (shown.purged || !!shown.deleted_at));
  const worker = $derived(shown?.task_id ? workers.find((w) => w.task_id === shown.task_id) ?? null : null);
  const recipeRows = $derived.by(() => {
    const r = shown?.recipe;
    if (!r) return [] as [string, string][];
    const rows: [string, string][] = [['Tool', r.tool]];
    if (r.model) rows.push(['Model', r.model]);
    if (r.seed != null) rows.push(['Seed', String(r.seed)]);
    if (r.negative) rows.push(['Negative', r.negative]);
    if (r.cost_usd != null) rows.push(['Cost', `$${r.cost_usd < 0.01 ? r.cost_usd.toFixed(3) : r.cost_usd.toFixed(2)}`]);
    if (r.source_url) rows.push(['Source', r.source_url]);
    if (r.params && typeof r.params === 'object' && !Array.isArray(r.params)) {
      for (const [k, v] of Object.entries(r.params as Record<string, unknown>)) rows.push([k, typeof v === 'string' ? v : JSON.stringify(v)]);
    } else if (r.params != null) {
      rows.push(['Params', JSON.stringify(r.params)]);
    }
    return rows;
  });

  let note = $state('');
  async function copy(text: string, what: string) {
    try {
      await clipboardWriteText(text);
      note = `${what} copied.`;
    } catch (e) {
      note = `Couldn't copy: ${e}`;
    }
  }
  async function reveal() {
    if (!shown) return;
    try {
      await revealInFileManager(shown.path);
    } catch (e) {
      note = String(e);
    }
  }

  // ── Players ──
  let videoEl = $state<HTMLVideoElement | null>(null);
  let audioEl = $state<HTMLAudioElement | null>(null);
  /** Space (GalleryView): play or pause whatever plays here. */
  export function togglePlay() {
    const m = videoEl ?? audioEl;
    if (!m) return;
    if (m.paused) void m.play();
    else m.pause();
  }
  const FRAME = 1 / 30;
  function stepFrame(dir: -1 | 1) {
    const v = videoEl;
    if (!v) return;
    v.pause();
    v.currentTime = Math.max(0, Math.min(v.duration || Infinity, v.currentTime + dir * FRAME));
  }

  // ── Image zoom and pan: a click toggles 1× and 2×, the wheel zooms about the pointer, a drag pans. ──
  let zoom = $state(1);
  let pan = $state({ x: 0, y: 0 });
  let dragged = false;
  function zoomAt(e: { clientX: number; clientY: number; currentTarget: EventTarget | null }, next: number) {
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    const cx = e.clientX - r.left - r.width / 2;
    const cy = e.clientY - r.top - r.height / 2;
    const z = Math.min(8, Math.max(1, next));
    // Keep the point under the pointer where it is.
    pan = z === 1 ? { x: 0, y: 0 } : { x: cx - ((cx - pan.x) * z) / zoom, y: cy - ((cy - pan.y) * z) / zoom };
    zoom = z;
  }
  function onWheel(e: WheelEvent) {
    e.preventDefault();
    zoomAt(e, zoom * Math.exp(-e.deltaY * 0.002));
  }
  function onImgDown(e: PointerEvent) {
    dragged = false;
    if (zoom === 1) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const start = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
    const move = (ev: PointerEvent) => {
      if (Math.abs(ev.clientX - start.x) + Math.abs(ev.clientY - start.y) > 3) dragged = true;
      pan = { x: start.px + ev.clientX - start.x, y: start.py + ev.clientY - start.y };
    };
    const up = () => {
      el.removeEventListener('pointermove', move);
      el.removeEventListener('pointerup', up);
      el.removeEventListener('pointercancel', up);
    };
    el.addEventListener('pointermove', move);
    el.addEventListener('pointerup', up);
    el.addEventListener('pointercancel', up);
  }
  function onImgClick(e: MouseEvent) {
    if (dragged) return;
    zoomAt(e, zoom === 1 ? 2 : 1);
  }

  const chipPic = (c: GalleryChip) => (c.picture ? galleryUrl(hiveId, c.id, c.picture) : null);
  const STATUSES: [string, string][] = [['picked', 'Pick'], ['final', 'Final'], ['rejected', 'Reject']];
  let collectBtn = $state<HTMLElement | null>(null);
  let sendBtn = $state<HTMLElement | null>(null);
</script>

<div class="detail">
  <div class="bar">
    <button type="button" class="back" onclick={onback}><Icon name="chevron" size={11} /> Grid</button>
    <span class="nav">
      <button type="button" disabled={!hasPrev} onclick={() => onstep(-1)} aria-label="Previous asset">←</button>
      <button type="button" disabled={!hasNext} onclick={() => onstep(1)} aria-label="Next asset">→</button>
    </span>
    {#if shown}<b class="t">{shown.title}</b>{/if}
  </div>

  {#if failed && !shown}
    <p class="hint">{failed}</p>
  {:else if !shown}
    <p class="hint">Reading the asset…</p>
  {:else}
    <div class="body">
      <div class="stage" data-kind={shown.kind}>
        {#if gone}
          <p class="hint">{shown.purged ? 'Emptied from the trash: only its record is left.' : 'In the trash.'}</p>
        {:else if shown.kind === 'image'}
          <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
          <div class="zoom" class:zoomed={zoom > 1} onwheel={onWheel} onpointerdown={onImgDown} onclick={onImgClick}>
            <img src={galleryUrl(hiveId, shown.id)} alt={shown.title} draggable="false" style="transform: translate({pan.x}px, {pan.y}px) scale({zoom})" />
          </div>
        {:else if shown.kind === 'video'}
          <!-- svelte-ignore a11y_media_has_caption -->
          <video bind:this={videoEl} src={galleryUrl(hiveId, shown.id)} poster={pic === 'thumb' ? galleryUrl(hiveId, shown.id, 'thumb') : undefined} controls playsinline></video>
          <div class="frames">
            <Tooltip text="Back one frame (1/30 s)"><button type="button" onclick={() => stepFrame(-1)} aria-label="Back one frame">⏮︎ frame</button></Tooltip>
            <Tooltip text="On one frame (1/30 s)"><button type="button" onclick={() => stepFrame(1)} aria-label="On one frame">frame ⏭︎</button></Tooltip>
          </div>
        {:else if shown.kind === 'audio'}
          {#if pic === 'wave'}<div class="wave" style="--wave: url('{galleryUrl(hiveId, shown.id, 'wave')}')" aria-hidden="true"></div>{/if}
          <audio bind:this={audioEl} src={galleryUrl(hiveId, shown.id)} controls></audio>
        {:else}
          {#if pic === 'thumb'}
            <img class="page" src={galleryUrl(hiveId, shown.id, 'thumb')} alt="{shown.title}: first page" draggable="false" />
          {:else}
            <div class="noprev"><Icon name={shown.mime === 'application/pdf' ? 'pdf' : 'file'} size={36} /><span>No preview</span></div>
          {/if}
          <!-- Reveal, never launch: an agent picks the stored file's extension (a .bat, a .terminal), and the default app would run it outside the sandbox as the human. -->
          <button type="button" class="act primary" onclick={reveal}>{revealLabel}</button>
        {/if}
      </div>

      <div class="side">
        <div class="acts">
          <Tooltip text={shown.starred ? 'Unstar (F)' : 'Star (F)'}>
            <button type="button" class="act" class:on={shown.starred} aria-pressed={shown.starred} disabled={shown.purged} onclick={() => actions.tag([shown.id], { starred: !shown.starred })}>★ Star</button>
          </Tooltip>
          {#each STATUSES as [s, label], k (s)}
            <Tooltip text="{label} ({k + 1}){shown.status === s ? ': again clears it' : ''}">
              <button type="button" class="act" class:on={shown.status === s} aria-pressed={shown.status === s} disabled={shown.purged} onclick={() => actions.tag([shown.id], { status: shown.status === s ? '' : s })}>{label}</button>
            </Tooltip>
          {/each}
        </div>
        {#if recommendation !== undefined && !gone}
          <p class="rec">The queen recommends <b>{recommendation || 'no status'}</b>: picking finals is yours in this Hive, and your pick answers it.</p>
          <!-- The buttons above toggle, so keeping the status it has needs its own answer. -->
          <Tooltip text="Answers the recommendation with the status it has now">
            <button type="button" class="act small" onclick={() => actions.tag([shown.id], { status: shown.status })}>Keep {shown.status || 'no status'}</button>
          </Tooltip>
        {/if}
        <div class="acts">
          {#if !gone}
            <Tooltip text="Onto the queen’s composer with its recipe quoted, for “three more like this”">
              <button type="button" class="act" onclick={() => actions.iterate(shown.id)}>Iterate</button>
            </Tooltip>
            <button type="button" class="act" bind:this={sendBtn} onclick={() => sendBtn && actions.pickWorker([shown.id], sendBtn)}>Send to worker…</button>
            <button type="button" class="act" bind:this={collectBtn} onclick={() => collectBtn && actions.pickCollection([shown.id], collectBtn)}>Add to collection…</button>
            <button type="button" class="act" onclick={() => actions.exportAssets([shown.id])}>Export…</button>
          {/if}
          <button type="button" class="act" onclick={reveal} disabled={gone}>{revealLabel}</button>
          <button type="button" class="act" onclick={() => shown && copy(shown.path, 'Path')} disabled={gone}>Copy path</button>
          {#if shown.deleted_at && !shown.purged}
            <button type="button" class="act" onclick={() => actions.restore([shown.id])}>Restore</button>
          {:else if !gone}
            <button type="button" class="act" onclick={() => actions.trash([shown.id])}>Move to trash</button>
          {/if}
        </div>
        {#if note}<p class="hint">{note}</p>{/if}

        <dl class="facts">
          <dt>Made by</dt>
          <dd>
            {#if shown.made_by.kind === 'worker' && shown.task_id}
              <button type="button" class="link" onclick={() => onopenworker(shown.task_id!)}>{madeByLabel(shown.made_by)}</button>
            {:else if shown.made_by.kind === 'coordinator'}
              <button type="button" class="link" onclick={onopencoordinator}>Queen</button>
            {:else}
              {madeByLabel(shown.made_by)}
            {/if}
          </dd>
          {#if shown.task_id}
            <dt>For</dt>
            <dd><button type="button" class="link" onclick={() => onopenworker(shown.task_id!)}>{worker?.title ?? shown.task_id}</button></dd>
          {/if}
          <dt>Filed</dt>
          <dd>{new Date(shown.created_ms || Date.parse(shown.created_at)).toLocaleString()}</dd>
          <dt>File</dt>
          <dd>{shown.kind}{shown.width && shown.height ? ` · ${shown.width}×${shown.height}` : ''}{shown.duration_ms ? ` · ${fmtDuration(shown.duration_ms)}` : ''} · {fmtBytes(shown.bytes)}</dd>
          {#if shown.tags.length}<dt>Tags</dt><dd>{shown.tags.join(', ')}</dd>{/if}
          {#if shown.status}<dt>Status</dt><dd>{shown.status}</dd>{/if}
          {#if shown.caption}<dt>Caption</dt><dd>{shown.caption}</dd>{/if}
        </dl>

        <h4>Recipe</h4>
        {#if shown.recipe}
          {#if shown.recipe.prompt}
            <div class="prompt">
              <p>{shown.recipe.prompt}</p>
              <button type="button" class="act small" onclick={() => copy(shown.recipe?.prompt ?? '', 'Prompt')}>Copy</button>
            </div>
          {/if}
          <dl class="kv">
            {#each recipeRows as [k, v] (k)}<dt>{k}</dt><dd>{v}</dd>{/each}
          </dl>
        {:else}
          <p class="hint">No recipe was filed with it, so it can't be made again as it was.</p>
        {/if}

        {#if shown.parent_chips.length || shown.child_chips.length}
          <h4>Lineage</h4>
          <div class="lineage">
            {#each shown.parent_chips as c (c.id)}
              <button type="button" class="chip" onclick={() => onnavigate(c.id)} aria-label="Parent: {c.title}">
                {#if chipPic(c)}<img src={chipPic(c)} alt="" draggable="false" />{:else}<Icon name="file" size={14} />{/if}
              </button>
            {/each}
            {#if shown.parent_chips.length}<span class="arrow" aria-hidden="true">←</span>{/if}
            <span class="chip me" aria-label="This asset">
              {#if pic === 'thumb'}<img src={galleryUrl(hiveId, shown.id, 'thumb')} alt="" draggable="false" />{:else}<Icon name="file" size={14} />{/if}
            </span>
            {#if shown.child_chips.length}<span class="arrow" aria-hidden="true">→</span>{/if}
            {#each shown.child_chips as c (c.id)}
              <button type="button" class="chip" onclick={() => onnavigate(c.id)} aria-label="Variant: {c.title}">
                {#if chipPic(c)}<img src={chipPic(c)} alt="" draggable="false" />{:else}<Icon name="file" size={14} />{/if}
              </button>
            {/each}
          </div>
        {/if}
      </div>
    </div>
  {/if}
</div>

<style>
  .detail { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .bar { display: flex; align-items: center; gap: 10px; padding: 8px 12px; border-bottom: 1px solid var(--bg-light); min-width: 0; }
  .back { display: inline-flex; align-items: center; gap: 4px; background: none; border: none; color: var(--accent); font: inherit; font-size: 12px; cursor: pointer; padding: 0; }
  .back :global(svg) { transform: rotate(90deg); }
  .nav { display: flex; gap: 4px; }
  .nav button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 0 7px; font: inherit; cursor: pointer; }
  .nav button:disabled { opacity: 0.4; cursor: default; }
  .t { font-size: 13px; overflow-wrap: anywhere; min-width: 0; }
  .body { flex: 1; min-height: 0; display: flex; flex-wrap: wrap; overflow-y: auto; }
  .stage { flex: 1 1 420px; min-width: 0; min-height: 280px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 10px; padding: 14px; background: color-mix(in srgb, var(--bg-dark) 60%, #000); }
  .zoom { width: 100%; height: 100%; min-height: 260px; overflow: hidden; display: flex; align-items: center; justify-content: center; cursor: zoom-in; touch-action: none; }
  .zoom.zoomed { cursor: grab; }
  .zoom img { max-width: 100%; max-height: 70vh; object-fit: contain; transform-origin: center; user-select: none; }
  video { max-width: 100%; max-height: 70vh; background: #000; }
  .frames { display: flex; gap: 6px; }
  .frames button { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 2px 8px; font: inherit; font-size: 11px; cursor: pointer; }
  .wave { width: 100%; max-width: 720px; height: 140px; background: var(--accent); -webkit-mask: var(--wave) center / 100% 100% no-repeat; mask: var(--wave) center / 100% 100% no-repeat; }
  audio { width: 100%; max-width: 720px; }
  .page { max-width: 100%; max-height: 60vh; object-fit: contain; border-radius: 4px; background: #fff; }
  .noprev { display: flex; flex-direction: column; align-items: center; gap: 6px; color: var(--fg-dim); font-size: 12px; }
  .side { flex: 1 1 260px; max-width: 420px; min-width: 0; padding: 12px 14px; display: flex; flex-direction: column; gap: 10px; font-size: 12px; }
  .acts { display: flex; flex-wrap: wrap; gap: 5px; }
  .act { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 6px; padding: 3px 9px; font: inherit; font-size: 11.5px; cursor: pointer; }
  .act:disabled { opacity: 0.45; cursor: default; }
  .act.on { border-color: var(--accent); color: var(--accent); }
  .rec { margin: 0; font-size: 11.5px; color: var(--yellow); }
  .act.primary { background: var(--accent); color: var(--bg-dark); border-color: var(--accent); font-weight: 600; }
  .act.small { padding: 1px 7px; font-size: 11px; align-self: flex-start; }
  .facts, .kv { margin: 0; display: grid; grid-template-columns: max-content 1fr; gap: 3px 10px; }
  dt { color: var(--fg-dim); }
  dd { margin: 0; overflow-wrap: anywhere; min-width: 0; }
  h4 { margin: 4px 0 0; font-size: 10px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  .prompt { display: flex; flex-direction: column; gap: 4px; background: var(--bg-medium); border-radius: 6px; padding: 8px 10px; }
  .prompt p { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; -webkit-user-select: text; user-select: text; }
  .kv dd { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; -webkit-user-select: text; user-select: text; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font: inherit; cursor: pointer; text-align: left; overflow-wrap: anywhere; }
  .link:hover { text-decoration: underline; }
  .lineage { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; }
  .chip { width: 44px; height: 44px; border-radius: 5px; overflow: hidden; border: 1px solid var(--bg-light); background: var(--bg-medium); display: flex; align-items: center; justify-content: center; padding: 0; cursor: pointer; color: var(--fg-dim); }
  .chip img { width: 100%; height: 100%; object-fit: cover; }
  .chip.me { border-color: var(--accent); cursor: default; }
  .arrow { color: var(--fg-dim); }
  .hint { margin: 0; padding: 4px 0; font-size: 11.5px; color: var(--fg-dim); }
  .detail > .hint { padding: 16px; }
  button:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
