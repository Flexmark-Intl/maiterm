import { beforeEach, describe, expect, it, vi } from 'vitest';

const check = vi.hoisted(() => vi.fn());
const filed = vi.hoisted(() => vi.fn());

vi.mock('$lib/tauri/commands', () => ({
  followUpRemoteHome: vi.fn(async () => ({ host: null })),
  hiveCheckBriefFiles: vi.fn(async () => undefined),
  hiveMediaCheckRequested: check,
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
  filed.mockReset();
  filed.mockImplementation(async () => ({ requestId: 'r1' }));
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
