# Agent Defaults (DESIGN, 2026-11-22, nothing built)

One Preferences section, **Agent Defaults**, with a part per runtime (Claude, Codex,
Antigravity), that sets the values every new session of that agent starts with: which model,
how hard it thinks, which advisor, how long transcripts are kept. Darryl wanted Fable as
Claude's advisor in every session without typing `/advisor` each time (2026-11-21); the rest of
the first version was picked from a shortlist on 2026-11-22 (§3).

It is the sister of Deshittification, not part of it. Deshittify switches anti-features OFF
(a toggle per rule); Agent Defaults picks VALUES (a dropdown or a number per setting). They
share the mechanics: §2.

## 1. What it is not

- **Not per tab or per session.** These are the agent's own user-level defaults, so they reach
  sessions maiTerm didn't start too. A session's own `/model`, `/effort`, `/advisor` or launch
  flag still wins for that session (flags and in-session choices sit above user settings in
  every runtime).
- **Not maiTerm state.** No `Preferences` field, no `AppData` field, nothing in
  `preference_meta`, no MCP tool. An agent that wants a different model asks its human.
- **Not Hive's launch settings.** The Hive launch builder (`hive_launch_args`) passes `--model`
  and `--effort` on every launch, coordinator and workers alike, so those two defaults never
  reach a Hive agent. The coordinator loads every settings source like a normal tab, so it gets
  the rest (the advisor, retention). Workers launch with `--setting-sources project` (a reviewer
  `local`) and never read user settings, with ONE exception: the launch builder passes the
  advisor itself as `--advisor`, read by `launch_advisor(root)` (`hive/launch.rs`) from the
  same `settings.json` the human's tabs read (Darryl, 2026-11-22: workers consult Fable too).
  No other Agent Default reaches a worker, retention included (§6).

## 2. Mechanics (borrowed from Deshittify, `commands/deshittify.rs`)

