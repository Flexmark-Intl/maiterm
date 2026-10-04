# Move project

A project folder moves — `~/DATA/IDE/aiTerm` becomes `~/DATA/IDE/maiterm` — and everything that
knew it by its path follows: tabs (live and archived), stack services, editor and diff tabs,
watch-script follow-ups and their approvals, and each agent runtime's per-project state, so a
resumed agent comes back in the same session with the same memory.

Two ways in, one engine:

- **Move project…** (workspace right-click menu): maiTerm moves the folder itself. Preferred —
  it can stop the agents first.
- **Locate moved project** (automatic): the folder was moved outside maiTerm. Asked after session
  restore, and whenever a tab finds its saved folder gone.

Code: `src-tauri/src/relocate/` (engine, detection, agent adapters),
`src-tauri/src/commands/relocate.rs` (the ordering), `src/lib/stores/relocate.svelte.ts` (each
window's half), `src/lib/components/MoveProjectModal.svelte`.

## 1. The ordering (`relocate_project`)

1. **Suspend.** Every window, over the window-request channel (`mailink/rpc.rs`, verb
   `relocate.suspend`), suspends its live tabs whose shell is in the folder, plus tabs that
   fell back to home because it was missing (§4). **A running agent must be stopped before the
   move**: Claude keeps appending to the transcript path it opened, and would recreate the old
   project directory behind us. Service tabs are left running; their saved `cwd` is rebased.
2. **Only if every window confirmed**, the folder is renamed (`fs::rename`, so one volume only —
   across disks the human moves it and repoints). A window that times out or refuses calls the
   whole move off, and the windows that did suspend wake their tabs again.
3. **Agents** (§3). Best effort per runtime, reported, never fatal: the folder has moved, and
   maiTerm's own paths must follow it regardless.
4. **maiTerm state** (§2), one write under the lock, saved.
5. **Patch + wake.** Each window (verb `relocate.apply`) takes the rebased path fields into its
   mirror — the frontend writes some of these back, and a stale copy would restore the old path
   — and wakes the tabs it suspended through the serial restore driver
   (`workspace-resume-tabs`). Each spawns in the new folder and runs its auto-resume command.
   A window that doesn't confirm this step is named in the result: its saved state is right,
   but it should be reloaded.

Refused outright: a relative path, a filesystem root, home or anything above it, a folder into
itself or onto its parent, a destination that exists.

## 2. maiTerm's paths (`relocate_state`)

A **prefix rebase at path-component boundaries** — `/a/aiTerm` rebases `/a/aiTerm/src`, never
`/a/aiTermX`. Case folds on macOS (a `cd`-typed `$PWD` keeps its typed case). A `~/…` value stays
`~/…` while the result is under home.

| Field | Note |
|---|---|
| `Tab.restore_cwd`, `auto_resume_cwd`, `last_cwd` | live and archived tabs; **not on SSH tabs** — their `~` is the remote home, and `last_cwd` there can be either side |
| `Tab.auto_resume_command`, `auto_resume_remembered_command` | free text: exact-case occurrences on a boundary (`/`, quote, space, `;&|)`) |
| `Tab.editor_file.file_path` | unless `is_remote` |
| `Tab.diff_context.file_path` | |
| `FollowUp.due.cwd` | watch scripts |
| `Service.cwd` | |
| `Preferences.backup_directory` | |
| `AppData.approved_watch_scripts` | digests of folder + script, one-way: an approval held by a follow-up that moved is re-recorded under the new folder. A moved folder is the same folder; the "same script elsewhere is another approval" rule (docs/follow-ups.md §5.1) is about a DIFFERENT folder |

## 3. Agent state (`relocate/agents.rs`)

All verified against the installed CLIs on 2026-10-04 (Claude Code 2.1.289, codex-cli 0.157.1,
gemini-cli 0.43.0).

**Claude.** `~/.claude/projects/<slug>/`, slug = every UTF-16 unit outside `[A-Za-z0-9]` → `-`,
past 200 chars cut + base-36 Java hash; of the *resolved* path. Shared by every managed account
(symlink), so one place. The slug is lossy (`a.b`/`a-b`, and `aiTerm-backup` shares
`aiTerm`'s prefix), so a name match is only a candidate: a session inside must have started under
the old folder. Subfolder sessions have their own slugs and are moved too. Memory lives under
the git root's slug and moves with it.
- No destination: rename the directory. Destination exists: merge entry by entry, `memory/`
  file by file; a clash stays behind and is reported, never overwritten.
- Each moved transcript gets Claude's own `{"type":"relocated","sessionId","relocatedCwd"}`
  appended — the last one wins over every `cwd` before it, so history isn't rewritten. Without
  it `claude --resume` from the new folder doesn't find the session.
- `projects` keys in `~/.claude.json` **and each account's own copy** (trust, MCP servers; an
  entry already at the new key wins field by field), `githubRepoPaths`, and `project` in each
  `history.jsonl` (up-arrow history).

**Codex.** `config.toml` `[projects."<path>"]` trust (edited with `toml_edit`, layout kept);
`state_*.sqlite` `threads.cwd`; `cwd` in each affected rollout's `session_meta`/`turn_context`
lines. Rollouts are filed by date, so nothing moves — but `codex resume <id>` asks which folder
to use when the recorded one is gone, which would stall an auto-resume.

**Gemini.** `projects.json` path → slug (keep the slug) and the slug folders' `.project_root`
markers — they must agree, or Gemini claims a fresh slug and the history looks lost. Older
installs: `tmp|history/<sha256(path)>`, one-way, so only paths maiTerm's state knows are found.
`trustedFolders.json` keys.

## 4. A folder that is gone (`relocate/detect.rs`)

`pty::spawn_pty` falls back to home when the cwd isn't a directory. That stays — a tab must
still open — but `TerminalPane` now asks `folder_exists` first, and for a missing folder:

- **holds the auto-resume command** — it would run in home and resume nothing;
- **stops saving its cwd** as the restore context — home would overwrite the folder it is
  waiting to go back to (the auto-resume-erasure class of bug);
- records the tab as a *fallback* (`relocate.svelte.ts`) and raises the locate prompt.

Locating the folder suspends fallback tabs, puts back the folder they wanted (suspending saved
home), rebases, and wakes them in the right place.

`find_missing` groups missing folders by the one that actually went away (`aiTerm` for tabs in
`aiTerm/src` and `aiTerm/website`) and suggests siblings that hold every subfolder saved state
needed, aren't already used by another tab, git repos first, newest first. A suggestion is
never applied on its own: a wrong pick runs every agent in the wrong project. A whole volume
unplugged (`/Volumes/x` gone) is not a move and isn't asked about.

Closing the prompt dismisses that folder for this run. Each window asks on its own; when one
locates it, the others' prompts close.

## 5. Not covered

- SSH tabs and remote folders.
- A move across volumes (maiTerm won't copy; move it, then locate).
- Agents in tabs maiTerm doesn't run (another terminal) — they keep writing under the old key.
