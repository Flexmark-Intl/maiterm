# Workspace Share

Status: **spec, not built** (2026-09-22).

A workspace exported so that **another user, on another computer**, can bootstrap it: the
same tabs, in the same repos, with the same stack, cloning whatever they do not have yet.

This is a sister feature to backup (File › Export/Import State), not an extension of it.
Backup restores *your* machine exactly — preferences, accounts, scrollback, session ids — and
is wrong for a handoff on every one of those. Share carries **structure**, never identity.

## 1. What travels, what does not

| Carried | Optional (default OFF) | Never |
|---|---|---|
| Workspace name, pane layout (`split_root`), tab names/order | Workspace notes + tab notes | Preferences (accounts, comms token, backup dir…) |
| Each local tab's **root + subpath** (§2) | The task board | Scrollback |
| Each SSH tab's ssh command + remote cwd (§2) | | Local agent session ids |
| Local editor tabs, as root-relative paths; remote editor tabs (`EditorFileInfo.is_remote`) as ssh command + `remote_path` | | Diff tabs (they hold full file contents) and board tabs |
| Stack services (**default ON**, deselectable per service), cwd as root-relative; `env` per §3 | | Archived tabs, `overlord` / `overlord_exempt` |
| An agent tab's runtime (`Tab.runtime`) and, for remote tabs only, its session id extracted from `trigger_variables` (§5) | | The rest of `trigger_variables`, import/attention flags, composer drafts |
| | | Mesh / bridge state (`mesh_purpose`, `bridge_all`), maiLink and comms bindings |

**Ids.** Every id is re-minted on import so importing the same file twice, or a file built
from a workspace you were once sent, never collides. `clone_workspace_with_id_mapping`
(`commands/window.rs`) re-mints workspace, pane, tab, split-branch, task and workstream ids,
but copies **service ids** (`stack`) and **workspace-note ids** verbatim. Service ids collide
for real: the frontend stack runtime is keyed by service id alone (`stack.svelte.ts`), so two
workspaces holding one id share status, in-flight starts and restart timers. (Duplicate
Workspace has the same latent bug today; fix it in that function, which fixes both.)

**Filtering is explicit.** That function is not a stripper: it copies `trigger_variables`,
`composer_draft`, `overlord_exempt`, notes, tasks, `mesh_purpose`, `auto_resume_*` and more,
and it sets the `restore_*` fields from live PTY context (so with none, as on import, they
become `None`). Export builds the share file from an allowlist of the Carried column; nothing
outside it is relied on to fall away.

## 2. Roots, not directories

Tabs sit in subdirectories: `aiTerm/`, `aiTerm/src-tauri/`, `aiTerm/website/` are **one** repo.
Export resolves every local cwd (tab `auto_resume_cwd ?? restore_cwd ?? last_cwd` — the
chain `workspaces.svelte.ts` already uses; `restore_cwd` is only a periodic snapshot and
`last_cwd` is the live value — plus service `cwd` and local editor files) to a **root**:

- In a git repo: root = `git rev-parse --show-toplevel`; the entry carries `subpath` relative
  to it. Root records **all** remotes (name → URL) and the checked-out branch.
- Not a repo, or a repo with no remote: a **plain root** — nothing to clone, nothing to match.

Root paths are stored home-relative (`~/DATA/IDE/aiTerm`) so an identical layout on the
receiving machine hits without a prompt. The user maps **roots**; tabs follow.

SSH tabs carry no local root. Their local `restore_cwd` is only where `ssh` was typed. A tab
is remote when it has `auto_resume_ssh_command ?? restore_ssh_command`; its remote cwd is
`auto_resume_remote_cwd ?? restore_remote_cwd ?? last_cwd` (suspend and the save-all snapshot
null `restore_remote_cwd`; `last_cwd` holds the remote path from OSC 7 or the prompt). With
none of the three, the tab carries no remote cwd and opens at the remote login directory.

## 3. Export

