# Agent Defaults (built 2026-11-22: local files, Preferences, SSH hosts)

Code: `src-tauri/src/agent_defaults.rs` (every read and write, path-parameterized, tests),
`commands/agent_defaults.rs` (Tauri), `components/agentDefaults/AgentDefaultsPane.svelte`.

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
- **Not maiTerm preferences.** No `Preferences` field, no `AppData` field, nothing in
  `preference_meta`, no MCP tool. An agent that wants a different model asks its human. The
  one thing maiTerm keeps is WHICH settings it manages and the value each was given
  (`~/.maiterm/agent-defaults.json`, §2.1), beside the files it describes and shared by every
  build like them.
- **Not Hive's launch settings.** The Hive launch builder (`hive_launch_args`) passes `--model`
  and `--effort` on every launch, coordinator and workers alike, so those two defaults never
  reach a Hive agent. The coordinator loads every settings source like a normal tab, so it gets
  the rest (the advisor, retention). Workers launch with `--setting-sources project` (a reviewer
  `local`) and never read user settings, with ONE exception: the launch builder passes the
  advisor itself as `--advisor`, read by `launch_advisor(root)` (`hive/launch.rs`) from the
  same `settings.json` the human's tabs read (Darryl, 2026-11-22: workers consult Fable too).
  No other Agent Default reaches a worker, retention included (§6).

## 2. Mechanics (borrowed from Deshittify, `commands/deshittify.rs`)

