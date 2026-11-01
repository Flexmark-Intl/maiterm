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
import { createDrafts, newGoalProposals } from './createDraft';
import { newGoalOffered } from './newGoal';
import { parseProposal, proposalTarget, type HiveCandidate, type HiveProposal } from './proposal';
import { info as logInfo } from '@tauri-apps/plugin-log';

const requestKey = (tabId: string) => `hive-proposal:${tabId}`;

/** The toast's line about the goal: its first sentence, cut short. */
function goalLine(goal: string): string {
  const first = goal.split('\n')[0].trim();
  return first.length > 140 ? `${first.slice(0, 139).trimEnd()}…` : first;
}

/** A new Hive: a workspace made for it, with the Create sheet seeded. Also the New goal sheet's
 *  "Make a new Hive instead". */
export async function reviewAsNewHive(p: HiveProposal): Promise<void> {
  const ws = await workspacesStore.createWorkspace(p.name);
  createDrafts.seed(ws.id, { ...p, target: { kind: 'new' } });
  window.dispatchEvent(new CustomEvent('hive-open-create', { detail: ws.id }));
  logInfo(`hive: proposal from tab ${p.fromTabId.slice(0, 8)} opened in the Create sheet on workspace ${ws.id.slice(0, 8)}`);
}

/** A New goal for the Done Hive it names: its workspace opened, and its New goal sheet seeded
 *  (HiveView opens it on mount, or on the event when it's already showing). A Hive that has
 *  since been deleted or restarted gets a new one instead. */
async function reviewAsNewGoal(p: HiveProposal, workspaceId: string, hiveId: string): Promise<void> {
  const hive = workspacesStore.workspaces.find((w) => w.id === workspaceId)?.hive;
  if (!hive || hive.id !== hiveId || !newGoalOffered(hive.status)) return reviewAsNewHive(p);
  newGoalProposals.seed(workspaceId, p);
  if (workspacesStore.activeWorkspaceId !== workspaceId) await workspacesStore.setActiveWorkspace(workspaceId);
  window.dispatchEvent(new CustomEvent('hive-open-new-goal', { detail: workspaceId }));
  logInfo(`hive: proposal from tab ${p.fromTabId.slice(0, 8)} opened as a new goal for workspace ${workspaceId.slice(0, 8)}`);
}

/** The desktop's review of a proposal, whichever its target: the seeded Create sheet in a new
 *  workspace, or the Done Hive's seeded New goal sheet. */
export function reviewProposal(p: HiveProposal): Promise<void> {
  return p.target.kind === 'new_goal' ? reviewAsNewGoal(p, p.target.workspaceId, p.target.hiveId) : reviewAsNewHive(p);
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
  const parsed = parseProposal(args, { tabId: tab.id, label: tab.name, ownFolder });
  if (typeof parsed === 'string') return { error: parsed };
  try {
    await hiveCheckBriefFiles(parsed.briefFiles);
  } catch (e) {
    return { error: String(e) };
  }
  const candidates: HiveCandidate[] = workspacesStore.workspaces.flatMap((w) => (w.hive ? [{ workspaceId: w.id, name: w.name, hive: w.hive }] : []));
  const p: HiveProposal = { ...parsed, target: proposalTarget(parsed.folders, candidates) };
  const t = p.target;
  // One proposal per agent: a second call replaces the first's request.
  toastStore.addRequest({
    key: requestKey(tab.id),
    title: t.kind === 'new_goal' ? `${p.fromLabel} proposes a new goal for ${t.hiveName}` : `${p.fromLabel} proposes a Hive: ${p.name}`,
    body:
      t.kind === 'new_goal'
        ? `${goalLine(p.goal)}\n${t.hiveName} already works in ${p.folders[0]} and is waiting for a goal. Click to review it as its new goal.`
        : `${goalLine(p.goal)}\nClick to review it in the Create sheet, in a new workspace.`,
    action: () => void reviewProposal(p),
  });
  return {
    result: 'offered',
    target: t.kind === 'new_goal' ? { kind: 'new_goal', hive: t.hiveName } : { kind: 'new' },
    note:
      t.kind === 'new_goal'
        ? `${t.hiveName}, a Hive that already works in ${p.folders[0]}, is done or paused, so your human is offered this as ` +
          "its new goal rather than a second Hive on the same folder (they can still choose a new Hive). Review opens its " +
          "New goal sheet filled in with your goal, brief and files; nothing changes until they confirm it, and you aren't " +
          'told when they do. Its settings (workers at once, models) stay as they are. Calling again replaces this proposal.'
        : 'Your human sees a request naming you. Review opens a new workspace with the Create sheet filled in from this; ' +
          "they can change anything, and nothing is created until they press Create. You aren't told when they do: the " +
          "Hive's coordinator starts on its own and reads the brief and files. Calling again replaces this proposal.",
    name: p.name,
    folders: p.folders,
    brief_files: p.briefFiles,
  };
}
