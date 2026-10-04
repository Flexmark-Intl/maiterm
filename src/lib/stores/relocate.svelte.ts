/**
 * Move project (docs/relocate.md) — this window's half.
 *
 * Rust runs the move (`commands/relocate.rs`) and asks every window, over the window-request
 * channel (`mailinkRequests.ts`), for two things only its own webview can do:
 *
 *  - `relocate.suspend`: suspend each live tab whose shell sits in the folder, so no agent is
 *    still writing under the old project key while the folder moves. Service tabs are left
 *    running — a dev server keeps its open folder across a rename, and its saved `cwd` is
 *    rebased for the next start.
 *  - `relocate.apply`: take the rebased paths into the mirror (the frontend writes some of these
 *    fields back, and a stale copy would put the old path right back), then wake the tabs it
 *    suspended — they spawn in the new folder and run their auto-resume command.
 *
 * The UI state for the dialog and the startup prompt lives here too.
 */
import { homeDir } from '@tauri-apps/api/path';
import { info as logInfo } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { MissingRoot, Service, Tab } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { agentStateStore } from '$lib/stores/agentState.svelte';
import { setFallback, clearFallback, wantedFolder } from '$lib/stores/relocateFallback';

let home: string | null = null;
async function getHome(): Promise<string> {
  if (home === null) home = (await homeDir()).replace(/\/+$/, '');
  return home;
}

function expand(p: string, h: string): string {
  if (p === '~') return h;
  if (p.startsWith('~/')) return `${h}/${p.slice(2)}`;
  return p;
}

/**
 * `path` is `root` or inside it, on a component boundary. `fold` folds case — Rust decides it
 * per volume (`relocate::case_insensitive`), since macOS can mount case-sensitive volumes too.
 */
export function isUnder(path: string, root: string, h: string, fold: boolean): boolean {
  let p = expand(path.trim(), h).replace(/\/+$/, '');
  let r = expand(root.trim(), h).replace(/\/+$/, '');
  if (fold) { p = p.toLowerCase(); r = r.toLowerCase(); }
  return p === r || p.startsWith(r + '/');
}

/** Opened in home because its folder was missing (relocateFallback.ts) — resume held. */
export function noteFallback(tabId: string, wanted: string) {
  setFallback(tabId, wanted);
  logInfo(`relocate: tab ${tabId.slice(0, 8)} wanted ${wanted}, which is missing — opened in home`);
  void relocateStore.promptMissing();
}

/**
 * The live tabs in this window with a shell in the folder — what a move would restart.
 * `agentRoots`: folders where only tabs running an AGENT count. A repoint passes the new folder:
 * an agent started there before the folder was located resumed its session from the OLD project
 * key and keeps writing there (live test), while a plain shell there is fine as it is.
 */
export async function liveTabsUnder(roots: string[], fold: boolean, agentRoots: string[] = []): Promise<{ workspaceId: string; paneId: string; tab: Tab; working: boolean }[]> {
  const h = await getHome();
  const out: { workspaceId: string; paneId: string; tab: Tab; working: boolean }[] = [];
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (tab.service_id) continue;
        const inst = terminalsStore.get(tab.id);
        if (!inst) continue;
        const wanted = wantedFolder(tab.id);
        if (wanted && roots.some((r) => isUnder(wanted, r, h, fold))) {
          out.push({ workspaceId: ws.id, paneId: pane.id, tab, working: false });
          continue;
        }
        let cwd: string | null = null;
        try {
          const info = await commands.getPtyInfo(inst.ptyId);
          if (info.foreground_command) continue; // an ssh session: its folder is remote
          cwd = info.cwd;
        } catch { /* fall back to what the shell last reported */ }
        cwd ??= terminalsStore.getOsc(tab.id)?.cwd ?? tab.last_cwd ?? null;
        if (!cwd) continue;
        const agent = agentStateStore.getState(tab.id);
        const hit = roots.some((r) => isUnder(cwd!, r, h, fold))
          || (!!agent && agentRoots.some((r) => isUnder(cwd!, r, h, fold)));
        if (!hit) continue;
        out.push({ workspaceId: ws.id, paneId: pane.id, tab, working: agent?.state === 'active' });
      }
    }
  }
  return out;
}

