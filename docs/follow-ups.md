# maiTerm Follow-ups — an agent's own way to pick the work back up later

> Status: **steps 1–5 built** 2026-09-30 (time, stack and task triggers, delivery, MCP tools,
> the human side, resume-then-deliver); only usage-limit continuation remains (§9, §10). Proposed 09-26. Owner: Darryl.
> Lives under the Overlord: live only when the Overlord and **Enable follow-ups** are both on
> (§4). Not yet released.
> Scope: an agent (or its human) schedules a prompt to be delivered back into **its own tab**
> later — at a time, or when something in the workspace happens. maiTerm holds the schedule,
> so it works for every runtime, survives the agent exiting and maiTerm restarting, and never
> needs the work to run anywhere but where it already is.

## 1. Why

An agent that needs to come back to something has no way to do it. "Check the deploy in
twenty minutes", "look at the flaky test once CI has run again", "continue when my usage
limit resets" all end the same way today: the agent stops, and the follow-up happens only if
the human remembers it.

Solo (0.10.0) added a "Set timer" action that gives a running agent a delayed prompt. It is
human-only, from the command palette. The idea worth taking is the delayed prompt; the part
worth going further on is **letting the agent set it**, because the agent is the one that
knows it has to come back.

### What Claude Code already has, and why it doesn't cover this

Read from the tool descriptions in a Claude Code session, 2026-09-26:

| | In-session cron (`CronCreate`) | Routines (`/schedule`, `RemoteTrigger`) |
|---|---|---|
| Where it runs | Inside the Claude process that created it | Anthropic's cloud, as a fresh session on a clone of the repo |
| Survives the agent exiting | Only if `durable`, and only fires once Claude is launched again in that folder | Yes — it is a separate agent |
| Runs in your context | Yes | No: a new session, a cloud checkout, none of your local services, files or SSH hosts |
| Runtimes | Claude only | Claude only |
| Visible to the human | Not outside that session | On claude.ai |
| Limits | Fires only while the REPL is idle; recurring jobs expire after 7 days | Repo must be clonable and runnable in the cloud environment |

Neither does the thing that matters here: *this* agent, in *this* tab, with everything it
already has open, picks the work back up. maiTerm owns the tab, so it can:

- deliver to **any runtime** — Codex and Gemini tabs get follow-ups too;
- deliver into the **real session**: same transcript, local services, files, SSH hosts;
- hold the follow-up **after the agent has exited**, and resume the agent to deliver it (§6);
- show every pending follow-up to the **human**, who can see and cancel them (§8);
- wait on **workspace events**, not only the clock: a stack service coming up, a task
  finishing (§5).

## 2. Shape

A **follow-up** is `{ text, due, author, created_at }` stored on the tab it is for. When it
comes due, maiTerm types `text` into that tab's agent as a prompt, framed so the agent knows
it is its own note (§7), and removes it.

```
agent ──createFollowUp──▶ Tab.follow_ups ──(tick: due?)──▶ agentDelivery ──▶ tab's agent
human ──tab menu───────▶        ▲                              │
                                └── kept until delivered ──────┘
```

Three decisions carry most of the design:

1. **A follow-up is for its own tab.** An agent cannot schedule a prompt into another tab.
   Cross-tab injection is `driveTab`, which exists only for the Overlord and sits behind three
   mechanical guards (`docs/overlord.md` §3). A follow-up that could target any tab would be
   `driveTab` without them, handed to every agent.

   **This has to be enforced; it is not the default.** The MCP server fills in `tabId` from
   the connection's affinity only when the caller leaves it out
   (`src-tauri/src/claude_code/server.rs` ~2810): an explicit `tabId` naming any tab in the
   instance is honoured, for every tool. So the follow-up tools (`OWN_TAB_ONLY_TOOLS`) accept an
   explicit `tabId` only when it names the connection's **stated** binding, checked in Rust
   before the call reaches the frontend (`own_tab_only_refusal`). All three are also on
   `PEER_ADDRESSING_TOOLS`, which refuses an identity that was only inferred after a
   reconnect.

   *Stated*, not merely bound, was learned live (2026-09-30). A connection whose header named
   tab A was inferred onto tab B, because A already had a connection and B was the only unbound
   one. `tabId: B` then matched that inferred binding and scheduled a prompt into B's agent.
   Elsewhere an explicit `tabId` counts as a statement of identity. Here the question is
   whether the caller *is* that tab, and naming an id proves nothing about that.
2. **One-shot only.** No recurrence. An agent that wants to keep checking re-arms itself when
   the follow-up fires, which means every repetition is a decision it takes with fresh
   context. Recurring, engine-owned schedules already exist: Overlord rituals. A recurring
   agent-owned timer is the loop the mesh and the Overlord both had to add loop control for.
3. **Never typed into a shell.** The Overlord's `require_live_repl` lesson applies unchanged:
   if the agent has gone, "check whether the deploy finished" becomes a bash command. A
   follow-up is delivered to a live agent, or it waits (§6).

## 3. Data model

