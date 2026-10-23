# Hive: a coordinator that spawns agents on a goal

> Status: DESIGN, 2026-10-23. Nothing built. Research behind it: `docs/claude-projects-research.md`
> (what Anthropic's cloud Projects do). Hive is the local version: the same coordinator,
> workers and shared memory, running in maiTerm on this machine, plus a gallery for results
> that aren't code.

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

A Hive works on a **primary folder plus any number of others**. They can be git repositories,
where each worker gets **its own worktree on its own branch** (its own copy of the code), or
plain folders such as footage, brand assets or documents. Every agent in the Hive reads and
writes one **shared memory**.

A Hive's goal doesn't have to be code. It can be a storyboard, a set of product shots, a
soundtrack or a research report. Everything an agent produces that a person would look at goes
into the Hive's **gallery**: a browsable library of images, video, audio and documents with how
each was made, in the spirit of Higgsfield's library.

```
┌ sidebar ┬─ Hive · Auth rewrite ──────────── Board · Gallery · Memory ──────────────────────┐
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
6. **Workers never land their own work; the coordinator does, through maiTerm.** A worker
   reports. The coordinator reviews it and lands it: by default straight into each repo's base
   branch (main), with no card (§8). maiTerm carries out the merge, never the agent's own git,
   so it can refuse a merge that would touch anyone's uncommitted work. A Hive setting switches
   to an integration branch where the human approves each land.
7. **Code lands in git; everything else lands in the gallery.** A file a person would look at
   is an asset, kept immutable, with who made it, for which task, and how. Rust owns the gallery
   store; agents add to it through a tool, never by writing into it (§10).
8. **Reuse before invent.** The cards are maiTerm Tasks. Prompts go through `PromptCard`,
   blockers through `BlockerCard`, delivery through `deliver_to_agent`, memory is Claude's own
   auto memory pointed at a shared directory, and "send this asset to an agent" is the
   composer's existing attachments.

## 3. Turning a workspace into a Hive

Creating a Hive is a sheet, opened from the workspace row's menu ("Make this a Hive…"). It's
offered only when the workspace has **no tabs** (or only one fresh, unused shell, which the Hive
replaces):

| Field | Meaning |
|---|---|
| Name | The workspace name; also the workstream name on the task board |
| Folders | **Primary** (required; default: the folder of the tab you came from) plus any number more. Each is detected as a git repo or a plain folder; a repo shows its Base branch (default: its current branch). See §6.1 |
| Goal | One line. Shown on the Hive header, and sent to the coordinator first |
| Instructions | Standing rules, the brief every agent in the Hive starts from (Projects' 16k-char instructions) |
| Workers at once | **Hard cap**, enforced in Rust (default 4) |
| Landing | **Land to Base** (default: the coordinator merges each finished worker into its repos' Base) or **Integration branch, I approve each land** (§8) |
| Land check | Optional command per repo that must pass in the worker's worktree before a land (e.g. `npm run check`) |
| Setup command | Per repo, run in every new worktree before its agent starts (e.g. `npm ci`) |
| Models | Coordinator and worker runtime/model/effort (default: Claude, coordinator lower effort, as Projects does) |
| Tools | What the Hive can make things with: generation MCP servers, skills, local CLIs, ComfyUI (§10.3). Detected; it tells the coordinator what is really available, and offers the `/generate` skill as an optional install |
| Budget | Dollars the Hive may spend on paid generation (default $10). A rule agents follow and a total the human watches (§10.3) |
| Push after land | Off by default. On: each land to Base also pushes that repo's Base (§8.5) |

On **Create**, maiTerm:

1. Lays out the Hive's folder on disk (§6.2) and the coordinator's worktrees: one per git
   folder, detached at its Base tip in the default mode, or on a new **integration branch**
   `hive/<slug>` in the approval mode. The coordinator reads code there, never in the human's
   checkouts.
2. Creates the shared memory directory and seeds it with the goal (§9), and an empty gallery
   (§10).
3. Creates the coordinator tab hidden, launched in its home folder (§6.3) with the Hive's
   settings, and sends it the Hive brief: role, goal, folders, tools, instructions, rules.
4. Switches the workspace's view to the Hive.

Folders can be added later in Hive settings. New workers can use them, and the coordinator is
told by message. A Hive can be turned back into a normal workspace only once it has no live
workers. Its tabs then appear in the strip as ordinary tabs, and nothing is deleted.

## 4. The view

### 4.1 Layout: Miller columns

The Hive header carries the name, the goal, worker counts, Pause, settings, and three views:
**Board · Gallery · Memory**. The coordinator column stays put whichever view is showing, so you
can talk to the Hive about what you're looking at.

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
- Hive worker column = `ChatColumn` + a worker header (task, folders and branches, diff stat,
  Land/Stop menu).

Each open column polls its own transcript every 3 s, as Focus does. With the coordinator and
two workers open, that's three polls. **The board never polls transcripts** (§4.4).

Additions specific to Hive inside the chat columns:

- **Spawn cards inline.** When the coordinator calls `hiveSpawnWorker`, that tool step renders
  as a small worker card (title, state dot) rather than a quiet tool line, as Projects shows a
  card under your message. Clicking it opens the worker's column.
- **Reports fold.** A worker's report arrives in the coordinator's transcript as a framed
  message (`⟦HIVE REPORT · Session store⟧ …`). It folds to one line with the report shown on
  click, the way subagent reports fold in Focus today (`injectedTurn`).
- **Assets inline.** A `galleryAdd` step renders as the asset's thumbnail (a small strip when an
  agent adds several at once). Clicking one opens it in the gallery (§10.4).

### 4.3 Cards

A card is **one worker plus the task it owns**. The task is the identity, so a worker that's
replaced (crashed, restarted) keeps its card.

```
┌─────────────────────────────────────┐
│ ● Session store                 4m  │   state dot · task title (wraps, never clipped) · age
│   Editing src/auth/store.ts         │   live activity line (§4.4)
│   "Split read/write paths; tests    │   latest task note: the context update
│    next"                            │
│   web · api          +212 −40       │   folders it works in · diff vs its landing target
│   claude · sonnet         ctx 18% ▮ │   runtime · model · context gauge
└─────────────────────────────────────┘
```

A worker that makes assets shows its newest one or two as thumbnails across the card's top in
place of the diff stat line. In a media Hive, that's how you see work arriving without opening
anything.

| State | Dot | From |
|---|---|---|
| Starting | grey pulse | Worktrees, setup or agent launch in progress |
| Working | green moving | Agent busy (hook state) |
| Needs you | orange; red for an `action` | Permission or question on screen, a task blocker, a watch script waiting |
| Idle | hollow | Turn ended without a report. Worth a look, never an alarm |
| Reported | blue check | The worker called `hiveReport` with `done` |
| Landing | blue pulse | Merging, held (§8.1), or waiting for the human in the approval mode |
| Landed | solid check, dimmed | Merged into its landing target(s) |
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
"Searching for refreshToken", "Rendering with ffmpeg". Between turns it reads "Idle · 2m".

**Diff stat** is `git diff --shortstat <target>...HEAD` in each of the worker's worktrees,
summed, recomputed by Rust after a `PostToolUse` on Edit/Write/Bash, debounced 5 s per worker.
That gives real numbers without a file watcher per worktree.

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
| Cmd+Shift+G | Switch between Board and Gallery |
| Cmd+W | Close the focused worker column (never the agent; needs two presses like tabs) |
| Escape | In a composer, blur it. In a column, close it. In the gallery's detail view, back to the grid. **Inside a raw terminal, Escape belongs to the terminal** (it interrupts the agent), so the column doesn't take it |

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
The same goes for every Hive kind field below (folder kind, asset kind, asset status).

## 6. Folders and worktrees

### 6.1 The Hive's folders

| Folder | How agents get it |
|---|---|
| **Git repo** | Each worker that needs it gets its own worktree of it (§6.4). The coordinator gets a read copy (a detached worktree). Changes reach the repo only by landing (§8) |
| **Plain folder** (footage, brand assets, docs: no git) | Shared **read-only by the brief**. There's no way to give each worker its own copy of an arbitrary folder, so nobody writes into it. Results go to the gallery, and from there are **delivered** into a folder deliberately (§10.6) |

The **primary** folder is where a worker works when its task doesn't say otherwise, and it comes
first in every brief. A Hive needs no git at all: a media Hive can have only plain folders.

`hiveSpawnWorker` names the folders a task needs (`folders: ["web", "api"]`, by the names shown
in the sheet; default: the primary). That's Projects' advice made mechanical: give a worker the
repos it needs rather than all of them, since each one is a worktree and a setup run.

### 6.2 On disk

```
<app data>/hive/<hive-id>/
  memory/                     shared memory (§9)
  gallery/                    the asset store (§10), owned by Rust
  coordinator/<folder>/       detached worktree per git folder
  wt/<worker-slug>/
    <folder>/                 the worker's worktree of each git folder it needs
    scratch/                  the worker's own working space: renders, downloads, drafts
```

It lives **outside every repo**, under `app_data_slug()` (never a hard-coded `com.aiterm.app`).
Nested inside a repo, every watcher would scan it: Vite, tsc, cargo, eslint, the Files column,
Claude's own Grep in the main checkout.

### 6.3 What an agent's folders look like to it

Claude has one working directory plus `--add-dir` for the rest. So:

- **The working directory** is the worktree of the task's first folder; for a task with no git
  folder, it's the worker's `scratch/`. The coordinator's is its read copy of the primary, or
  the Hive's own folder if the primary is plain.
- **Everything else is `--add-dir`**: the worker's other worktrees, the plain folders, and
  `scratch/`. They're launched with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`, because
  by default Claude doesn't load an added directory's `CLAUDE.md`, and each repo's rules
  have to reach the worker that edits it.
- The brief names every folder with its role: "web (git, your worktree, lands to main)",
  "brand (plain, read-only)", "scratch (yours: put renders here, then add them to the gallery)".

### 6.4 Worktrees

- **maiTerm creates them**, with `git worktree add`, rather than Claude's `--worktree`. It's
  runtime-agnostic (Codex workers later), the paths and branch names are ours, and so is
  cleanup. Claude's `--worktree` also can't branch off an arbitrary branch (`worktree.baseRef`
  is only `fresh` or `head`).
- **Branches**: `hive/<hive-slug>/<worker-slug>` in each repo, branched off that repo's
  **landing target's current tip** (Base by default, `hive/<hive-slug>` in the approval mode).
  A wave-2 worker therefore starts with wave 1's landed work.
- **Setup**: each repo's setup command runs in the new worktree, in the worker's own tab before
  its agent starts, so its output is in the raw terminal if it fails. Gitignored files the work
  needs (`.env`) are copied in from a `.worktreeinclude`-style list per repo, with Claude's
  semantics: only files that are gitignored, never tracked ones.

### 6.5 Build caches are the hidden cost

A fresh worktree has no `node_modules` and no `target/`. For a Rust repo like this one, each
worker would rebuild from scratch for minutes. Sharing one `CARGO_TARGET_DIR` serializes every
build on cargo's lock. The setup command is the place for this, plus two defaults the sheet
offers when it detects them: a per-worker `CARGO_TARGET_DIR` seeded by copying the main
checkout's `target/` with `cp -c` (APFS clone: instant, no space until written), and `npm ci`
with the shared npm cache. This is the biggest practical risk to "spawn 6 workers" feeling fast.
Measure it in the spike (§13).

### 6.6 Trust

A worktree is its own git root, and Claude's trust walk stops at the git root (the
trust-dialog memory). **Every new worktree would raise "Do you trust this folder?"**, and so
would the Hive's own folders under app data. Nothing automated may confirm that dialog
(`trust_dialog_open`, root CLAUDE.md). The human already chose these folders by creating the
Hive. So **maiTerm itself** (not an agent, which the permission classifier rightly refuses as
"Security Weaken") writes `hasTrustDialogAccepted` for each folder it creates. It writes it into
the `.claude.json` of the account the agent spawns under (`accounts/`, per-account roots), and
removes the entry when the folder is removed. Spike item: confirm it suppresses the dialog under
a managed account.

