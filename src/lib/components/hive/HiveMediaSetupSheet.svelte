<!--
  The media setup sheet (docs/hive-media.md §6): one provider's guide, its credits line and the
  one action its route allows, then Verify. Opened from a "Set up <provider>" button in "What it can
  make" (HiveMediaPanel), so it sits in the Create sheet, Hive settings or Preferences → Hive.

  Props are SEEDS, read once at mount: the panel mounts a fresh sheet for each opening.
    provider         the catalogue provider to set up.
    capabilityLabel  the row it was opened from ("Video"), for the heading only.
    workspaceId      the Hive's workspace (the Create sheet's, about to become one, or Hive
                     settings'); null from Preferences. A server this adds is scoped to it; from
                     Preferences the sheet asks. An install tab never opens in it.
    onclose          the human closed the sheet.
    onchanged        something on this Mac may have changed (a server added, an install ended, a
                     Verify ran): the panel re-fetches.

  Ordering: `alive` goes false at unmount, and every await is followed by a check of it before
  anything is written into the sheet, so an event or answer arriving after the sheet closed writes
  nothing. Writes that must finish whatever the human does (scoping a server just added) are not
  behind that check. Every listener is released at unmount, including one whose `listen` resolves
  after it. No secret is logged, toasted or put in a URL: the pasted key goes from its field to
  `mcpServerAdd`/`mcpServerSetToken` and the field is emptied on success.
