import { describe, it, expect } from 'vitest';
import { normalizeTabBatch, summarizeBatch, TAB_BATCH_MAX, type TabBatchRow } from './tabBatch';

/** Narrowing helper — every success case wants `ids`, every failure case wants `error`. */
function ok(r: ReturnType<typeof normalizeTabBatch>): { ids: string[]; batch: boolean } {
  if ('error' in r) throw new Error(`expected success, got: ${r.error}`);
  return r;
}

describe('normalizeTabBatch', () => {
  it('keeps a scalar call scalar', () => {
    const r = ok(normalizeTabBatch('a', undefined));
    expect(r).toEqual({ ids: ['a'], batch: false });
  });

  it('marks a list call as batch, including a single-element one', () => {
    expect(ok(normalizeTabBatch(undefined, ['a'])).batch).toBe(true);
    expect(ok(normalizeTabBatch(undefined, ['a', 'b'])).ids).toEqual(['a', 'b']);
  });

  // An empty array is still a list: the reply must carry `results`, not a scalar row the
  // agent would read as one tab having been dealt with.
  it('refuses an empty list rather than treating it as a no-op success', () => {
    expect(normalizeTabBatch(undefined, [])).toEqual({ error: 'tab_id or tab_ids is required.' });
  });

  it('refuses a call that names no tab at all', () => {
    expect(normalizeTabBatch(undefined, undefined)).toHaveProperty('error');
    expect(normalizeTabBatch('   ', undefined)).toHaveProperty('error');
  });

  // Mixing the two forms is the argument mistake an agent actually makes; dropping either
  // half would silently skip a tab it believes it named.
  it('concatenates both forms instead of letting one win', () => {
    expect(ok(normalizeTabBatch('a', ['b', 'c'])).ids).toEqual(['a', 'b', 'c']);
    expect(ok(normalizeTabBatch('a', ['b'])).batch).toBe(true);
  });

  // A second pass over an already-archived tab answers `not_boardable`, which would read as
  // a refusal of work that in fact succeeded.
  it('drops duplicates across and within both forms', () => {
    expect(ok(normalizeTabBatch('a', ['a', 'b', 'b', ' a '])).ids).toEqual(['a', 'b']);
  });

  it('trims whitespace and ignores blank entries', () => {
    expect(ok(normalizeTabBatch(undefined, [' a ', '', '  ', 'b'])).ids).toEqual(['a', 'b']);
  });

  it('caps the batch so it cannot outrun the 120s MCP response timeout', () => {
    const many = Array.from({ length: TAB_BATCH_MAX + 1 }, (_, i) => `t${i}`);
    const r = normalizeTabBatch(undefined, many);
    expect(r).toHaveProperty('error');
    expect((r as { error: string }).error).toContain(String(TAB_BATCH_MAX));
    expect(ok(normalizeTabBatch(undefined, many.slice(0, TAB_BATCH_MAX))).ids).toHaveLength(TAB_BATCH_MAX);
  });

  it('rejects wrong types rather than coercing them into ids', () => {
    expect(normalizeTabBatch(undefined, 'a')).toHaveProperty('error');
    expect(normalizeTabBatch(undefined, [1, 2])).toHaveProperty('error');
    expect(normalizeTabBatch(7, undefined)).toHaveProperty('error');
  });
});

describe('summarizeBatch', () => {
  const okRow = (id: string): TabBatchRow => ({ tab_id: id, ok: true });
  const badRow = (id: string): TabBatchRow => ({ tab_id: id, ok: false, reason: 'tab_in_use' });

  it('reports ok only when every row succeeded', () => {
    expect(summarizeBatch([okRow('a'), okRow('b')])).toMatchObject({ ok: true, succeeded: 2, refused: 0 });
  });

  // The point of the flag: an agent that checks `ok` alone must never be told a partial
  // sweep went fine.
  it('is not ok when a single row was refused', () => {
    expect(summarizeBatch([okRow('a'), badRow('b')])).toMatchObject({ ok: false, succeeded: 1, refused: 1 });
  });

  it('preserves row order and per-row detail', () => {
    const rows = [badRow('a'), okRow('b')];
    expect(summarizeBatch(rows).results).toEqual(rows);
  });
});
