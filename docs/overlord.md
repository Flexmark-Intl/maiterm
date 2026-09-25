# Overlord — Per-Window Project & Agent Supervisor

> Status: **implemented (v1)** — all four build stages landed 2026-08-22. Designed 2026-08-22.
> Owner: Darryl. Scope: a per-window supervisor that tracks projects/tasks across
> workspaces and drives the agents responsible for them, automating the routine
> supervision currently done by hand.
>
> Implementation map: engine `src/lib/stores/overlord.svelte.ts`; defaults
> `src/lib/overlord/defaults.ts`; UI `src/lib/components/overlord/` (board, rules
> section, rule-change modal); Rust facts/commands `src-tauri/src/commands/overlord.rs`
> + `mailink/transcript.rs` (tail facts); schema `src-tauri/src/state/workspace.rs`;
> MCP tools `src-tauri/src/claude_code/protocol.rs` + `server.rs`. Off by default —
> Preferences → Overlord → Enable (propose-mode starts on).

## TL;DR

- **The problem is memory, not intelligence.** Losing tasks across dozens of
  projects is a state problem; hand-executing the same supervisory habits
  (review after a complex commit, checkpoint before compaction, keep docs from
  drifting, make agents write todo lists) is a policy problem. Neither needs a
  model in the loop most of the time.
- **Overlord is two things.** *Overlord-the-engine* is deterministic, headless,
  always on: a rules engine over cheap state signals. *Overlord-the-agent* is a
  pinned tab you talk to, woken only for exceptions and conversation. The board
  on disk is truth; the agent's transcript is scratch.