- **Disk is the state.** The section reads each agent's file and shows what is there. A value
  the user (or the agent's own `/config`) changes by hand reads back as changed: no drift, no
  re-assert loop.
- **"Agent's default"** is a choice in every control, and means the key is ABSENT: maiTerm
  removes it rather than writing the runtime's current default, so a later change of default
  upstream reaches the user.
- **maiTerm touches only the keys this section offers.** Everything else in the file is
  preserved byte-for-byte where the format allows (TOML via `toml_edit`, as
  `mcp_proxy/entries.rs` already does).
- **An unreadable file blocks its runtime's part** with the parse error shown, and is never
  overwritten (the rule `read_settings_at` states and why).
- **Writes go through `util::json_file`** (`write_lock` + change-detecting replace), like
  `claude_code/lockfile.rs` and `claude_code/agy.rs`: live agents and maiTerm's own hook
  installer write the same files. (Deshittify's own `write_claude_settings` predates that helper
  and does not use it: worth moving over separately.)
- **An isolated instance makes no writes** (`instance::global_write_refusal`).
- **Managed accounts need nothing extra.** Every account root symlinks `settings.json` and
  `config.toml` back to the shared file (`accounts/mod.rs`), so one write reaches every account.

### 2.1 SSH hosts

The values travel to a bridged host the way Deshittify's do: a python3 merge appended to the
bridge setup script (`render_remote_setup_script`, run from `sshMcpBridge.svelte.ts`), and the
existing Antigravity merge in `commands/ssh_tunnel.rs` for its file. **This machine is the
source of truth in both directions**, as in Deshittify: a value set here is set there, and
"Agent's default" here removes the key there. The script rewrites a remote file only when
something changed, and never one it can't parse.

## 3. The settings (first version)

Every key below was checked against the installed binary or its changelog (Claude Code
2.1.330, Codex 0.166.0, Antigravity 1.5.14). **Re-check before building**: these are fast-moving
CLIs, and a wrong key is silently ignored.

### 3.1 Claude — `~/.claude/settings.json`

| Setting | Key | Notes |
|---|---|---|
| Advisor | `advisorModel` (+ `env.CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL`) | Off / opus / sonnet / fable. **The advisor needs Claude's feature flags, and Deshittify's "Disable telemetry" rule (`DISABLE_TELEMETRY`, also `DO_NOT_TRACK`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`) switches flag fetching off**: then `advisorModel` is silently ignored and `/advisor` says "isn't available while DISABLE_TELEMETRY is set". Setting `CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL=1` bypasses the flag (Claude Code 2.1.330, tested 2026-11-22: debug log shows "Server-side tool enabled with claude-fable-5-2" only with it), so the control writes it beside `advisorModel` and removes it with Off. The `/advisor` command itself stays refused; the setting still works. Claude Code refuses an advisor less capable than the main model, so the control warns when the pair won't activate. Some advisor models bill to usage credits: say so beside the control. Takes effect on new sessions, or after `/clear` / `/compact` in a running one. |
| Default model | `model` | The public aliases (opus, sonnet, fable, haiku) plus "Agent's default". |
| Effort | `modelSettings.<model id>.effortLevel` | Effort is stored PER MODEL (Darryl's file: Opus 5.5 medium, Opus 5 high, Sonnet 5 medium). The control edits the entry for the default model; other models' entries are shown read-only and left alone. **Verify** how an alias maps to the `modelSettings` key (`claude-opus-5-5`), and what the top-level `effortLevel` key does beside it, before building. |
| Keep transcripts | `cleanupPeriodDays` | Claude deletes transcripts after **30 days** by default, and resume, maiLink and the Loom read them. maiTerm's default is **500 days** (§4). Minimum 1. |

### 3.2 Codex — `~/.codex/config.toml`

| Setting | Key | Notes |
|---|---|---|
| Default model | `model` | Free text with suggestions (Codex's model names change too often for a fixed list). |
| Reasoning effort | `model_reasoning_effort` | Codex's levels. |
| Approvals reviewer | `approvals_reviewer` | e.g. `auto_review` (Darryl's value). **Verify** the allowed values. |

### 3.3 Antigravity — `~/.gemini/antigravity-cli/settings.json`

| Setting | Key | Notes |
|---|---|---|
| Agent mode | `agentMode` | `""` (Default), `accept-edits`, `plan`, `auto`. Note `""` is itself a saved choice ("Default"), distinct from absent ("Auto where available"). |
| Show thinking | `showThinking` | bool, off by default. |
| Queued messages | `queuedMessages` | `queue` / `send-immediately`. |
| Model, effort | **unknown keys** | `agy` has `--model` and `--effort` flags; find the settings keys (set them in `/config` and read the file back) before offering these. |

## 4. One default maiTerm applies itself

**Keep transcripts: 500 days**, applied once per user account like Deshittify's default-on
rules (`seed_default_rules`, marker file `~/.maiterm/deshittify-seeded` or a sibling of it):
only when `cleanupPeriodDays` is ABSENT. A value the user already set (Darryl's 548) is a choice
and is left alone; once seeded, a change in either direction sticks. Nothing else in this
section is applied without the human choosing it.

## 5. UI

A Preferences section beside Deshittification: three collapsible parts headed Claude, Codex,
Antigravity. A part whose CLI isn't installed is shown collapsed with "not installed"; its
values can still be set (they wait in the file). Each control shows the on-disk value, with
"Agent's default" for absent. No section-level switch: these are values, not rules. A line
under the section says changes apply to new sessions and reach bridged SSH hosts on their next
connect.

## 6. Open questions to settle while building

1. The `modelSettings` alias→id mapping and the top-level `effortLevel` (§3.1).
2. Antigravity's model and effort keys (§3.3).
3. **Settled: Hive workers and reviewers get the advisor** through `--advisor` (§1). The
   Advisor control should say so: the choice also applies to every Hive agent, so it
   multiplies the advisor's cost and time by the number of workers. When this section's reader
   lands, `launch_advisor` switches to it (the Hive Feature agent asked to be told its name).
   A Hive's worker transcripts are deleted with the Hive (`delete_hive`), never earlier: a Done
   or retired Hive's workers can still be resumed and their chats are read back.
4. **Settled: a worker runs no transcript cleanup at all.** Claude Code 2.1.330 skips the
   sweep when `--setting-sources` leaves out user settings and no enabled source sets
   `cleanupPeriodDays` ("Skipping retention cleanup: userSettings source is disabled"). So
   worker transcripts are swept by the human's own sessions, at the human's retention. **Never
   give workers a `cleanupPeriodDays` of their own** (in their `--settings` file, say, to keep
   them short-lived): that would turn the sweep on in every worker, over the whole shared
   `projects/` folder, at the worker's period — deleting the human's older transcripts.
