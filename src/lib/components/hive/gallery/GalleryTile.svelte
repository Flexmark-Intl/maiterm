<!--
  One asset in the gallery grid (docs/hive.md §10.4). It fills the box the grid placed it in, at
  its own aspect ratio:

  - an image: its thumbnail;
  - a video: its poster; hovering scrubs through its ten-frame strip by the pointer's x, and
    resting the pointer 400 ms plays it muted from the stored file;
  - audio: its waveform and duration, with a play button that plays it in place;
  - a document or 3D file: its first-page thumbnail and a type badge;
  - anything without a picture yet (pending, failed, no tool): the kind's icon and its title.

  Badges: star, final ✓, rejected (dimmed), and a stacked edge with "⧉ n variants" where it has
  children. Every picture comes through `gallery://` by asset id, never a path.
-->
<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { fmtDuration, galleryUrl, hasStrip, madeByLabel, picture, typeBadge } from '$lib/hive/gallery';
  import type { GalleryAssetRow } from '$lib/tauri/types';

  interface Props {
    hiveId: string;
    row: GalleryAssetRow;
    selected: boolean;
    flashing: boolean;
    /** The coordinator's finals recommendation waiting on the human ("" no status). */
    recommended?: string;
    onselect: (e: MouseEvent) => void;
    onopen: () => void;
  }
  let { hiveId, row, selected, flashing, recommended, onselect, onopen }: Props = $props();

  const pic = $derived(picture(row));
  /** A picture that 404s (a thumbnail swept, an asset trashed meanwhile) falls back to the icon. */
  let broken = $state(false);
  $effect(() => {
    void row.id;
    void pic;
    broken = false;
  });
  const strip = $derived(row.kind === 'video' && hasStrip(row));
  const duration = $derived(fmtDuration(row.duration_ms));
  const trashed = $derived(!!row.deleted_at);
  const tip = $derived(
    `${row.title} · ${madeByLabel(row.made_by)}${row.prompt_head ? `\n${row.prompt_head}` : ''}${trashed ? '\nIn the trash' : ''}${
      recommended !== undefined ? `\nThe coordinator recommends: ${recommended || 'no status'}` : ''
    }`,
  );

  // ── Video: scrub on hover, play muted after resting 400 ms ──
  let frame = $state<number | null>(null);
  let playing = $state(false);
  let rest: ReturnType<typeof setTimeout> | null = null;
  let last = { x: 0, y: 0 };
  function onMove(e: PointerEvent) {
    if (row.kind !== 'video') return;
    const el = e.currentTarget as HTMLElement;
    const r = el.getBoundingClientRect();
    if (strip) frame = Math.min(9, Math.max(0, Math.floor(((e.clientX - r.left) / Math.max(1, r.width)) * 10)));
    // A pointer that moves stops playback and scrubs again; one that rests plays.
    if (Math.abs(e.clientX - last.x) + Math.abs(e.clientY - last.y) > 4) {
      last = { x: e.clientX, y: e.clientY };
      playing = false;
      if (rest) clearTimeout(rest);
      rest = setTimeout(() => (playing = true), 400);
    }
  }
  function onLeave() {
    frame = null;
    playing = false;
    if (rest) clearTimeout(rest);
    rest = null;
  }
  $effect(() => () => { if (rest) clearTimeout(rest); });

  // ── Audio: plays in place ──
  let audioEl = $state<HTMLAudioElement | null>(null);
  let audioOn = $state(false);
  function onAudioClick(e: MouseEvent) {
    e.stopPropagation();
    toggleAudio();
  }
  function toggleAudio() {
    if (!audioOn) {
      audioOn = true;
      requestAnimationFrame(() => void audioEl?.play().catch(() => (audioOn = false)));
    } else if (audioEl?.paused) {
      void audioEl.play();
    } else {
      audioEl?.pause();
    }
  }
  let audioPaused = $state(true);

  /** Space on the grid's cursor (GalleryView): audio plays in place, a video plays muted as on
   *  a resting pointer. False for a tile with nothing to play. */
  export function togglePlay(): boolean {
    if (row.kind === 'audio') {
      toggleAudio();
      return true;
    }
    if (row.kind === 'video') {
      if (rest) clearTimeout(rest);
      rest = null;
      playing = !playing;
      return true;
    }
    return false;
  }
