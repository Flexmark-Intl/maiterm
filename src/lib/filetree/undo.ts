/**
 * The tree's undo stack and the words for an op's outcome (docs/file-tree.md §10.4).
 *
 * Pure, so the bookkeeping is tested on its own (undo.test.ts). The store owns one
 * `UndoStacks` per window; each root (`TreePlace.place`) has its own stack of 50, filled from
 * `OpResult.undo` and run by Cmd/Ctrl+Z while the tree has focus.
 *
 * An entry is dropped if its target changed on disk since: each undo op says what must still
 * be true for it to run (`undoExpectations`: what it acts on is there, where it puts things
 * back is free), and a change arriving from the watcher that breaks one marks the entry stale.
 * Changes this tree made itself are not "since": a later op of ours (renaming the file just
 * created) is undone first, LIFO, so it must not spoil the entry below it. Ours are the ops in
 * flight and the listings that arrive within `OWN_SETTLE_MS` of one settling (the hub re-lists
 * what an op touched, and the watcher's echo can follow it). The expectations are checked
 * against the model once more right before an undo runs, which catches a change made while
 * one of ours was settling; an undo pressed inside that window skips it, since the model may
 * not show our own last op yet. Rust refuses to clobber whatever this misses.
 *
 * A stale entry stays on the stack until Cmd+Z reaches it, so the human is told it can't be
 * undone instead of the press silently undoing something older.
 */
import type { FileOp, OpResult } from '$lib/tauri/types';

export const UNDO_MAX = 50;
/** How long after one of our ops settles a listing is still taken as its echo. */
export const OWN_SETTLE_MS = 1000;

export interface Expectation {
  rel: string;
  /** It must be there (what the undo acts on) or must not (where it puts something back). */
  exists: boolean;
}

export interface UndoEntry {
  /** The op that reverses the one done. */
  op: FileOp;
  /** What it undoes, for the footer and the announcement: "rename of “a.ts”". */
  label: string;
  expect: Expectation[];
  /** Why it can no longer run, once known. */
  stale: string | null;
}

/** `true`/`false` when the folder holding `rel` is listed, `undefined` when nobody knows. */
export type Lookup = (rel: string) => boolean | undefined;

function parentOf(rel: string): string {
  const i = rel.lastIndexOf('/');
  return i < 0 ? '' : rel.slice(0, i);
}

function nameOf(rel: string): string {
  const i = rel.lastIndexOf('/');
  return i < 0 ? rel : rel.slice(i + 1);
}

/** What must still hold for an undo op to run as recorded. */
export function undoExpectations(op: FileOp): Expectation[] {
  const src = op.src ?? [];
  switch (op.kind) {
    case 'rename': {
      if (src.length !== 1 || !op.name) return [];
      const back = parentOf(src[0]) ? `${parentOf(src[0])}/${op.name}` : op.name;
      const out: Expectation[] = [{ rel: src[0], exists: true }];
      // A case-only rename's way back names the same item: on a folding disk it "exists".
      if (back.toLowerCase() !== src[0].toLowerCase()) out.push({ rel: back, exists: false });
      return out;
    }
    case 'move': {
      const out: Expectation[] = src.map((rel) => ({ rel, exists: true }));
      (op.to ?? []).forEach((rel, i) => {
        if (rel.toLowerCase() !== (src[i] ?? '').toLowerCase()) out.push({ rel, exists: false });
      });
      return out;
    }
    case 'trash':
      return src.map((rel) => ({ rel, exists: true }));
    case 'restore':
      return src.map((rel) => ({ rel, exists: false }));
    default:
      return [];
  }
}

/** The first expectation the lookup contradicts, or null. Unknown folders never contradict. */
export function brokenExpectation(expect: Expectation[], lookup: Lookup): Expectation | null {
  for (const x of expect) {
    const v = lookup(x.rel);
    if (v !== undefined && v !== x.exists) return x;
  }
  return null;
}

export const STALE_CHANGED = 'it changed on disk since';

export class UndoStacks {
  private stacks = new Map<string, UndoEntry[]>();
  private inFlight = 0;
  private settledAt = Number.NEGATIVE_INFINITY;

  push(place: string, op: FileOp, label: string): UndoEntry {
    const entry: UndoEntry = { op, label, expect: undoExpectations(op), stale: null };
    const s = this.stacks.get(place) ?? [];
    s.push(entry);
    if (s.length > UNDO_MAX) s.splice(0, s.length - UNDO_MAX);
    this.stacks.set(place, s);
    return entry;
  }

  peek(place: string): UndoEntry | null {
    const s = this.stacks.get(place);
    return s?.length ? s[s.length - 1] : null;
  }

  pop(place: string): UndoEntry | null {
    return this.stacks.get(place)?.pop() ?? null;
  }

  size(place: string): number {
    return this.stacks.get(place)?.length ?? 0;
  }

  /** An op of ours started / finished: listings meanwhile and just after are its echo. */
  beginOp(): void {
    this.inFlight++;
  }

