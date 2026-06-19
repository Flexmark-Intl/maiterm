/**
 * Mesh conversation-graph layout (docs/mesh-workspace.md §9) — pure geometry, so the cockpit
 * SVG is a thin render over tested data (meshGraph.test.ts). Nodes are the agents on a circle;
 * edges are topic "stars" (owner → each participant) weighted by turn count, colored per topic,
 * and flagged `recent` when that topic saw a delivery in the last few seconds (the live pulse).
 */

import type { MeshTopic } from '$lib/tauri/types';
import type { MeshMember } from '$lib/stores/meshRouting';
import type { MeshEdge } from '$lib/stores/meshSend';

export interface GraphNode {
  tabId: string;
  role: string;
  x: number;
  y: number;
  live: boolean;
  active: boolean; // claudeState === 'active'
}

export interface GraphEdge {
  from: string;       // owner tabId
  to: string;         // participant tabId
  topicId: string;
  topicLabel: string;
  turns: number;
  hue: number;        // 0..359, stable per topic
  recent: boolean;    // a delivery on this topic within recentMs
  paused: boolean;
  x1: number; y1: number;
  x2: number; y2: number;
}

export interface GraphLayoutOpts {
  cx: number;
  cy: number;
  radius: number;
  /** A topic counts as "recently active" if an edge fired within this window. Default 4000ms. */
  recentMs?: number;
}

/** Deterministic hue from a topic id (so each thread keeps a stable color). */
export function topicHue(topicId: string): number {
  let h = 0;
  for (let i = 0; i < topicId.length; i++) h = (h * 31 + topicId.charCodeAt(i)) >>> 0;
  return h % 360;
}

export function computeGraph(
  members: MeshMember[],
  topics: MeshTopic[],
  edges: MeshEdge[],
  activeTabIds: Set<string>,
  nowMs: number,
  opts: GraphLayoutOpts,
  pausedTopicIds: Set<string> = new Set(),
): { nodes: GraphNode[]; edges: GraphEdge[] } {
  const recentMs = opts.recentMs ?? 4000;
  const n = members.length;

  // Nodes on a circle, first node at the top (-90°), clockwise.
  const nodes: GraphNode[] = members.map((m, i) => {
    const angle = n === 0 ? 0 : (-Math.PI / 2) + (2 * Math.PI * i) / n;
    return {
      tabId: m.tabId,
      role: m.role,
      x: opts.cx + opts.radius * Math.cos(angle),
      y: opts.cy + opts.radius * Math.sin(angle),
      live: m.live,
      active: activeTabIds.has(m.tabId),
    };
  });
  const pos = new Map(nodes.map((nd) => [nd.tabId, nd]));

  // Which topics fired recently (for the pulse)?
  const recentTopics = new Set<string>();
  for (const e of edges) {
    if (nowMs - e.ts <= recentMs) recentTopics.add(e.topicId);
  }

  // Edges: per open topic, a star from the owner to each other participant present on-screen.
  const out: GraphEdge[] = [];
  for (const t of topics) {
    if (t.state !== 'open') continue;
    const owner = pos.get(t.owner_tab_id);
    if (!owner) continue;
    const hue = topicHue(t.id);
    const recent = recentTopics.has(t.id);
    const paused = pausedTopicIds.has(t.id);
    for (const p of t.participants) {
      if (p === t.owner_tab_id) continue;
      const node = pos.get(p);
      if (!node) continue;
      out.push({
        from: owner.tabId, to: node.tabId,
        topicId: t.id, topicLabel: t.label, turns: t.turn, hue, recent, paused,
        x1: owner.x, y1: owner.y, x2: node.x, y2: node.y,
      });
    }
  }
  return { nodes, edges: out };
}
