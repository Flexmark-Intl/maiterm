import { beforeEach, describe, expect, it, vi } from 'vitest';

const hiveMediaInstallArm = vi.fn(async (_i: string, _t: string, _p: string) => {});
const createTab = vi.fn(async (_ws: string, _pane: string, _name: string, _o: unknown) => ({ id: 'tab-new' }));
const deleteTab = vi.fn(async (_ws: string, _pane: string, _tab: string) => {});
const createWorkspace = vi.fn(async (_name: string) => ({ id: 'ws-setup', panes: [{ id: 'pane-setup', tabs: [] }], active_pane_id: 'pane-setup' }));
const setActiveWorkspace = vi.fn(async (_id: string) => {});
const terminalGet = vi.fn((_tab: string): { ptyId: string } | undefined => ({ ptyId: 'pty-1' }));
const typeAtPrompt = vi.fn(async (_pty: string, _line: string, arm: () => Promise<void>): Promise<{ integration: boolean; began: boolean } | { error: string }> => {
  await arm();
  return { integration: true, began: true };
});

type Ws = { id: string; hive?: object | null; overlord?: boolean; suspended?: boolean; panes: { id: string; tabs: never[] }[]; active_pane_id?: string | null };
let workspaces: Ws[] = [];
let activeWorkspaceId: string | null = null;

vi.mock('$lib/tauri/commands', () => ({ hiveMediaInstallArm }));
vi.mock('@tauri-apps/plugin-log', () => ({ info: vi.fn(), warn: vi.fn() }));
vi.mock('$lib/stores/workspaces.svelte', () => ({
  workspacesStore: {
    get workspaces() { return workspaces; },
    get activeWorkspaceId() { return activeWorkspaceId; },
    createTab, deleteTab, createWorkspace, setActiveWorkspace,
  },
}));
vi.mock('$lib/stores/terminals.svelte', () => ({ terminalsStore: { get: terminalGet, waitForRegister: vi.fn(async () => {}) } }));
vi.mock('$lib/stores/stack.svelte', () => ({ stackStore: { typeAtPrompt } }));

(globalThis as unknown as { window: unknown }).window = { dispatchEvent: () => true };
(globalThis as unknown as { CustomEvent: unknown }).CustomEvent = class { constructor(public type: string, public init: unknown) {} };

const { installWorkspace, startMediaInstall } = await import('./mediaInstall');

const plain = (id: string, extra: Partial<Ws> = {}): Ws => ({ id, panes: [{ id: `pane-${id}`, tabs: [] }], active_pane_id: `pane-${id}`, ...extra });
const args = { install_id: 'i1', provider_id: 'homebrew-kit', title: 'Install Homebrew kit', line: 'brew install sox' };

beforeEach(() => {
  vi.clearAllMocks();
  workspaces = [plain('a'), plain('b')];
  activeWorkspaceId = 'b';
});

describe('installWorkspace', () => {
  it('is the active workspace when it takes a tab', () => {
    expect(installWorkspace(workspaces as never, 'b')?.id).toBe('b');
  });
  it('skips a Hive, the Overlord and a suspended workspace', () => {
    const ws = [plain('h', { hive: {} }), plain('o', { overlord: true }), plain('s', { suspended: true }), plain('ok')];
    expect(installWorkspace(ws as never, 'h')?.id).toBe('ok');
    expect(installWorkspace(ws.slice(0, 3) as never, 'h')).toBeNull();
  });
});

describe('startMediaInstall', () => {
  it('opens a local tab, arms the install, then types the line', async () => {
    const out = await startMediaInstall(args);
    expect(out).toEqual({ accepted: true, tab_id: 'tab-new', pty_id: 'pty-1', watches_exit: true, began: true });
    expect(createTab).toHaveBeenCalledWith('b', 'pane-b', 'Install Homebrew kit', { context: { cwd: null, sshCommand: null, remoteCwd: null } });
    expect(hiveMediaInstallArm).toHaveBeenCalledWith('i1', 'tab-new', 'pty-1');
    expect(typeAtPrompt.mock.calls[0][1]).toBe('brew install sox');
  });

  it('closes its tab and types nothing when Rust refuses the arm', async () => {
    hiveMediaInstallArm.mockRejectedValueOnce('maiTerm is no longer waiting for that install.');
    typeAtPrompt.mockImplementationOnce(async (_p, _l, arm) => {
      try { await arm(); } catch (e) { return { error: String(e) }; }
      return { integration: true, began: true };
    });
    const out = await startMediaInstall(args);
    expect(out).toEqual({ error: 'maiTerm is no longer waiting for that install.' });
    expect(deleteTab).toHaveBeenCalledWith('b', 'pane-b', 'tab-new');
  });

  it('opens a Setup workspace when none takes a tab, and leaves the human where they were', async () => {
    workspaces = [plain('h', { hive: {} })];
    activeWorkspaceId = 'h';
    await startMediaInstall(args);
    expect(createWorkspace).toHaveBeenCalledWith('Setup');
    expect(setActiveWorkspace).toHaveBeenCalledWith('h');
    expect(createTab.mock.calls[0][0]).toBe('ws-setup');
  });

  it('refuses a line with a line break or missing fields', async () => {
    expect(await startMediaInstall({ ...args, line: 'a\nb' })).toHaveProperty('error');
    expect(await startMediaInstall({ install_id: 'i1' })).toHaveProperty('error');
    expect(createTab).not.toHaveBeenCalled();
  });
});
