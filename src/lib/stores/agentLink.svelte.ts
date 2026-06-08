import { countedListen as listen } from '$lib/utils/listenCounter';
import * as commands from '$lib/tauri/commands';
import type { AgentLink } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore } from '$lib/stores/claudeState.svelte';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

/**
 * Agent Link — a bridge between two running Claude agents in different aiTerm panes.
 *
 * The human links the current tab to another running Claude session (picked from
 * any workspace). aiTerm FORKS that session (`claude --resume <id> --fork-session`)
 * into a fresh split pane beside the caller — an isolated peer with the target's
 * full context. The two agents then converse asynchronously via the
 * `sendToLinkedAgent` MCP tool; every message is injected as a real terminal turn
 * in the recipient's pane, so the human watches (and can interrupt with Esc).
 *
 * Identity is stamped by aiTerm (not self-asserted), so the recipient always knows
 * a message is from a peer agent — never confused for the human operator.
 *
 * Handshake (tight + routing-proof): a forked session resumes the target's
 * transcript, which already contains the target's `initSession` — so the fork
 * believes it is already initialized (as the wrong tab) and never re-binds its new
 * MCP connection, leaving its aiTerm tools unusable. So after the fork's Claude
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
 * Links are self-healing: at send time the recipient must still have a live Claude
 * session with the same session id recorded at link time; otherwise the link is
 * broken cleanly instead of routing into a dead/wrong target.
 */

const INJECT_GAP_MS = 120;           // gap between bracketed-paste and the submitting CR
const BUSY_TIMEOUT_MS = 300_000;     // safety: auto-clear busy if no Stop ever arrives
const FORK_BOOT_POLL_MS = 500;       // poll interval while waiting for the fork's Claude to register
const FORK_BOOT_TIMEOUT_MS = 15_000; // cap on waiting for the fork to boot before priming anyway
const FORK_SETTLE_MS = 1500;         // extra settle after the fork registers, so its TUI accepts input
const FORK_INIT_TIMEOUT_MS = 25_000; // if the fork never re-inits on this instance, tell the caller

type LinkRole = 'caller' | 'fork';

