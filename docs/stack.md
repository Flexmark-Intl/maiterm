# maiTerm Stack — a workspace's services, known to every tab in it

> Status: **proposed** 2026-09-11. Owner: Darryl.
> Scope: a workspace can declare the services its project runs (dev server, API, db,
> worker…), start them as tabs it owns, and expose them — status, ports, logs, control —
> to every human and agent tab in that workspace. Agents are writers, not just readers.

## 1. Why

Solo (soloterm.com, 0.10.1) has one idea worth taking: a project declares its *commands*
(`solo.yml`), the app runs them as managed processes, and everything else in the app —
including agents over MCP — can see and control them. In maiTerm today the same project
is five terminal tabs someone typed `npm run dev` into, and nothing knows which tab is the
API, whether it is up, or what port it took. An agent that breaks the dev server finds out
when its human tells it.

What is **not** worth taking is Solo's model. There, *process* is the primitive and a
terminal is one kind of process; maiTerm's primitive is the tab+PTY, and every subsystem
hangs off it — scrollback, search, OSC 133 exit codes, activity indicators, triggers,
`getTabContext`, Overlord rules, session restore, suspend/resume, the phone. A separate
"managed process" type would rebuild half of that.

> **Decision:** a service **is a terminal tab** with a binding. The stack is workspace
> state, maiTerm-owned, edited from the app and over MCP — the same shape as tasks
> (`docs/tasks.md`). No repo file in v1.

## 2. Shape

```
Workspace.stack[]                  ← source of truth (a workspace IS a project)
  ├─ Service tabs                  ← one live tab per running service, shell intact
  ├─ MCP: listStack / *Service / updateService / waitForService   ← agents, every runtime
  ├─ Priming line at SessionStart  ← "this workspace runs: web (ready :5173), api (crashed)"
  ├─ Sidebar section + rollup dot  ← humans
  ├─ System triggers               ← readiness / port capture on the service tab
  ├─ Overlord                      ← one consumer: service_crashed → Restart
  └─ maiLink                       ← deferred (§10)
```

### Why the definition is not a yaml file in the repo

- **A workspace has no root folder.** `Workspace` carries no path; the folders an agent is
  told about are derived from tab cwds at call time (`handleGetWorkspaceFolders`,
  `claudeCode.svelte.ts` — the Rust `collect_workspace_folders` is the SSH-install path and
  returns `$HOME`), and a workspace legitimately spans two repos or an SSH host. A
  `maiterm.yml` needs a root to be found from, which would mean inventing that concept to
  serve the file.
- **maiTerm owns state; repos and agents are consumers.** Same argument as tasks §2: one
  writer, validated payloads, instant UI, no file watcher, and it works on SSH tabs where a
  file would be on the wrong host.
- What Solo's yaml actually buys — a 10-second first run — is bought instead by the
  **suggester** (§8), which reads `package.json`/`Procfile`/`docker-compose.yml` and offers
  rows to import. Export to a shareable file can come later if a team ever needs it.

## 3. Data model

```rust
// src-tauri/src/state/workspace.rs — on Workspace, beside `tasks`/`workstreams`
pub struct Service {
    pub id: String,
    pub name: String,                 // "web", "api" — the handle agents use
    /// Case/whitespace-normalized name, the dedup key within a workspace. Recomputed on
    /// persist, same contract as `Workstream::normalized_name`.
    pub normalized_name: String,
    pub command: String,              // typed into the tab's shell, verbatim
    pub cwd: String,                  // absolute; defaults to the creating tab's last_cwd
    pub env: Vec<(String, String)>,   // exported before the command
    /// Mirrors Tab.auto_resume_ssh_command — None in v1 (local only), but the field exists
    /// so a remote service is a flag flip, not a schema change.
    pub ssh_command: Option<String>,
    pub auto_start: bool,             // start with the workspace
    /// "never" | "on_crash". "on_change" is v3 (needs a file watcher).
    pub restart: String,
    /// Regex over the tab's stripped output; first match → status ready. Optional named
    /// group `port` captures the port. Becomes a system trigger (§6.4).
    pub ready_pattern: Option<String>,
    /// Last known endpoint. Written by the ready trigger or by an agent over MCP
    /// (`updateService`) — never by socket sniffing. Persisted: the next start usually
    /// lands on the same port, and a stale value is labelled, not trusted (§9).
    pub port: Option<u16>,
    pub url: Option<String>,
    /// "human" | "agent" | "suggested"
    pub origin: String,
    pub created_at: String,
    pub updated_at: String,
}

// On Tab — the binding lives on the TAB, not on the service
pub service_id: Option<String>,
```

