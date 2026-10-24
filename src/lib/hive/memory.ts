/** The Hive's Memory view (docs/hive.md §9), pure: provenance labels, file tags, the kept-block
 *  editor's text and limits, and the Board ⇄ Memory cycle. HiveMemoryView renders; Rust decides
 *  everything (the index, provenance, the caps it enforces again). */

import type { HiveMemoryBudget, HiveMemoryFile } from '$lib/tauri/types';
import { HIVE_VIEWS, type HiveViewName } from './columns';

/** Cmd+Shift+Y (§4.6): Board → Memory → Board; the gallery joins in step 8. */
export function nextView(v: HiveViewName): HiveViewName {
  const i = HIVE_VIEWS.indexOf(v);
  return HIVE_VIEWS[(i + 1) % HIVE_VIEWS.length] ?? 'board';
}

export interface Provenance {
  text: string;
  tone: 'human' | 'coordinator' | 'worker' | 'unknown' | 'maiterm';
  tip: string;
}

/** Who wrote a file, as Rust recorded it: never the file's own frontmatter, which an agent can
 *  edit (§9 "Provenance comes from maiTerm"). */
export function provenance(f: Pick<HiveMemoryFile, 'by' | 'by_kind'>): Provenance {
  switch (f.by_kind) {
    case 'human':
      return { text: 'You', tone: 'human', tip: 'You wrote this in the Memory view' };
    case 'coordinator':
      return { text: 'Coordinator', tone: 'coordinator', tip: "Written by the coordinator's Write or Edit, as its hook reported it" };
    case 'worker':
      return { text: f.by || 'A worker', tone: 'worker', tip: `Written by worker ${f.by}'s Write or Edit, as its hook reported it` };
    case 'maiterm':
      return { text: 'maiTerm', tone: 'maiterm', tip: "maiTerm's own file: older index lines folded out of MEMORY.md" };
    default:
      return { text: 'Unknown', tone: 'unknown', tip: 'Changed by something no tool event explains (a shell command, an edit outside maiTerm), so nobody is credited' };
  }
}

/** The small tags beside a file's name. */
export function fileTags(f: Pick<HiveMemoryFile, 'indexed' | 'folded' | 'pinned' | 'in_subfolder' | 'archive'>): string[] {
  if (f.archive) return ['archive'];
  const tags: string[] = [];
  if (f.pinned) tags.push('pinned');
  else if (f.folded) tags.push('folded');
  else if (!f.indexed) tags.push('not indexed');
  return tags;
}

/** What each tag means, for its tooltip. */
export const TAG_TIP: Record<string, string> = {
  archive: "maiTerm's fold of older index lines. It goes when nothing needs it",
  pinned: 'Its line is in the kept block, so the index doesn’t list it again',
  folded: 'Its line was folded into an archive to keep MEMORY.md inside what Claude reads (200 lines, 25 KB)',
  'not indexed': 'Files in subfolders are listed here but never indexed: agents won’t find them from MEMORY.md',
};

/** The kept editor's textarea as lines: one per line, trailing blank lines dropped. */
export function keptLines(text: string): string[] {
  const lines = text.split('\n').map((l) => l.replace(/\r$/, ''));
  while (lines.length && !lines[lines.length - 1].trim()) lines.pop();
  return lines;
}

const utf8 = (s: string) => new TextEncoder().encode(s).length;

/** The cap Rust enforces again: null when `lines` fit. */
export function keptError(lines: readonly string[], b: Pick<HiveMemoryBudget, 'kept_max_lines' | 'kept_max_bytes'>): string | null {
  const bytes = lines.reduce((n, l) => n + utf8(l) + 1, 0);
  if (lines.length > b.kept_max_lines || bytes > b.kept_max_bytes) {
    return `The kept block holds at most ${b.kept_max_lines} lines and ${b.kept_max_bytes / 1000} KB (this is ${lines.length} lines, ${bytes} bytes)`;
  }
  return null;
}

const kb = (n: number) => (n / 1000).toFixed(n < 10_000 ? 1 : 0);

/** How much of what Claude reads MEMORY.md takes. */
export function budgetText(b: HiveMemoryBudget): string {
  return `MEMORY.md: ${b.lines} of ${b.max_lines} lines, ${kb(b.bytes)} of ${b.max_bytes / 1000} KB · kept block ${b.kept_lines} of ${b.kept_max_lines} lines`;
}

/** A new file's name as the human typed it: one name, `.md` added when missing. Null when it
 *  can't be one (Rust refuses the rest). */
export function newFileName(raw: string): string | null {
  const n = raw.trim();
  if (!n || n.startsWith('.') || /[/\\:\0]/.test(n)) return null;
  const name = n.endsWith('.md') ? n : `${n}.md`;
  if (name === 'MEMORY.md' || name === 'MEMORY.generated.md' || name.startsWith('archive-')) return null;
  return name;
}
