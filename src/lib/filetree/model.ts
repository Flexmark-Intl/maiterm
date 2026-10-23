/**
 * The Files tree's view model (docs/file-tree.md §12): folder data as Rust sent it, and the
 * flattened rows the virtual list draws.
 *
 * Pure on purpose — no Svelte, no IPC — so the splice and delta algebra is tested on its own
 * (model.test.ts). The store owns one per root and copies `rows` into a `$state.raw` after
 * each batch.
 *
 * Rules this file keeps:
 * - Rust owns the truth and the order. A folder's names are kept exactly as the wire sent
 *   them, and a delta says where each new name lands (`at`); nothing here sorts.
 * - `rows` is never mutated in place: every change builds a new array by splicing a copy, so
 *   the store can hand it to `$state.raw` and the list re-renders by identity.
 * - A full re-flatten happens only on a filter toggle. Expand, collapse, a listing and a delta
 *   each touch only the rows of the folder they are about.
 * - Generations are the convergence protocol (§5.3): a delta applies only to the generation
 *   it was computed from, a chunk only to the listing it belongs to. Anything else answers
 *   'resync' and the store asks for the whole folder again, so a missed message heals
 *   instead of drifting.
 */
import type { DirReq, GitLetter, GitUpdate, TreeEntry, TreeEvent } from '$lib/tauri/types';
import { TREE_FLAG_HIDDEN, TREE_FLAG_IGNORED, TREE_FLAG_LOOP } from '$lib/tauri/types';

/** 'm' is a message row: "Loading…", an error, "50,000 of N shown". Never focusable. */
export type RowKind = TreeEntry['k'] | 'm';

export interface Row {
  /** Root-relative POSIX path; a message row's key is its folder's key plus a NUL suffix. */
  key: string;
  /** 0 for the root's children. */
  depth: number;
  name: string;
  kind: RowKind;
  flags: number;
  /** Message rows only: what the row says, for styling. */
  msg?: 'loading' | 'error' | 'truncated' | 'slow' | 'empty';
}

/** One change to `rows`, already applied: `rows.splice(start, deleteCount, ...rows)`. */
export interface Splice {
  start: number;
  deleteCount: number;
  rows: Row[];
}

export interface DirData {
  gen: number;
  /** In the wire's order. */
  names: string[];
  entries: Map<string, TreeEntry>;
  /** 'loading' until the last chunk of a listing; 'error' after 'denied'. */
  state: 'loading' | 'ok' | 'error';
  err?: string;
  /** A network folder that hasn't answered yet ("Not responding…"). Cleared by its listing. */
  slow?: string;
  truncated: boolean;
  total: number;
  /** The listing being received: its generation and the next chunk expected. */
  building: { gen: number; next: number } | null;
}

type DirEvent = Extract<TreeEvent, { t: 'dir' }>;
type DeltaEvent = Extract<TreeEvent, { t: 'delta' }>;
type NotModifiedEvent = Extract<TreeEvent, { t: 'not_modified' }>;
type DirErrorEvent = Extract<TreeEvent, { t: 'dir_error' }>;

/** More changes than this in one delta re-render the folder's rows instead of splicing each:
 *  every splice copies `rows`, and a big burst into a 100k-row tree would copy it per name. */
const SPLICE_LIMIT = 32;

/** Where the truncated note sits in a capped folder (§5.3). */
export const MAX_ENTRIES = 50_000;

export function joinRel(parent: string, name: string): string {
  return parent ? `${parent}/${name}` : name;
}

export function parentRel(rel: string): string {
  const i = rel.lastIndexOf('/');
  return i < 0 ? '' : rel.slice(0, i);
}

export function baseName(rel: string): string {
  const i = rel.lastIndexOf('/');
  return i < 0 ? rel : rel.slice(i + 1);
}

/** `a/b/c` → `['a', 'a/b']`: the folders that must be open for `rel` to have a row. */
export function ancestorsOf(rel: string): string[] {
  const out: string[] = [];
  let i = rel.indexOf('/');
  while (i >= 0) {
    out.push(rel.slice(0, i));
    i = rel.indexOf('/', i + 1);
  }
  return out;
}