-->
<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import { warn as logWarn } from '@tauri-apps/plugin-log';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { McpServerView } from '$lib/tauri/commands';
  import type { HiveMediaInstallFinished, HiveMediaInstallStarted, HiveMediaModelStatus, MoveTargetWindow } from '$lib/tauri/types';
  import type { PanelProvider } from '$lib/hive/media';
  import { externalUrl, unverifiedNote } from '$lib/hive/media';
  import { startSignIn } from '$lib/mcp/addServer';
  import {
    bringsSpeechModel, canVerify, guideFor, installEndLine, newServerScope, reusedServerScope, scopeSentence, serverPlan, setupAction, speechModelLine, verifyView,
    type ScopeChoice, type SetupAction, type VerifyView,
  } from '$lib/hive/mediaSetup';

  interface Props {
    provider: PanelProvider;
    capabilityLabel: string;
    workspaceId: string | null;
    onclose: () => void;
    onchanged: () => void;
  }
  let props: Props = $props();

  // Seeds, taken once.
  const p = untrack(() => props.provider);
  const capLabel = untrack(() => props.capabilityLabel);
  const ws = untrack(() => props.workspaceId);
  const close = untrack(() => props.onclose);
  const changed = untrack(() => props.onchanged);

  const uid = $props.id();
  const titleId = `${uid}-title`;
  const keyId = `${uid}-key`;
  const DEV = import.meta.env.DEV;
  const url = p.route_url;
  const unverified = unverifiedNote(p);
  const speech = bringsSpeechModel(p);

  /** The action can change once: a server whose probe asks for a key rather than a sign-in. */
  let action = $state<SetupAction>(setupAction(p));
  let sheetEl = $state<HTMLDivElement | null>(null);
  let alive = true;
  const offs: UnlistenFn[] = [];

  /** One message about the last step: what happened, or what went wrong. */
  let note = $state<{ text: string; error: boolean } | null>(null);
  let busy = $state(false);
  let verifying = $state(false);
  let result = $state<VerifyView | null>(null);

  // MCP
  let servers = $state<McpServerView[] | null>(null);
  let native = $state<string[]>([]);
  let hiveWorkspaces = $state<string[]>([]);
  let scopeChoice = $state<ScopeChoice>('hives');
  let scopeTouched = false;
  /** The server this sheet added or reused. */
  let serverId = $state<string | null>(null);
  let waitingForSignIn = $state(false);
  let key = $state('');

  // Install
  let started = $state<HiveMediaInstallStarted | null>(null);
  let endLine = $state<string | null>(null);
  let model = $state<HiveMediaModelStatus | null>(null);

  const guide = $derived(guideFor(p, action));
  const isMcp = $derived(action === 'mcp_oauth' || action === 'mcp_token' || action === 'mcp_open');
  const plan = $derived(servers && isMcp ? serverPlan(servers, url, p.id, native, DEV) : null);
  const reused = $derived(plan && 'reuse' in plan ? plan.reuse : null);
  const hiveCount = $derived(hiveWorkspaces.length);
  const modelLine = $derived(model ? speechModelLine(model) : null);
  const signupUrl = externalUrl(p.signup_url);
  const billingUrl = externalUrl(p.billing_url);
  const appUrl = setupAction(p) === 'app' ? externalUrl(url) : null;

  /** A listener that is released at unmount, even one whose `listen` resolves after it. */
  function keep<T>(event: string, on: (payload: T) => void) {
    listen<T>(event, (e) => {
      if (alive) on(e.payload);
    })
      .then((off) => (alive ? offs.push(off) : off()))
      .catch((e) => void logWarn(`[HiveMediaSetupSheet] ${event}: not listening: ${String(e)}`));
  }

  function onWindowFocus() {
    // A local app: the human installed it in another window and came back.
    if (alive && action === 'app') void runVerify();
  }

  let raf = 0;
  onMount(() => {
    // The action first; a guide-only sheet has none, so its first button (Sign up, or Close).
    raf = requestAnimationFrame(() => (sheetEl?.querySelector<HTMLElement>('.act button') ?? sheetEl?.querySelector<HTMLElement>('.links button, .foot button'))?.focus());
    if (isMcp) {
      void loadServers();
      keep<{ id: string; status: { state: string } }>('mcp-proxy-status', (s) => {
        if (s.id !== serverId) return;
        if (waitingForSignIn && s.status.state === 'connected') {
          waitingForSignIn = false;
          note = { text: 'Signed in. Checking it works…', error: false };
          void runVerify();
        }
      });
    }
    if (speech) keep<HiveMediaModelStatus>('hive-media-model-progress', (m) => (model = m));
    if (action === 'app') window.addEventListener('focus', onWindowFocus);
  });

  onDestroy(() => {
    alive = false;
    cancelAnimationFrame(raf);
    window.removeEventListener('focus', onWindowFocus);
    for (const off of offs.splice(0)) off();
  });

  /** After a step, a focused button may have gone: give the keyboard back to the sheet, so Escape
   *  and Tab keep working. */
  function holdFocus() {
    requestAnimationFrame(() => {
      if (alive && sheetEl && !sheetEl.contains(document.activeElement)) sheetEl.focus();
    });
  }

  function fail(e: unknown) {
    if (!alive) return;
    note = { text: e instanceof Error ? e.message : String(e), error: true };
  }

  function hiveIds(windows: MoveTargetWindow[]): string[] {
    return windows.flatMap((w) => w.workspaces.filter((x) => x.hive).map((x) => x.id));
  }

  async function loadServers() {
    try {
      const [list, names, windows] = await Promise.all([
        commands.mcpServersList(),
        commands.mcpNativeNames().catch(() => [] as string[]),
        ws ? Promise.resolve([]) : commands.listMoveTargets().catch(() => []),
      ]);
      if (!alive) return;
      servers = list;
      native = names;
      hiveWorkspaces = hiveIds(windows);
      if (!scopeTouched && hiveWorkspaces.length === 0) scopeChoice = 'every';
    } catch (e) {
      fail(e);
    }
  }

  async function open(link: string | null) {
    if (!link) return;
    try {
      await shellOpen(link);
    } catch (e) {
      void logWarn(`[HiveMediaSetupSheet] could not open ${link}: ${String(e)}`);
      fail(`Couldn't open ${link}.`);
    }
  }

  // ─── Verify ───────────────────────────────────────────────────────────────

  /** A Verify asked for while one runs (the connected event during the human's own Verify, an
   *  install's end) is not dropped: it runs once more after, so the last answer is the newest. */
  let verifyAgain = false;

  async function runVerify() {
    if (verifying) {
      verifyAgain = true;
      return;
    }
    verifying = true;
    verifyAgain = false;
    try {
      const r = await commands.hiveMediaVerify(ws, p.id);
      if (!alive) return;
      result = verifyView(r);
      // "Checking it works…" is answered now; an error note stays.
      if (note && !note.error) note = null;
      if (r.state === 'ready') waitingForSignIn = false;
    } catch (e) {
      if (!alive) return;
      result = { label: "Didn't work", tone: 'bad', message: String(e), link: null, linkLabel: 'Open' };
    } finally {
      if (alive) verifying = false;
    }
    if (!alive) return;
    if (verifyAgain) {
      verifyAgain = false;
      return runVerify();
    }
    changed();
    holdFocus();
  }

  // ─── MCP: the human's add path ────────────────────────────────────────────

  /** Add the server (or reuse the one maiTerm serves for this URL) and scope it. The list is read
   *  again at the click: the one from mount may be stale. Once the add is made, the scope is set
   *  whether or not the sheet is still open: a Hive's server must not stay offered everywhere. */
  async function ensureServer(auth: 'oauth' | 'token' | 'none', token?: string): Promise<McpServerView | null> {
    // From Preferences, the Hives are read again too: a click before the mount's read answered
    // must not find "no Hive yet".
    const [list, names, windows] = await Promise.all([
      commands.mcpServersList(),
      commands.mcpNativeNames().catch(() => [] as string[]),
      ws ? Promise.resolve([]) : commands.listMoveTargets().catch(() => []),
    ]);
    if (!alive) return null;
    servers = list;
    native = names;
    if (!ws) hiveWorkspaces = hiveIds(windows);
    const got = serverPlan(list, url, p.id, names, DEV);
    if ('error' in got) throw new Error(got.error);
    if ('reuse' in got) {
      const s = got.reuse;
      const scope = reusedServerScope(s.workspaces, ws);
      if (scope) await commands.mcpServerSetWorkspaces(s.id, scope);
      if (token !== undefined) {
        if (s.auth !== 'token') throw new Error(`maiTerm already serves ${p.name} as “${s.entry_name}” with a ${s.auth === 'oauth' ? 'sign-in' : 'connection that needs no key'}. Change it in Preferences → MCP servers.`);
        await commands.mcpServerSetToken(s.id, token);
      }
      return s;
    }
    const scope = newServerScope(ws, scopeChoice, hiveWorkspaces);
    if (scope === null) throw new Error('There is no Hive yet. Choose Every workspace, or set this up from a Hive.');
    const s = await commands.mcpServerAdd(got.add, url, auth, { token: token ?? null });
    if (scope.length) await commands.mcpServerSetWorkspaces(s.id, scope);
    return s;
  }

  async function signInFlow() {
    if (serverId) return signInAgain();
    if (busy) return;
    busy = true;
    note = null;
    result = null;
    try {
      // A server maiTerm already serves for this URL needs no probe: what to do is read off the
      // server itself below. `reused` (from the mount's read) is only a hint; ensureServer reads again.
      const existing = reused;
      if (!existing) {
        const probe = await commands.mcpServerProbe(url);
        if (!alive) return;
        if (probe.auth === 'token') {
          action = 'mcp_token';
          note = { text: `${p.name} asks for a key rather than a sign-in. Make one on its page and paste it below.`, error: false };
          return;
        }
        if (probe.auth === 'oauth' && !probe.can_register) {
          throw new Error(`${p.name} doesn't let apps register themselves, so it needs a client id only you can make. Add ${url} in Preferences → MCP servers.`);
        }
        if (probe.auth === 'none') {
          const s = await ensureServer('none');
          if (!alive || !s) return;
          serverId = s.id;
          note = { text: `Added as “${s.entry_name}”. It needs no sign-in. Checking it works…`, error: false };
          await runVerify();
          return;
        }
      }
      const s = await ensureServer('oauth');
      if (!alive || !s) return;
      serverId = s.id;
      changed();
      if (s.status.state === 'connected') {
        note = { text: `maiTerm already serves ${p.name} as “${s.entry_name}”. Checking it works…`, error: false };
        await runVerify();
        return;
      }
      // Reused, and added with a key or with nothing: a sign-in would be refused.
      if (s.auth === 'token') {
        action = 'mcp_token';
        note = { text: `maiTerm serves ${p.name} as “${s.entry_name}” with a key. Paste a new one below.`, error: false };
        return;
      }
      if (s.auth === 'none') {
        note = { text: `maiTerm serves ${p.name} as “${s.entry_name}”; it needs no sign-in. Checking it works…`, error: false };
        await runVerify();
        return;
      }
      await startSignIn(s.id);
      if (!alive) return;
      waitingForSignIn = true;
      note = { text: 'Finish signing in in your browser. This checks it by itself when you’re done, or press Verify.', error: false };
    } catch (e) {
      fail(e);
    } finally {
      if (alive) {
        busy = false;
        holdFocus();
      }
    }
  }

  /** The server is added (or reused) already: only the browser sign-in again. */
  async function signInAgain() {
    if (!serverId || busy) return;
    busy = true;
    note = null;
    try {
      await startSignIn(serverId);
      if (!alive) return;
      waitingForSignIn = true;
      note = { text: 'Finish signing in in your browser. This checks it by itself when you’re done, or press Verify.', error: false };
    } catch (e) {
      fail(e);
    } finally {
      if (alive) {
        busy = false;
        holdFocus();
      }
    }
  }

  async function saveKey(e: SubmitEvent) {
    e.preventDefault();
    const value = key.trim();
    if (busy || !value) return;
    busy = true;
    note = null;
    result = null;
    try {
      const s = await ensureServer('token', value);
      if (!alive || !s) return;
      serverId = s.id;
      key = '';
      note = { text: `Key saved for “${s.entry_name}”. Checking it works…`, error: false };
      await runVerify();
    } catch (err) {
      fail(err);
    } finally {
      if (alive) {
        busy = false;
        holdFocus();
      }
    }
  }

  async function addOpen() {
    if (busy) return;
    busy = true;
    note = null;
    result = null;
    try {
      const s = await ensureServer('none');
      if (!alive || !s) return;
      serverId = s.id;
      note = { text: `Added as “${s.entry_name}”. Checking it works…`, error: false };
      await runVerify();
    } catch (e) {
      fail(e);
    } finally {
      if (alive) {
        busy = false;
        holdFocus();
      }
    }
  }

  // ─── brew and plugin: a visible tab ───────────────────────────────────────

  function onFinished(f: HiveMediaInstallFinished) {
    endLine = installEndLine(f);
    // Rust starts the download after this event, and sends no progress when the model is already
    // there: ask for its state (which joins a running download) rather than guess "downloading".
    if (f.model_download) void readModel();
    void runVerify();
  }

  async function readModel() {
    try {
      const m = await commands.hiveMediaWhisperModelDownload();
      if (alive) model = m;
    } catch (e) {
      fail(e);
    }
  }

  async function install() {
    if (busy) return;
    busy = true;
    note = null;
    result = null;
    endLine = null;
    started = null;
    // Listen BEFORE asking: a fast failure can end before the call answers. Until the tab is known,
    // an end for this provider is held, then matched to the tab.
    let tabId: string | null = null;
    let early: HiveMediaInstallFinished | null = null;
    try {
      const off = await listen<HiveMediaInstallFinished>('hive-media-install-finished', (e) => {
        const f = e.payload;
        if (!alive || f.provider_id !== p.id) return;
        if (tabId === null) early = f;
        else if (f.tab_id === tabId) onFinished(f);
      });
      if (!alive) {
        off();
        return;
      }
      offs.push(off);
      const s = await commands.hiveMediaInstall(p.id, ws);
      if (!alive) return;
      started = s;
      tabId = s.tab_id;
      const held = early as HiveMediaInstallFinished | null;
      if (held && held.tab_id === s.tab_id) onFinished(held);
      changed();
    } catch (e) {
      fail(e);
    } finally {
      if (alive) {
        busy = false;
        holdFocus();
      }
    }
  }

  async function showTab() {
    const id = started?.tab_id;
    if (!id) return;
    const { navigateToTab } = await import('$lib/stores/workspaces.svelte');
    await navigateToTab(id).catch((e) => fail(e));
  }

  async function retryModel() {
    if (busy) return;
    busy = true;
    try {
      const m = await commands.hiveMediaWhisperModelDownload();
      if (alive) model = m;
    } catch (e) {
      fail(e);
    } finally {
      if (alive) busy = false;
    }
  }

  // ─── The sheet ────────────────────────────────────────────────────────────

  /** Tab wraps inside the sheet: behind it sit the Create sheet's or Preferences' own controls,
   *  where an Escape would close that instead, and a Space would flip a pick nobody can see. */
  function trapTab(e: KeyboardEvent) {
    if (!sheetEl) return;
    const items = [...sheetEl.querySelectorAll<HTMLElement>('button, a[href], input, select, textarea, [tabindex]:not([tabindex="-1"])')].filter(
      (el) => !el.hasAttribute('disabled') && el.offsetParent !== null,
    );
    if (items.length === 0) {
      e.preventDefault();
      sheetEl.focus();
      return;
    }
    const first = items[0];
    const last = items[items.length - 1];
    const at = document.activeElement;
    if (e.shiftKey && (at === first || at === sheetEl)) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && at === last) {
      e.preventDefault();
      first.focus();
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Tab') {
      trapTab(e);
    } else if (e.key === 'Escape') {
      e.stopPropagation();
      e.preventDefault();
      close();
    } else if (e.key === 'Enter') {
      // The Create sheet takes Enter in a line field as "next step": not from in here.
      e.stopPropagation();
    }
  }