Workspace context menu → **Share Workspace…**. The dialog lists what will be carried, with the
two optional toggles (notes, tasks) and the per-service stack checklist. Before writing, it
warns per root when the receiver would not get your code:

- the branch has unpushed commits, or no upstream at all;
- the root is a plain root (the receiver must supply the directory themselves).

Warnings, not blocks.

**Service `env`.** `Service.env` often holds tokens, and the file's principle is structure,
not identity. Variable **names** always travel; **values** are dropped unless ticked per
variable in the dialog. On import, a service with a dropped value asks for it before its
first start. Output: `<workspace-name>.maiterm-workspace` (§6).

## 4. Import wizard

Entry points: opening a `.maiterm-workspace` file from the OS (§7), or File › Import Workspace….

**Step 1 — resolve roots.** For each root, expand `~` and check the recorded path:

One rule decides every candidate directory — the recorded path, `X/<repo-name>`, or one the
user picked:

- **absent or empty** → clone into it;
- **matches** (§4.1) → use it as-is, no clone;
- anything else → rejected, with the reason.

Step 1 applies it to each git root's recorded path; only rejected roots need a destination.
(Plain roots map only if their recorded path exists; otherwise they get a picker, with no
clone and no match check.)

If any roots need a destination, offer **"Create all in…"**: pick one parent `X`; each
unmapped git root's candidate is `X/<repo-name>`, where the name is the last path segment of
the `origin` URL (the first recorded remote when there is no `origin`). A rejection there is
**for that root only**; it falls back to a per-root prompt.

Per-root prompt: a directory picker that states which repo will be cloned into it, validated
by the same rule.

**Step 2 — clone.** Clones run in a **visible terminal tab**, one per root, never a hidden
subprocess: a clone can ask for an SSH passphrase, a 2FA touch, or host-key trust, and a
hidden one just hangs. `git clone --branch <recorded>`; if the branch does not exist on the
remote, fall back to the default branch and say so. The wizard waits on each clone's exit
status; a failure leaves that root unmapped with a retry.

**Step 3 — build.** Workspace inserted after the active one in the window the import was
started from (the OS-open path: the focused window). Tabs open at `mapped_root/subpath`. If a
subpath does not exist in the fresh clone (a directory that was never committed), fall back to
the root and note it on the tab.

### 4.1 "Matches"

A directory matches a root when:

1. it is a git repo **top level** (after resolving symlinks), not a subdirectory of one; and
2. **any** of its remotes, normalised, equals **any** of the root's recorded remotes.

Normalisation: strip `.git`, trailing `/`, `user@` and scheme; `host:path` → `host/path`;
lowercase the host. So `git@github.com:org/x.git` ≡ `https://github.com/org/x`. "Any against
any" is what keeps a fork (`origin` = fork, `upstream` = ours) valid.

## 5. Agent tabs

**What export can know.** The runtime is `Tab.runtime` (set by initSession / the SessionStart
hook, backfilled from a persisted session var). The session id is the runtime's
`session_id_var` in `trigger_variables` (`claudeSessionId`, `codexSessionId`). Both are sticky
— they survive the agent exiting — so export knows "this tab **ran** X", not "X is running".
It treats such a tab as an agent tab only when an agent is live in it at export, or its
`auto_resume_command` launches that runtime; otherwise it is a plain shell tab. The export
dialog shows which tabs it decided are agent tabs, so a wrong guess is visible and can be
unticked.

- **Local agent tab** → a **fresh** agent of the recorded runtime, in the mapped cwd. The
  sender's local transcript is on the sender's disk; there is nothing to resume.
