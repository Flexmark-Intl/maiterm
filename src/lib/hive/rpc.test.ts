import { beforeEach, describe, expect, it, vi } from 'vitest';

const claudeCodeRespond = vi.fn();
const adoptHiveTask = vi.fn();
const find = vi.fn();
const remove = vi.fn();
const adoptHiveTab = vi.fn();
const dropHiveTab = vi.fn();
const dropJobTab = vi.fn((_ws: string, tab: string): string | null => (tab === 'job-tab' ? 'j-0123abcd' : null));
const logError = vi.fn();
const update = vi.fn();
const forWorkspace = vi.fn((): { id: string; tab_id?: string | null }[] => []);
const terminalGet = vi.fn();
const relaunchAgent = vi.fn();
const workspaces: { id: string; hive?: object | null; panes?: { tabs: { id: string; hive_role?: string | null }[] }[] }[] = [];

vi.mock('$lib/tauri/commands', () => ({ claudeCodeRespond }));
vi.mock('@tauri-apps/plugin-log', () => ({ error: logError }));
vi.mock('$lib/stores/tasks.svelte', () => ({ tasksStore: { adoptHiveTask, find, remove, update, forWorkspace } }));
vi.mock('$lib/stores/workspaces.svelte', () => ({
  workspacesStore: { get workspaces() { return workspaces; }, adoptHiveTab, dropHiveTab, dropJobTab },
}));
vi.mock('$lib/stores/terminals.svelte', () => ({ terminalsStore: { get: terminalGet } }));
const listFollowUps = vi.fn((_tab: string): { id: string }[] => []);
const cancelFollowUp = vi.fn(async (_tab: string, _id: string) => true);
vi.mock('$lib/stores/followUps.svelte', () => ({ followUpsStore: { relaunchAgent, list: listFollowUps, cancel: cancelFollowUp } }));
const startJob = vi.fn();
const stopJob = vi.fn();
const cleanupJobs = vi.fn();
const forgetJob = vi.fn();
const stopStack = vi.fn(async (_ws: string): Promise<string[]> => []);
vi.mock('$lib/stores/stack.svelte', () => ({ stackStore: { startJob, stopJob, cleanupJobs, forgetJob, stopStack } }));

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

  it('a coordinator answer patches the row only while its question is still the one asked', async () => {
    const blocker = { kind: 'decision', question: 'Redis or Postgres?', asked_at: 'a1', asked_by: 'agent' };
    find.mockReturnValue({ ...task, status: 'blocked', blocker, notes: [] });
    const args = { workspace_id: 'ws', task_id: 't1', status: 'active', note: 'Decided by your coordinator: Redis' };
    await handleHiveRequest({ request_id: 'e1', verb: 'hive.task', args: { ...args, expect_asked_at: 'a0' } });
    expect(update).not.toHaveBeenCalled();
    await handleHiveRequest({ request_id: 'e2', verb: 'hive.task', args: { ...args, expect_asked_at: 'a1' } });
    expect(update).toHaveBeenCalledTimes(1);
    expect(update.mock.calls[0][2].status).toBe('active');
    find.mockReturnValue({ ...task, notes: [] });
    await handleHiveRequest({ request_id: 'e3', verb: 'hive.task', args: { ...args, expect_asked_at: 'a1' } });
    expect(update).toHaveBeenCalledTimes(1);
    const results = answered().map((a) => a.result as { error?: string; ok?: boolean });
    expect(results[0].error).toMatch(/^stale/);
    expect(results[1]).toEqual({ ok: true });
    expect(results[2].error).toMatch(/^stale/);
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

  it('a job start hands the store only a job row and its line, and answers what the store says', async () => {
    const row = { id: 'j-0000abcd', kind: 'job', name: 'cargo test · auth', cwd: '/wt' };
    startJob.mockResolvedValueOnce({ accepted: true, confirmed: true, tab_id: 'tab-j', pty_id: 'pty-j' });
    await handleHiveRequest({ request_id: 'j1', verb: 'stack.job.start', args: { workspace_id: 'ws', job_id: row.id, row, line: 'cd /wt && env …' } });
    expect(startJob).toHaveBeenCalledWith('ws', row.id, row, 'cd /wt && env …');
    for (const args of [
      { workspace_id: 'ws', job_id: row.id, row: { ...row, kind: undefined }, line: 'x' },
      { workspace_id: 'ws', job_id: 'j-ffffffff', row, line: 'x' },
      { workspace_id: 'ws', job_id: row.id, row },
    ]) {
      await handleHiveRequest({ request_id: 'bad', verb: 'stack.job.start', args });
    }
    expect(startJob).toHaveBeenCalledTimes(1);
    const results = answered().map((a) => a.result);
    expect(results[0]).toEqual({ accepted: true, confirmed: true, tab_id: 'tab-j', pty_id: 'pty-j' });
    expect(results.slice(1).every((r) => typeof (r as { error?: string }).error === 'string')).toBe(true);
  });

  it('a job stop and a cleanup go to the store and are answered', async () => {
    stopJob.mockResolvedValueOnce({ status: 'stopped' });
    cleanupJobs.mockResolvedValueOnce(2).mockResolvedValueOnce(1);
    await handleHiveRequest({ request_id: 's1', verb: 'stack.job.stop', args: { workspace_id: 'ws', job_id: 'j-0000abcd', tab_id: 't9' } });
    await handleHiveRequest({ request_id: 'c1', verb: 'stack.job.cleanup', args: { workspace_id: 'ws', tab_ids: ['t1', 7], job_ids: ['j-0000abcd'] } });
    await handleHiveRequest({ request_id: 'c2', verb: 'stack.job.cleanup', args: {} });
    await handleHiveRequest({ request_id: 'c3', verb: 'stack.job.cleanup', args: { workspace_id: 'ws', tab_ids: ['t2'], force: true } });
    expect(stopJob).toHaveBeenCalledWith('ws', 'j-0000abcd');
    expect(cleanupJobs).toHaveBeenNthCalledWith(1, 'ws', ['t1'], ['j-0000abcd'], false);
    expect(cleanupJobs).toHaveBeenNthCalledWith(2, 'ws', ['t2'], [], true);
    expect(answered().map((a) => a.result)).toEqual([{ ok: true, status: 'stopped' }, { ok: true, closed: 2 }, { error: 'workspace_id is required' }, { ok: true, closed: 1 }]);
  });

  it('a job stop that did not land is an error, never a quiet ok', async () => {
    stopJob.mockResolvedValueOnce({ error: "job j-0000abcd's terminal isn't reachable in this window" });
    await handleHiveRequest({ request_id: 's2', verb: 'stack.job.stop', args: { workspace_id: 'ws', job_id: 'j-0000abcd' } });
    await handleHiveRequest({ request_id: 's3', verb: 'stack.job.stop', args: { job_id: 'j-0000abcd' } });
    const results = answered().map((a) => a.result as { ok?: boolean; error?: string });
    expect(results.every((r) => typeof r.error === 'string' && !r.ok)).toBe(true);
    expect(stopJob).toHaveBeenCalledTimes(1);
  });

  it('a retire drops the tab and moves the row to its lane with the note, keeping the row', async () => {
    find.mockReturnValue({ ...task, notes: [] });
    await handleHiveRequest({ request_id: 'rt1', verb: 'hive.retire', args: { workspace_id: 'ws', task_id: 't1', tab_id: 'tab-w', status: 'dropped', note: 'Retired: wrong approach' } });
    expect(dropHiveTab).toHaveBeenCalledWith('ws', 'tab-w');
    expect(remove).not.toHaveBeenCalled();
    const [ws, id, patch] = update.mock.calls[0];
    expect([ws, id, patch.status]).toEqual(['ws', 't1', 'dropped']);
    expect(patch.notes.map((n: { text: string }) => n.text)).toEqual(['Retired: wrong approach']);
    // A waiter has no tab; a lane that isn't one is refused before anything is dropped.
    await handleHiveRequest({ request_id: 'rt2', verb: 'hive.retire', args: { workspace_id: 'ws', task_id: 't1', tab_id: null, status: 'done' } });
    await handleHiveRequest({ request_id: 'rt3', verb: 'hive.retire', args: { workspace_id: 'ws', task_id: 't1', tab_id: 'tab-w', status: 'retired' } });
    expect(dropHiveTab).toHaveBeenCalledTimes(1);
    expect(answered().map((a) => a.result)).toEqual([{ ok: true }, { ok: true }, { error: "status retired isn't a lane" }]);
    find.mockReset();
  });

  it('a delete drops every Hive tab and clears the tab of the rows they owned, keeping the rows', async () => {
    forWorkspace.mockReturnValueOnce([{ id: 't1', tab_id: 'tab-w' }, { id: 't2', tab_id: 'coord' }, { id: 't3', tab_id: 'mine' }, { id: 't4', tab_id: null }]);
    await handleHiveRequest({ request_id: 'd1', verb: 'hive.delete', args: { workspace_id: 'ws', tab_ids: ['coord', 'tab-w', 7] } });
    expect(dropHiveTab.mock.calls).toEqual([['ws', 'coord'], ['ws', 'tab-w']]);
    expect(update.mock.calls).toEqual([['ws', 't1', { tab_id: null }], ['ws', 't2', { tab_id: null }]]);
    expect(remove).not.toHaveBeenCalled();
    expect(dropJobTab).not.toHaveBeenCalled();
    await handleHiveRequest({ request_id: 'd2', verb: 'hive.delete', args: { workspace_id: 'ws' } });
    // Its run-queue job tabs go too, and the stack forgets each job whose tab went.
    await handleHiveRequest({ request_id: 'd3', verb: 'hive.delete', args: { workspace_id: 'ws', tab_ids: [], job_tab_ids: ['job-tab', 'not-a-job'] } });
    expect(dropJobTab.mock.calls).toEqual([['ws', 'job-tab'], ['ws', 'not-a-job']]);
    expect(forgetJob.mock.calls).toEqual([['j-0123abcd']]);
    expect(answered().map((a) => a.result)).toEqual([{ ok: true }, { error: 'workspace_id and tab_ids are required' }, { ok: true }]);
  });

  it('a finish stops the stack and cancels every Hive tab\u2019s follow-ups, reporting what failed', async () => {
    listFollowUps.mockImplementation((tab: string) => (tab === 'coord' ? [{ id: 'f-1' }, { id: 'f-2' }] : tab === 'tab-w' ? [{ id: 'f-3' }] : []));
    cancelFollowUp.mockImplementation(async (_tab: string, id: string) => id !== 'f-2');
    await handleHiveRequest({ request_id: 'f1', verb: 'hive.finish', args: { workspace_id: 'ws', tab_ids: ['coord', 'tab-w'] } });
    expect(stopStack).toHaveBeenCalledWith('ws');
    expect(cancelFollowUp.mock.calls).toEqual([['coord', 'f-1'], ['coord', 'f-2'], ['tab-w', 'f-3']]);
    expect(answered()[0].result).toEqual({ ok: false, stopped: true, cancelled: 2, failed: ['follow-up f-2 on tab coord'] });
    await handleHiveRequest({ request_id: 'f2', verb: 'hive.finish', args: { workspace_id: 'plain', tab_ids: [] } });
    await handleHiveRequest({ request_id: 'f3', verb: 'hive.finish', args: { workspace_id: 'ws' } });
    expect(answered().slice(1).map((a) => a.result)).toEqual([{ error: 'This window holds no such Hive.' }, { error: 'workspace_id and tab_ids are required' }]);
    expect(stopStack).toHaveBeenCalledOnce();
    listFollowUps.mockImplementation(() => []);
    cancelFollowUp.mockImplementation(async () => true);
  });

  it('a finish reports a service that didn’t stop, never stopped:true', async () => {
    // `stopStack` swallows each copy's refusal and answers it, so the verb must read the answer.
    stopStack.mockResolvedValueOnce(['api: leased by job j-1']);
    await handleHiveRequest({ request_id: 'f4', verb: 'hive.finish', args: { workspace_id: 'ws', tab_ids: [] } });
    expect(answered()[0].result).toEqual({ ok: false, stopped: false, cancelled: 0, failed: ['stack: api: leased by job j-1'] });
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
