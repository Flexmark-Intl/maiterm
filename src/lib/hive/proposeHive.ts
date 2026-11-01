/**
 * `proposeHive` in the caller's window (docs/hive.md §3.7). The proposal becomes a request toast
 * naming the agent; Review makes a workspace for it and opens the Create sheet there, filled in
 * (`createDrafts.seed`). Nothing else happens on the agent's say-so: the human's Create is the
 * approval (§7.3), as for any Hive.
 */

import type { Tab } from '$lib/tauri/types';
import { followUpRemoteHome, hiveCheckBriefFiles } from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { isWindows } from '$lib/utils/platform';
import { createDrafts } from './createDraft';
import { parseProposal, type HiveProposal } from './proposal';
import { info as logInfo } from '@tauri-apps/plugin-log';

const requestKey = (tabId: string) => `hive-proposal:${tabId}`;

/** The toast's line about the goal: its first sentence, cut short. */
function goalLine(goal: string): string {
  const first = goal.split('\n')[0].trim();
  return first.length > 140 ? `${first.slice(0, 139).trimEnd()}…` : first;
}

async function review(p: HiveProposal): Promise<void> {
  const ws = await workspacesStore.createWorkspace(p.name);
  createDrafts.seed(ws.id, p);
  window.dispatchEvent(new CustomEvent('hive-open-create', { detail: ws.id }));
  logInfo(`hive: proposal from tab ${p.fromTabId.slice(0, 8)} opened in the Create sheet on workspace ${ws.id.slice(0, 8)}`);
}

/** The tool's answer. `tab` is the caller, already found in this window. */
export async function proposeHive(tab: Tab, args: Record<string, unknown>): Promise<Record<string, unknown>> {
  if (isWindows()) return { error: "A Hive needs Claude's sandbox, which native Windows doesn't have." };
  if (tab.hive_role) return { error: "A Hive agent can't propose a Hive." };
  // A bridged ssh tab's paths are on its host, and a Hive's folders must be on this machine.
  const remote = await followUpRemoteHome(tab.id).catch(() => ({ host: null }));
  if (remote.host) return { error: `proposeHive is for an agent on this computer: your tab runs on ${remote.host}, and a Hive's folders must be local.` };
  const osc = terminalsStore.getOsc(tab.id);
  const ownFolder = (!osc?.cwdHost ? osc?.cwd : null) ?? tab.last_cwd ?? null;
  const p = parseProposal(args, { tabId: tab.id, label: tab.name, ownFolder });
  if (typeof p === 'string') return { error: p };
  try {
    await hiveCheckBriefFiles(p.briefFiles);
  } catch (e) {
    return { error: String(e) };
  }
  // One proposal per agent: a second call replaces the first's request.
  toastStore.addRequest({
    key: requestKey(tab.id),
    title: `${p.fromLabel} proposes a Hive: ${p.name}`,
    body: `${goalLine(p.goal)}\nClick to review it in the Create sheet, in a new workspace.`,
    action: () => void review(p),
  });
  return {
    result: 'offered',
    note:
      'Your human sees a request naming you. Review opens a new workspace with the Create sheet filled in from this; ' +
      "they can change anything, and nothing is created until they press Create. You aren't told when they do: the " +
      "Hive's coordinator starts on its own and reads the brief and files. Calling again replaces this proposal.",
    name: p.name,
    folders: p.folders,
    brief_files: p.briefFiles,
  };
}
