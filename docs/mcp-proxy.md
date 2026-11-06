# MCP servers through maiTerm

2026-10-13. Darryl asked whether maiTerm could manage MCP servers. The case he had in mind was a
shared environment: "I want my mcps authenticated with my users, while someone else connecting
to the shared environment would want theirs." Two more reasons followed:
- cross-AI: one setup serves Claude, Codex and Gemini;
- "if someone is asking an agent to install an mcp, the agent knows [maiTerm] is an option and can
  present it so [the] human can make [an] informed decision".

His condition: "so long as we don't create any problems with tool discovery and tool updates."
§4 exists for that sentence.

This is opt-in per server. Most MCPs stay installed natively in each runtime, and nothing here
changes those.

## 1. What it does

maiTerm signs in to a remote MCP server once, holds that sign-in, and serves the server's tools to
every agent through its own MCP server. Each agent sees the proxied server as an ordinary server
of its own (§3), with the upstream's tools unchanged.

Why maiTerm holds the sign-in, rather than writing tokens into each runtime's store:
- **Refresh tokens rotate.** The MCP spec requires rotation for public clients, and every server
  checked that issues refresh tokens (Linear, Notion, Sentry, Higgsfield, GitHub) does. A token
  copied to two holders dies for whichever refreshes second. Langsys issues no refresh tokens at
  all (3-month access tokens), which still makes it a password-grade secret to keep in one place.
- **The runtimes' stores are internal formats.** Claude keeps tokens in the keychain under
  `mcpOAuth` keys of `name|hash`, Codex in its own store, Gemini in
  `~/.gemini/mcp-oauth-tokens.json`. None of them is an interface.
- **A static header goes stale.** Claude does not fall back to OAuth when a configured
  `Authorization` header is refused, and a Codex static header has no refresh hook.

With the proxy the agents never hold the upstream token at all. They hold `MAITERM_AUTH`, which
already reaches maiTerm's own server.

**On a shared host, each person's agents reach their own maiTerm** through their own ssh reverse
tunnel, and so their own sign-in. That is the case Darryl started from, and it needs nothing
beyond the tunnel and remote config maiTerm already writes (§6).

## 2. Scope of v1

- **Upstream transport:** Streamable HTTP. Every server probed uses it. Stdio upstreams (`npx …`)
  came with Hive media's phase 5 (2026-11-05), for local servers that need a key: §2.1.
- **Upstream auth:**
  - none, e.g. mcp.svelte.dev;
  - a pasted bearer token (an API key or personal access token), stored in the keychain;
  - OAuth, per the MCP authorization spec (§5).
- **Runtimes:**
  - local Claude (every managed account) and local Codex;
  - Claude on ssh hosts, through the existing instance-independent entry form (§6).
  - Gemini waits for a maiTerm registrar for Gemini at all; today maiTerm writes no Gemini config.
    Remote Codex config is still baked per port (codex-integration-review C1), so it waits too.
- **Capabilities passed through:** `tools/*`. Resources and prompts are v2. The upstream's
  `instructions` from `initialize` are passed through.

### 2.1 Stdio upstreams (built 2026-11-05)

A local MCP server that needs only a key (MiniMax, Meshy: docs/hive-media.md §12.1) is run by
maiTerm itself and served at `/mcp/x/<id>` like any other proxied server. Agents never see its
command or its key. The code is `mcp_proxy/stdio.rs`.

**The record.** `ProxyServer.transport` is a string, `http` (the default, left out of the file, so
an http record reads and writes exactly as before) or `stdio`. A stdio server carries
`stdio: {route, command, args, env_key, env}`:
- `route` is the catalogue route it was set up from, which detection matches it by;
- `env_key` is the NAME of the variable the key goes in, never the key. It must match
  `^[A-Z][A-Z0-9_]*_(API_KEY|KEY|TOKEN|SECRET)$` (`MINIMAX_API_KEY`, `MESHY_API_KEY`);
- `env` is the route's fixed, non-secret settings besides the key (MiniMax's
  `MINIMAX_API_HOST`, `MINIMAX_MCP_BASE_PATH`, `MINIMAX_API_RESOURCE_MODE`), left out of the file
  when empty. It comes from the catalogue only, never from a webview or an agent. In a value,
  `{output}` stands for the server's output folder and is expanded in Rust at each start; the
  placeholder, not the folder, is saved. A setting may not be the key's variable, and no value
  may hold the key.