/** True when `rel` is `dir` or lies inside it. */
export function isWithin(rel: string, dir: string): boolean {
  return dir === '' || rel === dir || rel.startsWith(dir + '/');
}

export function isFolderKind(k: RowKind): boolean {
  return k === 'd' || k === 'L';
}

/** A folder or link to one, and not a loop back to an ancestor (shown, never opened). */
export function isExpandable(e: { k: RowKind; f: number } | { kind: RowKind; flags: number }): boolean {
  const k = 'k' in e ? e.k : e.kind;
  const f = 'f' in e ? e.f : e.flags;
  return isFolderKind(k) && !(f & TREE_FLAG_LOOP);
}

export class TreeModel {
  dirs = new Map<string, DirData>();
  /** Folders the human opened. Kept for a folder inside a collapsed one, so reopening the
   *  parent brings its open children back as they were. '' (the root) is always in it. */
  expanded = new Set<string>(['']);
  /** Folders Rust is following for this view: open AND every ancestor open. A subset of
   *  `expanded`. Collapsing a folder drops it and its open descendants from here (so a
   *  collapsed subtree costs no watches on Linux), not from `expanded`. */
  interest = new Set<string>(['']);
  rows: Row[] = [];
  /** Folders a delta deleted while Rust was following them: the store drains this into
   *  `ftCollapse` so the hub stops tracking a path that no longer exists. */
  pendingClose: string[] = [];
  /** Git decorations as Rust sent them (§7.3): changed files (a wholly untracked folder's key
   *  ends in '/') and folders' rolled-up severity. A git update never touches `rows`: only
   *  the mounted rows read these, through the store's `gitVersion`. */
  gitFiles = new Map<string, GitLetter>();
  gitDirs = new Map<string, GitLetter>();
  /** Past 20k changed files Rust sends folders only. */
  gitTruncated = false;
  /** Git keys are NFC while rows carry the on-disk name (`OpenAck.nfc_names`, macOS). */
  nfcNames = false;
  showHidden: boolean;
  showIgnored: boolean;

  constructor(opts: { showHidden?: boolean; showIgnored?: boolean } = {}) {
    this.showHidden = opts.showHidden ?? true;
    this.showIgnored = opts.showIgnored ?? true;
    this.rows = this.childRows('', 0);
  }

  // ── Rendering ─────────────────────────────────────────────────────────────────────────

  visible(e: TreeEntry): boolean {
    if (!this.showHidden && e.f & TREE_FLAG_HIDDEN) return false;
    if (!this.showIgnored && e.f & TREE_FLAG_IGNORED) return false;
    return true;
  }

  private entryRow(key: string, depth: number, e: TreeEntry): Row {
    return { key, depth, name: e.n, kind: e.k, flags: e.f };
  }

  private msgRow(rel: string, depth: number, msg: NonNullable<Row['msg']>, name: string): Row {
    return { key: `${rel}\u0000${msg}`, depth, name, kind: 'm', flags: 0, msg };
  }

  /** The rows for one entry: itself, then its subtree when it is an open folder. */
  private pushEntry(out: Row[], rel: string, depth: number, name: string): void {
    const d = this.dirs.get(rel);
    const e = d?.entries.get(name);
    if (!e || !this.visible(e)) return;
    const key = joinRel(rel, name);
    out.push(this.entryRow(key, depth, e));
    if (isExpandable(e) && this.expanded.has(key)) out.push(...this.childRows(key, depth + 1));
  }

  /** Every row under an open folder, recursively, as it should be drawn now. */
  childRows(rel: string, depth: number): Row[] {
    const out: Row[] = [];
    const d = this.dirs.get(rel);
    if (!d) {
      out.push(this.msgRow(rel, depth, 'loading', 'Loading…'));
      return out;
    }
    if (d.state === 'error') {
      out.push(this.msgRow(rel, depth, 'error', d.err || 'Can’t read this folder'));
      return out;
    }
    for (const name of d.names) this.pushEntry(out, rel, depth, name);
    if (d.names.length === 0) {
      if (d.slow) out.push(this.msgRow(rel, depth, 'slow', d.slow));
      else if (d.state === 'loading') out.push(this.msgRow(rel, depth, 'loading', 'Loading…'));
    }
    if (d.truncated && d.state === 'ok') {
      out.push(this.msgRow(rel, depth, 'truncated', `${d.names.length.toLocaleString()} of ${d.total.toLocaleString()} shown · search to find more`));
    }
    return out;
  }

