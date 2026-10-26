/** Who holds each open worker prompt of this window's Hives (docs/hive.md §4.7, §7.4): Rust's
 *  routes, mirrored from `hive-prompts-changed` (and `hive_prompt_routes` when a Hive view
 *  mounts). The gate (`hiveOwnsPrompt`), "With coordinator" on the cards and Take over read it.
 *
 *  It also hears `hive-prompt-notify`, Rust's word that a prompt is the human's after all, and
 *  dispatches it as `hive-human`, which the gate never holds back; and `hive-settings-changed`,
 *  a settings change the coordinator made on its own (§3.4). Init'd once per window by
 *  `+layout`: a notice must reach the human wherever they are, not only in the Hive view. */

import { getCurrentWindow } from '@tauri-apps/api/window';
import { error as logError } from '@tauri-apps/plugin-log';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { countedListen } from '$lib/utils/listenCounter';
import { hivePromptRoutes, hiveTakeOver } from '$lib/tauri/commands';
import type { HivePromptNotice, HivePromptRoute } from '$lib/tauri/types';
import { workspacesStore } from './workspaces.svelte';
import { hiveOwnsPromptIn, type NotifyKind } from '$lib/hive/model';
import { applyRoutes, findBlockerRoute, findRoute, handleNotice, type RoutesByWorkspace } from '$lib/hive/prompts';

const DEFAULT_TIMEOUT_SECS = 300;

function createHivePromptsStore() {
  let byWs = $state<RoutesByWorkspace>(new Map());
  const unlisteners: UnlistenFn[] = [];
  let started = false;
  /** Routes events heard: a read that started before one is stale. Not reactive. */
  let events = 0;

  function timeoutFor(workspaceId: string): number {
    const hive = workspacesStore.workspaces.find((w) => w.id === workspaceId)?.hive;
    return hive?.settings?.autonomy?.prompt_timeout_secs ?? DEFAULT_TIMEOUT_SECS;
  }

  return {
    routesFor(workspaceId: string): readonly HivePromptRoute[] {
      return byWs.get(workspaceId) ?? [];
    },

    /** The route of a tab's open prompt, by its prompt id. */
    ownerOf(tabId: string, key: string): HivePromptRoute | null {
      return findRoute(byWs, tabId, key)?.route ?? null;
    },

    /** The route of a worker's question blocker. */
    blockerOwner(taskId: string, askedAt: string): HivePromptRoute | null {
      return findBlockerRoute(byWs, taskId, askedAt)?.route ?? null;
    },

    /** Re-read one Hive's routes (its view's mount; a webview reload). */
    async refresh(workspaceId: string) {
      const seen = events;
      try {
        const routes = await hivePromptRoutes(workspaceId);
        // An event heard meanwhile is newer than this read.
        if (events === seen) byWs = applyRoutes(byWs, workspaceId, routes);
      } catch (e) {
        logError(`hive: prompt routes of ${workspaceId.slice(0, 8)} not read: ${e}`);
      }
    },

    /** Take over (§7.4): that one prompt becomes the human's alone. Rust emits the new routes. */
    async takeOver(tabId: string, key: string): Promise<void> {
      const hit = findRoute(byWs, tabId, key);
      const workspaceId = hit?.workspaceId ?? workspacesStore.workspaces.find((w) => w.panes.some((p) => p.tabs.some((t) => t.id === tabId)))?.id;
      if (!workspaceId) throw new Error('That prompt is no longer waiting.');
      await hiveTakeOver(workspaceId, tabId, key);
    },

    async init() {
      if (started) return;
      started = true;
      const target = getCurrentWindow().label;
      try {
        // Both are sent to this window only (`emit_to`).
        unlisteners.push(
          await countedListen<{ workspace_id: string; routes: HivePromptRoute[] }>('hive-prompts-changed', (e) => {
            events++;
            byWs = applyRoutes(byWs, e.payload.workspace_id, e.payload.routes ?? []);
          }, { target }),
          await countedListen<HivePromptNotice>('hive-prompt-notify', (e) => {
            const n = e.payload;
            // Imported here, not at the top: notificationDispatch asks this store's gate.
            void import('./notificationDispatch').then(({ dispatch }) => handleNotice(n, timeoutFor(n.workspace_id), dispatch));
          }, { target }),
          // The coordinator changed the Hive's settings on its own (docs/hive.md §3.4).
          await countedListen<{ workspace_id: string; text: string }>('hive-settings-changed', (e) => {
            const ws = workspacesStore.workspaces.find((w) => w.id === e.payload.workspace_id);
            const coord = ws?.panes.flatMap((p) => p.tabs).find((t) => t.hive_role === 'coordinator');
            void import('./notificationDispatch').then(({ dispatch }) =>
              dispatch(`Hive settings changed: ${ws?.name ?? 'a Hive'}`, e.payload.text, 'info', coord ? { tabId: coord.id } : undefined),
            );
          }, { target }),
        );
      } catch (e) {
        logError(`hive: prompt listeners not set up: ${e}`);
      }
      for (const ws of workspacesStore.workspaces) if (ws.hive) void this.refresh(ws.id);
    },

    destroy() {
      for (const u of unlisteners.splice(0)) u();
      started = false;
    },
  };
}

export const hivePrompts = createHivePromptsStore();

/** `hiveOwnsPromptIn` over this window's workspaces and routes: asked by
 *  `notificationDispatch.dispatch` for every notice with a `kind`. */
export function hiveOwnsPrompt(tabId: string | null | undefined, kind: NotifyKind): boolean {
  if (!tabId) return false;
  for (const ws of workspacesStore.workspaces) {
    if (!ws.hive) continue;
    const tab = ws.panes.flatMap((p) => p.tabs).find((t) => t.id === tabId);
    if (tab) return hiveOwnsPromptIn(ws.hive, tab, kind, hivePrompts.routesFor(ws.id));
  }
  return false;
}
