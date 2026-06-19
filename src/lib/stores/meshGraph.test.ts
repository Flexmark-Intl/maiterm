import { describe, it, expect } from 'vitest';
import { computeGraph, topicHue } from './meshGraph';
import type { MeshMember } from './meshRouting';
import type { MeshTopic } from '$lib/tauri/types';
import type { MeshEdge } from './meshSend';

const member = (tabId: string, role: string, live = true): MeshMember => ({ tabId, role, cwd: null, purpose: null, live });
const topic = (id: string, owner: string, participants: string[], turn = 1, state: 'open' | 'complete' = 'open'): MeshTopic => ({
  id, label: id, normalized_label: id, owner_tab_id: owner, state, participants, turn,
  created_at: '2026-01-01T00:00:00.000Z', updated_at: '2026-01-01T00:00:00.000Z',
});
const opts = { cx: 100, cy: 100, radius: 50 };

describe('topicHue', () => {
  it('is deterministic and in range', () => {
    expect(topicHue('abc')).toBe(topicHue('abc'));
    expect(topicHue('abc')).toBeGreaterThanOrEqual(0);
    expect(topicHue('abc')).toBeLessThan(360);
  });
});

describe('computeGraph', () => {
  it('lays nodes on the circle with the first at the top', () => {
    const { nodes } = computeGraph([member('a', 'A'), member('b', 'B'), member('c', 'C')], [], [], new Set(), 0, opts);
    expect(nodes).toHaveLength(3);
    // first node at top: (cx, cy - radius)
    expect(nodes[0].x).toBeCloseTo(100);
    expect(nodes[0].y).toBeCloseTo(50);
    // all nodes on the circle
    for (const nd of nodes) {
      const d = Math.hypot(nd.x - opts.cx, nd.y - opts.cy);
      expect(d).toBeCloseTo(opts.radius);
    }
  });

  it('marks active nodes from the active set', () => {
    const { nodes } = computeGraph([member('a', 'A'), member('b', 'B')], [], [], new Set(['b']), 0, opts);
    expect(nodes.find((n) => n.tabId === 'a')!.active).toBe(false);
    expect(nodes.find((n) => n.tabId === 'b')!.active).toBe(true);
  });

  it('builds owner→participant star edges for open topics, wired to node positions', () => {
    const members = [member('a', 'A'), member('b', 'B'), member('c', 'C')];
    const { nodes, edges } = computeGraph(members, [topic('t1', 'a', ['a', 'b', 'c'], 3)], [], new Set(), 0, opts);
    expect(edges).toHaveLength(2); // a→b, a→c (owner excluded as its own target)
    const aNode = nodes.find((n) => n.tabId === 'a')!;
    for (const e of edges) {
      expect(e.from).toBe('a');
      expect(e.x1).toBeCloseTo(aNode.x);
      expect(e.y1).toBeCloseTo(aNode.y);
      expect(e.turns).toBe(3);
    }
  });

  it('omits completed topics and edges to absent participants', () => {
    const members = [member('a', 'A'), member('b', 'B')];
    const done = computeGraph(members, [topic('t1', 'a', ['a', 'b'], 2, 'complete')], [], new Set(), 0, opts);
    expect(done.edges).toHaveLength(0);
    // participant 'z' isn't on the roster → no edge to it
    const ghost = computeGraph(members, [topic('t2', 'a', ['a', 'z'], 1)], [], new Set(), 0, opts);
    expect(ghost.edges).toHaveLength(0);
  });

  it('flags an edge recent when its topic fired within the window', () => {
    const members = [member('a', 'A'), member('b', 'B')];
    const ring: MeshEdge[] = [{ sender: 'a', recipient: 'b', topicId: 't1', topicLabel: 't1', turn: 1, ts: 9000 }];
    const recent = computeGraph(members, [topic('t1', 'a', ['a', 'b'])], ring, new Set(), 10000, { ...opts, recentMs: 4000 });
    expect(recent.edges[0].recent).toBe(true);
    const stale = computeGraph(members, [topic('t1', 'a', ['a', 'b'])], ring, new Set(), 20000, { ...opts, recentMs: 4000 });
    expect(stale.edges[0].recent).toBe(false);
  });

  it('marks paused topics', () => {
    const members = [member('a', 'A'), member('b', 'B')];
    const g = computeGraph(members, [topic('t1', 'a', ['a', 'b'])], [], new Set(), 0, opts, new Set(['t1']));
    expect(g.edges[0].paused).toBe(true);
  });
});
