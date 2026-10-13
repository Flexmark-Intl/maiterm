<script lang="ts">
  /** Preferences → MCP servers (docs/mcp-proxy.md §7).
   *
   *  maiTerm signs in to a server once and serves its tools to every agent, each server as its
   *  own entry in the agents' configs. Everything here is Rust-owned (`mcp_proxy`): this pane
   *  only asks and draws, and redraws on `mcp-proxy-status` / `mcp-proxy-changed`.
   *
   *  A sign-in opens the system browser and finishes on maiTerm's own callback, so the pane only
   *  learns of it through the status event. */
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import Button from '$lib/components/ui/Button.svelte';
  import StatusDot from '$lib/components/ui/StatusDot.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { McpProbe, McpServerView } from '$lib/tauri/commands';
  import type { MoveTargetWindow } from '$lib/tauri/types';

  let servers = $state<McpServerView[]>([]);
  let windows = $state<MoveTargetWindow[]>([]);
  let loaded = $state(false);
  /** One message at a time, drawn on the card it is about (null: the add form). */
  let feedback = $state<{ id: string | null; text: string; error: boolean } | null>(null);
  let busyId = $state<string | null>(null);

  // Add form
  let adding = $state(false);
  let url = $state('');
  let name = $state('');
  let nameTouched = $state(false);
  let probe = $state<McpProbe | null>(null);
  let probing = $state(false);
  let token = $state('');
  let clientId = $state('');
  let clientSecret = $state('');
  /** On an OAuth server that can't register maiTerm: paste a token instead. */
  let useToken = $state(false);

  // Per-card inline states
  let confirmRemove = $state<string | null>(null);
  let confirmAdopt = $state<string | null>(null);
  let tokenFor = $state<string | null>(null);
  let newToken = $state('');
  let scopeFor = $state<string | null>(null);
  let toolsFor = $state<string | null>(null);

  async function load() {
    try {
      servers = await commands.mcpServersList();
    } catch (e) {
      feedback = { id: null, text: String(e), error: true };
    }
    loaded = true;
  }

  onMount(() => {
    void load();
    commands.listMoveTargets().then((w) => (windows = w)).catch(() => {});
    const offs = [listen('mcp-proxy-status', () => void load()), listen('mcp-proxy-changed', () => void load())];
    return () => offs.forEach((p) => p.then((off) => off()));
  });

  /** A name from the URL's host: mcp.linear.app → linear, api.githubcopilot.com → githubcopilot. */
  function suggestName(u: string): string {
    try {
      const host = new URL(u).hostname;
      if (/^[\d.]+$/.test(host) || host === 'localhost') return 'local';
      const labels = host.split('.').filter((l) => !['mcp', 'api', 'www'].includes(l));
      const pick = labels.length > 1 ? labels[labels.length - 2] : labels[0] ?? '';
      return pick.toLowerCase().replace(/[^a-z0-9_-]/g, '-').replace(/^-+/, '').slice(0, 40);
    } catch {
      return '';
    }
  }

  /** Which check is current: a probe can take many seconds, and one that answers after the URL
   *  was edited (or after a newer check) describes another URL, so it is dropped. Otherwise a
   *  server could be added with the auth of the URL typed before it, its token sent there. */
  let probeSeq = 0;

  async function check() {
    if (probing) return;
    const seq = ++probeSeq;
    const target = url.trim();
    feedback = null;
    probe = null;
    probing = true;
    try {
      const p = await commands.mcpServerProbe(target);
      if (seq !== probeSeq || url.trim() !== target) return;
      probe = p;
      if (!nameTouched) name = suggestName(target);
    } catch (e) {
      if (seq === probeSeq) feedback = { id: null, text: String(e), error: true };
    } finally {
      if (seq === probeSeq) probing = false;
    }
  }

  function resetForm() {
    adding = false;
    url = '';
    name = '';
    nameTouched = false;
    probe = null;
    token = '';
    clientId = '';
    clientSecret = '';
    useToken = false;
  }

  async function add() {
    if (!probe) return;
    const auth = probe.auth === 'oauth' && useToken ? 'token' : probe.auth;
    busyId = '+';
    feedback = null;
    try {
      const s = await commands.mcpServerAdd(name.trim(), url.trim(), auth, { token, clientId, clientSecret });
      resetForm();
      await load();
      if (auth === 'oauth') await signIn(s);
    } catch (e) {
      feedback = { id: null, text: String(e), error: true };
    } finally {
      busyId = null;
    }
  }

  async function signIn(s: McpServerView) {
    busyId = s.id;
    feedback = null;
    try {
      const authUrl = await commands.mcpServerSignIn(s.id);
      await shellOpen(authUrl);
      feedback = { id: s.id, text: 'Finish signing in in your browser. This updates when you’re done.', error: false };
    } catch (e) {
      feedback = { id: s.id, text: String(e), error: true };
    } finally {
      busyId = null;
    }
  }

  async function act(s: McpServerView, f: () => Promise<unknown>, done?: string) {
    busyId = s.id;
    feedback = null;
    try {
      await f();
      if (done) feedback = { id: s.id, text: done, error: false };
      await load();
    } catch (e) {
      feedback = { id: s.id, text: String(e), error: true };
    } finally {
      busyId = null;
    }
  }

  function dot(state: string): 'green' | 'yellow' | 'red' | 'dim' | 'accent' {
    switch (state) {
      case 'connected': return 'green';
      case 'needs_sign_in': return 'yellow';
      case 'unreachable':
      case 'error': return 'red';
      case 'connecting': return 'accent';
      default: return 'dim';
    }
  }

  function stateLabel(s: McpServerView): string {
    const st = s.status;
    switch (st.state) {
      case 'connected': return `Connected · ${s.tools.length} tool${s.tools.length === 1 ? '' : 's'}`;
      case 'connecting': return 'Connecting…';
      case 'needs_sign_in': return `Needs ${s.auth === 'token' ? 'a new token' : 'a sign-in'}${st.detail ? ` — ${st.detail}` : ''}`;
      case 'unreachable': return `Unreachable${st.detail ? ` — ${st.detail}` : ''}. Its tools stay listed.`;
      case 'disabled': return 'Off';
      default: return st.detail || st.state;
    }
  }

  const allWorkspaces = $derived(windows.flatMap((w) => w.workspaces.map((ws) => ({ ...ws, window: w.name ?? w.active_workspace_name ?? 'Window' }))));

  function scopeLabel(s: McpServerView): string {
    if (s.workspaces.length === 0) return 'Every workspace';
    const names = s.workspaces.map((id) => allWorkspaces.find((w) => w.id === id)?.name ?? 'a removed workspace');
    return names.join(', ');
  }

  function toggleScope(s: McpServerView, wsId: string) {
    const next = s.workspaces.includes(wsId) ? s.workspaces.filter((w) => w !== wsId) : [...s.workspaces, wsId];
    void act(s, () => commands.mcpServerSetWorkspaces(s.id, next));
  }

  const canAdd = $derived(
    !!probe && /^[a-z0-9][a-z0-9_-]{0,39}$/.test(name.trim()) &&
      ((probe.auth === 'token' || (probe.auth === 'oauth' && useToken)) ? token.trim().length > 0
        : probe.auth === 'oauth' && !probe.can_register ? clientId.trim().length > 0
        : true),
  );
