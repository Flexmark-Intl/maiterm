import type { Terminal } from '@xterm/xterm';
import { setTabScrollback, killTerminal, serializeTerminal, searchTerminal, clearTerminalScrollback, saveTerminalScrollback } from '$lib/tauri/commands';
import { error as logError } from '@tauri-apps/plugin-log';
import { getCompiledTitlePatterns } from '$lib/utils/promptPattern';
import { preferencesStore } from '$lib/stores/preferences.svelte';

/**
 * Per-terminal state collected from OSC escape sequences.
 *
 * Supported:
 *   OSC 0/2 — title (shown in tab)
 *   OSC 7   — cwd   (used for split cloning)
 *
 * Future candidates:
 *   OSC 9   — desktop notification when a command finishes
 *   OSC 133 — shell integration (prompt/command boundaries)
 */
export interface OscState {
  title: string | null;
  cwd: string | null;
  /** Hostname from the OSC 7 URL — used to distinguish local vs remote cwd. */
  cwdHost: string | null;
  /** Remote cwd extracted from title via prompt pattern matching. */
  promptCwd: string | null;
}

interface TerminalInstance {
  terminal: Terminal;
  ptyId: string;
  workspaceId: string;
  paneId: string;
  tabId: string;
  osc: OscState;
}

export interface SplitContext {
  cwd: string | null;
  sshCommand: string | null;
  remoteCwd: string | null;
  /** When true, fire auto-resume command even though this is a split context (used by reload tab). */
  fireAutoResume?: boolean;
}