### 6.7 Isolation is a convention, not a wall

A worker launched in a `git worktree add` directory doesn't get Claude's worktree isolation
checks (those apply to `--worktree`/`EnterWorktree` sessions). Nothing stops a worker that
`cd`s into the main checkout, or writes into a plain folder. The brief says not to, and the
coordinator's tools only ever name the worktrees. A later option is a PreToolUse guard keyed
on the tab's folders, since maiTerm already receives every PreToolUse. It's not v1.

## 7. Agents, tools and authority

### 7.1 The coordinator

A normal agent tab with `hive_role: "coordinator"`. Its brief (sent once at launch, and again
after compaction through the SessionStart hook's additional context, as tab identity is today):

- It's the coordinator of Hive *X* with goal *G*. It plans, spawns, reviews, lands and curates.
  **It doesn't do the work itself**, beyond small reads to plan.
- The folders, with their roles. The tools the Hive really has (§10.3), and the rule that a
  missing one is a question for the human, never a mock or a substitute.
- Its tools, below. The cap. That memory is shared and read at start, so a decision that
  running workers need also goes to them by message.
- Results that aren't code go in the gallery, and curating them (picking finals, building
  collections) is its job.

### 7.2 MCP tools

All are refused unless the calling tab's `hive_role` and Hive match. That's enforced in Rust
from the stated tab identity, like `own_tab_only_refusal`.

