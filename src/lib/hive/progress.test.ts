import { describe, expect, it } from 'vitest';
import { crewSummary, progressView, PROGRESS_STALE_MS } from './progress';
import type { CardStateName } from './cards';

const at = new Date(2026, 10, 24, 14, 32).getTime();

describe('progressView', () => {
  it('shows nothing until the queen sets an estimate', () => {
    expect(progressView(undefined, 'active', at)).toBeNull();
    expect(progressView(null, 'paused', at)).toBeNull();
  });

  it('shows her estimate with the time she set it', () => {
    const v = progressView({ percent: 40, note: 'Auth landed', at }, 'active', at + 60_000)!;
    expect(v).toEqual({ percent: 40, note: 'Auth landed', when: 'as of 14:32', stale: false, done: false });
    expect(progressView({ percent: 40, note: 'n', at }, 'active', at + 2 * 86_400_000)!.when).toBe('as of Nov 24, 14:32');
  });

  it('goes stale after an hour without an update', () => {
    expect(progressView({ percent: 40, note: 'n', at }, 'active', at + PROGRESS_STALE_MS - 1)!.stale).toBe(false);
    expect(progressView({ percent: 40, note: 'n', at }, 'active', at + PROGRESS_STALE_MS)!.stale).toBe(true);
  });

  it('only Done says complete, whatever her last estimate', () => {
    expect(progressView({ percent: 70, note: 'n', at }, 'done', at)).toMatchObject({ percent: 100, done: true, stale: false });
    expect(progressView(undefined, 'done', at)).toMatchObject({ percent: 100, done: true });
    expect(progressView({ percent: 100, note: 'n', at }, 'active', at)!.done).toBe(false);
  });

  it('clamps a value from a bad record', () => {
    expect(progressView({ percent: 140, note: 'n', at }, 'active', at)!.percent).toBe(100);
    expect(progressView({ percent: -3, note: 'n', at }, 'active', at)!.percent).toBe(0);
  });
});

describe('crewSummary', () => {
  const many = (state: CardStateName, n: number, quiet = false) => Array.from({ length: n }, () => ({ state, quiet }));

  it('reads at 180 workers as four groups and words', () => {
    const s = crewSummary([...many('landed', 4), ...many('working', 5), ...many('starting', 1), ...many('queued', 169), ...many('needs_you', 1)]);
    expect(s.total).toBe(180);
    expect(s.segments).toEqual([
      { group: 'landed', n: 4 },
      { group: 'active', n: 6 },
      { group: 'attention', n: 1 },
      { group: 'waiting', n: 169 },
    ]);
    expect(s.line).toBe('4 landed · 6 in flight · 169 waiting');
    expect(s.attention).toEqual([{ key: 'needs_you', text: '1 needs you' }]);
  });

  it('names each kind of trouble, a quiet worker included', () => {
    const s = crewSummary([...many('needs_you', 2), ...many('failed', 1), ...many('blocked', 3), ...many('idle', 2, true), ...many('working', 1, true)]);
    expect(s.attention.map((a) => a.text)).toEqual(['2 need you', '1 failed', '3 blocked', '3 quiet']);
    expect(s.line).toBe('');
  });

  it('never counts a landed or retired worker as quiet', () => {
    const s = crewSummary([...many('landed', 2, true), ...many('retired', 1, true)]);
    expect(s.attention).toEqual([]);
    expect(s.line).toBe('2 landed · 1 retired');
  });

  it('never counts a retired worker as landed: it was given up on', () => {
    const s = crewSummary([...many('landed', 2), ...many('retired', 10)]);
    expect(s.segments).toEqual([
      { group: 'landed', n: 2 },
      { group: 'retired', n: 10 },
    ]);
    expect(s.line).toBe('2 landed · 10 retired');
  });

  it('never counts an undone land as landed', () => {
    const s = crewSummary([{ state: 'landed', quiet: true, undone: true }, ...many('landed', 2)]);
    expect(s.segments).toEqual([
      { group: 'landed', n: 2 },
      { group: 'undone', n: 1 },
    ]);
    expect(s.line).toBe('2 landed · 1 undone');
    expect(s.attention).toEqual([]);
  });

  it('names a suspect land as needing a look', () => {
    const s = crewSummary([{ state: 'landed', quiet: false, suspect: true }, ...many('landed', 3)]);
    expect(s.attention).toEqual([{ key: 'suspect', text: '1 suspect land' }]);
    expect(s.line).toBe('3 landed');
  });

  it('is empty with no workers', () => {
    expect(crewSummary([])).toEqual({ total: 0, segments: [], line: '', attention: [] });
  });
});
