import type { EditorView } from '@codemirror/view';

export interface EditorRegistryEntry {
  view: EditorView;
  filePath: string;
  isDirty: boolean;
}

// Simple mutable map - not reactive (accessed by reference)
const registry = new Map<string, EditorRegistryEntry>();

// Reactive set of dirty tab IDs for UI (e.g. tab indicators)
let dirtyTabs = $state(new Set<string>());

export function registerEditor(tabId: string, view: EditorView, filePath: string): void {
  registry.set(tabId, { view, filePath, isDirty: false });
}

/** The tab's file was renamed or moved under it (Files): same buffer, new path. */
export function setEditorFilePath(tabId: string, filePath: string): void {
  const entry = registry.get(tabId);
  if (entry) entry.filePath = filePath;
}

export function unregisterEditor(tabId: string): void {
  registry.delete(tabId);
  if (dirtyTabs.has(tabId)) {
    dirtyTabs = new Set([...dirtyTabs].filter(id => id !== tabId));
  }
}

export function setEditorDirty(tabId: string, dirty: boolean): void {
  const entry = registry.get(tabId);
  if (entry) entry.isDirty = dirty;
  // Update reactive set
  if (dirty && !dirtyTabs.has(tabId)) {
    dirtyTabs = new Set(dirtyTabs).add(tabId);
  } else if (!dirty && dirtyTabs.has(tabId)) {
    dirtyTabs = new Set([...dirtyTabs].filter(id => id !== tabId));
  }
}

export function isEditorDirty(tabId: string): boolean {
  return dirtyTabs.has(tabId);
}

export function getEditorByTabId(tabId: string): EditorRegistryEntry | undefined {
  return registry.get(tabId);
}

export function getEditorByFilePath(filePath: string): { tabId: string; entry: EditorRegistryEntry } | undefined {
  for (const [tabId, entry] of registry) {
    if (entry.filePath === filePath) return { tabId, entry };
  }
  return undefined;
}

/** Every registered editor, for lookups that can't match on the stored path alone. */
export function listEditors(): { tabId: string; entry: EditorRegistryEntry }[] {
  return [...registry].map(([tabId, entry]) => ({ tabId, entry }));
}

/** Diagnostic snapshot for getDiagnostics. */
export function getEditorRegistrySizes() {
  return {
    registered: registry.size,
    dirty_tabs: dirtyTabs.size,
  };
}
