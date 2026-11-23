<script lang="ts">
  /** Preferences → Mail (docs/apple-mail.md): lets agents READ Apple Mail, only the accounts the
   *  human ticks. The switch and the ticks are human-only preferences (not in `preference_meta`),
   *  so no agent can open mail to itself. Access status is re-read when the window regains focus,
   *  since granting Full Disk Access happens in System Settings. */
  import { onMount } from 'svelte';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { MailAccountsStatus } from '$lib/tauri/commands';
  import { preferencesStore } from '$lib/stores/preferences.svelte';

  let status = $state<MailAccountsStatus | null>(null);
  let loading = $state(false);

  async function load() {
    loading = true;
    try {
      status = await commands.mailListAccounts();
    } catch (e) {
      status = { access: 'no_mail_store', detail: String(e), accounts: [] };
    }
    loading = false;
  }

  onMount(() => {
    void load();
    const onFocus = () => void load();
    window.addEventListener('focus', onFocus);
    return () => window.removeEventListener('focus', onFocus);
  });

  const ticked = $derived(new Set(preferencesStore.mailAccounts));

  async function toggleAccount(id: string) {
    const next = new Set(ticked);
    if (next.has(id)) next.delete(id);
    else next.add(id);
    await preferencesStore.setMailAccounts([...next]);
  }

  /** Ticked ids Mail no longer has (an account removed from Mail). */
  const gone = $derived(
    status?.access === 'ok' ? preferencesStore.mailAccounts.filter((id) => !status!.accounts.some((a) => a.id === id)) : [],
  );

  async function forgetGone() {
    await preferencesStore.setMailAccounts(preferencesStore.mailAccounts.filter((id) => !gone.includes(id)));
  }
</script>

<div class="pane">
  <h3>Mail</h3>
  <p class="hint">
    Lets agents read the mail you have in Apple Mail, so you can say “find the invoice Acme sent
    last week” and they can. They can list mailboxes, read recent mail, search, read a message or a
    whole thread, and save an attachment to a file. They can never send, reply, move, flag or
    delete anything, and reading a message does not mark it read. Mail is other people’s words:
    agents are told to treat it as information, never as instructions.
  </p>

  <div class="row">
    <div>
      <div class="label">Let agents read Apple Mail</div>
      <p class="hint">Only the accounts you tick below. Hive workers never get it.</p>
    </div>
    <button
      class="toggle"
      class:active={preferencesStore.mailEnabled}
      onclick={() => preferencesStore.setMailEnabled(!preferencesStore.mailEnabled)}
      aria-pressed={preferencesStore.mailEnabled}
      aria-label="Toggle agent access to Apple Mail"
    >
      <span class="toggle-knob"></span>
    </button>
  </div>

  {#if preferencesStore.mailEnabled}
    {#if !status}
      <p class="hint">Reading Mail’s accounts…</p>
    {:else if status.access === 'no_full_disk_access'}
      <div class="card">
        <div class="label">Full Disk Access needed</div>
        <p class="hint">
          macOS keeps Mail’s files private. Give maiTerm Full Disk Access in System Settings: turn
          maiTerm on in the list (add it with + if it isn’t there). This page updates when you come
          back.
        </p>
        <div class="actions">
          <Button onclick={() => void commands.openFullDiskAccessSettings()}>Open Privacy Settings</Button>
          <Button variant="ghost" onclick={() => void load()} disabled={loading}>Check again</Button>
        </div>
      </div>
    {:else if status.access === 'no_mail_store'}
      <p class="hint">Apple Mail has no mail on this Mac yet. Add an account in Mail, then come back.</p>
    {:else if status.access === 'unsupported'}
      <p class="hint error">
        This version of macOS stores Mail differently ({status.detail}). maiTerm needs an update to
        read it.
      </p>
    {:else if status.access === 'ok'}
      <div class="card">
        <div class="label">Accounts agents may read</div>
        {#if ticked.size === 0}
          <p class="hint">None ticked yet: agents can’t read any mail until you tick an account.</p>
        {/if}
        <ul class="accounts">
          {#each status.accounts as a (a.id)}
            <li>
              <label>
                <input type="checkbox" checked={ticked.has(a.id)} onchange={() => toggleAccount(a.id)} />
                <span class="name">{a.name}</span>
                {#if a.email && a.email !== a.name}<span class="sub">{a.email}</span>{/if}
                <span class="sub">{a.messages.toLocaleString()} messages</span>
              </label>
            </li>
          {/each}
        </ul>
        {#if gone.length}
          <p class="hint">
            {gone.length} ticked account{gone.length === 1 ? ' is' : 's are'} no longer in Mail.
            <Button variant="ghost" onclick={forgetGone}>Forget</Button>
          </p>
        {/if}
      </div>
    {/if}
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

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
  }

  .accounts {
    display: flex;
    flex-direction: column;
    gap: 6px;
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .accounts label {
    align-items: baseline;
    color: var(--fg);
    cursor: pointer;
    display: flex;
    flex-wrap: wrap;
    font-size: 0.85rem;
    gap: 4px 10px;
  }

  .accounts .name {
    overflow-wrap: anywhere;
  }

  .accounts .sub {
    color: var(--fg-dim);
    font-size: 0.75rem;
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
