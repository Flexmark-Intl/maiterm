/**
 * What the agents did to the tree's rows (docs/file-tree.md §9.3, §9.4): the marks Rust sends
 * on a subscription (`agent` events), merged per row, and what a row, a folder, a chip and a
 * lens make of them.
 *
 * Pure on purpose (no Svelte, no IPC) so merging, lens filtering and colour assignment are
 * tested on their own (agents.test.ts). The store keeps one `AgentMarks` per root (on the
 * model, rel-keyed) and one `TurnTracker` per window (tab-keyed), and reads both through its
 * `agentVersion` counter, the way git decorations are read through `gitVersion` (§12.2).
 *
 * Rules this file keeps:
 * - Evidence is always named. A mark with `fs_during_tool` says "changed while ‹tab›'s Bash
 *   ran", never "‹tab› edited"; a row with no mark claims nothing about who changed it.
 * - Reads leave no lasting mark: they light for 2.7 s and are dropped. Everything else
 *   (edit, write, create, delete, changed, commit) stands for the session.
 * - "This turn" is the tab's newest turn as far as this window has seen: the newest turn on
 *   any of its marks, moved on by its prompt hook. A late mark from an earlier turn never
 *   counts as this turn's.
 * - Time-based looks (the pulse, the sweep, the read light) are CSS animations whose start is
 *   handed over as a time, so nothing here ticks; only the edge chips need to know when a row
 *   stops being lit, and `litUntil` says when.
 */
import type { AgentMark } from '$lib/tauri/types';
import { ancestorsOf, type LensItem, type RowKind } from './model';

/** How long a read or search lights its row (§9.3). */
export const READ_LIGHT_MS = 2700;
/** The sweep a completed write runs before settling into its dot (§9.3). */
export const SWEEP_MS = 1200;
/** Agent colours: `--ft-agent-0` … `--ft-agent-5` in app.css. */
export const PALETTE_SIZE = 6;

/** The rail's colour family: `--ft-read`, `--ft-write`, `--ft-create`, `--ft-delete`. */
export type OpClass = 'read' | 'write' | 'create' | 'delete';

export function opClass(op: string): OpClass {
  switch (op) {
    case 'read':
    case 'search':
      return 'read';
    case 'create':
    case 'commit':
      return 'create';
    case 'delete':
      return 'delete';
    default:
      return 'write';
  }
}

/** A mark that stands for the session (everything but a look at the file). */
export function isLasting(op: string): boolean {
  return op !== 'read' && op !== 'search';
}

/** A tab's turns as this window has seen them. */
export class TurnTracker {
  /** tab → its newest turn number (the ledger's own numbering, from the marks). */
  private turn = new Map<string, number>();
  /** tab → when its newest turn began and ended (null: still open), ms epoch. */
  private spans = new Map<string, { start: number; end: number | null }>();
  /** A counter of the edges heard (prompts, new sessions) and `clear()`s, never reset, and
   *  where each tab's newest edge and the last clear fell on it: what `seed` checks a read
   *  against. */
  private edgeSeq = 0;
  private lastEdge = new Map<string, number>();
  private clearedAt = 0;

  current(tabId: string): number {
    return this.turn.get(tabId) ?? 0;
  }

  isThisTurn(m: Pick<AgentMark, 'tab_id' | 'turn'>): boolean {
    return m.turn >= this.current(m.tab_id);
  }

  /** A mark arrived: a turn newer than any seen began (its prompt may never have reached
   *  this window: maiTerm started mid-turn, or the column was closed). */
  noteMark(m: AgentMark): void {
    const cur = this.turn.get(m.tab_id);
    if (cur === undefined || m.turn > cur) {
      this.turn.set(m.tab_id, m.turn);
      this.spans.set(m.tab_id, { start: m.at_ms, end: null });
    } else if (m.turn === cur) {
      const s = this.spans.get(m.tab_id);
      if (s && m.at_ms < s.start) s.start = m.at_ms;
    }
  }

