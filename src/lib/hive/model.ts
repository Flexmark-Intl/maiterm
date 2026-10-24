/** Hive decisions with no store behind them (docs/hive.md), so tests and stores share them. */

type TabFields = { id: string; service_id?: string | null; hive_role?: string | null; overlord_exempt?: boolean };
type WorkspaceFields = { id: string; overlord_exempt?: boolean };

/** What "go to this tab" means for it (docs/hive.md §5, "Opening a Hive tab"). A hidden tab is
 *  never `setActiveTab`'d: a service shows its console, a Hive agent its column in the Hive view. */
export type OpenTarget =
  | { kind: 'hive'; workspaceId: string; tabId: string }
  | { kind: 'service'; workspaceId: string; serviceId: string }
  | { kind: 'tab'; workspaceId: string; tabId: string };

export function openTargetFor(tab: TabFields, ws: WorkspaceFields): OpenTarget {
  if (tab.hive_role) return { kind: 'hive', workspaceId: ws.id, tabId: tab.id };
  if (tab.service_id) return { kind: 'service', workspaceId: ws.id, serviceId: tab.service_id };
  return { kind: 'tab', workspaceId: ws.id, tabId: tab.id };
}

/** The Overlord engine leaves this tab alone: the human exempted it, or a Hive's coordinator
 *  already supervises it (§7.3). A Hive is never made `overlord_exempt`, which switches off far
 *  more than the engine. */
export function engineSkips(tab: TabFields, ws: WorkspaceFields): boolean {
  return !!tab.overlord_exempt || !!ws.overlord_exempt || !!tab.hive_role;
}

/** Why an Overlord tool refuses a Hive tab (reason `'hive'`). */
export const HIVE_SUPERVISED_DETAIL =
  "That tab is a Hive's agent: a Hive's coordinator supervises this tab. Leave it alone: nothing here will drive, recover, answer, resume or close it.";

/** Asks the Hive view (build step 3d) to open or focus a Hive agent's column. */
export const HIVE_OPEN_COLUMN = 'hive-open-column';

export interface HiveOpenColumnDetail {
  workspaceId: string;
  tabId: string;
  /** The waiting item to scroll to, as `waitingStore.open` takes it. */
  focus?: string | null;
}

export function dispatchHiveOpenColumn(detail: HiveOpenColumnDetail): void {
  if (typeof window === 'undefined') return;
  window.dispatchEvent(new CustomEvent<HiveOpenColumnDetail>(HIVE_OPEN_COLUMN, { detail }));
}
