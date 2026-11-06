import { describe, expect, it, vi } from 'vitest';
import type { McpServerView } from '$lib/tauri/commands';

vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));
vi.mock('$lib/tauri/commands', () => ({ mcpServerSignIn: vi.fn() }));

const { StdioSetup, keyHint, keySendable, stdioPlan, defaultRoute, MIN_KEY_LEN } = await import('./mediaStdio');
type Deps = import('./mediaStdio').StdioDeps;
type State = import('./mediaStdio').StdioState;

const KEY = 'msy_0123456789abcdefSECRET';

function view(id: string, over: Partial<McpServerView> = {}): McpServerView {
  return {
    id, url: `stdio:${id}`, auth: 'key', enabled: true, workspaces: [], status: { state: 'connected', detail: '' } as McpServerView['status'],
    tools: [], fetched_at: null, added_at: 1, added_by: 'human', entry_name: id, collisions: [], duplicates: [], transport: 'stdio', route: id, ...over,
  };
}

/** Records every call in order, and every state the sheet would mirror. */
function rig(over: Partial<Deps> = {}) {
  const calls: string[] = [];
  const states: State[] = [];
  const deps: Deps = {
    keyStore: vi.fn(async (id: string, key: string) => void calls.push(`store ${id} ${key.length}`)),
    serverAdd: vi.fn(async (id: string, route: string, ws: string[]) => (calls.push(`add ${id} ${route} [${ws.join(',')}]`), view(id))),
    setWorkspaces: vi.fn(async (id: string, ws: string[]) => void calls.push(`scope ${id} [${ws.join(',')}]`)),
    keyDelete: vi.fn(async (id: string) => void calls.push(`delete ${id}`)),
    verify: vi.fn(async () => void calls.push('verify')),
    landed: vi.fn((s: McpServerView) => void calls.push(`landed ${s.id}`)),
    ...over,
  };
  const setup = new StdioSetup(deps, (s) => states.push(s));
  return { setup, deps, calls, states };
}

const target = (over = {}) => ({ id: 'meshy', routeId: 'meshy', reuse: null, scope: ['ws-hive'], reuseScope: null, ...over });