  /** The tab's prompt hook: a new turn starts now, so nothing seen so far is this turn's.
   *  `turn` is the ledger's number for it, sent with the hook: taken as is, it can't drift
   *  from a `seed` that raced the prompt, whichever lands first. A count of one on from what
   *  this window has seen could (an unseeded tab counts from 0). Without it, the count. */
  turnStarted(tabId: string, at: number, turn?: number | null): void {
    this.lastEdge.set(tabId, ++this.edgeSeq);
    const cur = this.turn.get(tabId);
    if (turn == null) {
      this.turn.set(tabId, (cur ?? 0) + 1);
      this.spans.set(tabId, { start: at, end: null });
      return;
    }
    // Already there: a seed read after the ledger recorded this prompt, or a mark of the turn
    // that beat its prompt's event. Older: a late event, never a step back.
    if (cur !== undefined && cur >= turn) return;
    this.turn.set(tabId, turn);
    this.spans.set(tabId, { start: at, end: null });
  }

  /** The tab's Stop hook: its turn is over. */
  turnEnded(tabId: string, at: number): void {
    const s = this.spans.get(tabId);
    if (s && s.end === null) s.end = at;
  }

  /** The newest turn's span. `end` is null while open; for a turn whose end was never seen,
   *  `open` (is the agent working now?) decides, else its last mark is the best end there is. */
  span(tabId: string, lastAt: number, open: boolean): { start: number; end: number } | null {
    const s = this.spans.get(tabId);
    if (!s) return null;
    return { start: s.start, end: s.end ?? (open ? Infinity : lastAt) };
  }

  /** A new session on the tab (not a compaction): the ledger starts its numbering again. */
  forget(tabId: string): void {
    this.lastEdge.set(tabId, ++this.edgeSeq);
    this.turn.delete(tabId);
    this.spans.delete(tabId);
  }

  /** Where the edges stand, taken as a `agentFilesTurns` read is sent and handed to `seed`. */
  edgeMark(): number {
    return this.edgeSeq;
  }

  /** The ledger's own count for the tab, read when the column opens (`agentFilesTurns`): the
   *  marks carry only turns something was written in, so a turn that has only read is known
   *  from here alone. A newer turn seen since the read (a mark, a prompt hook) wins.
   *
   *  `readAt` is `edgeMark()` from when the read was sent. A tab with a prompt or a new session
   *  heard since then is not seeded at all: the read can't be ordered against the hook's event
   *  (they travel apart), and after a new session its higher number is the stale one, which
   *  the comparison below would keep for good. Nor is anything after a `clear()`. */
  seed(tabId: string, turn: number, start: number, end: number | null, readAt?: number): void {
    if (readAt !== undefined && (this.clearedAt > readAt || (this.lastEdge.get(tabId) ?? 0) > readAt)) return;
    const cur = this.turn.get(tabId);
    if (cur !== undefined && cur > turn) return;
    const s = cur === turn ? this.spans.get(tabId) : undefined;
    this.turn.set(tabId, turn);
    // A Stop heard after the read already ended the turn the read still saw open.
    this.spans.set(tabId, { start, end: end ?? s?.end ?? null });
  }

  /** Forget every tab: what was learned can no longer be kept in step with the ledger. */
  clear(): void {
    this.turn.clear();
    this.spans.clear();
    this.lastEdge.clear();
    // A read sent before this lands on a tracker it no longer describes: it seeds nothing.
    this.clearedAt = ++this.edgeSeq;
  }

  remap(oldId: string, newId: string): void {
    const t = this.turn.get(oldId);
    if (t !== undefined) {
      this.turn.delete(oldId);
      this.turn.set(newId, t);
    }
    const s = this.spans.get(oldId);
    if (s) {
      this.spans.delete(oldId);
      this.spans.set(newId, s);
    }
    const e = this.lastEdge.get(oldId);
    if (e !== undefined) {
      this.lastEdge.delete(oldId);
      this.lastEdge.set(newId, e);
    }
  }
}