  /** Re-derive every row. Only for a filter change: everything else splices. */
  flatten(): void {
    this.rows = this.childRows('', 0);
  }

  setFilters(showHidden: boolean, showIgnored: boolean): boolean {
    if (showHidden === this.showHidden && showIgnored === this.showIgnored) return false;
    this.showHidden = showHidden;
    this.showIgnored = showIgnored;
    this.flatten();
    return true;
  }

  // ── Row ranges ────────────────────────────────────────────────────────────────────────

  indexOf(key: string, from = 0): number {
    const rows = this.rows;
    for (let i = from; i < rows.length; i++) if (rows[i].key === key) return i;
    return -1;
  }

  /** The folder's rows are on screen: it is the root, or its row exists and it is open. */
  isShown(rel: string): boolean {
    return rel === '' || (this.expanded.has(rel) && this.indexOf(rel) >= 0);
  }

  /** `[start, end)` of the rows under an open, shown folder, and their depth. Null when the
   *  folder has no row (an ancestor is closed or filtered out). */
  childRange(rel: string): { start: number; end: number; depth: number } | null {
    if (rel === '') return { start: 0, end: this.rows.length, depth: 0 };
    const i = this.indexOf(rel);
    if (i < 0) return null;
    const d = this.rows[i].depth;
    let end = i + 1;
    while (end < this.rows.length && this.rows[end].depth > d) end++;
    return { start: i + 1, end, depth: d + 1 };
  }

  private splice(start: number, deleteCount: number, rows: Row[], log: Splice[]): void {
    if (deleteCount === 0 && rows.length === 0) return;
    const next = this.rows.slice(0, start);
    for (const r of rows) next.push(r);
    for (let i = start + deleteCount; i < this.rows.length; i++) next.push(this.rows[i]);
    this.rows = next;
    log.push({ start, deleteCount, rows });
  }

  /** Replace everything under an open folder with what it should show now. */
  private rerender(rel: string, log: Splice[]): void {
    if (rel !== '' && !this.expanded.has(rel)) return;
    const range = this.childRange(rel);
    if (!range) return;
    this.splice(range.start, range.end - range.start, this.childRows(rel, range.depth), log);
  }

  // ── Expand / collapse ─────────────────────────────────────────────────────────────────

  private reqFor(rel: string): DirReq {
    const d = this.dirs.get(rel);
    return { rel, known_gen: d && d.state === 'ok' ? d.gen : null };
  }

  /** The open folders reachable from `rel` through open folders, `rel` first. */
  private reachableOpen(rel: string): string[] {
    const out = [rel];
    const walk = (dir: string) => {
      const d = this.dirs.get(dir);
      if (!d) return;
      for (const name of d.names) {
        const e = d.entries.get(name);
        if (!e || !isExpandable(e)) continue;
        const key = joinRel(dir, name);
        if (this.expanded.has(key)) {
          out.push(key);
          walk(key);
        }
      }
    };
    walk(rel);
    return out;
  }

  /** Open a folder. `open` is what Rust must follow now, with the generations held, so an
   *  unchanged folder answers `not_modified` instead of a re-list. */
  expand(rel: string): { splices: Splice[]; open: DirReq[] } {
    const splices: Splice[] = [];
    if (rel === '' || this.expanded.has(rel)) {
      // Already open (or the root): make sure Rust follows it if every ancestor is open too.
      const open = this.interest.has(rel) || !ancestorsOf(rel).every((a) => this.expanded.has(a))
        ? [] : this.follow(rel);
      return { splices, open };
    }
    this.expanded.add(rel);
    const i = this.indexOf(rel);
    if (i < 0) return { splices, open: [] }; // under a closed folder: remembered, not shown
    const row = this.rows[i];
    if (!isExpandable(row)) {
      this.expanded.delete(rel);
      return { splices, open: [] };
    }
    this.splice(i + 1, 0, this.childRows(rel, row.depth + 1), splices);
    return { splices, open: this.follow(rel) };
  }

