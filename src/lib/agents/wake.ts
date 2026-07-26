import { writeTerminal } from '$lib/tauri/commands';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
import { replayAutoResume } from '$lib/stores/triggers.svelte';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

/**
 * Headless single-tab wake — bring one unregistered tab back to a state where a remote
 * message can actually land in its agent.
 *
 * Two remedies, chosen by the CALLER (maiLink's backend probes the PTY's process tree and
 * passes the verdict, so the decision lives with the contract rather than being re-derived
 * here):
 *   • `init`   — the agent is still running, it just lost its registration → `/maiterm init`.
 *   • `resume` — the agent exited and the PTY is back at a shell prompt → replay auto-resume.
 *
 * Sending the wrong one is destructive in both directions (an ssh+resume replay typed into a
 * live agent injects junk and nests ssh; `/maiterm init` typed at a bare shell runs as a
 * command), which is why this takes the action rather than guessing.
 *
 * Sibling: `agentMesh.settleAndSendInit` does the same job for the mesh's workspace-wide
 * readiness pass. It has no deadline to respect — nothing types after it — so it waits out a
 * busy tab instead of abandoning the paste (see the `deadline` note below).
 */
export type WakeAction = 'init' | 'resume';

/** Tabs with a wake already running. A second send while one is in flight must not fire a
 *  second `/maiterm init` — the first is mid-settle and both pastes would collide. */
const inFlight = new Set<string>();

const INIT_QUIET_MS = 1500;
const INIT_QUIET_POLL_MS = 300;

/** Dialog-safe `/maiterm init` delivery. A resumed agent may be sitting at a startup dialog
 *  (e.g. "restore as is / compact first") that would swallow a straight paste — so send a bare
 *  CR to answer it, wait for the PTY to go output-quiet (compaction/thinking spinners repaint
 *  continuously), then deliver the init exactly once.
 *
 *  `deadline` is the caller's budget, not a nicety: whoever asked for the wake delivers a
 *  message of its own once the budget expires, so an init pasted after that point would land
 *  ON TOP of that message. Past the deadline we skip the paste rather than race it. */
async function settleAndSendInit(tabId: string, ptyId: string, deadline: number) {
  await writeTerminal(ptyId, [0x0d]);
  while (Date.now() < deadline) {
    const lastOut = terminalsStore.getLastOutputAt(tabId) ?? 0;
    if (Date.now() - lastOut >= INIT_QUIET_MS) break;
    await new Promise((res) => setTimeout(res, INIT_QUIET_POLL_MS));
  }
  if (claudeStateStore.getState(tabId)) return; // re-registered on its own while settling
  if (Date.now() >= deadline) {
    logInfo(`wake: ${tabId.slice(0, 8)} never went quiet within budget — skipping init paste`);
    return;
  }
  await bracketedPasteSubmit(ptyId, '/maiterm init');
}

/** Run one wake. Resolves when the remedy has been DELIVERED, not when the agent is ready —
 *  the caller watches registration for that. Concurrent calls for the same tab no-op. */
export async function wakeTab(tabId: string, action: WakeAction, budgetMs: number): Promise<void> {
  if (inFlight.has(tabId)) return;
  const inst = terminalsStore.get(tabId);
  if (!inst) return; // no live PTY — nothing to wake (a suspended workspace is resumed first)
  inFlight.add(tabId);
  try {
    if (action === 'init') {
      await settleAndSendInit(tabId, inst.ptyId, Date.now() + budgetMs);
    } else {
      await replayAutoResume(tabId);
    }
    logInfo(`wake: ${action} delivered to tab ${tabId.slice(0, 8)}`);
  } catch (e) {
    logError(`wake: ${action} failed for tab ${tabId.slice(0, 8)}: ${e}`);
  } finally {
    inFlight.delete(tabId);
  }
}
