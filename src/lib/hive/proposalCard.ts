/**
 * A Hive proposal as an agent request (`hive_propose`, docs/hive.md §3.7): the PLAN it would carry
 * out, and the card's words for it. Pure, so the words and the comparisons are tested without a
 * webview; lib/hive/proposalRequest.ts reads the world and acts.
 *
 * The phone's Approve stands in for the Create sheet's Create (or the New goal sheet's Start), so
 * the plan is everything the sheet would have filled in, taken when the request is filed: the
 * detected folders, the human's Hive defaults, the tools those name, an empty stack. Every word on
 * the card is maiTerm's and says what the approval does, the trust and the first commit it makes
 * included. The agent's words (goal, brief) go in `reason`, shown as a quote.
 */

import type { HiveFolder, HiveSpec } from '$lib/tauri/types';
import type { HivePhone } from './defaults';
import type { HiveProposal } from './proposal';

/** A proposed folder as detected when the card was filed. */
export interface PlanFolder {
  path: string;
  name: string;
  /** "git" | "plain". */
  kind: string;
  base: string | null;
  /** Run before each worker starts, outside the sandbox (§3.6). */
  setup: string | null;
  primary: boolean;
  trusted: boolean;
  /** Empty: Create makes it a repo on `base` and trusts it (§3.6). */
  fresh: boolean;
  /** A repo with files and no commit: the approval commits them first. */
  uncommitted: boolean;
}

export interface NewHivePlan {
  kind: 'new';
  folders: PlanFolder[];
  cap: number;
  coordModel: string;
  coordEffort: string;
  workerModel: string;
  workerEffort: string;
  budgetCents: number;
  pushAfterLand: boolean;
  phone: HivePhone;
  rules: string;
  /** Sorted ids, and their labels in the same order. */
  tools: string[];
  toolLabels: string[];
  /** The managed account trust is recorded for, or null for Claude's own config. */
  trustAccount: string | null;
}

export interface NewGoalPlan {
  kind: 'new_goal';
  workspaceId: string;
  hiveId: string;
  hiveName: string;
  /** What holds New goal now (`hiveNewGoalPreview`): any of these and the approval fails. */
  reasons: string[];
  unlanded: number;
  liveWorkers: string[];
  /** The Hive's rules, kept as they are. */
  instructions: string;
  nextChapter: number;
  /** The proposal's folders the Hive doesn't have: not added. */
  missingFolders: string[];
}

export type ProposalPlan = NewHivePlan | NewGoalPlan;

/** The stored arguments: the proposal and the plan it was carded with. */
export interface ProposalArgs {
  proposal: HiveProposal;
  plan: ProposalPlan;
}

/** A value with every object's keys sorted: the stored copy comes back through serde_json, whose
 *  objects are key-sorted, so the two never stringify alike otherwise. */
function canon(v: unknown): unknown {
  if (Array.isArray(v)) return v.map(canon);
  if (v && typeof v === 'object') {
    return Object.fromEntries(Object.keys(v as object).sort().map((k) => [k, canon((v as Record<string, unknown>)[k] ?? null)]));
  }
  return v ?? null;
}

/** Whether the world still matches the card: the plan as it would be built now, against the one
 *  the human read. */
export function samePlan(stored: unknown, now: ProposalPlan): boolean {
  return JSON.stringify(canon(stored)) === JSON.stringify(canon(now));
}

const MODEL = (m: string) => m.charAt(0).toUpperCase() + m.slice(1);
const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;
const PHONE = { coordinator: 'the queen only', all: 'the queen and its workers', none: 'nobody' } as const;
/** How much of the brief the card quotes: the whole of it goes to the queen. */
const BRIEF_QUOTE = 1200;

function listed(xs: string[], shown = 4): string {
  return xs.length <= shown ? xs.join(', ') : `${xs.slice(0, shown).join(', ')} (+${xs.length - shown} more)`;
}

/** The card's one line. */
export function proposalSummary(p: HiveProposal, plan: ProposalPlan): string {
  return plan.kind === 'new_goal' ? `Give the Hive “${plan.hiveName}” a new goal` : `Create the Hive “${p.name}”`;
}

/** The agent's words, quoted on the card: the goal, then the start of the brief. */
export function proposalReason(p: HiveProposal): string {
  const brief = p.brief.trim();
  if (!brief) return p.goal;
  const cut = brief.length > BRIEF_QUOTE ? `${brief.slice(0, BRIEF_QUOTE).trimEnd()}… (${brief.length - BRIEF_QUOTE} more characters, all of it in the brief)` : brief;
  return `${p.goal}\n\n${cut}`;
}

const fileName = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;

