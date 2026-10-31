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
  packs: [],
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
    packs: Array.isArray(r.packs) ? r.packs.filter((t): t is string => typeof t === 'string') : [],
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
  packs: string[];
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
    packs: [...new Set(d.packs)],
  };
}

/** The default tool ids this machine has: the stored ids plus every current member of a stored
 *  pack (a skill the pack added since it was saved included), minus anything not detected. */
export function defaultToolIds(d: Pick<WizardStart, 'tools' | 'packs'>, detected: readonly HiveDetectedTool[]): string[] {
  const members = detected.filter((t) => t.pack && d.packs.includes(t.pack)).map((t) => t.id);
  return knownTools([...d.tools, ...members], detected);
}

/** The packs every detected member of which is picked: what Preferences stores as `packs`. */
export function fullPacks(picked: ReadonlySet<string>, detected: readonly HiveDetectedTool[]): string[] {
  const byPack = new Map<string, boolean>();
  for (const t of detected) {
    if (!t.pack) continue;
    byPack.set(t.pack, (byPack.get(t.pack) ?? true) && picked.has(t.id));
  }
  return [...byPack].filter(([, all]) => all).map(([p]) => p).sort();
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
    packs: [...new Set(w.packs)],
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

/** One row of a group: a tool, or a pack of skills shown as one row (`HiveDetectedTool.pack`). */
export type ToolEntry =
  | { type: 'tool'; tool: HiveDetectedTool }
  | {
      type: 'pack';
      name: string;
      /** Every detected member: the pack's checkbox picks or clears all of them. */
      members: HiveDetectedTool[];
      /** The members the filter shows (all of them when it matched the pack's name). */
      shown: HiveDetectedTool[];
      /** Opened by the filter: a member matched, the pack's name didn't. */
      open: boolean;
    };

export interface ToolGroup {
  kind: string;
  label: string;
  /** Every tool the filter shows, pack members included: what the group's all/none acts on. */
  tools: HiveDetectedTool[];
  /** Packs first, by name, then the tools, by label. */
  entries: ToolEntry[];
}

const words = (query: string) => query.toLowerCase().split(/\s+/).filter(Boolean);
const byLabel = (a: HiveDetectedTool, b: HiveDetectedTool) => a.label.localeCompare(b.label, undefined, { sensitivity: 'base' });

/** Whether a tool matches the filter box: every word, in its label, host or id. */
export function toolMatches(t: HiveDetectedTool, query: string): boolean {
  const w = words(query);
  if (!w.length) return true;
  const hay = `${t.label} ${t.host ?? ''} ${t.id}`.toLowerCase();
  return w.every((x) => hay.includes(x));
}

/** The detected tools by kind, in a fixed group order (an unknown kind last, under its own
 *  name), filtered, empty groups left out. A pack matches by its name (all its members shown)
 *  or by any member (only the matching ones shown, and the pack opened). */
export function groupTools(tools: readonly HiveDetectedTool[], query = ''): ToolGroup[] {
  const q = words(query);
  const byKind = new Map<string, { loose: HiveDetectedTool[]; packs: Map<string, HiveDetectedTool[]> }>();
  for (const t of tools) {
    const g = byKind.get(t.kind) ?? { loose: [] as HiveDetectedTool[], packs: new Map<string, HiveDetectedTool[]>() };
    byKind.set(t.kind, g);
    if (t.pack) g.packs.set(t.pack, [...(g.packs.get(t.pack) ?? []), t]);
    else if (toolMatches(t, query)) g.loose.push(t);
  }
  const out: ToolGroup[] = [];
  const known = GROUPS.map((g) => g.kind);
  const kinds = [...known.filter((k) => byKind.has(k)), ...[...byKind.keys()].filter((k) => !known.includes(k)).sort()];
  for (const kind of kinds) {
    const g = byKind.get(kind)!;
    const entries: ToolEntry[] = [];
    for (const name of [...g.packs.keys()].sort((a, b) => a.localeCompare(b, undefined, { sensitivity: 'base' }))) {
      const members = [...g.packs.get(name)!].sort(byLabel);
      const byName = q.every((x) => name.toLowerCase().includes(x));
      const shown = byName ? members : members.filter((t) => toolMatches(t, query));
      if (shown.length) entries.push({ type: 'pack', name, members, shown, open: !byName && q.length > 0 });
    }
    for (const t of [...g.loose].sort(byLabel)) entries.push({ type: 'tool', tool: t });
    if (!entries.length) continue;
    out.push({
      kind,
      label: GROUPS.find((x) => x.kind === kind)?.label ?? kind,
      tools: entries.flatMap((e) => (e.type === 'tool' ? [e.tool] : e.shown)),
      entries,
    });
  }
  return out;
}

/** A pack's checkbox: when every member is picked it clears them all, otherwise picks them all. */
export function togglePack(picked: ReadonlySet<string>, members: readonly HiveDetectedTool[]): Set<string> {
  const next = new Set(picked);
  const all = members.every((t) => next.has(t.id));
  for (const t of members) {
    if (all) next.delete(t.id);
    else next.add(t.id);
  }
  return next;
}

/** The picks in words, as the wizard's summary and review say them: a fully picked pack is one
 *  item ("gstack (54 skills)"), a partly picked one its picked members. */
export function pickedItems(tools: readonly HiveDetectedTool[], picked: ReadonlySet<string>): string[] {
  return groupTools(tools).flatMap((g) =>
    g.entries.flatMap((e) => {
      if (e.type === 'tool') return picked.has(e.tool.id) ? [e.tool.label] : [];
      const mine = e.members.filter((t) => picked.has(t.id));
      if (mine.length && mine.length === e.members.length) return [`${e.name} (${e.members.length} ${e.members.length === 1 ? 'skill' : 'skills'})`];
      return mine.map((t) => t.label);
    }),
  );
}

/** A group's all/none toggle: when every shown tool is picked it clears them, otherwise it
 *  picks them all. Tools hidden by the filter are left as they were. */
export function toggleGroup(picked: ReadonlySet<string>, group: ToolGroup): Set<string> {
  return togglePack(picked, group.tools);
}

/** "ffmpeg, Figma, frontend-design (+4 more)". */
export function summarizeTools(labels: readonly string[], shown = 3): string {
  if (labels.length <= shown) return labels.join(', ');
  return `${labels.slice(0, shown).join(', ')} (+${labels.length - shown} more)`;
}
