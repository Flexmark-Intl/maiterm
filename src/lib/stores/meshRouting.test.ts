import { describe, it, expect, beforeEach } from 'vitest';
import { createMeshRouter, normalizeLabel, type MeshMember, type MeshRouter } from './meshRouting';

/** Deterministic deps: fixed clock, monotonic id minter, mutable roster. */
function makeHarness(members: MeshMember[]) {
  let seq = 0;
  const roster = [...members];
  const router = createMeshRouter({
    members: () => roster,
    now: () => '2026-01-01T00:00:00.000Z',
    mintId: () => `topic-${++seq}`,
  });
  return { router, roster };
}

const member = (tabId: string, role: string, live = true): MeshMember => ({ tabId, role, cwd: null, purpose: null, live });

describe('normalizeLabel', () => {
  it('collapses case and separators to a single dedup key', () => {
    expect(normalizeLabel('Auth Bug')).toBe('auth-bug');
    expect(normalizeLabel('auth_bug')).toBe('auth-bug');
    expect(normalizeLabel('  AUTH---bug  ')).toBe('auth-bug');
    expect(normalizeLabel('auth   bug')).toBe('auth-bug');
  });
  it('is empty for separator-only input', () => {
    expect(normalizeLabel('  __  ')).toBe('');
  });
});

describe('resolveRecipient', () => {
  let router: MeshRouter;
  beforeEach(() => {
    ({ router } = makeHarness([member('t-api', 'Backend API'), member('t-mob', 'Mobile App'), member('t-ops', 'DevOps')]));
  });

  it('resolves an exact tabId handle', () => {
    expect(router.resolveRecipient('t-api', 't-mob')).toEqual({ ok: true, tabId: 't-mob', role: 'Mobile App' });
  });

  it('resolves a unique role name case-insensitively', () => {
    expect(router.resolveRecipient('t-api', 'mobile app')).toEqual({ ok: true, tabId: 't-mob', role: 'Mobile App' });
  });

  it('errors with the roster on an unknown recipient (no silent drop)', () => {
    const r = router.resolveRecipient('t-api', 'Frontend');
    expect(r.ok).toBe(false);
    if (!r.ok) {
      expect(r.error).toContain('No peer named "Frontend"');
      expect(r.error).toContain('Backend API');
      expect(r.error).toContain('Mobile App');
    }
  });

  it('errors on an ambiguous role rather than guessing', () => {
    const { router: r2 } = makeHarness([member('t-1', 'Worker'), member('t-2', 'Worker'), member('t-3', 'Lead')]);
    const res = r2.resolveRecipient('t-3', 'Worker');
    expect(res.ok).toBe(false);
    if (!res.ok) expect(res.error).toContain('ambiguous');
  });

  it('rejects a self-send by handle and by role', () => {
    expect(router.resolveRecipient('t-api', 't-api').ok).toBe(false);
    expect(router.resolveRecipient('t-api', 'Backend API').ok).toBe(false);
  });

  it('defaults to the sole peer when recipient is omitted in a 2-agent mesh', () => {
    const { router: r2 } = makeHarness([member('a', 'Alice'), member('b', 'Bob')]);
    expect(r2.resolveRecipient('a', undefined)).toEqual({ ok: true, tabId: 'b', role: 'Bob' });
  });

  it('requires an explicit recipient when 2+ peers exist', () => {
    expect(router.resolveRecipient('t-api', undefined).ok).toBe(false);
  });
});