| Tool | Who | Does |
|---|---|---|
| `hiveSpawnWorker {title, brief, folders?, depends_on?, inputs?, model?, effort?}` | coordinator | Creates the task, worktrees, branches and hidden tab, runs setup, launches the agent with the Hive preamble + brief. `inputs` names gallery assets the worker starts from (their paths go in its brief). Refused above the cap. A worker with unmet `depends_on` is created in **Starting · waits on …** and launched when they land |
| `hiveListWorkers` | coordinator | Cards as data: state, task, folders, branches, diff stat, assets, last report |
| `hiveMessageWorker {worker, text, assets?}` | coordinator | Through `deliver_to_agent`: the draft, dialog and typing guards; a busy agent is no reason to hold. `assets` attach gallery files as paths |
| `hiveLand {worker, summary}` | coordinator | Lands the worker (§8). Answers with what happened in each repo, or why it refused |
| `hiveStopWorker {worker, reason}` | coordinator | Stops the agent; keeps worktrees and branches |
| `hiveReport {status: done\|blocked\|failed, summary, notes?, assets?}` | worker | Records the report on the task (status + note), and delivers it to the coordinator as a framed `⟦HIVE REPORT⟧` message, with the assets it's reporting |
| Gallery tools | all Hive agents | §10.3 |

