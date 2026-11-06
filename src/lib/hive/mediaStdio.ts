/** The media setup sheet's `stdio_mcp` arm (docs/hive-media.md §12.1): a local MCP server maiTerm
 *  runs itself, with a key the human pastes. Plain TypeScript (no runes) so its order and its key
 *  handling are tested: `HiveMediaSetupSheet.svelte` mirrors `StdioSetup`'s state into its own.
 *
 *  The order is fixed: the key goes to the keychain (`mcpStdioKeyStore`), then the server is added
 *  from its catalogue route (`mcpStdioServerAdd`: Rust makes the command, starts it and reads its
 *  first tool list before anything is written), then Verify. A failed add leaves the key stored, so
 *  "Try again" needs no second paste, and "Remove the stored key" forgets it (`mcpStdioKeyDelete`).
 *
 *  The key is a parameter of `submit` and nothing else: no field, state, log line or error message
 *  here ever holds it. The sheet empties its password field before it calls `submit`. */

import type { HiveMediaStdioRoute } from '$lib/tauri/types';
import type { McpServerView } from '$lib/tauri/commands';
import { freeServerName } from '$lib/mcp/addServer';

/** Rust refuses a shorter key (`mcp_proxy::secrets::MIN_KEY_LEN`): the sheet says so before
 *  sending. */
export const MIN_KEY_LEN = 16;

/** A hint under the key field, or null: only a non-empty paste shorter than a whole key. */
export function keyHint(typed: string): string | null {
  const n = typed.trim().length;
  if (n === 0 || n >= MIN_KEY_LEN) return null;
  return `That looks too short for a whole key: keys are ${MIN_KEY_LEN} characters or more. Check you copied all of it.`;
}

/** Whether Save can send what is typed. */
export function keySendable(typed: string): boolean {
  return typed.trim().length >= MIN_KEY_LEN;
}

/** The route the sheet starts on: the first (MiniMax's global account). */
export function defaultRoute(routes: readonly HiveMediaStdioRoute[]): HiveMediaStdioRoute | null {
  return routes[0] ?? null;
}

/** Reuse the server maiTerm already runs from this route, else add one under a free name. */
export type StdioPlan = { reuse: McpServerView } | { add: string } | { error: string };

/** A provider has ONE stdio server: the one set up from any of its routes (`routeIds`), the chosen
 *  route's first. A reuse on another route is moved to the chosen one in Rust (`setRoute`), so
 *  fixing a MiniMax key pasted for the wrong region never leaves a second, broken server behind.
 *  A new one is named after the provider, suffixed past another server's id or a name a native
 *  entry holds. */
export function stdioPlan(servers: readonly McpServerView[], routeId: string, providerId: string, nativeNames: readonly string[], dev: boolean, routeIds: readonly string[] = [routeId]): StdioPlan {
  const stdio = servers.filter((s) => s.transport === 'stdio' && s.route !== null);
  const served = stdio.find((s) => s.route === routeId) ?? stdio.find((s) => routeIds.includes(s.route ?? ''));
  if (served) return { reuse: served };
  const name = freeServerName(providerId, servers.map((s) => s.id), nativeNames, dev);
  return name ? { add: name } : { error: 'maiTerm couldn’t find a free name for this server. Remove an unused one in Preferences → MCP servers.' };
}

/** What `submit` acts on, decided by the sheet at the click. */
export interface StdioTarget {
  /** The server id the key is stored under and the server is added as. */
  id: string;
  routeId: string;
  /** The server maiTerm already runs from this route: its key is replaced, nothing is added. */
  reuse: McpServerView | null;
  /** A new server's workspaces ([] every workspace). */
  scope: string[];
  /** A reused server's new workspaces, or null to leave them. */
  reuseScope: string[] | null;
}

export interface StdioDeps {
  keyStore(id: string, key: string): Promise<void>;
  /** Move a reused server to another route of its provider (the other account's region). */
  setRoute(id: string, routeId: string): Promise<McpServerView>;
  serverAdd(id: string, routeId: string, workspaces: string[]): Promise<McpServerView>;
  setWorkspaces(id: string, workspaces: string[]): Promise<unknown>;
  keyDelete(id: string): Promise<void>;
  /** The sheet's Verify. It reports its own answer; a throw here is the sheet's to show. */
  verify(): Promise<void>;
  /** A server was added or its key replaced: the write landed, whether or not the sheet is open. */
  landed(server: McpServerView): void;
}

/** - `key`: waiting for a paste;
 *  - `storing`: the key is on its way to the keychain;
 *  - `starting`: the server's first start (a download) and first tool list;
 *  - `failed`: the start failed with the key stored: Try again, or Remove the stored key;
 *  - `verifying`, `added`: the server is added (or its key replaced), Verify runs / ran;
 *  - `removing`: the stored key is being deleted. */
