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
| Each SSH tab's ssh command + remote cwd | | Local agent session ids |
| Editor tabs, as root-relative paths | | Archived tabs, `overlord` / `overlord_exempt` |
| Stack services (**default ON**, deselectable per service), cwd as root-relative | | Trigger variables, import/attention flags, composer drafts |
| Which runtime an agent tab ran (Claude / Codex) | | Mesh / bridge state, comms bindings |

Every id is re-minted on import (`clone_workspace_with_id_mapping`), so importing the same
file twice, or a file built from a workspace you were once sent, never collides.

## 2. Roots, not directories

Tabs sit in subdirectories: `aiTerm/`, `aiTerm/src-tauri/`, `aiTerm/website/` are **one** repo.
Export resolves every local cwd (tab `auto_resume_cwd` ?? `restore_cwd`, service `cwd`,
editor file) to a **root**:

- In a git repo: root = `git rev-parse --show-toplevel`; the entry carries `subpath` relative
  to it. Root records **all** remotes (name → URL) and the checked-out branch.
- Not a repo, or a repo with no remote: a **plain root** — nothing to clone, nothing to match.

Root paths are stored home-relative (`~/DATA/IDE/aiTerm`) so an identical layout on the
receiving machine hits without a prompt. The user maps **roots**; tabs follow.

SSH tabs carry no local root. Their local `restore_cwd` is only where `ssh` was typed.

## 3. Export

Workspace context menu → **Share Workspace…**. The dialog lists what will be carried, with the
two optional toggles (notes, tasks) and the per-service stack checklist. Before writing, it
warns per root when the receiver would not get your code:

- the branch has unpushed commits, or no upstream at all;
- the root is a plain root (the receiver must supply the directory themselves).

Warnings, not blocks. Output: `<workspace-name>.maiterm-workspace` (§6).

## 4. Import wizard

Entry points: opening a `.maiterm-workspace` file from the OS (§7), or File › Import Workspace….

**Step 1 — resolve roots.** For each root, expand `~` and check the recorded path:

- exists **and matches** (§4.1) → mapped, no prompt;
- does not exist → needs a destination;
- exists and does **not** match → needs a destination (the recorded path is shown as rejected).

If any roots need a destination, offer **"Create all in…"**: pick one parent `X`; each unmapped
git root targets `X/<repo-name>` (repo name from the remote URL), then:

- `X/<repo-name>` absent → will clone;
- present and matches → use it, no clone;
- present and does not match → rejected **for that root only**; it falls back to a
  per-root prompt.

Per-root prompt: a directory picker that states which repo will be cloned into it. The chosen
directory is valid if it is **empty** (clone into it) or **matches** (use as-is). Anything else
is rejected with the reason. Plain roots always get a picker, with no clone and no match check.

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

- **Local agent tab** → a **fresh** agent of the recorded runtime, in the mapped cwd. The
  sender's local transcript is on the sender's disk; there is nothing to resume.
- **Remote agent tab** → a **fork** of the recorded remote session: `claude --resume <id>
  --fork-session`, `codex fork <id>` (`agent_runtime.rs` — `supports_fork`). Forking, never
  plain resume, so the sender and receiver never write to one session.

  The fork only works if the receiver's ssh login can read the sender's transcript — **the
  same remote user**, same remote cwd (Claude keys its project dir by cwd). When the receiver
  logs in as someone else, the resume fails ("No conversation found"). That is detected, and
  the tab falls back to a fresh agent in the remote cwd with a note; it never leaves a dead tab.

The session id travels in the file for remote tabs only. It is not a credential, but it names
a conversation on a shared host, which is why local ones are dropped.

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
(`tauri.conf.json`) — macOS `CFBundleDocumentTypes` + an exported UTI, Windows registry keys
via the NSIS installer, Linux a MIME type + the `.desktop` `MimeType=` (deb/rpm; AppImage does
not register, which is documented rather than worked around).

Delivery differs per OS, and each needs code:

- **macOS**: the path arrives as `RunEvent::Opened { urls }` on the **running** process, both
  on a cold launch and when already open. On a cold launch it arrives before any webview is
  ready, so it is queued in Rust and handed over when the first window's frontend asks.
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
