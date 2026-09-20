<script lang="ts">
  /** The Accounts preferences pane (docs/login.md §10).
   *
   *  Four states, not two: setup is a separate artifact from the toggle, so turning the feature
   *  off keeps the accounts and is reversible in one click, while "Clear setup" is the
   *  destructive path that revokes and removes them.
   *
   *  The duplicate guard (§5.1) lives here rather than in Rust because this is the side that
   *  knows the existing account list. It is not a nicety: the browser reuses its session, so
   *  "add a second account" commonly returns the first, reporting success. */
  import Button from '$lib/components/ui/Button.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import AccountsSetupModal from './AccountsSetupModal.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AccountRuntimeInfo, AccountIdentity, NewAccount } from '$lib/tauri/commands';
  import type { ManagedAccount } from '$lib/tauri/types';

  let runtimes = $state<AccountRuntimeInfo[]>([]);
  let showSetup = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  /** Inline confirm — `window.confirm()` does not work in Tauri webviews. */
  let confirmingClear = $state(false);
  /** Last identity read per account id, for the resolved-source row. */
  let identities = $state<Record<string, AccountIdentity>>({});

  $effect(() => {
    void (async () => {
      try {
        runtimes = await commands.listAccountRuntimes();
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      }
    })();
  });

  const accounts = $derived(preferencesStore.managedAccounts);
  const setupComplete = $derived(preferencesStore.accountsSetupComplete);
  const enabled = $derived(preferencesStore.accountsEnabled);

  function labelFor(slug: string): string {
    return runtimes.find(r => r.slug === slug)?.label ?? slug;
  }

  /** Accounts grouped by runtime, only for runtimes that actually have one. */
  const grouped = $derived(
    [...new Set(accounts.map(a => a.runtime))].map(slug => ({
      slug,
      label: labelFor(slug),
      rows: accounts.filter(a => a.runtime === slug),
    })),
  );

  /** Derived, never stored: an active id naming a deleted account resolves to nothing rather
   *  than to a stale row. Same reason the store looks the account up instead of holding a flag. */
  function activeIdFor(slug: string): string | null {
    return preferencesStore.activeAccountIds[slug] ?? null;
  }

  async function addAccount(account: NewAccount, runtime: string) {
    const { identity, account_id } = account;

    // Not an identity we can trust — something above the account's own login answered. Reporting
    // it as an account would put a row with no email in the list and poison the duplicate key.
    if (!identity.is_account_login) {
      await commands.discardAccountRoot(runtime, account_id);
      error =
        `Signed in, but ${identity.shadowed_by ?? 'another credential'} is answering instead of ` +
        `the account login, so maiTerm cannot tell which account this is. Unset it and try again.`;
      return;
    }

    // §5.1: the browser reuses its claude.ai session, so this is the common case, not the edge.
    const dupe = accounts.find(
      a =>
        a.runtime === runtime &&
        (a.email ?? null) === (identity.email ?? null) &&
        (a.org_id ?? null) === (identity.org_id ?? null),
    );
    if (dupe) {
      await commands.discardAccountRoot(runtime, account_id);
      error =
        `That is the account you already have (${dupe.label}). Your browser was still signed ` +
        `in to it. Sign out of the provider, or use a private window, then try again.`;
      return;
    }

    const row: ManagedAccount = {
      id: account_id,
      runtime,
      label: identity.email ?? identity.org_name ?? 'Account',
      email: identity.email ?? undefined,
      org_id: identity.org_id ?? undefined,
      org_name: identity.org_name ?? undefined,
      plan: identity.plan ?? undefined,
      created_at: Math.floor(Date.now() / 1000),
      last_verified_at: Math.floor(Date.now() / 1000),
    };
    identities = { ...identities, [account_id]: identity };
    await preferencesStore.setManagedAccounts([...accounts, row]);
    if (!activeIdFor(runtime)) await preferencesStore.setActiveAccount(runtime, account_id);
    if (!setupComplete) {
      await preferencesStore.setAccountsSetupComplete(true);
      await preferencesStore.setAccountsEnabled(true);
    }
    notice = `Added ${row.label}.`;
    error = null;
  }

  async function verify(account: ManagedAccount) {
    busy = true;
    error = null;
    try {
      const identity = await commands.readAccountIdentity(account.runtime, account.id);
      identities = { ...identities, [account.id]: identity };
      if (identity.is_account_login && identity.email !== (account.email ?? null)) {
        error =
          `${account.label} now reports ${identity.email ?? 'a different account'}. ` +
          `Its sign-in may have been replaced.`;
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function removeAccount(account: ManagedAccount) {
    busy = true;
    error = null;
    try {
      await commands.discardAccountRoot(account.runtime, account.id);
      await preferencesStore.setManagedAccounts(accounts.filter(a => a.id !== account.id));
      // Drop the active pointer with the account, so nothing holds an id for a row that is gone.
      if (activeIdFor(account.runtime) === account.id) {
        await preferencesStore.setActiveAccount(account.runtime, null);
      }
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /** The destructive half of §10. One place resets everything, so no later field can be
   *  forgotten here and left behind pointing at accounts that no longer exist. */
  async function clearSetup() {
    busy = true;
    error = null;
    try {
      for (const a of accounts) {
        try {
          await commands.discardAccountRoot(a.runtime, a.id);
        } catch (e) {
          // Keep going: a root that is already gone must not strand the rest of the reset.
          console.warn('discarding account root failed', a.id, e);
        }
      }
      await preferencesStore.setManagedAccounts([]);
      for (const slug of Object.keys(preferencesStore.activeAccountIds)) {
        await preferencesStore.setActiveAccount(slug, null);
      }
      await preferencesStore.setAccountsEnabled(false);
      await preferencesStore.setAccountsSetupComplete(false);
      identities = {};
      notice = 'Setup cleared.';
      confirmingClear = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<div class="pane">
  <h3>Accounts</h3>
  <p class="hint">
    Hold more than one agent login and switch between them per tab. Off by default — maiTerm
    does not touch your sign-in unless you turn this on.
  </p>

  {#if !setupComplete}
    <div class="empty">
      <p>
        Not set up. Setup explains what maiTerm does and does not do with your logins, then signs
        you in once.
      </p>
      <Button variant="primary" onclick={() => (showSetup = true)}>Set up…</Button>
    </div>
  {:else}
    <label class="toggle">
      <input
        type="checkbox"
        checked={enabled}
        disabled={busy}
        onchange={e => preferencesStore.setAccountsEnabled(e.currentTarget.checked)}
      />
      <span>
        <strong>Manage agent logins</strong>
        <span class="sub">
          {#if enabled}
            New tabs launch under the active account for their runtime.
          {:else}
            Accounts are kept, but nothing is applied — new tabs use your normal login.
          {/if}
        </span>
      </span>
    </label>

    {#each grouped as group (group.slug)}
      <div class="group">
        <div class="group-head">{group.label}</div>
        {#each group.rows as account (account.id)}
          {@const identity = identities[account.id]}
          {@const isActive = activeIdFor(group.slug) === account.id}
          <div class="row" class:active={isActive}>
            <div class="row-main">
              <div class="row-title">
                <StatusDot
                  color={isActive && enabled ? 'green' : 'dim'}
                  tooltip={isActive ? 'Active for this runtime' : 'Not active'}
                />
                <span class="name">{account.label}</span>
                {#if account.plan}<span class="pill">{account.plan}</span>{/if}
              </div>
              {#if account.org_name}<div class="meta">{account.org_name}</div>{/if}
              {#if identity && !identity.is_account_login}
                <div class="meta warn">
                  Resolving as {identity.shadowed_by ?? 'another credential'}, not this account
                </div>
              {/if}
            </div>
            <div class="row-actions">
              {#if !isActive}
                <Button
                  variant="ghost"
                  disabled={busy}
                  onclick={() => preferencesStore.setActiveAccount(group.slug, account.id)}
                >Use</Button>
              {/if}
              <Tooltip text="Check which identity this account currently resolves to">
                <Button variant="ghost" disabled={busy} onclick={() => verify(account)}>Verify</Button>
              </Tooltip>
              <Button variant="ghost" disabled={busy} onclick={() => removeAccount(account)}>Remove</Button>
            </div>
          </div>
        {/each}
      </div>
    {/each}

    <div class="actions">
      <Button variant="secondary" disabled={busy} onclick={() => (showSetup = true)}>
        Add account…
      </Button>
      {#if confirmingClear}
        <span class="confirm">
          Remove {accounts.length} account{accounts.length === 1 ? '' : 's'} and turn this off?
          <Button variant="ghost" disabled={busy} onclick={() => (confirmingClear = false)}>Cancel</Button>
          <Button variant="primary" disabled={busy} onclick={clearSetup}>Clear setup</Button>
        </span>
      {:else}
        <Button variant="ghost" disabled={busy} onclick={() => (confirmingClear = true)}>
          Clear setup
        </Button>
      {/if}
    </div>
  {/if}

  {#if error}<p class="error">{error}</p>{/if}
  {#if notice && !error}<p class="notice">{notice}</p>{/if}
</div>

{#if showSetup}
  <AccountsSetupModal
    {runtimes}
    oncomplete={async (account, runtime) => {
      showSetup = false;
      await addAccount(account, runtime);
    }}
    oncancel={() => (showSetup = false)}
  />
{/if}

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

  .empty {
    align-items: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    padding: 14px;
  }

  .empty p {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
  }

  .toggle {
    align-items: flex-start;
    cursor: pointer;
    display: flex;
    gap: 8px;
  }

  .toggle input {
    accent-color: var(--accent);
    margin-top: 2px;
  }

  .toggle span {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .toggle strong {
    color: var(--fg);
    font-size: 0.85rem;
  }

  .sub {
    color: var(--fg-dim);
    font-size: 0.75rem;
    line-height: 1.4;
  }

  .group {
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    overflow: hidden;
  }

  .group-head {
    background: var(--bg-medium);
    color: var(--fg-dim);
    font-size: 0.75rem;
    padding: 6px 10px;
    text-transform: uppercase;
  }

  .row {
    align-items: center;
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: space-between;
    padding: 8px 10px;
  }

  .row.active {
    background: color-mix(in srgb, var(--accent) 8%, transparent);
  }

  .row-main {
    display: flex;
    flex-direction: column;
    gap: 3px;
    min-width: 0;
  }

  .row-title {
    align-items: center;
    display: flex;
    gap: 6px;
  }

  .name {
    color: var(--fg);
    font-size: 0.85rem;
    overflow-wrap: anywhere;
  }

  .pill {
    background: var(--bg-light);
    border-radius: 3px;
    color: var(--fg-dim);
    font-size: 0.7rem;
    padding: 1px 5px;
    text-transform: uppercase;
  }

  .meta {
    color: var(--fg-dim);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }

  .meta.warn {
    color: #e5c07b;
  }

  .row-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .actions {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .confirm {
    align-items: center;
    color: var(--fg-dim);
    display: flex;
    flex-wrap: wrap;
    font-size: 0.8rem;
    gap: 6px;
  }

  .error {
    background: rgba(220, 80, 80, 0.12);
    border-radius: 4px;
    color: #e06c75;
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
    overflow-wrap: anywhere;
    padding: 8px;
  }

  .notice {
    color: var(--fg-dim);
    font-size: 0.8rem;
    margin: 0;
  }
</style>
