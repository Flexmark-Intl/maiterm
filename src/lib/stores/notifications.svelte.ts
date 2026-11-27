import { activityStore } from './activity.svelte';
import { preferencesStore } from './preferences.svelte';
import { workspacesStore } from './workspaces.svelte';
import { hiveView } from './hiveView.svelte';
import { terminalsStore } from './terminals.svelte';
import { dispatch } from './notificationDispatch';
import { isStripHidden } from '$lib/workspace/hiddenTabs';
import type { Tab } from '$lib/tauri/types';

/** Tracks when each tab's command started (for duration check). */
const commandStartTimes = new Map<string, number>();

function findTab(tabId: string): Tab | null {
  for (const ws of workspacesStore.workspaces) {
    for (const pane of ws.panes) {
      const tab = pane.tabs.find(t => t.id === tabId);
      if (tab) return tab;
    }
  }
  return null;
}

/** On screen: the active pane's active tab, or a Hive tab whose column is open (docs/hive.md §4.7). */
function isTabVisible(tabId: string): boolean {
  const ws = workspacesStore.activeWorkspace;
  if (!ws) return false;
  if (hiveView.tabColumnOpen(tabId)) return true;
  const pane = ws.panes.find(p => p.active_tab_id === tabId);
  if (!pane) return false;
  return pane.id === ws.active_pane_id;
}

function getTabName(tabId: string, tab: Tab): string {
  const osc = terminalsStore.getOsc(tabId);
  if (osc?.title) return osc.title;
  return tab.name || 'Terminal';
}

async function handleCommandComplete(tabId: string, exitCode: number) {
  if (preferencesStore.notificationMode === 'disabled') {
    commandStartTimes.delete(tabId);
    return;
  }
  const tab = findTab(tabId);
  // Strip-hidden tabs (Hive agents, Hive run/serve/land jobs, and workspace stack services)
  // are managed by maiTerm rather than direct human commands, so their shell exits are noise
  // (docs/hive.md §4, docs/stack.md §7).
  if (!tab || isStripHidden(tab) || isTabVisible(tabId)) {
    commandStartTimes.delete(tabId);
    return;
  }

  const startTime = commandStartTimes.get(tabId);
  commandStartTimes.delete(tabId);
  if (startTime !== undefined) {
    const elapsed = (Date.now() - startTime) / 1000;
    if (elapsed < preferencesStore.notifyMinDuration) return;
  } else {
    // No start time recorded — can't verify duration, skip
    return;
  }

  const name = getTabName(tabId, tab);
  const body = exitCode === 0
    ? `"${name}" has finished`
    : `"${name}" has finished (exit code ${exitCode})`;

  const type = exitCode === 0 ? 'success' as const : 'error' as const;
  await dispatch('Command Completed', body, type, { tabId, kind: 'completion' });
}

function handleCommandStart(tabId: string) {
  const tab = findTab(tabId);
  if (!tab || isStripHidden(tab)) {
    commandStartTimes.delete(tabId);
    return;
  }
  commandStartTimes.set(tabId, Date.now());
}

// Self-initializing subscriptions
activityStore.onCommandStart(handleCommandStart);
activityStore.onCommandComplete(handleCommandComplete);
