/**
 * `proposeHive` (docs/hive.md §3.7): an agent outside any Hive offers its human the Create sheet
 * filled in with its plan. This is the proposal's shape and its checks; nothing here creates
 * anything. The human's Create on the sheet stays the approval for the whole Hive (§7.3).
 */

import { MAX_BRIEF, MAX_BRIEF_FILES } from './model';
import { MAX_CAP } from './change';
import { HIVE_EFFORTS, HIVE_MODELS } from './defaults';

/** What the sheet starts from when it opens on a proposal instead of a blank wizard. */
export interface HiveProposal {
  /** The agent tab that proposed it, and its name as the human knows it. */
  fromTabId: string;
  fromLabel: string;
  name: string;
  goal: string;
  brief: string;
  briefFiles: string[];
  /** Absolute paths; the first is the primary. */
  folders: string[];
  cap: number | null;
  workerModel: string | null;
  workerEffort: string | null;
}

/** The longest name a proposal suggests; the human can type a longer one. */
const NAME_MAX = 60;

const isAbsolute = (p: string) => /^(\/|~\/|[A-Za-z]:[\\/]|\\\\)/.test(p);

function strings(v: unknown, field: string): string[] | string {
  if (v === undefined || v === null) return [];
  if (!Array.isArray(v) || v.some((x) => typeof x !== 'string')) return `${field} must be a list of paths.`;
  return [...new Set((v as string[]).map((s) => s.trim()).filter(Boolean))];
}

/** A name from the goal's first words, when the agent gave none. */
export function nameFromGoal(goal: string): string {
  const first = goal.trim().split(/[.\n]/)[0].trim();
  if (first.length <= NAME_MAX) return first;
  const cut = first.slice(0, NAME_MAX);
  const space = cut.lastIndexOf(' ');
  return (space > 20 ? cut.slice(0, space) : cut).trim();
}

/** The tool's arguments as a proposal, or why they can't be one. `ownFolder` is the caller's
 *  folder, the default when it names none. Paths are only checked for shape here: Rust checks
 *  the brief files exist (`hive_check_brief_files`), and the sheet detects each folder. */
export function parseProposal(
  args: Record<string, unknown>,
  from: { tabId: string; label: string; ownFolder: string | null },
): HiveProposal | string {
  const goal = typeof args.goal === 'string' ? args.goal.trim() : '';
  if (!goal) return 'goal is required: what the Hive should get done.';
  const brief = typeof args.brief === 'string' ? args.brief : '';
  if (brief.length > MAX_BRIEF) return `brief is ${brief.length} characters; the limit is ${MAX_BRIEF}. Attach the rest as brief_files.`;
  const briefFiles = strings(args.brief_files, 'brief_files');
  if (typeof briefFiles === 'string') return briefFiles;
  if (briefFiles.length > MAX_BRIEF_FILES) return `At most ${MAX_BRIEF_FILES} brief_files.`;
  const bad = briefFiles.find((p) => !isAbsolute(p));
  if (bad) return `brief_files must be absolute paths: ${bad} isn't.`;
  let folders = strings(args.folders, 'folders');
  if (typeof folders === 'string') return folders;
  const badFolder = folders.find((p) => !isAbsolute(p));
  if (badFolder) return `folders must be absolute paths: ${badFolder} isn't.`;
  if (!folders.length) {
    if (!from.ownFolder) return "Your folder isn't known: pass folders with the project folder's absolute path.";
    folders = [from.ownFolder];
  }
  let cap: number | null = null;
  if (args.cap !== undefined && args.cap !== null) {
    if (typeof args.cap !== 'number' || !Number.isInteger(args.cap) || args.cap < 1 || args.cap > MAX_CAP) return `cap is a whole number from 1 to ${MAX_CAP}.`;
    cap = args.cap;
  }
  const pick = (v: unknown, allowed: readonly string[], field: string): string | null | { error: string } => {
    if (v === undefined || v === null || v === '') return null;
    return typeof v === 'string' && allowed.includes(v) ? v : { error: `${field} is one of ${allowed.join(', ')}.` };
  };
  const workerModel = pick(args.worker_model, HIVE_MODELS, 'worker_model');
  if (workerModel && typeof workerModel === 'object') return workerModel.error;
  const workerEffort = pick(args.worker_effort, HIVE_EFFORTS, 'worker_effort');
  if (workerEffort && typeof workerEffort === 'object') return workerEffort.error;
  const name = typeof args.name === 'string' && args.name.trim() ? args.name.trim().slice(0, NAME_MAX) : nameFromGoal(goal);
  return {
    fromTabId: from.tabId,
    fromLabel: from.label,
    name,
    goal,
    brief,
    briefFiles,
    folders,
    cap,
    workerModel: workerModel as string | null,
    workerEffort: workerEffort as string | null,
  };
}
