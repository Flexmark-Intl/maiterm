import { describe, expect, it } from 'vitest';
import { askCount, autonomyLabel, CHECKPOINT_ROWS, formFor, lockedReason, toSave, validateForm } from './autonomy';
import type { Hive } from '$lib/tauri/types';

const hive = (over: Partial<Hive> = {}) => ({ landing_mode: 'base', checkpoints: {}, settings: {}, ...over }) as Pick<Hive, 'landing_mode' | 'checkpoints' | 'settings'>;

describe('the Autonomy form', () => {
  it('starts from the record, a missing key at its default', () => {
    const f = formFor(hive({ checkpoints: { permissions: 'ask' }, settings: { autonomy: { prompt_timeout_secs: 600, daily_digest: false } } }));
    expect(f.values).toEqual({ plan: 'coordinator', questions: 'coordinator', permissions: 'ask', landing: 'coordinator', finals: 'coordinator', interfaces: 'coordinator', settings: 'ask' });
    expect(f.digest).toBe(false);
    expect(f.minutes).toBe(10);
    expect(formFor(hive()).minutes).toBe(5);
    expect(formFor(hive({ settings: { autonomy: { prompt_timeout_secs: 20, daily_digest: true } } })).minutes).toBe(1);
    expect(formFor(hive({ landing_mode: 'integration' })).values.landing).toBe('ask');
    expect(CHECKPOINT_ROWS.map((r) => r.key)).toEqual(['plan', 'questions', 'permissions', 'landing', 'finals', 'interfaces', 'settings']);
    expect(formFor(hive({ checkpoints: { settings: 'coordinator' } })).values.settings, 'handed over').toBe('coordinator');
  });

  it('keeps Landing at Ask me in the integration mode', () => {
    const integ = hive({ landing_mode: 'integration' });
    expect(lockedReason(integ, 'landing')).toContain('Landing stays Ask me');
    expect(lockedReason(integ, 'plan')).toBeNull();
    expect(lockedReason(hive(), 'landing')).toBeNull();
    const f = formFor(integ);
    expect(validateForm(integ, f)).toBeNull();
    expect(validateForm(integ, { ...f, values: { ...f.values, landing: 'coordinator' } })).toContain('Landing stays Ask me');
    expect(validateForm(hive(), { ...formFor(hive()), values: { ...formFor(hive()).values, landing: 'coordinator' } })).toBeNull();
  });

  it('checks the timeout and the values', () => {
    const f = formFor(hive());
    for (const minutes of [0, 61, 1.5, Number.NaN]) expect(validateForm(hive(), { ...f, minutes }), String(minutes)).toContain('1 to 60');
    for (const minutes of [1, 60]) expect(validateForm(hive(), { ...f, minutes })).toBeNull();
    expect(validateForm(hive(), { ...f, values: { ...f.values, plan: 'maybe' as 'ask' } })).toContain('Plan: pick');
  });

  it('saves seconds and keeps a later build’s autonomy fields', () => {
    const h = hive({ settings: { autonomy: { prompt_timeout_secs: 300, daily_digest: true, later: 7 } as never } });
    const f = { ...formFor(h), minutes: 15, digest: false };
    f.values.questions = 'ask';
    const out = toSave(h, f);
    expect(out.checkpoints.questions).toBe('ask');
    expect(Object.keys(out.checkpoints)).toHaveLength(7);
    expect(out.autonomy).toEqual({ prompt_timeout_secs: 900, daily_digest: false, later: 7 });
  });

  it('labels the header button by how many ask the human', () => {
    // Settings asks by default (docs/hive.md §3.4).
    expect(askCount(hive())).toBe(1);
    const of = CHECKPOINT_ROWS.length;
    const handed = { settings: 'coordinator' };
    expect(autonomyLabel(hive({ checkpoints: handed }))).toBe('Queen decides');
    expect(autonomyLabel(hive())).toBe(`You decide 1 of ${of}`);
    expect(autonomyLabel(hive({ landing_mode: 'integration', checkpoints: handed }))).toBe(`You decide 1 of ${of}`);
    expect(autonomyLabel(hive({ checkpoints: { plan: 'ask', questions: 'ask' } }))).toBe(`You decide 3 of ${of}`);
  });
});
