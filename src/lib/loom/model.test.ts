import { describe, expect, it } from 'vitest';
import { makeTask } from '$lib/tasks/model';
import type { ChatTurn } from '$lib/tauri/commands';
import type { Task } from '$lib/tauri/types';
import { chatRows, decisionsQueue, injectedTurn, focusSections, focusSince, isQuiet, loomAgents, QUIET_MS, summarize, taskEventsFor, toolVerb, unexplainedBlocked } from './model';

const NOW = Date.parse('2026-09-27T18:00:00Z');
const task = (over: Partial<Task> = {}): Task => ({ ...makeTask({ title: over.title ?? 't' }, '2026-09-27T17:00:00Z'), ...over });
const blocker = (kind: 'decision' | 'action' | 'external', asked_at: string) => ({ kind, question: 'q', asked_at, asked_by: 'agent' as const });

describe('quiet', () => {
  it('flags only rows that claim to be moving', () => {
    const old = new Date(NOW - QUIET_MS - 1000).toISOString();
    expect(isQuiet(task({ status: 'active', updated_at: old }), NOW)).toBe(true);
    expect(isQuiet(task({ status: 'todo', updated_at: old }), NOW)).toBe(true);
    for (const status of ['backlog', 'blocked', 'review', 'done', 'dropped'] as const) {
      expect(isQuiet(task({ status, updated_at: old }), NOW)).toBe(false);
    }
    expect(isQuiet(task({ status: 'active' }), NOW)).toBe(false);
  });
});

describe('the decisions queue', () => {
  it('holds decisions and actions, oldest question first, and nothing else', () => {
    const a = task({ id: 'a', status: 'blocked', blocker: blocker('decision', '2026-09-27T10:00:00Z') });
    const b = task({ id: 'b', status: 'blocked', blocker: blocker('action', '2026-09-20T10:00:00Z') });
    const ext = task({ id: 'e', status: 'blocked', blocker: blocker('external', '2026-09-01T10:00:00Z') });
    const moved = task({ id: 'm', status: 'active', blocker: blocker('decision', '2026-09-01T10:00:00Z') });
    expect(decisionsQueue([a, b, ext, moved, task()]).map((t) => t.id)).toEqual(['b', 'a']);
  });
});

describe('blocked with no reason', () => {
  it('finds only rows nothing explains', () => {
    const dep = task({ id: 'dep', status: 'active' });
    const rows = [
      dep,
      task({ id: 'bare', status: 'blocked' }),
      task({ id: 'asks', status: 'blocked', blocker: blocker('decision', '2026-09-27T10:00:00Z') }),
      task({ id: 'waits', status: 'blocked', blocked_by: ['dep'] }),
      task({ id: 'mirror', status: 'blocked', origin: 'imported' }),
    ];
    expect(unexplainedBlocked(rows).map((t) => t.id)).toEqual(['bare']);
  });
});

describe('summary', () => {
  it('splits Blocked into needs-you and waiting, counting a dependency hold as waiting', () => {
    const dep = task({ id: 'dep', status: 'active' });
    const held = task({ id: 'held', status: 'todo', blocked_by: ['dep'] });
    const ask = task({ id: 'ask', status: 'blocked', blocker: blocker('decision', '2026-09-27T10:00:00Z') });
    const ext = task({ id: 'ext', status: 'blocked', blocker: blocker('external', '2026-09-27T10:00:00Z') });
    expect(summarize([dep, held, ask, ext], NOW)).toEqual({ active: 1, needsYou: 1, waiting: 2, quiet: 0 });
  });

  it('counts only what the loom draws: a parked row held by a dependency is not waiting', () => {
    const dep = task({ id: 'dep', status: 'active' });
    const parked = task({ id: 'p', status: 'backlog', blocked_by: ['dep'] });
    expect(summarize([dep, parked], NOW)).toEqual({ active: 1, needsYou: 0, waiting: 0, quiet: 0 });
  });

  it('never stores a dependency twice', () => {
    expect(makeTask({ title: 'b', blocked_by: ['a', 'a'] }).blocked_by).toEqual(['a']);
  });
});

