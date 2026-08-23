import { describe, expect, it } from 'vitest';
import {
  effectiveStatus,
  findDuplicate,
  hasUnmetDeps,
  makeTask,
  normalizeTitle,
  statusFromAgent,
} from './model';
import type { Task } from '$lib/tauri/types';

/** Build a task the way the store does, so `normalized_title` matches `title` unless a
 *  test deliberately overrides it (the legacy-row case below). */
const task = (over: Partial<Task> = {}): Task => {
  const base = makeTask({ title: over.title ?? 'x', tab_id: over.tab_id, status: over.status });
  return { ...base, ...over, normalized_title: over.normalized_title ?? base.normalized_title };
};

describe('normalizeTitle', () => {
  it('collapses the cosmetic drift agents produce when restating a task', () => {
    for (const input of ['Add auth guard', 'add auth guard', '  Add   auth  guard ', 'Add auth guard.', 'ADD AUTH GUARD:']) {
      expect(normalizeTitle(input)).toBe('add auth guard');
    }
  });

  it('keeps genuinely different titles apart', () => {
    expect(normalizeTitle('Add auth guard')).not.toBe(normalizeTitle('Add auth guards'));
    // Only trailing separators are trimmed — interior punctuation carries meaning.
    expect(normalizeTitle('Fix v1.2 parser')).toBe('fix v1.2 parser');
  });
});

describe('findDuplicate', () => {
  it('matches on normalized title so a re-primed agent does not duplicate its list', () => {
    const list = [task({ title: 'Write the tests', tab_id: 'a' })];
    expect(findDuplicate(list, 'write the tests.', 'a')).toBeDefined();
  });

  it('is scoped per tab — two agents may each own the same-titled task', () => {
    const list = [task({ title: 'Write the tests', tab_id: 'a' })];
    expect(findDuplicate(list, 'Write the tests', 'b')).toBeUndefined();
  });

  it('treats undefined and null assignees as the same backlog lane', () => {
    const list = [task({ title: 'Ship it', tab_id: null })];
    expect(findDuplicate(list, 'Ship it', undefined)).toBeDefined();
  });

  it('falls back to hashing the title when normalized_title was never stored', () => {
    const list = [task({ title: 'Legacy row', tab_id: 'a', normalized_title: '' })];
    expect(findDuplicate(list, 'legacy row', 'a')).toBeDefined();
  });
});

describe('dependencies', () => {
  it('blocks while a prerequisite is unfinished', () => {
    const dep = task({ id: 'd1', status: 'active' });
    const t = task({ id: 't1', status: 'backlog', blocked_by: ['d1'] });
    expect(hasUnmetDeps(t, [dep, t])).toBe(true);
    expect(effectiveStatus(t, [dep, t])).toBe('blocked');
  });

  it('unblocks once the prerequisite is done', () => {
    const dep = task({ id: 'd1', status: 'done' });
    const t = task({ id: 't1', status: 'backlog', blocked_by: ['d1'] });
    expect(effectiveStatus(t, [dep, t])).toBe('backlog');
  });

  it('ignores a prerequisite that no longer exists rather than wedging forever', () => {
    const t = task({ id: 't1', status: 'backlog', blocked_by: ['gone'] });
    expect(hasUnmetDeps(t, [t])).toBe(false);
  });

  it('never overrides done — a finished task is finished', () => {
    const dep = task({ id: 'd1', status: 'active' });
    const t = task({ id: 't1', status: 'done', blocked_by: ['d1'] });
    expect(effectiveStatus(t, [dep, t])).toBe('done');
  });
});

describe('statusFromAgent', () => {
  it('maps the runtimes\' vocabulary onto ours', () => {
    expect(statusFromAgent('completed')).toBe('done');
    expect(statusFromAgent('in_progress')).toBe('active');
    expect(statusFromAgent('pending')).toBe('backlog');
    expect(statusFromAgent(undefined)).toBe('backlog');
    expect(statusFromAgent('something-new')).toBe('backlog');
  });

  it('lets an unmet dependency win over pending/in_progress', () => {
    expect(statusFromAgent('in_progress', true)).toBe('blocked');
    // ...but not over completion.
    expect(statusFromAgent('completed', true)).toBe('done');
  });
});
