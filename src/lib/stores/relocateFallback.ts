/**
 * Tabs that spawned in home because their saved folder was missing → the folder they wanted
 * (docs/relocate.md §4). Its own module, importing nothing, so `tauri/commands.ts` can consult
 * it: every command that saves a tab's cwd (suspend, archive, the periodic restore context)
 * saves the WANTED folder instead of home while a tab is here — otherwise any one of them would
 * erase the folder the tab is waiting to go back to. In memory: a restart re-detects.
 */
const fallback = new Map<string, string>();

export function setFallback(tabId: string, wanted: string) {
  fallback.set(tabId, wanted);
}

export function clearFallback(tabId: string) {
  fallback.delete(tabId);
}

export function wantedFolder(tabId: string): string | undefined {
  return fallback.get(tabId);
}

/** The cwd to save for a tab: the folder it wanted while it is a fallback, else `cwd`. */
export function savedCwd(tabId: string, cwd: string | null): string | null {
  return fallback.get(tabId) ?? cwd;
}