</script>

<div class="pane">
  <h3>MCP servers</h3>
  <p class="hint">
    Sign in to an MCP server once in maiTerm and every agent can use it: Claude and Codex here, and
    Claude on your ssh hosts. On a shared host, your agents use your sign-in. maiTerm holds it and
    keeps it fresh. Each server appears to agents as its own entry, with its tools as the server
    publishes them. Servers installed directly in an agent keep working as before.
  </p>
  <p class="hint">
    Every tool of a server here loads into every agent session in its workspaces, so add the ones
    you use. Agents can propose a server, but only you can add one.
  </p>

  {#if loaded && servers.length === 0 && !adding}
    <div class="empty">
      <p>No servers yet.</p>
      <Button variant="primary" onclick={() => (adding = true)}>Add a server</Button>
    </div>
  {/if}

  {#if servers.length > 0}
    <ul class="cards">
      {#each servers as s (s.id)}
        <li class="card" class:off={!s.enabled}>
          <div class="card-head">
            <div class="card-title">
              <StatusDot color={dot(s.status.state)} pulse={s.status.state === 'connecting'} />
              <span class="name">{s.entry_name}</span>
              <span class="kind">{s.auth === 'oauth' ? 'sign-in' : s.auth === 'token' ? 'token' : 'no sign-in'}</span>
            </div>
            <button
              class="toggle"
              class:active={s.enabled}
              aria-label={s.enabled ? `Turn ${s.id} off` : `Turn ${s.id} on`}
              disabled={busyId === s.id}
              onclick={() => act(s, () => commands.mcpServerSetEnabled(s.id, !s.enabled))}
            ><span class="toggle-knob"></span></button>
          </div>
          <p class="sub url">{s.url}</p>
          <p class="sub" class:warn={s.status.state === 'needs_sign_in'} class:bad={s.status.state === 'unreachable' || s.status.state === 'error'}>
            {stateLabel(s)}
          </p>

          {#if s.collisions.length > 0}
            <p class="sub warn">
              {s.collisions.join(' and ')} already {s.collisions.length > 1 ? 'have' : 'has'} a server of your own named
              “{s.entry_name}”, so maiTerm’s isn’t written there. Rename one, or move it to maiTerm.
            </p>
          {/if}
          {#if s.duplicates.length > 0}
            <p class="sub warn">
              Also installed directly ({s.duplicates.join('; ')}): agents get every tool twice.
            </p>
          {/if}

          <div class="card-actions">
            {#if s.auth === 'oauth'}
              <Button variant={s.status.state === 'needs_sign_in' ? 'primary' : 'ghost'} disabled={busyId === s.id} onclick={() => signIn(s)}>
                {s.status.state === 'needs_sign_in' ? 'Sign in' : 'Sign in again'}
              </Button>
            {:else if s.auth === 'token'}
              <Button variant={s.status.state === 'needs_sign_in' ? 'primary' : 'ghost'} onclick={() => { tokenFor = tokenFor === s.id ? null : s.id; newToken = ''; }}>
                Replace token
              </Button>
            {/if}
            <Button variant="ghost" disabled={busyId === s.id || !s.enabled} onclick={() => act(s, () => commands.mcpServerRefresh(s.id), 'Tool list read again.')}>Refresh</Button>
            {#if s.tools.length > 0}
              <Button variant="ghost" onclick={() => (toolsFor = toolsFor === s.id ? null : s.id)}>Tools</Button>
            {/if}
            <Button variant="ghost" onclick={() => (scopeFor = scopeFor === s.id ? null : s.id)}>Workspaces</Button>
            {#if s.duplicates.length > 0 || s.collisions.length > 0}
              <Button variant="ghost" onclick={() => (confirmAdopt = s.id)}>Move to maiTerm</Button>
            {/if}
            <Button variant="ghost" onclick={() => (confirmRemove = s.id)}>Remove</Button>
          </div>

          {#if tokenFor === s.id}
            <div class="inline">
              <input type="password" placeholder="New token" bind:value={newToken} autocomplete="off" spellcheck="false" />
              <Button variant="primary" disabled={!newToken.trim() || busyId === s.id}
                onclick={() => act(s, async () => { await commands.mcpServerSetToken(s.id, newToken); tokenFor = null; newToken = ''; }, 'Token replaced.')}>Save</Button>
            </div>
          {/if}

          {#if toolsFor === s.id}
            <p class="sub tools">{s.tools.join(', ')}</p>
          {/if}

          {#if scopeFor === s.id}
            <div class="scope">
              <p class="sub">Offered to: <strong>{scopeLabel(s)}</strong>. Tick workspaces to limit it; none ticked means every workspace.</p>
              {#each allWorkspaces as ws (ws.id)}
                <label class="check">
                  <input type="checkbox" checked={s.workspaces.includes(ws.id)} disabled={busyId === s.id} onchange={() => toggleScope(s, ws.id)} />
                  <span>{ws.name}</span>
                </label>
              {/each}
            </div>
          {:else}
            <p class="sub">Workspaces: {scopeLabel(s)}</p>
          {/if}

          {#if confirmAdopt === s.id}
            <div class="confirm">
              <p class="sub">
                Remove your own entries for this server ({[...s.duplicates, ...s.collisions.map((r) => `${r}: ${s.entry_name}`)].join('; ')})
                so agents get it once, through maiTerm? Their sign-ins stay where they are; nothing is revoked.
              </p>
              <div class="card-actions">
                <Button variant="primary" disabled={busyId === s.id}
                  onclick={() => act(s, async () => { const gone = await commands.mcpServerAdoptNative(s.id); confirmAdopt = null; return gone; }, 'Moved to maiTerm. New agent sessions get it once.')}>Move to maiTerm</Button>
                <Button variant="ghost" onclick={() => (confirmAdopt = null)}>Cancel</Button>
              </div>
            </div>
          {/if}

          {#if confirmRemove === s.id}
            <div class="confirm">
              <p class="sub">
                Remove {s.id}? Agents stop seeing it in new sessions, and maiTerm forgets its sign-in. The
                service itself isn’t told, so revoke access there too if you want it gone.
              </p>
              <div class="card-actions">
                <Button variant="primary" disabled={busyId === s.id}
                  onclick={() => act(s, async () => { await commands.mcpServerRemove(s.id); confirmRemove = null; })}>Remove</Button>
                <Button variant="ghost" onclick={() => (confirmRemove = null)}>Cancel</Button>
              </div>
            </div>
          {/if}

          {#if feedback && feedback.id === s.id}
            <p class="sub" class:bad={feedback.error}>{feedback.text}</p>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}

  {#if servers.length > 0 && !adding}
    <div><Button variant="secondary" onclick={() => (adding = true)}>Add a server</Button></div>
  {/if}

  {#if adding}
    <div class="add">
      <label class="field">
        <span>Server URL</span>
        <div class="inline">
          <input type="url" placeholder="https://mcp.example.com/mcp" bind:value={url} spellcheck="false"
            onkeydown={(e) => { if (e.key === 'Enter' && url.trim()) void check(); }}
            oninput={() => { probe = null; probeSeq++; probing = false; }} />
          <Button variant="secondary" disabled={!url.trim() || probing} onclick={check}>{probing ? 'Checking…' : 'Check'}</Button>
        </div>
      </label>

      {#if probe}
        <p class="sub">{probe.detail}</p>
        <label class="field">
          <span>Name agents see it as</span>
          <input type="text" bind:value={name} spellcheck="false" oninput={() => (nameTouched = true)} />
        </label>

        {#if probe.auth === 'oauth'}
          <label class="check">
            <input type="checkbox" bind:checked={useToken} />
            <span>Use a token instead of signing in</span>
          </label>
        {/if}

        {#if probe.auth === 'token' || (probe.auth === 'oauth' && useToken)}
          <label class="field">
            <span>Token (an API key or access token from the service)</span>
            <input type="password" bind:value={token} autocomplete="off" spellcheck="false" />
          </label>
        {:else if probe.auth === 'oauth' && !probe.can_register}
          <label class="field">
            <span>Client id (from an app you registered with {probe.issuer})</span>
            <input type="text" bind:value={clientId} spellcheck="false" />
          </label>
          <label class="field">
            <span>Client secret, if the app has one</span>
            <input type="password" bind:value={clientSecret} autocomplete="off" spellcheck="false" />
          </label>
          <p class="sub">The app’s callback URL must allow http://127.0.0.1 on any port.</p>
        {/if}
      {/if}

      <div class="card-actions">
        <Button variant="primary" disabled={!canAdd || busyId === '+'} onclick={add}>
          {probe?.auth === 'oauth' && !useToken ? 'Add and sign in' : 'Add'}
        </Button>
        <Button variant="ghost" onclick={resetForm}>Cancel</Button>
      </div>
      {#if feedback && feedback.id === null}
        <p class="sub" class:bad={feedback.error}>{feedback.text}</p>
      {/if}
    </div>
  {:else if feedback && feedback.id === null}
    <p class="sub" class:bad={feedback.error}>{feedback.text}</p>
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
    margin: 0;
  }

  .cards {
    display: flex;
    flex-direction: column;
    gap: 10px;
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .card {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 12px 14px;
  }

  .card.off {
    opacity: 0.7;
  }

  .card-head {
    align-items: center;
    display: flex;
    gap: 12px;
    justify-content: space-between;
  }

  .card-title {
    align-items: center;
    display: flex;
    gap: 8px;
    min-width: 0;
  }

  .name {
    color: var(--fg);
    font-size: 0.9rem;
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .kind {
    color: var(--fg-dim);
    font-size: 0.7rem;
  }

  .sub {
    color: var(--fg-dim);
    font-size: 0.75rem;
    line-height: 1.4;
    margin: 0;
    overflow-wrap: anywhere;
  }

  .sub.url {
    font-family: var(--font-mono, monospace);
  }

  .sub.warn {
    color: var(--yellow);
  }

  .sub.bad {
    color: var(--red, #f7768e);
  }

  .card-actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .card-actions :global(.btn) {
    font-size: 0.75rem;
    padding: 3px 10px;
  }

  .inline {
    align-items: center;
    display: flex;
    gap: 8px;
  }

  .inline input {
    flex: 1;
    min-width: 0;
  }

  input[type='text'],
  input[type='url'],
  input[type='password'] {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    font-size: 0.8rem;
    padding: 5px 8px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .field > span {
    color: var(--fg);
    font-size: 0.8rem;
  }

  .check {
    align-items: center;
    color: var(--fg);
    display: flex;
    font-size: 0.8rem;
    gap: 6px;
  }

  .scope,
  .confirm,
  .add {
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding-top: 8px;
  }

  .add {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    gap: 10px;
    padding: 12px 14px;
  }

  .tools {
    font-family: var(--font-mono, monospace);
  }

  /* The app-standard switch, restated (scoped styles). */
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
</style>
