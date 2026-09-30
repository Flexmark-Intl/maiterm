# maiTerm Follow-ups — an agent's own way to pick the work back up later

> Status: **building** from 2026-09-30 (proposed 09-26). Owner: Darryl. Lives under the
> Overlord: live only when the Overlord and **Enable follow-ups** are both on (§4).
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
| Backup restore (any import) | Restored, **deduplicated**, losses logged | Stale by then — the late handling in §6.3 is what keeps that from being a surprise. An import puts tabs back as straight clones, so a copy can land beside the tab its follow-ups have since moved to (A reloaded to A′, or A dragged to another pane, then the backup restored). `AppData::settle_follow_ups_after_import` restores the invariant that **no follow-up is held twice**. It judges "was already there" by **position** (workspace, pane, tab), not tab id, because a moved tab keeps its id. What an import drops (an overwrite discards tabs created since the backup) is derived as before minus after and logged. |

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
2. **Nothing has been typed since this stretch of idle began** (`AgentTabSession.idleSince`).
   Typing into an agent's input box fires no hook, so a tab reads idle while its human
   composes. A paste plus CR would submit their half-written draft with the follow-up glued on.
   This holds until they send it. It was first compared against `updatedAt`, which a session
   start deliberately doesn't stamp. So the keystrokes that relaunched an exited agent by hand
   (`claude -c`), which is exactly how an agent picks up its follow-ups, counted as a draft, and
   the follow-ups waited for a turn nobody was going to start. `idleSince` is stamped on every
   entry into idle, session start and `/clear` included.

   Holds that are safe but may surprise: clearing a draft (Ctrl-U, Esc) holds until the next
   turn, and so does a local command that starts no turn (`/status`). **Unverified:** a TUI's
   mouse reports go through xterm's `onData` and are stamped as human input. If a runtime
   enables mouse tracking, a click or scroll in its tab would hold follow-ups the same way.
   Excluding them would change what the Overlord's rituals count as a takeover too, so it is
   left alone until it is seen.
3. 1.5 s of output quiet.
4. The delivery controller would inject now (`canDeliverNow`).
5. **A live agent process** (`getAgentLiveness`). A session entry is cleared by the SessionEnd
   hook, which never arrives when the agent is killed or its ssh tunnel is down, and without a
   process the paste lands in a shell. This is the evidence the Overlord (`replState`) and comms
   use, with the same known gap: a remote agent that dies while its ssh stays up still reads as
   live.

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

- A **clock badge** on a tab with pending follow-ups, with a Tooltip listing them (count, next
  due, and its text). Never a native `title=`.
- Tab context menu → **Follow-ups…**: the list, with Cancel, Deliver now, and a way to add
  one by hand (`author: "human"`). This is Solo's "Set timer", on the tab rather than in a
  palette.
- Held rows (§6.2) show why they're held and offer Deliver, which does the resume-then-deliver
  path on demand.
- The Overlord board could show pending follow-ups per tab. Not in v1; it is a read of
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

1. `FollowUp` on `Tab` (Rust + TS), the lifecycle table in §3 wired and tested, including the
   duplicate-clears / reload-moves pair (and `cargo check --tests` for the test literal).
   The `follow_ups_enabled` preference and its toggle under the Overlord (§4), with
   `follow_ups_live` computed in one place for everything to read.
2. `tryDeliverNow` on the delivery controller, with tests. Frontend store + tick, time
   trigger only, idle gate. `createFollowUp` / `listFollowUps` / `cancelFollowUp`, with the
   own-tab refusal and `PEER_ADDRESSING_TOOLS` entry in Rust (§2).
3. The human side: badge, menu, list.
4. Stack and task triggers.
5. Resume-then-deliver (§6.2).
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
