# Hive: a coordinator that spawns agents on a goal

> Status: DESIGN, 2026-10-23. Nothing built. Research behind it: `docs/claude-projects-research.md`
> (what Anthropic's cloud Projects do). Hive is the local version: the same coordinator,
> workers and shared memory, running in maiTerm on this machine.

## 1. What it is

A **Hive** is a workspace whose terminals you never see. You turn an empty workspace into a
Hive the way you turn one into a mesh. maiTerm then starts a **coordinator** agent and draws a
dedicated view in place of the split tree:

- **Left:** the coordinator's chat, one session rendered the way Loom Focus renders a chat.
  You give it a goal and talk to it here.
- **Middle:** the **board**. Every worker the coordinator spawned is a card: the task it owns,
  who it is, what it is doing right now, and whether it needs you.
- **Right:** **worker columns.** Click a card and that worker's chat opens as a column beside
  the board. You can read it, answer its prompt, or steer it directly.

Each worker runs in **its own git worktree on its own branch**: its own copy of the code. Every
agent in the Hive reads and writes one **shared memory**.

```
┌ sidebar ┬─ Hive · Auth rewrite ───────────────────────────────────────────────────────────┐
│         │ COORDINATOR            │ BOARD                     │ WORKER · Session store     │
│         │ claude · opus · 41% ▮  │ ── wave 1 ──  ── wave 2 ──│ claude · sonnet · 18% ▮    │
│         │                        │ ┌──────────┐  ┌─────────┐ │                            │
│         │ You: rewrite auth so…  │ │● Session │──│○ Login  │ │ Reading src/auth/store.ts  │
│         │                        │ │  store   │  │  flow   │ │ ▸ 4 tool steps             │
│         │ Spawned 3 workers:     │ │ Editing  │  │ waits on│ │ I've split the store into… │
│         │ ┌ Session store  ● ┐   │ │ store.ts │  │ Session │ │                            │
│         │ ┌ Token refresh  ◐ ┐   │ └──────────┘  └─────────┘ │ ┌ Permission: Bash ───────┐│
│         │ ┌ Test harness   ✓ ┐   │ ┌──────────┐              │ │ npm test  [Yes] [No]    ││
│         │                        │ │◐ Token   │ ⚠ needs you  │ └─────────────────────────┘│
│         │ [ message the Hive… ]  │ │  refresh │              │ [ message this worker… ]   │
└─────────┴────────────────────────┴──────────────────────────┴────────────────────────────┘
```

## 2. Principles (the decisions everything else follows from)

1. **A Hive is a workspace mode, not a new window or app.** It goes on `Workspace` like
   `bridge_all`, and gets its own branch in `+page.svelte` like `MeshStageView`.
2. **Every Hive agent is an ordinary terminal tab, hidden.** The coordinator and each worker
   are real PTYs running a real agent CLI, with everything maiTerm already does for an agent
   tab: hooks, tab identity, transcripts, prompts, the phone, accounts. They're hidden the way
   stack services are (`docs/stack.md` §7). **Nothing in the Hive is a new kind of agent.**
3. **Columns render the transcript, never the terminal.** A column is Loom Focus's chat for one
   tab. Resizing or opening a column therefore never resizes a PTY, which matters because a
   width change on a streaming Claude duplicates its scrollback (root CLAUDE.md). The PTYs sit
   at one fixed size for their whole lives.
4. **Closing a column never stops an agent.** You stop an agent through an explicit Stop.
   Cmd+W in a Hive closes the focused column.
5. **The human's act of creating the Hive is the approval envelope.** Inside a cap the human
   set, the coordinator can spawn workers without a card each. It can't exceed the cap, it
   can't create Hives, and workers can't spawn workers. Rust enforces all of this by tab
   identity (§7).
6. **Workers never land their own work.** A worker reports. The coordinator asks to land. The
   human approves the merge (§8), the same rule Anthropic's auto mode enforces ("merging a pull
   request no human has approved").
7. **Reuse before invent.** The cards are maiTerm Tasks. Prompts go through `PromptCard`, blockers
   through `BlockerCard`, delivery through `deliver_to_agent`, and memory is Claude's own auto
   memory pointed at a shared directory.

## 3. Turning a workspace into a Hive

Creating a Hive is a sheet, opened from the workspace row's menu ("Make this a Hive…"). It's
offered only when the workspace has **no tabs** (or only one fresh, unused shell, which the Hive
replaces):

| Field | Meaning |
|---|---|
| Name | The workspace name; also the workstream name on the task board |
| Repository | A local git repo (default: the folder of the tab you came from) |
| Base | The branch the Hive builds on (default: the repo's current branch) |
| Goal | One line. Shown on the board header, and sent to the coordinator first |
| Instructions | Standing rules, the brief every agent in the Hive starts from (Projects' 16k-char instructions) |
| Workers at once | **Hard cap**, enforced in Rust (default 4) |
| Setup command | Run in every new worktree before its agent starts (e.g. `npm ci`) |
| Models | Coordinator and worker runtime/model/effort (default: Claude, coordinator lower effort, as Projects does) |

On **Create**, maiTerm:

1. Makes the **integration branch** `hive/<slug>` off Base, and its worktree (§6). The coordinator
   lives there.
2. Creates the shared memory directory and seeds `MEMORY.md` with the goal (§9).
3. Creates the coordinator tab hidden, launched in the integration worktree with the Hive's
   settings, and sends it the Hive brief: role, goal, instructions, tools, rules.
4. Switches the workspace's view to the Hive.

A Hive can be turned back into a normal workspace only once it has no live workers. Its tabs
then appear in the strip as ordinary tabs, and nothing is deleted.

## 4. The view

### 4.1 Layout: Miller columns

Columns run left to right, each one resizable by its border (the widths are kept per viewer
in `localStorage`, as the Loom does):

| Column | Width | Notes |
|---|---|---|
| Coordinator | 380–560 px | Always present; can be collapsed to a 44 px strip with an unread dot |
| Board | flexible, min 260 px | Shrinks to a **rail** (compact rows) when a worker column opens |
| Worker columns | min 420 px each | **At most two** side by side. Opening a third replaces the least recently focused unpinned one. A pin keeps a column open |

The board turns into a rail rather than disappearing, because the human keeps needing to jump
between workers. The rail is the same cards in one-line form (state dot, title, activity
line), sorted *needs you → working → reported → the rest*.

This is not an overlay. The Loom left the terminal area because a heavy drawer fought a live
terminal for paint and keyboard (`docs/loom.md`). A Hive has no visible terminal, so its
columns are the whole main area, and nothing fights.

### 4.2 The chat column: one component, three places

`FocusView.svelte` today is one 1138-line component holding both the chat list and the open
chat. **The first step of this feature is to extract the open chat as `ChatColumn.svelte`**
(props: a tab id). It carries everything below the list: the header (model, effort, context
gauge), the transcript folded by `chatRows`, tool steps, task events, the prompt card, the task
question, attachments, the composer with its Sending/Queued/Delivered bubbles, draft refusal
and every guard. Then:

- Loom Focus = the list + `ChatColumn`, with no behaviour change. This ships first, alone, so a
  regression shows up where people already look.
- Hive coordinator column = `ChatColumn` + a Hive header (goal, worker count, Pause).
- Hive worker column = `ChatColumn` + a worker header (task, branch, diff stat, Land/Stop menu).

Each open column polls its own transcript every 3 s, as Focus does. With the coordinator and
two workers open, that's three polls. **The board never polls transcripts** (§4.4).

Additions specific to Hive inside the coordinator column:

- **Spawn cards inline.** When the coordinator calls `hiveSpawnWorker`, that tool step renders
  as a small worker card (title, state dot) rather than a quiet tool line, as Projects shows a
  card under your message. Clicking it opens the worker's column.
- **Reports fold.** A worker's report arrives in the coordinator's transcript as a framed
  message (`⟦HIVE REPORT · Session store⟧ …`). It folds to one line with the report shown on
  click, the way subagent reports fold in Focus today (`injectedTurn`).

### 4.3 Cards

A card is **one worker plus the task it owns**. The task is the identity, so a worker that's
replaced (crashed, restarted) keeps its card.

```
┌─────────────────────────────────────┐
│ ● Session store                 4m  │   state dot · task title (wraps, never clipped) · age
│   Editing src/auth/store.ts         │   live activity line (§4.4)
│   "Split read/write paths; tests    │   latest task note: the context update
│    next"                            │
│   hive/auth/session-store  +212 −40 │   branch · diff vs integration branch
│   claude · sonnet         ctx 18% ▮ │   runtime · model · context gauge
└─────────────────────────────────────┘
```

| State | Dot | From |
|---|---|---|
| Starting | grey pulse | Worktree, setup or agent launch in progress |
| Working | green moving | Agent busy (hook state) |
| Needs you | orange; red for an `action` | Permission or question on screen, a task blocker, a watch script waiting |
| Idle | hollow | Turn ended without a report. Worth a look, never an alarm |
| Reported | blue check | The worker called `hiveReport` with `done` |
| Landing | blue pulse | A land request is waiting for the human, or merging |
| Landed | solid check, dimmed | Merged into the integration branch |
| Failed / Stopped | red / grey | Reported `failed`, crashed, or stopped by the human |

A Needs-you card carries the waiting count and an orange edge. Clicking it opens the worker
column scrolled to the prompt card. The answer happens **in the column**, through the same
`PromptCard`/`BlockerCard` and the same 1.5 s guards, never on the card itself. Cards are small
and get re-laid-out often, so a button on a card is exactly where a stray click lands.

### 4.4 The board layout and the live activity line

**Layout: waves.** Cards sit in columns by dependency depth: wave 1 depends on nothing,
wave 2 on wave 1, and so on. The Loom Weave's SVG string layer (`docs/loom.md` "The weave")
draws dependency strings between cards, with the same vocabulary: dots moving along a string
mean the upstream task is active, an amber dash means the card is waiting on it. A Weave of
agent→task strings would show nothing here, since every worker owns exactly one task.
Dependencies are the relationship worth drawing, and the waves show the plan and its progress
in one picture. In rail mode the waves flatten into the sorted list.

**The activity line comes from hooks, not transcripts.** `PreToolUse`/`PostToolUse`/`Stop`
already reach `claude_code/server.rs` (HTTP hooks, `HookPhase::ToolPre`/`ToolPost`). A small
Rust map keeps `AgentActivity { tab_id, phase, tool, target, since }` per tab, where the target
is the file for Edit/Write/Read, the command's first line for Bash, or the pattern for Grep. It
pushes a throttled `hive-activity` event (at most 2 a second per tab, **sent to that window
only**: `win.emit` broadcasts to every window, see the common-pitfalls memory). The card
renders it with the phone's verbs (`toolVerb`): "Editing store.ts", "Running npm test",
"Searching for refreshToken". Between turns it reads "Idle · 2m".

**Diff stat** is `git diff --shortstat <integration>...HEAD` in the worker's worktree,
recomputed by Rust after a `PostToolUse` on Edit/Write/Bash, debounced 5 s per worker. That
gives real numbers without a file watcher per worktree.

### 4.5 Raw terminal, the escape hatch

Some things only the terminal shows: a dialog the chat doesn't parse, a crashed CLI, a setup
command's output. Each column's menu has **Show terminal**. It portals that tab's `TerminalPane`
into a slot in the column (`data-terminal-slot`, exactly as the stack drawer borrows a service
terminal) **at the PTY's existing size, scrolling horizontally if the column is narrower**, never
refitting it, per principle 3. The pane must not render a slot of its own for a hidden tab. The
portal takes the first match, which is the bug the stack drawer already hit.

### 4.6 Keyboard

In a Hive workspace there's no tab strip, so the tab keys are free:

| Key | Action |
|---|---|
| Cmd+1 | Focus the coordinator's composer |
| Cmd+2…9 | Open/focus the worker column for the Nth card in board order |
| Cmd+Shift+[ / ] | Move focus between columns |
| Cmd+W | Close the focused worker column (never the agent; needs two presses like tabs) |
| Escape | In a composer, blur it. In a column, close it. **Inside a raw terminal, Escape belongs to the terminal** (it interrupts the agent), so the column doesn't take it |

## 5. Hidden tabs: one predicate

Stack services established the rule that a hidden tab is **never `pane.active_tab_id`**. That
keeps the ~40 `activeTab` consumers (Cmd+W, Cmd+D, the composer dock, find…) from acting on a
tab nobody can see. Hive tabs take the same invariant. Rather than adding a second flag check
to every guard, the guards move to one predicate:

```ts
isHiddenTab(tab) = !!tab.service_id || !!tab.hive_role
```

with its Rust twin used by `pick_active_after_close`, `heal_pane_active_tab` and the rest. In a
Hive workspace the pane has **no active tab**, which `heal_pane_active_tab` already allows ("a
pane of only services legitimately has no active tab").

`Tab.hive_role` is a **string** (`"coordinator"` | `"worker"`), never a new serde enum variant.
An older build that meets an unknown variant fails to parse the whole state file and starts
empty (the serde-enum-downgrade memory, and `docs/loom.md` "Blocked with no reason recorded").

## 6. Worktrees: each worker's copy of the code

### 6.1 Where and how

- **maiTerm creates the worktrees**, with `git worktree add`, rather than Claude's `--worktree`.
  It's runtime-agnostic (Codex workers later), the paths and branch names are ours, and so is
  cleanup. Claude's `--worktree` also can't branch off an arbitrary branch (`worktree.baseRef`
  is only `fresh` or `head`).
- **Outside the repo**: `<app data>/hive/<hive-id>/wt/<worker-slug>/`, using
  `app_data_slug()`, never a hard-coded `com.aiterm.app`. Nested inside the repo, every watcher
  would scan it: Vite, tsc, cargo, eslint, the Files column, Claude's own Grep in the main
  checkout.
- **Branches**: integration `hive/<hive-slug>`; workers `hive/<hive-slug>/<worker-slug>`, branched
  off the integration branch's current tip. A wave-2 worker therefore starts with wave 1's
  landed work.
- **Setup**: the Hive's setup command runs in the new worktree, in the worker's own tab before
  its agent starts, so its output is in the raw terminal if it fails. Gitignored files the work
  needs (`.env`) are copied in from a `.worktreeinclude`-style list in the Hive settings, with
  Claude's semantics: only files that are gitignored, never tracked ones.

### 6.2 Build caches are the hidden cost

A fresh worktree has no `node_modules` and no `target/`. For a Rust repo like this one, each
worker would rebuild from scratch for minutes. Sharing one `CARGO_TARGET_DIR` serializes every
build on cargo's lock. The setup command is the place for this, plus two defaults the sheet
offers when it detects them: a per-worker `CARGO_TARGET_DIR` seeded by copying the main
checkout's `target/` with `cp -c` (APFS clone: instant, no space until written), and `npm ci`
with the shared npm cache. This is the biggest practical risk to "spawn 6 workers" feeling fast.
Measure it in the spike (§12).

### 6.3 Trust

A worktree is its own git root, and Claude's trust walk stops at the git root (the
trust-dialog memory). **Every new worktree would raise "Do you trust this folder?"**, and
nothing automated may confirm that dialog (`trust_dialog_open`, root CLAUDE.md). The human
already chose this repository by creating the Hive. So **maiTerm itself** (not an agent, which
the permission classifier rightly refuses as "Security Weaken") writes
`hasTrustDialogAccepted` for each worktree it creates. It writes it into the `.claude.json` of
the account the worker spawns under (`accounts/`, per-account roots), and removes the entry when
the worktree is removed. Spike item: confirm it suppresses the dialog under a managed account.

### 6.4 Isolation is a convention, not a wall

A worker launched in a `git worktree add` directory doesn't get Claude's worktree isolation
checks (those apply to `--worktree`/`EnterWorktree` sessions). Nothing stops a worker that
`cd`s into the main checkout. The brief says not to, and the coordinator's tools only ever name
the worktree. A later option is a PreToolUse guard keyed on the tab's worktree, since maiTerm
already receives every PreToolUse. It's not v1.

## 7. Agents, tools and authority

### 7.1 The coordinator

A normal agent tab with `hive_role: "coordinator"`. Its brief (sent once at launch, and again
after compaction through the SessionStart hook's additional context, as tab identity is today):

- It's the coordinator of Hive *X* with goal *G*. It plans, spawns, reviews and lands. **It
  doesn't do the work itself**, beyond small reads to plan.
- Its tools, below. The cap. That memory is shared and read at start, so a decision that
  running workers need also goes to them by message.
- It doesn't merge; it asks to land.

### 7.2 MCP tools

All are refused unless the calling tab's `hive_role` and Hive match. That's enforced in Rust
from the stated tab identity, like `own_tab_only_refusal`.

| Tool | Who | Does |
|---|---|---|
| `hiveSpawnWorker {title, brief, depends_on?, model?, effort?}` | coordinator | Creates the task, worktree, branch and hidden tab, runs setup, launches the agent with the Hive preamble + brief. Refused above the cap. A worker with unmet `depends_on` is created in **Starting · waits on …** and launched when they land |
| `hiveListWorkers` | coordinator | Cards as data: state, task, branch, diff stat, last report |
| `hiveMessageWorker {worker, text}` | coordinator | Through `deliver_to_agent`: the draft, dialog and typing guards; a busy agent is no reason to hold |
| `hiveRequestLand {worker, summary}` | coordinator | Files a land request (§8) |
| `hiveStopWorker {worker, reason}` | coordinator | Stops the agent; keeps worktree and branch |
| `hiveReport {status: done\|blocked\|failed, summary, notes?}` | worker | Records the report on the task (status + note), and delivers it to the coordinator as a framed `⟦HIVE REPORT⟧` message |

Workers keep every ordinary maiTerm tool (tasks, notes, follow-ups) and their runtime's own
subagents and workflows. What they can't do is spawn Hive workers or message other workers.
Projects doesn't let threads talk to each other either, and a star topology keeps the
coordinator the one place the plan lives. If peer talk turns out to be needed, the mesh already
exists.

### 7.3 Authority

| Action | Approval |
|---|---|
| Create a Hive | The human, by the sheet. No tool can |
| Spawn a worker within the cap | None; the Hive is the envelope (decision §13.1) |
| Raise the cap | The human, in Hive settings |
| Land a worker's branch | The human, on a land card (decision §13.2) |
| Merge the Hive into Base | The human, outside the Hive (a normal merge or MR) |
| A worker's permission prompt | The human, in the worker's column, or the phone |

The window's Overlord engine leaves Hive tabs alone. A Hive workspace is `overlord_exempt` by
default, because the coordinator already supervises its workers, and two supervisors driving one
agent is the double-drive problem `docs/overlord.md` §9.1.2 solved for escalations.

## 8. Landing

The local stand-in for Projects' pull requests:

1. A worker reports `done`. Its card goes to **Reported**.
2. The coordinator reviews: it reads the report, can read the worker's diff
   (`git diff hive/<hive>...hive/<hive>/<worker>`), and can send the worker back with
   `hiveMessageWorker`.
3. When satisfied, the coordinator calls `hiveRequestLand`. The card goes to **Landing**, and an
   **Agent request** card ("Land *Session store* into *hive/auth*?", with the diff stat and the
   coordinator's summary quoted as its reason) appears in the Loom's Decisions, in the worker's
   column, and on the phone. It's the existing agent-requests machinery (`docs/agent-requests.md`):
   only a human click or a paired phone approves.
4. On approval, maiTerm merges `--no-ff` **in the integration worktree**, which is the
   coordinator's own folder, so the human's main checkout is never touched. A conflict aborts the
   merge, and the card says so. The coordinator then tells the worker to rebase onto the
   integration branch and report again.
5. After landing, the worker is retired: the agent exits, and the worktree is removed **only if
   it's clean**. The branch is kept.

At the end, the integration branch `hive/<slug>` holds the whole result. The human merges it into
Base or opens an MR from it, outside the Hive.

## 9. Shared memory

**Mechanism: Claude's own auto memory, pointed at one directory.** Every Hive agent launches with
`--settings '{"autoMemoryDirectory": "<app data>/hive/<id>/memory"}'`, an absolute path (the only
kind Claude honours, per the auto-memory memory). That gives, natively:

- Each agent reads `MEMORY.md` at start and opens individual files when it needs them, which is
  exactly Projects' progressive load.
- Each agent writes memory files with the runtime's own memory behaviour, and a configured
  directory **gets write permission without prompts**. A symlinked one doesn't, which is why
  it's a configured path and not a symlink.

What maiTerm adds:

- **Index rebuild.** Several agents editing one `MEMORY.md` at once lose updates. maiTerm
  watches the directory and **regenerates `MEMORY.md` from the files' frontmatter**
  (name, description, a link), and the brief tells agents to write files and leave the index to
  maiTerm. That's the same shape as this user's memory index, kept consistent by construction.
- **Mid-task visibility is by message.** Agents read memory at start. A decision that running
  workers need right away goes from the coordinator to them with `hiveMessageWorker`. The brief
  says so, and nothing pretends running agents see memory live.
- **A Memory tab** in the Hive header lists the files, and lets the human read, edit and delete
  them. Projects started without this and was criticised for it.
- **A Library folder** (`<app data>/hive/<id>/library/`) for non-code outputs (reports,
  findings) that workers write and the human opens from the Hive header. Memory holds notes; the
  Library holds artifacts.

Repo rules stay in the repo's `CLAUDE.md`, which every worktree has because it's a checkout.
Hive memory holds facts about this Hive's work.

## 10. Lifecycle

| Event | What happens |
|---|---|
| Suspend the workspace | Every Hive tab suspends (PTYs killed); worktrees stay. Cards show Suspended |
| Resume | Each agent relaunches with its runtime's resume command **in its worktree**; the coordinator gets a Hive status digest |
| maiTerm restart | Same as resume, through session restore |
| Worker crash | Card → Failed; coordinator is told by a framed message; it may respawn (same task, new tab) |
| Reload a Hive tab | The existing reload path (new tab id, `remapTab` for tasks); the card is keyed by task, so it doesn't move |
| Pause (Hive header) | Stops new spawns and holds deliveries; running turns finish |
| Delete the Hive | A sheet lists every branch not landed and every dirty worktree. Clean worktrees are removed; **dirty ones are kept and listed**; no branch is deleted. Same rule as Claude's worktree cleanup: never delete work |

## 11. Not in v1

- **SSH Hives** (workers on a remote host). The pieces exist (bridge, remote watch scripts),
  but local first.
- **Mixed runtimes.** v1 is Claude only, because shared memory rides on Claude's
  `autoMemoryDirectory`. Codex workers need their own memory path.
- **Scheduled work in a Hive** (Projects' scheduled tasks). Follow-ups cover one-offs already.
- **Hive templates** (save a Hive's settings to start the next one).

## 12. Spikes before building

Each is an hour or less, and each can change the design:

1. **`--settings` carries `autoMemoryDirectory`** on an interactive launch, under a managed
   account, and memory writes don't prompt in auto mode.
2. **Trust write** suppresses the dialog for a maiTerm-made worktree under a managed account (§6.3).
3. **Fixed hidden PTY size**: a hidden tab spawned at, say, 120×40 never gets a resize from
   anywhere. Known today: a hidden new tab spawns at 80×24 (agent-requests memory).
4. **Build-cache cost**: time for a fresh worktree of this repo to reach a passing `npm run check`
   and `cargo check`, with and without the APFS-cloned `target/`.
5. **Concurrent memory writes**: two agents writing memory at once, with the index rebuild
   running, loses nothing.

## 13. Decisions for the human

1. **Spawn approval.** Recommended: spawns within the cap need no card (principle 5).
   Alternative: a card per worker, or a "propose first" mode where the coordinator's suggestions
   are cards with Start.
2. **Landing.** Recommended: an integration branch, with each land approved by the human on a
   card (§8). Alternative: workers' branches are left for the human to merge by hand, with no
   integration branch.
3. **Runtime scope.** Recommended: Claude only in v1 (§11).
4. **The window Overlord** leaves Hive tabs alone. Recommended: yes (§7.3).

## 14. Build order

1. Extract `ChatColumn` from `FocusView`; Loom unchanged. Ship alone.
2. The `isHiddenTab` predicate replaces the service-only checks (Rust + TS); no behaviour change.
3. Hive workspace: sheet, integration worktree, hidden coordinator tab, `HiveView` with the
   coordinator column only.
4. Workers: worktrees, setup, trust write, `hiveSpawnWorker`/`hiveListWorkers`/`hiveReport`,
   static cards.
5. The activity feed, diff stat, worker columns, raw terminal, keyboard.
6. Landing through agent requests; retire; delete flow.
7. Shared memory: `autoMemoryDirectory`, index rebuild, Memory tab, Library.
8. Suspend/resume/restart, phone polish (Hive tabs already appear as chats).
