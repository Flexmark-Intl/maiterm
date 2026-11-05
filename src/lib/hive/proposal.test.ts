import { describe, expect, it } from 'vitest';
import { nameFromGoal, parseProposal, proposalTarget } from './proposal';

const from = { tabId: 't1', label: 'planner', ownFolder: '/Users/me/app' };

describe('parseProposal', () => {
  it('needs a goal', () => {
    expect(parseProposal({}, from)).toMatch(/goal is required/);
    expect(parseProposal({ goal: '   ' }, from)).toMatch(/goal is required/);
  });

  it("defaults the folders to the agent's own and the name to the goal", () => {
    const p = parseProposal({ goal: 'Ship the billing page. Then polish.' }, from);
    expect(p).toMatchObject({ name: 'Ship the billing page', folders: ['/Users/me/app'], briefFiles: [], cap: null, workerModel: null });
  });

  it('refuses relative paths and asks for folders when its own is unknown', () => {
    expect(parseProposal({ goal: 'x', brief_files: ['plan.md'] }, from)).toMatch(/absolute/);
    expect(parseProposal({ goal: 'x', folders: ['src'] }, from)).toMatch(/absolute/);
    expect(parseProposal({ goal: 'x' }, { ...from, ownFolder: null })).toMatch(/pass folders/);
  });

  it('keeps the given order, dropping duplicates and blanks', () => {
    const p = parseProposal({ goal: 'x', folders: ['/b', '/a', '/b', ' '], brief_files: ['/p/plan.md', '/p/plan.md'] }, from);
    expect(p).toMatchObject({ folders: ['/b', '/a'], briefFiles: ['/p/plan.md'] });
  });

  it('takes an optional media list of strings, and refuses any other shape', () => {
    expect(parseProposal({ goal: 'x' }, from)).toMatchObject({ media: [] });
    expect(parseProposal({ goal: 'x', media: [' video ', 'music', 'video', ''] }, from)).toMatchObject({ media: ['video', 'music'] });
    expect(parseProposal({ goal: 'x', media: 'video' }, from)).toMatch(/media must be a list of capability ids/);
    expect(parseProposal({ goal: 'x', media: [3] }, from)).toMatch(/media must be a list of capability ids/);
  });

  it('checks the cap, model and effort', () => {
    expect(parseProposal({ goal: 'x', cap: 0 }, from)).toMatch(/cap/);
    expect(parseProposal({ goal: 'x', cap: 2.5 }, from)).toMatch(/cap/);
    expect(parseProposal({ goal: 'x', worker_model: 'gpt' }, from)).toMatch(/worker_model/);
    expect(parseProposal({ goal: 'x', worker_effort: 'huge' }, from)).toMatch(/worker_effort/);
    expect(parseProposal({ goal: 'x', cap: 4, worker_model: 'haiku', worker_effort: 'low' }, from)).toMatchObject({ cap: 4, workerModel: 'haiku', workerEffort: 'low' });
  });

  it('refuses too many files and a brief over the limit', () => {
    const files = Array.from({ length: 11 }, (_, i) => `/f/${i}.md`);
    expect(parseProposal({ goal: 'x', brief_files: files }, from)).toMatch(/At most 10/);
    expect(parseProposal({ goal: 'x', brief: 'a'.repeat(32_001) }, from)).toMatch(/limit/);
  });
});

describe('nameFromGoal', () => {
  it('cuts a long first sentence at a word', () => {
    const n = nameFromGoal('Rebuild the onboarding flow so that new users reach their first project in under a minute');
    expect(n.length).toBeLessThanOrEqual(60);
    expect(n.endsWith(' ')).toBe(false);
  });
});

describe('proposalTarget', () => {
  const hive = (id: string, status: string, paths: string[]) => ({ workspaceId: `w-${id}`, name: `Hive ${id}`, hive: { id, status, folders: paths.map((path) => ({ path })) } });

  it('offers a new goal to a done or paused Hive working in the primary folder', () => {
    expect(proposalTarget(['/r/app/'], [hive('a', 'done', ['/r/app'])])).toEqual({ kind: 'new_goal', workspaceId: 'w-a', hiveId: 'a', hiveName: 'Hive a' });
    expect(proposalTarget(['/r/app'], [hive('b', 'paused', ['/r/lib', '/r/app'])])).toMatchObject({ kind: 'new_goal', hiveId: 'b' });
  });

  it('leaves an active Hive alone, and matches the primary folder only', () => {
    expect(proposalTarget(['/r/app'], [hive('a', 'active', ['/r/app'])])).toEqual({ kind: 'new' });
    expect(proposalTarget(['/r/web', '/r/app'], [hive('a', 'done', ['/r/app'])])).toEqual({ kind: 'new' });
    expect(proposalTarget([], [hive('a', 'done', ['/r/app'])])).toEqual({ kind: 'new' });
  });
});
