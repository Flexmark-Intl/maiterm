/** Hive decisions with no store behind them (docs/hive.md), so tests and stores share them. */

import { MAX_CAP } from './change';

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
  /** The agent's tab; empty for a waiting worker, which has none yet (then `taskId` names it). */
  tabId: string;
  /** A worker's task: its card's identity, tab or not (§4.3). */
  taskId?: string | null;
  /** The waiting item to scroll to, as `waitingStore.open` takes it. */
  focus?: string | null;
}

/** What a notification is about, for `hiveOwnsPrompt` (docs/hive.md §4.7). */
export type HivePromptKind = 'permission' | 'question' | 'compaction' | 'completion';
export const HIVE_PROMPT_KINDS: readonly HivePromptKind[] = ['permission', 'question', 'compaction', 'completion'];
/** Rust's "this worker prompt needs you" (`hive-prompt-notify`): not a `HivePromptKind`, so the
 *  gate never holds it back. */
export const HIVE_HUMAN_KIND = 'hive-human';
export type NotifyKind = HivePromptKind | typeof HIVE_HUMAN_KIND;

/** The checkpoint a notice of `kind` falls under (Rust's `checkpoints::for_prompt_kind`):
 *  compaction and completion follow Workers' questions (plan D14). */
export function checkpointFor(kind: string): 'permissions' | 'questions' | null {
  if (kind === 'permission') return 'permissions';
  if (kind === 'question' || kind === 'compaction' || kind === 'completion') return 'questions';
  return null;
}

type CheckpointFields = { landing_mode: string; checkpoints?: Record<string, string> | null; status?: string };

/** Whether `key` is set to Ask me; a key the record lacks reads as its default for the landing
 *  mode (Rust's `checkpoints::asks`). */
export function checkpointAsks(hive: CheckpointFields, key: string): boolean {
  // Defaults as `create::default_checkpoints`: Settings asks; Landing asks in the integration mode.
  const set = hive.checkpoints?.[key] ?? (key === 'settings' || (key === 'landing' && hive.landing_mode === 'integration') ? 'ask' : 'coordinator');
  return set === 'ask';
}

/** A route as the gate and the cards read it (`HivePromptRoute`). */
export type RouteFields = { tab_id: string; key: string; kind: string; owner: string; why?: string; timed_out: boolean; taken_over: boolean };

/** The coordinator holds it alone: neither timed out nor taken over. Only then is the human's
 *  side read-only (§7.4 "With coordinator"); Rust refuses the human's answer then too (plan D12). */
export const coordinatorHolds = (r: Pick<RouteFields, 'owner' | 'timed_out' | 'taken_over'> | null | undefined): boolean =>
  !!r && r.owner === 'coordinator' && !r.timed_out && !r.taken_over;

/** Routed to the human at once (the allowlist's reason, no coordinator), under a coordinator
 *  checkpoint: Rust's `route_in` sent `hive-prompt-notify` for it, so that is the human's one
 *  notice. Claude's own permission Notification lands ~6 s after the hook Rust routes on. */
const rustNotified = (r: RouteFields): boolean =>
  r.owner === 'human' && !r.timed_out && !r.taken_over && !!r.why && !['ask', 'timeout', 'take_over'].includes(r.why);

/** Whether this tab's prompt or notice of `kind` is its Hive coordinator's to answer, so no
 *  toast, OS notification or sound goes to the human for it (§4.7); it still shows in the Loom,
 *  the corner card and the card's dot. The TS twin of Rust's `hive::owns_prompt` (the phone's).
 *
 *  Only a worker's, in a Hive that isn't done, under a checkpoint that is the coordinator's. A
 *  permission or question the coordinator holds is its own, and one Rust hasn't routed yet (the
 *  screen is read about a second after the hook) reads as owned too: Rust sends
 *  `hive-prompt-notify` if it turns out to be the human's after all (plan D13), and one it already
 *  sent that for is held too, so the human hears of it once. */
export function hiveOwnsPromptIn(
  hive: CheckpointFields | null | undefined,
  tab: Pick<TabFields, 'id' | 'hive_role'> | null | undefined,
  kind: string,
  routes: readonly RouteFields[],
): boolean {
  if (!hive || !tab || tab.hive_role !== 'worker' || hive.status === 'done') return false;
  const cp = checkpointFor(kind);
  if (!cp || checkpointAsks(hive, cp)) return false;
  if (kind === 'compaction' || kind === 'completion') return true;
  const mine = routes.filter((r) => r.tab_id === tab.id && r.kind === kind);
  return mine.every((r) => coordinatorHolds(r) || rustNotified(r));
}

/** Why the human's answer to a held worker question is refused (the phone shows it as is). */
export const HIVE_COORDINATOR_HOLDS =
  "Your Hive's coordinator answers this question. Take it over in the worker's column, or answer it once you're told it waited too long.";

