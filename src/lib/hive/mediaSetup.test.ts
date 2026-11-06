import { describe, expect, it, vi } from 'vitest';
import type { McpServerView } from '$lib/tauri/commands';

vi.mock('@tauri-apps/plugin-shell', () => ({ open: vi.fn() }));
vi.mock('$lib/tauri/commands', () => ({ mcpServerSignIn: vi.fn() }));

const { bringsSpeechModel, canVerify, guideFor, installEndLine, newServerScope, reusedServerScope, scopeSentence, serverPlan, setupAction, speechModelLine, speechModelProgress, verifyIsPrimary, verifyView } = await import('./mediaSetup');

const route = (route_kind: string, auth = 'none', route_url = '', installable = false) => ({ route_kind, auth, route_url, installable });

describe('setupAction', () => {
  it('maps each route kind to its action', () => {
    expect(setupAction(route('remote_mcp', 'oauth', 'https://mcp.fal.ai/mcp-relay'))).toBe('mcp_oauth');
    expect(setupAction(route('remote_mcp', 'token', 'https://mcp.replicate.com/sse'))).toBe('mcp_token');
    expect(setupAction(route('remote_mcp', 'none', 'https://mcp.svelte.dev/mcp'))).toBe('mcp_open');
    expect(setupAction(route('brew', 'none', '', true))).toBe('install');
    expect(setupAction(route('plugin', 'none', '', true))).toBe('install');
    expect(setupAction(route('local_app', 'none', 'https://www.comfy.org/download'))).toBe('app');
    expect(setupAction(route('manual', 'token'))).toBe('guide');
  });
  it('gives the guide where an action would be refused or guessed', () => {
    // Remotion: a plugin with no confirmed marketplace.
    expect(setupAction(route('plugin', 'none', '', false))).toBe('guide');
    // An MCP route with no usable URL, or an auth this build doesn't know.
    expect(setupAction(route('remote_mcp', 'oauth', ''))).toBe('guide');
    expect(setupAction(route('remote_mcp', 'oauth', 'javascript:alert(1)'))).toBe('guide');
    expect(setupAction(route('remote_mcp', 'passkey', 'https://x.example/mcp'))).toBe('guide');
    expect(setupAction(route('stdio_mcp', 'token'))).toBe('guide');
    const stdio = { id: 'meshy', label: '', key_url: 'https://www.meshy.ai/developers/keys', command_shown: 'npx -y @meshy-ai/meshy-mcp-server@0.6.7', takes_key: true };
    expect(setupAction({ ...route('stdio_mcp', 'token'), stdio_routes: [stdio] })).toBe('mcp_stdio');
    expect(setupAction({ ...route('stdio_mcp', 'token'), stdio_routes: [] })).toBe('guide');
    expect(setupAction(route(''))).toBe('guide');
  });
  it('verifies any provider with a route, a guide-only one included', () => {
    expect(canVerify({ route_kind: 'manual' })).toBe(true);
    expect(canVerify({ route_kind: 'local_app' })).toBe(true);
    expect(canVerify({ route_kind: '' })).toBe(false);
  });
  it('never shows a guide that promises an install maiTerm will refuse', () => {
    const remotion = { name: 'Remotion', route_kind: 'plugin', guide: ['maiTerm opens a tab and installs the Remotion plugin for Claude Code.'] };
    const g = guideFor(remotion, 'guide');
    expect(g).toEqual({
      lead: "Remotion can't be installed from here yet.",
      steps: ["Install Remotion's Claude Code plugin yourself, in a terminal.", 'Come back and press Verify.'],
    });
    expect(guideFor(remotion, 'install')).toEqual({ steps: remotion.guide, lead: null });
    const minimax = { name: 'MiniMax', route_kind: 'manual', guide: ['Make a MiniMax account and an API key.'] };
    expect(guideFor(minimax, 'guide').steps).toEqual(minimax.guide);
    expect(guideFor(minimax, 'guide').lead).toContain('the steps are yours');
  });
  it('knows a Homebrew kit that brings whisper.cpp', () => {
    expect(bringsSpeechModel({ route_kind: 'brew', capabilities: ['finish', 'transcribe'] })).toBe(true);
    expect(bringsSpeechModel({ route_kind: 'plugin', capabilities: ['transcribe'] })).toBe(false);
    expect(bringsSpeechModel({ route_kind: 'brew', capabilities: ['finish'] })).toBe(false);
  });
});

describe('scope', () => {
  it('scopes a new server to the Hive it was set up from', () => {
    expect(newServerScope('ws-hive', 'every', ['a', 'b'])).toEqual(['ws-hive']);
  });
  it('asks in Preferences: every workspace, or the Hives there are', () => {
    expect(newServerScope(null, 'every', ['a'])).toEqual([]);
    expect(newServerScope(null, 'hives', ['a', 'b', 'a'])).toEqual(['a', 'b']);
  });
  it('never turns "Hive workspaces only" with no Hive into every workspace', () => {
    expect(newServerScope(null, 'hives', [])).toBeNull();
  });
  it('adds the Hive to a reused server limited elsewhere, and leaves the rest alone', () => {
    expect(reusedServerScope(['x'], 'ws-hive')).toEqual(['x', 'ws-hive']);
    expect(reusedServerScope([], 'ws-hive')).toBeNull();
    expect(reusedServerScope(['ws-hive'], 'ws-hive')).toBeNull();
    expect(reusedServerScope(['x'], null)).toBeNull();
  });
  it('says where it goes', () => {
    expect(scopeSentence('ws', 'every', 0)).toContain("this Hive's workspace");
    expect(scopeSentence(null, 'hives', 2)).toContain('(2)');
    expect(scopeSentence(null, 'hives', 0)).toContain('no Hive yet');
  });
});

