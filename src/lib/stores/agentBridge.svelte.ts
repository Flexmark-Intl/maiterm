import { countedListen as listen } from '$lib/utils/listenCounter';
import * as commands from '$lib/tauri/commands';
import type { AgentBridge } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore } from '$lib/stores/claudeState.svelte';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

/**
 * Agent Bridge — a bridge between two running Claude agents in different maiTerm panes.
 *
 * The human bridges the current tab to another running Claude session (picked from
 * any workspace). maiTerm FORKS that session (`claude --resume <id> --fork-session`)
 * into a fresh split pane beside the caller — an isolated peer with the target's
 * full context. The two agents then converse asynchronously via the
 * `sendToBridgedAgent` MCP tool; every message is injected as a real terminal turn
 * in the recipient's pane, so the human watches (and can interrupt with Esc).
 *
 * Identity is stamped by maiTerm (not self-asserted), so the recipient always knows
 * a message is from a peer agent — never confused for the human operator.
 *
 * Handshake (tight + routing-proof): a forked session resumes the target's
 * transcript, which already contains the target's `initSession` — so the fork
 * believes it is already initialized (as the wrong tab) and never re-binds its new
 * MCP connection, leaving its maiTerm tools unusable. So after the fork's Claude
 * comes up we inject a directive forcing it to re-`initSession` as ITS OWN tab. The
 * opener (caller → "introduce yourself") then fires off the fork's real
 * `claude-init-session` event, which proves the fork is up, on THIS instance, and
 * tool-capable — not a flaky `SessionStart` hook.
 *
 * Delivery readiness model: `ready` (accepts prompts — caller immediately; fork once
 * it initializes), `busy` (a message was injected, awaiting its Stop — prevents
 * mid-turn double-injection), `hasCompletedTurn` (after a Stop, claudeState's
 * active/idle is trustworthy and we defer to it).
 *
 * Bridges are self-healing: at send time the recipient must still have a live Claude
 * session; if its session id drifted (it resumed) the bridge re-binds rather than
 * breaking, and a closed tab tears the bridge down cleanly.
 */

const INJECT_GAP_MS = 120;           // gap between bracketed-paste and the submitting CR
const BUSY_TIMEOUT_MS = 300_000;     // safety: auto-clear busy if no Stop ever arrives
const FORK_BOOT_POLL_MS = 500;       // poll interval while waiting for the fork's Claude to register
const FORK_BOOT_TIMEOUT_MS = 15_000; // cap on waiting for the fork to boot before priming anyway
const FORK_SETTLE_MS = 1500;         // extra settle after the fork registers, so its TUI accepts input
const FORK_INIT_TIMEOUT_MS = 25_000; // if the fork never re-inits on this instance, tell the caller

type BridgeRole = 'caller' | 'fork' | 'peer';

interface BridgeEntry {
  /** The tab this agent is bridged to. */
  partnerTabId: string;
  /** Human-readable label of the partner (for the agent's own awareness). */
  partnerLabel: string;
  /** Conversation turn counter (incremented on each message this tab sends). */
  turn: number;
  /** Partner's last-known Claude session id. Refreshed when the partner re-inits
   *  after a resume; used to detect a drifted session and re-bind (NOT to break —
   *  the persisted bridge is authoritative, so a new id means "it resumed"). */
  partnerSessionId?: string;
  /** Whether this tab initiated the bridge or is the forked peer. */
  role: BridgeRole;
  /** Human-written description of the PARTNER (what it's expert on / how to use it),
   *  fed into this tab's opener. In-memory only (one-time, not persisted). */
  purpose?: string;
}

interface DeliveryState {
  ready: boolean;
  busy: boolean;
  hasCompletedTurn: boolean;
  /** Framed envelopes waiting to be delivered to this tab. */
  queue: string[];
  busyTimer?: ReturnType<typeof setTimeout>;
}

const enc = (s: string) => Array.from(new TextEncoder().encode(s));
const sleep = (ms: number) => new Promise<void>((r) => setTimeout(r, ms));

