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
  import { error as logError } from '@tauri-apps/plugin-log';
  import Button from '$lib/components/ui/Button.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import AccountsSetupModal from './AccountsSetupModal.svelte';
  import AccountSwitchModal from './AccountSwitchModal.svelte';
  import AccountRemoteModal from './AccountRemoteModal.svelte';
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
  /** The account whose remote-token dialog is open, if any (§6). */
  let minting = $state<ManagedAccount | null>(null);
  /** Whether the VAULT holds a token per account id — not whether state says so.
   *
   *  Rust stores the token before the frontend records `token_minted_at`, so every way that
   *  second step can fail (a refused save, the window closing mid-mint, a reply landing in a
   *  destroyed webview) leaves a live one-year credential with no metadata pointing at it. Keyed
   *  on metadata, the strip would show "not set up" and hide the only control that removes it. */
  let hasToken = $state<Record<string, boolean>>({});

  async function refreshTokenPresence() {
    const seen: Record<string, boolean> = {};
    for (const a of accounts) {
      try {
        seen[a.id] = await commands.hasAccountToken(a.id);
      } catch (e) {
        // Unreadable keychain counts as "might have one" — the recovery affordance is the
        // thing that must not disappear.
        logError(`[accounts] token presence check failed for ${a.id}: ${e}`);
        seen[a.id] = !!a.token_minted_at;
      }
    }
    hasToken = seen;
  }

  $effect(() => {
    // Re-run whenever the account list changes identity-wise.
    const ids = accounts.map(a => a.id).join(',');
    void ids;
    void refreshTokenPresence();
  });
  /** Per-account draft in the "add a host" field. Keyed by id so two rows do not share one. */
  let hostDraft = $state<Record<string, string>>({});

  /** A token is one year old at most (§2.4), and §7 forbids reading expiry out of the
   *  credential — it is a private format the CLI does not expose — so it is derived from our own
   *  mint record and nothing else. */
  const TOKEN_LIFETIME_SECS = 365 * 24 * 60 * 60;

  function tokenExpiry(mintedAt: number): { label: string; warn: boolean } {
    const secsLeft = mintedAt + TOKEN_LIFETIME_SECS - Math.floor(Date.now() / 1000);
    const days = Math.floor(secsLeft / 86400);
    if (days < 0) return { label: 'Token expired', warn: true };
    if (days === 0) return { label: 'Token expires today', warn: true };
    // §7 warns at T-30d: this is the silent-failure case the whole feature exists for — a
    // one-year token dies around month eleven on a box nobody is watching, and per §6.1 the
    // tab does not fail, it comes up as whoever else that host is signed in to.
    return { label: `Token expires in ${days} day${days === 1 ? '' : 's'}`, warn: days <= 30 };
  }

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
        logError(`[accounts] listing private browsers failed: ${e}`);
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
      // Remote follows the active account, so say so — otherwise "now using X" reads as a
      // local-only claim and the SSH tabs quietly changing identity on their next spawn is a
      // surprise. Only mentioned when this account can actually reach a host: promising a
      // remote switch that cannot happen is worse than saying nothing.
      // **Keyed on `token_minted_at`, because that is what `remote_account_for_host` keys
      // on** — not on `hasToken`, which is vault truth. The two genuinely diverge: a mint whose
      // metadata write failed leaves a token in the keychain that Rust will refuse to
      // propagate, and promising a remote switch that cannot happen is the thing this message
      // exists to avoid. The strip still shows that orphan (so it can be removed); it just must
      // not be counted as working.
      const reachesRemotes =
        !!account.token_minted_at &&
        (account.remote_all_hosts || (account.remote_hosts ?? []).length > 0);
      // A modal, not a notice: this is the one control here that does not take effect
      // immediately, and the follow-up question — "so how do I move the tabs I have open?" —
      // needs an answer, not a statement. A line under the table was too quiet for both.
      announce = {
        title: `Now using ${account.label}`,
        subtitle: reachesRemotes
          ? 'Tabs you open from now on run as this account — including SSH tabs to the hosts it covers.'
          : 'Tabs you open from now on run as this account.',
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

  /** Write one account's row back, leaving every other field alone. */
  async function patchAccount(id: string, patch: Partial<ManagedAccount>) {
    await preferencesStore.setAccountsState({
      accounts: accounts.map(a => (a.id === id ? { ...a, ...patch } : a)),
    });
  }

  /** Enable a host for this account — §6: "per host, explicit, and never inferred".
   *
   *  Typed in rather than picked from the tabs that happen to be open, which is the whole
   *  point: a standing one-year credential belongs on a box the user names on purpose, not on
   *  whatever they last SSH'd into. */
  /** `user@host`, `host`, or an ssh alias. Deliberately narrow.
   *
   *  This string is compared against a tab's ssh target and will end up near a command line
   *  that `buildSshCommand` builds, which already applies exactly this discipline to the two
   *  values it interpolates. A quote or a `$(…)` in here is an injection surface the moment
   *  anything interpolates rather than compares — and per §6.1 a host that merely never matches
   *  fails SILENTLY, billing a remote tab to the wrong account. */
  const HOST_SHAPE = /^[A-Za-z0-9._@-]+$/;

  async function addHost(account: ManagedAccount) {
    // Hostnames are case-insensitive, so normalize rather than let `nova` and `Nova` sit in the
    // list as two entries that behave identically and look like a bug.
    const host = (hostDraft[account.id] ?? '').trim().toLowerCase();
    if (!host) return;
    if (!HOST_SHAPE.test(host)) {
      error =
        `"${host}" does not look like a host. Use a hostname, an ssh alias, or user@host — ` +
        `letters, digits, dot, dash, underscore and @ only.`;
      return;
    }
    const hosts = account.remote_hosts ?? [];
    if (hosts.includes(host)) {
      hostDraft = { ...hostDraft, [account.id]: '' };
      return;
    }
    busy = true;
    error = null;
    try {
      await patchAccount(account.id, { remote_hosts: [...hosts, host] });
      hostDraft = { ...hostDraft, [account.id]: '' };
      notice = !account.token_minted_at
        ? `${host} added, but this account's token was never recorded — remove it and mint ` +
          `again before SSH tabs will use it.`
        : activeIdFor(account.runtime) === account.id
          ? `New SSH tabs to ${host} will use ${account.label}.`
          : `${host} will use ${account.label} whenever it is the active account.`;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /** "Use on every SSH host" — the catch-all, scoped to this account being active.
   *
   *  **No mutual exclusion, and that is the point.** Only the ACTIVE account is ever propagated
   *  (`remote_account_for_host`), so several accounts can each say "cover everything when I am
   *  the one in use" without any of them contending. Switching accounts switches remotes with
   *  it. An earlier version turned this off on other accounts because it thought two of them
   *  could claim one host at once — they cannot, because the question is only ever asked of
   *  one. */
  async function setAllHosts(account: ManagedAccount, value: boolean) {
    busy = true;
    error = null;
    try {
      await patchAccount(account.id, { remote_all_hosts: value });
      const isActive = activeIdFor(account.runtime) === account.id;
      notice = value
        ? `${account.label} will be used on every SSH host` +
          (isActive ? '.' : ' while it is the active account.')
        : `${account.label} now covers only the hosts named below.`;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function removeHost(account: ManagedAccount, host: string) {
    busy = true;
    error = null;
    try {
      await patchAccount(account.id, {
        remote_hosts: (account.remote_hosts ?? []).filter(h => h !== host),
      });
      // Says what it does NOT do. §9.4: nothing local revokes these, so a host that already
      // holds the token keeps working — claiming otherwise here would be the one place this
      // feature lies about security.
      notice =
        `New SSH tabs to ${host} will no longer use ${account.label}. Sessions already open ` +
        `there, and the token already on that host, are unaffected.`;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  /** Forget the token and every host that depended on it, in one write.
   *
   *  Hosts go with it deliberately: a host list with no token behind it is a promise the
   *  feature cannot keep, and per §6.1 the failure is silent — the tab comes up as whoever that
   *  host was already signed in to rather than erroring. */
  async function forgetToken(account: ManagedAccount) {
    busy = true;
    error = null;
    notice = null;
    try {
      // **Keychain FIRST, state second — the opposite of `removeAccount` below, and for a
      // reason specific to this pair rather than a contradiction of it.**
      //
      // `vault::delete` can genuinely be refused: Deny on a keychain prompt, a locked login
      // keychain, no UI session — and a dev build provokes that prompt on every rebuild because
      // ad-hoc signing changes the binary hash. With state written first, a refusal left a live
      // one-year token in the keychain while the strip had already re-rendered to "Not set up",
      // hiding the control that would retry. Silent and unreachable.
      //
      // This order fails loudly and recoverably instead: the delete throws, state still says
      // there is a token, the button is still there, and `vault::delete` is idempotent so the
      // retry works. (`removeAccount` persists first for the opposite reason — there a rolled
      // back save would restore a row whose config root is already gone.)
      await commands.forgetAccountToken(account.id);
      // The catch-all goes too. Clearing only the host list left `remote_all_hosts` armed, so
      // a later re-mint silently resumed propagating to EVERY host without the user asking for
      // it again — the one setting where a surprise "on" is other people's money.
      await patchAccount(account.id, {
        token_minted_at: undefined,
        remote_hosts: [],
        remote_all_hosts: false,
      });
      hasToken = { ...hasToken, [account.id]: false };
      notice =
        `Removed the remote token for ${account.label}. Hosts that already have it keep ` +
        `working until it expires — revoke it in your Anthropic account settings to cut it off.`;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  async function removeAccount(account: ManagedAccount) {
    busy = true;
    error = null;
    notice = null;
    try {
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
      // **Persist BEFORE discarding the root.** `save()` can genuinely be refused — the
      // two-instance conflict guard aborts rather than clobber a newer state file — and
      // `setAccountsState` rolls back on failure. Discarding first meant the rollback restored a
      // row whose root and credential were already gone: every new tab would then be handed a
      // CLAUDE_CONFIG_DIR pointing nowhere, and the runtime would recreate it bare — no hooks,
      // no MCP, no identity, signed out. This order fails the safe way instead, leaving a root
      // the startup prune collects.
      await preferencesStore.setAccountsState({ accounts: remaining, activeIds });
      await commands.discardAccountRoot(account.runtime, account.id);
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
      // Snapshot before the write, which empties the list.
      const toDiscard = accounts.map(a => ({ runtime: a.runtime, id: a.id }));
      // One write. Everything this feature owns is reset here, so a field added later cannot be
      // forgotten in a second reset path.
      //
      // And it happens FIRST, for the same reason as in `removeAccount`: a refused save rolls
      // the rows back, and rows restored after their roots were deleted point every new tab at
      // a directory that no longer exists. Roots left behind by a failed clear are collected by
      // the startup prune; rows pointing at nothing are not.
      await preferencesStore.setAccountsState({
        accounts: [],
        activeIds: {},
        enabled: false,
        setupComplete: false,
      });
      for (const a of toDiscard) {
        try {
          await commands.discardAccountRoot(a.runtime, a.id);
        } catch (e) {
          // Keep going: a root that is already gone must not strand the rest of the reset.
          logError(`[accounts] discarding root ${a.id} failed: ${e}`);
        }
      }
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

  {#if runtimes.length > 0 && !runtimes.some(r => r.supported)}
    <!-- Nothing here can work on this platform, so offer nothing that looks like it can.
         `supported` is the single flag every consumer asks — the setup picker, the Rust
         sign-in, the spawn path — so this cannot drift out of step with what actually runs.
         Until 2026-09-20 there was no such branch and Windows users could complete setup and
         watch a row go green while every tab kept using their normal login. -->
    <div class="empty">
      <p>
        Not available on this system yet. maiTerm can hold agent logins on macOS and Linux; the
        Windows build cannot locate the agent CLI or create an account directory without
        elevation, so the feature stays off rather than half-work.
      </p>
    </div>
  {:else if !setupComplete}
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
          <!-- Keyed on what the VAULT holds, not on `token_minted_at`. A token can exist with no
               metadata — a refused save, a window closed mid-mint — and keying on metadata hid
               the only control that removes it. -->
          {@const tokenPresent = hasToken[account.id] ?? !!account.token_minted_at}
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

          <!-- §6. Its own strip under the row rather than more buttons in it: this is the one
               part of the feature where maiTerm holds a real credential, and it should not read
               as another action alongside Use and Verify. -->
          <div class="remote">
            {#if !tokenPresent}
              <div class="remote-head">
                <span class="remote-label">Remote hosts</span>
                <Button variant="ghost" disabled={busy} onclick={() => (minting = account)}>
                  Set up…
                </Button>
              </div>
              <p class="remote-hint">
                Not set up. SSH tabs use whatever login each host already has.
              </p>
            {:else}
              <div class="remote-head">
                <span class="remote-label">Remote hosts</span>
                {#if account.token_minted_at}
                  {@const exp = tokenExpiry(account.token_minted_at)}
                  <span class="meta" class:warn={exp.warn}>{exp.label}</span>
                {:else}
                  <!-- A token in the keychain that state never recorded. Say so plainly rather
                       than show a blank: this is the leftover of an interrupted mint, and the
                       useful action is to remove it and start again. -->
                  <!-- Vault holds a token, state does not. Rust keys propagation on the
                       metadata, so this token is inert — say that, or the hosts below read as
                       working. -->
                  <span class="meta warn">
                    Token present but never recorded — not in use. Remove it and mint again.
                  </span>
                {/if}
                <Button variant="ghost" disabled={busy} onclick={() => forgetToken(account)}>
                  Remove token
                </Button>
              </div>

              <!-- The catch-all. A switch rather than a checkbox, per app standard. -->
              <div class="all-hosts">
                <span class="setting-label" id={`all-hosts-${account.id}`}>
                  Use on every SSH host
                </span>
                <button
                  class="toggle small"
                  class:active={account.remote_all_hosts}
                  disabled={busy}
                  aria-pressed={!!account.remote_all_hosts}
                  aria-labelledby={`all-hosts-${account.id}`}
                  onclick={() => setAllHosts(account, !account.remote_all_hosts)}
                >
                  <span class="toggle-knob"></span>
                </button>
              </div>
              {#if account.remote_all_hosts}
                <p class="remote-hint">
                  While this is the active account, every SSH tab uses it. Switching accounts
                  switches your remotes too.
                </p>
              {/if}

              {#if (account.remote_hosts ?? []).length}
                <div class="hosts">
                  {#each account.remote_hosts ?? [] as host (host)}
                    <span class="host">
                      {host}
                      <button
                        type="button"
                        class="host-x"
                        disabled={busy}
                        aria-label={`Stop using ${account.label} on ${host}`}
                        onclick={() => removeHost(account, host)}>×</button
                      >
                    </span>
                  {/each}
                </div>
              {:else if !account.remote_all_hosts}
                <!-- Only when the catch-all is off, or this contradicts the line above it. -->
                <p class="remote-hint">
                  Token ready. Turn on “Use on every SSH host”, or name hosts one at a time —
                  nothing is sent anywhere until you do.
                </p>
              {/if}

              <form
                class="host-add"
                onsubmit={e => {
                  e.preventDefault();
                  void addHost(account);
                }}
              >
                <input
                  type="text"
                  placeholder="hostname or ssh alias"
                  disabled={busy}
                  bind:value={
                    () => hostDraft[account.id] ?? '',
                    v => (hostDraft = { ...hostDraft, [account.id]: v })
                  }
                />
                <Button variant="ghost" disabled={busy}>Add host</Button>
              </form>
            {/if}
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

    <h4>SSH hosts</h4>
    <p>
      An SSH tab runs as the active account only where you have switched that on — per host, or
      everything. It needs a token of its own, which is minted separately and does not expire for
      a year; the dialog that mints it explains what that costs. Hosts you have not enabled keep
      whatever login they already have.
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

{#if minting}
  <AccountRemoteModal
    account={minting}
    oncomplete={async (acct, mint) => {
      // **`acct` comes from the dialog, not from `minting`.** The pane's rows stay in the tab
      // order behind the modal, so a second row's "Set up…" could swap `minting` mid-flight and
      // this would record the token against the wrong account — giving that row an expiry and a
      // host form for a token it does not own, while the real one is left with a live credential
      // and no metadata. Both halves of that are silent (§6.1).
      //
      // Record BEFORE closing, same reason as the setup modal: closing destroys the component
      // that owns the promise, so a failure would have nowhere to report.
      try {
        await patchAccount(acct.id, { token_minted_at: mint.minted_at });
        notice =
          `Remote token ready for ${acct.label}` +
          (mint.identity.email && mint.identity.email !== acct.email
            ? ` — note it resolved as ${mint.identity.email}`
            : '') +
          `. Turn on “Use on every SSH host”, or add hosts one at a time.`;
      } catch (e) {
        // The token IS in the keychain — Rust stores before it returns — so this failure means
        // a live credential with no metadata. Say that, and rely on the strip keying off the
        // vault rather than off state so "Remove token" is still there to clean it up.
        error =
          `${e instanceof Error ? e.message : String(e)} — the token was minted and is in your ` +
          `keychain, but could not be recorded. Use “Remove token” and try again.`;
      }
      await refreshTokenPresence();
      minting = null;
    }}
    oncancel={() => (minting = null)}
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

  /* --- Remote hosts (§6) --- */

  .remote {
    background: var(--bg-dark);
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 10px 10px;
  }

  .remote-head {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .remote-label {
    color: var(--fg-dim);
    font-size: 0.7rem;
    letter-spacing: 0.04em;
    margin-right: auto;
    text-transform: uppercase;
  }

  .remote-hint {
    color: var(--fg-dim);
    font-size: 0.75rem;
    line-height: 1.4;
    margin: 0;
  }

  .all-hosts {
    align-items: center;
    display: flex;
    gap: 10px;
    justify-content: space-between;
  }

  .all-hosts .setting-label {
    font-size: 0.8rem;
  }

  /* Same switch as the pane's main toggle, scaled for a nested row. */
  .toggle.small {
    height: 18px;
    width: 32px;
  }

  .toggle.small .toggle-knob {
    height: 14px;
    width: 14px;
  }

  .toggle.small.active .toggle-knob {
    transform: translateX(14px);
  }

  .hosts {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }

  .host {
    align-items: center;
    background: var(--bg-medium);
    border-radius: 3px;
    color: var(--fg);
    display: inline-flex;
    font-size: 0.75rem;
    gap: 4px;
    /* These are user@host strings in a 200-320px pane. Wrapping beats a tooltip nobody hovers,
       and beats two hosts clipping to the same visible text. */
    overflow-wrap: anywhere;
    padding: 2px 4px 2px 7px;
  }

  .host-x {
    background: none;
    border: none;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 0.85rem;
    line-height: 1;
    padding: 0 3px;
  }

  .host-x:hover:not(:disabled) {
    color: #e06c75;
  }

  .host-x:disabled {
    cursor: default;
    opacity: 0.5;
  }

  .host-add {
    display: flex;
    gap: 4px;
  }

  .host-add input {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    flex: 1;
    font-size: 0.75rem;
    min-width: 0;
    padding: 4px 7px;
  }

  .host-add input:focus {
    border-color: var(--accent);
    outline: none;
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