/**
 * One move's suspensions in this window. A move can be called off AFTER this window was asked —
 * it answered too late, or another window refused — and the request it was answering is gone by
 * then, so nothing would ever wake what it suspended. Rust sends `relocate.abort` to every window
 * instead; whatever this window suspended for that move, before or after the abort, is woken.
 */
const sessions = new Map<string, { aborted: boolean; suspended: string[] }>();

function session(id: string) {
  let s = sessions.get(id);
  if (!s) { s = { aborted: false, suspended: [] }; sessions.set(id, s); }
  return s;
}

export async function suspendUnder(roots: string[], sessionId: string, fold: boolean, agentRoots: string[] = []): Promise<{ suspended: string[] } | { error: string }> {
  const s = session(sessionId);
  if (!s.aborted) {
    for (const t of await liveTabsUnder(roots, fold, agentRoots)) {
      if (s.aborted) break;
      await workspacesStore.suspendTab(t.workspaceId, t.paneId, t.tab.id);
      s.suspended.push(t.tab.id);
    }
  }
  if (s.aborted) {
    wakeTabs(s.suspended.splice(0));
    return { error: 'the move was called off' };
  }
  logInfo(`relocate: suspended ${s.suspended.length} tab(s) in ${roots[0]}`);
  return { suspended: s.suspended.slice() };
}

export function abortSession(sessionId: string): { woken: number } {
  const s = session(sessionId);
  s.aborted = true;
  const ids = s.suspended.splice(0);
  wakeTabs(ids);
  return { woken: ids.length };
}

/**
 * Wake suspended tabs through the same serial driver a workspace resume uses (+page.svelte):
 * no navigation, one mount at a time, each pane's active tab first.
 */
function wakeTabs(ids: string[]) {
  const wake = new Set(ids);
  if (wake.size === 0) return;
  const items: { workspaceId: string; paneId: string; tabId: string; label: string }[] = [];
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      for (const tab of pane.tabs) {
        if (!wake.has(tab.id)) continue;
        const item = { workspaceId: ws.id, paneId: pane.id, tabId: tab.id, label: `${ws.name} › ${tab.name || 'Terminal'}` };
        if (tab.id === pane.active_tab_id) items.unshift(item);
        else items.push(item);
      }
    }
  }
  if (items.length > 0) window.dispatchEvent(new CustomEvent('workspace-resume-tabs', { detail: items }));
}

interface Patch {
  session?: string;
  tabs?: { workspace_id: string; tab: Tab }[];
  stacks?: { workspace_id: string; stack: Service[] }[];
  wake?: string[];
}

/** Path-bearing fields only: the rest of the mirror tab is live state Rust doesn't own. */
const TAB_PATH_FIELDS = [
  'restore_cwd', 'auto_resume_cwd', 'last_cwd', 'auto_resume_command', 'auto_resume_remembered_command',
  'editor_file', 'diff_context', 'follow_ups',
] as const;

export function applyPatch(patch: Patch): { applied: number; woken: number } {
  let applied = 0;
  for (const { workspace_id, tab } of patch.tabs ?? []) {
    const ws = workspacesStore.workspaces.find((w) => w.id === workspace_id);
    const mirror = ws?.panes.flatMap((p) => p.tabs).find((t) => t.id === tab.id)
      ?? ws?.archived_tabs?.find((t) => t.id === tab.id);
    if (!mirror) continue;
    const m = mirror as unknown as Record<string, unknown>;
    const src = tab as unknown as Record<string, unknown>;
    for (const f of TAB_PATH_FIELDS) m[f] = src[f] ?? null;
    clearFallback(tab.id);
    applied++;
  }
  for (const { workspace_id, stack } of patch.stacks ?? []) {
    const ws = workspacesStore.workspaces.find((w) => w.id === workspace_id);
    if (ws) ws.stack = stack;
  }
  // Wake what this window suspended for the move — its own record, not only the ids Rust
  // echoes back, so a tab suspended after its answer was cut off still comes back.
  const ids = new Set(patch.wake ?? []);
  if (patch.session) {
    for (const id of sessions.get(patch.session)?.suspended ?? []) ids.add(id);
    sessions.delete(patch.session);
  }
  wakeTabs([...ids]);
  // Every window asks about a missing folder on its own; once any of them located it, the
  // others' prompts are moot.
  void relocateStore.closeIfLocated();
  return { applied, woken: ids.size };
}