```rust
pub struct FollowUp {
    pub id: String,
    pub text: String,
    pub due: FollowUpDue,
    /// "agent" (created over MCP) or "human" (created from the UI). Shown on the row, and
    /// it decides the envelope wording (§7).
    pub author: String,
    pub created_at: String,          // RFC 3339, wall clock
    /// Deliver no later than this; past it, the follow-up is dropped and the drop is logged
    /// and shown (§6.3). None = no expiry beyond the global cap.
    pub expires_at: Option<String>,
}

/// A STRING kind, never a Rust enum. This is persisted on `Tab`, and an enum variant an older
/// build doesn't know makes that build fail to parse AppData, load EMPTY state and overwrite
/// the backup (memory: serde-enum-variant-downgrade-wipe). With a string, an unknown kind
/// deserializes fine and the follow-up is simply never due on the older build.
pub struct FollowUpDue {
    pub kind: String,                    // "at" | "service_ready" | "service_stopped" | "task_done"
    pub at: Option<String>,              // kind "at": RFC 3339, wall clock
    // Services and tasks are per WORKSPACE, and the tab can move. The condition records the
    // workspace it was set in, so a moved tab's follow-up still looks in the right place.
    pub workspace_id: Option<String>,
    pub service_id: Option<String>,      // service_ready / service_stopped (stopped or crashed)
    pub task_id: Option<String>,         // task_done
    pub label: Option<String>,           // the service name / task title when set, for every reader
    // An event condition that HAPPENED: set once, in Rust (`meet_tab_follow_up` — the first
    // observation stands), and from then on the follow-up is due like a time one. Persisted,
    // because a service transition is seen once and delivery may wait hours for the agent.
    pub met_at: Option<String>,
    pub outcome: Option<String>,         // "it came up", "it crashed (exit 1)", "it was DROPPED, not done"…
}
```

`Tab.follow_ups: Vec<FollowUp>`, persisted with the tab.

**It lives on the `Tab`, which makes its lifecycle a set of decisions, one per path that
copies or moves a tab** (memory: reload-mints-a-new-tab-id). Each one here is deliberate:

| Path | Follow-ups | Why |
|------|-----------|-----|
| Reload (`carry_tab_state_on_reload`) | **Moved** — one line | The replacement is the same session, and `carry_tab_record` copies the whole record (`..src`, `commands/workspace.rs` ~2113), so the new tab gets them for free. But reload *copies*, then deletes the original later (`reloadTab` → `deleteTab`, `workspaces.svelte.ts` ~2564 → ~2593), and in between **both tabs hold the same follow-ups**. So clear them on the original in the release block that already does this for comms claims, under the same write lock (~2195). |
| Duplicate / split clone / copy to workspace / new conversation | **Cleared** | A duplicate that inherited them would deliver every follow-up twice, to two agents. Every frontend path (`duplicateTab`, `splitPaneWithContext`, `copyTabToWorkspace`, `newConversationFrom`) builds a fresh `Tab` and copies named fields, so they drop a new field by default. `Tab` has no `Default`: the three constructors (`state/workspace.rs` ~1873ff) and `clone_workspace_with_id_mapping` (`commands/window.rs` ~593, behind `duplicate_workspace` and `duplicate_window`) are full struct literals, so adding the field is a compile error in all four until each says empty. Plus the `fully_populated` test literal (`commands/workspace.rs` ~3272), which only `cargo check --tests` compiles. |
| Move to another pane or workspace | Carried | Same tab, same session. (Unlike `service_id`, which `move_tab_to_workspace` clears because a service belongs to its workspace — `commands/workspace.rs` ~467. A follow-up belongs to the tab, and its condition carries its own `workspace_id`.) |
| Workspace Share export | **Never** | The share file is an allowlist of `Shared*` types (`docs/workspace-share.md`), so a new `Tab` field doesn't reach it unless added. Keep it that way: a follow-up is this agent's note about this session. |
| Archive | Held | Delivered on restore, marked late (§6.3). Archiving is reversible; dropping them would make it not. |
| Close | **Dropped, logged** | Nothing left to deliver to. Log it from the store's `deleteTab`, **not** from Rust `delete_tab`: reload removes the original through `delete_tab` too, so logging there reports a false drop on every reload. |
| Delete an archived tab (`delete_archived_tab`, ~2452) | **Dropped, logged** | Same as close. |
| Close a pane, a workspace, a window | **Dropped, logged** | Each removes tabs without passing through the store's `deleteTab` — the commonest is the last tab of a split pane, which Cmd+W closes as a *pane*. Pane and workspace log from the store (`deletePane`, `deleteWorkspace`). Window close and `reset_window` log in Rust, where no reload passes; `reset_window` is what deleting a window's *last* workspace and closing the last macOS window actually call. |
| Backup restore (any import) | Restored, **deduplicated**, losses logged | Stale by then — the late handling in §6.3 is what keeps that from being a surprise. An import puts tabs back as straight clones, so a copy can land beside the tab its follow-ups have since moved to (A reloaded to A′, or A dragged to another pane, then the backup restored). `AppData::settle_follow_ups_after_import` restores the invariant that **no follow-up is held twice**. It judges "was already there" by **position** (workspace, pane, tab), not tab id, because a moved tab keeps its id. What an import drops (an overwrite discards tabs created since the backup) is derived as before minus after and logged. **Deliberately not undone:** restoring an older backup also brings back follow-ups that were DELIVERED after it was taken, and they go out again, marked late. A restore returns to the old state, and maiTerm keeps no record of delivered follow-ups to subtract. A human restoring a backup chose that state. |

The stack-service and task conditions reference ids. A service id survives reload; a task id
is stable. If the referenced service or task no longer exists when checked, the follow-up
**fires**, marked with the reason ("service `web` was removed") rather than waiting forever.
An unmeetable condition is still news to the agent.