- Neither the key's variable nor a setting may be one that steers the child or its launcher, as a
  backstop to the rule above: `PATH`, `HOME`, `TMPDIR`, the shells' (`ENV`, `BASH_ENV`, `IFS`,
  `CDPATH`, `ZDOTDIR`), the proxies and trust stores (`HTTP(S)_PROXY`, `ALL_PROXY`,
  `SSL_CERT_FILE`, `REQUESTS_CA_BUNDLE`), the interpreters' (`PERL5OPT`, `RUBYOPT`,
  `JAVA_TOOL_OPTIONS`), and anything starting `NODE_`, `PYTHON`, `NPM_CONFIG_`, `UV_`, `PIP_`,
  `DYLD_`, `LD_`, `JAVA_`, `XDG_` or `MALLOC`, compared without regard to case
  (`stdio::forbidden_env`). A key in `HOME`, say, would make npm write it to disk as a folder
  name inside the output folder.
Its `url` is `stdio:<route>`, which `valid_url` would refuse and which is never dialled. An older
build reads that url, fails to reach it, and keeps serving the saved list with failing calls. If
it saves the file, it drops the fields it doesn't know, and the server must be removed and added
again. Its `auth` is `key` (or `none` for a route without a key), so an older build never finds a
bearer token for it.

**The child.** There is one child per server, shared by every downstream session.
- That gives up per-agent isolation, which is acceptable for the stateless generators the
  catalogue offers.
- maiTerm numbers every request it sends to the child, so concurrent calls from different agents
  never collide. An agent's own JSON-RPC id never reaches the child; its answer goes back under
  it.
- Framing is newline-delimited JSON-RPC. A line that isn't JSON (a package's banner) is skipped,
  and a request the child makes is answered: `ping` with `{}`, anything else with -32601.
- `initialize` and `notifications/initialized` are sent once per child, and its `initialize`
  result is the catalog's `serverInfo` and `instructions`.
- A `notifications/tools/list_changed` from the child re-reads the list, and a change goes out to
  every downstream stream as §4.2 says.
- It is started as `watch.rs` starts a script: `env_clear`, then the login PATH (`login_path`),
  then the key variable, then the route's fixed settings, and nothing else of maiTerm's
  environment (no HOME, no `MAITERM_*`). It gets its own process group, killed whole when it
  stops, and every child is stopped when the app quits (`stdio::stop_all`, from
  `run_shutdown_cleanup`), since nothing else ends a group outside maiTerm's.
- A stop (a removal, a new key, a disable, the quit) kills a child still in its handshake too, and
  a start that a stop overlapped keeps no child: a removed server never comes back up, and a child
  never keeps an old key.
- A caller that leaves (an agent that disconnects drops its call) leaves nothing waiting, so a
  call nobody waits for never holds off the idle stop.
- Its working directory is `<OS cache dir>/<app_data_slug>/mcp-out/<id>`
  (`stdio::output_folder`), never inside maiTerm's data folder.

**Lifecycle.**
- **Restart.** A child that exits is restarted with backoff (1 s doubling to 60 s, back to 1 s
  after a child that stayed up a minute), as long as an agent used it in the last 10 minutes.
  Calls in flight when it exits answer `isError` with a sentence, and every tool stays listed.
- **Idle stop.** A child that no agent has sent a `tools/call` or `tools/list` for 10 minutes is
  stopped, unless a call is still running.
- **What doesn't count as use.** The proxy's own hourly re-list and a child's `list_changed` read
  only a running child: neither counts as use, and neither starts a stopped one. A restart isn't
  use either, so a child that keeps failing stops being restarted 10 minutes after an agent last
  used it. A child started by Verify, a human's Refresh or a balance check lives 10 minutes from
  its start.
- **A stopped server is `idle`.** That is not an outage, and Hive media counts it as ready. It
  answers `tools/list` from its saved list without starting. A tool call starts it, within 120 s.
- **A failure never removes a tool (§4.1).** A start or call that fails answers `isError` with a
  sentence.

**Verify before write** (`add_stdio`). The setup flow stores the key first, then adds the server,
in this order:
1. Start the child, waiting up to 180 s for `initialize`, since `npx` and `uvx` download the
   package the first time.
2. Read its first `tools/list`.
3. Only then save the server, with that list as its catalog and its workspace scope already set,
   and write its entries into the agents' configs.

So no agent ever sees it with an empty list. If Verify fails, nothing is saved or written and
the child is gone. **The key is left in the keychain**, so a retry needs no second paste, and
`mcp_stdio_key_delete` removes it when the setup is abandoned.

