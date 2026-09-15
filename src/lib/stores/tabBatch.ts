/**
 * Argument normalization and result shaping for the batched tab-lifecycle tools
 * (`archiveTab` / `closeTab` / `deleteArchivedTab` with `tab_ids`).
 *
 * Pure, and separate from `overlord.svelte.ts` for the same reason `meshRouting.ts` is
 * separate from `agentMesh.svelte.ts`: this is the part that is worth testing directly, and
 * the store it serves cannot be imported outside a Svelte runtime.
 *
 * Batching here is a TRANSPORT convenience, never a relaxation of anything. Cleanup arrives
 * as a list — a human pointing the Overlord at a window full of finished sessions — and one
 * MCP call per tab cost a model turn per tab. What it must not become is a way to retire a
 * tab that would have been refused on its own, so the store still runs every per-tab guard
 * and writes every per-tab ledger entry; this module only decides WHICH ids are in play and
 * how the outcomes are reported back.
 */

/** One tab's outcome inside a batched call. */
export interface TabBatchRow {
  tab_id: string;
  ok: boolean;
  reason?: string;
  detail?: string;
}

/**
 * The result of a batched tab-lifecycle call.
 *
 * `ok` is true only when EVERY row succeeded, so an agent that checks the top-level flag and
 * nothing else is never told a partial sweep went fine. The counts save it tallying `results`
 * itself to write an accurate ack.
 */
export interface TabBatchResult {
  ok: boolean;
  succeeded: number;
  refused: number;
  results: TabBatchRow[];
}

/**
 * Most tabs one batched call may name.
 *
 * A TIMEOUT limit, not a safety one — the per-tab guards are the safety. Each tab is retired
 * through a Tauri command that persists the whole workspace tree, and the MCP server abandons
 * a tool call after 120s, its clock starting before the webview even sees the call. A batch
 * that overruns leaves the agent holding a timeout while the work carries on behind it, which
 * is the one outcome worse than making it send two calls.
 */
export const TAB_BATCH_MAX = 50;

export function summarizeBatch(results: TabBatchRow[]): TabBatchResult {
  const succeeded = results.filter((r) => r.ok).length;
  return { ok: succeeded === results.length, succeeded, refused: results.length - succeeded, results };
}

/**
 * Turn a tool's `tab_id` / `tab_ids` arguments into the list to act on.
 *
 * Returns `{ error }` for anything the caller must fix before any work starts, and
 * `{ ids, batch }` otherwise — `batch` recording whether the agent asked for a list, because
 * that decides the shape of the reply (a scalar call keeps its original
 * `{ ok, reason, detail }`; a list gets `results`).
 *
 * Both forms may be present: they are concatenated rather than one silently winning, since
 * mixing them is exactly the argument mistake an agent makes and dropping either half would
 * skip a tab it believes it named.
 *
 * Duplicates are dropped rather than acted on twice — a second pass over an already-archived
 * tab comes back `not_boardable`, which reads as a refusal of work that in fact succeeded.
 */
export function normalizeTabBatch(
  tab_id: unknown,
  tab_ids: unknown,
): { error: string } | { ids: string[]; batch: boolean } {
  const batch = tab_ids !== undefined && tab_ids !== null;
  if (batch && (!Array.isArray(tab_ids) || tab_ids.some((t) => typeof t !== 'string'))) {
    return { error: 'tab_ids must be an array of tab id strings.' };
  }
  if (tab_id !== undefined && tab_id !== null && typeof tab_id !== 'string') {
    return { error: 'tab_id must be a tab id string.' };
  }
  const raw = [...(typeof tab_id === 'string' ? [tab_id] : []), ...(batch ? (tab_ids as string[]) : [])];
  const ids = [...new Set(raw.map((t) => t.trim()).filter(Boolean))];
  if (!ids.length) return { error: 'tab_id or tab_ids is required.' };
  if (ids.length > TAB_BATCH_MAX) {
    return { error: `Too many tabs in one call (${ids.length}); send at most ${TAB_BATCH_MAX} per call.` };
  }
  return { ids, batch };
}