interface LinkEntry {
  /** The tab this agent is linked to. */
  partnerTabId: string;
  /** Human-readable label of the partner (for the agent's own awareness). */
  partnerLabel: string;
  /** Conversation turn counter (incremented on each message this tab sends). */
  turn: number;
  /** Partner's last-known Claude session id. Refreshed when the partner re-inits
   *  after a resume; used to detect a drifted session and re-bind (NOT to break —
   *  the persisted link is authoritative, so a new id means "it resumed"). */
  partnerSessionId?: string;
  /** Whether this tab initiated the link or is the forked peer. */
  role: LinkRole;
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

function createAgentLinkStore() {
  // Both tabs of a link get an entry pointing at each other (symmetric).
  const links = new Map<string, LinkEntry>();
  // Delivery state is keyed by the RECIPIENT tab.
  const delivery = new Map<string, DeliveryState>();
  // Forked partners awaiting init → opener-into-caller (keyed by fork tab id).
  const pendingOpeners = new Map<string, { callerTabId: string }>();
  // Best-effort cwd label when live OSC cwd isn't available yet.
  const cwdHint = new Map<string, string>();
  // Reactive version bump so UI ($derived) can react to link changes.
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

  /** Clean display name for identity envelopes (strips link glyphs). */
  function label(tabId: string): string {
    const loc = resolveTab(tabId);
    if (!loc) return 'unknown agent';
    return loc.tab.name.replace(/^[⇄↔→]\s*/u, '').trim() || 'agent';
  }

  function getCwd(tabId: string): string | null {
    const osc = terminalsStore.getOsc(tabId);
    return osc?.cwd ?? osc?.promptCwd ?? cwdHint.get(tabId) ?? null;
  }

  /** Persist this tab's link to the backend (or clear it if the in-memory entry is
   *  gone), so the pairing survives an app restart and can be rehydrated. The live
   *  routing stays in memory; only the durable pairing is written here. */
  async function persistLink(tabId: string) {
    const loc = resolveTab(tabId);
    if (!loc) return; // tab gone — nothing to persist to
    const link = links.get(tabId);
    const payload = link
      ? {
          partner_tab_id: link.partnerTabId,
          partner_label: link.partnerLabel,
          partner_session_id: link.partnerSessionId ?? null,
          role: link.role,
          turn: link.turn,
        }
      : null;
    try {
      await commands.setTabAgentLink(loc.ws.id, loc.pane.id, tabId, payload);
    } catch (e) {
      logError(`agentLink: failed to persist link for tab ${tabId.slice(0, 8)}: ${e}`);
    }
  }

  // ─── Injection ──────────────────────────────────────────────────────────────

  /** Write a prompt into a tab's PTY as a bracketed paste, then submit with CR.
   *  Bracketed paste keeps multi-line content as one prompt (newlines don't submit
   *  early); the deferred CR submits it. */
  async function injectPrompt(tabId: string, text: string): Promise<boolean> {
    const inst = terminalsStore.get(tabId);
    if (!inst) {
      logError(`agentLink: cannot inject — no terminal instance for tab ${tabId.slice(0, 8)}`);
      return false;
    }
    try {
      await commands.writeTerminal(inst.ptyId, enc(`\x1b[200~${text}\x1b[201~`));
      await sleep(INJECT_GAP_MS);
      await commands.writeTerminal(inst.ptyId, enc('\r'));
      return true;
    } catch (e) {
      logError(`agentLink: inject failed for tab ${tabId.slice(0, 8)}: ${e}`);
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

  // ─── Envelopes (identity stamped by aiTerm) ──────────────────────────────────

  function buildEnvelope(senderTabId: string, message: string, turn: number): string {
    const name = label(senderTabId);
    const cwd = getCwd(senderTabId);
    const where = cwd ? `, working in ${cwd}` : '';
    return (
      `⟦AGENT-LINK⟧ Message from "${name}"${where} — a peer AI agent, NOT your human operator. [turn ${turn}]\n` +
      `Reply with the sendToLinkedAgent tool. If this fully answers the request, you can stop — don't reply just to acknowledge.\n\n` +
      message
    );
  }

  function buildOpener(callerTabId: string, partnerTabId: string): string {
    const partnerName = links.get(callerTabId)?.partnerLabel ?? label(partnerTabId);
    const cwd = getCwd(partnerTabId);
    const where = cwd ? ` (working in ${cwd})` : '';
    return (
      `⟦AGENT-LINK⟧ You are now linked to "${partnerName}"${where} — a peer AI agent forked with the FULL context of that session. ` +
      `It can answer questions and do research about that codebase.\n\n` +
      `Introduce yourself with the sendToLinkedAgent tool: say who you are, what you're working on, and why you're reaching out — then ask your first question. ` +
      `The other agent's replies arrive here as new prompts. When you have what you need, just stop; no need to sign off.`
    );
  }

  /** Directive injected into the fork to force it to re-initialize as its OWN tab
   *  (a resumed/forked session otherwise inherits the target's initSession and never
   *  re-binds its new MCP connection, so its aiTerm tools stay unusable). */
  function buildForkInitDirective(forkTabId: string, peerLabel: string): string {
    return (
      `⟦AGENT-LINK⟧ You are now a FORKED peer agent in a NEW aiTerm tab (id ${forkTabId}). ` +
      `This is a fresh tab with a fresh MCP connection, so you must re-initialize: call your aiterm initSession tool with tabId "${forkTabId}" right now. ` +
      `Disregard any tab id mentioned earlier in this conversation — you are "${forkTabId}" now.\n\n` +
      `You have been linked to a peer AI agent ("${peerLabel}") via aiTerm Agent Link. ` +
      `After initializing, reply with a one-line readiness note, then wait — the peer's message will arrive as a new prompt.`
    );
  }

  /** Sent to the caller if the fork never re-initializes on this instance. */
  function buildLinkFailedNote(forkTabId: string): string {
    return (
      `⟦AGENT-LINK⟧ The link to "${label(forkTabId)}" could not be completed — the forked agent did not initialize on this aiTerm instance ` +
      `(it may have connected to a different one). You can run /aiterm init in the new pane and retry, or unlink and link again.`
    );
  }

  /** After spawning a fork, wait for its Claude to register on THIS instance, then
   *  inject the re-init directive. The handshake (opener → caller) fires separately,
   *  when the fork's initSession actually lands (see the claude-init-session handler
   *  in init()). If the fork never inits here, the caller is told rather than left
   *  hanging. */
  async function primeFork(forkTabId: string) {
    for (let waited = 0; waited < FORK_BOOT_TIMEOUT_MS; waited += FORK_BOOT_POLL_MS) {
      if (!pendingOpeners.has(forkTabId)) return;            // already handshaked / unlinked
      if (claudeStateStore.getState(forkTabId)) break;        // fork's Claude is up on this instance
      await sleep(FORK_BOOT_POLL_MS);
    }
    await sleep(FORK_SETTLE_MS);
    if (!pendingOpeners.has(forkTabId)) return;

    const peerLabel = links.get(forkTabId)?.partnerLabel ?? 'your linked peer';
    const ok = await injectPrompt(forkTabId, buildForkInitDirective(forkTabId, peerLabel));
    if (!ok) {
      logError(`agentLink: failed to prime fork ${forkTabId.slice(0, 8)}`);
      return;
    }
    // Backstop: if the fork doesn't re-init on this instance, don't leave the caller waiting.
    setTimeout(() => {
      const po = pendingOpeners.get(forkTabId);
      if (!po) return;                                        // handshake completed
      pendingOpeners.delete(forkTabId);
      if (tabExists(po.callerTabId)) void deliver(po.callerTabId, buildLinkFailedNote(forkTabId));
    }, FORK_INIT_TIMEOUT_MS);
  }

  // ─── Lifecycle: link / unlink ────────────────────────────────────────────────

  function cleanup(tabId: string) {
    const d = delivery.get(tabId);
    if (d?.busyTimer) clearTimeout(d.busyTimer);
    delivery.delete(tabId);
    links.delete(tabId);
    pendingOpeners.delete(tabId);
    cwdHint.delete(tabId);
  }

  return {
    get version() { return version; },

    getInternalSizes() {
      return { links: links.size, delivery: delivery.size, pending_openers: pendingOpeners.size };
    },

    isLinked(tabId: string): boolean {
      void version;
      return links.has(tabId);
    },

    getPartnerTabId(tabId: string): string | null {
      return links.get(tabId)?.partnerTabId ?? null;
    },

    getPartnerLabel(tabId: string): string | null {
      void version;
      return links.get(tabId)?.partnerLabel ?? null;
    },

    /** For the getLinkedAgent MCP tool. */
    getLinkInfo(tabId: string) {
      const link = links.get(tabId);
      if (!link) return { linked: false };
      return {
        linked: true,
        partner: {
          tabId: link.partnerTabId,
          label: link.partnerLabel,
          cwd: getCwd(link.partnerTabId),
          available: tabExists(link.partnerTabId),
        },
      };
    },

    /**
     * Fork `target`'s session into a split beside `callerTabId` and link the two.
     * `target` comes from the picker (getClaudeSessions / claudeState).
     */
    async establishLink(
      callerTabId: string,
      target: { sessionId: string; tabName: string; workspaceName: string; cwd: string | null; sshCommand?: string | null; remoteCwd?: string | null },
    ): Promise<{ ok: true; partnerTabId: string; partnerLabel: string } | { ok: false; error: string }> {
      const loc = resolveTab(callerTabId);
      if (!loc) return { ok: false, error: 'Caller tab not found.' };
      if (links.has(callerTabId)) return { ok: false, error: 'This tab is already linked. Unlink it first.' };

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

      links.set(callerTabId, { partnerTabId, partnerLabel, turn: 0, role: 'caller' });
      // The fork's entry knows the caller's session id up front; the caller learns
      // the fork's session id when the fork initializes (see init() handler).
      links.set(partnerTabId, { partnerTabId: callerTabId, partnerLabel: callerLabel, turn: 0, partnerSessionId: callerSessionId, role: 'fork' });
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
      // Persist both sides so the link survives a restart (rehydrate rebuilds it).
      void persistLink(callerTabId);
      void persistLink(partnerTabId);
      void primeFork(partnerTabId);

      logInfo(`agentLink: linked ${callerTabId.slice(0, 8)} ⇄ ${partnerTabId.slice(0, 8)} (fork of ${target.sessionId.slice(0, 8)})`);
      return { ok: true, partnerTabId, partnerLabel };
    },

    /** Handle a sendToLinkedAgent tool call from `senderTabId`. */
    async sendFromTab(senderTabId: string, message: string) {
      const link = links.get(senderTabId);
      if (!link) {
        return { ok: false, error: 'You are not linked to any agent. Ask the human to link a session via the Agent Link picker.' };
      }
      const recipient = link.partnerTabId;
      if (!tabExists(recipient)) {
        this.unlink(senderTabId);
        return { ok: false, error: 'The linked agent is no longer available (its tab was closed). Link closed.' };
      }
      if (!message || !message.trim()) {
        return { ok: false, error: 'Message is empty.' };
      }
      // The persisted link is authoritative, so a session-id change means the partner
      // RESUMED (not "a stranger") — re-bind to its new id rather than breaking. If
      // the partner has no live session it's dormant/resuming: deliver() will queue.
      const recipState = claudeStateStore.getState(recipient);
      if (recipState && link.partnerSessionId && recipState.sessionId !== link.partnerSessionId) {
        link.partnerSessionId = recipState.sessionId;
        void persistLink(senderTabId);
        logInfo(`agentLink: re-bound ${senderTabId.slice(0, 8)}'s partner to resumed session ${recipState.sessionId.slice(0, 8)}`);
      }
      link.turn += 1;
      void persistLink(senderTabId); // keep the turn counter durable
      const text = buildEnvelope(senderTabId, message, link.turn);
      const status = await deliver(recipient, text);
      const recipName = link.partnerLabel;
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
      return { ok: false, error: 'Delivery failed (could not write to the linked terminal).' };
    },

    /** Break the link from either side and notify the survivor. This is a permanent
     *  teardown (user-initiated or tab closed) — it clears the persisted pairing too,
     *  unlike a session-end which only suspends. */
    unlink(tabId: string) {
      const link = links.get(tabId);
      if (!link) return;
      const partner = link.partnerTabId;
      cleanup(tabId);
      cleanup(partner);
      bump();
      // Clear the durable pairing on both tabs (persistLink writes null when the
      // in-memory entry is gone). For a closed tab resolveTab fails and it's skipped.
      void persistLink(tabId);
      void persistLink(partner);
      // Best-effort notice to the survivor (if it exists and isn't mid-turn).
      if (tabExists(partner) && claudeStateStore.getState(partner)?.state !== 'active') {
        void injectPrompt(partner, '⟦AGENT-LINK⟧ The agent you were linked with has disconnected. The link is closed.');
      }
      logInfo(`agentLink: unlinked ${tabId.slice(0, 8)} ⇄ ${partner.slice(0, 8)}`);
    },

    async init() {
      // claude-init-session lands in two situations we care about:
      //   1. A fresh fork completing its handshake (primeFork forced the init).
      //   2. An already-linked tab re-initializing after a resume (or a rehydrated
      //      link coming back online) — re-bind it.
      const u1 = await listen<{ tab_id: string | null; session_id: string }>('claude-init-session', (e) => {
        const { tab_id, session_id } = e.payload;
        if (!tab_id) return;

        // Case 1: fork handshake. Proves the fork is up, on THIS instance, and
        // tool-capable. Record its session id on the caller, mark it ready, send the
        // opener to the caller.
        const po = pendingOpeners.get(tab_id);
        if (po) {
          pendingOpeners.delete(tab_id);
          const callerLink = links.get(po.callerTabId);
          if (callerLink) { callerLink.partnerSessionId = session_id; void persistLink(po.callerTabId); }
          const d = delivery.get(tab_id);
          if (d) { d.ready = true; void flush(tab_id); }
          if (tabExists(po.callerTabId)) void deliver(po.callerTabId, buildOpener(po.callerTabId, tab_id));
          logInfo(`agentLink: fork ${tab_id.slice(0, 8)} initialized → opener to caller ${po.callerTabId.slice(0, 8)}`);
          return;
        }

        // Case 2: a linked tab resumed. Refresh the PARTNER's record of this tab's
        // (possibly new) session id so the partner's self-healing send re-binds, and
        // mark this tab deliverable again so any queued messages flush.
        const link = links.get(tab_id);
        if (link) {
          const partner = links.get(link.partnerTabId);
          if (partner && partner.partnerSessionId !== session_id) {
            partner.partnerSessionId = session_id;
            void persistLink(link.partnerTabId);
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
          logInfo(`agentLink: ${tab_id.slice(0, 8)} re-initialized → link re-bound`);
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

      // Session ended (process exit). DON'T tear the link down — the agent may resume
      // (app-restart auto-resume or a manual resume) and re-bind via Case 2 above.
      // Just suspend live delivery; the durable pairing is kept so it can come back.
      // Only an explicit unlink or a closed tab removes the link permanently.
      const u3 = await listen<{ session_id: string; tab_id: string | null }>('claude-hook-session-end', (e) => {
        const tabId = e.payload.tab_id;
        if (!tabId || !links.has(tabId)) return;
        const d = delivery.get(tabId);
        if (d) {
          d.ready = false;
          d.busy = false;
          if (d.busyTimer) { clearTimeout(d.busyTimer); d.busyTimer = undefined; }
        }
        bump();
        logInfo(`agentLink: ${tabId.slice(0, 8)} session ended → link dormant (awaiting resume)`);
      });
      unlisteners.push(u3);
    },

    /** Rebuild in-memory links from persisted agent_link fields. Call once after
     *  workspaces load. Only restores a pair when both tabs exist and reciprocally
     *  reference each other; orphans are cleared. Last-known session ids are refreshed
     *  as each agent re-inits (Case 2 above). */
    rehydrate() {
      const persisted = new Map<string, AgentLink>();
      for (const ws of workspacesStore.workspaces)
        for (const pane of ws.panes)
          for (const tab of pane.tabs)
            if (tab.agent_link) persisted.set(tab.id, tab.agent_link);

      let restored = 0;
      for (const [tabId, al] of persisted) {
        if (links.has(tabId)) continue; // already live this session
        const partnerAl = persisted.get(al.partner_tab_id);
        // Require a reciprocal pairing (both tabs present, pointing at each other).
        if (!partnerAl || partnerAl.partner_tab_id !== tabId) {
          void persistLink(tabId); // orphan → clear (no in-memory entry → writes null)
          continue;
        }
        links.set(tabId, {
          partnerTabId: al.partner_tab_id,
          partnerLabel: al.partner_label,
          turn: al.turn ?? 0,
          partnerSessionId: al.partner_session_id ?? undefined,
          role: al.role === 'fork' ? 'fork' : 'caller',
        });
        // Deliverable only once this tab's Claude is live. On a cold restart it isn't
        // up yet → ready=false; the init handler flips it on resume. If already live
        // (e.g. webview reload), start ready.
        const live = !!claudeStateStore.getState(tabId);
        delivery.set(tabId, { ready: live, busy: false, hasCompletedTurn: true, queue: [] });
        restored++;
      }
      if (restored) { bump(); logInfo(`agentLink: rehydrated ${restored / 2} link(s) from persisted state`); }
    },

    destroy() {
      for (const u of unlisteners) u();
      unlisteners.length = 0;
      for (const d of delivery.values()) if (d.busyTimer) clearTimeout(d.busyTimer);
      links.clear();
      delivery.clear();
      pendingOpeners.clear();
      cwdHint.clear();
    },
  };
}

export const agentLinkStore = createAgentLinkStore();
