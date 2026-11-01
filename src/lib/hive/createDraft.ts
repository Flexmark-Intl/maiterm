import type { HiveProposal } from './proposal';

/**
 * The Create wizard's answers per workspace, kept when it closes so reopening it lands where the
 * human left off (docs/hive.md §3): leaving to deal with something, an Escape or a stray click
 * on the backdrop never throws away a typed goal and brief. In memory only: a restart starts
 * fresh. Cleared by Create and by Start over.
 */
const drafts = new Map<string, unknown>();
/** An agent's proposal the sheet starts from on its next open, when there's no draft
 *  (`proposeHive`, docs/hive.md §3.7). Taken once: what the human then types is the draft. */
const seeds = new Map<string, HiveProposal>();

export const createDrafts = {
  seed(workspaceId: string, proposal: HiveProposal): void {
    seeds.set(workspaceId, proposal);
  },
  takeSeed(workspaceId: string): HiveProposal | undefined {
    const s = seeds.get(workspaceId);
    seeds.delete(workspaceId);
    return s;
  },
  get<T>(workspaceId: string): T | undefined {
    return drafts.get(workspaceId) as T | undefined;
  },
  set<T>(workspaceId: string, draft: T): void {
    drafts.set(workspaceId, draft);
  },
  clear(workspaceId: string): void {
    drafts.delete(workspaceId);
  },
};
