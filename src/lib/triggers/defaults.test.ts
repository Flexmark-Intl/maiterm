import { describe, expect, it } from 'vitest';
import type { Trigger } from '$lib/tauri/types';
import { DEFAULT_TRIGGERS, pruneHiddenDefaultTriggers, seedDefaultTriggers } from './defaults';

/**
 * Same retirement rule as the Overlord seeder: a retired default may delete maiTerm's work,
 * never the human's.
 *
 * This side is not hypothetical. `DEFAULT_TRIGGERS` was emptied wholesale when hooks replaced
 * it, so EVERY default-linked trigger is stale — the old filter deleted the lot on the next
 * launch, edits included, and would do it again to anyone restoring an old state file.
 */

const RETIRED = 'retired_trigger_for_test';

function trigger(over: Partial<Trigger> = {}): Trigger {
  return {
    id: crypto.randomUUID(),
    name: 'A trigger',
    description: null,
    enabled: true,
    pattern: 'foo',
    workspaces: [],
    tabs: [],
    cooldown: 0,
    variables: [],
    plain_text: true,
    actions: [],
    ...over,
  } as Trigger;
}

describe('seedDefaultTriggers — retiring a default', () => {
  it('drops an untouched copy of a retired default', () => {
    const seeded = seedDefaultTriggers([trigger({ default_id: RETIRED })], []);
    expect(seeded).not.toBeNull();
    expect(seeded!.some((t) => t.default_id === RETIRED)).toBe(false);
  });

  it('KEEPS an edited copy, as a plain user trigger with its edits intact', () => {
    const edited = trigger({
      default_id: RETIRED,
      user_modified: true,
      name: 'My renamed trigger',
      pattern: 'my own pattern',
    });

    const seeded = seedDefaultTriggers([edited], []);
    expect(seeded).not.toBeNull();

    const kept = seeded!.find((t) => t.id === edited.id);
    expect(kept, 'an edited trigger must survive its template being retired').toBeDefined();
    expect(kept!.name).toBe('My renamed trigger');
    expect(kept!.pattern).toBe('my own pattern');
    expect(kept!.default_id ?? null).toBeNull();
    expect(kept!.user_modified ?? false).toBe(false);
  });

  it('never drops a user-created trigger', () => {
    const mine = trigger({ name: 'Mine' });
    const seeded = seedDefaultTriggers([mine], []);
    if (seeded) expect(seeded.some((t) => t.id === mine.id)).toBe(true);
  });
});

describe('pruneHiddenDefaultTriggers', () => {
  it('forgets a deletion of a template that has since been retired', () => {
    expect(pruneHiddenDefaultTriggers([RETIRED])).toEqual([]);
  });

  it('returns null when nothing is stale, so nothing is written', () => {
    expect(pruneHiddenDefaultTriggers([])).toBeNull();
    expect(pruneHiddenDefaultTriggers(Object.keys(DEFAULT_TRIGGERS))).toBeNull();
  });
});
