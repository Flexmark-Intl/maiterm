import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { createDeliveryController } from './agentDelivery';

/**
 * Regression contract for the recipient-keyed FIFO mailbox extracted from agentBridge.
 * These tests are the guarantee referenced by the eng review (T1): the 1:1 bridge — and the
 * mesh built on the same core — delivers strictly in order, and the e3d4eb8 fix (a newer
 * message must never jump a non-empty queue) holds.
 */

function makeHarness(opts: { cooldownMs?: number; drainTickMs?: number } = {}) {
  const attempts: { tabId: string; text: string }[] = [];
  const live = new Set<string>();
  const awaiting = new Set<string>();
  let injectResult = true;

  const ctl = createDeliveryController(
    {
      inject: async (tabId, text) => {
        attempts.push({ tabId, text }); // recorded on every attempt (success OR failure)
        return injectResult;
      },
      liveState: (tabId) => live.has(tabId),
      awaitingHuman: (tabId) => awaiting.has(tabId),
    },
    { cooldownMs: opts.cooldownMs ?? 1000, drainTickMs: opts.drainTickMs ?? 1500 },
  );

  return {
    ctl,
    attempts,
    live,
    awaiting,
    setInjectResult: (v: boolean) => { injectResult = v; },
    textsFor: (tabId: string) => attempts.filter((a) => a.tabId === tabId).map((a) => a.text),
  };
}

describe('agentDelivery — FIFO mailbox', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });

  it('delivers immediately when ready and the queue is empty', async () => {
    const h = makeHarness();
    h.live.add('A');
    h.ctl.claim('A', 'bridge', true);

    const r = await h.ctl.deliver('A', 'm1');

    expect(r).toBe('delivered');
    expect(h.textsFor('A')).toEqual(['m1']);
  });

  it('queues when the session is not live, then drains in order on markReady', async () => {
    const h = makeHarness();
    h.ctl.claim('A', 'bridge', true); // ready flag set, but liveState false → not deliverable

    expect(await h.ctl.deliver('A', 'm1')).toBe('queued');
    expect(await h.ctl.deliver('A', 'm2')).toBe('queued');
    expect(h.textsFor('A')).toEqual([]);

    h.live.add('A');
    h.ctl.markReady('A'); // flushes the head; cooldown drains the rest
    await vi.runAllTimersAsync();

    expect(h.textsFor('A')).toEqual(['m1', 'm2']);
  });

  it('REGRESSION e3d4eb8: a new message never jumps a non-empty queue', async () => {
    const h = makeHarness();
    h.live.add('A');
    h.ctl.claim('A', 'bridge', true);
    h.awaiting.add('A'); // at a human prompt → held → messages queue

    expect(await h.ctl.deliver('A', 'A1')).toBe('queued');
    expect(await h.ctl.deliver('A', 'A2')).toBe('queued');
    expect(h.textsFor('A')).toEqual([]);

    // Human answers; the tab is deliverable again, but the queue hasn't drained yet.
    h.awaiting.delete('A');
    // A fresh message arrives in that window. Pre-fix it would inject AHEAD of A1/A2.
    expect(await h.ctl.deliver('A', 'A3')).toBe('queued');

    await vi.runAllTimersAsync();

    // Strict FIFO: oldest-first, the newcomer last.
    expect(h.textsFor('A')).toEqual(['A1', 'A2', 'A3']);
  });

  it('serializes back-to-back sends through the cooldown, preserving order', async () => {
    const h = makeHarness({ cooldownMs: 1000 });
    h.live.add('A');
    h.ctl.claim('A', 'bridge', true);

    expect(await h.ctl.deliver('A', 'm1')).toBe('delivered'); // immediate, arms cooldown
    expect(await h.ctl.deliver('A', 'm2')).toBe('queued');    // held by cooldown
    expect(h.textsFor('A')).toEqual(['m1']);

    await vi.runAllTimersAsync();
    expect(h.textsFor('A')).toEqual(['m1', 'm2']);
  });

  it('re-queues a failed inject at the FRONT (order preserved on retry)', async () => {
    const h = makeHarness();
    h.live.add('A');
    h.ctl.claim('A', 'bridge', true);
    h.setInjectResult(false);

    // Deliverable, so it attempts the inject, fails, and re-queues at the front.
    expect(await h.ctl.deliver('A', 'A1')).toBe('queued');
    expect(h.textsFor('A')).toEqual(['A1']); // one failed attempt

    h.setInjectResult(true);
    await vi.runAllTimersAsync(); // drain poller retries the same head message

    expect(h.textsFor('A')).toEqual(['A1', 'A1']); // retried (front-of-queue), then delivered
  });

  it('does not deliver into a tab with no delivery entry', async () => {
    const h = makeHarness();
    h.live.add('ghost');
    // No claim() → no entry.
    expect(await h.ctl.deliver('ghost', 'x')).toBe('failed');
    expect(h.textsFor('ghost')).toEqual([]);
    h.ctl.markReady('ghost'); // no-op, must not create an entry
    expect(h.ctl.has('ghost')).toBe(false);
  });
});

