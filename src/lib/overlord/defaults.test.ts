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

describe('seedDefaultOverlordRules — a flagged rule that is still ours', () => {
  const ID = 'checkpoint_at_context_pressure';
  const tmpl = DEFAULT_OVERLORD_RULES[ID];
  /** Exactly what shipped before 2026-10-05 — a separate prep and "Prepare for compaction." */
  const OLD_SEQUENCE = [
    { kind: 'process', text: 'Before we continue — make sure any relevant docs, memory, code comments and tasks are updated if needed.', await: { until: 'turn_end' }, timeout_seconds: 900, on_timeout: 'abort' },
    { kind: 'process', text: 'Prepare for compaction.', await: { until: 'turn_end' }, timeout_seconds: 600, on_timeout: 'abort' },
    { kind: 'slash', text: '/compact', runtimes: ['claude'], await: { until: 'context_below', pct: 30 }, timeout_seconds: 300, on_timeout: 'notify_human' },
  ] as OverlordRule['sequence'];

  function shipped(over: Partial<OverlordRule> = {}): OverlordRule {
    return rule({ ...structuredClone(tmpl), default_id: ID, ...over });
  }

  it('migrates a flagged copy of a previous version, keeping scope and enabled', () => {
    const r = shipped({ sequence: OLD_SEQUENCE, user_modified: true, enabled: false, workspaces: ['ws-1'] });
    const row = seedDefaultOverlordRules([r], [])!.find((x) => x.default_id === ID)!;
    expect(row.sequence).toEqual(tmpl.sequence);
    expect(row.user_modified).toBe(false);
    expect(row.enabled).toBe(false);
    expect(row.workspaces).toEqual(['ws-1']);
  });

  it('clears the flag on a copy identical to the current template', () => {
    const seeded = seedDefaultOverlordRules([shipped({ user_modified: true })], []);
    expect(seeded!.find((x) => x.default_id === ID)!.user_modified).toBe(false);
  });

  it('leaves a real edit frozen', () => {
    const mine = [...OLD_SEQUENCE.slice(0, 1), { ...OLD_SEQUENCE[1], text: 'My own wording.' }, OLD_SEQUENCE[2]];
    const r = shipped({ sequence: mine, user_modified: true });
    const seeded = seedDefaultOverlordRules([r], []);
    // null = nothing changed; otherwise the row must still be there, untouched.
    const row = seeded ? seeded.find((x) => x.id === r.id) : r;
    expect(row).toBeDefined();
    expect(row!.sequence[1].text).toBe('My own wording.');
    expect(row!.user_modified).toBe(true);
  });

  it('reports no change for an untouched, current rule', () => {
    const all = Object.entries(DEFAULT_OVERLORD_RULES).map(([id, t]) => rule({ ...structuredClone(t), default_id: id }));
    expect(seedDefaultOverlordRules(all, [])).toBeNull();
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
