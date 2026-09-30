/**
 * Follow-ups — prompts scheduled back into a tab's own agent (docs/follow-ups.md).
 *
 * The follow-ups themselves live on `Tab.follow_ups`, persisted: this store holds no copy. It
 * adds and removes them (writing the tab's list whole through `setTabFollowUps`), and runs the
 * delivery tick for THIS window's tabs — every window runs its own, over its own workspaces.
 *
 * Delivery (§6.1) goes through `agentDelivery.tryDeliverNow`, never `deliver()`: that one
 * queues, delivers later without a word, can't be withdrawn and forgets on restart. Here the
 * follow-up stays on the tab until an inject has actually succeeded, and is removed then — so a
 * crash in between delivers it twice rather than never, the cheaper failure.
 */
import { info as logInfo, error as logError } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { FollowUp, Tab } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { agentDelivery } from '$lib/stores/agentDeliveryLive';
import { resolveCreate, isDue, dueAt, envelope, statusOf, type CreateArgs, type Resolved, type FollowUpStatus } from '$lib/followUps/model';

/** How often due follow-ups are looked for. Also runs on focus and visibility, because
 *  WKWebView throttles timers in an occluded window — the wall-clock comparison makes a late
 *  tick merely late, never wrong. */
const TICK_MS = 15_000;
const HOUR_MS = 60 * 60_000;

export interface FollowUpView {
  id: string;
  text: string;
  author: string;
  status: FollowUpStatus;
  due_at: string | null;
  created_at: string;
  expires_at: string | null;
}

function createFollowUpsStore() {
  /** Creation times per tab, for the 20-an-hour limit. In memory: a restart resetting the
   *  window is harmless — it is a loop backstop, not an accounting record. */
  const createdAt = new Map<string, number[]>();
  /** Tabs with an inject in flight, so one slow write can't be doubled by the next tick. */
  const delivering = new Set<string>();
  let timer: ReturnType<typeof setInterval> | undefined;
  let running = false;
  const onWake = () => { void tick(); };

  function locate(tabId: string): { workspaceId: string; tab: Tab } | null {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find(t => t.id === tabId);
        if (tab) return { workspaceId: ws.id, tab };
      }
      const archived = ws.archived_tabs.find(t => t.id === tabId);
      if (archived) return { workspaceId: ws.id, tab: archived };
    }
    return null;
  }

  /** Persist first, then update the mirror — the store pattern everywhere else. Always derives
   *  the new list from the tab's CURRENT list, never from a snapshot taken before an await, so a
   *  cancel and a delivery racing each other can't resurrect what the other removed. */
  async function update(tabId: string, change: (current: FollowUp[]) => FollowUp[]): Promise<boolean> {
    const loc = locate(tabId);
    if (!loc) return false;
    const next = change(loc.tab.follow_ups ?? []);
    await commands.setTabFollowUps(loc.workspaceId, tabId, next);
    const again = locate(tabId);
    if (again) again.tab.follow_ups = next;
    return true;
  }

  function recentCreations(tabId: string, now: number): number {
    const kept = (createdAt.get(tabId) ?? []).filter(t => now - t < HOUR_MS);
    createdAt.set(tabId, kept);
    return kept.length;
  }

  async function deliverDue(tab: Tab, now: number) {
    const due = (tab.follow_ups ?? []).filter(f => isDue(f, now)).sort((a, b) => (dueAt(a) ?? 0) - (dueAt(b) ?? 0));
    if (due.length === 0 || delivering.has(tab.id)) return;
    // The idle gate (§6.1): only between turns. The delivery controller would deliver to a live
    // session mid-turn, which suits a peer's reply; a follow-up is never urgent, and runtimes
    // differ in what they do with input typed mid-turn.
    if (agentStateStore.getState(tab.id)?.state !== 'idle') return;
    const f = due[0]; // one per tab per tick: the next one waits for this one's turn to end
    delivering.add(tab.id);
    try {
      const r = await agentDelivery.tryDeliverNow(tab.id, envelope(f, now));
      if (r !== 'delivered') return; // held or failed: it is still on the tab, next tick retries
      await update(tab.id, cur => cur.filter(x => x.id !== f.id));
      logInfo(`follow-ups: delivered ${f.id.slice(0, 8)} to tab ${tab.id.slice(0, 8)}`);
    } catch (e) {
      logError(`follow-ups: delivering ${f.id.slice(0, 8)} to tab ${tab.id.slice(0, 8)} failed: ${e}`);
    } finally {
      delivering.delete(tab.id);
    }
  }

  async function tick() {
    // Off means held, not discarded: pending follow-ups stay on their tabs, visible and
    // cancellable, and go out once the feature is back on (§4).
    if (running || !preferencesStore.followUpsLive) return;
    running = true;
    try {
      const now = Date.now();
      for (const ws of workspacesStore.workspaces) {
        // A suspended workspace's tabs have no agents to deliver to; waking one because a
        // timer fired is the human's call (§6.2). Archived tabs wait for their restore.
        if (ws.suspended) continue;
        for (const pane of ws.panes) {
          for (const tab of pane.tabs) {
            if (tab.follow_ups?.length) await deliverDue(tab, now);
          }
        }
      }
    } finally {
      running = false;
    }
  }

  return {
    init() {
      if (timer) return;
      timer = setInterval(() => { void tick(); }, TICK_MS);
      window.addEventListener('focus', onWake);
      document.addEventListener('visibilitychange', onWake);
      void tick();
    },

    destroy() {
      if (timer) clearInterval(timer);
      timer = undefined;
      window.removeEventListener('focus', onWake);
      document.removeEventListener('visibilitychange', onWake);
    },

    /** Schedule one. `author` is "agent" over MCP, "human" from the UI. */
    async create(tabId: string, args: CreateArgs, author: 'agent' | 'human'): Promise<Resolved> {
      const loc = locate(tabId);
      if (!loc) return { ok: false, reason: 'tab_not_found', detail: 'maiTerm does not know this tab.' };
      const now = Date.now();
      const r = resolveCreate(args, {
        now,
        pending: (loc.tab.follow_ups ?? []).length,
        createdLastHour: recentCreations(tabId, now),
        author,
        newId: () => crypto.randomUUID(),
      });
      if (!r.ok) return r;
      await update(tabId, cur => [...cur, r.followUp]);
      createdAt.get(tabId)!.push(now);
      return r;
    },

    /** This tab's follow-ups, each with where it stands right now. */
    list(tabId: string): FollowUpView[] {
      const now = Date.now();
      return (locate(tabId)?.tab.follow_ups ?? []).map(f => ({
        id: f.id,
        text: f.text,
        author: f.author,
        status: statusOf(f, now),
        due_at: f.due.at ?? null,
        created_at: f.created_at,
        expires_at: f.expires_at ?? null,
      }));
    },

    /** Remove one from this tab. False if the tab doesn't hold it. */
    async cancel(tabId: string, id: string): Promise<boolean> {
      const loc = locate(tabId);
      if (!loc || !(loc.tab.follow_ups ?? []).some(f => f.id === id)) return false;
      await update(tabId, cur => cur.filter(f => f.id !== id));
      logInfo(`follow-ups: ${id.slice(0, 8)} cancelled on tab ${tabId.slice(0, 8)}`);
      return true;
    },

    /** Run the tick now (tests, and "Deliver now" in step 3). */
    tick,
  };
}

export const followUpsStore = createFollowUpsStore();