**The binding is on the tab and only on the tab.** `Service` has no `tab_id`. "Which tab
runs `api`" is derived: the tab in this workspace whose `service_id == api.id`. This is what
makes the reload/duplicate/restore rules (§5) fall out of existing machinery instead of
needing a `remapTab` call per lifecycle path — the tasks system stores `tab_id` on the row
and pays for it on every reload.

**Runtime state is not persisted.** `status`, `since`, `last_exit_code`, and the pid live in
the frontend store (`src/lib/stores/stack.svelte.ts`), rebuilt on boot. Persisting a status
is how a suspended workspace comes back reporting "crashed" for services nobody started.

### Status

```
stopped   no service tab, or the tab's shell is at a prompt with exit 0 / SIGINT
starting  command sent, no ready match yet (and no exit)
running   process alive, no ready_pattern defined → this is as good as it gets
ready     ready_pattern matched, or an agent set ready over MCP
crashed   command exited non-zero (OSC 133 D), or the tab's shell itself died
```

A dead shell is the rare case and it is **already handled by deletion**: `pty-close-${ptyId}`
carries no payload and its one listener (`TerminalPane.svelte`) deletes the tab unless a
suspend is in flight. The stack store does not fight that — it observes the close of a
bound tab, records `crashed` (unless a stop was in flight), and the next start mints a fresh
tab. Because the binding lives on the tab, "no bound tab" is a perfectly good `stopped` /
`crashed` representation; nothing has to be respawned in place.

`stopped` and `crashed` are different lanes for the same reason `done` and `dropped` are:
a suspend, a Ctrl-C, and a human `stopService` all land on `stopped`, and none of them may
raise an Overlord card. Only a non-zero exit or a dead PTY is `crashed`.

## 4. A service is a tab, and its shell stays up

A service tab is an ordinary terminal tab (`tab_type: 'terminal'`) that maiTerm spawned
into `cwd`, exported `env` into, and typed `command` into. The shell underneath is the
user's normal shell with shell integration injected — so the **process exits, the shell
does not**. That single fact makes the lifecycle cheap:

| Verb | Mechanism | Nothing new needed |
|---|---|---|
| start | `send_command`-style PTY write: `cd`, exports, command, CR | trigger action already exists |
| stop | write `^C`; if still foreground after 3s, `^C` again; then `kill` the child | needs the foreground query below |
| restart | stop, then start, same tab, same shell | — |
| exit detection | OSC 133 `D;<code>` on the service tab | `term-osc133-${ptyId}` carries the code |
| shell death | `pty-close-${ptyId}` — the tab is deleted; store records `crashed` | existing event + listener |
| logs | `getTabContext` on the bound tab — reads the Rust grid while the pane is registered | existing tool |
| readiness | system trigger scoped to the tab via `Trigger.tabs` | trigger engine, per-tab scope exists |

The human can click into the tab, Ctrl-C it, poke at the process, and run the command
again by hand — the tab reads the OSC 133 result either way and the status follows. A
service tab is never a black box the way a managed process is.

**Never type into a foreground that isn't ours.** Every write is gated on the foreground
executable matching the service, or the shell sitting at its prompt. The comms watcher
learned this the hard way (`agent_owns_terminal`): a wrong "yes" types a command into
whatever the human left running there. A wrong "no" costs a retry.

