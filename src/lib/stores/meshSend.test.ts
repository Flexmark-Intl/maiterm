import { describe, it, expect, beforeEach, vi } from 'vitest';
import { performMeshSend, type MeshEdge, type MeshSendDeps } from './meshSend';
import { createMeshRouter, type MeshMember, type MeshRouter } from './meshRouting';
import { createDeliveryController, type DeliverResult } from './agentDelivery';

const member = (tabId: string, role: string, live = true): MeshMember => ({ tabId, role, cwd: null, purpose: null, live });

/** Router over a fixed roster + a configurable fake deliver, capturing edges + persists. */
function harness(roster: MeshMember[], deliverImpl: (tabId: string, text: string) => Promise<DeliverResult>) {
  let seq = 0;
  const router = createMeshRouter({
    members: () => roster,
    now: () => '2026-01-01T00:00:00.000Z',
    mintId: () => `topic-${++seq}`,
  });
  const edges: MeshEdge[] = [];
  const delivered: { tabId: string; text: string }[] = [];
  let persists = 0;
  const deps: MeshSendDeps = {
    router,
    deliver: async (tabId, text) => { delivered.push({ tabId, text }); return deliverImpl(tabId, text); },
    // Mirror the real builder: stamp the SENDER's role (resolved from senderTabId), never the recipient's.
    buildEnvelope: (senderTabId, topic, turn, msg) =>
      `[${roster.find((m) => m.tabId === senderTabId)?.role ?? senderTabId}|${topic.label}|${turn}] ${msg}`,
    emitEdge: (e) => edges.push(e),
    persistTopics: () => { persists++; },
    isLive: (tabId) => roster.find((m) => m.tabId === tabId)?.live ?? false,
    now: () => 1234,
  };
  return { router, deps, edges, delivered, persists: () => persists };
}

const ROSTER = [member('t-api', 'Backend API'), member('t-mob', 'Mobile App')];

describe('performMeshSend', () => {
  it('delivers, routes to the recipient, emits one edge, and commits the turn', async () => {
    const h = harness(ROSTER, async () => 'delivered');
    const r = await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 'Mobile App', topic: 'Auth', message: 'hello' });
    expect(r.ok).toBe(true);
    if (r.ok) { expect(r.delivered).toBe(true); expect(r.recipient).toBe('Mobile App'); }
    // routed to the recipient handle (t-mob), but the envelope's "from" is the SENDER's role
    // (Backend API) — NOT the recipient's (Mobile App). This is the misroute-label regression guard.
    expect(h.delivered).toEqual([{ tabId: 't-mob', text: '[Backend API|Auth|1] hello' }]);
    // exactly one confirmed edge, sender→recipient
    expect(h.edges).toHaveLength(1);
    expect(h.edges[0]).toMatchObject({ sender: 't-api', recipient: 't-mob', turn: 1, ts: 1234 });
    // topic committed: turn 1, both participants
    const t = h.router.all()[0];
    expect(t.turn).toBe(1);
    expect(t.participants.sort()).toEqual(['t-api', 't-mob']);
  });

  it('queued send commits the turn but emits NO edge (phantom-edge guard)', async () => {
    const h = harness(ROSTER, async () => 'queued');
    const r = await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 't-mob', topic: 'Auth', message: 'hi' });
    expect(r.ok).toBe(true);
    if (r.ok) { expect(r.queued).toBe(true); expect(r.delivered).toBe(false); }
    expect(h.edges).toHaveLength(0);
    expect(h.router.all()[0].turn).toBe(1); // committed — it is in the recipient's queue, not lost
  });

  it('failed send commits NOTHING and emits no edge', async () => {
    const h = harness(ROSTER, async () => 'failed');
    const r = await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 't-mob', topic: 'Auth', message: 'hi' });
    expect(r.ok).toBe(false);
    expect(h.edges).toHaveLength(0);
    // topic was created by resolveTopicForSend, but no turn was committed (no phantom turn)
    expect(h.router.all()[0].turn).toBe(0);
  });

  it('rejects an unknown recipient with a clear error and never delivers', async () => {
    const h = harness(ROSTER, async () => 'delivered');
    const r = await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 'Nobody', topic: 'Auth', message: 'hi' });
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain('No peer named "Nobody"');
    expect(h.delivered).toHaveLength(0);
    expect(h.edges).toHaveLength(0);
  });

  it('rejects a self-send and an empty message', async () => {
    const h = harness(ROSTER, async () => 'delivered');
    expect((await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 't-api', topic: 'X', message: 'hi' })).ok).toBe(false);
    expect((await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 't-mob', topic: 'X', message: '   ' })).ok).toBe(false);
    expect(h.delivered).toHaveLength(0);
  });

  it('rejects a send on a completed topic (Codex #9)', async () => {
    const h = harness(ROSTER, async () => 'delivered');
    const start = h.router.startTopic('t-api', 'Done');
    if (!start.ok) throw new Error('setup');
    h.router.completeTopic('t-api', start.topic.id, false);
    const r = await performMeshSend(h.deps, { senderTabId: 't-mob', recipient: 't-api', topic: start.topic.id, message: 'still there?' });
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.error).toContain('complete');
    expect(h.delivered).toHaveLength(0);
  });

  it('a loop-control gate pauses the send: no delivery, no turn commit, no edge', async () => {
    const h = harness(ROSTER, async () => 'delivered');
    // Seed a topic at turn 2 so the prospective next turn is 3.
    const start = h.router.startTopic('t-api', 'Loop');
    if (!start.ok) throw new Error('setup');
    h.router.bumpTurn(start.topic.id);
    h.router.bumpTurn(start.topic.id);
    const r = await performMeshSend(
      { ...h.deps, gate: (_t, nextTurn) => (nextTurn > 2 ? { ok: false, reason: 'soft', turn: nextTurn, cap: 2 } : { ok: true }) },
      { senderTabId: 't-mob', recipient: 't-api', topic: start.topic.id, message: 'one more?' },
    );
    expect(r.ok).toBe(true);
    if (r.ok) { expect(r.paused).toBe(true); expect(r.pauseReason).toBe('soft'); expect(r.delivered).toBe(false); }
    expect(h.delivered).toHaveLength(0);            // nothing injected
    expect(h.edges).toHaveLength(0);                // no edge
    expect(h.router.get(start.topic.id)?.turn).toBe(2); // turn not advanced
  });

  it('persists once on topic creation and once when the recipient first joins', async () => {
    const h = harness(ROSTER, async () => 'delivered');
    await performMeshSend(h.deps, { senderTabId: 't-api', recipient: 't-mob', topic: 'New', message: 'hi' });
    // create (1) + new participant t-mob (1) = 2; sender was already the owner-participant
    expect(h.persists()).toBe(2);
  });
});

