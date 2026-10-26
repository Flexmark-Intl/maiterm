/** The Hive's budget for paid generation (docs/hive.md §10.3, §7.8), pure: the header's total and
 *  its colour, and the gallery settings sheet's fields. Rust checks everything again
 *  (`gallery::budget::check_change`); this only says early what it would refuse. */

import type { GallerySettingsChange, Hive, HiveSpend } from '$lib/tauri/types';

const MICROS_PER_CENT = 10_000;
/** The header turns amber here, red once the budget is reached. */
export const AMBER_AT = 0.8;
export const MAX_BUDGET_DOLLARS = 100_000;
export const MAX_GENERATOR_HOSTS = 50;
export const MAX_OUTPUT_FOLDERS = 20;

/** Dollars as Rust says them (`budget::usd`): "$10.40", "$1,250.00", under a cent "$0.003". */
export function usd(micros: number): string {
  const sign = micros < 0 ? '-' : '';
  const m = Math.abs(Math.round(micros));
  if (m > 0 && m < MICROS_PER_CENT) {
    const mills = Math.max(1, Math.floor((m + 500) / 1000));
    return `${sign}$0.${String(mills).padStart(3, '0')}`;
  }
  const cents = Math.floor((m + MICROS_PER_CENT / 2) / MICROS_PER_CENT);
  const dollars = Math.floor(cents / 100).toString().replace(/\B(?=(\d{3})+(?!\d))/g, ',');
  return `${sign}$${dollars}.${String(cents % 100).padStart(2, '0')}`;
}

export type BudgetLevel = 'ok' | 'amber' | 'over';

export function budgetLevel(s: HiveSpend): BudgetLevel {
  if (s.over) return 'over';
  if (s.budget_micros > 0 && s.spent_micros >= s.budget_micros * AMBER_AT) return 'amber';
  return 'ok';
}

/** The header's Budget readout: "$3.20 of $10.00". */
export function budgetLabel(s: HiveSpend): string {
  return `${usd(s.spent_micros)} of ${usd(s.budget_micros)}`;
}

/** How full the budget meter is, 0–1 (an unset budget reads empty). */
export function budgetFill(s: HiveSpend): number {
  if (s.budget_micros <= 0) return s.over ? 1 : 0;
  return Math.min(1, s.spent_micros / s.budget_micros);
}

/** The total across Hives shows only when another Hive has spent (§7.8, plan D26). */
export function allHivesLabel(s: HiveSpend): string | null {
  return s.all_hives_micros > s.spent_micros ? `all Hives ${usd(s.all_hives_micros)}` : null;
}

/** One allowlist entry as Rust takes it (`budget::normalize_host`): the host, or why not. */
export function normalizeHost(raw: string): { host: string } | { error: string } {
  const h = raw.trim().replace(/\.+$/, '').toLowerCase();
  const shown = raw.trim().slice(0, 80);
  if (!h) return { error: "A host can't be empty." };
  if (h.includes('://') || /[/:@?#[\]]/.test(h)) return { error: `“${shown}” isn't a bare host name: no scheme, port or path.` };
  const name = h.startsWith('*.') ? h.slice(2) : h;
  const labels = name.split('.');
  if (labels.length < 2) return { error: `“${shown}” needs a dot: a whole top-level domain is too wide.` };
  const labelOk = (l: string) => l.length > 0 && l.length <= 63 && !l.startsWith('-') && !l.endsWith('-') && /^[a-z0-9-]+$/.test(l);
  if (name.length > 253 || !labels.every(labelOk)) return { error: `“${shown}” isn't a host name (letters, digits and hyphens between dots).` };
  const tld = labels[labels.length - 1];
  if (/^\d+$/.test(tld)) return { error: `“${shown}” is an IP address: list host names only.` };
  if (tld === 'localhost' || tld === 'local') return { error: `“${shown}” is this computer or its network: a worker's localhost stays closed.` };
  return { host: h };
}

/** The sheet's fields, as typed. */
export interface GalleryForm {
  /** Dollars, as the field holds them. */
  dollars: string;
  outputFolders: string[];
  hosts: string[];
}

export function formFor(hive: Pick<Hive, 'budget_cents' | 'settings'>): GalleryForm {
  const g = hive.settings?.gallery;
  return {
    dollars: (hive.budget_cents / 100).toFixed(2),
    outputFolders: [...(g?.output_folders ?? [])],
    hosts: [...(g?.generator_hosts ?? [])],
  };
}

/** Whole cents from the dollars field, or null when it isn't an amount. */
export function centsOf(dollars: string): number | null {
  const t = dollars.trim().replace(/^\$/, '').replace(/,/g, '');
  if (!/^\d+(\.\d{0,2})?$/.test(t)) return null;
  return Math.round(Number(t) * 100);
}

/** What Rust would refuse, said early; null when it would save. */
export function validateForm(f: GalleryForm): string | null {
  const cents = centsOf(f.dollars);
  if (cents === null) return 'The budget is an amount in dollars, like 25 or 12.50.';
  if (cents > MAX_BUDGET_DOLLARS * 100) return `The budget can be at most ${usd(MAX_BUDGET_DOLLARS * 1_000_000)}.`;
  if (f.outputFolders.length > MAX_OUTPUT_FOLDERS) return `At most ${MAX_OUTPUT_FOLDERS} output folders.`;
  const seen = new Set<string>();
  for (const raw of f.hosts) {
    const r = normalizeHost(raw);
    if ('error' in r) return r.error;
    seen.add(r.host);
  }
  if (seen.size > MAX_GENERATOR_HOSTS) return `At most ${MAX_GENERATOR_HOSTS} generator hosts.`;
  return null;
}

export function toSave(f: GalleryForm): GallerySettingsChange {
  const hosts: string[] = [];
  for (const raw of f.hosts) {
    const r = normalizeHost(raw);
    if ('host' in r && !hosts.includes(r.host)) hosts.push(r.host);
  }
  return { budget_cents: centsOf(f.dollars) ?? 0, output_folders: f.outputFolders, generator_hosts: hosts };
}

/** Re-detect's offer: hosts detected now that the list lacks, in detection order. */
export function newHosts(current: string[], detected: string[]): string[] {
  const have = new Set(current.map((h) => h.trim().toLowerCase()));
  return detected.filter((h) => !have.has(h.toLowerCase()));
}
