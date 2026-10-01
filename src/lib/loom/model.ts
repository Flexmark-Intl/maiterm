/** Workstream Loom (docs/loom.md): pure helpers behind the loom, decisions and Focus views.
 *  No runes and no stores here, so every rule is unit-testable. */

import type { AgentState } from '$lib/agents/types';
import type { ChatTurn } from '$lib/tauri/commands';
import type { Task, TaskStatus } from '$lib/tauri/types';
import { effectiveStatus, hasUnmetDeps, isRetired } from '$lib/tasks/model';

/** A task untouched this long reads as quiet. The board has "active" rows from a month ago that
 *  otherwise look exactly like live work. */
export const QUIET_MS = 14 * 24 * 60 * 60 * 1000;

/** Quiet means the row claims to be moving (active or to-do) and nothing has touched it. */
export function isQuiet(task: Task, now: number): boolean {
  if (task.status !== 'active' && task.status !== 'todo') return false;
  const at = Date.parse(task.updated_at);
  return Number.isFinite(at) && now - at > QUIET_MS;
}

/** The lane a task is drawn in: its effective status, so a dependency-blocked row reads blocked. */
export function laneOf(task: Task, all: Task[], parked?: ReadonlySet<string>): TaskStatus {
  return effectiveStatus(task, all, parked);
}

/** Tasks waiting on the human, oldest question first: the order they should be answered in. */
export function decisionsQueue(tasks: Task[]): Task[] {
  return tasks
    .filter((t) => t.status === 'blocked' && (t.blocker?.kind === 'decision' || t.blocker?.kind === 'action'))
    .sort((a, b) => Date.parse(a.blocker!.asked_at) - Date.parse(b.blocker!.asked_at));
}

/** Blocked with nothing saying why: no blocker record and no unmet dependency. The question it
 *  stopped on exists only in the agent's scrollback. Imported rows are left out: they mirror
 *  the runtime's own task list, whose dependencies live in that store and never reach
 *  `blocked_by`, so they would all read as unexplained. */
export function unexplainedBlocked(tasks: Task[], parked?: ReadonlySet<string>): Task[] {
  return tasks.filter(
    (t) => t.status === 'blocked' && !t.blocker && t.origin !== 'imported' && !hasUnmetDeps(t, tasks, parked),
  );
}

export interface LoomSummary {
  active: number;
  /** Waiting on the human: decision and action blockers. */
  needsYou: number;
  /** Blocked for any other reason: dependencies, external waits, or no stated reason. */
  waiting: number;
  quiet: number;
}

export function summarize(tasks: Task[], now: number, parked?: ReadonlySet<string>): LoomSummary {
  const s: LoomSummary = { active: 0, needsYou: 0, waiting: 0, quiet: 0 };
  for (const t of tasks) {
    // Counts what the loom draws: backlog is the parking lot and isn't drawn, even when a
    // dependency makes its effective lane read blocked.
    if (t.status === 'backlog') continue;
    const lane = laneOf(t, tasks, parked);
    if (lane === 'active') s.active++;
    if (lane === 'blocked') {
      if (t.status === 'blocked' && (t.blocker?.kind === 'decision' || t.blocker?.kind === 'action')) s.needsYou++;
      else s.waiting++;
    }
    if (isQuiet(t, now)) s.quiet++;
  }
  return s;
}

export interface LoomAgent {
  tabId: string;
  name: string;
  /** null for a tab with open tasks and no agent session (a closed or never-started agent). */
  state: AgentState | null;
  /** What it is doing right now, when it is doing something. */
  doing: string | null;
  /** Its open (not retired) tasks. */
  taskIds: string[];
}

export interface AgentTabInput {
  tabId: string;
  name: string;
  state: AgentState | null;
  toolDetail?: string | null;
}

/** The left column: every agent tab, plus any tab still holding open tasks. Ordered by what
 *  needs attention (permission, then working, then idle, then no session), then by name, so the
 *  column doesn't reshuffle as agents finish turns. */