describe('the agent column', () => {
  it('keeps tabs with a session or open work, most urgent first, and says what a working agent is doing', () => {
    const tasks = [task({ id: 'x', tab_id: 'gone' }), task({ id: 'y', tab_id: 'done-only', status: 'done' })];
    const rows = loomAgents(
      [
        { tabId: 'idle', name: 'b', state: 'idle', toolDetail: 'stale' },
        { tabId: 'work', name: 'a', state: 'active', toolDetail: 'Bash: npm test' },
        { tabId: 'ask', name: 'c', state: 'permission' },
        { tabId: 'gone', name: 'd', state: null },
        { tabId: 'done-only', name: 'e', state: null },
      ],
      tasks,
    );
    expect(rows.map((r) => r.tabId)).toEqual(['ask', 'work', 'idle', 'gone']);
    expect(rows.find((r) => r.tabId === 'work')!.doing).toBe('Bash: npm test');
    expect(rows.find((r) => r.tabId === 'idle')!.doing).toBeNull();
    expect(rows.find((r) => r.tabId === 'gone')!.taskIds).toEqual(['x']);
  });
});

describe('the condensed chat', () => {
  const turn = (msg_id: string, role: ChatTurn['role'], text = '', kind?: ChatTurn['kind']): ChatTurn => ({ msg_id, role, text, ts: 0, ...(kind ? { kind } : {}) });

  it('uses the phone vocabulary for tool verbs', () => {
    expect(toolVerb('Read(src/a.ts)')).toBe('reading files');
    expect(toolVerb('Edit(src/a.ts)')).toBe('editing files');
    expect(toolVerb('Bash(npm test)')).toBe('bash');
    expect(toolVerb('mcp__maiterm__listTasks')).toBe('maiterm');
    expect(toolVerb('WebFetch(https://x)')).toBe('web');
  });

  it('folds each run of tool turns into one row and keeps events as rules', () => {
    const rows = chatRows([
      turn('1', 'user', 'go'),
      turn('2', 'tool', 'Read(a)'),
      turn('3', 'tool', 'Grep(b)'),
      turn('4', 'tool', 'Edit(a)'),
      turn('5', 'system', 'peer', 'peer_message'),
      turn('6', 'tool', 'Bash(x)'),
      turn('7', 'agent', 'done'),
    ]);
    expect(rows.map((r) => r.kind)).toEqual(['turn', 'tools', 'rule', 'tools', 'turn']);
    const run = rows[1];
    expect(run.kind === 'tools' && run.key).toBe('2');
    expect(run.kind === 'tools' && run.verbs).toEqual(['reading files', 'editing files']);
  });
});

describe('task events in the chat', () => {
  const at = (s: string) => Date.parse(`2026-09-27T${s}Z`);
  const turn = (msg_id: string, role: ChatTurn['role'], ts: number, kind?: ChatTurn['kind']): ChatTurn => ({ msg_id, role, text: msg_id, ts, ...(kind ? { kind } : {}) });
  const t = task({
    id: 'k', title: 'Deploy', tab_id: 'tab', created_at: '2026-09-27T10:00:00Z',
    notes: [
      { at: '2026-09-27T10:05:00Z', text: 'Waiting on your decision: Deploy now?', by: 'agent' },
      { at: '2026-09-27T10:09:00Z', text: 'Decided: Wait', by: 'human' },
      { at: '2026-09-27T10:10:00Z', text: 'tests green', by: 'agent' },
    ],
  });

  it('classifies a task log by what wrote it', () => {
    expect(taskEventsFor('tab', [t, task({ tab_id: 'other' })]).map((e) => e.kind)).toEqual(['added', 'asked', 'answered', 'note']);
  });

  it('places events by time, breaking tool runs, and drops ones older than the chat', () => {
    const rows = chatRows(
      [turn('u', 'user', at('10:01:00')), turn('r1', 'tool', at('10:04:00')), turn('r2', 'tool', at('10:06:00')), turn('a', 'agent', at('10:11:00'))],
      taskEventsFor('tab', [t]),
    );
    expect(rows.map((r) => (r.kind === 'task' ? r.event.kind : r.kind))).toEqual(['turn', 'tools', 'asked', 'tools', 'answered', 'note', 'turn']);
  });

  it('gathers tasks added back to back into one block', () => {
    const made = (id: string, at: string) => task({ id, title: id, tab_id: 'tab', created_at: `2026-09-27T${at}Z`, notes: [] });
    const rows = chatRows(
      [turn('u', 'user', at('10:00:00')), turn('a', 'agent', at('10:10:00'))],
      taskEventsFor('tab', [made('x', '10:01:00'), made('y', '10:01:00'), made('z', '10:02:00')]),
    );
    expect(rows.map((r) => r.kind)).toEqual(['turn', 'added', 'turn']);
    const block = rows[1];
    expect(block.kind === 'added' && block.events.map((e) => e.taskId)).toEqual(['x', 'y', 'z']);
  });

  it('keys two notes written in the same millisecond apart', () => {
    const same = task({ id: 'd', tab_id: 'tab', notes: [{ at: '2026-09-27T10:05:00Z', text: 'a', by: 'agent' }, { at: '2026-09-27T10:05:00Z', text: 'b', by: 'agent' }] });
    const keys = taskEventsFor('tab', [same]).map((e) => e.key);
    expect(new Set(keys).size).toBe(keys.length);
  });

  it('puts nothing into a live-screen snapshot', () => {
    const rows = chatRows([turn('ctx_tab', 'system', at('10:30:00'), 'terminal_snapshot')], taskEventsFor('tab', [t]));
    expect(rows.map((r) => r.kind)).toEqual(['turn']);
  });
});

