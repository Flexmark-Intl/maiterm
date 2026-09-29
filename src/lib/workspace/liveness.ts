import type { Workspace } from '$lib/tauri/types';
import { terminalsStore } from '$lib/stores/terminals.svelte';

/** Whether a workspace is awake: what the sidebar draws undimmed and the Loom lists.
 *
 *  Derived from terminal liveness, not the standalone `suspended` flag: clicking a workspace
 *  without resuming any tab (it lands on a resume prompt with no live PTY) clears the flag but
 *  wakes nothing, and a workspace whose tabs were suspended one by one never set it. A workspace
 *  with terminal tabs is awake iff at least one has a live (or spawning) PTY. Editor/diff-only
 *  workspaces have nothing to resume, so they fall back to the flag.
 *
 *  Reactive when called inside `$derived`/`$effect` or a template: it reads the terminals
 *  store's instance version. */
export function workspaceIsLive(ws: Workspace): boolean {
  void terminalsStore.instanceVersion; // re-evaluate on PTY register/unregister
  let hasTerminalTab = false;
  for (const pane of ws.panes) {
    for (const tab of pane.tabs) {
      // A running service is not the workspace being awake: a workspace whose own tabs
      // are all suspended would read as live forever behind its dev server.
      if (tab.service_id) continue;
      const isTerminal = tab.tab_type === 'terminal' || !tab.tab_type;
      if (!isTerminal) continue;
      hasTerminalTab = true;
      if (terminalsStore.get(tab.id) || terminalsStore.isSpawning(tab.id)) return true;
    }
  }
  if (hasTerminalTab) return false;
  return !ws.suspended;
}
