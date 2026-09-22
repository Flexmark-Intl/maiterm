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
   *  account can hand back a token for THAT account — and **maiTerm cannot detect that**.
   *  `auth status --json` gives a token no email, org or plan whatever it is worth (§2.4), so
   *  there is nothing to compare. The mint proves the token WORKS and stops there. The browser
   *  picker is therefore not a convenience here, it is the only control over which account the
   *  token belongs to. */
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
  /** A cancel has been asked for and the mint has not answered yet. */
  let cancelling = $state(false);

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
        // Assigned, not or-ed in: the grace-period note ("the agent opened its own window")
        // is emitted BEFORE a late shim capture, and leaving it up next to "Opened in Chrome"
        // tells the user to approve in a window that does not exist — which is the wrong-account
        // mint this dialog exists to prevent.
        error = e.payload.open_error ?? null;
        if (onUrl === 'copy' && loginUrl) void copyLink();
        // **Deliberately not focused.** The flow normally finishes on its own through the
        // runtime's localhost callback; the code field is the fallback for when the browser
        // shows a code instead. Focusing it would assert it is the next step, which sends the
        // user hunting for a code that usually does not exist.
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
    codeSent = true;
    try {
      await commands.submitAccountCode(target.id, value);
    } catch (e) {
      codeError = e instanceof Error ? e.message : String(e);
      codeSent = false;
    }
  }

  /** Offer the field again after a code the agent would not take.
   *
   *  The agent validates the `<code>#<state>` shape itself and re-prompts on a bad one — copying
   *  only the half before the `#` is common enough that it has its own error message. Without a
   *  way back, one mistyped character meant sitting on "Finishing…" until the deadline. */
  async function retryCode() {
    codeError = null;
    codeSent = false;
    try {
      await commands.resetAccountCode(target.id);
    } catch (e) {
      logError(`[accounts] resetting the mint code failed: ${e}`);
    }
    requestAnimationFrame(() => codeEl?.focus());
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
    // The button that was clicked unmounts with the explain phase, dropping focus to body —
    // where the backdrop's Escape handler never fires and Escape closes Preferences instead,
    // the exact failure `handleKeydown` exists to prevent.
    requestAnimationFrame(() => panelEl?.focus());
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
    cancelling = false;
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
    cancelling = true;
    try {
      await commands.cancelAccountLogin(target.id);
    } catch (e) {
      logError(`[accounts] cancelling the mint failed: ${e}`);
    }
    // **Deliberately does NOT close the dialog.** Cancelling after the browser has finished
    // cannot un-mint the token — there is no revoke (§9.4) — so Rust answers that cancel with a
    // message saying a live credential now exists and only the user can retire it. Unmounting
    // here would throw that away: the in-flight `mint()` rejects into a component that is gone.
    // The flow lands in `mint()`'s catch, which shows the reason and returns to 'explain'; the
    // footer's Cancel then closes as usual.
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
    <header class="head">
      <h3>Use {target.label} on remote hosts</h3>
      <p class="lead">
        Mints a long-lived token for this account. maiTerm keeps it in your OS keychain and hands
        it to SSH tabs on the hosts you choose, so a remote agent runs as {target.label} instead
        of whatever that host is signed in to.
      </p>
    </header>

    <div class="body">
      <!-- First in the body so a failed mint's reason is the first thing seen on return, not
           something at the bottom of a scroll. -->
      {#if error}<p class="error">{error}</p>{/if}

      {#if phase === 'minting'}
        <!-- While a mint runs this is the ONLY content. It used to render below the full
             disclosure, so the link and the code field — the things the user needs right now —
             arrived under a page of text they had already agreed to. -->
        <section class="progress" aria-live="polite">
          <div class="progress-head">
            <span class="pulse" aria-hidden="true"></span>
            <h4>
              {#if cancelling}Cancelling…
              {:else if openedPrivately}Waiting for you in {browser?.label ?? 'a private window'}
              {:else if copied}Link copied — waiting for approval
              {:else if loginUrl}Waiting for approval in your browser
              {:else}Starting the agent…{/if}
            </h4>
          </div>
          <p class="hint">
            {#if !loginUrl}
              The authorization link appears here as soon as the agent produces it.
            {:else if pasteCode}
              This is the agent's own fallback link and cannot finish on its own — the window it
              opened can. It is here so you can see where it was sending you.
            {:else if openedPrivately}
              Approve there and this finishes on its own. If that window was not private, close it
              and use Copy.
            {:else if copied}
              Paste it into a private window to mint as a different account.
            {:else}
              Approve in the browser and this finishes on its own. To mint as a different account,
              open the link in a private window instead.
            {/if}
          </p>
          {#if loginUrl}
            <div class="link-row">
              <code class="url">{loginUrl}</code>
              <Button variant="secondary" onclick={copyLink}>{copied ? 'Copy again' : 'Copy'}</Button>
            </div>
          {/if}

          <!-- A FALLBACK, not the next step. The link carries
               `redirect_uri=http://localhost:<port>/callback` and finishes through the agent's own
               listener, so approving in the browser is normally all there is to do. Some browsers
               land on the agent's manual page instead and show a code; this is where that code
               comes back. Captioned as the required action, it sent people hunting for a code
               that was not there. -->
          {#if needsCode}
            <div class="code-step">
              <h4>{codeSent ? 'Code sent' : 'Browser showed a code?'}</h4>
              <p class="hint">
                {#if codeSent}
                  Waiting for the agent to accept it.
                {:else}
                  Only if approving did not finish this on its own — paste the code here.
                {/if}
              </p>
              {#if codeSent}
                <div class="link-row">
                  <Button variant="ghost" onclick={retryCode}>Enter a different code</Button>
                </div>
              {:else}
                <div class="link-row">
                  <input
                    bind:this={codeEl}
                    class="code-input"
                    type="text"
                    spellcheck="false"
                    autocapitalize="off"
                    autocorrect="off"
                    aria-label="Code from the browser"
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
      {:else}
        <!-- The things someone should know BEFORE agreeing. Each is a real property of the
             credential, not boilerplate, and each headline states it outright — the detail is
             one click away rather than gone, because §8 requires the trade to be stated here,
             not discovered on a host weeks later. Six full paragraphs used to push the choice
             and the buttons below the fold. The billing line leads because it is the first
             question anyone asks about a long-lived token; the scope line sits right behind. -->
        <section>
          <h4>Before you mint</h4>
          <ul class="facts">
            <li class="fact good">
              <details>
                <summary>
                  Uses your {target.plan ? target.plan.toUpperCase() : 'Pro/Max/Team'} subscription,
                  not API credits
                </summary>
                <p>
                  The token authenticates against the plan this account already has. Remote usage
                  is billed exactly as local usage is.
                </p>
              </details>
            </li>
            <li class="fact">
              <details>
                <summary>No Remote Control or claude.ai connectors on those hosts</summary>
                <p>
                  It is a narrower credential than a sign-in. Model requests and your local MCP
                  servers work normally — maiTerm's own bridge included — but
                  <em>Remote Control sessions</em> and <em>claude.ai connectors</em> are unavailable,
                  and <code>--bare</code> sessions ignore it. If you need those on a host, sign that
                  host in itself instead.
                </p>
              </details>
            </li>
            <li class="fact">
              <details>
                <summary>Lasts a year and never rotates</summary>
                <p>
                  Not rotating is exactly why one token can serve several hosts at once — and also
                  why it is worth protecting.
                </p>
              </details>
            </li>
            <li class="fact">
              <details>
                <summary>Removing it here does not switch it off on hosts</summary>
                <p>
                  maiTerm stops handing it out, but a host that already has it keeps working until
                  the token expires, a year after it was minted. Nothing maiTerm or the agent CLI
                  offers can revoke it early.
                </p>
              </details>
            </li>
            <li class="fact">
              <details>
                <summary>Readable on the host like any shell-profile secret</summary>
                <p>
                  Anyone who can read that user's environment on the host can read the token.
                </p>
              </details>
            </li>
            <li class="fact">
              <details>
                <summary>Only named hosts leave a record of where it went</summary>
                <p>
                  Since nothing revokes these, the list of hosts you name is the only note of where
                  the token went. Turn on “every SSH host” and there is no such list to consult
                  later.
                </p>
              </details>
            </li>
          </ul>
          <p class="aside">Your local tabs are unaffected — they keep full sign-ins.</p>
        </section>

        <!-- Same trap as adding a second account, and worse here: the mint takes whichever
             account the browser session holds, so a stale session mints for the wrong one — and
             maiTerm cannot detect it afterwards (§2.4). -->
        <section class="note">
          <h4>The browser decides which account this is for</h4>
          <p>
            The token belongs to whichever account the browser is signed in to, and maiTerm
            <strong>cannot check afterwards</strong> — a token carries no profile.
            {#if browsers.length}
              A private {browser?.label ?? ''} window has no session to reuse.
            {:else}
              Copy the link and open it in a private window to be sure.
            {/if}
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
      {/if}
    </div>

    <div class="footer">
      <!-- Enabled WHILE minting: the common reason to abort is seeing the browser open signed in
           to the wrong account, and without this the only escape was a key that closed the
           window and left the flow running to completion. -->
      <Button variant="ghost" disabled={cancelling} onclick={() => (busy ? cancelMint() : oncancel())}>
        {cancelling ? 'Cancelling…' : busy ? 'Cancel mint' : 'Cancel'}
      </Button>
      {#if !busy}
        <!-- The safe path is the primary one. The copy above says the browser window is the only
             control over which account the token is for, so the default browser — the one most
             likely signed in to somebody else — is the secondary choice, not the big button. -->
        <Button variant="secondary" onclick={() => mint('none')}>Mint in default browser</Button>
        {#if browser}
          <Button variant="primary" onclick={() => mint('private')}>
            Mint in private {browser.label}
          </Button>
        {:else}
          <Button variant="primary" onclick={() => mint('copy')}>Mint and copy link</Button>
        {/if}
      {/if}
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

  .modal:focus {
    outline: none;
  }

  .head {
    border-bottom: 1px solid var(--bg-light);
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 14px 16px 12px;
  }

  h3 {
    color: var(--fg);
    font-size: 0.95rem;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .head .lead {
    max-width: 62ch;
  }

  /* --- Before you mint: one line per fact, detail on demand --- */

  .facts {
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    gap: 0;
    list-style: none;
    padding: 0;
  }

  .fact + .fact {
    border-top: 1px solid var(--bg-light);
  }

  .fact summary {
    align-items: baseline;
    color: var(--fg);
    cursor: pointer;
    display: flex;
    gap: 8px;
    list-style: none;
    padding: 7px 10px;
  }

  .fact summary::-webkit-details-marker {
    display: none;
  }

  /* The marker carries meaning: green for the one reassuring fact, amber for the costs. */
  .fact summary::before {
    background: var(--yellow);
    border-radius: 50%;
    content: '';
    flex: none;
    height: 6px;
    transform: translateY(-1px);
    width: 6px;
  }

  .fact.good summary::before {
    background: var(--green);
  }

  .fact summary::after {
    color: var(--fg-dim);
    content: '›';
    margin-left: auto;
    transition: transform 0.15s;
  }

  .fact details[open] summary::after {
    transform: rotate(90deg);
  }

  .fact summary:hover {
    background: color-mix(in srgb, var(--bg-light) 30%, transparent);
  }

  .fact summary:focus-visible {
    outline: 1px solid var(--accent);
    outline-offset: -1px;
  }

  .fact details p {
    padding: 0 10px 9px 24px;
  }

  /* --- While minting --- */

  .progress {
    background: var(--bg-medium);
    border-radius: 6px;
    padding: 12px;
  }

  .progress-head {
    align-items: center;
    display: flex;
    gap: 8px;
  }

  .progress-head h4 {
    margin: 0;
  }

  .progress > .hint {
    margin-top: 4px;
  }

  /* The one piece of unprompted motion: it says the mint is alive while the user is away in
     the browser, which is otherwise indistinguishable from a hang. */
  .pulse {
    animation: pulse 1.4s ease-in-out infinite;
    background: var(--accent);
    border-radius: 50%;
    flex: none;
    height: 8px;
    width: 8px;
  }

  @keyframes pulse {
    50% {
      opacity: 0.25;
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .pulse {
      animation: none;
    }
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
    background: color-mix(in srgb, var(--red) 12%, transparent);
    border-radius: 4px;
    color: var(--red);
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