"No longer exists" has two false positives to rule out:
- **A moved tab.** Look up in the condition's own `workspace_id`, not the tab's current
  workspace, or every follow-up on a moved tab fires as orphaned.
- **An archived task.** `archive_tab` lifts the archived tab's tasks out of `workspace.tasks`
  into `tab.archived_tasks` (~2365). A follow-up on *another* tab waiting on one of them
  must search `archived_tabs[*].archived_tasks` too, or it reads a parked task as deleted.

## 4. MCP surface

Three tools, scoped to the calling tab (enforced — §2). They are served by the frontend
store, which is where every tool not handled by `handle_backend_tool` already goes
(`server.rs` ~2878).

**Gated under the Overlord (decided 2026-09-30).** The preference is **Preferences → Overlord
→ Enable follow-ups** (`follow_ups_enabled`, default `true`), sitting under "Enable Overlord"
in `OverlordRulesSection.svelte` and disabled while the Overlord is off. The feature is live
only when **both** are on:

```
follow_ups_live = overlord_enabled && follow_ups_enabled
```

So turning the Overlord on turns follow-ups on with it, and from then on they are a toggle of
their own. The point is the same contract the task and stack tools have: **an agent is never
told about follow-ups, and never carries their schemas, unless the feature is live.** One
computed value gates all four places, so they can't disagree:

- `tool_list_response(tasks_enabled, stack_enabled)` (`protocol.rs` ~50) takes
  `follow_ups_live` as a third parameter, so the tools vanish from the list.
- The handler re-checks it when called, as the stack tools do (`claudeCode.svelte.ts` ~948) —
  a client holding a stale tool list is still refused.
- `session_priming_text` (§ Priming below) adds the line only when it is live.
- The delivery tick holds when it is off. Follow-ups already pending stay on their tabs,
  visible and cancellable, and deliver when it is turned back on. Turning a feature off
  is not a reason to throw an agent's notes away.