describe('turns the harness wrote', () => {
  it('folds a subagent hand-back to its report', () => {
    const text = 'Another Claude session sent a message:\n<agent-message from="a1">\n[Subagent hand-back] The text below is framing. The report follows:\n  ## Findings\n\n  **1.** a bug\n</agent-message>\n\nThat "other Claude session" is…';
    expect(injectedTurn(text)).toEqual({ label: 'Subagent report received', body: '## Findings\n\n**1.** a bug' });
  });

  it('leaves the human alone', () => {
    expect(injectedTurn('please look at the agent-message handling')).toBeNull();
  });
});

describe('Focus', () => {
  const chat = (tabId: string, over: Partial<Parameters<typeof focusSections>[0][number]> = {}) => ({
    tabId, state: 'idle' as const, unread: false, lastActivity: 0, asks: false, ...over,
  });

  it('starts its window at local midnight yesterday', () => {
    const d = new Date(focusSince(NOW));
    expect([d.getHours(), d.getMinutes()]).toEqual([0, 0]);
    expect(NOW - d.getTime()).toBeGreaterThan(24 * 3600 * 1000);
    expect(NOW - d.getTime()).toBeLessThanOrEqual(48 * 3600 * 1000);
  });

  it('widens and narrows the recent window by whole local days', () => {
    const today = new Date(focusSince(NOW, 0));
    expect([today.getHours(), today.getMinutes()]).toEqual([0, 0]);
    expect(NOW - today.getTime()).toBeLessThanOrEqual(24 * 3600 * 1000);
    expect(focusSince(NOW, 6)).toBeLessThan(focusSince(NOW, 1));
    const threeDaysAgo = chat('old', { lastActivity: NOW - 3 * 24 * 3600 * 1000 });
    expect(focusSections([threeDaysAgo], NOW, null, 1).recent).toEqual([]);
    expect(focusSections([threeDaysAgo], NOW, null, 6).recent.map((c) => c.tabId)).toEqual(['old']);
  });

  it('pins what needs you, then working, then recent; old read chats drop out', () => {
    const s = focusSections(
      [
        chat('old'),
        chat('recent', { lastActivity: NOW - 3600_000 }),
        chat('unread', { unread: true }),
        chat('working', { state: 'active' }),
        chat('ask', { asks: true, state: 'active' }),
        chat('perm', { state: 'permission' as never }),
      ],
      NOW,
    );
    expect(s.needsYou.map((c) => c.tabId)).toEqual(['ask', 'perm']);
    expect(s.working.map((c) => c.tabId)).toEqual(['working']);
    expect(s.recent.map((c) => c.tabId)).toEqual(['recent', 'unread']);
  });

  it('keeps the open chat listed when it qualifies for nothing else', () => {
    const s = focusSections([chat('gone', { state: null })], NOW, 'gone');
    expect(s.recent.map((c) => c.tabId)).toEqual(['gone']);
    expect(focusSections([chat('gone', { state: null })], NOW).recent).toEqual([]);
  });
});
