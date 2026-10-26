import { describe, expect, it } from 'vitest';
import type { AgentRequest, HivePromptRoute, Task, TaskBlocker } from '$lib/tauri/types';
import type { PendingApproval } from '$lib/stores/followUps.svelte';
import { cardTask, needsYouCount, needsYouQueue, type NeedsYouInput } from './needsYou';

const blocker = (asked_at: string, kind: TaskBlocker['kind'] = 'decision'): TaskBlocker => ({ kind, question: 'Which store?', options: [], asked_at, asked_by: 'agent' });
const task = (id: string, tab: string | null, b: TaskBlocker | null = null): Task => ({
  id,
  title: id,
  normalized_title: id,
  status: b ? 'blocked' : 'active',
  tab_id: tab,
  origin: 'agent',
  created_at: '',
  updated_at: '',
  blocker: b,
});
const route = (over: Partial<HivePromptRoute>): HivePromptRoute => ({
  tab_id: 'w1', task_id: 't1', key: 'p_1', kind: 'permission', checkpoint: 'permissions', owner: 'human', why: 'ask',
  opened_ms: 5, due_ms: 0, taken_over: false, timed_out: false, ...over,
});
const workers = [
  { badge: 1, title: 'Session store', task_id: 't1', tab_id: 'w1', phase: 'running' },
  { badge: 2, title: 'Login page', task_id: 't2', tab_id: 'w2', phase: 'landed' },
];
const input = (over: Partial<NeedsYouInput> = {}): NeedsYouInput => ({
  coordinatorTabId: 'c', workers, tasks: [], agent: () => null, routes: [], approvals: [], requests: [], ...over,
});

describe('Hive Needs you', () => {
  it("gathers a worker's sub-task blocker under the worker, a coordinator's own, and leaves external waits out", () => {
    const tasks = [
      task('t1', 'w1'),
      task('sub', 'w1', blocker('2026-10-26T10:00:00Z', 'action')),
      task('mine', 'c', blocker('2026-10-26T09:00:00Z')),
      task('ci', 'w1', blocker('2026-10-26T08:00:00Z', 'external')),
    ];
    const q = needsYouQueue(input({ tasks }));
    expect(q.map((i) => [i.kind, i.asker?.name])).toEqual([['blocker', 'Coordinator'], ['blocker', '#1 Session store']]);
    expect(needsYouCount(q)).toBe(2);
  });

  it('what the coordinator holds goes last and is not counted; a timed-out one is the human again', () => {
    const tasks = [task('t1', 'w1', blocker('2026-10-26T08:00:00Z')), task('mine', 'c', blocker('2026-10-26T09:00:00Z'))];
    const held = route({ kind: 'blocker', key: 'b_t1_2026-10-26T08:00:00Z', owner: 'coordinator', why: '' });
    let q = needsYouQueue(input({ tasks, routes: [held] }));
    expect(q.map((i) => [i.key, i.held])).toEqual([['b:mine', false], ['b:t1', true]]);
    expect(needsYouCount(q)).toBe(1);
    q = needsYouQueue(input({ tasks, routes: [{ ...held, timed_out: true }] }));
    expect(needsYouCount(q)).toBe(2);
  });

  it("an open prompt counts on a live worker or the coordinator, not on one whose agent is done", () => {
    const states: Record<string, { state: string; toolName?: string }> = {
      w1: { state: 'permission' },
      w2: { state: 'permission' },
      c: { state: 'active', toolName: 'AskUserQuestion' },
    };
    const q = needsYouQueue(input({ agent: (t) => states[t], routes: [route({ owner: 'coordinator', why: '' })] }));
    expect(q.map((i) => [i.key, i.held, i.kind === 'prompt' ? i.what : ''])).toEqual([
      ['p:c', false, 'Asking a question'],
      ['p:w1', true, 'Waiting for permission'],
    ]);
  });

  it("takes only this Hive's scripts and pending requests", () => {
    const approval = (tabId: string, id: string, archived = false) =>
      ({ workspaceId: 'ws', tabId, tabName: tabId, archived, followUp: { id, created_at: '2026-10-26T10:00:00Z' } }) as unknown as PendingApproval;
    const req = (id: string, tab_id: string, status: AgentRequest['status'] = 'pending') => ({ id, tab_id, status, requested_at: 1 }) as AgentRequest;
    const q = needsYouQueue(input({
      approvals: [approval('w1', 'f1'), approval('elsewhere', 'f2'), approval('w1', 'f3', true)],
      requests: [req('r1', 'c'), req('r2', 'elsewhere'), req('r3', 'c', 'denied')],
    }));
    expect(q.map((i) => i.key)).toEqual(['r:r1', 's:f1']);
  });

  it("a worker's card reads a blocked sub-task as its own while its task isn't blocked", () => {
    const own = task('t1', 'w1');
    const sub = task('sub', 'w1', blocker('2026-10-26T10:00:00Z', 'action'));
    expect(cardTask([own, sub], 't1', 'w1')).toMatchObject({ id: 't1', status: 'blocked', blocker: sub.blocker });
    const ownBlocked = task('t1', 'w1', blocker('2026-10-26T11:00:00Z'));
    expect(cardTask([ownBlocked, sub], 't1', 'w1')).toBe(ownBlocked);
    expect(cardTask([own], 't1', 'w1')).toBe(own);
    expect(cardTask([own, sub], 't1', null)).toBe(own);
  });
});
