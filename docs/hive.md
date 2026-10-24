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
   `bridge_all` (`Workspace.hive`, §3.2), and gets its own branch in `+page.svelte` like
   `MeshStageView`.
2. **Every Hive agent is an ordinary terminal tab, hidden.** The coordinator and each worker
   are real PTYs running a real agent CLI, with everything maiTerm already does for an agent
   tab: hooks, tab identity, transcripts, prompts, the phone, accounts. They're kept out of the
   tab strip the way stack services are (`docs/stack.md` §7), but unlike a service they **are
   agents**: hidden from the strip is not the same as "not an agent" (§5). **Nothing in the
   Hive is a new kind of agent.**
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
   so it can refuse a merge that would touch anyone's uncommitted work. A land that changes
   agent or execution config always goes to the human (§8.1). Choosing, when the Hive is
   created, an integration branch instead makes the human approve each land (§8.3).
7. **Code lands in git; everything else lands in the gallery.** A file a person would look at
   is an asset, kept immutable, with who made it, for which task, and how. Rust owns the gallery
   store; agents add to it through a tool, never by writing into it (§10).
8. **maiTerm owns the facts; agents read them.** Whether a worker exists, runs, waits, or has
   landed comes from tabs, PTYs, hooks and git, never from a ledger agents are asked to keep
   in step with reality. The coordinator's tools read state; none of them declare it. A worker's
   "done" counts only with receipts (§7.7). ruflo is the cautionary example (§15): its swarm is
   JSON records the model keeps in sync, and most of its bugs are those records drifting.
9. **Reuse before invent.** The cards are maiTerm Tasks. Prompts go through `PromptCard`,
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
| Folders | **Primary** (required) plus any number more. The Primary defaults to the folder of the window's active tab (the workspace being converted usually has none, so it's the tab the human was last looking at), and to nothing when that tab has no known folder; the human then picks one. Each folder is detected as a git repo or a plain folder; a repo shows its Base branch (default: its current branch). See §6.1 |
| Goal | One line. Shown on the Hive header, and sent to the coordinator first |
| Instructions | Standing rules, the brief every agent in the Hive starts from (Projects' 16k-char instructions) |
| Workers at once | **Hard cap**, enforced in Rust (default 4). What it counts is §3.1 |
| Landing | **Land to Base** (default: the coordinator merges each finished worker into its repos' Base) or **Integration branch, I approve each land** (§8). **Fixed at creation.** It's a branch topology, not a checkpoint: the Landing checkpoint (§7.4) only decides who calls `hiveLand` *within* the mode, and integration mode is always Ask me. Switching later would fast-forward integration commits into Base, so a different mode means a new Hive |
| Stack | The services this Hive's work needs, proposed from the repos' own stacks and the human's other workspaces (§6.8). See step 3 below |
| Land check | Optional command per repo that must pass before a land (e.g. `npm run check`), run on the reviewed commit merged with Base in a temporary worktree (§8.1) |
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
2. Creates the shared memory directory, writes its `MEMORY.md` with the fences (§9) and seeds
   it with the goal, and creates an empty gallery (§10).
3. Sets up the Hive's stack (§6.8). Stacks are per workspace (`Workspace.stack`), so copying
   the repo's stack as is would start a second Postgres or dev server beside the human's. So:
   - **Infrastructure the human already runs** in another workspace is not copied. The Hive's
     service is a reference, `Service.shared_from {workspace_id, service_id}`: the Hive never
     starts, stops or retargets it, and its port fills the Hive's env templates.
   - **Code-bearing services** must take their port from `{port}`. One with a hard-coded port
     that another workspace's service is using is refused, and the sheet says which. One with
     a free hard-coded port is imported as a one-copy service, taken in turns, and flagged.
   - **The main read copy** reuses the human's running service for that repo on Base when
     there is one, rather than starting its own.
4. Creates the coordinator tab hidden, launched in its home folder (§6.3) with the Hive's
   settings, and sends it the Hive brief: role, goal, folders, tools, instructions, rules.
5. Switches the workspace's view to the Hive.

Folders can be added later in Hive settings. New workers can use them, and the coordinator is
told by message.

### 3.1 What the cap counts

"Workers at once" counts **agent processes that are live or launching**: a worker in Starting
once its agent has been launched, Working, Needs you, Idle, Reported or Landing (§4.3), and
every reviewer (§7.7). It doesn't count the coordinator, a worker that's Landed, Failed or
Stopped, or a worker **waiting on `depends_on`**. A waiting worker is only a task and a card: its
worktrees, setup and agent are created at launch, so it branches from Base as it is then, after
the work it waited on has landed (§6.4). Rust re-checks the cap, the fuses (§7.8) and the
quota breaker at launch; if any of them blocks, the worker keeps waiting and its card says why.
A dependency that fails or is undone shows "blocked: <dep> failed" (or "undone") on the waiter's
card, and the coordinator is told by a framed message.

### 3.2 The record

A Hive is `Workspace.hive: Option<Hive>` in the state file:

```rust
struct Hive {
    id: String,              // its own uuid, never the workspace id
    goal: String,
    instructions: String,
    folders: Vec<HiveFolder>, // { name, path, kind, base, setup, land_check, primary }
    cap: u32,
    landing_mode: String,    // "base" | "integration"; fixed at creation
    models: HiveModels,      // coordinator and worker runtime/model/effort
    tools: Vec<String>,
    checkpoints: BTreeMap<String, String>, // checkpoint -> "coordinator" | "ask"
    budget_cents: u64,
    push_after_land: bool,
    settings: HiveSettings,  // the other Hive settings sections name: prompt timeout (§4.7),
                             // localhost widening (§6.7), queued commands and resources (§6.8),
                             // extra config paths (§8.1), routing (§7.9), output folders (§10.3)
    criteria: Vec<String>,   // completion criteria, current list (§11.4)
    status: String,          // "active" | "paused" | "done"
    done_at: Option<i64>,    // set by hiveFinish (§11.4)
}
```

Every kind field (`kind`, `landing_mode`, the checkpoint values, `status`) is a **String**, for
the same reason as `Tab.hive_role` (§5): a new serde enum variant makes an older build load an
empty state. The id is the Hive's own because `<app data>/hive/<id>/` is named by it, and
duplicating a window or importing re-mints workspace ids (`window.rs:749-756`).

Where each piece of state lives:

| Where | What |
|---|---|
| State file | `Workspace.hive`, `Tab.hive_role`, the Hive's tasks (its workstream), its stack definitions, pending `⟦HIVE RUN⟧` results (§6.8), and the machine-wide spawn counters and breaker expiry (§7.8) |
| `<app data>/hive/<id>/` | Memory, gallery, worktrees, scratch, each task's launch record and session list (§11.1), precedents (§7.5), and the logs Rust keeps for the Hive (§6.2) |
| Rust memory only | The run queue (§6.8), spawn reservations (§7.8). Rebuilt from the above after a restart, never persisted as truth (principle 8) |

### 3.3 Turning a Hive back into a workspace

A Hive can be turned back into a normal workspace only once it has no live workers. Rust then
**clears `hive_role` on every tab** in the workspace and runs `heal_pane_active_tab`, so the tabs
appear in the strip as ordinary tabs and one becomes active. Without that, they'd stay hidden
and the workspace would look empty. `Workspace.hive` is cleared; worktrees, memory and gallery
stay on disk, and nothing is deleted. The other lifecycle paths (duplicate, move, archive,
close, share) are in §11.2.

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
| Board | flexible, min 260 px | Shrinks to a **rail** (200–240 px, compact rows) when a worker column opens |
| Worker columns | min 420 px each | **At most two** side by side. Opening a third replaces the least recently focused unpinned one. A pin keeps a column open |

**The widths have to fit a laptop.** Coordinator 380 + rail 200 + two workers at 420 is about
1420 px before the 215 px sidebar and an open Files column (280 px, which counts in the
budget). When the main area is narrower than what's open, maiTerm gives way in this order:

1. Drop to **one worker column** (the least recently focused unpinned one closes; a pinned one
   stays and the other closes).
2. **Collapse the coordinator** to its 44 px strip.
3. Nothing further: the main area **never scrolls horizontally**, and no step refits a PTY
   (principle 3, §4.5).

The board turns into a rail rather than disappearing, because the human keeps needing to jump
between workers. The rail is the same cards in compact form (badge number §4.6, state dot,
title, activity line), sorted by the order in §4.3. A rail title **wraps to two lines** rather
than clipping, since worker titles often share a prefix ("Session store: reads", "Session
store: writes"), and the full title is on a `Tooltip.svelte` (never a native `title=`).

**Files in a Hive.** The Files column follows "the active tab" (`docs/file-tree.md`), which a
Hive never has, and today's fallback (`fileTree.svelte.ts:649-705`) would adopt the first hidden
Hive tab. In a Hive it follows the **focused column's tab** instead: a worker column shows that
worker's primary worktree, the coordinator column its read copy (§6.3). `followableTab` and
`fallbackTab` use `isStripHidden` (§5), so they never pick a Hive tab on their own.

This is not an overlay. The Loom left the terminal area because a heavy drawer fought a live
terminal for paint and keyboard (`docs/loom.md`). A Hive has no visible terminal, so its
columns are the whole main area, and nothing fights.

### 4.2 The chat column: one component, three places

`FocusView.svelte` today is one 1138-line component holding both the chat list and the open
chat. **The first step of this feature is to extract the open chat as `ChatColumn.svelte`.**
It carries everything below the list: the transcript folded by `chatRows`, tool steps, task
events, the prompt card, the task question, attachments, the composer with its
Sending/Queued/Delivered bubbles, draft refusal and every guard.

Its props are a **source**, not a tab id, because a retired worker (§8.4) has no tab:

```ts
type ChatSource =
  | { kind: 'tab'; tabId: string }
  | { kind: 'retired'; hiveId: string; taskId: string };
// props: { source: ChatSource; readOnly: boolean; header: Snippet }
```

- `readOnly` hides the composer and the prompt and blocker answers. A retired source is always
  read-only.
- `header` is a snippet the caller passes. The common part (model, effort, context gauge) stays
  in `ChatColumn`; the Overlord-only pieces in today's Focus header (ritual, Release, Trigger,
  `FocusView.svelte:175-185`) move out into the snippet Loom passes, so a Hive column never
  carries them.
- A `tab` source reads through `get_tab_transcript(tab_id)` as today. A `retired` source reads
  through a new Rust `get_task_transcript(hive_id, task_id)`, which reads **only paths maiTerm
  recorded** for that task and never a path sent from the frontend.

**What's recorded.** A transcript path recorded once goes stale: `/clear`, a resume and a reload
all start a new session. So on every SessionStart for a Hive tab, Rust appends
`{session_id, transcript_path, started_at}` to that task's session list, kept with its launch
record (§11.1), deduplicated by `session_id`. `get_task_transcript` renders the sessions in order, with
a divider between them, so a replaced or cleared worker's whole history reads as one column.

Then:

- Loom Focus = the list + `ChatColumn` + the Loom header snippet, with no behaviour change.
  This ships first, alone, so a regression shows up where people already look.
- Hive coordinator column = `ChatColumn` + a Hive header (goal, worker count, Pause).
- Hive worker column = `ChatColumn` + a worker header (task, folders and branches, diff stat,
  Land/Stop menu).
- Retired worker = `ChatColumn` with a `retired` source, read-only, + a worker header without
  the menu.

Each open column polls its own transcript every 3 s, as Focus does. With the coordinator and
two workers open, that's three polls. **The board never polls transcripts** (§4.4), and the
header's meta is pushed, not polled (§4.4), so the only transcript reads are the open
columns'.

Additions specific to Hive inside the chat columns:

- **Spawn cards inline.** When the coordinator calls `hiveSpawnWorker`, that tool step renders
  as a small worker card (title, state dot) rather than a quiet tool line, as Projects shows a
  card under your message. Clicking it opens the worker's column.
- **Reports fold.** A worker's report arrives in the coordinator's transcript as a framed
  message (`⟦HIVE REPORT · Session store⟧ …`). It folds to one line with the report shown on
  click, the way subagent reports fold in Focus today. `injectedTurn` (`loom/model.ts:206-216`)
  matches only "Another Claude session…", so this needs its own recogniser beside it: a user
  turn in a coordinator's transcript that starts with `⟦HIVE REPORT ·`, title up to the closing
  `⟧`. It only folds the turn for display; nothing acts on it.
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

| Sort | State | Dot | From |
|---|---|---|---|
| 1 | Needs you | orange; red for an `action` | Permission or question on screen, a task blocker, a watch script waiting. The human or the coordinator may be the one answering (§4.7) |
| 2 | Blocked | amber | The worker called `hiveReport {status: "blocked"}`: it's waiting on the coordinator, not the human |
| 3 | Working | green moving | Agent busy (hook state) |
| 4 | Starting | grey pulse | Worktrees, setup or agent launch in progress. Sub-label **waits on …** while unmet `depends_on` holds it (§3.1), or **blocked: <dep> failed / undone** |
| 5 | Idle | hollow | Turn ended without a report. Worth a look, never an alarm |
| 6 | Reported | blue check | The worker called `hiveReport` with `done` |
| 7 | Landing | blue pulse | Merging, held (§8.1), or waiting for the human in the approval mode |
| 8 | Failed / Stopped | red / grey | Reported `failed`, crashed, or stopped by the human |
| 9 | Suspended | grey, hollow | Its tab is suspended (the workspace or the tab, §11.2). Resuming it relaunches the worker through the launch builder (§11.1) |
| 10 | Landed | solid check, dimmed | Merged into its landing target(s). Qualifiers: **suspect** (the post-land check failed, §8.6; sorts with Needs you while it stands) and **undone** (§8.6) |
| 11 | Retired | dimmed, no dot | Agent exited after landing or stopping; the chat stays readable (§8.4) |

The rail sorts by the first column, then by badge number (§4.6) within a state.

A Needs-you card carries the waiting count and an orange edge. Clicking it opens the worker
column scrolled to the prompt card. The answer happens **in the column**, through the same
`PromptCard`/`BlockerCard` and the same 1.5 s guards, never on the card itself. While the
checkpoint for that prompt is Coordinator, the column shows it as **With coordinator**,
read-only with a take-over action (§7.4). Cards are small
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
already reach `claude_code/server.rs`, and the agent state the frontend already keeps per tab
carries `toolName`, `toolDetail` and `idleSince` (`agentState.svelte.ts:85-90`, fed from
`app_state.rs:138-144`). The card renders from those; no new Rust map or event. A new
verb-plus-target formatter turns them into "Editing store.ts", "Running npm test", "Searching
for refreshToken", "Rendering with ffmpeg": the target is the file's base name for
Edit/Write/Read, the command's first line for Bash, the pattern for Grep. Between turns it reads
"Idle · 2m". The phone's `toolVerb` (`loom/model.ts:108-117`) returns a verb only, and it stays
unchanged; the formatter sits beside it. A dedicated activity map and event come back only if a
measured need shows up.

**Card meta (model, context) is pushed, not read per card.** Today `getTabMeta` parses the
transcript and Focus polls it every 3 s (`commands/overlord.rs:115-123`), which on a board of
cards would be a transcript read per card. Instead Rust recomputes `meta_for` for a Hive worker
on Stop, SessionStart and compaction, debounced per worker, without going through
`pending_queue`, and pushes it in an event **sent to that Hive's window only** (`win.emit`
broadcasts to every window, see the common-pitfalls memory). The budget is one transcript read
per open column (§4.2) and none per card.

**Diff stat** is computed by Rust in each of the worker's worktrees and summed, recomputed after
a `PostToolUse` on Edit/Write/Bash, debounced 5 s per worker. `<target>...HEAD` would compare
commits only, and read +0 −0 through all the editing before a commit, so the stat counts the
working tree:

- **Total:** `git diff --shortstat $(git merge-base <target> HEAD)`, which covers committed and
  uncommitted changes to tracked files, plus a count of untracked files
  (`git ls-files --others --exclude-standard`). Nothing touches the worker's index.
- **Committed:** `git diff --shortstat <target>...HEAD`, shown separately in the worker header
  (§4.2), since that is what a land would take.

That gives real numbers without a file watcher per worktree.

### 4.5 Raw terminal, the escape hatch

Some things only the terminal shows: a dialog the chat doesn't parse, a crashed CLI, a setup
command's output. Each column's menu has **Show terminal**. It portals that tab's `TerminalPane`
into a slot in the column (`data-terminal-slot`). The stack drawer is the model **for the slot
mechanics only**: the pane must not render a slot of its own for a hidden tab, since the portal
takes the first match, which is the bug the stack drawer already hit.

The drawer is not the model for sizing. A `TerminalPane` today fits and resizes its PTY in four
places: the visibility effect (`TerminalPane.svelte:1460-1471`), `attachToSlot` (`:481-489`),
the ResizeObserver at 150 ms (`:1185-1195`) and font changes (`:1545-1556`). Any one of them
would fire when the column opens or is dragged, and a width change on a streaming worker
duplicates its scrollback (principle 3). So a Hive terminal mounts in a **`fixedSize` mode**
that skips `fitWithPadding` and `resizeTerminal` on all four paths. xterm takes the PTY's
recorded cols and rows, and the slot is `overflow: auto`, so it scrolls in both directions when
the column is smaller. A font change in that mode re-renders at the new cell size without
touching the PTY.

Spike 3 (§13) proves this: show, hide and drag-resize the column on a streaming worker, change
the font size, and check `resize_pty` is never called.

### 4.6 Keyboard

In a Hive workspace there's no tab strip, so the tab keys are free:

| Key | Action |
|---|---|
| Cmd+1 | Focus the coordinator's composer |
| Cmd+2…9 | Open/focus the worker column with badge 1…8 |
| Cmd+Shift+[ / ] | Move focus between columns |
| Cmd+Shift+Y | Cycle the view: Board → Gallery → Memory |
| Cmd+W | Close the focused worker column (never the agent; needs two presses like tabs) |
| Escape | In a composer, blur it. In a column, close it. In the gallery's detail view, back to the grid. **Inside a raw terminal, Escape belongs to the terminal** (it interrupts the agent), so the column doesn't take it |

**Badge numbers.** Board order shifts every time a card changes state (§4.3), so a key bound to
"the Nth card" would open a different worker from one press to the next. Each worker instead
gets a **number in spawn order**, shown as a badge on its card and rail row. It's kept for the
task's life, kept across a replacement (crash, restart, re-route), and never reused, even after
the worker retires. Cmd+2…9 map to badges 1–8; workers past 8 are reached by click.

**Not Cmd+Shift+G.** G is the editor's find-previous, which is why the Loom took J
(`+layout.svelte:1105-1106`). No editor tab is active in a Hive today, but the gallery's detail
view or a later notes pane could hold one, so the Hive stays off G (the conservative choice).

**Scoping.** The Hive's Cmd keys above are added to `isAppShortcut` (`+layout.svelte:912`), so
they work with focus in a composer, the Files tree or the gallery. The gallery's bare keys
(arrows, Space, Enter, F, 1/2/3, C, §10.4) act only while focus is inside `[data-gallery]`, and
never while focus is in an input, a textarea or a composer, mirroring how `[data-filetree]`
scopes the Files column's keys. Inside `[data-gallery]`, only `isAppShortcut` keys reach the
app.

### 4.7 Notifications

A worker's prompt is normally the coordinator's to answer (§7.4). Today's dispatch paths don't
know that:

- `isTabVisible` needs `pane.active_tab_id`, which a Hive tab never is (§5), so every Hive tab
  counts as not visible.
- The permission dispatch has no visibility check at all.
- AskUserQuestion and compaction dispatch for any tab that isn't visible
  (`agentState.svelte.ts:95-100,411,460,537,632`).
- The doorbell rings for designated tabs (`mailink/mod.rs:202,4962,6812`).

Left alone, every worker prompt would toast, raise an OS notification and ring the phone, and the
human and the coordinator would both answer it. So every one of those paths, plus agent-exit
completion and `ring_devices`, asks one gate first: **`hiveOwnsPrompt(tab, kind)`**, with a Rust
twin for the doorbell.

- **The checkpoint for that kind is Coordinator** (Workers' questions, Workers' permission
  prompts, §7.4): no toast, no OS notification, no phone push, no command-completion notice. The
  prompt still shows where the human looks on purpose: the Loom, the corner card and the card's
  dot, as for a muted tab (`docs/mute.md`).
- **Still reaches the human as today:** a task blocker the coordinator raises, anything under a
  checkpoint set to Ask me, and everything that never moves to the coordinator (§7.4: the
  workspace-trust and plan dialogs, any prompt or row outside `hiveAnswerWorker`'s allowlist, a
  spend past the budget, raising the cap or budget). The gate returns false for those.
- **Timeout:** a coordinator-owned prompt still unanswered after **T** (a Hive setting, default
  5 minutes) notifies the human once, through the normal dispatch, and is then the human's to
  answer as well. A stuck or busy coordinator never leaves a worker waiting silently.
- **Visibility:** an open Hive column on a focused window counts as visible for its tab, so
  command completion and the like follow today's "not visible" rule there.
- **Phone:** Hive tabs appear grouped under their Hive. A coordinator-owned prompt is served
  `respondable:false` with no options, so the phone can't race the coordinator; past the
  timeout it becomes respondable. This is a wire change and bumps `protocolVersion`
  (`docs/mailink-protocol.md` §13.5).
- **Not `Tab.muted`.** Muting is the human's own setting and silences everything from a tab,
  including the timeout notice and blockers. The gate is computed from the Hive's checkpoints
  each time and never writes a mute flag.

## 5. Hidden tabs: two predicates

Stack services established the rule that a hidden tab is **never `pane.active_tab_id`**. That
keeps the ~40 `activeTab` consumers (Cmd+W, Cmd+D, the composer dock, find…) from acting on a
tab nobody can see. Hive tabs take the same invariant.

But today's `!service_id` checks mean two different things, and only one of them applies to a
Hive tab. Some mean "this tab is in the strip"; many mean "this tab is an agent" (a service
"runs a dev server, not an agent", `claudeCode.svelte.ts:2025-2027`). A Hive tab is out of the
strip **and** an agent. Folding both into one `isHiddenTab` would make Hive agents unable to own
tasks and drop them from the Loom, the mesh layout, browser link and liveness. So there are two
predicates:

```ts
isStripHidden(tab) = !!tab.service_id || !!tab.hive_role   // not in the strip
isServiceTab(tab)  = !!tab.service_id                      // not an agent
```

`isStripHidden` has a Rust twin, used by `pick_active_after_close`, `heal_pane_active_tab` and
the rest of `commands/workspace.rs:390-441`. In a Hive workspace the pane has **no active
tab**, which `heal_pane_active_tab` already allows ("a pane of only services legitimately has no
active tab"). Every existing `!service_id` check is sorted into one of the two:

| Call site | Predicate | Hive tab |
|---|---|---|
| Strip and active tab: `SplitPane.svelte:51`, `TerminalTabs.svelte:138`, `commands/workspace.rs:390-441`; tab cycling, Cmd+1-9, Cmd+W/D, tab counts, Quick Open | `isStripHidden` | Excluded |
| Task assignment and workspace tab ids: `claudeCode.svelte.ts:2025-2027` | `isServiceTab` | An agent; owns its task |
| Loom Focus and Weave: `FocusView.svelte:77,95`, `LoomView.svelte:35` | `isServiceTab` | Listed, grouped under its Hive workspace, so a waiting prompt is visible from the Loom too. Opening one goes through the Hive branch below |
| Mesh: `agentMesh.svelte.ts:218,273,731`, `agentRequests.svelte.ts:65`, `server.rs:973` | `isServiceTab`, plus a Hive check | Shown by `getMeshLayout` as a Hive member, but **never a mesh member and never bridged**: `setMesh`, `bridgeWith` and a mesh move are refused for a `hive_role` tab. Workers talk to the coordinator, not to other agents (the conservative choice) |
| Browser link: `browser_link/ws.rs:314` | `isServiceTab` | Linkable. Only the human links, so nothing new is opened to an agent |
| Liveness: `liveness.ts:21` | `isServiceTab` | Counted as an agent |
| Account-switch reload: `+layout.svelte:465` | `isServiceTab`, plus a Hive check | **Skipped.** Reloading a worker would relaunch it without its Hive launch (§11.1) |
| Share export and relocate | `isServiceTab` | Share excludes Hive workspaces entirely (§11.2) |

Step 2 of the build order (§16) is behaviour-neutral **for service tabs** only: every service
keeps exactly today's behaviour under whichever predicate its call site moves to.

**Opening a Hive tab.** `navigateToTab` (toasts, OS notifications, Overlord chips, the Loom,
`switchTab`) and `waiting.openOnTab` redirect a service tab to the console drawer
(`workspaces.svelte.ts:2042,3048-3055`, `waiting.svelte.ts:130`); a Hive tab would fall
through to `setActiveTab` on a hidden tab. They branch on the kind instead: a `hive_role` tab
opens the Hive view, opens or focuses that agent's column (the coordinator column for the
coordinator, read-only for a retired worker) and scrolls to its waiting prompt. They never call
`setActiveTab` on it. This ships in the same build step that first creates `hive_role` tabs.

`Tab.hive_role` is a **string** (`"coordinator"` | `"worker"`), never a new serde enum variant.
An older build that meets an unknown variant fails to parse the whole state file and starts
empty (the serde-enum-downgrade memory, and `docs/loom.md` "Blocked with no reason recorded").
The same goes for every Hive kind field (the `Hive` record's in §3.2, and asset kind and asset
status in §10).

## 6. Folders and worktrees

### 6.1 The Hive's folders

| Folder | How agents get it |
|---|---|
| **Git repo** | Each worker that needs it gets its own worktree of it (§6.4). The coordinator gets a read copy (a detached worktree). Changes reach the repo only by landing (§8) |
| **Plain folder** (footage, brand assets, docs: no git) | Shared **read-only**: it's on no agent's write allowlist (§6.7). There's no way to give each worker its own copy of an arbitrary folder, so **no agent writes into it directly**. Results go to the gallery, and from there maiTerm **delivers** them into a folder deliberately, new files only (§10.6, §7.3) |

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
  tasks/<task-id>/            launch record, settings file, session list (§11.1), owned by Rust
  precedents/                 the Hive's precedents (§7.5), owned by Rust
  logs/                       land log (§8.6), decision log (§7.6), run log (§7.7), memory
                              provenance (§9); owned by Rust, on no agent's write allowlist
  coordinator/
    home/                     the coordinator's working directory (empty but for its own files)
  wt.noindex/
    coordinator/<folder>/     the coordinator's detached read copy per git folder
    <worker-slug>/
      <folder>/               the worker's worktree of each git folder it needs
      scratch/                the worker's own working space: renders, downloads, drafts
      claude-tmp/             the worker's CLAUDE_CODE_TMPDIR (§6.7)
      cargo-target/           the worker's CARGO_TARGET_DIR, with its slim seed (§6.5)
    land/<id>/                temporary worktrees for land and post-land checks (§8.1, §8.6)
```

It lives **outside every repo**, under `app_data_slug()` (never a hard-coded `com.aiterm.app`).
Nested inside a repo, every watcher would scan it: Vite, tsc, cargo, eslint, the Files column,
Claude's own Grep in the main checkout. `coordinator` and `land` are reserved worker slugs, and
`scratch`, `claude-tmp` and `cargo-target` can't be folder names (they sit beside a worker's worktrees).

**Every worktree lives under `wt.noindex/`.** Spotlight skips a folder whose name ends in
`.noindex`; without it, Spotlight indexed a fresh worktree at about 100% CPU (spike 4, §13.1).

### 6.3 What an agent's folders look like to it

Claude has one working directory plus `--add-dir` for the rest. So:

- **The working directory** is the worktree of the task's first folder; for a task with no git
  folder, it's the worker's `scratch/`. The coordinator's is `coordinator/home/`, never the
  Hive root: from the root, its Grep and Glob would walk every worker's worktree, every `scratch/`
  and the gallery. Its read copies and the plain folders are `--add-dir`.
- **Everything else is `--add-dir`**: the worker's other worktrees, the plain folders, and
  `scratch/`. They're launched with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`, because
  by default Claude doesn't load an added directory's `CLAUDE.md`, and each repo's rules
  have to reach the worker that edits it.
- **The gallery is `--add-dir` on every launch** (`<hive>/gallery`), for every Hive agent. In
  auto mode a read outside the working directories didn't prompt (spike 14, §13.1), but a worker
  in manual or acceptEdits mode gets the outside-read prompt, which offers "block reads… from now
  on" and "keep allowing", both of which write the user's settings. The `--add-dir` keeps that
  prompt from ever appearing. Writes there are denied (the sandbox's `denyWrite` plus
  `Write(//…)`/`Edit(//…)` deny rules, §6.7): the gallery is Rust's, and assets enter it only
  through the gallery tools (§10.3).
- The brief names every folder with its role: "web (git, your worktree, lands to main)",
  "brand (plain, read-only)", "scratch (yours: put renders here, then add them to the gallery)".
  For a secondary repo it says to commit with `git -C <worktree> …`: `cd <worktree> && git …` is
  refused by Claude's own check on changing directory before a version-control command (spike 6,
  §13.1).