> **This guard needs one new primitive.** `PtyInfo.foreground_command` is *not* the
> foreground executable — on every platform it reports `Some` only for `ssh`/`mosh`/`autossh`
> and `None` otherwise, and `AgentLiveness` is `{agent_running, ssh_foreground}`. Neither can
> say "the foreground is `node`" or "the shell is at its prompt". The unix code already finds
> the foreground job leader and then filters it to ssh names (`pty/manager.rs` ~1263); a
> `foreground_executable(pty_id) -> Option<String>` that skips the filter is the whole
> change, plus the Windows equivalent. `spawn_blocking`, per the pinwheel rule. v1 item.

### The four tab states, applied

| State (docs/overlord.md §11) | A service tab there means |
|---|---|
| live, visible | running normally |
| live, in a background workspace | the normal case. Its `TerminalPane` is mounted but hidden — that is how every running tab in a non-active workspace already works (restore mounts them and they stay mounted; unmount kills the PTY). Rust-side hidden-tab frame gating makes it cost nothing to watch |
| suspended tab / suspended workspace | `stopped`; definition intact; `resumeWorkspace` re-runs `auto_start` services |
| archived | not allowed — archiving a service tab **stops** it and unbinds; the definition stays (§5) |

**Spawn is mount-driven, and the headless primitive already exists.** `spawnTerminal` runs
in `TerminalPane`'s `onMount`, and session restore does not spawn any other way — it adds
each tab to the activated sets and awaits the pane's registration (`driveRestore`,
`+page.svelte`). The same file has an `activate-tab` window event whose stated purpose is to
mount a tab "so its TerminalPane spawns a PTY … without switching what the desktop is
showing". `startService` on a background workspace is that event followed by the PTY write
once the pane registers. No navigation, no fallback.

## 5. Lifecycle rules

- **Workspace open / resume** → `auto_start` services start, serially, before other tabs.
- **Suspend** → all services `stopped` (the PTYs die with the workspace). Not `crashed`.
- **Crash** → `restart: on_crash` re-sends the command with backoff (1s, 2s, 4s… cap 30s)
  and a **ceiling of 5 in 10 minutes**, after which the service stays `crashed` and the
  Overlord card asks a human. Without the ceiling a hidden tab loops at line rate.
- **Cmd+W on a service tab** → stop + unbind, keep the definition. Removing a service is a
  sidebar action, never a tab close. (The two-press confirm still applies.)
- **Duplicate** → the copy has no binding. A duplicate of the api tab is a plain terminal in
  the api's cwd — genuinely useful — and two tabs claiming `api` would be the comms-binding
  bug again. This falls out for free: there is no Rust `duplicate_tab`; the TS `duplicateTab`
  (and the Cmd+D split path) call `createTab` → `Tab::new()` and copy fields one at a time,
  so an uncopied field stays `None`. **The path that forces a decision is
  `clone_workspace_with_id_mapping`** (`commands/window.rs`, behind `duplicate_workspace` and
  `duplicate_window`): it builds an exhaustive `Tab { … }` literal, so adding `service_id` is
  a compile error until someone writes `service_id: None` there. That is the right answer —
  a duplicated workspace copies the *stack definition* (it is on `Workspace`) but none of its
  services are running in the copy, so no tab in it may claim one.
