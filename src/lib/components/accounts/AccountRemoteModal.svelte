<script lang="ts">
  /** Mint a remote token for one account — docs/login.md §6 step 1.
   *
   *  **This dialog's job is the disclosure, not the button.** Everything else in the Accounts
   *  pane hands the credential back to the runtime and keeps only a directory. This one makes
   *  maiTerm hold a real secret: a standing token, valid a year, that does not rotate and that
   *  nothing local can revoke (§9.4). Someone agreeing to that should be told what they are
   *  agreeing to, at the moment they agree, and not in a settings page they will never reopen.
   *
   *  The browser picker is here for the same reason it is on sign-in and it matters MORE here:
   *  `setup-token` runs the same browser flow, so a browser already signed in to another
   *  account can hand back a token for THAT account. maiTerm verifies what it minted before
   *  storing it (§6.1), so the failure is caught — but catching it after a round trip is worse
   *  than avoiding it. */
  import { error as logError } from '@tauri-apps/plugin-log';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AccountLoginUrl, PrivateBrowserInfo, TokenMint } from '$lib/tauri/commands';
  import type { ManagedAccount } from '$lib/tauri/types';

  interface Props {
    account: ManagedAccount;
    /** Carries the account BACK. The parent must not re-read its own `minting` state when this
     *  resolves: the pane's rows stay in the tab order behind this dialog, so a second row's
     *  "Set up…" can swap `minting` while a mint is in flight, and the result would then be
     *  recorded against the wrong account — giving one row a token it does not own and leaving
     *  the real one with a live credential and no metadata. */
    oncomplete: (account: ManagedAccount, mint: TokenMint) => void | Promise<void>;
    oncancel: () => void;
  }
  let { account, oncomplete, oncancel }: Props = $props();

  /** The account this dialog opened for, frozen at mount. `account` is a prop and can be
   *  swapped underneath an in-flight mint; this cannot. */
  const target = account;

  let panelEl = $state<HTMLDivElement | null>(null);
  // Svelte's `autofocus` is not focus — it compiles to a microtask that only acts when
  // activeElement is body, which is false when a keyboard opened this and false in WebKit when
  // a mouse clicked the opener button. Without this the backdrop's key handler never fires and
  // Escape reaches the window handler, which closes Preferences outright.
  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.focus());
    return () => cancelAnimationFrame(id);
  });

  let phase = $state<'explain' | 'minting'>('explain');
  let error = $state<string | null>(null);
  const busy = $derived(phase !== 'explain');

  let loginUrl = $state<string | null>(null);
  let pasteCode = $state(false);
  let copied = $state(false);
  let openedPrivately = $state(false);
  let onUrl = $state<'none' | 'copy' | 'private'>('none');

  /** The mint is waiting for the code the browser showed. See `submitAccountCode`. */
  let needsCode = $state(false);
  let code = $state('');
  let codeSent = $state(false);
  let codeError = $state<string | null>(null);
  let codeEl = $state<HTMLInputElement | null>(null);

  let browsers = $state<PrivateBrowserInfo[]>([]);
  let browserId = $state<string | null>(null);
  const browser = $derived(browsers.find(b => b.id === browserId) ?? browsers[0] ?? null);

  $effect(() => {
    void (async () => {
      try {
        browsers = await commands.listPrivateBrowsers();
        browserId ??= browsers[0]?.id ?? null;
      } catch (e) {
        logError(`[accounts] listing private browsers failed: ${e}`);
      }
    })();
  });

  // Scoped to THIS account's mint. The event broadcasts, and a second preferences window
  // running its own flow would otherwise overwrite this one's link.
  $effect(() => {
    let unlisten: UnlistenFn | null = null;
    let dead = false;
    void (async () => {
      const fn = await listen<AccountLoginUrl>(commands.ACCOUNT_LOGIN_URL_EVENT, e => {
        if (e.payload.account_id !== target.id) return;
        loginUrl = e.payload.url || null;
        pasteCode = e.payload.paste_code;
        needsCode = e.payload.needs_code;
        openedPrivately = e.payload.opened && onUrl === 'private';
        if (e.payload.open_error) error = e.payload.open_error;
        if (onUrl === 'copy' && loginUrl) void copyLink();
        // The browser has the user's attention; the field they have to come back to should
        // already be waiting for a paste when they do.
        if (needsCode) requestAnimationFrame(() => codeEl?.focus());
      });
      if (dead) void fn();
      else unlisten = fn;
    })();
    return () => {
      dead = true;
      void unlisten?.();
    };
  });

  /** Hand the code to the waiting mint.
   *
   *  Failure here is recoverable and must stay that way: a rejected code leaves the mint running
   *  (Rust only latches `code_sent` on a code it accepted), so the field stays open for another
   *  go rather than the dialog ending the attempt. */
  async function sendCode() {
    const value = code.trim();
    if (!value || codeSent) return;
    codeError = null;
    try {
      await commands.submitAccountCode(target.id, value);
      codeSent = true;
      code = '';
    } catch (e) {
      codeError = e instanceof Error ? e.message : String(e);
    }
  }

  async function copyLink() {
    if (!loginUrl) return;
    try {
      await clipboardWriteText(loginUrl);
      copied = true;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function mint(then: 'none' | 'copy' | 'private') {
    onUrl = then;
    phase = 'minting';
    error = null;
    // A retry is a NEW mint with a new link and a new code. Left standing, `codeSent` would hide
    // the field the second attempt depends on, and the old link would sit above it looking
    // current — this dialog is reachable again after any failure, so it has to be re-enterable.
    loginUrl = null;
    needsCode = false;
    codeSent = false;
    codeError = null;
    code = '';
    copied = false;
    openedPrivately = false;
    try {
      const openWith = then === 'private' ? (browser?.id ?? null) : then === 'copy' ? null : 'default';
      // The row's OWN runtime, not a literal. Only Claude can mint today, but a hardcoded slug
      // would mint a Claude token against a Codex account's id the moment another runtime is
      // supported — and it would bypass the Rust guard, which only ever sees what we send.
      const result = await commands.mintAccountToken(target.runtime, target.id, { openWith });
      // The browser took focus; without this the result lands in a window behind it and reads
      // as "Preferences closed itself".
      await focusThisWindow();
      await oncomplete(target, result);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      phase = 'explain';
      await focusThisWindow();
    }
  }

  /** Abort a mint that is still running.
   *
   *  The child is killed, so the browser flow cannot complete later and store a token the user
   *  walked away from. `cancel_account_login` is keyed by account id and reaches a mint as well
   *  as a sign-in: the mint runs on a PTY and is registered separately, and the same command
   *  flags both — from here they are one action. */
  async function cancelMint() {
    try {
      await commands.cancelAccountLogin(target.id);
    } catch (e) {
      logError(`[accounts] cancelling the mint failed: ${e}`);
    }
    oncancel();
  }

  async function focusThisWindow() {
    try {
      const win = getCurrentWindow();
      if (await win.isMinimized()) await win.unminimize();
      await win.setFocus();
    } catch {
      // Focus is a courtesy. Never let it swallow the mint result.
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    const closeKey =
      e.key === 'Escape' || (e.key.toLowerCase() === 'w' && (e.metaKey || e.ctrlKey));
    if (!closeKey) return;
    // **Swallow these unconditionally.** Preferences is its own window and closes itself on
    // Escape and Cmd+W, and the mint child does NOT stop when it does — so pressing Escape at
    // "Minting…" (the reflex) closed the window while the flow ran to completion and stored a
    // live one-year token that no UI could then reach, because the strip keys off metadata the
    // destroyed webview never got to write.
    e.stopPropagation();
    e.preventDefault();
    // Unlike the sign-in modal, this one CAN cancel while busy, so Escape does the right thing
    // rather than merely refusing to make things worse.
    if (busy) void cancelMint();
    else oncancel();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="backdrop"
  role="presentation"
  tabindex="-1"
  onclick={e => {
    if (e.target === e.currentTarget && !busy) oncancel();
  }}
  onkeydown={handleKeydown}
>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-label="Set up remote logins"
    bind:this={panelEl}
    tabindex="-1"
  >
    <h3>Use {target.label} on remote hosts</h3>

    <div class="body">
      <section>
        <h4>What this creates</h4>
        <p>
          A <strong>separate long-lived token</strong> for this account, minted by the agent in
          your browser. maiTerm keeps it in your OS keychain and hands it to SSH tabs on hosts
          you choose — so a remote agent runs as {target.label} instead of as whatever that
          host happens to be signed in to.
        </p>
      </section>

      <!-- The things someone should know BEFORE agreeing, not after. Each is a real property of
           the credential, not boilerplate. The billing line leads because it is the first
           question anyone asks about a long-lived token, and the answer is reassuring — but the
           scope line has to sit right behind it, because §8 requires the trade to be stated
           here rather than discovered on a host weeks later. -->
      <section>
        <h4>What you should know first</h4>
        <ul>
          <li>
            <strong>It uses your subscription, not API credits.</strong> The token authenticates
            against the same {target.plan ? target.plan.toUpperCase() : 'Pro/Max/Team'} plan
            this account already has. Remote usage is billed exactly as local usage is.
          </li>
          <li>
            <strong>It is a narrower credential than a sign-in.</strong> On hosts using it,
            model requests and your local MCP servers work normally — maiTerm's own bridge
            included — but <em>Remote Control sessions</em> and <em>claude.ai connectors</em> are
            unavailable, and <code>--bare</code> sessions ignore it. If you need those on a
            remote host, sign that host in itself instead of enabling it here.
          </li>
          <li>
            <strong>It lasts a year.</strong> It does not rotate, which is exactly why one token
            can serve several hosts at once — and also why it is worth protecting.
          </li>
          <li>
            <strong>Removing it here does not switch it off there.</strong> maiTerm stops handing
            it out, but a host that already has it keeps working until the token expires. To cut
            a token off everywhere, revoke it in your Anthropic account settings.
          </li>
          <li>
            <strong>On the host it is a file-level secret.</strong> Anyone who can read that
            user's environment can read the token — the same exposure as any credential in a
            shell profile there.
          </li>
          <li>
            <strong>Naming hosts keeps a record; “every SSH host” does not.</strong> Since
            nothing revokes these, the list of hosts you named is the only note of where the
            token went. Turn on the catch-all and there is no such list to consult later.
          </li>
        </ul>
        <p class="aside">
          None of this applies to your local tabs — those use full sign-ins and lose nothing.
        </p>
      </section>

      <!-- Same trap as adding a second account, and worse here: the mint takes whichever
           account the browser session holds, so a stale session mints for the wrong one. -->
      {#if browsers.length}
        <section class="note">
          <p>
            Your browser may already be signed in to a different account — the token would then
            be minted for <em>that</em> one. <strong>Mint privately</strong> opens a fresh
            {browser?.label ?? 'private'} window with no session to reuse. maiTerm checks which
            account the token actually belongs to before keeping it, so a mismatch is caught
            either way.
          </p>
          {#if browsers.length > 1}
            <div class="segments" role="radiogroup" aria-label="Private window">
              {#each browsers as b (b.id)}
                <button
                  type="button"
                  class="segment"
                  class:selected={browser?.id === b.id}
                  role="radio"
                  aria-checked={browser?.id === b.id}
                  disabled={busy}
                  onclick={() => (browserId = b.id)}>{b.label}</button
                >
              {/each}
            </div>
          {/if}
        </section>
      {:else}
        <section class="note">
          <p>
            Your browser may already be signed in to a different account — the token would then
            be minted for <em>that</em> one. Use <strong>Mint and copy link</strong> and paste it
            into a private window. maiTerm checks which account the token actually belongs to
            before keeping it.
          </p>
        </section>
      {/if}

      {#if phase === 'minting'}
        <section class="link-box">
          {#if loginUrl}
            <h4>
              {#if openedPrivately}Opened in {browser?.label ?? 'a private window'}
              {:else if copied}Link copied
              {:else}Authorization link{/if}
            </h4>
            <p class="hint">
              {#if pasteCode}
                This is the agent's own fallback link and cannot finish on its own — the window
                it opened can. It is here so you can see where it was sending you.
              {:else if openedPrivately}
                Finish there. If that window was not private, close it and use Copy.
              {:else if copied}
                On your clipboard. Paste it into a private window to mint as a different account.
              {:else}
                Open this in a private window to mint as a different account.
              {/if}
            </p>
            <div class="link-row">
              <code class="url">{loginUrl}</code>
              <Button variant="ghost" onclick={copyLink}>{copied ? 'Copy again' : 'Copy'}</Button>
            </div>
          {:else if !needsCode}
            <p class="hint">Waiting for the agent to produce an authorization link…</p>
          {/if}

          <!-- The step that makes a mint different from a sign-in. `setup-token` runs no
               localhost callback, so nothing completes on its own: approving in the browser
               produces a CODE, and this is where it comes back. -->
          {#if needsCode}
            <div class="code-step">
              <h4>{codeSent ? 'Finishing…' : 'Paste the code'}</h4>
              <p class="hint">
                {#if codeSent}
                  Exchanging it for a token, then checking which account it actually belongs to.
                {:else}
                  Approving in the browser gives you a code rather than finishing by itself.
                  Copy it and paste it here.
                {/if}
              </p>
              {#if !codeSent}
                <div class="link-row">
                  <input
                    bind:this={codeEl}
                    class="code-input"
                    type="text"
                    spellcheck="false"
                    autocapitalize="off"
                    autocorrect="off"
                    placeholder="Code from the browser"
                    bind:value={code}
                    onkeydown={e => {
                      if (e.key === 'Enter') { e.preventDefault(); void sendCode(); }
                    }}
                  />
                  <Button variant="secondary" disabled={!code.trim()} onclick={sendCode}>
                    Submit
                  </Button>
                </div>
              {/if}
              {#if codeError}<p class="error">{codeError}</p>{/if}
            </div>
          {/if}
        </section>
      {/if}

      {#if error}<p class="error">{error}</p>{/if}
    </div>

    <div class="footer">
      <!-- Enabled WHILE minting: the common reason to abort is seeing the browser open
           signed in to the wrong account, and without this the only escape was a key that
           closed the window and left the flow running to completion. -->
      <Button variant="ghost" onclick={() => (busy ? cancelMint() : oncancel())}>
        {busy ? 'Cancel mint' : 'Cancel'}
      </Button>
      {#if browser}
        <Button variant="secondary" disabled={busy} onclick={() => mint('private')}>
          Mint privately
        </Button>
      {:else}
        <Button variant="secondary" disabled={busy} onclick={() => mint('copy')}>
          Mint and copy link
        </Button>
      {/if}
      <Button variant="primary" disabled={busy} onclick={() => mint('none')}>
        {busy ? 'Minting…' : 'Mint token'}
      </Button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    align-items: center;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    inset: 0;
    justify-content: center;
    position: fixed;
    z-index: 1000;
  }

  .modal {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    display: flex;
    flex-direction: column;
    max-height: 85vh;
    max-width: 560px;
    width: 92%;
  }

  h3 {
    border-bottom: 1px solid var(--bg-light);
    color: var(--fg);
    font-size: 0.95rem;
    margin: 0;
    padding: 14px 16px;
    overflow-wrap: anywhere;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    overflow-y: auto;
    padding: 16px;
  }

  h4 {
    color: var(--fg);
    font-size: 0.8rem;
    font-weight: 600;
    margin: 0 0 4px;
  }

  p,
  li {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
    overflow-wrap: anywhere;
  }

  ul {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin: 0;
    padding-left: 18px;
  }

  .note {
    background: var(--bg-medium);
    border-radius: 6px;
    padding: 10px 12px;
  }

  .aside {
    color: var(--fg-faint, var(--fg-dim));
    font-size: 0.75rem;
    margin-top: 8px;
  }

  code {
    background: var(--bg-dark);
    border-radius: 3px;
    padding: 1px 4px;
  }

  .segments {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 8px;
  }

  .segment {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 0.75rem;
    padding: 4px 10px;
  }

  .segment.selected {
    background: color-mix(in srgb, var(--accent) 18%, transparent);
    border-color: var(--accent);
    color: var(--fg);
  }

  .segment:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .link-box {
    background: var(--bg-medium);
    border-radius: 6px;
    padding: 10px 12px;
  }

  .link-row {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    margin-top: 8px;
  }

  .url {
    background: var(--bg-dark);
    border-radius: 4px;
    color: var(--fg-dim);
    flex: 1 1 14rem;
    font-size: 0.7rem;
    min-width: 0;
    overflow-wrap: anywhere;
    padding: 6px 8px;
  }

  .hint {
    color: var(--fg-dim);
    font-size: 0.75rem;
  }

  /* Separated from the link above it because it is a second step, not more detail about the
     first — the user leaves for the browser between them. */
  .code-step {
    border-top: 1px solid var(--bg-light);
    margin-top: 12px;
    padding-top: 12px;
  }

  .code-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    flex: 1 1 14rem;
    font-family: inherit;
    font-size: 0.75rem;
    min-width: 0;
    padding: 6px 8px;
  }

  .code-input:focus {
    border-color: var(--accent);
    outline: none;
  }

  .error {
    background: rgba(220, 80, 80, 0.12);
    border-radius: 4px;
    color: #e06c75;
    font-size: 0.8rem;
    line-height: 1.5;
    overflow-wrap: anywhere;
    padding: 8px;
  }

  .footer {
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: flex-end;
    padding: 12px 16px;
  }
</style>
