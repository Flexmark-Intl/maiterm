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
  import { writeText as clipboardWriteText } from '@tauri-apps/plugin-clipboard-manager';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AccountLoginUrl, PrivateBrowserInfo, TokenMint } from '$lib/tauri/commands';
  import type { ManagedAccount } from '$lib/tauri/types';

  interface Props {
    account: ManagedAccount;
    oncomplete: (mint: TokenMint) => void | Promise<void>;
    oncancel: () => void;
  }
  let { account, oncomplete, oncancel }: Props = $props();

  let phase = $state<'explain' | 'minting'>('explain');
  let error = $state<string | null>(null);
  const busy = $derived(phase !== 'explain');

  let loginUrl = $state<string | null>(null);
  let pasteCode = $state(false);
  let copied = $state(false);
  let openedPrivately = $state(false);
  let onUrl = $state<'none' | 'copy' | 'private'>('none');

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
        if (e.payload.account_id !== account.id) return;
        loginUrl = e.payload.url;
        pasteCode = e.payload.paste_code;
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

  async function mint(then: 'none' | 'copy' | 'private') {
    onUrl = then;
    phase = 'minting';
    error = null;
    try {
      const openWith = then === 'private' ? (browser?.id ?? null) : then === 'copy' ? null : 'default';
      const result = await commands.mintAccountToken('claude', account.id, { openWith });
      await oncomplete(result);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      phase = 'explain';
    }
  }
</script>

<div
  class="backdrop"
  role="presentation"
  onclick={e => {
    if (e.target === e.currentTarget && !busy) oncancel();
  }}
  onkeydown={e => {
    if (e.key === 'Escape' && !busy) oncancel();
  }}
>
  <div class="modal" role="dialog" aria-modal="true" aria-label="Set up remote logins">
    <h3>Use {account.label} on remote hosts</h3>

    <div class="body">
      <section>
        <h4>What this creates</h4>
        <p>
          A <strong>separate long-lived token</strong> for this account, minted by the agent in
          your browser. maiTerm keeps it in your OS keychain and hands it to SSH tabs on hosts
          you choose — so a remote agent runs as {account.label} instead of as whatever that
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
            against the same {account.plan ? account.plan.toUpperCase() : 'Pro/Max/Team'} plan
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
          {:else}
            <p class="hint">Waiting for the agent to produce an authorization link…</p>
          {/if}
        </section>
      {/if}

      {#if error}<p class="error">{error}</p>{/if}
    </div>

    <div class="footer">
      <Button variant="ghost" onclick={oncancel} disabled={busy}>Cancel</Button>
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