  private follow(rel: string): DirReq[] {
    const open: DirReq[] = [];
    for (const r of this.reachableOpen(rel)) {
      if (this.interest.has(r)) continue;
      this.interest.add(r);
      open.push(this.reqFor(r));
    }
    return open;
  }

  /** Close a folder. `close` is what Rust can stop following: the folder and every open
   *  folder below it. They stay in `expanded` (and keep their data and generation). */
  collapse(rel: string): { splices: Splice[]; close: string[] } {
    const splices: Splice[] = [];
    if (rel === '' || !this.expanded.has(rel)) return { splices, close: [] };
    const range = this.childRange(rel);
    this.expanded.delete(rel);
    if (range) this.splice(range.start, range.end - range.start, [], splices);
    const close: string[] = [];
    for (const r of [...this.interest]) {
      if (r !== '' && isWithin(r, rel)) {
        this.interest.delete(r);
        close.push(r);
      }
    }
    return { splices, close };
  }

  /** Collapse All: only the root stays open. */
  collapseAll(): { close: string[] } {
    const close = [...this.interest].filter((r) => r !== '');
    this.expanded = new Set(['']);
    this.interest = new Set(['']);
    this.rows = this.childRows('', 0);
    return { close };
  }

  /** Open every folder above `rel` so it gets a row once their listings arrive. */
  reveal(rel: string): { splices: Splice[]; open: DirReq[] } {
    const splices: Splice[] = [];
    const open: DirReq[] = [];
    for (const a of ancestorsOf(rel)) {
      const r = this.expand(a);
      splices.push(...r.splices);
      open.push(...r.open);
      // An ancestor whose row isn't there yet (its parent hasn't listed): remember it open,
      // and follow it so it lists as soon as the parent has.
      if (!this.interest.has(a) && ancestorsOf(a).every((x) => this.expanded.has(x))) {
        this.expanded.add(a);
        this.interest.add(a);
        open.push(this.reqFor(a));
      }
    }
    return { splices, open };
  }

  // ── Events ────────────────────────────────────────────────────────────────────────────

  applyDir(ev: DirEvent): Splice[] | 'resync' {
    const log: Splice[] = [];
    if (!this.interest.has(ev.rel)) return log;
    let d = this.dirs.get(ev.rel);
    if (ev.chunk === 0) {
      d = {
        gen: ev.gen,
        names: ev.entries.map((e) => e.n),
        entries: new Map(ev.entries.map((e) => [e.n, e])),
        state: ev.last ? 'ok' : 'loading',
        truncated: ev.truncated,
        total: ev.total,
        building: ev.last ? null : { gen: ev.gen, next: 1 },
      };
      this.dirs.set(ev.rel, d);
      this.rerender(ev.rel, log);
      return log;
    }
    if (!d || !d.building || d.building.gen !== ev.gen || d.building.next !== ev.chunk) {
      if (d) d.building = null;
      return 'resync';
    }
    d.building.next++;
    const start = d.names.length;
    for (const e of ev.entries) {
      d.names.push(e.n);
      d.entries.set(e.n, e);
    }
    d.truncated = ev.truncated;
    d.total = ev.total;
    if (ev.last) {
      d.building = null;
      d.state = 'ok';
      d.slow = undefined;
      this.rerender(ev.rel, log);
      return log;
    }
    // A middle chunk: its rows go at the end of the folder's range, so a 50k folder paints
    // as chunks land without re-rendering what is already there.
    if (!this.isShown(ev.rel)) return log;
    const range = this.childRange(ev.rel);
    if (!range) return log;
    let at = range.end;
    while (at > range.start && this.rows[at - 1].kind === 'm' && this.rows[at - 1].depth === range.depth) at--;
    const rows: Row[] = [];
    for (let i = start; i < d.names.length; i++) this.pushEntry(rows, ev.rel, range.depth, d.names[i]);
    // A loading placeholder (chunk 0 was empty) goes once there are rows.
    this.splice(at, range.end - at, rows, log);
    return log;
  }

