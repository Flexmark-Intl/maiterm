<script lang="ts">
  /** Preferences → Chrome extension (docs/browser-link.md §5).
   *
   *  The toggle writes the native-messaging host manifests (and lists the browser tools to
   *  agents); the extension is loaded unpacked until it has a store listing. Links are made only
   *  in the extension's popup — this pane shows them and can break them. Everything it draws is
   *  Rust-owned, redrawn on `browser-link-changed`. */
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import Button from '$lib/components/ui/Button.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { BrowserLinkStatus } from '$lib/tauri/commands';
  import { preferencesStore } from '$lib/stores/preferences.svelte';

  let status = $state<BrowserLinkStatus | null>(null);
  let feedback = $state<{ text: string; error: boolean } | null>(null);
  let busy = $state(false);

  async function load() {
    try {
      status = await commands.browserLinkStatus();
    } catch (e) {
      feedback = { text: String(e), error: true };
    }
  }

  onMount(() => {
    void load();
    const off = listen('browser-link-changed', () => void load());
    return () => void off.then((f) => f());
  });

  async function toggle() {
    busy = true;
    feedback = null;
    try {
      await preferencesStore.setBrowserLinkEnabled(!preferencesStore.browserLinkEnabled);
      await commands.browserLinkSync();
      await load();
    } catch (e) {
      feedback = { text: String(e), error: true };
    }
    busy = false;
  }

  async function loadUnpacked() {
    busy = true;
    feedback = null;
    try {
      const dir = await commands.browserLinkPrepareUnpacked();
      await commands.revealInFileManager(dir);
      const opened = await commands.browserLinkOpenExtensionsPage();
      feedback = {
        text: opened
          ? 'Chrome is open on its extensions page. Turn on Developer mode (top right), click “Load unpacked”, and choose the folder shown in Finder.'
          : 'Open chrome://extensions in Chrome, turn on Developer mode, click “Load unpacked”, and choose the folder shown.',
        error: false,
      };
    } catch (e) {
      feedback = { text: String(e), error: true };
    }
    busy = false;
  }

  async function unlink(tabId: string) {
    await commands.browserLinkUnlink(tabId);
    await load();
  }
</script>

<div class="pane">
  <h3>Chrome extension</h3>
  <p class="hint">
    Link a tab in your own Chrome (or Brave, Edge, Arc) to one of your agent tabs, and that agent
    can diagnose it (console errors, network requests and their responses) and drive it (read the
    page, click, type, navigate), signed in as you, even while you look at another tab. Only you can
    link a tab, from the extension’s icon in the browser, and an agent can use only the tab linked
    to it. Chrome shows a debugging bar while one works; cancelling that bar unlinks the tab.
  </p>

  <div class="row">
    <div>
      <div class="label">Let agents diagnose and drive linked browser tabs</div>
      <p class="hint">Gives agents the browser tools and lets the extension find maiTerm.</p>
    </div>
    <button
      class="toggle"
      class:active={preferencesStore.browserLinkEnabled}
      onclick={toggle}
      disabled={busy}
      aria-pressed={preferencesStore.browserLinkEnabled}
      aria-label="Toggle the browser link"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>

  {#if preferencesStore.browserLinkEnabled && status}
    <div class="card">
      <div class="label">Install the extension</div>
      <p class="hint">
        Not in the Chrome Web Store yet, so it is loaded from a folder maiTerm keeps up to date.
        You do this once per browser.
      </p>
      {#if status.extension_dir}
        <code class="path">{status.extension_dir}</code>
      {/if}
      <div>
        <Button onclick={loadUnpacked} disabled={busy}>Load unpacked…</Button>
      </div>
      <p class="hint">
        {#if status.installed_in.length}
          Set up for {status.installed_in.join(', ')}.
        {:else}
          No Chromium browser found. Open Chrome once, then turn this off and on again.
        {/if}
      </p>
    </div>

    <div class="card">
      <div class="label">Connected browsers</div>
      {#if status.connections.length}
        {#each status.connections as c}
          <div class="line"><StatusDot color="green" /> {c.browser} · extension {c.version}</div>
        {/each}
      {:else}
        <div class="line"><StatusDot color="dim" /> None. Once the extension is loaded it connects within 30 seconds.</div>
      {/if}
    </div>

    <div class="card">
      <div class="label">Linked tabs</div>
      {#if status.links.length}
        {#each status.links as l (l.maiterm_tab_id)}
          <div class="link">
            <div class="link-text">
              <div class="name">{l.tab_name ?? 'A closed tab'}</div>
              <div class="hint">{l.browser}: {l.title || l.url}</div>
            </div>
            <Button variant="ghost" onclick={() => unlink(l.maiterm_tab_id)}>Unlink</Button>
          </div>
        {/each}
      {:else}
        <p class="hint">
          None. In the browser, open the page, click the maiTerm icon in the toolbar, and choose the
          agent tab.
        </p>
      {/if}
    </div>
  {/if}

  {#if feedback}
    <p class="hint" class:error={feedback.error}>{feedback.text}</p>
  {/if}
</div>

<style>
  .pane {
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  h3 {
    color: var(--fg);
    font-size: 1rem;
    margin: 0;
  }

  .hint {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .hint.error {
    color: var(--red, #f7768e);
  }

  .row {
    align-items: flex-start;
    display: flex;
    gap: 16px;
    justify-content: space-between;
  }

  .label {
    color: var(--fg);
    font-weight: 600;
  }

  .card {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px 14px;
  }

  .card .label {
    color: var(--fg-dim);
    font-size: 0.75rem;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }

  .path {
    background: var(--bg-dark);
    border-radius: 4px;
    font-size: 0.8rem;
    overflow-wrap: anywhere;
    padding: 6px 8px;
    user-select: all;
  }

  .line {
    align-items: center;
    color: var(--fg);
    display: flex;
    font-size: 0.85rem;
    gap: 8px;
  }

  .link {
    align-items: center;
    display: flex;
    gap: 12px;
    justify-content: space-between;
  }

  .link-text {
    min-width: 0;
  }

  .name {
    color: var(--fg);
    overflow-wrap: anywhere;
  }

  .toggle {
    background: var(--bg-light);
    border: none;
    border-radius: 11px;
    cursor: pointer;
    flex: none;
    height: 22px;
    position: relative;
    transition: background-color 0.2s;
    width: 40px;
  }

  .toggle.active {
    background: var(--accent);
  }

  .toggle-knob {
    background: white;
    border-radius: 50%;
    height: 18px;
    left: 2px;
    position: absolute;
    top: 2px;
    transition: transform 0.2s;
    width: 18px;
  }

  .toggle.active .toggle-knob {
    transform: translateX(18px);
  }
</style>