/** The card's consequences, one line each: what Approve does, every part of it. */
export function proposalEffects(p: HiveProposal, plan: ProposalPlan): string[] {
  const files = p.briefFiles.length ? [`Brief files, copied for the queen: ${listed(p.briefFiles.map(fileName))}.`] : [];
  if (plan.kind === 'new_goal') {
    return [
      ...(plan.reasons.length || plan.unlanded
        ? [`It can't take a new goal right now: ${[...plan.reasons, ...(plan.unlanded ? [`${plural(plan.unlanded, 'branch', 'branches')} of work ${plan.unlanded === 1 ? "isn't" : "aren't"} landed yet`] : [])].join(' ')}. Approving fails until that changes; the agent can propose again.`]
        : []),
      `The current goal is kept as goal ${plan.nextChapter} in the Hive's history, and the queen starts a fresh session on this one. Its memory folder is kept.`,
      ...(plan.liveWorkers.length ? [`${plural(plan.liveWorkers.length, 'worker')} still running ${plan.liveWorkers.length === 1 ? 'stops' : 'stop'}: ${listed(plan.liveWorkers)}.`] : []),
      ...files,
      "The Hive's own settings stay: its folders, workers at once, models and rules.",
      ...(plan.missingFolders.length ? [`Not added: ${listed(plan.missingFolders)}, which the Hive doesn't work in.`] : []),
      ...(p.cap || p.workerModel || p.workerEffort ? ['The worker settings the agent suggested are not applied: a new goal keeps the Hive’s.'] : []),
      'From the phone, Approve starts it as shown. On the desktop, Open shows it in the New goal sheet to change first.',
    ];
  }
  const trustUnder = plan.trustAccount ? ` under “${plan.trustAccount}”` : '';
  const folders = plan.folders.map((f) => {
    const where = f.kind === 'git' ? `git repo, Base ${f.base ?? 'its current branch'}` : 'plain folder, shared as it is';
    const fresh = f.fresh ? `; an empty folder, so maiTerm makes it a repo on ${f.base ?? 'main'} and trusts it` : '';
    return `${f.primary ? 'Primary' : 'Folder'}: ${f.path} (${where})${fresh}.`;
  });
  const setups = plan.folders.filter((f) => f.setup).map((f) => `Before each worker starts, maiTerm runs “${f.setup}” in ${f.name}, outside the sandbox.`);
  const trusts = plan.folders
    .filter((f) => f.kind === 'git' && !f.trusted && !f.fresh)
    .map((f) => `Approving also trusts ${f.path} for Claude${trustUnder}: its project settings, hooks and MCP servers then run without asking, in every copy the Hive makes.`);
  const commits = plan.folders
    .filter((f) => f.uncommitted)
    .map((f) => `Approving also commits the files in ${f.path} (what .gitignore leaves) as the repo's first commit.`);
  return [
    ...folders,
    ...commits,
    ...trusts,
    ...setups,
    `Up to ${plural(plan.cap, `${MODEL(plan.workerModel)} worker`)} at once (${plan.workerEffort} effort); the queen is ${MODEL(plan.coordModel)} (${plan.coordEffort} effort).`,
    plan.toolLabels.length ? `Tools: ${listed(plan.toolLabels)}.` : 'No tools beyond the agents’ own.',
    `Budget $${(plan.budgetCents / 100).toFixed(plan.budgetCents % 100 ? 2 : 0)}. Work lands on Base${plan.pushAfterLand ? ' and is pushed' : ', not pushed'}.`,
    ...files,
    ...(plan.rules.trim() ? ['Rules for every agent: your default rules.'] : []),
    `On your phone: ${PHONE[plan.phone]}. Nothing in its stack yet.`,
    'From the phone, Approve creates it as shown, in a new workspace the desktop switches to. On the desktop, Open shows it in the Create sheet to change first.',
  ];
}

/** What Create sends, from the plan alone (the sheet's `create()` for these choices). */
export function specFromPlan(p: HiveProposal, plan: NewHivePlan): HiveSpec {
  const folders: HiveFolder[] = plan.folders.map((f) => ({
    name: f.name,
    path: f.path,
    kind: f.kind,
    base: f.kind === 'git' ? f.base : null,
    setup: f.kind === 'git' && !f.fresh ? f.setup : null,
    land_check: null,
    primary: f.primary,
    local_remotes: [],
  }));
  return {
    name: p.name,
    goal: p.goal,
    instructions: plan.rules,
    brief: p.brief,
    brief_files: p.briefFiles,
    folders,
    cap: plan.cap,
    landing_mode: 'base',
    models: {
      coordinator: { runtime: 'claude', model: plan.coordModel, effort: plan.coordEffort },
      worker: { runtime: 'claude', model: plan.workerModel, effort: plan.workerEffort },
    },
    tools: plan.tools,
    budget_cents: plan.budgetCents,
    push_after_land: plan.pushAfterLand,
    stack: [],
    keep_off_phone: plan.phone === 'none',
    workers_on_phone: plan.phone === 'all',
  };
}