It is the preference pair, deliberately not "this window has an Overlord workspace" (which
the Overlord's own priming also checks): the tool list is served per connection, not per
window, and a feature that appeared and vanished with a workspace would be hard to reason
about.

| Tool | Does |
|------|------|
| `createFollowUp` | `{ text, at? , in_minutes?, when_service_ready?, when_service_stopped?, when_task_done?, expires_in_minutes? }` — exactly one trigger. Returns the follow-up with its resolved `due`. |
| `listFollowUps` | This tab's pending follow-ups. |
| `cancelFollowUp` | `{ id }` — only this tab's. |

Limits, enforced in the tool and returned as a refusal that says which one tripped
(`driveTab`'s rule: a refusal names what is holding it):

- **10 pending per tab.**
- **At least 1 minute out.** Anything sooner is not a follow-up; it is the agent's own turn.
- **At most 7 days out**, and the same cap is the default `expires_at` for event-triggered
  ones. Matches `CronCreate`'s recurring cap; a follow-up nobody has thought about for a week
  is not one to act on unannounced.
- **Rate: 20 created per tab per hour.** The backstop for an agent that re-arms in a tight
  loop, same role as the Overlord's `max_per_hour`.

**Priming.** When follow-ups are on, the priming says they exist in one line, and lists this
tab's pending ones. A resumed agent then knows what it already scheduled instead of
scheduling it again — the task system's re-send duplication problem (`docs/tasks.md` §3,
"Tab ids are not durable") in another form. The priming is `session_priming_text`
(`server.rs` ~493), in Rust, reading `app_data` — shared by the `initSession` reply and the
SessionStart hook — so the pending list is read from persisted `Tab.follow_ups`, never from
frontend state. Codex receives no hook context, so a Codex tab sees the line only when it
calls `initSession`.

## 5. Triggers

- **Time.** Compared against the **wall clock** on every tick, never a stored duration. A
  laptop that slept through the due time wakes, sees the follow-up is past due, and delivers
  it marked late. A duration-based timer would silently shift everything by the sleep.
- **Stack service ready / stopped — on a TRANSITION, never on the level.** Service runtime
  status is never persisted, and a service nothing has reported reads as `stopped`
  (`stack_priming_list`'s `unwrap_or("stopped")`, `server.rs` ~575). Evaluated as a level,
  "when `web` stops" would fire at every app launch, and at once for a service that was
  never started. So these fire on a change the store observes while the tab's window is
  running: `ready` entered, or `stopped`/`crashed` entered *from* a running state. A
  transition that happened while maiTerm was closed is not seen, which is right — nothing
  observed it, so nothing can claim it.

  "Ready" is the stack's own status, which is set three ways: an address read from the
  service's output (`stack.svelte.ts` ~891), a match on its `ready_pattern`, which may carry
  no port (~864), or an agent's `updateService { ready: true }` (`claudeCode.svelte.ts`
  ~1080). A follow-up on a worker that does none of those waits until `expires_at`.
- **Task done.** Read from the task store. `dropped` counts as done-for-this-purpose but is
  named as dropped in the envelope: the agent asked to hear when the task ended, and a
  retraction is an ending it needs to know about. (This is the opposite of dependency
  handling, where `dropped` does not satisfy a dependent — there the question is "may I
  start", here it is "tell me what happened".)

**As built (step 4, 2026-09-30):**

- **Transitions come from one place.** `stackStore.setRt` is the only writer of a service's
  status, and it tells `onTransition` listeners about every change. The follow-up store
  listens from `init` (whether or not the feature is live: an event that happens while
  follow-ups are off is still recorded, and delivers when they are back on). `serviceOutcome`
  in `followUps/model.ts` decides, from `from` → `to`, whether the edge meets the follow-up and
  what to tell the agent.
- **Met is persisted.** The store calls `meet_tab_follow_up`, which sets `met_at` + `outcome`
  once. `dueAt` is then `met_at`, so the delivery gate, the late check, the badge and the list
  treat it exactly like a time follow-up that has come due.
- **Levels are checked on the tick** (`checkConditions`): a task that has ended, and anything
  that no longer exists — the condition's workspace (closed, or moved to another window, where
  this window could never see it happen), the service, or the task. Tasks wait for
  `tasksStore.loaded` AND this workspace's list: an unloaded list is not evidence that a task was
  deleted. **"Gone" must be seen twice, 10 s apart** (`GONE_CONFIRM_MS`), because ordinary moves
  leave things briefly in neither place: a workspace moved in from another window arrives before
  its task list, and an archive lifts a tab's tasks off the board an IPC round trip before they
  land on the archived tab. Met is permanent, so a false "deleted" could never be taken back.
- **`starting` → `stopped` is not a stop.** It is a start that never ran or was called off —
  which is exactly what a reloaded window's auto-start produces for a service still running in its
  reattached tab (stack runtime isn't persisted, so the reload reads it as stopped). A crash out
  of `starting` does count. **Known gap:** after a window reload the runtime starts from `stopped`,
  so a service that was running across the reload and crashes later shows no edge, and its
  `when_service_stopped` follow-up waits to expiry. Fixing it means the stack store adopting a
  reattached service's run, which is a stack change, not a follow-up one.
- **A condition that already holds is refused at creation** (`already_ready`, `not_running`,
  `already_ended`). These fire on the NEXT change, so accepting "when `web` is ready" for a
  ready service would wait for it to go down and come back up. The refusal tells the agent to
  act now instead.
- Event follow-ups always carry an `expires_at`: 7 days, or `expires_in_minutes` up to that.
- Only the calling tab's project: a task id from another project is refused (`other_project`).

Not in v1, deliberately: file changes, CI, arbitrary shell conditions. Each is either a
polling loop maiTerm would have to own or a way to run a command on a schedule, and the
second is exactly what §2's "never typed into a shell" rules out.

## 6. Delivery

### 6.1 The path

Delivery goes through **`agentDelivery`** (core in `src/lib/stores/agentDelivery.ts`, the
live instance in `agentDeliveryLive.ts`), the mailbox the bridge and the mesh already share.
Follow-ups take no slot and no owner tag: `tryDeliverNow` works for a tab with no slot at all,
which is most of them. One mailbox per tab is the point of that module: two controllers would
mean two `injecting` guards for one PTY, and a follow-up paste could land inside a bridge
message. It
already holds while the agent is at a permission or elicitation prompt (Claude: permission or
an open `AskUserQuestion`; Codex: permission only — `src/lib/agents/adapter.ts`) and
serializes injections.

**But its `deliver()` can't be used as it stands.** It is fire-and-forget:

- When a tab isn't deliverable, `deliver()` queues the text and returns `'queued'`
  (`agentDelivery.ts` ~136). The drain later injects it and tells nobody — no completion
  callback.
- A queued item can't be taken back, so Cancel and "Deliver now" couldn't reach it.
- The queue is in memory, so a restart loses it.
- It has no idea of idle. `busy` is a 1s post-inject cooldown, and `ready` isn't cleared when
  a turn starts, so a queued item drains mid-turn the moment a permission prompt clears.

So the follow-up store **never queues in the controller**. The follow-up stays in
`Tab.follow_ups`, the one durable place, until delivered. The store checks its own gate
(below), then calls one new controller method, `tryDeliverNow(tabId, text)`. That method
injects only if the tab is deliverable *and its queue is empty*, and otherwise returns
`'held'` without queueing. The injection still goes through the controller's `injecting`
guard, so it can't interleave with a bridge or mesh paste. Anything held is retried on the
next tick.

Two notes on the controller:
- A failed inject there is re-queued and retried on a fixed 1.5s drain tick, not backed off.
  That doesn't matter here, since follow-ups don't use its queue.
- A slot's `ready` flag is flipped only by the bridge and mesh listeners, and `remap` forces
  it false after a reload (~212). A follow-up-only slot would work by accident at best. The
  follow-up store therefore reads readiness from the agent state store directly rather than
  from the slot.

**Follow-ups wait for the agent to be idle**, where bridge messages don't. `agentDelivery`
delivers to a live session mid-turn, which suits a peer's reply. A follow-up is never urgent,
and some runtimes take input typed mid-turn differently from Claude Code's queueing (Codex is
unverified here). So the store's gate is: agent state `idle` (`agentState.svelte.ts`), not
awaiting the human, and `tryDeliverNow` accepts. Waiting for a completed turn costs a little
latency and removes the question.

The tick runs in the frontend, where delivery has to happen anyway (it needs agent state,
the adapter's `isAwaitingHumanInput`, and the PTY). Because WKWebView throttles timers in an
occluded window (memory: screen-sleep-webview-occlusion-stall), the tick also runs on
`visibilitychange` and window focus. The wall-clock comparison (§5) makes a late tick merely
late, never wrong. If a hung webview proves to be a real problem, `src-tauri/src/commands/scheduler.rs`
is the precedent for timers that must not depend on one. It owns the backup and memory-sampler
loops for exactly that reason. It wouldn't help much here, though, since delivery needs the
webview anyway.

**Taken, then typed: at-most-once (revised 2026-09-30).** This first said "removed only after
the inject succeeds", preferring a duplicate to a loss. Review showed the duplicate wasn't
confined to crashes. A reload MOVES follow-ups to the replacement in Rust, but the frontend
mirror of the original keeps them until the reload finishes. A tick in between typed one into
the original's agent, and the replacement then delivered it again. So delivery now **takes**
the follow-up off its tab in Rust first (`take_tab_follow_up`: atomic, and `None` if the tab no
longer holds it), types it only if the take succeeded, and puts it back if the inject doesn't
happen. A crash in the milliseconds between take and inject loses it. That is rarer than a
reload, and the loss is logged.

The same review retired the whole-list write (`set_tab_follow_ups`). Two changes in flight
both computed their list from the same mirror, so the second undid the first and a cancelled
follow-up came back. Every change is now one atomic Rust operation (`add_tab_follow_up`,
`take_tab_follow_up`), and the mirror only ever takes Rust's answer.

**The gate before a take** (`holdReason` in `stores/followUps.svelte.ts`), cheapest check first:
1. The agent is `idle`.
2. **No keystroke in the last 2 s**, since it may not have reached the screen yet.
3. **The input box is empty, read off the screen** (`mailink/input_box.rs`, command
   `agent_input_box`). Typing into an agent's input fires no hook, so nothing reports a draft,
   and a paste plus CR would submit the human's half-written prompt with the follow-up glued
   on. The parser finds Claude Code's box: the bottom-most `❯` line sitting directly under a
   horizontal rule, running to the rule below it, with **both the `❯` and the rules at column
   0**. Claude indents every continuation line of a draft by two columns. Matching after a trim
   let a draft that itself contained `──` and `❯` (a pasted screen tail) pass for the box and
   read `empty`; review caught it. Dimmed placeholder text is blanked first
   (`screen_text_undimmed`). A box with text holds delivery; an empty one is safe whatever the
   keystroke history says. Also verified on the real grid: that pasted screen tail reads
   `has_text`, and a long paste collapsed to `[Pasted text #N +M lines]` reads `has_text` (the
   chip is not drawn dim).

   **Why the screen, not timestamps (2026-09-30).** Two timestamp rules were tried, and review
   broke each in a different direction.
   - Keystrokes after `updatedAt` held follow-ups forever. A session start doesn't stamp it, so
     the `claude -c` that relaunched an exited agent counted as a draft, and relaunching is
     exactly how an agent picks its follow-ups back up.
   - Keystrokes after a new `idleSince` let **type-ahead** through. Text typed while the agent
     booted predates the SessionStart that stamps it, so it read as "before idle".

   The screen answers the actual question. Verified on the real grid: empty box → delivered
   even with recent keystrokes; a draft → held past due, untouched; the draft cleared with
   Ctrl-U → delivered, with only the follow-up submitted.

   **Where the box isn't recognised** (another runtime, a layout change), the answer is
   `unknown`, never `empty`. Delivery then falls back to the timestamp rule: anything typed
   since this stretch of idle began (`AgentTabSession.idleSince`) holds. `idleSince` is stamped
   on every entry into idle, and a SessionStart always begins a new stretch, even when a
   relaunch resumes the same session id over a stale entry. The fallback still has the
   type-ahead gap, which is why it is the fallback. **Unverified:** a TUI's mouse reports go
   through xterm's `onData` and count as human input, so where the fallback applies, a click in
   a mouse-tracking TUI would hold follow-ups.
4. 1.5 s of output quiet.
5. The delivery controller would inject now (`canDeliverNow`).
6. **A live agent process** (`getAgentLiveness`). A session entry is cleared by the SessionEnd
   hook, which never arrives when the agent is killed or its ssh tunnel is down, and without a
   process the paste lands in a shell. This is the evidence the Overlord (`replState`) and comms
   use, with the same known gap: a remote agent that dies while its ssh stays up still reads as
   live.

**A keystroke after the gate aborts the delivery.** The gate reads the box and then still
awaits several things (the liveness sweep, the take, the trust-dialog check). Someone who starts
typing inside that window would get the paste landed after their keys and submitted with them.
So the keystroke time is snapshotted when the gate starts and must be unchanged before the take
and, last, inside `injectPrompt` just before the paste is written (the `beforePaste` predicate).
If it changed, the follow-up is given back. **What remains:** keys pressed during the paste's
own settle (about 150 ms for a typical follow-up, capped at 1.2 s), between the paste and its
CR, are appended to the follow-up and submitted with it. No check made before the paste can see
them. That is a sub-second overlap, not a draft being submitted.

`tryDeliverNow` returning `'delivered'` means the bytes reached the PTY, not that the agent read
them, which is as far as any delivery here can see.

### 6.2 When the agent isn't there

| The tab | What happens |
|---------|--------------|
| Live agent, idle | Deliver. |
| Live agent, busy or at a human prompt | Wait. |
| Tab at a shell, agent exited, runtime resumable | **Resume the agent, then deliver.** Reuse the Overlord's `recoverTab` path (`overlord.svelte.ts` ~3683), which already types a resume into a shell safely: the command is `resumeCommandFor` (`agentState.svelte.ts` ~27), a template from `src/lib/agents/resume.ts` such as `claude --resume %claudeSessionId`, filled in by `interpolateVariables` (`triggers.svelte.ts` ~777) from the tab's `trigger_variables`; if a `%` survives substitution there is no session id, and it refuses (`no_session_id`) rather than typing a broken command. Delivery then waits for the resumed session to register and complete its first turn — the "ready is reachable too early" watch item in `docs/overlord.md` §4.0 is this exact risk. |
| Tab at a shell, not resumable (no session id, or an SSH tab whose connection is gone) | **Hold and surface.** The row goes to "due — agent not running" with a Deliver button (§8). Never typed. |
| Suspended tab or suspended workspace | Hold and surface. Waking a suspended workspace because a timer fired is the human's call, not a follow-up's. |
| Archived | Held; delivered on restore. |

Resuming the agent is the one place a follow-up types anything that isn't its own text, and
it types only the runtime's own resume command for that tab's session — never the tab's
stored `auto_resume_command`, which is free text a user (or an imported workspace file) may
have edited into anything. It is still worth a preference (`follow_ups_resume_agent`,
default on) because it starts a session — and spends quota — without the human there.

**As built (step 5, 2026-09-30)** — `resumeAgent` in `stores/followUps.svelte.ts`, reached when
the delivery gate's answer is "no agent" (no session entry, or a stale one whose process the
liveness probe can't find). It does not call the Overlord's `recoverTab`, whose `stopped` verdict
comes from the Overlord's own probe cycle and which types a bracketed paste; it reuses the same
pieces (`resumeCommandFor` + `interpolateVariables`, the `%` refusal) behind a stricter gate,
because this is the one place a follow-up types into a shell. **Verified live** (bash 3.2, the
dev app, a real Claude Haiku): the agent `/exit`ed, the follow-up came due, maiTerm typed the
resume, and delivered 15 s later; with `echo half-typed` on the shell line the probe read
non-empty, after Ctrl-U still non-empty, and at a fresh prompt empty. The empty-line check took
seven review rounds; each one found a way a resume could be glued onto a human's command.

- **Preference `follow_ups_resume_agent`** (Preferences → Overlord → "Restart the agent for a
  follow-up", default on, inert while follow-ups are off). A human's Deliver now restarts it
  whatever the preference: the click is the consent.
- **Not for** an Overlord-exempt tab (or workspace), an archived tab, or a tab that has run ssh
  (`restore_ssh_command` / `auto_resume_ssh_command`): a remote agent can't be seen from here,
  and resuming its session locally is the wrong machine.
- **The session must be on this machine** (`agent_session_is_local`: its transcript is under
  the local `~/.claude/projects` — not the ssh mirror — or `~/.codex/sessions`). A session id
  recorded while the agent ran over a hand-typed `ssh` sets no ssh field on the tab, and once
  the connection is gone the shell is local. Gemini has no findable transcript, so it holds.
- **Evidence of an empty shell prompt**, all of it: no agent process and no ssh in the
  foreground (`getAgentLiveness`); the shell in the foreground (`get_pty_foreground_job`); the
  shell's own OSC 133 prompt (A) as the last thing it did, with no command begun (B/C) since; no
  keystroke since that prompt; and, last and decisive, **the shell's own answer that its command
  line is empty** (`probe_shell_line`).

  **Why ask the shell (review of 0a090e0).** Every outside signal had a hole that would have
  glued the resume onto a command and run it. A paste (Cmd+V) or a dropped file writes to the
  PTY without stamping a keystroke. And **type-ahead** — keys typed while `make` ran — lands on
  the next prompt's line *before* that prompt's own mark, so "nothing typed since the prompt"
  read true with `git commit -am wip` sitting on the line. It is the draft check's lesson again
  (§6.1): stop inferring, read the fact.

  The probe is an **empty bracketed paste**, `ESC[200~ESC[201~`, answered by maiTerm's zsh
  integration: a wrapper around whatever `bracketed-paste` widget is bound (plugins included)
  that, when the paste changed nothing, prints `OSC 1337;MaitermLine=<pid>;<len>`. The shell
  announces `MaitermLineProbe=<pid>` at startup, and Rust probes only the PTY's own shell (its
  child process), only when that shell announced, and only when a fresh foreground read says
  that shell holds the terminal — so a nested shell or a program it started never receives it.
  (The first live run held on "can't tell which shell": at a bare prompt the foreground read
  names no pid, so the check has to be made in Rust against the PTY's child.) An empty paste is harmless wherever it isn't answered. The length counts `$PREBUFFER`:
  at a continuation prompt (`> `, `for>`) `$BUFFER` is empty while the earlier lines wait, and
  a resume would have become part of that command (review of eb17567). oh-my-zsh's
  `bracketed-paste-magic` highlights a paste by blocking for the next key before returning, so
  the answer came out only when another byte arrived — and a human's Cmd+V arriving first (it
  writes to the PTY without stamping a keystroke) released a "0" while the pasted text sat on the
  line (review of 197ebbf). The wrapper drops the `paste:` highlight for the call, so the answer
  is immediate; the cost is that pasted text isn't highlighted in maiTerm's zsh. Rust still sends
  a second probe if the first gets no answer (a slow shell). Verified on a real zsh 5.9, with and without that widget:
  empty line → 0; `git comm` → 8; type-ahead during `sleep` → 18; type-ahead `echo GLUED \⏎`
  → 13; a real paste → no report.

  **bash is read off the grid instead** (`terminal/prompt_line.rs`). macOS's `/bin/bash` is 3.2,
  which has no `READLINE_LINE`, so it can't be asked — and it is a real login shell here (the
  first live run held because every tab was bash). maiTerm's bash `PROMPT_COMMAND` ends PS1 with
  an invisible `\[OSC 1337;MaitermPromptEnd\]` and announces `MaitermPromptMarks=<pid>`. The
  PTY reader splits each read at those marks and at OSC 133 B/C. **The line is empty only while
  nothing at all has been drawn since the mark**: an idle, empty prompt draws nothing, and every
  way the line stops being empty draws something — an echoed key, a paste, type-ahead readline
  redraws after the prompt, a continuation prompt, the `(reverse-i-search)` banner, vi-mode's
  bell. A redraw of PS1 (Ctrl-L, SIGWINCH) re-prints the mark. The first version compared the
  cursor's position with where it stood at the mark instead, and review of e1cac85 broke it: once
  scrollback is at its cap, "history size + screen line" stops naming a row, so a continuation
  prompt — or a line exactly a multiple of the width long — scrolled the cursor back onto the
  stored coordinates and read as empty. **And nothing written to the PTY since the mark**
  (review of 5cd717a): a key that draws nothing (an Esc, a Ctrl-X, anything with echo off) was
  still written, and the mark records the PTY's `bytes_written` count (`write_pty` counts every
  write before the shell can read it). Before the prompt, the bash `PROMPT_COMMAND` reports a
  prompt that can't be judged — echo off (`stty -a`; type-ahead would sit on the line unseen) or
  vi editing mode (`bind -v`; keys at an empty prompt are commands) — as `MaitermPromptUnsafe`,
  and that prompt is never empty.

  **A prefix key typed ahead during the previous command is neutralised, not detected.** It is
  written before the mark and draws nothing after it (review of 072d75b), then eats the resume's
  first keys (`Esc` + `c` is capitalize-word: `laude --resume …` ran). Two ways of detecting it
  failed review: counting input from the command start held every resume after an interactive
  agent session (the human's keys into the agent counted), and moving that count to Claude's
  SessionEnd hook was wrong both ways (Claude stops reading stdin *before* it sends the hook, and
  a headless `claude -p` in the tab sends one too). So bash's resume is typed behind a **Ctrl-G**,
  readline's `abort`, bound in every emacs keymap — plain, after Esc, after Ctrl-X — which cancels
  a pending prefix or numeric argument and leaves the line as it was. Verified on bash 3.2.57:
  Esc, Ctrl-X, Esc-1 and Esc-Esc typed during `sleep 1`, then Ctrl-G plus a command — it ran
  intact every time. It is followed by a **Ctrl-U**: with `stty -ixon` or `-iexten` (common in a
  bashrc, to free C-s), a typed-ahead C-q / C-v reaches readline as quoted-insert and takes the
  Ctrl-G as a literal `^G` — `^Gclaude …` ran (review of 9a58944); Ctrl-U deletes it, and does
  nothing on an empty line. Reviewed clean: C-x C-u, C-x `(`, Esc Esc, Esc C-x, C-x Esc, C-],
  M-#, and anything that draws (`(arg: 1)`, `(i-search)`, a bell) holds instead. (Not in zsh: there Ctrl-G aborts the whole line, and a pending prefix makes
  the probe's own bytes resolve into it, which reads non-empty and holds — leaving a few stray
  characters on the line, a known cosmetic cost.) A mark split across two reads is missed and
  holds. zsh's answer likewise counts vi command mode as not empty. Verified against bytes
  captured from bash 3.2.57: a fresh prompt reads empty, `git commit -am wip` typed during
  `sleep 1` reads not empty (tests use those exact bytes).
  **fish, shells spawned before this existed, and no integration all hold**, with "start it
  yourself".
