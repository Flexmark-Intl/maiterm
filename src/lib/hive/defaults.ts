/**
 * Defaults for new Hives (docs/hive.md §3, Preferences → Hive): what the Make this a Hive
 * wizard starts from, and the grouped, filterable tool list both it and Preferences show.
 * Pure: no store, no Tauri.
 */
import type { HiveDefaults, HiveDetectedTool } from '$lib/tauri/types';

export const HIVE_MODELS = ['opus', 'sonnet', 'haiku'] as const;
export const HIVE_EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max'] as const;
export type HivePhone = 'coordinator' | 'all' | 'none';

/** What the wizard starts from when the human has set nothing. The cap's built-in is the
 *  preflight's `default_cap`; until it answers, the wizard shows `FALLBACK_CAP`. */
export const BUILTIN = {
  coordModel: 'opus',
  coordEffort: 'medium',
  workerModel: 'sonnet',
  workerEffort: 'high',
  budgetDollars: 10,
  pushAfterLand: false,
  phone: 'coordinator' as HivePhone,
} as const;
export const FALLBACK_CAP = 4;

export const EMPTY_HIVE_DEFAULTS: HiveDefaults = {
  tools: [],
  coordinator_model: '',
  coordinator_effort: '',
  worker_model: '',
  worker_effort: '',
  cap: null,
  budget_dollars: null,
  push_after_land: false,
  phone: '',
  rules: '',
};

/** The stored record with every field present (Rust omits nothing, but a partial one from an
 *  older file or a test must not read as `undefined`). */
export function normalizeHiveDefaults(raw: Partial<HiveDefaults> | null | undefined): HiveDefaults {
  const r = raw ?? {};
  return {
    tools: Array.isArray(r.tools) ? r.tools.filter((t): t is string => typeof t === 'string') : [],
    coordinator_model: r.coordinator_model ?? '',
    coordinator_effort: r.coordinator_effort ?? '',
    worker_model: r.worker_model ?? '',
    worker_effort: r.worker_effort ?? '',
    cap: typeof r.cap === 'number' ? r.cap : null,
    budget_dollars: typeof r.budget_dollars === 'number' ? r.budget_dollars : null,
    push_after_land: !!r.push_after_land,
    phone: r.phone ?? '',
    rules: r.rules ?? '',
  };
}

const oneOf = <T extends string>(v: string, list: readonly T[], fallback: T): T =>
  (list as readonly string[]).includes(v) ? (v as T) : fallback;

/** The wizard's starting values. A stored value this build doesn't know (a model a newer
 *  build added, say) falls back to the built-in rather than reaching a `<select>` it isn't in. */
export interface WizardStart {
  coordModel: string;
  coordEffort: string;
  workerModel: string;
  workerEffort: string;
  /** Null: the preflight's default cap. */
  cap: number | null;
  budgetDollars: number;
  pushAfterLand: boolean;
  phone: HivePhone;
  rules: string;
  tools: string[];
}
export function wizardStart(d: HiveDefaults): WizardStart {
  return {
    coordModel: oneOf(d.coordinator_model, HIVE_MODELS, BUILTIN.coordModel),
    coordEffort: oneOf(d.coordinator_effort, HIVE_EFFORTS, BUILTIN.coordEffort),
    workerModel: oneOf(d.worker_model, HIVE_MODELS, BUILTIN.workerModel),
    workerEffort: oneOf(d.worker_effort, HIVE_EFFORTS, BUILTIN.workerEffort),
    cap: d.cap !== null && Number.isInteger(d.cap) && d.cap >= 1 ? d.cap : null,
    budgetDollars: d.budget_dollars !== null && d.budget_dollars >= 0 ? d.budget_dollars : BUILTIN.budgetDollars,
    pushAfterLand: d.push_after_land,
    phone: oneOf(d.phone, ['coordinator', 'all', 'none'] as const, BUILTIN.phone),
    rules: d.rules,
    tools: [...new Set(d.tools)],
  };
}

/** The ids that are detected on this machine, in their given order. A default tool that's
 *  gone (an MCP server removed, a program uninstalled) is dropped silently. */
export function knownTools(ids: Iterable<string>, detected: readonly HiveDetectedTool[]): string[] {
  const have = new Set(detected.map((t) => t.id));
  return [...new Set(ids)].filter((id) => have.has(id));
}

/** What "Save as my defaults" writes: the wizard's tool picks and crew choices. The rules
 *  default is kept as it was: the rules typed for one Hive are rarely everyone's. */
export function defaultsFromWizard(
  w: Omit<WizardStart, 'rules' | 'cap'> & { cap: number },
  was: HiveDefaults,
): HiveDefaults {
  return {
    tools: [...new Set(w.tools)],
    coordinator_model: w.coordModel,
    coordinator_effort: w.coordEffort,
    worker_model: w.workerModel,
    worker_effort: w.workerEffort,
    cap: w.cap,
    budget_dollars: Math.max(0, Math.round(w.budgetDollars)),
    push_after_land: w.pushAfterLand,
    phone: w.phone,
    rules: was.rules,
  };
}

// ── Tools, grouped by kind ──

const GROUPS: { kind: string; label: string }[] = [
  { kind: 'mcp', label: 'MCP servers' },
  { kind: 'skill', label: 'Skills' },
  { kind: 'cli', label: 'Programs' },
  { kind: 'comfyui', label: 'Local generators' },
];

export interface ToolGroup {
  kind: string;
  label: string;
  tools: HiveDetectedTool[];
}

/** Whether a tool matches the filter box: every word, in its label, host or kind. */
export function toolMatches(t: HiveDetectedTool, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const hay = `${t.label} ${t.host ?? ''} ${t.id}`.toLowerCase();
  return words.every((w) => hay.includes(w));
}

/** The detected tools by kind, in a fixed group order (an unknown kind last, under its own
 *  name), each group sorted by label, filtered, empty groups left out. */
export function groupTools(tools: readonly HiveDetectedTool[], query = ''): ToolGroup[] {
  const byKind = new Map<string, HiveDetectedTool[]>();
  for (const t of tools) {
    if (!toolMatches(t, query)) continue;
    const list = byKind.get(t.kind) ?? [];
    list.push(t);
    byKind.set(t.kind, list);
  }
  const known = GROUPS.map((g) => g.kind);
  const kinds = [...known.filter((k) => byKind.has(k)), ...[...byKind.keys()].filter((k) => !known.includes(k)).sort()];
  return kinds.map((kind) => ({
    kind,
    label: GROUPS.find((g) => g.kind === kind)?.label ?? kind,
    tools: [...byKind.get(kind)!].sort((a, b) => a.label.localeCompare(b.label, undefined, { sensitivity: 'base' })),
  }));
}

/** A group's all/none toggle: when every shown tool is picked it clears them, otherwise it
 *  picks them all. Tools hidden by the filter are left as they were. */
export function toggleGroup(picked: ReadonlySet<string>, group: ToolGroup): Set<string> {
  const next = new Set(picked);
  const all = group.tools.every((t) => next.has(t.id));
  for (const t of group.tools) {
    if (all) next.delete(t.id);
    else next.add(t.id);
  }
  return next;
}

/** "ffmpeg, Figma, frontend-design (+4 more)". */
export function summarizeTools(labels: readonly string[], shown = 3): string {
  if (labels.length <= shown) return labels.join(', ');
  return `${labels.slice(0, shown).join(', ')} (+${labels.length - shown} more)`;
}