### 6.4 Worktrees

- **maiTerm creates them**, with `git worktree add`, rather than Claude's `--worktree`. It's
  runtime-agnostic (Codex workers later), the paths and branch names are ours, and so is
  cleanup. Claude's `--worktree` also can't branch off an arbitrary branch (`worktree.baseRef`
  is only `fresh` or `head`).
- **Branches**: `hive/<hive-slug>--<worker-slug>` in each repo, branched off that repo's
  **landing target's current tip** (Base by default, `hive/<hive-slug>` in the approval mode).
  Not `hive/<hive-slug>/<worker-slug>`: in the approval mode `hive/<hive-slug>` is itself a
  branch, and git refuses a ref under another ref's name (measured, git 2.54). A slug never
  holds `--`, so the name splits one way (`worktree::worker_branch`).
  A wave-2 worker therefore starts with wave 1's landed work.
- **Setup**: each repo's setup command runs in the new worktree, in the worker's own tab before
  its agent starts, so its output is in the raw terminal if it fails. It runs under the worker's
  sandbox profile (§6.7, §6.8), like every job on a worker branch. Gitignored files the work
  needs (`.env`) are copied in from a `.worktreeinclude`-style list per repo, with Claude's
  semantics: only files that are gitignored, never tracked ones. As built, a line is a path from
  the repo's top with `*`/`?` in its last part only. Unlike gitignore, a line without a slash
  (`.env`) names the top-level file only, never one deeper; when nothing is there the line is
  logged, so a monorepo writes `apps/web/.env`. `**`, `!` and folders are skipped and logged, a
  path git won't answer for (inside a submodule) is logged and not copied rather than failing
  the spawn, and nothing is read or written through a symlink. A Hive folder's name can't be a
  worker's own folder's (`scratch`, `claude-tmp`, `cargo-target`) or `coordinator` in any case:
  on macOS's default volume `Scratch` is `scratch`, and git checks out into an empty folder.

### 6.5 Build caches are the hidden cost

A fresh worktree has no `node_modules` and no `target/`. Sharing one `CARGO_TARGET_DIR`
serializes every build on cargo's lock. The setup command is the place for this, plus two
defaults the sheet offers when it detects them:

- **Rust: a per-worker `CARGO_TARGET_DIR` with a slim seed.** maiTerm copies, with `cp -c` (an
  APFS clone), only `target/debug/.fingerprint/`, `target/debug/build/` and
  `target/debug/deps/*.{rmeta,rlib,dylib,d}`, ideally from a check-only target dir. Never a clone
  of the whole `target/`: this repo's is 330 GB in about 790k files, and the clone (279 s) plus
  its first check (101 s) is worse than a cold build (about 50 s and 250 CPU-seconds). The slim
  seed copies in 40 s and its first check takes 24 s, with the CPU spent only on the crate
  itself, which rebuilds because its fingerprint includes the worktree path (spike 4, §13.1).
- **npm: `npm ci` with the shared npm cache.** About 2 s; its cost is negligible.

The shared caches sit outside the worker's folders (`~/.cargo` registry and git dirs, the npm
cache, sccache's dir), and a sandboxed write outside the working folder is refused by default
(§6.7), so the sandbox's write allowlist names them explicitly, together with the per-worker
target dir. Without that, a sandboxed `npm ci` or `cargo build` fails on its first cache write.

### 6.6 Trust

Claude raises "Do you trust this folder?" for a folder it has no trust entry for, and nothing
automated may confirm that dialog (`trust_dialog_open`, root CLAUDE.md). How Claude decides,
as measured on 2.1.309 (spike 2, §13.1):

- **Trust is read only from the launching root's `.claude.json`**
  (`$CLAUDE_CONFIG_DIR/.claude.json` under a managed account). An entry only in `~/.claude.json`
  doesn't count for a managed account.
- **Entries are keyed by realpath.** A key spelled through a symlink doesn't count, so maiTerm
  canonicalizes a path before writing it.
- **Trust on a main checkout covers all its linked worktrees.** Claude maps a worktree to its
  main repo. The reverse doesn't hold, and a worktree session files its project data under the
  main repo's key, creating that key itself (with trust false) if it's missing.

So **a worktree of a repo the human already trusts needs no trust write at all**, and that is
the usual case: the coordinator's read copies, every worker worktree and every temporary land
worktree come up without the dialog. For a repo the human hasn't trusted, the entry that would
count is the main repo's, which also trusts the human's own checkout; the Create sheet says the
repo isn't trusted, and trusting it is the human's act, not maiTerm's (not measured beyond the
mapping above; the conservative choice).

**maiTerm writes trust only for Hive folders that aren't worktrees**: the coordinator's `home/`
and a scratch-only worker's `scratch/`, which it creates itself. The human chose the Hive by
creating it, so **maiTerm itself** (not an agent, which the permission classifier rightly refuses
as "Security Weaken") writes `projects["<realpath>"].hasTrustDialogAccepted = true` for them.

- **Trust doesn't keep a worker's branch out.** Since every worktree inherits its repo's trust,
  an unlanded, worker-authored branch is trusted too, a reviewer's included. Trust lets a
  folder's project hooks, `.mcp.json` and settings run unseen, outside the Bash sandbox, so the
  safeguards are elsewhere: no worker can write `.claude/**` or `.mcp.json` (the mandatory
  Write/Edit deny rules and `denyWrite` of §6.7), and **reviewers launch with
  `--setting-sources user`**, so the reviewed branch's project hooks, settings and MCP servers
  never load (§7.7). A reviewer whose launch would raise the trust dialog is a launch failure,
  never a dialog anything answers.
- **Every account root that needs one.** A respawn after an account switch, or a resume, can
  come up under a different account than the one the folder was made under, and the seed-once
  merge in `accounts/mod.rs` carries only `mcpServers`, never a later trust write. So an entry
  maiTerm writes goes into the `.claude.json` of **every** root `Homes::real()` covers
  (`accounts/`, per-account roots), through relocate's change-detecting `update_file`
  (`relocate/agents.rs:118-147`) lifted into a shared helper. Claude 2.1.309 re-reads and merges
  rather than writing back a stale copy (the entry survived a storm of six sessions); the helper
  covers the small unlocked race that remains.
- **Re-verified before every launch** in a Hive folder, under the account that launch will use.
  A missing entry is re-written before the agent starts, never discovered as a dialog on screen.

**Removal** happens when the folder is removed, and deletes only the keys maiTerm wrote, never
Claude's own entry for a repo (which a worktree session creates itself). A respawn after an
account switch wasn't exercised in spike 2; the build step's acceptance test covers it.

### 6.7 Isolation: what the sandbox contains, and what it doesn't

A worker launched in a `git worktree add` directory doesn't get Claude's worktree isolation
checks (those apply to `--worktree`/`EnterWorktree` sessions). What keeps a worker in its folders
is **Claude's sandbox**, and that sandbox covers less than its name suggests. This section says
exactly what it holds, so nothing else in the design leans on a wall that isn't there.

**What maiTerm launches every worker with** (through the launch's `--settings`, so it survives
resume and relaunch, §11.1):

```json
"sandbox": {
  "enabled": true,
  "failIfUnavailable": true,
  "allowUnsandboxedCommands": false,
  "filesystem": { "allowWrite": [...], "denyWrite": [...] },
  "network": { "allowedDomains": [...], "strictAllowlist": true }
}
```

