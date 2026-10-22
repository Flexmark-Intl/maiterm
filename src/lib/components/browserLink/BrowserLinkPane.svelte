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
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import { isMac } from '$lib/utils/platform';

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

  // Chrome installs an unpacked extension only by hand, so the card is the steps themselves; these
  // run the parts maiTerm can. Each refreshes the folder first, so step 3 always finds it current.
  let openFailed = $state(false);
  let copied = $state(false);

  async function prepare(): Promise<string | null> {
    feedback = null;
    try {
      return await commands.browserLinkPrepareUnpacked();
    } catch (e) {
      feedback = { text: String(e), error: true };
      return null;
    }
  }

  async function openExtensionsPage() {
    busy = true;
    if (await prepare()) openFailed = !(await commands.browserLinkOpenExtensionsPage());
    busy = false;
  }

  async function copyPath() {
    const dir = await prepare();
    if (!dir) return;
    try {
      await clipboardWriteText(dir);
      copied = true;
      setTimeout(() => (copied = false), 2000);
    } catch (e) {
      feedback = { text: String(e), error: true };
    }
  }

  async function showFolder() {
    const dir = await prepare();
    if (!dir) return;
    try {
      await commands.revealInFileManager(dir);
    } catch (e) {
      feedback = { text: String(e), error: true };
    }
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
      {#if !status.installed_in.length}
        <p class="hint">
          No Chromium browser found. Open Chrome once, then turn this off and on again.
        </p>
      {:else}
        {#snippet steps(s: BrowserLinkStatus)}
          <p class="hint">
            It isn't in the Chrome Web Store yet, and Chrome lets no app install an extension for
            you, so you add it by hand from a folder maiTerm keeps up to date. Once per browser
            ({s.installed_in.join(', ')}); updates arrive with maiTerm.
          </p>
          <ol class="steps">
            <li>
              Open Chrome's extensions page.
              <div class="actions">
                <Button onclick={openExtensionsPage} disabled={busy}>Open in Chrome</Button>
              </div>
              {#if openFailed}
                <p class="hint">Chrome didn't open. In your browser, go to <code>chrome://extensions</code>.</p>
              {/if}
            </li>
            <li>Turn on <strong>Developer mode</strong>, the switch at the top right of that page.</li>
            <li>
              Click <strong>Load unpacked</strong> and choose this folder:
              {#if s.extension_dir}
                <code class="path">{s.extension_dir}</code>
              {/if}
              <div class="actions">
                <Button variant="ghost" onclick={copyPath}>{copied ? 'Copied' : 'Copy path'}</Button>
                <Button variant="ghost" onclick={showFolder}>Show in {isMac() ? 'Finder' : 'folder'}</Button>
              </div>
              {#if isMac() && s.extension_dir?.includes('/Library/')}
                <p class="hint">
                  The folder is inside your hidden Library. In Chrome's file picker, press ⌘⇧G, paste
                  the path, press Return, then click Select.
                </p>
              {/if}
            </li>
            <li>
              Pin it: click the puzzle-piece icon in Chrome's toolbar and pin <strong>maiTerm</strong>.
              That icon is where you link a tab.
            </li>
          </ol>
          <p class="hint">
            Chrome then shows under Connected browsers within 30 seconds. If it doesn't, quit and
            reopen Chrome.
          </p>
        {/snippet}
        {#if status.connections.length}
          <p class="hint">Installed and connected.</p>
          <details>
            <summary class="hint">Install it in another browser</summary>
            {@render steps(status)}
          </details>
        {:else}
          {@render steps(status)}
        {/if}
      {/if}
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
    display: block;
    background: var(--bg-dark);
    border-radius: 4px;
    font-size: 0.8rem;
    overflow-wrap: anywhere;
    padding: 6px 8px;
    user-select: all;
  }

  .steps {
    color: var(--fg);
    display: flex;
    flex-direction: column;
    font-size: 0.85rem;
    gap: 10px;
    line-height: 1.5;
    margin: 0;
    padding-left: 20px;
  }

  .steps li {
    list-style: decimal;
  }

  .steps li > * + * {
    margin-top: 6px;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  summary {
    cursor: pointer;
    margin-bottom: 8px;
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