// ── UI state ────────────────────────────────────────────────────────────────────────────────

export interface MoveRequest {
  /** 'move': maiTerm moves the folder. 'repoint': it was moved already; point maiTerm at it. */
  mode: 'move' | 'repoint';
  old: string;
  /** Prefilled new location (a repoint's best candidate), or null. */
  suggested: string | null;
  missing?: MissingRoot;
  /** This window's dialog started the run (see `closeIfLocated`). */
  ranHere?: boolean;
}

function createRelocateStore() {
  let request = $state<MoveRequest | null>(null);
  let missing = $state<MissingRoot[]>([]);
  let dismissed = $state<Set<string>>(new Set());
  let promptQueued: Promise<void> | null = null;

  return {
    get request() { return request; },
    get missing() { return missing.filter((m) => !dismissed.has(m.root)); },

    open(r: MoveRequest) { request = r; },

    /** "Move project…" from a workspace: the project its active tab sits in. */
    async openForWorkspace(workspaceId: string): Promise<string | null> {
      const ws = workspacesStore.workspaces.find((w) => w.id === workspaceId);
      const pane = ws?.panes.find((p) => p.id === ws.active_pane_id) ?? ws?.panes[0];
      const tab = pane?.tabs.find((t) => t.id === pane.active_tab_id) ?? pane?.tabs[0];
      if (!tab) return 'That workspace has no tab to take a folder from.';
      let cwd: string | null = null;
      const inst = terminalsStore.get(tab.id);
      if (inst) {
        try {
          const info = await commands.getPtyInfo(inst.ptyId);
          if (info.foreground_command) return 'Its active tab is an SSH session — only local folders can be moved.';
          cwd = info.cwd;
        } catch { /* fall through */ }
      }
      cwd ??= tab.auto_resume_cwd ?? tab.restore_cwd ?? tab.last_cwd ?? null;
      if (!cwd) return "Its active tab hasn't reported a folder yet.";
      try {
        const root = await commands.projectRootOf(cwd);
        request = { mode: 'move', old: root, suggested: null };
        return null;
      } catch (e) {
        return String(e);
      }
    },
    /** Closing a locate prompt without locating = not now, for this run. */
    close() {
      if (request?.mode === 'repoint') dismissed = new Set([...dismissed, request.old]);
      request = null;
    },

    /** Look for saved folders that are gone. Cheap: a stat per saved folder. */
    async scan() {
      try {
        missing = await commands.findMissingFolders();
      } catch { missing = []; }
      return this.missing;
    },
    /** Not now — for this run of the app. Tabs in it still fall back to home until it's located. */
    dismiss(root: string) { dismissed = new Set([...dismissed, root]); },

    async closeIfLocated() {
      const r = request;
      if (r?.mode !== 'repoint') return;
      const still = await this.scan();
      // The open dialog may be the one that ran it, showing its result: leave that one be.
      if (request === r && !still.some((m) => m.root === r.old) && !r.ranHere) request = null;
    },

    /** The dialog marks its own request when it runs, so the patch it causes doesn't close it. */
    // In place: a new object would read as a new request and reset the dialog.
    markRan() { if (request) request.ranHere = true; },

    /**
     * Ask about the first missing folder nobody has dismissed — at startup, and whenever a tab
     * finds its folder gone. Coalesced: several tabs of one project spawn together, one ask.
     */
    promptMissing(): Promise<void> {
      promptQueued ??= new Promise<void>((resolve) => setTimeout(async () => {
        promptQueued = null;
        try {
          if (request) return;
          const [first] = await this.scan();
          if (first) request = { mode: 'repoint', old: first.root, suggested: first.candidates[0] ?? null, missing: first };
        } finally {
          resolve();
        }
      }, 400));
      return promptQueued;
    },
  };
}

export const relocateStore = createRelocateStore();
