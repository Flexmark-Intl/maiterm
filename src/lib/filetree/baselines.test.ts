import { describe, expect, it } from 'vitest';
import { baselineDiffTabName, baselineStatWords, baselineUnavailableWords } from './baselines';

describe('turn baselines in words', () => {
  it('names the diff tab after the turn and the agent', () => {
    expect(baselineDiffTabName('foo.ts', 'turn', 14, 'claude', 'tab 3')).toBe('foo.ts (turn 14 · claude, tab 3)');
    expect(baselineDiffTabName('foo.ts', 'session', 2, 'codex', 'api')).toBe('foo.ts (session start · codex, api)');
  });

  it('does not say the runtime twice, or when it is unknown', () => {
    expect(baselineDiffTabName('a.rs', 'turn', 1, 'claude', 'Claude')).toBe('a.rs (turn 1 · Claude)');
    expect(baselineDiffTabName('a.rs', 'turn', 1, '', 'tab 3')).toBe('a.rs (turn 1 · tab 3)');
  });

  it('says why there is no baseline', () => {
    expect(baselineUnavailableWords('too_large', 'turn')).toMatch(/2 MB/);
    expect(baselineUnavailableWords('not_captured', 'session')).toMatch(/this session began before maiTerm started/);
    expect(baselineUnavailableWords('something_new', 'turn')).toMatch(/kept no start/);
  });

  it('counts lines only when there is a baseline', () => {
    expect(baselineStatWords({ added: 12, removed: 3, unavailable: null, turn: 14 })).toBe('+12 −3 since turn 14 began');
    expect(baselineStatWords({ added: 0, removed: 0, unavailable: 'binary', turn: 14 })).toBeNull();
  });
});
