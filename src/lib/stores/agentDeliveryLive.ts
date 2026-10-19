import { terminalsStore } from '$lib/stores/terminals.svelte';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { getAdapter } from '$lib/agents/adapter';
import { deliverPrompt } from '$lib/utils/agentPrompt';
import { createDeliveryController } from '$lib/stores/agentDelivery';
import { agentDialogOpen, agentInputBox, trustDialogOpen } from '$lib/tauri/commands';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

/**
 * The ONE live delivery mailbox — the agentDelivery core wired to real PTYs, shared by the
 * 1:1 Agent Bridge (agentBridge.svelte.ts) and the Mesh (agentMesh.svelte.ts). A tab can be
 * both a bridge partner and a mesh member; giving each store its own controller gave such a
 * tab two `injecting` guards, so a bridge paste and a mesh paste could overlap on one PTY.
 * Slots are owner-tagged (`claim`/`release` with DELIVERY_OWNER_*), so either side leaving
 * can't drop the other's queue.
 *
 * Owned by +layout: created at import, destroyed once there — the stores don't destroy it.
 */

export const DELIVERY_OWNER_BRIDGE = 'bridge';
export const DELIVERY_OWNER_MESH = 'mesh';

/** Write a prompt into a tab's PTY as a bracketed paste, then submit with CR. Bracketed
 *  paste keeps multi-line content as one prompt (newlines don't submit early); the deferred,
 *  settle-scaled CR submits it. Shares bracketedPasteSubmit with the composer dock so the
 *  submit timing can't drift apart — a too-short gap here was dropping the CR on long replies
 *  (a 20-line message stages as `[Pasted text]` but never sends). Also used directly for
 *  one-off writes that intentionally bypass the queue (fork re-init directive, disconnect
 *  notice). A tab whose maiterm-tab mod takes prompts gets it submitted instead (`deliverPrompt`),
 *  and only a typed one is subject to `clearToType`, the caller's draft check. */
export async function injectPrompt(
  tabId: string,
  text: string,
  beforePaste?: () => boolean,
  clearToType: () => Promise<boolean> = async () => true,
): Promise<boolean> {
  const inst = terminalsStore.get(tabId);
  if (!inst) {
    logError(`agentDelivery: cannot inject — no terminal instance for tab ${tabId.slice(0, 8)}`);
    return false;
  }
  try {
    // Every bridge and mesh write ends in a CR, and at Claude's workspace-trust dialog that CR
    // confirms "No, exit". The queued path never gets here for a tab with no session, but the
    // one-off writes do: the disconnect notice goes to any partner that isn't `active`, and the
    // fork directive goes in anyway once the fork's boot timeout expires.
    if (await trustDialogOpen(tabId)) {
      logInfo(`agentDelivery: ${tabId.slice(0, 8)} is at the workspace-trust dialog — not typing into it`);
      return false;
    }
    // The caller's last word, with nothing async left between it and the write.
    // Both guard the typing only: a prompt the tab's mod takes goes around the box. The draft
    // check reads the screen, so the caller's last word comes after it.
    const via = await deliverPrompt(tabId, inst.ptyId, text, async () => (await clearToType()) && (!beforePaste || beforePaste()));
    return via !== false;
  } catch (e) {
    logError(`agentDelivery: inject failed for tab ${tabId.slice(0, 8)}: ${e}`);
    return false;
  }
}

export const agentDelivery = createDeliveryController({
  // A peer's message is held (queued; the drain retries) while the human has a draft in a
  // Claude agent's input box: its CR would submit the draft along with it. Read off the screen,
  // so only Claude's box counts — another runtime's doesn't parse ('unknown'), and there the
  // human's last 2 s of typing is the only evidence of one (it may not have echoed yet anywhere).
  // Not held where the tab's mod submits it around the draft. Never held for a busy agent.
  inject: (tabId, text, beforePaste) =>
    injectPrompt(tabId, text, beforePaste, async () => {
      const keys = terminalsStore.getLastTakeoverInputAt(tabId);
      if (keys !== undefined && Date.now() - keys < 2000) return false;
      // A dialog the hooks haven't reported yet (a permission Notification lags ~6 s): typed
      // into a busy agent, the paste's CR would approve it.
      if (await agentDialogOpen(tabId).catch(() => true)) return false;
      return !(workspacesStore.getTabRuntime(tabId) === 'claude' && (await agentInputBox(tabId).catch(() => 'unknown')) === 'has_text');
    }),
  liveState: (tabId) => !!claudeStateStore.getState(tabId),
  awaitingHuman: (tabId) => {
    const st = claudeStateStore.getState(tabId);
    return !!st && getAdapter(workspacesStore.getTabRuntime(tabId)).isAwaitingHumanInput(st);
  },
});
