import { describe, expect, it } from 'vitest';
import { fixedRows, folderLines } from './settings';
import type { Hive } from '$lib/tauri/types';

const hive = (over: Partial<Hive> = {}): Hive => ({
  id: 'h1', goal: 'Ship it', instructions: '', cap: 4, landing_mode: 'base', tools: [], checkpoints: {}, budget_cents: 1000,
  push_after_land: false, settings: {}, criteria: [], status: 'active', slug: 'ship', created_at: '',
  models: { coordinator: { runtime: 'claude', model: 'opus', effort: 'medium' }, worker: { runtime: 'claude', model: 'sonnet', effort: 'high' } },
  folders: [
    { name: 'web', path: '/r/web', kind: 'git', base: 'main', setup: 'npm ci', land_check: null, primary: true, local_remotes: [] },
    { name: 'brand', path: '/r/brand', kind: 'plain', primary: false, local_remotes: [] },
  ],
  ...over,
});

describe('Hive settings, read-only half', () => {
  it('says what was fixed at creation, and why landing is', () => {
    const rows = fixedRows(hive(), 'Ship');
    const get = (l: string) => rows.find((r) => r.label === l)!;
    expect(get('Name').value).toBe('Ship');
    expect(get('Workers at once').value).toBe('4');
    expect(get('Landing').value).toBe('Land to Base');
    expect(get('Landing').note).toMatch(/new Hive/);
    expect(get('Coordinator').value).toBe('Claude opus, medium effort');
    expect(get('On the phone').value).toBe('Shown like any agent');
    const integ = fixedRows(hive({ landing_mode: 'integration', settings: { keep_off_phone: true } }), 'Ship');
    expect(integ.find((r) => r.label === 'Landing')!.value).toBe('Integration branch hive/ship, you approve each land');
    expect(integ.find((r) => r.label === 'On the phone')!.value).toMatch(/^Kept off/);
  });

  it('lists each folder with what it was set up with', () => {
    expect(folderLines(hive())).toEqual([
      { head: 'web (Primary) · git repo', path: '/r/web', detail: 'Base main · setup: npm ci' },
      { head: 'brand · plain, read-only', path: '/r/brand', detail: '' },
    ]);
  });
});
