# Overlord — Per-Window Project & Agent Supervisor

> Status: **design / proposed**. Not a committed plan. Date: 2026-08-22.
> Owner: Darryl. Scope: a per-window supervisor that tracks projects/tasks across
> workspaces and drives the agents responsible for them, automating the routine
> supervision currently done by hand.

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
  outcome: 'sent' | 'blocked_no_repl' | 'blocked_guard' | 'acked' | 'timed_out';
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
  | { event: 'agent_unready' }                                // dormant, PTY alive
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
| `agent_unready` | `agentState` + `get_agent_liveness` | free |
| `permission_pending` | `agentState` | free |
| `task_stale` / `directive_unacked` | board timers | trivial |
| `no_todo_list` | **TodoWrite mirror — needs building** | medium |
| `commit` | **needs building** | medium |

`commit` has no clean existing source. Options, best first:

1. parse `Bash(git commit)` tool calls out of the transcript tail — most
   reliable, attributes to the right agent, won't fire on Gemini
2. poll `git log` per tab cwd — cheap, coarse, can't tell which agent
3. OSC 133 + command text — breaks on SSH tabs

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

The ritual must be **resumable**: a tab can go idle, hit a permission prompt, or
be interrupted by the human halfway through.

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
  directive_id?: string,     // usually omitted — see below
})
→ { received: true, outstanding_directive: string | null }
```

One shape, four uses (ready / ack / status / escalate). Bounded so it stays cheap
and parseable, and defined as a **protocol rather than a file format** — which is
what keeps it clean across Claude, Codex and Gemini.

### Why `directive_id` is optional

It falls out of `only_if_no_outstanding`. With directives serialized per tab
there is at most one outstanding directive per tab, so an ack matches on
`(tab_id, most recent outstanding)` unambiguously.

**That is what keeps injection genuinely envelope-free** — no `(ack: d_7f3a)`
marker riding along in the injected text to re-trigger the "this isn't my
operator" instinct the design exists to avoid. Keep the field for the rare
parallel case; leave it unset in normal operation.

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

More GUI than terminal, evolving the mesh cockpit rather than adding a surface.

### Task model — minimum viable

`id`, `title`, `workspace_id`, `tab_id` (assignee, nullable = backlog), `state`
(`backlog` / `active` / `blocked` / `review` / `done`), `origin`
(`human` / `overlord` / `agent`), `created_at`, `updated_at`, optional `topic_id`
linking the mesh conversation that is its vehicle.

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
2. **TodoWrite mirror + read-only board** in the cockpit. Still no agent — pure
   visibility.
3. **Rule engine + preferences UI.** Generalize step 1 into `OverlordRule`, seed
   `DEFAULT_OVERLORD_RULES`, add review-after-commit and doc-drift. Ledger lands
   here. Run in propose-mode.
4. **Overlord-the-agent**, once the board is rich enough that it can be cheap.
   `replyToOverlord`, escalations, `proposeRuleChanges`.

---

## 13. Open questions

1. **`only_if_no_outstanding` — global or per-rule?** Global serialization makes
   the protocol clean but queues an urgent nudge behind a slow checkpoint.
   Leaning global for v1.
2. **`no_todo_list` requires the TodoWrite mirror** — the one prerequisite build
   in the whole schema. Worth it, or cut the event from v1?
3. **Global rules span windows.** Preferences are global, so a global-scoped rule
   applies in both the work and personal windows. Judged acceptable: the rules
   worth scoping global are universal hygiene, and intensity differences get
   workspace scope. Revisit only if it bites.
4. **Does the agent read compaction summaries?** Deliberately not for now — it
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
| Model in the loop for routine nudges | Slow, costly, non-deterministic, and Overlord's own context blows out watching 40 tabs. |
