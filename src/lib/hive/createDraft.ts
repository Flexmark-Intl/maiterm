/**
 * The Create wizard's answers per workspace, kept when it closes so reopening it lands where the
 * human left off (docs/hive.md §3): leaving to deal with something, an Escape or a stray click
 * on the backdrop never throws away a typed goal and brief. In memory only: a restart starts
 * fresh. Cleared by Create and by Start over.
 */
const drafts = new Map<string, unknown>();

export const createDrafts = {
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
