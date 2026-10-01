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
import { stackStore, type ServiceRuntime } from '$lib/stores/stack.svelte';
import { tasksStore } from '$lib/stores/tasks.svelte';
import {
  resolveCreate, isDue, isExpired, dueAt, envelope, statusOf, serviceOutcome, taskOutcome, isWaitingOnEvent, triggerText,
  type CreateArgs, type EventKind, type Resolved, type ResolvedEvent, type FollowUpStatus,
} from '$lib/followUps/model';

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
  /** A time one's due time; an event one's, once its event has happened. */
  due_at: string | null;
  /** An event one's trigger, "when service `web` is ready"; null for a time one. */
  trigger: string | null;
  /** What happened, once an event one's condition is met: "it crashed (exit 1)". */
  outcome: string | null;
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
  let stopTransitions: (() => void) | undefined;
  const onWake = () => { void tick(); };

  /** Every tab in this window that can hold follow-ups — live, archived, in a suspended workspace.
   *  An event happens whatever the tab is doing; delivery is what waits for the agent. */
  function* everyTab(): Generator<{ workspaceId: string; tab: Tab }> {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) for (const tab of pane.tabs) yield { workspaceId: ws.id, tab };
      for (const tab of ws.archived_tabs) yield { workspaceId: ws.id, tab };
    }
  }

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

  /** Record that an event follow-up's condition happened (§5) — in Rust, once; the first
   *  observation stands. From then on it is due like a time one, and waits only for the agent. */
  async function meet(workspaceId: string, tabId: string, f: FollowUp, outcome: string): Promise<void> {
    try {
      const list = await commands.meetTabFollowUp(workspaceId, tabId, f.id, new Date().toISOString(), outcome);
      if (!list) return; // gone (delivered, cancelled, moved by a reload) or already met
      mirror(tabId, list);
      logInfo(`follow-ups: ${f.id.slice(0, 8)} on tab ${tabId.slice(0, 8)} is due — ${f.due.kind} ${f.due.label ?? ''}: ${outcome}`);
    } catch (e) {
      logError(`follow-ups: marking ${f.id.slice(0, 8)} met failed: ${e}`);
    }
  }

  /** Waiting, unexpired event follow-ups in this window. */
  function* waitingOnEvents(now: number): Generator<{ workspaceId: string; tab: Tab; f: FollowUp }> {
    for (const { workspaceId, tab } of everyTab()) {
      for (const f of tab.follow_ups ?? []) {
        if (isWaitingOnEvent(f) && !isExpired(f, now)) yield { workspaceId, tab, f };
      }
    }
  }

  /** A stack service changed status. The only evidence a service came up or went down is a change
   *  this window's stack store made — so these are met here, as they happen, and never by reading
   *  the status later (it isn't persisted, and reads `stopped` for a service nobody started). */
  function onServiceTransition(serviceId: string, from: string, to: ServiceRuntime) {
    for (const { workspaceId, tab, f } of waitingOnEvents(Date.now())) {
      if (f.due.service_id !== serviceId) continue;
      const outcome = serviceOutcome(f.due.kind, from, to);
      if (outcome) void meet(workspaceId, tab.id, f, outcome);
    }
  }

  /** The conditions that ARE levels: a task that has ended (tasks are persisted), and anything a
   *  follow-up waits on that no longer exists — which fires, with the reason, rather than waiting
   *  a week for nothing (§3). Looked up in the workspace the condition was SET in, not the tab's
   *  current one: a moved tab's follow-up still points where it pointed. */
  async function checkConditions(now: number) {
    for (const { workspaceId, tab, f } of [...waitingOnEvents(now)]) {
      const ws = workspacesStore.workspaces.find(w => w.id === f.due.workspace_id);
      let outcome: string | null;
      if (!ws) {
        // This window can't see that workspace's stack or tasks, so nothing here could ever meet it.
        outcome = 'the workspace it was in was closed, or moved to another window';
      } else if (f.due.kind === 'task_done') {
        // Not loaded is not "deleted": an absent list is no evidence the task is gone.
        if (!tasksStore.loaded) continue;
        const task = tasksStore.find(ws.id, f.due.task_id ?? '')
          // Parked with an archived tab, the task still exists (`archive_tab` lifts it off the board).
          ?? ws.archived_tabs.flatMap(t => t.archived_tasks ?? []).find(t => t.id === f.due.task_id);
        outcome = taskOutcome(task ? task.status : null);
      } else {
        outcome = (ws.stack ?? []).some(s => s.id === f.due.service_id) ? null : 'the service was removed from the stack';
      }
      if (outcome) await meet(workspaceId, tab.id, f, outcome);
    }
  }

  /** What an event trigger names, looked up in the calling tab's project. Refuses a condition that
   *  already holds: these fire on the NEXT change, so "when `web` is ready" for a service that is
   *  ready would wait for it to go down and come back — tell the agent now instead. */
  function resolveEvent(workspaceId: string, kind: EventKind, ref: string): ResolvedEvent {
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    if (!ws) return { ok: false, reason: 'tab_not_found', detail: 'maiTerm does not know this tab’s project.' };
    if (kind === 'task_done') {
      if (!tasksStore.loaded) return { ok: false, reason: 'not_ready', detail: 'Tasks are still loading — try again in a moment.' };
      const task = tasksStore.find(ws.id, ref)
        ?? ws.archived_tabs.flatMap(t => t.archived_tasks ?? []).find(t => t.id === ref);
      if (!task) {
        const elsewhere = tasksStore.findAnywhere(ref);
        return elsewhere
          ? { ok: false, reason: 'other_project', detail: `Task ${ref} belongs to another project; a follow-up can wait only on this project's tasks.` }
          : { ok: false, reason: 'unknown_task', detail: `No task ${ref} in this project — pass the full id from listTasks.` };
      }
      if (task.status === 'done' || task.status === 'dropped') {
        return { ok: false, reason: 'already_ended', detail: `“${task.title}” has already ended (${task.status}) — act on it now.` };
      }
      return { ok: true, workspace_id: ws.id, task_id: task.id, label: task.title };
    }
    const services = ws.stack ?? [];
    const s = services.find(x => x.id === ref) ?? services.find(x => x.name.toLowerCase() === ref.toLowerCase());
    if (!s) {
      const have = services.length ? `it has: ${services.map(x => x.name).join(', ')}` : 'it has none';
      return { ok: false, reason: 'unknown_service', detail: `No service "${ref}" in this project's stack (${have}). listStack shows them.` };
    }
    const st = stackStore.status(s.id);
    if (kind === 'service_ready' && st === 'ready') {
      return { ok: false, reason: 'already_ready', detail: `\`${s.name}\` is already ready — act on it now. This trigger waits for the NEXT time it comes up.` };
    }
    if (kind === 'service_stopped' && st !== 'starting' && st !== 'running' && st !== 'ready') {
      return { ok: false, reason: 'not_running', detail: `\`${s.name}\` isn't running (${st}), so it can't stop. Start it, or wait for it with when_service_ready.` };
    }
    return { ok: true, workspace_id: ws.id, service_id: s.id, label: s.name };
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
      await checkConditions(now);
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
      // Subscribed whether or not the feature is live: an event that happens while follow-ups are
      // off is still recorded, so it delivers when they are turned back on (§4 — off is held).
      stopTransitions = stackStore.onTransition(onServiceTransition);
      window.addEventListener('focus', onWake);
      document.addEventListener('visibilitychange', onWake);
      void tick();
    },

    destroy() {
      if (timer) clearInterval(timer);
      timer = undefined;
      stopTransitions?.();
      stopTransitions = undefined;
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
        resolveEvent: (kind, ref) => resolveEvent(loc.workspaceId, kind, ref),
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
          due_at: f.due.kind === 'at' ? (f.due.at ?? null) : (f.due.met_at ?? null),
          trigger: triggerText(f),
          outcome: f.due.outcome ?? null,
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
      // Early: before its time, or before its event has happened.
      return deliverOne(loc.tab, f, now, due == null || due > now);
    },

    /** Run the tick now (tests). */
    tick,
  };
}

export const followUpsStore = createFollowUpsStore();