describe('topic registry', () => {
  let router: MeshRouter;
  beforeEach(() => {
    ({ router } = makeHarness([member('t-api', 'Backend API'), member('t-mob', 'Mobile App')]));
  });

  it('create-on-first-send mints a topic owned by the sender', () => {
    const r = router.resolveTopicForSend('t-api', 'Auth Refactor');
    expect(r.ok).toBe(true);
    if (r.ok) {
      expect(r.created).toBe(true);
      expect(r.topic.owner_tab_id).toBe('t-api');
      expect(r.topic.normalized_label).toBe('auth-refactor');
      expect(r.topic.participants).toEqual(['t-api']);
      expect(r.topic.state).toBe('open');
    }
  });

  it('dedups variant labels to one open topic (Codex #7)', () => {
    const a = router.resolveTopicForSend('t-api', 'Auth Bug');
    const b = router.resolveTopicForSend('t-mob', 'auth_bug');
    expect(a.ok && b.ok).toBe(true);
    if (a.ok && b.ok) {
      expect(b.created).toBe(false);
      expect(b.topic.id).toBe(a.topic.id);
    }
    expect(router.open()).toHaveLength(1);
  });

  it('reuses an existing topic referenced by id', () => {
    const a = router.resolveTopicForSend('t-api', 'Deploy');
    if (!a.ok) throw new Error('setup');
    const b = router.resolveTopicForSend('t-mob', a.topic.id);
    expect(b.ok).toBe(true);
    if (b.ok) { expect(b.created).toBe(false); expect(b.topic.id).toBe(a.topic.id); }
  });

  it('rejects a send on a completed topic by id (Codex #9)', () => {
    const a = router.resolveTopicForSend('t-api', 'Migration');
    if (!a.ok) throw new Error('setup');
    router.completeTopic('t-api', a.topic.id, false);
    const b = router.resolveTopicForSend('t-mob', a.topic.id);
    expect(b.ok).toBe(false);
    if (!b.ok) expect(b.error).toContain('complete');
  });

  it('requires a non-empty topic arg', () => {
    expect(router.resolveTopicForSend('t-api', '').ok).toBe(false);
    expect(router.resolveTopicForSend('t-api', undefined).ok).toBe(false);
  });

  it('completes owner-only, lets the human override, and is idempotent', () => {
    const a = router.startTopic('t-api', 'Schema');
    if (!a.ok) throw new Error('setup');
    // non-owner agent cannot complete
    expect(router.completeTopic('t-mob', a.topic.id, false).ok).toBe(false);
    // owner can
    const c1 = router.completeTopic('t-api', a.topic.id, false);
    expect(c1.ok).toBe(true);
    if (c1.ok) expect(c1.alreadyComplete).toBe(false);
    // idempotent
    const c2 = router.completeTopic('t-api', a.topic.id, false);
    expect(c2.ok).toBe(true);
    if (c2.ok) expect(c2.alreadyComplete).toBe(true);
  });

  it('lets the human complete a topic owned by any agent', () => {
    const a = router.startTopic('t-api', 'Orphan');
    if (!a.ok) throw new Error('setup');
    const c = router.completeTopic(null, a.topic.id, true);
    expect(c.ok).toBe(true);
  });

  it('tracks participants and turn counts', () => {
    const a = router.startTopic('t-api', 'Chat');
    if (!a.ok) throw new Error('setup');
    expect(router.recordParticipant(a.topic.id, 't-mob')).toBe(true);
    expect(router.recordParticipant(a.topic.id, 't-mob')).toBe(false); // already present
    expect(router.bumpTurn(a.topic.id)).toBe(1);
    expect(router.bumpTurn(a.topic.id)).toBe(2);
    const snap = router.snapshot();
    expect(snap[0].participants).toEqual(['t-api', 't-mob']);
    expect(snap[0].turn).toBe(2);
  });

  it('rejects a UUID-shaped topic arg that matches no topic (no junk-labeled thread)', () => {
    const r = router.resolveTopicForSend('t-api', '7a289d83-3036-4845-a7f2-429219d64d5b');
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain('not found');
    expect(router.all()).toHaveLength(0);
  });

  it('round-trips through load() (persisted seed)', () => {
    const a = router.startTopic('t-api', 'Persisted');
    if (!a.ok) throw new Error('setup');
    const saved = router.snapshot();
    const { router: r2 } = makeHarness([member('t-api', 'Backend API'), member('t-mob', 'Mobile App')]);
    r2.load(saved);
    expect(r2.open()).toHaveLength(1);
    // a new send with the same normalized label reuses the persisted topic
    const b = r2.resolveTopicForSend('t-mob', 'persisted');
    expect(b.ok).toBe(true);
    if (b.ok) { expect(b.created).toBe(false); expect(b.topic.id).toBe(a.topic.id); }
  });
});