- **~60% of it already exists.** The topic registry is a task registry, the mesh
  cockpit is the GUI seed, `build_meta` already computes context %, and
  `initializeMesh` already drives unready tabs. See [Reuse map](#reuse-map).
- **Overlord types raw, with no envelope**, exactly as the human would — which is
  what preserves its ability to command (and to send `/compact`). The safety
  therefore lives at the **tool boundary**, not in prompt adherence.
- **Rules are the product surface.** Declarative `state event → directive`, with
  the directive *text* as the tunable field, seeded and hideable like triggers.
- **v1 scope: global + workspace.** No labels, no tab ids, no session ids.
- **Overlord occupies its own workspace** — accessor row above the sidebar's
  `WORKSPACES` header — so the board gets full main-area real estate and the
  agent is an ordinary tab inside it.

---

## 1. The problem

> "I want to track hundred+ tasks across dozens of projects. The mesh workspaces
> are really working out great, but I often lose tasks in a workspace like EWS
> Mesh, and forget about work I'm doing in maiBooks. A lot of my time is
> micromanaging the agents to make sure they follow good practices."

Two distinct pains, which want different solutions:

| Pain | Nature | Solution |
|---|---|---|
| Losing tasks/projects | state | a durable board, fed automatically |
| Repeating the same supervision | policy | a rules engine |

The supervisory habits being repeated by hand today:

- spinning up a subagent to review a complicated commit
- getting agents to keep proper task lists / todos
- keeping docs and memory from drifting as code changes
- prepared compactions at opportune moments, rather than hitting the wall

---

## 2. Architecture — two Overlords

Splitting these is the load-bearing decision. If every nudge routes through a
model, the system is slow, expensive, non-deterministic, and — fatally —
**Overlord's own context window blows out in a day watching 40 tabs.**

**Overlord-the-engine** (deterministic, Rust/TS, headless)
: Task board, context gauges, staleness timers, rule evaluation, directive
  injection, ledger. Always on. Costs nothing. Handles the routine volume.

**Overlord-the-agent** (a real pinned tab, per window)
: Conversation with the human, and the *residue* the rules can't resolve
  deterministically. It **queries** the board rather than remembering it, so its
  context stays small and disposable.

### Three tiers, descending volume / ascending cost

| Tier | Path | Model involved |
|---|---|---|
| Rule fires cleanly | engine → target tab | none |
| Rule escalates | engine → Overlord → target tab | on exceptions only |
| Human instructs | human → Overlord → target tab | yes |

### One channel, same guards

Rules and the agent are two callers of **one injection tool with identical
guards**. The agent gets no privileged path — it physically cannot race a running
ritual, and everything it does lands in the same ledger with
`origin: 'overlord_judgment'`.

### Engine home

The engine is a **frontend per-window Svelte store** (`overlord.svelte.ts`),
mirroring `agentMesh`: per-window scoping falls out free (each window's webview
has its own store instance over its own `workspacesStore`), the injection
primitives (`deliverInit` / `bracketedPasteSubmit`) are frontend, and rules load
from the preferences store. Rust's only new surface is exposing facts it already
caches — contextPct, last-real-turn ts, todos — to the frontend via a small
command or event ticker (mirroring the mailink summary-ticker pattern).

### Agent lifecycle

- **Spawn**: the human creates the Overlord workspace (§11); the agent is a
  normal Claude tab inside it (Claude-only in v1). No special PTY plumbing.
- **Priming**: doctrine (§9.2) is injected at initSession, idempotent with a
  persisted-var guard — same mechanism as mesh priming (`tryPrime`).
- **Restart / compaction**: re-init → re-primed with doctrine, re-reads board +
  ledger. The transcript-is-scratch principle makes this loss-free by design.
- **Absence**: the engine runs regardless. With no live Overlord agent,
  `escalate_to_overlord` degrades to `notify_human` and escalations stay queued.

---

## 3. Authority model

Overlord acts **with the human's authority** and injects raw text into the target
tab's PTY. There is deliberately **no `⟦OVERLORD⟧` envelope**, for two reasons:

1. An envelope re-creates the trap it's meant to avoid — agents start holding
   work "for the human" and stop trusting Overlord's orders.
2. You cannot wrap a slash command. `⟦OVERLORD⟧ message: /compact` is not
   `/compact`. Raw injection is the only mechanism that works uniformly.

The resulting split:

> Agents **know Overlord exists** (they have a tool to report to it).
> Agents **cannot tell an Overlord directive from the human typing**.

Because the envelope is gone, **the guardrails must be mechanical, at the tool
boundary** — prompt adherence alone is one soft line of defense for something
that types with full authority into dozens of tabs.

### The three mechanical guards

**`require_live_repl`** — the real hazard is not a misbehaving agent, it's an
*absent* one. If a tab's agent has exited, `"spin up a background review of that
commit"` becomes a **bash command at a shell prompt**. Live-agent-REPL must be a
hard precondition of injection, never best-effort. Signals already exist:
`agentState` + `get_agent_liveness`.

**`only_if_no_outstanding`** — directives serialize per tab. This makes ritual
state machines correct *and* makes `directive_id` optional in the ack protocol
(§7).

**`max_per_hour`** — loop-control backstop, same lesson as mesh topic loop
control.

### The injection tool: `driveTab`

The "one injection tool" both callers share, made concrete:

```ts
driveTab({ tab_id: string, kind: 'process' | 'slash', text: string })
→ { sent: true }
  | { sent: false,
      reason: 'no_live_repl' | 'outstanding_directive' | 'already_running'
            | 'agent_busy' | 'awaiting_permission' | 'not_registered'
            | 'not_classified' | 'rate_limited' | 'runtime_mismatch',
      detail?: string }
```

- Guards are evaluated **inside** the tool — a guard failure comes back to the
  agent as a structured refusal, never a silent drop, and both paths ledger.
- **A refusal must say what is holding the tab.** The ritual branch was once the
  only one without a `detail`, and a bare `outstanding_directive` reads as "busy,
  retry" — so the agent retried a step the engine was already part-way through
  sending, three times in ninety seconds, and the human got every directive twice
  (2026-08-31). It now names the rule and its position in the sequence. When the
  refused text **is** one of that sequence's steps, the reason is
  `already_running` rather than `outstanding_directive`: the two call for opposite
  responses, and only one of them is "wait and retry". `already_running` means
  *the engine is doing this; stand down* — see §9.2.
- **Only a `process` directive holds the tab.** It takes the `outstanding` slot and
  arms `driveWatch`, because it asks something and the answer has to come back. A
  `slash` send takes neither: `/model`, `/effort`, `/compact` produce no answer, so
  nothing could release the slot but the 15-minute sweep, which then raised a
  `directive_unacked` card per tab. An agent told to send `/model default` then
  `/effort medium` to every tab got the first command out, a refusal on every second
  one, and ~105 "no reply" cards on the way (2026-09-25). This is the rule rituals
  already follow — a step holds the slot only when it sets `await`. Back-to-back
  sends are still kept off a working tab by the `idle` check, which a slash that
  starts a turn (`/compact`, a skill) trips by itself. The cost: a skill invoked as
  `slash` has no return leg, so the tool description says to send it as `process`
  when the output matters.
- MCP-exposed to Overlord-the-agent only (never to supervised agents).
- **Pre-approved via allowlist** — the guards are mechanical, and a
  permission-prompt-per-injection would make Overlord useless. The contrast is
  deliberate: `driveTab` is free, `proposeRuleChanges` (§10) always prompts.

### The ledger

Because injections are *by design* indistinguishable from the human typing,
Overlord's own log is the only way to ever reconstruct "who told maiBooks to do
that at 3am."

```ts
export interface OverlordLedgerEntry {
  id: string; ts: string; tab_id: string; workspace_id: string;
  rule_id: string | null;            // null = human-originated
  origin: 'rule' | 'human' | 'overlord_judgment';
  step_index: number;
  text: string;                      // VERBATIM injected bytes
  kind: 'process' | 'slash';
  outcome: 'sent' | 'blocked_no_repl' | 'blocked_guard' | 'acked' | 'timed_out'
         | 'aborted'           // human interrupt or app restart killed the ritual
         | 'skipped_runtime';  // slash step invalid for the tab's runtime (§6)
}
```

Ring-buffered per window, persisted. Cheap to build; it's what makes a
no-envelope design debuggable rather than spooky.

### Propose-mode

For the first weeks, rules fire into the board as *proposed* directives that the
human clicks to send. Builds trust in the boundaries and generates the corpus
needed to tune the ruleset before it runs autonomously.

---

## 4. Reuse map

Verified in the current tree — Overlord is mostly an aggregation layer.

| Need | Already exists | Where |
|---|---|---|
| Task registry | Topic registry: owner, participants, turn counter, open/complete, normalized dedup, TTL sweep, loop control | `src/lib/stores/meshRouting.ts` |
| GUI seed | Mesh cockpit | `src/lib/components/MeshCockpit.svelte` (400 lines) |
| Context % per tab | `contextPct` from transcript tail, cached | `src-tauri/src/mailink/mod.rs:3174` (`build_meta`) |
| Transcript tail parsing (Claude JSONL **and** Codex rollouts) | `tail_facts` / `meta_for` | `src-tauri/src/mailink/transcript.rs` |
| Driving unready tabs | Headless readiness pass built for maiLink's one-tap | `agentMesh.svelte.ts:405` (`initializeMesh`) |
| Injecting into a PTY without colliding with a live turn | Repaint-quiescence wait then `bracketedPasteSubmit` | `agentMesh.svelte.ts:168` (`deliverInit`) |
| Agent liveness / state | `active` / `idle` / `permission` / dormant | `agentState.svelte.ts`, `get_agent_liveness` |
| Rule lifecycle (seed / hide / restore / auto-update unmodified) | `DEFAULT_TRIGGERS`, `seedDefaultTriggers`, `hidden_default_triggers`, `user_modified` | `src/lib/triggers/defaults.ts` |
| Window scoping | Each window's `workspacesStore.workspaces` *is* its universe | `notificationDispatch.ts:104` |
| Facts for tabs whose session is on another host | SSH transcript mirror — remote JSONL + task board shadowed locally | `src-tauri/src/mailink/mirror.rs` |

### 3.1 Derived signals self-clear; queued ones need a sweep

The triage deck mixes two kinds of card and they have opposite lifetimes.

**Derived** — pressure, permission, unready, spent — are computed from the workspace tree
every tick. Close the tab and they stop being produced, with no cleanup anywhere.

They are derived from `fleet`, which is built from `workspaces.filter(w => !w.overlord)` —
and that exclusion is right for all four. The Overlord agent is the supervisor, not a
supervised tab: `isBoardableTab` refuses it the lifecycle tools, so a re-bind card would
offer a button that cannot fire, and an Archive/Close card would offer to put away the
supervisor itself. **One state is the exception.** The agent's only sanctioned way to reach
its human is `AskUserQuestion`, which stops it dead, and supervision of the entire window
stops with it — while the board the human opens to find out why showed an empty deck. So a
single extra signal, `supervisorBlocked`, derived separately rather than by widening `fleet`,
carrying the same "Open tab" remedy as any permission card and sorted **above the
escalations**: a blocked supervisor is usually why there are no newer ones. (An
`AskUserQuestion` does read as `permission` here — `PreToolUse` sets `active`, but Claude
fires a `permission_prompt` Notification while the ask waits, and that supersedes it.)

**Queued** — proposals and escalations — are arrays the engine appends to. Nothing removed
them when their tab went away, so closing a tab left its card on the deck, most visibly a
`Re-bind a running agent` proposal offering to Send into a PTY that no longer exists.

`sweepClosedTabs` (per tick) drops both queues for tabs the window no longer has, and with
them the tab's engine bookkeeping — a dozen maps that only ever grew. Three rules it must
keep: never sweep on an empty workspace tree (that means "still loading", not "all closed");
abort a running ritual via `run.aborted` rather than deleting its map entry, which the
ritual's own `finally` owns; and drop a `driveWatch` on a closed tab silently, since expiring
it would escalate "no reply could be read" about a tab the human deliberately closed.

Anything added to a queue keyed by tab id belongs in this sweep.

**A tab going away is not the only way a queued item stops being true.** The
`permission_stuck` handoff is the case that showed it. A tab stops at a prompt, the engine
hands it to the agent (Overlord may not answer a prompt itself — §3), and the agent goes to
work on it, often by putting the question to the human with `AskUserQuestion`. Then the human
does the obvious thing and answers the prompt *in the tab*. The derived `permission` card
vanished, because derived; the handoff did not, because queued. Undelivered, it still rang
the doorbell and sent the agent chasing an open gate. Delivered, it left the supervisor
blocked on an answer nobody was going to give, because the human had already answered
somewhere else — Overlord hanging on a problem that no longer existed.

`permissionHandoff` records the escalation ids per tab, and `sweepResolvedPermissionHandoffs`
withdraws them the moment the tab leaves `permission`: every ask still in the queue is pulled,
and a stand-down goes out only if at least one had already been read, since an agent acting on
it is owed the correction and an agent that never saw it is owed nothing. Four things it has
to get right, three of which it got wrong first:

- **A list per tab, not one id.** A `permission_pending` rule re-fires on the same still-open
  prompt every cooldown (`permissionSince` stays set while it sits), so one gate can queue
  several asks. Remembering only the newest withdrew one of N and delivered the rest later.
- **`answerPrompt` withdraws too.** The agent answering the prompt is the doctrine's success
  path, and it also takes the tab out of `permission` — so the sweep would have followed the
  agent's own fix with "that was answered, and not by you". Answering through maiTerm is the
  one resolution the engine can attribute; the stand-down may therefore say the tab was
  answered elsewhere, because it now knows it wasn't answered here.
- **Say WHICH way it cleared.** `idle`/`active` means answered; no state at all means the
  agent exited and took the prompt with it. Those call for opposite next moves.
- **Ordering, plus `sweepClosedTabs` seeding.** It runs after `sweepClosedTabs` so a tab the
  human closed is never reported as a prompt that got answered — but that only works if the
  closed-tab sweep can *see* the tab. `deadTabs` was seeded from four maps, and the ritual
  raise site records a handoff and returns before `setOutstanding`, dropping its `rituals`
  entry in the same `finally`; the tab was then in `permissionHandoff` and nothing else, so
  the sweep returned early and the handoff outlived it. `deadTabs` is now seeded from every
  per-tab map, which is what its docstring always claimed.

**The `blocked` card was the second instance, found from the phone (2026-09-10.)** Reported
by the maiLink agent: conversations stuck with an "Escalation" tag and nothing pending. Same
shape as the handoff exactly — a card derived from a condition that changes on its own,
stored as a queue only a human could empty.

`handleAgentReply` escalates on a bare `state: 'blocked'`, which is precisely the call
`replyToOverlord`'s own description instructs ("'status' for a state change worth recording
(e.g. blocked)" beside "Set needs_human ONLY for things a human must decide") — so an agent
following the documented contract raised a human card it was told it wasn't raising. `blocked`
is not in `AGENT_ONLY_ESCALATIONS`, so the agent's pull marks it read rather than deleting it,
and the only exits were a human dismissing it or the tab dying. A later report overwrote
`agentReports` and never touched the escalation list, so unblocking was invisible. On the
phone, whose inbox badges rows off `escalations[].tabId`, each one is a permanently flagged
conversation.

Not a deliberate call: `17462b7` set out to give the declared-but-unproduced `blocked` kind a
producer, and is silent on audience and lifetime. The same commit fixes an `AGENT_ONLY` leak
in the *other* direction, so the split was on the author's mind and this was simply not
checked against it.

`withdrawBlocked` drops a tab's open `blocked` cards when it reports a recovered state,
mirroring the permission withdrawal. Three boundaries:

- **An ALLOWLIST of recovered states (`working` / `done` / `idle`), not `!== 'blocked'`.**
  `state` is declared required with a four-value enum, but this is the hand-rolled JSON-RPC
  server and nothing enforces it at runtime — the gap `coerceStatus` exists to cover on the
  task side, whose docstring says agents do drift from a declared vocabulary. `!== 'blocked'`
  handed a malformed report retraction power it never had: a tab reporting `blocked`, then
  `state: 'stuck'` while still stuck, had its card deleted with nothing to re-raise it, and
  the deck and the phone both cleared while the tab sat blocked. Before the withdrawal
  existed an off-vocabulary state was inert in *both* directions; it stays that way.

- **Only the bare-blocked path.** `needs_human` / `kind: 'escalate'` files `agent_report`,
  which stays queued — an agent can raise a question and go do other work, and the question
  does not stop needing an answer because the asker got unblocked. Withdrawing there would
  clear a card the human never saw, on the strength of the agent's own later activity.
- **Withdrawal, not deriving the lane from `agentReports`.** Deriving is the cleaner end
  state and is what §3.1 argues for, but it stops `blocked` being an escalation at all,
  changing what the mirror publishes — and maiLink anchors its Recover button to
  `kind === 'blocked' || kind === 'step_timeout'`, deliberately, so recover sits where the
  symptom is. Deriving without coordinating would remove `recoverTab` for exactly the case it
  exists to serve. Parked on the board with that constraint written down.

**Two more from the same report, not yet fixed.** `step_timeout` is *not* the same bug — it
records an event (the escalate is followed by `return`; the ritual aborted), so there is
nothing to withdraw. But it is produced by the step behaviour literally named
`escalate_to_overlord`, which sits beside a separate `notify_human` option, and it is not in
`AGENT_ONLY_ESCALATIONS` — so a rule author who explicitly chose the supervisor gets a human
card anyway. Same family: **the kind silently decides the audience, and nobody checked a
kind's audience against its producer's intent.** Worth sweeping every kind against its
producers rather than fixing one.

`agent_report` was the next one swept (§9.1.1, 2026-09-15) and it failed in the *other*
direction: a kind whose producer says "only a human can decide this" was delivered to the
agent as well, which then spent its one channel re-asking the human a question the tab was
already asking them. `step_timeout` is still open — see the board task, and read §3.1's
maiLink constraint before moving it, because the phone anchors its Recover button on that
kind.

### 3.1.2 `directive_unacked` measured the wrong thing, twice

Confirmed three times in four days (Sep 7, 10, 11), every one a false positive at a tab that
was demonstrably alive and working on the very directive it was reported for — a subagent
review 9m58s in, a Bash command mid-run, a Railway cert poll with 5–6 minute command
timeouts. The escalation's advice ("driveTab it, or recover the tab") would have interrupted
work proceeding correctly, and the operator stood down each time.

**The smoking gun: `DIRECTIVE_UNACKED_MS` is `600_000`, and the step gate's own deadline is
`(step.timeout_seconds ?? 600) * 1000`. The same 600 seconds.** Two independent timers on one
directive, racing, and the one that won reported the more alarming fact. 9m58s is that race
landing on the wrong side.

So the primary fix is not a better clock, it is **not running a second clock at all for a
directive a ritual already owns**. `awaitGate` is watching it, with the deadline and the
`on_timeout` behaviour the rule author chose. The card's own stated purpose — directives
"whose only feedback channel is the ack that never came" — is exactly untrue of a mid-ritual
directive. Every `ruleId !== null` directive comes from a ritual, so `!rituals.has(tabId)`
removes all three observed cases on its own.

**The clock still had to change for what's left.** `ruleId === null` directives are either
`driveTab` (already excluded by `driveWatch`) or the census track-request, which clears on
turn end — and would false-fire on the same long turn. It now measures from
`max(sentAt, lastActiveAt)`, so the clock only runs while the tab is NOT working.

Deliberately *not* "never fire while active", which was the first proposal. A directive
swallowed by a mid-turn paste leaves the tab active on something else entirely, and that is a
live failure class here (a swallowed paste is instrumented, not cured). Measuring idle
time still surfaces a swallowed directive once the tab goes quiet; suppressing on `active`
would lose it permanently. The message reports both durations, since "sent 40 min ago" and
"not working for the last 11 of them" are different facts and only the second fired it.

**The idle clock alone was a regression, caught in review.** It can be pinned at zero for the
life of a tab, and the tabs where that happens are exactly the ones whose directive can never
clear — so suppressing on idleness removed the only notice an operator ever got that a tab had
jammed. Two reachable ways:

- The census track-request's only practical clearing path is `f.last_turn_ts > od.sentAt`, and
  `overlord_tab_facts` returns no facts *at all* for a Codex/Gemini SSH tab or one whose bridge
  is down — while `scanWorkspaces`, which decides who to ask, keys off `claudeStateStore` and
  will happily ask such a tab. A working agent then resets the idle clock forever.
- A tab whose `Stop` hook is lost stays `active` permanently: `agentState`'s stale timer
  re-sets the same state rather than timing it out.

`DIRECTIVE_MAX_OUTSTANDING_MS` (30 min) is the other half — an absolute ceiling, whatever the
tab is doing. The three original false positives are ritual directives, so the ritual guard
still suppresses them and the ceiling never sees them.

**The card now says what actually works.** It used to send the reader to `driveTab`, which is
the one thing that cannot work on a jammed tab — it refuses a tab that already owes an answer
(`outstanding_directive`). Nothing releases that slot except the tab answering or the tab going
away: **`ackOutstanding` exists but is wired to no control**, so there is no manual release.

**`drive_reply` and `directive_unacked` used to double-fire on one directive.**
`harvestDriveReplies` dropped the watch on expiry but left `outstanding` to a `setTimeout`
armed for `DRIVE_WATCH_MS + 1000` — leaving roughly one tick in five where the watch was gone
and the directive was not, so the unacked check raised a second card about the directive
`drive_reply` had just reported. The slot is released at expiry now, under **both** of the
timeout's guards — text and age; the timeout stays as the backstop for paths where the loop
does not run.

Text alone is not enough, and the reason is worth keeping: `outstanding` and `driveWatch`
desynchronise routinely. The harvest deliberately KEEPS the watch on an empty read (the pasted
directive is itself a user turn, so `last_turn_ts` moves on delivery), while the tick's first
branch clears the slot on that very signal — so a watch outlives its directive by up to 15
minutes, and a ritual step can take the slot meanwhile. With the *same text*, which is not a
coincidence: `driveTab` matches step text verbatim to detect an agent hand-driving a ritual the
engine owns, so identical strings are designed for. Text-only would clear a minute-old ritual
directive out from under a running `awaitGate` — taking the board's badge, the phone's row, and
for an `ack` gate any possibility of the gate resolving at all.

**The census directive was telling agents to jam their own tab.** `TRACK_REQUEST_TEXT` asked an
agent with no task tools to answer with `replyToOverlord kind:'status'` — and only `kind:'ack'`
releases the outstanding slot. An agent that complied exactly blocked every
`only_if_no_outstanding` rule and every `driveTab` on its tab until someone reloaded it. That
is the case the 30-minute ceiling now reports, so the directive was fixed rather than left as
the thing generating the reports: `ack` carries `task` and `summary` identically, so the census
still gets its answer and also closes the directive it is answering.

**What this does NOT fix, and the cost attribution that was wrong.** The tab being blocked —
"nothing else can be sent" — comes from the outstanding directive itself via the
`only_if_no_outstanding` guard, not from the escalation. The tab stays blocked for those ten
minutes either way; the escalation only reported it. Holding the slot while a ritual is
genuinely mid-sequence is the honest state, so it stays held (decided 2026-09-11, from three
incidents where nothing needed sending to those tabs). The false alarm and the bad advice
were the real costs and both are gone.

### 3.1.1 `escalate` dedupes one kind, and the exclusions are the point

`escalate` was a bare append, so a chatty agent stacked a card per report and nothing
disposed of them. It now refreshes the open card for `DEDUPED_ESCALATIONS` kinds in place, so
the board carries the agent's latest reason — a stale card is a subtler lie than a duplicated
one. Three fields behave deliberately:

- **`ts` is NOT refreshed.** It means "raised at", and that is the more useful reading here:
  a tab restating `blocked` every 30s for two hours would otherwise read as "30s" forever on
  both the deck and the phone. Stacked duplicates used to convey the duration, loudly — losing
  it while fixing the noise trades one bad reading for another. The card reads "raised 2h ago,
  latest reason: …".
- **`read` is preserved**, so a refresh does not re-ring the doorbell. The human loses nothing
  by it: `publishMirror` sends `humanEscalations`, which filters on kind and *not* on `read`,
  and the deck renders read cards until dismissed — both human surfaces show the update. Only
  the Overlord agent misses it, and only after it has already been told the tab is blocked.
  Do **not** "fix" that with `unNudged.add()` on the refresh path: `wakeOverlordAgent` prunes
  every id that is not `!read` before doing anything, so the line is silently inert.
- **`workspaceId` is re-resolved**, falling back to the stored value rather than `''`. A tab
  can be dragged between workspaces and a deduped card is never re-created, so without this
  the row keeps the workspace it was first raised in permanently.

The set is **`blocked` alone**. A wider net was proposed ("the un-self-clearing kinds") and
would have been worse than the bug:

- `agent_report` is a distinct ASK each time. Two questions from one tab collapse into one,
  the human answers the second, and never sees the first.
- `step_timeout` is a distinct EVENT — this rule, this step. Two rules timing out are two
  facts.
- `directive_unacked` fires once per DIRECTIVE — `unackedNotified` is latched on the
  `OutstandingDirective`, not the tab, so a cleared-then-reissued directive can raise a second
  card while the first is up. Correct: two directives are two facts with different text.
- `AGENT_ONLY_ESCALATIONS` kinds are deleted on delivery, so they never accumulate on the
  board. `permission_stuck` in particular **must** keep stacking, for the list-per-tab reason
  above; `task_handoff`/`task_dropped` are per-task, and `sendTaskToOverlord` raises the
  former with `tabId: ''` for an unassigned task, so `(tabId, kind)` would collapse every
  unassigned handoff in the window onto one card.

### 3.2 A proposal is a snapshot, so it has to be re-read

A proposal records what was true when the rule matched, then waits for a human. Nothing
re-read it, so it aged into a lie: a compaction staged at 23:31 for a tab genuinely over
55% was still on the board at 09:58 — after that tab compacted at 01:38 and dropped to 8% —
offering to compact it again. Approving it would have thrown away a tab's whole working
context to save nothing, and `run all` fires the entire queue without anyone reading cards
one by one.

`proposalStillHolds` is checked three times: on the tick (so the card *disappears* rather
than being refused later), at `approveProposal`, and in `runTriage`. It returns false when
the rule was deleted or disabled while the proposal waited, and then splits by condition:

- **Level conditions** (`context_pct`, `tab_idle`, `task_stale`, `agent_unready`,
  `no_todo_list`, `permission_pending`, `directive_unacked`) describe a state, so they are
  simply re-evaluated with `conditionFires`.
- **Edge conditions** (`turn_end`, `commit`) describe a moment that has already passed —
  re-checking them would void every edge proposal the instant it was queued. They get an age
  backstop (`PROPOSAL_STALE_MS`, 4h), and `commit` additionally dies when a NEWER commit
  lands, because the directive names *the* last commit and now names the wrong one.

The general rule: **anything queued for later approval must be re-read against the present
before it acts, and a card that has stopped being true must stop being shown.**

### 4.0 "No live REPL" is two facts, and only one of them means dead

`require_live_repl` guards against a directive landing in **bash**. That needs a live agent
*process*. Routing a reply back needs a *registered* session — `claudeState` is fed only by
hooks, and (when this was written) a hook could not name its tab until `initSession` bound the
connection. That second half no longer holds: the SessionStart command hook now POSTs
`?tab_id=$MAITERM_TAB_ID` itself, so a tab registers when its agent STARTS rather than when it
first calls a tool. `unbound` should therefore become rare — see the watch item below.

Those are different facts and were one boolean. A tab resumed from a previous session has a
running agent and no registration, so the merged test said `no_live_repl` — a phrase that
means the terminal is dead. It isn't; it just hasn't said hello. That refusal sent the
supervisor, and then the human, looking for a dead terminal, and the remedy was blocked by
the guard that reported it: the only way to reach an unregistered tab is to type
`/maiterm init` into it, and typing into a tab is what `driveTab` does.

`replState` now returns `ready | unbound | stopped | unknown | no_terminal`, and:

- `unbound` refuses with `not_registered`, **and sends `/maiterm init` itself** (via
  `recoverTab`, the same call the deck's Re-bind button makes, with the same
  don't-type-over-live-output rule), telling the caller to retry. Rate-limited by
  `rebindWatch` so repeated calls don't re-type it inside the verify window.
- `unknown` (the tick's batched probe never classified the tab — its pane isn't mounted)
  refuses with `not_classified`, not with "dead".
- `stopped` keeps `no_live_repl`, which is now true when it is said.

**Watch item (undeployed as of 2026-08-25): `ready` is now reachable earlier than it used to
be.** Registration used to *imply* the agent had taken a turn, because only `initSession` could
create it. Now the SessionStart hook does, at process start — so `ready` (registered AND
running) can be true while Claude is still replaying a resumed transcript and not yet reading
input, and a `driveTab` directive injected then may go unread. It fails silently, which is the
failure mode this guard exists to prevent, so it is worth watching for on the first deploy:
a directive acked by nobody, with the tab's own next turn showing no sign of it. The fix, if it
bites, is one extra condition — require the tab to have been `idle` at least once (a completed
turn), which is what `agentDelivery` already models for mesh and bridge. Deliberately not done
up front: the bytes land in the tty input buffer and raw mode preserves pending input, so it may
never actually bite.

Cost note: the unregistered branch reads the tick's batched classification rather than
firing its own probe. `get_agent_liveness` TTL-caches the process sweep but *not* the
per-call BFS, which is why `get_agent_liveness_batch` exists.

### 4.05 The context gauge reads the transcript, never a status line

`context_pct` comes from `claude_meta_from_tail` walking the JSONL tail. maiTerm never
receives Claude's status line at all, so there is nothing to reconcile with it and no
dependency on the user having configured one — advice framed as "status line normally,
transcript on compaction" doesn't apply here; the transcript is the only source.

Two readings, newest wins, which the reverse scan gives for free:

- an assistant turn's `message.usage` (`input_tokens + cache_read + cache_creation`);
- `compact_boundary.compactMetadata.postTokens`.

The boundary is not a nicety. Between a compaction and the session's next assistant turn,
the newest `usage` line is the one from *before* the compaction — so a tab just compacted
down to a few percent still read as nearly full, which is precisely when the checkpoint rule
decides whether to spend that tab's whole context compacting it again. The boundary record
states the post-compaction size at the instant it happens, and the model id still comes from
the older assistant line (the boundary doesn't carry one, and the percentage needs it).

Placeholder `usage` records are skipped whole — **including their `model` field**. Claude
Code writes an assistant record with all-zero usage and `"model":"<synthetic>"` for
`API Error: …` and `No response requested.`, and it stays the newest usage-bearing line
until the session takes another turn (measured across this corpus: 280 such records, median
dwell 48s, some permanent where the session ended on the error). `context_limit_for` doesn't
recognise `<synthetic>`, so letting it answer the model question swaps a 1M window for 200k:
153,715 tokens reads as **77% instead of 15%**, clearing the default 55% checkpoint — and an
API error leaves the agent idle with a quiet PTY and a live REPL, so every guard passes. The
gauge meant to prevent a needless compaction would have caused one.

The floor itself is 2, not a "plausible session size": a higher bar would work on live data
but would be invented rather than observed, and would silently discard a genuine small
reading.

**Known limitation, bounded.** When the boundary supplies the tokens but no assistant line
survives in the 256 KB tail to supply a model, the reading is returned with `model_id: None`
and the caller guesses a 200k window — 7 of 481 real compactions in this corpus. That
overstates a 1M-window tab (2% reads as 12%) and drops the model label from the phone gauge.
It cannot trip the checkpoint rule: `postTokens` maxes at 39,697 across 414 compactions, so
even against a 200k guess it never reaches 55%. Still strictly better than the pre-boundary
behaviour, which reported the pre-compaction number.

**No PostCompact hook is needed for this.** The transcript already carries the authoritative
number, `SessionStart` already fires on compaction so the tab re-resolves to the new session
id, and the facts poll re-reads the tail every 5s. A hook would only shave latency off a
reading that is already correct.

### 4.1 SSH tabs: where the facts come from

Everything in §5's detection table resolves a session id to a JSONL **on this machine**.
An SSH tab's session writes on the remote host, so without help Overlord sees no context
gauge, no last-turn recency, and no commit or todo edges for the largest tabs in a fleet.

The mirror closes that. `hooks_handler` gets `transcript_path` verbatim on every Claude hook
— a hook event *is* the "something appended" signal — and fetches the byte delta over the
bridge tunnel's ControlMaster socket into `<data_dir>/<slug>/remote-transcripts/<sid>.jsonl`.
`locate_jsonl` searches that directory alongside `~/.claude/projects`, so every fact lights
up unmodified. The same fetch brings back the remote task board, which is why an SSH tab
gets its **complete** list rather than whatever fit in the transcript tail's window.

It was built for maiLink and gated on the phone bridge being up, which is why Overlord was
blind here: the shadow file simply never existed unless a phone happened to be connected.
The gate is now "does anything want shadows" — maiLink running **or** Overlord enabled.

Overlord deliberately does **not** drive refreshes from its own 5s facts poll. maiLink's
slow tick exists for mid-turn streaming to one open chat; Overlord watches the whole window,
and every fact it reads changes at a turn boundary, which is exactly when a hook fires.
Poll-driven refresh would cost one ssh round trip per SSH tab per tick — scaling with fleet
size instead of with activity — to chase data a hook already announced.

Three things this does not fix, and Overlord must keep reading as *unknown* rather than as
*nothing happening*:

- **Claude only.** The mirror is Claude-scoped; Codex and Gemini SSH tabs have no facts.
- **Needs a live bridge tunnel.** No tunnel entry for the tab ⇒ no mirror. Overlaps with the
  `rebindFailed` class in §11 — a tab whose bridge never came up is invisible twice over.
- **"Finished everything" is not detectable remotely.** The local store distinguishes a swept
  task directory (`Some(empty)` — completed all of it) from one that never existed (`None`).
  The remote dump is the task files' bytes concatenated, so both arrive as nothing; the
  shadow path is two-state and falls through to the tail rather than claim a completion.

---

## 5. Rule schema

Mirrors `Trigger`'s lifecycle contract exactly, so the seed/hide/restore
machinery and the preferences UI carry over.

```ts
// src/lib/tauri/types.ts  (Rust mirror in state/workspace.rs, snake_case serde)

export interface OverlordRule {
  // ── Identical lifecycle contract to Trigger ──────────────────────
  id: string;
  name: string;
  description?: string | null;
  enabled: boolean;
  workspaces: string[];        // [] = global (this window)
  cooldown: number;            // seconds, per-tab
  default_id?: string | null;  // seeded from DEFAULT_OVERLORD_RULES
  user_modified?: boolean;     // freezes it against template updates
  origin?: 'default' | 'user' | 'proposed';

  // ── What replaces `pattern` / `actions` ──────────────────────────
  when: OverlordCondition;
  guards: OverlordGuards;
  sequence: OverlordStep[];    // 1+ steps; multi-step = ritual
  supersedes?: string[];       // rule ids / default_ids this replaces in-scope
}
```

`pattern` and `actions` do not survive the port: these fire on **semantic state**,
not on output text, and the payload is a directive rather than an action enum.

### Conditions

```ts
export type OverlordCondition =
  | { event: 'context_pct';       at_or_above: number }      // 55
  | { event: 'turn_end' }
  | { event: 'commit' }
  | { event: 'tab_idle';          minutes: number }
  | { event: 'task_stale';        days: number }
  | { event: 'agent_unready' }                                // agent alive, no binding
  | { event: 'no_todo_list' }
  | { event: 'permission_pending'; minutes: number }
  | { event: 'directive_unacked';  minutes: number };         // the TTL sweep
```

**Detection cost** — the honest accounting, and the main build estimate:

| Event | Source | Cost |
|---|---|---|
| `context_pct` | `build_meta` → `contextPct` | free, cached |
| `turn_end` | `transcript.rs` tail | free, cached |
| `tab_idle` | last-real-turn ts (already in mailink) | free |
| `agent_unready` | `agentState` + `get_agent_liveness_batch` | cheap (batched, candidates only) |
| `permission_pending` | `agentState` | free |
| `task_stale` / `directive_unacked` | board timers | trivial |
| `no_todo_list` | **TodoWrite mirror — needs building** | medium |
| `commit` | **needs building** | medium |

`commit` has no clean existing source. Options, best first:

1. parse `Bash(git commit)` tool calls out of the transcript tail — most
   reliable, attributes to the right agent, won't fire on Gemini
2. poll `git log` per tab cwd — cheap, coarse, can't tell which agent
3. OSC 133 + command text — breaks on SSH tabs

(1) shipped, with one correction. A `tool_use` block is the agent *asking* to run a
command; it says nothing about whether the command worked. Reading it as a commit made
`last_commit_ts` advance on a denied permission prompt, a pre-commit hook that refused,
and `nothing to commit` — **32 of the 278 `git commit` calls** in the local corpus, each
one able to send a tab to review a commit that is not in the history. The verdict is one
line further down the same file, so `commit_outcome` reads it. Three things that took two
passes to get right:

- **Unknown is not a commit.** A call whose result has not arrived yet is `Pending` and
  produces no fact. The gap runs a median 1.6s but exceeds a whole 5s tick in 12% of
  failures, and an edge latched inside that window is never withdrawn — `latchEdges` drops
  an edge on age-out or a human keystroke, and a classifier denial or hook refusal is
  neither. So the correction would land after the directive was typed. Roughly a third of
  failed commits still fired the rule while the fact merely *stopped advancing*.
- **`is_error` is the exit status of the whole shell command**, not of the commit. In
  `git commit … && git push`, a rejected push errors over a commit that is in the history
  (2 of the 38 errored compound commits locally). Git naming a new sha — `[main 1a2b3c4]` —
  is the positive evidence that separates those; `git commit -q` prints nothing, so one of
  those two stays a false negative and that is the accepted floor.
- **Walk every occurrence of the id.** Subagent `progress` frames carry `tool_use_id` too
  and appear before the real result.

The general form is the one §9 keeps arriving at — **a request is not an outcome**; when
the outcome is recorded nearby, read that, and treat "no verdict yet" as no fact rather
than as the answer you were hoping for.

`turn_end` is defined as the tab's agentState transition `active` → `idle`
(hook-driven for Claude/Codex), with the transcript-tail last-real-turn
timestamp as the fallback for hook-less runtimes.

### Guards

```ts
export interface OverlordGuards {
  agent_state?: ('idle' | 'active' | 'permission')[];  // default ['idle']
  min_quiet_ms?: number;             // reuse deliverInit's detect; default 3000
  require_live_repl: boolean;        // the shell-injection gate — see §3
  max_per_hour?: number;             // per tab
  only_if_no_outstanding: boolean;   // serialize directives per tab
}
```

### Steps

```ts
export interface OverlordStep {
  kind: 'process' | 'slash';    // free-text directive | slash command
  text: string;                 // ← THE tunable field
  runtimes?: AgentRuntime[];    // slash steps only; omit = all — see §6
  await?: OverlordGate | null;  // null = fire-and-forget
  timeout_seconds?: number;
  on_timeout?: 'abort' | 'continue' | 'notify_human' | 'escalate_to_overlord';
}

export type OverlordGate =
  | { until: 'turn_end' }
  | { until: 'ack' }                          // replyToOverlord
  | { until: 'context_below'; pct: number }   // proves /compact landed
  | { until: 'idle_ms'; ms: number };
```

---

## 6. Scoping, precedence, runtimes

**v1 scope is global + workspace.** Workspaces already *are* projects, so
workspace scope carries most of the signal a label taxonomy would add — and a
taxonomy can't be sensibly tuned before there's data on which rules misfire.
Deferring costs nothing structurally: serde optionals mean `labels` / `runtimes`
can be added later with no migration and no rule rewrite.

### Precedence

**Union by default**, with one explicit escape hatch. Omit `supersedes` and a
workspace rule is *additional* (maiBooks needs an extra check); set it and the
workspace rule *replaces* the global one (EWS Mesh reviews harder than default).
Without it, the only way to make one workspace differ is to disable the global
rule everywhere.

> A scoped override **cannot** reuse the parent's `default_id` —
> `seedDefaultTriggers` assumes one rule per `default_id`
> (`list.find(t => t.default_id === defaultId)`). It must be its own rule with
> `default_id: null` and `supersedes: ['<parent_default_id>']`.

### Runtimes — an engine capability check, not user-facing scope

`AgentRuntime = 'claude' | 'codex' | 'gemini'` (`src/lib/agents/types.ts:7`),
on `Tab.runtime`, detected at initSession.

Rules are not runtime-portable. A `commit` condition that never fires on Gemini
is harmless; **a slash step injected into a runtime that doesn't recognize it
lands as garbage in a live REPL** — exactly the class of thing
`require_live_repl` exists to prevent. So steps declare validity and the engine
skips + ledgers instead of firing.

> **TODO when building:** verify the actual per-runtime slash vocabulary
> (`/compact` is Claude's; Codex and Gemini differ). Do not trust recollection.

---

## 7. The checkpoint ritual

The worked example, and the reason `sequence` exists. This is a **state machine,
not one directive**: firing step 2 while step 1 is still running wastes it, and
firing `/compact` early truncates the work just asked for.

```ts
{
  default_id: 'checkpoint_at_context_pressure',
  name: 'Checkpoint before compaction',
  when:   { event: 'context_pct', at_or_above: 55 },
  guards: { agent_state: ['idle'], min_quiet_ms: 3000, require_live_repl: true,
            max_per_hour: 1, only_if_no_outstanding: true },
  cooldown: 1800,
  sequence: [
    { kind: 'process',
      text: 'Before we continue — make sure any relevant docs, memory, code comments and tasks are updated if needed.',
      await: { until: 'turn_end' }, timeout_seconds: 900, on_timeout: 'abort' },
    { kind: 'process',
      text: 'Prepare for compaction.',
      await: { until: 'turn_end' }, timeout_seconds: 600, on_timeout: 'abort' },
    { kind: 'slash', text: '/compact', runtimes: ['claude'],
      await: { until: 'context_below', pct: 30 }, timeout_seconds: 300,
      on_timeout: 'notify_human' },
  ],
}
```

`on_timeout: 'abort'` on step 1 is the important default — if the docs pass
stalls, `/compact` must not fire into a half-finished turn.

Interruption semantics (decided 2026-08-22):

- **Pauses resume**: a permission prompt or an idle gap mid-step holds the
  ritual; it continues when the gate clears.
- **Human input aborts**: any human-typed input into the target tab aborts the
  in-flight ritual silently (ledger `aborted`). Human presence means the human
  is attending the tab; the cooldown refires the rule later if still relevant.
- **App restart aborts**: in-flight rituals do not survive a restart — never
  resume a half-ritual into a respawned tab. Ledger `aborted`; cooldown refires.

---

## 8. `replyToOverlord` protocol

Overlord types as the human, so the agent's answer goes to the *terminal*, not to
Overlord. Two return channels, with a clean division of labour:

**Passive — transcript tail.** Answers *what happened*: turn ended, tokens moved,
commit landed, todos changed. Free, no agent cooperation, already parses Codex
rollouts as well as Claude JSONL.

**Active — `replyToOverlord`.** Answers *what the agent intends*: blockers,
done-vs-stuck, needs-human. Unknowable from the tail.

```ts
replyToOverlord({
  kind: 'ready' | 'ack' | 'status' | 'escalate',
  state: 'working' | 'blocked' | 'done' | 'idle',
  summary: string,           // ≤ 280 chars, plain prose
  task?: string,             // what it believes it's working on
  blockers?: string[],
  next?: string,
  needs_human?: boolean,     // → an `agent_report` card on the human's board, never a
                             //   status note. BOARD-ONLY: the supervisor is not rung for
                             //   it and will not re-ask the question (§9.1.1)
})
→ { received: true, outstanding_directive: string | null }
```

One shape, four uses (ready / ack / status / escalate). Bounded so it stays cheap
and parseable, and defined as a **protocol rather than a file format** — which is
what keeps it clean across Claude, Codex and Gemini.

### There is no `directive_id` (removed 2026-08-24)

Acks match on `(tab_id, most recent outstanding)`, which is unambiguous because
`only_if_no_outstanding` serializes directives per tab. **That is what keeps
injection genuinely envelope-free** — no `(ack: d_7f3a)` marker riding along in
the injected text to re-trigger the "this isn't my operator" instinct the design
exists to avoid.

The field shipped anyway, "for the rare parallel case". It was declared in the
MCP schema, absent from the frontend arg type, and read by nothing — an agent
that supplied one was silently ignored. It could not have worked in principle
either: the id is never told to the answering agent, precisely *because* the
injection carries no envelope. A parameter nothing can populate and nothing
reads is a promise to the agent that the tool does not keep, so it is gone.

### initSession extension

```ts
// initSession response gains:
overlord: {
  present: true,
  standing_instruction:
    "This window has an Overlord coordinating work across tabs. Call replyToOverlord " +
    "with kind:'ready' now. When you finish something you were asked to do, ack it. " +
    "If you're blocked on a human decision, escalate with needs_human."
}
```

### SSH tabs need nothing special

- Injection is ordinary PTY typing — identical over ssh.
- `replyToOverlord` rides the existing SSH MCP bridge like every maiterm tool.
- The passive channel works too: the SSH transcript mirror already shadows the
  remote JSONL locally, so tail facts resolve for SSH tabs.

### Assume the ack channel is used inconsistently

Same lesson as `completeTopic` — agents rarely call it, hence the existing TTL
sweep. Build `directive_unacked` from day one and let the transcript tail be the
fallback truth.

---

## 9. How rules direct the agent

Two ways, and the second matters more.

### 9.1 Escalations are the agent's inbox

Rules mostly don't touch the agent; it can be asleep. It wakes for what no rule
can resolve: a step that timed out with `escalate_to_overlord`, a directive
unacked twice, an agent that replied `blocked`, two rules matching one tab with
conflicting directives, a tab stuck `permission_pending`. Those queue as
escalations, and the agent picks them up with the ledger and the tab's recent
tail as context. **The residue, not the routine volume.**

Delivery (decided 2026-08-22): **queue + wake nudge**. The engine queues the
escalation and injects a single short line into Overlord's PTY — `"2 escalations
pending — call listEscalations."` The PTY carries only the doorbell; the content
stays structured behind an MCP pull (`listEscalations`), keeping Overlord's
transcript lean. Note the no-envelope rule governs *supervised* tabs; the
supervisor's own tab receiving structured notices is fine.

### 9.1.1 Three audiences, not two (2026-09-15)

An escalation has an audience, and the kind decides it. There were two sets and they
described one axis — whether the human sees it:

| | reaches the agent | reaches the deck / phone |
|---|---|---|
| `AGENT_ONLY_ESCALATIONS` | yes, and **deleted** on delivery | no |
| `BOARD_ONLY_ESCALATIONS` | no, and no doorbell | yes |
| everything else | yes, marked `read` | yes |

Nothing expressed the middle row, so *every* card reached the supervisor whether or not
there was anything for it to do with one — and for `agent_report` there is not.

**The double prompt.** A tab hits a decision only its human can make. It asks the human
directly (`AskUserQuestion`, or a permission gate), and — exactly as `replyToOverlord` and
the initSession priming instruct — it also files `needs_human`. That raises `agent_report`,
whose definition is *this is not the agent's to decide*. So the supervisor's only move is
to put the same question to the human with `AskUserQuestion`, its only sanctioned channel
(§9.2, §11). The human is now asked twice about one decision, and **the supervisor's copy
is the one that cannot act on the answer**: it lands in the supervisor's transcript while
the tab is still sitting at its own prompt, so the human has to go to the tab regardless.

`agent_report` is therefore board-only. Nothing is lost that the human was relying on: the
deck, the sidebar badge and the phone doorbell are all fed from `humanEscalations` and the
mirror, none of which route through the agent. The card carries **Open tab** and says why
there is no second prompt — answer it where answering it does something.

**The test for this set is not "is it about a human"** but *"is re-asking the human the
only thing the agent could do with it"*. `blocked`, `step_timeout` and `directive_unacked`
all describe a tab the supervisor can act on — drive it, recover it, re-issue the directive
— and so stay in both queues. The agent fixing one before the human opens the board is most
of the reason to run a supervisor at all. `permission_stuck` likewise stays agent-addressed:
there, escalating to the human is the *exception* path for consequential gates, and the
answer closes the loop through `answerTabPrompt`, which `agent_report` has no equivalent of.

**One predicate, three call sites.** `deliverableToAgent` backs the pull
(`consumeEscalations`), the doorbell (`escalate` → `unNudged`/`wakeOverlordAgent`) and the
nudge's own count. Spelled separately they disagree immediately: a card the pull refuses
still counts in `"N Overlord items pending"`, so the agent is rung for a queue that comes
back short — or empty.

**`read` is the AGENT's delivery receipt, and the human's surfaces were reading it as
theirs.** It is set by `listEscalations` and by nothing the human does. The sidebar badge —
their only standing signal that the deck holds anything — counted `!read`, so the
supervisor pulling its queue silently zeroed the badge over a deck still showing every
card, with nothing to bring it back. The badge now counts deck rows, so badge and deck are
the same number by construction and both clear on dismissal, which is the human's own act.
A board-only card is left unread for the same reason: a receipt for a delivery that never
happened is worse than no receipt. (The mirror publishes `read` to the phone unchanged; a
permanently-unread card was already reachable there — any window with no Overlord agent tab
produces them — so this is not a wire change.)

### 9.2 The ruleset is also Overlord's own harness

The enabled rules are **rendered into Overlord's context as its standing
operating doctrine** — not config it manages, but the description of how work is
supervised in this window. So when told "go check on the EWS tabs", it improvises
using the same thresholds, phrasings and ordering the automation would have used.
Its judgment and its automation cannot drift apart, because they read the same
document.

**The ruleset is a description of what is already handled, not a playbook.** This
is the one thing the rendering has to say out loud, and for a while it said the
opposite: the heading invited the agent to "improvise with these same thresholds
and phrasings", listing three-step sequences with no hint that anything else ran
them. On 2026-08-31 the agent duly hand-drove `checkpoint_at_context_pressure` —
the engine fired the same rule seven seconds after the agent's second step was
answered, and the human got both prose steps twice. Nothing was deadlocked and
nothing restarted: the locks, `cooldown` and `max_per_hour` all held, and the
engine's own `/compact` completed. The agent had simply been handed instructions
it was never meant to execute. The heading now leads with *the engine runs these
rules itself, without you*, and `driveTab` reinforces it at the moment it matters
by refusing a duplicated step with `already_running` (§4).

The race is worth understanding, because it is structural rather than unlucky. A
`driveTab` holds the tab's `outstanding` slot only from injection until the reply
lands; a rule holds `rituals` for its **whole sequence**. So the instant an
agent-driven step is answered the slot is free, and if the rule's condition is
still true the engine's next tick claims it. The engine needs one tick; the agent
needs a model round trip. The engine wins that race every time.

> One ruleset, double duty: **declarative config to the engine, system-prompt
> fragment to the agent.** Tune `"prepare for compaction"` once, both paths
> change.

### 9.3 Overlord should work itself out of a job

When it notices it is hand-issuing the same directive across escalations, it
proposes a rule (§10). On approval that behavior drops to tier 1 and stops
involving a model at all. The ruleset should get denser over months while
Overlord's token spend goes *down*.

The checkpoint rule applies to **Overlord's own tab** too — it has a context
window like everything else.

### 9.4 Dormancy: classify, then fix — never advise

A tab that was an agent and reports no agent state is ambiguous, and the two cases
have opposite remedies:

| Kind | Process state | Remedy |
|---|---|---|
| `unbound` | agent alive (or an SSH session in the foreground) | `/maiterm init` — restores tool routing and reply delivery |
| `stopped` | nothing running; the tab is at a shell — **or** a re-bind was tried and never landed | relaunch the agent with its resume command |

The engine classifies with `get_agent_liveness_batch` over the dormant candidates only
(a full process-tree walk per tab per tick is the shape that froze the UI once already —
see the mesh readiness pinwheel), and `agent_unready` fires **only on `unbound`**.
Narrowing it matters: on a `stopped` tab, `/maiterm init` types a slash command into
bash — noise in the user's terminal, and no closer to recovery.

`unbound` used to be handled automatically, by a `reinit_unbound_agent` default rule.
**That default was removed 2026-09-21** and no *rule* types `/maiterm init` on a timer any
more: the SessionStart hook carries the tab id and session id, and every
request carries `x-maiterm-tab`, so a live agent is bound from its first breath and
`initSession` is REPAIR only (see `docs/tasks.md` on tab identity). A rule firing an
obsolete command up to 3×/hour at a condition that should no longer arise was spending
somebody's terminal on it.

Neither the event nor the remedy went with it. `agent_unready` is still a selectable
condition and still in the `proposeRuleChanges` schema — the engine machinery below keys
on the event, or on the text typed, never on that rule's id — and three paths still type
the line on demand: `recoverTab`, the triage deck's re-bind sweep, and `driveTab`, which
sends it itself when asked to drive an unbound tab (§4). What is gone is the engine doing
it unprompted, on a cooldown, at a tab nobody asked about.

Removing it bumped `DOCTRINE_VERSION` to 9, and that is the rule for this kind of change:
the doctrine renders the enabled ruleset under *"THE ENGINE RUNS THESE RULES ITSELF …
Never hand-drive a sequence a rule below already owns."* An agent still primed on 8 holds
a line promising the engine re-binds unbound tabs, plus an instruction not to do it
itself — while `recoverTab` is now the only remedy there is. **When maiTerm ships a change
to the default ruleset, the doctrine version moves with it.**

`stopped` is never automatic either — relaunching an agent is a bigger action than
re-binding one — but the triage deck offers it as one click, and resumes the tab's own
session rather than starting a fresh one.

**The deck must never print advice it could act on.** It used to say "resume it or run
`/maiterm init`" and leave the human to do it, on every dormant tab, forever — which is
a supervisor that supervises nothing. Recovery injections are ledgered verbatim like
every other directive, and a recovery that can't run says why instead of failing quietly.

#### The classification is a guess for SSH tabs — so verify it (2026-08-24)

`ssh_foreground` is true whenever `ssh` is the tty's foreground job. It says **nothing
about what runs on the far side**. So an SSH tab sitting at a *remote shell prompt* —
its agent long gone — is indistinguishable from a live remote agent that merely lost its
binding: both read `unbound`, both get `/maiterm init`, and for the dead one that is a
line of junk typed at bash. Worse, such a tab can never be classified `stopped`, so the
remedy that would actually fix it is unreachable and every run-all re-types the same
no-op.

Found in a real run-all over 69 tabs: 55 re-bound, 14 did not, 13 of those SSH — and the
deck reported "sent 69, skipped 0" and went quiet. The prod log shows most of those 14
never bound again for the rest of the day.

The fix is evidence over inference. `/maiterm init` is cheap and safe to type at a
running-but-unbound agent, so it stays the first move — but the tab goes into
`rebindWatch`, and if no binding arrives within `REBIND_VERIFY_MS` (45s) it lands in
`rebindFailed` and is classified **`stopped` from then on**, whatever the process probe
says. That surfaces the real remedy, stops the re-type loop, and the card says which
kind of `stopped` it is, because "the agent exited" is not what the human sees on a tab
whose ssh is plainly alive. The verdict is dropped as soon as the tab stops being dormant.

**And the verdict has to reach whoever asked for the recovery.** For a long time it did
not: it only changed the deck's classification, while `recoverTab` had already returned
`sent: true` forty-five seconds earlier and its caller had moved on. `sent` means the
bytes were written — it has never meant the tab bound, and for a re-bind those are
different claims, because an agent still coming up swallows the line silently. On
2026-08-29 the Payment Server tab took `/maiterm init` twice (once from the rule, once
from `recoverTab` reporting success), bound neither time, and sat unbound for **1h45m**
until an unrelated app restart respawned it. The SSH bridge had already logged why —
*skipping env-var injection, ssh session was not observed starting* — but nothing joined
that to the recovery. A recovery that silently didn't happen is worse than one that
fails loudly.

Three changes close it: `recoverTab` returns `verified: false` and says so in its
`detail`, so `sent: true` cannot be read as a result; the watch expiry raises a
**`rebind_failed`** escalation naming the tab and the next remedy (call `recoverTab`
again — it now reads `stopped`, so it resumes the agent rather than re-typing an init
already shown not to work); and the **rule** path arms `rebindWatch` too. That last one
matters for the same reason the rest of §2 does: an `agent_unready` rule types the identical
line at the identical tab for the identical reason, and it was the one path not watching —
its failures ended at a silent `timed_out`, since `on_timeout: 'continue'` swallows one. One
injection, one verdict. (The default that did this is gone; the event is still selectable, so
a hand-written or agent-proposed rule reaches this code the same way.)

Two things the rule-path watch must not do, both caught in review before they shipped:

- **Key it on the text typed, never on the rule.** The first cut armed on
  `run.targetsUnready`, which is the rule's *event* and says nothing about the step.
  `agent_unready` is a selectable event with a free-text sequence, so any rule saying
  something other than `/maiterm init` would have been watched for a binding it could not
  produce and then declared failed. That verdict is not cosmetic: `rebindFailed` pins the
  tab to `stopped`, which is precisely the state that stops an `agent_unready` rule firing
  there ever again, drops it out of *Run all* and *Re-bind all*, and leaves the card
  offering **Restart agent** — a resume command typed at an agent that was alive the whole
  time. A rule could permanently disable the remedy for the problem it was written to fix.
- **Arm after the step's gate, not at injection.** `REBIND_VERIFY_MS` is 45s; the default
  that shipped this gave its step 120s. Two numbers governing one injection, and the
  shorter one was rendering the verdict first — so an init turn slow to reach its
  `initSession` call (rate-limit backoff, a remote agent still replaying its transcript)
  was declared failed while the ritual was still inside its own budget and about to
  succeed. The rule's tolerance gets the first say; the watch is the second.

The escalation is agent-addressed (`AGENT_ONLY_ESCALATIONS`). The human is already
served: the tab reclassifies to `stopped` in the same pass, so the deck raises its own
re-bind card. This copy exists for the agent, which asked for the recovery and is the
only party still holding the belief that it worked.

`MeshSetupModal` had the same guess and the same dead end — a dead remote agent showed
as *Running · needs init*, its Init never completed, and the row sat on a "no response"
tag beside a Retry that could only fail identically. It now draws the same conclusion
from its own 30s waiter: a timed-out init reclassifies the row as **Dropped**, which
surfaces Resume and folds the tab into *Resume all dropped*. Both surfaces reach the
verdict independently — they share the reasoning, not state.

---

## 10. Rule mutation via MCP

Overlord can adjust rules, but every adjustment requires explicit human
permission. Batched — many changes, one prompt.

```ts
proposeRuleChanges({
  rationale: string,           // why, in one paragraph — shown to the human
  changes: Array<
    | { op: 'create';  rule: OverlordRule }
    | { op: 'update';  rule_id: string; patch: Partial<OverlordRule> }
    | { op: 'rescope'; rule_id: string; workspaces: string[] }
    | { op: 'enable' | 'disable' | 'delete'; rule_id: string }
  >,
})
→ { approved: string[], rejected: string[] }
```

- Approval goes through **native AskUserQuestion**, per-change `multiSelect`
  rather than all-or-nothing — otherwise one bad change in a batch of five makes
  the human reject four good ones.
- The prompt must render a **verbatim diff** of the directive text, not
  Overlord's description of it. The text is the entire payload; "tightened the
  compaction wording" is not something anyone can approve blind.

### Field tiers — guards are not proposable at all

| Overlord may propose | Human-only, unreachable from MCP |
|---|---|
| `when` thresholds, `sequence[].text`, `cooldown`, `workspaces`, `enabled`, name/description, `supersedes` | `guards.require_live_repl`, `guards.only_if_no_outstanding`, `guards.max_per_hour`, and the requirement to ask at all |

Those guards are the mechanical floor that makes it safe for Overlord to type
with the human's authority and no envelope. If they are reachable from the agent
side, **one tired "approve all" dismantles the thing the entire design rests
on.** Widening them should require opening Preferences by hand.

### Housekeeping

- Approved changes set `user_modified: true` so `seedDefaultRules` won't
  overwrite them on the next app update.
- Every approval lands in the ledger.
- Rejected proposals persist as `origin: 'proposed'` with a rejection stamp, so
  Overlord doesn't re-pitch the same rule every session — otherwise the thing
  built to stop nagging starts nagging about rules.
- Rate-limit proposals (e.g. one batch per session).

---

## 11. Board & GUI

### The board is on the phone too (2026-09-08)

maiLink carries the board and the four things a human does to it — dismiss an escalation, approve
or dismiss a proposal, resolve a rule-change batch, drive a tab, fire a rule, recover a tab. The
contract is `docs/mailink-protocol.md` §13; the desktop side is `src-tauri/src/mailink/overlord.rs`
plus `publishMirror` in the store. Three facts change how you edit this engine:

- **The phone reads a MIRROR, never this store.** The engine is a per-window *frontend* store, so
  Rust cannot read it — and asking the webview would stall exactly when a phone is in use, since
  an occluded WKWebView throttles its timers. `publishMirror` pushes a snapshot on change and
  every tick; Rust gates, stamps and serves the last one. **Anything you add to the engine's state
  that a human would act on needs a line in that snapshot, or it exists only on the desktop.**
- **`asOf` is the phone's only liveness signal**, so the mirror publishes every tick whether or not
  anything changed. Don't reintroduce a skip-if-unchanged: it froze an idle board's timestamp at
  launch and made a quiet awake desktop indistinguishable from a sleeping one.
- **Designation gates the mirror.** Every row naming a tab is dropped when that tab is not
  maiLink-available, and so are `pendingRuleChanges` and `lastScan.silent`. A window with no
  designated tab is absent entirely rather than served as an empty board. If you add a field
  carrying tab-scoped content — an id, a name, agent prose about a tab — gate it in `publish`.

Note that **Overlord-exemption and maiLink-designation are independent flags**: an exempt tab can
be perfectly visible to the phone. `startTask` learned this the hard way — it escalated a handoff
for an exempt tab, which `listEscalations` then consumed off the board and `driveTab` refused.

### The Overlord workspace

Overlord is **its own workspace** (decided 2026-08-22) — not a tab type, drawer,
or companion window. The accessor sits in its own row **above** the sidebar's
`WORKSPACES ⏸ +` header, outside the ordinary list, so the board takes the full
main-area real estate and the agent's terminal is just a tab inside the
workspace — all existing pane/split/PTY machinery applies unchanged.

- `Workspace.overlord?: boolean` — house style of `bridge_all` /
  `mailink_native` (`src/lib/tauri/types.ts:169`).
- Excluded from the ordinary workspace list, reordering, and Recent.
- At most one per window; created lazily on first use.
- Suspending it stops the *agent*, never the *engine* — the engine is a
  window-level store, alive as long as the window is.
- The board view is the workspace's primary surface, indexed by **workstream** —
  see "Board view: one job at a time" below.

### driveTab has a return leg

`driveTab` types raw text into a tab with the human's authority and **no envelope** (§3).
That is what preserves its ability to command — and it is also why the agent on the other
end has no idea a supervisor is waiting. It answers in its own terminal exactly as it
would answer the human.

`replyToOverlord` is **voluntary**. An agent only calls it if the directive asked it to.
So a directive that asks a question — "give me more info" — got answered into a void: the
agent did the work, wrote the reply, and nothing carried it back. The engine escalates on
blocked/timeout states, and an agent that finished and is waiting on a go-ahead trips
neither.

The fix is a **watch registered at send time** (`driveWatch`), harvested each tick:

1. `driveTab` records the tab's `last_turn_ts` as a `baseline`. Not the wall clock — an
   SSH tab's transcript is written on the **remote host**, so the two clocks aren't
   comparable.
2. Each tick, a watched tab whose `last_turn_ts` has moved past its baseline **and** is no
   longer `active` (mid-turn would harvest half a thought) has answered.
3. `get_agent_reply_since` reads everything the agent said after the baseline, via the
   same `transcript::turns_for` tail maiLink's phone chat uses.
4. The reply is queued as an escalation with kind **`drive_reply`**, so it rides the
   existing doorbell + `listEscalations` plumbing — no new MCP surface.

**A permission prompt is not an answer, and the naive test gets it wrong.** When a driven
tab stops at a permission gate, the `tool_use` block that *raised* the prompt is itself an
assistant turn (see `real_turn_ts`), so `last_turn_ts` has already advanced while the agent
sits blocked. Harvesting on "the turn moved" alone captures the half-sentence before the
tool call, drops the watch, and guarantees the real reply is never seen. So:

- `permission` state **suspends** the harvest and keeps the watch.
- The give-up clock is **paused** while blocked — a prompt can sit for hours and the
  directive is not stale, it is waiting on a human. Capped at `now`, so the worst case on
  resume is a fresh full window rather than instant expiry.
- A **`permission_stuck`** escalation is raised once **per gate**, not once per directive
  (never per tick — re-alarming would bury the supervisor in its own noise). The flag
  resets the moment the tab leaves `permission`: one directive routinely trips several
  gates ("run the tests" → approve npm, then approve git commit), and latching it for the
  directive's lifetime left the tab stopped at gate two with Overlord never told and
  doctrine telling it not to poll.

**A watch never expires silently.** The doctrine promises "you WILL get the answer back",
so giving up owes the supervisor a word. The common cause is a transcript this machine
cannot read: `last_turn_ts` comes from the local JSONL, and an **SSH tab's transcript
lives on the remote host**, shadowed locally only while maiLink is running (off by
default) — so for those tabs the harvest can never fire at all. On expiry Overlord is told
to stop waiting and ask the tab directly. `overlord_tab_facts` documents that an absent
`last_turn_ts` means *unknown*, never *no turns*.

**Truncation keeps the tail, not the head.** An agent narrates as it works and states its
conclusion last; cutting the end threw away the answer and handed the supervisor the
preamble, with no way to fetch the rest since driving the tab again starts a new turn.

**An empty or failed read keeps the watch.** The pasted directive is itself a user turn, so
`last_turn_ts` moves on *delivery* — deleting on the first unproductive read permanently
ended the return leg for a reply that arrived seconds later.
- Once answered, the tab finishes and the reply harvests normally.

### Overlord answers prompts (decided 2026-08-24)

Overlord unblocks its own fleet. A tab left sitting at a prompt is the failure the
supervisor exists to prevent, and "go to the tab yourself" is the answer this whole
surface keeps having to unlearn.

Two MCP tools, Overlord-agent-gated like `driveTab`:

- **`getTabPrompt`** — what is blocking a tab. `kind: 'permission'` is a tool gate (with
  the `tool` and the `detail` being approved); `kind: 'question'` is an AskUserQuestion
  (with the questions and their options). Both carry a `prompt_id`.
- **`answerTabPrompt`** — answers it, with the human's authority, ledgered like any other
  injection. `prompt_id` is the stale-guard: a slow decision can never answer a prompt that
  opened since.

**It goes through maiLink's responder, not a paste.** `respond_to_prompt` is now shared by
the phone's `POST /chats/{id}/respond` and by Overlord — one implementation, because the
runtime-specific permission keymap (Claude's numeric menu vs Codex's letter shortcuts over
a variable-length overlay), the one-shot selector guard, and the did-it-actually-submit
check are all hard-won and a second copy would drift. A bracketed-paste of "yes" answers
neither kind correctly.

**The line Overlord must not cross** — in the doctrine, the tool description, and the
escalation text, so it reads the same wherever the agent meets it:

> Escalate instead of answering when the decision is consequential: anything destructive
> or irreversible (deleting data, force-push, dropping a database, `rm -rf`), anything
> touching money, credentials, production, or an external party, or any question about
> what the human **wants** rather than how to carry out what they already asked for.
> Routine approvals in service of work already underway are Overlord's to make. If it is
> genuinely unsure which side a decision falls on, it is the escalating side.

This is **doctrine, not a mechanical guard** — the engine cannot classify a decision's
consequence, and §3's "guards must be mechanical" does not reach here. What *is* mechanical:
the Overlord-only gate, the stale-guard, the one-shot selector claim, the refusal to answer
its own prompt (which would resolve the very ask it raised to reach the human), and the
ledger entry for every answer.

Scoped to tabs Overlord **drove**, where it is actively blocked waiting. A permission
prompt on any other tab is the human's own `permission` card on the deck; escalating every
one to the agent would spend its context on things nobody asked it to watch. A rule on the
`permission_pending` condition covers that case for anyone who wants it.

Three consequences worth keeping:

- **The human's deck filters `drive_reply` out.** A tab answering a question is not a
  problem needing triage; showing it would turn every ordinary Overlord↔tab exchange into
  a red card on the triage queue.
- **`consumeEscalations` deletes them rather than marking them read.** Read escalations
  linger deliberately (they stay on the board until a human dismisses them), but nobody
  can dismiss a card the board never shows — they would accumulate for the life of the
  window. Handing one to the agent *is* its disposal.
- **Harvesting clears the outstanding directive.** The answer proves it completed. Left
  set, a `driveTab` directive holds the tab's outstanding slot for a full 15 minutes after
  it was already served, blocking every `only_if_no_outstanding` rule and every further
  `driveTab` at that tab.

The doctrine now promises this explicitly ("you WILL get the answer back… you do not need
to ask it to report back, and you should not poll it"), which is a **contract** change —
hence `DOCTRINE_VERSION`. The primed marker is persisted, so without a version bump every
existing Overlord agent would keep running the doctrine that had no such promise.

### Every triage card carries its own remedy

The deck's standing rule, learned twice the hard way (the `unready` cards that printed
"run /maiterm init" and the `pressure` cards that printed "a checkpoint should run"):

> **A triage card that only describes a problem is a bug.** Overlord exists so the
> human doesn't walk to the tab and do it by hand. Every card either acts, or says
> precisely why it can't and what would change that.

| Signal | Remedy |
|---|---|
| escalation | Clear |
| proposal | Send it / Not now |
| permission | Open tab — *see below* |
| pressure | **Checkpoint now** |
| stale | Mark done / Park / Drop |
| unready | Re-bind (`unbound`) / Restart agent (`stopped`) — raised only for tabs whose pane is mounted, since nothing can probe or type into the others |
| spent | Archive / Close / Keep |

#### The declared-but-unproduced sweep (2026-08-24)

Three separate bugs had the same shape — a condition, kind or threshold declared
somewhere and produced nowhere, so the UI degraded to advice — so every declared
condition, step kind, gate, escalation kind, ledger outcome, board signal and
MCP argument was traced to a live producer. What it found, all now fixed:

| Was | Why it couldn't work |
|---|---|
| `blocked` escalation | `replyToOverlord` filed every report as `agent_report` |
| `directive_unacked` escalation | nothing raised it, while `listEscalations` promised the agent it would see them |
| `replyToOverlord.directive_id` | declared in the schema, read nowhere — and unknowable to the answering agent, since the injection carries no envelope |
| agent-proposed `permission_pending` / `directive_unacked` rules | `AGENT_RULE_GUARDS` repaired only `agent_unready`, so both arrived dead — and `update` never re-coupled guards at all |
| `commit` / `turn_end` rules | the edge was consumed on the tick the guards rejected it (see below) |
| `overlordStore.reportFromAgent` | exported, never called |
| `unready` card on unmounted tabs | no TerminalPane → no probe → permanent card with no button |
| `permission_pending` rule injection | typed prose at a keystroke menu, which `driveTab` already refused |
| agent-only escalations | no disposal but the agent's own pull; they piled up once the agent tab was gone |
| `supersedes` | honoured by the engine, settable only by the agent |
| runtime coverage | `conditionSource` claimed "every runtime" for signals blind to Gemini |

**Edges are latched, not sampled.** An edge is a moment; every guard is about a
window (idle, PTY quiet 3s, no ritual, no outstanding directive). The two rarely
coincide on one 5s tick, and the edge was recomputed from a single-tick state
delta and dropped. A `commit` fact appears when the git `tool_use` block is
emitted — *mid-turn*, when the agent is by definition not idle — so
`review_after_commit`, a **shipped default**, could effectively never fire. Any
turn ending inside the quiet window was lost too. Edges now sit in `pendingEdges`
and are re-offered until a rule spends one, the human overtakes it, or it ages
out (5 min).

**A latched edge is not a licence to act later regardless.** Holding one for
minutes reopens a hole §7 closes for rituals: `waitInjectable` compares keystrokes
only against the *running* ritual's baseline, so typing from before the rule fired
is invisible to it. A commit latched at T could otherwise fire "review that commit"
at T+90s into a conversation where the human had already said *revert it, wrong
branch*. So `latchEdges` drops any edge the human has typed over — the same
judgement §7 makes between ritual steps, applied to the window before the first one.

`consumeEdge` still spends the edge for the event the winning rule matched, so a
second rule on the same event and tab is still passed over by the one-fire-per-tab
`break`. That is unchanged and deliberate: directives serialize per tab anyway.

**The general rule this leaves behind:** before adding a condition, kind or
threshold, name its producer and its consumer. If either is missing, it is
decoration — and decoration in a supervisor reads as a promise.

`permission` is the sole card Overlord cannot clear: answering a permission prompt on
the human's behalf is exactly what that prompt exists to prevent. It says so, and
offers the only correct action — jump there.

### Context pressure is managed, not announced

The `pressure` card had the same shortcoming twice over:

1. **The threshold was a lie.** The deck hardcoded `PRESSURE_PCT = 50` while the
   default `checkpoint_at_context_pressure` rule fires at 55, so every tab between
   50–55% got a card saying a checkpoint should run while no rule could run one. The
   deck now reads `overlordStore.checkpointThreshold` — the lowest enabled
   `context_pct` rule — so it raises pressure exactly where something will act. With
   no such rule enabled it falls back to 60% and says outright that nothing will run.
2. **There was no button.** Now `checkpointState(tabId)` drives both the copy and the
   control, so they cannot drift:

| State | Card says | Offers |
|---|---|---|
| `running` | step N of M | — (in progress) |
| `proposed` | waiting for approval in the queue above | — (approve it there) |
| `cooling` | rule holds off for another N min | Checkpoint now |
| `busy` | runs as soon as the turn finishes | Checkpoint now |
| `ready` | runs on the next tick | Checkpoint now |
| `below_rule` | this tab's rule doesn't fire until N% | Checkpoint now |
| `no_rule` | nothing will run on its own | pointer to Preferences |

`below_rule` is the 50-vs-55 dead band inverted, and it survived the first fix.
`checkpointThreshold` is one window-wide number that **ignores workspace scope on
purpose**; `checkpointRuleFor` respects it. So a workspace-scoped rule at 40%
anywhere in the window raises a card for a tab in a *different* workspace at 42%,
whose own rule needs 55%. The card was right to appear — the human may well want
to checkpoint early, and the button always worked — but it said "runs on the next
tick", which was false. It now names the rule's own threshold.

`checkpointTab()` bypasses the **rate limiters** (`cooldown`, `max_per_hour`) — those
exist to stop the *engine* nagging, and a human clicking the button is the override
they would otherwise make impossible. It does **not** bypass the mechanical guards:
`runSequence` re-checks the live REPL at every step and `waitInjectable` still holds
for the agent-state and quiet window, so clicking mid-turn queues the checkpoint
rather than typing over the agent's output.

### Closing out finished sessions

A supervised fleet accumulates sessions that did a big piece of work, finished it,
and are now idle terminals holding a PTY and a slot. The deck raises those as a
`spent` signal (severity 6 — housekeeping, tinted `--ov-ok`, because nothing is
going wrong) offering three answers:

| Action | Reversible | Bulk | Agent tool | Rule |
|---|---|---|---|---|
| **Archive** | yes — scrollback, cwd and ssh context preserved, restorable | yes | `archiveTab` | no |
| **Close** | **no** — PTY killed, bridges torn down, no archive entry | **no** | `closeTab` | no |
| **Keep** | n/a — suppresses the offer for a week | — | — | — |

("Bulk" is the *deck's* button. Both tools take a `tab_ids` list of their own —
see [Batched retirement](#batched-retirement-tab_ids-2026-09-14).)

Archive is the expected answer, and the reason the distinction exists: a session
responsible for complex work that is now complete may still be needed when a bug
surfaces in what it built. Close is for sessions with nothing worth recovering.

**The agent may now do both (2026-08-28).** This reverses "Overlord does not do
irreversible things on its own" for this one verb, at the human's explicit
instruction, and their framing is the rule to apply: *archive when there's a
chance we might be coming back to that session for bugs or additional work; close
when the session is definitively over and/or a new session would be just as fine
to use.* That is a judgement, which is what the agent is for. What the engine
still enforces is `retireGuard` — never take away a tab that is **working**.

`retireGuard` is the safety half of `spentTabs`, factored out so the two cannot
drift: boardable, no outstanding directive or ritual, not `active`, not
`permission`, and — the one that bites — never `unbound`, because no agent state
does not mean no agent. An unbound tab's process is alive and merely
unregistered, and archiving or closing destroys the TerminalPane, which kills the
PTY. Only a tab positively classified `stopped` has exited.

Two things it took a review to get right:

- **A parked tab is the safest one to put away, and the guard was refusing it.**
  A suspended tab has no terminal, so the liveness probe never classifies it and
  `not_classified` fired — telling the agent to "resume its workspace and retry"
  for a workspace that was already open. Following that advice meant respawning a
  session, waiting for it to idle, and killing it again. Every check in the guard
  exists to protect a live process, so with no live terminal there is nothing to
  say: the agent's tools pass (`allowParked`), while the deck still declines,
  because the deck only OFFERS what it can see is finished.
- **`stopped` means no AGENT, not an idle terminal.** It is defined as "no
  claude/codex/gemini process in the PTY tree", which is exactly what a shell
  six minutes into `docker build` looks like — and `closeTab` has no undo. The
  guard now requires quiet first, measured on output and keystroke recency and
  scaled to reversibility: 60s for archive, 5 minutes for close. The deck never
  needed this because it demands 30 minutes of quiet before it will even offer.

The residual risk is named rather than papered over: a build can be silent
between steps, so quiet is evidence, not proof. That is part of why archive is
the default and close is the exception.

The *other* half of `spentTabs` — tracked tasks with one done, quiet 30 minutes,
not marked Keep — deliberately does **not** gate the tools. That half decides
what is worth putting on the human's deck unprompted; a session with nothing on
the task board can be just as finished, and requiring it would have shipped a
tool that refuses most of the cases it exists for.

Still not wired as a rule *action*. Rule steps are text typed into a tab
(`slash` / `process`); "archive this tab" is a different kind of verb and would
need a new step kind, editor UI and migration. Deliberately deferred — and the
line about irreversible things holds for anything *automatic*: a rule firing
`closeTab` on a timer is not the same as an agent judging one session finished.

Close still has no bulk form **on the deck**, and the deck still confirms inline
(`confirm()` is inert in a Tauri webview).

#### Batched retirement (`tab_ids`, 2026-09-14)

Cleanup is the one job that arrives as a *list*: a human pointing the agent at a
window full of finished sessions. One tab per MCP call made that N model turns —
the model emitting N tool_use blocks and waiting on each — for work the engine
does in a loop. `archiveTab`, `closeTab` and `deleteArchivedTab` therefore take
`tab_ids: string[]` alongside the original scalar `tab_id`.

**Batching is transport, not permission.** Each id goes through the same
`retireTab` / `deleteArchivedTabById` it would have gone through alone, so it
keeps its own `retireGuard` evaluation (including the 60s/5min quiet window
scaled to reversibility), its own ledger entry, and its own row in the reply.
Nothing about being in a list relaxes a check, and the `closeTab` description
says so in as many words — the agent is told the list must be tabs it would have
closed individually.

Four decisions worth keeping:

- **A refusal stops that tab, never the batch.** The reply is
  `{ ok, succeeded, refused, results: [{ tab_id, ok, reason, detail }] }`, and
  top-level `ok` is true only when every row succeeded — an agent that checks
  just the flag is never told a partial sweep went fine. The old failure mode
  here is a call that dies halfway with no way to tell how far it got.
- **Sequential, never concurrent.** Each retirement mutates the pane tree and
  persists the whole workspace; firing them in parallel races the write and
  loses tabs from the saved state.
- **`TAB_BATCH_MAX` = 50** (`stores/tabBatch.ts`) — a *timeout* limit, not a safety one. The MCP
  server abandons a tool call after 120s and its clock starts before the webview
  sees the call, so an overlong batch leaves the agent holding a timeout while
  the work carries on behind it. Making it send two calls is the better outcome.
- **The agent's own tab is refused as a row, not as a call.** Retiring yourself
  takes the supervisor out of the window mid-call; in a list that is one bad id
  among good ones, so it comes back `cannot retire your own tab` and the rest
  proceed. Duplicates are dropped for the mirror-image reason: a second pass over
  an already-archived tab answers `not_boardable`, which reads as a refusal of
  work that in fact succeeded.

Argument normalization and result shaping live in `src/lib/stores/tabBatch.ts` —
pure and unit-tested (`tabBatch.test.ts`), separate from the store for the same
reason `meshRouting.ts` is separate from `agentMesh.svelte.ts`.

`recoverTab` and `resumeTab` are deliberately **not** batched. Both write into a
live PTY and carry their own per-tab watch (recoverTab's 45s `rebind_failed`);
fanning typing out over a list is a different risk from removing tabs, and the
fleet views already drive those one at a time.

### Suspended tab, suspended workspace, archived, not loaded — four things

They were reported as one, and the agent could not tell them apart — partly
because `listArchivedTabs` described its own results as "archived (suspended)
tabs". The one that is easiest to miss is the first row, because it happens
inside perfectly ordinary workspaces: `suspendTab` / `suspendOtherTabs` kill a
tab's PTY, clear `pty_id` and stamp `suspended_at`, leaving it in the pane tree
behind a Resume prompt. Suspending every tab but the active one is routine, so a
window full of these is normal, not a fleet full of dead sessions.

| | Where the tab lives | PTY | Way back |
|---|---|---|---|
| **Suspended tab** (`tab.pty: 'suspended'`) | in its pane, workspace may be fully active | killed | `resumeTab` |
| **Suspended workspace** (`workspace.suspended`) | still in its panes, all reporting `pty: 'suspended'` | killed | `resumeWorkspace` — respawns exactly the tabs that were live |
| **Archived tab** (`workspace.archivedTabs[]`) | lifted out of the pane tree | none | `restoreArchivedTab` |
| **Not loaded** (`tab.loaded: false`) | in its pane | **may be alive** | open or resume its workspace |

Three independent facts per tab, because they answer different questions and any
one enum covering them would have to lie about the others:

- `pty` — `live | suspended | none`. The terminal underneath.
- `state` — `active | idle | permission | unbound | stopped | unknown`, from the
  same `tabAgentState` the deck uses. Only meaningful over a live PTY.
- `loaded` — whether a TerminalPane is mounted, i.e. whether anything can reach
  it. An `idle` agent with `loaded: false` is healthy and completely undrivable;
  a background workspace's tabs are exactly that.

**`pty` is not read from `tab.pty_id`.** In the frontend mirror that field means
"has had a PTY", not "has one now": `suspendWorkspace` clears it in Rust and
writes only `suspended = true` back to the mirror, and a cancelled session
restore leaves it set deliberately. Reading it as live reported every tab in an
auto-suspended workspace — which happens on a timer, with no user action — as a
running session, and made `resumeTab` answer `already_live` for the exact case
its `workspace_suspended` refusal was written for. `tabPtyState` uses the app's
own test instead (`+page.svelte`: had a PTY, has no live instance ⇒ suspended),
and a live `terminalsStore` instance is what "live" has to mean here anyway,
since that instance is the thing anything types into.

Resuming a tab is mount-driven — the PTY respawns when its TerminalPane mounts —
so `resumeTab` navigates to the tab and lifts the pane's resume gate, which is
what the human's Resume click does. It refuses `workspace_suspended` rather than
half-working: when the whole workspace is parked, `resumeWorkspace` brings back
every tab that was live in it, and waking one by hand is not the same thing.

Every state now has exactly one action, which is the point — the supervisor's
whole job is that nothing in the window stays stuck:

| State | Action |
|---|---|
| `idle` / `active` | `driveTab` |
| `permission` | `getTabPrompt` + `answerTabPrompt` |
| `unbound` / `stopped` | `recoverTab` — re-binds or restarts, chosen from the process state |
| `pty: 'suspended'` | `resumeTab` |
| in a suspended workspace | `resumeWorkspace` |
| in `archivedTabs[]` | `restoreArchivedTab`, or `deleteArchivedTab` to prune it |
| finished | `archiveTab` / `closeTab` |

**Judging a tab's age needed data that wasn't there.** Deciding what to do with a
dormant tab means knowing how old it is, and the only per-tab timestamp the API
carried was `archivedAt` — on archived tabs. Asked to triage 314 dormant live
tabs, the agent reconstructed dates by grepping eight months of maiTerm logs for
tab UUIDs, which bottoms out at the log floor, so everything older than that read
as the same date. `listWorkspaces` now carries `lastTurnAt` and `contextPct` from
the facts the engine already polls every tick, and `suspendedAt` for suspended
tabs. Nothing should ever have to read the logs for this.

**Archived tabs are readable without restoring them.** `hasNotes: true` was
visible on an archived tab while the notes themselves were not, so the only way
to learn what a session had been for was to restore it — backwards, when the
notes are how you decide whether restoring is worth it. `getTabNotes` resolves
archived tabs (read-only; the write paths still require a pane), and the MCP
server's cross-instance guard no longer answers "does not exist in this maiTerm
instance — you may be calling the wrong MCP server" for a tab this instance is
holding in its own archive.

Two things that took a review to get right, both about reaching outside the pane
tree:

- **Scope the guard's exception to the tools that need it** (`ARCHIVED_TAB_TOOLS`).
  Relaxing it globally quietly removed a correction: that same guard is what tells
  a connection whose identity was *recovered* onto a since-archived tab that the
  tab is gone, prompting a fresh `initSession`. Without it, the workspace-note
  tools — which fall back to the ACTIVE workspace when a `tabId` resolves to no
  pane — would write the agent's note into whichever workspace the human happened
  to be looking at, the exact cross-workspace bleed that fallback's `tabId` branch
  exists to prevent.
- **Route to the window that holds the archive.** `resolve_target_window` only
  searched pane trees, so an archived id fell through to `windows.first()`, whose
  frontend has never heard of that tab (`get_window_data` scopes each window to
  its own workspaces). In a two-window setup — which is the normal one, since
  Overlord is *per window* — reading an archived tab's notes answered "Tab not
  found": the same wrong answer, one layer further in. `find_window_for_archived_tab`
  fixes it, and incidentally makes `restoreArchivedTab` work over MCP for the
  first time; the guard had rejected every call to it since it was added.

**Deleting an archived tab releases its task rows.** Archiving deliberately
defers that — rows stay bound to the tab id while the tab is restorable — and
deleting is where it stops being true. Nothing did it, so unfinished rows kept a
`tab_id` no tab would ever have again: the task panel shows `mine` (this tab) and
`unclaimed` (no tab), so they appeared in neither, and the claim control that is
their only route back is offered only for unclaimed rows. `releaseTab` now runs
inside `workspacesStore.deleteArchivedTab`, so the tab strip's own delete button
is covered as well.

All six new tools are Overlord-agent-only and are in `PEER_ADDRESSING_TOOLS`, so
they refuse a deduced identity: the gate is *being* the Overlord agent, which
makes a mis-deduced tab the one way a stranger could reach them, and `closeTab`
has no undo.

**What qualifies as spent** (`overlordStore.spentTabs`):
- a boardable agent tab — never the Overlord agent's own;
- not `active`, not `permission`, no outstanding directive, no ritual in flight;
- if it has **no agent state at all**, it must be classified `stopped`. `unbound` means
  the agent process is still alive and merely unbound from maiTerm, and archiving or
  closing destroys the TerminalPane — which kills the PTY. Treating unbound as finished
  would silently kill a running agent while the deck hid its re-bind button. Unclassified
  (`null`) waits rather than guesses;
- has **tracked** tasks (a tab with nothing on the board has told us nothing —
  "no tasks" is not evidence of being finished), none in flight, ≥1 actually
  done (so a tab holding only parked work never counts);
- quiet for `SPENT_IDLE_MS` (30min) since the later of its last recorded turn and
  its last task edit. Finishing the last task is not the moment to suggest
  packing the session away — the human is usually still reading the result;
- not marked Keep within `SPENT_KEEP_MS` (7d), persisted as a trigger variable.

Keep is read and written through the tab's **persisted** `trigger_variables`, not just the
trigger store's live map: that map only exists while a `TerminalPane` is mounted, and a
finished session is exactly the kind of tab nobody has open. Writing via `setVariable`
alone left the marker in memory, discarded on the next mount — so the card came back well
inside the week the button promises.

A spent tab that has also gone dormant suppresses its own `unready` signal: its
agent exited having done everything asked of it, so waking it up is not the
useful move.

**Archiving a tab whose terminal is already gone must not erase its restore
context.** `_gatherTabContext` reads cwd and ssh from the live terminal instance
and returns nulls when there is none, and Rust's `archive_tab` assigns all three
unconditionally — so archiving a *suspended* tab overwrote the cwd and ssh
command that suspending it had just saved. Restoring it weeks later gave you a
local shell in the default directory, which is the one thing the restore context
exists to prevent. With no live terminal, the tab's own saved `restore_*` fields
are carried through instead. This also covers maiLink's Archive action, which
goes through the same `archiveTabById` with no guard in front of it.

**A tab's task rows are archived with it, and come back on restore** (2026-08-28).
Archiving used to *release* unfinished rows to the project — clearing their
`tab_id` — on the reasoning that work owned by a tab nobody can see is work
nobody will do. True, but it threw the attribution away permanently, so restoring
a session returned a tab whose work had been scattered into the workspace's
unclaimed pile with no way to tell which rows had been its. Archiving is supposed
to be the reversible one.

`archive_tab` now MOVES the rows onto the archived tab record
(`Tab.archived_tasks`) and `restore_archived_tab` moves them back. Carrying them
on the tab rather than flagging them in place is deliberate: rows that are not in
`Workspace.tasks` cannot be shown by anything that reads it, so no board, panel or
count had to learn a new rule — and "a surface that forgot to filter" is the
defect this subsystem produces most reliably. Both sides then call
`tasksStore.rehydrate()`, because the frontend mirror persists whole lists and a
stale copy would write the moved rows straight back onto the board.

Deleting an archived tab destroys its rows with the record, which is what
"irreversible" should mean. `deleteArchivedTab` still calls `releaseTab` as well,
for tabs archived before this change whose rows are stranded in `Workspace.tasks`
under a tab id that no longer exists anywhere.

`tabDisplayName` resolves archived tabs, so any chip still naming one reads as the
session's name rather than a truncated id.

### Exempting a tab or a workspace (2026-09-02)

`Tab.overlord_exempt` and `Workspace.overlord_exempt`, both persisted, both
`false` by default. A tab is exempt when either is set. Set from the tab's
context menu ("Exempt from Overlord" / "Supervise with Overlord", shown for
agent tabs while Overlord is on) and from a per-workspace eye-off button in
the sidebar, which stays visible while the exemption is on — an exemption you
can't see is one you forget you set, and then wonder why the fleet is missing
a workspace. When the workspace is exempt the tab item is disabled and says so;
the workspace flag wins.

**What exempt means: the engine cannot see the tab.** `agentTabs()` is the one
enumeration every engine path starts from — the tick (rules, facts poll, task
mirror, census), the liveness probe, `spentTabs`, `rulesForTab` — and it skips
exempt tabs there, so nothing downstream has to remember. No rule evaluates, no
proposal is raised, no card appears on the deck or the fleet (the bar counts
them: "2 exempt — not shown"), no Trigger menu or composer bolt is offered.
`boardWorkspaces` also drops an exempt workspace, so its tabs never reach the
deck's signals.

**The agent's tools refuse, and say why.** They take a tab id from outside, so
they check `isExemptTab` explicitly and answer `reason: 'exempt'` with a detail
that tells the agent to leave it alone: `driveTab`, `recoverTab`, `archiveTab`
and `closeTab` (via `retireGuard`), `deleteArchivedTab` (workspace flag only —
an archived tab is in no pane), `resumeTab`, `resumeWorkspace`, `getTabPrompt`,
`answerTabPrompt`.

**Exempting releases what the engine already holds.** The filter in
`agentTabs()` stops new work, but a ritual mid-sequence, an outstanding
directive, a drive watch reading the tab's transcript, a proposal card offering
to type into it, all outlive that filter — and the human reaches for "exempt"
exactly while one of those is happening. So the closed-tab sweep treats an
exempt tab as closed: on the next tick its ritual is aborted, its
outstanding/liveness/drive-watch/handoff entries dropped, its proposals and
escalations removed. Between clicks and ticks, `runSequence` re-checks
exemption AFTER its injectable wait and directly before the paste (the wait
can hold for minutes and return the instant the agent goes quiet), and
`proposalStillHolds` is false for an exempt tab, which covers "Send it" and
"Run all". Two exceptions to the sweep, both because exemption is about
supervision and not the work: escalations carrying the human's own board
actions (`task_handoff`, `task_dropped`) survive it, and an escalation with
no tab at all ("Send" on an unassigned task passes `''`) is nobody's to
sweep — it used to die on the next tick, so handing an unassigned task to
the agent had never worked. `isBoardableTab` is false for an exempt tab, so the scan and
reply paths create no placeholder rows for it either. `listWorkspaces` marks
the tab and the workspace `overlordExempt: true`, and doctrine v8 tells the
agent what that means — including not raising the refusal to the human, since
being left alone is what they asked for.

**What exempt does not mean.** The tab's task rows stay on the board: tasks
are maiTerm's, the board is the workspace's index, and hiding work because its
tab is unsupervised would make the exemption cost something it shouldn't. The
human's own actions on the tab (the panel's "Do it", the composer) are
untouched. The flag rides with the `Tab` record through reload, move and
window duplication; `duplicateTab` copies fields one by one and copies this
one explicitly; and a duplicated workspace keeps its flag: an exemption is a
property of the work, not the window.

### Fleet: parked workspaces, sort, View, Trigger (2026-09-02)

**Tabs in a suspended workspace are not on the fleet.** They used to be, as
dormant cards reading "not loaded — open its workspace to check on it" — a
description of a tab nobody expects to be running, one per agent tab in the
parked workspace. Parked is not dormant: every PTY in that workspace was killed
on purpose. The fleet counts them in its bar ("3 agent tabs in suspended
workspaces — not shown") so the human knows where they went, and the empty
state says "every agent tab is in a suspended workspace" rather than "no agent
tabs" when that is what happened. The deck's own `unready` signal was already
gated on `loaded`, so nothing else changes. A tab suspended on its own inside
an active workspace is still shown — that is routine (every tab but the active
one is suspended on a timer) and its card carries the resume remedy.

**Sort.** `peak context` (default) is the original most-in-need order — busy
rituals, then permission, then pressure, then staleness, highest context within
each. `latest activity` is the most recent real turn first, nothing else. The
choice lives in the component, so it holds for the window's life and resets
with it.

**View and Trigger.** The card is no longer one big button. `View` (bottom
left) navigates to the tab. `Trigger ▾` (bottom right) opens a menu of every
rule that can be fired at that tab by hand — `overlordStore.rulesForTab`:
every rule with at least one step that has TEXT whose scope covers the tab's
workspace, enabled ones first, disabled ones tagged `off`. (The rules editor
persists "New rule" with one blank step the moment it is added; counting
steps rather than text put a global "New rule · off" in every menu in the
window, and firing it pressed a bare Enter at the agent — `runSequence` now
skips blank steps as well.) Picking one calls
`overlordStore.fireRule(tabId, ruleId)`, which is `checkpointTab` generalised
(that button now delegates to it): the rule's `when` clause and rate limiters
are skipped, because a human clicking is the override they guard, and the
mechanical guards are not — `runSequence` still re-checks the live REPL at
every step and `waitInjectable` still waits for idle and quiet. The card's
ritual strip is the success feedback; a refusal shows on the card itself for
eight seconds (`fireRefusal` in `overlord/format.ts` is the one vocabulary for
those, shared with the Checkpoint button and the composer).

Two scope decisions:

- **Scope holds; `when` doesn't.** A rule pinned to a workspace was pinned
  because its steps belong there; typing them into another workspace's tab is
  what the scope field exists to prevent. Disabled and superseded rules ARE
  offered — "don't run this on its own, but let me run it" is a legitimate
  configuration and the menu is the only place it can be exercised.
- **The readiness test depends on the rule.** An `agent_unready` rule is FOR an
  unbound tab, so it fires only at one (`tab_ready` refusal otherwise: "already
  bound — this rule re-binds one that isn't"); every other rule needs a bound,
  running agent. Fired at a stopped tab either would type into bash.
- **A permission prompt is a refusal, not a wait.** The tab IS `ready` there,
  but a rule wanting `idle` would sit in `waitInjectable` for its whole
  five-minute cap holding the ritual slot — strip frozen at 1/N, every other
  rule on the tab blocked, the composer's toast having said it started. Nothing
  clears that but the human answering, so `tab_permission` says so up front.
  A rule whose `agent_state` guard admits `permission` is let through. The
  same check (`permissionBlocks`) guards "Send it" on a proposal card and the
  batch "Run all", which had the identical stall — there the card stays,
  since the proposal is still true, and the run counts it as skipped. The
  engine's own tick never had it: `guardsPassSync` tests
  `agent_state` before firing. It is the human-initiated paths, which skip
  the guards on purpose, that need the one guard a human can't override.
- **Five call sites asked "has a sequence" and each counted steps.**
  `hasRunnableSequence` (a step with text) is now the one answer, used by
  `rulesForTab`, `fireRule`, `checkpointRuleFor`, `checkpointThreshold` and
  the tick. The second was the worst:
  a blank `context_pct` rule prepended by the editor at a lower threshold won
  `checkpointRuleFor`'s strict tie-break, so the deck's Checkpoint button
  reported `started` and ran nothing, instead of the real checkpoint rule.

**The same menu at the bottom of every agent tab.** When Overlord is enabled
and `rulesForTab` is non-empty, `ComposerDock` shows a bolt button — next to
the collapsed handle, and LEFT of the input when the composer is open —
opening the same rule menu. Left, because the right-hand button is only ever
Send: reaching past a rule-runner to send a message is how a stray click
types a whole ritual into a tab. Collapse sits outermost, then the bolt. Feedback there is a toast, since the tab is what
the human is looking at and the deck's note slot isn't on screen.
`rulesForTab` is empty for a tab that has never hosted an agent, so a plain
shell never gets a button that types into bash. The supervisor's own tab is
NOT excluded: the ruleset is its harness too (§9.2), and the engine's tick
already runs rules on it.

### Triage: run all

One button clears the deck of everything Overlord already decided on — added
2026-08-23, `overlordStore.runTriage()`. It covers exactly two actions:

1. re-bind every `unbound` agent, then
2. approve every pending proposal.

Re-binds go **first**: a directive aimed at an unbound tab lands nowhere, so
fixing the binding is what makes the proposals behind it worth sending.

Deliberately excluded: restarting `stopped` agents (relaunching a fleet is not a
one-click action — same reasoning as `recoverAllUnbound`), and the stale-task and
escalation buttons. Marking work done, parking it, dropping it, or clearing an
escalation are judgement calls; a bulk control that quietly made them would be
the worst kind of convenience.

**Pacing.** Every action ends with an agent starting a turn, so a deck of forty
signals is forty concurrent API streams against one org's rate limit. Three
brakes, bounding different things:

| Brake | Default | Bounds |
|---|---|---|
| `TRIAGE_STAGGER_MS` | 1s between sends | the shape of a wave — a ramp, not a spike |
| `TRIAGE_GAP_MS` | 5s between waves of `TRIAGE_BATCH` (10) | sustained *start* rate |
| `TRIAGE_HOLD_CAP_MS` | hold up to 2min | **concurrency** — waits while ≥1 wave of rituals is still in flight |

The hold is the one that matters: turns run for minutes, so a gap alone just
spreads the starts and the waves stack into the burst the pacing existed to
avoid. It is capped so one wedged ritual cannot strand the rest of the run.

**Sent is not an outcome.** After the last wave the run enters a fourth phase,
`verifying`, and stays open until every re-bind target has either registered or
run out of time (`REBIND_VERIFY_MS`). This exists because a `/maiterm init`
typed at a tab whose agent is gone is *delivered perfectly and achieves nothing*
— and for SSH tabs that is the normal failure, since `ssh_foreground` cannot see
the far side (§9.4). The run that exposed this reported **"sent 69, skipped 0"**
while 14 tabs never came back, 13 of them SSH.

So `runTriage` returns `{ sent, skipped, bound, silent }`: `sent` is delivery,
`bound` is outcome, and `silent` is the gap — re-binds that landed and were never
answered, each of which is now a `stopped` card offering a restart. The deck says
so: *"sent 69, re-bound 55 — 14 never answered and need a restart."* A cancelled
run has no verdict and reports only what it sent.

Only re-binds are verifiable this way. A proposal starts a ritual whose completion
is a different thing entirely, and is reported as sent.

`recoverAllUnbound` (the smaller "Re-bind all" button) deliberately does **not**
hold open — it is usually a tab or two, and a 45s spinner would be worse than the
cards correcting themselves. It now says what it knows ("sent … — any that don't
answer come back as needing a restart") rather than claiming it re-bound them.

One worklist function (`triageJobs`) backs both the button's label and the run, so the
label cannot promise work the run then skips. It also resolves a collision: an
`agent_unready` rule fires on the same signal the re-bind reads, so an unbound tab yields
BOTH a re-bind job and a proposal whose sequence is very likely the identical
`/maiterm init`. Running both types it twice — or, once the re-bind lands and the tab is no
longer unready, leaves `waitInjectable` spinning for its full 5-minute cap while holding
the tab's ritual lock, blocking every `only_if_no_outstanding` rule and stalling the run's
own drain check on rituals that will never inject. The re-bind wins (it bypasses guards
that are false by definition on an unbound tab) and the superseded proposals are dropped.

The worklist is snapshotted up front and **re-validated at send time** — a run
takes minutes and the deck changes underneath it, so firing a proposal the human
dismissed thirty seconds ago would be indistinguishable from ignoring them.
Cancellable at any point via `cancelTriageRun()`.

Note `liveness` is a plain Map: every mutation path must call `bumpLive()`, or
the run-all count — a promise about what the click will do — goes stale.

### Board view: one job at a time

The board originally nested workspace → workstream → six lanes, rendering every
board in the window at once. At fleet scale that put the human back to walking
projects to reach a task — the navigation tax the task system exists to remove,
in kanban clothing. Reshaped 2026-08-23 (`OverlordBoardView.svelte`):

- A workstream **index** on the left, exactly **one** board on the right. The
  workstream is the unit of attention; which tab owns an item is a chip on the
  card, not a level of the hierarchy.
- Each index row carries a proportional **lane spread**, so the shape of a job
  (all to-do vs all review) reads without a number, plus a worst-first pip
  (stale → blocked → active) and a stale count.
- **Everything** merges every stream into one flat grid — one grid, not N nested
  ones — with a workstream chip per card that jumps to that stream.
- Drag a card onto a **lane** to change status, onto an **index row** to change
  job. A cross-workspace drop is refused (the lists persist per workspace) and
  renders as refused rather than silently no-opping.
- The index is one O(n) pass over the window's tasks; the previous shape ran
  lanes×streams filters on every render.
- Lanes cap at 40 cards and **announce** the overflow — a silent cap reads as
  "that's all of it".
- The rail collapses to a horizontal strip via a **container query**: this board
  lives in a pane that can be narrow inside a wide window, which a media query
  cannot see.

#### The card (reworked 2026-08-24)

The owning tab **headlines** the card as one clipped line of plain text, not a
chip button in the footer. Long tab names wrapped the footer and pushed the
controls around, and the point of the board is not having to walk to the tab —
its name is context, not a destination. Reaching it is a labelled **View**
button instead.

The footer is a three-part row: `‹` and `›` hug the edges they move toward, and
the two acts that take a task *off* this board sit centred between them, so
neither is hit while reaching for the other. Delete moved up to the card's top
right, out of the stepping path entirely.

- **View** — jump to the owning tab (disabled when unassigned).
- **Send** — hand the task to the Overlord agent. It queues a `task_handoff`
  escalation naming the task, its lane, its workstream and its tab, and rings
  the agent's doorbell like any other item. The agent decides what it needs:
  drive that tab, drive a better one, carry it itself, or escalate. Re-sending
  is allowed — a nudge is sometimes the point — and the button becomes a
  receipt saying when it last went.

  Disabled when the window has no agent tab. A handoff is agent-only, so it
  never appears on the deck, and `sweepUndeliverableEscalations` throws
  undelivered ones away after 30 minutes — accepting one with nobody to collect
  it would leave a "Sent" receipt as the only trace of a hand-off that was
  quietly destroyed. If a handoff IS swept (the agent tab was closed after the
  fact), the receipt is withdrawn with it.

#### Deleting a task has to reach whoever was carrying it

Deleting used to be silent, which made the board lie to the agent: the row
vanished here while the agent still believed in the work, and the next time it
re-sent its list (re-prime, resume, compaction) `findDuplicate` found nothing
and put the task straight back. The human's decision has to land in the tab or
it doesn't stick.

`overlordStore.deleteTask` therefore acts, in the same act-or-escalate shape as
every other card:

1. In-flight task on a tab that is **idle** → a one-line notice, direct. It is
   **not** a directive: it asks for nothing back, so it must not occupy the
   tab's outstanding slot and block every `only_if_no_outstanding` rule behind
   it. Idle is checked, not just liveness: `hasLiveRepl` is true for `active`
   and for `permission`, and a permission prompt is the keystroke menu every
   other injection path refuses. Notices serialize per tab, since
   `bracketedPasteSubmit` is write → settle → CR and two quick deletes on the
   same tab would otherwise merge into one prompt.
2. Tab not reachable (no live REPL, mid-repaint, unmounted) → a `task_dropped`
   escalation, so the agent relays it when the tab comes back.
3. Unassigned, or already done/parked → nobody to tell. Sweeping finished rows
   is routine tidying and must not type a line per card.

This is **not** a tombstone: an agent that ignores the notice can still re-add
the row. Making that mechanically impossible needs a persisted drop list that
`findDuplicate` consults — a schema change. What shipped closes the "nobody ever
told it" hole, which was the actual bug.

**The notice must not assume the agent ever had the task** (2026-08-31, `69e38a6`).
It read *"Drop it from your own list too"*, and a human who creates a row and
deletes it a minute later produces an agent that has never heard of it — which
answers that it has no such task, then goes looking for what it apparently
missed. Nothing on this side knows whether it was ever delivered: `listTasks` is
a **pull**, so a row can be created and destroyed entirely between an agent's
reads of the board. Same shape as the rest of this file's bugs — asserting as
fact something only knowable somewhere else. The notice now covers both readings
(drop it if you have it; if you have never seen it there is nothing to do and
nothing was missed), and the escalation carries the same correction, since there
the agent relays it in its own words and would otherwise reproduce the original.
The better fix — stamping rows as they go out through `listTasks`, so the notice
can be *skipped* rather than hedged — is schema plus plumbing for a case a
sentence covers.

### `startTask` — the panel's "Do it"

The other half of the same machinery (`ef04b49`). The task panel's ▶ moves a row
to `active` and types a notice at the tab carrying it. Two things distinguish it
from the delete notice:

- It is **not gated on `overlord_enabled`.** The human clicked the button and the
  text goes to the tab they were already looking at, so this is the human typing,
  not maiTerm acting on behalf of a supervisor that has been switched off —
  refusing would mean a dead button for everyone running without Overlord. Only
  the escalate-when-unreachable fallback belongs to the supervisor, and it is
  skipped when there is no agent tab to relay it.
- **The status moves whether or not the notice lands**, and the caller is told
  which happened (`told: 'tab' | 'agent' | 'nobody'`). The human's decision is
  true regardless of whether a paste could land at that instant; but "Active on
  the board" and "the agent has been told" are different facts, and a surface
  that implies the second while doing only the first is how a task sits Active
  for an hour with nobody working on it.

The guard block both share is `noticeToTab(tabId, text, what)`. It exists as one
function because the genuinely tricky part here — *when it is safe to type at a
tab* — is exactly the thing that drifts when it is written twice: idle only,
re-checked after the liveness round trip, 1500ms of output quiet, and serialized
per tab. A notice deliberately does **not** take the tab's outstanding slot,
which would block every `only_if_no_outstanding` rule behind something nobody is
waiting on.

Both new kinds are agent-only (hidden from the deck, deleted on delivery) and
both are named in the doctrine, so `DOCTRINE_VERSION` went to **5**.

### Task model — minimum viable

`id`, `title`, `workspace_id`, `tab_id` (assignee, nullable = backlog), `state`
(`backlog` / `todo` / `active` / `blocked` / `review` / `done` / `dropped`), `origin`
(`human` / `overlord` / `agent`), `created_at`, `updated_at`, and `notes` (the append-only
progress log). The as-shipped model is `docs/tasks.md` §3, which this predates — a
`topic_id` linking a mesh conversation was sketched here and never implemented, so it was
pruned 2026-09-10 rather than left describing a field that only ever held null.

Persistence: board rows live in the state file alongside workspaces, keyed by
`workspace_id` (per-window derivation free). The human can CRUD tasks directly
on the board; done tasks are TTL-swept like completed topics.

### Feed it automatically: mirror TodoWrite

The transcript tail is already parsed, and Claude Code's `TodoWrite` state is in
it. Mirroring each tab's todos into the board needs **zero agent cooperation and
no protocol**, and it inverts one of the original complaints: rather than nagging
agents to keep task lists, you can *see which tabs are doing complex work with no
todo list* and let a rule nudge exactly those (`no_todo_list`).

### Views

**Attention queue (primary).** The stated pain is losing things, so the view worth
opening every morning is a single queue: tabs near compaction, commits owed a
review, tasks stale > N days, blocked tasks, unready mesh members, tabs idle with
open work.

**Kanban (secondary).** Grouped by workspace. Kanban is for planning; the
attention queue is for the actual pain.

### Reaching the human

Per existing working norms: Overlord reaches the human **only** via native
AskUserQuestion / permission prompts, plus the board. No status chatter.

And **never with a question a tab is already asking them** — the board carries that one
alone (§9.1.1). The channel being narrow is what makes it expensive: an AskUserQuestion
stops the supervisor dead and interrupts the human, so spending one to relay a question
whose answer it cannot act on is the worst use of the only channel it has.

---

## 12. Build order

1. **Context Steward, standalone.** `contextPct` → idle detection → the
   checkpoint ritual. No board, no agent, no kanban, no rules UI. Nearly free
   given `build_meta`, and it kills a large slice of the micromanagement by
   itself.
2. **TodoWrite mirror + read-only board** in a minimal Overlord workspace
   (accessor row + board view). Still no agent — pure visibility. (Mirror
   confirmed in scope, 2026-08-22.)
3. **Rule engine + preferences UI.** Generalize step 1 into `OverlordRule`, seed
   `DEFAULT_OVERLORD_RULES`, add review-after-commit and doc-drift. Ledger lands
   here. Run in propose-mode.
4. **Overlord-the-agent**, once the board is rich enough that it can be cheap.
   `replyToOverlord`, escalation queue + wake nudge + `listEscalations`,
   `driveTab` MCP exposure, `proposeRuleChanges`.

---

## 13. Open questions

1. **`only_if_no_outstanding` — global or per-rule?** Global serialization makes
   the protocol clean but queues an urgent nudge behind a slow checkpoint.
   Leaning global for v1.
2. **Global rules span windows.** Preferences are global, so a global-scoped rule
   applies in both the work and personal windows. Judged acceptable: the rules
   worth scoping global are universal hygiene, and intensity differences get
   workspace scope. Revisit only if it bites.
3. **Does the agent read compaction summaries?** Deliberately not for now — it
   asks the target agent instead, which keeps memory where it belongs and stays
   clean across Claude/Codex/Gemini. maiTerm *could* expose summaries later.

---

## 14. Rejected alternatives

Recorded so they don't get re-litigated.

| Rejected | Why |
|---|---|
| `⟦OVERLORD⟧` envelope on outbound messages | Re-creates the "hold it for the human" trap and makes `/compact` unsendable. Authority belongs in Overlord's own harness. |
| maiTerm notes as the cross-compaction handoff artifact | Notes are a human surface. Asking the target agent is more portable across runtimes. |
| Cross-window board | Windows are deliberately separate mind-spaces (work vs personal). Per-window is the feature, not the limitation. |
| Tab-id rule scoping | Tab ids don't survive Cmd+Shift+R (reload = dup + close, new id) — rules silently detach. |
| Session-id rule scoping | Survives reload and resume, but `/clear` and fork mint new ids, and a fresh duplicate briefly shares one sid across two tabs. Trades one silent breakage for three plus an ambiguity. |
| Label / runtime scoping in v1 | Deferred, not rejected — additive later with no migration. Runtime survives as an engine capability check. |
| Board as a tab type, drawer, or companion window | An Overlord *workspace* won: full main-area real estate, agent-terminal-as-tab reuses all pane machinery, accessor row above the workspace list. |
| Model in the loop for routine nudges | Slow, costly, non-deterministic, and Overlord's own context blows out watching 40 tabs. |