/** "With coordinator" under a held prompt or question (§7.4): when the human hears of it. */
export function withCoordinatorText(dueMs: number | null | undefined, now: number): string {
  if (!dueMs) return 'With coordinator';
  const mins = Math.max(1, Math.ceil((dueMs - now) / 60_000));
  return `With coordinator · you're told if it isn't answered in ${mins} min`;
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
};

/** The first thing stopping Create, in a sentence, or null. Same rules and order as Rust's
 *  `validate_spec`; the Create wizard asks each step's part on its own. */
export function validateSpec(spec: SpecFields): string | null {
  return validateAbout(spec) ?? validateFolders(spec) ?? validateCap(spec);
}

/** The wizard's Goal step: the name, the goal, the instructions. */
export function validateAbout(spec: Pick<SpecFields, 'name' | 'goal' | 'instructions'>): string | null {
  if (!spec.name.trim()) return 'Name the Hive.';
  if (!slugify(spec.name)) return "The name needs at least one letter or digit: it names the Hive's branches.";
  if (!spec.goal.trim()) return 'Give the Hive a goal.';
  const n = charCount(spec.instructions);
  if (n > MAX_INSTRUCTIONS) return `The instructions are ${n} characters; the limit is 16,000. Shorten them: nothing is cut.`;
  return null;
}

/** The wizard's Crew step. */
export function validateCap(spec: Pick<SpecFields, 'cap'>): string | null {
  if (!Number.isInteger(spec.cap) || spec.cap < 1 || spec.cap > MAX_CAP) return `Workers at once is 1 to ${MAX_CAP}.`;
  return null;
}

/** The wizard's Folders step. */
export function validateFolders(spec: Pick<SpecFields, 'folders'>): string | null {
  const primaries = spec.folders.filter((f) => f.primary).length;
  if (primaries === 0) return 'Pick a Primary folder.';
  if (primaries > 1) return 'Only one folder can be Primary.';
  // Rust's `resolve_folders` refuses it too, once it has re-detected the repo.
  const baseless = spec.folders.find((f) => f.kind === 'git' && !f.base?.trim() && !f.detectedBase?.trim());
  if (baseless) return `${baseless.name?.trim() || baseless.path}: pick a Base branch (the repo is on a detached HEAD), or share it as a plain folder.`;
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

type AdoptTab = { id: string };
type AdoptPane<T extends AdoptTab> = { id: string; tabs: T[]; active_tab_id?: string | null };
type AdoptWorkspace<T extends AdoptTab, S> = { hive?: unknown; name: string; stack?: S[]; panes: AdoptPane<T>[] };

/** Patch what `hive_create` installed into the window's mirror of that workspace, in place, so
 *  the panes and tabs still there are left alone: the Hive, its name, the stack (the Create
 *  sheet's imports went in with the install — a mirror without them hides them, and the stack
 *  store's next whole-list write erases them), the coordinator's hidden tab, the fresh shell
 *  Rust took out, and each pane's active tab, which is Rust's. */
export function adoptInstalled<T extends AdoptTab, S>(
  mirror: AdoptWorkspace<T, S>,
  created: AdoptWorkspace<T, S>,
  coordinatorTabId: string,
  replacedTabId: string | null,
): void {
  mirror.hive = created.hive;
  mirror.name = created.name;
  mirror.stack = created.stack ?? [];
  for (const pane of created.panes) {
    const coord = pane.tabs.find((t) => t.id === coordinatorTabId);
    const into = mirror.panes.find((p) => p.id === pane.id);
    if (!into) continue;
    if (replacedTabId) into.tabs = into.tabs.filter((t) => t.id !== replacedTabId);
    if (coord && !into.tabs.some((t) => t.id === coordinatorTabId)) into.tabs.push(coord);
    into.active_tab_id = pane.active_tab_id ?? null;
  }
}

/** The header's line while paused (§7.8, §11.2), by `Hive.paused_by`: an older pause recorded no
 *  cause, and only a usage limit paused one then. */
export function pausedLabel(pausedBy: string | undefined | null): string {
  switch (pausedBy) {
    case 'human':
      return 'Paused by you: no new workers start';
    case 'account':
      return "Paused: an agent's Claude account failed. Fix it, then resume";
    default:
      return 'Paused: usage limit. No new workers start';
  }
}

/** Whether a board fetch failed because the workspace stopped being a Hive (deleted, or turned
 *  back into a workspace) while it was in flight: `hive_workers`' own refusals, or the store
 *  already without the Hive. Logged at info, not as an error. */
export function boardGone(error: string, stillHive: boolean): boolean {
  return !stillHive || /isn't a Hive|Workspace not found/.test(error);
}

/** What the header's "Throttled · memory pressure" means (§6.8). */
export function throttleTip(critical: boolean): string {
  const held = critical
    ? 'The computer is critically short of memory: no worker or queued run starts, and the heavy runs go one at a time.'
    : 'The computer is short of memory: no worker or queued run starts until it eases.';
  return `${held} A worker the coordinator starts meanwhile waits on its card and launches on its own once memory frees up.`;
}
