/**
 * `proposeHive` as an agent request (docs/hive.md §3.7, docs/agent-requests.md): the card the
 * phone and the Loom show, and what an approval does. Filed in the caller's window, which carries
 * it out:
 * - approved on the DESKTOP, it opens the seeded Create sheet or New goal sheet
 *   (`reviewProposal`): the human's Create there is still the approval for the Hive;
 * - approved from the PHONE, it is that Create (or Start), made from the plan the card showed
 *   (lib/hive/proposalCard.ts). The plan is built again first, and if anything on it moved the
 *   card is replaced by a fresh one rather than carried out unseen.
 */

import type { AgentRequest, HiveFolderInfo, Tab } from '$lib/tauri/types';
import {
  hiveCommitFirst,
  hiveDetectFolder,
  hiveDetectTools,
  hiveNewGoal,
  hiveNewGoalPreview,
  hivePreflight,
  hiveTrustRepo,
} from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { info as logInfo, error as logError } from '@tauri-apps/plugin-log';
import { defaultToolIds, FALLBACK_CAP, wizardStart } from './defaults';
import { pickerTools } from './media';
import { folderName, validateSpec } from './model';
import { newGoalOffered } from './newGoal';
import { proposalTarget, type HiveCandidate, type HiveProposal } from './proposal';
import { createHive } from './create';
import {
  proposalEffects,
  proposalReason,
  proposalSummary,
  samePlan,
  specFromPlan,
  type NewGoalPlan,
  type NewHivePlan,
  type PlanFolder,
  type ProposalArgs,
  type ProposalPlan,
} from './proposalCard';

export const PROPOSE_KIND = 'hive_propose';

const samePath = (a: string, b: string) => a.replace(/[\\/]+$/, '') === b.replace(/[\\/]+$/, '');

/** The Done or paused Hive this window has on the proposal's primary folder, as it reads now. */
function currentTarget(p: HiveProposal): HiveProposal['target'] {
  const candidates: HiveCandidate[] = workspacesStore.workspaces.flatMap((w) => (w.hive ? [{ workspaceId: w.id, name: w.name, hive: w.hive }] : []));
  return proposalTarget(p.folders, candidates);
}

/** The plan for a new Hive, from the folders as detected now and the human's defaults. Throws
 *  with why a folder can't be used. */
async function newHivePlan(p: HiveProposal, workspaceId: string): Promise<NewHivePlan> {
  const infos: HiveFolderInfo[] = [];
  for (const path of p.folders) {
    try {
      infos.push(await hiveDetectFolder(path));
    } catch (e) {
      throw new Error(`${path}: ${String(e).replace(/^Error: /, '')}`);
    }
  }
  const folders: PlanFolder[] = [];
  for (const [i, info] of infos.entries()) {
    if (folders.some((f) => f.path === info.path)) throw new Error(`${info.path} is named twice.`);
    const fresh = info.empty;
    const kind = fresh ? 'git' : info.kind;
    folders.push({
      path: info.path,
      name: folderName(info.path, folders.map((f) => f.name)),
      kind,
      base: kind === 'git' ? (info.base ?? (fresh ? 'main' : null)) : null,
      setup: kind === 'git' && !fresh ? (info.setup ?? null) : null,
      primary: i === 0,
      trusted: info.trusted,
      fresh,
      uncommitted: info.kind === 'git' && info.unborn && !info.empty,
    });
  }
  const d = wizardStart(preferencesStore.hiveDefaults);
  // Asked of the caller's workspace for the trust account and the default cap only: the Hive gets
  // a workspace of its own, checked again when it is made.
  const pre = await hivePreflight(workspaceId, [], []);
  if (!pre.platform_ok) throw new Error("A Hive needs Claude's sandbox, which native Windows doesn't have.");
  if (!pre.claude_ok) throw new Error(pre.claude_detail ?? 'Claude Code 2.1.295 or newer is needed for a Hive.');
  const detected = pickerTools(await hiveDetectTools().catch(() => []));
  const tools = defaultToolIds(d, detected).sort();
  const label = new Map(detected.map((t) => [t.id, t.label]));
  return {
    kind: 'new',
    folders,
    cap: p.cap ?? d.cap ?? pre.default_cap ?? FALLBACK_CAP,
    coordModel: d.coordModel,
    coordEffort: d.coordEffort,
    workerModel: p.workerModel ?? d.workerModel,
    workerEffort: p.workerEffort ?? d.workerEffort,
    budgetCents: Math.round(d.budgetDollars * 100),
    // Rust pushes only lands to Base, which is where a phone-made Hive lands.
    pushAfterLand: d.pushAfterLand,
    phone: d.phone,
    rules: d.rules,
    tools,
    toolLabels: tools.map((id) => label.get(id) ?? id),
    trustAccount: pre.trust_account ?? null,
  };
}