**The key.**
- **Where it lives.** It is kept in the keychain under §5's rules (`secrets::store_env_key`, item
  `<id>#key` under the `.mcp` service). It is read at spawn and reaches the child only through its
  environment. The child's record keeps it for one purpose: **everything the child sends has the
  key replaced with `[key removed]`** before it goes anywhere (`stdio::scrub`): an error's message
  and data, a result's text, a tool's description in the saved list. A server that echoes its key
  can't send it to an agent, a status, Verify or the file.
- **Where it never appears:** argv, a log line, an error, a status, `Debug` output, `.claude.json`,
  the Codex config, `mcp-servers.json`, or any Tauri event or command return. A route that puts
  the key on its command line or in a setting is refused at add, and a child whose replaced key
  turns out to be in one is not started.
- **The child's stderr is never logged or forwarded**, since a server may print its own key. It is
  drained, and only its line count and the exit status are logged. A child's own JSON-RPC error
  goes back to the agent whose call it answers, but its words never reach a status, which is
  logged and sent to the frontend, nor a call maiTerm makes itself (`call_once`, Verify's balance
  check), which gets only the error's code.
- **Who can set it.** Only the human, at the Mac: `mcp_stdio_key_store` (the setup sheet's paste),
  never a maiLink route or an agent tool. Replacing the key stops the child, so the next call
  starts it with the new one.
- **Removing the server** stops its child, deletes its key and removes its entries. Its output
  folder stays: it holds what the server made.

## 3. One server entry per proxied MCP

A proxied server called `linear` is written into each runtime's config as its **own** entry named
`linear`, pointing at `http://127.0.0.1:<port>/mcp/x/linear` (`${MAITERM_PORT}` on ssh hosts).
Its tools are not folded into `maiterm`.

This is what keeps discovery identical to a native install:
- **Tool names stay the same.** They are `mcp__linear__create_issue` in Claude and
  `mcp_linear_create_issue` in Gemini, as a native install would give.
- **Permission rules and runtime toggles keep working.** `/mcp`, enable/disable, and per-server
  allow rules like `mcp__linear__*` apply as usual.
- **List changes stay per server** (§4.2).
- **A failing upstream affects only its own entry.**

Each entry carries `x-claude-code-ide-authorization` (or Codex's equivalent) and `x-maiterm-tab`,
exactly like the `maiterm` entry. A marker header `x-maiterm-proxy: <data slug>/<id>` is how
maiTerm knows the entry is its own when it rewrites or removes it (§6).

**Name collisions.** If a runtime already has a native entry under that name, maiTerm doesn't
overwrite it. The human picks another name, or chooses **Move to maiTerm**, which removes the
native entry because the human clicked it.

**Duplicates.** A native entry with the same URL under another name is shown as a duplicate
("Claude also has this as `linear-native`"), because two copies double every tool in context.
The same check runs at add time and whenever the native configs change.

## 4. Discovery and updates

### 4.1 The list is stable

- **Tools are served verbatim.** Name, description, `inputSchema`, `outputSchema`, `annotations`
  and `_meta` are untouched, in the upstream's order, and pagination is flattened.
- **The catalog is persisted.** The last good `tools/list` of each upstream is saved in AppData.
  It is not a secret: tool schemas are public to anyone signed in. A session that starts while the
  upstream is down, or before maiTerm has reconnected after a restart, gets the same list it got
  yesterday.
- **Failures never remove tools.** A down upstream or an expired sign-in leaves its tools listed.
  A call then returns `isError` with a sentence the model can relay, e.g. "maiTerm: Linear sign-in
  expired. The human can reconnect it in maiTerm → Preferences → MCP servers." The human is told
  once per outage. Dropping tools mid-session would change what the agent can do partway through
  a task, and in Claude it resets the prompt cache.
- **Only the upstream removes a tool.** A removal arrives through a real changed `tools/list`
  (§4.2), and maiTerm does not second-guess it.

### 4.2 Changes are forwarded

maiTerm keeps one catalog connection per upstream, with the `GET` SSE stream open. Upstream
`notifications/tools/list_changed`, a reconnect, or an hourly refresh re-fetches the list. If the
list differs, maiTerm persists it and sends `notifications/tools/list_changed` to every open
downstream stream on that entry. It also advertises `tools.listChanged: true`, so downstream needs
`GET /mcp/x/<id>` streams (maiTerm's own `/mcp` has none today).

How each runtime handles the notice. This was measured on 2026-10-13 against a test server, with
the notice sent both on the GET stream and on a POST response:

| | Claude 2.1.296 | Gemini 0.43.0 | Codex 0.157.1 |
|---|---|---|---|
| Opens `GET /mcp` | yes | yes | yes |
| Re-lists on `list_changed` | yes, within ~1 ms | yes | **no**: it receives the notice and ignores it |
| New tool callable in the same session | yes | yes | no, only in its next session |
| Re-lists without a notice | no | no | no |

So Codex sessions keep the tool set they started with, and the stability rules in §4.1 matter
most there.

Claude hides MCP tools behind tool search. A refreshed list reaches the search index at once, but
nothing announces a new tool to the model.

### 4.3 Context cost

Every proxied server's tools load into every session that sees the entry. Claude defers them
behind tool search; Codex and Gemini load them all. Two consequences:
- **Proxying is opt-in per server.**
- **An entry can be limited to chosen workspaces.** maiTerm knows each request's tab from
  `x-maiterm-tab`. A tab outside the scope gets an empty `tools/list`, and that is stable for the
  tab.

## 5. Signing in (OAuth)

This follows the MCP authorization spec, 2025-11-25 revision, with 2025-06-18 servers handled
too. Facts were checked live on 2026-10-13.

1. **When to sign in.** The need is discovered when the server is added. A 401 cannot be the
   trigger: svelte.dev needs no auth at all, and langsys.dev answers `initialize` and `tools/list`
   unauthenticated and returns 401 only on `tools/call`. So the add flow fetches protected
   resource metadata (PRM) itself:
   - first the `resource_metadata` in a 401's `WWW-Authenticate`;
   - else the path-suffixed `/.well-known/oauth-protected-resource/<path>`;
   - else the root one.
   A 401 in the middle of use also means "sign in again".
2. **Finding the authorization server** from the PRM's `authorization_servers`:
   - **Pick a server that can do this flow.** It must support `authorization_code` and list S256
     in `code_challenge_methods_supported`; refuse if that is missing. Higgsfield lists a
     device-code-only server first.
   - **Try metadata URLs in the spec's order.** RFC 8414 comes first, including the
     path-insertion form GitHub uses, then OIDC. Clerk's OIDC document lacks the
     `registration_endpoint` its RFC 8414 one has.
   - **Retry on server errors.** Higgsfield's PRM returns 502 intermittently.
3. **Registering as a client.** In order:
   - a client id the human supplies (GitHub has neither DCR nor CIMD);
   - else DCR (`token_endpoint_auth_method: none`). The registration is saved, keyed by issuer and
     redirect URI, and redone when the redirect URI changes.
   - CIMD (a maiterm.dev-hosted client document) is v2. The spec itself notes CIMD cannot stop
     another local process from claiming to be maiTerm.
4. **The authorization request.**
   - PKCE S256, a `state` that is checked on return, and `resource` set to the PRM's `resource`
     exactly as written (servers disagree with themselves about trailing slashes and paths).
   - Scopes: the challenge's `scope`, else the PRM's `scopes_supported`, else none. Add
     `offline_access` where it is advertised.
   - The redirect is `http://127.0.0.1:<maiTerm's MCP port>/oauth/callback` on maiTerm's existing
     server, opened in the system browser.
5. **Tokens.**
   - **Storage.** The access token, refresh token and expiry go in the OS keychain under maiTerm's
     own service name (`app_data_slug()`-scoped, `accounts::vault`'s rules: never in a log, an
     error or `Debug`, no plaintext fallback). The keychain being unavailable disables OAuth
     upstreams rather than degrading to a file.
   - **Refresh.** One refresh in flight per upstream. The rotated refresh token is persisted
     before the new access token is used. Refresh runs before expiry and once on a 401.
   - **Step-up.** A 403 `insufficient_scope` triggers a re-authorization with the wider scope, at
     most once per call.
   - **What the human sees.** A refresh failure marks the upstream "needs sign-in" (§4.1).

`docs/login.md` §9 says the setup-token is maiTerm's only secret. This feature adds a second kind,
so that section changes with it.

## 6. Where entries are written

- **Local Claude.** `mcpServers.<name>` in `~/.claude.json` and in every managed account's merged
  file, through the same path that keeps `mcpServers.maiterm` current. The 30 s drift self-heal
  covers them too.
- **Local Codex.** `[mcp_servers.<name>]` in `~/.codex/config.toml`, with `url`, the auth header
  and `env_http_headers` for the tab, the way `codex.rs` writes maiTerm's own table.
- **Claude on ssh hosts.** The remote setup writes the same instance-independent form as the
  remote `maiterm` entry:
  - URL `http://127.0.0.1:${MAITERM_PORT}/mcp/x/<id>`;
  - header `${MAITERM_AUTH}`.
  So every maiTerm writes identical bytes, and each user's agents reach their own maiTerm.
  - **A remote account can be shared** by several of this human's tabs, by other people's
    maiTerms (a team account such as `ews@nova`), and by both flavors. So the remote side is
    **additive only**:
    - entries are written by name;
    - a name held by an entry without the marker is the human's own and is never overwritten;
    - **nothing is ever deleted**. The marker is the same for every maiTerm of a flavor, so it
      can't say which maiTerm wants an entry: deleting the one this maiTerm removed would take it
      from a teammate still using it (review of e363dc33 — the first version had tombstones).
  - **An entry a maiTerm doesn't serve is inert.** `/mcp/x/<name>` for an unknown name answers
    as an empty server (`not_served`): `initialize` succeeds, `tools/list` is empty, a call says
    why. So another person's entry, or a removed or disabled one, shows no failure. A teammate on
    a maiTerm older than this feature answers it with 404, and sees it as a failed server.
  - **The path carries the entry name** (`/mcp/x/linear-dev` from a dev build). Both flavors'
    entries resolve `${MAITERM_PORT}` to whichever maiTerm launched the shell; with a bare id in
    both paths, that one maiTerm served the same server twice.
  - **Changes reach bridged hosts at once.** A change in maiTerm emits
    `mcp-proxy-entries-changed`, and each window re-runs just the merge on the hosts it has
    bridged (`refreshProxyEntries`). A host not bridged now gets it at its next connect.
  - Code: `entries::remote` builds `{set}`; `sshMcpBridge.svelte.ts` merges it with python3, or jq
    where there is no python3.
- **When they are written.**
  - At startup, before any tab spawns.
  - Re-asserted by the 30 s drift loop. That loop also checks each managed account root,
    because a running `claude` saves its in-memory copy of the file back over the root's.
  - Rewritten when a server is added, removed, enabled or disabled, or when a runtime's
    integration is switched.
  - Removed at exit, like `maiterm` itself: the port changes every launch, and an entry dialling
    a dead port is a failed server in every session.
- **Which maiTerm owns an entry.** The marker header's value is `<data slug>/<id>`
  (`com.aiterm.app/linear`). A dev build names its entries `<id>-dev`. The installed app and a
  dev build share these files, and each leaves the other's entries alone: an entry marked by
  another flavor counts as a collision, never as stale.
- `entries.rs` is the code.

## 7. Adding a server

**The human adds a server in maiTerm → Preferences → MCP servers.**
- The list shows each server's status (connected, needs sign-in, unreachable), tool count, which
  runtimes get it, and its workspace scope.
- **Add** takes a URL and a name (suggested from the host). It probes and shows what it found:
  "no sign-in needed", "OAuth via linear.app", or "needs a token". Then Sign in or Save.
- Each server also has **Sign in again**, **Remove**, and **Move to maiTerm** on a native
  duplicate.
- **The Hive media setup sheet** (docs/hive-media.md §6) adds a provider's server through the same
  path, from the human's click: the name suggestion, the URL comparison and the sign-in opener are
  one module (`lib/mcp/addServer.ts`) shared by this pane, an approved `mcp_add` and the sheet. The
  sheet applies `mcp_add`'s checks at filing rather than refusing: it reuses a server for a URL
  maiTerm already serves, and suffixes a name a native entry holds (`fal-2`). It then scopes the
  server (a Hive's own workspace, or the human's choice from Preferences). The sign-in, here and
  there, opens only an http(s) page.

**An agent can propose a server but never add one.** Agents get two tools on maiTerm's own
server:
- **`listMcpServers`** (read-only). It returns the managed servers with their status and tool
  counts, and the native entries that duplicate them.
- **`addMcpServer`**. It files an agent request of kind `mcp_add` (docs/agent-requests.md), which
  the human approves on the card in the Loom's Decisions or on the phone.
  - **The description** tells the agent: when a human asks to install or add an MCP server, tell
    them maiTerm can manage it, give the trade-off, and let them choose. Do not call this before
    they have chosen.
  - **The card's words are maiTerm's.** It shows the URL (origin and path), the name, which
    runtimes get it, whether a sign-in follows, and that the server's tools load into every
    session started afterwards. The agent's `reason` is shown quoted.
  - **Approval re-checks.** Carrying out an agent's request, `add` refuses a URL maiTerm already
    serves, however it was added meanwhile. The human may add one URL twice on purpose (two
    accounts of one service, scoped to different workspaces), so their own add form isn't held to
    it. URLs compare with a case-insensitive scheme and host and a case-sensitive path.
  - **Approving adds the server.** Approved at the desktop, an OAuth sign-in opens in the browser
    at once. Approved from the phone, it waits as "needs sign-in" for the human at the Mac. A
    token server waits for the human to paste one. Neither the agent nor the approval completes a
    sign-in.
  - **Refused when it is filed:**
    - a URL that may carry a credential: user info, a fragment, any query string, or a path
      segment that reads as a key (long, letters mixed with digits, as Zapier's `/s/<key>/mcp`).
      It would reach the card, the request, the plaintext config and every agent's
      `listMcpServers`. The human can add such a server themselves;
    - a name the human's own entry holds in Claude's or Codex's config (it would never be written
      there);
    - a service that doesn't let apps register (GitHub), which needs a client id or token only
      the human has: the agent is told to point them at the add form;
    - a name or URL maiTerm already serves.
  - **The trade-off, as the tool and the card put it:**
    - Through maiTerm: one sign-in shared by Claude and Codex here and Claude on your ssh hosts,
      signed in as you on a shared host, and kept fresh by maiTerm.
    - Native: works without maiTerm running, and each runtime signs in for itself.

**A phone approval can't finish a sign-in.** The OAuth redirect is loopback on the Mac, so the
phone cannot complete it. A phone-approved server that needs one waits as "needs sign-in" until the
human is at the Mac, and the phone card says so. This is a deliberate exception to "never go to
desktop", and it is open to a better answer (an https redirect through the maiTerm relay is the
candidate).

## 8. Security

- **Who can reach upstream tools.** Anything holding `MAITERM_AUTH` can call them, which is the
  same set of processes that can already call maiTerm's own tools. A native install gives the
  same agents the same reach.
- **Proxy traffic is never logged.** Tool arguments and results are not logged, only server, tool
  name, status and duration.
- **No token passthrough.** The upstream token goes only to that upstream. The downstream
  `MAITERM_AUTH` never goes upstream.
- **What persists where.**
  - Plaintext in AppData: the catalog, the client registration (client id only), the scope,
    names and URLs. A URL the human adds by hand is stored as typed, so one that embeds its key
    is plaintext there too (agents see it masked in `listMcpServers`, and can't propose one).
  - The keychain: everything else.
  - Never on the phone or in a share file.

## 9. Build order

1. Upstream client (Streamable HTTP: initialize, list with pagination, call, GET stream), catalog
   persistence, `/mcp/x/<id>` downstream with GET streams, error semantics. Auth: none and pasted
   token.
2. Entry writers: local Claude (all accounts) and local Codex, plus the duplicate and collision
   checks.
3. OAuth: discovery, DCR, PKCE, callback, keychain, refresh, step-up.
4. Preferences → MCP servers.
5. `listMcpServers` / `addMcpServer` + the `mcp_add` request kind and card.
6. ssh hosts (Claude).
7. v2: Gemini registrar, resources/prompts, CIMD, remote Codex. (Stdio upstreams were built
   2026-11-05 for Hive media, §2.1.)

**Status 2026-10-13:** steps 1–6 built and reviewed (each step's review found real defects, all
fixed; last commit 03ff26e6). Not yet deployed. Verified on dev:
- real Claude and Codex calling proxied tools through the written entries;
- list changes forwarded, and an outage keeps the tools listed;
- OAuth against strict mocks (registration, PKCE, `resource` check, rotating refresh with family
  revocation, no `expires_in`, a foreign `resource` refused);
- discovery probes against langsys, linear, notion, sentry, higgsfield and github;
- the agent tools over the real MCP path;
- the remote merge under nova's Python 3.8, in a scratch HOME.

**Still to verify after deploy:**
- a real remote Claude reaching a proxied tool through the tunnel. Not done from dev: the
  released v3.6.0 answers `/mcp/x/<unknown>` with 404, so dev entries on a shared account show as
  failed servers in sessions started from it;
- a real OAuth sign-in to a real service.
