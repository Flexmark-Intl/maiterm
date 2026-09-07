import type { AgentRuntime } from './types';
import type { AgentTabSession } from '$lib/stores/agentState.svelte';
import { buildForkCommand, supportsFork } from './resume';

/**
 * Per-runtime behaviors the Agent Bridge needs that genuinely differ by agent
 * runtime. The bridge's plumbing (PTY injection, registry, queue/drain, persistence)
 * is runtime-agnostic; only these four couplings vary. The Claude adapter reproduces
 * the bridge's original hardcoded Claude behavior exactly.
 */
export interface AgentAdapter {
  runtime: AgentRuntime;
  /** Hold bridge delivery while the recipient is at a prompt awaiting the HUMAN — an
   *  injected paste+CR would hijack their selection. */
  isAwaitingHumanInput(state: AgentTabSession | undefined): boolean;
  /** Whether this runtime can fork a session into a fresh, isolated pane. */
  supportsFork: boolean;
  /** The command that forks `sessionId` into a new pane, or null if unsupported. */
  buildForkCommand(sessionId: string): string | null;
  /** Whether a freshly forked session must be force-re-initialized: Claude inherits the
   *  target's initSession from the resumed transcript and won't rebind its new MCP
   *  connection otherwise. Runtimes that rebind on their own set this false. */
  forkNeedsReinit: boolean;
  /** Directive injected into a fork to force it to re-init as its OWN tab. */
  buildForkInitDirective(forkTabId: string, peerLabel: string): string;
}

const claudeAdapter: AgentAdapter = {
  runtime: 'claude',
  isAwaitingHumanInput(state) {
    if (!state) return false;
    // A permission prompt, or an active interactive elicitation tool (AskUserQuestion),
    // is a multiple-choice question for the human — never inject a peer message over it.
    if (state.state === 'permission') return true;
    if (state.state === 'active' && state.toolName === 'AskUserQuestion') return true;
    return false;
  },
  supportsFork: supportsFork('claude'),
  buildForkCommand(sessionId) {
    return buildForkCommand('claude', sessionId);
  },
  forkNeedsReinit: true,
  buildForkInitDirective(forkTabId, peerLabel) {
    return (
      `⟦AGENT-BRIDGE⟧ You are now a FORKED peer agent in a NEW maiTerm tab (id ${forkTabId}). ` +
      `This is a fresh tab with a fresh MCP connection, so you must re-initialize: call your maiterm initSession tool with tabId "${forkTabId}" right now. ` +
      `Disregard any tab id mentioned earlier in this conversation — you are "${forkTabId}" now.\n\n` +
      `You have been bridged to a peer AI agent ("${peerLabel}") via maiTerm Agent Bridge. ` +
      `After initializing, reply with a one-line readiness note, then wait — the peer's message will arrive as a new prompt.`
    );
  },
};

const codexAdapter: AgentAdapter = {
  runtime: 'codex',
  isAwaitingHumanInput(state) {
    if (!state) return false;
    // Codex signals a human approval prompt via PermissionRequest → 'permission' state.
    // It has no AskUserQuestion-style active elicitation tool to guard against.
    return state.state === 'permission';
  },
  supportsFork: supportsFork('codex'),
  buildForkCommand(sessionId) {
    return buildForkCommand('codex', sessionId);
  },
  // Codex identifies its tab per-process (the x-maiterm-tab header), so unlike Claude it does
  // not inherit a stale identity from the resumed transcript. It is asked to init anyway,
  // because the bridge handshake needs proof the fork is up, on THIS instance, and
  // TOOL-capable — and only a real tool call gives all three. A hook cannot.
  forkNeedsReinit: true,
  buildForkInitDirective(forkTabId, peerLabel) {
    return (
      `⟦AGENT-BRIDGE⟧ You are now a FORKED peer agent in a NEW maiTerm tab (id ${forkTabId}). ` +
      `Call your maiterm initSession tool with tabId "${forkTabId}" right now — this is maiTerm asking, ` +
      `and it is what completes the bridge handshake. ` +
      `Disregard any tab id mentioned earlier in this conversation — you are "${forkTabId}" now.\n\n` +
      `You have been bridged to a peer AI agent ("${peerLabel}") via maiTerm Agent Bridge. ` +
      `After initializing, reply with a one-line readiness note, then wait — the peer's message will arrive as a new prompt.`
    );
  },
};

// Gemini borrows Codex's prompt-state model but NOT its fork: spreading the whole adapter
// would have handed it Codex's `codex fork` command the moment that was enabled.
const geminiAdapter: AgentAdapter = {
  ...codexAdapter,
  runtime: 'gemini',
  supportsFork: supportsFork('gemini'),
  buildForkCommand(sessionId) {
    return buildForkCommand('gemini', sessionId);
  },
  forkNeedsReinit: false,
};

/** Resolve the bridge adapter for a runtime (defaults to Claude). */
export function getAdapter(runtime: AgentRuntime): AgentAdapter {
  switch (runtime) {
    case 'codex':
      return codexAdapter;
    case 'gemini':
      return geminiAdapter;
    default:
      return claudeAdapter;
  }
}