- Typed as a plain line plus CR, not a bracketed paste: the shell may not have bracketed paste
  on (macOS bash 3.2).
- **Typed once, then watched.** For two minutes the row says it is coming up; after that, "it
  did not come up — start it, or Deliver now to try again". It is never retyped on its own. Once
  an agent registers in the tab again, the slate is clean.
- Then the ordinary gate delivers it: idle, empty input box, quiet. A follow-up the human asked
  for EARLY is remembered across the restart (`wantedEarly`), since the tick would otherwise leave
  a not-yet-due one alone.

### 6.3 Late, expired, and orphaned

- **Late** — delivered more than 5 minutes after due. The envelope says by how much, so the
  agent can tell "the deploy should be done by now" from "this was yesterday's deploy".
- **Expired** — past `expires_at` without being deliverable. Dropped, logged, and shown on the
  tab's follow-up list as expired until the human clears it. Silent expiry is the defect class
  this project keeps finding (memory: absence-read-as-a-claim): an agent that set a follow-up
  and never heard back has to be distinguishable from one that never set it.
- **Orphaned condition** — the service or task it waits on is gone. Fires, with the reason
  (§3).

## 7. What the agent sees

A follow-up is **framed**, unlike an Overlord directive:

```
⟦FOLLOW-UP⟧ You scheduled this at 14:02 for 14:22 (delivered on time):
Check whether the staging deploy finished; if it failed, read the job log before retrying.
```