  applyNotModified(ev: NotModifiedEvent): Splice[] | 'resync' {
    if (!this.interest.has(ev.rel)) return [];
    const d = this.dirs.get(ev.rel);
    return d && d.gen === ev.gen && d.state === 'ok' ? [] : 'resync';
  }

  applyDelta(ev: DeltaEvent): Splice[] | 'resync' {
    const log: Splice[] = [];
    if (!this.interest.has(ev.rel)) return log;
    const d = this.dirs.get(ev.rel);
    if (!d || d.state !== 'ok' || d.gen !== ev.from || ev.at.length !== ev.add.length + ev.chg.length) return 'resync';

    const placed = [...ev.add, ...ev.chg];
    const gone = new Set<string>(ev.del);
    for (const e of ev.chg) gone.add(e.n);
    const removedRows = new Set<string>();
    for (const n of gone) {
      const old = d.entries.get(n);
      if (old && this.visible(old)) removedRows.add(n);
    }

    // A deleted folder takes its open descendants with it. (A changed one keeps them: a
    // flag change on an open folder must not close it.)
    for (const n of ev.del) {
      const key = joinRel(ev.rel, n);
      for (const r of [...this.expanded]) if (r !== '' && isWithin(r, key)) this.expanded.delete(r);
      for (const r of [...this.interest]) {
        if (r !== '' && isWithin(r, key)) {
          this.interest.delete(r);
          this.pendingClose.push(r);
        }
      }
      for (const r of [...this.dirs.keys()]) if (r !== '' && isWithin(r, key)) this.dirs.delete(r);
    }

    // The data, exactly as Rust has it now.
    const kept = d.names.filter((n) => !gone.has(n));
    for (const n of gone) d.entries.delete(n);
    const order = placed.map((e, i) => ({ e, at: ev.at[i] })).sort((a, b) => a.at - b.at);
    for (const { e, at } of order) {
      kept.splice(Math.min(at, kept.length), 0, e.n);
      d.entries.set(e.n, e);
    }
    d.names = kept;
    d.gen = ev.gen;
    d.total = kept.length;

    if (!this.isShown(ev.rel)) return log;
    if (removedRows.size + placed.length > SPLICE_LIMIT) {
      this.rerender(ev.rel, log);
      return log;
    }
    const range = this.childRange(ev.rel);
    if (!range) return log;

    // Removals: each row and its subtree, scanning only this folder's direct children.
    let end = range.end;
    for (const n of removedRows) {
      const key = joinRel(ev.rel, n);
      const i = this.directChild(range.start, end, range.depth, key);
      if (i < 0) continue;
      let j = i + 1;
      while (j < end && this.rows[j].depth > range.depth) j++;
      this.splice(i, j - i, [], log);
      end -= j - i;
    }
    // Insertions: before the row of the next visible sibling in the new order.
    for (const { e } of order) {
      if (!this.visible(e)) continue;
      const rows: Row[] = [];
      this.pushEntry(rows, ev.rel, range.depth, e.n);
      const at = this.insertionPoint(ev.rel, range.start, end, range.depth, e.n);
      this.splice(at, 0, rows, log);
      end += rows.length;
    }
    return log;
  }

  /** Index of a direct child's row in `[start, end)`, or -1. */
  private directChild(start: number, end: number, depth: number, key: string): number {
    for (let i = start; i < end; i++) {
      const r = this.rows[i];
      if (r.depth === depth && r.key === key) return i;
    }
    return -1;
  }

  /** Where `name`'s row goes among its folder's rows: before the first later sibling that
   *  has a row, else after the last child (and its subtree), before any message row. */
  private insertionPoint(rel: string, start: number, end: number, depth: number, name: string): number {
    const d = this.dirs.get(rel)!;
    const idx = d.names.indexOf(name);
    const later = new Set<string>();
    for (let k = idx + 1; k < d.names.length; k++) later.add(joinRel(rel, d.names[k]));
    for (let i = start; i < end; i++) {
      const r = this.rows[i];
      if (r.depth === depth && (later.has(r.key) || r.kind === 'm')) return i;
    }
    return end;
  }