async function newGoalPlan(p: HiveProposal, t: Extract<HiveProposal['target'], { kind: 'new_goal' }>): Promise<NewGoalPlan> {
  const preview = await hiveNewGoalPreview(t.workspaceId);
  const hive = workspacesStore.workspaces.find((w) => w.id === t.workspaceId)?.hive;
  return {
    kind: 'new_goal',
    workspaceId: t.workspaceId,
    hiveId: t.hiveId,
    hiveName: t.hiveName,
    reasons: preview.allowed ? preview.reasons : preview.reasons.length ? preview.reasons : ["This Hive can't take a new goal right now."],
    unlanded: preview.unlanded.length,
    liveWorkers: [...preview.live_workers].sort(),
    instructions: preview.instructions,
    nextChapter: preview.next_chapter,
    missingFolders: p.folders.filter((f) => !(hive?.folders ?? []).some((h) => samePath(h.path, f))),
  };
}

function planFor(p: HiveProposal, workspaceId: string): Promise<ProposalPlan> {
  return p.target.kind === 'new_goal' ? newGoalPlan(p, p.target) : newHivePlan(p, workspaceId);
}

/** File the proposal's card. Resolves the tool's answer: the request, or why it can't be one. */
export async function fileHiveProposal(
  tab: Tab,
  p: HiveProposal,
  workspaceId: string,
  file: (kind: AgentRequest['kind'], args: Record<string, unknown>, summary: string, effects: string[], reason: string) => Promise<Record<string, unknown>>,
): Promise<Record<string, unknown>> {
  let plan: ProposalPlan;
  try {
    plan = await planFor(p, workspaceId);
  } catch (e) {
    return { error: String(e instanceof Error ? e.message : e) };
  }
  if (plan.kind === 'new') {
    // Refused to the agent, not left for the human to find on the card. A repo with no commit
    // isn't a problem here: the approval commits it, and the card says so.
    const problem = validateSpec({
      name: p.name,
      goal: p.goal,
      instructions: plan.rules,
      brief: p.brief,
      brief_files: p.briefFiles,
      folders: plan.folders.map((f) => ({ ...f, uncommitted: false })),
      cap: plan.cap,
    });
    if (problem) return { error: `This can't be offered as it is: ${problem}` };
  }
  const args: ProposalArgs = { proposal: p, plan };
  return file(PROPOSE_KIND, args as unknown as Record<string, unknown>, proposalSummary(p, plan), proposalEffects(p, plan), proposalReason(p));
}