Late, event-triggered and human-created variants change the first line
("…for when `web` was ready — it came up at 15:10", "…delivered 3h 12m late",
"Your human scheduled this at…").

The Overlord deliberately has no envelope (`docs/overlord.md` §3): it acts with the human's
authority, and an envelope made agents treat its orders as optional. A follow-up is the
reverse case. Its author is the agent itself, possibly hours earlier, possibly about a
situation that has since changed, and it must **not** read as the human having just typed
it. The envelope is what lets the agent weigh it as its own earlier note. It is also why a
follow-up can't smuggle in a slash command: an enveloped `/compact` is not `/compact`, which
here is the desired property.

## 8. The human side

**As built (step 3, 2026-09-30):**

- **A clock badge** beside the tab's other indicators (`TerminalTabs.svelte`, text from
  `badgeSummary` in `followUps/model.ts`, in a `Tooltip`, never a native `title=`):
  - dim while waiting;
  - accent once one is past due and waiting for the agent;
  - faded when held (feature off) or when only expired ones remain.
  
  It stays visible while the feature is off: held follow-ups still need seeing.
- **Tab context menu → Follow-ups…** opens `components/followUps/FollowUpsModal.svelte`
  through the `open-follow-ups` event, which `+layout` owns. It is offered on any agent tab
  while the feature is live, and on any tab that already holds follow-ups. Each row shows:
  - where it stands (`whenText`: "in 12m", "due 3m ago", "expired 2h ago");
  - who added it;
  - for a due one, what is holding it.
  
  The levers are Deliver now, Cancel (Clear for an expired one), and adding one by hand
  (`author: "human"`). This is Solo's "Set timer", on the tab rather than in a palette.