  applyError(ev: DirErrorEvent): { splices: Splice[]; close: string[] } {
    const splices: Splice[] = [];
    const close: string[] = [];
    if (!this.interest.has(ev.rel)) return { splices, close };
    if (ev.code === 'gone') {
      // The folder went away under us: drop it and what was open below it. Its own row
      // leaves with its parent's delta.
      const range = ev.rel === '' ? null : this.childRange(ev.rel);
      if (range && this.expanded.has(ev.rel)) this.splice(range.start, range.end - range.start, [], splices);
      for (const r of [...this.interest]) if (r !== '' && isWithin(r, ev.rel)) { this.interest.delete(r); close.push(r); }
      for (const r of [...this.expanded]) if (r !== '' && isWithin(r, ev.rel)) this.expanded.delete(r);
      for (const r of [...this.dirs.keys()]) if (isWithin(r, ev.rel) && r !== '') this.dirs.delete(r);
      return { splices, close };
    }
    if (ev.code === 'timeout') {
      const d = this.dirs.get(ev.rel) ?? this.emptyDir();
      d.slow = ev.msg || 'Not responding…';
      this.dirs.set(ev.rel, d);
      this.rerender(ev.rel, splices);
      return { splices, close };
    }
    const d = this.emptyDir();
    d.state = 'error';
    d.err = ev.code === 'denied' ? 'Permission denied' : ev.msg;
    this.dirs.set(ev.rel, d);
    this.rerender(ev.rel, splices);
    return { splices, close };
  }

  private emptyDir(): DirData {
    return { gen: 0, names: [], entries: new Map(), state: 'loading', truncated: false, total: 0, building: null };
  }

  // ── Git ───────────────────────────────────────────────────────────────────────────────

  /** A `git` event: replace everything (`full`), or drop `del` and set the rest. Keys can
   *  move between the maps, so `del` clears both before anything is set. */
  applyGit(u: GitUpdate): void {
    if (u.full) {
      this.gitFiles.clear();
      this.gitDirs.clear();
    }
    for (const k of u.del) {
      this.gitFiles.delete(k);
      this.gitDirs.delete(k);
    }
    for (const [k, v] of u.files) this.gitFiles.set(k, v);
    for (const [k, v] of u.dirs) this.gitDirs.set(k, v);
    this.gitTruncated = u.truncated;
  }

  clearGit(): void {
    this.gitFiles.clear();
    this.gitDirs.clear();
    this.gitTruncated = false;
  }

  /** What a row shows for git: its own letter, a folder's roll-up (a dot), or untracked
   *  because a folder above it is wholly untracked. Null when clean. */
  gitOf(rel: string, folder: boolean): { letter: GitLetter; rollup: boolean } | null {
    // An accented name stored NFD (as Finder and Cocoa write it) is keyed NFC by git. Only
    // non-ASCII names can differ, so the common row skips the normalize.
    if (this.nfcNames && /[^\x00-\x7f]/.test(rel)) rel = rel.normalize('NFC');
    const own = this.gitFiles.get(rel) ?? (folder ? this.gitFiles.get(rel + '/') : undefined);
    if (own) return { letter: own, rollup: false };
    if (folder) {
      const d = this.gitDirs.get(rel);
      if (d) return { letter: d, rollup: true };
    }
    if (this.gitFiles.size) {
      for (const a of ancestorsOf(rel)) if (this.gitFiles.get(a + '/') === '?') return { letter: '?', rollup: false };
    }
    return null;
  }

  /** The Changed lens's paths: every changed file, and each wholly untracked folder as one
   *  folder leaf. */
  changedItems(): LensItem[] {
    const out: LensItem[] = [];
    for (const k of this.gitFiles.keys()) {
      if (k.endsWith('/')) out.push({ rel: k.slice(0, -1), kind: 'd' });
      else out.push({ rel: k, kind: this.entry(k)?.k ?? 'f' });
    }
    return out;
  }

  // ── Queries ───────────────────────────────────────────────────────────────────────────

  entry(rel: string): TreeEntry | undefined {
    if (rel === '') return undefined;
    return this.dirs.get(parentRel(rel))?.entries.get(baseName(rel));
  }