export type StdioStage = 'key' | 'storing' | 'starting' | 'failed' | 'verifying' | 'added' | 'removing';

export interface StdioState {
  stage: StdioStage;
  /** The id a key is stored under, once one is. */
  keyFor: string | null;
  /** The server added or reused. */
  server: McpServerView | null;
  /** What went wrong, in words (Rust's, which never carry the key). */
  error: string | null;
  /** The last step's outcome, when it isn't an error. */
  note: string | null;
}

function words(e: unknown): string {
  return (e instanceof Error ? e.message : String(e)).replace(/^Error: /, '');
}

export class StdioSetup {
  #s: StdioState = { stage: 'key', keyFor: null, server: null, error: null, note: null };
  #target: StdioTarget | null = null;
  #deps: StdioDeps;
  #onchange: (s: StdioState) => void;
  /** False once the sheet is gone: nothing more is reported (writes in flight still finish). */
  alive = true;

  constructor(deps: StdioDeps, onchange: (s: StdioState) => void) {
    this.#deps = deps;
    this.#onchange = onchange;
  }

  get state(): StdioState {
    return { ...this.#s };
  }

  get busy(): boolean {
    return ['storing', 'starting', 'verifying', 'removing'].includes(this.#s.stage);
  }

  #set(patch: Partial<StdioState>) {
    this.#s = { ...this.#s, ...patch };
    if (this.alive) this.#onchange(this.state);
  }

  /** Store the key, then add the server (or replace a reused one's key), then Verify. The key is
   *  used for the one call and kept nowhere. */
  async submit(key: string, target: StdioTarget): Promise<void> {
    if (this.busy) return;
    this.#target = target;
    this.#set({ stage: 'storing', error: null, note: null });
    try {
      await this.#deps.keyStore(target.id, key);
    } catch (e) {
      this.#set({ stage: 'key', error: words(e) });
      return;
    }
    this.#set({ keyFor: target.id });
    return target.reuse ? this.#reuse(target.reuse, target.routeId, target.reuseScope) : this.#add();
  }

  /** The step after the key failed with the key stored: run it again, no second paste. */
  async retry(): Promise<void> {
    const t = this.#target;
    if (this.busy || this.#s.stage !== 'failed' || !t || !this.#s.keyFor) return;
    return t.reuse ? this.#reuse(t.reuse, t.routeId, t.reuseScope) : this.#add();
  }

  /** Forget the key this sheet stored (the setup is abandoned). Back to the paste. */
  async removeKey(): Promise<void> {
    const id = this.#s.keyFor;
    if (this.busy || this.#s.stage !== 'failed' || !id || this.#target?.reuse) return;
    this.#set({ stage: 'removing', error: null, note: null });
    try {
      await this.#deps.keyDelete(id);
      this.#set({ stage: 'key', keyFor: null, note: 'The stored key is removed.' });
    } catch (e) {
      this.#set({ stage: 'failed', error: words(e) });
    }
  }

  /** The provider's server maiTerm already runs: Rust stopped its child when the key was replaced,
   *  and the next call starts it with the new one. Moved to the chosen route if it is on another
   *  (a key for the other region), and offered to this Hive if it wasn't. */
  async #reuse(found: McpServerView, routeId: string, scope: string[] | null): Promise<void> {
    this.#set({ stage: 'starting', error: null, note: null });
    let s = found;
    try {
      if (s.route !== routeId) s = await this.#deps.setRoute(s.id, routeId);
      if (scope) await this.#deps.setWorkspaces(s.id, scope);
    } catch (e) {
      this.#set({ stage: 'failed', error: words(e) });
      return;
    }
    this.#deps.landed(s);
    this.#set({ server: s, note: `Key replaced for “${s.entry_name}”. Checking it works…` });
    return this.#verify();
  }

  async #add(): Promise<void> {
    const t = this.#target;
    if (!t) return;
    this.#set({ stage: 'starting', error: null, note: null });
    let s: McpServerView;
    try {
      s = await this.#deps.serverAdd(t.id, t.routeId, t.scope);
    } catch (e) {
      this.#set({ stage: 'failed', error: words(e) });
      return;
    }
    this.#deps.landed(s);
    this.#set({ server: s, note: `Added as “${s.entry_name}”. Checking it works…` });
    return this.#verify();
  }

  async #verify(): Promise<void> {
    this.#set({ stage: 'verifying' });
    try {
      await this.#deps.verify();
    } finally {
      this.#set({ stage: 'added', note: null });
    }
  }
}
