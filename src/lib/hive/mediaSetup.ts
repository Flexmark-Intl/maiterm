/** The media setup sheet's logic (docs/hive-media.md §6), as pure functions: which action a
 *  provider's route gets, where a server it adds is offered (scope), which MCP server it reuses or
 *  what it names a new one, and Verify's, an install's and the speech model's results in words.
 *  `HiveMediaSetupSheet.svelte` does the calls. Every kind on the wire is a String: a kind this
 *  build doesn't know gets the guide, never a guess. */

import type { HiveMediaInstallFinished, HiveMediaModelStatus, HiveMediaProvider, HiveMediaVerifyResult } from '$lib/tauri/types';
import type { McpServerView } from '$lib/tauri/commands';
import { freeServerName, sameServerUrl, suggestServerName } from '$lib/mcp/addServer';
import { externalUrl } from './media';

// ─── The action ─────────────────────────────────────────────────────────────

/** What the sheet offers for a provider:
 *  - `mcp_oauth`: add its MCP server through maiTerm, then sign in in the browser;
 *  - `mcp_token`: open its key page, paste the key (saved to the keychain as the proxy's token);
 *  - `mcp_open`: add its MCP server, which needs no sign-in;
 *  - `mcp_stdio`: maiTerm runs its local MCP server itself: paste its key, then maiTerm adds it from
 *    the catalogue route (lib/hive/mediaStdio.ts);
 *  - `install`: a brew or plugin install in a visible tab;
 *  - `app`: a download link, re-detected when the window comes back to the front;
 *  - `guide`: the guide only (`manual`, a plugin with no confirmed source, an unknown kind). */
export type SetupAction = 'mcp_oauth' | 'mcp_token' | 'mcp_open' | 'mcp_stdio' | 'install' | 'app' | 'guide';

export function setupAction(p: Pick<HiveMediaProvider, 'route_kind' | 'auth' | 'route_url' | 'installable'> & { stdio_routes?: HiveMediaProvider['stdio_routes'] }): SetupAction {
  switch (p.route_kind) {
    case 'stdio_mcp':
      // A route this build can't name (none listed) gets the guide, never a guess.
      return p.stdio_routes?.length ? 'mcp_stdio' : 'guide';
    case 'remote_mcp':
      if (!externalUrl(p.route_url)) return 'guide';
      if (p.auth === 'oauth') return 'mcp_oauth';
      if (p.auth === 'token') return 'mcp_token';
      if (p.auth === 'none') return 'mcp_open';
      return 'guide';
    case 'brew':
    case 'plugin':
      return p.installable ? 'install' : 'guide';
    case 'local_app':
      return 'app';
    default:
      return 'guide';
  }
}

/** Whether Verify means anything: every provider with a route can be re-detected, a guide-only
 *  one too (the human followed the guide; Verify then finds what they set up). Only a provider with
 *  no route at all has nothing to look for. */
export function canVerify(p: Pick<HiveMediaProvider, 'route_kind'>): boolean {
  return p.route_kind !== '';
}

/** What the sheet shows above its action: the catalogue's guide, unless the guide describes an
 *  install maiTerm can't run (a plugin with no confirmed source, Remotion today: its guide says
 *  "maiTerm opens a tab"). Then the steps are the human's, said in maiTerm's words. */
export function guideFor(p: Pick<HiveMediaProvider, 'name' | 'route_kind' | 'guide'>, action: SetupAction): { steps: string[]; lead: string | null } {
  if (action === 'guide' && p.route_kind === 'plugin') {
    return {
      steps: [`Install ${p.name}'s Claude Code plugin yourself, in a terminal.`, 'Come back and press Verify.'],
      lead: `${p.name} can't be installed from here yet.`,
    };
  }
  if (action === 'guide' && p.route_kind === 'brew') {
    return { steps: [], lead: `maiTerm can't run this Homebrew install. Install it in a terminal yourself, then press Verify.` };
  }
  return { steps: p.guide, lead: action === 'guide' ? `maiTerm can't set ${p.name} up for you: the steps are yours. Press Verify when they're done.` : null };
}