- `failIfUnavailable: true`: without it, a machine where the sandbox can't start runs every
  command **unsandboxed, silently**. With it, the worker fails to start and its card says why.
  (Its failure path couldn't be forced on macOS, spike 10, §13.1.)
- `allowUnsandboxedCommands: false`: by default a blocked command is retried with
  `dangerouslyDisableSandbox`, which turns every block into an escape.
- `strictAllowlist: true`: without it, auto mode widened `allowed_domains` per call and got
  through (spike 10, §13.1). With it, a host off the list gets a 403 and a `<sandbox_violations>`
  note, with no retry and no prompt.

**What it contains: shell commands only** (Bash, PowerShell, Monitor). For those, writes are
limited to the working folder plus `allowWrite`, minus `denyWrite`, and network to
`allowedDomains`. A write outside the working folder is refused by default, `$HOME` included
(spike 10 re-run, §13.1), so `allowWrite` names only what lies outside it and must be writable.

**What it does not contain**, and what bounds each instead:

| Outside the sandbox | Bounded by |
|---|---|
| **Write and Edit** tools | **The Write tool ignores the sandbox**: in auto mode it wrote `.claude/settings.json` and another repo's `.git/config` with no prompt (spike 10, §13.1). So `Write`/`Edit` deny rules in the launch settings are **mandatory**, for `.claude/**`, `.mcp.json` and every repo's `.git/{config,hooks/**,info/**}`, the gallery and the other `denyWrite` paths below. They must use the `//` form for an absolute path, `Write(//abs/**)`: `Edit(/abs/**)` matches nothing (spike 14). The worker's PreToolUse command hook (§6.8) also refuses a Write/Edit whose real path resolves outside its folders, since a deny rule is a glob and the hook reads the path |
| **WebFetch** | **It escapes the allowlist**: in auto mode it fetched an unlisted host despite an allow rule naming one domain, and `deny: ["WebFetch(domain:*)"]` also cuts the sandbox's own network (spike 10). So WebFetch is **removed** from a worker whose role doesn't need it (`--disallowedTools`). One that needs it gets a PreToolUse command hook on WebFetch that refuses any host not on its `allowedDomains` |
| **MCP servers** (generators, maiTerm's MCP proxy) | They run in maiTerm's or their own process. Bounded by the budget (§10.3), by what the proxy serves, and by `--strict-mcp-config` with only the servers the worker needs (§6.8). The proxy is **not** on the shell allowlist: an agent reaches it as an MCP server, never by `curl` |
| **Hooks** (project and user) | Not sandboxed at all. Every worktree inherits its repo's trust (§6.6), so the bounds are that no worker can write `.claude/**` or `.mcp.json` (the deny rules above plus `denyWrite`) and reviewers run with `--setting-sources user`. maiTerm's own hooks rely on running outside the sandbox: they are `type:"command"` (§6.8, §11.1) |
| **Commands maiTerm runs for a worker** (`hiveRun` jobs, setup, land checks, services on its branch) | These run in maiTerm's PTYs, not Claude's. maiTerm applies the **same sandbox profile** at spawn (§6.8), so nothing the worker can name runs with more than the worker has |

**Edit deny rules alone don't hold a path.** With `Write`/`Edit` denied on a folder, Bash `cp`
into it was refused but `python3 -c "open(…,'w')"` got through (spike 14, §13.1). So every path
denied to Write/Edit is also on the sandbox's `denyWrite`, and the reverse: each layer covers
the other's gap.

Media CLIs (ffmpeg, ImageMagick, sox, Blender) stay sandboxed like any command: they run inside
the worker's folders because `allowWrite` names them, not because they're exempt. `excludedCommands`
is never used for them, since an excluded command gets full disk and network, not "unrestricted
inside its folders".

**The write allowlist** is derived per worker in Rust, never typed:

- `allowWrite`: the worker's worktrees other than its working folder, its `scratch/`; for each
  worktree, the main repo's git entries below; the cargo registry and git dirs, the npm cache,
  sccache's dir and the per-worker target dir (§6.5).
- **A linked worktree's git entries.** A Hive worktree is a linked worktree of the human's repo,
  whose `.git` isn't otherwise writable. For worker branch `B` in worktree `N`, a commit needs
  exactly (spike 10 re-run, §13.1):
  - `allowWrite`: `<main>/.git/worktrees/N`, `<main>/.git/objects`, `<main>/.git/refs/heads/B`,
    `<main>/.git/refs/heads/B.lock` and `<main>/.git/logs/refs/heads/B`;
  - `denyWrite`: `<main>/.git/{hooks,config,info}` and
    `<main>/.git/worktrees/N/{commondir,gitdir,config.worktree}`.

  The branch entries are **per file, never the `refs/heads` folder**, so a worker can't move
  Base or another worker's branch. `packed-refs` is left out: the commit still succeeds, with a
  harmless error. `objects/` has to be writable, so a worker writes into the shared main repo's
  object store: a residual risk to that store, not a boundary. maiTerm
  creates the branch before launch, since a slashed name needs its parent folder. The same set
  is written for the primary worktree as for secondary ones: Claude protects the working-folder
  repo's `.git/hooks` and `config` itself, but not an `--add-dir` repo's, where Bash wrote a hook
  and the Write tool overwrote `.git/config` (spike 10).
- `denyWrite`, explicitly, on top of those: every repo's `.git/{config,hooks,info}`, other
  worktrees' admin dirs, `.claude/**` and `.mcp.json` in every worktree, the gallery (§6.3), the
  hook scripts of §6.8 and any local-path git remote (§7.3). Each also has its
  `Write(//…)`/`Edit(//…)` deny rule (above).
- **The human's checkouts** get `Write(//…)`/`Edit(//…)` deny rules. In the sandbox they are
  refused by default (outside every working folder) and aren't put on `denyWrite` whole, since
  that would also cover the `.git` entries a worker's commit needs. **Nor can the rule cover the
  whole checkout** (measured, step 4b's probe): Claude folds an `Edit` deny rule into the
  sandbox's write denials, and `Edit(//<checkout>/**)` refused the commit's
  `.git/worktrees/<N>/index.lock`. So a checkout the worker has a worktree of is denied entry by
  entry, everything at its top but `.git` (`settings::checkout_denies`); an entry made after the
  launch is still refused by the sandbox and by the gate, which reads the real path. A checkout
  it has no worktree of is denied whole.
- maiTerm's own git calls on worker content pass `-c core.fsmonitor=false -c
  core.hooksPath=/dev/null`, so a hook or fsmonitor a worker planted never runs as maiTerm.

**Temp is shared, and is not a boundary.** Sandboxed Bash always sees
`TMPDIR=/tmp/claude-<uid>`, whatever maiTerm sets, and that folder is writable by **every**
sandboxed session of the same user: every worker of every Hive and the human's own sessions. It
can't be denied either: Claude writes its cwd file there, and denying it breaks every Bash call
(spike 10 re-run, §13.1). So:

- worktrees, `scratch/` and anything a result depends on **never live under it**;
- maiTerm sets `CLAUDE_CODE_TMPDIR` per worker (`wt.noindex/<worker>/claude-tmp/`, §6.2), which
  moves Claude's own state (tasks, sockets) out of the shared folder;
- briefs say `mktemp -p "$TMPDIR"` (plain `mktemp` fails in the sandbox) and that anything in
  temp is visible to, and changeable by, every other worker.

**Localhost** is closed by default (measured, spike 10). Two settings open it, and both open
**every** port, maiTerm's 8765 and the MCP proxy included: `allowLocalBinding: true`, and a bare
`127.0.0.1` in `allowedDomains` (spike 9). So a worker reaches local services through MCP or
`hiveRun` jobs (§6.8), and maiTerm's own hooks don't need localhost (they are command hooks,
§6.8). If a Hive ever needs a local port, it's opened **by port only**, `127.0.0.1:<port>`, as an
explicit per-Hive widening in Hive settings, never for maiTerm's MCP, proxy or maiLink port.
Bash can reach that port through the sandbox's proxy, so the service there must not trust a
local caller.

**A block is a blocker, not a retry.** The detector is a **`PostToolUseFailure` command hook**
on the worker: it parses the `<sandbox_violations>` note in the failed call's output and turns
it into a worker blocker naming the host or path. PostToolUse doesn't fire for the failure, and
http hooks don't arrive from a sandboxed tab (spike 10, §13.1). The blocker goes to the
coordinator, which can't widen the sandbox and raises its own blocker to the human (§7.4); the
fix is in Hive settings (§14.16).

**Native Windows** has no Claude sandbox. With `failIfUnavailable`, a Hive worker can't start
there; the Create sheet says so up front rather than offering an unsandboxed Hive (the
conservative choice; WSL is a later option).

The brief still tells workers to stay in their folders, and the coordinator's tools only ever
name their worktrees. The brief is the explanation; the table above is the enforcement.

### 6.8 Running things: one shared stack and a run queue (Darryl's direction, 2026-10-23)

One machine can't run a dev server, a database and a full test suite per worker. Isolating
each worker (its own ports, its own container) multiplies exactly that load. So a Hive
**shares what can be shared and schedules what can't**. maiTerm runs the schedule; the
coordinator only sets priorities.

**What decides it: does the thing run the worker's code?**

| Kind | Examples | How a Hive runs it |
|---|---|---|
| **Infrastructure**: doesn't run any branch's code | Postgres, Redis, MinIO, an emulator, ComfyUI, a local model | **One instance** in the Hive's stack (`docs/stack.md`), used by every worker. Each worker gets its **own namespace** inside it: database `app_<worker>`, a key prefix, a bucket prefix, through env vars maiTerm sets from a template (`DATABASE_URL=…/app_{worker}`). When the human already runs it in another workspace, the Hive's service is a `Service.shared_from {workspace_id, service_id}` reference (§3): its port fills the templates, and the Hive never starts, stops, retargets or leases it |
| **Code-bearing**: runs one branch's code | A dev server, an API built from source, `tauri:dev`, an e2e suite, a full `cargo test` | **The same stack service, relaunched on the branch it's needed for** (§6.9). One that can take a port runs several copies at once, one per branch; one that can't is taken in turns |
| **Cheap and local** | Typechecking a file, one module's unit tests, lint | **Runs freely** in the worker's own tab, no queue |

**The run queue (Rust).** A worker runs anything heavy through `hiveRun {command, folder,
timeout?}`. `hiveRun` is worker-only, and it is **not a free command string**:

- **It must match a queued command.** Per repo, the Hive lists the commands that are queued
  (`cargo test`, `npm run tauri:dev`, `playwright test`…), set by the human in Hive settings,
  each with the resources it `needs`. `command` must start with one of those entries as a
  prefix. Further arguments are allowed; shell metacharacters (`;`, `|`, `&`, `$`, backticks,
  redirections, newlines) are refused. maiTerm splits it into argv and execs it, never through
  `sh -c`. `needs` comes from the matched entry, never from the worker.
- **It runs under the worker's sandbox profile.** Every job maiTerm runs on a worker's branch
  (`hiveRun`, setup, land checks, post-land checks, a code-bearing service retargeted onto the
  branch) runs worker-authored code: `package.json` scripts, `build.rs`, tests. Rust wraps the
  job's spawn in the same sandbox profile the worker's own commands get (§6.7: writes to its
  folders and caches, network to its allowlist). A job is never a way to do what the worker
  itself may not. Spike 15 (§13) proves that no maiTerm tool a worker can call reaches
  unsandboxed execution.
- **Resources.** Hive settings declare, per repo, what jobs need: `cargo` (CPU-heavy, 2 at a
  time), `tauri-dev` (one at a time: its ports and data folder), `e2e` (one: a browser and a
  port), `gpu` (one: ComfyUI), `migrate:<db>` (exclusive per namespace). A job starts when
  everything it needs has room. Jobs that don't compete run in parallel; jobs that do, wait.
- **A job is a stack service run on the worker's branch** (§6.9): a test suite is a service
  whose command exits, an e2e run is one that needs the dev server on the same branch first.
  Its output is visible in the worker's column, and its exit code is maiTerm's own record
  (OSC 133 D). That's what makes it a receipt (§7.7).
- **The worker isn't held hostage.** `hiveRun` answers at once with the job's id and place in
  the queue. The result arrives as a framed `⟦HIVE RUN⟧` message between turns: the follow-ups
  delivery path (`docs/follow-ups.md`), take-before-type, held only for a dialog or a draft. A
  short job that finishes within a few seconds answers in the tool result directly.
- **The coordinator sets priority, not timing.** `hiveRunPriority` (coordinator-only) moves a
  job forward (the run blocking a land, say). Deciding when each test runs turn by turn would be
  slow and wrong for an LLM, and it's easy for a queue. That's principle 8 again.
- **Services stay warm.** A service left running on a branch is reused when the next job wants
  that branch, and relaunched otherwise. Separately, one copy of the dev server can stay up on
  **main** (the coordinator's read copy), so the human always has main to look at. That copy
  reuses the human's own running service for the repo on Base when there is one (§3).

**A job is not a service.** Services and jobs share the stack's tabs and start line, but not its
crash semantics: a non-zero exit is `crashed`, the default `on_crash` re-runs it up to five
times while it holds its resources, the Overlord's `service_crashed` rule fires, `was_running`
relaunches it after a restart, and a clean exit reads as `stopped`, the same as a human stop
(`docs/stack.md`). So a stack entry gets `kind: "service" | "job"`, a **String** (§3.2). A job:

- never restarts and never sets `was_running`;
- is kept out of the stack priming line, the rollup and Overlord rules;
- records `{exit_code, started_at, ended_at, stopped_by}`, and that record is the receipt;
- requires shell integration on its PTY. A job with no OSC 133 D has no exit code, which makes
  it an invalid run, never a pass.

**Who owns the queue.** Rust schedules: it holds the queue, the resources and the leases.
Starting and stopping a stack tab today lives in the window's store (`stack.svelte.ts`), which
the webview throttles when occluded. So a start or stop is an rpc to that window's store,
answered `{accepted, confirmed}` (`mailink/rpc.rs`'s shape). Until it's confirmed the job reads
"waiting on window", and its timeout starts only on confirm. Completion is recorded in Rust,
from the OSC 133 D it already parses on the job's PTY, not reported back by the webview. A job
command without shell integration is refused at `hiveRun`. (Moving the stack lifecycle into Rust
would remove the rpc; it's the alternative if the rpc proves fragile, not a v1 prerequisite.)

**Leases.** A service a running job depends on (the dev server under an e2e run, a one-copy
service on that branch) is **leased** to the job until it ends. Rust checks the lease on every
path that retargets or stops a service: an agent's `restartService {service, on}`, the human's
Run on ▸ (§6.9), warm reuse by another job, and `stopService`.

- An agent's retarget of a leased service is queued behind the lease, never applied mid-run.
- The human is never queued without being told: Run on ▸ or Stop on a leased service offers
  **Run when free** or **Stop anyway**. Stop anyway marks the job **interrupted** (never a
  receipt, never a pass) and tells its owning worker by `⟦HIVE RUN⟧`.
- Stack verbs from a worker tab on a queue-managed service go through the queue too.
- Worker tabs can't create, update or remove services, and can't `startStack`/`stopStack`. At
  most they start or restart a **human-defined** service **on their own branch**, through the
  queue and under their sandbox profile.

**Queue state across stop, suspend, reload and restart.** Jobs are keyed by **task**, not tab
id, since a reload mints a new tab id.

- A pending `⟦HIVE RUN⟧` result is a Rust-authoritative delivery stored like a follow-up
  (`Tab.follow_ups` semantics: moved on reload, take-before-type), so it survives a reload and
  reaches the worker exactly once.
- Resources and leases are released when a worker stops, suspends, crashes or retires, or its
  task is dropped. A suspended worker's job never holds `tauri-dev`. On resume the worker is told
  its run was released.
- The queue itself is in Rust memory (§3.2). After a maiTerm restart it starts empty, and each
  worker that had a job queued or running is told "your run was interrupted; run it again", so
  none waits forever on a result that will never come. Activity entries for those jobs are
  cleared.

**The deny is a speed bump and an audit log, not a guarantee.** A worker's shell call that runs
a queued command directly is **denied by its PreToolUse hook**, with the reason "run this
through hiveRun", and every refusal is written to the run log. Measured (spike 9, §13.1):

- **A cooperative model stops**, and the hook catches the obvious wrappers: `sh -c`, `bash -lc`,
  `env X=…`, aliases, full paths. Sonnet refused to route around it.
- **It isn't a wall.** These ran past it: a split string (`c=car; ${c}go test`), `base64 -d | sh`,
  a subcommand alias (`cargo t`), a script written with the Write tool and run as `./t.sh` (the
  hook sees only that), and `git send-pack`. No pattern match on a command line closes that list.

So nothing in the design relies on the deny. **What limits a worker that goes around it is the
sandbox and the queue's resources**: a bypassed `cargo test` still runs only inside the worker's
sandbox (§6.7: its folders, its network allowlist, no local ports), and the shared things the
queue schedules (the dev server's ports and data folder, the GPU, a namespace's migrations) are
maiTerm's services, which a worker can't bind, start or retarget except through the queue. A
bypass costs the machine an unscheduled run, which the memory-pressure ceiling below still
bounds, and it's on the record. Everything that isn't queued runs freely, inside the sandbox.

**The hook is a `type:"command"` hook, and fails closed.** maiTerm's global PreToolUse is a plain
http hook (`lockfile.rs:540`), and a connection failure, a timeout (600 s by default) or a non-2xx
lets the call through. Worse, **the sandbox breaks localhost http hooks**: in a sandboxed tab
every `type:"http"` hook to 127.0.0.1 fails, so with `onFailure:"block"` every Bash call is
blocked, and PostToolUse and Stop never arrive (spike 9). Hooks themselves run outside the
sandbox, so a command hook reaches maiTerm with no allowlist entry. So every Hive agent gets its
**own** hook through the launch's `--settings` (on a worker it denies queued commands; on every
Hive tab it carries §7.3's push refusal), leaving the global hook alone:

- a **maiTerm-owned script**, at a path outside every agent's writable paths and on every
  `denyWrite` (§6.7), that forwards the event to maiTerm and **exits 2 on any failure** (maiTerm
  down, slow or answering non-2xx);
- `onFailure: "block"`, a short timeout, matcher `Bash|PowerShell` (plus `Monitor` once proven);
  the same hook carries the Write/Edit path check (§6.7);
- the script finds maiTerm's current port on every call, so a restart or a port walk can't
  strand it;
- workers are refused on a Claude older than 2.1.295, where `onFailure` doesn't exist.

With `onFailure:"block"`, a closed port, a timeout and an HTTP 500 all blocked the call; without
it, the call ran (spike 9). Every other hook a Hive tab needs is a command hook too (§11.1).

**Land checks can be batched** (a merge queue, like GitHub's or bors). Land checks are the
heaviest runs, and they're one at a time per repo. A batch is made only of workers **already
passed to `hiveLand`** (§8.1), never of workers that are merely Reported: review happens first.

- maiTerm builds an **integration chain** in a temporary worktree: Base, then each member's
  pinned commit merged `--no-ff`, in order. It runs the land check **once**, on that chain.
- Pass: Base is **fast-forwarded to the tested commit**, under the land lock. §8.1's per-worker
  step 1 (merging Base into the worker) is skipped for members, and a member counts as landed
  when its merge commit is an ancestor of the new Base. One land-log entry is written per worker,
  naming its merge commit, so Undo is a `git revert -m 1` of that commit (§8.6).
- Fail: the batch is split in half and each half tried again, until the culprit is found.
- The post-land check (§8.6) follows the same rule as for a single land: since Base
  fast-forwards to the tested chain, it runs only if the landed tree differs from the tested
  one, and then once per batch.
- If Base moves before the fast-forward, the batch is rebuilt.
- A worker that touches several repos batches only if every repo batches. The approval mode
  (§8.3) never batches: each land is its own card.

When everything passes, N workers cost one suite instead of N. Batching comes after per-worker
landing works (§16), not before it.

**The machine sets the ceiling.** The agents themselves aren't free. A Claude session plus its
MCP-server children (a median of seven) is a median 0.9 GB, p90 1.1 GB, and about 2 GB for a
long session; idle, it uses about 0% CPU, so the cost is what it runs (spike 8, §13.1). So:

- **The default worker cap is 4.** The sheet's ceiling scales at about **one worker per 16 GB
  of RAM above a 32 GB reserve**, and never more than **ncpu/4**:
  `max(1, min((RAM−32)/16, ncpu/4))`, so 5 on a 128 GB, 20-CPU Mac and 1 at 32 GB. A cap set
  above it needs the human's confirmation in the sheet (`create::cap_ceiling`).
- **Workers launch with `--strict-mcp-config`** and only the MCP servers they need (maiTerm's,
  and the generators the role uses), since the MCP children are half a session's memory.
- **Pressure** is read from `kern.memorystatus_vm_pressure_level` and the free-memory
  percentage: at level 2 or more, or under 25% free, Rust starts nothing new and lowers the
  effective worker cap; at level 4, or under 10% free, the heavy resources drop to one each. The
  header says "Throttled · memory pressure".
- **CPU is read as idle %, never load average.** With 69 sessions running, load average was 80
  while the CPU was 67% idle. `cargo`'s capacity of 2 stands.

**Shared caches** cut the remaining cost: sccache across worktrees for Rust (each worktree's
`target/` lock remains, but compiled crates are shared), the npm cache, and the slim APFS-clone
seed of §6.5.

**maiTerm as its own pilot repo** needs one change first: a `tauri:dev` instance must accept a
data-folder and port override. Today two instances share `com.aiterm.dev` and ports, including
8765, where the installed maiTerm listens. With the override, `tauri-dev` is still a one-at-a-time
resource, since even one extra maiTerm is heavy.

**Containers stay a later, per-Hive option.** Apple's `container` (1.0, June 2026) gives each
container its own IP on macOS 26, so ports can't collide. It suits Linux-shaped stacks where
isolation is worth the memory. It can't run Mac apps or reach the Apple GPU, and an agent inside
one is a remote agent. So it arrives with SSH Hives, as one piece of work (§12).

### 6.9 Any service, on any branch (a stack feature, 2026-10-23)

Darryl's ask: relaunching a service on another branch should be simple. It is, because the stack
already types `cd <cwd> && <command>` on every start (`startLine`, `src/lib/stack/model.ts`). Running a
service on a branch is that same line with the folder moved onto the branch's worktree, then a
restart. **This is built into the stack itself, not into Hive**, so every workspace gets it and a
Hive simply uses it.

**What a service gains:**

- **Copies, keyed by `(service_id, target)`.** A `target` says which checkout a copy runs: empty
  means the service's own folder, as today; otherwise it names a worktree (a Hive worker's, or
  one maiTerm made for a branch). The target lives on the **tab** that runs the copy, as
  `Tab.service_target` beside `Tab.service_id`: carried on reload, cleared on duplicate (like
  `service_id`), never shared between tabs. Today one tab per service is the invariant
  (`set_tab_service_id` clears the service from other tabs, `commands/workspace.rs:1562-1571`;
  `boundTab` takes the first match; runtime status is keyed by service id). All of those are
  re-keyed by the pair: uniqueness, `boundTab`, runtime status and the Rust mirror. Running
  intent is per copy too (`was_running` per `(service_id, target)`), so a relaunch brings each
  copy back on its own branch, rather than one persisted `target` field on the service.
- **The folder follows the branch.** The service's `cwd` is taken relative to its repo's top level
  and re-rooted in the target worktree: `~/code/app/web` on main becomes
  `<worktree>/web` on `session-store`. The mapping is computed in Rust (paths are Rust's,
  `docs/file-tree.md`), never by string-prefixing in JS.
- **Placeholders** in the command and env, filled in per target: `{port}` (a free port maiTerm
  picks), `{branch}`, `{worker}`, `{checkout}`. `DATABASE_URL=postgres://localhost/app_{worker}` is
  how one shared database gives each branch its own namespace (§6.8).

**One verb, three ways to reach it:**

- **The human:** the service's menu in the sidebar gets **Run on ▸** main, each open worktree, and
  "Other branch…". The row then reads `web · on session-store :5174`.
- **Any agent:** an optional `on: "main" | "<branch>" | "<worker>"` on the existing tools,
  keeping their `service` argument: `startService`, `stopService`, `restartService`,
  `waitForService`, `getServiceOutput` and `updateService`. Without `on`, a tool acts on the
  service's only running copy; with several copies running and no `on`, it's refused with the
  list of copies, never a guess.
- **The Hive's run queue,** which is just a caller of the same verb.

Every one of these checks the copy's lease first (§6.8): a copy a running job depends on is
never retargeted or stopped out from under it.

A branch with no worktree yet gets one, never by switching the human's own checkout, because
other agents are working in it:

- An **existing checkout of that branch** (a Hive worker's worktree, say) is used as it is.
- Otherwise maiTerm makes a **detached** worktree under the app's data folder, in a `.noindex`
  folder so Spotlight leaves it alone (§6.2), reused next time
  and removed when its branch is gone. Detached means it doesn't move when the branch does, so
  before each start maiTerm runs `git checkout --detach <branch>` there to bring it to the
  branch's tip, refuses (and says why) if that worktree has tracked changes, and the row shows
  the commit it runs: `web · on session-store@3f2a91c :5174`.

**Several branches at once, or in turns:**

- A service whose command or env uses `{port}` can run as **several copies**, one per branch, each
  in its own hidden service tab (`web@main :5173`, `web@session-store :5174`), up to a
  per-service copy limit and the machine's headroom (§6.8). `listStack`, the sidebar and the
  console drawer show **one row per copy**.
- A service that hard-codes its port (or a data folder) runs **one copy at a time**. Retargeting it
  stops the copy that's up and starts it on the new branch (once its lease is free), which takes
  seconds for most dev servers, since the worktree's dependencies are already installed.

The run queue decides **when** and **on which branch**; the stack runs the copies, the job kind
and leases of §6.8 keep a retarget from cutting a run short, and a code-bearing copy on a worker
branch runs under that worker's sandbox profile (§6.7). When this is built, its details move
into `docs/stack.md`, whose §3 (one tab per service) changes with it.

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

**What limits it is enforcement, not the brief.** The coordinator holds every key (spawn, land,
answer a worker's prompt, deliver), and it reads worker text, which §7.4 names as a
prompt-injection channel. So "it doesn't do the work itself" is also made true by its launch.
The settings below are the set that passed spike 13 (§13.1), plus the two additions that spike
named. An earlier draft failed: `--tools` with a pattern in it silently dropped Bash, and
`autoAllowBashIfSandboxed` (default true) ran any sandboxed Bash command regardless of the
allow rules, while auto mode let Write go anywhere.

- **Tools, by name only:** `--tools "Read,Grep,Glob,Bash,Write,Edit"`. `--tools` takes tool names;
  a pattern such as `Bash(git diff:*)` there silently drops the tool. Patterns belong in
  permission rules.
- **`--permission-mode dontAsk`.** It is what makes the allow list the whole list: a tool or
  command not allowed is denied, never asked. AskUserQuestion is always denied under it, which
  fits the coordinator asking only by blocker (§7.4).
- **Sandbox** (§6.7), with its own profile: `autoAllowBashIfSandboxed: false`; `allowWrite` its
  home (also passed as `--add-dir <home>`); `denyWrite` the repos and its read copies; network
  `allowedDomains: []` with `strictAllowlist: true`, since it reaches generators and maiTerm only
  as MCP servers. Its memory writes go through Claude's own exemption for `autoMemoryDirectory`
  (§9; spike 1 saw it in every permission mode it tried, and step 3's acceptance test checks it
  under dontAsk).
- **`permissions.allow`:** `Bash(git diff *)`, `Bash(git log *)`, `Bash(git show *)`; Write and
  Edit on its home (`//` form, §6.7); and the maiTerm tools it uses (hive, gallery, task,
  notes), which dontAsk would otherwise deny. They're listed **by name, never as the server
  wildcard**: MCP calls are made by Claude's own process, outside the sandbox, and
  `createService` plus `startService` alone would type any command into an unsandboxed shell. The
  tools that act outside the sandbox (services, browser, `driveTab`, follow-ups, `createTab`,
  `setPreference`…) are also on `deny` as a backstop (`settings::ESCAPE_MCP_TOOLS`). Its cwd is `home/`, not a repo, so reviews
  either run with a repo as cwd or need `git -C` rules (`Bash(git -C * diff *)` and the same for
  `log` and `show`); the coordinator gets the `git -C` rules, since its home stays its cwd (§6.3).
  Each also needs its bare form, `Bash(git -C * diff)`: a trailing ` *` is optional only in a
  pattern with no other wildcard, so `git -C * diff *` refused a bare `git -C <copy> diff` while
  `git diff *` allows a bare `git diff` (step 3's probe, §13.1).
- **`permissions.deny`:** WebFetch and WebSearch, the push rules of §7.3, and the git options
  that turn a review into a command (`settings::GIT_ESCAPE_DENIES`: `Bash(git * -c *)`,
  `Bash(git -c *)`, `--config-env`, `--exec-path=`, `--git-dir`, `--work-tree`, `--ext-diff`,
  `--textconv`, `--output`, `--no-index`). The `*` in `Bash(git -C * diff)` spans any global
  option, and `git -C <copy> -c core.fsmonitor=<cmd> diff` ran `<cmd>` (the 3e review, §13.1).
- **Git runs only in a repo maiTerm made.** Home is writable, so a `.git/config` (or a bare
  repo's `config`) the coordinator wrote there would run `core.fsmonitor` on its next `git diff`.
  The gate refuses any git whose repo is outside the read copies and Hive folders: from home, a
  relative `-C` after a `cd`, or a `-C` into home. It also refuses `-c`/`--config-env` and git's
  config and command variables (`GIT_CONFIG_*`, `GIT_EXTERNAL_DIFF`, `GIT_DIR`, pagers, editors
  other than `true`). No permission rule can hold this: a rule can't say which folder `-C` names,
  and Claude allows a plain read-only `git diff` with no rule at all. So for the coordinator the
  gate is load-bearing here, and it fails closed.
- The fail-closed command hook of §6.8 (carrying §7.3's push refusal) applies to it as to every
  Hive agent.

**Brief size.** Claude re-sends SessionStart context only up to 10,000 characters; above that it
gets a file path and a 2,000-character preview, and a Hive's Instructions field alone can be
16k. So every brief (the coordinator's and each worker's) is assembled in two parts: the
**invariant rules first**, under 2,000 characters (who its human is, how it asks, what it may
never do), and the rest in a `brief.md` in its home or `scratch/`, which those first lines tell
it to read. The assembled brief has a size cap, and an Instructions field that would exceed it
is refused in the sheet, not truncated.

### 7.2 MCP tools

All are refused unless the calling tab's `hive_role` and Hive match. That's enforced in Rust
from the stated tab identity, like `own_tab_only_refusal`.

**Executed by.** Rust validates every call (identity, Hive, limits) and keeps what must be
atomic. What lives in a window today (task lists, which the task store owns and writes whole,
`docs/tasks.md`; a tab's PTY, which exists only once the webview activates it) is done by that
window's store through an rpc answered `{accepted, confirmed}` (`mailink/rpc.rs`'s shape). A
call whose window isn't loaded is refused, never half-done. Every Hive task write (spawn,
`hiveReport`, `hiveVerdict`) goes through that store, never a list written from Rust.

| Tool | Who | Executed by | Does |
|---|---|---|---|
| `hiveSpawnWorker {title, role, brief, folders?, depends_on?, inputs?, reviews?, model?, effort?}` | coordinator | Rust reserves (§7.8), then window rpc | Creates the task, worktrees, branches and hidden tab, runs setup, and launches the agent with the Hive preamble, its role (§7.7) and the brief. `inputs` names gallery assets the worker starts from (their paths go in its brief). `reviews` makes it the reviewer of another worker's result (§7.7). Refused above the cap or by a fuse (§7.8). A worker with unmet `depends_on` is created in **Starting · waits on …** as a task and card only; its worktrees, setup and agent come at launch, when they land (§3.1) |
| `hiveListWorkers` | coordinator | Rust | Cards as data: state, task, folders, branches, diff stat, assets, last report |
| `hiveMessageWorker {worker, text, assets?}` | coordinator | Rust (`deliver_to_agent`) | The draft, dialog and typing guards; a busy agent is no reason to hold. `assets` attach gallery files as paths |
| `hiveLand {worker, summary, commits: {repo: sha}}` | coordinator | Rust | Lands the worker at the reviewed commits (§8). Answers with what happened in each repo, or why it refused |
| `hiveUndoLand {land}` | coordinator | Rust | Reverts a land (§8.6). Refused in the approval mode (§8.3), where undoing is the human's |
| `hiveStopWorker {worker, reason}` | coordinator | Rust, then window rpc | Stops the agent; keeps worktrees and branches |
| `hiveRestartWorker {worker, fresh?}` | coordinator | Rust (launch builder), then window rpc | Relaunches a crashed or stopped worker in its own tab and worktrees (§11.3) |
| `hiveFinish {report}` | coordinator | Rust | Declares the goal met and stops the Hive; refused while anything is live, queued, held or unlanded (§11.4) |
| `hiveAnswerWorker {worker, prompt_id, answer}` | coordinator | Rust | Answers a worker's on-screen question or permission prompt, within the allowlist of §7.4. Refused while that checkpoint is Ask me |
| `hiveRunPriority {job}` | coordinator | Rust (the queue) | Moves a queued job forward (§6.8). Priority only, never timing or resources |
| `hivePrecedents {category?, query?}` | coordinator | Rust | Reads the precedents that bear on a decision (§7.5) |
| `hiveRecordPrecedent {blocker_id, rule}` | coordinator | Rust | Records a precedent from a blocker **the human answered**; Rust copies the question, options and answer (§7.5) |
| `hiveRun {command, folder, timeout?}` | worker | Rust (the queue), window rpc to start the job | Queues a heavy command on the worker's branch (§6.8): it must match a human-set queued command, and runs under the worker's sandbox profile. Answers with the job id |
| `hiveReport {status: done\|blocked\|failed, summary, receipts?, notes?}` | worker | Rust checks receipts, task store writes | Records the report on the task (status + note), and delivers it to the coordinator as a framed `⟦HIVE REPORT⟧` message (§7.4: worker text is quoted, never a frame). `done` is refused without receipts (§7.7). `blocked` puts the card in **Blocked** (§4.3) |
| `hiveVerdict {worker, verdict: pass\|fail, issues?}` | reviewer worker | Rust, task store writes | A review's result in the fail-report shape (§7.7), delivered to the coordinator and recorded on the reviewed task |
| Gallery tools | all Hive agents; `galleryDeliver` coordinator only, within §7.3's limits | Rust | §10.3 |

Workers keep the ordinary maiTerm tools for their own work (tasks, notes, follow-ups) and their
runtime's own subagents and workflows, with these limits, refused in Rust by `hive_role`:

- **Stack tools** (§6.8): no `createService`, `updateService` or `removeService`, no
  `startStack`/`stopStack`. At most a start or restart of a **human-defined** service **on the
  worker's own branch**, through the run queue, under its sandbox profile, and never of a
  service another job holds a lease on. A free command string never reaches a PTY from a worker.
- **`replyToOverlord`** is withheld: a worker asks only as §7.4's routing table says.
- **The `blocker` field** on a worker's tab is described as asking the **coordinator**, not the
  human (§7.4), unlike every other agent's priming.
- No spawning Hive workers and no messaging other workers.

Projects doesn't let threads talk to each other either, and a star topology keeps the
coordinator the one place the plan lives. If peer talk turns out to be needed, the mesh already
exists.

### 7.3 Authority

| Action | Approval |
|---|---|
| Create a Hive; add folders | The human, by the sheet / settings. No tool can |
| Spawn a worker within the cap | None; the Hive is the envelope (decision §14.1) |
| Raise the cap | The human, in Hive settings |
| Land a worker into Base (default mode) | None: the coordinator calls `hiveLand`, and maiTerm merges behind its guards (§8). A land that changes agent or execution config is the human's (§8.1) |
| Land a worker (approval mode) | The human, on a land card; merging the integration branch into Base is the human's, outside the Hive |
| Deliver assets into a Hive plain folder | None for new files, within the limits below; overwriting anything, and any other destination, is the human's (Export…, §10.6) |
| Push | **No agent.** Only maiTerm's land path pushes, and only when the Hive's Push after land setting is on (default off), never forced (§8.5) |
| Spend past the budget | The human, on a blocker the agent raises first (§10.3). Never delegable (§7.4) |
| A worker's question or permission prompt | The coordinator by default; a checkpoint can send it to the human (§7.4) |
| Undo a land | The coordinator in the default mode (`hiveUndoLand`); the human in the approval mode (§8.6) |

**No Hive agent pushes.** Auto mode allows a push to any branch, the default branch included,
with no prompt, so "Push after land = off" has to be enforced, not trusted. **No deny rule
reliably stops a push**: with `permissions.deny` on `git push`, `sh -c 'git push'`,
`git -C . push` and `git -c x=y push` all reached a local bare remote, and `git send-pack`
passes the hook too (spike 9, §13.1). What stops a push is that **no remote is reachable** from a
Hive agent:

- **Network remotes:** a repo's git remote host is **not** on any agent's network allowlist
  (§14.16), and `strictAllowlist` holds it there (§6.7). A host allowlist can't tell a fetch from
  a push anyway, so maiTerm does any fetch itself.
- **Local-path remotes** (a bare repo on disk, `file://`): maiTerm reads each repo's remotes when
  a folder is added, and every local-path remote goes on every agent's `denyWrite` (§6.7), with
  its `Write(//…)`/`Edit(//…)` deny rules. It's outside the working folder, so a sandboxed write
  there is refused by default as well.
- `hiveLand`, `hiveUndoLand` and every other Hive tool refuse to push; only §8.5 does.

Kept as speed bumps and an audit trail, not as the guarantee:

- `permissions.deny` in every Hive agent's launch `--settings`, in the **space-wildcard form**:
  `Bash(git * push*)`, `Bash(git push*)`, `Bash(git * send-pack*)`, `Bash(git remote*)` and
  `Bash(gh pr merge*)`. A `:*` in mid-pattern matches nothing, and `Bash(git push:*)` and
  `Bash(git -C * push:*)` both missed `git -C <wt> push` (spike 6, §13.1).
- The fail-closed command hook (§6.8) on **every** Hive tab, coordinator included, refuses and
  logs `git push`, `git send-pack` and `git -C <path>` for a path outside the agent's folders.

**`galleryDeliver` limits** (Rust, before any byte is written; Export… is the human's for
everything else):

- `folder` must canonicalize, symlinks resolved, to one of the Hive's **plain** folders or a
  subfolder of one, and the joined target's parent is canonicalized again and re-checked.
- No destination inside a git work tree or a `.git` directory, whatever the folder is called.
- Each of `names` is a single path component: no separator, no `..`, no leading dot.
- Files are created with `O_EXCL`, so a name that appears between check and write is a
  conflict (it gets a suffix, §10.6), never an overwrite.

No agent writes a plain folder directly (§6.1): delivery by maiTerm is the one path in.

**The Overlord engine skips Hive tabs.** The coordinator already supervises its workers, and two
supervisors driving one agent is the double-drive problem `docs/overlord.md` §9.1.2 solved for
escalations. But a Hive is **not** made `overlord_exempt`: that flag switches off far more than
the engine. The engine gets its own skip, by `is_hive` / `hive_role`, covering rules, rituals,
`driveTab` and escalations, and nothing else.

| Gated today on `overlord_exempt` | Evidence | In a Hive |
|---|---|---|
| Overlord rules, rituals, `driveTab`, escalations | `docs/overlord.md` | **Skipped**, by the engine's own `is_hive` check |
| Stack tools (`resolveStackScope`: "exempt from agent tooling") | `claudeCode.svelte.ts:1022` | **Kept**; workers within §7.2's limits |
| Watch scripts (`scriptHome`; `watch.rs` skips exempt workspaces) | `followUps.svelte.ts:438`, `watch.rs:331` | **Kept** |
| Relaunching an exited agent for a follow-up | `followUps.svelte.ts:518` | **Kept**, through the Hive launch builder (§11.1) |
| `answerTabPrompt` (refuses an exempt target; Overlord-only anyway) | `claudeCode.svelte.ts:340` | Not used: the coordinator has `hiveAnswerWorker`, authorized in Rust by coordinator identity (§7.4) |
| Follow-up delivery needs `follow_ups_live()` (Overlord on) | `state/workspace.rs:2336` | `⟦HIVE RUN⟧` results and Hive follow-ups go through `deliver_to_agent` and are **not** gated on `follow_ups_live()`: a Hive works with the Overlord off |

A human who sets `overlord_exempt` on a Hive tab or workspace by hand gets exactly today's
meaning, the conservative reading: the paths in the table are off for it, and its column says
so. The Hive's own tools, authorized by `hive_role`, are unaffected.

### 7.4 Autonomy: the coordinator decides (decided 2026-10-23)

The default is that **the coordinator makes every decision it can**, and the human is asked only
where a checkpoint says so. The checkpoints are Hive settings, and each one is set to
*Coordinator* or *Ask me*:

| Checkpoint | Default | Notes |
|---|---|---|
| Plan: which workers, how many, in what order | Coordinator | "Show me the plan first" is the Ask-me setting |
| Workers' questions | Coordinator | One asker per question (`docs/overlord.md` §9.1.2), routed as the table below says |
| Workers' permission prompts | Coordinator | Within the allowlist below |
| Landing | Coordinator | Gates who calls `hiveLand` within the landing mode, which is fixed at creation (§3). In the approval mode (§8.3) it's always Ask me and can't be earned (§7.6) |
| Picking finals in the gallery | Coordinator | Creative Hives often set this to Ask me |
| Adding a dependency, changing a public interface | Coordinator | Listed so a Hive can tighten them |

Spending past the budget and pushing are not checkpoints: spending is never delegable (below),
and no agent pushes (§7.3).

What never moves to the coordinator, whatever the settings (Rust refusals by tab identity, not
words in a brief):

- creating a Hive, raising the cap or the budget, and **spending past the budget**;
- Claude's workspace-trust dialog (`trust_dialog_open`) and the plan dialog, whose rows are mode
  changes;
- any prompt option that persists a setting or a rule, or changes a mode (below);
- pushing (§7.3);
- a land, or an undo, whose diff changes agent or execution config (§8.1);
- delivering into anything but a Hive plain folder, and overwriting anything (§7.3, §10.6).

Overwriting outside the Hive's folders can't be read off a Bash prompt, so it isn't claimed as
something `hiveAnswerWorker` checks: it is held by the sandbox's write scope (§6.7) and by
maiTerm's own file operations, which never overwrite.

**`hiveAnswerWorker` is an allowlist.** Claude's dialogs offer far more than "always allow" and
"don't ask again": "Yes, and switch to auto mode", "switch to accept edits", the plan dialog's
rows, the outside-read prompt's "block reads… from now on" and "keep allowing" (which write the
user's settings), and the sandbox-escape prompt. A denylist misses the next one. So Rust accepts
only:

| On screen | The coordinator may answer |
|---|---|
| A tool permission dialog | The row whose label is exactly **Yes**, or exactly **No**, or Esc. Any other row is refused |
| An AskUserQuestion | One of the listed options (or several, where the question allows), by label |
| The plan dialog, the trust dialog, the outside-read prompt, a sandbox-escape prompt, anything unrecognised | Nothing: it goes to the human |

The `prompt_id` stale guard and the screen read stay. The 1.5 s guard does not apply: it is a
UI guard against a human's stray click (`PromptCard.svelte`, `BlockerCard.svelte`), not part of
the answer path, and an agent's answer is checked against the `prompt_id` instead.

**Who a worker's need goes to.** One channel per question, so nothing reaches the human twice:

| A worker needs | Goes to | How |
|---|---|---|
| An answer to a question | The coordinator; the human if Workers' questions is Ask me | A **blocker** on its task (the worker's priming and `blocker` field say "your coordinator"). Rust routes it by the checkpoint. Answered back by `hiveMessageWorker` or, from the human, as a blocker answer |
| A permission prompt answered | The coordinator; the human if Workers' permission prompts is Ask me | `hiveAnswerWorker`, within the allowlist |
| Money: a spend past the budget (§10.3) | **The human**, always | A blocker the worker marks `budget`. Mislabelling only ever sends a question to the human, the safe direction |
| A sandbox block (a host or path refused, §6.7) | The coordinator, which can't widen it | The coordinator raises its **own** blocker to the human, naming the host or path (§14.16) |
| To say it can't go on | The coordinator | `hiveReport {status: "blocked"}`: a status, not a question; the card reads **Blocked** (§4.3) |

`replyToOverlord` is withheld from workers (§7.2), and AskUserQuestion in a worker is answered by
the coordinator like any on-screen prompt. While a checkpoint is Coordinator, the worker's
blockers and prompts show to the human as **With coordinator**: read-only, with a take-over
action that moves that one question to the human.

**What it costs.** Answering prompts is new authority for an agent. A permission dialog appears
when auto mode judged an action worth asking about, and a coordinator persuaded by a worker's
transcript (a prompt injection carried in from a web page, say) could approve it. The allowlist
and the never-delegated list keep the worst outcomes out of reach, every answer is logged
(§7.6), and setting the checkpoint to Ask me puts the human back in front of every prompt.

**Worker text is data.** Everything a worker writes that maiTerm delivers (reports, verdicts,
blocker text, issues) reaches the coordinator framed as that worker's words. Rust escapes any
`⟦…⟧` frame and any line-leading `[maiTerm]` in it first, so a worker can't forge a
`⟦HIVE RUN⟧` result or "[maiTerm] The human answered".

**The coordinator never blocks the Hive on the human.** An AskUserQuestion in the coordinator's
own tab would stop its whole turn, and with it every worker waiting for a decision. So the
coordinator asks with a **task blocker** (`docs/tasks.md` §3.1: a `decision` with options and
the one it recommends) and keeps coordinating everything else. The question reaches the human in
the coordinator's column, the Loom's Decisions and the phone. The answer comes back typed as
"[maiTerm] The human answered", which is already how blockers work.

### 7.5 Learning from the human's decisions

Every decision that reaches the human is a lesson, and the Hive keeps it as a **precedent**:

```
precedent: {
  id, hive_id, blocker_id, asked_at, answered_at,
  category,                  // Rust-assigned: the checkpoint it fell under (below)
  label?,                    // the coordinator's free label ("visual style"); never counted
  question, context, options, recommended,   // copied by Rust from the blocker
  answer, reason?,           // the human's answer, and their words if they gave any (Rust)
  rule,                      // the coordinator's wording, shown as such: "When X, choose Y, because Z"
  scope: "hive" | "global",
  cited: n, overridden: n,   // kept by Rust, never by an agent
}
```

- **Recording needs no approval**, unlike the window playbook's proposals, because it can only
  record the human's own answer. `hiveRecordPrecedent {blocker_id, rule}` is accepted only for a
  blocker **the human answered** (a click in maiTerm or the phone, never an agent's answer).
  Rust copies the question, options, recommendation and answer from the blocker; the
  coordinator supplies only the `rule` line, which the Memory view labels as its wording beside
  the human's answer.
- **Precedents are Rust-owned storage**, not memory files: `<app data>/hive/<id>/precedents/`,
  beside `memory/`, outside every `autoMemoryDirectory` and every sandbox allowlist. An agent
  that writes into the memory folder can't make a precedent, and nothing a worker writes is
  "human by definition". The human can read, edit or delete each one in the Memory view.
- **The category is assigned by Rust** from where the decision came from: a held `hiveLand`, a
  routed permission prompt, a gallery final, the plan blocker, a worker question. A free label
  helps search and never counts toward a streak (§7.6).
- **Before asking, the coordinator checks the precedents.** The brief says: if one settles the
  question, decide, and **cite it** in the chat ("Chose the darker grade, per your precedent of
  Oct 23: 'moody over bright for this brand'"). Citing is what makes autonomy auditable: the
  human sees the decision and the reason in one line, and can say no.
- **A correction is a precedent too.** When the human overrides a decision, Rust counts it
  against the precedent that was cited, and the new answer replaces it. A rule that's overridden
  twice is retired by Rust, not tweaked. The `cited` and `overridden` counters are Rust's,
  updated from its decision log, never from an agent's edit (principle 8).
- **Precedents reach the coordinator when it decides, not as a growing brief.** The brief lists
  categories with counts. `hivePrecedents {category?, query?}` returns the relevant ones.
  That's the window playbook's design (handed over on `listEscalations`, never pasted into the
  doctrine), applied per Hive.
- **Global precedents** carry a person's taste across Hives: commit conventions, a visual style,
  how much testing is enough. A global precedent steers every other Hive, so Rust refuses
  `scope: global` from any tool. Promotion is a human click in the Memory view, which shows the
  rule beside the original question and answer; the coordinator may only suggest it in chat.
  Global precedents live in `<app data>/hive/precedents/`, every Hive's coordinator can query
  them, and a citation names the Hive the precedent came from.

### 7.6 Earning more autonomy

Confidence is counted, not felt:

- Only **delegable** checkpoints are eligible (§7.4's table). Nothing on the never-delegated
  list, spending past the budget included, can be earned.
- For each eligible checkpoint set to Ask me, Rust counts, from its own decision log, the
  human's recent answers in that **Rust-assigned** category (§7.5) and how often the
  coordinator's **recommended** option was the one chosen. Free labels never count, so a run of
  trivial questions can't build a streak.
- When the recommendation has been taken **5 times in a row** in one category, and no precedent
  there was overridden in that run, **maiTerm** (not the coordinator) files the offer once, as a
  `decision` blocker on the coordinator's task: "You've agreed with the coordinator's last 5
  calls on *landing*. Let it handle landing from now on?", with options and a recommendation.
  So it waits where every decision does: the coordinator's column, the Loom's Decisions and the
  phone, never a bespoke card. Yes flips that checkpoint to Coordinator. No resets the count, and
  the offer isn't repeated until another 5. The offer is excluded from precedents and streaks.
- It works the other way too. Two overrides of the coordinator's own decisions in one category
  within a day file the same kind of blocker, offering to hand that category back to the human.
- **The decision log** (Hive header → Memory → Decisions) lists every decision with who made
  it (coordinator, by precedent, or the human), the precedent cited, and the outcome. A daily
  digest in the coordinator's chat summarises what it decided alone. It's a setting, on by default.

### 7.7 Roles, briefs, receipts and reviews

What the coordinator spawns is shaped by a protocol taken from agency-agents (§15): typed
handoffs, a reviewer separate from the implementer, a retry cap, and escalation options. It's
expressed as fields maiTerm checks, not Markdown it hopes for.

**Roles.** A role is a short definition the coordinator picks for each worker: *mission*,
*rules*, *deliverable*, *done means*, and the tools it needs. A few dozen lines, with no
persona text. maiTerm ships a **curated catalogue** condensed from agency-agents' divisions
(engineering, testing, design, marketing, game development…), each with its MIT notice and the
upstream commit it came from. The Hive's media roles (image, video, audio, edit) are written
by us, because agency-agents' are advisory: none of them calls a generator or defines what a
finished asset must carry. The catalogue is data. The coordinator's brief lists role names and
one-line descriptions, and the chosen role's body goes into the worker's brief only. It is
**never** installed as Claude subagents: 282 descriptions in every worker's context, each
inheriting every tool, is exactly the bloat to avoid. A Hive's instructions can add roles of
its own. **As built (step 4a)**, the shipped catalogue is thirteen roles written by us
(`source: maiterm`, `resources/hive/roles/*.md`), none copied from agency-agents: backend,
frontend, test, refactor, docs, api-tester, ui-reviewer, code-reviewer, image, video, audio,
edit and art-director.

**Pairings.** Each kind of task has a builder role and a checker role (agency-agents' pairing
table, adapted): backend API → backend engineer + API tester; UI → frontend engineer + UI
reviewer; a hero image → image maker + art director. The coordinator uses the pairing unless it
has a reason not to, and says the reason.

**The brief** is typed: goal, context (files, assets, precedents that apply), acceptance
criteria (each one checkable), the evidence wanted, what to do when something is missing (stop
and report, never substitute), and attempt *n* of *max*.

**Receipts.** `hiveReport` with `done` is refused unless it carries evidence maiTerm can check:

| Receipt | Checked by maiTerm |
|---|---|
| `commits: [sha]` | Each exists on the worker's branch |
| `checks: [{job, summary}]` | `job` is a `hiveRun` job id (§6.8). Rust checks from its own run log: the job belongs to this worker's task, ran on its branch at a commit at or after the reported tip (the tip is that commit or an ancestor of it), and exited 0, read from the job's service tab (OSC 133 D), never the agent's word. A check with no record is refused |
| `assets: [id]` | Each is in the gallery with this worker's `task_id` (which `galleryAdd` always sets for a worker, §10.3). Assets from an earlier incarnation of the same task count |
| `files: [path]` | Each exists in the worker's folders |

Checks can't come from the worker's own tab: an agent's Bash tool runs as Claude's subprocess,
not at the tab's shell prompt, so it emits no OSC 133, and the activity store keeps one state
per tab with no history (`osc.rs:190-194`, `activity.svelte.ts:56-74`). Reading a Bash exit code
from a hook is an option only after a spike. Until the run queue exists (§16), `done` accepts
only `commits`, `files` and `assets` receipts.

ruflo's rule, adopted: *registered ≠ reachable ≠ healthy ≠ done*. A report that says "done"
without receipts is a report of hope.

**Reviews.** For work where it matters (the role says so, or the Hive's instructions do), the
coordinator spawns a **reviewer** with `reviews: <worker>`. It's a separate session with read
access to the builder's worktree (a detached worktree of the builder's branch) and its assets.
That worktree inherits its repo's trust like every worktree (§6.6), so the dialog is no
safeguard there: the reviewer launches with `--setting-sources user`, and the branch's project
hooks, settings and MCP servers never load.
It answers with `hiveVerdict`:

- `pass`, or `fail` with issues, each one *expected / actual / evidence / fix / files*.
- The builder gets the issues and the rule "fix only these; add nothing".
- **Inconclusive is a fail.** A reviewer that couldn't check something says so, and that's a
  fail with the gap as the issue.
- **Three attempts**, then escalation with one of five resolutions: reassign, decompose, revise
  the brief, accept as is, or defer. The coordinator picks one under the checkpoints, and a
  precedent can settle it.
- Gates are **things a command can check** (tests pass, the file exists, the screenshot was
  taken, the render is 1080p and 30 s long). They're never a score an agent asserts ("95% on
  brand"), which agency-agents' gates invite and which only produces made-up numbers.

**Shared files have one owner.** Lockfiles, manifests and other files every task touches
(`package-lock.json`, `Cargo.lock`, a routes table) are each owned by one worker at a time, as
the coordinator assigns. The brief says: if you need a change in a file you don't own, report
it. This is ruflo's governance rule, and it's what keeps parallel landings from colliding on the
same three files.

### 7.8 Fuses

A Hive runs agents unattended, and agents spend the human's Claude plan as well as money. ruflo
learned this the hard way: workers in many worktrees silently drained users' quota. maiTerm
checks these in Rust **before** a worker starts:

- the worker cap (§3.1);
- **spawns per hour and per day**, counted **across every Hive on the machine**, not per Hive
  (decided 2026-10-23): the Claude plan they draw on is shared, so two Hives each under their own
  limits could still drain it together (defaults 20 and 100; Projects enforces 200 threads a day
  across all of a user's projects). Each Hive's budget stays its own, and the header also shows
  the total across Hives;
- **a quota breaker**, also machine-wide (below).

**One reservation, under one lock.** Two spawns at once must not both pass the cap. Rust takes
the cap slot, the spawns-per-hour and per-day entries and the breaker check together, under one
lock, **before** any worktree or tab exists, and releases the reservation if the spawn fails or
its window doesn't confirm within a timeout (§7.2). The spawn counters and the breaker's expiry
are persisted in AppData, so a maiTerm restart neither resets the day's count nor lifts a pause.

**The quota breaker.** Claude's own auto-continue after a usage limit is narrower than "Claude
waits and resumes": it needs a subscription and Claude 2.1.234 or later, it takes no prompts
during the wait, it re-arms at most twice, it doesn't start for a reset more than 24 h away (a
weekly limit), and after more than 30 minutes of sleep it needs an Enter. A Hive breaks it
easily, since any delivery into the tab ends the wait. So:

- Every Hive agent's `--settings` installs a **StopFailure** hook (matcher `rate_limit`). Stop
  doesn't fire on an API error, so the Stop hook can't see a limit. maiTerm parses the reset
  time from it.
- When any Hive agent hits a limit, **every** Hive stops starting workers **until that reset
  time**, each coordinator is told, and each header shows "Paused · usage limit until 14:20".
- While the usage-limit wait is on a tab's screen, maiTerm **holds every Hive delivery** to it
  (`hiveMessageWorker`, `⟦HIVE RUN⟧`, reports to the coordinator), so nothing ends the wait.
- After the machine wakes from sleep, the Enter the wait needs is sent through the delivery
  gate (`deliver_to_agent`), never typed blind.
- A **weekly** limit, no subscription, a reset time that can't be parsed, or an auto-continue
  that has stopped (re-armed twice already) **pauses the Hive** and files a blocker for the
  human, the conservative reading: nothing waits on a resume that won't come.

**Overshoot.** The cap and spawn counts can't be overshot, given the lock. The budget can: it is
self-reported (§10.3), and concurrent workers each mid-spend can carry the total past it by the
spends in flight, which the header and the budget field say. Every paid render records its cost,
including ones discarded or failed, so the total is what was spent, not what was kept.

The coordinator is told its remaining headroom with every `hiveListWorkers`.

**What learning may not do** (ruflo's rule, and the window playbook's): nothing the Hive learns
widens its own authority. Precedents are the human's own answers. Earned autonomy (§7.6)
changes a checkpoint only on the human's click. No precedent, memory or role can raise the cap,
the budget, the fuses or a tool's reach.

### 7.9 Model routing with Jev (Darryl's idea, 2026-10-23)

Every worker is a full Claude session, and most of a Hive's cost is which model each one runs
on. A rename across twenty files doesn't need the model a concurrency redesign needs. So before a
worker launches, maiTerm decides its **tier** (model + effort). ruflo's router is a keyword table
with a fixed 0.6 "confidence"; ours uses **Jev**, TypeSafe's System One model: a fast judgment
model that returns typed answers with probabilities, not text. Designed from the live TypeSafe
docs (intent routing, composite scoring, confidence).

**Jev judges the task; code picks the model.** Asking Jev "which model?" would make it guess at
model names, prices and capabilities, which change every few months and which it has no evidence
for. Instead it answers narrow questions about the **task**, and a policy in code (weights and
thresholds the human can see and change) turns those answers into a tier. This is TypeSafe's own
guidance: keep policy in code, decompose broad judgments, keep the signals visible.

**One request per spawn, all questions together** (they run in parallel; TypeSafe measured
batching at about 12× cheaper and 10× faster than separate calls). The `state` is the worker's
brief, role, acceptance criteria and folders, plus what's known about earlier attempts:

| Question | Type | Levels (each a concrete situation, as the Score docs advise) |
|---|---|---|
| `reasoning_depth` | Score | mechanical edit or rename · routine change that follows an existing pattern · new logic in one area that needs several files understood · a design across subsystems, concurrency, or state that must not be lost · open-ended research or architecture with no pattern to follow |
| `ambiguity` | Score | the brief names the exact change and how to check it's done · the brief names the change; a few details are left to a reasonable reading · the brief names the problem or goal; the worker chooses the approach · the brief names only an area or aim; the worker decides what the problem even is |
| `blast_radius` | Score | docs, tests or scratch only · one module, trivially reverted · shared code with many callers · persisted state, auth, money, secrets or migrations; startup or single-instance logic, where a mistake stops the app running; a security boundary (what runs where, who can reach whom) |
| `context_breadth` | Score | one file · one module · several subsystems · most of the codebase |
| `needs_taste` | Noul | Does doing this well depend on aesthetic or editorial judgment (visual direction, copy, tone, a final pick among renders)? |
| `is_design` | Noul | Is the deliverable a design, plan or decision document that later work will build on, rather than code or a routine docs update? |

The `ambiguity` wording above replaced a first draft ("fully specified with checkable
criteria…"). The new wording asks what the brief leaves to the worker, and it halved Jev's
low-confidence answers on that question (20 of 36 down to 9) in spike 12. The two extra
`blast_radius` hazards at the top level are untested wording, added because spike 12's
single-instance-lock and run-where-it-shouldn't fixes scored only 1.7–2.2.

**The policy (code, in Hive settings, defaults shown):**

1. Normalise each Score to 0–1 and combine:
   `difficulty = 0.35·reasoning_depth + 0.20·ambiguity + 0.25·blast_radius + 0.20·context_breadth`.
2. Map to a tier:
   - below 0.25 → **Light** (Haiku, low effort);
   - below 0.50 → **Standard** (Sonnet, medium);
   - below 0.75 → **Strong** (Sonnet, high);
   - otherwise **Max** (Opus, high).

   The tiers name model ids in settings, so a new model release is a settings change, not a code
   change. The defaults are the current Haiku 5.5, Sonnet 5.5 and Opus 5.5.
3. Hard rules, applied after the sum, because "any serious risk" is a condition, not a weight (the
   docs' warning about compensating scores):
   - `reasoning_depth` at 2.9 or above → **Max**. The weighted sum can't reach Max in practice:
     the highest seen in spike 12 was 0.70. Every under-routed hard code task there (an SSH helper,
     file operations with safety rules, an OAuth client, coalesced state saves) scored 2.90–3.02.
   - `is_design` above 0.7 → **Max**. A design doc touches only docs, so `blast_radius` pulled design
     work down a tier, while what it governs is everything built after it. In spike 12 the two
     design docs scored 0.92 and 0.97, and nothing else scored above 0.45;
   - `blast_radius` at its top level → at least **Strong**;
   - `needs_taste` above 0.7 → at least **Strong**;
   - a reviewer runs at least the tier of the worker it reviews.
4. **Uncertainty doesn't buy a bigger model.** A first draft moved the tier up one whenever any
   Score's confidence was under 0.5. Spike 12 showed that rule caused most of the routing error:
   it fired on 20 of 33 tasks, turned a right answer into an over-route 11 times, and fixed a wrong
   one only 4 times. Dropping it alone raised agreement from 11 to 18 of 33. So:
   - one low-confidence Score changes nothing;
   - three or more low-confidence Scores mean Jev can't tell what the task is (it happened on a
     release note). The worker gets the Hive's default tier, marked "routed by default", and the
     coordinator may choose instead. Guessing up from "I can't tell" picked the most expensive
     tier for a version bump.
5. **The cascade.** A worker that fails review, or reports `blocked` because the task is beyond
   it, comes back on its next attempt one tier up (the docs' cascade pattern). Three attempts
   (§7.7) can therefore run Standard → Strong → Max.
6. **Floors and ceilings** per Hive: "never below Sonnet", or "never Opus" when the budget is
   tight.
7. **The coordinator can override**, with `model` on `hiveSpawnWorker`. It must give a reason,
   and the override is logged beside what the router would have picked.

**It learns from outcomes, which is the real advantage over a keyword table.** Every spawn records
the router's inputs and answers (each probability and confidence), the tier chosen, any override,
and the outcome: first-attempt pass, attempts used, escalations, tokens and time, and whether the
human stepped in.
- **v1 shows it.** A routing report in the Hive's Memory view: "Light workers passed review first
  time on 46 of 50 tasks with `reasoning_depth` below 1". The human tunes the weights and
  thresholds from numbers.
- **Later, it tunes itself.** The recorded probabilities are exactly the features TypeSafe suggests
  for a small classical model trained on labelled outcomes. Its suggestions are proposals the human
  applies (§7.8: learning never widens its own authority, and spending is authority).

**Routing is an optimisation, never a dependency.** No TypeSafe key, the API unreachable, or an
answer later than 3 s: the worker launches on the coordinator's choice, or the Hive's default
tier, and the card says "routed by default". A spawn never waits on the router.

**What leaves the machine.** The router sends each worker's brief (task text, file names, the
role) to TypeSafe's API. That's a third party seeing the Hive's work, so routing is **off until
the human turns it on**, with that sentence beside the switch. The API key lives in the OS
keychain, never in preferences or `preference_meta`, never shown to an agent. It moves to the
Secrets Vault when that's built.

**Also later:**
- The same shape can pick a **generator quality** for media: a draft-quality render for
  exploration, the expensive model only for a final.
- It could also decide whether a **question can be settled by a precedent** before it goes to the
  coordinator.

Both have to be measured before either is built.

## 8. Landing

The local stand-in for Projects' pull requests. Both modes start the same way:

1. A worker reports `done`. Its card goes to **Reported**.
2. The coordinator reviews: it reads the report, can read the worker's diff in each repo
   (`git diff <target>...hive/<hive-slug>--<worker-slug>`, the §6.4 name from
   `worktree::worker_branch`), and can send the worker back with
   `hiveMessageWorker`.
3. When satisfied, the coordinator calls `hiveLand {worker, summary, commits: {repo: sha}}`,
   naming the commit it reviewed in each repo. The card goes to **Landing**.

**A land is pinned to the reviewed commit.** A live worker can commit after the review, after the
check, or after the human approves a land card, so "the worker's tip" read at landing time is a
tree nobody looked at. Rust refuses `hiveLand`:

- if a repo's `hive/<hive-slug>--<worker-slug>` tip isn't the sha the coordinator named ("branch moved since
  you reviewed it": review again);
- if the worker's worktree is dirty (`git status --porcelain` non-empty). Uncommitted files would
  count toward a check but never land;
- while the worker is mid-turn. From the call until the land ends, deliveries to the worker are
  held (`deliver_to_agent`), so nothing prompts it to commit underneath the land.

A worker with no git folders (a media task) has nothing to land. Its `hiveLand` just retires it,
since its results are already in the gallery.

### 8.1 Default: land to Base (decided 2026-10-23)

maiTerm lands the worker into Base (main) with no card (unless its diff changes agent or
execution config, below), in two git steps per repo. Neither one
can touch anyone's uncommitted work (both behaviours checked in a scratch repo, 2026-10-23), and
neither runs in the worker's live worktree:

1. **Bring the reviewed commit up to date, in a temporary worktree.** maiTerm adds a detached
   worktree at the pinned sha, seeded like a worker's (setup, the slim cache seed, §6.5), and runs
   `git merge <Base>` there. A conflict aborts, and `hiveLand` answers with the conflicting files.
   The coordinator tells the worker to resolve them in its own tree and report again, which means
   a new sha and a new review. The land check (if set) then runs on that merge, as a queued job
   (§6.8) under the worker's sandbox profile, and a failure refuses the land with its output. The
   result is the **tested commit**.
2. **Advance Base to exactly the tested commit, by fast-forward only.** Since it contains Base,
   Base moves to it with no new merge:
   - if Base is checked out somewhere (usually the human's main checkout, where other agents may
     be working), `git -C <that checkout> merge --ff-only <tested commit>`. Git refuses if that
     would overwrite an uncommitted change there. **The land then waits**: the card reads
     "Landing · held: main checkout has uncommitted changes to `src/x.ts`", and the coordinator
     is told, rather than anything being stashed or overwritten;
   - if Base isn't checked out anywhere, `git update-ref refs/heads/<Base> <tested> <old tip>`,
     a compare-and-swap that refuses if Base moved meanwhile.

   Then the worker's branch is advanced to the tested commit by the same compare-and-swap
   (`update-ref` from the pinned sha), so the branch holds exactly what landed.

**Base moved between the steps.** A human's or another agent's commit on Base makes step 2 fail
as not-fast-forward, which is not the dirty hold above. maiTerm redoes step 1 and the check
against the new Base, up to **3 attempts**, then holds: "Landing · held: Base keeps moving", and
the coordinator is told. It never loops.

**The land lock.** Lands are serialized per repository, and the lock is held across steps 1 and
2, so two coordinators or two workers can't race. It's an `O_EXCL` lockfile in the repo's git
common dir holding the pid and which build took it (dev or prod), because an in-process lock
wouldn't serialize the dev and prod maiTerms (separate data dirs, one repo). A lock whose pid is
gone is recovered.

After a land, maiTerm moves the coordinator's read copy to the new Base tip, and any new worker
branches from it. Files in the main checkout change under agents working there, the same as when
any one of them commits. The land log (§8.6) records the reviewed sha, the tested commit, and
Base before and after.

**A land that changes agent or execution config goes to the human, always.** A worker's commits
can add hooks or scripts that run on the human's machine, outside any sandbox, the next time
anyone opens a session, commits or builds there. So Rust checks the diff from Base to the tested
commit, and if it touches any of these, the land files an **Agent request** card naming the
files, whatever the Landing checkpoint says (§7.4 lists it as never delegated):

- `.claude/**`, `.mcp.json`, `CLAUDE.md` and `AGENTS.md` at any depth;
- `.husky/**`, `.githooks/**`, CI definitions (`.github/workflows/**`, `.gitlab-ci.yml`);
- `package.json` when its `scripts` object changes (compared parsed; a file that doesn't parse
  counts as changed), `.envrc`, `build.rs`;
- any path the human adds to the list in Hive settings.

On approval the land proceeds from the same pinned sha. An Undo whose revert touches these paths
is checked the same way (§8.6). The list errs wide on purpose: a false positive costs one click.

**Batched lands** (§6.8). Workers already passed to `hiveLand` in one repo can land as a batch:
maiTerm builds an integration chain (Base, then each member's pinned sha merged `--no-ff`) in a
temporary worktree, checks it once, and fast-forwards Base to that tested chain under the lock.
For members, step 1 above is skipped and the worker's branch isn't advanced: a member counts as
landed when its merge commit is an ancestor of the new Base. Each member gets its own land-log
entry naming its merge commit. A member that conflicts in the chain, or whose diff trips the
config rule above, leaves the batch and lands alone. A worker that touches several repos batches
only if every repo batches, and the approval mode (§8.3) never batches.

### 8.2 Several repos in one land

There's no atomic commit across repositories, so a worker that changed two repos lands in two
passes:

1. **Step 1 (update + land check) runs in every repo first.** If any repo fails, nothing lands.
   The land locks are taken in folder order, so two multi-repo lands can't deadlock. If any repo
   trips the config rule (§8.1), the whole land waits on one card naming every repo's files.
2. **Then step 2 fast-forwards each repo in folder order** (or in the order the coordinator
   passes). Each repo's change has passed its own check, so landing one without the other leaves
   nothing broken in that repo. If a later repo is held, the card says so ("Landed web; holding
   api: uncommitted changes to `routes.ts`"), and the next `hiveLand` lands only what's left,
   from the same pinned shas.

What can break is the pair: a web change that needs an api change not yet landed. That's the
coordinator's to order, the same as Projects reporting "which PRs need to merge first". The brief
says so, and the land answer always names which repos landed.

### 8.3 Mode: integration branch, I approve each land

`hiveLand` files an **Agent request** card ("Land *Session store* into *hive/auth*?", with the
diff stat and the coordinator's summary quoted as its reason) in the Loom's Decisions, in the
worker's column and on the phone. It's the existing agent-requests machinery
(`docs/agent-requests.md`): only a human click or a paired phone approves. At the end,
`hive/<slug>` holds the whole result in each repo, and the human merges it into Base or opens
MRs from it, outside the Hive.

The mode is fixed when the Hive is created (§3), because it's a branch topology, not a
checkpoint: switching to land-to-Base later would fast-forward integration merges into Base. In
this mode the Landing checkpoint is always Ask me, and it's excluded from earned autonomy (§7.6).

**The card is pinned too.** It stores the sha per repo that `hiveLand` named, its diff stat is
computed from that sha, and on approval maiTerm merges **exactly that sha** `--no-ff` into
`hive/<slug>`, never the branch's tip. If the worker's branch has moved since, the card says
"branch moved since you approved" and the newer commits need a land of their own. The merge and
the land check run in a temporary worktree, as in §8.1; `hive/<slug>` then advances by
compare-and-swap, and the land log records the sha.

**What "Base" means in this mode** is `hive/<slug>`, everywhere:

| | Land to Base (§8.1) | Integration branch |
|---|---|---|
| Landing target | Base | `hive/<slug>`, by a `--no-ff` merge of the approved sha |
| Coordinator's read copy | Moves to Base's new tip | Its worktree on `hive/<slug>`, moved to the new tip |
| New workers branch from | Base | `hive/<slug>` |
| Post-land check (§8.6) | At Base's tip | At `hive/<slug>`'s tip |
| Undo (§8.6) | The coordinator or the human | **The human's**: `git revert -m 1 <merge>` on `hive/<slug>`, from the land log |
| Push after land (§8.5) | The setting | Off and hidden: `hive/<slug>` is pushed by the human |
| Batching (§6.8) | Yes | Never; each land is its own card |

### 8.4 After landing

The worker is retired: the agent exits, and its worktrees are removed **only if clean** (a
pinned land already refused a dirty one, §8). Its `scratch/` is removed only once everything in
it the worker reported is in the gallery. The branches are kept. Its launch record (§11.1) is
kept with the task, for its transcript list.

**A retired worker's chat stays readable** (decided 2026-10-23). Its card moves to Landed or
Retired, and clicking it opens the same chat column, read-only, with a `retired` source (§4.2).
maiTerm appends `{session_id, transcript_path, started_at}` to the task's session list on every
SessionStart, so the list survives the tab's removal, a `/clear`, a resume, a reload and a
restart (§11.2). Rust's `get_task_transcript(hive_id, task_id)` reads only those recorded paths
and renders the sessions in order with a divider between them. The column says "Retired · landed
`a1b2c3d`" in place of a composer. "Why did it do that?" a week later is a click, not a search
through `~/.claude/projects`.

### 8.5 Pushing

Landing is local unless the Hive's **Push after land** setting is on (decided 2026-10-23: a
setting, default off). When it's on, a land to Base pushes that repo's Base to its upstream right
after the fast-forward (once per batch, for a batch). A push that's refused (the remote moved,
auth) doesn't undo the land. The card shows "Landed · push held: <reason>", the coordinator is
told, and the next land pushes both. It never force-pushes.

maiTerm's land path is the only thing that pushes. No Hive agent can (§7.3), and the setting
doesn't exist in the approval mode (§8.3).

### 8.6 Undoing a land (decided 2026-10-23)

Landing straight to main needs a way back. maiTerm keeps a **land log** per Hive: worker, repo,
the reviewed sha, the tested commit, Base before and after, the merge commit for a batch member,
the time, and the coordinator's summary.

- **Undo land** makes a **revert commit** on Base and lands it through the same guarded steps as
  a land (temporary worktree, land check, lock, fast-forward, Base-moved retry). It never resets
  or rewrites history: Base may have moved on, others may have pulled, and a reset can't be
  pushed without force.
  - A land that was a fast-forward is reverted as one commit that applies the inverse of
    `git diff <Base before> <Base after>` three-way onto the current Base. A conflict means later
    work builds on the land, so the undo is refused and goes to the coordinator as fix-forward.
  - A batch member, or an approval-mode land, has its own merge commit: `git revert -m 1 <merge>`.
  - A revert that touches the config paths of §8.1 waits on a human card like the land did.
- **Where Undo lives:** the land log, the worker column's menu, and `hiveUndoLand {land}` for the
  coordinator. **Never on a card** (§4.3): cards are re-laid-out constantly, and a stray click
  there would write a revert to main. The human's Undo opens an inline confirmation in place,
  naming the repo, the commits it reverts and the branch it writes to, before anything is
  written. In the approval mode, Undo is the human's alone (§8.3).
- **A land that breaks Base stops landing.** Since every land fast-forwards Base to exactly the
  commit its check passed, the landed tree is the tested tree, and re-running the same check
  there proves nothing. So the **post-land check** is the land check run on Base's tip alone, as
  a queued job (§6.8) in a temporary worktree seeded like a worker's, not in the read copy (which
  has no setup and is the coordinator's). It runs when a land check fails, to tell a broken worker
  from a broken Base, and after any land whose landed tree's hash differs from the tested one.
  When Base alone fails, the lands since Base last passed are marked *suspect*, every further
  land in that repo is held, and it goes to the coordinator first (undo, or fix forward with a
  worker). It goes to the human only if the coordinator can't settle it.
- Undoing a land doesn't reopen the worker. The coordinator decides whether the work comes back
  as a new task.

## 9. Shared memory

**Mechanism: Claude's own auto memory, pointed at one directory.** Every Hive agent's launch
settings (§11.1) carry `"autoMemoryDirectory": "<app data>/hive/<id>/memory"`, an absolute path (the only
kind Claude honours, per the auto-memory memory). That gives, natively:

- Each agent reads `MEMORY.md` at start and opens individual files when it needs them, which is
  exactly Projects' progressive load.
- Each agent writes memory files with the runtime's own memory behaviour, and a configured
  directory **gets write permission without prompts**. A symlinked one doesn't, which is why
  it's a configured path and not a symlink.

Why redirect at all: Claude's default auto memory is keyed by git repository and **shared across
all its worktrees**. Left alone, the Hive's workers would write into the human's everyday memory
for that repo, which every other session there reads. With several folders there would also be
several memories, not one. That's also why the settings are **re-passed on every launch**
(§11.1). Resumed without them, an agent's recorded system prompt still names the Hive's folder,
but the write exemption is gone (manual and acceptEdits deny the write; auto allows it), and
after its **first compaction** the agent switches to the per-repo default and writes into the
human's memory (spike 1, §13.1). The leak starts at that compaction, not at the resume, which
makes it easy to miss; so every Hive relaunch goes through the launch builder.

What maiTerm adds:

- **`MEMORY.md` exists from the start.** maiTerm creates it, with its fences (below), when the
  Hive is created (§3). Two sessions that both created it with Write lost one session's index
  lines: a Write to a file that doesn't exist yet gets no "modified since read" check. Once the
  file exists, Edit's stale-read check makes a second writer re-read, and nothing was lost
  (spike 5, §13.1).
- **Index rebuild.** Several agents editing one `MEMORY.md` at once can still lose updates.
  maiTerm watches the directory and **regenerates its section of `MEMORY.md` from the files'
  frontmatter**. It parses Claude's own formats: an index line is `- [Title](file.md) — hook`, and
  a memory file carries frontmatter `name`, `description` and `metadata {node_type, type,
  originSessionId, modified}`. The brief tells agents to write files and leave the index to
  maiTerm, but Claude's own auto memory writes a pointer line in that format to `MEMORY.md`
  whenever it saves a file, and no brief stops that. So the rebuild expects agent lines and
  absorbs them rather than fighting them.
- **The file has fenced sections.** Block HTML comments mark them:

  ```
  <!-- maiterm:kept:begin — human and pinned lines; maiTerm never rewrites these -->
  ...
  <!-- maiterm:kept:end -->
  <!-- maiterm:index:begin — generated from the files; edit the files, not these lines -->
  - [Session store decision](session-store.md) — Redis, not Postgres · coordinator
  <!-- maiterm:index:end -->
  ```

  On each rebuild, a line outside both fences is sorted:
  - A line that links a memory file in the directory is an **index entry**: it's absorbed into
    that file's generated line (the file's frontmatter wins), so agent pointer lines never pile
    up or duplicate.
  - A line that links nothing is moved, verbatim, into `unfiled.md` (itself a memory file,
    provenance `unknown`), and the Memory view offers it to the human to keep, pin or delete.
    Moving is not dropping.
  - A human's edit through the Memory view, or a line the human pins there, goes in the kept
    block.
- **The rebuild never loses a line it didn't write.** ruflo's index curation once replaced a
  hand-kept 75-line `MEMORY.md` with a 6-line stub. Its fix, adopted: a rebuild may add, reorder
  and fold its own lines, and never rewrites the kept block. Claude loads only the first 200
  lines or 25 KB of `MEMORY.md`, so the budget goes to the kept block first, and the generated
  index gets what's left, one line per file. Over that, the oldest generated lines **fold** into
  one dated archive line linking an `archive-<date>.md` index, so every file stays reachable; a
  fold isn't a drop. If the kept block alone passes its cap (60 lines or 8 KB), or a rebuild would
  drop a line for any other reason, it writes `MEMORY.generated.md` beside the file instead, and
  the Memory view says why.
- **Write protocol.** A rebuild runs debounced after the last change in the directory. It reads
  `MEMORY.md`, builds the new text, writes a temp file in the same directory, and renames it over
  the original only if the file still has the content it read; if an agent wrote in between, it
  starts again from the new content. The Memory view's edits go through the same writer.
- **Provenance comes from maiTerm, not from the file.** Frontmatter is agent-editable, so a
  `writer:` field in it, or Claude's own `originSessionId`, proves nothing. Rust keeps each file's provenance in its own record,
  beside `memory/` and outside every agent's write scope: a change matched by path and time to a
  Write/Edit tool event (PostToolUse) from a Hive tab is that tab's (`coordinator` or the
  worker's title); an edit through the Memory view is `human`; anything else (a Bash write, a
  change no tool event explains) is `unknown`. The index line shows Rust's value. Agents weigh
  "the human said" above "a worker concluded", and nothing in this directory is human by
  definition: **precedents aren't memory files at all** (§7.5). They live in Rust-owned storage
  outside every `autoMemoryDirectory` and sandbox allowlist, and are never indexed here. ruflo's
  provenance column is the model for this.
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
| Local ComfyUI | Its API answering on its configured port | Workflows run through the HTTP API, reached through an MCP server or a `hiveRun` job (a `gpu` resource), since a worker's localhost is closed by default (§6.7) |

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
- **Every paid render counts, kept or not.** A discarded or failed render cost the same, so it
  is recorded too: a `galleryAdd` with no `path` and a `recipe.cost_usd` records a spend only (a
  ledger row, no asset). The total is what was spent, not what was kept (§7.8).
- This accounting is **self-reported**. maiTerm can't see inside an API call an agent makes
  directly, so the budget is a rule agents follow plus a total the human watches, not a wall.
  Concurrent workers each mid-spend can carry the total past it by the spends in flight (§7.8).
  The only hard limit is the provider account's own credit, which is why the sheet says so next
  to the field.

**Filing**, all Hive agents:

| Tool | Does |
|---|---|
| `galleryAdd {path?, title, kind?, recipe?, parents?, tags?, collection?, caption?}` | Files one asset (or several, as a list). `task_id` and `made_by` come from the caller's identity, never from arguments. Answers with the asset id. With no `path`, records a spend only (above) |
| `galleryList {kind?, made_by?, task?, collection?, status?, tag?, query?, limit?}` | Assets as data, newest first |
| `galleryGet {id}` | One asset with its recipe and lineage, and the stored file's path, to use as an input |
| `galleryTag {id, tags?, status?, starred?}` | Workers may tag and star; `final` and `rejected` are the coordinator's and the human's |
| `galleryCollect {collection, add?, remove?, order?}` | Coordinator: build and order collections ("Storyboard v2", "Shortlist") |
| `galleryDeliver {assets \| collection, folder, names?}` | Coordinator: §10.6 |

**`galleryAdd` takes a path only from the caller's own folders**: its `scratch/`, its worktrees,
the system temp directory, or an **output folder the human listed** in Hive settings for a tool
that writes only to its own place. It's matched whole against the canonical path, with symlinks
resolved first (the path-authorization memory: never a prefix test). The rule **prevents
accidents**, not a determined agent: anything an agent can read it can copy into its `scratch/`
first, and file from there. So a gallery asset is treated as no more private than the agent's
transcript (`docs/secrets-vault.md` §10.1), and the gallery, which the phone reaches later, is
not a place secrets are kept out of by this rule. The other residual channel is a worker's
allowlisted network hosts (§14.16): a host a generator needs can also receive a file.

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
1/2/3 set Pick/Final/Reject, C compares the selection, Escape goes back. These bare keys act only
while focus is inside `[data-gallery]`, never in an input, a textarea or a composer, and inside
`[data-gallery]` only `isAppShortcut` keys reach the app, as `[data-filetree]` does for Files
(§4.6).

### 10.5 Thumbnails, metadata, serving

- Rust generates thumbnails and reads metadata when an asset is filed, off the main thread.
  - **macOS**: QuickLook (`qlmanage -t`, present on every Mac) gives thumbnails for images,
    video, PDF and Office files with no install, in 0.15–0.4 s each. For audio it gives only a
    generic icon (spike 7, §13.1).
  - **ffmpeg/ffprobe**, when installed (they are here), add dimensions and duration, video hover
    strips and **audio waveforms** (`showwavespic`, about 0.1 s). A hover strip is made with
    parallel seeks, one per frame, or `-skip_frame nokey` (keyframes only), **never a full
    decode**: for ten frames of a long video, a full-decode `fps` filter took 3.6 s, parallel
    seeks 1.4 s and keyframes 0.47 s. Without ffmpeg, an audio tile shows the kind icon.
  - **Windows/Linux**: ffmpeg plus the `image` crate.
  - With neither tool, a tile shows a kind icon and the title. Nothing is required.
- **Serving.** Nothing serves local files to the webview today: the asset protocol isn't enabled
  (no `protocol-asset` feature in `Cargo.toml:19`, no `assetProtocol` in `tauri.conf.json`), and
  both CSPs (`tauri.conf.json:28`, `tauri.dev.conf.json:5`) block it. maiTerm registers its own
  **`gallery://<hive id>/<asset id>`** URI scheme (`http://gallery.localhost/...` on Windows),
  served by Rust **by asset id**: the handler looks the id up in the Hive's index and opens only
  that stored file, so no path from the webview ever reaches the filesystem. It answers `Range`
  requests, which video seeking needs, and thumbnails are `.../<asset id>/thumb`. Both CSPs add
  the scheme to `img-src` and a `media-src`. This is the conservative choice over enabling the
  asset protocol with a per-Hive `allow_directory` scope, which would put a filesystem scope in
  the webview's hands; that stays the fallback if the scheme can't stream large video.

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

### 11.1 The Hive launch record, and one builder for every launch

A Hive agent's first launch carries everything that makes it a Hive agent: `--add-dir` for each
of its folders and the gallery (§6.3), with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1` so
each added repo's `CLAUDE.md` loads (confirmed, spike 6, §13.1); `--settings` with the memory
directory (§9), the sandbox and the Write/Edit deny rules (§6.7), the push denies (§7.3) and its
hooks (below); `--strict-mcp-config` with only the MCP servers it needs (§6.8); its tool set
(WebFetch removed where the role doesn't need it, §6.7; the coordinator's restricted set,
§7.1); its env, including `CLAUDE_CODE_TMPDIR` (§6.7); `--model` and `--effort` (§7.9); and for
a reviewer `--setting-sources user` (§6.6). Every **re**launch today uses the generic
`claude --resume %claudeSessionId` (`agents/resume.ts:104`, `triggers/defaults.ts:5`): session
restore (`+page.svelte:226-254`), tab and workspace resume (`workspaces.svelte.ts:369-374`),
reload, a follow-up relaunch (`followUps.svelte.ts:534`), `recoverTab`. And agent init writes
that generic command over the tab's auto-resume unless it's pinned
(`agentState.svelte.ts:501-507`). After any restart, a worker would come back without its
folders, unsandboxed, at medium effort, and, from its first compaction, writing into the
human's per-repo memory (the leak §9 exists to prevent; spike 1, §13.1). So:

- **Rust persists a launch record per task**, in `<app data>/hive/<id>/tasks/<task id>/`:
  runtime, model, effort, cwd, the current add-dirs, the env maiTerm sets, the setting sources,
  the path of a per-agent settings file beside it, and the task's session list (§4.2). It never
  holds a credential: the account env comes from the normal spawn path at each launch (§6.6,
  `docs/login.md`).
- **One builder**, `hive_launch_args(record, resume)`, rewrites the settings file from the
  record and builds the whole command line, with `--resume <session>` or without it. It's the
  only way a `hive_role` tab's agent starts: first launch, every relaunch path above,
  `hiveRestartWorker` and waking a Done Hive (below). The folders and allowlist can change during
  a Hive's life (a folder added, the Tools row re-detected), and the builder reads the current
  record each time.
- **Every relaunch re-passes everything**, the settings included, never only `--resume`. In
  particular **`--effort` on every relaunch**: on resume Claude keeps the model but effort falls
  back to medium (spike 11, §13.1). `--model` is passed too, so the record, not the resumed
  session, decides it.
- **Every hook in a Hive tab is `type:"command"`.** The sandbox breaks localhost http hooks
  (spike 9, §13.1), so in a sandboxed tab maiTerm's global http hooks fail silently: no agent
  state, no permission card, no Stop. The settings file therefore installs command-hook versions
  of all of them: maiTerm's state, permission-ledger and PreModelSwitch hooks, the fail-closed
  PreToolUse deny (§6.8), the WebFetch host check and the `PostToolUseFailure` block detector
  (§6.7), and the StopFailure hook (§7.8). Each is the same maiTerm-owned script, outside every
  writable path and on `denyWrite`; localhost is opened by port only, if ever (§6.7), never to
  make http hooks work. **As built (step 3):**
  - The script is `<app data>/hive/bin/maiterm-hive-hook`. maiTerm rewrites it and the `port`
    file beside it on every server bind, and the script reads that file on every call, so a
    restart on a new port can't strand a tab. `$MAITERM_PORT` is only the fallback.
  - Only the gate blocks: `gate` mode (PreToolUse on `Bash|PowerShell|Write|Edit`, `onFailure:
    "block"`) posts to its own route, `/hive/gate`. That route is never deduplicated against the
    mod's copy, because a dropped duplicate answers an empty 200. The script exits 0 only on the
    exact body `allow`.
  - `observe` mode forwards every event the hooks list to `/hooks?via=hive`, PreToolUse for
    every tool included, and always exits 0. So a gated call is posted twice, once to each
    route. Its output is printed only for SessionStart (`prime=1`, the invariant rules) and
    PreModelSwitch.
  - The coordinator launches with `--setting-sources project`, so the user's global http hooks
    don't load there at all. Under dontAsk the allow list must be the whole list, so the user's
    own allow rules stay out too.
  - The launch line `env -u`s the agent markers and the parent-session variables (a parent's
    `ANTHROPIC_API_KEY` gave "Invalid API key"). Under a managed account it also removes every
    variable that would answer instead of the account's login.
  - `hive_launch_args` itself is pure. `prepare_launch` rewrites the record, the settings,
    `mcp.json` and `brief.md` before every launch.
- **`hive_role` tabs skip the generic paths**: `handleEnableAutoResume` and the auto-resume
  template migration never touch them, so nothing writes a generic resume command over a Hive
  tab.
- **No record, no launch.** If the record is missing or unreadable, the launch is refused and
  the card goes to **Failed** ("launch record missing"). It never falls back to a generic launch,
  because the fallback is exactly the silent leak above.

Spike 1 proved the mechanism (`--settings` carries `autoMemoryDirectory` with no prompt in
every permission mode). Steps 3 and 4 (§16) aren't done until every relaunch path goes through
the builder, checked after a suspend and resume, a maiTerm restart, a reload and a `/compact`:
`/memory` shows the Hive's directory, the add-dirs are present, the sandbox is on, the effort
is the record's, and maiTerm still receives the tab's hook events.

### 11.2 Events

| Event | What happens |
|---|---|
| Suspend the workspace or a Hive tab | Every Hive tab (or that one) suspends (PTYs killed); worktrees, memory and gallery stay. Cards show Suspended. Its run-queue jobs and leases are released (§6.8) |
| Resume | Each agent relaunches through the launch builder (§11.1), resuming its session **in its own folder**; the coordinator gets a Hive status digest |
| maiTerm restart | Same as resume, through session restore, which calls the builder for `hive_role` tabs |
| Reload a Hive tab | The existing reload path (new tab id, `carry_tab_state_on_reload` carries `hive_role`, `remapTab` for tasks). The card, the launch record, jobs and gallery assets are keyed by task, so nothing moves. The relaunch goes through the builder, and the new session joins the task's session list |
| Worker crash | Defined in §11.3. Card → Failed; the coordinator is told by a framed message, and may `hiveRestartWorker` |
| Pause (Hive header) | Stops new spawns and holds deliveries; running turns finish |
| Duplicate a Hive tab | **Refused**: two agents on one worktree and one task |
| Duplicate a Hive workspace, or a window holding one | **Refused** for the workspace; a window duplicate skips Hive workspaces and says so. A clone of a Hive would be hidden tabs with no Hive, or a second Hive on the same worktrees and branches, and neither is safe (the conservative choice over cloning with `hive` dropped and `hive_role` cleared) |
| Move a Hive tab (`moveTab`, drag) | **Refused**. A worker belongs to its Hive's worktrees, task list and coordinator |
| Move a Hive workspace to another window | **Refused in v1**: the window-scoped rpc, events and spawn reservations (§7.2) would need re-homing mid-flight |
| Archive a Hive tab | **Refused**. A worker leaves through Stop, land or retire, which keep its worktrees and its readable chat (§8.4) |
| `closeTab` on a Hive tab | For a worker, means **Stop** (`hiveStopWorker`): the agent exits, worktrees and branches are kept. For the coordinator, refused: a Hive ends through Done or Delete |
| Share export | Hive workspaces are **excluded**: the share file is built from an allowlist (`docs/workspace-share.md`), and neither `Workspace.hive` nor `Tab.hive_role` is on it, so a Hive never reaches a `.maiterm-workspace` |
| Turn back into a workspace | §3.3: clears `hive_role` on every tab and runs `heal_pane_active_tab` |
| The goal is met (decided 2026-10-23) | §11.4 |
| Delete the Hive | A sheet lists every branch not landed, every dirty worktree and the gallery's size. Clean worktrees are removed; **dirty ones are kept and listed**; no branch is deleted; the gallery stays unless ticked. Same rule as Claude's worktree cleanup: never delete work |

### 11.3 A crashed worker, and restarting one

**Crash** is defined, so it can't be confused with Idle, an exit or a suspend: the worker's
agent has left the foreground with its shell at an empty prompt (`get_pty_foreground_job`, OSC
133 A), with **no `hiveReport` since its last launch**, and the tab isn't suspended. A tab whose
PTY itself died is Failed too. An agent that exits after reporting is Reported, not crashed.

`hiveSpawnWorker` can't restart one: it would run `git worktree add` on a branch that exists and
mint a second task. So:

- `hiveSpawnWorker` **refuses a slug** whose branch or worktree already exists.
- **`hiveRestartWorker {worker, fresh?}`** (coordinator) relaunches in the **same tab and
  worktrees** through the launch builder, only at an empty prompt (the follow-ups gate,
  `docs/follow-ups.md` §6.2) and after the trust re-check (§6.6). By default it resumes the last
  session; `fresh` starts a new one, with the brief re-sent. The cap, fuses, quota breaker and
  memory pressure are re-checked first (§7.8), and the badge number is kept (§4.6).
- If the tab is gone, it opens a new hidden tab on the existing worktrees and moves the task's
  ownership to it, the way reload does (`remapTab`).

### 11.4 Done

At the start, the coordinator turns the one-line goal into **completion criteria**, each one
checkable like a receipt (§7.7), and posts them where the human sees them first; a criterion can
be changed in conversation, and Rust keeps the current list on the Hive record. Done is then a
tool, not a word in the chat (principle 8): **`hiveFinish {report}`** (coordinator). The report
addresses every criterion with its receipts, and lists what landed, the assets that are final,
what was spent and what the coordinator decided alone.

Rust **refuses** `hiveFinish` while:

- any worker is live (Starting with its agent launched, Working, Needs you, Blocked, Idle,
  Reported or Landing), or a reviewer is;
- a run-queue job is queued or running (§6.8);
- a land is held (§8.1);
- a worker has commits not on its landing target, unless the report names that branch as
  abandoned (the human then sees it in the report, and the branch is kept);
- a criterion has no receipt that verifies.

On accept, maiTerm posts the report in the coordinator's column, records `done_at`, sets the
Hive's `status` to `done`, stops the stack services **the Hive started** (never one it only
references through `shared_from`, §6.8), cancels the Hive tabs' follow-ups and watch scripts,
and exits the coordinator. A Done Hive has no running agents, no watchers and costs nothing.

**Waking it.** A message from the human to a Done Hive's coordinator column relaunches the
coordinator through the launch builder (§11.1), resuming its session, and delivers the message
through `deliver_to_agent` once its first prompt is up. The Hive's status goes back to `active`.

## 12. Not in v1

- **SSH Hives and container workers** (workers on a remote host, or each in its own Apple
  `container` VM). One piece of work: maiTerm treats a container as a local remote host,
  reached by `container exec` instead of `ssh`, over the remote-agent path that already exists
  (the bridge, the §6 login handoff, the transcript mirror). Local and shared first (§6.8).
- **Mixed runtimes.** v1 is Claude only, because shared memory rides on Claude's
  `autoMemoryDirectory` and folders on `--add-dir`. Codex workers need their own paths for both.
- **Scheduled work in a Hive** (Projects' scheduled tasks). Follow-ups cover one-offs already.
- **Hive templates** (save a Hive's settings to start the next one).
- **Spreading load across accounts.** v1 spawns every agent the way a new tab would (§14.15).
  Sharing a Hive's load across several logins or keys is a later idea, which has to respect each
  plan's terms.
- **Hives on native Windows.** Claude has no sandbox there, and a Hive never runs unsandboxed
  (§14.16), so the Create sheet refuses. Running workers under WSL is the later option.
- **A gallery across all Hives.** Each Hive's gallery stands alone in v1; one window-wide view
  over all of them (Higgsfield's single library) is a later read over the same stores.

## 13. Spikes before building

Each is an hour or less, and each can change the design:

1. **`--settings` carries `autoMemoryDirectory`** on an interactive launch, under a managed
   account, and memory writes don't prompt in auto mode. Then suspend and resume the worker,
   restart maiTerm and reload the tab: each relaunch comes through the launch builder (§11.1),
   and `/memory` still shows the Hive's directory, the add-dirs are present and the sandbox is on.
2. **Trust write** suppresses the dialog for a maiTerm-made worktree under a managed account
   (§6.6). Then, with a live Claude session rewriting its `.claude.json` at the same time, the
   entry survives; and after an account switch, a respawn in the same worktree still comes up
   without the dialog, because the entry is in every root and re-verified before launch.
3. **Fixed hidden PTY size**: a hidden tab spawned at, say, 120×40 never gets a resize from
   anywhere. Known today: a hidden new tab spawns at 80×24 (agent-requests memory). Then, with
   the `fixedSize` mode (§4.5), show, hide and drag-resize Show terminal's column on a streaming
   worker and change the font size: `resize_pty` is never called.
4. **Build-cache cost**: time for a fresh worktree of this repo to reach a passing `npm run check`
   and `cargo check`, with and without the APFS-cloned `target/`.
5. **Concurrent memory writes**: two agents writing memory at once, with the index rebuild
   running, loses nothing. Run it with real agents saving through Claude's native auto memory, so
   their own `MEMORY.md` pointer lines are absorbed into the generated index and never duplicated
   or dropped (§9).
6. **`--add-dir` folders**: with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`, an added
   worktree's `CLAUDE.md` loads, and auto mode writes there without prompting.
7. **Thumbnail cost**: `qlmanage -t` and an ffmpeg frame strip on a 4K, 2-minute video; both must
   stay well under the time an agent takes to make the next asset.
8. **The machine budget**: memory and CPU of one idle and one busy Claude session, so the
   default worker cap and the pressure thresholds come from numbers, not guesses (§6.8).
9. **The PreToolUse deny** stops a worker's direct `cargo test`, and the worker then uses
   `hiveRun` rather than finding a workaround (§6.8). The deny **fails closed**: with maiTerm
   stopped, and with it answering slower than the hook's timeout, the call is blocked, not let
   through. The bypasses are tried: `sh -c "cargo test"`, a script file, an alias, and the same
   for `git push` (§7.3).
10. **The sandbox with media tools** (§6.7, §14.16), on the exact settings of §14.16: a
    sandboxed worker reaches a hosted generator's API and its MCP servers (the proxy as an MCP
    server, not by `curl`), and runs ffmpeg on files in its own folders, with the allowlist built
    from the Tools row. It also checks:
    - a commit in an `--add-dir` (secondary) repo succeeds, and `npm ci` and `cargo build` use
      the shared caches;
    - a write to a repo's `.git/hooks/` or `config`, or to `.claude/settings.json`, is refused;
    - a host not on the allowlist ends as a worker blocker naming it, never a retry outside the
      sandbox, and an unavailable sandbox fails the launch rather than running unsandboxed;
    - localhost is unreachable until the per-Hive widening is on.
    Gates step 4 (§16).
11. **Launching on a tier**: a hidden worker tab starts Claude on a chosen model and effort
    (`--model`, and effort through `--settings` or the first `/effort`), and the transcript
    confirms both (§7.9).
12. **Router on real tasks** (§7.9). **First run done 2026-10-23**: results at
    `/private/tmp/claude-503/hive-spikes/router/results.md`.
    - **What it used:** 33 of this repo's public commits, each treated as a brief, sent to
      `jev-latest` (`jev-1.14.0`).
    - **First draft:** 11 of 33 tiers matched a human label. The low-confidence bump caused most of
      the misses.
    - **With the changes now in §7.9:** 22 of 33, with none off by two tiers. Those changes are the
      reworded `ambiguity`, no bump, `reasoning_depth` ≥ 2.9 → Max, and `is_design`.
    - **Latency and cost:** p50 157 ms, p95 199 ms; about 800 input tokens a call.
    - **Caveats:** a commit message is written after the work and usually states the fix, so it
      reads easier and better specified than a real brief. The tuned rules were also fitted to the
      same 33 labels.
    - **No second offline run (decided 2026-10-23).** A re-run on task-board briefs would only
      have measured agreement with one person's tier labels, and it meant sending board text,
      including unfixed security findings, to TypeSafe before routing is even on. The
      out-of-sample check is the routing log instead (§7.9 "It learns from outcomes"): every
      routed spawn records the router's answers next to the real outcome (first-attempt pass,
      attempts, tokens, time). Before trusting the default weights, read that report after the
      first few dozen routed workers.
13. **The coordinator, restricted** (§7.1): launched with its sandbox profile and tool set, it
    can still plan, spawn, read a worker's diff, review and land, and cannot run general Bash,
    WebFetch, or write outside its home and memory folder.
14. **Gallery and memory reads**: under a fresh account, a worker reads a gallery asset and the
    Hive's memory directory without the outside-read prompt (the gallery `--add-dir`, §6.3), and
    a write into the gallery is refused.
15. **No maiTerm tool reaches unsandboxed execution**: from a worker tab, try every maiTerm tool
    that runs something (`hiveRun` with metacharacters or an unlisted command, `createService`,
    `startService`, `restartService {on}`, setup, a land check, a follow-up or watch script); each
    is refused or runs under the worker's sandbox profile (§6.8, §7.2).

### 13.1 Results so far (2026-10-23)

**Spike 3, by reading the code. The hidden size is set by four things:**
- the mount fit (`TerminalPane.svelte:579-597`) runs even when the tab isn't visible;
- a tab with no slot spawns at xterm's 80×24, or at its last saved size;
- an approved `createTab` becomes the pane's ACTIVE tab (`commands/workspace.rs:360`), so in the
  active workspace it isn't hidden at all;
- `resize_terminal` (`commands/terminal.rs:242`) is the only caller of `resize_pty`, and it's
  reached only from `TerminalPane`.

The paths that call it:

| Path | Lines in `TerminalPane.svelte` | Gate |
|---|---|---|
| `attachToSlot` | 480-491 | `visible` |
| Reattach refit after a window reload | 956-966 | **none** |
| ResizeObserver | 1188-1197 | `visible` |
| Visibility effect | 1462-1472 | `visible` |
| Font, cursor or theme change | 1537-1559 | `visible` |

So `fixedSize` must:
- skip `fitWithPadding` and all five `resizeTerminal` calls (one guard, ideally one `fitAndSync`);
- not create the ResizeObserver;
- spawn at the launch record's size;
- on reattach, adopt the grid's size without resizing.

SplitPane's `visibleTabs` (`SplitPane.svelte:51`) must leave out `hive_role` tabs, or its slot wins
the portal. As a backstop, Rust locks the size of a Hive PTY and `resize_pty` refuses it with a
warning. The build step's acceptance test:
1. log every `resize_pty`;
2. spawn at 120×40 and stream;
3. open, hide and drag Show terminal; switch workspace; toggle Files and the sidebar; change font
   and theme; reload the window and the tab; suspend and resume; restart maiTerm;
4. expect zero `resize_pty` lines for that PTY and no duplicated blocks, with an ordinary tab as the
   control.

**Spike 15, by reading the code:** every path an agent tab can use to make something run, and the
acceptance test for steps 4 and 5b. Each row is called from a worker tab, both as itself and with
a foreign `tabId`, and must be refused or seen running under the sandbox profile.

| Path | Today | A Hive worker |
|---|---|---|
| `createService` | no approval; `auto_start` defaults to true, so it runs on the next activation | refused |
| `updateService` | can rewrite a human-defined service's command, env or cwd | refused; "human-defined" also needs a never-agent-edited flag |
| `startService` / `restartService` | types the start line into an unsandboxed shell | only human-defined services on its own branch, through the queue, in a sandboxed service PTY |
| `startStack` / `stopStack` | | refused |
| Watch scripts | run as maiTerm, unsandboxed, after human approval | refused in v1, or sandboxed with approval still required and the unattended waiver ignored |
| `createTab`, `restoreArchivedTab`, `switchTab` onto an unactivated tab | each replays a stored auto-resume | refused |
| `setPreference` (e.g. `windows_shell`), `addMcpServer`, mesh sends | | refused |
| Overlord tools | `driveTab`, `answerTabPrompt` and `recoverTab` can still TARGET a Hive tab | refuse a Hive target, or route it through the launch builder and `hiveAnswerWorker` |
| maiTerm's own git on worker content | | passes `-c core.fsmonitor=false -c core.hooksPath=/dev/null` |

Found beyond Hive:
- **Fixed in `fb9f1c2f`:** `setAutoResume` and `setTriggerVariable` accepted any `tabId`, so any
  agent could plant the command another tab replays.
- **Still open, on the task board:**
  - `updateService` can rewrite a human service;
  - the stack verbs accept a foreign `tabId`;
  - trigger variables are interpolated into commands unescaped.

**Spike 12:** see item 12 above and §7.9.

**Spikes 1, 5, 6, 11 and 14** were run on Claude Code 2.1.309 with `claude -p`, plus one
interactive run through `expect`.
- **1, the memory folder: passes.**
  - `--settings '{"autoMemoryDirectory":…}'` puts memory there with no prompt in every
    permission mode. Claude exempts that folder natively.
  - Resumed WITHOUT the settings, the recorded system prompt still names the Hive's folder. The
    write exemption is gone, though: manual and acceptEdits deny the write, and auto allows it.
  - After the first `/compact`, the agent switches to the per-repo default.

  So §9's leak starts at the first compaction, not at resume, and the launch builder re-passing
  the settings (§11.1) is still required.
- **5, two writers at once: partial.**
  - Two sessions both creating `MEMORY.md` with Write lost one session's index lines. A Write
    to a file that doesn't exist yet gets no "modified since read" check.
  - Once the file exists, Edit's stale-read check made the second writer re-read, and nothing
    was lost.

  So **maiTerm creates `MEMORY.md` (with its fences) when the Hive is created**, and the index
  rebuild stays. Claude's own formats: index lines are `- [Title](file.md) — hook`; files carry
  frontmatter `name`, `description`, `metadata {node_type, type, originSessionId, modified}`.
  `originSessionId` is agent-editable, so §9's Rust-owned provenance stands.
- **6, `--add-dir`: passes.**
  - The added worktree's `CLAUDE.md` loads only with `CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD=1`.
  - Write and Edit there work under acceptEdits.
  - A commit in the secondary repo needs `git -C <wt> …`. `cd <wt> && git …` is refused by
    Claude's own "changes directory before a version-control command" check.
  - **Push denies:** `Bash(git push:*)` and `Bash(git -C * push:*)` did NOT stop
    `git -C <wt> push`. `Bash(git * push*)` does. §7.3's deny rules use the space-wildcard form,
    because a `:*` mid-pattern matches nothing.
- **11, model and effort: passes.**
  - `--effort low|medium|high|xhigh|max`, or the setting `effortLevel`.
  - The transcript records `.message.model`, `"effort"` and `perTurnEffort`.
  - On resume the model is kept but **effort falls back to medium**, so the launch builder passes
    `--effort` on every relaunch.
- **14, reads and gallery denies:**
  - Reads from the gallery `--add-dir` work.
  - In auto mode an outside read did NOT prompt, in `-p` or interactively, on this account and
    under `/private/tmp`. So the claim that the first outside read prompts even in auto mode
    didn't reproduce. The gallery `--add-dir` still matters for manual and acceptEdits workers.
  - **A deny rule needs the `//` form for an absolute path.** `Edit(/abs/**)` did nothing;
    `Edit(//abs/**)` blocked Write and Edit and still allowed Read.
  - Bash `cp` into the denied folder was also refused, but a `python3 -c "open(…,'w')"` got
    through. So the sandbox's `denyWrite` is required, not optional.
- **A side effect to know about:** these `-p` runs loaded the user's settings hooks, so the
  installed maiTerm received hook events, including permission requests, from about 35 scratch
  sessions. Later spikes run with `disableAllHooks` unless hooks are what they test.

**Spikes 9, 10 and 13** were run on Claude Code 2.1.309 with `claude -p --setting-sources project
--strict-mcp-config`. Logs and settings are in `/private/tmp/claude-503/hive-spikes/enforce/logs/`.

- **9, the PreToolUse deny: partial.**
  - **Fail-closed works.** With `onFailure:"block"`, a closed port, a timeout and an HTTP 500 all
    blocked the call. Without it, the call ran.
  - **A cooperative model stops**, and the obvious wrappers are caught: `sh -c`, `bash -lc`,
    `env X=…`, aliases, full paths. Sonnet refused to route around the deny and named the bypasses
    instead.
  - **It isn't a wall.** These ran anyway:
    - a split string, `c=car; ${c}go test`;
    - `base64 -d | sh`;
    - `cargo t`;
    - a script written with the Write tool and then run, where the hook sees only `./t.sh`;
    - `git send-pack`.

    `permissions.deny` on `git push` let `sh -c 'git push'`, `git -C . push` and
    `git -c x=y push` all reach a local bare remote.
  - **The sandbox breaks localhost http hooks.** With the sandbox on, every `type:"http"` hook to
    127.0.0.1 fails, so `onFailure:block` blocks every Bash call, and PostToolUse/Stop events never
    arrive. maiTerm's own global hooks are `type:"http"` (`lockfile.rs`), so in a sandboxed tab they
    would fail silently.
  - **What works is a `type:"command"` hook.** Hooks run outside the sandbox. A maiTerm-owned script
    that forwards to maiTerm and exits 2 on any failure works with the sandbox on, with no
    allowlist entry, and fails closed.

    The alternative is to allowlist exactly `127.0.0.1:<port>`. Bash can then reach that one port
    through the proxy, so it needs auth, and it must never be the MCP or maiLink port. Bare
    `127.0.0.1` opens every port.
- **10, the sandbox: partial.**
  - **The key names in §6.7 and §14.16 are right.** `strictAllowlist` is honoured from `--settings`.
  - **The host allowlist works.** A host off the list gets a 403 and a `<sandbox_violations>`
    note, with no retry and no prompt. Without `strictAllowlist`, auto mode widened
    `allowed_domains` per call and got through.
  - **WebFetch escapes it.** In auto mode WebFetch fetched an unlisted host despite an allow rule
    for one domain. `deny:["WebFetch(domain:*)"]` also kills the sandbox's network. So: remove
    WebFetch from workers that don't need it, or put a PreToolUse hook on it.
  - **The blocked-host detector must be a `PostToolUseFailure` command hook.** PostToolUse doesn't
    fire for the failure, and http hooks don't arrive.
  - **Repo internals:**
    - The **primary** repo's `.git/hooks`, `config` and `.claude/settings.json` are protected by
      Claude.
    - A **secondary** (`--add-dir`) repo's are NOT. Bash wrote its hook and config, and the Write
      tool overwrote its `.git/config`. An explicit `denyWrite` on `<repo>/.git/{hooks,config,info}`
      blocks this, and commits there still work.
  - **The Write tool ignores the sandbox.** In auto mode it wrote `.claude/settings.json` and
    another repo's `.git/config` with no prompt. A `Write(//abs/**)` deny rule blocked it. So the
    Write/Edit deny rules for `.claude/**`, `.mcp.json` and every repo's `.git/{config,hooks/**,
    info/**}` are mandatory.
  - **`/tmp/claude-<uid>/` is shared and writable by every sandboxed session.** Sandboxed Bash gets
    `TMPDIR` there, which makes it a cross-worker channel. Worktrees and scratch must never live
    under it, and a worker's temp must be its own folder. This also meant the outside-cwd and
    secondary-objects checks couldn't be tested cleanly. **They need a re-run with a scratch root
    outside `/tmp/claude-<uid>`.**
  - **Other results:**
    - Localhost is closed by default, and `allowLocalBinding:true` opens every port.
    - ffmpeg works inside the working dir.
    - Commits in the primary and secondary worktrees work in auto mode.
    - `failIfUnavailable` couldn't be proven: the availability check couldn't be made to fail on
      macOS.
- **13, the restricted coordinator: fails as §7.1 specifies it, passes with the corrected
  settings.**
  - **`--tools` takes tool names only.** `Bash(git diff:*)` there silently dropped Bash.
  - **`autoAllowBashIfSandboxed` defaults to true.** Any sandboxed Bash command then ran regardless
    of the git allow rules, and auto mode let Write go anywhere.
  - **What worked:**
    - `--tools "Read,Grep,Glob,Bash,Write,Edit" --permission-mode dontAsk --add-dir <home>`;
    - sandbox `{autoAllowBashIfSandboxed:false, allowWrite:[<home>], denyWrite:[<repos>],
      network:{allowedDomains:[], strictAllowlist:true}}`;
    - `permissions.allow` for `Bash(git diff *)`, `Bash(git log *)`, `Bash(git show *)` and
      home Write/Edit;
    - `deny` for WebFetch and WebSearch.

    `dontAsk` is what makes the allow list the whole list.
  - **Still to add:** `mcp__maiterm__*` allow entries (dontAsk denies unlisted tools), and either
    a cwd in the repo or `Bash(git -C * diff *)`-style rules for reviews. AskUserQuestion is always
    denied in dontAsk, which fits the blocker-only rule.

**What these spikes change in the spec** (now applied to the sections themselves):
- **§6.8 "It's enforced":** the deny is a `type:"command"` hook, outside every writable path and
  on `denyWrite`, that exits 2 on failure. It's a speed bump plus an audit log, NOT a guarantee:
  drop "doesn't route around it". The real limits are the sandbox and the resources.
- **§11.1:** every hook in a sandboxed Hive tab is command-type, including maiTerm's state,
  permission-ledger and PreModelSwitch hooks. Otherwise they fail silently.
- **§7.3 push:** no deny rule stops a push. What stops it is that no remote is reachable: network
  remotes are off the allowlist, and a local-path remote is under `denyWrite`.
- **§6.7:**
  - Write/Edit deny rules are mandatory.
  - Secondary repos get an explicit `denyWrite`.
  - WebFetch is removed or hooked.
  - The blocked-host detector is a `PostToolUseFailure` command hook.
  - `/tmp/claude-<uid>` is shared: worktrees and scratch never live there.
  - Localhost is opened by port, never by host.
- **§7.1:** the coordinator's launch settings are the working set above. Note that `--tools`
  doesn't take patterns.

**Spikes 4, 7 and 8** were measured on an M1 Ultra with 128 GiB and 20 CPUs, with 69 Claude sessions
running. Load average was 25–87, but CPU was 67% idle.
- **4, build caches:**
  - `npm ci` on the shared cache takes about 2 s, and `npm run check` 9.5 s.
  - `cargo check` takes about 50 s cold (about 250 CPU-seconds, 1.3–2.4 GB).
  - **Cloning the whole `target/` is worse than cold.** The main target is **330 GB and about 790k
    files**. `cp -c` took 279 s, and the first check on the clone took 101 s.
  - **A slim seed wins.** `cp -c` of only `debug/.fingerprint`, `debug/build` and
    `deps/*.{rmeta,rlib,dylib,d}` takes 40 s, then 23.7 s to the first check. Only the crate
    itself rebuilds, because its fingerprint includes the worktree path.
  - **§6.5 uses the slim seed**, ideally from a check-only target dir.
  - **Hive worktrees go under a `.noindex` folder**, because Spotlight indexed the new tree at
    about 100% CPU.
- **7, thumbnails:**
  - `qlmanage -t` takes 0.15–0.4 s per asset, but gives audio only a generic icon. Waveforms come
    from ffmpeg `showwavespic` (0.12 s).
  - A 10-frame hover strip takes 3.6 s with a full-decode `fps` filter, 1.4 s with ten parallel
    seeks, and 0.47 s with `-skip_frame nokey`.
  - §10.5 uses seeks or keyframes, never a full decode. Thumbnail cost isn't a concern.
- **8, the machine budget:**
  - A Claude process is about 400 MB, but each session also runs a median of 7 MCP-server children
    (about 480 MB). Session plus children: median 0.9 GB, p90 1.1 GB, long sessions about 2 GB.
  - Idle sessions use about 0% CPU. The cost is what they run.
  - So, in §6.8:
    - the default worker cap is 4, scaling at about one worker per 16 GB above a 32 GB reserve
      and no more than ncpu/4: the ceiling is `max(1, min((RAM−32)/16, ncpu/4))`, which is 5 on
      this Mac (128 GB gives 6, but 20 CPUs give 5; an earlier draft said 6);
    - workers launch with `--strict-mcp-config` and only the MCP servers they need;
    - start nothing new at `kern.memorystatus_vm_pressure_level` ≥ 2 or under 25% free, and drop
      heavy resources to one at level 4 or under 10% free;
    - **use CPU idle, not load average** (80 with 67% idle here). Cargo's capacity of 2 stands.

**Spike 2, the trust write: passes, with a correction.** Claude Code 2.1.309, under a managed
account, with `expect`, 68–69 live sessions.
- **The write works.** `projects["<realpath>"].hasTrustDialogAccepted = true` in the account's
  `.claude.json` suppressed the dialog.
- **Claude reads trust only from `$CLAUDE_CONFIG_DIR/.claude.json`.** An entry only in
  `~/.claude.json` didn't count.
- **Entries are keyed by realpath.** A symlink-path key didn't count, so maiTerm canonicalizes the
  path before writing.
- **The entry survived** 3 minutes, a session that loaded the config before the write and saved on
  exit, and a storm of 6 sessions (8 rewrites in 30 s, 0 of 30 samples lost). Claude 2.1.309
  re-reads and merges rather than writing back a stale copy. Keep the change-detecting
  `update_file` helper for the small unlocked race.
- **Correction: trust on a main checkout covers ALL its linked worktrees.** Claude maps a worktree
  to its main repo, so a sibling worktree had no dialog with only the repo trusted. The reverse
  isn't true. A worktree session also files its project data under the main repo's key, and
  creates that key itself with trust false. What follows:
  - For a repo the human already trusts (the usual case), **maiTerm needs no trust write for its
    worktrees at all.** §6.6's writes matter only for Hive folders that aren't worktrees of a
    trusted repo, such as the Hive's own folder and a scratch-only worker.
  - **A reviewer's worktree of an unlanded branch is trusted too.** So §6.6's "no trust entry for a
    reviewer's worktree" doesn't stop the dialog being skipped there. The safeguard is launching
    reviewers with `--setting-sources user`, so that branch's project hooks, settings and MCP never
    load, plus the mandatory Write/Edit deny on `.claude/**` and `.mcp.json` for every worker
    (spike 10).
  - Removal deletes only the keys maiTerm wrote, never Claude's own repo entry.
- **Each account root needs its own entry where one is needed.** The seed-once merge in
  `accounts/mod.rs` carries only `mcpServers`, not a later trust write. A respawn after an account
  switch wasn't exercised.

**Spike 10 re-run**, with scratch outside `/tmp/claude-<uid>` (git 2.54.0, macOS Seatbelt):
- **Writes outside the working folder are refused** by default in auto and default mode
  (`Operation not permitted`), including to `$HOME`.
- **An `--add-dir` repo of the worker's own is writable**, and commits there work with no
  `allowWrite`. Its `.git/hooks`, `config` and `info` are writable too, so each one needs an
  explicit `denyWrite`.
- **A linked worktree whose main repo isn't otherwise writable** needs, for worker branch B and
  worktree N:
  - `allowWrite` `<main>/.git/worktrees/N`, `<main>/.git/objects`, `<main>/.git/refs/heads/B`,
    `<main>/.git/refs/heads/B.lock` and `<main>/.git/logs/refs/heads/B`;
  - `denyWrite` `<main>/.git/{hooks,config,info}` and `worktrees/N/{commondir,gitdir,config.worktree}`.

  Use per-file branch entries, never the `refs/heads` folder, so a worker can't move main or
  another worker's branch. Leave out `packed-refs`: the commit still succeeds, with a harmless
  error. `objects/` has to be writable, so a worker writes objects into the shared main repo.
  maiTerm creates the branch before launch, since slashed names need their parent folder.
- **A worker can't get its own temp.** Sandboxed Bash always sees `TMPDIR=/tmp/claude-<uid>`,
  whatever is set. Denying that folder breaks every Bash call, because Claude writes its cwd file
  there. `CLAUDE_CODE_TMPDIR` moves only Claude's own state (tasks, sockets), which is still worth
  setting per worker. Plain `mktemp` fails in the sandbox, so briefs say `mktemp -p "$TMPDIR"`.
  **Temp is shared across workers and is not a boundary**, which §6.7 has to say plainly.

**All 15 spikes are done.** The corrections above are folded into the sections they affect: §6.5,
§6.6, §6.7, §6.8, §7.1, §7.3, §7.7, §9 and §11.1, and with them §3, §6.2, §6.3, §6.9, §10.5,
§14.16 and §16.

**Step 3's acceptance probes (2026-10-23, Claude Code 2.1.310, `claude-haiku-5-5`).** Automated in
`src-tauri/src/hive/probe.rs` as `#[ignore]` tests: `cargo test --lib hive::probe -- --ignored
--nocapture`. Each one lays out a scratch Hive with the real `create_layout` (a scratch git repo as
the primary folder, under the real temp dir's realpath, never `/tmp/claude-<uid>` or the repo),
writes the launch record, and runs the code's own `settings.json`, `mcp.json` and launch line
(`write_launch_files`, `hive_launch_args`, `hive_launch_line`) as `claude -p`, one tool call per
run. The parent session's variables are removed, `MAITERM_PORT` names a closed port, and the hook
script's port file names either a closed port or the probe's own listener, which answers
`/hive/gate` with the real `gate::decide_event` and a SessionStart with the real
`hive::session_started` and invariant rules. Nothing reached the installed maiTerm (no probe tab id
in its log). All four pass:

- **Spike 13, the restricted coordinator: passes, after two fixes to the generator and one to
  the gate.**
  - `init` shows `permissionMode: dontAsk` and exactly the tools `Bash, Edit, Glob, Grep, Read,
    Write`. WebFetch and WebSearch aren't offered at all, so the model can't call them.
  - `ls /`, a `cat` of a file outside its folders and `curl https://example.com` are all denied
    by dontAsk ("Permission to use Bash has been denied…"). One run had the model add
    `allowed_domains` to the curl; the sandbox refused that, because `strictAllowlist` makes the
    configured list the whole list.
  - `git -C <read copy> diff` returns the diff, and `git -C <copy> log --oneline -1` works.
  - **The fix:** at first the bare `git -C <copy> diff` was denied. A trailing ` *` is optional
    only in a pattern with no other wildcard: `Bash(git diff *)` allows a bare `git diff`, but
    `Bash(git -C * diff *)` needs an argument after `diff`. The generator now also emits
    `Bash(git -C * diff)`, `… log)` and `… show)` (§7.1). Quoted and unquoted paths both match.
  - A Write outside home is refused by the gate ("outside the folders this agent may write"), and
    the file never appears. A Write in home succeeds.
  - `git -C <copy> remote -v` and `git -C <copy> push` are refused by the gate with its own
    reasons, and both refusals land in `logs/gate.log`. The PreToolUse gate runs before
    Claude's permission check: it saw every Bash call, including the ones dontAsk then denied.
    In one run the model declined to push without making a call at all, citing the invariant
    rules from the SessionStart priming. That shows the command hook's `prime=1` reply reaches
    its context.
  - **Git ran a command, until the second fix** (found in review, then probed:
    `spike_13_git_runs_no_command_of_the_agents`). "Cannot run general Bash" didn't hold
    through git. The `*` in `Bash(git -C * diff)` spans `-c core.fsmonitor=<cmd>`, and the gate
    refused only `-c alias.*`, so `git -C <copy> -c core.fsmonitor='touch …' diff` ran the
    `touch` inside the sandbox; `diff.external` and a repo written in home do the same. Now:
    - With the real gate: the `-c core.fsmonitor` call, `git -C <home>/planted diff` (a repo
      the probe planted with an fsmonitor) and a plain `git diff --stat` from home are all
      refused by the gate, and no marker file appears.
    - With the gate answering `allow` to everything, so only the generated rules decide: the
      `-c core.fsmonitor` call and `git -C <copy> diff --no-index <outside file> /dev/null` are
      denied by the new deny rules (`settings::GIT_ESCAPE_DENIES`). But the planted repo's
      fsmonitor **ran**, through the `Bash(git -C * diff)` rule. And with the plain rules
      removed as an experiment, `git diff --stat` from home still ran: Claude's own read-only
      check allowed it with no rule. Which repo git runs in is therefore held by the gate
      alone, which fails closed (§7.1).
  - maiTerm's MCP server shows `failed` here, which is expected with the port closed. That MCP
    works from a sandboxed coordinator was measured separately (`listWindows` over
    `127.0.0.1:${MAITERM_PORT}` with `${MAITERM_AUTH}` expansion), so the probe doesn't call the
    installed app.
- **The gate fails closed: passes.** With nothing listening, `git -C <copy> push`, the ordinary
  `git -C <copy> diff` and a Write in the coordinator's own home were all blocked with the
  script's "the Hive gate fails closed" message. Write is on the gate's matcher, so **with
  maiTerm down the coordinator can't write at all, its memory included**. That is the intended
  trade.
- **Spike 1 under dontAsk: passes, across a resume and a `/compact`.**
  - A memory save went to `<hive>/memory/probe-codeword.md` (Claude's frontmatter format), with
    no prompt and nothing in the account's per-project memory folder. The Write passed the gate,
    whose write roots include `memory/`.
  - The real SessionStart path recorded the session in `tasks/coordinator/sessions.json`. A
    relaunch built from `last_session` (`--resume <sid>`, same session id) ran `/compact`, and
    the stream showed a `compact_boundary`. A second save after that still landed in the Hive's
    `memory/`, not in the per-repo default. So the builder re-passing `--settings` on every
    launch closes §9's compaction leak.
- **Step 4b's worker probe: passes, after one fix to the generator**
  (`hive_worker_launch_probe`, `claude-haiku-5-5` on 2.1.310). It sets up a scratch Hive with `web`
  (primary, a bare local remote) and `api`, and a worker with worktrees of both, made by the real
  `create_worker_worktrees`. The launch is the real `worker_record`, `worker_settings_json` and
  line, setting sources included. `MAITERM_PORT` and the port file name the probe's own
  listener, which answers the gate with the real `decide_event`. Both repos check in a hostile
  `.claude/settings.json` (`allowedDomains: [example.com]`, `allowLocalBinding`,
  `excludedCommands: [curl *]`, `allowWrite: [/]`, `disableAllHooks`), which the worktrees carry
  from Base. The results:
  - `init` shows `permissionMode: auto`, and neither WebFetch nor WebSearch is offered.
  - `git commit` in the cwd worktree and `git -C <api worktree> commit` both land on
    `hive/probe--probe-w`. The secondary repo prints the harmless `packed-refs.lock` error §6.7
    predicts.
  - `echo x > <api>/.git/hooks/x` gets `Operation not permitted`.
  - The Write tool's `<wt>/.claude/settings.json` is refused by the deny rule, and a `python3
    open()` of it by the sandbox.
  - `curl https://example.com` gets a 403 and a `<sandbox_violations>` note. The
    `PostToolUseFailure` command hook forwards it, and `parse_violations` names the host.
  - `curl 127.0.0.1:<listener>` never reaches the listener.
  - `git push local` is refused by the gate, and the bare repo gets no ref.
  - SessionStart arrives as `via=hive`, and the real `session_started` marks the worker running.
  - **The fix:** at first both commits failed on `index.lock` in the admin dir, which was on
    `allowWrite`. The cause was the whole-checkout `Edit` deny rule, which Claude also applies
    to the sandbox (§6.7).
  - **A refused write carries no `<sandbox_violations>` note** on macOS, only the program's own
    `Operation not permitted`, so `parse_violations` also reads the absolute path off those
    lines of a failed Bash call: only in a write's shape (a shell redirect, git's `Unable to
    create`, Python's `PermissionError`) and only for a path outside the worker's `allowWrite`
    or under its `denyWrite`, since most EPERMs (a privacy-protected read, a chmod) aren't the
    sandbox's. A refused localhost connect carries no note either. The auto-mode classifier
    sometimes refuses the hook write before the sandbox sees it.
  - **The hostile project file takes nothing:** example.com still gets the proxy's 403, curl
    stays sandboxed, localhost stays closed, the hooks still fire. In `-p` the folder is
    untrusted, so this shows the run, not a trusted worktree; what holds there is that Claude
    2.1.310's settings schema ignores a project file's sandbox values once `--settings` sets
    `allowUnsandboxedCommands: false`, and `strictAllowlist` drops its `allowedDomains`.
- **Review fixes after the 4b probe** (all in the generator; the probe re-passed):
  - **Workers load project settings only**, like the coordinator (plan decision 12 assumed
    `--settings` outranks everything, but Claude merges the sandbox's lists across sources and
    honours a USER file's `allowLocalBinding` and `allowedDomains`). Each switch that weakens the
    sandbox is pinned off in `--settings` (`network.allowLocalBinding`, `allowAllUnixSockets`,
    `allowAppleEvents`, `allowNestedUserNamespaces`, `enableWeakerNestedSandbox`,
    `enableWeakerNetworkIsolation`, `filesystem.disabled`, `disableAllHooks`), and the remotes'
    hosts go on `network.deniedDomains`, which beats any allow from any source.
  - **The coordinator's git roots are the repos maiTerm made**: its read copies, each worker's
    recorded worktrees and the human's folders, never all of `wt.noindex/`, where workers now
    write. A git base under a nested `.git` or git dir below its root is refused, as is
    `--git-dir`: a repo a worker planted there ran its `core.fsmonitor` as the coordinator.
  - **A whole-folder denial never covers a `.git` the worker commits into** (`deny_around`): a
    Hive folder inside another, or a local remote that is one, is denied entry by entry.
  - **A worker's `CARGO_TARGET_DIR` is decided once**, at its first launch, by whether any of its
    git folders is a Rust project (`worker.json` `cargo_target`), never by whether the human has
    a built target that moment.
  - `createBackup` joins `ESCAPE_MCP_TOOLS`; reads of other tabs' output, maiTerm's logs and
    diagnostics, and comms are denied to workers. A worker's priming drops the Overlord,
    stack and follow-up lines and says its blocker goes to its coordinator.
- **Still by hand** (they need an interactive tab): spike 2's trust dialog under a managed account
  and after an account switch; `/memory` after suspend and resume, a maiTerm restart and a tab
  reload; and hook events still arriving at a real maiTerm through those relaunches.

## 14. Decisions

Decided 2026-10-23:

1. **Spawn approval:** spawns within the cap need no card (principle 5).
2. **Landing:** by default the coordinator lands each worker straight into Base (§8.1). The
   integration branch with human-approved lands is chosen when the Hive is created (§3, §8.3).
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
9. **Autonomy:** the coordinator decides as much as possible; human checkpoints are Hive
   settings; decisions that reach the human become precedents that let the coordinator decide
   them next time, and earn it whole categories (§7.4–7.6). Darryl's direction; the mechanics are
   self-answered.
10. **Undo:** a land log and Undo land, which makes a revert commit, never a reset; a land that
    breaks Base holds further lands (§8.6).
11. **Done:** completion criteria up front, a final report against them, then the Hive goes
    quiet (§11.4).
12. **Retired workers:** their chats stay readable from their cards (§8.4).
13. **Several Hives:** spawn fuses and the quota breaker count across all Hives; budgets stay
    per Hive (§7.8).
14. **Running things:** one shared stack and a run queue, not an environment per worker.
    Infrastructure is shared with a namespace per worker; code-bearing services relaunch on the
    branch they're needed for (§6.9); maiTerm schedules, the coordinator sets priority (§6.8).
    Darryl's direction. Containers are a later per-Hive option, built together with SSH Hives.
15. **Accounts:** every Hive agent spawns exactly as a new agent tab does today, under the
    account a new tab would get, the same as the Overlord (`docs/login.md` §5). No per-Hive
    account picker in v1. Spreading a Hive's load across several accounts is a later idea (§12).
16. **Sandbox:** every Hive agent runs in Claude's sandbox (§6.7), the coordinator included with
    its own profile (§7.1), widened per Hive. Darryl's caveat: **don't kneecap the workers that
    need tools.** So the allowlists are derived by Rust from the Hive's folders and Tools row
    (§10.3), never typed by hand.
    - **Settings**, in each launch's `--settings` so they survive every relaunch (§11.1):
      `sandbox: {enabled: true, failIfUnavailable: true, allowUnsandboxedCommands: false,
      filesystem: {allowWrite, denyWrite}, network: {allowedDomains, strictAllowlist: true}}`.
      Without `failIfUnavailable` a missing sandbox runs commands unsandboxed silently; without
      `allowUnsandboxedCommands: false` a blocked command is retried outside it; without
      `strictAllowlist` auto mode adds hosts per command.
    - **What it covers: shell commands only** (Bash, PowerShell, Monitor). Write/Edit, WebFetch,
      MCP servers and hooks run outside it and are bounded as §6.7's table says (mandatory
      `Write(//…)`/`Edit(//…)` deny rules, mirrored on `denyWrite`, plus the command hook's path
      check; WebFetch removed, or gated by a command hook on its host; MCP by the budget, the
      proxy and `--strict-mcp-config`; hooks by no worker writing `.claude/**` or `.mcp.json` and
      reviewers running with `--setting-sources user`). Every hook maiTerm installs is
      `type:"command"`, since the sandbox breaks localhost http hooks (§11.1). Commands maiTerm
      runs for a worker (`hiveRun`, setup, land checks, services on its branch) get the same
      profile at spawn (§6.8).
    - **Network allowlist:** each detected generator's API host (Higgsfield, fal.ai, Kling,
      WaveSpeed, Kie.ai) and the package registries. **Not** maiTerm's MCP proxy (agents reach it
      as an MCP server, never by shell) and **not** any repo's git remote: a host allowlist can't
      tell a fetch from a push, so maiTerm does any fetch itself (§7.3). Every allowed host is
      also a residual exfiltration channel; the allowlist limits reach, it doesn't make content
      private.
    - **Write allowlist and denylist** (§6.7): writes outside the working folder are refused by
      default. `allowWrite` is the worker's other worktrees and `scratch/`; for each worktree,
      the main repo's `.git/worktrees/<name>`, `.git/objects` and per-file entries for its own
      branch (`refs/heads/B`, `refs/heads/B.lock`, `logs/refs/heads/B`), never `refs/heads` and
      never `packed-refs`; the cargo registry and git dirs, the npm cache, sccache and the
      per-worker target dir. `denyWrite` is every repo's `.git/{config,hooks,info}`, each
      worktree's `commondir`, `gitdir` and `config.worktree`, other worktrees' admin dirs,
      `.claude/**` and `.mcp.json`, the hook scripts, any local-path remote and the gallery; the
      human's checkouts are refused by default and carry Write/Edit deny rules. Temp (`/tmp/claude-<uid>`) is shared by every sandboxed session and
      can't be denied, so it is not a boundary and nothing a worker owns lives there.
    - **Localhost** is closed by default: `allowLocalBinding` and a bare `127.0.0.1` in
      `allowedDomains` each open every local port, maiTerm's 8765 and the proxy included. Local
      services (ComfyUI, a dev server) are reached through MCP or `hiveRun`; a per-Hive widening,
      if ever, opens one port (`127.0.0.1:<port>`), never maiTerm's own.
    - **Media CLIs** (ffmpeg, ImageMagick, sox, Blender) stay sandboxed and work because
      `allowWrite` names the worker's folders. `excludedCommands` is never used: an excluded
      command gets full disk and network.
    - **A block is a blocker.** A `PostToolUseFailure` command hook parsing
      `<sandbox_violations>` turns a sandbox refusal into a blocker
      naming the host or path, routed to the coordinator, which raises its own blocker to the
      human (§7.4); the fix is one click in Hive settings, not a worker working around it.
    - **Native Windows** has no Claude sandbox, so with `failIfUnavailable` a Hive can't run
      there and the Create sheet says so; the conservative choice over an unsandboxed Hive (WSL
      is a later option, §12).
17. **Model routing:** Jev judges each task on narrow dimensions; a visible policy in code picks
    the tier; uncertainty and failures move up; outcomes are recorded so the weights can be tuned
    from evidence. Off until the human turns it on, since briefs go to TypeSafe's API (§7.9).
    Darryl's idea; the design follows the live TypeSafe docs.

**Changed by the 2026-10-23 review.** These override any earlier wording above or elsewhere:

- **A land that touches agent or execution config always goes to the human**, whatever the
  checkpoint (hooks, `.claude/`, `.mcp.json`, CLAUDE.md/AGENTS.md, git hooks, CI, package
  scripts, `.envrc`, `build.rs`), undo reverts included (§8.1, §7.4).
- **Spending past the budget is never delegable**: not a checkpoint, never earned (§7.4, §7.6).
- **The coordinator is sandboxed and tool-restricted** (§7.1); the brief explains, the launch
  enforces.
- **No Hive agent pushes.** Only maiTerm's land path does, when Push after land is on (§7.3,
  §8.5). Item 7 is that setting.
- **`galleryDeliver` writes only into the Hive's plain folders**, new files only; anything else
  is the human's Export… (§7.3, §10.6).
- **Hives are not `overlord_exempt`.** The engine skips Hive tabs by `hive_role`; the stack,
  watch scripts, resume and prompt answering keep working (§7.3). Item 4 is that skip.
- **Precedents live in Rust-owned storage**, recorded only from a blocker the human answered,
  outside every agent's memory directory (§7.5).
- **Landing mode is fixed at creation** and isn't a checkpoint; the landing checkpoint gates
  `hiveLand` within the mode (§3, §8.3). Item 2's setting is chosen in the sheet.

## 15. Prior art: what Hive takes and what it leaves

Studied 2026-10-23. The claims below were read from the repos; the main numbers and the
skip-permissions default were checked by hand.

**agency-agents** (github.com/msitarzewski/agency-agents; MIT; ~163k stars; very active). A
library of 282 persona-style agent prompts in 18 divisions, plus playbooks.
- **Takes:** the protocol layer (the brief, pass, fail and escalation templates, as typed
  fields); a reviewer separate from the builder; three attempts then escalate; "inconclusive is
  a fail"; the task-type → builder + checker pairing table; a condensed role catalogue (§7.7).
- **Leaves:** persona text (identity, "vibe", fictional "memories"); installing the files as
  Claude subagents; its 7-phase "agency" pipeline built on 12–24-week phases; numeric quality
  gates an agent can't measure; its orchestrator's hard-coded roster.

**ruflo** (github.com/ruvnet/ruflo, the renamed claude-flow; MIT; ~79k stars; one main author;
several releases a day).
- **What it actually is:** a coordination ledger (JSON and SQLite) around Claude Code's own
  subagents. Its "agents" are records rather than processes, its consensus is a vote tally in a
  file, and its own 2026-05-29 audit found faked benchmarks, an inert "LoRA" and a reward sign
  bug.
- **Takes:**
  - the never-shrink rule for a curated `MEMORY.md`, and provenance on memory (§9);
  - receipts, and "registered ≠ reachable ≠ healthy ≠ done" (§7.7);
  - a launch fuse checked before spawning, with a quota breaker (§7.8);
  - one owner for shared manifests and lockfiles (§7.7);
  - "learning may propose, never widen its own authority" (§7.8);
  - for worktrees, a namespaced branch per worker and detached worktrees for read-only agents
    (reviewers).
- **Leaves:**
  - agents as ledger rows: maiTerm owns process truth (principle 8);
  - LLM "consensus" and Byzantine voting;
  - one SQLite vector store shared across processes;
  - workers as subagents inside the coordinator's own process, which loses isolation,
    per-worker accounts, cost attribution and visibility;
  - keyword "routing" with fixed confidence numbers;
  - its 108 persona agents.
- **Red flag:** its `hive-mind spawn` declares `dangerously-skip-permissions` with
  `default: true` (`v3/@claude-flow/cli/src/commands/hive-mind.ts:604`), so its Queen runs with
  every permission prompt skipped unless the user opts out. **Hive never launches an agent with
  skipped permissions.** Its agents run in auto mode, under the limits of §7.4.

## 16. Build order

1. Extract `ChatColumn` from `FocusView`; Loom unchanged. Ship alone. **Built** (`b7116db0`).
2. The `isStripHidden` / `isServiceTab` predicates replace the service-only checks (Rust + TS),
   sorted by the §5 call-site table. Narrowed to the swap itself: behaviour-neutral **for service
   tabs** only, since no `hive_role` tab exists yet; the Hive-specific answers (Loom, mesh,
   browser link, account-switch reload) land with the step that first creates Hive tabs.
   **Built** (`dc2949ca`).
3. Hive workspace: `Workspace.hive` (§3.2, landing mode fixed at creation), the sheet (folders,
   stack step, Tools detection for the Tools row), the Hive folder on disk, coordinator read
   copies, the hidden coordinator tab, `HiveView` with the coordinator column only. This step
   also owns, because the coordinator is the first Hive agent launched:
   - the **launch record and launch builder** (§11.1), with every relaunch path for a
     `hive_role` tab going through it;
   - the coordinator's **sandbox profile and restricted tool set** (§7.1), the push denies and
     fail-closed hook (§7.3, §6.8);
   - the command-type hooks (§11.1), and trust written where needed, to every account root
     (§6.6); `navigateToTab`/`openOnTab`, the Hive answers
     of §5's call-site table (Loom, mesh, browser link, account-switch reload) and the lifecycle
     table (§11.2) for Hive tabs; the engine's `hive_role` skip in place of `overlord_exempt`
     (§7.3).
   Not done until spikes 1, 2 and 13 (§13) pass.
   **Built 2026-10-23**, in five parts:
   - 3a `2e95b6b7`: the record, the folder on disk, trust writes and the lifecycle refusals;
   - 3b `d474f767`: the launch builder, the settings, the command hooks and the fail-closed gate;
   - 3c `7988a843`: every relaunch path through the builder, and the Hive answers;
   - 3d `6e25debb`: the Create sheet, `hive_create` and `HiveView` with the coordinator column;
   - 3e: the acceptance probes (`hive/probe.rs`).

   Spikes 1 and 13 and the fail-closed gate pass as automated probes against the real generated
   launch (§13.1). Spike 2's trust write is unit-tested against scratch account roots. Its
   interactive half, the dialog under a managed account and after an account switch, is still
   checked by hand, along with `/memory` across suspend, restart and reload.

   **Downgrade:** an older build drops `Workspace.hive` and `hive_role` on save. The tabs come
   back as ordinary shells with no resume command, and the Hive folder stays on disk.
4. Workers: worktrees per folder, `--add-dir` (the gallery's included), setup, the role
   catalogue, fuses, `hiveSpawnWorker`/`hiveListWorkers`/`hiveReport`/`hiveRestartWorker`,
   static cards, `hiveOwnsPrompt` (§4.7; until `hiveAnswerWorker` ships in step 7 the
   coordinator can't answer a prompt, so the gate returns false and every worker prompt reaches
   the human as today). This step owns:
   - the worker **sandbox settings** of §14.16 (allow/deny lists derived from folders and the
     Tools row, localhost closed, `failIfUnavailable`), the mandatory Edit/Write deny rules and
     the `PostToolUseFailure` command hook that turns a block into a blocker;
   - the **spawn reservation** (§7.8): cap, spawn counts and breaker taken under one Rust lock
     before any worktree exists, released on failure or timeout;
   - every worker launch and relaunch through the step 3 builder.
   Receipts accept **commits, files and assets only**; `checks` waits for 5b. Gated on spike 10
   (and 14); on native Windows the sheet refuses to create a Hive (§14.16).
   **Built 2026-10-24**, in six parts:
   - 4a `3e8c0b56`: `worker.json` (`hive/worker.rs`), a worker's worktrees, `.worktreeinclude` copy and
     slim cargo seed (`hive/worktree.rs`, over the git helpers moved to `hive/git.rs`), and the
     role catalogue (`hive/roles.rs`), listed in the coordinator's brief. Nothing launches a
     worker yet.
   - 4b `bd22543e`: a worker launches through the step 3 builder: `launch::worker_record` (auto mode,
     `--disallowedTools`, its own temp and cargo target), `settings::worker_settings_json`,
     `scratch/brief.md` and the worker's invariant rules, `gate::worker_scope`, the worker branch
     of `session_started`, and `hive/sandbox_events.rs`. Nothing calls it yet but tests and the
     acceptance probe (§13.1).
   - 4c `c9e37a8e`: the fuses and the spawn reservation (`hive/fuses.rs`): cap, hour and day counts and
     the breaker under one `std::sync::Mutex`, the counts and breaker in
     `<app data>/hive/fuses.json` (not the state file: Rust's alone), pending reservations in
     memory with a 120 s expiry and released on drop. The StopFailure wiring reads Claude's own
     words ("You've hit your session limit · resets 3pm (Zone)", 2.1.310): a reset within 24 h
     trips the breaker and tells every other coordinator (the notice and refusal show Claude's
     own minute; the breaker holds to its end). A weekly, monthly, dated or unparseable one, a
     third waited limit on one tab within 24 h (auto-continue read as re-arming twice, counted
     in memory), or a retried 429 (`API Error: …`) on a tab not known to run on a plan (spawned
     under a managed account with a plan read back) pauses that Hive (`status = "paused"`,
     `hive-changed` to its window). **Rust files the human's blocker itself**, in the same
     write: an `action` blocker on a "Hive paused: usage limit" row on the coordinator's tab,
     sent to the tasks store over `mailink-tasks-changed`, because the coordinator is often the
     agent at the limit (a weekly limit is every agent's on that plan). Its coordinator is told
     only when another tab hit it, and told not to ask again. A plan's capacity 429 does
     nothing. The tab that hit the limit is never typed into. `hive_resume` un-pauses and
     closes the row (no button yet). Nothing reserves yet: `hiveSpawnWorker` (4d) does.
   - 4d `264ec00d`: `hiveSpawnWorker` and `hiveListWorkers` (`hive/tools.rs`). A spawn registers
     under one lock, then reserves, makes the worktrees, runs the preflight (Claude's version,
     repo trust), writes the launch record and puts the hidden worker tab and its task row in one
     unsaved write; the window adopts them over the Hive's own request channel
     (`hive-frontend-request`, `hive/rpc.rs` and `src/lib/hive/rpc.ts`, unreachable from MCP), and
     only then is it saved and the reservation committed. Every failure after the reservation
     takes back the tab, row, worktrees, branches, task folder and slot. A worker with unmet
     dependencies waits without a slot. `tools/list` shows Hive tools only to their role, and Rust
     refuses Hive tools to the wrong role, escape and Overlord tools to every Hive agent, and an
     `initSession` that would rebind a Hive agent to another tab. A reload moves `worker.json`'s
     tab; live workers remount when the Hive view does.
   - 4e `54ee4d6a`: `hiveReport` (`hive/report.rs`: commits must be on the worker's branch and not already on
     its landing target when it started; files must exist in its worktrees or scratch, symlinks
     resolved; `assets` and `checks` refused until steps 8 and 5b; `done` needs one), and the
     coordinator's `hiveMessageWorker`, `hiveStopWorker` and `hiveRestartWorker`
     (`hive/control.rs`). A stop marks the worker `stopped` before it signals, and signals only a
     `claude` the terminal shows in front, through `kill_pty_foreground_job`; anything else in front
     (a first launch's setup, with `&& … claude` typed after it) or a front it can't read refuses
     the stop and puts the phase back, since a `stopped` agent would run uncounted. A restart takes
     only a `failed` or `stopped` worker, or a `running` one at its shell's empty prompt (or whose
     tab is gone); one stopped before it ever launched goes back to `waiting` instead. It needs no
     agent in front, takes a reservation with its own slot not counted twice, and types the
     builder's line through the follow-ups' empty-prompt gate (`followUpsStore.relaunchAgent`), or
     mounts the tab, or gives the task a new hidden tab when its tab is gone. A relaunch the window
     never answers puts an exited `running` worker back `failed`, so a late wake types nothing, and
     a new tab's unanswered spawn gets its `hive.unspawn` queued after it. A waiter's launch writes
     `launching` only over the `waiting` record it read: a stop meanwhile stands. Every Hive task write
     is a window verb the tasks store applies (`hive.task`: note, status, blocker), and a rolled-back
     waiter's row is put back (`hive.unspawn` `restore_task`), never removed. Everything maiTerm
     types into a Hive agent goes through `hive/deliver.rs`: the draft-hold gate, and a hold while
     that tab waits out a usage limit (the reset `assess_limit` saw, in memory), re-armed waits
     included, and the draft-hold queue holds maiTerm's own messages through one too; worker text
     is `frame_escape`d (no `⟦`/`⟧` or look-alikes, no line opening `[maiTerm]` after any line
     break a reader honours, U+2028/2029 included, no control or invisible characters).
     Crashes and sandbox refusals are read off a worker's own `via=hive` hooks ahead of the mod
     dedup (`hive/events.rs`, which would otherwise drop that copy for the mod's): a SessionEnd
     (not `/clear`) with the shell back at its prompt, or a PTY that exited on its own, fails a
     `launching`/`running` worker and tells the coordinator (a PTY's worker is found by its
     `worker.json`, since the window closes the tab on `pty-close`, often first); a refused host or path files one
     `external` blocker per target (`violations_seen`) and tells the coordinator it can't widen
     the sandbox. Waiters (`hive/waiters.rs`) are polled after every Hive task write, report,
     stop, crash and resume, one poll per Hive at a time: met = the dependency's task is done (or
     its row gone), failed or dropped blocks with one notice, and a launch refused past its
     reservation backs off five minutes. The worker and coordinator settings also deny the
     Overlord's tools (`OVERLORD_CALLER_TOOLS`). Not yet: a poll when the breaker's reset passes
     (the next task write does it), and the hold for a usage-limit wait is lost on a restart.
   - 4f: static cards and the board. `hive_workers` returns the board (`tools::board_view`, the
     view `hiveListWorkers` also reads, without its git count): each worker's record with its tab,
     dependencies as task ids, and times, plus the headroom. `HiveView` refetches it when its
     window hears `hive-workers-changed` (`emit_to`, never a broadcast), when its tasks change,
     when its Hive's status or its workers' tab ids do, and at the board's `refresh_at` (Rust's
     instant for a change no event reports: the breaker ending, a spawn leaving the hour or day).
     It never polls. Besides the tools, three paths change a worker's view outside a tool call, and
     each emits: a session start that moves `launching` to `running` (`SessionStart.phase_changed`),
     a reload's `remap_worker_tab`, and a tripped breaker (to every Hive). Starting is
     `launching` (or `waiting`) only: a running worker with no hook state, as after a webview
     reload, reads Idle. A Needs-you card always shows its count. A card's lane, blocker, note and agent state are
     read live from their stores (`src/lib/hive/cards.ts`: `cardState`, `sortCards`, `waves`), so
     none of it waits on a fetch. The step 4 states are Needs you (a blocker on a task in Blocked,
     or a permission prompt or AskUserQuestion while the worker is live), Blocked, Working,
     Starting (with its `wait_reason`), Idle, Reported, Failed/Stopped and Suspended. A busy agent
     reads Working even in `reported` or `blocked`, since a coordinator's message sets it going
     without changing its phase. Waves wrap under each other on a narrow board instead of
     scrolling it sideways. A card has no buttons: a click sends `HIVE_OPEN_COLUMN` (with the
     task id, since a waiting worker has no tab), which focuses the card until worker columns
     arrive in step 5. `hiveOwnsPrompt` (TS: `notificationDispatch.dispatch`, fed a `kind` by
     agentState's permission, question and compaction notices and by command completion) and
     its Rust twin `hive::owns_prompt` (`ring_devices`, which every doorbell path goes through)
     return false until step 7. Not yet: a watch script waiting doesn't count toward Needs you.
5. The activity feed, diff stat, worker columns, raw terminal (`fixedSize`, spike 3), keyboard.
   **5a built** (the pushed feed, Rust only, nothing renders it yet): `hive/diffstat.rs` measures
   each worktree (merge base with `refs/heads/<target>`, else `base_sha`; total, committed,
   untracked) on the git dir `worker.json` recorded, never the worktree's own `.git` file, which
   is the worker's to rewrite; with `--no-ext-diff --no-textconv --no-color`, `LC_ALL=C` and
   `diff.autoRefreshIndex=false`, since git 2.54's `git diff` wrote the refreshed index back with
   `GIT_OPTIONAL_LOCKS=0` set (a test holds the index byte-identical). `hive/feed.rs` holds the
   per-key `Debounce` (the first touch sets the deadline, one compute per key at a time, a touch
   during one queues one rerun) and the worker meta (`mailink::tab_agent_meta`: `getTabMeta`
   without the `pending_queue` scan). `events::observed` routes through `feed_actions`:
   PostToolUse(Failure) on Edit/Write/MultiEdit/NotebookEdit/Bash/PowerShell → diff stat (5 s),
   Stop → both, SessionStart → both (meta 2 s); an accepted report touches the diff stat, a
   rolled-back spawn forgets both. Pushed as `hive-diff-stat` / `hive-worker-meta` by `emit_to`
   only when changed; `hive_diff_stats` / `hive_worker_metas` answer the view's mount from the
   cache, computing a missing entry once. No timer exists for an idle worker.
5b. The stack feature first: any service on any branch (§6.9: `Tab.service_target`, re-rooted
   cwd, placeholders, Run on ▸, copies per branch, `shared_from`). Then the Hive run queue:
   namespaces, job kind, `hiveRun`, leases, the PreToolUse deny, pressure-aware capacity, and
   jobs under the worker's sandbox profile (spikes 9 and 15). `checks` receipts by job id
   (§7.7) arrive here. For maiTerm as the pilot repo, the `tauri:dev` data-folder and port
   override comes first. No batching yet: it needs landing.
6. Reviewers (`hiveVerdict`, three attempts, escalation). Landing: land to Base (default)
   pinned to the reviewed sha, including several repos, the config-touching land rule (§8.1),
   the approval mode (chosen at creation, §3) through agent requests; the **land log and Undo**
   (`hiveUndoLand`, §8.6); then **batched land checks** over workers already passed to
   `hiveLand` (§6.8); retire; delete flow.
7. Shared memory: `autoMemoryDirectory`, index rebuild, Memory view; checkpoints,
   `hiveAnswerWorker`, precedents in Rust storage, the decision log and earned autonomy.
8. Gallery: store and index, `galleryAdd`/`List`/`Get`/`Tag`/`Collect`, thumbnails, the grid
   and detail view; then compare, Iterate/attach, collections, deliver (plain folders only,
   §7.3). The **budget**: recorded spend per paid render, the header total and the
   stop-and-ask blocker (§10.3). The sandbox allowlist extends to each detected generator's host.
9. Done and lifecycle: `hiveFinish` and waking a Done Hive (§11.4), queue state across
   suspend/resume/restart (§6.8), phone polish (Hive tabs already appear as chats; the gallery
   feed later).