describe('agentDelivery — one slot, two owners (bridge + mesh on the same tab)', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => { vi.clearAllTimers(); vi.useRealTimers(); });

  it('a second claim keeps the existing queue and readiness (never resets)', async () => {
    const h = makeHarness();
    h.ctl.claim('A', 'mesh', false); // not ready → mesh message queues
    expect(await h.ctl.deliver('A', 'mesh-1')).toBe('queued');

    h.ctl.claim('A', 'bridge', true); // bridge joins; must NOT wipe the mesh queue or flip ready
    expect(h.ctl.queueDepth('A')).toBe(1);
    expect(h.ctl.isReady('A')).toBe(false);
    expect(h.ctl.ownedBy('A', 'mesh')).toBe(true);
    expect(h.ctl.ownedBy('A', 'bridge')).toBe(true);
  });

  it('one owner releasing does not drop the other owner\'s queued messages', async () => {
    const h = makeHarness();
    h.ctl.claim('A', 'mesh', false);
    h.ctl.claim('A', 'bridge', false);
    expect(await h.ctl.deliver('A', 'mesh-1')).toBe('queued');

    h.ctl.release('A', 'bridge'); // bridge disconnects
    expect(h.ctl.has('A')).toBe(true);
    expect(h.ctl.queueDepth('A')).toBe(1);

    h.live.add('A');
    h.ctl.markReady('A');
    await vi.runAllTimersAsync();
    expect(h.textsFor('A')).toEqual(['mesh-1']);

    h.ctl.release('A', 'mesh'); // last owner out → slot gone
    expect(h.ctl.has('A')).toBe(false);
  });

  it('messages from both owners share ONE FIFO on the tab (no interleaving, arrival order)', async () => {
    const h = makeHarness({ cooldownMs: 100 });
    h.live.add('A');
    h.ctl.claim('A', 'bridge', true);
    h.ctl.claim('A', 'mesh', true);

    expect(await h.ctl.deliver('A', 'bridge-1')).toBe('delivered');
    expect(await h.ctl.deliver('A', 'mesh-1')).toBe('queued');   // cooldown from bridge-1
    expect(await h.ctl.deliver('A', 'bridge-2')).toBe('queued');
    await vi.runAllTimersAsync();

    expect(h.textsFor('A')).toEqual(['bridge-1', 'mesh-1', 'bridge-2']);
  });

  it('remap carries owners and is a no-op the second time (both stores call it)', () => {
    const h = makeHarness();
    h.ctl.claim('old', 'bridge', true);
    h.ctl.claim('old', 'mesh', true);
    h.ctl.remap('old', 'new');
    h.ctl.remap('old', 'new'); // the other store's call finds nothing under 'old'
    expect(h.ctl.has('old')).toBe(false);
    expect(h.ctl.ownedBy('new', 'bridge')).toBe(true);
    expect(h.ctl.ownedBy('new', 'mesh')).toBe(true);
    expect(h.ctl.isReady('new')).toBe(false); // forced not-ready until the new id re-inits
  });

  it('an ownerless entry (markReadyOrCreate) is removed by any release', () => {
    const h = makeHarness();
    h.ctl.markReadyOrCreate('A');
    h.ctl.release('A', 'bridge');
    expect(h.ctl.has('A')).toBe(false);

    h.ctl.markReadyOrCreate('B', 'mesh');
    h.ctl.release('B', 'bridge'); // not an owner → keeps it
    expect(h.ctl.has('B')).toBe(true);
    h.ctl.release('B', 'mesh');
    expect(h.ctl.has('B')).toBe(false);
  });
});