export function loomAgents(tabs: AgentTabInput[], tasks: Task[]): LoomAgent[] {
  const open = (tabId: string) =>
    tasks.filter((t) => t.tab_id === tabId && !isRetired(t.status) && t.status !== 'backlog').map((t) => t.id);
  const rank: Record<string, number> = { permission: 0, active: 1, idle: 2 };
  return tabs
    .map((t) => ({
      tabId: t.tabId,
      name: t.name,
      state: t.state,
      doing: t.state === 'active' ? (t.toolDetail ?? null) : null,
      taskIds: open(t.tabId),
    }))
    .filter((a) => a.state !== null || a.taskIds.length > 0)
    .sort((a, b) => (rank[a.state ?? ''] ?? 3) - (rank[b.state ?? ''] ?? 3) || a.name.localeCompare(b.name));
}

// ── Condensed chat (the phone's vocabulary: maiLink thread page, toolVerb and the rows) ──

/** Activity verb for a tool marker like `Bash(npm test)`, as the collapsed row reads it. */
export function toolVerb(text: string): string {
  const name = text.split('(')[0].trim().toLowerCase();
  if (name.includes('bridge')) return 'bridge';
  if (name.startsWith('mcp__')) return name.split('__')[1] || 'tools';
  if (['edit', 'write', 'multiedit', 'notebookedit'].includes(name)) return 'editing files';
  if (['read', 'grep', 'glob', 'toolsearch'].includes(name) || name.includes('search')) return 'reading files';
  if (name === 'bash' || name.includes('shell') || name.includes('exec') || name.includes('run')) return 'bash';
  if (name.includes('fetch') || name.includes('web')) return 'web';
  return 'tools';
}

/** Something that happened to one of the agent's tasks, placed in its chat by time. The phone's
 *  transcript doesn't carry these; they come from the task's own record and log. */
export interface TaskEvent {
  /** Unique within the chat: the task and the note's position in its log. Two notes written in
   *  one updateTasks batch share a millisecond, so a time-based key collides. */
  key: string;
  ts: number;
  kind: 'added' | 'asked' | 'answered' | 'note';
  text: string;
  taskId: string;
  title: string;
}

const ASKED = /^(Waiting on your decision|Needs you|Waiting outside): /;
const ANSWERED = /^(Decided: |Done by the human|Cleared by the human|Handled in the tab by the human)/;

/** Task events for one tab: each task it carries being added, and every line of its log,
 *  classified by the prefixes `blockerNote` and `answerBlocker` write. */
export function taskEventsFor(tabId: string, tasks: Task[]): TaskEvent[] {
  const out: TaskEvent[] = [];
  for (const t of tasks) {
    if (t.tab_id !== tabId) continue;
    const created = Date.parse(t.created_at);
    if (Number.isFinite(created)) out.push({ key: `${t.id}:added`, ts: created, kind: 'added', text: t.title, taskId: t.id, title: t.title });
    (t.notes ?? []).forEach((n, i) => {
      const at = Date.parse(n.at);
      if (!Number.isFinite(at)) return;
      const kind = ASKED.test(n.text) ? 'asked' : ANSWERED.test(n.text) ? 'answered' : 'note';
      out.push({ key: `${t.id}:n${i}`, ts: at, kind, text: n.text, taskId: t.id, title: t.title });
    });
  }
  return out.sort((a, b) => a.ts - b.ts);
}

export type ChatRow =
  | { kind: 'turn'; turn: ChatTurn }
  | { kind: 'tools'; key: string; turns: ChatTurn[]; verbs: string[] }
  /** Something that happened that wasn't a message: a peer message or a goal change. */
  | { kind: 'rule'; turn: ChatTurn }
  | { kind: 'task'; key: string; event: TaskEvent }
  /** Tasks added back to back (an agent planning out loud adds six at once): one block, not
   *  six rules. */
  | { kind: 'added'; key: string; events: TaskEvent[] };

/** Fold a transcript into rows: each run of consecutive tool turns becomes one row keyed by its
 *  first msg_id, so a growing run updates in place while the agent streams. Task events are
 *  placed by time, from the first turn on (older ones belong to a chat that isn't shown), and
 *  a task event breaks a tool run so it is never folded away. A live-screen snapshot has no
 *  history, so no events are placed into it. */
