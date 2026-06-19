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