Workers keep every ordinary maiTerm tool (tasks, notes, follow-ups) and their runtime's own
subagents and workflows. What they can't do is spawn Hive workers or message other workers.
Projects doesn't let threads talk to each other either, and a star topology keeps the
coordinator the one place the plan lives. If peer talk turns out to be needed, the mesh already
exists.

### 7.3 Authority

| Action | Approval |
|---|---|
| Create a Hive; add folders | The human, by the sheet / settings. No tool can |
| Spawn a worker within the cap | None; the Hive is the envelope (decision §14.1) |
| Raise the cap | The human, in Hive settings |
| Land a worker into Base (default mode) | None: the coordinator calls `hiveLand`, and maiTerm merges behind its guards (§8) |
| Land a worker (approval mode) | The human, on a land card; merging the integration branch into Base is the human's, outside the Hive |
| Deliver assets into a folder | None for new files; overwriting anything is the human's (§10.6) |
| Push | Only if the Hive's Push after land setting is on (default off), never forced (§8.5) |
| Spend past the budget | The human, on a blocker the agent raises first (§10.3) |
| A worker's permission prompt | The human, in the worker's column, or the phone |

The window's Overlord engine leaves Hive tabs alone. A Hive workspace is `overlord_exempt` by
default, because the coordinator already supervises its workers, and two supervisors driving one
agent is the double-drive problem `docs/overlord.md` §9.1.2 solved for escalations.

## 8. Landing

The local stand-in for Projects' pull requests. Both modes start the same way:

1. A worker reports `done`. Its card goes to **Reported**.
2. The coordinator reviews: it reads the report, can read the worker's diff in each repo
   (`git diff <target>...hive/<hive>/<worker>`), and can send the worker back with
   `hiveMessageWorker`.
3. When satisfied, the coordinator calls `hiveLand`. The card goes to **Landing**.

A worker with no git folders (a media task) has nothing to land. Its `hiveLand` just retires it,
since its results are already in the gallery.

### 8.1 Default: land to Base (decided 2026-10-23)

maiTerm lands the worker into Base (main) with no card, in two git steps per repo. Neither one
can touch anyone's uncommitted work (both behaviours checked in a scratch repo, 2026-10-23):

1. **Bring the worker up to date, in the worker's own worktree:** `git merge <Base>`. A conflict
   aborts, and `hiveLand` answers with the conflicting files. The coordinator tells the worker to
   resolve them in its own tree and report again. The land check (if set) then runs there, and a
   failure refuses the land with its output.
2. **Advance Base by fast-forward only.** Since the worker now contains Base, Base moves to the
   worker's tip with no new merge:
   - if Base is checked out somewhere (usually the human's main checkout, where other agents may
     be working), `git -C <that checkout> merge --ff-only <worker tip>`. Git refuses if that would
     overwrite an uncommitted change there. **The land then waits**: the card reads "Landing ·
     held: main checkout has uncommitted changes to `src/x.ts`", and the coordinator is told,
     rather than anything being stashed or overwritten;
   - if Base isn't checked out anywhere, `git update-ref refs/heads/<Base> <tip> <old tip>`,
     a compare-and-swap that refuses if Base moved meanwhile.

Lands are serialized per repository (one lock), so two coordinators or two workers can't race.
After a land, maiTerm moves the coordinator's read copy to the new Base tip, and any new worker
branches from it. Files in the main checkout change under agents working there, the same as when
any one of them commits.

### 8.2 Several repos in one land

There's no atomic commit across repositories, so a worker that changed two repos lands in two
passes:

