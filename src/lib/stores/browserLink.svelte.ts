import { listen } from '@tauri-apps/api/event';
import { error as logError } from '@tauri-apps/plugin-log';
import * as commands from '$lib/tauri/commands';
import type { BrowserLinkView } from '$lib/tauri/commands';
import { workspacesStore } from '$lib/stores/workspaces.svelte';
import { dispatch } from '$lib/stores/notificationDispatch';

/** A link as the extension reported it when the human made it (`browser-link-linked`). */
interface LinkedEvent {
  maiterm_tab_id: string;
  handle: string;
  title: string;
  url: string;
}

/** Which tabs hold linked Chrome tabs (docs/browser-link.md), so the tab strip can show it: a link
 *  is made in Chrome, and nothing in maiTerm said so. Rust owns the links; this mirrors them on
 *  every `browser-link-changed`, and announces a link the human just made. */
function createBrowserLinkStore() {
  let links = $state<BrowserLinkView[]>([]);
  let started = false;

  async function load() {
    try {
      links = await commands.browserLinkLinks();
    } catch (e) {
      logError(`browserLink: could not read links: ${e}`);
    }
  }

  function inThisWindow(tabId: string): string | null {
    for (const ws of workspacesStore.workspaces)
      for (const pane of ws.panes) {
        const t = pane.tabs.find((x) => x.id === tabId);
        if (t) return t.name;
      }
    return null;
  }

  return {
    /** A tab's linked Chrome tabs, b1 first. */
    linksFor(tabId: string): BrowserLinkView[] {
      const num = (h: string) => Number(h.slice(1)) || 0;
      return links.filter((l) => l.maiterm_tab_id === tabId).sort((a, b) => num(a.handle) - num(b.handle));
    },

    init() {
      if (started) return;
      started = true;
      void load();
      void listen('browser-link-changed', () => void load());
      // Every window hears it; only the one holding the tab says so.
      void listen<LinkedEvent>('browser-link-linked', ({ payload: l }) => {
        const name = inThisWindow(l.maiterm_tab_id);
        if (name == null) return;
        const page = l.title || l.url;
        void dispatch('Linked to Chrome', `${name} can now use ${l.handle}: ${page}`, 'info', { tabId: l.maiterm_tab_id });
      });
    },
  };
}

export const browserLinkStore = createBrowserLinkStore();