describe('topic lifecycle (sweep / delete / clear)', () => {
  const DAY = 24 * 60 * 60 * 1000;
  const OPTS = { staleOpenMs: 7 * DAY, completedRetentionMs: 2 * DAY };
  // Harness clock is fixed at 2026-01-01; sweep "now" is passed explicitly.
  const T0 = Date.parse('2026-01-01T00:00:00.000Z');
  let router: MeshRouter;
  beforeEach(() => {
    ({ router } = makeHarness([member('t-api', 'Backend API'), member('t-mob', 'Mobile App')]));
  });

  it('sweep auto-completes open topics idle past the threshold', () => {
    const stale = router.startTopic('t-api', 'Stale');
    const other = router.startTopic('t-api', 'Also Stale');
    if (!stale.ok || !other.ok) throw new Error('setup');
    const { autoCompleted, expired } = router.sweep(T0 + 8 * DAY, OPTS);
    expect(autoCompleted.map((t) => t.label).sort()).toEqual(['Also Stale', 'Stale']);
    expect(expired).toEqual([]);
    // Auto-completed topics stay listed (dimmed) — updated_at was re-based to the fixed clock.
    expect(router.all()).toHaveLength(2);
    expect(router.open()).toHaveLength(0);
  });

  it('sweep keeps an open topic under the idle threshold open', () => {
    const a = router.startTopic('t-api', 'Active');
    if (!a.ok) throw new Error('setup');
    const { autoCompleted, expired } = router.sweep(T0 + 3 * DAY, OPTS);
    expect(autoCompleted).toEqual([]);
    expect(expired).toEqual([]);
    expect(router.open()).toHaveLength(1);
  });

  it('sweep expires completed topics past retention, keeps recent closures', () => {
    const old = router.startTopic('t-api', 'Old Done');
    if (!old.ok) throw new Error('setup');
    router.completeTopic('t-api', old.topic.id, false); // updated_at = T0 (fixed clock)
    const { autoCompleted, expired } = router.sweep(T0 + 3 * DAY, OPTS);
    expect(autoCompleted).toEqual([]);
    expect(expired).toEqual([old.topic.id]);
    expect(router.all()).toHaveLength(0);
    // A closure inside the retention window survives.
    const recent = router.startTopic('t-api', 'Recent Done');
    if (!recent.ok) throw new Error('setup');
    router.completeTopic('t-api', recent.topic.id, false);
    expect(router.sweep(T0 + 1 * DAY, OPTS).expired).toEqual([]);
    expect(router.all()).toHaveLength(1);
  });

  it('never auto-completes and expires a topic in the same pass', () => {
    const a = router.startTopic('t-api', 'Abandoned');
    if (!a.ok) throw new Error('setup');
    // Idle far past BOTH thresholds: it auto-completes but must survive this sweep.
    const { autoCompleted, expired } = router.sweep(T0 + 30 * DAY, OPTS);
    expect(autoCompleted).toHaveLength(1);
    expect(expired).toEqual([]);
    expect(router.all()).toHaveLength(1);
  });

  it('remove() deletes by id; unknown id returns null', () => {
    const a = router.startTopic('t-api', 'Doomed');
    if (!a.ok) throw new Error('setup');
    expect(router.remove(a.topic.id)?.label).toBe('Doomed');
    expect(router.all()).toHaveLength(0);
    expect(router.remove('nope')).toBeNull();
  });

  it('clearCompleted() removes all completed topics and returns their ids', () => {
    const open = router.startTopic('t-api', 'Still Going');
    const done1 = router.startTopic('t-api', 'Done A');
    const done2 = router.startTopic('t-mob', 'Done B');
    if (!open.ok || !done1.ok || !done2.ok) throw new Error('setup');
    router.completeTopic('t-api', done1.topic.id, false);
    router.completeTopic(null, done2.topic.id, true);
    const removed = router.clearCompleted();
    expect(removed.sort()).toEqual([done1.topic.id, done2.topic.id].sort());
    expect(router.all().map((t) => t.label)).toEqual(['Still Going']);
    expect(router.clearCompleted()).toEqual([]); // idempotent on an already-clean registry
  });
});