- **Deliver now uses the same gate as the tick.** Both call `deliverOne()`; only the due time
  is waived. Held, it says why, and the envelope of an early one says "(delivered early, at
  your human's request)".
- **Every reason is true of the tab.** The synchronous part of the gate, `tabHold`, is checked
  most-absent first: no terminal loaded, no agent, a permission prompt, then busy. Review
  caught "the agent is busy — it goes when this turn ends" shown for a tab with no agent at
  all ("no session" read as "not idle"). A tab archived while the modal is open is found and
  noted; one that has gone (closed, or reloaded under a new id) says so rather than showing
  an empty list.
- Deliver now on a tab whose agent has exited restarts it, then delivers (§6.2).
- The Overlord board could show pending follow-ups per tab. Not built; it is a read of
  `Tab.follow_ups` when wanted.
- **maiLink** would need a protocol bump (every wire change does, `docs/mailink-protocol.md`
  §13.5). Not in v1.

## 9. Phase 2: continue when the usage limit resets

The most compelling use, and the one an MCP tool cannot provide by itself: **a rate-limited
agent can't make a tool call**, so it can't schedule its own continuation. maiTerm has to
notice the limit and create the follow-up on its behalf, with `author: "maiterm"` and the
envelope saying so.

What has to be established first — none of it is known yet, and nothing in maiTerm detects
a usage limit today:

