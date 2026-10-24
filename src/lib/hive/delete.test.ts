import { describe, expect, it } from 'vitest';
import { deleteRefusal, deleteSummary, formatBytes } from './delete';
import type { HiveDeletePreview, HiveDeleteResult } from '$lib/tauri/types';

const preview = (over: Partial<HiveDeletePreview> = {}): HiveDeletePreview => ({
  unlanded: [], dirty: [], scratch: [], gallery_files: 0, gallery_bytes: 0, live_agents: [], lands_in_flight: [], branches: [], ...over,
});

const result = (over: Partial<HiveDeleteResult> = {}): HiveDeleteResult => ({
  worktrees_removed: 0, kept: [], branches: [], gallery_kept: false, trust_removed: 0, requests_failed: 0, folder_left: null, dropped_tabs: [], active_tabs: [], ...over,
});

describe('delete sheet', () => {
  it('sizes read as people write them', () => {
    expect(formatBytes(512)).toBe('512 B');
    expect(formatBytes(1536)).toBe('1.5 KB');
    expect(formatBytes(25 * 1024 * 1024)).toBe('25 MB');
  });

  it('is refused only while a worker lands', () => {
    expect(deleteRefusal(null)).toBeNull();
    expect(deleteRefusal(preview({ dirty: ['web: /x (2 uncommitted)'] }))).toBeNull();
    expect(deleteRefusal(preview({ lands_in_flight: ['#2 api'] }))).toBe('#2 api is landing: wait for it to finish or be held.');
  });

  it('says what went and what stayed', () => {
    expect(deleteSummary(result({ worktrees_removed: 3, kept: ['web: /wt (1 untracked file)'], branches: ['web: a', 'web: b'], gallery_kept: true, folder_left: '/h' })))
      .toBe('3 clean worktrees removed, 1 item kept on disk (work in it), 2 branches kept, the gallery kept. What stayed is in /h.');
    expect(deleteSummary(result({ worktrees_removed: 1 }))).toBe('1 clean worktree removed, no gallery kept.');
  });
});