- **Disk is the state of each value here.** The section reads each agent's file and shows what
  is there. A value the user (or the agent's own `/config`) changes by hand reads back as
  changed, and is never re-asserted here. For a managed setting the section also says when the
  file no longer holds the recorded value hosts are given (§2.1).
- **maiTerm changes only the settings the human ticked** (Darryl, 2026-11-22). Each row has a
  tick; an unticked setting's control is read-only and maiTerm never changes it, here or on a
  host. Ticking records what this machine's file holds and writes nothing. A switch at the top
  turns the whole section off (nothing changed anywhere) and keeps the ticks.
- **"Agent's default"** is a choice in every control, and means the key is ABSENT: maiTerm
  removes it rather than writing the runtime's current default, so a later change of default
  upstream reaches the user.
- **maiTerm touches only the keys this section offers.** Everything else in the file is kept:
  TOML byte-for-byte (`toml_edit`, a changed value keeps its comment), JSON by value.
- **Claude's file is the one the human's tabs read**: `~/.claude/settings.json` with a managed
  account (every account root links it), else the user's own exported `CLAUDE_CONFIG_DIR`, else
  `~/.claude`, as `hive::launch::launch_config_root` picks. A link to a missing file is refused,
  never replaced.
- **An unreadable file blocks its runtime's part** with the parse error shown, and is never
  overwritten (the rule `read_settings_at` states and why).
- **Writes take `util::json_file`'s `write_lock` and a change-detecting replace**
  (`util::json_file::edit_text` / `edit_json_object`): live agents write the same files. Since
  2026-11-22 every maiTerm writer of them goes through the same helper — the hook installer
  (`lockfile.rs`, also on its reassert timer), Deshittify, the Codex registrar (`codex.rs`
  `edit_document` / `edit_hooks_json`), the MCP proxy's Codex entries and Antigravity's — so none
  can overwrite a value set here in the same instant, a link is written through to its target,
  and a file that doesn't parse is refused by all of them.
- **JSON keys come back sorted** (serde_json without `preserve_order`), as from every other
  maiTerm JSON writer; values and the keys we don't own are kept. TOML is kept as written.
- **An isolated instance makes no writes** (`instance::global_write_refusal`).
- **Managed accounts need nothing extra.** Every account root symlinks `settings.json` and
  `config.toml` back to the shared file (`accounts/mod.rs`), so one write reaches every account.

### 2.1 SSH hosts (built 2026-11-22)

**Every managed setting is mirrored to every bridged host, removals included** (Darryl,
2026-11-22: "the point of the setting is to make sure all hosts follow same settings; so removal
mirrors, but disable stops changing anything"). A value set here is set there; "Agent's
default" here removes the key there, even a value the host set for itself. An UNTICKED setting
is left alone everywhere, which is how a host keeps its own value.

- **Hosts get the RECORDED value, never what this machine's file holds at connect time.**
  `Managed.keys` (`~/.maiterm/agent-defaults.json`) maps `claude.advisor`, `codex.effort`… to the
  value the human chose (JSON null: the agent's default), written when they tick (this machine's
  value then) or pick. Anything that drops a key here — a hand edit, another writer's bug —
  would otherwise become a removal on every host, and a removed `cleanupPeriodDays` lets Claude
  sweep a host's transcripts at 30 days. When the file here drifts from the record, the row
  says so and offers to give hosts this machine's value. Ticking refuses a value a host would
  break on (wrong JSON type, a Codex reviewer Codex won't start on); a hand-typed model id is
  carried as it is.
- **How:** `agent_defaults::remote_script`, run by `sshMcpBridge.svelte.ts` on every connect
  AFTER the bridge's own setup and Deshittify's script (all three read-modify-write the same
  remote files), never gating `connected`; also appended last to "Install MCP for Current
  User". A tick, a value or the switch changed in Preferences also reaches hosts bridged NOW:
  the command emits `agent-defaults-changed` and every window runs the script on its connected
  hosts (`refreshAgentDefaults`). On each host it waits for the remote writes that window
  already has in flight there (a connect's setup, Deshittify and Agent Defaults runs), and
  refreshes are serialized, so within a window the last change lands last. Not ordered against
  another window's writes or a connect that starts mid-refresh: seconds wide, and the next
  connect or change corrects it. Without this, a host
  whose tabs bridged before the change kept its old values until a reconnect (2026-11-23:
  server02 had no advisor after the human ticked it). Two python3 snippets: a JSON merge for `~/.claude/settings.json` and
  `~/.gemini/antigravity-cli/settings.json` (`[path, value]` pairs; a removal also drops an
  object it empties: `env`, a `modelSettings` entry), and a textual top-level edit of
  `~/.codex/config.toml` (python3 has no TOML writer), checked with `tomllib` where the host has
  Python 3.11+.
- **Left alone:** a host without the agent's folder (nothing created), a file that doesn't
  parse, a link to a missing file. A file is rewritten only when something changed, through a
  temp file beside the link's target. Empty script when the section is off or nothing is
  ticked. An isolated instance builds none (`global_write_refusal`).
- **Remote Claude file is `~/.claude/settings.json`**, as Deshittify's: a host's own
  `CLAUDE_CONFIG_DIR` is not followed.

## 3. The settings (first version)

Every key below was checked against the installed binary or its changelog (Claude Code
2.1.330, Codex 0.166.0, Antigravity 1.5.14). **Re-check before building**: these are fast-moving
CLIs, and a wrong key is silently ignored.

### 3.1 Claude — `~/.claude/settings.json`

| Setting | Key | Notes |
|---|---|---|
| Advisor | `advisorModel` (+ `env.CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL`) | Off / opus / sonnet / fable. **The advisor needs Claude's feature flags, and Deshittify's "Disable telemetry" rule (`DISABLE_TELEMETRY`, also `DO_NOT_TRACK`, `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC`) switches flag fetching off**: then `advisorModel` is silently ignored and `/advisor` says "isn't available while DISABLE_TELEMETRY is set". Setting `CLAUDE_CODE_ENABLE_EXPERIMENTAL_ADVISOR_TOOL=1` bypasses the flag (Claude Code 2.1.330, tested 2026-11-22: debug log shows "Server-side tool enabled with claude-fable-5-2" only with it), so the control writes it beside `advisorModel` and removes it with Off. The `/advisor` command itself stays refused; the setting still works. Claude Code refuses an advisor less capable than the main model, so the control warns when the pair won't activate. Some advisor models bill to usage credits: say so beside the control. Takes effect on new sessions, or after `/clear` / `/compact` in a running one. |
| Default model | `model` | The public aliases (opus, sonnet, fable, haiku) plus "Agent's default". |
| Effort | `modelSettings.<model id>.effortLevel` | Effort is stored PER MODEL ID, so the section has a row per model: Opus, Sonnet, Fable, Haiku, written under the id each alias resolves to on a first-party login (`claude-opus-5-5`, `claude-sonnet-5-5`, `claude-fable-5-2`, `claude-haiku-5-5`: `CLAUDE_EFFORT_MODELS`, re-check per Claude release). An id key wins over an alias key, which is why the id is written. Other entries in the file are shown read-only. Levels kept from settings: `low`, `medium`, `high`, `xhigh` (`max` is never persisted). The top-level `effortLevel` in user settings is legacy (it applies only to older models) and is left alone. Clearing removes `effortLevel`, and the model's entry only when nothing else is in it. |
| Keep transcripts | `cleanupPeriodDays` | Claude deletes transcripts after **30 days** by default, and resume, maiLink and the Loom read them. maiTerm's default is **500 days** (§4). Minimum 1. |

### 3.2 Codex — `~/.codex/config.toml`

| Setting | Key | Notes |
|---|---|---|
| Default model | `model` | Free text with suggestions (Codex's model names change too often for a fixed list). |
| Reasoning effort | `model_reasoning_effort` | `minimal`, `low`, `medium`, `high`, `xhigh`, `max` (Codex's own list; `none` left out). Codex starts on ANY string here, so the list is maiTerm's check, not Codex's. |
| Approvals reviewer | `approvals_reviewer` | `user` or `auto_review`. Codex refuses to START on any other value ("failed to load bootstrap configuration"). |

### 3.3 Antigravity — `~/.gemini/antigravity-cli/settings.json`

| Setting | Key | Notes |
|---|---|---|
| Agent mode | `agentMode` | `""` (Default), `accept-edits`, `plan`, `auto`. Note `""` is itself a saved choice ("Default"), distinct from absent ("Auto where available"). |
| Show thinking | `showThinking` | bool, off by default. |
| Queued messages | `queuedMessages` | `queue` / `send-immediately`. |
| Model, effort | **not offered** | `agy` has `--model`/`--effort` flags, but 1.5.14's changelog names no `settings.json` key for them and `/model` doesn't save there (only the Orchestrator's planner/executor, under `"orchestrator"`). |

## 4. One default maiTerm applies itself

**Keep transcripts: 500 days**, applied once per Claude settings FILE (marker lines in
`~/.maiterm/agent-defaults-seeded`, `claude_retention <path>`), at every launch until settled,
first launch included: only when `cleanupPeriodDays` is ABSENT. A value the user already set
(Darryl's 548) is a choice and is left alone; once seeded, a change in either direction sticks. An
unreadable file, a link to a missing file, or no Claude folder yet leaves it unsettled, to be tried
next launch. Nothing else in this section is applied without the human choosing it. The seed
is local only: it ticks nothing, so it never reaches a host.

## 5. UI

A Preferences section beside Deshittification: three collapsible parts headed Claude, Codex,
Antigravity. A part whose CLI isn't installed is shown collapsed with "not installed"; its
values can still be set (they wait in the file). Each control shows the on-disk value, with
"Agent's default" for absent. Each row has a tick (§2): unticked, its control is read-only.
One section switch, "maiTerm sets the ticked settings". A line under the section says changes
apply to new sessions and reach SSH hosts on their next bridge connect.

## 6. Questions settled while building

1. **Settled (2026-11-22):** effort per model id, the top-level `effortLevel` left alone (§3.1).
2. **Settled:** Antigravity's model and effort are not offered (§3.3).
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