- **The signal.** The house rule is that agent facts come from the transcript, not the screen
  (memory: mailink-meta-fields-from-transcript). Two leads already sit in code that reads
  transcripts and throws them away:
  - Claude writes synthetic records (`isApiErrorMessage`, `"model":"<synthetic>"`) that
    `src-tauri/src/mailink/transcript.rs` meets and skips (~999). A limit error is likely
    one of these.
  - Codex `token_count` events carry a `rate_limits` object that nothing reads (fixtures near
    ~2768).

  The reset time has to be read out of whichever is right, with its time zone.
  Verify against real limit events per runtime before designing the parser; a fixture proves
  only the shape assumed (memory: verify-parsers-against-real-transcripts).
- **Consent.** Auto-continuing at reset spends the new window's quota the moment it opens,
  possibly on work the human would rather not resume, and across N tabs at once. So it is
  **opt-in**, per tab or per workspace, never a global default. With several limited tabs,
  delivery should be staggered rather than all at the reset minute.
- **The text.** A bare "continue" is right for Claude Code. Other runtimes may need
  something else; it belongs in the runtime adapter.

## 10. Build order

Steps 1–5 are **built** (2026-09-30), each reviewed until clean. Step 1 took three review rounds,
step 2 five (the draft check was rebuilt twice before the screen read), step 3 two. Step 6
remains.

1. `FollowUp` on `Tab` (Rust + TS), the lifecycle table in §3 wired and tested, including the
   duplicate-clears / reload-moves pair (and `cargo check --tests` for the test literal).
   The `follow_ups_enabled` preference and its toggle under the Overlord (§4), with
   `follow_ups_live` computed in one place for everything to read.
2. `tryDeliverNow` on the delivery controller, with tests. Frontend store + tick, time
   trigger only, idle gate. `createFollowUp` / `listFollowUps` / `cancelFollowUp`, with the
   own-tab refusal and `PEER_ADDRESSING_TOOLS` entry in Rust (§2).
3. The human side: badge, menu, list.
4. Stack and task triggers. **Built** (§5 "As built").
5. Resume-then-deliver (§6.2). **Built** (§6.2 "As built").
6. Phase 2 (§9), after the signal is proven on real limit events.

## 11. Open questions

- **Idle gate for Claude Code.** If mid-turn delivery proves harmless for Claude Code (it
  queues typed input), should the idle wait apply to Codex/Gemini only? Start strict; relax
  per runtime with evidence.
- **Minimum delay.** One minute is a guess. An agent that wants "in 30 seconds" probably
  wants `waitForService` or its own next turn, but that is untested.
- **Should an agent see follow-ups it didn't create?** `listFollowUps` returns every
  follow-up on its tab, including human-created ones. That seems right (it's the agent's
  tab), but the human may have meant one as a private reminder.
- **Overlord visibility.** Pending follow-ups are exactly what a supervisor wants to know
  before deciding a tab is stalled. Worth wiring once v1 is in use.
