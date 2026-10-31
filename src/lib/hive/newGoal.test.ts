import { describe, expect, it } from 'vitest';
import { chapterLine, goalNumberLabel, newGoalOffered, newGoalProblem, pendingHeadline, pendingKey, pendingRow, showPendingLayer } from './newGoal';
import type { HiveChapter, HiveNewGoalPreview, HivePendingBranch } from '$lib/tauri/types';

const branch = (over: Partial<HivePendingBranch> = {}): HivePendingBranch => ({
  worker: 'Worker 3 · Fix the login form', badge: 3, folder: 'app', branch: 'hive/w3', target: 'main', ahead: 3, ...over,
});

const preview = (over: Partial<HiveNewGoalPreview> = {}): HiveNewGoalPreview => ({
  allowed: true, reasons: [], unlanded: [], live_workers: [], goal: 'Old', brief: '', brief_files: [], instructions: '', next_chapter: 1, memory: null, ...over,
});

const draft = { goal: 'Ship the new login', brief: '', instructions: '', briefFiles: [] };

describe('new goal', () => {
  it('is offered on a done or paused Hive only', () => {
    expect(newGoalOffered('done')).toBe(true);
    expect(newGoalOffered('paused')).toBe(true);
    expect(newGoalOffered('active')).toBe(false);
    expect(newGoalOffered(undefined)).toBe(false);
  });

  it('shows the layer only while offered and something is not landed', () => {
    expect(showPendingLayer('done', preview({ unlanded: [branch()] }))).toBe(true);
    expect(showPendingLayer('active', preview({ unlanded: [branch()] }))).toBe(false);
    expect(showPendingLayer('done', preview())).toBe(false);
    expect(showPendingLayer('paused', null)).toBe(false);
  });

  it('counts commits across branches in the headline', () => {
    expect(pendingHeadline([branch({ ahead: 1 })])).toBe("1 commit isn't landed yet");
    expect(pendingHeadline([branch({ ahead: 2 }), branch({ ahead: 3 })])).toBe("5 commits aren't landed yet");
    expect(pendingHeadline([branch({ ahead: 0 })])).toBe("A branch isn't landed yet");
  });

  it('names the place and the distance of each branch', () => {
    expect(pendingRow(branch())).toEqual({ worker: 'Worker 3 · Fix the login form', place: 'app: hive/w3 → main', ahead: '3 commits ahead' });
    expect(pendingRow(branch({ target: null, ahead: 1 })).place).toBe('app: hive/w3 → its landing target');
    expect(pendingRow(branch({ ahead: 1 })).ahead).toBe('1 commit ahead');
  });

  it('keys the pending set by what is in it', () => {
    expect(pendingKey([branch()])).toBe(pendingKey([branch()]));
    expect(pendingKey([branch()])).not.toBe(pendingKey([branch({ ahead: 4 })]));
  });

  it('refuses until the preview allows, everything is landed and there is a goal', () => {
    expect(newGoalProblem(null, draft)).toBe('Reading the Hive…');
    expect(newGoalProblem(preview({ allowed: false, reasons: ['The Hive is still working on its goal.'] }), draft)).toBe('The Hive is still working on its goal.');
    expect(newGoalProblem(preview({ allowed: false }), draft)).toBe("This Hive can't take a new goal right now.");
    expect(newGoalProblem(preview({ unlanded: [branch()] }), draft)).toBe('Land the pending commits first.');
    expect(newGoalProblem(preview(), { ...draft, goal: '  ' })).toBe('Write the new goal.');
    expect(newGoalProblem(preview(), { ...draft, brief: 'x'.repeat(32_001) })).toMatch(/brief/);
    expect(newGoalProblem(preview(), draft)).toBeNull();
  });

  it('numbers the goal only once there are chapters', () => {
    expect(goalNumberLabel({ chapters: undefined })).toBeNull();
    expect(goalNumberLabel({ chapters: [] })).toBeNull();
    const c: HiveChapter = { goal: 'A', started_at: '', ended_at: '2026-10-30T10:00:00Z', done_at: 1, report_summary: ' All met. ', spent_cents: 250 };
    expect(goalNumberLabel({ chapters: [c] })).toBe('Goal 2');
    const line = chapterLine(c, 1);
    expect(line.title).toBe('Goal 1: A');
    expect(line.meta).toMatch(/done, \$2\.50 spent$/);
    expect(line.summary).toBe('All met.');
    expect(chapterLine({ ...c, done_at: null, spent_cents: 0, ended_at: '' }, 1).meta).toBe('replaced before Done');
  });
});