describe('performMeshSend × real delivery controller (routed to recipient queue)', () => {
  let router: MeshRouter;
  let ctl: ReturnType<typeof createDeliveryController>;
  let injected: string[];
  let live: Set<string>;
  let deps: MeshSendDeps;
  let edges: MeshEdge[];

  beforeEach(() => {
    vi.useFakeTimers();
    injected = [];
    live = new Set<string>(); // recipient NOT live yet → sends queue
    edges = [];
    router = createMeshRouter({
      members: () => ROSTER,
      now: () => '2026-01-01T00:00:00.000Z',
      mintId: () => 'topic-1',
    });
    ctl = createDeliveryController({
      inject: async (_tabId, text) => { injected.push(text); return true; },
      liveState: (tabId) => live.has(tabId),
      awaitingHuman: () => false,
    });
    ctl.ensure('t-mob', false);
    deps = {
      router,
      deliver: (tabId, text) => ctl.deliver(tabId, text),
      buildEnvelope: (_s, topic, turn, msg) => `[${turn}] ${msg}`,
      emitEdge: (e) => edges.push(e),
      persistTopics: () => {},
      isLive: (tabId) => live.has(tabId),
      now: () => 1,
    };
  });

  it('queues while the recipient is offline, then flushes in FIFO order on markReady', async () => {
    const r1 = await performMeshSend(deps, { senderTabId: 't-api', recipient: 't-mob', topic: 'topic-1', message: 'first' });
    const r2 = await performMeshSend(deps, { senderTabId: 't-api', recipient: 't-mob', topic: 'topic-1', message: 'second' });
    expect(r1.ok && !r1.delivered).toBe(true);
    expect(r2.ok && !r2.delivered).toBe(true);
    expect(injected).toHaveLength(0);   // nothing delivered while offline
    expect(edges).toHaveLength(0);       // no edges for queued sends

    // Recipient comes online → queue drains oldest-first.
    live.add('t-mob');
    ctl.markReady('t-mob');
    await vi.runAllTimersAsync();
    expect(injected).toEqual(['[1] first', '[2] second']);
  });
});