1. **Step 1 (update + land check) runs in every repo first.** If any repo fails, nothing lands.
2. **Then step 2 fast-forwards each repo in folder order** (or in the order the coordinator
   passes). Each repo's change has passed its own check, so landing one without the other leaves
   nothing broken in that repo. If a later repo is held, the card says so ("Landed web; holding
   api: uncommitted changes to `routes.ts`"), and the next `hiveLand` lands only what's left.

What can break is the pair: a web change that needs an api change not yet landed. That's the
coordinator's to order, the same as Projects reporting "which PRs need to merge first". The brief
says so, and the land answer always names which repos landed.

### 8.3 Setting: integration branch, I approve each land

`hiveLand` files an **Agent request** card ("Land *Session store* into *hive/auth*?", with the
diff stat and the coordinator's summary quoted as its reason) in the Loom's Decisions, in the
worker's column and on the phone. It's the existing agent-requests machinery
(`docs/agent-requests.md`): only a human click or a paired phone approves. On approval, maiTerm
merges `--no-ff` in the coordinator's worktree of each repo, now on `hive/<slug>`. At the end,
`hive/<slug>` holds the whole result in each repo, and the human merges it into Base or opens
MRs from it, outside the Hive.

### 8.4 After landing

The worker is retired: the agent exits, and its worktrees are removed **only if clean**. Its
`scratch/` is removed only once everything in it the worker reported is in the gallery. The
branches are kept.

### 8.5 Pushing

Landing is local unless the Hive's **Push after land** setting is on (decided 2026-10-23: a
setting, default off). When it's on, a land to Base pushes that repo's Base to its upstream right
after the fast-forward. A push that's refused (the remote moved, auth) doesn't undo the land.
The card shows "Landed · push held: <reason>", the coordinator is told, and the next land pushes
both. It never force-pushes.

## 9. Shared memory

**Mechanism: Claude's own auto memory, pointed at one directory.** Every Hive agent launches with
`--settings '{"autoMemoryDirectory": "<app data>/hive/<id>/memory"}'`, an absolute path (the only
kind Claude honours, per the auto-memory memory). That gives, natively:

- Each agent reads `MEMORY.md` at start and opens individual files when it needs them, which is
  exactly Projects' progressive load.
- Each agent writes memory files with the runtime's own memory behaviour, and a configured
  directory **gets write permission without prompts**. A symlinked one doesn't, which is why
  it's a configured path and not a symlink.

Why redirect at all: Claude's default auto memory is keyed by git repository and **shared across
all its worktrees**. Left alone, the Hive's workers would write into the human's everyday memory
for that repo, which every other session there reads. With several folders there would also be
several memories, not one.

What maiTerm adds:

- **Index rebuild.** Several agents editing one `MEMORY.md` at once lose updates. maiTerm
  watches the directory and **regenerates `MEMORY.md` from the files' frontmatter**
  (name, description, a link), and the brief tells agents to write files and leave the index to
  maiTerm. That's the same shape as this user's memory index, kept consistent by construction.
  Claude loads only the first 200 lines or 25 KB of `MEMORY.md`, so the rebuilt index is one
  line per file, and over the limit the oldest are folded into a dated archive entry.
- **Mid-task visibility is by message.** Agents read memory at start. A decision that running
  workers need right away goes from the coordinator to them with `hiveMessageWorker`. The brief
  says so, and nothing pretends running agents see memory live.
- **The Memory view** in the Hive header lists the files, and lets the human read, edit and
  delete them. Projects started without this and was criticised for it.

Memory holds **notes** (decisions, requirements, pitfalls, the style the human wants). The
gallery holds **results**. Repo rules stay in each repo's `CLAUDE.md`, which every worktree has
because it's a checkout.

## 10. The gallery

### 10.1 What it's for

Everything a Hive produces that isn't a commit: images, video, audio, documents (reports,
PDFs, decks), 3D files, and for code Hives too, screenshots, recordings and test reports.
It replaces the earlier "Library" folder idea.

What it borrows from Higgsfield's library: one place where every generation lands, newest
first; browse by kind; every item keeps the **recipe** that made it (prompt, model, settings),
so you can iterate from it; variations stack under their original; favourites and collections
for the ones you keep. What it adds for a team of agents: who made each asset and for which
task, and a one-click path from any asset back to the agent's chat.

### 10.2 The asset

```
Asset {
  id, kind: "image" | "video" | "audio" | "document" | "model3d" | "other",   // string, not enum
  file, mime, bytes, width?, height?, duration_ms?,
  title, caption?,
  made_by: { tab_id, role, worker_title } | "human",
  task_id?, created_at,
  recipe?: { tool, model?, prompt?, negative?, seed?, params?: json, source_url?, cost_usd? },
  parents: [asset id],          // what it was derived from: a variant, an edit, an upscale, a cut
  status: "" | "picked" | "final" | "rejected",
  starred, tags: [string], collections: [collection id],
}
Collection { id, name, asset_ids (ordered), cover? }
```

- **Immutable.** An asset's file never changes. An edit, an upscale or a re-cut is a **new asset
  whose parent is the old one**, and that lineage is how variations stack. The store's files are
  written read-only, so an agent that tries to edit one in place gets an error rather than
  silently changing history.
