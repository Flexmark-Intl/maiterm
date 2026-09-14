import { terminalsStore } from '$lib/stores/terminals.svelte';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { getAdapter } from '$lib/agents/adapter';
import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
import { createDeliveryController } from '$lib/stores/agentDelivery';
import { error as logError } from '@tauri-apps/plugin-log';

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
 *  notice). */
export async function injectPrompt(tabId: string, text: string): Promise<boolean> {
  const inst = terminalsStore.get(tabId);
  if (!inst) {
    logError(`agentDelivery: cannot inject — no terminal instance for tab ${tabId.slice(0, 8)}`);
    return false;
  }
  try {
    await bracketedPasteSubmit(inst.ptyId, text);
    return true;
  } catch (e) {
    logError(`agentDelivery: inject failed for tab ${tabId.slice(0, 8)}: ${e}`);
    return false;
  }
}

export const agentDelivery = createDeliveryController({
  inject: injectPrompt,
  liveState: (tabId) => !!claudeStateStore.getState(tabId),
  awaitingHuman: (tabId) => {
    const st = claudeStateStore.getState(tabId);
    return !!st && getAdapter(workspacesStore.getTabRuntime(tabId)).isAwaitingHumanInput(st);
  },
});
