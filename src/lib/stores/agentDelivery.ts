/**
 * Agent message delivery core — the recipient-keyed FIFO mailbox shared by the 1:1
 * Agent Bridge (agentBridge.svelte.ts) and the N:M Mesh (Phase 1). Pure logic plus three
 * injected side effects (PTY write, liveness, human-prompt check), so it carries NO
 * Tauri/Svelte imports and is unit-testable in isolation (agentDelivery.test.ts).
 *
 * The invariant this module owns is STRICT FIFO per recipient:
 *
 *   sender A ─┐
 *   sender B ─┼─▶ delivery[recipient].queue  ──(drain, oldest-first)──▶ inject → PTY
 *   sender C ─┘            ▲                                                  │
 *                         └────── failed inject re-queued at FRONT ──────────┘
 *
 *   • push to TAIL, shift from HEAD (FIFO)
 *   • a failed inject is unshifted back to the FRONT (order preserved)
 *   • a freshly arriving message NEVER jumps a non-empty queue — it goes to the back
 *     (regression e3d4eb8: the "newest-first" reorder a recipient saw when a hold cleared
 *     between drain ticks)
 *
 * Gating (deliverable): inject only when the recipient has a live session, is NOT at a
 * human prompt (permission / elicitation), is not mid-cooldown, and has no inject in flight.
 * `busy` is a short post-inject cooldown that auto-clears, so the queue can never wedge.
 * A drain poller backstops anything queued while held; it self-stops when all queues empty.
 *
 * One instance serves BOTH the bridge and the mesh (agentDeliveryLive.ts): a tab can hold a
 * 1:1 bridge and sit on a mesh at the same time, and two controllers would mean two
 * `injecting` guards for one PTY — the interleaving this module exists to prevent. So a
 * slot is OWNED, not created: each consumer `claim`s it under its own tag and `release`s
 * its tag; the entry (and its queue) goes away only when the last owner lets go, so a
 * bridge disconnect can't drop messages the mesh has queued for the same tab.
 */

export interface DeliveryState {
  /** Live session that can accept a prompt (false while dormant/resuming). */
  ready: boolean;
  /** Short post-injection cooldown — serializes injections, auto-clears. NOT "awaiting a Stop". */
  busy: boolean;
  /** Framed envelopes waiting to be delivered to this tab. */
  queue: string[];
  /** Consumers holding this slot ('bridge', 'mesh'). Empty = created by markReadyOrCreate
   *  without an owner; a release by anyone then removes it. */
  owners: Set<string>;
  busyTimer?: ReturnType<typeof setTimeout>;
}

export interface DeliveryDeps {
  /** Write framed text to the tab's PTY as a submitted prompt. Resolves true on success. */
  inject(tabId: string, text: string): Promise<boolean>;
  /** Is there a live session that can receive a prompt? (false while dormant/resuming) */
  liveState(tabId: string): boolean;
  /** Is the recipient at a prompt awaiting the HUMAN (permission / interactive elicitation)? */
  awaitingHuman(tabId: string): boolean;
}

export interface DeliveryControllerOptions {
  /** Post-injection cooldown (serializes injects, lets the TUI register input). Default 1000ms. */
  cooldownMs?: number;
  /** Drain-poll backstop interval while anything is queued. Default 1500ms. */
  drainTickMs?: number;
}

export type DeliverResult = 'delivered' | 'queued' | 'failed';

