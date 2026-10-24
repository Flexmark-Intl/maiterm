/** The land log view's words (docs/hive.md §8.6), as pure functions over what Rust answered
 *  (hive/land/undo.rs `LandLogView`, `UndoPreview`). Undo lives here and in the worker column's
 *  menu, never on a card (§4.3), and its confirmation names the repo, the commits it reverts, the
 *  branch it writes and any config files before anything is written. */

import type { HiveLandLog, HiveLandLogEntry, HiveUndoPreview } from '$lib/tauri/types';

const short = (sha: string | null | undefined) => (sha ?? '').slice(0, 7);
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** A line that moved its target: what Undo can take back. */
export const movedTarget = (e: Pick<HiveLandLogEntry, 'kind'>) => ['land', 'batch_member', 'integration', 'undo'].includes(e.kind);

export function kindLabel(e: Pick<HiveLandLogEntry, 'kind' | 'check'>): string {
  switch (e.kind) {
    case 'land':
      return 'Landed';
    case 'batch_member':
      return 'Landed in a batch';
    case 'integration':
      return 'Landed (approved)';
    case 'undo':
      return 'Undo';
    case 'base_check':
      return e.check === 'failed' ? 'Post-land check failed' : 'Post-land check passed';
    default:
      return e.kind;
  }
}

/** "main a1b2c3d → e4f5a6b" for a line that moved its target, "main at a1b2c3d" for a check. */
export function moveLine(e: Pick<HiveLandLogEntry, 'kind' | 'target' | 'base_before' | 'base_after'>): string {
  if (!movedTarget(e)) return `${e.target} at ${short(e.base_after)}`;
  return `${e.target} ${short(e.base_before)} → ${short(e.base_after)}`;
}

/** What a line's qualifiers say: undone, suspect, its push. */
export function flags(e: Pick<HiveLandLogEntry, 'undone_by' | 'suspect' | 'pushed' | 'kind'>): string[] {
  const out: string[] = [];
  if (e.undone_by) out.push(`undone by ${e.undone_by}`);
  if (e.suspect) out.push('suspect');
  if (movedTarget(e) && e.pushed.startsWith('held')) out.push(`push ${e.pushed}`);
  return out;
}

/** Why the human can't undo this line now, or null when they can. Lines that never moved a target
 *  (a post-land check) and undos themselves never offer it at all (`offersUndo`). */
export function undoBlocked(e: Pick<HiveLandLogEntry, 'id' | 'undone_by'>, log: Pick<HiveLandLog, 'undos'>): string | null {
  if (e.undone_by) return `Already undone (${e.undone_by}).`;
  const open = log.undos.find((u) => u.undoes === e.id && u.state !== 'held');
  if (!open) return null;
  return open.state === 'awaiting_human' ? 'Its undo waits for your approval on a card.' : `Being undone now (${open.state}).`;
}

export const offersUndo = (e: Pick<HiveLandLogEntry, 'kind'>) => movedTarget(e) && e.kind !== 'undo';

/** Newest first, a worker's own lines only when `taskId` is given. */
export function shownEntries(log: Pick<HiveLandLog, 'entries'>, taskId?: string | null): HiveLandLogEntry[] {
  const all = taskId ? log.entries.filter((e) => e.task_id === taskId) : log.entries;
  return [...all].reverse();
}

export interface UndoConfirm {
  title: string;
  /** What it writes and what it reverts, then the commits, then the rest. */
  head: string[];
  commits: { sha: string; subject: string }[];
  /** Commits not listed. */
  more: number;
  tail: string[];
  /** Set when it can't be undone: the confirm button stays off. */
  refusal: string | null;
}

/** The inline confirmation (§8.6): the repo, the branch it writes, the commits it reverts, any
 *  config files and whether a check tests it, before anything is written. */
export function undoConfirm(p: HiveUndoPreview): UndoConfirm {
  const head = [
    `Writes a revert commit to ${p.target} in ${p.folder}, on top of ${short(p.tip)}. Nothing is reset: the land stays in the log and in git.`,
    p.method === 'merge'
      ? `Reverts the merge that landed “${p.title}” (${p.worker}), and with it ${plural(p.commits_total, 'commit')}:`
      : `Reverts the ${plural(p.commits_total, 'commit')} that landed “${p.title}” (${p.worker}), as one commit:`,
  ];
  const tail: string[] = [];
  if (p.config_files.length) tail.push(`It changes agent or execution config: ${p.config_files.join(', ')}.`);
  tail.push(p.has_check ? 'Its land check runs on the revert before it lands.' : `${p.folder} has no land check: nothing tests the revert before it lands.`);
  tail.push(`Workers waiting on ${p.worker} stay blocked; the worker isn't reopened.`);
  return {
    title: `Undo ${p.id}?`,
    head,
    commits: p.commits,
    more: Math.max(0, p.commits_total - p.commits.length),
    tail,
    refusal: p.refusal?.trim() || null,
  };
}

/** The repos the header warns about: suspect, with what it means. */
export function suspectLines(log: Pick<HiveLandLog, 'repos'>): string[] {
  return log.repos
    .filter((r) => r.suspect)
    .map((r) => `${r.folder}: ${r.target} fails its land check alone${r.suspects.length ? ` since ${r.suspects.join(', ')}` : ''}. Lands there need a fix forward until a land or an undo passes its check.`);
}
