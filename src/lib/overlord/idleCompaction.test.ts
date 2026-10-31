import { describe, expect, it } from 'vitest';
import type { OverlordRule } from '$lib/tauri/types';
import { idleCompactionRefusal, ruleCompacts, type IdleCompactionFacts } from './idleCompaction';

const NOW = 1_800_000_000_000;
const MIN = 60_000;

const rule = (sequence: unknown[]) => ({ sequence }) as unknown as OverlordRule;
const CHECKPOINT = rule([
  { kind: 'process', text: 'Prepare for compaction' },
  { kind: 'slash', text: '/compact', runtimes: ['claude', 'codex'] },
]);

const idle = (over: Partial<IdleCompactionFacts> = {}): IdleCompactionFacts => ({
  now: NOW,
  supervised: true,
  proposeMode: false,
  rule: CHECKPOINT,
  runtime: 'claude',
  state: 'idle',
  occupied: false,
  lastTurnTs: NOW - 54 * MIN,
  lastHumanInputAt: NOW - 54 * MIN,
  ...over,
});

describe('idleCompactionRefusal', () => {
  it('takes over a quiet session at Claude idle timing', () => {
    expect(idleCompactionRefusal(idle())).toBeNull();
    expect(idleCompactionRefusal(idle({ lastHumanInputAt: undefined }))).toBeNull();
  });

  it('never takes over a threshold compaction, which comes mid-turn or on a fresh prompt', () => {
    expect(idleCompactionRefusal(idle({ state: 'active' }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ state: undefined }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ lastTurnTs: NOW - 2 * MIN }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ lastHumanInputAt: NOW - 5_000 }))).not.toBeNull();
  });

  it('refuses when the age of the session can not be read', () => {
    expect(idleCompactionRefusal(idle({ lastTurnTs: undefined }))).not.toBeNull();
  });

  it('leaves compaction alone where the ritual could not run', () => {
    expect(idleCompactionRefusal(idle({ supervised: false }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ proposeMode: true }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ rule: null }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ occupied: true }))).not.toBeNull();
    expect(idleCompactionRefusal(idle({ rule: rule([{ kind: 'process', text: 'Save your work' }]) }))).not.toBeNull();
  });
});

describe('ruleCompacts', () => {
  it('needs a /compact step this runtime runs', () => {
    expect(ruleCompacts(CHECKPOINT, 'claude')).toBe(true);
    expect(ruleCompacts(rule([{ kind: 'slash', text: '/compact keep the plan' }]), 'claude')).toBe(true);
    expect(ruleCompacts(CHECKPOINT, 'gemini')).toBe(false);
    expect(ruleCompacts(rule([{ kind: 'process', text: '/compact' }]), 'claude')).toBe(false);
    expect(ruleCompacts(rule([{ kind: 'slash', text: '/clear' }]), 'claude')).toBe(false);
  });
});