/** What a row draws for the agents (§9.3). */
export interface RowAgentView {
  /** The 2 px rail, when it has something to say now. `t0` is the mark's time, for the CSS
   *  animation's start (`mode` pending pulses, sweep and read fade, the rest are steady). */
  rail: { cls: OpClass; mode: 'pending' | 'sweep' | 'read' | 'failed' | 'deleted'; t0: number } | null;
  /** Tabs that wrote it this turn (not failed), most recent first: the dot is the first's
   *  colour, and two or more split the rail. */
  writers: string[];
  /** The newest lasting mark is a delete that went through this turn: the name is struck. */
  deleted: boolean;
  /** A write is running on it now ("editing now"). */
  editing: boolean;
  /** Pulsing, sweeping or lit: until when (Infinity while a call is running), else null. */
  litUntil: number | null;
  /** Its lasting marks this session, newest first (for the tooltip's words). */
  marks: AgentMark[];
}

const EMPTY: RowAgentView = { rail: null, writers: [], deleted: false, editing: false, litUntil: null, marks: [] };

/** One root's marks, by row. */
export class AgentMarks {
  /** rel → slot → mark. A slot is a tab, its (sub)agent and whether the mark is a read, so a
   *  read after an edit lights the row without wiping the edit's "written this turn". */
  byRel = new Map<string, Map<string, AgentMark>>();

  get size(): number {
    return this.byRel.size;
  }

  clear(): void {
    this.byRel.clear();
  }

  /** Apply an `agent` event: each mark replaces its slot's older one. Returns the rels it
   *  touched. Reads older than their light are dropped on the way. */
  merge(marks: AgentMark[], now: number): string[] {
    const touched: string[] = [];
    for (const m of marks) {
      if (!m.rel) continue;
      const read = !isLasting(m.op);
      const slot = `${m.tab_id}\u0000${m.agent}\u0000${read ? 'r' : 'w'}`;
      let slots = this.byRel.get(m.rel);
      if (!slots) this.byRel.set(m.rel, (slots = new Map()));
      const old = slots.get(slot);
      // Marks for one slot arrive in order, but a primed (standing) mark may come after a
      // live one: never let an older mark replace a newer.
      if (old && old.at_ms > m.at_ms && !(old.phase === 'pending' && m.phase !== 'pending')) continue;
      slots.set(slot, m);
      touched.push(m.rel);
    }
    this.pruneReads(now);
    return touched;
  }

  /** Drop reads whose light is over; they never leave a lasting mark. */
  pruneReads(now: number): void {
    for (const [rel, slots] of this.byRel) {
      for (const [k, m] of slots) {
        if (!isLasting(m.op) && m.phase !== 'pending' && now - m.at_ms > READ_LIGHT_MS) slots.delete(k);
      }
      if (slots.size === 0) this.byRel.delete(rel);
    }
  }

  /** A tab's marks follow it to the new id a reload gives it. */
  remapTab(oldId: string, newId: string): void {
    for (const slots of this.byRel.values()) {
      for (const [k, m] of [...slots]) {
        if (m.tab_id !== oldId) continue;
        slots.delete(k);
        slots.set(`${newId}${k.slice(oldId.length)}`, { ...m, tab_id: newId });
      }
    }
  }

  /** A tab's new session starts afresh (the ledger forgot its history too). */
  forgetTab(tabId: string): void {
    for (const [rel, slots] of this.byRel) {
      for (const [k, m] of slots) if (m.tab_id === tabId) slots.delete(k);
      if (slots.size === 0) this.byRel.delete(rel);
    }
  }

  marksOf(rel: string): AgentMark[] {
    const slots = this.byRel.get(rel);
    return slots ? [...slots.values()] : [];
  }

  /** Tabs with any mark here. */
  tabs(): Set<string> {
    const out = new Set<string>();
    for (const slots of this.byRel.values()) for (const m of slots.values()) out.add(m.tab_id);
    return out;
  }
}

