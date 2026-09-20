<script lang="ts">
  /** First-run setup for managed accounts (docs/login.md §10).
   *
   *  This is a modal rather than a toggle because turning the feature on the first time has to
   *  TEACH and AUTHENTICATE before it can mean anything — §10's four states exist for that
   *  reason. The disclosure is not boilerplate: §8's losses are real and a user who would rather
   *  keep Remote Control should be able to decline here, before anything is created.
   *
   *  Follows ServiceModal — same backdrop/panel/btn vocabulary, same explicit rAF focus
   *  (Svelte's `autofocus` is not focus; it no-ops when a keyboard opened the dialog). */
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AccountRuntimeInfo, NewAccount } from '$lib/tauri/commands';

  interface Props {
    runtimes: AccountRuntimeInfo[];
    /** Called with the account the sign-in produced. The parent owns the duplicate check and
     *  persistence — it is the side that knows the existing account list (§5.1). */
    oncomplete: (account: NewAccount, runtime: string) => Promise<void>;
    oncancel: () => void;
  }

  let { runtimes, oncomplete, oncancel }: Props = $props();

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

  async function signIn() {
    error = null;
    phase = 'signing-in';
    try {
      const account = await commands.beginAccountLogin(runtime);
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
    if (e.key === 'Escape' && !busy) {
      e.stopPropagation();
      oncancel();
    }
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
  aria-label="Set up managed accounts"
  tabindex="-1"
>
  <div class="panel" bind:this={panelEl} tabindex="-1">
    <div class="header">
      <div class="title">Set up managed accounts</div>
      <div class="subtitle">
        maiTerm holds your agent logins, switches between them per tab, and keeps track of
        when they expire.
      </div>
    </div>

    <div class="body">
      <section>
        <h4>What this does</h4>
        <p>
          Each account gets its own config directory. A tab launched under an account uses that
          account's login, so two orgs can run side by side in different tabs at the same time.
        </p>
      </section>

      <section>
        <h4>What it does not do</h4>
        <p>
          maiTerm never reads, writes or parses your login, and never touches the runtime's
          Keychain item. It owns the <em>directory</em>; the runtime owns the credential inside
          it and does its own sign-in, refresh and sign-out.
        </p>
      </section>

      <section>
        <h4>What it costs</h4>
        <p>
          Nothing, on this machine. Your hooks, skills, commands, permissions and MCP servers
          are shared into every account, so a managed tab behaves exactly like an unmanaged one
          and signs in the same way.
        </p>
      </section>

      <section>
        <h4>Where credentials live</h4>
        <p>
          Anything maiTerm holds goes in your OS keychain under maiTerm's own entry — never in
          <code>aiterm-state.json</code>, and never reachable by an agent over MCP.
        </p>
      </section>

      <section>
        <h4>This machine only</h4>
        <p>
          Accounts apply to tabs on this computer. Signing SSH hosts in is not built yet; when
          it is, it will be opt-in per host and explained there, because it carries a tradeoff
          this does not.
        </p>
      </section>

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
        <p class="hint">
          Your browser will open. If you are already signed in to a different account there,
          sign out first or use a private window — otherwise it will return the account you
          already have, and maiTerm will tell you so rather than adding a duplicate.
        </p>
      </section>

      {#if error}
        <p class="error">{error}</p>
      {/if}
    </div>

    <div class="footer">
      <Button variant="ghost" onclick={oncancel} disabled={busy}>Cancel</Button>
      <Button variant="primary" onclick={signIn} disabled={busy || supported.length === 0}>
        {#if phase === 'signing-in'}Waiting for browser…
        {:else if phase === 'saving'}Saving…
        {:else}Sign in and enable{/if}
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

  .footer {
    border-top: 1px solid var(--bg-light);
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 10px 14px;
  }
</style>
