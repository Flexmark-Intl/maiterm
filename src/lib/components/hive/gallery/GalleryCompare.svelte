<!--
  Two to four assets side by side (docs/hive.md §10.4, step 8e), in the gallery's area: picking a
  winner from variants is most of the work in a media Hive.

  Videos play in sync: play, pause and seek on the pane the human last touched (the leader) are
  mirrored on the others, and drift past 0.1 s is corrected (`syncActions`; the followers' own
  events are ignored, or each correction would bounce). Audio is A/B: one plays at a time, and X
  switches to the next at the shared position. F and 1/2/3 act on the focused pane (GalleryView
  routes the keys here); a click on a pane focuses it.
-->
<script lang="ts">
  import Icon from '$lib/components/Icon.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import type { GalleryAssetRow, GalleryTagChange } from '$lib/tauri/types';
  import { galleryUrl, madeByLabel, picture } from '$lib/hive/gallery';
  import { abSwitch, syncActions, type SyncEvent } from '$lib/hive/galleryCurate';

  interface Props {
    hiveId: string;
    ids: string[];
    rows: GalleryAssetRow[];
    recommendations: Record<string, string>;
    onback: () => void;
    ontag: (ids: string[], change: GalleryTagChange) => void;
    onopen: (id: string) => void;
  }
  let { hiveId, ids, rows, recommendations, onback, ontag, onopen }: Props = $props();

  const byId = $derived(new Map(rows.map((r) => [r.id, r])));
  const panes = $derived(ids.map((id) => byId.get(id)).filter((r): r is GalleryAssetRow => !!r));
  let focused = $state(0);
  $effect(() => {
    if (focused >= panes.length) focused = Math.max(0, panes.length - 1);
  });

  // ── Players: one element per pane, null for a pane with nothing to play. Videos in sync play
  //    the first one's sound only: four soundtracks at once say nothing. ──
  const media: (HTMLMediaElement | null)[] = [];
  /** The pane whose events drive the others: the one last touched. */
  let leader = 0;

  function touch(i: number) {
    focused = i;
    leader = i;
  }

  function onVideoEvent(i: number, kind: SyncEvent['kind']) {
    const me = media[i];
    if (!me) return;
    const videos = panes.map((p, j) => (p.kind === 'video' ? media[j] : null));
    const states = videos.map((v) => (v ? { paused: v.paused, currentTime: v.currentTime } : { paused: true, currentTime: me.currentTime }));
    for (const a of syncActions(leader, i, { kind, time: me.currentTime }, states)) {
      const v = videos[a.index];
      if (!v) continue;
      if (a.seek !== undefined) v.currentTime = a.seek;
      if (a.play === true) void v.play().catch(() => {});
      if (a.play === false) v.pause();
    }
  }

  /** Audio is A/B: a pane that starts playing stops every other audio pane. */
  function onAudioPlay(i: number) {
    panes.forEach((p, j) => {
      if (j !== i && p.kind === 'audio') media[j]?.pause();
    });
  }

  /** X: the next audio pane plays from where the playing one is. */
  export function switchAudio() {
    const audio = panes.map((p, j) => ({ p, j })).filter(({ p }) => p.kind === 'audio');
    if (audio.length < 2) return;
    const playing = audio.findIndex(({ j }) => media[j] && !media[j]!.paused);
    const fromIdx = playing >= 0 ? playing : Math.max(0, audio.findIndex(({ j }) => j === focused));
    const time = media[audio[fromIdx].j]?.currentTime ?? 0;
    const sw = abSwitch(fromIdx, audio.length, time);
    if (!sw) return;
    const stop = media[audio[sw.stop].j];
    const next = media[audio[sw.play].j];
    stop?.pause();
    if (next) {
      next.currentTime = sw.at;
      void next.play().catch(() => {});
      touch(audio[sw.play].j);
    }
  }

  /** Space: the focused pane plays or pauses (videos carry the others with them). */
  export function togglePlay() {
    const m = media[focused];
    if (!m) return;
    leader = focused;
    if (m.paused) void m.play().catch(() => {});
    else m.pause();
  }

  export function focusedId(): string | null {
    return panes[focused]?.id ?? null;
  }

  export function moveFocus(dir: -1 | 1) {
    if (!panes.length) return;
    focused = (focused + dir + panes.length) % panes.length;
    leader = focused;
  }

  const STATUSES: [string, string][] = [['picked', 'Pick'], ['final', 'Final'], ['rejected', 'Reject']];
</script>

