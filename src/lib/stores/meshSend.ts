/**
 * Mesh send orchestration — the pure send path shared by the live store (agentMesh.svelte.ts)
 * and its tests. Composes the two tested cores (meshRouting + agentDelivery) with the
 * commit/edge rules that are easy to get subtly wrong:
 *
 *   • resolve recipient (stable handle) and topic (create-on-first-send) — delegated to the
 *     router; a failure here is a clean tool error, never a misroute.
 *   • build the envelope with the NEXT turn number, attempt delivery, THEN commit topic state
 *     — so a hard delivery failure leaves the turn counter untouched (no phantom turn).
 *   • a `delivered` send commits + emits a conversation edge; a `queued` send commits (it is
 *     held in the recipient's FIFO queue, not lost) but emits NO edge; a `failed` send commits
 *     nothing and emits nothing (phantom-edge guard, design §16.5 / 16.6).
 *
 * No Svelte/Tauri imports — the deliver fn, envelope builder, edge sink and liveness check
 * are injected, so the whole path is unit-testable (meshSend.test.ts).
 */

import type { MeshTopic } from '$lib/tauri/types';
import type { MeshRouter } from '$lib/stores/meshRouting';
import type { DeliverResult } from '$lib/stores/agentDelivery';

/** A confirmed conversation edge — emitted only on actual delivery. */
export interface MeshEdge {
  sender: string;
  recipient: string;
  topicId: string;
  topicLabel: string;
  turn: number;
  ts: number;
}

export type MeshSendResult =
  | { ok: true; delivered: boolean; queued: boolean; recipient: string; topic: { id: string; label: string }; note: string }
  | { ok: false; error: string };

export interface MeshSendDeps {
  router: MeshRouter;
  /** Deliver framed text to a recipient tab. The store's closure also lazily ensures the
   *  recipient has a delivery slot before delegating to the FIFO mailbox. */
  deliver: (recipientTabId: string, text: string) => Promise<DeliverResult>;
  buildEnvelope: (senderTabId: string, senderRole: string, topic: MeshTopic, turn: number, message: string) => string;
  emitEdge: (e: MeshEdge) => void;
  /** Persist the topic registry after a structural change (create / new participant). */
  persistTopics: () => void;
  /** Is the recipient a live session right now? (only used to phrase the queued note) */
  isLive: (tabId: string) => boolean;
  /** Epoch ms for the edge timestamp (injected for deterministic tests). */
  now: () => number;
}

export async function performMeshSend(
  deps: MeshSendDeps,
  args: { senderTabId: string; recipient?: string; topic?: string; message: string },
): Promise<MeshSendResult> {
  const { router } = deps;

  const message = (args.message ?? '').trim();
  if (!message) return { ok: false, error: 'Message is empty.' };

  const rr = router.resolveRecipient(args.senderTabId, args.recipient);
  if (!rr.ok) return { ok: false, error: rr.error };

  const tr = router.resolveTopicForSend(args.senderTabId, args.topic);
  if (!tr.ok) return { ok: false, error: tr.error };
  if (tr.created) deps.persistTopics();

  // Build with the next turn number, deliver, then commit on success (no phantom turn).
  const nextTurn = tr.topic.turn + 1;
  const text = deps.buildEnvelope(args.senderTabId, rr.role, tr.topic, nextTurn, message);
  const status = await deps.deliver(rr.tabId, text);
  if (status === 'failed') {
    return { ok: false, error: 'Delivery failed (could not write to the recipient terminal).' };
  }

  // The message is delivered or committed to the recipient's queue — commit topic state.
  router.bumpTurn(tr.topic.id);
  const np1 = router.recordParticipant(tr.topic.id, args.senderTabId);
  const np2 = router.recordParticipant(tr.topic.id, rr.tabId);
  if (tr.created || np1 || np2) deps.persistTopics();

  if (status === 'delivered') {
    deps.emitEdge({
      sender: args.senderTabId,
      recipient: rr.tabId,
      topicId: tr.topic.id,
      topicLabel: tr.topic.label,
      turn: nextTurn,
      ts: deps.now(),
    });
  }

  const summary = { id: tr.topic.id, label: tr.topic.label };
  if (status === 'delivered') {
    return {
      ok: true, delivered: true, queued: false, recipient: rr.role, topic: summary,
      note: `Delivered to ${rr.role} on topic "${tr.topic.label}". Their reply arrives as a new prompt — finish your turn now.`,
    };
  }
  const offline = !deps.isLive(rr.tabId);
  return {
    ok: true, delivered: false, queued: true, recipient: rr.role, topic: summary,
    note: offline
      ? `${rr.role} is offline; your message is queued on topic "${tr.topic.label}" and delivers when it resumes.`
      : `${rr.role} is busy; your message is queued on topic "${tr.topic.label}" and delivers when they're free.`,
  };
}
