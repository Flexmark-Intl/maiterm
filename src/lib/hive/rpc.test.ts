import { beforeEach, describe, expect, it, vi } from 'vitest';

const claudeCodeRespond = vi.fn();
const adoptHiveTask = vi.fn();
const find = vi.fn();
const remove = vi.fn();
const adoptHiveTab = vi.fn();
const dropHiveTab = vi.fn();
const logError = vi.fn();
const update = vi.fn();
const terminalGet = vi.fn();
const relaunchAgent = vi.fn();
const workspaces: { id: string; hive?: object | null; panes?: { tabs: { id: string; hive_role?: string | null }[] }[] }[] = [];

vi.mock('$lib/tauri/commands', () => ({ claudeCodeRespond }));
vi.mock('@tauri-apps/plugin-log', () => ({ error: logError }));
vi.mock('$lib/stores/tasks.svelte', () => ({ tasksStore: { adoptHiveTask, find, remove, update } }));
vi.mock('$lib/stores/workspaces.svelte', () => ({
  workspacesStore: { get workspaces() { return workspaces; }, adoptHiveTab, dropHiveTab },
}));
vi.mock('$lib/stores/terminals.svelte', () => ({ terminalsStore: { get: terminalGet } }));
vi.mock('$lib/stores/followUps.svelte', () => ({ followUpsStore: { relaunchAgent } }));

const activated: string[] = [];
(globalThis as unknown as { window: unknown }).window = {
  dispatchEvent: (e: { type: string; detail: string }) => { if (e.type === 'activate-tab') activated.push(e.detail); return true; },
};
(globalThis as unknown as { CustomEvent: unknown }).CustomEvent = class {
  constructor(public type: string, init: { detail: string }) { this.detail = init.detail; }
  detail: string;
};

const { handleHiveRequest } = await import('./rpc');

const answered = () => claudeCodeRespond.mock.calls.map(([id, result]) => ({ id, result }));
const task = { id: 't1', title: 'Login API', status: 'active' };
const stream = { id: 's1', name: 'Ship it' };
const tab = { id: 'tab-w', hive_role: 'worker' };

