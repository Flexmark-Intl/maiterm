/** Hive decisions with no store behind them (docs/hive.md), so tests and stores share them. */

type TabFields = { id: string; service_id?: string | null; hive_role?: string | null; overlord_exempt?: boolean };
type WorkspaceFields = { id: string; overlord_exempt?: boolean };

/** What "go to this tab" means for it (docs/hive.md §5, "Opening a Hive tab"). A hidden tab is
 *  never `setActiveTab`'d: a service shows its console, a Hive agent its column in the Hive view. */
export type OpenTarget =
  | { kind: 'hive'; workspaceId: string; tabId: string }
  | { kind: 'service'; workspaceId: string; serviceId: string }
  | { kind: 'tab'; workspaceId: string; tabId: string };

export function openTargetFor(tab: TabFields, ws: WorkspaceFields): OpenTarget {
  if (tab.hive_role) return { kind: 'hive', workspaceId: ws.id, tabId: tab.id };
  if (tab.service_id) return { kind: 'service', workspaceId: ws.id, serviceId: tab.service_id };
  return { kind: 'tab', workspaceId: ws.id, tabId: tab.id };
}

/** The Overlord engine leaves this tab alone: the human exempted it, or a Hive's coordinator
 *  already supervises it (§7.3). A Hive is never made `overlord_exempt`, which switches off far
 *  more than the engine. */
export function engineSkips(tab: TabFields, ws: WorkspaceFields): boolean {
  return !!tab.overlord_exempt || !!ws.overlord_exempt || !!tab.hive_role;
}

/** Why an Overlord tool refuses a Hive tab (reason `'hive'`). */
export const HIVE_SUPERVISED_DETAIL =
  "That tab is a Hive's agent: a Hive's coordinator supervises this tab. Leave it alone: nothing here will drive, recover, answer, resume or close it.";

/** Why a Hive workspace takes no new tab: it would be a live shell behind the Hive view. */
export const HIVE_NO_NEW_TABS = "A Hive's agents are started by the Hive, not as new tabs.";

/** Asks the Hive view (build step 3d) to open or focus a Hive agent's column. */
export const HIVE_OPEN_COLUMN = 'hive-open-column';

export interface HiveOpenColumnDetail {
  workspaceId: string;
  tabId: string;
  /** The waiting item to scroll to, as `waitingStore.open` takes it. */
  focus?: string | null;
}

/** The last open asked for, kept briefly: `navigateToTab` switches workspace and dispatches at
 *  once, which can be before the Hive view has mounted to hear it. */
let pendingOpen: { detail: HiveOpenColumnDetail; at: number } | null = null;

export function dispatchHiveOpenColumn(detail: HiveOpenColumnDetail): void {
  pendingOpen = { detail, at: Date.now() };
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent<HiveOpenColumnDetail>(HIVE_OPEN_COLUMN, { detail }));
}

/** An open for `workspaceId` asked in the last two seconds, taken once (a freshly mounted view). */
export function takePendingHiveOpen(workspaceId: string, now = Date.now()): HiveOpenColumnDetail | null {
  const p = pendingOpen;
  if (!p || p.detail.workspaceId !== workspaceId || now - p.at > 2000) return null;
  pendingOpen = null;
  return p.detail;
}

// ── The Create sheet (docs/hive.md §3). Rust re-checks all of this in `hive_create`. ──────────

/** Projects' instructions limit, in characters (Rust counts `chars()`): refused, never cut. */
export const MAX_INSTRUCTIONS = 16_000;

/** Characters as Rust counts them (code points), not UTF-16 units. */
export const charCount = (s: string) => [...s].length;

/** The `hive/<slug>` branch part: lowercase ASCII letters and digits, any other run one dash,
 *  at most 60. The twin of Rust's `hive::create::slugify`, for the preview. */
export function slugify(name: string): string {
  let out = '';
  for (const c of name) {
    if (/^[A-Za-z0-9]$/.test(c)) out += c.toLowerCase();
    else if (out && !out.endsWith('-')) out += '-';
  }
  return out.slice(0, 60).replace(/-+$/, '');
}

