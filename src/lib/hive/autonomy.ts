/** Hive settings → Autonomy (docs/hive.md §7.4, §4.7, §7.6), pure: the seven checkpoints as the
 *  modal edits them, the prompt timeout in minutes and the daily digest. Rust checks all of it
 *  again (`checkpoints::validate`, the 60–3,600 s clamp); this only says early what it would refuse. */

import type { AutonomySettings, Hive } from '$lib/tauri/types';
import { checkpointAsks } from './model';

export type CheckpointValue = 'coordinator' | 'ask';

export interface CheckpointRow {
  key: string;
  label: string;
  /** §7.4's note: what the checkpoint covers and what Ask me means for it. */
  note: string;
}

export const CHECKPOINT_ROWS: readonly CheckpointRow[] = [
  { key: 'plan', label: 'Plan', note: 'Which workers, how many, in what order. Ask me is "show me the plan first".' },
  { key: 'questions', label: "Workers' questions", note: "A worker's AskUserQuestion or question blocker, and its compaction and completion notices." },
  { key: 'permissions', label: "Workers' permission prompts", note: 'Only a plain Yes or No: rows that save a rule or change a mode always come to you.' },
  { key: 'landing', label: 'Landing', note: 'Who calls hiveLand, within the landing mode fixed at creation.' },
  { key: 'finals', label: 'Picking finals in the gallery', note: 'Creative Hives often set this to Ask me.' },
  { key: 'interfaces', label: 'Adding a dependency, changing a public interface', note: 'Listed so a Hive can tighten them.' },
  { key: 'settings', label: 'Changing Hive settings', note: 'Workers at once, models, and the hosts workers reach. Ask me (the default) puts each change on a card for you. Raising workers at once always comes to you; never pushing.' },
];

/** What never moves to the coordinator, whatever the settings (§7.4): Rust refuses it by tab. */
export const NEVER_DELEGATED: readonly string[] = [
  "Creating a Hive, raising the budget or workers at once, and spending past the budget",
  "Claude's workspace-trust dialog and the plan dialog",
  'Any prompt option that saves a setting or a rule, or changes a mode',
  'Pushing',
  'A land, or an undo, that changes agent or execution config',
  'Delivering into anything but a Hive plain folder, and overwriting anything',
];

export const TIMEOUT_MIN_MINUTES = 1;
export const TIMEOUT_MAX_MINUTES = 60;
const DEFAULT_TIMEOUT_SECS = 300;

export interface AutonomyForm {
  values: Record<string, CheckpointValue>;
  /** The prompt timeout, in whole minutes. */
  minutes: number;
  digest: boolean;
}

type HiveFields = Pick<Hive, 'landing_mode' | 'checkpoints' | 'settings'>;

/** Why a checkpoint can't be changed here, or null. */
export function lockedReason(hive: Pick<Hive, 'landing_mode'>, key: string): string | null {
  if (key === 'landing' && hive.landing_mode === 'integration') {
    return 'This Hive lands to its integration branch with your approval of each land (fixed at creation), so Landing stays Ask me.';
  }
  return null;
}

export function formFor(hive: HiveFields): AutonomyForm {
  const values: Record<string, CheckpointValue> = {};
  for (const r of CHECKPOINT_ROWS) values[r.key] = checkpointAsks(hive, r.key) ? 'ask' : 'coordinator';
  const secs = hive.settings?.autonomy?.prompt_timeout_secs ?? DEFAULT_TIMEOUT_SECS;
  return { values, minutes: Math.max(TIMEOUT_MIN_MINUTES, Math.round(secs / 60)), digest: hive.settings?.autonomy?.daily_digest ?? true };
}

/** The first thing stopping Save, in a sentence, or null. */
export function validateForm(hive: Pick<Hive, 'landing_mode'>, form: AutonomyForm): string | null {
  for (const r of CHECKPOINT_ROWS) {
    const v = form.values[r.key];
    if (v !== 'coordinator' && v !== 'ask') return `${r.label}: pick Coordinator or Ask me.`;
    if (v === 'coordinator' && lockedReason(hive, r.key)) return lockedReason(hive, r.key);
  }
  if (!Number.isInteger(form.minutes) || form.minutes < TIMEOUT_MIN_MINUTES || form.minutes > TIMEOUT_MAX_MINUTES) {
    return `The prompt timeout is a whole number of minutes, ${TIMEOUT_MIN_MINUTES} to ${TIMEOUT_MAX_MINUTES}.`;
  }
  return null;
}

/** What `hiveSetAutonomy` takes. Fields of `settings.autonomy` a later build added go back as they were. */
export function toSave(hive: HiveFields, form: AutonomyForm): { checkpoints: Record<string, string>; autonomy: AutonomySettings } {
  const checkpoints: Record<string, string> = {};
  for (const r of CHECKPOINT_ROWS) checkpoints[r.key] = form.values[r.key];
  return { checkpoints, autonomy: { ...(hive.settings?.autonomy ?? {}), prompt_timeout_secs: form.minutes * 60, daily_digest: form.digest } };
}

/** How many checkpoints come to the human. */
export function askCount(hive: Pick<Hive, 'landing_mode' | 'checkpoints'>): number {
  return CHECKPOINT_ROWS.filter((r) => checkpointAsks(hive, r.key)).length;
}

/** The header's Autonomy readout: "Coordinator decides", or "You decide 2 of 5". */
export function autonomyLabel(hive: Pick<Hive, 'landing_mode' | 'checkpoints'>): string {
  const n = askCount(hive);
  return n === 0 ? 'Coordinator decides' : `You decide ${n} of ${CHECKPOINT_ROWS.length}`;
}