const server = (id: string, url: string): McpServerView => ({
  id, url, auth: 'oauth', enabled: true, workspaces: [], status: { state: 'connected', detail: '', since: 0 }, tools: [], fetched_at: null, added_at: 0, added_by: 'human', entry_name: id, collisions: [], duplicates: [], transport: 'http', route: null,
});

describe('serverPlan', () => {
  it('reuses a server maiTerm already serves for the URL, whatever its name', () => {
    const s = server('video', 'https://MCP.fal.ai/mcp-relay/');
    expect(serverPlan([s], 'https://mcp.fal.ai/mcp-relay', 'fal', [], false)).toEqual({ reuse: s });
  });
  it('names a new one after the provider, suffixed past a native entry', () => {
    expect(serverPlan([], 'https://mcp.fal.ai/mcp-relay', 'fal', [], false)).toEqual({ add: 'fal' });
    expect(serverPlan([], 'https://mcp.fal.ai/mcp-relay', 'fal', ['fal'], false)).toEqual({ add: 'fal-2' });
    expect(serverPlan([server('fal', 'https://other.example/mcp')], 'https://mcp.fal.ai/mcp-relay', 'fal', [], false)).toEqual({ add: 'fal-2' });
  });
  it('falls back to the host for a provider id that is no server name', () => {
    expect(serverPlan([], 'https://mcp.fal.ai/mcp-relay', 'Fal AI', [], false)).toEqual({ add: 'fal' });
  });
});

describe('verifyView', () => {
  it('says each state in plain words, with the billing link as a button', () => {
    const v = verifyView({ state: 'needs_credit', message: 'Signed in, but fal answered 402.', link: 'https://fal.ai/dashboard/billing', tool_count: 12 });
    expect(v).toEqual({ label: 'Needs credit', tone: 'warn', message: 'Signed in, but fal answered 402.', link: 'https://fal.ai/dashboard/billing', linkLabel: 'Add credit' });
    expect(verifyView({ state: 'ready', message: 'ok', link: null, tool_count: 3 }).tone).toBe('ok');
    expect(verifyView({ state: 'error', message: 'x', link: null, tool_count: null }).label).toBe("Didn't work");
  });
  it('never hands a non-web link to a button, and reads an unknown state as unconfirmed', () => {
    expect(verifyView({ state: 'needs_credit', message: '', link: 'file:///x', tool_count: null }).link).toBeNull();
    expect(verifyView({ state: 'quota', message: 'm', link: null, tool_count: null })).toMatchObject({ label: 'Checked', tone: 'neutral' });
  });
});

describe('installEndLine and speechModelLine', () => {
  it('says how the install ended', () => {
    expect(installEndLine({ ok: true, ended: 'exited', exit_code: 0 })).toBe('The install finished.');
    expect(installEndLine({ ok: false, ended: 'exited', exit_code: 1 })).toContain('exit code 1');
    expect(installEndLine({ ok: false, ended: 'closed', exit_code: null })).toContain('closed');
  });
  it('shows the model download and offers a retry when it failed', () => {
    expect(speechModelLine({ state: 'downloading', done: 50_000_000, total: 147_964_211, error: null })).toEqual({ text: 'Downloading the speech model: 50 MB of 148 MB', retry: false, busy: true });
    expect(speechModelLine({ state: 'failed', done: 0, total: null, error: 'the hash did not match' })).toMatchObject({ retry: true, text: "The speech model didn't download: the hash did not match" });
    expect(speechModelLine({ state: 'ready', done: 1, total: 1, error: null }).retry).toBe(false);
  });
});

describe('verifyIsPrimary and speechModelProgress', () => {
  it('makes Verify the primary only when it is the way forward', () => {
    const idle = { installStarted: false, signInOpened: false };
    expect(verifyIsPrimary('guide', idle)).toBe(true);
    expect(verifyIsPrimary('app', idle)).toBe(true);
    expect(verifyIsPrimary('install', idle)).toBe(false);
    expect(verifyIsPrimary('install', { ...idle, installStarted: true })).toBe(true);
    expect(verifyIsPrimary('mcp_oauth', idle)).toBe(false);
    expect(verifyIsPrimary('mcp_oauth', { ...idle, signInOpened: true })).toBe(true);
    expect(verifyIsPrimary('mcp_token', idle)).toBe(false);
    // A local server: Save key is the way forward until the server is added.
    expect(verifyIsPrimary('mcp_stdio', idle)).toBe(false);
    expect(verifyIsPrimary('mcp_stdio', { ...idle, stdioAdded: true })).toBe(true);
  });
  it('gives a bar only for a download of known size', () => {
    expect(speechModelProgress({ state: 'downloading', done: 74_000_000, total: 148_000_000 })).toEqual({ done: 74_000_000, total: 148_000_000, percent: 50 });
    expect(speechModelProgress({ state: 'downloading', done: 5, total: null })).toBeNull();
    expect(speechModelProgress({ state: 'ready', done: 1, total: 1 })).toBeNull();
  });
});
