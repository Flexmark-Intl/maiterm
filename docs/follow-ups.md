# maiTerm Follow-ups — an agent's own way to pick the work back up later

> Status: **proposed** 2026-09-26. Not built. Owner: Darryl.
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

pub enum FollowUpDue {
    At { at: String },                                   // RFC 3339, wall clock
    ServiceReady { service_id: String },
    ServiceStopped { service_id: String },               // stopped or crashed
    TaskDone { task_id: String },
}
```

`Tab.follow_ups: Vec<FollowUp>`, persisted with the tab.

**It lives on the `Tab`, which makes its lifecycle a set of decisions, one per path that
copies or moves a tab** (memory: reload-mints-a-new-tab-id). Each one here is deliberate:

| Path | Follow-ups | Why |
|------|-----------|-----|
| Reload (`carry_tab_state_on_reload`) | **Carried** | Reload copies the whole `Tab` record; the replacement is the same session. Free, and correct. |
| Duplicate / split clone / `clone_workspace_with_id_mapping` | **Cleared** | A duplicate that inherited them would deliver every follow-up twice, to two agents. Same treatment as `service_id`: a duplicate is built as a fresh `Tab`, and `clone_workspace_with_id_mapping` sets it to `None` explicitly (`commands/window.rs` ~593) — both places need the new field. |
| Move to another pane or workspace | Carried | Same tab, same session. (Unlike `service_id`, which `move_tab_to_workspace` clears because a service belongs to its workspace — `commands/workspace.rs` ~468. A follow-up belongs to the tab.) |
| Workspace Share export | **Never** | The share file is an allowlist of `Shared*` types (`docs/workspace-share.md`), so a new `Tab` field doesn't reach it unless added. Keep it that way: a follow-up is this agent's note about this session. |
| Archive | Held | Delivered on restore, marked late (§6.3). Archiving is reversible; dropping them would make it not. |
| Close | **Dropped, logged** | Nothing left to deliver to. |
| Backup restore | Restored | Stale by then — the late handling in §6.3 is what keeps that from being a surprise. |

The stack-service and task conditions reference ids. A service id survives reload; a task id
is stable. If the referenced service or task no longer exists when checked, the follow-up
**fires**, marked with the reason ("service `web` was removed") rather than waiting forever.
An unmeetable condition is still news to the agent.

## 4. MCP surface

Three tools, scoped to the calling tab, gated by a preference like the task and stack tools
(`follow_ups_enabled`, default on).

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

**Priming.** When follow-ups are on, the `initSession` / SessionStart priming says they exist
in one line, and lists this tab's pending ones. A resumed agent then knows what it already
scheduled instead of scheduling it again — the task system's re-send duplication problem
(`docs/tasks.md` §3, "Tab ids are not durable") in another form.

## 5. Triggers

- **Time.** Compared against the **wall clock** on every tick, never a stored duration. A
  laptop that slept through the due time wakes, sees the follow-up is past due, and delivers
  it marked late. A duration-based timer would silently shift everything by the sleep.
- **Stack service ready / stopped.** Read from the stack store's runtime status
  (`docs/stack.md`). "Ready" means the stack's own meaning — it announced an address — so a
  follow-up on a worker that never announces one waits until `expires_at`.
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

Delivery goes through **`agentDelivery`** (`src/lib/stores/agentDeliveryLive.ts`), the FIFO
mailbox the bridge and the mesh already share, under a new owner tag
(`DELIVERY_OWNER_FOLLOWUP`). One mailbox per tab is the point of that module: two
controllers would mean two `injecting` guards for one PTY, and a follow-up paste could land
inside a bridge message. It already holds while the agent is at a permission or elicitation
prompt, serializes injections, and backs off on failure.

**One addition: follow-ups wait for the agent to be idle**, where bridge messages don't.
`agentDelivery` delivers to a live session mid-turn, which suits a peer's reply. A
follow-up is never urgent, and some runtimes take input typed mid-turn differently from
Claude Code's queueing — Codex is unverified here. Waiting for a completed turn costs a
little latency and removes the question.

The tick runs in the frontend, where delivery has to happen anyway (it needs `claudeState`,
the adapter's `isAwaitingHumanInput`, and the PTY). Because WKWebView throttles timers in an
occluded window (memory: screen-sleep-webview-occlusion-stall), the tick is also run on
`visibilitychange` and window focus, and the wall-clock comparison (§5) makes a late tick
merely late, never wrong.

**Removed only after the inject succeeds.** A crash between writing the prompt and
persisting the removal delivers it twice on the next launch; the alternative loses it
silently. A duplicate "check the deploy" is the cheaper failure.

### 6.2 When the agent isn't there

| The tab | What happens |
|---------|--------------|
| Live agent, idle | Deliver. |
| Live agent, busy or at a human prompt | Wait. |
| Tab at a shell, agent exited, runtime resumable | **Resume the agent, then deliver.** The resume command is built the way `src/lib/agents/resume.ts` builds it (the runtime's session id from `trigger_variables`). Delivery waits for the resumed session to register and complete its first turn — the "ready is reachable too early" watch item in `docs/overlord.md` §4.0 is this exact risk. |
| Tab at a shell, not resumable (no session id, or an SSH tab whose connection is gone) | **Hold and surface.** The row goes to "due — agent not running" with a Deliver button (§8). Never typed. |
| Suspended tab or suspended workspace | Hold and surface. Waking a suspended workspace because a timer fired is the human's call, not a follow-up's. |
| Archived | Held; delivered on restore. |

Resuming the agent is the one place a follow-up types anything that isn't its own text, and
it types only what auto-resume would already type for that tab. It is still worth a
preference (`follow_ups_resume_agent`, default on) because it starts a session — and spends
quota — without the human there.

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

- **The signal.** Candidates: an entry in the transcript JSONL (the house rule is that agent
  facts come from the transcript, not the screen — memory: mailink-meta-fields-from-transcript),
  a hook, or the TUI's own text. The reset time has to be read out of it, with its time zone.
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
   duplicate-clears / reload-carries pair.
2. Frontend store + tick, time trigger only, delivery through `agentDelivery` with the idle
   gate. `createFollowUp` / `listFollowUps` / `cancelFollowUp`.
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
