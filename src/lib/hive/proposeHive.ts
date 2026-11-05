/**
 * `proposeHive` in the caller's window (docs/hive.md §3.7). The proposal becomes an agent request
 * (`hive_propose`, lib/hive/proposalRequest.ts) naming the agent, on the Loom and the phone. Open
 * on the desktop makes a workspace for it and opens the Create sheet there, filled in
 * (`createDrafts.seed`), and the human's Create is the approval (§7.3); a phone's Approve is that
 * Create, made from exactly what its card showed.
 */

import type { Tab } from '$lib/tauri/types';
import { followUpRemoteHome, hiveCheckBriefFiles, hiveMediaCheckRequested } from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { agentRequestsStore } from '$lib/stores/agentRequests.svelte';
import { isWindows } from '$lib/utils/platform';
import { createDrafts, newGoalProposals } from './createDraft';
import { newGoalOffered } from './newGoal';
import { parseProposal, proposalTarget, type HiveCandidate, type HiveProposal } from './proposal';
import { info as logInfo, warn as logWarn } from '@tauri-apps/plugin-log';

/** A new Hive: a workspace made for it, with the Create sheet seeded. Also the New goal sheet's
 *  "Make a new Hive instead". */
export async function reviewAsNewHive(p: HiveProposal): Promise<void> {
  const ws = await workspacesStore.createWorkspace(p.name);
  createDrafts.seed(ws.id, { ...p, target: { kind: 'new' } });
  window.dispatchEvent(new CustomEvent('hive-open-create', { detail: ws.id }));
  logInfo(`hive: proposal from tab ${p.fromTabId.slice(0, 8)} opened in the Create sheet on workspace ${ws.id.slice(0, 8)}`);
}

/** A New goal for the Done Hive it names: its workspace opened, then its New goal sheet seeded and
 *  announced. Seeded only after the switch, so a failed switch leaves nothing behind to open a
 *  sheet later; HiveView opens the sheet on the event (or on mount, if it mounts after the seed),
 *  and an open sheet takes it in place. A Hive that has since been deleted or restarted gets a new
 *  one instead. */
async function reviewAsNewGoal(p: HiveProposal, workspaceId: string, hiveId: string): Promise<void> {
  const hive = workspacesStore.workspaces.find((w) => w.id === workspaceId)?.hive;
  if (!hive || hive.id !== hiveId || !newGoalOffered(hive.status)) return reviewAsNewHive(p);
  if (workspacesStore.activeWorkspaceId !== workspaceId) await workspacesStore.setActiveWorkspace(workspaceId);
  newGoalProposals.seed(workspaceId, p);
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
  // The catalogue judges the media ids (hive/media.rs): unknown ones are dropped and named in the
  // answer, repeats count once. A catalogue that can't be asked is not a reason to refuse the plan.
  let dropped: string[] = [];
  let media: string[] = [];
  // When the catalogue can't be asked, the ids are neither accepted nor called wrong, and none is
  // echoed back: Rust's check is what bounds and shortens an agent's list.
  let mediaUnchecked = false;
  if (parsed.media?.length) {
    try {
      const checked = await hiveMediaCheckRequested(parsed.media);
      media = checked.capabilities;
      dropped = checked.dropped;
    } catch (e) {
      logWarn(`hive: couldn't check a proposal's media list: ${e}`);
      mediaUnchecked = true;
    }
  }
  const candidates: HiveCandidate[] = workspacesStore.workspaces.flatMap((w) => (w.hive ? [{ workspaceId: w.id, name: w.name, hive: w.hive }] : []));
  const p: HiveProposal = { ...parsed, media, target: proposalTarget(parsed.folders, candidates) };
  const t = p.target;
  // A request card (lib/hive/proposalRequest.ts): the Loom and the phone show it. One proposal per
  // agent: Rust ends the tab's older pending one.
  const filed = await agentRequestsStore.fileHiveProposal(tab, p);
  if ('error' in filed) return filed;
  return {
    ...filed,
    target: t.kind === 'new_goal' ? { kind: 'new_goal', hive: t.hiveName } : { kind: 'new' },
    note:
      (t.kind === 'new_goal'
        ? `${t.hiveName}, a Hive that already works in ${p.folders[0]}, is done or paused, so your human is offered this as ` +
          'its new goal rather than a second Hive on the same folder (they can still choose a new Hive). On the desktop, ' +
          'Open shows its New goal sheet filled in with your goal, brief and files, and nothing changes until they start it; ' +
          'from their phone, Approve starts it as the card shows. Its settings (workers at once, models) stay as they are.'
        : 'Your human sees a card naming you, in maiTerm and on their phone. On the desktop, Open makes a new workspace with ' +
          'the Create sheet filled in from this, which they can change, and nothing is created until they press Create; from ' +
          "their phone, Approve creates it as the card shows, with their Hive defaults. The Hive's coordinator starts on its " +
          'own and reads the brief and files.') +
      " You are told when they answer. Calling again replaces this proposal. Don't ask them to approve it in chat: only the card counts.",
    name: p.name,
    folders: p.folders,
    brief_files: p.briefFiles,
    ...(media.length ? { media } : {}),
    ...(mediaUnchecked ? { media_note: "The media list couldn't be checked against the catalogue, so it was not applied; your human can still open “What it can make” in the sheet." } : {}),
    ...(dropped.length ? { media_dropped: dropped, media_note: `Not media capabilities, so left out: ${dropped.join(', ')}.` } : {}),
  };
}
