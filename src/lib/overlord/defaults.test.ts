import { describe, expect, it } from 'vitest';
import type { OverlordRule } from '$lib/tauri/types';
import {
  DEFAULT_OVERLORD_RULES,
  pruneHiddenDefaultOverlordRules,
  seedDefaultOverlordRules,
} from './defaults';

/**
 * The retirement rule (2026-09-21): **a retired default may delete maiTerm's work, never the
 * human's.** The seeder used to filter every rule whose `default_id` had left the template map,
 * which deleted a rule the human had spent time editing — silently, with no ledger row, no
 * toast, and nothing in `hidden_default_overlord_rules` to restore from.
 *
 * These are regression tests for a data-loss path, so they assert on the human's fields
 * surviving, not just on the row count.
 */

const RETIRED = 'retired_default_for_test';

function rule(over: Partial<OverlordRule> = {}): OverlordRule {
  return {
    id: crypto.randomUUID(),
    name: 'A rule',
    description: null,
    enabled: true,
    workspaces: [],
    cooldown: 900,
    when: { event: 'turn_end' },
    guards: {},
    sequence: [{ kind: 'process', text: 'do the thing' }],
    ...over,
  } as OverlordRule;
}

describe('seedDefaultOverlordRules — retiring a default', () => {
  it('drops an untouched copy of a retired default', () => {
    const seeded = seedDefaultOverlordRules([rule({ default_id: RETIRED })], []);
    expect(seeded).not.toBeNull();
    expect(seeded!.some((r) => r.default_id === RETIRED)).toBe(false);
  });

  it('KEEPS an edited copy, as a plain user rule with its edits intact', () => {
    const edited = rule({
      default_id: RETIRED,
      user_modified: true,
      name: 'My renamed rule',
      cooldown: 60,
      sequence: [{ kind: 'process', text: 'my own wording' }],
    });

    const seeded = seedDefaultOverlordRules([edited], []);
    expect(seeded).not.toBeNull();

    const kept = seeded!.find((r) => r.id === edited.id);
    expect(kept, 'an edited rule must survive its template being retired').toBeDefined();
    // Their work, untouched.
    expect(kept!.name).toBe('My renamed rule');
    expect(kept!.cooldown).toBe(60);
    expect(kept!.sequence[0].text).toBe('my own wording');
    // No longer a default, and no longer claiming to be an edit of one — the editor enables
    // "Reset to default" on `user_modified` alone, so leaving it set offers a dead button.
    expect(kept!.default_id ?? null).toBeNull();
    expect(kept!.user_modified ?? false).toBe(false);
  });

  it('leaves a user-created rule alone either way', () => {
    const mine = rule({ name: 'Mine' });
    const seeded = seedDefaultOverlordRules([mine], []);
    // Either unchanged (null) or present — never dropped.
    if (seeded) expect(seeded.some((r) => r.id === mine.id)).toBe(true);
  });

  it('still auto-updates an un-modified live default, and still freezes an edited one', () => {
    const [defaultId, tmpl] = Object.entries(DEFAULT_OVERLORD_RULES)[0];

    const stale = seedDefaultOverlordRules([rule({ default_id: defaultId, name: 'stale name' })], []);
    expect(stale!.find((r) => r.default_id === defaultId)!.name).toBe(tmpl.name);

    const frozen = seedDefaultOverlordRules(
      [rule({ default_id: defaultId, name: 'my name', user_modified: true })],
      [],
    );
    // `frozen` may be null (nothing to change) — either way the name must not be rewritten.
    const row = (frozen ?? []).find((r) => r.default_id === defaultId);
    if (row) expect(row.name).toBe('my name');
  });
});

describe('pruneHiddenDefaultOverlordRules', () => {
  it('forgets a deletion of a template that has since been retired', () => {
    expect(pruneHiddenDefaultOverlordRules([RETIRED])).toEqual([]);
  });

  it('returns null when every hidden id still resolves, so nothing is written', () => {
    const live = Object.keys(DEFAULT_OVERLORD_RULES)[0];
    expect(pruneHiddenDefaultOverlordRules([live])).toBeNull();
    expect(pruneHiddenDefaultOverlordRules([])).toBeNull();
  });

  it('keeps live ids while dropping retired ones', () => {
    const live = Object.keys(DEFAULT_OVERLORD_RULES)[0];
    expect(pruneHiddenDefaultOverlordRules([live, RETIRED])).toEqual([live]);
  });

  it('a hidden default is not re-seeded, so agreement stays agreement', () => {
    const live = Object.keys(DEFAULT_OVERLORD_RULES)[0];
    const seeded = seedDefaultOverlordRules([], [live]);
    expect((seeded ?? []).some((r) => r.default_id === live)).toBe(false);
  });
});
