/** What a Hive board shows beyond its worker records (docs/hive.md §4.4): each worker's diff stat
 *  and meta, pushed by Rust (`hive-diff-stat`, `hive-worker-meta`, sent to this window only) after
 *  one read at mount, and the activity line's sticky. Nothing here polls: the one interval is a
 *  30 s render clock for the age labels, and it reads nothing. One Hive at a time, the one on
 *  screen (+page keys HiveView by workspace). */

import { untrack } from 'svelte';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { error as logError } from '@tauri-apps/plugin-log';
import { countedListen } from '$lib/utils/listenCounter';
import { hiveDiffStats, hiveWorkerMetas, type HiveWorkerMetaEvent, type TabMeta } from '$lib/tauri/commands';
import type { HiveDiffStat, HiveDiffStatEvent } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { STICKY_MS, stickyStep, type Sticky } from '$lib/hive/activity';

const CLOCK_MS = 30_000;

function createHiveFeedStore() {
  let attached: string | null = null;
  let diffStats = $state<Map<string, HiveDiffStat>>(new Map());
  let metas = $state<Map<string, TabMeta | null>>(new Map());
  let stickies = $state<Map<string, Sticky>>(new Map());
  let now = $state(Date.now());
  let teardown: (() => void) | null = null;

  function setSticky(tabId: string, s: Sticky | null) {
    untrack(() => {
      if (!s && !stickies.has(tabId)) return;
      const next = new Map(stickies);
      if (s) next.set(tabId, s);
      else next.delete(tabId);
      stickies = next;
    });
  }

  function attach(workspaceId: string) {
    if (attached === workspaceId) return;
    teardown?.();
    attached = workspaceId;
    diffStats = new Map();
    metas = new Map();
    stickies = new Map();
    now = Date.now();
    const id8 = workspaceId.slice(0, 8);
    let gone = false;
    const unlisteners: (() => void)[] = [];
    const target = { target: getCurrentWindow().label };

    const onStat = countedListen<HiveDiffStatEvent>('hive-diff-stat', (e) => {
      if (gone || e.payload.workspace_id !== workspaceId) return;
      diffStats = new Map(diffStats).set(e.payload.stat.task_id, e.payload.stat);
    }, target);
    const onMeta = countedListen<HiveWorkerMetaEvent>('hive-worker-meta', (e) => {
      if (gone || e.payload.workspace_id !== workspaceId) return;
      metas = new Map(metas).set(e.payload.tab_id, e.payload.meta);
    }, target);
    // Listening first, then the mount's read, so nothing pushed in between is lost. A push that
    // lands before the read answers is the newer of the two and is kept.
    Promise.all([onStat, onMeta])
      .then((us) => {
        if (gone) return us.forEach((u) => u());
        unlisteners.push(...us);
        hiveDiffStats(workspaceId)
          .then((list) => {
            if (gone) return;
            const next = new Map(diffStats);
            for (const s of list) {
              const had = next.get(s.task_id);
              if (!had || had.computed_at < s.computed_at) next.set(s.task_id, s);
            }
            diffStats = next;
          })
          .catch((e) => logError(`hive: diff stats of ${id8} not read: ${e}`));
        hiveWorkerMetas(workspaceId)
          .then((list) => {
            if (gone) return;
            const next = new Map(metas);
            for (const m of list) if (!next.has(m.tab_id)) next.set(m.tab_id, m.meta);
            metas = next;
          })
          .catch((e) => logError(`hive: worker metas of ${id8} not read: ${e}`));
      })
      .catch((e) => logError(`hive: feed listeners for ${id8}: ${e}`));

    // The sticky: the last tool line held for a beat after PostToolUse clears it, so the line
    // doesn't flicker to "Thinking" between two tools. Frontend only; no new Rust event.
    const last = new Map<string, string>();
    const timers = new Map<string, ReturnType<typeof setTimeout>>();
    const stopEffects = $effect.root(() => {
      $effect(() => {
        const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
        const tabs = ws?.panes.flatMap((p) => p.tabs).filter((t) => t.hive_role === 'worker').map((t) => t.id) ?? [];
        const at = Date.now();
        for (const tab of tabs) {
          const step = stickyStep(last.get(tab), agentStateStore.getState(tab), at);
          if (step.last) last.set(tab, step.last);
          else last.delete(tab);
          if (step.sticky === undefined) continue;
          clearTimeout(timers.get(tab));
          timers.delete(tab);
          setSticky(tab, step.sticky);
          const s = step.sticky;
          if (s) {
            timers.set(tab, setTimeout(() => {
              timers.delete(tab);
              if (untrack(() => stickies.get(tab)) === s) setSticky(tab, null);
            }, STICKY_MS));
          }
        }
      });
    });

    const clock = setInterval(() => (now = Date.now()), CLOCK_MS);
    teardown = () => {
      gone = true;
      unlisteners.forEach((u) => u());
      stopEffects();
      for (const t of timers.values()) clearTimeout(t);
      clearInterval(clock);
    };
  }

  /** Only the Hive that attached: a keyed remount may mount the next before this one goes. */
  function detach(workspaceId: string) {
    if (attached !== workspaceId) return;
    teardown?.();
    teardown = null;
    attached = null;
  }

  return {
    attach,
    detach,
    diffStat(taskId: string): HiveDiffStat | undefined {
      return diffStats.get(taskId);
    },
    meta(tabId: string | null | undefined): TabMeta | null {
      return tabId ? (metas.get(tabId) ?? null) : null;
    },
    sticky(tabId: string | null | undefined): Sticky | undefined {
      return tabId ? stickies.get(tabId) : undefined;
    },
    /** The render clock: ages only. */
    get now() {
      return now;
    },
  };
}

export const hiveFeed = createHiveFeedStore();
