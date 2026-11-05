import { describe, expect, it } from 'vitest';
import { proposalEffects, proposalReason, proposalSummary, samePlan, specFromPlan, type NewGoalPlan, type NewHivePlan } from './proposalCard';
import type { HiveProposal } from './proposal';

const proposal: HiveProposal = {
  fromTabId: 't1',
  fromLabel: 'Planner',
  name: 'Site rebuild',
  goal: 'Rebuild the marketing site.',
  brief: 'Use the new brand colours.',
  briefFiles: ['/Users/me/plan/spec.md'],
  folders: ['/Users/me/site'],
  cap: null,
  workerModel: null,
  workerEffort: null,
  target: { kind: 'new' },
};

const plan: NewHivePlan = {
  kind: 'new',
  folders: [
    { path: '/Users/me/site', name: 'site', kind: 'git', base: 'main', setup: 'npm ci', primary: true, trusted: false, fresh: false, uncommitted: false },
    { path: '/Users/me/assets', name: 'assets', kind: 'plain', base: null, setup: null, primary: false, trusted: true, fresh: false, uncommitted: false },
  ],
  cap: 4,
  coordModel: 'opus',
  coordEffort: 'medium',
  workerModel: 'sonnet',
  workerEffort: 'high',
  budgetCents: 1000,
  pushAfterLand: false,
  phone: 'coordinator',
  rules: '',
  tools: ['b', 'a'],
  toolLabels: ['Figma', 'ffmpeg'],
  trustAccount: 'Work',
};

describe('a proposal card', () => {
  it('says everything Approve does to a new Hive, the trust it gives included', () => {
    const fx = proposalEffects(proposal, plan);
    expect(proposalSummary(proposal, plan)).toBe('Create the Hive “Site rebuild”');
    expect(fx).toContain('Primary: /Users/me/site (git repo, Base main).');
    expect(fx.some((l) => l.startsWith('Approving also trusts /Users/me/site for Claude under “Work”'))).toBe(true);
    expect(fx.some((l) => l.includes('runs “npm ci” in site, outside the sandbox'))).toBe(true);
    expect(fx).toContain('Up to 4 Sonnet workers at once (high effort); the queen is Opus (medium effort).');
    expect(fx).toContain('Budget $10. Work lands on Base, not pushed.');
    // A trusted plain folder is not trusted again.
    expect(fx.filter((l) => l.includes('trusts')).length).toBe(1);
  });

  it('names the media the agent expects to need, and says the phone sets none up', () => {
    expect(proposalEffects(proposal, plan).some((l) => l.startsWith('Media the agent expects'))).toBe(false);
    const fx = proposalEffects({ ...proposal, media: ['video', 'music'] }, plan);
    expect(fx.find((l) => l.startsWith('Media the agent expects to need: video, music.'))).toContain('Approving from the phone sets none up');
  });

  it('names a first commit it makes', () => {
    const fx = proposalEffects(proposal, { ...plan, folders: [{ ...plan.folders[0], uncommitted: true }] });
    expect(fx.some((l) => l.startsWith('Approving also commits the files in /Users/me/site'))).toBe(true);
  });

  it('quotes the goal and the start of a long brief as the agent’s words', () => {
    expect(proposalReason({ ...proposal, brief: '' })).toBe('Rebuild the marketing site.');
    const long = proposalReason({ ...proposal, brief: 'x'.repeat(1500) });
    expect(long).toContain('… (300 more characters, all of it in the brief)');
  });

  it('says what a new goal stops and what it can’t do yet', () => {
    const g: NewGoalPlan = {
      kind: 'new_goal', workspaceId: 'w', hiveId: 'h', hiveName: 'Site', reasons: [], unlanded: 2,
      liveWorkers: ['w-1'], instructions: '', nextChapter: 1, missingFolders: ['/Users/me/other'],
    };
    const fx = proposalEffects({ ...proposal, cap: 3 }, g);
    expect(proposalSummary(proposal, g)).toBe('Give the Hive “Site” a new goal');
    expect(fx[0]).toContain("2 branches of work aren't landed yet");
    expect(fx).toContain('1 worker still running stops: w-1.');
    expect(fx.some((l) => l.startsWith('Not added: /Users/me/other'))).toBe(true);
    expect(fx.some((l) => l.startsWith('The worker settings the agent suggested are not applied'))).toBe(true);
  });

  it('compares plans whatever order their keys come back in', () => {
    const back = JSON.parse(JSON.stringify(plan, Object.keys(plan).sort()));
    const sorted = Object.fromEntries(Object.entries(plan).sort(([a], [b]) => b.localeCompare(a)));
    expect(samePlan(sorted, plan)).toBe(true);
    expect(samePlan(back, { ...plan, folders: [{ ...plan.folders[0], trusted: true }, plan.folders[1]] })).toBe(false);
  });

  it('creates what the card showed', () => {
    const spec = specFromPlan(proposal, plan);
    expect(spec.folders.map((f) => [f.name, f.base, f.setup, f.primary])).toEqual([['site', 'main', 'npm ci', true], ['assets', null, null, false]]);
    expect(spec).toMatchObject({ cap: 4, landing_mode: 'base', stack: [], budget_cents: 1000, keep_off_phone: false, workers_on_phone: false, tools: ['b', 'a'] });
  });
});