describe('the stdio setup: store, add, verify', () => {
  it('stores the key, then adds the server from its route scoped to the Hive, then verifies', async () => {
    const { setup, calls } = rig();
    await setup.submit(KEY, target());
    expect(calls).toEqual([`store meshy ${KEY.length}`, 'add meshy meshy [ws-hive]', 'landed meshy', 'verify']);
    expect(setup.state.stage).toBe('added');
    expect(setup.state.server?.id).toBe('meshy');
  });

  it('never holds the key after sending it: not in any state, nor the object', async () => {
    const { setup, states } = rig({ serverAdd: vi.fn(async () => Promise.reject(new Error('maiTerm could not set up meshy: its server exited'))) });
    await setup.submit(KEY, target());
    await setup.retry();
    await setup.removeKey();
    expect(states.length).toBeGreaterThan(3);
    for (const s of states) expect(JSON.stringify(s)).not.toContain('SECRET');
    expect(JSON.stringify(setup)).not.toContain('SECRET');
    expect(JSON.stringify(setup.state)).not.toContain('SECRET');
  });

  it('adds nothing when the key store is refused, and asks for the key again', async () => {
    const { setup, calls, deps } = rig({ keyStore: vi.fn(async () => Promise.reject(new Error("keychain store failed: that doesn't look like a whole key"))) });
    await setup.submit('short', target());
    expect(deps.serverAdd).not.toHaveBeenCalled();
    expect(deps.verify).not.toHaveBeenCalled();
    expect(calls).toEqual([]);
    expect(setup.state).toMatchObject({ stage: 'key', keyFor: null, error: "keychain store failed: that doesn't look like a whole key" });
  });

  it('a failed first start keeps the key: Try again adds without a second paste', async () => {
    let fail = true;
    const { setup, calls } = rig({
      serverAdd: vi.fn(async (id: string, route: string) => {
        calls.push(`add ${id} ${route}`);
        if (fail) throw new Error('maiTerm could not set up meshy: it took longer than 180 s');
        return view(id);
      }),
    });
    await setup.submit(KEY, target());
    expect(setup.state).toMatchObject({ stage: 'failed', keyFor: 'meshy', error: 'maiTerm could not set up meshy: it took longer than 180 s' });
    fail = false;
    await setup.retry();
    expect(calls).toEqual([`store meshy ${KEY.length}`, 'add meshy meshy', 'add meshy meshy', 'landed meshy', 'verify']);
    expect(setup.state.stage).toBe('added');
  });

  it('Remove the stored key forgets it and goes back to the paste', async () => {
    const { setup, calls } = rig({ serverAdd: vi.fn(async () => Promise.reject(new Error('npx not found'))) });
    await setup.submit(KEY, target());
    await setup.removeKey();
    expect(calls).toEqual([`store meshy ${KEY.length}`, 'delete meshy']);
    expect(setup.state).toMatchObject({ stage: 'key', keyFor: null, error: null, note: 'The stored key is removed.' });
    // Nothing left to retry or remove.
    await setup.retry();
    await setup.removeKey();
    expect(calls).toEqual([`store meshy ${KEY.length}`, 'delete meshy']);
  });

  it('a refused removal stays failed with its words', async () => {
    const { setup } = rig({ serverAdd: vi.fn(async () => Promise.reject(new Error('x'))), keyDelete: vi.fn(async () => Promise.reject(new Error('keychain: denied'))) });
    await setup.submit(KEY, target());
    await setup.removeKey();
    expect(setup.state).toMatchObject({ stage: 'failed', keyFor: 'meshy', error: 'keychain: denied' });
  });

  it('a server maiTerm already runs from the route gets the new key, this Hive, and Verify: nothing is added', async () => {
    const reuse = view('meshy', { workspaces: ['ws-other'] });
    const { setup, calls, deps } = rig();
    await setup.submit(KEY, target({ reuse, reuseScope: ['ws-other', 'ws-hive'] }));
    expect(calls).toEqual([`store meshy ${KEY.length}`, 'scope meshy [ws-other,ws-hive]', 'landed meshy', 'verify']);
    expect(deps.serverAdd).not.toHaveBeenCalled();
    // Its key is the server's: never removed from here.
    expect(setup.state.stage).toBe('added');
  });

  it('runs one step at a time: a second Save while one runs is ignored', async () => {
    let release!: () => void;
    const gate = new Promise<void>((r) => (release = r));
    const { setup, deps } = rig({ keyStore: vi.fn(async () => gate) });
    const first = setup.submit(KEY, target());
    expect(setup.busy).toBe(true);
    await setup.submit(KEY, target());
    expect(deps.keyStore).toHaveBeenCalledTimes(1);
    release();
    await first;
    expect(deps.serverAdd).toHaveBeenCalledTimes(1);
  });

  it('reports nothing once the sheet is gone, though the write still lands', async () => {
    const { setup, states, deps } = rig();
    setup.alive = false;
    await setup.submit(KEY, target());
    expect(states).toEqual([]);
    expect(deps.landed).toHaveBeenCalledTimes(1);
  });
});

describe('the key field and the plan', () => {
  it('hints at a key too short to be whole, and only then', () => {
    expect(MIN_KEY_LEN).toBe(16);
    expect(keyHint('')).toBeNull();
    expect(keyHint('   ')).toBeNull();
    expect(keyHint('msy_123')).toContain('too short');
    expect(keyHint('x'.repeat(16))).toBeNull();
    expect(keySendable('x'.repeat(15))).toBe(false);
    expect(keySendable(` ${'x'.repeat(16)} `)).toBe(true);
  });

  it('reuses the server set up from the same route, else names a new one after the provider', () => {
    const servers = [view('fal', { transport: 'http', route: null, url: 'https://mcp.fal.ai/mcp-relay' }), view('minimax', { route: 'minimax' })];
    expect(stdioPlan(servers, 'minimax', 'minimax', [], false)).toEqual({ reuse: servers[1] });
    // The mainland route is another server: the name is suffixed past the global one.
    expect(stdioPlan(servers, 'minimax-mainland', 'minimax', [], false)).toEqual({ add: 'minimax-2' });
    expect(stdioPlan(servers, 'meshy', 'meshy', [], false)).toEqual({ add: 'meshy' });
    // A native entry holding the name as agents would see it.
    expect(stdioPlan([], 'meshy', 'meshy', ['meshy-dev'], true)).toEqual({ add: 'meshy-2' });
    expect(defaultRoute([])).toBeNull();
  });
});