/** Whether Verify is the sheet's primary button: when it is the only way forward. A guide-only
 *  route or a local app (the human does the setup elsewhere), an install that has started (running
 *  or finished), and a browser sign-in that has opened (from then on "Sign in again" is secondary,
 *  also after Verify answered, e.g. "Needs credit"). Otherwise the route's own action is the primary
 *  and Verify is secondary. */
export function verifyIsPrimary(action: SetupAction, s: { installStarted: boolean; signInOpened: boolean; stdioAdded?: boolean; stdioRepasting?: boolean }): boolean {
  if (action === 'guide' || action === 'app') return true;
  if (action === 'install') return s.installStarted;
  if (action === 'mcp_oauth') return s.signInOpened;
  // Once the server is added Verify is the way forward, unless Verify refused the key: then saving a
  // new key (perhaps for the other account) is, and Verify would only re-check the old one.
  if (action === 'mcp_stdio') return (s.stdioAdded ?? false) && !(s.stdioRepasting ?? false);
  return false;
}

/** The speech model's download as a fraction for a progress bar: null unless it is downloading and
 *  its size is known. */
export function speechModelProgress(m: Pick<HiveMediaModelStatus, 'state' | 'done' | 'total'>): { done: number; total: number; percent: number } | null {
  if (m.state !== 'downloading' || !m.total || m.total <= 0) return null;
  const done = Math.min(Math.max(m.done, 0), m.total);
  return { done, total: m.total, percent: Math.round((done / m.total) * 100) };
}

/** A Homebrew route that brings whisper.cpp: maiTerm downloads its speech model after it. */
export function bringsSpeechModel(p: Pick<HiveMediaProvider, 'route_kind' | 'capabilities'>): boolean {
  return p.route_kind === 'brew' && p.capabilities.includes('transcribe');
}

// ─── Scope ──────────────────────────────────────────────────────────────────

/** Asked only from Preferences: every workspace, or only the Hives' workspaces. */
export type ScopeChoice = 'every' | 'hives';

/** The workspaces a server this sheet ADDS is offered to (`mcp_server_set_workspaces`; [] is every
 *  workspace). Started from a Hive (`workspaceId`: the Create sheet's or Hive settings'), only that
 *  Hive's workspace, so one setup doesn't load the provider's tools into every Claude tab (§4.3).
 *  From Preferences, the human's choice; "hives" with no Hive anywhere is null: it can't be said
 *  as a list, and [] would mean every workspace. */
export function newServerScope(workspaceId: string | null, choice: ScopeChoice, hiveWorkspaces: readonly string[]): string[] | null {
  if (workspaceId) return [workspaceId];
  if (choice === 'every') return [];
  const ids = [...new Set(hiveWorkspaces)];
  return ids.length ? ids : null;
}

/** A REUSED server's new scope, or null to leave it alone. From a Hive, a server limited to other
 *  workspaces gains this one (the human's click asked for it here); one offered everywhere already
 *  reaches it. From Preferences the scope the human set stays. */
export function reusedServerScope(current: readonly string[], workspaceId: string | null): string[] | null {
  if (!workspaceId || current.length === 0 || current.includes(workspaceId)) return null;
  return [...current, workspaceId];
}

/** What the sheet says about scope before the human acts. */
export function scopeSentence(workspaceId: string | null, choice: ScopeChoice, hiveCount: number): string {
  if (workspaceId) return "Only this Hive's workspace gets its tools: other tabs don't load them.";
  if (choice === 'every') return 'Every workspace gets its tools: each agent session there loads them.';
  return hiveCount ? `Only the Hive workspaces there are now (${hiveCount}) get its tools. A Hive made later gets them when you set it up there.` : 'There is no Hive yet, so it can only be offered to every workspace for now.';
}

// ─── Which server ───────────────────────────────────────────────────────────

/** Reuse the server maiTerm already serves for this URL, else add one under a free name. */
export type ServerPlan = { reuse: McpServerView } | { add: string } | { error: string };

