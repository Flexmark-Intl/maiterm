# maiTerm Stack — a workspace's services, known to every tab in it

> Status: **shipped in v2.4.0**, 2026-09-15. Built 09-11, run against a real project 09-13,
> service tabs moved out of the tab strip into the console drawer (§7) on 09-15. §11 has the
> commits, what each review round found, and what the live runs corrected. Owner: Darryl.
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
The store does publish its snapshot to Rust (`publish_stack_runtime` → `AppState.stack_runtime`,
in memory only) so the SessionStart priming can say what is up without a webview round trip
— the same reason the Overlord board is mirrored for the phone.

### Status

```
stopped   no service tab, or the tab's shell is at a prompt with exit 0 / SIGINT
starting  command sent, no ready match yet (and no exit)
running   process alive, nothing announced an address yet
ready     the output scan matched (a ready_pattern, or a built-in address shape), or an
          agent set ready over MCP
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
| ready for input | the shell's own OSC 133 **A** (`activityStore.onShellPrompt`, raw feed): a fresh shell has none until its rc finishes; a live one must have prompted since its last B/C | `term-osc133-${ptyId}` |
| the command began | the first **B/C** after our write (`onCommandBegin`) → `running`; only now does a D count as ours | same |
| exit detection | **D** after that B/C (`onCommandExit`, raw feed). The gated `onCommandComplete` hides exits inside the pane's 2s mount window and under its 2s completion floor — exactly where a service that fails at boot exits. And a fresh shell's first prompt emits an unconditional `D;0`, which is why a D before our B/C is ignored (second review, 2026-09-11) | same |
| no integration at all | `shell_integration` off, or no A within 12s of the start → tty-foreground fallback: write when no external job holds the tty, `running` when one takes it, exit when it leaves (`watchNoIntegration`, 2s tick), `crashed` "(no exit code)". A PTY reattached after a webview reload pays the 12s (its A predates the store) | foreground probe |

**`shell_at_prompt` is not "idle at a prompt".** It is `tpgid == pgid`: no *external* job owns
the tty. Builtins, rc files, functions and command substitutions never change it, so a shell
three seconds into `.zshrc` reads exactly like one waiting at its prompt. The first version
of this store typed on that signal and inferred crashes from it, and both were wrong in
production-shaped ways (typing into a shell still sourcing its rc; calling a healthy
service crashed because its rc took >2s). It is now used for two things only: capturing
the pid of a job we saw begin, and the no-integration fallback.
| shell death | `pty-close-${ptyId}` — the tab is deleted; store records `crashed` | existing event + listener |
| logs | `getTabContext` on the bound tab — reads the Rust grid while the pane is registered | existing tool |
| readiness | system trigger scoped to the tab via `Trigger.tabs` | trigger engine, per-tab scope exists |

The human can click into the tab, Ctrl-C it, poke at the process, and run the command
again by hand — the tab reads the OSC 133 result either way and the status follows. A
service tab is never a black box the way a managed process is.

**Never type into a foreground that isn't ours.** A start is typed only when the shell is
at its prompt; a stop signals only the **pid recorded at start**, and refuses when there is
no recorded pid or the foreground is a different one — never "whatever is in front". The
comms watcher learned this the hard way (`agent_owns_terminal`): a wrong "yes" types a
command into whatever the human left running there, or SIGTERMs their vim. A wrong "no"
costs a retry. `start` and `stop` also dedupe in flight: the status alone cannot guard
re-entry, because `start` does four IPC round trips before it can set `starting`, and a
double-click lands inside that window and mints a second tab. A second `start` gets the
**same promise** (so an auto-restart timer or a second click never evaporates); a `stop`
during a start asks it to abort — it bails before the write, or the stop waits for it to
settle and proceeds against the pid it recorded.

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
- **Move tab to another workspace** → `move_tab_to_workspace` clears `service_id` (the
  stack is per-workspace; the process goes with the tab, the binding does not), and
  `reconcileBindings` only looks inside the owning workspace, so the source shows it
  `crashed` rather than "running" with no tab. Tasks has the same open item (§8.4 there).
- **Reload of a service tab** → `service_id` rides along, but the new shell has nothing
  running in it. The store records the `ptyId` it typed into and `reconcileBindings` reads
  a bound tab whose live PTY differs as `stopped` ("its tab was reloaded — start it again").
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

Readiness detection was designed as a trigger: `ready_pattern` becoming a **system-owned
trigger** scoped to the service tab, which would have needed a `system: true` flag so these
never appeared in the trigger editor nor were swept into `hidden_default_triggers`.

**Built differently, and more simply.** The stack scans the output itself
(`stackStore.observeOutput`, §9) rather than renting the trigger engine. Two reasons: the
trigger engine early-returns when the user has no triggers configured, so the stack would
have depended on unrelated state being non-empty; and readiness wanted a built-in default
for services with no pattern at all, which is not a thing the trigger model can express.
The flag is unneeded and the trigger editor never learns the stack exists.

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
Click → show it in the console drawer, click again → hide. Right-click → Start/Restart ·
Stop · Show/Hide console · Edit… · Remove. `ContextMenu.svelte` already existed as a shared component
(`TerminalTabs.svelte` uses it); the sidebar simply had never used it.

**Rollup dot on the workspace row**, batch semantics like the Claude indicator: green when
every `auto_start` service is ready/running, amber (pulsing) while any is starting, amber
hollow when some are up and some never started (`partial`), red if any is crashed, none when
the stack is empty or fully stopped. `rollupStatus` in `stack/model.ts`, tested.

**Workspace row menu** (right-click the row — no `⋯` glyph, the row is crowded enough): Start
stack · Stop stack · Restart stack · Add service… · Import from project… (§8). This is how a
workspace with an empty stack gets its first service; the section itself only renders once
there is something in it (or the menu just asked for it).

**Tab strip — service tabs are not in it** (changed 2026-09-15; v1 shipped them as ordinary
tabs and that was wrong). A service tab is created in the background (`createTab({
background: true })` — Rust's `create_tab` makes a new tab active, so the previous active
tab is put back before the mirror sees it) and never appears in the strip: it takes no slot,
no `Cmd+1-9` number, and has no close button. The v1 behaviour cost a service its life the
first time a human clicked into one to read it and then closed the tab.

**The console drawer** (`components/stack/ServiceConsole.svelte`) is how a service is seen.
It is absolutely positioned inside `.main-content`, floating over the terminal area:

- **A drawer, not a dock.** A side dock is a flex sibling, so opening it changes the working
  terminal's WIDTH — and a width change makes Claude Code re-render its transcript into
  scrollback (root CLAUDE.md). The drawer changes neither width nor height of the tab
  underneath; verified in the app (198×50 before, during and after).
- **The terminal is borrowed, not moved.** The flat `TerminalPane` in `+page.svelte` portals
  its container into the drawer's `data-terminal-slot`, exactly as the mesh stage does.
  `+page` extends the `visible` prop for the drawer's tab the same way the `meshStage`
  branch already extends it. Showing or hiding a service therefore does nothing to its PTY.
  **The pane must not render a slot for a service tab** — the portal takes the first match in
  the DOM, and the pane's slot would win, leaving the drawer empty.
- Header: a pill per service (switch without closing), the last-reported endpoint, Start or
  Restart/Stop, and ×. Drag the top edge to resize; the height is `stack_console_height`.
- Dismissed by Escape, by the × , by clicking the same service again — or by clicking the
  work behind it (a capture-phase `pointerdown` inside `.main-content`, which does not
  swallow the click, so the terminal still takes focus). Clicks in the sidebar are exempt:
  they swap what the drawer shows. Escape typed **inside** the service's terminal belongs to
  that terminal — so the drawer must not hold focus, or it has no keyboard dismiss at all.
  The drawer does not focus anything, and `TerminalPane` does not focus a service tab when
  it becomes visible or when it mounts (it focuses only a terminal the human is looking at,
  which also stops a background session restore pulling the cursor out of your tab). Click
  into the console to type.

**The invariant that makes it safe: a service tab is never `pane.active_tab_id`.** With it,
every one of the ~40 `workspacesStore.activeTab` consumers is correct without knowing that
services exist. It is enforced in `setActiveTab` (refuses), `pickNextActiveTab` and Rust's
`pick_active_after_close` — used by `delete_tab`, `archive_tab` and all three `move_tab_*`
commands, because `active_tab_id` is persisted from Rust and a frontend-only guard would not
survive a restart — plus `reloadTab` (does not switch to a reloaded service tab) and
`load()` (heals a pane that already had one).

The one moment none of those cover is **binding**: `create_tab` makes every new tab active,
and a service's tab is an ordinary tab until `setTabServiceId` runs a beat later. So the
invariant is re-applied there, through `heal_pane_active_tab` — a Rust command rather than
`set_active_tab` because the answer may be **none** (a pane of only services legitimately
has no active tab, and `set_active_tab` can only name one). It runs on unbinding too, so a
`removeService` that hands a tab back to the strip can select it. Do not gate that call on
the mirror's `active_tab_id`: `createTab({ background })` deliberately leaves the mirror
alone while Rust moves on, so the very disagreement it repairs makes the gate false.

Two counts that used to be `pane.tabs.length` now have to mean "visible": the strip's own
grouping, new-tab naming, the archive/close/split guards, and `tabCycleList`. `reorderTabs`
persists the WHOLE pane, so the strip's drag has to splice the hidden tabs back into the id
list or the drag deletes them. `closeTabOrPane` is shared by the × , Cmd+W and `pty-close`:
a pane that still hosts services stays (showing its empty state) rather than handing their
PTYs to `deletePane`.

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

## 9. Ports and readiness — the service's own words first

Port discovery was the hard part of every earlier version of this idea. The answer is that
the service already announces the address, in its own output, every single time:

1. **maiTerm's own output scan** — `stackStore.observeOutput`, fed every PTY chunk from a
   service tab by `TerminalPane`. A human-set `ready_pattern` overrides the built-in shapes
   (its optional `port` group carries the port); otherwise `detectEndpoint` in
   `stack/model.ts` recognises what dev servers print. Either route means
   `endpoint_source: 'observed'` and `ready`.
2. `updateService { port }` from an agent — **reported**. No longer the only way a port
   arrives; it is now the correction path, and how a service that prints nothing gets one.
3. The persisted `port` from last time — **stale**, and labelled as such in `listStack` so
   an agent does not curl a port from yesterday.
4. Socket scanning of the PTY's process tree — **not built**. It is the only approach that
   needs the subprocess-scan machinery, with its `spawn_blocking` and never-on-an-edge rules,
   for a case the first three cover.

Three rules the scan obeys, all of them about staying cheap and staying honest:

- **Provenance belongs to a RUN, not a definition.** `ServiceRuntime.endpointFrom` is
  cleared at every start, which is what makes a carried-over port read as stale. Deriving
  it from `ready_pattern` instead — as `endpointSource` did until this landed — made
  `'observed'` unreachable while still advertising it to agents.
- **The scan stops.** At the first answer of a run, and in any case `SCAN_WINDOW_MS` (5 min)
  after the start: a server announces itself while it is starting, and a queue worker that
  prints all day and serves nothing should not be matched against forever.
- **The host must be loopback.** A start-up banner routinely also carries a docs link and a
  LAN address; only the loopback one is what this service is serving. `0.0.0.0` and `[::]`
  are rewritten to `localhost` so the stored URL is one a browser can follow — which is what
  makes the sidebar's launch affordance (shift-click a row, or its `↗`) possible at all.

## 10. The phone

Stack status and Restart from maiLink is an obvious §13 addition (`GET /stack`, WS
`stack`, `POST /stack/{service}/restart`), and a `protocolVersion` bump. Deferred, with one
thing decided now: the status vocabulary on the wire is the five words in §3, and
`endpoint_source` ships with the endpoint. The phone must never fall back to "go to the
desktop" — a crashed service it can see is a crashed service it can restart.

## 11. As built (v1, 2026-09-11)

| Stage | Commits |
|---|---|
| Model: `Service` on `Workspace`, `Tab.service_id`, `set_workspace_stack` (coarse replace, normalizes server-side), `set_tab_service_id` (clears the same service from any other tab in the same write), `service_id: None` in `clone_workspace_with_id_mapping`, TS mirrors | `2e78c3b` |
| The write guard: `get_pty_foreground_job` → `PtyForeground { shell_at_prompt, executable, command, pid }` (unix: tty foreground pgid, same query as the ssh probe minus the filter; Windows: deepest first-child chain, approximate); `kill_pty_foreground_job` signals only if `pid` is still the foreground leader | `14d2a4c`, `fc44e63` |
| Store `stack.svelte.ts` + pure `stack/model.ts` (tests): start/stop/restart, OSC 133 exit → status via `activityStore.onCommandComplete`, bound-tab-gone → crashed, backoff + ceiling, auto-start on activation, `createTab({ background })`, runtime publish to Rust | `fc44e63` |
| Sidebar section, rollup dot, workspace row menu, service modal | `656892b` |
| Eleven MCP tools (frontend-handled, workspace-scoped, `stack_enabled` gate, write verbs on `PEER_ADDRESSING_TOOLS`), live priming line from the Rust mirror | `1e60fab` |
| Suggester (`commands/stack.rs`, tests) + import checklist + `createService` with no args | `c4401df` |
| Review fixes (seven defects): unfiltered `onCommandExit` + post-start settle for fast exits; in-flight guard on start/stop; move clears the binding and reconciliation is workspace-scoped; a stop that gives up keeps `stopping`; stop refuses without a recorded pid; `waitForService` capped at 100s under the 120s MCP response timeout; rollup is batch-true with `partial`; reload of a service tab reads as stopped | `982711f` |
| Second review (five defects in the fix): start/exit rebuilt on the raw OSC 133 A/B-C/D sequence instead of the tty foreground (first-prompt `D;0` was filing every fresh start as stopped; rcs >2s read as crashes); in-flight starts are shared promises and stops abort/await them; a suspended service tab reads as stopped | `d586596` |
| Third review (three defects, one cause): shell facts were keyed by TAB id, but a tab id outlives its shell — a respawned shell inherited the dead one's "prompted" fact and was typed into mid-rc; a reload's new id had no facts and fell into the no-integration path. Facts are now keyed by PTY id (the raw feed carries it), a B/C or D from a different PTY is never ours, and the B/C wait is 30s so a slow prompt hook cannot orphan a queued command | `b5740f1` |
| Fourth review: a start resets every per-run field (a stale `ptyId` had let `reconcileBindings` void a start mid-flight — the "kept ptyId" from the third round was inert and harmful, reverted); reconciliation skips in-flight starts; the first-prompt wait is skipped when integration is off and abortable by Stop | `c1b213e` |
| **Runtime verification** (2026-09-13, `tauri:dev`, `maiSoft/website` — pnpm + vite): import from `package.json` (pm from the lockfile, `dev` pre-ticked), `startService` in under a second with the command typed after the shell's A, Ctrl-C → exit 130 → stopped, `kill -TERM` → 143 → crashed → restarted 1s later, `stopService`/`stopStack` via ^C, `waitForService`, `updateService { port, ready }` → `ready` and `endpoint_source: stale` after a stop, `removeService` refused while running, tab reload → binding carried to the new id and `stopped`, app relaunch → binding restored and auto-start on the active workspace, suspend → all stopped, resume → auto-start, a `sh -c 'sleep 1; exit 1'` service → five backed-off restarts then the ceiling note, sidebar rollup red over a crashed service, Start/Stop stack from the row menu. Four defects fixed on the way: a `~/…` cwd was never expanded (the scan of `~/DATA/IDE` found nothing; a saved one would have been typed as `cd '~/…'`); the reload note read as suspended (reconciled before the fresh pane mounted) and a suspend read as reloaded (PTYs die before the flag flips); the priming advertised a stale URL on a stopped service; and a stop on a crashed service left its backoff timer armed, so "Stop" was followed by a restart | `dd028cb`, `5b89221`, `25dfe49`, `2a8f703` |
| Review of those fixes (three findings): the `~` expansion lived in Rust only, so the mirror the store types from still said `cd '~/…'` — `set_workspace_stack` returns the stored rows and the store (and `createService`/`updateService`) carry them; a suspend of a single TAB unregisters before it stamps `suspended_at`, the same ordering as the workspace case, so the effect tracks the stamp and corrects the note; a cancelled auto-restart is un-booked from `restarts` so it costs no slot of the ceiling. Each verified live | `0a2261b` |
| Review of that: assigning the returned list to the mirror rolled back any stack write issued while the IPC was in flight, and the next writer persisted the rollback (a concurrent `createService` vanished from memory and disk). The store now merges only `cwd`/`normalized_name` by id onto whatever the mirror holds when the reply lands — the mirror only ever advances | `7bd6722` |
| **Service tabs leave the tab strip** (2026-09-15): closing one killed its service, because a service was a tab like any other. Now it is not in the strip at all — it opens in the console drawer (§7), which borrows its terminal through the portal. Plus the invariant (`active_tab_id` is never a service tab, in Rust too), the visible-tab counts, the shared `closeTabOrPane`, and the paths that used to walk into a service tab by accident: Quick Open's `cd` target, Suspend Other Tabs, archive, the workspace row's count/activity/live dots, task assignees, and `switchTab` | `6a27160`, `6af617b`, `ba99464` |
| Review of the drawer (five findings): the three `move_tab_*` commands still picked a replacement active tab the old way, so moving your last ordinary tab out of a pane selected the service behind it and Cmd+W then took its PTY — the exact loss the change exists to prevent; a pane of only services has NO active tab, which made `createTab({background})` reachable with nothing to restore (`heal_pane_active_tab` applies the invariant in Rust and can answer "none", which `set_active_tab` cannot — `load()`'s self-heal goes through it too, so the null case persists); `viewConsole` dispatched `activate-tab` with an object where the listener reads a bare id, so an unmounted service tab opened a blank drawer forever; `navigateToTab` on a service tab did nothing (a toast click switched workspace and stopped) and now opens the console; Escape inside the service's terminal is the terminal's byte, and the drawer no longer takes focus on open | `0d0e8c2` |
| Review of those (two findings, both ordering, both verified in the app rather than by reading): the heal ran inside `createTab`, before `setTabServiceId`, so Rust saw an ordinary tab and did nothing — it now runs on bind and unbind, ungated by the mirror; and the drawer still took the keyboard, because `TerminalPane` focuses at the end of mount and on every false→true `visible`, not only where the removed call was. A terminal now takes focus only when it is one the human is looking at | `58d4280` |
| **Shipped in v2.4.0** (2026-09-15). Fact-checking the docs site against the source first caught two promises the code does not keep: `restartService` claimed a 10s wait for the service to come back (it replies as soon as the command is running), and `waitForService` offered to block until "ready" for a service with a `ready_pattern`, which nothing evaluates — so it would have waited out the full timeout. Both descriptions and the service form now say what the code does | `a8c556c`, `d50db98` |

| **Ports read from the service's own output** (2026-09-15): `updateService` was the ONLY way a port ever reached the sidebar, so a human running the stack without agents never saw one — and `endpoint_source: 'observed'` was advertised to agents while being unreachable, since it was derived from `ready_pattern` and nothing evaluated that. `observeOutput` now scans a service tab's chunks for the address it announces, `ready_pattern` overrides the built-in shapes where one is set, and `endpointFrom` records the real provenance per RUN so a carried-over port reads as stale. Plus the launch affordance the URL makes possible: shift-click a sidebar row, its `↗`, or the context menu | *this change* |

Where the build departed from the plan above it, the plan was wrong: the guard became a
struct rather than a bare executable name because the **pid** is the thing the stop path
needs (the executable of `npm run dev` fronts as `npm`, `node` or `sh`); `startLine` uses
`env K=V cmd` rather than `K=V cmd` so fish works; and the "nine" tools are eleven —
`startStack`/`stopStack` earned their own names rather than a `service: "*"` convention.

**v2** — ~~readiness with port capture~~ **done** (2026-09-15, §9): built as the stack's own
output scan rather than a system trigger, with a built-in default so a service needs no
pattern at all, and `ready_pattern` overriding it where one is set. The wording held back
at v2.4.0 is restored in step — the service form, the `waitForService` row, the `listStack`
`endpoint_source` enum and the session priming all describe the scan now. Still open: env
at spawn; Overlord `service_crashed` + `driveTab` refusal + `kind:
'service'` in `listWorkspaces`; pinned cluster / glyph / collapse for service tabs; SSH
services (`ssh_command` becomes live — the spawn is the auto-resume triple).

**v3** — `restart: on_change` (needs a watcher; `notify` crate), phone, export to a repo
file, socket-based port discovery if anything ever needs it.

## 12. Open questions

1. ~~Headless spawn~~ — resolved (§4): `activate-tab` mounts a hidden pane; restore already
   keeps background panes mounted.
2. ~~Does `getTabContext` serve a background tab?~~ — resolved: `getTerminalText` reads the
   Rust grid whenever the pane is registered (mount → destroy), and falls back to the SQLite
   scrollback snapshot otherwise. Given 1, a running service always reads the live grid.
3. **Windows foreground is an approximation.** No tty process groups there; `foreground_job`
   walks the deepest first-child chain under the shell and reads "no children" as "at the
   prompt". Good enough to refuse typing over a running program; not good enough to tell two
   background jobs apart. Nobody runs the stack on Windows yet.
4. **Auto-start fires on activation, not at boot.** A background workspace's services start
   the first time it becomes active this session (`autoStarted` set, cleared by suspend).
   Starting every workspace's stack at launch is a preference waiting for someone to want it.
5. ~~Shell integration on the service tab~~ — built: no A within 12s of the start (or the
   preference off) → the tty-foreground fallback, with `watchNoIntegration` polling every 2s
   for the job leaving the tty. Coarser than OSC 133 and blind to the exit code, by design.
6. **Stack in a Mesh workspace.** Nothing special: the roster is agent tabs; a service tab
   is not one. Confirm `agentMesh` derives its roster from `runtime`/`mailink_native`, not
   from "every terminal tab".
7. **Two services, one port** (`web` and `storybook` both claiming 5173 across restarts).
   Last observed wins; `listStack` shows both with the same port and the human sorts it out.
   Not worth a conflict model.
8. **A fresh store over a shell that is already running the service.** Seen in the
   2026-09-13 run under dev HMR: the webview reloads, the PTY and its vite survive, the new
   store's auto-start waits 12s for an A that was emitted before it existed, falls to the tty
   probe, and files the service `stopped — the shell is busy` while the server is up. Only a
   webview reload reaches this (an app relaunch kills the PTYs; a WebContent crash is the
   production analogue, and the terminals are blank then anyway). Adoption of a running job
   stays rejected (§13); the human presses Ctrl-C and starts it, or it stays a plain tab.
9. ~~Removing a service leaves its tab~~ — still true, and now it is the feature: the tab
   was hidden while it was a service, and `removeService` clears `service_id`, so it simply
   reappears in the strip as an ordinary terminal holding its output.
10. **A pane can hold nothing but services.** Legal by design (`closeTabOrPane` keeps it, so
    `deletePane` never takes a running service's PTY with it), and it renders as the normal
    empty pane. Nothing in the empty state says "3 services are running in here" yet.

## 13. Rejected

| Rejected | Why |
|---|---|
| A managed-process primitive (Solo's model) | Rebuilds scrollback, search, exit codes, activity, triggers, Overlord, restore, phone — all of which a tab already has. |
| `maiterm.yml` in the repo as source of truth | Workspaces have no root and span hosts; maiTerm owns state; the suggester buys the same first run. |
| `Service.tab_id` | Non-durable (reload mints a new id) — the tasks system carries `remapTab` calls on every lifecycle path for exactly this. Binding on the tab is derived and free. |
| Persisting status | A parked workspace would wake up "crashed". Status is rebuilt from the tab. |
| Respawning the PTY to restart | The shell survives the process; typing the command again is cheaper, visible, and leaves the human's shell state (venv, exports) intact. |
| Socket scanning for ports | Only needed when nothing else knows the port; with agents as writers, nothing else is the rare case. |
| ~~Service tabs hidden from the tab strip in v1~~ | **Overturned 2026-09-15 (§7).** The reasoning was wrong about the problem: it was never clutter. A service in the strip has a close button, and the first thing Darryl did was click into one to read it and then close the tab, which killed the service. The hide mechanism turned out to be one filter plus one invariant, not a new subsystem. |
| Human-only `removeService` | An agent may retract a service it created (`origin: 'agent'`), mirroring `dropped` for tasks; it may not remove a human's. |
