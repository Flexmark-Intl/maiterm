import { beforeEach, describe, expect, it, vi } from 'vitest';

const claudeCodeRespond = vi.fn();
const adoptHiveTask = vi.fn();
const find = vi.fn();
const remove = vi.fn();
const adoptHiveTab = vi.fn();
const dropHiveTab = vi.fn();
const logError = vi.fn();
const workspaces: { id: string; hive?: object | null }[] = [];

vi.mock('$lib/tauri/commands', () => ({ claudeCodeRespond }));
vi.mock('@tauri-apps/plugin-log', () => ({ error: logError }));
vi.mock('$lib/stores/tasks.svelte', () => ({ tasksStore: { adoptHiveTask, find, remove } }));
vi.mock('$lib/stores/workspaces.svelte', () => ({
  workspacesStore: { get workspaces() { return workspaces; }, adoptHiveTab, dropHiveTab },
}));

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
    workspaces.push({ id: 'ws', hive: { id: 'h1' } }, { id: 'plain', hive: null });
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
