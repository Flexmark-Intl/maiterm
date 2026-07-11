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
import type { LoopVerdict, LoopReason } from '$lib/stores/meshLoopControl';

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
  | { ok: true; delivered: boolean; queued: boolean; paused?: boolean; pauseReason?: LoopReason; recipient: string; topic: { id: string; label: string }; note: string }
  | { ok: false; error: string };

export interface MeshSendDeps {
  router: MeshRouter;
  /** Deliver framed text to a recipient tab. The store's closure also lazily ensures the
   *  recipient has a delivery slot before delegating to the FIFO mailbox. */
  deliver: (recipientTabId: string, text: string) => Promise<DeliverResult>;
  /** Frame a message for delivery. The sender's identity (role + cwd) is resolved from
   *  `senderTabId` inside the builder — never passed in — so the "from" line can't be
   *  mislabeled with the recipient's (or any other) name. */
  buildEnvelope: (senderTabId: string, topic: MeshTopic, turn: number, message: string) => string;
  emitEdge: (e: MeshEdge) => void;
  /** Persist the topic registry after a structural change (create / new participant). */
  persistTopics: () => void;
  /** Is the recipient a live session right now? (only used to phrase the queued note) */
  isLive: (tabId: string) => boolean;
  /** Epoch ms for the edge timestamp (injected for deterministic tests). */
  now: () => number;
  /** Loop-control gate (§10): evaluate the prospective next turn on this topic. When it
   *  blocks, the send neither delivers nor commits — the runaway is halted. Optional: a 1:1
   *  bridge / a test without loop control simply omits it. */
  gate?: (topic: MeshTopic, nextTurn: number) => LoopVerdict;
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

  // Loop control (§10): gate the prospective next turn. A blocked send is halted here —
  // nothing is injected and the turn counter does not advance — so a runaway truly pauses.
  const nextTurn = tr.topic.turn + 1;
  if (deps.gate) {
    const verdict = deps.gate(tr.topic, nextTurn);
    if (!verdict.ok) {
      const summary = { id: tr.topic.id, label: tr.topic.label };
      const note =
        verdict.reason === 'soft'
          ? `Topic "${tr.topic.label}" is paused at the soft turn cap (${verdict.cap}). It will not deliver until your human resumes or completes it from the mesh cockpit. Stop here and let them steer.`
          : verdict.reason === 'hard'
            ? `Topic "${tr.topic.label}" hit the hard turn ceiling (${verdict.cap}) — it is force-paused and can only be completed, not resumed. Tell your human this thread needs a decision or to close it.`
            : `Topic "${tr.topic.label}" has been open past its time limit and is force-paused. Your human must resume or complete it. Stop here.`;
      return { ok: true, delivered: false, queued: false, paused: true, pauseReason: verdict.reason, recipient: rr.role, topic: summary, note };
    }
  }

  // Build with the next turn number, deliver, then commit on success (no phantom turn).
  // The envelope's "from" identity comes from senderTabId (resolved inside buildEnvelope) —
  // NOT rr.role, which is the RECIPIENT and would mislabel the sender.
  const text = deps.buildEnvelope(args.senderTabId, tr.topic, nextTurn, message);
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