</script>

<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" role="presentation" onclick={(e) => { if (e.target === e.currentTarget) close(); }}>
  <div class="sheet" role="dialog" aria-modal="true" aria-labelledby={titleId} tabindex="-1" bind:this={sheetEl} onkeydown={onKeydown}>
    <div class="head">
      <h3 id={titleId}>Set up {p.name}</h3>
      <span class="for">{capLabel}</span>
      <button type="button" class="x" aria-label="Close" onclick={close}>&times;</button>
    </div>

    {#if p.creditsLine}<p class="credits">{p.creditsLine}</p>{/if}
    {#if unverified}<p class="unv">{unverified}</p>{/if}

    {#if guide.steps.length}
      <ol class="guide">
        {#each guide.steps as step, i (i)}<li>{step}</li>{/each}
      </ol>
    {/if}
    {#if (signupUrl && action !== 'mcp_token') || billingUrl}
      <p class="links">
        {#if signupUrl && action !== 'mcp_token'}<button type="button" class="link" onclick={() => open(signupUrl)}>Sign up</button>{/if}
        {#if billingUrl}<button type="button" class="link" onclick={() => open(billingUrl)}>Billing</button>{/if}
      </p>
    {/if}

    <div class="act">
      {#if action === 'mcp_oauth' || action === 'mcp_open' || action === 'mcp_token'}
        {#if reused}
          <p class="dim">maiTerm already serves this as “{reused.entry_name}”: setting up uses that server{ws && reused.workspaces.length && !reused.workspaces.includes(ws) ? ' and offers it to this Hive too' : ''}.</p>
        {:else}
          <p class="dim">maiTerm adds {p.name}’s MCP server (<span class="url">{url}</span>) and serves it to your agents{action === 'mcp_oauth' ? ', then opens its sign-in in your browser' : ''}.</p>
          {#if ws}
            <p class="dim">{scopeSentence(ws, scopeChoice, hiveCount)}</p>
          {:else}
            <fieldset class="scope">
              <legend>Which workspaces get its tools?</legend>
              <label class="choice">
                <input type="radio" name="{uid}-scope" value="hives" bind:group={scopeChoice} disabled={hiveCount === 0} onchange={() => (scopeTouched = true)} />
                <span>Hive workspaces only{hiveCount === 0 ? ' (no Hive yet)' : ''}</span>
              </label>
              <label class="choice">
                <input type="radio" name="{uid}-scope" value="every" bind:group={scopeChoice} onchange={() => (scopeTouched = true)} />
                <span>Every workspace</span>
              </label>
              <p class="dim">{scopeSentence(null, scopeChoice, hiveCount)}</p>
            </fieldset>
          {/if}
        {/if}
      {/if}

      {#if action === 'mcp_oauth'}
        <div class="row">
          <Button variant="primary" aria-disabled={busy} onclick={signInFlow}>{busy ? 'Working…' : serverId ? 'Sign in again' : reused?.status.state === 'connected' ? 'Use it' : 'Sign in'}</Button>
        </div>
      {:else if action === 'mcp_open'}
        <div class="row"><Button variant="primary" aria-disabled={busy} onclick={addOpen}>{busy ? 'Working…' : reused ? 'Use it' : `Add ${p.name}`}</Button></div>
      {:else if action === 'mcp_token'}
        {#if signupUrl}
          <div class="row"><Button variant="secondary" onclick={() => open(signupUrl)}>Open {p.name} to make a key</Button></div>
        {/if}
        <form class="key" onsubmit={saveKey}>
          <label for={keyId}>Paste the key</label>
          <div class="row">
            <input id={keyId} type="password" bind:value={key} autocomplete="off" spellcheck="false" />
            <Button variant="primary" type="submit" aria-disabled={busy || !key.trim()}>{busy ? 'Saving…' : 'Save key'}</Button>
          </div>
          <p class="dim">It goes to your keychain and only to {p.name}: never into an agent’s config.</p>
        </form>
      {:else if action === 'install'}
        {#if started}
          <p class="running">
            {endLine ? 'Ran' : 'Running'} in a tab: <code>{started.command_shown}</code>
          </p>
          {#if ws}<button type="button" class="link" onclick={showTab}>Show the tab</button>{:else}<p class="dim">It’s in your maiTerm window.</p>{/if}
          {#if endLine}
            <p class="dim" role="status">{endLine}</p>
            {#if result?.tone !== 'ok'}<div class="row"><Button variant="secondary" aria-disabled={busy} onclick={install}>Run it again</Button></div>{/if}
          {/if}
          {#if !started.watches_exit && !endLine}
            <p class="dim">maiTerm can’t see this shell finish. Press Re-detect when it’s done.</p>
          {/if}
        {:else}
          <div class="row"><Button variant="primary" aria-disabled={busy} onclick={install}>{busy ? 'Opening a tab…' : `Install ${p.name}`}</Button></div>
          <p class="dim">It runs in a terminal tab you can watch{ws ? ', outside this Hive' : ''}.</p>
        {/if}
        {#if modelLine}
          <div class="row" role="status">
            <span class="dim" class:bad={modelLine.retry}>{modelLine.text}</span>
            {#if modelLine.retry}<button type="button" class="link" onclick={retryModel}>Try again</button>{/if}
          </div>
        {/if}
      {:else if action === 'app'}
        {#if appUrl}<div class="row"><Button variant="primary" onclick={() => open(appUrl)}>Download {p.name}</Button></div>{/if}
        <p class="dim">maiTerm checks again when you come back to this window.</p>
      {:else if guide.lead}
        <p class="dim">{guide.lead}</p>
      {/if}
    </div>

    {#if note}
      <p class="note" class:bad={note.error} role={note.error ? 'alert' : 'status'}>{note.text}</p>
    {/if}

    {#if result}
      <div class="result" role="status">
        <span class="state {result.tone}">{result.label}</span>
        <span class="msg">{result.message}</span>
        {#if result.link}<button type="button" class="link" onclick={() => open(result?.link ?? null)}>{result.linkLabel}</button>{/if}
      </div>
    {/if}

    <div class="foot">
      {#if canVerify(p)}
        <Button variant="ghost" aria-disabled={verifying} onclick={() => { if (!verifying) void runVerify(); }}>
          {verifying ? 'Checking…' : action === 'install' && started && !started.watches_exit ? 'Re-detect' : 'Verify'}
        </Button>
      {/if}
      <Button variant="ghost" onclick={close}>Close</Button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4); display: flex; justify-content: center; align-items: flex-start; padding: 12vh 16px 16px; z-index: 1100; overflow-y: auto; }
  .sheet { box-sizing: border-box; display: flex; flex-direction: column; gap: 8px; width: min(480px, 100%); min-width: 0; padding: 14px 16px; background: var(--bg-medium); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 8px; box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4); }
  .sheet:focus { outline: none; }
  .sheet :global(.btn[aria-disabled='true']) { opacity: 0.5; cursor: default; }
  .head { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 10px; min-width: 0; }
  h3 { margin: 0; font-size: 1.077rem; font-weight: 600; overflow-wrap: anywhere; }
  .for { flex: 1; font-size: 0.846rem; color: var(--fg-dim); overflow-wrap: anywhere; }
  .x { background: none; border: none; color: var(--fg-dim); font-size: 1.385rem; line-height: 1; padding: 0 4px; cursor: pointer; }
  .x:hover { color: var(--fg); }
  p { margin: 0; overflow-wrap: anywhere; }
  .credits { font-size: 0.923rem; }
  .unv { font-size: 0.769rem; font-style: italic; color: var(--fg-dim); }
  .guide { margin: 0; padding-left: 20px; font-size: 0.923rem; overflow-wrap: anywhere; }
  .guide li { padding: 1px 0; }
  .links, .row { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 14px; min-width: 0; }
  .act { display: flex; flex-direction: column; gap: 8px; padding-top: 8px; border-top: 1px solid var(--bg-light); min-width: 0; }
  .dim { font-size: 0.846rem; color: var(--fg-dim); }
  .url, code { font-family: var(--font-mono, monospace); font-size: 0.808rem; overflow-wrap: anywhere; }
  .running { font-size: 0.923rem; }
  .running code { color: var(--fg); background: var(--bg-dark); padding: 1px 4px; border-radius: 3px; }
  .link { background: none; border: none; padding: 0; color: var(--accent); font: inherit; font-size: 0.846rem; cursor: pointer; text-align: left; }
  .link:hover { text-decoration: underline; }
  .link:focus-visible, .x:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }
  .scope { margin: 0; padding: 6px 8px; display: flex; flex-direction: column; gap: 4px; border: 1px solid var(--bg-light); border-radius: 5px; min-width: 0; }
  .scope legend { padding: 0 4px; font-size: 0.808rem; color: var(--fg-dim); }
  .choice { display: flex; align-items: center; gap: 6px; font-size: 0.923rem; cursor: pointer; }
  .choice input { accent-color: var(--accent); }
  .key { display: flex; flex-direction: column; gap: 4px; min-width: 0; }
  .key label { font-size: 0.846rem; }
  .key input { flex: 1 1 12em; min-width: 0; padding: 5px 8px; background: var(--bg-dark); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 5px; font: inherit; }
  .key input:focus { outline: none; border-color: var(--accent); }
  .note { font-size: 0.846rem; color: var(--fg); }
  .bad { color: var(--red); }
  .result { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 8px; padding: 6px 8px; background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; font-size: 0.846rem; min-width: 0; }
  .msg { flex: 1 1 14em; min-width: 0; overflow-wrap: anywhere; }
  .state { font-size: 0.769rem; padding: 0 6px; border-radius: 8px; border: 1px solid var(--bg-light); color: var(--fg-dim); overflow-wrap: anywhere; }
  .state.ok { color: var(--green); border-color: var(--green); }
  .state.warn { color: var(--yellow); border-color: var(--yellow); }
  .state.bad { color: var(--red); border-color: var(--red); }
  .foot { display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; padding-top: 4px; }
</style>