<div class="compare">
  <div class="bar">
    <button type="button" class="back" onclick={onback}><Icon name="chevron" size={11} /> Grid</button>
    <span class="hint">Comparing {panes.length}: videos play together; X switches audio; F and 1/2/3 act on the focused pane.</span>
  </div>
  <div class="panes" style="--n: {Math.max(1, panes.length)}">
    {#each panes as p, i (p.id)}
      {@const pic = picture(p)}
      <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
      <div class="pane" class:focused={focused === i} class:rejected={p.status === 'rejected'} onpointerdown={() => touch(i)}>
        <div class="stage">
          {#if p.kind === 'video'}
            <!-- svelte-ignore a11y_media_has_caption -->
            <video
              bind:this={media[i]}
              src={galleryUrl(hiveId, p.id)}
              poster={pic === 'thumb' ? galleryUrl(hiveId, p.id, 'thumb') : undefined}
              controls
              playsinline
              muted={i !== 0}
              onplay={() => onVideoEvent(i, 'play')}
              onpause={() => onVideoEvent(i, 'pause')}
              onseeked={() => onVideoEvent(i, 'seek')}
              ontimeupdate={() => onVideoEvent(i, 'tick')}
            ></video>
          {:else if p.kind === 'audio'}
            {#if pic === 'wave'}<div class="wave" style="--wave: url('{galleryUrl(hiveId, p.id, 'wave')}')" aria-hidden="true"></div>{/if}
            <audio bind:this={media[i]} src={galleryUrl(hiveId, p.id)} controls onplay={() => onAudioPlay(i)}></audio>
          {:else if p.kind === 'image'}
            <img src={galleryUrl(hiveId, p.id)} alt={p.title} draggable="false" />
          {:else if pic === 'thumb'}
            <img src={galleryUrl(hiveId, p.id, 'thumb')} alt="{p.title}: first page" draggable="false" />
          {:else}
            <div class="noprev"><Icon name="file" size={28} /><span>No preview</span></div>
          {/if}
        </div>
        <div class="foot">
          <button type="button" class="t" onclick={() => onopen(p.id)}>{p.title}</button>
          <span class="by">{madeByLabel(p.made_by)}{p.model ? ` · ${p.model}` : ''}</span>
          {#if recommendations[p.id] !== undefined}
            <span class="rec">Queen recommends: {recommendations[p.id] || 'no status'}
              <Tooltip text="Answers the recommendation with the status it has now">
                <button type="button" class="act" onclick={() => ontag([p.id], { status: p.status })}>Keep {p.status || 'no status'}</button>
              </Tooltip>
            </span>
          {/if}
          <div class="acts">
            <Tooltip text={p.starred ? 'Unstar (F)' : 'Star (F)'}>
              <button type="button" class="act" class:on={p.starred} aria-pressed={p.starred} onclick={() => ontag([p.id], { starred: !p.starred })}>★</button>
            </Tooltip>
            {#each STATUSES as [s, label], k (s)}
              <Tooltip text="{label} ({k + 1}){p.status === s ? ': again clears it' : ''}">
                <button type="button" class="act" class:on={p.status === s} aria-pressed={p.status === s} onclick={() => ontag([p.id], { status: p.status === s ? '' : s })}>{label}</button>
              </Tooltip>
            {/each}
          </div>
        </div>
      </div>
    {/each}
  </div>
</div>

<style>
  .compare { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; }
  .bar { display: flex; align-items: center; gap: 12px; padding: 8px 12px; border-bottom: 1px solid var(--bg-light); flex-wrap: wrap; }
  .back { display: inline-flex; align-items: center; gap: 4px; background: none; border: none; color: var(--accent); font: inherit; font-size: 12px; cursor: pointer; padding: 0; }
  .back :global(svg) { transform: rotate(90deg); }
  .hint { font-size: 11.5px; color: var(--fg-dim); }
  .panes { flex: 1; min-height: 0; display: grid; grid-template-columns: repeat(var(--n), minmax(0, 1fr)); gap: 8px; padding: 8px; overflow: auto; }
  .pane { min-width: 0; display: flex; flex-direction: column; border: 1px solid var(--bg-light); border-radius: 6px; overflow: hidden; background: var(--bg-medium); }
  .pane.focused { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .stage { flex: 1; min-height: 160px; display: flex; flex-direction: column; align-items: center; justify-content: center; gap: 6px; padding: 8px; background: color-mix(in srgb, var(--bg-dark) 60%, #000); }
  .stage img, .stage video { max-width: 100%; max-height: 60vh; object-fit: contain; }
  .pane.rejected .stage img { opacity: 0.4; filter: grayscale(0.7); }
  .wave { width: 100%; height: 90px; background: var(--accent); -webkit-mask: var(--wave) center / 100% 100% no-repeat; mask: var(--wave) center / 100% 100% no-repeat; }
  audio { width: 100%; }
  .noprev { display: flex; flex-direction: column; align-items: center; gap: 4px; color: var(--fg-dim); font-size: 11px; }
  .foot { display: flex; flex-direction: column; gap: 4px; padding: 8px 10px; font-size: 11.5px; }
  .t { background: none; border: none; padding: 0; color: var(--fg); font: inherit; font-weight: 600; text-align: left; cursor: pointer; overflow-wrap: anywhere; }
  .t:hover { color: var(--accent); }
  .by { color: var(--fg-dim); overflow-wrap: anywhere; }
  .rec { color: var(--yellow); }
  .acts { display: flex; flex-wrap: wrap; gap: 4px; }
  .act { background: var(--bg-dark); border: 1px solid var(--bg-light); color: var(--fg-dim); border-radius: 5px; padding: 1px 8px; font: inherit; font-size: 11px; cursor: pointer; }
  .act.on { color: var(--fg); border-color: var(--accent); }
  button:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }
</style>