/** A row's look at `now`. */
export function rowView(marks: AgentMark[], now: number, turns: TurnTracker): RowAgentView {
  if (marks.length === 0) return EMPTY;
  const lasting = marks.filter((m) => isLasting(m.op)).sort((a, b) => b.at_ms - a.at_ms);
  const thisTurn = lasting.filter((m) => turns.isThisTurn(m));
  const writers: string[] = [];
  for (const m of thisTurn) if (m.phase !== 'failed' && !writers.includes(m.tab_id)) writers.push(m.tab_id);

  const pending = thisTurn.find((m) => m.phase === 'pending');
  const newest = thisTurn[0];
  const deleted = !!newest && newest.op === 'delete' && newest.phase === 'ok';
  const reads = marks.filter((m) => !isLasting(m.op)).sort((a, b) => b.at_ms - a.at_ms);
  const read = reads.find((m) => m.phase === 'pending' || now - m.at_ms < READ_LIGHT_MS);

  let rail: RowAgentView['rail'] = null;
  let litUntil: number | null = null;
  if (pending) {
    rail = { cls: opClass(pending.op), mode: 'pending', t0: pending.at_ms };
    litUntil = Infinity;
  } else if (newest && newest.phase === 'ok' && now - newest.at_ms < SWEEP_MS) {
    rail = { cls: opClass(newest.op), mode: 'sweep', t0: newest.at_ms };
    litUntil = newest.at_ms + SWEEP_MS;
  } else if (read) {
    rail = { cls: 'read', mode: 'read', t0: read.at_ms };
    litUntil = read.phase === 'pending' ? Infinity : read.at_ms + READ_LIGHT_MS;
  } else if (newest && newest.phase === 'failed') {
    rail = { cls: opClass(newest.op), mode: 'failed', t0: newest.at_ms };
  } else if (deleted) {
    rail = { cls: 'delete', mode: 'deleted', t0: newest.at_ms };
  }
  return { rail, writers, deleted, editing: !!pending, litUntil, marks: lasting };
}

/** Collapsed folders over activity (§9.3): per folder, how many files under it were changed
 *  this turn and by whom most. Never expands anything; the row decides whether to show it. */
export function folderBadges(marks: AgentMarks, turns: TurnTracker): Map<string, { count: number; tab: string }> {
  const perFolder = new Map<string, Map<string, number>>();
  const totals = new Map<string, number>();
  for (const [rel, slots] of marks.byRel) {
    const tabs = new Set<string>();
    for (const m of slots.values()) {
      if (isLasting(m.op) && m.phase !== 'failed' && turns.isThisTurn(m)) tabs.add(m.tab_id);
    }
    if (tabs.size === 0) continue;
    for (const dir of ancestorsOf(rel)) {
      totals.set(dir, (totals.get(dir) ?? 0) + 1);
      let byTab = perFolder.get(dir);
      if (!byTab) perFolder.set(dir, (byTab = new Map()));
      for (const t of tabs) byTab.set(t, (byTab.get(t) ?? 0) + 1);
    }
  }
  const out = new Map<string, { count: number; tab: string }>();
  for (const [dir, byTab] of perFolder) {
    let best = '';
    let most = -1;
    for (const [t, n] of byTab) if (n > most) [best, most] = [t, n];
    out.set(dir, { count: totals.get(dir)!, tab: best });
  }
  return out;
}

/** Files changed this turn per tab, for the agent strip's chips. */
export function changedThisTurn(marks: AgentMarks, turns: TurnTracker): Map<string, number> {
  const out = new Map<string, number>();
  for (const slots of marks.byRel.values()) {
    const tabs = new Set<string>();
    for (const m of slots.values()) {
      if (isLasting(m.op) && m.phase !== 'failed' && turns.isThisTurn(m)) tabs.add(m.tab_id);
    }
    for (const t of tabs) out.set(t, (out.get(t) ?? 0) + 1);
  }
  return out;
}

/**
 * The Turn and Session lenses (§9.4): every path an agent wrote (or that changed on disk while
 * its shell ran, or that it committed), this turn or this session, for one tab (`tab`) or all
 * of them. A call that failed changed nothing and is left out; one still running is in.
 */
export function agentLensItems(
  marks: AgentMarks,
  turns: TurnTracker,
  scope: 'turn' | 'session',
  tab: string | null,
  kindOf: (rel: string) => RowKind | undefined,
): LensItem[] {
  const out: LensItem[] = [];
  for (const [rel, slots] of marks.byRel) {
    let hit = false;
    for (const m of slots.values()) {
      if (!isLasting(m.op) || m.phase === 'failed') continue;
      if (tab !== null && m.tab_id !== tab) continue;
      if (scope === 'turn' && !turns.isThisTurn(m)) continue;
      hit = true;
      break;
    }
    if (hit) out.push({ rel, kind: kindOf(rel) ?? 'f' });
  }
  return out;
}