  endOp(now: number): void {
    this.inFlight = Math.max(0, this.inFlight - 1);
    this.settledAt = now;
  }

  isOwn(now: number): boolean {
    return this.inFlight > 0 || now - this.settledAt < OWN_SETTLE_MS;
  }

  /**
   * The folder `dir` changed on disk. `names` are the entries the change named (a delta's
   * adds, deletes and changes), or null for a whole re-listing. Entries whose expectations
   * in that folder no longer hold go stale, unless the change is one of ours.
   */
  noteChange(place: string, dir: string, names: string[] | null, lookup: Lookup, now: number): number {
    if (this.isOwn(now)) return 0;
    const s = this.stacks.get(place);
    if (!s) return 0;
    const named = names ? new Set(names) : null;
    let marked = 0;
    for (const e of s) {
      if (e.stale) continue;
      const here = e.expect.filter((x) => parentOf(x.rel) === dir && (!named || named.has(nameOf(x.rel))));
      if (here.length && brokenExpectation(here, lookup)) {
        e.stale = STALE_CHANGED;
        marked++;
      }
    }
    return marked;
  }

  /**
   * Right before running: anything the model now contradicts makes it stale. Not while one of
   * ours is in flight or settling: the model may still show the folder before it (the hub's
   * re-list arrives after the op answers), so a quick second Cmd+Z would read the first
   * undo's own result as a change on disk. Then only what `noteChange` already marked counts,
   * and Rust refuses whatever would clobber or is gone.
   */
  check(entry: UndoEntry, lookup: Lookup, now: number): string | null {
    if (!entry.stale && !this.isOwn(now) && brokenExpectation(entry.expect, lookup)) entry.stale = STALE_CHANGED;
    return entry.stale;
  }
}

// ── Words ───────────────────────────────────────────────────────────────────────────────────

/** Whose words an op's result is told in: this computer's file manager's, or `remote` for a
 *  root on an ssh host, whose trash is maiTerm's own folder there (`~/.maiterm/trash`, §15.2)
 *  whatever this computer is, and which Undo puts back on every client OS. */
export type Platform = 'mac' | 'windows' | 'linux' | 'remote';

/** Where trashed things go, in the platform's own word. */
export function trashPlace(p: Platform): string {
  if (p === 'remote') return 'the host’s trash (~/.maiterm/trash)';
  return p === 'windows' ? 'the Recycle Bin' : 'the Trash';
}

/** The trash menu entry's label, as each platform's file manager says it (§10.4). */
export function trashLabel(p: Platform): string {
  return p === 'windows' ? 'Delete' : 'Move to Trash';
}

function quoted(rels: string[]): string {
  return rels.length === 1 ? `“${nameOf(rels[0])}”` : `${rels.length} items`;
}

/** What an op did, for the footer and the live region. Null when nothing happened. */
export function describeResult(op: FileOp, r: OpResult, p: Platform): string | null {
  const done = r.done;
  const parts: string[] = [];
  if (done.length) {
    const from = done.map((d) => d.from);
    const to = done.map((d) => d.to ?? d.from);
    switch (op.kind) {
      case 'create_file': parts.push(`Created ${quoted(from)}`); break;
      case 'create_dir': parts.push(`Created folder ${quoted(from)}`); break;
      case 'rename': parts.push(`Renamed ${quoted(from)} to ${quoted(to)}`); break;
      case 'move': parts.push(`Moved ${quoted(from)}`); break;
      case 'copy': case 'import': parts.push(`Copied ${quoted(to)}`); break;
      case 'duplicate': parts.push(`Duplicated ${quoted(from)}`); break;
      case 'trash': parts.push(`Moved ${quoted(from)} to ${trashPlace(p)}`); break;
      case 'delete': parts.push(`Deleted ${quoted(from)} permanently`); break;
      case 'restore': parts.push(`Put back ${quoted(from)}`); break;
    }
  }
  if (r.skipped.length) parts.push(`${r.skipped.length} left as ${r.skipped.length === 1 ? 'it was' : 'they were'}`);
  if (r.failed.length) {
    const f = r.failed[0];
    parts.push(r.failed.length === 1 ? `“${nameOf(f.rel)}”: ${f.msg}` : `${r.failed.length} failed (“${nameOf(f.rel)}”: ${f.msg})`);
  }
  return parts.length ? parts.join(' · ') : null;
}

/** The undo entry's label: what Cmd+Z will reverse. */
export function undoLabel(op: FileOp, r: OpResult): string {
  const from = r.done.map((d) => d.from);
  switch (op.kind) {
    case 'create_file': return `new file ${quoted(from)}`;
    case 'create_dir': return `new folder ${quoted(from)}`;
    case 'rename': return `rename of ${quoted(from)}`;
    case 'move': return `move of ${quoted(from)}`;
    case 'copy': case 'import': return `copy of ${quoted(from)}`;
    case 'duplicate': return `duplicate of ${quoted(from)}`;
    case 'trash': return `trash of ${quoted(from)}`;
    default: return op.kind;
  }
}