- **Remote agent tab** → a **fork** of the recorded remote session: `claude --resume <id>
  --fork-session`, `codex fork <id>` (the forms `src/lib/agents/resume.ts` builds). Forking,
  never plain resume, so the sender and receiver never write to one session. Codex fork is
  wired but **not yet exercised live** (`docs/codex-integration-review.md`) — verify it as part
  of this feature.

  The fork only works if the receiver's ssh login can read the sender's transcript: **the same
  remote user** (Claude: `~/.claude/projects/<cwd-key>/`; Codex: `~/.codex/sessions/`, keyed by
  date rather than cwd). The importer reuses the recorded remote cwd, so Claude's cwd keying is
  satisfied by construction. When the receiver logs in as someone else, Claude prints
  `No conversation found with session ID: <id>` and exits 1 (Claude Code 2.1.280; Codex's
  equivalent is unverified).

  **Detection is new work** — nothing in the codebase watches for this today. The fork is typed
  into a visible shell, so the signal is the agent returning to the shell prompt (OSC 133) with
  a non-zero status within a few seconds of launch; the message text is a secondary check,
  never the only one. On failure the tab gets a fresh agent in the remote cwd with a note; it
  never leaves a dead tab.

The session id travels in the file for remote tabs only, extracted on its own: the rest of
`trigger_variables` never travels. It is not a credential, but it names a conversation on a
shared host, which is why local ones are dropped.

## 6. File format

`.maiterm-workspace`: gzip'd JSON, **not** `AppData`, its own schema:

```jsonc
{
  "format": "maiterm-workspace",
  "version": 1,
  "exported_at": "2026-09-22T…Z",
  "maiterm_version": "2.x.y",
  "workspace": { "name": "…", "split_root": {…}, "panes": [ … tabs with root_id + subpath … ] },
  "roots": [ { "id": "r1", "path": "~/DATA/IDE/aiTerm", "kind": "git",
               "remotes": { "origin": "git@github.com:…" }, "branch": "main" } ],
  "services": [ … root_id + subpath instead of cwd … ],
  "notes": null,   // present only when opted in
  "tasks": null
}
```

A version above what this build reads is refused with "update maiTerm", never partially
imported.

## 7. Opening the file from the OS

maiTerm registers `.maiterm-workspace` in Tauri's `bundle.fileAssociations`
(`tauri.conf.json`):

- **macOS**: the bundler generates `CFBundleDocumentTypes` + an exported UTI, merged with
  `src-tauri/Info.plist` (which declares none, so no conflict).
- **Windows**: the NSIS installer — the only Windows artifact CI ships — writes the
  association with `"%1"`, so the path arrives in argv.
- **Linux**: the bundler only writes `MimeType=` into the `.desktop` file. It does **not**
  ship a shared-mime-info definition, so nothing maps `*.maiterm-workspace` to that type and
  the deb would not associate the file. The deb must also install its own
  `/usr/share/mime/packages/maiterm.xml` and run `update-mime-database` in postinst. The
  AppImage (the updater artifact, so every self-updating Linux user) does not register at
  all: those users get File › Import Workspace…, documented rather than worked around. No rpm
  is shipped.

Delivery differs per OS, and each needs code:

- **macOS**: files arrive as `RunEvent::Opened { urls }` on the **running** process, both on a
  cold launch and when already open. The variant only exists on macOS/iOS/Android (Tauri
  2.11.3), so the handler is `cfg`-gated, and it carries `file://` URLs (`to_file_path()`), not
  paths. On a cold launch it arrives before any webview is ready, so it is queued in Rust and
  handed over when the first window's frontend asks.
- **Windows / Linux**: the OS launches **a second process** with the path in argv. maiTerm has
  no single-instance guard today, and a second maiTerm on the same state file is a corruption
  risk regardless of this feature. `tauri-plugin-single-instance` forwards the argv to the
  running process and exits the second one; a cold launch reads its own argv.

Dev builds are never bundled, so they never register; testing the OS path needs a local
deploy build.

## 8. Open questions

- Private repos the receiver cannot access fail at clone (step 2) with git's own message.
  Worth an earlier `git ls-remote` probe in step 1? Proposed: yes, it is cheap and it turns a
  late failure into a "you don't have access to X" before anything is created.
- Stack service commands may call tools the receiver does not have. Out of scope for v1: the
  service fails visibly in its own tab like any other.
