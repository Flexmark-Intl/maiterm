import { describe, expect, it } from 'vitest';
import { deleteRefusal, deleteSummary, formatBytes, keptLines, removedLines } from './delete';
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

  it('keeps the memory by default, names it, and moves it to Removed when ticked', () => {
    const p = preview({
      dirty: ['web: /wt (1 untracked file)'],
      memory: { path: '/h/memory', files: 2, bytes: 2048 },
      removed: ['The decision log: 3 entries (/h/logs/decisions.jsonl)'],
      gallery_files: 1,
      gallery_bytes: 512,
    });
    expect(keptLines(p, false)).toEqual(['web: /wt (1 untracked file)', 'Shared memory: /h/memory (2 files, 2.0 KB)']);
    expect(removedLines(p, false, false)).toEqual(['The decision log: 3 entries (/h/logs/decisions.jsonl)']);
    expect(keptLines(p, true)).toEqual(['web: /wt (1 untracked file)']);
    expect(removedLines(p, true, true)).toEqual([
      'The gallery: 1 asset, 512 B',
      'Shared memory: /h/memory (2 files)',
      'The decision log: 3 entries (/h/logs/decisions.jsonl)',
    ]);
    expect(keptLines(preview(), false)).toEqual([]);
    expect(deleteSummary(result({ worktrees_removed: 1, memory_kept: '/h/memory', folder_left: '/h' })))
      .toBe('1 clean worktree removed, no gallery kept, the memory kept. What stayed is in /h.');
  });

  it('says what went and what stayed', () => {
    expect(deleteSummary(result({ worktrees_removed: 3, kept: ['web: /wt (1 untracked file)'], branches: ['web: a', 'web: b'], gallery_kept: true, folder_left: '/h' })))
      .toBe('3 clean worktrees removed, 1 item kept on disk (work in it), 2 branches kept, the gallery kept. What stayed is in /h.');
    expect(deleteSummary(result({ worktrees_removed: 1 }))).toBe('1 clean worktree removed, no gallery kept.');
  });
});
