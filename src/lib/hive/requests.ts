/** The agent requests a Hive puts to the human: its land (src-tauri/src/hive/land/approval.rs,
 *  docs/hive.md §8.1, §8.3), its coordinator's settings change (§3.4) and media ask (`tool_setup`,
 *  docs/hive-media.md §8). Rust files them, carries an approval out itself and tells the
 *  coordinator (⟦HIVE LAND⟧, ⟦HIVE TOOLS⟧…), so a window never executes one, and never types the
 *  asker an outcome notice of its own: the coordinator would be told twice. */

import type { AgentRequest, HiveWorkerCard } from '$lib/tauri/types';

const HIVE_KINDS: ReadonlySet<string> = new Set(['hive_land', 'hive_undo', 'hive_settings', 'tool_setup']);

export const isHiveRequest = (r: Pick<AgentRequest, 'kind'>): boolean => HIVE_KINDS.has(r.kind);

/** Whether this window carries out an approved request: only one it filed, never a Hive's. */
export function windowCarriesOut(r: Pick<AgentRequest, 'kind' | 'window_label'>, me: string): boolean {
  return r.window_label === me && !isHiveRequest(r);
}

/** Whether the window tells the asking agent what became of its request. */
export const windowTellsAsker = (r: Pick<AgentRequest, 'kind'>): boolean => !isHiveRequest(r);

/** The pending card a worker's land waits on, if this window has it. */
export function landCard(pending: readonly AgentRequest[], card: Pick<HiveWorkerCard, 'land'>): AgentRequest | null {
  const id = card.land?.state === 'awaiting_human' ? card.land.request : null;
  return id ? (pending.find((r) => r.id === id) ?? null) : null;
}

/** Hive cards waiting in this window that it hasn't announced yet: Rust files them, so no tool
 *  handler here announces them as it does its own. Marks them announced. */
export function unannouncedHiveCards(pending: readonly AgentRequest[], announced: Set<string>): AgentRequest[] {
  const out = pending.filter((r) => isHiveRequest(r) && r.status === 'pending' && !announced.has(r.id));
  for (const r of out) announced.add(r.id);
  return out;
}