/**
 * Two agents wrote this file in turns that overlapped (§9.3's conflict badge): the pair, most
 * recent writer first, or null. `turnSpan` gives each tab's newest turn as a time span.
 */
export function overlappingWriters(
  marks: AgentMark[],
  turns: TurnTracker,
  turnSpan: (tabId: string) => { start: number; end: number } | null,
): [string, string] | null {
  const writers: string[] = [];
  for (const m of [...marks].sort((a, b) => b.at_ms - a.at_ms)) {
    if (!isLasting(m.op) || m.phase === 'failed' || !turns.isThisTurn(m)) continue;
    // A change seen on disk while a shell ran, or a commit, is evidence about the file, not a
    // second writer racing the first.
    if (m.evidence !== 'tool') continue;
    if (!writers.includes(m.tab_id)) writers.push(m.tab_id);
  }
  for (let i = 0; i < writers.length; i++) {
    for (let j = i + 1; j < writers.length; j++) {
      const a = turnSpan(writers[i]);
      const b = turnSpan(writers[j]);
      if (a && b && a.start <= b.end && b.start <= a.end) return [writers[i], writers[j]];
    }
  }
  return null;
}

/**
 * Agent colours (§9.3): stable per tab while it is present. A tab keeps the index it had; a
 * new one, taken in the window's agent order, gets the lowest index nobody present holds, and
 * past six the least used. Tabs no longer present give theirs back.
 */
export function assignColours(prev: ReadonlyMap<string, number>, ordered: string[]): Map<string, number> {
  const out = new Map<string, number>();
  const present = new Set(ordered);
  for (const [t, c] of prev) if (present.has(t)) out.set(t, c);
  for (const t of ordered) {
    if (out.has(t)) continue;
    const used = new Array<number>(PALETTE_SIZE).fill(0);
    for (const c of out.values()) used[c]++;
    let pick = 0;
    for (let c = 1; c < PALETTE_SIZE; c++) if (used[c] < used[pick]) pick = c;
    out.set(t, pick);
  }
  return out;
}

/** "editing now", "edited", "changed on disk while ‹tab›'s Bash ran": a mark in words, with
 *  its evidence, for tooltips. `who` names the tab; `tool` is the call's tool when known. */
export function markWords(m: AgentMark, who: string, thisTurn: boolean, tool?: string | null): string {
  const when = thisTurn ? 'this turn' : 'earlier this session';
  if (m.evidence === 'fs_during_tool') return `changed on disk while ${who}’s ${tool || 'shell command'} ran (${when})`;
  if (m.evidence === 'commit') return `committed during ${who}’s turn (${when})`;
  const verb: Record<string, string> = { edit: 'edited', write: 'written', create: 'created', delete: 'deleted', changed: 'changed', commit: 'committed' };
  const v = verb[m.op] ?? m.op;
  const by = tool ? `${who} (${tool} tool)` : who;
  if (m.phase === 'pending') return `being ${v} by ${by} now`;
  if (m.phase === 'failed') return `${by} tried to change it and failed (${when})`;
  return `${v} by ${by} ${when}`;
}

/** An agent tab's folder is inside the root (or is it): the agent strip's test, on the paths
 *  as given (no IPC per tab, §4.3). Component-wise, case-folded where the disk folds. */
export function cwdInRoot(cwd: string, roots: string[], sep: string, caseInsensitive: boolean): boolean {
  const norm = (p: string) => {
    let s = p.replace(/[\\/]+$/, '');
    if (caseInsensitive) s = s.toLowerCase();
    return s;
  };
  const c = norm(cwd);
  for (const r of roots) {
    const root = norm(r);
    if (!root) continue;
    if (c === root || c.startsWith(root + sep) || (sep !== '/' && c.startsWith(root + '/'))) return true;
  }
  return false;
}
