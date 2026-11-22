import { beforeEach, describe, expect, it, vi } from 'vitest';

const check = vi.hoisted(() => vi.fn());
const filed = vi.hoisted(() => vi.fn());
const detect = vi.hoisted(() => vi.fn());
const grants = vi.hoisted(() => vi.fn(async (_f: string[], h: string[], s: string[]) => [h.map((x) => x.toLowerCase()), s]));

vi.mock('$lib/tauri/commands', () => ({
  followUpRemoteHome: vi.fn(async () => ({ host: null })),
  hiveCheckBriefFiles: vi.fn(async () => undefined),
  hiveCheckWorkerGrants: grants,
  hiveMediaCheckRequested: check,
  hiveDetectTools: detect,
}));
vi.mock('$lib/stores/workspaces.svelte', () => ({ workspacesStore: { workspaces: [] } }));
vi.mock('$lib/stores/terminals.svelte', () => ({ terminalsStore: { getOsc: () => ({ cwd: '/Users/me/app' }) } }));
vi.mock('$lib/stores/agentRequests.svelte', () => ({ agentRequestsStore: { fileHiveProposal: filed } }));
vi.mock('$lib/utils/platform', () => ({ isWindows: () => false }));
vi.mock('@tauri-apps/plugin-log', () => ({ info: vi.fn(), warn: vi.fn() }));

import { proposeHive } from './proposeHive';
import type { Tab } from '$lib/tauri/types';

const tab = { id: 't1', name: 'planner' } as unknown as Tab;

beforeEach(() => {
  check.mockReset();
  detect.mockReset();
  filed.mockReset();
  filed.mockImplementation(async () => ({ requestId: 'r1' }));
});

describe('proposeHive worker grants', () => {
  it('refuses the agent a grant Create would refuse, so no card is filed that can never succeed', async () => {
    grants.mockRejectedValueOnce(new Error('api.github.com reaches github.com, where this Hive’s repos push'));
    const res = await proposeHive(tab, { goal: 'Ship it', worker_hosts: ['api.github.com'] });
    expect(String(res.error)).toContain('github.com');
    expect(filed).not.toHaveBeenCalled();
  });

  it('files the kept forms, which the card names', async () => {
    const res = await proposeHive(tab, { goal: 'Ship it', worker_hosts: ['API.Example.com'], worker_sockets: ['/tmp/.s.PGSQL.5432'] });
    expect(grants).toHaveBeenCalledWith(['/Users/me/app'], ['API.Example.com'], ['/tmp/.s.PGSQL.5432']);
    expect(filed.mock.calls[0][1].workerHosts).toEqual(['api.example.com']);
    expect(res.worker_hosts).toEqual(['api.example.com']);
  });

  it('asks nothing when the proposal grants nothing', async () => {
    grants.mockClear();
    await proposeHive(tab, { goal: 'Ship it' });
    expect(grants).not.toHaveBeenCalled();
  });
});

describe('proposeHive media', () => {
  it('files the checked ids with the proposal and answers with them', async () => {
    check.mockResolvedValue({ capabilities: ['video', 'music'], dropped: [] });
    const res = await proposeHive(tab, { goal: 'Make a promo', media: ['video', 'music'] });
    expect(check).toHaveBeenCalledWith(['video', 'music']);
    expect(filed.mock.calls[0][1].media).toEqual(['video', 'music']);
    expect(res.media).toEqual(['video', 'music']);
    expect(res).not.toHaveProperty('media_dropped');
    expect(res).not.toHaveProperty('media_note');
  });

  it('names the ids the catalogue dropped, and files only the known ones', async () => {
    check.mockResolvedValue({ capabilities: ['music'], dropped: ['hologram'] });
    const res = await proposeHive(tab, { goal: 'Make a promo', media: ['music', 'hologram'] });
    expect(filed.mock.calls[0][1].media).toEqual(['music']);
    expect(res.media_dropped).toEqual(['hologram']);
    expect(res.media_note).toBe('Not media capabilities, so left out: hologram.');
  });

  it('does not ask the catalogue, or add fields, when no media is passed', async () => {
    const res = await proposeHive(tab, { goal: 'Fix the parser' });
    expect(check).not.toHaveBeenCalled();
    expect(filed.mock.calls[0][1].media).toEqual([]);
    expect(res).not.toHaveProperty('media');
    expect(res).not.toHaveProperty('media_dropped');
    expect(res).not.toHaveProperty('media_note');
  });

  it('still files the proposal when the check fails, applies none, and echoes none of the ids', async () => {
    check.mockRejectedValue(new Error('catalogue gone'));
    const long = 'x'.repeat(500);
    const res = await proposeHive(tab, { goal: 'Make a promo', media: [long, 'video'] });
    expect(filed).toHaveBeenCalledTimes(1);
    expect(filed.mock.calls[0][1].media).toEqual([]);
    expect(res).not.toHaveProperty('media');
    expect(res).not.toHaveProperty('media_dropped');
    expect(String(res.media_note)).toMatch(/couldn't be checked.*not applied/);
    expect(JSON.stringify(res)).not.toContain(long);
  });
});

describe('proposeHive tools', () => {
  it('files the detected tool ids and names the dropped ones', async () => {
    detect.mockResolvedValue([{ id: 'mcp:figma' }, { id: 'cli:brew' }]);
    const res = await proposeHive(tab, { goal: 'Ship', tools: ['mcp:figma', 'mcp:gone', 'cli:brew'] });
    expect(filed.mock.calls[0][1].tools).toEqual(['mcp:figma']);
    expect(res.tools).toEqual(['mcp:figma']);
    expect(res.tools_dropped).toEqual(['mcp:gone', 'cli:brew']);
    expect(res.tools_note).toBe('Not detected on this Mac, so left out: mcp:gone, cli:brew.');
  });

  it('still files the proposal when detection fails, with no tools and a note', async () => {
    detect.mockRejectedValue(new Error('no'));
    const res = await proposeHive(tab, { goal: 'Ship', tools: ['mcp:figma'] });
    expect(filed).toHaveBeenCalledTimes(1);
    expect(filed.mock.calls[0][1].tools).toEqual([]);
    expect(res).not.toHaveProperty('tools');
    expect(String(res.tools_note)).toMatch(/couldn't be detected.*not applied/);
  });

  it('files the hosts and sockets as given and echoes them', async () => {
    const res = await proposeHive(tab, { goal: 'Ship', worker_hosts: ['api.example.com'], worker_sockets: ['/tmp/.s.PGSQL.5432'] });
    expect(filed.mock.calls[0][1]).toMatchObject({ workerHosts: ['api.example.com'], workerSockets: ['/tmp/.s.PGSQL.5432'] });
    expect(res).toMatchObject({ worker_hosts: ['api.example.com'], worker_sockets: ['/tmp/.s.PGSQL.5432'] });
  });
});