- **Reload** → `carry_tab_record` copies the whole record, so `service_id` rides
  automatically and the derived binding moves to the new tab when the original is deleted.
  No remap. Reload of a running service is a restart (the new tab's shell is fresh).
- **Archive** → refused for a bound tab; the UI offers "Stop and archive", which unbinds
  first. A service is never in `archived_tabs`.
- **Move tab to another workspace** → clears `service_id` (the stack is per-workspace).
  Tasks has the same open item (§8.4 there); here it is decided.
- **Session restore** → the bound tabs come back through the ordinary restore; since the
  binding is a tab field, nothing to reconcile. Status is `stopped` until the command runs.
- **Workspace delete** → services go with it. **Workspace export/import** → the stack
  travels (it is on the workspace), tabs do not.

## 6. Awareness — four channels, different in kind

### 6.1 MCP (the one that matters)

Frontend-handled tools (dispatch in `claudeCode.svelte.ts`, like the Overlord tools),
because start/stop are PTY operations the store owns. Scoped to the **calling tab's
workspace** via connection→tab affinity, exactly as the task tools are; a tab cannot see or
touch another project's stack. All gated on a `stack_enabled` preference (default on) so
the schemas can be switched off wholesale.

| Tool | What it does |
|---|---|
| `listStack` | Every service: `name`, `status`, `since`, `port`, `url`, `cwd`, `command`, `last_exit_code`, `tab_id`, and `endpoint_source: 'observed' \| 'reported' \| 'stale'` (§9) |
| `getServiceOutput { service, lines }` | Recent stripped output of the bound tab. `getTabContext` under a name an agent will reach for |
| `startService` / `stopService` / `restartService { service }` | The verbs. Reply carries the resulting status; `restartService` waits up to 10s for ready before replying, so an agent's next step sees a live server |
| `startStack` / `stopStack` | All `auto_start` services / all running services |
| `waitForService { service, timeout }` | Block until `ready` (or `running` when no pattern), else return the status and the last 20 lines |
| `updateService { service, port?, url?, ready?, note? }` | **The agent as writer.** It ran the server and read ":5173" — it reports it. `ready: true` sets status when no pattern exists. `note` appends to a bounded log on the service, same shape as task notes |
| `createService { name, command, cwd?, env?, auto_start?, restart?, ready_pattern? }` | Idempotent by normalized name (returns the existing one). `origin: 'agent'`. Does not start it — an agent that wants it up calls `startService`, which is one more call and one more thing the human can see |
| `removeService { service }` | Refused while running; stop first. Human-only delete was the tasks rule; here an agent that created a service may remove it (`origin: 'agent'` only) |

Two guards carry over from the Overlord tools verbatim: workspace scope, and
`overlord_exempt` workspaces refuse. Whether an agent may `stopStack` at all is a
preference, `stack_agent_control` (default on — the human sees every write in the sidebar
and the ledger, and an agent that cannot restart the server it just broke is worse than one
that can).

**Inferred identity refuses the write verbs.** `startService`/`stopService`/`restartService`/
`updateService`/`createService`/`removeService` join `PEER_ADDRESSING_TOOLS`: a reconnect
that *guessed* the tab must not stop another project's database. Reads keep the
convenience.

### 6.2 Priming

`session_priming_text` gains a line on every agent tab whose workspace has a non-empty
stack, **rendered from live state**, not static text like the task line:

> "This workspace runs a stack maiTerm manages: web (ready, http://localhost:5173), api
> (crashed, exit 1 at 08:12), db (running). Use listStack / getServiceOutput /
> restartService rather than starting these yourself; if you start a server maiTerm does
> not know about, register it with createService, and report a port you observed with
> updateService."

It fires on resume/fork/compact too, which is exactly when a mid-project agent has lost
track of what is running.

### 6.3 Env at spawn

Every tab spawned in the workspace gets `MAITERM_STACK=web,api,db` and, per service with a
known endpoint, `MAITERM_SVC_WEB_URL` / `MAITERM_SVC_WEB_PORT`. Cheap, and right for shells
and scripts. It is a **snapshot**: a tab opened before the api came up carries stale
values, which is why agents are pointed at MCP instead. v2.

### 6.4 Triggers

Readiness detection *is* a trigger: `ready_pattern` becomes a **system-owned trigger**
scoped to the service tab — `set_tab_state(ready)`, `%port` captured if the pattern names
it. The engine already does regex over stripped output with capture groups into
`trigger_variables`, dedup, cooldown, **and per-tab scope** — `Trigger.tabs: string[]` is
filtered in `processOutput` beside the workspace filter. The only thing missing is a
`system: true` flag so these never appear in the trigger editor and are never swept into
`hidden_default_triggers`.

The 15s auto-resume suppression window is decided per pane mount from the tab's
`autoResumeCommand`, and a service tab has none (its command is typed by the store, not
replayed by auto-resume), so it already gets the plain 2s window. Keep it that way: a dev
server that prints "ready" in 900ms would be missed under the long window — which is also
the reason **not** to implement `start` through the auto-resume path.

### 6.5 Overlord

- A new rule, `service_crashed` (`crashed` for > 10s with the restart ceiling hit, or
  `restart: never`). Its card carries **Restart** and **Show output** — doctrine says a card
  that only describes a problem is a bug.
- `driveTab` **refuses** service tabs. Neither `overlord_exempt` nor "agent tab" fits: the
  engine should see them (to raise the card) but never type into them. Classification is
  `tab.service_id.is_some()`; `listWorkspaces` reports `kind: 'service'` so the agent knows
  without probing.
- Fleet views count service tabs separately so a workspace with four services does not
  read as four dormant agents.

## 7. UI

**Sidebar.** A collapsible **Stack** section under the workspace row (the workstream rail
is the pattern): one line per service — status dot, name, `:port` when known, uptime.
Click → `navigateToTab` (starts it first if stopped, with a confirm when the workspace is
suspended). Right-click → Start/Stop/Restart · Edit · Remove. The sidebar has no context
menu today (`WorkspaceSidebar.svelte`, none in 1246 lines); `TerminalTabs.svelte` has one —
lift it into a shared component rather than growing a second.

**Rollup dot on the workspace row**, batch semantics like the Claude indicator: green when
every `auto_start` service is ready/running, amber while any is starting, red if any is
crashed, none when the stack is empty or fully stopped.

**Workspace submenu** (the row's `⋯`, new): Start stack · Stop stack · Restart stack · Add
service… · Import from project… (§8).

**Tab strip.** Service tabs are shown, in the pinned cluster, with a distinct glyph. Simplest
possible v1; a "collapse services" toggle is v2 if the clutter is real. Names are the
service name and `custom_name` is set, so OSC titles from the process don't rename them.

**The service tab itself** gets a one-line header strip above the terminal — status, port,
Restart — the same slot the SSH bridge bolt and the `@` comms badge use in the tab bar, but
in-pane so it is visible while you read the log.

**Editing.** A small form (name, command, cwd, env rows, auto-start, restart policy, ready
pattern with a "test against current output" button). Inline in the sidebar section is too
narrow (200–320px, and `white-space: nowrap` there is a regression); a modal following
`HelpModal.svelte`.

## 8. The suggester

`suggest_stack(cwd)` — a Rust command, `spawn_blocking`, no shell-outs:

| Source | Rows |
|---|---|
| `package.json` | scripts named `dev`, `start`, `serve`, `watch`, `storybook`, `test:watch`… as `npm run <name>` (or `pnpm`/`bun`/`yarn` by lockfile) |
| `Procfile` | one row per line, verbatim |
| `docker-compose.yml` / `compose.yml` | one row per service as `docker compose up <svc>`, plus one "all" row |
| `justfile` / `Makefile` | recipes/targets matching `dev\|serve\|watch\|run` |

Offered as a checklist in the Add service flow, pre-ticked for `dev`/`start`; imported rows
get `origin: 'suggested'`. An agent gets the same list through `createService`'s reply when
called with no arguments — so "set up this project's stack" is one round trip for it too.

## 9. Ports and readiness — agent-reported first

Port discovery was the hard part of every earlier version of this idea. It collapses once
agents are writers:

1. `ready_pattern` with a `port` group — observed, authoritative (`endpoint_source:
   'observed'`).
2. `updateService { port }` from the agent that read the output — reported.
3. The persisted `port` from last time, until the service is next `ready` — **stale**, and
   labelled as such in `listStack` so an agent does not curl a port from yesterday.
4. Socket scanning of the PTY's process tree — **not built**. It is the only approach that
   needs the subprocess-scan machinery, with its `spawn_blocking` and never-on-an-edge rules,
   for a case the first three cover.

## 10. The phone

Stack status and Restart from maiLink is an obvious §13 addition (`GET /stack`, WS
`stack`, `POST /stack/{service}/restart`), and a `protocolVersion` bump. Deferred, with one
thing decided now: the status vocabulary on the wire is the five words in §3, and
`endpoint_source` ships with the endpoint. The phone must never fall back to "go to the
desktop" — a crashed service it can see is a crashed service it can restart.

## 11. Build order

**v1 — a complete feature on existing plumbing**

1. [ ] Model: `Service` on `Workspace`, `Tab.service_id`; `set_workspace_stack` (coarse
       replace, like `set_workspace_mesh_topics`, normalizing `normalized_name` server-side);
       TS mirrors; `service_id: None` in `clone_workspace_with_id_mapping`; `cargo check --tests`.
2. [ ] Rust: `foreground_executable(pty_id)` — the ssh-filtered foreground-job query without
       the filter, unix + Windows, `spawn_blocking` (§4). This is the write guard.
3. [ ] Store: `stack.svelte.ts` — runtime status, start/stop/restart via PTY writes behind
       the guard, `activate-tab` to mount a background tab before the first write, OSC 133
       exit → status, bound-tab close → crashed, restart backoff + ceiling, `auto_start` on
       workspace activate/resume.
4. [ ] Sidebar section + rollup dot + shared context menu + edit modal + workspace submenu.
5. [ ] MCP: the nine tools, workspace-scoped, `stack_enabled` gate, write verbs on the
       inferred-identity refusal list; priming line rendered from state.
6. [ ] Suggester command + import flow.
7. [ ] `docs/stack.md` → "as built" table; CLAUDE.md data model + tools table.

**v2** — readiness as system trigger with port capture; env at spawn; Overlord
`service_crashed` + `driveTab` refusal + `kind: 'service'` in `listWorkspaces`; tab-strip
collapse toggle; SSH services (`ssh_command` becomes live — the spawn is the auto-resume
triple).

**v3** — `restart: on_change` (needs a watcher; `notify` crate), phone, export to a repo
file, socket-based port discovery if anything ever needs it.

## 12. Open questions

1. ~~Headless spawn~~ — resolved (§4): `activate-tab` mounts a hidden pane; restore already
   keeps background panes mounted.
2. ~~Does `getTabContext` serve a background tab?~~ — resolved: `getTerminalText` reads the
   Rust grid whenever the pane is registered (mount → destroy), and falls back to the SQLite
   scrollback snapshot otherwise. Given 1, a running service always reads the live grid.
3. **Shell integration on the service tab.** Exit detection is OSC 133; a shell where
   integration failed to inject reports nothing. Fallback: `PtyInfo` foreground polling on
   a slow tick (5s) for service tabs only — acceptable, since a crash is not sub-second work.
4. **Stack in a Mesh workspace.** Nothing special: the roster is agent tabs; a service tab
   is not one. Confirm `agentMesh` derives its roster from `runtime`/`mailink_native`, not
   from "every terminal tab".
5. **Two services, one port** (`web` and `storybook` both claiming 5173 across restarts).
   Last observed wins; `listStack` shows both with the same port and the human sorts it out.
   Not worth a conflict model.

## 13. Rejected

| Rejected | Why |
|---|---|
| A managed-process primitive (Solo's model) | Rebuilds scrollback, search, exit codes, activity, triggers, Overlord, restore, phone — all of which a tab already has. |
| `maiterm.yml` in the repo as source of truth | Workspaces have no root and span hosts; maiTerm owns state; the suggester buys the same first run. |
| `Service.tab_id` | Non-durable (reload mints a new id) — the tasks system carries `remapTab` calls on every lifecycle path for exactly this. Binding on the tab is derived and free. |
| Persisting status | A parked workspace would wake up "crashed". Status is rebuilt from the tab. |
| Respawning the PTY to restart | The shell survives the process; typing the command again is cheaper, visible, and leaves the human's shell state (venv, exports) intact. |
| Socket scanning for ports | Only needed when nothing else knows the port; with agents as writers, nothing else is the rare case. |
| Service tabs hidden from the tab strip in v1 | A new hide mechanism for a clutter problem that may not exist. Pinned cluster first; measure. |
| Human-only `removeService` | An agent may retract a service it created (`origin: 'agent'`), mirroring `dropped` for tasks; it may not remove a human's. |
