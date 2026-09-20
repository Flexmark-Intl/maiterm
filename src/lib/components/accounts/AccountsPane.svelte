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
  import AccountSwitchModal from './AccountSwitchModal.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AccountRuntimeInfo, AccountIdentity, NewAccount } from '$lib/tauri/commands';
  import type { ManagedAccount } from '$lib/tauri/types';

  let runtimes = $state<AccountRuntimeInfo[]>([]);
  /** Only so the duplicate message can name the button that fixes it. Empty is normal — Safari
   *  has no private-window switch — and the wording changes to match. */
  let privateBrowsers = $state<string[]>([]);
  let showSetup = $state(false);
  let busy = $state(false);
  let error = $state<string | null>(null);
  let notice = $state<string | null>(null);
  /** Inline confirm — `window.confirm()` does not work in Tauri webviews. */
  let confirmingClear = $state(false);
  /** The post-change modal, when a change has just been made that only affects NEW tabs.
   *  Switching accounts and toggling the feature are the same situation and get the same
   *  dialog — the reload offer is the whole point, and it applied to both from the start. */
  let announce = $state<{ title: string; subtitle: string; destination: string } | null>(null);
  /** Last identity read per account id, for the resolved-source row. */
  let identities = $state<Record<string, AccountIdentity>>({});

  $effect(() => {
    void (async () => {
      try {
        runtimes = await commands.listAccountRuntimes();
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      }
      try {
        privateBrowsers = (await commands.listPrivateBrowsers()).map(b => b.label);
      } catch (e) {
        // Costs only the sharper wording below. Not worth a red box on a working pane.
        console.warn('listing private browsers failed', e);
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

  /** Never throws. It runs while the setup modal is closing, so a rejection here would surface
   *  on a component that is about to be destroyed: the user would finish a browser sign-in and
   *  see the UI change in no way at all — no row, no error, just a line in the log. */
  async function addAccount(account: NewAccount, runtime: string) {
    try {
      await addAccountInner(account, runtime);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    }
  }

  async function addAccountInner(account: NewAccount, runtime: string) {
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
      // Name the button that fixes this, not the manual equivalent — by the time this shows,
      // the modal has closed and the user is looking at a wall of text telling them to go and
      // do by hand the thing "Add account…" now offers in one click.
      error =
        `That is the account you already have (${dupe.label}). Your browser was still signed ` +
        `in to it. ` +
        (privateBrowsers.length
          ? `Try again with “Sign in privately”, which opens a private ${privateBrowsers[0]} ` +
            `window with no session to reuse — or sign out of the provider first.`
          : `Use “Sign in and copy link”, then paste it into a private/incognito window — or ` +
            `sign out of the provider first.`);
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
    // One write, not four. A half-applied sequence could persist the account row while setup
    // was still false, which the pane renders as "Not set up" with the account unreachable.
    const activeIds = { ...preferencesStore.activeAccountIds };
    if (!activeIds[runtime]) activeIds[runtime] = account_id;
    await preferencesStore.setAccountsState({
      accounts: [...accounts, row],
      activeIds,
      ...(setupComplete ? {} : { setupComplete: true, enabled: true }),
    });
    notice = `Added ${row.label}.`;
    error = null;
  }

  /** Check an account still resolves to the identity we recorded.
   *
   *  **Reports success out loud.** It used to say nothing at all when everything was fine, which
   *  is indistinguishable from a dead button — and the one case it did report, a changed email,
   *  is rare enough that the button looked broken to everyone. A check whose "all good" is
   *  silence is not a check the user can trust. */
  async function verify(account: ManagedAccount) {
    busy = true;
    error = null;
    notice = null;
    try {
      const identity = await commands.readAccountIdentity(account.runtime, account.id);
      identities = { ...identities, [account.id]: identity };

      // Branch on is_account_login, never on logged_in: precedence is a fall-through, so some
      // other rung answering reports logged_in with no email at all.
      if (!identity.is_account_login) {
        error = identity.logged_in
          ? `${account.label} is signed in, but as ` +
            `${identity.shadowed_by ?? 'something else'} rather than this account. ` +
            `Remove it and add it again.`
          : `${account.label} is signed out. Remove it and add it again to sign back in.`;
        return;
      }
      if (identity.email !== (account.email ?? null)) {
        error =
          `${account.label} now reports ${identity.email ?? 'a different account'}. ` +
          `Its sign-in may have been replaced.`;
        return;
      }

      const now = Math.floor(Date.now() / 1000);
      await preferencesStore.setAccountsState({
        accounts: accounts.map(a => (a.id === account.id ? { ...a, last_verified_at: now } : a)),
      });
      notice = `${account.label} is still signed in${identity.plan ? ` on ${identity.plan}` : ''}.`;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /** Switch which account new tabs run as.
   *
   *  **Says out loud that open tabs do not move**, because "Active" reads as a claim about the
   *  whole app and every other switch in Preferences takes effect immediately. This one cannot:
   *  the account is an environment variable handed to the shell when the tab spawns, and a
   *  process's environment is fixed at exec — nothing can rewrite it from outside afterwards.
   *  Saying so at the moment of the click is the only place it lands. */
  async function makeActive(account: ManagedAccount) {
    busy = true;
    error = null;
    try {
      await preferencesStore.setActiveAccount(account.runtime, account.id);
      // A modal, not a notice: this is the one control here that does not take effect
      // immediately, and the follow-up question — "so how do I move the tabs I have open?" —
      // needs an answer, not a statement. A line under the table was too quiet for both.
      announce = {
        title: `Now using ${account.label}`,
        subtitle: 'Tabs you open from now on run as this account.',
        destination: account.label,
      };
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /** Turn management on or off.
   *
   *  Gets the SAME dialog as switching accounts, because it is the same situation: the change
   *  reaches new tabs only, and the useful question is "what about the ones I have open?".
   *  Toggling off without it looked like nothing had happened — every running tab carried on
   *  under its managed account with no sign that the feature was now off. */
  async function setEnabled(value: boolean) {
    busy = true;
    error = null;
    try {
      await preferencesStore.setAccountsEnabled(value);
      const active = preferencesStore.activeAccountFor('claude');
      announce = value
        ? {
            title: 'Managed logins turned on',
            subtitle: 'Tabs you open from now on run as the active account.',
            destination: active?.label ?? 'the active account',
          }
        : {
            title: 'Managed logins turned off',
            subtitle: 'Tabs you open from now on use your normal login.',
            destination: 'your normal login',
          };
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /** What the status dot means, in words. A coloured dot with no legend is a puzzle, and this
   *  one has three states rather than the two its colour suggests. */
  function statusTooltip(isActive: boolean, on: boolean): string {
    if (!isActive) return 'Not active — use “Use” to run new tabs as this account';
    return on
      ? 'Active — new tabs launch under this account'
      : 'Active, but management is off — new tabs use your normal login';
  }

  /** "3 minutes ago", for the last successful Verify. */
  function agoLabel(unixSecs: number): string {
    const secs = Math.max(0, Math.floor(Date.now() / 1000) - unixSecs);
    if (secs < 60) return 'just now';
    const mins = Math.floor(secs / 60);
    if (mins < 60) return `${mins} minute${mins === 1 ? '' : 's'} ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours} hour${hours === 1 ? '' : 's'} ago`;
    const days = Math.floor(hours / 24);
    return `${days} day${days === 1 ? '' : 's'} ago`;
  }

  async function removeAccount(account: ManagedAccount) {
    busy = true;
    error = null;
    notice = null;
    try {
      await commands.discardAccountRoot(account.runtime, account.id);
      const remaining = accounts.filter(a => a.id !== account.id);
      // Drop the active pointer with the account in the SAME write, so no persisted state ever
      // names a row that is gone.
      const activeIds = { ...preferencesStore.activeAccountIds };
      let promoted: ManagedAccount | null = null;
      if (activeIds[account.runtime] === account.id) {
        delete activeIds[account.runtime];
        // **Promote a replacement rather than leaving the runtime with none.** Clearing the
        // pointer alone is correct bookkeeping and a bad outcome: the feature then silently
        // does nothing — new tabs quietly use the normal login while the toggle still says
        // they launch under the active account. Observed after removing the active account
        // with the feature switched off, where nothing surfaced it until the toggle went back
        // on and appeared broken.
        promoted = remaining.find(a => a.runtime === account.runtime) ?? null;
        if (promoted) activeIds[account.runtime] = promoted.id;
      }
      await preferencesStore.setAccountsState({ accounts: remaining, activeIds });
      notice = promoted
        ? `Removed ${account.label}. ${promoted.label} is now the active account.`
        : `Removed ${account.label}.`;
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
      // One write. Everything this feature owns is reset here, so a field added later cannot be
      // forgotten in a second reset path.
      await preferencesStore.setAccountsState({
        accounts: [],
        activeIds: {},
        enabled: false,
        setupComplete: false,
      });
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
        Not set up. What this does and does not do with your logins is below — setup signs you in
        once and turns it on.
      </p>
      <Button variant="primary" onclick={() => (showSetup = true)}>Set up…</Button>
    </div>
  {:else}
    <div class="setting">
      <div class="setting-text">
        <span class="setting-label" id="accounts-enabled-label">Manage agent logins</span>
        <span class="sub">
          {#if enabled}
            Tabs launch under the active account for their runtime. Tabs already open are
            unaffected — an account is chosen when a tab starts.
          {:else}
            Accounts are kept, but nothing is applied — new tabs use your normal login.
          {/if}
        </span>
      </div>
      <button
        class="toggle"
        class:active={enabled}
        disabled={busy}
        onclick={() => setEnabled(!enabled)}
        aria-pressed={enabled}
        aria-labelledby="accounts-enabled-label"
      >
        <span class="toggle-knob"></span>
      </button>
    </div>

    {#each grouped as group (group.slug)}
      <div class="group">
        <div class="group-head">{group.label}</div>
        {#if enabled && !activeIdFor(group.slug)}
          <!-- Enabled with nothing active is a silent no-op: the toggle claims tabs launch
               under the active account while there is none, so they quietly use the normal
               login instead. Say it rather than let the pane imply otherwise. -->
          <div class="row-warn">
            No active account — new tabs use your normal login. Choose one with “Use”.
          </div>
        {/if}
        {#each group.rows as account (account.id)}
          {@const identity = identities[account.id]}
          {@const isActive = activeIdFor(group.slug) === account.id}
          <div class="row" class:active={isActive}>
            <div class="row-main">
              <div class="row-title">
                <!-- The tooltip goes on a padded wrapper, not the dot: the dot is 6px, which is
                     a hover target most people never hit, so the explanation may as well not
                     exist. StatusDot gets no `tooltip` prop here — that would nest two. -->
                <Tooltip text={statusTooltip(isActive, enabled)}>
                  <span class="dot-hit">
                    <StatusDot color={isActive && enabled ? 'green' : 'dim'} />
                  </span>
                </Tooltip>
                <span class="name">{account.label}</span>
                {#if account.plan}<span class="pill">{account.plan}</span>{/if}
                {#if isActive}<span class="pill active-pill">Active</span>{/if}
              </div>
              {#if account.org_name}<div class="meta">{account.org_name}</div>{/if}
              {#if account.last_verified_at}
                <div class="meta">Verified {agoLabel(account.last_verified_at)}</div>
              {/if}
              {#if identity && !identity.is_account_login}
                <div class="meta warn">
                  {#if !identity.logged_in}
                    <!-- Nothing is answering. A different problem from being shadowed, with a
                         different fix, so it must not borrow that wording. -->
                    Signed out — sign in again to use this account
                  {:else if identity.shadowed_by}
                    Resolving as {identity.shadowed_by}, not this account
                  {:else}
                    Signed in, but not as this account
                  {/if}
                </div>
              {/if}
            </div>
            <div class="row-actions">
              {#if !isActive}
                <Tooltip text="Run tabs opened from now on as this account">
                  <Button variant="ghost" disabled={busy} onclick={() => makeActive(account)}>
                    Use
                  </Button>
                </Tooltip>
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

  <!-- §10's disclosure. It lives HERE rather than only in the setup modal because a modal is
       read once, under pressure to get past it, and then is unreachable forever — while the
       questions it answers ("did I give maiTerm my credentials?") get asked months later. On
       the pane it is available before setup, so it still teaches before the decision, and
       afterwards, when it is the only place left to check. -->
  <section class="disclosure">
    <h4>What this does</h4>
    <p>
      Each account gets its own config directory. A tab launched under an account uses that
      account's login, so two orgs can run side by side in different tabs at the same time.
    </p>
    <p>
      An account is chosen when a tab <em>starts</em>. Switching the active account changes what
      new tabs use; tabs already open keep the account they started with, and so does an agent
      already running in one. To move a tab across, open a new one — or reload it
      (<code>Cmd+Shift+R</code>), which respawns its shell.
    </p>

    <h4>What it does not do</h4>
    <p>
      maiTerm never reads, writes or parses your login, and never touches the runtime's Keychain
      item. It owns the <em>directory</em>; the runtime owns the credential inside it and does
      its own sign-in, refresh and sign-out.
    </p>

    <h4>What it costs</h4>
    <p>
      Nothing, on this machine. Your hooks, skills, commands, permissions and MCP servers are
      shared into every account, so a managed tab behaves exactly like an unmanaged one and signs
      in the same way.
    </p>

    <h4>Where credentials live</h4>
    <p>
      Anything maiTerm holds goes in your OS keychain under maiTerm's own entry — never in
      <code>aiterm-state.json</code>, and never reachable by an agent over MCP.
    </p>

    <h4>This machine only</h4>
    <p>
      Accounts apply to tabs on this computer. Signing SSH hosts in is not built yet; when it is,
      it will be opt-in per host and explained there, because it carries a tradeoff this does not.
    </p>
  </section>
</div>

{#if announce}
  <AccountSwitchModal
    title={announce.title}
    subtitle={announce.subtitle}
    destination={announce.destination}
    onclose={() => (announce = null)}
  />
{/if}

{#if showSetup}
  <AccountsSetupModal
    {runtimes}
    mode={setupComplete ? 'add' : 'setup'}
    existing={accounts.map(a => a.label)}
    oncomplete={async (account, runtime) => {
      // Persist BEFORE unmounting the modal: closing first destroys the component that owns the
      // in-flight promise, so anything that went wrong afterwards had nowhere to be reported.
      await addAccount(account, runtime);
      showSetup = false;
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

  .setting {
    align-items: flex-start;
    display: flex;
    gap: 12px;
    justify-content: space-between;
  }

  .setting-text {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }

  .setting-label {
    color: var(--fg);
    font-size: 0.85rem;
  }

  /* The app-standard switch — same geometry as every other toggle in Preferences. Scoped
     styles mean it has to be restated here rather than inherited from the page. */
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

  .toggle:disabled {
    cursor: default;
    opacity: 0.5;
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

  .active-pill {
    background: color-mix(in srgb, var(--accent) 22%, transparent);
    color: var(--accent);
  }

  /* Enlarges the dot's hover target without moving it: negative margin cancels the padding. */
  .dot-hit {
    align-items: center;
    display: inline-flex;
    margin: -6px;
    padding: 6px;
  }

  .meta {
    color: var(--fg-dim);
    font-size: 0.75rem;
    overflow-wrap: anywhere;
  }

  .meta.warn {
    color: #e5c07b;
  }

  .row-warn {
    border-top: 1px solid var(--bg-light);
    color: #e5c07b;
    font-size: 0.75rem;
    line-height: 1.4;
    padding: 6px 10px;
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

  .disclosure {
    border-top: 1px solid var(--bg-light);
    margin-top: 4px;
    padding-top: 12px;
  }

  .disclosure h4 {
    color: var(--fg);
    font-size: 0.8rem;
    font-weight: 600;
    margin: 0 0 3px;
  }

  .disclosure h4:not(:first-child) {
    margin-top: 12px;
  }

  .disclosure p {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .disclosure code {
    background: var(--bg-dark);
    border-radius: 3px;
    padding: 1px 4px;
  }
</style>