/** Carry out an approved proposal. Resolves the outcome line; throws with why it wasn't done. */
export async function carryOutHiveProposal(
  r: AgentRequest,
  refile: (p: HiveProposal) => Promise<Record<string, unknown>>,
): Promise<string> {
  const { proposal: stored, plan } = r.args as unknown as ProposalArgs;
  if (r.decided_by !== 'phone') {
    // Dynamic: proposeHive.ts files through the request store, which imports this module.
    const { reviewProposal } = await import('./proposeHive');
    await reviewProposal(stored);
    return stored.target.kind === 'new_goal'
      ? `opened in “${stored.target.hiveName}”’s New goal sheet; nothing changes until the human starts it there`
      : 'opened in the Create sheet in a new workspace; nothing is created until the human presses Create';
  }
  // The world as it is now, against the card the human read: a Hive since made or finished on the
  // same folder changes the target, and anything else on the card can move too.
  const p: HiveProposal = { ...stored, target: currentTarget(stored) };
  const callerWs = workspacesStore.workspaces.find((w) => w.panes.some((x) => x.tabs.some((t) => t.id === r.tab_id)))?.id ?? workspacesStore.activeWorkspaceId;
  let now: ProposalPlan | null = null;
  try {
    if (callerWs) now = await planFor(p, callerWs);
  } catch (e) {
    throw new Error(`nothing was done: ${e instanceof Error ? e.message : e}`);
  }
  if (!now || p.target.kind !== stored.target.kind || !samePlan(plan, now)) {
    const res = await refile(p);
    const why = 'what it would do changed after you were asked, so nothing was done';
    throw new Error('error' in res ? `${why}, and it can't be offered again: ${res.error}` : `${why}; a fresh card shows it as it is now`);
  }
  return now.kind === 'new_goal' ? startNewGoal(p, now) : createFromPlan(p, now);
}

async function startNewGoal(p: HiveProposal, plan: NewGoalPlan): Promise<string> {
  if (plan.reasons.length || plan.unlanded) throw new Error(`“${plan.hiveName}” can't take a new goal right now: ${plan.reasons[0] ?? 'its work isn’t landed yet'}`);
  const hive = workspacesStore.workspaces.find((w) => w.id === plan.workspaceId)?.hive;
  if (!hive || hive.id !== plan.hiveId || !newGoalOffered(hive.status)) throw new Error(`“${plan.hiveName}” is no longer waiting for a goal; nothing was done`);
  await hiveNewGoal(plan.workspaceId, { goal: p.goal, brief: p.brief, brief_files: p.briefFiles, instructions: plan.instructions, keep_memory: true });
  logInfo(`hive: proposal from tab ${p.fromTabId.slice(0, 8)} started as a new goal for workspace ${plan.workspaceId.slice(0, 8)} from the phone`);
  return `“${plan.hiveName}” started on the new goal; its queen is starting a fresh session`;
}

async function createFromPlan(p: HiveProposal, plan: NewHivePlan): Promise<string> {
  // What the card said the approval does to the folders, before anything is made: a failure here
  // leaves no workspace behind.
  for (const f of plan.folders.filter((x) => x.uncommitted)) {
    const info = await hiveCommitFirst(f.path).catch((e) => { throw new Error(`committing ${f.path} failed, so nothing was created: ${e}`); });
    if (info.unborn) throw new Error(`${f.path} still has no commit, so nothing was created`);
  }
  for (const f of plan.folders.filter((x) => x.kind === 'git' && !x.trusted && !x.fresh)) {
    const info = await hiveTrustRepo(f.path).catch((e) => { throw new Error(`trusting ${f.path} failed, so nothing was created: ${e}`); });
    if (!info.trusted) throw new Error(`${f.path} was trusted, but Claude still reads it as untrusted; nothing was created`);
  }
  const ws = await workspacesStore.createWorkspace(p.name);
  try {
    const pre = await hivePreflight(ws.id, [], []);
    if (!pre.eligible) throw new Error(pre.reason ?? "the new workspace can't become a Hive");
    await createHive(ws.id, specFromPlan(p, plan));
  } catch (e) {
    // An empty workspace left from a phone approval would be clutter nobody asked for.
    await workspacesStore.deleteWorkspace(ws.id).catch((d) => logError(`hive: removing the unused workspace ${ws.id.slice(0, 8)} failed: ${d}`));
    throw new Error(`the Hive wasn't created: ${String(e instanceof Error ? e.message : e).replace(/^Error: /, '')}`);
  }
  logInfo(`hive: proposal from tab ${p.fromTabId.slice(0, 8)} created as workspace ${ws.id.slice(0, 8)} from the phone`);
  return `created “${p.name}” in a new workspace, which the desktop switched to; its queen is starting`;
}