/** The add path's checks at filing (§6, as `mcp_add`'s): a URL maiTerm already serves is reused
 *  (the first one, as listed); a name a native entry or another server holds gets a suffix. The name
 *  is the provider's id ("fal", "elevenlabs") when it is a valid server name, else the one the
 *  Preferences form would suggest from the URL. */
export function serverPlan(servers: readonly McpServerView[], url: string, providerId: string, nativeNames: readonly string[], dev: boolean): ServerPlan {
  const served = servers.find((s) => sameServerUrl(s.url, url));
  if (served) return { reuse: served };
  const ids = servers.map((s) => s.id);
  const name = freeServerName(providerId, ids, nativeNames, dev) ?? freeServerName(suggestServerName(url), ids, nativeNames, dev);
  return name ? { add: name } : { error: `maiTerm couldn't find a free name for this server. Add ${url} yourself in Preferences → MCP servers.` };
}

// ─── Results in words ───────────────────────────────────────────────────────

const VERIFY_LABELS = new Map<string, string>([
  ['ready', 'Ready'],
  ['needs_sign_in', 'Needs sign-in'],
  ['needs_credit', 'Needs credit'],
  ['unreachable', 'Not reachable'],
  ['installed', 'Installed, not checked'],
  ['error', "Didn't work"],
]);

export interface VerifyView {
  label: string;
  tone: 'ok' | 'warn' | 'bad' | 'neutral';
  /** Rust's sentence, already in maiTerm's words. */
  message: string;
  /** A button's target: http(s) only, else null. */
  link: string | null;
  linkLabel: string;
}

/** Verify's answer as the sheet's row shows it. A state a later Rust build adds reads "Checked",
 *  neutral, with its message: nothing says it works. `keyRoute`: the provider takes a pasted key
 *  (a local server, or a remote one with a token), which has no sign-in, so a refusal reads
 *  "Key refused". */
export function verifyView(r: HiveMediaVerifyResult, opts: { keyRoute?: boolean } = {}): VerifyView {
  const tone = r.state === 'ready' ? 'ok' : r.state === 'error' ? 'bad' : r.state === 'needs_sign_in' || r.state === 'needs_credit' || r.state === 'unreachable' ? 'warn' : 'neutral';
  return {
    label: r.state === 'needs_sign_in' && opts.keyRoute ? 'Key refused' : (VERIFY_LABELS.get(r.state) ?? 'Checked'),
    tone,
    message: r.message,
    link: r.link ? externalUrl(r.link) : null,
    linkLabel: r.state === 'needs_credit' ? 'Add credit' : 'Open',
  };
}

/** How an install's tab ended, in words. The status (Verify) is the truth: this only says why it
 *  might not be ready. */
export function installEndLine(f: Pick<HiveMediaInstallFinished, 'ok' | 'ended' | 'exit_code'>): string {
  if (f.ended === 'closed') return "The install's tab closed before the install finished.";
  if (f.ok) return 'The install finished.';
  return f.exit_code === null ? 'The install stopped.' : `The install stopped with exit code ${f.exit_code}. Its tab shows why.`;
}

function mb(bytes: number): string {
  return `${Math.round(bytes / 1_000_000)} MB`;
}

/** The speech model's state in words, and whether to offer a retry. */
export function speechModelLine(m: Pick<HiveMediaModelStatus, 'state' | 'done' | 'total' | 'error'>): { text: string; retry: boolean; busy: boolean } {
  switch (m.state) {
    case 'ready':
      return { text: 'Speech model ready.', retry: false, busy: false };
    case 'downloading':
      return { text: m.total ? `Downloading the speech model: ${mb(m.done)} of ${mb(m.total)}` : `Downloading the speech model: ${mb(m.done)}`, retry: false, busy: true };
    case 'failed':
      return { text: `The speech model didn't download${m.error ? `: ${m.error}` : '.'}`, retry: true, busy: false };
    case 'missing':
      return { text: 'The speech model is not downloaded yet.', retry: true, busy: false };
    default:
      return { text: `Speech model: ${m.state}`, retry: false, busy: false };
  }
}