export function chatRows(turns: ChatTurn[], events: TaskEvent[] = []): ChatRow[] {
  const snapshot = turns.some((t) => t.kind === 'terminal_snapshot');
  const start = turns.length ? turns[0].ts : Infinity;
  const pending = snapshot ? [] : events.filter((e) => e.ts >= start);
  const out: ChatRow[] = [];
  let ei = 0;
  const flush = (upTo: number) => {
    while (ei < pending.length && pending[ei].ts <= upTo) {
      const e = pending[ei++];
      const last = out[out.length - 1];
      if (e.kind === 'added' && last?.kind === 'added') last.events.push(e);
      else if (e.kind === 'added') out.push({ kind: 'added', key: e.key, events: [e] });
      else out.push({ kind: 'task', key: e.key, event: e });
    }
  };
  for (const turn of turns) {
    flush(turn.ts);
    const last = out[out.length - 1];
    if (turn.kind === 'peer_message' || turn.kind === 'goal_status') out.push({ kind: 'rule', turn });
    else if (turn.role === 'tool' && last?.kind === 'tools') last.turns.push(turn);
    else if (turn.role === 'tool') out.push({ kind: 'tools', key: turn.msg_id, turns: [turn], verbs: [] });
    else out.push({ kind: 'turn', turn });
  }
  flush(Infinity);
  for (const r of out) if (r.kind === 'tools') r.verbs = [...new Set(r.turns.map((t) => toolVerb(t.text)))];
  return out;
}

/** A "user" turn the harness wrote, not the human: a subagent's report handed back. Shown
 *  folded under a plain label, with the body a click away. (Turns that START with a tag, such
 *  as a background task's `<task-notification>`, never get here: the transcript reader drops
 *  them as system noise.) */
export interface Injected {
  label: string;
  /** The report itself, without the harness's framing. */
  body: string;
}

export function injectedTurn(text: string): Injected | null {
  const t = text.trimStart();
  if (t.startsWith('Another Claude session sent a message')) {
    const inner = t.match(/<agent-message[^>]*>([\s\S]*?)(<\/agent-message>|$)/)?.[1] ?? t;
    const after = inner.split(/The report follows:\s*\n/)[1] ?? inner;
    // The harness indents every line of the report by two spaces.
    const body = after.replace(/^ {2}/gm, '').trim();
    const handback = inner.includes('[Subagent hand-back]');
    return { label: handback ? 'Subagent report received' : 'Message from another session', body };
  }
  return null;
}

// ── Focus: the phone's inbox rules (maiLink inbox-view.ts isFocused / focusSince) ──

/** Local midnight at the start of yesterday: the oldest activity Focus still counts as recent.
 *  Day first, then midnight, so a zone whose DST starts at midnight can't carry an hour back. */
export function focusSince(now: number): number {
  const d = new Date(now);
  d.setDate(d.getDate() - 1);
  d.setHours(0, 0, 0, 0);
  return d.getTime();
}

export interface FocusChat {
  tabId: string;
  state: AgentState | null;
  unread: boolean;
  lastActivity: number;
  /** An open decision or action blocker on one of its tasks. */
  asks: boolean;
}

export interface FocusSections<T extends FocusChat> {
  needsYou: T[];
  working: T[];
  recent: T[];
}

/** Needs you (a permission prompt, or a question on one of its tasks), then working, then
 *  unread or active since the start of yesterday. Opening a chat never removes it: `openId`
 *  is always listed (in recent when it qualifies for nothing else), so a chat answered after
 *  its agent exited keeps a row to come back to. */
export function focusSections<T extends FocusChat>(chats: T[], now: number, openId?: string | null): FocusSections<T> {
  const since = focusSince(now);
  const needsYou = chats.filter((c) => c.state === 'permission' || c.asks);
  const working = chats.filter((c) => !needsYou.includes(c) && c.state === 'active');
  const recent = chats
    .filter(
      (c) => !needsYou.includes(c) && !working.includes(c) && (c.unread || c.lastActivity >= since || c.tabId === openId),
    )
    .sort((a, b) => b.lastActivity - a.lastActivity);
  return { needsYou, working, recent };
}