- **Stored in** `<app data>/hive/<id>/gallery/`: files by id, `thumbs/`, and a SQLite index
  (maiTerm already ships SQLite for maiLink). The index is Rust's alone.
- **Adding copies the file** with an APFS clone (`clonefile`) where possible, so a 2 GB render
  costs no disk until one copy changes, and the asset outlives the worker's `scratch/`.

### 10.3 How agents make and file assets

**Making** is whatever the Hive's agents can really run. The sheet's **Tools** row (§3) detects
these, in this order (decided 2026-10-23), and the brief carries the same list, so the
coordinator plans with real tools:

| Kind | Detected how | Examples |
|---|---|---|
| Hosted generation APIs as MCP servers | The agents' MCP config, including entries served by maiTerm's MCP proxy (`docs/mcp-proxy.md`) | Higgsfield (one MCP, 30+ image and video models), fal.ai, Kling, WaveSpeed AI, Kie.ai |
| Skills | `SKILL.md` folders the agents load (user and project scope) | The `/generate` skill (Sabrina Ramonov's, built on a RoboNuggets guide: picks the cheapest capable model on Kie.ai, quotes the cost, keeps a cost ledger), RoboNuggets' `higgsfield-skill` and `seedance-skill`. The sheet offers `/generate` as an optional install to boost what a Hive can make |
| Local CLIs | On `PATH` | ffmpeg, ImageMagick, sox, Blender |
| Local ComfyUI | Its API answering on its configured port | Workflows run through the HTTP API |

An agent renders into its `scratch/`. A generator that answers with a URL is downloaded there
first, and a skill that writes to a folder of its own is pointed at `scratch/` where it allows it.
The agent then files the result.

**Paid generation spends real money**, and a Hive can run many workers at once. So a Hive has a
**budget** (sheet and settings, in dollars; default $10):

- An asset's recipe carries its **cost** where the generator reports one. The `/generate` skill
  and most hosted APIs quote before running. The Hive header shows the running total against
  the budget.
- The brief carries the budget and the rule: **before a spend that would cross it, stop and ask
  the human** with a task blocker (a Needs-you card), never decide alone.
- A skill's own "approve this cost?" question, asked inside a hidden worker, arrives as a
  Needs-you card like any other question. The Hive's instructions can pre-approve spends under a
  per-item amount so workers don't stall on every 3-cent frame.
- This accounting is **self-reported**. maiTerm can't see inside an API call an agent makes
  directly, so the budget is a rule agents follow plus a total the human watches, not a wall.
  The only hard limit is the provider account's own credit, which is why the sheet says so next
  to the field.

**Filing**, all Hive agents:

| Tool | Does |
|---|---|
| `galleryAdd {path, title, kind?, recipe?, parents?, tags?, collection?, caption?}` | Files one asset (or several, as a list). `task_id` and `made_by` come from the caller's identity, never from arguments. Answers with the asset id |
| `galleryList {kind?, made_by?, task?, collection?, status?, tag?, query?, limit?}` | Assets as data, newest first |
| `galleryGet {id}` | One asset with its recipe and lineage, and the stored file's path, to use as an input |
| `galleryTag {id, tags?, status?, starred?}` | Workers may tag and star; `final` and `rejected` are the coordinator's and the human's |
| `galleryCollect {collection, add?, remove?, order?}` | Coordinator: build and order collections ("Storyboard v2", "Shortlist") |
| `galleryDeliver {assets \| collection, folder, names?}` | Coordinator: §10.6 |

**`galleryAdd` takes a path only from the caller's own folders**: its `scratch/`, its worktrees,
the system temp directory, or an **output folder the human listed** in Hive settings for a tool
that writes only to its own place. It's matched whole against the canonical path, with symlinks
resolved first (the path-authorization memory: never a prefix test). The gallery is reachable from the
phone later, so it mustn't become a way to lift any file on the machine into view.

Every agent's brief says: anything you produce that a person would look at goes in the gallery,
**with the recipe that made it**. A render without its prompt and settings can't be iterated
on, and iterating is the point.

The human adds assets too, by dropping files on the gallery (references, mood boards, footage).
They're `made_by: "human"`, and the coordinator can hand them to workers as `inputs`.

### 10.4 The gallery view

It takes the board's place (Board · Gallery in the header); the coordinator column stays on the
left.

```
┌ COORDINATOR ──────┬─ GALLERY ───────────────────────────────── 🔍 prompt, title, tag ─┐
│ You: three more   │ All · Images · Video · Audio · Docs      Group: Task ▾   ▭▭▭ size │
│ like the second,  │ ┌ filters ─┐ ┌──────┐┌────────────┐┌──────┐┌──────┐               │
│ warmer light      │ │ Made by  │ │      ││            ││ ▶ 0:12│ │ ≋≋≋≋ │               │
│                   │ │ Task     │ │  ★   ││   final ✓  ││      ││ 2:04 │               │
│ [▣ hero-2.png]    │ │ Status   │ └──────┘└────────────┘└──────┘└──────┘               │
│ [ message… ]      │ │ Starred  │ ┌────────────┐┌──────┐ ⧉3 variants                   │
│                   │ │ Tags     │ │            ││      │                               │
│                   │ │ Collect. │ └────────────┘└──────┘                               │
└───────────────────┴──────────────────────────────────────────────────────────────────────┘
```

**The grid.** A masonry layout that keeps each asset's aspect ratio (images and video), with a
size slider. It's virtualized, because a media Hive reaches thousands of assets. Kind tabs run
across the top, and a filter rail holds Made by, Task, Status, Starred, Tags and Collections.
Search covers titles, prompts and tags. **Group by** None, Task, Worker, Day or Collection.

- Images: the thumbnail.
- Video: a poster frame. **Hovering scrubs** through a frame strip, and holding plays muted.
- Audio: the waveform with its duration. Click to play in place.
- Documents: a first-page thumbnail with a type badge.
- Badges: star, final ✓, rejected (dimmed), and a stacked edge with **"⧉ 3 variants"** where an
  asset has children. The children fold under their original unless "Show variants" is on.

**New assets stream in** as agents file them, by a window-scoped event, with a brief highlight.
**Following** (on by default while agents are working) keeps the newest in view.
Scrolling away turns it off, the way a terminal stops following when you scroll up.

**The detail view** opens in the gallery's area (never over the coordinator), with arrow keys
for previous and next:

- A large preview with real players: `<video>` with frame stepping, `<audio>` over its waveform,
  zoom for images, page through documents.
- **The recipe**: prompt (with copy), model, tool, seed, settings.
- **Made by** the worker and **for** the task, each a link that opens that worker's column
  (principle: every result is one click from the agent that made it).
- **Lineage**: a strip of parents ← this → children.
- Actions:
  - Star; Pick / Final / Reject.
  - Add to collection.
  - **Iterate**: attaches the asset to the coordinator's composer, with the recipe quoted, ready
    for "three more like this, warmer light".
  - **Send to worker…**: attaches it to a worker's composer.
  - Reveal in Finder, Copy path, Export….

**Compare.** Select two to four assets, then Compare: side by side, video playback synced, audio
A/B switched by a key. Picking a winner from variants is most of the work in a media Hive.

**Multi-select** with bulk star, status, tag, collect and export. Dragging assets onto any
composer attaches them, through the composer's existing attachments
(`lib/composer/attachments.ts`), so the agent receives paths it can open.

**Keyboard:** arrows move, Space plays or pauses, Enter opens the detail view, F stars,
1/2/3 set Pick/Final/Reject, C compares the selection, Escape goes back.

### 10.5 Thumbnails, metadata, serving

- Rust generates thumbnails and reads metadata when an asset is filed, off the main thread.
  - **macOS**: QuickLook (`qlmanage -t`, present on every Mac) gives thumbnails for images,
    video, audio, PDF and Office files with no install.
  - **ffmpeg/ffprobe**, when installed (they are here), add dimensions and duration, video hover
    strips and audio waveforms.
  - **Windows/Linux**: ffmpeg plus the `image` crate.
  - With neither tool, a tile shows a kind icon and the title. Nothing is required.
- The webview reads gallery files through Tauri's asset protocol **scoped to the gallery folder
  only**, which also gives video range requests for seeking. It is never a general filesystem
  scope.

### 10.6 Delivering assets into folders

`galleryDeliver` is the media counterpart of landing: chosen assets (or a collection, in order)
copied into a folder, with names given or derived from titles.

- **Into a plain folder:** new files only. A name that exists gets a suffix, and **overwriting is
  the human's** (Export… with Replace).
- **Into a git folder:** not written directly. The coordinator spawns (or messages) a worker to
  put them into its worktree, and they arrive by landing, so the commit is reviewable like any
  other change. That covers "put the final icons in `web/public/icons` and wire them up".

### 10.7 Lifecycle

- Rejecting doesn't delete. **Delete** moves an asset to the gallery's trash; the human empties
  it.
- Deleting a Hive keeps its gallery unless the human ticks "also delete the gallery (4.2 GB)" in
  the delete sheet.
- The phone gets the gallery later (maiLink already sends files, `sendFilesToPhone`). A gallery
  feed is a protocol change, so it bumps `protocolVersion` (`docs/mailink-protocol.md` §13.5).

## 11. Lifecycle

| Event | What happens |
|---|---|
| Suspend the workspace | Every Hive tab suspends (PTYs killed); worktrees, memory and gallery stay. Cards show Suspended |
| Resume | Each agent relaunches with its runtime's resume command **in its own folder**; the coordinator gets a Hive status digest |
| maiTerm restart | Same as resume, through session restore |
| Worker crash | Card → Failed; coordinator is told by a framed message; it may respawn (same task, new tab) |
| Reload a Hive tab | The existing reload path (new tab id, `remapTab` for tasks); the card is keyed by task, so it doesn't move. Gallery assets keep the task id, not the tab id, for the same reason |
| Pause (Hive header) | Stops new spawns and holds deliveries; running turns finish |
| Delete the Hive | A sheet lists every branch not landed, every dirty worktree and the gallery's size. Clean worktrees are removed; **dirty ones are kept and listed**; no branch is deleted; the gallery stays unless ticked. Same rule as Claude's worktree cleanup: never delete work |

## 12. Not in v1

- **SSH Hives** (workers on a remote host). The pieces exist (bridge, remote watch scripts),
  but local first.
- **Mixed runtimes.** v1 is Claude only, because shared memory rides on Claude's
  `autoMemoryDirectory` and folders on `--add-dir`. Codex workers need their own paths for both.
- **Scheduled work in a Hive** (Projects' scheduled tasks). Follow-ups cover one-offs already.
- **Hive templates** (save a Hive's settings to start the next one).
- **A gallery across all Hives.** Each Hive's gallery stands alone in v1; one window-wide view
  over all of them (Higgsfield's single library) is a later read over the same stores.

## 13. Spikes before building

Each is an hour or less, and each can change the design:

1. **`--settings` carries `autoMemoryDirectory`** on an interactive launch, under a managed
   account, and memory writes don't prompt in auto mode.
2. **Trust write** suppresses the dialog for a maiTerm-made worktree under a managed account (§6.6).
3. **Fixed hidden PTY size**: a hidden tab spawned at, say, 120×40 never gets a resize from
   anywhere. Known today: a hidden new tab spawns at 80×24 (agent-requests memory).
4. **Build-cache cost**: time for a fresh worktree of this repo to reach a passing `npm run check`
   and `cargo check`, with and without the APFS-cloned `target/`.
5. **Concurrent memory writes**: two agents writing memory at once, with the index rebuild
   running, loses nothing.
6. **`--add-dir` folders**: with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`, an added
   worktree's `CLAUDE.md` loads, and auto mode writes there without prompting.
7. **Thumbnail cost**: `qlmanage -t` and an ffmpeg frame strip on a 4K, 2-minute video; both must
   stay well under the time an agent takes to make the next asset.

## 14. Decisions

Decided 2026-10-23:

1. **Spawn approval:** spawns within the cap need no card (principle 5).
2. **Landing:** by default the coordinator lands each worker straight into Base (§8.1). The
   integration branch with human-approved lands is a setting (§8.3).
3. **Runtime scope:** Claude only in v1 (§12).
4. **The window Overlord** leaves Hive tabs alone (§7.3).
5. **Folders:** one primary plus any number more, git or plain; plain folders are read-only
   inputs (§6.1). Self-answered, per the brief to decide as a pro would.
6. **Gallery:** per Hive, immutable assets with recipes and lineage, filed by tool, owned by
   Rust (§10). Self-answered likewise.

7. **Push:** a Hive setting, default off (§8.5).
8. **Generators:** hosted APIs as MCP servers, skills, local CLIs and local ComfyUI, with Kling,
   fal.ai, WaveSpeed AI, Kie.ai and Higgsfield named, and the `/generate` skill offered as an
   optional install (§10.3). Paid generation brings a per-Hive budget, self-answered.

## 15. Build order

1. Extract `ChatColumn` from `FocusView`; Loom unchanged. Ship alone.
2. The `isHiddenTab` predicate replaces the service-only checks (Rust + TS); no behaviour change.
3. Hive workspace: sheet (folders), the Hive folder on disk, coordinator read copies, hidden
   coordinator tab, `HiveView` with the coordinator column only.
4. Workers: worktrees per folder, `--add-dir`, setup, trust write,
   `hiveSpawnWorker`/`hiveListWorkers`/`hiveReport`, static cards.
5. The activity feed, diff stat, worker columns, raw terminal, keyboard.
6. Landing: land to Base (default) including several repos, the approval-mode setting through
   agent requests; retire; delete flow.
7. Shared memory: `autoMemoryDirectory`, index rebuild, Memory view.
8. Gallery: store and index, `galleryAdd`/`List`/`Get`/`Tag`/`Collect`, thumbnails, the grid
   and detail view; then compare, Iterate/attach, collections, deliver.
9. Suspend/resume/restart, phone polish (Hive tabs already appear as chats; the gallery feed
   later).
