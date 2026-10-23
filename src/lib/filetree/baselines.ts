/**
 * Turn baselines in words (docs/file-tree.md §14): the diff tab's title, why there is no
 * baseline, and the Turn lens tooltip's line counts. Pure, so the wording is tested.
 */
import type { BaselineStat } from '$lib/tauri/types';

export type BaselineScope = 'turn' | 'session';

/** Who the diff is against: "claude, tab 3", or just the tab when its name already says the
 *  runtime (a tab named "claude"), or when the runtime isn't known. */
function who(runtime: string, tabName: string): string {
  if (!runtime || tabName.toLowerCase() === runtime.toLowerCase()) return tabName;
  return `${runtime}, ${tabName}`;
}

/** The diff tab's title: "foo.ts (turn 14 · claude, tab 3)", or for the session's start
 *  "foo.ts (session start · claude, tab 3)". */
export function baselineDiffTabName(fileName: string, scope: BaselineScope, turn: number, runtime: string, tabName: string): string {
  const when = scope === 'turn' ? `turn ${turn}` : 'session start';
  return `${fileName} (${when} · ${who(runtime, tabName)})`;
}

/** Why a baseline can't be shown, as a sentence (`Baseline.unavailable`). */
export function baselineUnavailableWords(why: string, scope: BaselineScope): string {
  const start = scope === 'turn' ? 'this turn' : 'this session';
  switch (why) {
    case 'timeout': return 'The file took too long to read before the agent’s first edit, so its start wasn’t kept.';
    case 'binary': return 'It isn’t a text file, so its start wasn’t kept.';
    case 'too_large': return 'It is over 2 MB, so its start wasn’t kept.';
    case 'unreadable': return 'It couldn’t be read before the agent’s first edit.';
    case 'evicted': return 'Its start was dropped to keep memory under 64 MB.';
    case 'pending': return 'Its start is still being read. Try again in a moment.';
    case 'current_unreadable': return 'The file on disk can’t be read as text now.';
    case 'not_captured':
    default:
      return `maiTerm kept no start for it ${start}: the agent hasn’t edited it with Edit, Write or a patch (a shell command’s writes aren’t captured), or ${start} began before maiTerm started.`;
  }
}

/** The tooltip's words for a file's change since the turn began: "+12 −3 since turn 14
 *  began", or null when there is nothing to count. */
export function baselineStatWords(st: BaselineStat): string | null {
  if (st.unavailable) return null;
  return `+${st.added} −${st.removed} since turn ${st.turn} began`;
}
