/** New goal (docs/hive.md §11.5): what the New goal sheet and the pending-commits layer over the
 *  board say. Pure, so the words and the gates are tested without a webview. */
import type { Hive, HiveChapter, HiveNewGoalPreview, HivePendingBranch } from '$lib/tauri/types';
import { charCount, MAX_BRIEF, MAX_BRIEF_FILES, MAX_INSTRUCTIONS } from './model';

/** New goal is offered on a Done or paused Hive only; Rust re-checks (`allowed`). */
export function newGoalOffered(status: string | null | undefined): boolean {
  return status === 'done' || status === 'paused';
}

/** The layer over the workers shows while New goal is offered and anything isn't landed. */
export function showPendingLayer(status: string | null | undefined, preview: HiveNewGoalPreview | null): boolean {
  return newGoalOffered(status) && !!preview && preview.unlanded.length > 0;
}

const plural = (n: number, one: string, many: string) => `${n} ${n === 1 ? one : many}`;

/** "3 commits aren't landed yet", counting every branch's commits. */
export function pendingHeadline(unlanded: HivePendingBranch[]): string {
  const n = unlanded.reduce((s, b) => s + Math.max(0, b.ahead), 0);
  if (n === 0) return unlanded.length === 1 ? "A branch isn't landed yet" : `${unlanded.length} branches aren't landed yet`;
  return n === 1 ? "1 commit isn't landed yet" : `${n} commits aren't landed yet`;
}

/** One row of the list: who, where, and how far ahead. */
export function pendingRow(b: HivePendingBranch): { worker: string; place: string; ahead: string } {
  return {
    worker: b.worker,
    place: `${b.folder}: ${b.branch} → ${b.target || 'its landing target'}`,
    ahead: `${plural(Math.max(0, b.ahead), 'commit', 'commits')} ahead`,
  };
}

/** A stable key for the pending set: the layer opens again when it changes. */
export function pendingKey(unlanded: HivePendingBranch[]): string {
  return unlanded.map((b) => `${b.folder}\u0000${b.branch}\u0000${b.ahead}`).join('|');
}

export interface NewGoalDraft {
  goal: string;
  brief: string;
  instructions: string;
  briefFiles: string[];
}

/** Why the sheet's submit is off, one plain sentence, or null when it can go. The preview's own
 *  reasons and the pending commits are shown in their own blocks, so they only switch it off here. */
export function newGoalProblem(preview: HiveNewGoalPreview | null, d: NewGoalDraft): string | null {
  if (!preview) return 'Reading the Hive…';
  if (!preview.allowed || preview.reasons.length) return preview.reasons[0] ?? "This Hive can't take a new goal right now.";
  if (preview.unlanded.length) return 'Land the pending commits first.';
  if (!d.goal.trim()) return 'Write the new goal.';
  if (charCount(d.brief) > MAX_BRIEF) return 'The project brief is longer than 32,000 characters.';
  if (charCount(d.instructions) > MAX_INSTRUCTIONS) return 'The rules are longer than 16,000 characters.';
  if (d.briefFiles.length > MAX_BRIEF_FILES) return `Attach at most ${MAX_BRIEF_FILES} files to the brief.`;
  return null;
}

/** "Goal 2" while the Hive has earlier goals; null for a Hive on its first. */
export function goalNumberLabel(hive: Pick<Hive, 'chapters'> | null | undefined): string | null {
  const n = hive?.chapters?.length ?? 0;
  return n ? `Goal ${n + 1}` : null;
}

/** One earlier goal's line under the header: when it ended and how. */
export function chapterLine(c: HiveChapter, n: number): { title: string; meta: string; summary: string } {
  const ended = c.ended_at ? new Date(c.ended_at) : null;
  const when = ended && !Number.isNaN(ended.getTime()) ? ended.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' }) : '';
  const how = c.done_at ? 'done' : 'replaced before Done';
  const spent = c.spent_cents > 0 ? `, $${(c.spent_cents / 100).toFixed(2)} spent` : '';
  return { title: `Goal ${n}: ${c.goal}`, meta: `${when ? `${when}, ` : ''}${how}${spent}`, summary: c.report_summary.trim() };
}
