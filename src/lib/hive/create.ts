/** Creating a Hive from the sheet (docs/hive.md §3): Rust lays it out and installs it, taking the
 *  shell it replaces out in the same write; the frontend mirrors that and mounts the coordinator,
 *  whose mount types the launch builder's line (lib/hive/launch.ts). */

import { hiveCreate } from '$lib/tauri/commands';
import type { HiveCreated, HiveSpec } from '$lib/tauri/types';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { info as logInfo } from '@tauri-apps/plugin-log';

export async function createHive(workspaceId: string, spec: HiveSpec): Promise<HiveCreated> {
  const created = await hiveCreate(workspaceId, spec);
  logInfo(`hive: created in workspace ${workspaceId.slice(0, 8)}, coordinator ${created.coordinator_tab_id.slice(0, 8)}`);
  workspacesStore.adoptHive(created.workspace, created.coordinator_tab_id, created.replace_tab?.tab_id ?? null);
  if (workspacesStore.activeWorkspaceId !== workspaceId) await workspacesStore.setActiveWorkspace(workspaceId);
  // A hidden tab is never a pane's active tab, so only this mounts it (as stack.svelte.ts does).
  window.dispatchEvent(new CustomEvent('activate-tab', { detail: created.coordinator_tab_id }));
  return created;
}
