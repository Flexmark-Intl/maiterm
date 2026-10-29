import { describe, expect, it, vi } from 'vitest';

const { hiveBlockerAnswered } = vi.hoisted(() => ({ hiveBlockerAnswered: vi.fn(async (..._args: unknown[]): Promise<string | null> => 'D-1') }));
vi.mock('$lib/tauri/commands', () => ({ hiveBlockerAnswered }));

import { applyRoutes, blockerAnswerArg, blockerKey, findBlockerRoute, findRoute, handleNotice, noticeFor, recordHumanBlockerAnswer } from './prompts';
import type { HivePromptNotice, HivePromptRoute, TaskBlocker } from '$lib/tauri/types';

const route = (over: Partial<HivePromptRoute> = {}): HivePromptRoute => ({
  tab_id: 'k', task_id: 't1', key: 'p_k_1', kind: 'permission', checkpoint: 'permissions', owner: 'coordinator', why: '',
  opened_ms: 1, due_ms: 300_001, taken_over: false, timed_out: false, ...over,
});

const notice = (over: Partial<HivePromptNotice> = {}): HivePromptNotice => ({
  workspace_id: 'w', tab_id: 'k', kind: 'permission', title: 'Hive · #2 Session store', why: 'timeout', ...over,
});

const blocker: TaskBlocker = {
  kind: 'decision', question: 'Redis or Postgres?', options: [{ label: 'Redis', recommended: true }, { label: 'Postgres' }],
  asked_at: '2026-10-24T10:00:00Z', asked_by: 'agent',
};

describe('the routes mirror', () => {
  it('applies a workspace’s routes whole, and finds a prompt or a blocker by its key', () => {
    let m = applyRoutes(new Map(), 'w', [route(), route({ key: blockerKey('t1', blocker.asked_at), kind: 'blocker' })]);
    m = applyRoutes(m, 'w2', [route({ tab_id: 'j', key: 'q_j_1', kind: 'question' })]);
    expect(findRoute(m, 'k', 'p_k_1')?.workspaceId).toBe('w');
    expect(findRoute(m, 'j', 'q_j_1')?.workspaceId).toBe('w2');
    expect(findRoute(m, 'k', 'q_j_1')).toBeNull();
    expect(findBlockerRoute(m, 't1', blocker.asked_at)?.route.kind).toBe('blocker');
    expect(findBlockerRoute(m, 't1', 'an older asked_at')).toBeNull();
    m = applyRoutes(m, 'w', []);
    expect(findRoute(m, 'k', 'p_k_1')).toBeNull();
    expect(m.has('w')).toBe(false);
  });
});

describe('hive-prompt-notify', () => {
  it('is dispatched as hive-human, which the gate never holds back', () => {
    const dispatch = vi.fn();
    handleNotice(notice(), 300, dispatch);
    expect(dispatch).toHaveBeenCalledWith('Hive · #2 Session store', "Needs your answer: the queen hasn't answered in 5 min", 'info', { tabId: 'k', kind: 'hive-human' });
  });

  it('says why it is the human’s', () => {
    expect(noticeFor(notice({ why: 'ask', kind: 'blocker' }), 300).body).toBe("Needs your answer: Workers' questions is set to Ask me");
    expect(noticeFor(notice({ why: 'ask' }), 300).body).toBe("Needs your answer: Workers' permission prompts is set to Ask me");
    expect(noticeFor(notice({ why: 'the plan dialog’s rows change the permission mode' }), 300).body).toBe('Needs you: the plan dialog’s rows change the permission mode');
    expect(noticeFor(notice(), 600).body).toContain('in 10 min');
  });
});

describe('blocker answers into the decision log', () => {
  it('sends the option’s label and the words, only for a Hive', async () => {
    expect(blockerAnswerArg(blocker, { asked_at: blocker.asked_at, option: 1, text: ' it outlives a restart ' })).toEqual({ option: 'Postgres', text: 'it outlives a restart' });
    expect(blockerAnswerArg(blocker, { asked_at: blocker.asked_at, text: 'neither' })).toEqual({ option: null, text: 'neither' });

    const warn = vi.fn();
    expect(recordHumanBlockerAnswer({ id: 'plain' }, 't1', blocker, { asked_at: blocker.asked_at, option: 0 }, warn)).toBeNull();
    expect(recordHumanBlockerAnswer({ id: 'plain', hive: null }, 't1', blocker, { asked_at: blocker.asked_at, option: 0 }, warn)).toBeNull();
    expect(hiveBlockerAnswered).not.toHaveBeenCalled();

    await expect(recordHumanBlockerAnswer({ id: 'w', hive: {} }, 't1', blocker, { asked_at: blocker.asked_at, option: 0 }, warn)).resolves.toBe('D-1');
    expect(hiveBlockerAnswered).toHaveBeenCalledWith('w', 't1', blocker, { option: 'Redis', text: '' });

    hiveBlockerAnswered.mockRejectedValueOnce(new Error('gone'));
    await expect(recordHumanBlockerAnswer({ id: 'w', hive: {} }, 't1', blocker, { asked_at: blocker.asked_at, text: 'x' }, warn)).resolves.toBeNull();
    expect(warn).toHaveBeenCalledWith(expect.stringContaining('wasn\'t logged: Error: gone'));
  });
});