  /** The folders to ask again with their held generations: a returning view (§4.2). */
  restoreReqs(): DirReq[] {
    return [...this.interest].map((r) => this.reqFor(r));
  }
}

/** Index of the parent row of `rows[i]` in the rows on screen (-1 for a root child or an
 *  index outside them). Taken over the rows the index came from: find and the Changed lens
 *  show a pruned list, and a lens index read against the real tree's rows named another row
 *  (or ran off the end). */
export function parentRowIndex(rows: Row[], i: number): number {
  if (i < 0 || i >= rows.length) return -1;
  const d = rows[i].depth;
  for (let j = i - 1; j >= 0; j--) if (rows[j].depth < d) return j;
  return -1;
}

// ── Pruned trees: find results and the Changed lens (§8, §9.4) ─────────────────────────────

/** One path a pruned tree must show; its folders come with it. */
export interface LensItem {
  rel: string;
  kind: RowKind;
}

interface LensNode {
  kind: RowKind;
  children: Map<string, LensNode>;
}

function lensCmp(a: [string, LensNode], b: [string, LensNode]): number {
  const fa = isFolderKind(a[1].kind) ? 0 : 1;
  const fb = isFolderKind(b[1].kind) ? 0 : 1;
  if (fa !== fb) return fa - fb;
  const la = a[0].toLowerCase();
  const lb = b[0].toLowerCase();
  if (la !== lb) return la < lb ? -1 : 1;
  return a[0] < b[0] ? -1 : a[0] > b[0] ? 1 : 0;
}

/** Who wants a file revealed: the tree following the active file (`auto`), the focus
 *  shortcut handing the keyboard back (`focus`), or the human's "Reveal in Files" (`asked`). */
export type RevealKind = 'auto' | 'focus' | 'asked';

/**
 * What a reveal does. In the real tree it opens the folders above the file (and selects it
 * unless `auto`). Under find or the Changed lens only "Reveal in Files" may put the pruned
 * view away: the focus shortcut is how the keyboard gets back to find's results after Enter
 * opened one, so it must not throw the query away, and neither it nor an automatic reveal
 * opens folders in the hidden real tree (that would move what Escape restores).
 */
export function revealPlan(kind: RevealKind, pruned: boolean, rowShown: boolean): 'tree' | 'put-away' | 'select' | 'scroll' | 'none' {
  if (!pruned) return 'tree';
  if (kind === 'asked') return 'put-away';
  if (!rowShown) return 'none';
  return kind === 'focus' ? 'select' : 'scroll';
}

/**
 * The rows of a pruned tree: every item plus the folders above it, folders first, each open
 * unless it is in `collapsed`. The listing order is Rust's and these paths aren't all listed,
 * so siblings sort here (folders first, then case-insensitive); this never touches the real
 * tree's rows. `entryOf` supplies kinds and flags for paths the model has listed; with hidden
 * files off, a path with a dot segment is left out. An empty result is one 'empty' message
 * row saying `emptyText`.
 */
export function prunedRows(
  items: LensItem[],
  collapsed: Set<string>,
  entryOf: (rel: string) => TreeEntry | undefined,
  opts: { showHidden: boolean; emptyText: string },
): Row[] {
  const root: LensNode = { kind: 'd', children: new Map() };
  for (const it of items) {
    if (!it.rel) continue;
    const parts = it.rel.split('/');
    if (!opts.showHidden && parts.some((p) => p.startsWith('.'))) continue;
    let node = root;
    let rel = '';
    for (let i = 0; i < parts.length; i++) {
      rel = joinRel(rel, parts[i]);
      const last = i === parts.length - 1;
      let next = node.children.get(parts[i]);
      if (!next) {
        const known = entryOf(rel)?.k;
        next = { kind: last ? it.kind : known && isFolderKind(known) ? known : 'd', children: new Map() };
        node.children.set(parts[i], next);
      } else if (!last && !isFolderKind(next.kind)) {
        next.kind = 'd';
      }
      node = next;
    }
  }
  const out: Row[] = [];
  const walk = (node: LensNode, rel: string, depth: number) => {
    for (const [name, child] of [...node.children].sort(lensCmp)) {
      const key = joinRel(rel, name);
      out.push({ key, depth, name, kind: child.kind, flags: entryOf(key)?.f ?? 0 });
      if (child.children.size && !collapsed.has(key)) walk(child, key, depth + 1);
    }
  };
  walk(root, '', 0);
  if (out.length === 0) out.push({ key: '\u0000empty', depth: 0, name: opts.emptyText, kind: 'm', flags: 0, msg: 'empty' });
  return out;
}