type SpecFolder = { path: string; primary: boolean; name?: string; kind?: string; base?: string | null; detectedBase?: string | null };
type SpecFields = {
  name: string;
  goal: string;
  instructions: string;
  folders: SpecFolder[];
  cap: number;
  cap_confirmed: boolean;
};

/** The first thing stopping Create, in a sentence, or null. Same rules and order as Rust's
 *  `validate_spec`. */
export function validateSpec(spec: SpecFields, ceiling: number): string | null {
  if (!spec.name.trim()) return 'Name the Hive.';
  if (!slugify(spec.name)) return "The name needs at least one letter or digit: it names the Hive's branches.";
  if (!spec.goal.trim()) return 'Give the Hive a goal.';
  const n = charCount(spec.instructions);
  if (n > MAX_INSTRUCTIONS) return `The instructions are ${n} characters; the limit is 16,000. Shorten them: nothing is cut.`;
  const primaries = spec.folders.filter((f) => f.primary).length;
  if (primaries === 0) return 'Pick a Primary folder.';
  if (primaries > 1) return 'Only one folder can be Primary.';
  // Rust's `resolve_folders` refuses it too, once it has re-detected the repo.
  const baseless = spec.folders.find((f) => f.kind === 'git' && !f.base?.trim() && !f.detectedBase?.trim());
  if (baseless) return `${baseless.name?.trim() || baseless.path}: pick a Base branch (the repo is on a detached HEAD), or share it as a plain folder.`;
  if (!Number.isInteger(spec.cap) || spec.cap < 1) return 'Workers at once must be at least 1.';
  if (spec.cap > ceiling && !spec.cap_confirmed) {
    return `${spec.cap} workers at once is above this computer's ceiling of ${ceiling}. Confirm it to go ahead.`;
  }
  return null;
}

/** Names Rust's `layout::validate` refuses: the coordinator's slug, and a worker's own folders
 *  that sit beside its worktrees (`paths::is_reserved_folder_name`). */
const RESERVED_FOLDER_NAMES = ['coordinator', 'scratch', 'claude-tmp', 'cargo-target'];

/** A folder's default name: its last path component, made unique among `taken` with -2, -3…
 *  Compared ignoring case, as Rust does: on macOS's default volume `Scratch` is `scratch`. */
export function folderName(path: string, taken: string[]): string {
  const base = path.replace(/[\\/]+$/, '').split(/[\\/]/).pop()?.replace(/^\.+/, '') || 'folder';
  const clash = (n: string) => {
    const lower = n.toLowerCase();
    return taken.some((t) => t.toLowerCase() === lower) || RESERVED_FOLDER_NAMES.includes(lower);
  };
  let name = base;
  for (let i = 2; clash(name); i++) name = `${base}-${i}`;
  return name;
}

type EligibleTab = {
  id: string;
  tab_type?: string;
  runtime?: string | null;
  auto_resume_command?: string | null;
  notes?: string | null;
  service_id?: string | null;
  hive_role?: string | null;
  agent_bridge?: unknown;
};
type EligibleWorkspace = {
  hive?: unknown;
  overlord?: boolean;
  bridge_all?: boolean;
  suspended?: boolean;
  panes: { tabs: EligibleTab[] }[];
  archived_tabs?: unknown[];
};
type EligibleTask = { tab_id?: string | null; status: string };

/** Whether the workspace row offers "Make this a Hive…": no tabs, or one fresh, unused shell the
 *  Hive replaces (§3). The record's half of Rust's check; whether that shell is running something
 *  only Rust can see, so the menu still asks `hive_preflight`. */
export function isEligible(ws: EligibleWorkspace, tasks: EligibleTask[]): boolean {
  if (ws.hive || ws.overlord || ws.bridge_all || ws.suspended || ws.archived_tabs?.length) return false;
  const tabs = ws.panes.flatMap((p) => p.tabs);
  if (tabs.length === 0) return true;
  if (tabs.length > 1) return false;
  const t = tabs[0];
  const fresh = (t.tab_type ?? 'terminal') === 'terminal'
    && !t.runtime
    && !t.auto_resume_command?.trim()
    && !t.notes?.trim()
    && !t.service_id
    && !t.hive_role
    && !t.agent_bridge;
  return fresh && !tasks.some((k) => k.tab_id === t.id && k.status !== 'done' && k.status !== 'dropped');
}
