import { describe, expect, it } from 'vitest';
import { conditionChip, contextConditionMet, contextFiresAtPct, describeCondition } from './format';

const FLOORED = { event: 'context_pct', at_or_above: 35, min_tokens: 130_000 } as const;
const PLAIN = { event: 'context_pct', at_or_above: 55 } as const;

describe('contextConditionMet', () => {
  it('needs the percentage and the floor together', () => {
    // 35% of a 200k window: past the percentage, under the floor.
    expect(contextConditionMet(FLOORED, { context_pct: 35, context_used: 70_000 })).toBe(false);
    expect(contextConditionMet(FLOORED, { context_pct: 65, context_used: 130_000 })).toBe(true);
    // 13% of a 1M window: past the floor, under the percentage.
    expect(contextConditionMet(FLOORED, { context_pct: 13, context_used: 130_000 })).toBe(false);
    expect(contextConditionMet(FLOORED, { context_pct: 35, context_used: 350_000 })).toBe(true);
  });

  it('never fires on a size it cannot read', () => {
    expect(contextConditionMet(FLOORED, undefined)).toBe(false);
    expect(contextConditionMet(FLOORED, { context_pct: 90 })).toBe(false);
  });

  it('leaves a rule with no floor exactly as it was', () => {
    expect(contextConditionMet(PLAIN, { context_pct: 55 })).toBe(true);
    expect(contextConditionMet(PLAIN, { context_pct: 54, context_used: 900_000 })).toBe(false);
  });
});

describe('contextFiresAtPct', () => {
  it('reports the higher of the percentage and the floor, for this window', () => {
    expect(contextFiresAtPct(FLOORED, 200_000)).toBe(65);
    expect(contextFiresAtPct(FLOORED, 250_000)).toBe(52);
    expect(contextFiresAtPct(FLOORED, 1_000_000)).toBe(35);
  });

  it('states only the percentage when the window is unknown or there is no floor', () => {
    expect(contextFiresAtPct(FLOORED, undefined)).toBe(35);
    expect(contextFiresAtPct(PLAIN, 200_000)).toBe(55);
  });
});

describe('condition wording', () => {
  it('names the floor when there is one', () => {
    expect(describeCondition(FLOORED)).toBe('context reaches 35% and 130k tokens');
    expect(conditionChip(FLOORED)).toBe('35% 130K CTX');
  });

  it('reads as before without one', () => {
    expect(describeCondition(PLAIN)).toBe('context reaches 55%');
    expect(conditionChip(PLAIN)).toBe('55% CTX');
  });
});
