<script lang="ts">
  /** Sign-in dialog for managed accounts (docs/login.md §10).
   *
   *  **Two modes, because they answer different questions.** `setup` runs once, before the
   *  feature is enabled, and has to TEACH before it authenticates — §8's losses are real and a
   *  user who would rather keep Remote Control should be able to decline before anything is
   *  created. `add` runs every time after that, when all of that has already been read and
   *  agreed to; repeating it there is noise that hides the one thing that IS new each time —
   *  the browser is already signed in to the account you just added, so a second sign-in
   *  silently returns the first one (§5.1).
   *
   *  Follows ServiceModal — same backdrop/panel/btn vocabulary, same explicit rAF focus
   *  (Svelte's `autofocus` is not focus; it no-ops when a keyboard opened the dialog). */
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type {
    AccountRuntimeInfo,
    NewAccount,
    AccountLoginUrl,
    PrivateBrowserInfo,
  } from '$lib/tauri/commands';

  interface Props {
    runtimes: AccountRuntimeInfo[];
    /** `setup` the first time — full disclosure, and finishing turns the feature on. `add`
     *  afterwards: the disclosure has been read, so this is about the browser session. */
    mode: 'setup' | 'add';
    /** Labels of the accounts already held, for the §5.1 warning. Empty in `setup`. */
    existing?: string[];
    /** Called with the account the sign-in produced. The parent owns the duplicate check and
     *  persistence — it is the side that knows the existing account list (§5.1). */
    oncomplete: (account: NewAccount, runtime: string) => Promise<void>;
    oncancel: () => void;
  }

  let { runtimes, mode, existing = [], oncomplete, oncancel }: Props = $props();

  let phase = $state<'explain' | 'signing-in' | 'saving'>('explain');
  let error = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  const supported = $derived(runtimes.filter(r => r.supported));
  const unsupported = $derived(runtimes.filter(r => !r.supported));
  let runtime = $state('claude');

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.focus());
    return () => cancelAnimationFrame(id);
  });

  const busy = $derived(phase !== 'explain');

  /** The id of the sign-in in flight. Minted here rather than in Rust so Cancel has something
   *  to name while `beginAccountLogin` is still outstanding. */
  let pendingId = $state<string | null>(null);
  /** The authorization URL, once the runtime prints it. Not credential material — it is the
   *  START of an OAuth flow, and whoever opens it still has to authenticate. */
  let loginUrl = $state<string | null>(null);
  /** What to do with the URL the moment it exists, rather than making the user click again
   *  after the default browser has already stolen focus. Chosen by which button started the
   *  sign-in; `none` is the plain path, which still gets the link box to act on by hand. */
  let onUrl = $state<'none' | 'copy' | 'private'>('none');
  let copied = $state(false);
  let openedPrivately = $state(false);

  /** Browsers that can be told to open a private window. **Empty is a normal answer** — Safari
   *  has no such switch — so nothing here may be the only way through; the link and its Copy
   *  button are always offered. */
  let browsers = $state<PrivateBrowserInfo[]>([]);
  let browserId = $state<string | null>(null);
  const browser = $derived(browsers.find(b => b.id === browserId) ?? browsers[0] ?? null);

  $effect(() => {
    void (async () => {
      try {
        browsers = await commands.listPrivateBrowsers();
        browserId ??= browsers[0]?.id ?? null;
      } catch (e) {
        // Not an error the user needs: it only costs them the shortcut, and the link is still
        // right there. Surfacing it would put a red box on a dialog that works fine.
        console.warn('listing private browsers failed', e);
      }
    })();
  });

  // One listener for the whole modal, not one per attempt: a listener registered inside signIn()
  // would have to be torn down on every exit path, and the one that matters (an error thrown
  // between registering and awaiting) is the easiest to miss.
  $effect(() => {
    let unlisten: UnlistenFn | null = null;
    let dead = false;
    void (async () => {
      const fn = await listen<AccountLoginUrl>(commands.ACCOUNT_LOGIN_URL_EVENT, e => {
        // Scoped to OUR attempt. The event broadcasts to every window, and a second preferences
        // window running its own sign-in would otherwise overwrite this one's link.
        if (e.payload.account_id !== pendingId) return;
        loginUrl = e.payload.url;
        // Only the clipboard is handled here — Rust does the opening, since it is holding the
        // URL that actually completes. Report what it DID, not what it was asked to do.
        openedPrivately = e.payload.opened && onUrl === 'private';
        if (e.payload.open_error) error = e.payload.open_error;
        if (onUrl === 'copy') void copyLink();
      });
      if (dead) void fn();
      else unlisten = fn;
    })();
    return () => {
      dead = true;
      void unlisten?.();
    };
  });

  async function copyLink() {
    if (!loginUrl) return;
    try {
      await clipboardWriteText(loginUrl);
      copied = true;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function openPrivately() {
    if (!loginUrl || !browser) return;
    try {
      await commands.openPrivateWindow(browser.id, loginUrl);
      openedPrivately = true;
    } catch (e) {
      // Report it, but the sign-in is still running and the link is still on screen — this is a
      // shortcut that failed, not the attempt.
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function cancelSignIn() {
    if (!pendingId) return;
    try {
      await commands.cancelAccountLogin(pendingId);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function signIn(then: 'none' | 'copy' | 'private') {
    error = null;
    loginUrl = null;
    copied = false;
    openedPrivately = false;
    onUrl = then;
    phase = 'signing-in';
    const id = crypto.randomUUID();
    pendingId = id;
    try {
      // Rust opens the link, because only Rust can see the URL that works — the runtime prints
      // a different one, pointing at a hosted page that asks for a code to paste.
      const account = await commands.beginAccountLogin(runtime, id, {
        openWith:
          then === 'private' ? (browser?.id ?? 'default') : then === 'copy' ? undefined : 'default',
      });
      // The browser took focus to authorize and does not give it back — this window ends up
      // behind the main one, which reads as "preferences closed itself". Whatever the outcome,
      // the next thing to look at is in here: the new account, or the error explaining why
      // there isn't one.
      await focusThisWindow();
      phase = 'saving';
      await oncomplete(account, runtime);
    } catch (e) {
      await focusThisWindow();
      error = e instanceof Error ? e.message : String(e);
      phase = 'explain';
    } finally {
      pendingId = null;
      onUrl = 'none';
    }
  }

  async function focusThisWindow() {
    try {
      const win = getCurrentWindow();
      // Unminimize first: setFocus alone does not restore a minimized window.
      if (await win.isMinimized()) await win.unminimize();
      await win.setFocus();
    } catch {
      // Focus is a courtesy. Never let it swallow the sign-in result.
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    const closeKey = e.key === 'Escape' || (e.key.toLowerCase() === 'w' && (e.metaKey || e.ctrlKey));
    if (!closeKey) return;
    // Swallow these WHILE BUSY as well as when idle. The preferences window closes itself on
    // Escape and Cmd+W, and the sign-in child process does not stop when it does — pressing
    // Escape at "Waiting for browser…" (the reflex, because Cancel is greyed out) closed the
    // window and left a root behind that later completed a real login and was persisted
    // nowhere. Not cancellable yet, so the least it can do is not disappear.
    e.stopPropagation();
    if (busy) {
      e.preventDefault();
      return;
    }
    oncancel();
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget && !busy) oncancel();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="backdrop"
  onclick={handleBackdropClick}
  onkeydown={handleKeydown}
  role="dialog"
  aria-modal="true"
  aria-label={mode === 'setup' ? 'Set up managed accounts' : 'Add an account'}
  tabindex="-1"
>
  <div class="panel" bind:this={panelEl} tabindex="-1">
    <div class="header">
      <div class="title">{mode === 'setup' ? 'Set up managed accounts' : 'Add an account'}</div>
      <div class="subtitle">
        {#if mode === 'setup'}
          maiTerm holds your agent logins, switches between them per tab, and keeps track of
          when they expire.
        {:else}
          Sign in to another account. It joins the list and can be made active per runtime.
        {/if}
      </div>
    </div>

    <div class="body">
      {#if mode === 'setup'}
        <!-- The five disclosure sections live on the Accounts pane behind this dialog, not
             here. A modal is read once, under pressure to get past it, and is then unreachable;
             the pane shows the same text before setup AND afterwards, which is when the
             questions it answers actually get asked. -->
        <section>
          <p>
            Signing in creates a config directory for this account. maiTerm never reads or
            parses the login itself — the runtime owns the credential, maiTerm owns the
            directory. <strong>What this does and does not do</strong> is on the Accounts pane
            behind this dialog.
          </p>
        </section>
      {:else}
        <section>
          <h4>Your browser is already signed in</h4>
          <p>
            {#if existing.length}
              You already hold <strong>{existing.join(', ')}</strong>, and your browser is very
              likely still signed in there.
            {/if}
            The provider reuses that session, so a second sign-in usually returns the same account
            without asking — maiTerm will spot the duplicate and refuse it rather than adding
            a row that is a copy of one you have.
          </p>
          <p>Two ways through:</p>
          <ul>
            {#if browser}
              <li>
                <strong>Sign in privately</strong> below opens the link in a new private
                {browser.label} window, which has no session to reuse. Your normal browser is not
                opened at all, so there is no signed-in tab to click by mistake.
              </li>
            {:else}
              <li>
                <strong>Sign in and copy link</strong> below puts the sign-in link on your
                clipboard and opens nothing; paste it into a private/incognito window, which has
                no session to reuse.
              </li>
            {/if}
            <li>Or sign out of the provider in your browser first, then sign in here.</li>
          </ul>
        </section>
      {/if}

      <section>
        <h4>Sign in</h4>
        {#if supported.length > 1}
          <label class="field">
            <span class="label">Runtime</span>
            <select bind:value={runtime} disabled={busy}>
              {#each supported as r (r.slug)}
                <option value={r.slug}>{r.label}</option>
              {/each}
            </select>
          </label>
        {:else if supported.length === 1}
          <p>Signing in to <strong>{supported[0].label}</strong>.</p>
        {/if}
        {#if unsupported.length}
          <p class="hint">
            Not yet available: {unsupported.map(r => r.label).join(', ')}.
          </p>
        {/if}
        {#if browsers.length > 1}
          <!-- Segmented rather than a dropdown: there are two or three of these, they are all
               worth seeing at once, and picking one is a single click instead of two. -->
          <div class="field">
            <span class="label" id="private-browser-label">Private window</span>
            <div class="segments" role="radiogroup" aria-labelledby="private-browser-label">
              {#each browsers as b (b.id)}
                <button
                  class="segment"
                  class:selected={browser?.id === b.id}
                  role="radio"
                  aria-checked={browser?.id === b.id}
                  disabled={busy}
                  onclick={() => (browserId = b.id)}
                >{b.label}</button>
              {/each}
            </div>
          </div>
        {/if}
        {#if mode === 'setup'}
          <p class="hint">
            Your browser will open. If you are already signed in to a different account there,
            sign out first or use a private window — otherwise it will return the account you
            already have, and maiTerm will tell you so rather than adding a duplicate.
          </p>
        {/if}
      </section>

      {#if phase === 'signing-in'}
        <!-- Shown for BOTH buttons, not just the copy one: someone who clicked plain "Sign in"
             and only then noticed the wrong account in the browser needs the link too, and the
             link is dead once this attempt times out. -->
        <section class="link-box">
          {#if loginUrl}
            <h4>
              {#if openedPrivately}Opened in {browser?.label ?? 'a private window'}
              {:else if copied}Link copied
              {:else}Sign-in link{/if}
            </h4>
            <p class="hint">
              {#if openedPrivately}
                Finish signing in there. If that window was not private, close it and use Copy.
              {:else if copied}
                On your clipboard. Nothing else was opened — paste it into a private/incognito
                window to finish as a different account.
              {:else}
                Open this in a private/incognito window to sign in as a different account.
              {/if}
            </p>
            <div class="link-row">
              <code class="url">{loginUrl}</code>
              {#if browser}
                <Button variant="secondary" onclick={openPrivately}>Open in {browser.label}</Button>
              {/if}
              <Button variant="ghost" onclick={copyLink}>{copied ? 'Copy again' : 'Copy'}</Button>
            </div>
          {:else}
            <p class="hint">Waiting for the runtime to produce a sign-in link…</p>
          {/if}
        </section>
      {/if}

      {#if error}
        <p class="error">{error}</p>
      {/if}
    </div>

    <div class="footer">
      {#if phase === 'signing-in'}
        <!-- Enabled DURING the sign-in: a greyed-out Cancel is what sent people to Escape,
             which used to close the window and leave the login running. -->
        <Button variant="ghost" onclick={cancelSignIn}>Cancel sign-in</Button>
      {:else}
        <Button variant="ghost" onclick={oncancel} disabled={busy}>Cancel</Button>
        <!-- The second-account path, one click. Which one it is depends on what this machine
             can actually do: a private window when a browser supports being told to open one,
             the clipboard otherwise. Both end up in the same place. -->
        {#if browser}
          <Button
            variant="secondary"
            onclick={() => signIn('private')}
            disabled={busy || supported.length === 0}
          >Sign in privately</Button>
        {:else}
          <Button
            variant="secondary"
            onclick={() => signIn('copy')}
            disabled={busy || supported.length === 0}
          >Sign in and copy link</Button>
        {/if}
      {/if}
      <Button variant="primary" onclick={() => signIn('none')} disabled={busy || supported.length === 0}>
        {#if phase === 'signing-in'}Waiting for browser…
        {:else if phase === 'saving'}Saving…
        {:else if mode === 'setup'}Sign in and enable
        {:else}Sign in{/if}
      </Button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 10vh;
    z-index: 1000;
  }

  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    max-height: 78vh;
    width: 520px;
  }

  .header {
    border-bottom: 1px solid var(--bg-light);
    padding: 12px 14px 10px;
  }

  .title {
    color: var(--fg);
    font-size: 1rem;
    font-weight: 600;
  }

  .subtitle {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.4;
    margin-top: 3px;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    overflow-y: auto;
    padding: 14px;
  }

  h4 {
    color: var(--fg);
    font-size: 0.85rem;
    font-weight: 600;
    margin: 0 0 4px;
  }

  p {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0 0 6px;
    overflow-wrap: anywhere;
  }

  p:last-child {
    margin-bottom: 0;
  }

  ul {
    color: var(--fg-dim);
    display: flex;
    flex-direction: column;
    font-size: 0.8rem;
    gap: 5px;
    line-height: 1.5;
    margin: 0;
    padding-left: 18px;
  }

  .hint {
    font-size: 0.75rem;
  }

  .error {
    background: rgba(220, 80, 80, 0.12);
    border-radius: 4px;
    color: #e06c75;
    padding: 8px;
  }

  code {
    background: var(--bg-dark);
    border-radius: 3px;
    padding: 1px 4px;
  }

  .link-box {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 10px;
  }

  .link-row {
    align-items: center;
    display: flex;
    gap: 8px;
    margin-top: 6px;
  }

  .url {
    color: var(--fg-dim);
    /* The panel is 520px and these URLs run to several hundred characters — clamped to two
       lines rather than allowed to push the footer off the bottom of the dialog. */
    display: -webkit-box;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    flex: 1;
    font-size: 0.7rem;
    min-width: 0;
    overflow: hidden;
    overflow-wrap: anywhere;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    margin-bottom: 6px;
  }

  .label {
    color: var(--fg-dim);
    font-size: 0.75rem;
  }

  select {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    font-size: 0.85rem;
    padding: 5px 7px;
  }

  .segments {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    display: flex;
    gap: 2px;
    padding: 2px;
    /* Two or three short labels, but a narrow pane is a narrow pane — let them wrap rather
       than squeezing each one to an unreadable width. */
    flex-wrap: wrap;
  }

  .segment {
    background: transparent;
    border: none;
    border-radius: 3px;
    color: var(--fg-dim);
    cursor: pointer;
    flex: 1;
    font-size: 0.8rem;
    min-width: 64px;
    padding: 4px 10px;
    transition: background-color 0.15s, color 0.15s;
    white-space: nowrap;
  }

  .segment:hover:not(:disabled):not(.selected) {
    background: var(--bg-light);
    color: var(--fg);
  }

  .segment.selected {
    background: var(--accent);
    color: var(--bg-dark);
  }

  .segment:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .footer {
    border-top: 1px solid var(--bg-light);
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 10px 14px;
  }
</style>
