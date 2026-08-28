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
      reason: 'no_live_repl' | 'outstanding_directive' | 'agent_busy'
            | 'rate_limited' | 'runtime_mismatch' }
```

- Guards are evaluated **inside** the tool — a guard failure comes back to the
  agent as a structured refusal, never a silent drop, and both paths ledger.
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
      text: 'Before we continue — make sure any relevant docs and memory are updated if needed.',
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
  needs_human?: boolean,     // → AskUserQuestion, never a status note
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

### 9.2 The ruleset is also Overlord's own harness

The enabled rules are **rendered into Overlord's context as its standing
operating doctrine** — not config it manages, but the description of how work is
supervised in this window. So when told "go check on the EWS tabs", it improvises
using the same thresholds, phrasings and ordering the automation would have used.
Its judgment and its automation cannot drift apart, because they read the same
document.

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

`unbound` is handled automatically by the `reinit_unbound_agent` default rule (still
subject to propose-mode). `stopped` is never automatic — relaunching an agent is a
bigger action than re-binding one — but the triage deck offers it as one click, and
resumes the tab's own session rather than starting a fresh one.

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
drift: boardable, not `active`, not `permission`, no outstanding directive or
ritual, and — the one that bites — never `unbound`, because no agent state does
not mean no agent. An unbound tab's process is alive and merely unregistered, and
archiving or closing destroys the TerminalPane, which kills the PTY. Only a tab
positively classified `stopped` has exited; `null` means unclassified, where the
answer is to wait rather than guess.

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

Close still has no bulk form, and the deck still confirms inline (`confirm()` is
inert in a Tauri webview).

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
| **Suspended workspace** (`workspace.suspended`) | still in its panes | killed | `resumeWorkspace` — respawns exactly the tabs that were live |
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
| in `archivedTabs[]` | `restoreArchivedTab` |
| finished | `archiveTab` / `closeTab` |

All five new tools are Overlord-agent-only and are in `PEER_ADDRESSING_TOOLS`, so
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

Archiving releases unfinished rows to the project (`tasksStore.releaseTab`) **after** the
archive succeeds, never before: releasing first meant a failed archive left the tab in
place with its parked rows already persisted back to the project, silently, with nothing
to undo it. The release itself is right — work owned by a tab nobody can see is work
nobody will do — it just has to follow the archive. Done rows keep their
tab id, and `tabDisplayName` now resolves archived tabs so those chips keep
reading as the session's name rather than a truncated id.

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
label cannot promise work the run then skips. It also resolves a collision: the default
`reinit_unbound_agent` rule fires on the same `agent_unready` signal the re-bind reads, so
every unbound tab yields BOTH a re-bind job and a proposal whose sequence is the identical
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

Both new kinds are agent-only (hidden from the deck, deleted on delivery) and
both are named in the doctrine, so `DOCTRINE_VERSION` went to **5**.

### Task model — minimum viable

`id`, `title`, `workspace_id`, `tab_id` (assignee, nullable = backlog), `state`
(`backlog` / `active` / `blocked` / `review` / `done`), `origin`
(`human` / `overlord` / `agent`), `created_at`, `updated_at`, optional `topic_id`
linking the mesh conversation that is its vehicle.

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