function createTerminalsStore() {
  let instances = $state<Map<string, TerminalInstance>>(new Map());
  /** Bumped on register/unregister so external $derived can track instance set changes. */
  let instanceVersion = $state(0);
  let searchVisibleFor = $state<string | null>(null);
  let canvasTabs = $state(new Set<string>());
  let _shuttingDown = false;
  const splitContexts = new Map<string, SplitContext>();
  // PTY IDs that should NOT be killed on component destroy (e.g. tab moving between workspaces)
  const preservedPtyIds = new Set<string>();
  // Listeners notified when any terminal's OSC state changes
  const oscListeners = new Set<(tabId: string, osc: OscState) => void>();
  // Dirty tracking: tabs that have received PTY output since last auto-save.
  // Prevents pointless serialization of idle terminals, which creates large
  // temporary strings that pressure the GC (81 terminals × ~300KB = 24MB/cycle).
  const dirtyTabs = new Set<string>();
  // Tabs whose PTY is being spawned — treated as "active" by the tab grouping
  // logic so they don't flash into the suspended group before registration.
  let spawningTabs = $state(new Set<string>());

  function emitOscChange(tabId: string, osc: OscState) {
    for (const fn of oscListeners) fn(tabId, osc);
  }

  return {
    get instances() { return instances; },
    get shuttingDown() { return _shuttingDown; },
    get searchVisibleFor() { return searchVisibleFor; },
    isCanvasRenderer(tabId: string) { return canvasTabs.has(tabId); },
    markDirty(tabId: string) { dirtyTabs.add(tabId); },
    isDirty(tabId: string) { return dirtyTabs.has(tabId); },
    clearDirty(tabId: string) { dirtyTabs.delete(tabId); },
    markSpawning(tabId: string) { spawningTabs = new Set(spawningTabs).add(tabId); },
    isSpawning(tabId: string) { return spawningTabs.has(tabId); },
    canvasRendererLoaded(tabId: string) { canvasTabs = new Set(canvasTabs).add(tabId); },
    canvasRendererUnloaded(tabId: string) { const s = new Set(canvasTabs); s.delete(tabId); canvasTabs = s; },

    preservePty(ptyId: string) {
      preservedPtyIds.add(ptyId);
    },

    consumePreserve(ptyId: string): boolean {
      return preservedPtyIds.delete(ptyId);
    },

    setSplitContext(tabId: string, ctx: SplitContext) {
      splitContexts.set(tabId, ctx);
    },

    consumeSplitContext(tabId: string): SplitContext | undefined {
      const ctx = splitContexts.get(tabId);
      if (ctx) splitContexts.delete(tabId);
      return ctx;
    },

    hasSplitContext(tabId: string): boolean {
      return splitContexts.has(tabId);
    },

    register(
      tabId: string,
      terminal: Terminal,
      ptyId: string,
      workspaceId: string,
      paneId: string
    ) {
      instances = new Map(instances);
      instances.set(tabId, {
        terminal, ptyId,
        workspaceId, paneId, tabId,
        osc: { title: null, cwd: null, cwdHost: null, promptCwd: null },
      });
      if (spawningTabs.has(tabId)) {
        const s = new Set(spawningTabs);
        s.delete(tabId);
        spawningTabs = s;
      }
      instanceVersion++;
    },

    unregister(tabId: string) {
      instances = new Map(instances);
      instances.delete(tabId);
      instanceVersion++;
    },

    updateTabLocation(tabId: string, workspaceId: string, paneId: string) {
      const instance = instances.get(tabId);
      if (!instance) return;
      instance.workspaceId = workspaceId;
      instance.paneId = paneId;
      instances = new Map(instances);
    },

    get(tabId: string): TerminalInstance | undefined {
      return instances.get(tabId);
    },

    /** Reactive version counter — read this in $derived to track register/unregister. */
    get instanceVersion() { return instanceVersion; },

    /** Diagnostic snapshot of internal map/set sizes (for getDiagnostics). */
    getInternalSizes() {
      return {
        instances: instances.size,
        canvas_renderer_tabs: canvasTabs.size,
        spawning_tabs: spawningTabs.size,
        split_contexts: splitContexts.size,
        preserved_pty_ids: preservedPtyIds.size,
        osc_listeners: oscListeners.size,
        dirty_tabs: dirtyTabs.size,
      };
    },

    // --- OSC state ---

    updateOsc(tabId: string, patch: Partial<OscState>) {
      const instance = instances.get(tabId);
      if (!instance) return;
      // Auto-derive promptCwd from title via prompt patterns (with \p optional,
      // since terminal titles omit the prompt character)
      if (patch.title) {
        const patterns = getCompiledTitlePatterns(preferencesStore.promptPatterns);
        for (const re of patterns) {
          const m = patch.title.match(re);
          if (m?.[1]) { patch.promptCwd = m[1].trim(); break; }
        }
      }
      Object.assign(instance.osc, patch);
      emitOscChange(tabId, instance.osc);
    },

    getOsc(tabId: string): OscState | undefined {
      return instances.get(tabId)?.osc;
    },

    onOscChange(fn: (tabId: string, osc: OscState) => void): () => void {
      oscListeners.add(fn);
      return () => oscListeners.delete(fn);
    },

    // --- terminal actions ---

    focusTerminal(tabId: string) {
      const instance = instances.get(tabId);
      if (instance) {
        instance.terminal.focus();
      }
    },

    async clearTerminal(tabId: string) {
      const instance = instances.get(tabId);
      if (!instance) return;
      // Clear the Rust alacritty_terminal scrollback buffer.
      // The command emits a fresh frame so xterm.js updates automatically.
      try {
        await clearTerminalScrollback(instance.ptyId);
        // Persist the cleared state so auto-save doesn't re-save stale content
        await saveTerminalScrollback(instance.ptyId, instance.tabId);
      } catch { /* terminal may have been killed */ }
      dirtyTabs.delete(tabId);
    },

    showSearch(tabId: string) {
      searchVisibleFor = tabId;
    },

    hideSearch(tabId: string) {
      if (searchVisibleFor === tabId) {
        searchVisibleFor = null;
      }
      const instance = instances.get(tabId);
      if (instance) {
        instance.terminal.focus();
      }
    },

    toggleSearch(tabId: string) {
      if (searchVisibleFor === tabId) {
        this.hideSearch(tabId);
      } else {
        this.showSearch(tabId);
      }
    },

    async findNext(tabId: string, query: string) {
      const instance = instances.get(tabId);
      if (instance && query) {
        try {
          await searchTerminal(instance.ptyId, query, false);
        } catch { /* terminal may have been killed */ }
      }
    },

    async findPrevious(tabId: string, query: string) {
      const instance = instances.get(tabId);
      if (instance && query) {
        try {
          await searchTerminal(instance.ptyId, query, false);
        } catch { /* terminal may have been killed */ }
      }
    },

    async killAllTerminals(): Promise<void> {
      const ptyIds = [...instances.values()].map(i => i.ptyId);
      await Promise.allSettled(
        ptyIds.map(id => killTerminal(id).catch(e => logError(`killAll: ${id} failed: ${e}`)))
      );
    },

    async saveAllScrollback(): Promise<void> {
      _shuttingDown = true;

      // Serialize all terminals via Rust backend
      const saves: Promise<void>[] = [];
      for (const [tabId, instance] of instances) {
        saves.push(
          (async () => {
            try {
              await saveTerminalScrollback(instance.ptyId, instance.tabId);
            } catch (e) {
              logError(`saveAllScrollback: FAILED ${tabId} - ${e}`);
            }
          })()
        );
      }

      const results = await Promise.allSettled(saves);
      for (const r of results) {
        if (r.status === 'rejected') logError(`Failed to save scrollback: ${r.reason}`);
      }
    }
  };
}

export const terminalsStore = createTerminalsStore();