function createAgentBridgeStore() {
  // Both tabs of a bridge get an entry pointing at each other (symmetric).
  const bridges = new Map<string, BridgeEntry>();
  // Delivery state is keyed by the RECIPIENT tab.
  const delivery = new Map<string, DeliveryState>();
  // Forked partners awaiting init → opener-into-caller (keyed by fork tab id).
  const pendingOpeners = new Map<string, { callerTabId: string }>();
  // Best-effort cwd label when live OSC cwd isn't available yet.
  const cwdHint = new Map<string, string>();
  // Reactive version bump so UI ($derived) can react to bridge changes.
  let version = $state(0);
  const unlisteners: (() => void)[] = [];

  function bump() { version++; }

  function resolveTab(tabId: string) {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find((t) => t.id === tabId);
        if (tab) return { ws, pane, tab };
      }
    }
    return null;
  }

  function tabExists(tabId: string): boolean {
    return resolveTab(tabId) !== null;
  }

  /** Clean display name for identity envelopes (strips bridge glyphs). */
  function label(tabId: string): string {
    const loc = resolveTab(tabId);
    if (!loc) return 'unknown agent';
    return loc.tab.name.replace(/^[⇄↔→]\s*/u, '').trim() || 'agent';
  }

  function getCwd(tabId: string): string | null {
    const osc = terminalsStore.getOsc(tabId);
    return osc?.cwd ?? osc?.promptCwd ?? cwdHint.get(tabId) ?? null;
  }

  /** Persist this tab's bridge to the backend (or clear it if the in-memory entry is
   *  gone), so the pairing survives an app restart and can be rehydrated. The live
   *  routing stays in memory; only the durable pairing is written here. */
  async function persistBridge(tabId: string) {
    const loc = resolveTab(tabId);
    if (!loc) return; // tab gone — nothing to persist to
    const bridge = bridges.get(tabId);
    const payload = bridge
      ? {
          partner_tab_id: bridge.partnerTabId,
          partner_label: bridge.partnerLabel,
          partner_session_id: bridge.partnerSessionId ?? null,
          role: bridge.role,
          turn: bridge.turn,
        }
      : null;
    try {
      await commands.setTabAgentBridge(loc.ws.id, loc.pane.id, tabId, payload);
    } catch (e) {
      logError(`agentBridge: failed to persist bridge for tab ${tabId.slice(0, 8)}: ${e}`);
    }
  }

  // ─── Injection ──────────────────────────────────────────────────────────────

  /** Write a prompt into a tab's PTY as a bracketed paste, then submit with CR.
   *  Bracketed paste keeps multi-line content as one prompt (newlines don't submit
   *  early); the deferred CR submits it. */
  async function injectPrompt(tabId: string, text: string): Promise<boolean> {
    const inst = terminalsStore.get(tabId);
    if (!inst) {
      logError(`agentBridge: cannot inject — no terminal instance for tab ${tabId.slice(0, 8)}`);
      return false;
    }
    try {
      await commands.writeTerminal(inst.ptyId, enc(`\x1b[200~${text}\x1b[201~`));
      await sleep(INJECT_GAP_MS);
      await commands.writeTerminal(inst.ptyId, enc('\r'));
      return true;
    } catch (e) {
      logError(`agentBridge: inject failed for tab ${tabId.slice(0, 8)}: ${e}`);
      return false;
    }
  }

  // ─── Delivery gating ──────────────────────────────────────────────────────────

  function deliverable(tabId: string): boolean {
    const d = delivery.get(tabId);
    if (!d || !d.ready || d.busy) return false;
    // Post-boot: claudeState is trustworthy — require a LIVE, idle session. No live
    // state means the partner is dormant/resuming, so queue (don't inject into a
    // shell). Boot window (pre first Stop): trust `ready`.
    if (d.hasCompletedTurn) {
      const st = claudeStateStore.getState(tabId);
      return !!st && st.state !== 'active';
    }
    return true;
  }

  function setBusy(tabId: string) {
    const d = delivery.get(tabId);
    if (!d) return;
    d.busy = true;
    if (d.busyTimer) clearTimeout(d.busyTimer);
    d.busyTimer = setTimeout(() => {
      const cur = delivery.get(tabId);
      if (!cur) return;
      cur.busy = false;
      void flush(tabId);
    }, BUSY_TIMEOUT_MS);
  }

  /** Deliver framed text to a tab, or queue it if the tab isn't deliverable. */
  async function deliver(tabId: string, text: string): Promise<'delivered' | 'queued' | 'failed'> {
    const d = delivery.get(tabId);
    if (!d) return 'failed';
    if (!deliverable(tabId)) {
      d.queue.push(text);
      return 'queued';
    }
    const ok = await injectPrompt(tabId, text);
    if (!ok) {
      d.queue.push(text);
      return 'queued';
    }
    setBusy(tabId);
    return 'delivered';
  }

  /** Try to deliver the next queued message to a tab (called when it goes idle). */
  async function flush(tabId: string) {
    const d = delivery.get(tabId);
    if (!d || !deliverable(tabId)) return;
    const next = d.queue.shift();
    if (next === undefined) return;
    const ok = await injectPrompt(tabId, next);
    if (ok) setBusy(tabId);
    else d.queue.unshift(next);
  }

  // ─── Envelopes (identity stamped by maiTerm) ──────────────────────────────────

  function buildEnvelope(senderTabId: string, message: string, turn: number): string {
    const name = label(senderTabId);
    const cwd = getCwd(senderTabId);
    const where = cwd ? `, working in ${cwd}` : '';
    return (
      `⟦AGENT-BRIDGE⟧ Message from "${name}"${where} — a peer AI agent, NOT your human operator. [turn ${turn}]\n` +
      `Reply with the sendToBridgedAgent tool. If this fully answers the request, you can stop — don't reply just to acknowledge.\n\n` +
      message
    );
  }

  function buildOpener(callerTabId: string, partnerTabId: string, forked = true): string {
    const bridge = bridges.get(callerTabId);
    const partnerName = bridge?.partnerLabel ?? label(partnerTabId);
    const cwd = getCwd(partnerTabId);
    const where = cwd ? ` (working in ${cwd})` : '';
    const what = forked
      ? `a peer AI agent forked with the FULL context of that session`
      : `a peer AI agent running in another tab`;
    const purpose = bridge?.purpose?.trim();
    const ctx = purpose ? ` Your human operator describes it as: "${purpose}".` : '';
    return (
      `⟦AGENT-BRIDGE⟧ You are now bridged to "${partnerName}"${where} — ${what}.${ctx}\n\n` +
      `Don't message it yet. First check in with your human operator: tell them the bridge is ready, summarize in a sentence what this peer can help with, and propose 2-3 specific things you could ask it that are relevant to your current work. Then wait for the human to say what to consult it about.\n\n` +
      `When the human gives the go-ahead, use the sendToBridgedAgent tool — open by identifying yourself (who you are, what you're working on) and why you're reaching out, then ask. The peer's replies arrive here as new prompts; when you have what you need, just stop.`
    );
  }

  /** Heads-up delivered to an EXISTING tab that the human just bridged into (it didn't
   *  initiate and isn't a fork, so prime it like primeFork primes a fork). */
  function buildExistingBridgeNotice(peerLabel: string): string {
    return (
      `⟦AGENT-BRIDGE⟧ You have been bridged to a peer AI agent ("${peerLabel}") via maiTerm Agent Bridge — a peer agent in another tab, NOT your human operator. ` +
      `It may reach out to consult you; its messages arrive here as new prompts. Reply with the sendToBridgedAgent tool. ` +
      `There's nothing to do until its message arrives — carry on with your work.`
    );
  }

  /** Directive injected into the fork to force it to re-initialize as its OWN tab
   *  (a resumed/forked session otherwise inherits the target's initSession and never
   *  re-binds its new MCP connection, so its maiTerm tools stay unusable). */
  function buildForkInitDirective(forkTabId: string, peerLabel: string): string {
    return (
      `⟦AGENT-BRIDGE⟧ You are now a FORKED peer agent in a NEW maiTerm tab (id ${forkTabId}). ` +
      `This is a fresh tab with a fresh MCP connection, so you must re-initialize: call your aiterm initSession tool with tabId "${forkTabId}" right now. ` +
      `Disregard any tab id mentioned earlier in this conversation — you are "${forkTabId}" now.\n\n` +
      `You have been bridged to a peer AI agent ("${peerLabel}") via maiTerm Agent Bridge. ` +
      `After initializing, reply with a one-line readiness note, then wait — the peer's message will arrive as a new prompt.`
    );
  }

  /** Sent to the caller if the fork never re-initializes on this instance. */
  function buildBridgeFailedNote(forkTabId: string): string {
    return (
      `⟦AGENT-BRIDGE⟧ The bridge to "${label(forkTabId)}" could not be completed — the forked agent did not initialize on this maiTerm instance ` +
      `(it may have connected to a different one). You can run /maiterm init in the new pane and retry, or disconnect and bridge again.`
    );
  }

  /** After spawning a fork, wait for its Claude to register on THIS instance, then
   *  inject the re-init directive. The handshake (opener → caller) fires separately,
   *  when the fork's initSession actually lands (see the claude-init-session handler
   *  in init()). If the fork never inits here, the caller is told rather than left
   *  hanging. */
  async function primeFork(forkTabId: string) {
    for (let waited = 0; waited < FORK_BOOT_TIMEOUT_MS; waited += FORK_BOOT_POLL_MS) {
      if (!pendingOpeners.has(forkTabId)) return;            // already handshaked / disconnected
      if (claudeStateStore.getState(forkTabId)) break;        // fork's Claude is up on this instance
      await sleep(FORK_BOOT_POLL_MS);
    }
    await sleep(FORK_SETTLE_MS);
    if (!pendingOpeners.has(forkTabId)) return;

    const peerLabel = bridges.get(forkTabId)?.partnerLabel ?? 'your bridged peer';
    const ok = await injectPrompt(forkTabId, buildForkInitDirective(forkTabId, peerLabel));
    if (!ok) {
      logError(`agentBridge: failed to prime fork ${forkTabId.slice(0, 8)}`);
      return;
    }
    // Backstop: if the fork doesn't re-init on this instance, don't leave the caller waiting.
    setTimeout(() => {
      const po = pendingOpeners.get(forkTabId);
      if (!po) return;                                        // handshake completed
      pendingOpeners.delete(forkTabId);
      if (tabExists(po.callerTabId)) void deliver(po.callerTabId, buildBridgeFailedNote(forkTabId));
    }, FORK_INIT_TIMEOUT_MS);
  }

  // ─── Lifecycle: bridge / disconnect ────────────────────────────────────────────────

  function cleanup(tabId: string) {
    const d = delivery.get(tabId);
    if (d?.busyTimer) clearTimeout(d.busyTimer);
    delivery.delete(tabId);
    bridges.delete(tabId);
    pendingOpeners.delete(tabId);
    cwdHint.delete(tabId);
  }

  return {
    get version() { return version; },

    getInternalSizes() {
      return { bridges: bridges.size, delivery: delivery.size, pending_openers: pendingOpeners.size };
    },

    isBridged(tabId: string): boolean {
      void version;
      return bridges.has(tabId);
    },

    getPartnerTabId(tabId: string): string | null {
      return bridges.get(tabId)?.partnerTabId ?? null;
    },

    getPartnerLabel(tabId: string): string | null {
      void version;
      return bridges.get(tabId)?.partnerLabel ?? null;
    },

    /** For the getBridgedAgent MCP tool. */
    getBridgeInfo(tabId: string) {
      const bridge = bridges.get(tabId);
      if (!bridge) return { bridged: false };
      return {
        bridged: true,
        partner: {
          tabId: bridge.partnerTabId,
          label: bridge.partnerLabel,
          cwd: getCwd(bridge.partnerTabId),
          available: tabExists(bridge.partnerTabId),
        },
      };
    },

    /**
     * Fork `target`'s session into a split beside `callerTabId` and bridge the two.
     * `target` comes from the picker (getClaudeSessions / claudeState).
     */
    async establishBridge(
      callerTabId: string,
      target: { sessionId: string; tabName: string; workspaceName: string; cwd: string | null; sshCommand?: string | null; remoteCwd?: string | null },
      purpose?: string,
    ): Promise<{ ok: true; partnerTabId: string; partnerLabel: string } | { ok: false; error: string }> {
      const loc = resolveTab(callerTabId);
      if (!loc) return { ok: false, error: 'Caller tab not found.' };
      if (bridges.has(callerTabId)) return { ok: false, error: 'This tab is already bridged. Disconnect it first.' };

      const partnerLabel = `${target.tabName} · ${target.workspaceName}`;
      const res = await workspacesStore.forkSessionIntoSplit(
        loc.ws.id,
        loc.pane.id,
        {
          sessionId: target.sessionId,
          cwd: target.cwd,
          sshCommand: target.sshCommand ?? null,
          remoteCwd: target.remoteCwd ?? null,
        },
        target.tabName,
      );
      if (!res) return { ok: false, error: 'Failed to spawn the forked partner pane.' };

      const partnerTabId = res.newTabId;
      const callerLabel = `${label(callerTabId)}`;
      const callerSessionId = claudeStateStore.getState(callerTabId)?.sessionId;

      bridges.set(callerTabId, { partnerTabId, partnerLabel, turn: 0, role: 'caller', purpose: purpose?.trim() || undefined });
      // The fork's entry knows the caller's session id up front; the caller learns
      // the fork's session id when the fork initializes (see init() handler).
      bridges.set(partnerTabId, { partnerTabId: callerTabId, partnerLabel: callerLabel, turn: 0, partnerSessionId: callerSessionId, role: 'fork' });
      // Caller is an established agent (past its boot window) → trust claudeState
      // immediately (hasCompletedTurn) so the opener can't inject mid-turn. The
      // forked partner becomes ready when its initSession lands.
      delivery.set(callerTabId, { ready: true, busy: false, hasCompletedTurn: true, queue: [] });
      delivery.set(partnerTabId, { ready: false, busy: false, hasCompletedTurn: false, queue: [] });
      if (target.cwd) cwdHint.set(partnerTabId, target.cwd);
      const callerCwd = getCwd(callerTabId);
      if (callerCwd) cwdHint.set(callerTabId, callerCwd);
      // The opener fires when the fork actually initializes; primeFork forces that init.
      pendingOpeners.set(partnerTabId, { callerTabId });
      bump();
      // Persist both sides so the bridge survives a restart (rehydrate rebuilds it).
      void persistBridge(callerTabId);
      void persistBridge(partnerTabId);
      void primeFork(partnerTabId);

      logInfo(`agentBridge: bridged ${callerTabId.slice(0, 8)} ⇄ ${partnerTabId.slice(0, 8)} (fork of ${target.sessionId.slice(0, 8)})`);
      return { ok: true, partnerTabId, partnerLabel };
    },

    /**
     * Bridge `callerTabId` to an ALREADY-RUNNING Claude tab — no fork, no new pane.
     * For when the split is already set up (e.g. auto-reconnect failed but both agents
     * are still live) and the human just wants to re-establish the bridge.
     */
    async bridgeExistingTab(
      callerTabId: string,
      targetTabId: string,
      purpose?: string,
    ): Promise<{ ok: true; partnerTabId: string; partnerLabel: string } | { ok: false; error: string }> {
      if (callerTabId === targetTabId) return { ok: false, error: 'Cannot bridge a tab to itself.' };
      const callerLoc = resolveTab(callerTabId);
      const targetLoc = resolveTab(targetTabId);
      if (!callerLoc) return { ok: false, error: 'Caller tab not found.' };
      if (!targetLoc) return { ok: false, error: 'Target tab not found.' };

      const targetState = claudeStateStore.getState(targetTabId);
      if (!targetState) return { ok: false, error: 'The target tab has no running Claude session.' };
      const callerState = claudeStateStore.getState(callerTabId);

      const callerLabel = label(callerTabId);
      const targetLabel = `${targetLoc.tab.name} · ${targetLoc.ws.name}`;

      // Don't hijack a bridge the target already has with a DIFFERENT agent.
      const targetPartner = bridges.get(targetTabId)?.partnerTabId;
      if (targetPartner && targetPartner !== callerTabId) {
        return { ok: false, error: `"${targetLabel}" is already bridged to another agent. Disconnect it there first.` };
      }
      // Abandon any stale bridge the caller has with a DIFFERENT agent (notify it).
      const callerPartner = bridges.get(callerTabId)?.partnerTabId;
      if (callerPartner && callerPartner !== targetTabId) this.disconnect(callerTabId);

      // Repairing an existing caller<->target pair (e.g. a failed auto-reconnect) →
      // reconnect in place without re-introducing an ongoing conversation. Otherwise
      // it's a fresh bridge → run the full intro flow.
      const repairing = bridges.get(callerTabId)?.partnerTabId === targetTabId;
      const callerTurn = bridges.get(callerTabId)?.turn ?? 0;
      const targetTurn = bridges.get(targetTabId)?.turn ?? 0;

      // Symmetric bridge between two established agents — both ready, both trust
      // claudeState immediately, each records the other's live session id.
      bridges.set(callerTabId, { partnerTabId: targetTabId, partnerLabel: targetLabel, turn: callerTurn, partnerSessionId: targetState.sessionId, role: 'caller', purpose: purpose?.trim() || undefined });
      bridges.set(targetTabId, { partnerTabId: callerTabId, partnerLabel: callerLabel, turn: targetTurn, partnerSessionId: callerState?.sessionId, role: 'peer' });
      delivery.set(callerTabId, { ready: true, busy: false, hasCompletedTurn: true, queue: [] });
      delivery.set(targetTabId, { ready: true, busy: false, hasCompletedTurn: true, queue: [] });
      const callerCwd = getCwd(callerTabId); if (callerCwd) cwdHint.set(callerTabId, callerCwd);
      const targetCwd = getCwd(targetTabId); if (targetCwd) cwdHint.set(targetTabId, targetCwd);

      bump();
      void persistBridge(callerTabId);
      void persistBridge(targetTabId);

      if (repairing) {
        logInfo(`agentBridge: repaired existing bridge ${callerTabId.slice(0, 8)} ⇄ ${targetTabId.slice(0, 8)}`);
      } else {
        // Prime the target (it didn't initiate) and have the caller introduce itself.
        void deliver(targetTabId, buildExistingBridgeNotice(callerLabel));
        void deliver(callerTabId, buildOpener(callerTabId, targetTabId, false));
        logInfo(`agentBridge: bridged existing ${callerTabId.slice(0, 8)} ⇄ ${targetTabId.slice(0, 8)} (no fork)`);
      }
      return { ok: true, partnerTabId: targetTabId, partnerLabel: targetLabel };
    },

    /** Handle a sendToBridgedAgent tool call from `senderTabId`. */
    async sendFromTab(senderTabId: string, message: string) {
      const bridge = bridges.get(senderTabId);
      if (!bridge) {
        return { ok: false, error: 'You are not bridged to any agent. Ask the human to bridge a session via the Agent Bridge picker.' };
      }
      const recipient = bridge.partnerTabId;
      if (recipient === senderTabId) {
        // Corrupt/misrouted bridge (partner points at self) — never inject into the
        // sender's own terminal. Surface it rather than acting on bad routing.
        return { ok: false, error: 'Bridge routing error: this tab appears bridged to itself. Ask the human to reconnect the bridge.' };
      }
      if (!tabExists(recipient)) {
        this.disconnect(senderTabId);
        return { ok: false, error: 'The bridged agent is no longer available (its tab was closed). Bridge closed.' };
      }
      if (!message || !message.trim()) {
        return { ok: false, error: 'Message is empty.' };
      }
      // The persisted bridge is authoritative, so a session-id change means the partner
      // RESUMED (not "a stranger") — re-bind to its new id rather than breaking. If
      // the partner has no live session it's dormant/resuming: deliver() will queue.
      const recipState = claudeStateStore.getState(recipient);
      if (recipState && bridge.partnerSessionId && recipState.sessionId !== bridge.partnerSessionId) {
        bridge.partnerSessionId = recipState.sessionId;
        void persistBridge(senderTabId);
        logInfo(`agentBridge: re-bound ${senderTabId.slice(0, 8)}'s partner to resumed session ${recipState.sessionId.slice(0, 8)}`);
      }
      bridge.turn += 1;
      void persistBridge(senderTabId); // keep the turn counter durable
      const text = buildEnvelope(senderTabId, message, bridge.turn);
      const status = await deliver(recipient, text);
      const recipName = bridge.partnerLabel;
      if (status === 'delivered') {
        return { ok: true, delivered: true, recipient: recipName, note: `Delivered to ${recipName}. Their reply will arrive as a new prompt — finish your turn now.` };
      }
      if (status === 'queued') {
        const offline = !claudeStateStore.getState(recipient);
        const note = offline
          ? `${recipName} is currently offline (its session isn't running). Your message is queued and will be delivered when it resumes.`
          : `${recipName} is busy; your message is queued and will be delivered when they're free.`;
        return { ok: true, delivered: false, queued: true, recipient: recipName, note };
      }
      return { ok: false, error: 'Delivery failed (could not write to the bridged terminal).' };
    },

    /** Break the bridge from either side and notify the survivor. This is a permanent
     *  teardown (user-initiated or tab closed) — it clears the persisted pairing too,
     *  unlike a session-end which only suspends. */
    disconnect(tabId: string) {
      const bridge = bridges.get(tabId);
      if (!bridge) return;
      const partner = bridge.partnerTabId;
      cleanup(tabId);
      cleanup(partner);
      bump();
      // Clear the durable pairing on both tabs (persistBridge writes null when the
      // in-memory entry is gone). For a closed tab resolveTab fails and it's skipped.
      void persistBridge(tabId);
      void persistBridge(partner);
      // Best-effort notice to the survivor (if it exists and isn't mid-turn).
      if (tabExists(partner) && claudeStateStore.getState(partner)?.state !== 'active') {
        void injectPrompt(partner, '⟦AGENT-BRIDGE⟧ The agent you were bridged with has disconnected. The bridge is closed.');
      }
      logInfo(`agentBridge: disconnected ${tabId.slice(0, 8)} ⇄ ${partner.slice(0, 8)}`);
    },

    async init() {
      // claude-init-session lands in two situations we care about:
      //   1. A fresh fork completing its handshake (primeFork forced the init).
      //   2. An already-bridged tab re-initializing after a resume (or a rehydrated
      //      bridge coming back online) — re-bind it.
      const u1 = await listen<{ tab_id: string | null; session_id: string }>('claude-init-session', (e) => {
        const { tab_id, session_id } = e.payload;
        if (!tab_id) return;

        // Case 1: fork handshake. Proves the fork is up, on THIS instance, and
        // tool-capable. Record its session id on the caller, mark it ready, send the
        // opener to the caller.
        const po = pendingOpeners.get(tab_id);
        if (po) {
          pendingOpeners.delete(tab_id);
          const callerBridge = bridges.get(po.callerTabId);
          if (callerBridge) { callerBridge.partnerSessionId = session_id; void persistBridge(po.callerTabId); }
          const d = delivery.get(tab_id);
          if (d) { d.ready = true; void flush(tab_id); }
          if (tabExists(po.callerTabId)) void deliver(po.callerTabId, buildOpener(po.callerTabId, tab_id));
          logInfo(`agentBridge: fork ${tab_id.slice(0, 8)} initialized → opener to caller ${po.callerTabId.slice(0, 8)}`);
          return;
        }

        // Case 2: a bridged tab resumed. Refresh the PARTNER's record of this tab's
        // (possibly new) session id so the partner's self-healing send re-binds, and
        // mark this tab deliverable again so any queued messages flush.
        const bridge = bridges.get(tab_id);
        if (bridge) {
          const partner = bridges.get(bridge.partnerTabId);
          if (partner && partner.partnerSessionId !== session_id) {
            partner.partnerSessionId = session_id;
            void persistBridge(bridge.partnerTabId);
          }
          const d = delivery.get(tab_id);
          if (d) {
            d.ready = true;
            d.busy = false;
            if (d.busyTimer) { clearTimeout(d.busyTimer); d.busyTimer = undefined; }
          } else {
            delivery.set(tab_id, { ready: true, busy: false, hasCompletedTurn: true, queue: [] });
          }
          bump();
          void flush(tab_id);
          logInfo(`agentBridge: ${tab_id.slice(0, 8)} re-initialized → bridge re-bound`);
        }
      });
      unlisteners.push(u1);

      // A turn finished → that tab is idle and alive again. Clear busy, (re)enable
      // delivery (a Stop proves liveness, e.g. after a webview reload), flush queue.
      const u2 = await listen<{ session_id: string; tab_id: string | null }>('claude-hook-stop', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId) return;
        const d = delivery.get(tabId);
        if (!d) return;
        d.hasCompletedTurn = true;
        d.ready = true;
        d.busy = false;
        if (d.busyTimer) { clearTimeout(d.busyTimer); d.busyTimer = undefined; }
        void flush(tabId);
      });
      unlisteners.push(u2);

      // Session ended (process exit). DON'T tear the bridge down — the agent may resume
      // (app-restart auto-resume or a manual resume) and re-bind via Case 2 above.
      // Just suspend live delivery; the durable pairing is kept so it can come back.
      // Only an explicit disconnect or a closed tab removes the bridge permanently.
      const u3 = await listen<{ session_id: string; tab_id: string | null }>('claude-hook-session-end', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !bridges.has(tabId)) return;
        const d = delivery.get(tabId);
        if (d) {
          d.ready = false;
          d.busy = false;
          if (d.busyTimer) { clearTimeout(d.busyTimer); d.busyTimer = undefined; }
        }
        bump();
        logInfo(`agentBridge: ${tabId.slice(0, 8)} session ended → bridge dormant (awaiting resume)`);
      });
      unlisteners.push(u3);
    },

    /** Rebuild in-memory bridges from persisted agent_bridge fields. Call once after
     *  workspaces load. Only restores a pair when both tabs exist and reciprocally
     *  reference each other; orphans are cleared. Last-known session ids are refreshed
     *  as each agent re-inits (Case 2 above). */
    rehydrate() {
      const persisted = new Map<string, AgentBridge>();
      for (const ws of workspacesStore.workspaces)
        for (const pane of ws.panes)
          for (const tab of pane.tabs)
            if (tab.agent_bridge) persisted.set(tab.id, tab.agent_bridge);

      let restored = 0;
      for (const [tabId, al] of persisted) {
        if (bridges.has(tabId)) continue; // already live this session
        // A bridge must point at a DIFFERENT tab. A self-reference is corrupt data —
        // never restore it (it would make the agent its own bridged peer).
        if (al.partner_tab_id === tabId) {
          void persistBridge(tabId); // clear the bad entry
          continue;
        }
        const partnerAl = persisted.get(al.partner_tab_id);
        // Require a reciprocal pairing (both tabs present, pointing at each other).
        if (!partnerAl || partnerAl.partner_tab_id !== tabId) {
          void persistBridge(tabId); // orphan → clear (no in-memory entry → writes null)
          continue;
        }
        bridges.set(tabId, {
          partnerTabId: al.partner_tab_id,
          partnerLabel: al.partner_label,
          turn: al.turn ?? 0,
          partnerSessionId: al.partner_session_id ?? undefined,
          role: al.role === 'fork' || al.role === 'peer' ? al.role : 'caller',
        });
        // Deliverable only once this tab's Claude is live. On a cold restart it isn't
        // up yet → ready=false; the init handler flips it on resume. If already live
        // (e.g. webview reload), start ready.
        const live = !!claudeStateStore.getState(tabId);
        delivery.set(tabId, { ready: live, busy: false, hasCompletedTurn: true, queue: [] });
        restored++;
      }
      if (restored) { bump(); logInfo(`agentBridge: rehydrated ${restored / 2} bridge(s) from persisted state`); }
    },

    destroy() {
      for (const u of unlisteners) u();
      unlisteners.length = 0;
      for (const d of delivery.values()) if (d.busyTimer) clearTimeout(d.busyTimer);
      bridges.clear();
      delivery.clear();
      pendingOpeners.clear();
      cwdHint.clear();
    },
  };
}

export const agentBridgeStore = createAgentBridgeStore();
