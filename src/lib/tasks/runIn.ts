/**
 * The tab an action card's command is FOR (docs/tasks.md §3.1.3), judged when it is USED.
 *
 * `blocker.run_in` is a tab id stored at filing, and the task it sits on can change hands or
 * be copied (a claim, a release and reclaim, a duplicate, an import), and tab ids are re-minted
 * by reloads. Keeping it right along every such path was tried and missed one path per review.
 * So the rule the human relies on is checked where it matters: the tab is still a peer (mesh
 * peer or live bridge partner) of the task's CURRENT carrier, the agent Run will tell. Anything
 * else is `stale`, and Run, the card's label and the second corner treat it as such.
 *
 * "The current carrier" is deliberate: an agent that takes over a blocked task takes over its
 * question, as with every blocker (the answer is typed to the new carrier), and so this one's
 * outcome too. The card's "For …" line stays true either way.
 */
import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
import type { Task } from '$lib/tauri/types';

export type RunInTarget = { ok: true; tabId: string } | { ok: false; tabId: string } | null;

/** Whether `target` is a tab `asker` can message: a mesh peer, or its live bridge partner. */
export function isPeerOf(asker: string, target: string): boolean {
  if (asker === target) return false;
  if (agentMeshStore.areMeshPeers(asker, target)) return true;
  return agentBridgeStore.isBridgedToLivePartner(asker) && agentBridgeStore.getPartnerTabId(asker) === target;
}

/** null: the command is for the asker's own tab. `ok: false`: it names a tab that is no
 *  longer a peer of whoever carries the task now (or nobody carries it). */
export function runInTarget(task: Pick<Task, 'tab_id' | 'blocker'>): RunInTarget {
  const target = task.blocker?.run_in;
  if (!target || target === task.tab_id) return null;
  return { ok: !!task.tab_id && isPeerOf(task.tab_id, target), tabId: target };
}
