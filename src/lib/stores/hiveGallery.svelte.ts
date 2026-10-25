/** The Hive gallery view's state (docs/hive.md §10.4). Per workspace, in memory: the query (kind
 *  tab, filters, search), grouping, "Show variants", the selection, following and the open detail.
 *  Per viewer, in localStorage: the tile size. The rows and collections are the attached Hive's,
 *  the one on screen (+page keys HiveView by workspace), fetched through `GalleryFeed` when the
 *  gallery shows and refetched when this window hears `hive-gallery-changed` (`emit_to`, never a
 *  broadcast). `version` moves on every such event, shown or not: the board's cards follow it. */

import { getCurrentWindow } from '@tauri-apps/api/window';
import { error as logError } from '@tauri-apps/plugin-log';
import { countedListen } from '$lib/utils/listenCounter';
import { galleryCollections, galleryList } from '$lib/tauri/commands';
import type { GalleryAssetRow, GalleryChangedEvent, GalleryCollection } from '$lib/tauri/types';
import { defaultQuery, filterToQuery, type GalleryQuery, type GroupBy } from '$lib/hive/gallery';
import { GalleryFeed, realTimers } from '$lib/hive/galleryFeed';

export interface GalleryUi {
  query: GalleryQuery;
  group: GroupBy;
  showVariants: boolean;
  /** Ordered: the last is the keyboard's cursor. */
  selection: string[];
  /** Following the newest; null until the human chooses, which reads as "while agents work". */
  following: boolean | null;
  detail: string | null;
}

const defaultUi = (): GalleryUi => ({ query: defaultQuery(), group: 'none', showVariants: false, selection: [], following: null, detail: null });

const SIZE_KEY = 'hive.gallery.size';
export const SIZE_MIN = 120;
export const SIZE_MAX = 420;
const clampSize = (n: number) => Math.min(SIZE_MAX, Math.max(SIZE_MIN, Math.round(n)));
function savedSize(): number {
  try {
    const n = Number(localStorage.getItem(SIZE_KEY));
    return n ? clampSize(n) : 220;
  } catch {
    return 220;
  }
}

const COLLECTIONS_MS = 300;

function createHiveGalleryStore() {
  let uis = $state<Map<string, GalleryUi>>(new Map());
  let size = $state(typeof localStorage === 'undefined' ? 220 : savedSize());
  let attached: string | null = null;
  let feed: GalleryFeed | null = null;
  let rows = $state<GalleryAssetRow[]>([]);
  let loaded = $state(false);
  let collections = $state<GalleryCollection[]>([]);
  let version = $state(0);
  /** Moves when the flash set does, so tiles re-read `flashing`. */
  let flashTick = $state(0);
  let active = false;
  let teardown: (() => void) | null = null;

  function ui(wsId: string): GalleryUi {
    return uis.get(wsId) ?? defaultUi();
  }
  function setUi(wsId: string, patch: Partial<GalleryUi>) {
    uis = new Map(uis).set(wsId, { ...ui(wsId), ...patch });
  }

  let collTimer: ReturnType<typeof setTimeout> | null = null;
  function refetchCollections(wsId: string, soon = false) {
    if (collTimer) clearTimeout(collTimer);
    collTimer = setTimeout(() => {
      collTimer = null;
      galleryCollections(wsId)
        .then((c) => { if (attached === wsId) collections = c; })
        .catch((e) => logError(`hive: gallery collections of ${wsId.slice(0, 8)} not read: ${e}`));
    }, soon ? 0 : COLLECTIONS_MS);
  }

  /** Which attach is current: a view torn down after its successor mounted detaches nothing. */
  let attachSeq = 0;

  function attach(wsId: string): number {
    // Already this Hive's: the feed stays, the new view owns it.
    if (attached === wsId) return ++attachSeq;
    attachSeq++;
    teardown?.();
    attached = wsId;
    rows = [];
    loaded = false;
    collections = [];
    active = false;
    const f = new GalleryFeed(wsId, {
      list: galleryList,
      ...realTimers(),
      onError: (e) => logError(`hive: gallery of ${wsId.slice(0, 8)} not read: ${e}`),
    }, () => {
      if (feed !== f) return;
      rows = f.rows;
      loaded = f.loaded;
      version = f.version;
      flashTick++;
    });
    feed = f;
    f.setFilter(filterToQuery(ui(wsId).query));
    let gone = false;
    let unlisten: (() => void) | undefined;
    countedListen<GalleryChangedEvent>('hive-gallery-changed', (e) => {
      if (gone || !f.onEvent(e.payload)) return;
      if (active) refetchCollections(wsId);
    }, { target: getCurrentWindow().label })
      .then((u) => { if (gone) u(); else unlisten = u; })
      .catch((e) => logError(`hive: gallery-changed listener: ${e}`));
    teardown = () => {
      gone = true;
      unlisten?.();
      f.dispose();
      if (collTimer) clearTimeout(collTimer);
      collTimer = null;
      if (feed === f) feed = null;
    };
    return attachSeq;
  }

  return {
    attach,
    /** `token` is what `attach` answered: only the view that attached last detaches. */
    detach(wsId: string, token: number) {
      if (attached !== wsId || token !== attachSeq) return;
      teardown?.();
      teardown = null;
      attached = null;
    },
    /** The gallery is the view on screen: rows are fetched only while it is. */
    setActive(wsId: string, on: boolean) {
      if (attached !== wsId || !feed) return;
      active = on;
      feed.setActive(on);
      if (on) refetchCollections(wsId, true);
    },
    get rows() {
      return rows;
    },
    get loaded() {
      return loaded;
    },
    get collections() {
      return collections;
    },
    /** Every gallery change this window heard for the attached Hive. */
    get version() {
      return version;
    },
    isFlashing(id: string): boolean {
      void flashTick;
      return feed?.isFlashing(id) ?? false;
    },
    get size() {
      return size;
    },
    setSize(n: number) {
      size = clampSize(n);
      try {
        localStorage.setItem(SIZE_KEY, String(size));
      } catch {
        /* per-viewer nicety only */
      }
    },
    ui,
    setQuery(wsId: string, patch: Partial<GalleryQuery>) {
      const query = { ...ui(wsId).query, ...patch };
      setUi(wsId, { query });
      if (attached === wsId) feed?.setFilter(filterToQuery(query));
    },
    setGroup(wsId: string, group: GroupBy) {
      setUi(wsId, { group });
    },
    setShowVariants(wsId: string, on: boolean) {
      setUi(wsId, { showVariants: on });
    },
    select(wsId: string, ids: string[]) {
      setUi(wsId, { selection: ids });
    },
    setFollowing(wsId: string, on: boolean) {
      if (ui(wsId).following !== on) setUi(wsId, { following: on });
    },
    openDetail(wsId: string, id: string | null) {
      setUi(wsId, { detail: id, ...(id ? { selection: [id] } : {}) });
    },
  };
}

export const hiveGallery = createHiveGalleryStore();