/**
 * Find's highlights per row: for each hit, its matched character positions (code points, as
 * Rust counts them) mapped onto the name of each row on its path, so a folder lights the
 * letters of the query that fell in its name. Positions are within the row's name, in code
 * points.
 */
export function matchHighlights(hits: { rel: string; idx: number[] }[]): Map<string, number[]> {
  const out = new Map<string, Set<number>>();
  for (const h of hits) {
    if (!h.idx.length) continue;
    const chars = Array.from(h.rel);
    // Each segment's start (in code points) and the row key it ends at.
    const segs: { start: number; key: string }[] = [];
    let start = 0;
    for (let i = 0; i <= chars.length; i++) {
      if (i === chars.length || chars[i] === '/') {
        segs.push({ start, key: chars.slice(0, i).join('') });
        start = i + 1;
      }
    }
    let s = 0;
    for (const i of h.idx) {
      while (s + 1 < segs.length && segs[s + 1].start <= i) s++;
      if (chars[i] === '/') continue;
      const seg = segs[s];
      let set = out.get(seg.key);
      if (!set) out.set(seg.key, (set = new Set()));
      set.add(i - seg.start);
    }
  }
  const res = new Map<string, number[]>();
  for (const [k, v] of out) res.set(k, [...v].sort((a, b) => a - b));
  return res;
}

/** A name split into runs for drawing, `true` runs highlighted. `marks` are code-point
 *  positions in `name`. */
export function highlightRuns(name: string, marks: number[] | undefined): { text: string; hit: boolean }[] {
  if (!marks?.length) return [{ text: name, hit: false }];
  const set = new Set(marks);
  const runs: { text: string; hit: boolean }[] = [];
  let i = 0;
  for (const ch of name) {
    const hit = set.has(i++);
    const last = runs[runs.length - 1];
    if (last && last.hit === hit) last.text += ch;
    else runs.push({ text: ch, hit });
  }
  return runs;
}

/** The ancestors of the row at the top of the viewport whose own rows have scrolled away,
 *  outermost first, at most `max` (the deepest kept): the sticky header (§12.3). */
export function stickyAncestors(rows: Row[], top: number, max: number): number[] {
  if (max <= 0 || top <= 0 || top >= rows.length) return [];
  const chain: number[] = [];
  let depth = rows[top].depth;
  for (let j = top - 1; j >= 0 && depth > 0; j--) {
    if (rows[j].depth < depth) {
      chain.push(j);
      depth = rows[j].depth;
    }
  }
  chain.reverse();
  return chain.length > max ? chain.slice(chain.length - max) : chain;
}

/** Type-ahead (§8): the next row after `from` whose name starts with `prefix`, wrapping. */
export function typeAheadMatch(rows: Row[], from: number, prefix: string): number {
  if (!prefix || rows.length === 0) return -1;
  const p = prefix.toLowerCase();
  const n = rows.length;
  // A single repeated letter cycles through matches; a longer prefix stays on the current
  // row while it still matches (typing "src" lands on src and stays there).
  const startAt = prefix.length > 1 && from >= 0 ? from : from + 1;
  for (let k = 0; k < n; k++) {
    const i = (((startAt + k) % n) + n) % n;
    const r = rows[i];
    if (r.kind !== 'm' && r.name.toLowerCase().startsWith(p)) return i;
  }
  return -1;
}

/** The visible window of a fixed-height list: `[first, last)` with overscan. */
export function windowOf(scrollTop: number, viewport: number, rowH: number, count: number, overscan = 10): { first: number; last: number } {
  const first = Math.max(0, Math.floor(scrollTop / rowH) - overscan);
  const last = Math.min(count, Math.ceil((scrollTop + viewport) / rowH) + overscan);
  return { first, last: Math.max(first, last) };
}
