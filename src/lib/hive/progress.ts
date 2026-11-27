/** The Hive header's two gauges (docs/hive.md §4.1.1), pure.
 *
 *  - The queen's progress estimate (`hiveSetProgress`): her claim, shown as hers with the time she
 *    set it. Only Done says complete.
 *  - The crew: what the workers are doing, as one bar of four groups and words for anything that
 *    needs a look. It replaced one tile per worker, which turned to noise past a few dozen. */

import type { HiveProgress } from '$lib/tauri/types';
import type { CardStateName } from './cards';

/** An estimate older than this reads as old: the queen has stopped keeping it up. */
export const PROGRESS_STALE_MS = 60 * 60 * 1000;

export interface ProgressView {
  percent: number;
  note: string;
  /** "as of 14:32", "as of Nov 24, 14:32". */
  when: string;
  stale: boolean;
  done: boolean;
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function clock(at: number, now: number): string {
  const d = new Date(at);
  const hm = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
  const n = new Date(now);
  const sameDay = d.getFullYear() === n.getFullYear() && d.getMonth() === n.getMonth() && d.getDate() === n.getDate();
  return sameDay ? hm : `${MONTHS[d.getMonth()]} ${d.getDate()}, ${hm}`;
}

/** What the progress gauge shows: null while the queen hasn't set one (and the Hive isn't done).
 *  A Done Hive reads complete whatever her last estimate said. */
export function progressView(progress: HiveProgress | null | undefined, status: string, now: number = Date.now()): ProgressView | null {
  if (status === 'done') return { percent: 100, note: progress?.note ?? '', when: '', stale: false, done: true };
  if (!progress) return null;
  const percent = Math.max(0, Math.min(100, Math.round(progress.percent)));
  return { percent, note: progress.note, when: `as of ${clock(progress.at, now)}`, stale: now - progress.at >= PROGRESS_STALE_MS, done: false };
}

/** A card's place in the crew bar. `quiet` is a worker idle past 10 minutes with nothing on its way
 *  (the card's activity tone), whatever its state. */
export type CrewGroup = 'landed' | 'active' | 'attention' | 'waiting';

const GROUP: Record<CardStateName, CrewGroup> = {
  landed: 'landed',
  retired: 'landed',
  working: 'active',
  starting: 'active',
  landing: 'active',
  reported: 'active',
  needs_you: 'attention',
  blocked: 'attention',
  failed: 'attention',
  queued: 'waiting',
  idle: 'waiting',
  stopped: 'waiting',
  suspended: 'waiting',
};

/** The states named in words, in this order, because one of them among 180 is still news. */
const ATTENTION: readonly { key: CardStateName | 'quiet'; label: string }[] = [
  { key: 'needs_you', label: 'need you' },
  { key: 'failed', label: 'failed' },
  { key: 'blocked', label: 'blocked' },
  { key: 'quiet', label: 'quiet' },
];

export interface CrewSummary {
  total: number;
  /** The bar, in order landed → active → attention → waiting; empty groups left out. */
  segments: { group: CrewGroup; n: number }[];
  /** "4 landed · 6 in flight · 59 waiting" (empty groups left out). */
  line: string;
  /** "1 needs you", "2 failed"…: shown apart, coloured. */
  attention: { key: CardStateName | 'quiet'; text: string }[];
}

/** The crew gauge from each worker's card state and whether it's quiet. */
export function crewSummary(cards: readonly { state: CardStateName; quiet: boolean }[]): CrewSummary {
  const groups: Record<CrewGroup, number> = { landed: 0, active: 0, attention: 0, waiting: 0 };
  const named = new Map<string, number>();
  for (const c of cards) {
    const key = c.quiet && GROUP[c.state] !== 'attention' && GROUP[c.state] !== 'landed' ? 'quiet' : c.state;
    const group = key === 'quiet' ? 'attention' : GROUP[c.state];
    groups[group] += 1;
    if (group === 'attention') named.set(key, (named.get(key) ?? 0) + 1);
  }
  const order: CrewGroup[] = ['landed', 'active', 'attention', 'waiting'];
  const segments = order.filter((g) => groups[g] > 0).map((group) => ({ group, n: groups[group] }));
  const words: [CrewGroup, string][] = [['landed', 'landed'], ['active', 'in flight'], ['waiting', 'waiting']];
  const line = words.filter(([g]) => groups[g] > 0).map(([g, w]) => `${groups[g]} ${w}`).join(' · ');
  const attention = ATTENTION.filter((a) => named.get(a.key)).map((a) => {
    const n = named.get(a.key)!;
    return { key: a.key, text: `${n} ${a.key === 'needs_you' && n === 1 ? 'needs you' : a.label}` };
  });
  return { total: cards.length, segments, line, attention };
}
