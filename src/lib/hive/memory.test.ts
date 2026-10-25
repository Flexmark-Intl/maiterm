import { describe, expect, it } from 'vitest';
import { budgetText, fileTags, keptError, keptLines, newFileName, nextView, provenance } from './memory';
import { parseView, serializeView, defaultView } from './columns';

const file = { indexed: true, folded: false, pinned: false, in_subfolder: false, archive: false };
const budget = { lines: 7, bytes: 512, kept_lines: 1, kept_bytes: 20, max_lines: 200, max_bytes: 25000, kept_max_lines: 60, kept_max_bytes: 8000 };

describe('memory view', () => {
  it('cycles Board, Gallery and Memory (Cmd+Shift+Y)', () => {
    expect(nextView('board')).toBe('gallery');
    expect(nextView('gallery')).toBe('memory');
    expect(nextView('memory')).toBe('board');
  });

  it('keeps the view per workspace across a reload, and reads a bad value as the board', () => {
    const v = { ...defaultView(), view: 'memory' as const };
    expect(parseView(serializeView(v)).view).toBe('memory');
    expect(parseView(serializeView({ ...defaultView(), view: 'gallery' })).view).toBe('gallery');
    expect(parseView(JSON.stringify({ view: 'timeline' })).view).toBe('board');
    expect(parseView(null).view).toBe('board');
  });

  it("labels provenance from Rust's record, never the file", () => {
    expect(provenance({ by: 'human', by_kind: 'human' }).text).toBe('You');
    expect(provenance({ by: 'coordinator', by_kind: 'coordinator' }).tone).toBe('coordinator');
    expect(provenance({ by: '#2 Session store', by_kind: 'worker' })).toMatchObject({ text: '#2 Session store', tone: 'worker' });
    expect(provenance({ by: 'maiTerm', by_kind: 'maiterm' }).tone).toBe('maiterm');
    expect(provenance({ by: 'human', by_kind: 'weird' })).toMatchObject({ text: 'Unknown', tone: 'unknown' });
  });

  it('tags folded, pinned, unindexed and archive files', () => {
    expect(fileTags(file)).toEqual([]);
    expect(fileTags({ ...file, indexed: false, folded: true })).toEqual(['folded']);
    expect(fileTags({ ...file, indexed: false, pinned: true })).toEqual(['pinned']);
    expect(fileTags({ ...file, indexed: false, in_subfolder: true })).toEqual(['not indexed']);
    expect(fileTags({ ...file, indexed: false, archive: true })).toEqual(['archive']);
  });

  it('reads the kept editor as lines and holds it to the cap', () => {
    expect(keptLines('- a\r\n- b\n\n\n')).toEqual(['- a', '- b']);
    expect(keptLines('- a\n\n- b')).toEqual(['- a', '', '- b']);
    expect(keptError(['- a'], budget)).toBeNull();
    expect(keptError(Array.from({ length: 61 }, (_, i) => `${i}`), budget)).toMatch(/at most 60 lines/);
    expect(keptError(['é'.repeat(4000)], budget)).toMatch(/8001 bytes/);
  });

  it('describes the budget', () => {
    expect(budgetText(budget)).toBe('MEMORY.md: 7 of 200 lines, 0.5 of 25 KB · kept block 1 of 60 lines');
  });

  it('names a new file', () => {
    expect(newFileName(' style ')).toBe('style.md');
    expect(newFileName('notes.md')).toBe('notes.md');
    for (const bad of ['', '.hidden', 'a/b', 'MEMORY', 'MEMORY.generated.md', 'archive-1', 'c:x']) expect(newFileName(bad)).toBeNull();
  });
});