</script>

<Tooltip text={tip} block>
  <!-- A listbox option: the grid owns the keyboard (arrows, Enter), so the tile isn't a tab stop. -->
  <div
    class="tile"
    class:selected
    class:flash={flashing}
    class:rejected={row.status === 'rejected'}
    class:trashed
    class:stacked={row.children > 0}
    data-kind={row.kind}
    data-asset-id={row.id}
    role="option"
    tabindex="-1"
    aria-selected={selected}
    aria-label="{row.title}, {row.kind}{row.status ? `, ${row.status}` : ''}{row.starred ? ', starred' : ''}"
    onclick={onselect}
    ondblclick={onopen}
    onkeydown={(e) => { if (e.key === 'Enter') onopen(); }}
    onpointermove={onMove}
    onpointerleave={onLeave}
  >
    {#if pic === 'thumb' && !broken}
      <img class="pic" src={galleryUrl(hiveId, row.id, 'thumb')} alt="" draggable="false" loading="lazy" onerror={() => (broken = true)} />
    {:else if pic === 'wave' && !broken}
      <div class="wave" style="--wave: url('{galleryUrl(hiveId, row.id, 'wave')}')" aria-hidden="true"></div>
    {:else}
      <div class="icon" aria-hidden="true">
        <Icon name={row.kind === 'image' ? 'image' : row.mime === 'application/pdf' ? 'pdf' : 'file'} size={22} />
        <span class="ititle">{row.title}</span>
        {#if row.thumb === 'pending'}<span class="pending">making a preview…</span>{/if}
      </div>
    {/if}
    {#if row.kind === 'video' && frame !== null && strip && !playing}
      <div
        class="strip"
        style="background-image: url('{galleryUrl(hiveId, row.id, 'strip')}'); background-position: {(frame * 100) / 9}% 0"
        aria-hidden="true"
      ></div>
    {/if}
    {#if row.kind === 'video' && playing}
      <!-- svelte-ignore a11y_media_has_caption -->
      <video class="play" src={galleryUrl(hiveId, row.id)} muted autoplay loop playsinline></video>
    {/if}
    {#if row.kind === 'audio'}
      <button class="aplay" aria-label={audioOn && !audioPaused ? 'Pause' : 'Play'} onclick={onAudioClick} ondblclick={(e) => e.stopPropagation()}>
        <Icon name={audioOn && !audioPaused ? 'pause' : 'resume'} size={12} />
      </button>
      {#if audioOn}
        <audio bind:this={audioEl} src={galleryUrl(hiveId, row.id)} onplay={() => (audioPaused = false)} onpause={() => (audioPaused = true)} onended={() => (audioPaused = true)}></audio>
      {/if}
    {/if}
    <span class="badges">
      {#if row.starred}<span class="b star" aria-hidden="true">★</span>{/if}
      {#if row.status === 'final'}<span class="b final">final ✓</span>{/if}
      {#if row.status === 'picked'}<span class="b picked">picked</span>{/if}
      {#if row.status === 'rejected'}<span class="b rej">rejected</span>{/if}
      {#if recommended !== undefined}<span class="b rec">→ {recommended || 'no status'}?</span>{/if}
    </span>
    <span class="corner">
      {#if (row.kind === 'video' || row.kind === 'audio') && duration}<span class="b dur">{row.kind === 'video' ? '▶ ' : ''}{duration}</span>{/if}
      {#if row.kind === 'document' || row.kind === 'model3d' || row.kind === 'other'}<span class="b type">{typeBadge(row.file)}</span>{/if}
    </span>
    {#if row.children > 0}<span class="variants">⧉ {row.children} variant{row.children === 1 ? '' : 's'}</span>{/if}
    {#if trashed}<span class="b trash-b">In the trash</span>{/if}
  </div>
</Tooltip>

<style>
  .tile {
    position: relative;
    width: 100%;
    height: 100%;
    border-radius: 6px;
    overflow: hidden;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    cursor: pointer;
    box-sizing: border-box;
    outline: none;
    /* Shift-click extends the selection, never a text selection; a press may become a drag. */
    -webkit-user-select: none;
    user-select: none;
  }
  .tile.selected { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .tile.flash { animation: flash 1.2s ease-out; }
  @keyframes flash { from { box-shadow: 0 0 0 3px var(--accent); } to { box-shadow: 0 0 0 0 transparent; } }
  .tile.rejected .pic, .tile.rejected .wave, .tile.trashed .pic { opacity: 0.35; filter: grayscale(0.7); }
  /* §10.4: an asset with children carries a stacked edge. */
  .tile.stacked { box-shadow: 3px 3px 0 -1px var(--bg-medium), 3px 3px 0 0 var(--bg-light); }
  .tile.stacked.selected { box-shadow: 0 0 0 1px var(--accent), 3px 3px 0 -1px var(--bg-medium), 3px 3px 0 0 var(--accent); }
  .pic, .play { position: absolute; inset: 0; width: 100%; height: 100%; object-fit: cover; display: block; }
  .play { background: #000; }
  .strip { position: absolute; inset: 0; background-size: 1000% 100%; background-repeat: no-repeat; }
  /* The waveform is white on transparent: tinted here with the theme's accent. */
  .wave { position: absolute; inset: 10% 6% 22%; background: var(--accent); -webkit-mask: var(--wave) center / 100% 100% no-repeat; mask: var(--wave) center / 100% 100% no-repeat; opacity: 0.85; }
  .icon { position: absolute; inset: 0; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; padding: 10px; color: var(--fg-dim); text-align: center; }
  .ititle { font-size: 11px; color: var(--fg); overflow-wrap: anywhere; display: -webkit-box; -webkit-line-clamp: 3; line-clamp: 3; -webkit-box-orient: vertical; overflow: hidden; }
  .pending { font-size: 10px; }
  .badges { position: absolute; top: 5px; left: 5px; display: flex; gap: 4px; flex-wrap: wrap; }
  .corner { position: absolute; bottom: 5px; right: 5px; display: flex; gap: 4px; }
  .b { font-size: 10px; line-height: 15px; padding: 0 5px; border-radius: 4px; background: color-mix(in srgb, var(--bg-dark) 78%, transparent); color: var(--fg); font-variant-numeric: tabular-nums; }
  .star { color: var(--yellow); }
  .final { color: var(--green); }
  .picked { color: var(--accent); }
  .rej { color: var(--red); }
  .rec { color: var(--yellow); }
  .type { letter-spacing: 0.04em; }
  .trash-b { position: absolute; bottom: 5px; left: 5px; color: var(--fg-dim); }
  .variants { position: absolute; bottom: 5px; left: 5px; font-size: 10px; line-height: 15px; padding: 0 5px; border-radius: 4px; background: color-mix(in srgb, var(--bg-dark) 78%, transparent); color: var(--fg-dim); }
  .aplay { position: absolute; left: 6px; bottom: 5px; width: 22px; height: 22px; border-radius: 50%; border: none; background: var(--accent); color: var(--bg-dark); display: flex; align-items: center; justify-content: center; cursor: pointer; padding: 0; }
  .aplay:focus-visible { outline: 2px solid var(--fg); }
  .tile[data-kind='audio'] .variants { left: 34px; }
  @media (prefers-reduced-motion: reduce) { .tile.flash { animation: none; box-shadow: 0 0 0 2px var(--accent); } }
</style>
