/** The Hive settings sheet's read-only half (docs/hive.md §3.2, §4.1): what was fixed at Create,
 *  as a human reads it. What can change has its own sheet (autonomy, run queue, budget). */

import type { Hive } from '$lib/tauri/types';

export interface SettingRow {
  label: string;
  value: string;
  /** Why it can't be changed here, when that isn't obvious. */
  note?: string;
}

const model = (m: { model: string; effort: string }) => `Claude ${m.model || 'default'}${m.effort ? `, ${m.effort} effort` : ''}`;

export function fixedRows(hive: Hive, name: string): SettingRow[] {
  const rows: SettingRow[] = [
    { label: 'Name', value: name || '(unnamed)', note: "The workspace's name: rename it from its menu in the sidebar." },
    { label: 'Goal', value: hive.goal || '(none)' },
    { label: 'Workers at once', value: String(hive.cap) },
    {
      label: 'Landing',
      value: hive.landing_mode === 'integration' ? `Integration branch hive/${hive.slug}, you approve each land` : 'Land to Base',
      note: 'Fixed at creation: it is a branch topology. A different mode means a new Hive.',
    },
    { label: 'Coordinator', value: model(hive.models.coordinator) },
    { label: 'Workers', value: model(hive.models.worker) },
    { label: 'Tools', value: hive.tools.length ? hive.tools.join(', ') : 'None' },
    { label: 'Push after land', value: hive.push_after_land ? 'On' : 'Off' },
    { label: 'On the phone', value: hive.settings?.keep_off_phone ? "Kept off: no agent of this Hive is a chat on the phone" : 'Shown like any agent' },
  ];
  return rows;
}

/** One line per folder: name, kind and path, then Base, setup and land check where set. */
export function folderLines(hive: Hive): { head: string; path: string; detail: string }[] {
  return hive.folders.map((f) => ({
    head: `${f.name}${f.primary ? ' (Primary)' : ''} · ${f.kind === 'git' ? 'git repo' : 'plain, read-only'}`,
    path: f.path,
    detail: [f.base ? `Base ${f.base}` : null, f.setup ? `setup: ${f.setup}` : null, f.land_check ? `land check: ${f.land_check}` : null]
      .filter(Boolean)
      .join(' · '),
  }));
}