describe('handleHiveRequest', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    activated.length = 0;
    workspaces.length = 0;
    workspaces.push(
      { id: 'ws', hive: { id: 'h1' }, panes: [{ tabs: [{ id: 'coord', hive_role: 'coordinator' }, { id: 'tab-w', hive_role: 'worker' }] }] },
      { id: 'plain', hive: null, panes: [{ tabs: [{ id: 't1' }] }] },
    );
  });

  it('a task patch appends the note, sets the status and the blocker, on the row that is here', async () => {
    find.mockReturnValue({ ...task, notes: [{ at: 'x', text: 'started', by: 'agent' }] });
    const blocker = { kind: 'external', question: 'The sandbox refused host example.com', asked_at: 'now', asked_by: 'agent' };
    await handleHiveRequest({ request_id: 'p1', verb: 'hive.task', args: { workspace_id: 'ws', task_id: 't1', note: 'Reported done: built it', status: 'review' } });
    await handleHiveRequest({ request_id: 'p2', verb: 'hive.task', args: { workspace_id: 'ws', task_id: 't1', status: 'blocked', blocker } });
    await handleHiveRequest({ request_id: 'p3', verb: 'hive.task', args: { workspace_id: 'ws', task_id: 't1', note: 'Stopped', status: null } });
    const [first, second, third] = update.mock.calls;
    expect(first[0]).toBe('ws');
    expect(first[1]).toBe('t1');
    expect(first[2].status).toBe('review');
    expect(first[2].notes.map((n: { text: string }) => n.text)).toEqual(['started', 'Reported done: built it']);
    expect(second[2]).toEqual({ status: 'blocked', blocker });
    expect(third[2].status).toBeUndefined();
    expect(answered().map((a) => a.result)).toEqual([{ ok: true }, { ok: true }, { ok: true }]);
    find.mockReset();
  });

  it('a task patch is refused for a row not here, a status off the lanes, or a blocker off Blocked', async () => {
    find.mockReturnValueOnce(undefined);
    await handleHiveRequest({ request_id: 'q1', verb: 'hive.task', args: { workspace_id: 'ws', task_id: 'gone', note: 'x' } });
    find.mockReturnValue(task);
    await handleHiveRequest({ request_id: 'q2', verb: 'hive.task', args: { workspace_id: 'ws', task_id: 't1', status: 'finished' } });
    await handleHiveRequest({ request_id: 'q3', verb: 'hive.task', args: { workspace_id: 'ws', task_id: 't1', blocker: { question: 'q' } } });
    await handleHiveRequest({ request_id: 'q4', verb: 'hive.task', args: { workspace_id: 'ws' } });
    expect(update).not.toHaveBeenCalled();
    expect(answered().every((a) => typeof (a.result as { error?: string }).error === 'string')).toBe(true);
    find.mockReset();
  });

  it('a relaunch goes through the follow-ups gate on a mounted terminal, and mounts one that is not', async () => {
    terminalGet.mockReturnValue({ ptyId: 'p' });
    relaunchAgent.mockResolvedValueOnce(null).mockResolvedValueOnce('someone is typing in the tab');
    await handleHiveRequest({ request_id: 'l1', verb: 'hive.relaunch', args: { workspace_id: 'ws', tab_id: 'tab-w' } });
    await handleHiveRequest({ request_id: 'l2', verb: 'hive.relaunch', args: { workspace_id: 'ws', tab_id: 'tab-w' } });
    expect(relaunchAgent).toHaveBeenCalledWith('tab-w');
    expect(activated).toEqual([]);
    terminalGet.mockReturnValue(undefined);
    await handleHiveRequest({ request_id: 'l3', verb: 'hive.relaunch', args: { workspace_id: 'ws', tab_id: 'tab-w' } });
    expect(activated).toEqual(['tab-w']);
    for (const tab_id of ['coord', 't1', 'nope']) {
      await handleHiveRequest({ request_id: tab_id, verb: 'hive.relaunch', args: { workspace_id: 'ws', tab_id } });
    }
    expect(answered().map((a) => a.result)).toEqual([
      { ok: true },
      { error: 'someone is typing in the tab' },
      { ok: true },
      { error: 'This window holds no such worker tab.' },
      { error: 'This window holds no such worker tab.' },
      { error: 'This window holds no such worker tab.' },
    ]);
    expect(relaunchAgent).toHaveBeenCalledTimes(2);
  });

  it('a relaunch that throws is still answered', async () => {
    terminalGet.mockReturnValue({ ptyId: 'p' });
    relaunchAgent.mockRejectedValueOnce(new Error('probe failed'));
    await handleHiveRequest({ request_id: 'l4', verb: 'hive.relaunch', args: { workspace_id: 'ws', tab_id: 'tab-w' } });
    expect(answered()).toEqual([{ id: 'l4', result: { error: 'probe failed' } }]);
    terminalGet.mockReset();
  });

  it('an unspawn with restore_task puts a waiting worker’s row back instead of removing it', async () => {
    const was = { ...task, status: 'todo', tab_id: null };
    await handleHiveRequest({ request_id: 'u3', verb: 'hive.unspawn', args: { workspace_id: 'ws', tab_id: 'tab-w', restore_task: was } });
    expect(dropHiveTab).toHaveBeenCalledWith('ws', 'tab-w');
    expect(adoptHiveTask).toHaveBeenCalledWith('ws', was, null);
    expect(remove).not.toHaveBeenCalled();
    expect(answered().map((a) => a.result)).toEqual([{ ok: true }]);
  });

  it('a launched worker: tab adopted, row adopted, tab mounted, then answered', async () => {
    await handleHiveRequest({ request_id: 'r1', verb: 'hive.spawn', args: { workspace_id: 'ws', task, workstream: stream, tab } });
    expect(adoptHiveTab).toHaveBeenCalledWith('ws', tab);
    expect(adoptHiveTask).toHaveBeenCalledWith('ws', task, stream);
    expect(activated).toEqual(['tab-w']);
    expect(answered()).toEqual([{ id: 'r1', result: { ok: true } }]);
  });

  it('a waiting worker has a row and no tab', async () => {
    await handleHiveRequest({ request_id: 'r2', verb: 'hive.spawn', args: { workspace_id: 'ws', task, workstream: stream } });
    expect(adoptHiveTab).not.toHaveBeenCalled();
    expect(adoptHiveTask).toHaveBeenCalledWith('ws', task, stream);
    expect(activated).toEqual([]);
    expect(answered()).toEqual([{ id: 'r2', result: { ok: true } }]);
  });

  it('refuses a workspace that is no Hive here, adopting nothing', async () => {
    for (const workspace_id of ['plain', 'elsewhere']) {
      await handleHiveRequest({ request_id: workspace_id, verb: 'hive.spawn', args: { workspace_id, task, tab } });
    }
    expect(adoptHiveTab).not.toHaveBeenCalled();
    expect(adoptHiveTask).not.toHaveBeenCalled();
    expect(answered().map((a) => a.result)).toEqual([{ error: 'This window holds no such Hive.' }, { error: 'This window holds no such Hive.' }]);
  });

  it('refuses malformed arguments', async () => {
    await handleHiveRequest({ request_id: 'a', verb: 'hive.spawn', args: { workspace_id: 'ws' } });
    await handleHiveRequest({ request_id: 'b', verb: 'hive.spawn', args: { workspace_id: 'ws', task, tab: 'tab-w' } });
    await handleHiveRequest({ request_id: 'c', verb: 'hive.unspawn', args: { workspace_id: 'ws' } });
    expect(answered().every((a) => typeof (a.result as { error?: string }).error === 'string')).toBe(true);
    expect(answered()).toHaveLength(3);
    expect(adoptHiveTask).not.toHaveBeenCalled();
  });

  it('answers with the error when a store throws, and the row is never adopted after a refused tab', async () => {
    adoptHiveTab.mockImplementationOnce(() => { throw new Error('Only a worker tab is adopted this way.'); });
    await handleHiveRequest({ request_id: 'r3', verb: 'hive.spawn', args: { workspace_id: 'ws', task, tab } });
    expect(adoptHiveTask).not.toHaveBeenCalled();
    expect(activated).toEqual([]);
    expect(answered()).toEqual([{ id: 'r3', result: { error: 'Only a worker tab is adopted this way.' } }]);
    expect(logError).toHaveBeenCalled();
  });

  it('unspawn takes back what was adopted, and what was not is no error', async () => {
    find.mockReturnValueOnce(task);
    await handleHiveRequest({ request_id: 'u1', verb: 'hive.unspawn', args: { workspace_id: 'ws', task_id: 't1', tab_id: 'tab-w' } });
    expect(dropHiveTab).toHaveBeenCalledWith('ws', 'tab-w');
    expect(remove).toHaveBeenCalledWith('ws', 't1');
    find.mockReturnValueOnce(undefined);
    await handleHiveRequest({ request_id: 'u2', verb: 'hive.unspawn', args: { workspace_id: 'ws', task_id: 't2', tab_id: null } });
    expect(remove).toHaveBeenCalledTimes(1);
    expect(answered().map((a) => a.result)).toEqual([{ ok: true }, { ok: true }]);
  });

  it('an unknown verb is answered, never left waiting', async () => {
    await handleHiveRequest({ request_id: 'x', verb: 'hive.land', args: {} });
    expect(answered()).toEqual([{ id: 'x', result: { error: 'Unknown Hive request: hive.land' } }]);
  });

  it('a failed answer is logged, not thrown', async () => {
    claudeCodeRespond.mockRejectedValueOnce(new Error('gone'));
    await expect(handleHiveRequest({ request_id: 'y', verb: 'hive.unspawn', args: { workspace_id: 'ws', task_id: 't' } })).resolves.toBeUndefined();
    expect(logError).toHaveBeenCalled();
  });
});
