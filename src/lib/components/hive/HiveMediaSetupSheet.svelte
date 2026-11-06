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
                     Verify ran): the panel re-fetches. Called after a server write lands even
                     when the sheet has closed.
    onstarted        optional: the install tab or server a step started, reported even after a
                     close, so a parent that outlives the panel hears its end (lib/hive/mediaWatch.ts).
    install          optional: an install Rust already started for this provider (an approved
                     media ask from the phone, docs/hive-media.md §8). The sheet shows its tab as if
                     its own Install had started it, and never offers a second one while it runs.
    installFinished  optional: that install's end, heard before the sheet opened.

  Ordering: `alive` goes false at unmount, and every await is followed by a check of it before
  anything is written into the sheet, so an event or answer arriving after the sheet closed writes
  nothing. Writes that must finish whatever the human does (scoping a server just added) are not
  behind that check. Every listener is released at unmount, including one whose `listen` resolves
  after it. No secret is logged, toasted or put in a URL: the pasted key goes from its field to
  `mcpServerAdd`/`mcpServerSetToken` and the field is emptied on success.

  A `stdio_mcp` provider (MiniMax, Meshy: docs/hive-media.md §12.1) is a local server maiTerm runs
  itself. Its arm is `StdioSetup` (lib/hive/mediaStdio.ts): the key to the keychain, then the server
  added from its catalogue route (Rust makes the command), then Verify; a failed start offers Try
  again and Remove the stored key. Its password field is emptied BEFORE the key is sent, so the key
  is in no state, store or sessionStorage once Save is pressed. The phone never reaches this arm.
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
  import type { PanelProvider, SetupStarted } from '$lib/hive/media';
  import { externalUrl, unverifiedNote } from '$lib/hive/media';
  import { startSignIn } from '$lib/mcp/addServer';
  import { hiveToolSetup } from '$lib/stores/hiveToolSetup.svelte';
  import { StdioSetup, defaultRoute, keyHint, keySendable, stdioPlan, type StdioState } from '$lib/hive/mediaStdio';
  import {
    bringsSpeechModel, canVerify, guideFor, installEndLine, speechModelProgress, verifyIsPrimary, newServerScope, reusedServerScope, scopeSentence, serverPlan, setupAction, speechModelLine, verifyView,
    type ScopeChoice, type SetupAction, type VerifyView,
  } from '$lib/hive/mediaSetup';

  interface Props {
    provider: PanelProvider;
    capabilityLabel: string;
    workspaceId: string | null;
    onclose: () => void;
    onchanged: () => void;
    /** Optional: what this sheet started whose end it may not live to see (an install's tab, a
     *  server it added or reused), for a parent that outlives it to watch. */
    onstarted?: (s: SetupStarted) => void;
    /** The tools ready when the panel opened this sheet: passed through on `onstarted`, the
     *  baseline that tells what this setup made ready. */
    readyBefore?: string[];
    install?: HiveMediaInstallStarted | null;
    installFinished?: HiveMediaInstallFinished | null;
  }
  let props: Props = $props();

  // Seeds, taken once.
  const p = untrack(() => props.provider);
  const capLabel = untrack(() => props.capabilityLabel);
  const ws = untrack(() => props.workspaceId);
  const close = untrack(() => props.onclose);
  const changed = untrack(() => props.onchanged);
  const reportStarted = untrack(() => props.onstarted);
  const readyBefore = untrack(() => [...(props.readyBefore ?? [])]);
  const preStarted = untrack(() => props.install ?? null);
  const preFinished = untrack(() => props.installFinished ?? null);

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

  // A local server maiTerm runs itself (stdio_mcp).
  const stdioRoutes = p.stdio_routes ?? [];
  let routeId = $state(defaultRoute(stdioRoutes)?.id ?? '');
  let stdio = $state<StdioState>({ stage: 'key', keyFor: null, server: null, error: null, note: null });
  const stdioSetup = new StdioSetup(
    {
      keyStore: commands.mcpStdioKeyStore,
      setRoute: commands.mcpStdioServerSetRoute,
      serverAdd: commands.mcpStdioServerAdd,
      setWorkspaces: commands.mcpServerSetWorkspaces,
      keyDelete: commands.mcpStdioKeyDelete,
      verify: () => runVerify(),
      // The write landed: say so whether or not the sheet is still open (the panel outlives it).
      landed: (s) => {
        if (alive) serverId = s.id;
        reportStarted?.({ provider_id: p.id, server_id: s.id, ready_before: readyBefore });
        changed();
      },
    },
    (st) => {
      stdio = st;
      // A failed start's reason and a removal's confirmation are said inside the stdio block, next
      // to what they are about (above Try again / Remove, above the paste); the rest in the note.
      if (st.stage === 'failed' || (st.stage === 'key' && !st.error)) note = null;
      else if (st.error) note = { text: st.error, error: true };
      else if (st.note) note = { text: st.note, error: false };
    },
  );

  // Install
  let started = $state<HiveMediaInstallStarted | null>(preStarted);
  let endLine = $state<string | null>(null);
  let model = $state<HiveMediaModelStatus | null>(null);

  const guide = $derived(guideFor(p, action));
  const isRemote = $derived(action === 'mcp_oauth' || action === 'mcp_token' || action === 'mcp_open');
  const isMcp = $derived(isRemote || action === 'mcp_stdio');
  const plan = $derived(servers && isRemote ? serverPlan(servers, url, p.id, native, DEV) : null);
  const reused = $derived(plan && 'reuse' in plan ? plan.reuse : null);
  const route = $derived(stdioRoutes.find((r) => r.id === routeId) ?? null);
  const routeIds = stdioRoutes.map((r) => r.id);
  const sPlan = $derived(servers && action === 'mcp_stdio' && route ? stdioPlan(servers, route.id, p.id, native, DEV, routeIds) : null);
  /** The key can be pasted (again): before any, and after a Verify that didn't say Ready (a key for
   *  the other region, a revoked key), with the account choice back too. */
  const stdioPasting = $derived(stdio.stage === 'key' || stdio.stage === 'storing' || (stdio.stage === 'added' && !verifying && result !== null && result.tone !== 'ok'));
  const stdioReused = $derived(sPlan && 'reuse' in sPlan ? sPlan.reuse : null);
  const stdioBusy = $derived(stdio.stage === 'storing' || stdio.stage === 'starting' || stdio.stage === 'verifying' || stdio.stage === 'removing');
  const hint = $derived(keyHint(key));
  /** The launcher the first start downloads the package with: `npx`, `uvx`. */
  const launcher = $derived(route?.command_shown.split(' ')[0] ?? '');
  const hiveCount = $derived(hiveWorkspaces.length);
  const modelLine = $derived(model ? speechModelLine(model) : null);
  const modelProgress = $derived(model ? speechModelProgress(model) : null);
  /** A sign-in has opened for the server this sheet added or reused (it may since have finished). */
  const signInOpened = $derived(waitingForSignIn || serverId !== null);
  /** Verify refused the key and the paste is back: Save is the next step, not Verify. */
  const stdioRepasting = $derived(stdio.stage === 'added' && stdioPasting);
  const verifyPrimary = $derived(verifyIsPrimary(action, { installStarted: started !== null, signInOpened, stdioAdded: stdio.stage === 'added', stdioRepasting }));
  const signupUrl = externalUrl(p.signup_url);
  /** Where the key is made: the chosen route's own page (a region's), else the provider's. */
  const keyUrl = $derived(externalUrl(route?.key_url ?? '') ?? signupUrl);
  /** The key button names the site it opens when the provider has several accounts (MiniMax's
   *  minimax.io and platform.minimaxi.com), so the choice above it and the page match. */
  const keySite = $derived.by(() => {
    if (stdioRoutes.length < 2 || !keyUrl) return p.name;
    try {
      return new URL(keyUrl).host.replace(/^www\./, '');
    } catch {
      return p.name;
    }
  });
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
    // A local server's sheet is a paste: its key field, so the human can paste at once.
    raf = requestAnimationFrame(() => {
      if (action === 'mcp_stdio') {
        // Already moved on (a step started before this frame): the stage's own focus rule holds.
        if (sheetEl?.contains(document.activeElement)) return;
        // Without scrolling: the heading stays in view as the sheet opens.
        (document.getElementById(keyId) ?? sheetEl)?.focus({ preventScroll: true });
        return;
      }
      (sheetEl?.querySelector<HTMLElement>('.act button') ?? sheetEl?.querySelector<HTMLElement>('.links button, .foot button'))?.focus();
    });
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
    // Every setup starts when its sheet opens: the parent's watch gets the baseline before any Verify
    // here can find the tool ready (a guide-only route the human sets up in a terminal, an app
    // already running with no Download). The later tab and server reports add what to watch; the
    // first start's baseline wins.
    reportStarted?.({ provider_id: p.id, ready_before: readyBefore });
    if (preStarted) void adoptInstall(preStarted, preFinished);
    if (speech) keep<HiveMediaModelStatus>('hive-media-model-progress', (m) => (model = m));
    if (action === 'app') window.addEventListener('focus', onWindowFocus);
  });

  // The stdio arm's focus at each stage: the key field when the paste comes back (a refused
  // Verify, a removed key), Try again after a failed start, and the sheet itself while a step runs
  // (the field it was in is gone), so Escape and Tab stay in the sheet throughout.
  let wasPasting = untrack(() => stdioPasting);
  $effect(() => {
    const pasting = stdioPasting;
    const stage = stdio.stage;
    untrack(() => {
      if (pasting && !wasPasting) {
        requestAnimationFrame(() => {
          if (alive) document.getElementById(keyId)?.focus();
        });
      } else if (stage === 'failed') {
        requestAnimationFrame(() => {
          if (alive) sheetEl?.querySelector<HTMLElement>('.stdio-retry button')?.focus();
        });
      } else if (stage === 'storing' || stage === 'starting' || stage === 'verifying' || stage === 'removing') {
        holdFocus();
      }
      wasPasting = pasting;
    });
  });

  // How long the first start has run, so the wait doesn't look frozen.
  let startedAt = 0;
  let elapsed = $state(0);
  $effect(() => {
    if (stdio.stage !== 'starting') return;
    startedAt = Date.now();
    elapsed = 0;
    const t = setInterval(() => (elapsed = Math.floor((Date.now() - startedAt) / 1000)), 1000);
    return () => clearInterval(t);
  });

  onDestroy(() => {
    alive = false;
    stdioSetup.alive = false;
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
      result = verifyView(r, { keyRoute: action === 'mcp_stdio' || action === 'mcp_token' });
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
      // The write landed: say so whether or not the sheet is still open (the panel outlives it).
      reportStarted?.({ provider_id: p.id, server_id: s.id, ready_before: readyBefore });
      if (scope || token !== undefined) changed();
      return s;
    }
    const scope = newServerScope(ws, scopeChoice, hiveWorkspaces);
    if (scope === null) throw new Error('There is no Hive yet. Choose Every workspace, or set this up from a Hive.');
    const s = await commands.mcpServerAdd(got.add, url, auth, { token: token ?? null });
    if (scope.length) await commands.mcpServerSetWorkspaces(s.id, scope);
    // The write landed: say so whether or not the sheet is still open (the panel outlives it).
    reportStarted?.({ provider_id: p.id, server_id: s.id, ready_before: readyBefore });
    changed();
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

  // ─── stdio: a local server maiTerm runs itself ────────────────────────────

  /** Save key: the field is emptied first, so the key lives only in this call's argument. The server
   *  list is read again at the click (the one from mount may be stale), the scope decided as the
   *  remote add path decides it, then `StdioSetup` stores the key, adds the server and verifies. */
  async function saveStdioKey(e: SubmitEvent) {
    e.preventDefault();
    const r = route;
    if (!r || stdioBusy || busy || !keySendable(key)) return;
    const value = key.trim();
    key = '';
    note = null;
    result = null;
    busy = true;
    try {
      const [list, names, windows] = await Promise.all([
        commands.mcpServersList(),
        commands.mcpNativeNames().catch(() => [] as string[]),
        ws ? Promise.resolve([]) : commands.listMoveTargets().catch(() => []),
      ]);
      if (!alive) return;
      servers = list;
      native = names;
      if (!ws) hiveWorkspaces = hiveIds(windows);
      const got = stdioPlan(list, r.id, p.id, names, DEV, routeIds);
      if ('error' in got) throw new Error(got.error);
      const reuse = 'reuse' in got ? got.reuse : null;
      const scope = reuse ? [] : newServerScope(ws, scopeChoice, hiveWorkspaces);
      if (scope === null) throw new Error('There is no Hive yet. Choose Every workspace, or set this up from a Hive.');
      busy = false;
      await stdioSetup.submit(value, {
        id: reuse ? reuse.id : 'add' in got ? got.add : '',
        routeId: r.id,
        reuse,
        scope,
        reuseScope: reuse ? reusedServerScope(reuse.workspaces, ws) : null,
      });
    } catch (err) {
      fail(err);
    } finally {
      if (alive) {
        busy = false;
        holdFocus();
      }
    }
  }

  async function retryStdio() {
    note = null;
    result = null;
    await stdioSetup.retry();
    holdFocus();
  }

  async function removeStdioKey() {
    note = null;
    result = null;
    await stdioSetup.removeKey();
    holdFocus();
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
      // Reported even after a close: the install runs on in its tab.
      reportStarted?.({ provider_id: p.id, tab_id: s.tab_id, ready_before: readyBefore });
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

  /** An install Rust started before this sheet opened: its end is heard here as if Install had
   *  started it, and the parent's watch learns its tab, so the tick and Ready flow is the same.
   *  As install() does with `early`: the listener is up BEFORE the end held elsewhere is read (the
   *  seed, or the store's record of an end that came between the opening's take and now), and the
   *  end is handled once, whichever reports it. It matches the adopted tab only: a "Run it again"
   *  is install()'s, with its own listener. */
  async function adoptInstall(s: HiveMediaInstallStarted, finished: HiveMediaInstallFinished | null) {
    reportStarted?.({ provider_id: p.id, tab_id: s.tab_id, ready_before: readyBefore });
    let handled = false;
    const end = (f: HiveMediaInstallFinished) => {
      if (handled || !alive) return;
      handled = true;
      onFinished(f);
    };
    try {
      const off = await listen<HiveMediaInstallFinished>('hive-media-install-finished', (e) => {
        if (e.payload.provider_id === p.id && e.payload.tab_id === s.tab_id) end(e.payload);
      });
      if (!alive) {
        off();
        return;
      }
      offs.push(off);
    } catch (e) {
      void logWarn(`[HiveMediaSetupSheet] hive-media-install-finished: not listening: ${String(e)}`);
    }
    const held = finished?.tab_id === s.tab_id ? finished : hiveToolSetup.installEnd(s.tab_id);
    if (held) end(held);
  }

  async function showTab() {
    const id = started?.tab_id;
    if (!id) return;
    const { navigateToTab } = await import('$lib/stores/workspaces.svelte');
    await navigateToTab(id).catch((e) => fail(e));
  }

  /** A local app is set up outside maiTerm, so its start is the download: the parent's watch
   *  learns the provider and its baseline, and ticks the app if a later Verify finds it ready. */
  function downloadApp() {
    reportStarted?.({ provider_id: p.id, ready_before: readyBefore });
    void open(appUrl);
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

    {#if action === 'guide' && guide.lead}<p class="lead">{guide.lead}</p>{/if}
    <!-- Re-pasting after a refused Verify, the steps are done: the paste and its choices fit without them. -->
    {#if guide.steps.length && !stdioRepasting}
      <ol class="guide">
        {#each guide.steps as step, i (i)}<li>{step}</li>{/each}
      </ol>
    {/if}
    {#if (signupUrl && action !== 'mcp_token' && action !== 'mcp_stdio') || billingUrl}
      <p class="links">
        {#if signupUrl && action !== 'mcp_token' && action !== 'mcp_stdio'}<button type="button" class="link" onclick={() => open(signupUrl)}>Sign up</button>{/if}
        {#if billingUrl}<button type="button" class="link" onclick={() => open(billingUrl)}>Billing</button>{/if}
      </p>
    {/if}

    {#snippet scopeLine()}
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
    {/snippet}

    <div class="act">
      {#if action === 'mcp_stdio' && route}
        {#if stdioRoutes.length > 1 && stdioPasting && stdio.stage !== 'storing'}
          <fieldset class="scope">
            <legend>Which {p.name} account is the key for?</legend>
            {#each stdioRoutes as r (r.id)}
              <label class="choice">
                <input type="radio" name="{uid}-route" value={r.id} bind:group={routeId} />
                <span>{r.label || r.id}</span>
              </label>
            {/each}
            <p class="dim">A key works only for the account you choose here.</p>
          </fieldset>
        {:else if stdioRoutes.length > 1}
          <!-- Once the key is on its way, the account is fixed: said, not a disabled control. -->
          <p class="state-line">Account: {route.label || route.id}</p>
        {/if}
        <p class="state-line">
          maiTerm runs <code>{route.command_shown}</code> on this Mac itself, outside every sandbox, and serves it to your agents. The version is pinned, but the package’s dependencies aren’t. Its first start downloads it with {launcher}, which can take a minute or more.
        </p>
        {#if stdioReused}
          <p class="dim">maiTerm already runs this as “{stdioReused.entry_name}”: a new key replaces its key{stdioReused.route !== route.id ? `, and it moves to ${route.label || 'this route'}` : ''}{ws && stdioReused.workspaces.length && !stdioReused.workspaces.includes(ws) ? ', and it is offered to this Hive too' : ''}.</p>
        {:else}
          {@render scopeLine()}
        {/if}
        {#if stdioPasting}
          {#if stdioRepasting}
            <p class="state-line">Verify re-checks the key already saved. {stdioRoutes.length > 1 ? 'To use another account, choose it and paste its key.' : 'To use another key, paste it here.'}</p>
          {:else if stdio.stage === 'key' && stdio.note}
            <p class="state-line" role="status">{stdio.note}</p>
          {/if}
          {#if keyUrl}<div class="row"><Button variant="secondary" onclick={() => open(keyUrl)}>Open {keySite} to make a key</Button></div>{/if}
          <form class="key" onsubmit={saveStdioKey}>
            <label for={keyId}>Paste the key</label>
            <div class="row">
              <input id={keyId} type="password" bind:value={key} autocomplete="off" spellcheck="false" />
              <Button variant="primary" type="submit" aria-disabled={busy || stdioBusy || !keySendable(key)}>{#if busy || stdio.stage === 'storing'}<span class="spin" aria-hidden="true"></span>Saving…{:else}{stdioReused ? 'Save key' : 'Save key and start'}{/if}</Button>
            </div>
            {#if hint}<p class="hint" role="status">{hint}</p>{/if}
            <p class="dim">It goes to your keychain and reaches {p.name}’s server only in its environment: never an agent’s config, a log or a command line.</p>
          </form>
        {:else if stdio.stage === 'starting'}
          <p class="state-line busy-line" role="status"><span class="spin" aria-hidden="true"></span>Starting {p.name}… {elapsed} s</p>
        {:else if stdio.stage === 'failed'}
          {#if stdio.error}<p class="note bad" role="alert">{stdio.error}</p>{/if}
          <p class="state-line">{stdio.keyFor && !stdioReused ? 'Your key is saved in your keychain. Try again reuses it; Remove deletes it.' : 'Your key is saved. Try again reuses it.'}</p>
          <div class="row">
            <span class="stdio-retry"><Button variant="primary" onclick={retryStdio}>Try again</Button></span>
            {#if !stdioReused && stdio.keyFor}<span class="danger"><Button variant="ghost" onclick={removeStdioKey}>Remove the stored key</Button></span>{/if}
          </div>
        {:else if stdio.stage === 'removing'}
          <p class="state-line busy-line" role="status"><span class="spin" aria-hidden="true"></span>Removing the stored key…</p>
        {/if}
      {/if}

      {#if action === 'mcp_oauth' || action === 'mcp_open' || action === 'mcp_token'}
        {#if reused}
          <p class="dim">maiTerm already serves this as “{reused.entry_name}”: setting up uses that server{ws && reused.workspaces.length && !reused.workspaces.includes(ws) ? ' and offers it to this Hive too' : ''}.</p>
        {:else}
          <p class="dim">maiTerm adds {p.name}’s MCP server (<span class="url">{url}</span>) and serves it to your agents{action === 'mcp_oauth' ? ', then opens its sign-in in your browser' : ''}.</p>
          {@render scopeLine()}
        {/if}
      {/if}

      {#if action === 'mcp_oauth'}
        <div class="row">
          <Button variant={signInOpened ? 'secondary' : 'primary'} aria-disabled={busy} onclick={signInFlow}>{busy ? 'Working…' : serverId ? 'Sign in again' : reused?.status.state === 'connected' ? 'Use it' : 'Sign in'}</Button>
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
            <p class="state-line" role="status">{endLine}</p>
            {#if result?.tone !== 'ok'}<div class="row"><Button variant="secondary" aria-disabled={busy} onclick={install}>Run it again</Button></div>{/if}
          {/if}
          {#if !started.watches_exit && !endLine}
            <p class="state-line">maiTerm can’t see this shell finish. Press Re-detect when it’s done.</p>
          {/if}
        {:else}
          <div class="row"><Button variant="primary" aria-disabled={busy} onclick={install}>{busy ? 'Opening a tab…' : `Install ${p.name}`}</Button></div>
          <p class="dim">It runs in a terminal tab you can watch{ws ? ', outside this Hive' : ''}.</p>
        {/if}
        {#if modelLine}
          <div class="model" role="status">
            <span class="state-line model-text" class:bad={modelLine.retry}>{modelLine.text}</span>
            {#if modelProgress}
              <span class="bar" role="progressbar" aria-label="Speech model download" aria-valuemin={0} aria-valuemax={modelProgress.total} aria-valuenow={modelProgress.done}>
                <span class="fill" style:width="{modelProgress.percent}%"></span>
              </span>
            {/if}
            {#if modelLine.retry}<button type="button" class="link try" onclick={retryModel}>Try again</button>{/if}
          </div>
        {/if}
      {:else if action === 'app'}
        {#if appUrl}<div class="row"><Button variant="primary" onclick={downloadApp}>Download {p.name}</Button></div>{/if}
        <p class="state-line">maiTerm checks again when you come back to this window.</p>
      {/if}
    </div>

    {#if note}
      <p class="note" class:bad={note.error} role={note.error ? 'alert' : 'status'}>{note.text}</p>
    {/if}

    {#if result}
      <div class="result" role="status">
        <span class="state {result.tone}">{result.label}</span>
        <span class="msg">{result.message}</span>
        {#if result.link}<div class="result-act"><Button variant="secondary" onclick={() => open(result?.link ?? null)}>{result.linkLabel}</Button></div>{/if}
      </div>
    {/if}

    <div class="foot">
      {#if canVerify(p)}
        <Button variant={verifyPrimary ? 'primary' : 'secondary'} aria-disabled={verifying || stdioBusy} onclick={() => { if (!verifying && !stdioBusy) void runVerify(); }}>
          {verifying ? 'Checking…' : action === 'install' && started && !started.watches_exit ? 'Re-detect' : 'Verify'}
        </Button>
      {/if}
      <Button variant="ghost" onclick={close}>Close</Button>
    </div>
  </div>
</div>

<style>
  .backdrop { position: fixed; inset: 0; background: rgba(0, 0, 0, 0.4); display: flex; justify-content: center; align-items: flex-start; padding: min(8vh, 40px) 16px 16px; z-index: 1100; overflow-y: auto; }
  /* Never taller than the window: the sheet scrolls inside and its footer (Verify, Close) stays in
     view, whatever the stage and however many choices (Preferences adds the scope fieldset). */
  .sheet { box-sizing: border-box; display: flex; flex-direction: column; gap: 8px; width: min(480px, 100%); min-width: 0; max-height: calc(100vh - min(8vh, 40px) - 16px); overflow-y: auto; scroll-padding-bottom: 56px; padding: 14px 16px 0; background: var(--bg-medium); color: var(--fg); border: 1px solid var(--bg-light); border-radius: 8px; box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4); }
  .sheet:focus { outline: none; }
  .sheet :global(.btn[aria-disabled='true']) { opacity: 0.5; cursor: default; }
  /* A secondary button here is an action to read, not a hint: Verify, Run it again, the key page. */
  .sheet :global(.btn.secondary) { color: var(--fg); }
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
  .state-line { font-size: 0.846rem; color: var(--fg); overflow-wrap: anywhere; }
  .lead { font-size: 0.923rem; color: var(--fg); }
  .model { display: flex; align-items: center; gap: 8px; min-width: 0; }
  .model-text { flex: 1 1 auto; min-width: 0; }
  .bar { flex: 0 0 72px; height: 4px; border-radius: 2px; background: var(--bg-light); overflow: hidden; }
  .fill { display: block; height: 100%; background: var(--accent); }
  .try { flex: none; }
  .result-act { flex-basis: 100%; display: flex; }
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
  .hint { font-size: 0.808rem; color: var(--yellow); }
  .bad { color: var(--red); }
  .result { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 8px; padding: 6px 8px; background: var(--bg-dark); border: 1px solid var(--bg-light); border-radius: 5px; font-size: 0.846rem; min-width: 0; }
  .msg { flex: 1 1 14em; min-width: 0; overflow-wrap: anywhere; }
  .state { font-size: 0.769rem; padding: 0 6px; border-radius: 8px; border: 1px solid var(--bg-light); color: var(--fg-dim); overflow-wrap: anywhere; }
  .state.ok { color: var(--green); border-color: var(--green); }
  .state.warn { color: var(--yellow); border-color: var(--yellow); }
  .state.bad { color: var(--red); border-color: var(--red); }
  .foot { position: sticky; bottom: 0; display: flex; flex-wrap: wrap; justify-content: flex-end; gap: 6px; padding: 6px 0 14px; background: var(--bg-medium); }
  .busy-line { display: flex; align-items: center; gap: 8px; }
  .spin { display: inline-block; flex: none; width: 12px; height: 12px; margin-right: 6px; vertical-align: -2px; border: 2px solid var(--bg-light); border-top-color: var(--accent); border-radius: 50%; animation: spin 0.8s linear infinite; }
  .busy-line .spin { margin-right: 0; }
  @keyframes spin { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spin { animation-duration: 2.4s; } }
  .danger :global(.btn) { color: var(--red); }
</style>