export function createDeliveryController(deps: DeliveryDeps, opts: DeliveryControllerOptions = {}) {
  const COOLDOWN_MS = opts.cooldownMs ?? 1000;
  const DRAIN_TICK_MS = opts.drainTickMs ?? 1500;

  // Delivery state is keyed by the RECIPIENT tab.
  const delivery = new Map<string, DeliveryState>();
  // Tabs with an inject in flight — a hard serialization guard so two bracketed pastes can
  // never interleave at the PTY. Independent of the `busy` cooldown (which a Stop can clear),
  // so an event firing mid-injection can't race a second write in.
  const injecting = new Set<string>();
  // Backstop poller, live only while some tab has queued messages (see ensureDrainPump).
  let drainTimer: ReturnType<typeof setInterval> | undefined;

  function deliverable(tabId: string): boolean {
    const d = delivery.get(tabId);
    if (!d || !d.ready || d.busy || injecting.has(tabId)) return false;
    if (!deps.liveState(tabId)) return false; // dormant/resuming → queue
    // Hold while the recipient is at a prompt awaiting the HUMAN (a permission prompt, or a
    // runtime-specific interactive elicitation) — an injected paste+CR would hijack their pick.
    if (deps.awaitingHuman(tabId)) return false;
    return true;
  }

  function armCooldown(tabId: string) {
    const d = delivery.get(tabId);
    if (!d) return;
    d.busy = true;
    if (d.busyTimer) clearTimeout(d.busyTimer);
    d.busyTimer = setTimeout(() => {
      const cur = delivery.get(tabId);
      if (!cur) return;
      cur.busy = false;
      cur.busyTimer = undefined;
      void flush(tabId);
    }, COOLDOWN_MS);
  }

  function releaseCooldown(tabId: string) {
    const d = delivery.get(tabId);
    if (!d) return;
    d.busy = false;
    if (d.busyTimer) { clearTimeout(d.busyTimer); d.busyTimer = undefined; }
  }

  function pumpQueues() {
    let anyQueued = false;
    for (const [tabId, d] of delivery) {
      if (d.queue.length === 0) continue;
      anyQueued = true;
      void flush(tabId);
    }
    if (!anyQueued && drainTimer) { clearInterval(drainTimer); drainTimer = undefined; }
  }

  function ensureDrainPump() {
    if (!drainTimer) drainTimer = setInterval(pumpQueues, DRAIN_TICK_MS);
  }

  /** inject under the in-flight guard — `injecting.has(tabId)` is true for the whole write,
   *  and deliverable() rejects while it is, so no two injections to the same tab can overlap
   *  regardless of what events fire in between. */
  async function injectExclusive(tabId: string, text: string): Promise<boolean> {
    injecting.add(tabId);
    try { return await deps.inject(tabId, text); }
    finally { injecting.delete(tabId); }
  }

  /** Deliver framed text to a tab, or queue it if the tab isn't deliverable. Ordering rule:
   *  never jump the queue. If messages are already waiting (held while the recipient was
   *  dormant or at a human prompt), this newer message goes to the BACK and the in-order
   *  drain delivers it after them. Injecting directly here would land a newer message AHEAD
   *  of older queued ones (regression e3d4eb8). */
  async function deliver(tabId: string, text: string): Promise<DeliverResult> {
    const d = delivery.get(tabId);
    if (!d) return 'failed';
    if (d.queue.length > 0) {
      d.queue.push(text);
      ensureDrainPump();
      void flush(tabId); // nudge the drain now (it always delivers the OLDEST first)
      return 'queued';
    }
    if (!deliverable(tabId)) {
      d.queue.push(text);
      ensureDrainPump();
      return 'queued';
    }
    const ok = await injectExclusive(tabId, text);
    if (!ok) {
      d.queue.push(text);
      ensureDrainPump();
      return 'queued';
    }
    armCooldown(tabId);
    return 'delivered';
  }

  /** Try to deliver the next queued message to a tab (oldest first). */
  async function flush(tabId: string) {
    const d = delivery.get(tabId);
    if (!d || !deliverable(tabId)) return;
    const next = d.queue.shift();
    if (next === undefined) return;
    const ok = await injectExclusive(tabId, next);
    if (ok) armCooldown(tabId);
    else { d.queue.unshift(next); ensureDrainPump(); }
  }

  return {
    has(tabId: string) { return delivery.has(tabId); },
    size() { return delivery.size; },
    queueDepth(tabId: string) { return delivery.get(tabId)?.queue.length ?? 0; },
    isReady(tabId: string) { return delivery.get(tabId)?.ready ?? false; },

    /** Claim a tab's delivery slot for `owner`. Creates the entry (with `ready`) if absent;
     *  an existing one is kept as-is — its queue and readiness belong to every owner, so a
     *  second claimant never resets what the first has in flight. Idempotent per owner. */
    claim(tabId: string, owner: string, ready: boolean) {
      const d = delivery.get(tabId);
      if (d) { d.owners.add(owner); return; }
      delivery.set(tabId, { ready, busy: false, queue: [], owners: new Set([owner]) });
    },

    /** Drop `owner`'s claim. The entry — queue included — is removed only when no owner
     *  remains, so one consumer leaving can't discard another's queued messages. No-op if
     *  `owner` never claimed it, EXCEPT for an ownerless entry (markReadyOrCreate without an
     *  owner), which any release removes. */
    release(tabId: string, owner: string) {
      const d = delivery.get(tabId);
      if (!d) return;
      d.owners.delete(owner);
      if (d.owners.size > 0) return;
      if (d.busyTimer) clearTimeout(d.busyTimer);
      delivery.delete(tabId);
    },

    /** Does `owner` currently hold this tab's slot? */
    ownedBy(tabId: string, owner: string) {
      return delivery.get(tabId)?.owners.has(owner) ?? false;
    },

    /** Carry a tab's queue (and owners) to a new id (tab reload mints a new id). Forces
     *  not-ready so nothing injects into a booting shell until the new id re-inits.
     *  Idempotent: the bridge and the mesh both call it for one reload, and the second
     *  call finds nothing under the old id. */
    remap(oldId: string, newId: string) {
      const d = delivery.get(oldId);
      if (!d) return;
      delivery.delete(oldId);
      if (d.busyTimer) { clearTimeout(d.busyTimer); d.busyTimer = undefined; }
      d.ready = false;
      d.busy = false;
      delivery.set(newId, d);
    },

    /** ready=true, release any cooldown, flush. No-op if the tab has no delivery entry
     *  (e.g. a Stop on a non-bridged tab). */
    markReady(tabId: string) {
      const d = delivery.get(tabId);
      if (!d) return;
      d.ready = true;
      releaseCooldown(tabId);
      void flush(tabId);
    },

    /** Like markReady but creates the entry if missing (resume/rehydrate path), claimed for
     *  `owner` when one is given. */
    markReadyOrCreate(tabId: string, owner?: string) {
      if (!delivery.has(tabId)) delivery.set(tabId, { ready: true, busy: false, queue: [], owners: new Set() });
      const d = delivery.get(tabId)!;
      if (owner) d.owners.add(owner);
      d.ready = true;
      releaseCooldown(tabId);
      void flush(tabId);
    },

    /** ready=false + release cooldown (session ended; awaiting resume). No-op if no entry. */
    markDormant(tabId: string) {
      const d = delivery.get(tabId);
      if (!d) return;
      d.ready = false;
      releaseCooldown(tabId);
    },

    deliver,
    flush,

    destroy() {
      if (drainTimer) { clearInterval(drainTimer); drainTimer = undefined; }
      for (const d of delivery.values()) if (d.busyTimer) clearTimeout(d.busyTimer);
      delivery.clear();
      injecting.clear();
    },
  };
}

export type DeliveryController = ReturnType<typeof createDeliveryController>;
