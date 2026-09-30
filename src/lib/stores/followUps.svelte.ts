/**
 * Follow-ups — prompts scheduled back into a tab's own agent (docs/follow-ups.md).
 *
 * The follow-ups themselves live on `Tab.follow_ups`, persisted, and Rust is the authority for
 * them: this store changes them only through atomic commands (`addTabFollowUp`,
 * `takeTabFollowUp`) and mirrors the answer. It also runs the delivery tick for THIS window's
 * tabs — every window runs its own, over its own workspaces.
 *
 * Delivery (§6.1) goes through `agentDelivery.tryDeliverNow`, never `deliver()`: that one
 * queues, delivers later without a word, can't be withdrawn and forgets on restart. A follow-up
 * is TAKEN off its tab before it is typed, and put back if the inject doesn't happen. That makes
 * delivery at-most-once: a crash in the milliseconds between take and inject loses it. The other
 * order — type, then remove — delivered one follow-up from two tabs whenever a reload moved it
 * mid-delivery, which is an ordinary event rather than a crash.
 */
import { info as logInfo, warn as logWarn, error as logError } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { FollowUp, Tab } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { agentDelivery } from '$lib/stores/agentDeliveryLive';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { resolveCreate, isDue, isExpired, dueAt, envelope, statusOf, type CreateArgs, type Resolved, type FollowUpStatus } from '$lib/followUps/model';

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

  /** Mirror what Rust says the list now is. Rust is the authority: every change is one atomic
   *  command there (add, take), and the mirror only ever takes its answer — a list computed here
   *  from the mirror and written back is what lost updates when two changes were in flight. */
  function mirror(tabId: string, list: FollowUp[]) {
    const loc = locate(tabId);
    if (loc) loc.tab.follow_ups = list;
  }

  /** Remove one — atomically, and only if the tab still holds it. False: it wasn't there. */
  async function take(tabId: string, id: string): Promise<boolean> {
    const loc = locate(tabId);
    if (!loc) return false;
    const list = await commands.takeTabFollowUp(loc.workspaceId, tabId, id);
    if (list === null) return false;
    mirror(tabId, list);
    return true;
  }

  async function add(tabId: string, f: FollowUp): Promise<void> {
    const loc = locate(tabId);
    if (!loc) throw new Error('maiTerm does not know this tab.');
    mirror(tabId, await commands.addTabFollowUp(loc.workspaceId, tabId, f));
  }

  /** Why this tab can't take a follow-up right now, or null. Every check is one a live
   *  delivery has to pass — the order is cheapest first, and the one async probe last. */
  async function holdReason(tab: Tab): Promise<string | null> {
    // Between turns only (§6.1): the delivery controller would deliver mid-turn, which suits a
    // peer's reply; a follow-up is never urgent, and runtimes differ on input typed mid-turn.
    const st = agentStateStore.getState(tab.id);
    if (st?.state !== 'idle') return 'agent not idle';
    // Typing right now: a keystroke this recent may not have reached the screen yet.
    const typed = terminalsStore.getLastTakeoverInputAt(tab.id);
    if (typed !== undefined && Date.now() - typed < 2000) return 'your human is typing';
    // And the screen has settled — the same 1.5 s the Overlord's own notices wait for.
    if (Date.now() - (terminalsStore.getLastOutputAt(tab.id) ?? 0) < 1500) return 'output still arriving';
    // No draft in the input box. Typing into an agent's input fires no hook, so nothing reports
    // a draft, and a paste plus CR would submit it with the follow-up glued on. The box is READ
    // off the screen (mailink/input_box.rs): keystroke timestamps were tried first and failed
    // both ways — a relaunched agent's own `claude -c` held its follow-ups forever, then
    // type-ahead during boot slipped through. Timestamps remain only where the screen isn't a
    // layout maiTerm recognises: anything typed since this stretch of idle began holds.
    const box = await commands.agentInputBox(tab.id);
    if (box === 'has_text') return 'a draft is in the input box';
    if (box === 'unknown' && typed !== undefined && typed > (st.idleSince ?? st.updatedAt)) {
      return 'your human may have a draft (input box not readable)';
    }
    if (!agentDelivery.canDeliverNow(tab.id)) return 'another message is being delivered';
    // A session entry is not an agent: it is cleared by the SessionEnd hook, which never comes
    // when the process is killed or its ssh tunnel is down. Without a live process the paste
    // lands in a shell. Same evidence the Overlord (replState) and comms rely on — and the same
    // known gap: a remote agent that dies while its ssh stays up still reads as live.
    const inst = terminalsStore.get(tab.id);
    if (!inst) return 'no terminal';
    try {
      const live = await commands.getAgentLiveness(inst.ptyId);
      if (!(live.agent_running || live.ssh_foreground)) return 'no agent running';
    } catch {
      return 'no agent running';
    }
    // The probe awaited; state can move under it.
    if (agentStateStore.getState(tab.id)?.state !== 'idle') return 'agent not idle';
    return null;
  }

  function recentCreations(tabId: string, now: number): number {
    const kept = (createdAt.get(tabId) ?? []).filter(t => now - t < HOUR_MS);
    createdAt.set(tabId, kept);
    return kept.length;
  }

  async function deliverDue(tab: Tab, now: number) {
    const due = (tab.follow_ups ?? []).filter(f => isDue(f, now)).sort((a, b) => (dueAt(a) ?? 0) - (dueAt(b) ?? 0));
    if (due.length === 0 || delivering.has(tab.id)) return;
    delivering.add(tab.id);
    const f = due[0]; // one per tab per tick: the next one waits for this one's turn to end
    let held = false; // taken off the tab and not yet delivered or given back
    try {
      if (await holdReason(tab)) return;
      // CLAIM it before typing anything: taken off the tab in Rust, atomically. If a reload has
      // moved it to a replacement tab (or a cancel beat us), the take finds nothing and this
      // tab does nothing — the replacement delivers it. Delivering and then removing is what
      // let one follow-up go out from both tabs.
      if (!(await take(tab.id, f.id))) return;
      held = true;
      const r = await agentDelivery.tryDeliverNow(tab.id, envelope(f, now));
      if (r === 'delivered') {
        held = false;
        logInfo(`follow-ups: delivered ${f.id.slice(0, 8)} to tab ${tab.id.slice(0, 8)}`);
        return;
      }
      // Claimed but not typed — the state moved in the gap. Put it back for the next tick.
      await add(tab.id, f);
      held = false;
    } catch (e) {
      // The at-most-once cost, named when it is paid: the tab went away (closed, reloaded, its
      // window shut) between the take and the give-back. Never a generic "failed".
      if (held) logWarn(`follow-ups: ${f.id.slice(0, 8)} LOST — taken from tab ${tab.id.slice(0, 8)} but neither delivered nor given back ("${f.text.slice(0, 60)}"): ${e}`);
      else logError(`follow-ups: delivering ${f.id.slice(0, 8)} to tab ${tab.id.slice(0, 8)} failed: ${e}`);
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
        // Expired ones will never be delivered, so they don't hold a slot (§6.3).
        pending: (loc.tab.follow_ups ?? []).filter(f => !isExpired(f, now)).length,
        createdLastHour: recentCreations(tabId, now),
        author,
        newId: () => crypto.randomUUID(),
      });
      if (!r.ok) return r;
      // Counted before the await, so two creates in flight can't both slip under the limit.
      createdAt.get(tabId)!.push(now);
      await add(tabId, r.followUp);
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
      if (!(await take(tabId, id))) return false;
      logInfo(`follow-ups: ${id.slice(0, 8)} cancelled on tab ${tabId.slice(0, 8)}`);
      return true;
    },

    /** Run the tick now (tests, and "Deliver now" in step 3). */
    tick,
  };
}

export const followUpsStore = createFollowUpsStore();
