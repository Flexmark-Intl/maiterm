<!--
  The gallery's viewer (docs/hive.md §10.4): one asset as large as the gallery's area allows,
  over the whole gallery (never over the coordinator). It is both the detail view and the Space
  preview: Space, Enter or a double-click opens it; Space, Escape or × closes it; ← and → step
  through the grid's order.

  - An image fits the stage (never blown up past its own size). A click or 100% shows it pixel
    for pixel, the wheel zooms about the pointer, a drag pans when it's larger than the stage.
  - A video plays with frame steps, audio over its waveform, a document shows its first page with
    Reveal (paging is deferred, plan D9; nothing here launches a stored file, whose extension an
    agent chose).
  - Star and Pick / Final / Reject sit in the bar; every other action is in More… and the
    right-click menu, the same menu the grid uses.
  - Info (the i key) is a drawer over the stage's side, never shrinking the media: who made it
    and for which task (each a link to that agent's column), the recipe, lineage, and the queen's
    recommendation.
-->
<script lang="ts">
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { galleryGet, revealInFileManager } from '$lib/tauri/commands';
  import type { GalleryAssetDetail, GalleryChip, GalleryTagChange, HiveWorkerCard } from '$lib/tauri/types';
  import { clampPan, fitScale, fmtBytes, fmtDuration, galleryUrl, madeByLabel, picture, zoomAbout, ZOOM_MAX } from '$lib/hive/gallery';
  import { isMac, isWindows } from '$lib/utils/platform';

  const revealLabel = isMac() ? 'Reveal in Finder' : isWindows() ? 'Show in File Explorer' : 'Open Containing Folder';

  interface Props {
    workspaceId: string;
    hiveId: string;
    id: string;
    /** Moves on every gallery change: the viewer reads its asset again. */
    version: number;
    workers: HiveWorkerCard[];
    /** Its place in the grid's order (1-based; 0 when it isn't listed) and the order's length. */
    position: number;
    total: number;
    hasPrev: boolean;
    hasNext: boolean;
    /** The info drawer is open (remembered for the session by the gallery). */
    info: boolean;
    oninfo: () => void;
    onclose: () => void;
    onnavigate: (id: string) => void;
    onstep: (dir: -1 | 1) => void;
    onopenworker: (taskId: string) => void;
    onopencoordinator: () => void;
    /** The selection's menu, at a point on screen. */
    onmenu: (x: number, y: number, anchor?: HTMLElement) => void;
    ontag: (ids: string[], change: GalleryTagChange) => void;
    /** The coordinator's finals recommendation on it, waiting on the human ("" no status). */
    recommendation?: string;
  }
  let {
    workspaceId,
    hiveId,
    id,
    version,
    workers,
    position,
    total,
    hasPrev,
    hasNext,
    info,
    oninfo,
    onclose,
    onnavigate,
    onstep,
    onopenworker,
    onopencoordinator,
    onmenu,
    ontag,
    recommendation,
  }: Props = $props();

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
  // A different asset starts fitted, at the start of its media.
  $effect(() => {
    void id;
    zoom = null;
    pan = { x: 0, y: 0 };
    nat = { w: 0, h: 0 };
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
  const FRAME = 1 / 30;
  function stepFrame(dir: -1 | 1) {
    const v = videoEl;
    if (!v) return;
    v.pause();
    v.currentTime = Math.max(0, Math.min(v.duration || Infinity, v.currentTime + dir * FRAME));
  }

  // ── The image: fitted, or zoomed (null is fitted) about the pointer, and panned ──
  const MARGIN = 24;
  let sw = $state(0);
  let sh = $state(0);
  let nat = $state({ w: 0, h: 0 });
  let zoom = $state<number | null>(null);
  let pan = $state({ x: 0, y: 0 });
  /** The picture's own size: read off the loaded file, the record's until then. */
  const img = $derived(nat.w > 0 ? nat : { w: shown?.width ?? 0, h: shown?.height ?? 0 });
  const room = $derived({ w: Math.max(1, sw - MARGIN), h: Math.max(1, sh - MARGIN) });
  const fit = $derived(fitScale(img, room));
  const scale = $derived(zoom ?? fit);
  const zoomed = $derived(zoom !== null && zoom > fit + 1e-3);
  const shownPan = $derived(clampPan(pan, img, scale, { w: sw, h: sh }));
  let dragged = false;

  function zoomTo(next: number | null, at = { x: 0, y: 0 }) {
    const to = next === null ? fit : Math.min(ZOOM_MAX, Math.max(fit, next));
    if (next === null || to <= fit + 1e-3) {
      zoom = null;
      pan = { x: 0, y: 0 };
      return;
    }
    pan = clampPan(zoomAbout(shownPan, at, scale, to), img, to, { w: sw, h: sh });
    zoom = to;
  }
  function pointAt(e: { clientX: number; clientY: number; currentTarget: EventTarget | null }) {
    const r = (e.currentTarget as HTMLElement).getBoundingClientRect();
    return { x: e.clientX - r.left - r.width / 2, y: e.clientY - r.top - r.height / 2 };
  }
  function onWheel(e: WheelEvent) {
    e.preventDefault();
    zoomTo(scale * Math.exp(-e.deltaY * (e.ctrlKey ? 0.01 : 0.002)), pointAt(e));
  }
  function onImgDown(e: PointerEvent) {
    dragged = false;
    if (e.button !== 0 || !zoomed) return;
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const start = { x: e.clientX, y: e.clientY, px: shownPan.x, py: shownPan.y };
    const move = (ev: PointerEvent) => {
      if (Math.abs(ev.clientX - start.x) + Math.abs(ev.clientY - start.y) > 3) dragged = true;
      pan = clampPan({ x: start.px + ev.clientX - start.x, y: start.py + ev.clientY - start.y }, img, scale, { w: sw, h: sh });
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
  /** A click shows it pixel for pixel (twice that when it already fits at its own size), and
   *  back to fitted. */
  function onImgClick(e: MouseEvent) {
    if (dragged) return;
    zoomTo(zoomed ? null : fit < 1 ? 1 : 2, pointAt(e));
  }

  function onMenu(e: MouseEvent) {
    e.preventDefault();
    onmenu(e.clientX, e.clientY);
  }

  const chipPic = (c: GalleryChip) => (c.picture ? galleryUrl(hiveId, c.id, c.picture) : null);
  const STATUSES: [string, string][] = [['picked', 'Pick'], ['final', 'Final'], ['rejected', 'Reject']];
  let moreBtn = $state<HTMLButtonElement | null>(null);
  /** The asset on show, for a menu on one the grid doesn't list. */
  export function current(): GalleryAssetDetail | null {
    return shown;
  }
  /** The More… button, where the keyboard's menu opens. */
  export function menuAnchor(): HTMLElement | null {
    return moreBtn;
  }
  /** A mouse press on a bar button leaves the keyboard on the gallery: Space then closes the
   *  viewer instead of pressing the button again. */
  const noFocus = (e: MouseEvent) => e.preventDefault();
</script>

<div class="viewer" role="dialog" aria-label={shown ? shown.title : 'Asset'}>
  <div class="bar">
    <IconButton tooltip="Close (Esc or Space)" onclick={onclose} onmousedown={noFocus}><Icon name="close" size={14} /></IconButton>
    <span class="nav">
      <IconButton tooltip="Previous (←)" disabled={!hasPrev} onclick={() => onstep(-1)} onmousedown={noFocus}><span class="flip"><Icon name="chevron" size={13} /></span></IconButton>
      <IconButton tooltip="Next (→)" disabled={!hasNext} onclick={() => onstep(1)} onmousedown={noFocus}><Icon name="chevron" size={13} /></IconButton>
    </span>
    <span class="title">
      {#if shown}<Tooltip text={shown.title} block><b class="t">{shown.title}</b></Tooltip>{/if}
      {#if position > 0}<span class="pos">{position} of {total}</span>{/if}
    </span>
    {#if shown && !shown.purged}
      <span class="group" role="group" aria-label="Curate">
        <Tooltip text={shown.starred ? 'Unstar (F)' : 'Star (F)'}>
          <button type="button" class="tb star" class:on={shown.starred} aria-pressed={shown.starred} onmousedown={noFocus} onclick={() => ontag([shown.id], { starred: !shown.starred })}>★</button>
        </Tooltip>
        {#each STATUSES as [s, label], k (s)}
          <Tooltip text="{label} ({k + 1}){shown.status === s ? ': again clears it' : ''}">
            <button type="button" class="tb" class:on={shown.status === s} aria-pressed={shown.status === s} onmousedown={noFocus} onclick={() => ontag([shown.id], { status: shown.status === s ? '' : s })}>{label}</button>
          </Tooltip>
        {/each}
      </span>
    {/if}
    {#if shown && !gone && shown.kind === 'image'}
      <span class="group" role="group" aria-label="Zoom">
        <Tooltip text="Fit the space">
          <button type="button" class="tb" class:on={!zoomed} aria-pressed={!zoomed} onmousedown={noFocus} onclick={() => zoomTo(null)}>Fit</button>
        </Tooltip>
        <Tooltip text="Pixel for pixel (or click the image)">
          <button type="button" class="tb" class:on={zoomed && Math.abs(scale - 1) < 1e-3} onmousedown={noFocus} onclick={() => zoomTo(1)}>100%</button>
        </Tooltip>
        <span class="pct" aria-live="polite">{Math.round(scale * 100)}%</span>
      </span>
    {:else if shown && !gone && shown.kind === 'video'}
      <span class="group" role="group" aria-label="Frames">
        <Tooltip text="Back one frame (1/30 s)"><button type="button" class="tb" onmousedown={noFocus} onclick={() => stepFrame(-1)} aria-label="Back one frame">⏮︎</button></Tooltip>
        <Tooltip text="On one frame (1/30 s)"><button type="button" class="tb" onmousedown={noFocus} onclick={() => stepFrame(1)} aria-label="On one frame">⏭︎</button></Tooltip>
      </span>
    {/if}
    <span class="end">
      <Tooltip text="Every action on it (or right-click)">
        <button type="button" class="tb" bind:this={moreBtn} onmousedown={noFocus} onclick={() => moreBtn && onmenu(moreBtn.getBoundingClientRect().left, moreBtn.getBoundingClientRect().bottom + 4, moreBtn)}>More…</button>
      </Tooltip>
      <IconButton tooltip={info ? 'Hide info (I)' : 'Info: who made it, the recipe, lineage (I)'} active={info} aria-pressed={info} onclick={oninfo} onmousedown={noFocus}>
        <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" aria-hidden="true"><circle cx="8" cy="8" r="6.3" /><path d="M8 7.2v4" stroke-linecap="round" /><circle cx="8" cy="4.9" r="0.4" fill="currentColor" /></svg>
      </IconButton>
    </span>
  </div>

  <div class="body">
    {#if failed && !shown}
      <p class="hint center">{failed}</p>
    {:else if !shown}
      <p class="hint center">Reading the asset…</p>
    {:else}
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="stage" data-kind={shown.kind} bind:clientWidth={sw} bind:clientHeight={sh} oncontextmenu={onMenu}>
        {#if gone}
          <p class="hint center">{shown.purged ? 'Emptied from the trash: only its record is left.' : 'In the trash. Restore it from More… to use it again.'}</p>
        {:else if shown.kind === 'image'}
          <!-- svelte-ignore a11y_click_events_have_key_events -->
          <div class="zoom" class:zoomed class:canzoom={!zoomed} onwheel={onWheel} onpointerdown={onImgDown} onclick={onImgClick}>
            <img
              src={galleryUrl(hiveId, shown.id)}
              alt={shown.title}
              draggable="false"
              class:ready={img.w > 0}
              onload={(e) => { const el = e.currentTarget as HTMLImageElement; nat = { w: el.naturalWidth, h: el.naturalHeight }; }}
              style="width: {img.w * scale}px; height: {img.h * scale}px; transform: translate(calc(-50% + {shownPan.x}px), calc(-50% + {shownPan.y}px))"
            />
          </div>
        {:else if shown.kind === 'video'}
          <!-- svelte-ignore a11y_media_has_caption -->
          <video class="fill" bind:this={videoEl} src={galleryUrl(hiveId, shown.id)} poster={pic === 'thumb' ? galleryUrl(hiveId, shown.id, 'thumb') : undefined} controls playsinline></video>
        {:else if shown.kind === 'audio'}
          <div class="audio">
            {#if pic === 'wave'}<div class="wave" style="--wave: url('{galleryUrl(hiveId, shown.id, 'wave')}')" aria-hidden="true"></div>{/if}
            <audio src={galleryUrl(hiveId, shown.id)} controls></audio>
          </div>
        {:else}
          <div class="doc">
            {#if pic === 'thumb'}
              <img class="page" src={galleryUrl(hiveId, shown.id, 'thumb')} alt="{shown.title}: first page" draggable="false" />
            {:else}
              <div class="noprev"><Icon name={shown.mime === 'application/pdf' ? 'pdf' : 'file'} size={44} /><span>No preview</span></div>
            {/if}
            <!-- Reveal, never launch: an agent picks the stored file's extension (a .bat, a .terminal), and the default app would run it outside the sandbox as the human. -->
            <button type="button" class="primary" onclick={reveal}>{revealLabel}</button>
          </div>
        {/if}
      </div>

      {#if info}
        <aside class="info" aria-label="Info">
          {#if recommendation !== undefined && !gone}
            <div class="rec">
              <p>The queen recommends <b>{recommendation || 'no status'}</b>: picking finals is yours in this Hive, and your pick answers it.</p>
              <!-- The status buttons toggle, so keeping the status it has needs its own answer. -->
              <Tooltip text="Answers the recommendation with the status it has now">
                <button type="button" class="act" onclick={() => ontag([shown.id], { status: shown.status })}>Keep {shown.status || 'no status'}</button>
              </Tooltip>
            </div>
          {/if}

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
            <dd>{shown.kind}{shown.width && shown.height ? `, ${shown.width}×${shown.height}` : ''}{shown.duration_ms ? `, ${fmtDuration(shown.duration_ms)}` : ''}, {fmtBytes(shown.bytes)}</dd>
            {#if shown.tags.length}<dt>Tags</dt><dd>{shown.tags.join(', ')}</dd>{/if}
            {#if shown.status}<dt>Status</dt><dd>{shown.status}</dd>{/if}
            {#if shown.caption}<dt>Caption</dt><dd>{shown.caption}</dd>{/if}
            <dt>Path</dt>
            <dd class="path">
              <span>{shown.path}</span>
              <span class="row">
                <button type="button" class="act" onclick={() => copy(shown.path, 'Path')} disabled={gone}>Copy path</button>
                <button type="button" class="act" onclick={reveal} disabled={gone}>{revealLabel}</button>
              </span>
            </dd>
          </dl>
          {#if note}<p class="hint">{note}</p>{/if}

          <h4>Recipe</h4>
          {#if shown.recipe}
            {#if shown.recipe.prompt}
              <div class="prompt">
                <p>{shown.recipe.prompt}</p>
                <button type="button" class="act" onclick={() => copy(shown.recipe?.prompt ?? '', 'Prompt')}>Copy prompt</button>
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
                <Tooltip text="Parent: {c.title}">
                  <button type="button" class="chip" onclick={() => onnavigate(c.id)} aria-label="Parent: {c.title}">
                    {#if chipPic(c)}<img src={chipPic(c)} alt="" draggable="false" />{:else}<Icon name="file" size={14} />{/if}
                  </button>
                </Tooltip>
              {/each}
              {#if shown.parent_chips.length}<span class="arrow" aria-hidden="true">←</span>{/if}
              <span class="chip me" aria-label="This asset">
                {#if pic === 'thumb'}<img src={galleryUrl(hiveId, shown.id, 'thumb')} alt="" draggable="false" />{:else}<Icon name="file" size={14} />{/if}
              </span>
              {#if shown.child_chips.length}<span class="arrow" aria-hidden="true">→</span>{/if}
              {#each shown.child_chips as c (c.id)}
                <Tooltip text="Variant: {c.title}">
                  <button type="button" class="chip" onclick={() => onnavigate(c.id)} aria-label="Variant: {c.title}">
                    {#if chipPic(c)}<img src={chipPic(c)} alt="" draggable="false" />{:else}<Icon name="file" size={14} />{/if}
                  </button>
                </Tooltip>
              {/each}
            </div>
          {/if}
        </aside>
      {/if}
    {/if}
  </div>
</div>

<style>
  .viewer { position: absolute; inset: 0; z-index: 20; display: flex; flex-direction: column; min-width: 0; min-height: 0; background: var(--bg-dark); container-type: inline-size; }
  .bar { flex: none; display: flex; flex-wrap: wrap; align-items: center; gap: 6px 10px; padding: 6px 10px; border-bottom: 1px solid var(--bg-light); min-width: 0; font-size: 11.5px; }
  .nav { display: flex; gap: 2px; }
  .flip { display: inline-flex; transform: rotate(180deg); }
  .title { flex: 1 1 160px; min-width: 0; display: flex; align-items: baseline; gap: 8px; }
  .title > :global(.tooltip-wrapper) { min-width: 0; flex: 0 1 auto; width: auto; }
  .t { display: block; font-size: 12.5px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .pos { flex: none; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .group { display: inline-flex; align-items: center; gap: 3px; }
  .end { display: inline-flex; align-items: center; gap: 4px; margin-left: auto; }
  .tb { height: 24px; box-sizing: border-box; background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 5px; padding: 0 8px; font: inherit; font-size: 11.5px; cursor: pointer; white-space: nowrap; }
  .tb:hover { color: var(--fg); }
  .tb.on { color: var(--fg); border-color: var(--accent); background: color-mix(in srgb, var(--accent) 14%, var(--bg-medium)); }
  .tb.star.on { color: var(--yellow); }
  .pct { min-width: 38px; text-align: right; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .body { position: relative; flex: 1; min-height: 0; display: flex; }
  .stage { flex: 1; min-width: 0; min-height: 0; position: relative; overflow: hidden; background: color-mix(in srgb, var(--bg-dark) 55%, #000); }
  .zoom { position: absolute; inset: 0; overflow: hidden; touch-action: none; }
  .zoom.canzoom { cursor: zoom-in; }
  .zoom.zoomed { cursor: grab; }
  .zoom.zoomed:active { cursor: grabbing; }
  .zoom img { position: absolute; left: 50%; top: 50%; max-width: none; user-select: none; -webkit-user-select: none; opacity: 0; }
  .zoom img.ready { opacity: 1; }
  .fill { position: absolute; inset: 12px; width: calc(100% - 24px); height: calc(100% - 24px); object-fit: contain; background: #000; }
  .audio { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 16px; padding: 24px; box-sizing: border-box; }
  .wave { width: 100%; max-width: 1400px; height: min(50%, 320px); background: var(--accent); -webkit-mask: var(--wave) center / 100% 100% no-repeat; mask: var(--wave) center / 100% 100% no-repeat; }
  .audio audio { width: min(100%, 900px); }
  .doc { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 12px; padding: 16px; box-sizing: border-box; }
  .page { min-height: 0; max-width: 100%; max-height: calc(100% - 44px); object-fit: contain; border-radius: 4px; background: #fff; }
  .noprev { display: flex; flex-direction: column; align-items: center; gap: 8px; color: var(--fg-dim); font-size: 12px; }
  .primary { background: var(--accent); color: var(--bg-dark); border: 1px solid var(--accent); border-radius: 6px; padding: 4px 12px; font: inherit; font-size: 12px; font-weight: 600; cursor: pointer; }
  .info {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    z-index: 2;
    width: min(340px, 88%);
    box-sizing: border-box;
    overflow-y: auto;
    padding: 12px 14px 16px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    font-size: 12px;
    background: color-mix(in srgb, var(--bg-dark) 94%, transparent);
    border-left: 1px solid var(--bg-light);
    box-shadow: -8px 0 24px rgba(0, 0, 0, 0.35);
  }
  .rec { display: flex; flex-direction: column; gap: 6px; align-items: flex-start; }
  .rec p { margin: 0; font-size: 11.5px; color: var(--yellow); }
  .act { background: var(--bg-medium); border: 1px solid var(--bg-light); color: var(--fg); border-radius: 5px; padding: 1px 8px; font: inherit; font-size: 11px; cursor: pointer; }
  .act:disabled { opacity: 0.45; cursor: default; }
  .facts, .kv { margin: 0; display: grid; grid-template-columns: max-content 1fr; gap: 4px 10px; }
  dt { color: var(--fg-dim); }
  dd { margin: 0; overflow-wrap: anywhere; min-width: 0; -webkit-user-select: text; user-select: text; }
  .path { display: flex; flex-direction: column; gap: 4px; font-size: 11px; color: var(--fg-dim); }
  .row { display: flex; flex-wrap: wrap; gap: 4px; }
  h4 { margin: 4px 0 0; font-size: 11.5px; font-weight: 600; color: var(--fg); }
  .prompt { display: flex; flex-direction: column; align-items: flex-start; gap: 6px; background: var(--bg-medium); border-radius: 6px; padding: 8px 10px; }
  .prompt p { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; -webkit-user-select: text; user-select: text; }
  .kv dd { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font: inherit; cursor: pointer; text-align: left; overflow-wrap: anywhere; }
  .link:hover { text-decoration: underline; }
  .lineage { display: flex; flex-wrap: wrap; align-items: center; gap: 5px; }
  .chip { width: 44px; height: 44px; border-radius: 5px; overflow: hidden; border: 1px solid var(--bg-light); background: var(--bg-medium); display: flex; align-items: center; justify-content: center; padding: 0; cursor: pointer; color: var(--fg-dim); }
  .chip img { width: 100%; height: 100%; object-fit: cover; }
  .chip.me { border-color: var(--accent); cursor: default; }
  .arrow { color: var(--fg-dim); }
  .hint { margin: 0; font-size: 11.5px; color: var(--fg-dim); }
  .hint.center { margin: auto; padding: 24px; text-align: center; max-width: 380px; }
  .stage > .hint.center { position: absolute; inset: 0; display: flex; align-items: center; justify-content: center; max-width: none; }
  button:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
  @container (max-width: 560px) {
    .pct, .pos { display: none; }
  }
</style>
