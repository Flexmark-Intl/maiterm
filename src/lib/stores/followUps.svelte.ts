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
  /** Only for a `due` one: why it hasn't gone yet, as far as the tab itself shows — the
   *  synchronous checks only (the input box and liveness are read at delivery). Null when
   *  nothing about the tab is holding it. */
  waiting: string | null;
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
  /** The synchronous part of the gate: what the tab itself is doing, in words that are TRUE of
   *  it. Checked most-absent first — no terminal, then no agent, then a prompt, then busy —
   *  because "no session" read as "not idle" once told a human with no agent running that it
   *  would go "when this turn ends". Null: the tab is idle, the async checks decide. */
  function tabHold(tab: Tab): string | null {
    // No instance covers suspended and archived tabs AND an ordinary one not opened since launch
    // (terminals mount lazily) — so name none of them.
    if (!terminalsStore.get(tab.id)) return "the tab isn't loaded — it goes once the tab is open and its agent is running";
    const st = agentStateStore.getState(tab.id);
    if (!st) return 'no agent is running in the tab — it goes after an agent starts there';
    if (st.state === 'permission') return 'the agent is waiting on a permission prompt';
    // Between turns only (§6.1): the delivery controller would deliver mid-turn, which suits a
    // peer's reply; a follow-up is never urgent, and runtimes differ on input typed mid-turn.
    if (st.state !== 'idle') return 'the agent is busy — it goes when this turn ends';
    return null;
  }

  async function holdReason(tab: Tab): Promise<string | null> {
    const quick = tabHold(tab);
    if (quick) return quick;
    const st = agentStateStore.getState(tab.id)!;
    // Typing right now: a keystroke this recent may not have reached the screen yet.
    const typed = terminalsStore.getLastTakeoverInputAt(tab.id);
    if (typed !== undefined && Date.now() - typed < 2000) return 'someone is typing in the tab';
    // And the screen has settled — the same 1.5 s the Overlord's own notices wait for.
    if (Date.now() - (terminalsStore.getLastOutputAt(tab.id) ?? 0) < 1500) return 'output still arriving';
    // No draft in the input box. Typing into an agent's input fires no hook, so nothing reports
    // a draft, and a paste plus CR would submit it with the follow-up glued on. The box is READ
    // off the screen (mailink/input_box.rs): keystroke timestamps ALONE were tried first and
    // failed both ways — a relaunched agent's own `claude -c` held its follow-ups forever, then
    // type-ahead during boot slipped through. They still guard around the read — the 2 s check
    // above, `deliverOne`'s after-the-gate abort — and decide it where the screen isn't a layout
    // maiTerm recognises: anything typed since this stretch of idle began holds.
    const box = await commands.agentInputBox(tab.id);
    if (box === 'has_text') return "there's a draft in the agent's input box — send or clear it first";
    if (box === 'unknown' && typed !== undefined && typed > (st.idleSince ?? st.updatedAt)) {
      return 'something was typed in the tab since the agent went idle, and its input box can’t be read';
    }
    if (!agentDelivery.canDeliverNow(tab.id)) return 'another message is being delivered';
    // A session entry is not an agent: it is cleared by the SessionEnd hook, which never comes
    // when the process is killed or its ssh tunnel is down. Without a live process the paste
    // lands in a shell. Same evidence the Overlord (replState) and comms rely on — and the same
    // known gap: a remote agent that dies while its ssh stays up still reads as live.
    const inst = terminalsStore.get(tab.id);
    if (!inst) return tabHold(tab) ?? "the tab isn't running";
    try {
      const live = await commands.getAgentLiveness(inst.ptyId);
      if (!(live.agent_running || live.ssh_foreground)) return 'no agent is running in the tab';
    } catch {
      return 'no agent is running in the tab';
    }
    // The probe awaited; state can move under it — and say what it moved to.
    return tabHold(tab);
  }

  function recentCreations(tabId: string, now: number): number {
    const kept = (createdAt.get(tabId) ?? []).filter(t => now - t < HOUR_MS);
    createdAt.set(tabId, kept);
    return kept.length;
  }

  async function deliverDue(tab: Tab, now: number) {
    const due = (tab.follow_ups ?? []).filter(f => isDue(f, now)).sort((a, b) => (dueAt(a) ?? 0) - (dueAt(b) ?? 0));
    if (due.length === 0) return;
    await deliverOne(tab, due[0], now); // one per tab per tick: the next waits for this turn to end
  }

  /** Deliver one follow-up through the full gate. Null when delivered; otherwise why not, in
   *  words a human can act on. The tick and the human's "Deliver now" both come through here,
   *  so the button can never skip a check the tick makes — only the due time is theirs to
   *  waive. `early`: delivered before it was due, at the human's request. */
  async function deliverOne(tab: Tab, f: FollowUp, now: number, early = false): Promise<string | null> {
    if (delivering.has(tab.id)) return 'another follow-up is being delivered to this tab';
    delivering.add(tab.id);
    let held = false; // taken off the tab and not yet delivered or given back
    // Any human keystroke after this moment aborts the delivery. `holdReason` reads the box
    // and then awaits more (the liveness sweep, the take, the trust check); someone who starts
    // typing inside that window would get the paste landed after their keys and submitted with
    // them. Checked again before the take and, last, just before the paste is written.
    const keysAtGate = terminalsStore.getLastTakeoverInputAt(tab.id);
    const untouched = () => terminalsStore.getLastTakeoverInputAt(tab.id) === keysAtGate;
    try {
      const reason = await holdReason(tab);
      if (reason) return reason;
      if (!untouched()) return 'someone is typing in the tab';
      // CLAIM it before typing anything: taken off the tab in Rust, atomically. If a reload has
      // moved it to a replacement tab (or a cancel beat us), the take finds nothing and this
      // tab does nothing — the replacement delivers it. Delivering and then removing is what
      // let one follow-up go out from both tabs.
      if (!(await take(tab.id, f.id))) return 'it is no longer on this tab';
      held = true;
      const r = await agentDelivery.tryDeliverNow(tab.id, envelope(f, now, early), untouched);
      if (r === 'delivered') {
        held = false;
        logInfo(`follow-ups: delivered ${f.id.slice(0, 8)} to tab ${tab.id.slice(0, 8)}${early ? ' (early, by hand)' : ''}`);
        return null;
      }
      // Claimed but not typed — the state moved in the gap. Put it back for the next tick.
      await add(tab.id, f);
      held = false;
      return 'the tab changed state just before delivery — try again';
    } catch (e) {
      // The at-most-once cost, named when it is paid: the tab went away (closed, reloaded, its
      // window shut) between the take and the give-back. Never a generic "failed".
      if (held) logWarn(`follow-ups: ${f.id.slice(0, 8)} LOST — taken from tab ${tab.id.slice(0, 8)} but neither delivered nor given back ("${f.text.slice(0, 60)}"): ${e}`);
      else logError(`follow-ups: delivering ${f.id.slice(0, 8)} to tab ${tab.id.slice(0, 8)} failed: ${e}`);
      return String(e);
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
      const tab = locate(tabId)?.tab;
      const live = preferencesStore.followUpsLive;
      return (tab?.follow_ups ?? []).map(f => {
        const status = statusOf(f, now);
        const waiting = status !== 'due' ? null : !live ? 'held: follow-ups are off' : tab ? tabHold(tab) : null;
        return {
          id: f.id,
          text: f.text,
          author: f.author,
          status,
          due_at: f.due.at ?? null,
          created_at: f.created_at,
          expires_at: f.expires_at ?? null,
          waiting,
        };
      });
    },

    /** The tab a follow-up list belongs to, live or archived — the same search the store's own
     *  operations use, so a caller can't show "nothing" for a tab the store can see. */
    findTab(tabId: string): { tab: Tab; archived: boolean } | null {
      const loc = locate(tabId);
      if (!loc) return null;
      const ws = workspacesStore.workspaces.find(w => w.id === loc.workspaceId);
      return { tab: loc.tab, archived: !!ws?.archived_tabs.some(t => t.id === tabId) };
    },

    /** Remove one from this tab. False if the tab doesn't hold it. */
    async cancel(tabId: string, id: string): Promise<boolean> {
      if (!(await take(tabId, id))) return false;
      logInfo(`follow-ups: ${id.slice(0, 8)} cancelled on tab ${tabId.slice(0, 8)}`);
      return true;
    },

    /** The human's "Deliver now": the full gate, only the due time waived. Null when
     *  delivered; otherwise why it was held. Expired ones stay expired — cancel those. */
    async deliverNow(tabId: string, id: string): Promise<string | null> {
      const loc = locate(tabId);
      const f = loc?.tab.follow_ups?.find(x => x.id === id);
      if (!loc || !f) return 'it is no longer on this tab';
      const now = Date.now();
      if (isExpired(f, now)) return 'it has expired — cancel it, or add a new one';
      if (!preferencesStore.followUpsLive) return 'follow-ups are off (Preferences → Overlord)';
      const due = dueAt(f);
      return deliverOne(loc.tab, f, now, due != null && due > now);
    },

    /** Run the tick now (tests). */
    tick,
  };
}

export const followUpsStore = createFollowUpsStore();
