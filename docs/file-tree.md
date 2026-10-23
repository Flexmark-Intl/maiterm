# maiTerm Files: a project explorer that knows what the agents are doing

> Status: **design, 2026-10-22**. Nothing is built yet. Owner: Darryl. Implemented phase by
> phase (§17); each phase ships on its own.
> Scope: a window-level file tree (UI name **Files**, code module `filetree`) that follows the
> active tab's project. It is fast on 100k-file repos, network mounts, 70 tabs and ssh hosts,
> it costs 0% CPU while nothing changes on disk, and it shows what each agent tab is reading
> and changing as it happens, from evidence rather than guesses.
>
> Sources: three competing designs (perf, agent, ux) and three critiques, built on six research
> passes (the mod's source, the UI, Rust fs, ssh, cwd tracking, agent file data) that were checked
> against the repo at `baae9de4`.
> Every claim this doc leans on was re-checked there:
> - `+layout.svelte:912-944`: `isAppShortcut`, which includes Cmd+N and Cmd+Shift+N.
> - `state/workspace.rs:919-963`: `WindowData`.
> - `Cargo.toml:58`: notify pinned to `macos_fsevent`.
> - `pty/manager.rs:1395-1412`: lsof is spawned per call.
> - `claude_code/server.rs:3688-3698`: `PostToolUseFailure` is folded into ToolPost.
> - `claude_code/server.rs:2726, 2818`: the peer and own-tab tool lists.
> - `claude_code/lockfile.rs:540`: PreToolUse is an HTTP hook, so it blocks the tool.
> - `commands/overlord.rs:216-258`: `agent_dialog_open` (it covers the trust dialog) and `deliver_to_agent`.
> - `TerminalPane.svelte:1391-1398`: the drop path types into a Claude tab with no dialog check.
> - `lib.rs:270-274`: Rust already sees `WindowEvent::Focused`.
> - The repo has no `tauri::ipc::Channel` anywhere yet.
>
> **As built, phase 1 backend (2026-10-22, `src-tauri/src/filetree/`).** Where it differs from the
> text below:
> - **Repo info is read off the disk**, not from `git rev-parse`: the nearest `.git` at or above the
>   root (a folder, or a `gitdir:` file for worktrees/submodules, plus its `commondir`). The
>   subprocess cost ~20 ms on the path to the first rows. `ft_resolve_place`'s root still comes
>   from `share::git::toplevel`, cached.
> - **The watcher starts in the background.** FSEvents' `FSEventStreamStart` blocks for seconds
>   while fseventsd is busy (measured 1.5–4.5 s right after a 150k-file checkout). Until it runs,
>   snapshots aren't trusted and every expand reads the disk; once it runs, every open folder is
>   listed again. `caps.watch` reports what it will be ("live") meanwhile.
> - **No `Resync` event.** An overflowed channel or a backend rescan re-lists the affected open
>   folders in Rust and sends ordinary deltas, so the client never needs the round trip.
> - **A folder's own rename or delete re-lists every open folder below it**: FSEvents and inotify
>   report only the renamed path, never its descendants (the convergence test caught this).
> - **Resume sends a whole `dir` per changed folder**, not a collapsed delta: the hub keeps one
>   snapshot per folder, not the client's old one. A truncated folder (past 50k) is also always
>   resent whole. There is no `toolarge` code; the cap is `truncated`.
> - **A page reload is detected with `on_page_load` (Started)**: a Channel to a reloaded page
>   still "sends" without an error, so the spec's "failed send" signal never fires.
> - **NFC folding is macOS-only.** APFS/HFS+ treat NFC and NFD as one name; ext4 and NTFS keep
>   them as two files, where folding would match a path to a file it doesn't name.
> - **Linux polling can't see the viewport.** Polled folders are the over-budget open folders,
>   checked every 2 s while a subscription to the hub is unpaused in a focused window. On
>   `ENOSPC` or over budget the least recently opened folder (never the root) gives its watch up.
> - **Remote places resolve but don't open.** `ft_resolve_place` answers an ssh tab with
>   `kind: "remote"` and the remote cwd as root (no top level without the helper); `prompt_cwd`
>   is accepted but unused, since dropping a fish `~/D/I/m/x` needs the helper's existence check.
>   `ft_open` refuses a remote place until phase 6.
> - **The cold-open first chunk is sent before the rest is sorted** (select the first 512, sort
>   them, send, then sort the rest), only for a folder with no snapshot yet.
> - `file_tree_exclude` lines are gitignore patterns (no separate GlobSet); a loop-flagged link
>   keeps `k: "L"` with flag 4; `Health.git` is `"off"` and `StatInfo.numstat` null until phase 2.
> - **Budgets measured** (release, this machine, loaded by other work): root open of the
>   100k-file fixture 2–7 ms (budget 50); first chunk of a 50k folder 33–44 ms (budget 30) against
>   a bare `read_dir` of the same folder of 22–77 ms in the same runs, so the disk floor alone can
>   exceed the budget here. The ignored budget test asserts 30 ms or ≤8 ms above that floor.
>   Idle: zero coalescer wakeups over 10 s.
>
> **As built, phase 1 frontend (2026-10-22).** Where it differs from the text below:
> - **`Delta` carries `at`**: the position of each `add` then `chg` entry in the folder after
>   the delta. The add list alone doesn't say where a new name goes, and the client must not
>   sort (§12.1). The convergence test's client keeps wire order and applies `at`.
> - **The model returns `Splice[] | 'resync'`** for listings and deltas as specified, but
>   expand/collapse return what Rust must follow or drop (`open: DirReq[]` / `close`), and a
>   delta that deletes an open folder queues it on `pendingClose`. Collapsing a folder drops it
>   and its open children from Rust's interest but keeps them open in the view, so reopening
>   the parent restores them, asked with their generations.
> - **Column width and open** live on `workspacesStore` (read with the window before the first
>   render, so terminals spawn at their final width), not on the tree store.
> - **Preview tabs** go through `openInEditor({…, replaceTabId})`; EditorPane no longer takes
>   focus while focus is in `[data-filetree]`, and a real open hands focus to the editor.
> - **"Open With Default App"** needed a new command, `open_with_default_app`.
> - **Alt+→ skips ignored folders** (node_modules, target): opening them is never the point of
>   a recursive expand, and they would spend the 2,000-row budget.
> - **"Outside root · Show"** pins the editor file's folder (there is no resolve-by-path
>   command); **"Reveal in Files" on a terminal tab** brings that tab to the front so the tree
>   follows it, then selects its folder.
> - **Help** is the help window (`routes/help`), not a `HelpModal.svelte`; the shortcut row and
>   a "Files" key section are there.
> - **The titlebar button** sits at the titlebar's right end: the left holds the macOS traffic
>   lights (overlay title bar).
> - Not in phase 1 by design: the find/filter bar (phase 2; Cmd+F and `/` do nothing in the
>   tree yet), New File/Folder buttons and every write (phase 3), the agent strip, lenses,
>   edge chips and follow-agent (phase 4).
>
> **As built, phase 1 fix: the active-file highlight (2026-10-22).** The dev check found the
> `active-file` row (and the ⋯ menu's "Reveal Active File") going stale:
> - **The highlight follows the active tab's FILE, not root resolves.** It was refreshed only by
>   resolves keyed on the tab id, so a preview swapping its file (same id) kept the old row lit,
>   and a terminal tab in front never cleared it. Now `filetree/activeFile.ts` derives the target
>   from the tab's type and path and `FilesColumn` re-places it on every change of that path or
>   of the subscription (`syncActiveFile`); no target clears at once, and a lookup for an earlier
>   target is dropped. Resolves no longer touch it.
> - **Diff tabs highlight their file too**, but never offer "Outside root · Show": a diff's path
>   may be an ssh agent's file, whose folder doesn't exist here.
> - **A double-click on a menu item no longer falls through.** The ⋯ menu is not inside the tree
>   (no event propagates to a row), but an item acts on `click` and the menu unmounts, so a
>   double-click's second press hit the row underneath: selected, opened, focused.
>   `ContextMenu` now swallows presses with `detail > 1` for 600 ms after an item acts, for
>   every menu in the app.
>
> **As built, phase 2: git and find (2026-10-22, `filetree/{git,search}.rs`, `FindBar.svelte`,
> `BranchRow.svelte`).** Where it differs from the text below:
> - **OSC 133 D is raised in Rust**, in the PTY reader where `term-osc133-<pty>` comes from, not
>   sent back by the frontend: no IPC round trip, and Rust has the shell's live cwd
>   (`proc_pidinfo`). `FileTreeState::command_done(pid)` only queues the pid (nothing at all
>   while no repository hub exists); one thread resolves the cwd and signals every hub whose
>   root, or whose repository's top level, contains it. The ToolPost trigger waits for phase 4.
> - **Git signals need watches the root's watcher may not have.** The repository's git dir,
>   common dir and `refs/heads` get NonRecursive watches of their own unless the root's watcher
>   is recursive and they are inside the root (a root inside a larger repo, a linked worktree,
>   Linux, shallow roots). Lock files are noise: git renames `index.lock` onto `index`.
> - **Scheduling details:** a trailing delay never pushes a run more than 1 s past the first
>   signal (a build writing without pause still decorates); run starts are 300 ms apart; a run
>   nobody can see (every subscription paused or stalled) is deferred to the next resume; the
>   watcher starting runs git once (changes made before it ran were never reported). Network
>   mounts refresh on edges only, like slow repos. git missing or `safe.directory` refusing the
>   repo stops automatic runs until Refresh, with the reason in Health.
> - **A status run that changes nothing sends nothing** (not one empty message). A shallow
>   root decorates only when it is itself a repository's top level.
> - **Wire additions:** `GitUpdate.count` (the branch row's "N changed", also past the 20k cap);
>   a wholly untracked folder's key keeps git's trailing slash (`build/`) so the client can mark
>   what is inside it; `TreeEvent::SearchReady` (an index finished building: re-ask a query
>   answered `indexing`); `SearchResult.unavailable` plus an `index_shallow` argument on
>   `ft_search` for "Index this folder" on a home or volume root; `Health.git` is `off` | `live`
>   | `slow` | `error`, its note joined to the watch note. The TS types are `GitUpdate`,
>   `TreeSearchResult`, `TreeSearchHit` (a terminal-search `SearchResult` already existed).
> - **The index** is rebuilt in the background, the old one still answering, when a folder
>   appears or goes, two paths share a hash, or a batch changes more than 4,096 paths; its
>   eviction is the building thread's timed wait (one wakeup per 5 min while an index exists).
>   A walk stops at 1M paths. `ls-files -co` lists a conflicted path once per stage: deduped.
> - **Hover numstat** runs only for a file with a letter other than `?` (a clean file spawns
>   nothing), inside the same two git slots, cached until a status run changes anything.
> - **Pruned trees** (find results, the Changed lens) sort siblings in JS, folders first then
>   case-insensitive: these paths are not all listed, so there is no Rust order to keep. They
>   have their own open/closed set, which is why Escape restores the real tree exactly. With
>   hidden files off, a path with a dot segment is left out of them. The best hit takes focus
>   (not selection); Enter opens it pinned.
> - **The Changed lens** is toggled from the branch row only: the Lens bar arrives with the agent
>   lenses (phase 4). `openGitDiff({subId, abs}, rel, ref)` takes the subscription rather than a
>   place, and `ref` may be `'index'`; `openReadOnlyDiff` is the read-only flow it shares with
>   the agent's `showDiff`.
> - **`file_tree_git`** (§16, default on) is a Preferences toggle and applies to open trees at
>   once: off clears every view's decorations.
> - **Budgets measured** (release, this machine): a keystroke over 100k generated paths, worst of
>   four queries, 17.6 ms (budget 30). Not yet checked in the running app: the dev check (a
>   commit typed in a terminal decorates in about 150 ms).
>
> **As built, phase 2 review fixes (2026-10-22).** Where they changed the above:
> - **A status run carries the `file_tree_git` epoch it started in**, and `apply` drops a run
>   that outlived a change (checked under the data lock): a run still going when decorations
>   were turned off used to publish onto the cleared data. The slow-repo clock starts once a
>   git slot is held, so waiting behind other repos' runs never trips slow mode.
> - **A root inside a wholly untracked folder** (git reports only `? new/sub/` or `? new/`)
>   lists its own untracked top level with `ls-files -o --directory --no-empty-directory`,
>   so it decorates like any other root.
> - **NFC on macOS.** Git keys and the root's prefix are NFC whatever
>   `core.precomposeunicode` says, numstat looks the row up the same way, and
>   `OpenAck.nfc_names` tells the model to look a non-ASCII row (an on-disk NFD name) up by
>   its NFC spelling. The find index keeps every path NFC there too, so the watcher's NFD name
>   and `ls-files`' NFC one are one entry, and hits come back NFC (they open fine on APFS).
> - **git runs with `LC_ALL=C`** in the tree: `diff_base` and the `safe.directory` check read
>   stderr.
> - **The index lists only what is on disk**: `ls-files -cdo -t` drops the `R` (deleted from
>   the worktree) paths `-c` still lists. Git's deletions reconcile as files, so one the
>   watcher already removed costs no rebuild, and an index-only deletion (`git rm --cached`,
>   also listed untracked) is not reconciled as gone.
> - **Stale queries are per subscription** (`ft_search`'s `seq` counts per window), so two
>   windows on one root never cancel each other's scans. **Match positions are code points**:
>   nucleo is fed one slot per code point, not its grapheme (or byte) `Utf32Str::new`.
> - **Pruned views:** the parent guide and ArrowLeft work on the rows on screen
>   (`parentRowIndex`). Toggling the Changed lens while find has a query moves the saved
>   views with it, so Escape restores the right one. "Reveal in Files" under find or the lens
>   puts both away first; an automatic reveal there only scrolls to the row if the lens shows
>   it, and does not open folders in the hidden tree.
> - **Compare with HEAD** diffs against an empty right side only when `ft_stat` says the file
>   is gone; a file still on disk that `readFile` refuses (over 2 MB, not UTF-8) is an error.
>
> **As built, phase 2 second review fixes (2026-10-22).** Where they changed the above:
> - **A status run reconciles Find by looking on disk**, not by git's letters: every file the
>   run names (each changed file, and a rename's source) is checked with a stat under the lock
>   the watcher's own changes take, and the index follows what is there. Letters alone kept
>   dropping files still on disk after `git rm --cached -r dir` (git lists the folder as one
>   `? dir/`), missed a staged rename's two sides (Linux's only signal for a collapsed
>   folder), and re-inserted a file made and removed during the run. A run naming over 20k
>   files marks the index dirty instead. `Published` carries one `named` list.
> - **An untracked root's contents come from two git runs** with fixed globs (`:(glob)*` for
>   top-level files; `:(glob)*/**` with `--directory --no-empty-directory` for top-level
>   folders), each one pass over the folder. A pathspec per name was quadratic (seconds at
>   20k entries, enough to trip slow mode) and could overrun Windows' command line. A failed
>   run keeps git's own entry and logs.
> - **Hover counts** are cached per (rel, git gen, the file's size and mtime): a file that
>   stays `M` while it is edited bumps no gen. A staged rename is diffed with `-M` and its
>   source named (`:(top,literal)`), so it counts its edits rather than its size; the record
>   for the file is picked by path.
> - **Find leaves out `S` (skip-worktree) entries**: outside a sparse checkout's cone, in the
>   index, not on disk, and never reported deleted.
> - **Builds carry a generation**: starting one wakes the previous build's thread, which exits,
>   so only the newest stays to evict.
> - **The focus shortcut keeps find and the lens** (`revealPlan`): Cmd/Ctrl+Shift+B with an
>   editor in front selects the file's row if the pruned view shows it and otherwise only
>   focuses. Only "Reveal in Files" puts the pruned view away.
>
> **As built, phase 2 third review fixes (2026-10-23).** Where they changed the above:
> - **The untracked root's file run stops at its top level.** `:(glob)*` alone still walks
>   into every folder (git matches the folder's name and descends), so it visited every file
>   under the root on every status run: an unignored `node_modules` meant slow mode or a
>   timeout. It now also passes the exclude pathspec `:(exclude,glob)*/**`, which git prunes
>   on, and a test reads git's trace2 counters to hold that. (This code landed inside
>   2e811a04, another session's commit that swept up the staged change.)
> - **Find keeps an `S` entry that is on disk.** `git update-index --skip-worktree` (hiding
>   local edits to `.env` and the like) sets the same bit as a sparse cone's exclusion, and
>   status never names such a file, so dropping every `S` lost it for good. An `S` path is
>   listed when a lookup finds it; folder answers are memoized, so a cone's missing folders
>   cost one lookup each.
> - **A case-only rename leaves one name.** On a volume that folds case (APFS, NTFS) a stat
>   of `Foo.ts` finds `foo.ts`, so reconcile and the watcher re-indexed the old name.
>   `paths::spelled_on_disk` compares the stored spelling (canonicalizing answers it per
>   component; a symlink's own name comes from its folder's listing). The watcher asks for
>   every file it finds on such a volume; reconcile asks only for names that collide once
>   case is folded, which is exactly a rename's two sides, so ordinary runs cost nothing extra.
>
> **As built, phase 3 backend (2026-10-23, `filetree/ops.rs`, `util/naming.rs`).** The UI half
> (InlineName, ConfirmStrip, dnd, the undo stack, editor retargeting) is not in it. Where it
> differs from the text below:
> - **Wire additions:** `FileOp.to` (exact destination rels, one per `src`, for a move: a
>   move's undo puts each item back where it was under the name it had, since a multi-select
>   spans folders and keep-both renames), `FileOp.trashed_at` (unix seconds, for restore's
>   match by time), `OpResult.skipped` (`conflict: "skip"`, or already in place), and
>   `OpFail.code` `inside_itself`. The TS type for `Needs` is `OpNeeds`.
> - **Whole-op refusals are the command's error**, before anything changes: a rel that could
>   step out, the root as an item, a link on the way that resolves outside (a dangling one
>   too), a bad ticket, an import source that isn't absolute. Per-item outcomes are
>   `failed`; a guard the human answers is `needs` with nothing done.
> - **Never clobber is the OS's refusal**, not a check before the rename: `renamex_np`
>   `RENAME_EXCL` (macOS), raw `renameat2` `RENAME_NOREPLACE` (Linux), `MoveFileExW` without
>   `REPLACE_EXISTING` (Windows); only a volume that can't falls back to check-then-rename.
>   A rename that names the same file on this volume (case or, on macOS, NFC/NFD only) hops
>   through a temporary name. Cross-volume moves verify file and byte counts.
> - **Undo of a create, copy, duplicate, import or restore is a `trash`** of what it made (a
>   create's outermost new folder). Trash's undo is `restore` on Linux/Windows, none on macOS.
> - **macOS trashes through NSFileManager**, not the crate's default Finder script: no
>   automation prompt or sound, and Finder needn't answer. Put Back is lost on some systems
>   either way. Tests drive a stand-in `TrashBackend`, so they never fill the real Trash.
> - **The delete ticket is checked before the guards and used up after them**, so a resend
>   with `ack_agents` after the second confirmation still holds a valid ticket.
> - **The live-cwd guard covers permanent delete too**, and reads holders only when a real
>   folder (not a link to one) is moved, renamed, trashed or deleted: each live terminal's
>   shell cwd (`cwd_of_pid` at that moment, not `get_pty_info`, which also sweeps the process
>   table for the foreground command), each agent session's cwd in a live tab that rides no
>   ssh tunnel (an ssh agent's cwd names a remote folder), and each service the stack mirror
>   reports starting/running/ready, by its `Service.cwd`.
> - **`agent_active` is a stub** that names no tab until phase 4's ledger exists, so
>   `ack_agents` is accepted and changes nothing yet.
> - **Windows-illegal names are refused in Rust on Windows** (a reserved name would open the
>   device); the frontend's wider check on case-folding disks stays a frontend rule.
> - **`ft_op` re-lists the open folders an op touched** (`RootHub::touched`), so the result
>   arrives with the answer rather than after the watcher's window, and at all on a network
>   mount. `Caps.trash` and `Caps.writable` are now true on every local root.
> - **Not tested here:** Windows (the verbatim/UNC and reserved-name tests are `cfg(windows)`),
>   and Linux's real tmpfs move runs only where `/dev/shm` is another volume; elsewhere the
>   cross-device fallback is tested through an injected rename that answers `EXDEV`.

## 1. Why

maiTerm is where agents work on projects, and the human has no view of the project itself. The
Claude Code "filetree" mod (data-goblin/claude-code-filetree v0.4.1) shows that a tree with live
agent activity is worth having. It is also limited by the mod API:
- no watcher: a 2 s poll of `.git/index` mtimes;
- it guesses touched files by tokenising Bash;
- it is read-only, with no multi-select and no file operations;
- it is local only, and only one agent's view.

maiTerm does not have those limits. It has a native watcher, exact tool events per tab from
hooks (`server.rs:4719`, `4833`, currently thrown away by `agentState.svelte.ts:498-551`), its
own editor and diff tabs, the ssh ControlMaster, and every agent tab in the window.

Three properties are non-negotiable, and most decisions below follow from them:
1. **Idle is free.** Nothing polls on a local disk. The cost scales with distinct roots on screen,
   never with the number of tabs.
2. **The human owns the view.** Nothing opens, closes, resizes, scrolls or expands the tree except
   the human. That is partly a UX rule, and partly because any width change on a streaming PTY
   makes Claude duplicate its scrollback (`terminal/CLAUDE.md:49`).
3. **Evidence, labelled.** Agent activity comes from tool events and the watcher, and each mark
   says which kind of evidence it has. A missing mark is never read as "the human did it" (the
   absence-read-as-a-claim class).

## 2. Shape

```
Window
 └─ Files column (one per window, left, between .sidebar-wrapper and <main>)
      fileTree store ── Channel<Envelope> per subscription ──┐
                                                            │
Rust  FileTreeState                                          │
 ├─ RootHub per place  (refcounted across windows, 30 s grace after last sub)
 │    ├─ dir snapshots + per-dir generation   (only subscribed dirs)
 │    ├─ watcher (per-OS, §6) → coalescer → Delta / Resync
 │    ├─ GitWorker (one run in flight + one queued, global semaphore 2)
 │    ├─ SearchIndex (lazy, evicted after 5 min idle)
 │    └─ remote backend (phase 6): one Python helper per host_key
 ├─ AgentFileLedger  ← hooks_handler ToolPre/ToolPost/Prompt/Stop
 │    └─ marks routed to the hubs whose root contains the path
 └─ Baselines (phase 5) ← first write per (tab, turn, path) at PreToolUse
MCP: revealInTree · getTreeSelection · getAgentFileActivity · showDiff ref "@turn"/"@session"
```

There are no new `Tab` fields, no persisted enums and no maiLink wire change. `protocolVersion` stays **0.23**.

## 3. Placement and anatomy

### 3.1 The column

**Decision:** a window-level column between `.sidebar-wrapper` and `<main class="main-content">`
(`+page.svelte:546-579`), on the left, one instance per window. It follows the window's active tab.

Rejected alternatives:
- **Per-pane right docks beside Notes/Tasks:** N trees in a split layout, and every toggle reflows each split.
- **A section inside WorkspaceSidebar:** 215–400 px tall, fights the workspace list, and Cmd+B hides it.
- **An overlay over the terminal area:** `docs/loom.md` records that terminal-area overlays were tried and abandoned. Overlay mode is not built, not even as an option.

**Width rules** (the scrollback-duplication constraint):
- **The column never opens or closes by itself.** Not on reveal, not on an MCP call, not on a
  workspace switch, not on first run. Only the human's toggle changes it.
- **Resize uses a ghost line.** While dragging, a 2 px `--accent` guide follows the pointer, and
  the column and terminals do not move. On `pointerup` the width is committed once, and saved once.
- There is no CSS `transition` on width, and open/close is instant.
- The width is clamped to 200–560 px, default 280.

**Persistence.** On `WindowData`, next to `sidebar_width` (`workspace.rs:931-934`):
- `file_tree_open: bool`, `#[serde(default)]`
- `file_tree_width: u32`, `#[serde(default = "default_file_tree_width")]` = 280

The setters are `set_file_tree_open` and `set_file_tree_width` in `commands/workspace.rs`, modelled
on `set_sidebar_width` (line 993). Duplicate window copies both. These are per-window layout
settings, so they create no Tab reload, duplicate or share obligations.

**Toggle surfaces:**
- **Cmd/Ctrl+Shift+B** (§11).
- A Files `IconButton` in the titlebar next to the sidebar expander, with a `Tooltip`. Never `title=`.
- "Reveal in Files" in the terminal and editor tab context menus (`TerminalTabs.svelte`). It reveals,
  and opens the column only because the human clicked it.

The toggle does NOT go in each pane's tab strip next to Notes/Tasks (`TerminalTabs.svelte:1470-1487`).
Those buttons are per tab and Files is per window, so placing it there would suggest the wrong scope.

### 3.2 Anatomy, top to bottom

1. **Header (28 px).**
   - The root name in bold, plus a host chip on a remote root (`ews@nova`).
   - Clicking the name opens the **root switcher**: the deduped roots of every non-service terminal
     tab in the workspace, each with its tab names and agent dots, plus Pinned, Recent (5), and
     "Open Folder…" (native dialog, which pins).
   - Right-side buttons: New File, New Folder, Refresh, Collapse All, Pin, and "⋯". The "⋯" menu
     holds show hidden, show ignored, follow tab, follow agent, reveal active file, and settings.
     Every button has a `Tooltip`.
2. **Branch row (20 px, only inside a git repo).**
   - Shows `⎇ main ↑2 ↓0 · 7 changed`, and the short SHA on a detached HEAD.
   - Clicking "7 changed" toggles the Changed scope (§9.4).
   - Clicking the branch name copies it.
3. **Agent strip (only when an agent tab's cwd is inside the root).**
   - One chip per agent tab, in that tab's stable agent colour (§9.3).
   - Each chip shows a state dot (active/idle/waiting from `agentState`) and the number of files changed this turn.
   - Clicking a chip scopes the Turn/Session lenses to that tab.
4. **Lens bar** (shown once the repo or an agent gives it something to show): `All · Changed · Turn · Session`.
5. **Find bar.** Hidden until `/` or Cmd/Ctrl+F while the tree has focus (§8).
6. **Sticky ancestors** (0–`file_tree_sticky_max` rows), then the **virtual tree**.
7. **Edge chips.** Overlaid on the top or bottom edge of the tree body when lit rows are scrolled out of view
   (`↑ 3 active`, `↓ 1 editing`). Clicking one scrolls to the row.
8. **Footer status line.** Drawn **only when there is something to say**:
   - degraded health (`watch: polling 12 folders (inotify limit)`, `git: slow repo, refresh on demand`, `network mount: not watched`, `ssh: needs key auth`);
   - `3 selected`;
   - an op result with Undo (`Moved 4 items · Undo`);
   - the inline ConfirmStrip (§10.4).

   An absent footer means healthy. The footer is never a permanent "all good" badge.

**Rows** are 22 px; density gives 20/22/24. Left to right:
- indent guides (12 px step; the focused row's parent guide is highlighted in `--accent` at 50%);
- the chevron;
- the icon;
- the name, which ellipsizes;
- a right gutter, in this order: the agent badge, the editor dot (open, or filled when dirty, from
  `editorRegistry.dirtyTabs`), then the git letter.

A 2 px left-edge **agent rail** sits under the indent (§9.3). The cwd folder carries a
**you-are-here** tick in the gutter.

**Tooltips.** One delegated `TooltipBubble` (`Tooltip.svelte`) is anchored to the hovered row
after 400 ms. It appears only when one of these is true:
- the label is clipped (`scrollWidth > clientWidth`);
- the row has decorations that need words ("Modified · +12 −3 · written by claude (tab 3) this turn");
- the human hovers with Alt held.

Its contents come from one `ft_stat` (§5.2). That covers the "no `title=`" rule and the
"nowrap needs a tooltip" rule with a single bubble.

**Icons.**
- An inline SVG sprite (`<symbol>` plus `<use href>`) of about 45 glyphs, in `src/lib/filetree/icons.ts`.
- Lookup is by exact filename first, then by extension.
- Colours are theme tokens mixed toward `--fg` with `color-mix(in srgb, var(--x) 80%, var(--fg))`,
  so QuickOpen's near-invisible json `#292929` cannot happen.
- `Icon.svelte` gains `folder`, `folder-open`, `chevron`, `new-file`, `new-folder`, `collapse-all` and `refresh`.
- There is no third-party icon theme and no Nerd Fonts.

**Accessibility.** A visually hidden `aria-live="polite"` region announces op results ("Moved 3
items"). With `prefers-reduced-motion`, every animation becomes a static tint.

## 4. Root resolution and follow policy

### 4.1 One resolver, in Rust

```rust
#[tauri::command]
async fn ft_resolve_place(state, tab_id: String, hints: PlaceHints) -> Result<TreePlace, String>;

#[derive(Deserialize)]
pub struct PlaceHints {
    pub osc_cwd: Option<String>,    // terminalsStore osc.cwd (Rust doesn't keep it)
    pub osc_host: Option<String>,   // osc.cwdHost
    pub prompt_cwd: Option<String>, // osc.promptCwd, derived from the OSC 0 title by frontend patterns
    pub fresh: bool,                // bypass the 800 ms ps cache (manager.rs:1059-1065) on ssh edges
}

#[derive(Serialize)]
pub struct TreePlace {
    pub place: String,              // watch::place form: "/abs" or "user@host:/abs" (the hub key)
    pub kind: String,               // "local" | "remote"   (String, never an enum)
    pub host_key: Option<String>,   // remote: tunnel host_key, already watch::connect_args-stripped
    pub host_label: Option<String>,
    pub root: String,               // native abs (local) / POSIX abs (remote)
    pub cwd_rel: Option<String>,    // you-are-here, root-relative POSIX; None if cwd == root
    pub source: String,             // "pty"|"osc7"|"agent"|"last_cwd"|"auto_resume"|"restore"|"prompt"|"pin"
    pub fs_class: String,           // "local"|"network"|"unknown"
    pub shallow: bool,              // §6.5
    pub missing: Option<String>,    // a saved folder that no longer exists, NEVER replaced by $HOME
}
```

**Why Rust, plus hints from the frontend.** Every edge then costs one IPC round trip, and the chain
is defined in one place for every caller. Two of the signals, `promptCwd` and OSC 7, are kept only
in `terminalsStore` (`terminals.svelte.ts:298-309`), so the frontend passes them in as hints.

The precedence encodes Move project's proven chain (`relocate.svelte.ts:62-92, 226-247`). A
candidate counts only if it exists.

**Local tab** (no ssh foreground):
1. The live shell cwd (`get_pty_info`; `foreground_command` is null).
2. The non-stale hint `osc_cwd`.
3. `AgentSessionInfo.cwd` (`app_state.rs:134`).
4. `expand_home(last_cwd)`.
5. `auto_resume_cwd`.
6. `restore_cwd`.

**ssh tab** (`foreground_command` set):
1. `AgentSessionInfo.cwd`. This is the remote project, and the best remote signal.
2. `osc_cwd`, if not stale. Stale means it equals the local shell cwd, the house test (`workspaces.svelte.ts:1029-1036`).
3. `prompt_cwd`, only if it is absolute or `~/`-prefixed and it exists through the helper. A fish
   `prompt_pwd` abbreviation (`~/D/I/m/x`) fails the existence check and is dropped.
4. `auto_resume_remote_cwd`.
5. `restore_remote_cwd`.

`last_cwd` is **never** used for an ssh tab (`share/mod.rs:708-732`).

**Root and edge cases:**
- **Root** = git toplevel of the cwd (`share::git::toplevel`, through `util/proc.rs`), else the cwd.
  Results are cached per directory in an LRU of 256 entries, invalidated by a hub's `.git` watcher.
  Remote roots are resolved by the helper's `toplevel` op.
- **Missing folder.** A tab whose saved folder is gone resolves to `missing`. The tree shows "Folder
  missing: ~/x/y" with a Move project link and never adopts `$HOME` (relocate.md §4).
- **Source tab.** It is the active pane's active tab, if that is a terminal tab with no `service_id`.
  Stack service tabs are never "the active tab" (stack.md §7). "Live" is `terminalsStore.get(tabId)`,
  never `tab.pty_id`.
- **Suspended or not-loaded tabs** resolve through the saved chain alone.
- **Windows.** PowerShell's `Set-Location` doesn't change the process cwd, and pwsh/cmd have no OSC
  integration. So on Windows the chain usually lands on the agent cwd or a saved cwd, `source` says
  so, and the first time follow lands on home the empty state suggests Pin.

### 4.2 Follow: coarse and driven by events

- **When to re-resolve.** Only on:
  - tab activation;
  - OSC 133 A (prompt drawn, `TerminalPane.svelte:789`);
  - an OSC 7 or title change;
  - `agent-hook-session-start` for the source tab.

  These are debounced at 150 ms, with `fresh: true` on ssh start/exit edges. **Never on a timer.**
- **Inside the root, reveal; outside, re-root.** If the new cwd is under the current root, only the
  you-are-here tick moves. It is revealed (ancestors expanded, scrolled into view only if
  off-screen) and is **never selected**, because the selection belongs to the human. The tree
  re-roots only when the cwd leaves the root. So `cd src/lib` never reloads anything, and while an
  agent runs the shell can't `cd`, so the root is stable for free.
- **Editor and diff tabs never re-root.** With `file_tree_auto_reveal` on, the active editor's file
  is revealed if it is under the root (expand plus scroll-if-off-screen, no focus). If it is
  outside the root, the header shows a muted "Outside root · Show" link and does not jump.
- **Pin.**
  - The header pin freezes the root for the workspace: `Workspace.file_tree_pin: Option<String>`, in `watch::place` form. It is a String, so a downgrade can't wipe state.
  - "Set as Root" in the context menu and the root switcher also pin.
  - Unpinning resumes following.
- **Switching roots is instant.** The store keeps view state (expanded set, scroll top, selection,
  and the TreeModel with its per-folder gens) for the last 8 roots. A hub stays alive 30 s after
  its last subscriber closes. Coming back sends `known_gen` per folder and receives `NotModified`
  or `Delta` (§5.3), never a full re-list.

### 4.3 Prerequisite fixes (phase 0)

- **macOS cwd lookup.** `get_cwd_for_pid` spawns `lsof` per call (`manager.rs:1395-1412`, tens of
  ms). Replace it with `libc::proc_pidinfo(pid, PROC_PIDVNODEPATHINFO, …)`, which takes
  microseconds and spawns nothing. Re-resolving on every prompt across 70 tabs needs this. Linux
  (`/proc`) and Windows (sysinfo) are unchanged.
- **OSC 7 decoding.** `osc.rs:201-215` `percent_decode` pushes each byte as a `char`, so a UTF-8
  folder name decodes wrongly. Decode into bytes, then `String::from_utf8_lossy`.
- **Agent cwd.** `agentState.svelte.ts:365` drops the SessionStart `cwd`. Keep it, so the tree can
  tell agent tabs in this root (the agent strip) without an IPC call per tab.

## 5. Wire contract, Rust commands, events

### 5.1 Paths on the wire

**Decision: every path between Rust and the frontend is ROOT-RELATIVE POSIX** (`src/lib/x.ts`,
`""` for the root), keyed by the hub's `place`. This is the contract for every `ft_*` command and
every event.

Rust alone maps to native paths, in `filetree/paths.rs`:
- join with the native separator;
- `share::canon` for the hub root (it strips `\\?\` except on UNC);
- strip the canonical root from event paths, which also fixes macOS `/tmp` vs `/private/tmp`;
- fold case only where `relocate::case_insensitive` says the disk folds;
- normalise to NFC before comparing hook paths, because APFS keeps NFD names. This adds the
  `unicode-normalization` crate (not in the lockfile today).

This keeps `\\?\`, separators and drive-letter case out of the frontend. It makes local and
remote trees identical, and it makes the containment guard (§10.1) trivial.

The frontend needs native paths in exactly two places, and each has one helper in
`src/lib/filetree/paths.ts`:
- **rel → abs**: `relToAbs(ack, rel)` uses `OpenAck.root_native` and `OpenAck.sep`. This is pure
  string work and can't go wrong.
- **abs → rel**: revealing an editor's file or a hook path goes through **Rust** (`ft_locate`),
  because it needs canonicalisation and case folding.

`paths.ts` replaces the tree's use of the `startsWith('/')` and `split('/')` habits in
`openFile.ts:14-78` and `claudeCode.svelte.ts:545-570`.

### 5.2 Commands

Every command is `async`, does its work in `tauri::async_runtime::spawn_blocking` (or on tokio
processes), and returns `Result<T, String>`. This follows the pinwheel rule: `list_files` and
`git_show_file` are counter-examples, never templates. Each command is registered in `lib.rs`
`generate_handler!`, typed in `tauri/types.ts`, and wrapped in `tauri/commands.ts`.

```rust
// filetree/commands.rs
ft_resolve_place(tab_id: String, hints: PlaceHints) -> TreePlace                         // §4.1
ft_open(window: tauri::Window, place: String, restore: Vec<DirReq>,
        on_event: tauri::ipc::Channel<Envelope>) -> OpenAck
ft_close(sub_id: u64) -> ()
ft_pause(sub_id: u64, paused: bool) -> ()                    // document.visibilitychange
ft_ack(sub_id: u64, seq: u64) -> ()                          // §5.4 backpressure
ft_expand(sub_id: u64, dirs: Vec<DirReq>) -> ()              // results stream as Dir / NotModified
ft_collapse(sub_id: u64, dirs: Vec<String>) -> ()            // drops interest (Linux/remote watches)
ft_refresh(sub_id: u64, rel: Option<String>) -> ()           // None = every subscribed dir + git
ft_locate(sub_id: u64, abs_paths: Vec<String>) -> Vec<Option<String>>   // abs → rel, None = outside
ft_stat(sub_id: u64, rel: String) -> StatInfo                // hover only
ft_search(sub_id: u64, query: String, limit: u32, seq: u64) -> SearchResult      // phase 2
ft_diff_base(sub_id: u64, rel: String, base: String /*"HEAD"|"index"*/) -> Option<String> // phase 2
ft_op(sub_id: u64, op: FileOp) -> OpResult                   // phase 3
ft_prepare_delete(sub_id: u64, rels: Vec<String>) -> DeleteTicket                 // phase 3
agent_files_activity(tab_id: String, scope: String /*"turn"|"session"*/, include_reads: bool)
        -> Vec<LedgerEntry>                                  // phase 4
agent_files_marks(window: tauri::Window) -> Vec<AgentMarkAbs> // phase 4: priming on mount
agent_files_baseline(tab_id: String, path_abs: String, scope: String) -> Baseline // phase 5
set_file_tree_open(window, open: bool) -> ();  set_file_tree_width(window, width: u32) -> ()
set_workspace_file_tree_pin(workspace_id: String, place: Option<String>) -> ()   // save_state
```

```rust
pub struct DirReq { pub rel: String, pub known_gen: Option<u64> }
pub struct OpenAck { pub sub_id: u64, pub root_native: String, pub sep: String,
    pub case_insensitive: bool, pub repo: Option<RepoInfo>, pub caps: Caps }
pub struct RepoInfo { pub toplevel_rel: String, pub git_dir: String, pub common_dir: String }
pub struct Caps { pub watch: String /*"live"|"partial"|"poll"|"none"*/, pub git: bool,
    pub trash: bool, pub remote: bool, pub writable: bool }
pub struct StatInfo { pub size: u64, pub mtime_ms: i64, pub exec: bool,
    pub link_target: Option<String>, pub numstat: Option<(u32, u32)> /*git diff HEAD, this file only, cached per git gen*/ }
pub struct SearchResult { pub seq: u64, pub indexing: bool, pub items: Vec<SearchHit> }
pub struct SearchHit { pub rel: String, pub score: u32, pub idx: Vec<u32> }
```

`ft_open` restores expansion in the same call (`restore`), so a returning view costs one round trip.

**Subscription lifecycle:**
- A subscription records its window label from the `tauri::Window` argument.
- A failed `Channel::send` (the webview reloaded) or `WindowEvent::Destroyed` drops the subscription.
- Closing the column calls `ft_close`, not pause. The hub's 30 s grace and the frontend's gens make a reopen cheap.

### 5.3 Events: one Channel per subscription, generation-checked

`Channel` is new to the repo. It is used because `emit` on a window, on the app, and `emit_dual`
all broadcast to every webview (`server.rs:91-94`). A Channel reaches exactly one webview, in order.

```rust
#[derive(Serialize)] pub struct Envelope { pub seq: u64, #[serde(flatten)] pub ev: TreeEvent }

#[derive(Serialize)] #[serde(tag = "t", rename_all = "snake_case")]   // wire-only, never persisted
pub enum TreeEvent {
    Dir { rel: String, gen: u64, chunk: u32, last: bool, total: u32, truncated: bool, entries: Vec<Entry> },
    NotModified { rel: String, gen: u64 },
    Delta { rel: String, from: u64, gen: u64, add: Vec<Entry>, del: Vec<String>, chg: Vec<Entry> },
    DirError { rel: String, code: String /*"denied"|"gone"|"timeout"|"toolarge"*/, msg: String },
    Git(GitUpdate),
    Agent { marks: Vec<AgentMark> },
    Health { watch: String, git: String, note: Option<String> },
    Resync { rel: Option<String> },        // client must ft_refresh(rel)
    RootGone,
}
#[derive(Serialize)] pub struct Entry { pub n: String, pub k: String /*"f"|"d"|"l"|"L"|"o"*/, pub f: u8 }
// k: file, dir, link (to file/broken), Link-to-dir, other. f bits: 1 ignored, 2 hidden, 4 link-loop, 8 unreadable.
```

`TreeEvent` is a serde enum, and that is allowed here: it is never persisted, so the downgrade-wipe
rule (memory: serde-enum-variant-downgrade-wipe) doesn't apply. The short keys matter: 50k entries
come to about 1.2 MB instead of about 3 MB.

**Convergence protocol:**
- Every subscribed folder has a monotonically increasing `gen`.
- A `Delta` is applied only when the client holds `gen == from`. Otherwise the client calls
  `ft_refresh(rel)` and receives a full `Dir`.
- So a missed message, a reordered chunk or a resubscribe race heals itself instead of drifting.
- `Dir` streams in 512-entry chunks. The first chunk of a 50k-entry folder arrives within 30 ms,
  and the list paints as chunks land.
- The hard cap is **50,000 entries per folder**. Past it, `truncated` is set and a final row reads
  "50,000 of N shown · search to find more".

### 5.4 Backpressure: never push into a stalled webview

An occluded WKWebView throttles while Rust keeps running (memory: screen-sleep-webview-occlusion-stall),
and it does not reliably set `document.hidden`. So a pause signal from the frontend alone is not
enough. Three layers:

1. **Explicit pause.** On `visibilitychange` to hidden, the frontend calls `ft_pause(true)`.
2. **Ack window (the Rust-side backstop).**
   - Each sub tracks `sent_seq`, `acked_seq` and the byte size of unacked messages.
   - The frontend acks from inside the Channel `onmessage` handler: on every 32nd message, or when
     ≥100 ms have passed since the last ack, checked on arrival. No timer is involved.
   - A stalled webview therefore stops acking by itself.
   - When `sent_seq - acked_seq > 256` or unacked bytes exceed 8 MB, the sub becomes **stalled**,
     which behaves exactly like paused.
3. **Paused or stalled mode.**
   - The hub stops sending and only adds folders to a per-sub dirty set, at O(folders) memory.
   - On resume (`ft_pause(false)`, or the first ack after a stall that brings the window back
     under both limits) it sends one collapsed `Dir`/`Delta` per dirty folder and one
     `Git { full: true }`. A `Dir` a stall cut short carries on from its next chunk while the
     snapshot's generation is unchanged: restarting at chunk 0 never finished a folder of more
     than ~32 chunks, because the client acks every 32 messages. A `Dir` whose last chunk is the
     one that filled the window went out whole and is not resent. A cold-open chunk 0 sent before the sort that
     met a paused or stalled sub was never sent, so that sub gets the whole listing from chunk 0.

### 5.5 Listing (`filetree/list.rs`)

- `std::fs::read_dir`, with the type from `d_type` (free on unix, and already in the
  `FindNextFile` data on Windows, including `FILE_ATTRIBUTE_HIDDEN`).
- **No per-entry `lstat`.** The only extra syscalls are one `stat` per symlink (link-to-dir vs
  link) and one per `DT_UNKNOWN` (some FUSE/NFS).
- Size, mtime and exec come only on hover (`ft_stat`), so the tree has no size or date columns.
- **Ignore matching** uses a cached matcher stack (`filetree/ignore_cache.rs`):
  - one `ignore::gitignore::Gitignore` per folder's `.gitignore`, built once with `GitignoreBuilder`;
  - stacked root→folder, plus `.git/info/exclude`, `core.excludesFile` (read once per hub), and the
    user's `file_tree_exclude` `GlobSet`;
  - a cached matcher is invalidated when the watcher sees its `.gitignore` change.
- **Never** `WalkBuilder::max_depth(1).parents(true)` per expand. It re-reads every ancestor `.gitignore` on every call.
- **Ignored entries are flagged, not dropped,** so "show ignored" is a client-side filter with no IO.
- Built-in excludes are never listed: `.git` (the folder itself; `.github` and `.gitignore` are
  shown), `.DS_Store` and `Thumbs.db`. Dotfiles are shown by default.
- **Sort:** done once in Rust. Folders first, then a natural, case-insensitive comparator
  (numeric-aware). There is no `Intl.Collator` in JS.
- **Symlinked folders** list their target but are keyed by the link path. A loop is detected by
  `(dev, ino)` against the ancestor chain (the `same-file` crate, already in the lockfile) and shown as
  a non-expandable link.
- **In-flight dedupe:** one listing per `(hub, rel)` at a time. A second request for the same folder
  joins the first, so a hung NFS read is never re-issued onto another blocking thread.
- **IO slots:** `fs_slots` is 32 global and 4 per hub, or 2 on a network mount. A hung `stat` pins at
  most a hub's slots, never the blocking pool or the main thread.
- **Network soft deadline:** 3 s. After that the row shows "not responding…" and the request keeps
  running in the background (deduped, so it is never repeated).

## 6. Watching, per OS

All backends feed one **coalescer thread per hub**:
- Raw events go into a bounded crossbeam channel of 65,536. On overflow the hub sets a flag that
  forces a root-wide resync. **Events are never dropped silently.**
- The window closes after **75 ms of quiet or 250 ms total**.
- Each path is classified before any allocation-heavy work:
  1. **visible**: the parent folder is subscribed. With ≤64 events for that folder, only those names
     are `symlink_metadata`ed; otherwise the folder is re-listed and diffed against its snapshot.
     The result is one `Delta` with `gen + 1`.
  2. **git signal**: `.git/index`, `HEAD`, `refs/**`, `packed-refs`, `MERGE_HEAD`, or any worktree
     path that is not ignored.
  3. **noise**: `.git/objects`, `.git/logs`, ignored subtrees (checked against the cached matcher),
     and `.DS_Store`. `npm install` costs one prefix check per event and no git run.
- `Event::need_rescan()` (FSEvents MustScanSubDirs, a Windows buffer overflow, inotify
  `IN_Q_OVERFLOW`) re-lists every subscribed folder under that path and marks git dirty.
- Linked worktrees and submodules: `.git` may be a file. The hub watches `git rev-parse --git-dir`
  and `--git-common-dir` (resolved once and stored in `RepoInfo`).

### 6.1 macOS

- `notify` stays `default-features = false, features = ["macos_fsevent"]` (`Cargo.toml:58`).
  kqueue's one fd per file is the EMFILE trap this repo already hit.
- **One `FsEventWatcher` per hub, `RecursiveMode::Recursive` on the root, created once.** notify 7
  stops and restarts the stream on every `watch()` call, so the watched path set never changes
  per expand. Recursion is free in fds (about 1 per root), and the coalescer filters the volume.

### 6.2 Linux

- **One process-wide inotify instance**: one `notify::INotifyWatcher` owned by `FileTreeState`, whose
  handler routes events to hubs by path prefix. `max_user_instances` defaults to 128 and is shared
  with other apps.
- Watches are `NonRecursive`, on: the root, expanded folders, the git dir, and `refs/heads`.
- **The budget** is `min(max_user_watches / 8, 8192)` across all hubs, read from
  `/proc/sys/fs/inotify/max_user_watches`.
- On `ENOSPC` (notify `MaxFilesWatch`) or over budget, the least recently expanded folders fall
  back to **visible-only polling**: a 2 s mtime check of that folder, only while its row is in the
  viewport and the window is focused. Health shows "watch: polling N folders (inotify limit)".
- **Stated limitation:** there are no recursive watches, so a change deep inside an unexpanded
  folder is not seen immediately. Git decorations for it still refresh on the edges in §7.2, every
  one of which is an edge and not a poll. §9.2 states what this does to agent attribution.

### 6.3 Windows

- One `ReadDirectoryChangesWatcher` per hub, recursive on the root, in one handle.
- A buffer overflow means a rescan.
- Event paths go through `share::canon`.
- Case folding only where `relocate::case_insensitive` confirms it; NTFS is not assumed.

### 6.4 Network mounts (every OS)

- Classified in `filetree/fsclass.rs`:
  - macOS: `statfs` `f_fstypename` ∈ {smbfs, nfs, afpfs, webdav, macfuse, osxfuse}.
  - Linux: `statfs` `f_type` NFS, CIFS, SMB2, FUSE magics.
  - Windows: `GetDriveTypeW == DRIVE_REMOTE` or UNC, via `windows-sys` with the
    `Win32_Storage_FileSystem` feature added to the existing direct dependency (`Cargo.toml:86`).
- **No watcher**, because FSEvents, inotify and SMB RDCW miss remote writers.
- Revalidation is by `known_gen` on expand, on window focus, and by a 15 s poll of visible
  subscribed folders while the window is focused.
- Git auto-refresh is off (manual and edges only), and Health says so.

### 6.5 Shallow mode

A root equal to `$HOME`, `/`, a drive root, `/Volumes/*` or `/mnt/*` is **shallow**. Without this,
FSEvents or RDCW over a home directory streams events from browser caches and `~/Library`
continuously, and idle would no longer be zero.

In shallow mode:
- the watcher watches only subscribed folders, NonRecursive (on macOS this accepts a stream restart
  per expand, which is harmless because a newly expanded folder is listed anyway);
- there is no search index outside a repo unless the human asks for one ("Index this folder");
- git is off unless the root is itself a repo.

### 6.6 Lifecycle and idle

- A hub dies **30 s** after its last subscriber closes, dropping its watcher, snapshots, git worker and index.
- A root that is deleted or renamed away sends `RootGone`. The banner reads "This folder no longer
  exists · Follow the tab / pick another", and there is no automatic re-root to home.
- **Refresh on regaining focus:** when a window regains focus after more than 60 s unfocused, its
  subs revalidate subscribed folders by `known_gen`. This catches anything FSEvents coalesced away
  while the machine slept.
- There is no periodic local re-list. The only timers anywhere are the Linux over-budget poll, the
  network poll (visible and focused only), and the remote helper ping.

## 7. Git

### 7.1 Command

```
git --no-optional-locks -C <root> status --porcelain=v2 -z --branch --untracked-files=normal
```

It is built by `src-tauri/src/util/proc.rs`, the shared hardened command builder:
- stdin null;
- `GIT_OPTIONAL_LOCKS=0`, `GIT_TERMINAL_PROMPT=0`, and `GIT_SSH_COMMAND` with BatchMode (as `share/git.rs` does);
- `CREATE_NO_WINDOW` on Windows (today only `windows_setup.rs:274` sets it);
- `kill_on_drop`.

The flags are chosen as follows:
- **`-unormal`, not `-uall`.** An untracked folder implies its children are untracked, and the tree
  infers them. `-uall` enumerates every file of every untracked tree on every refresh.
- **No `--ignored`.** Ignore state comes from the cached matchers (§5.5), for listed folders only.
- **No git2 or gix.** The subprocess honours the user's config, fsmonitor, LFS and `safe.directory`,
  with no C/OpenSSL build on three OSes. The repo already requires git.

### 7.2 Scheduling

- **One run in flight plus one queued per repo.** Any number of signals during a run collapse into one rerun.
- **Global `git_slots` semaphore of 2.** Seventy tabs on 10 repos cannot pile up git processes.
- **Triggers:**
  - a `.git/index`/HEAD/refs change, 120 ms trailing;
  - a worktree change that is not ignored, 300 ms trailing;
  - the **OSC 133 D** edge (command finished) in any tab whose resolved place is under the root, so
    a `git commit` typed in the terminal decorates in about 150 ms;
  - an agent `ToolPost` for a write under the root;
  - window focus regained;
  - manual refresh.
- **Timeout 10 s**, then the process is killed.
- **Slow-repo mode.** Two consecutive runs over 2 s switch the repo to refreshing on `.git/index`/HEAD
  changes, focus and manual refresh only, and Health says so. A run under 1 s leaves the mode.
- If git is missing, or `safe.directory` refuses the repo, decorations are off with a one-line Health reason.

### 7.3 Payload and roll-up

```rust
pub struct GitUpdate { pub gen: u64, pub full: bool,
    pub files: Vec<(String, String)>,  // rel → "M"|"A"|"D"|"R"|"C"(conflict)|"?"(untracked)|"T"
    pub dirs: Vec<(String, String)>,   // rolled-up severity, same letters
    pub del: Vec<String>,              // keys that went clean since the last update
    pub branch: Option<BranchInfo>, pub truncated: bool }
pub struct BranchInfo { pub head: String, pub oid: String, pub upstream: Option<String>, pub ahead: i32, pub behind: i32 }
```

- **The roll-up is computed in Rust.** Each changed path walks its ancestors, and a folder takes the
  most severe state: conflict > deleted > modified > added > untracked.
- **Only changed keys are sent** (`full: false`), so a status run that changes nothing costs one empty message.
- Past 20k changed files, only `dirs` and counts are sent.
- The branch row reads the `# branch.*` headers of the same run.

**Rendering:**
- name colour plus a trailing letter: M `--yellow`, A/? `--green`, D `--red` with strikethrough, R `--cyan`, conflict `--magenta` bold with "!";
- ignored entries at 55% opacity;
- a folder gets a 6 px dot in its roll-up colour.

### 7.4 Line counts and diff

- **No per-row `+N −N` in v1.** The tooltip shows the counts for the hovered file only:
  `git diff --numstat -z HEAD -- <rel>`, run inside `ft_stat` and cached per (rel, git gen).
  numstat over the whole repo scales with diff size and is the first thing to fall over in big refactors.
- **"Compare with HEAD"** reuses `handleShowDiff`'s read-only flow (`claudeCode.svelte.ts:756-792`),
  extracted as `openGitDiff(place, rel, ref)`. It is fed by `ft_diff_base`.
- `git_show_file` is fixed in phase 0 as its own commit: `spawn_blocking`, `/` separators in
  `ref:path`, `util/proc.rs`, and a `share::canon` strip so `/tmp` vs `/private/tmp` works.

### 7.5 No git writes

v1 has **no stage, unstage, discard, checkout or commit**. `git add` and `git restore` take
`index.lock` and race the agents' own commits in the same repo, and a Discard button under a
running agent destroys its work. Commit and push stay in the terminal.

## 8. Find

There are two layers.

1. **Type-ahead (always on).** Printable characters, while the tree is focused, jump to the next
   visible row whose name starts with the typed prefix. The prefix resets after 1 s and is
   highlighted. This is the Finder, Explorer, VS Code and JetBrains convention. **Bare letters are
   never commands by default.**
2. **Filter (`/` or Cmd/Ctrl+F while the tree is focused).**
   - **Index.** Built per hub, lazily, on the first query:
     - inside a repo, `git ls-files -co --exclude-standard -z` (about 100 ms for 100k files);
     - otherwise, `ignore::WalkBuilder::build_parallel` (gitignore-aware, depth 20, `.git` skipped).
   - **Storage and freshness.** The index is one `String` arena plus offsets, about 6 MB per 100k
     paths. It is kept fresh from watcher events. On Linux, deep changes are reconciled from each
     status run's untracked and deleted entries. It is evicted after 5 min with no query.
   - **Matching.** `nucleo-matcher = "0.3"` with `Config::DEFAULT.match_paths()`, smart case and
     smart normalisation. It returns the top 500 with match indices.
   - **Stale queries.** Each query carries `seq`, and an `AtomicU64` per hub makes stale queries
     stop early.
   - **Display.** Results show as a **pruned tree**: matches plus their ancestors, auto-expanded,
     with highlighted characters. That keeps context, unlike a flat list. While building, the bar
     shows "indexing…".
   - **Keys.** Arrows work, Enter opens, and Escape restores the previous expansion and scroll exactly.

The Cmd+F routing: the capture handler (§11.1) gives Cmd/Ctrl+F to the tree when the tree has focus,
before the terminal and editor branches. QuickOpen keeps its own code in v1. Moving it onto
`ft_search` (dropping its 10k cap and JS scorer, `QuickOpen.svelte:65-89`) is post-v1.

## 9. Agent awareness

### 9.1 `AgentFileLedger` (`src-tauri/src/agent_files/`)

The ledger is fed inside `hooks_handler` (`server.rs:3932`) in the existing arms: ToolPre (`:4719`),
ToolPost (`:4833`), Prompt (`:4565`) and Stop (`:4442`).

Recording rules:
- **Read the raw `hook_event_name` before `normalize_hook_event`.** It folds `PostToolUseFailure` into
  ToolPost (`:3698`), and the ledger keeps failures visible.
- **Dedupe Pre events by `tool_use_id`.** The mod relay and the HTTP hook both deliver PreToolUse,
  and whichever arrives first wins.
- **Recording is a non-blocking push.** A `parking_lot::Mutex` push plus a send on a channel to the
  ledger dispatcher thread. The hook reply never waits on routing.

**Extraction (`extract.rs`):**
- `file_path`: Read is `read`; Edit and MultiEdit are `edit`; Write is `write`, or `create` if the file
  didn't exist at Pre time.
- `notebook_path` (NotebookEdit) is `edit`.
- `path` on Grep and Glob is `search` on that folder.
- Codex `apply_patch` paths come from the `*** Add File:`, `Update File:`, `Delete File:` and `Move to:` lines,
  resolved against the session cwd. This is marked **unverified** until a real codex-cli trace shows
  PreToolUse fires for apply_patch (`docs/codex-integration-review.md`).
- Remote tabs are keyed by (tunnel `host_key`, absolute remote path), the `watch::place` idea.
- Bash is **not parsed** (§9.2).

```rust
pub struct LedgerEntry {
    pub path: String,            // canonical abs (local) or "user@host:/abs" (remote) — NFC
    pub op: String,              // "read"|"search"|"edit"|"write"|"create"|"delete"|"changed"
    pub tool: String, pub tool_use_id: Option<String>,
    pub agent: String,           // gate::agent_key; "" = main agent
    pub status: String,          // "pending"|"ok"|"failed"
    pub evidence: String,        // "tool"|"fs_during_tool"
    pub turn: u64, pub t_pre_ms: i64, pub t_post_ms: Option<i64>,
}
struct TabLedger { session_id: String, runtime: String, turn: u64, turn_open: bool,
                   entries: VecDeque<LedgerEntry> /* ring, 2,000 per tab */ }
```

**Turns.** `UserPromptSubmit` starts turn N+1 and `Stop` closes it. Subagents share the parent's
session and are told apart by `gate::agent_key` (`app_state.rs:188-191`).

**Lifetime.**
- The ledger is in memory only. It is never persisted, so it adds no Tab field, no serde enum and no
  reload/duplicate/share decision.
- `reloadTab` gets one more remap call, `agent_files_remap_tab(old, new)`, next to its existing
  `remapTab` calls (`workspaces.svelte.ts:2758-2766`), because reload mints a new tab id.
- A closed tab's ledger is dropped.

**Delivery.**
- The dispatcher routes each entry to the hubs whose root contains the path, by a component-wise,
  case-fold-aware prefix check.
- Each matching hub gets at most **one coalesced `Agent { marks }` per 100 ms**, carrying root-relative paths.
- Raw `tool_input` (Write content can be megabytes) is **never** re-broadcast. The existing `emit_dual`
  hook broadcasts stay as they are, and the tree doesn't consume them.

```rust
pub struct AgentMark { pub rel: String, pub op: String, pub phase: String /*pending|ok|failed*/,
    pub tab_id: String, pub agent: String, pub turn: u64, pub evidence: String, pub at_ms: i64 }
```

### 9.2 Shell writes: evidence, not guesses

For Bash, PowerShell and other shell tools:
- A Pre registers an in-flight shell call `{tab_id, tool_use_id, hub}` for the hub containing the
  agent's cwd. The matching Post or PostToolUseFailure removes it.
- While **exactly one** agent tab has a shell call in flight in a hub, every non-ignored *visible*
  or *git-signal* path the coalescer classifies is recorded as `op: "changed"`, `evidence: "fs_during_tool"`.
- With two or more in flight, the change is left unattributed and shows only as a git change.
- The UI words it as **"changed while claude's Bash ran"**, never "claude edited".

Known coverage gaps. They are stated, not hidden; `getAgentFileActivity` returns `coverage` per hub,
`"full"|"expanded_only"|"none"`:
- **Linux:** only expanded folders are watched (§6.2), so a write deep in a collapsed folder is not attributed.
- **Network mounts and shallow roots:** none or partial.
- **Ignored paths** are never attributed, because the coalescer drops them as noise.
- **Background Bash** (`run_in_background`): writes after Post are not attributed.
- **Human writes:** a human's edit or command that lands during the window is attributed too. The
  word "while" exists for exactly this.

**Commits.** When HEAD moves while exactly one agent tab has an open turn in that repo, the git
worker runs `git diff-tree --no-commit-id --name-only -r -z <old_oid> <new_oid>` once. Those rows
flash green, labelled "committed during claude's turn".

### 9.3 What the human sees

- **Activity rail** (2 px, the left edge of the row, in the op colour):
  - `--ft-read` violet for read and search;
  - `--ft-write` amber for edit and write;
  - `--ft-create` green for create and commit;
  - `--ft-delete` red, with the name struck through;
  - a `--ft-fail` red outline for failed.

  It is CSS only: one `@keyframes` on one element, compositor-only, with the start time in a CSS
  variable. There is no per-character shimmer and no 90 ms JS tick (the mod's `rows.tsx`).
  - **Editing now:** the rail pulses between Pre and Post.
  - On Post `ok`, a 1.2 s sweep runs. "Written this turn" then settles into a small persistent dot
    in the agent's colour.
- **Reads** light faintly for 2.7 s and leave no persistent mark.
- **Agent colour.** Each agent tab gets a stable hue from a 6-token palette, assigned in the window's
  agent order and shared by strip chips, rails and dots. When two agents wrote one file, the rail
  splits.
- **Collapsed folders over activity** get a count badge (`2✎`) in the most active agent's colour.
  **They never auto-expand.**
- **Edge chips** (§3.2 item 7) replace the mod's lit rows pinned above the window, which shifted content under the cursor.
- **Follow agent.** Off by default, per window, in memory only. It is armed by choosing an agent chip
  and toggling the eye in the "⋯" menu. On that tab's writes it reveals the row, expanding
  ancestors and scrolling, **only if the human hasn't touched the tree for 3 s**.
- **Conflict badge.** A red ⚠ appears when either:
  - a file is dirty in an editor tab and an agent wrote it this turn; this adds a "Compare buffer ↔ disk" action;
  - two agents wrote the same file in overlapping open turns; the tooltip names both tabs.
- **Rename/delete guard.** See §10.3.

### 9.4 Scopes (lenses)

Every lens except All renders a **pruned tree**: matching paths plus ancestors, with the same rows
model as filter mode.
- **Changed**: git-dirty paths (phase 2).
- **Turn**: this turn's ledger writes plus `fs_during_tool`, for the chosen agent chip, or all agents
  in the root when none is chosen. Each row's tooltip names its evidence ("Edit tool", "on disk
  while Bash ran", "modified in git").
- **Session**: the same over the session.

The `1`–`4` keys select lenses only in vim mode (§11). Otherwise lenses are switched by click.

### 9.5 Sending files to an agent: insert, never submit

There is one shared helper, `src/lib/utils/insertPaths.ts`:
`insertPathsIntoTab(tabId, items: {abs: string; remote?: {hostKey: string; path: string}}[])`.
Its callers: the tree's "Add to ‹tab›" (context menu, Alt+Enter, a drag onto a terminal) **and** the
existing terminal OS-drop path, which closes that path's missing dialog check
(`TerminalPane.svelte:1391-1398`).

1. Refuse unless `terminalsStore.get(tabId)` is live and the tab has no `service_id`.
2. `await agentDialogOpen(tabId)`, which covers hooked, on-screen and trust dialogs
   (`overlord.rs:216-224`). If a dialog is open, **refuse** with a toast and type nothing. A typed
   key picks a row there, and Enter on the trust dialog confirms "No, exit".
3. **Agent tab** (an agent session registered for the tab, any runtime): write each absolute path
   plus a space with `writeTerminal(ptyId, bytes, true)`, **200 ms apart, no CR**. This is the proven
   chip convention. `@path` is not used, because Claude's autocomplete popup behaviour is unverified.
4. **Plain shell:** `shellQuote(path, shellKind)` from `src/lib/utils/shellQuote.ts`. It replaces the
   private POSIX-only `escapePathForTerminal` (`TerminalPane.svelte:415`) with:
   - POSIX: backslash quoting;
   - pwsh: `'…'` with `''` doubling;
   - cmd: `"…"`.
5. **ssh tab:**
   - If the item has `remote.hostKey` equal to the tab's tunnel host, type the remote path directly.
   - If the tree is local and the tab is remote, use the existing upload-then-type branch of the drop path.

"Ask about this…" puts the paths into the tab's ComposerDock as attachments. The human writes the
prompt and sends it through the composer's existing gated path. **The tree never submits.**

### 9.6 MCP tools

Gating:
- The tools are listed only while `file_tree_enabled && file_tree_agent_tools`.
- `tool_list_response`'s positional bools (`protocol.rs:55-61`) are refactored into a
  `pub struct ToolGates { tasks, stack, follow_ups_live, escalations_to_overlord, browser_link, file_tree }`
  in the same phase, as its own commit.
- The `[&str; 58]` / `[&str; 26]` array lengths grow to match.

| Tool | Args | Behaviour |
|---|---|---|
| `revealInTree` | `{path, select?: bool=false}` | Acts in the **caller's tab's window** only. Never opens a closed column (that would change width) and never takes focus. Returns `{result}`, one of: `revealed`; `noted` (the human used the tree in the last 3 s, so a chip "claude points at `src/x.ts`" appears instead of moving); `tree_closed` (the toggle button gets a dot badge); `outside_root`. On `OWN_TAB_ONLY_TOOLS`. |
| `getTreeSelection` | `{}` | The human's selection in the caller's window: `{host: null \| "user@host", root, focused, selected: [abs paths], humanIdleMs}`. Paths only, never content. Handled like `getCurrentSelection`, in the caller's webview. On `OWN_TAB_ONLY_TOOLS`. This is the pull answer to the mod's prompt injection: maiTerm does **not** inject the selection into prompts. |
| `getAgentFileActivity` | `{tabId?, scope?: "turn"\|"session"="turn", includeReads?: false}` | Ledger entries with `status`, `evidence` and per-hub `coverage`. Defaults to the caller's own tab. An explicit `tabId` follows `PEER_ADDRESSING_TOOLS` (same mesh). Paths only. |
| `showDiff` (extended, phase 5) | `ref` accepts `"@turn"` / `"@session"` | A read-only diff against the **caller's own** baseline. A baseline from another tab is never served. |

No preference is added to `preference_meta`; agents get these tools and nothing else. There is no
priming line: the tool descriptions carry it.

## 10. File operations and safety (`filetree/ops.rs`)

### 10.1 Rules enforced in Rust on every write

- **Containment.**
  - Every `src` and `dest` is joined to the hub root.
  - Any `..`, absolute or empty component is rejected.
  - Each existing component is `symlink_metadata`ed, and the op is refused if a symlink resolves outside the canonical root.
  - The comparison is component-wise, case-folded only where the disk folds.
  - Acting on the root itself is refused.
  - `import` alone may take sources outside the root; it never takes a destination outside it.

  This is the first path scope any fs command in the app has. The editor commands still accept any path.
- **No symlink following** during a recursive copy, move fallback or delete. The copy walker uses
  `symlink_metadata` and recreates links as links.
- **Never clobber.**
  - Create uses `OpenOptions::create_new`, and create-dir uses `create_dir`.
  - Rename and move refuse to overwrite unless `conflict: "keep_both"`, which picks a fresh name.
  - A case-only rename on a case-insensitive volume goes through a temp-name hop.
  - Move tries `rename` first. On `EXDEV` it copies, verifies sizes, and only then removes the source.
- **Trash is on one thread.** Every `trash` crate call (5.x) runs on one dedicated `ft-trash` worker
  thread, because the crate warns of UB from concurrent mount-point queries on Linux. Where trashing
  fails (network volume, no XDG trash), the op answers `needs: no_trash` and the UI offers permanent
  delete through the ConfirmStrip. There is **never a silent `rm` fallback**.
- **Permanent delete takes a ticket.**
  - `ft_prepare_delete(sub, rels)` mints a single-use token bound to that exact rel set, valid for 60 s.
  - It returns `{token, files, dirs}`, and the ConfirmStrip shows those counts.
  - `ft_op{kind:"delete"}` without a matching token is refused, so a stray IPC can't delete.

### 10.2 Operations

```rust
pub struct FileOp {
    pub kind: String,      // "create_file"|"create_dir"|"rename"|"move"|"copy"|"duplicate"|"trash"|"delete"|"import"|"restore"
    pub src: Vec<String>,  // rels; for "import": native abs OS paths
    pub dest: Option<String>,          // destination folder rel
    pub name: Option<String>,          // create/rename; "a/b/c.ts" creates intermediates
    pub conflict: String,              // "fail"|"keep_both"|"skip"
    pub token: Option<String>,         // "delete"
    pub ack_agents: bool,              // second confirm given (§10.3)
}
pub struct OpResult { pub done: Vec<OpItem>, pub failed: Vec<OpFail>,
    pub undo: Option<FileOp>, pub needs: Option<Needs> }
pub struct OpItem { pub from: String, pub to: Option<String> }
pub struct OpFail { pub rel: String, pub code: String, pub msg: String }
pub struct Needs { pub code: String /*"no_trash"|"agent_active"|"live_cwd"*/, pub detail: String, pub tabs: Vec<String> }
```

- **Keep-both naming** follows each platform's file manager: macOS `name copy.ext` / `name copy 2.ext`,
  Windows `name - Copy.ext` / `name - Copy (2).ext`, Linux `name (copy).ext` / `name (copy 2).ext`.
  `unique_path` (`editor.rs:290`) is generalised to take the pattern.
- **Restore** (the undo of trash): Linux and Windows only, through `trash::os_limited::{list, restore_all}`
  on the trash thread, matched by original path and deletion time.

### 10.3 maiTerm-specific guards

- **Live cwd → Move project.** Moving, renaming or trashing a folder that is, or contains, any of the
  following is refused with "Tab ‘X’ is working in this folder. Use Move project…", which opens the
  relocate flow (`docs/relocate.md`):
  - a live terminal's shell cwd (`get_pty_info`; cheap after §4.3);
  - a live agent session's cwd;
  - a running stack service's cwd.

  Relocate is the only path that suspends agents first; otherwise a live Claude recreates the old project dir.
- **Agent in flight.** Renaming, moving or deleting a path with a pending Pre, or written this turn by
  an agent, answers `needs: agent_active` naming the tabs. The ConfirmStrip asks a second time, and
  `ack_agents: true` proceeds.
- **Open editors** (checked in the frontend, before calling):
  - Renaming or moving an open file retargets its editor tabs through `updateEditorTabFile`, and
    their `watch_file` is re-armed.
  - Trashing a dirty open file asks first ("store.ts has unsaved changes").

### 10.4 UI

- **Inline new and rename** (`InlineName.svelte`):
  - **Placement.** New items appear at the top of the target folder's children, after expanding it.
    Rename happens in place, with the basename before the extension preselected (`a.test.ts` → `a.test`).
  - **Focus.** Focused with `await tick(); input.focus({preventScroll: true})`. Never Svelte
    `autofocus`, and never rAF, which stalls when the webview is occluded.
  - **Commit and cancel.** Enter commits and Escape cancels. A real blur commits. If the column
    unmounts mid-edit, **nothing is created or renamed**: an accidental create is worse than a lost name.
  - **Live validation, in words, under the input:**
    - an empty name;
    - `/` in a rename;
    - Windows-illegal names, enforced on Windows and on any root whose disk folds case:
      `<>:"\|?*`, a trailing dot or space, reserved names (CON, PRN, AUX, NUL, COM1–9, LPT1–9);
    - an existing name, case-folded where the disk folds.
  - **Nested create** of `a/b/c.ts` makes the intermediate folders. A new file then opens as a pinned editor tab.
- **ConfirmStrip** (in the footer; `confirm()` is dead in the webview). It is used for permanent
  delete, `no_trash`, `agent_active` and dirty-trash. Its buttons are focusable, and Escape cancels.
- **Undo.**
  - A frontend stack of 50 entries per root, filled from `OpResult.undo`, run by Cmd/Ctrl+Z while the tree has focus.
  - An entry is dropped if its target changed on disk since then (watcher gen).
  - The verbs are honest per platform: on macOS a trash toast reads "Moved to Trash · Show in Trash"
    (the crate has no restore there); Linux and Windows offer Undo.
  - **Undoable beats confirmable**: moves never ask.
- **Pointer drag-and-drop.** HTML5 DnD is dead here (`OverlordBoardView.svelte:236-251`).
  - The controller is `src/lib/filetree/dnd.ts`, with a 5 px threshold (the TerminalTabs pattern) and
    a ghost chip ("store.ts" / "3 items").
  - **Targets, hit-tested on the pointer:**
    - a tree folder row: highlighted, auto-expands after 600 ms;
    - the root header;
    - `[data-terminal-slot]`: insert, §9.5;
    - the ComposerDock: attach.
  - **Move vs copy.** The default is move. Alt (macOS) or Ctrl (Windows/Linux) copies, and the ghost shows ＋.
  - Autoscroll within 24 px of the edges. Escape cancels.
  - **OS drops in** arrive through `getCurrentWebview().onDragDropEvent`. The column hit-tests its own
    rect, checks `isConnected` and visibility (as `TerminalPane.svelte:1300-1324` does), and finds the
    target row arithmetically as `floor((y - top + scrollTop) / rowH)`. The drop runs `ft_op{kind:"import"}`.
- **Context menu** (`ContextMenu.svelte`, with platform-correct labels; multi-select hides the
  single-item entries):

```
Open                 Enter        Open to the Side      ⌘↩ / Ctrl+Enter
Open Preview         Space        Open With Default App
──
Add to ‹claude · tab 3›   ⌥↩      Ask about this…       Open Terminal Here
──
Reveal in Finder | Show in File Explorer | Open Containing Folder          (mac | win | linux)
Copy Path    Copy Relative Path
──
New File…   New Folder…   Cut   Copy   Paste   Duplicate   Rename…  F2
Move to Trash | Delete (Recycle Bin) | Move to Trash    Delete Permanently…
──
Compare with HEAD    Compare with Turn Start    Compare with Session Start
──
Set as Root (pins)
```

"Open Terminal Here" calls `createTab` with that cwd. On a remote root it uses
`ssh … -t 'cd <q> && exec $SHELL -l'`, built through `sshArgsOf` with the remote-quoted path.

### 10.5 Opening files: preview tabs and dedupe

There is one helper, `src/lib/utils/openInEditor.ts`:
`openInEditor({abs, remote?, mode: 'preview'|'pin'|'side', line?})`.
- It lifts the workspace-wide dedupe out of `claudeCode.handleOpenFile` (`claudeCode.svelte.ts:602-700`),
  because `createEditorTab` doesn't dedupe (`workspaces.svelte.ts:1233-1249`).
- `utils/openFile.ts` and `handleOpenFile` migrate onto it.

**Preview tabs** (VS Code semantics, `file_tree_preview_tabs`, default on):
- **Opening.** A single click or Space opens a preview tab (italic title) in the pane holding the
  active tab. The next preview replaces the file inside that same tab through `updateEditorTabFile` and
  `editor-replace-file`. Space keeps focus in the tree.
- **Becoming permanent.** A preview becomes a normal tab on: its first edit (watching
  `editorRegistry.dirtyTabs`), a double-click on the row, Enter, or a double-click on its tab.
  A preview with unsaved edits is never replaced.
- **Never replaces a terminal.** An editor tab is inserted after a busy agent terminal, and the terminal keeps running.
- **Storage.** In memory only: `SvelteMap<paneId, tabId>` in the store, **never persisted and never
  a Tab field**. After a restart, previews are simply normal tabs.

Other open behaviours:
- **Remote files** open through the existing remote editor path (`EditorFileInfo.is_remote`), using
  the tab's ssh command via `sshArgsOf`.
- **Folders.** A click selects; the chevron or Enter toggles. A double-click expands and **never re-roots**.

## 11. Keyboard

### 11.1 Routing

The global handler runs in the capture phase on `window` (`+layout.svelte:1381`):
- Extract today's inline `isAppShortcut` expression into a function `isAppShortcut(e)`.
- Add `(e.shiftKey && key === 'b')` to it, so Cmd/Ctrl+Shift+B works from editor tabs.
- Add `const treeFocused = !!document.activeElement?.closest('[data-filetree]')`.
- **When `treeFocused`, only `isAppShortcut(e)` keys are handled by the app.** Every other key returns
  early and reaches the tree's own `keydown`, which calls `preventDefault` and `stopPropagation` on
  what it handles. Without this, Cmd+D in the tree would split the pane.

Cmd/Ctrl+N and Cmd+Shift+N are app shortcuts (new window, duplicate window). **They stay with the
app.** The tree has no Cmd+N binding: new file and new folder come from the header buttons and the
context menu. Cmd+N must never silently stop opening a window.

### 11.2 Map

| Key | Action |
|---|---|
| Cmd/Ctrl+Shift+B (global) | Closed: open and focus. Open but unfocused: focus. Focused: close. When an editor tab is active, also reveals its file. |
| ↑ ↓ · PgUp PgDn · Home End | Move focus |
| → / ← | Expand or step to the first child / collapse or go to the parent |
| Alt+→ / Alt+← | Expand or collapse recursively. Recursive expand stops at 2,000 rows and says so. |
| Shift+↑↓, Shift+click · Cmd/Ctrl+click · Cmd/Ctrl+A | Extend selection · toggle · select all visible |
| Space · Enter · Cmd/Ctrl+Enter | Preview, keeping focus · open pinned (file) or toggle (folder) · open to the side |
| Alt+Enter | Add path(s) to the followed tab (§9.5) |
| F2 | Rename (every platform; Enter does not rename on macOS) |
| Delete; macOS Cmd+Backspace | Trash |
| Shift+Delete; macOS Cmd+Alt+Backspace | Delete permanently (ConfirmStrip) |
| Cmd/Ctrl+C / X / V | Copy / cut / paste files (in-app clipboard) |
| Cmd/Ctrl+Z | Undo the last tree op |
| macOS Cmd+Alt+C / Cmd+Alt+Shift+C · elsewhere Shift+Alt+C / Ctrl+Shift+Alt+C | Copy absolute / relative path |
| Cmd/Ctrl+Shift+. | Toggle hidden files |
| `/`, Cmd/Ctrl+F | Filter |
| Printable characters | Type-ahead |
| Escape | Staged: clear the filter, then clear a multi-selection, then hand focus to the active terminal (`terminalsStore.focusTerminal`) |

**Vim mode** (`file_tree_vim_keys`, default off) adds `j k h l`, `gg G`, `o`, `d` (diff vs HEAD),
`t`/`s` (diff vs turn/session start), `y`/`Y` (copy relative/absolute), `a` (add to agent) and
`1`–`4` (lenses). In vim mode, type-ahead needs a leading `'`. Bare-letter actions never exist
outside vim mode: an accidental `a` while typing a filename must not type into an agent.

## 12. Frontend architecture

### 12.1 Who owns what

- **Rust owns the truth:** snapshots, generations, git, the ledger and the index.
- **The frontend owns the view:** expanded set, selection, focus, scroll and the flattened rows.
- Listings are never re-derived or re-sorted in JS.

### 12.2 Reactivity discipline (the most important frontend call)

```ts
// src/lib/filetree/model.ts — pure, Vitest-tested, no Svelte imports except $state in the store
export type Row = { key: string; depth: number; name: string; kind: 'f'|'d'|'l'|'L'|'o'; flags: number };
export class TreeModel {
  dirs = new Map<string, { gen: number; names: string[]; entries: Map<string, Entry>;
                           state: 'loading'|'ok'|'error'; err?: string; truncated: boolean }>();
  expanded = new Set<string>();
  git = new Map<string, string>();          // rel → letter (files and rolled-up dirs)
  agent = new Map<string, AgentMarkView>();  // rel → latest mark per rel
  applyDir(ev): Splice[]; applyDelta(ev): Splice[] | 'resync'; expand(rel): Splice[]; collapse(rel): Splice[];
}
```

- `rows` is `$state.raw<Row[]>`, replaced by **splicing into a copy**:
  - expand splices the visible subtree in after the parent;
  - collapse removes the contiguous range `[i+1, nextRowWithDepth ≤ d)`;
  - a `Delta` maps to splices inside that folder's range;
  - a full re-flatten happens only on a scope, filter or hidden/ignored toggle.
- Collapse-all is O(1): replace with the root rows.
- Folder data lives in plain `Map`s. Reactivity comes from coarse `$state(0)` counters,
  `structureVersion`, `gitVersion` and `agentVersion`, each bumped once per applied batch.
- **A git or agent update never touches `rows`.** Only the roughly 60 mounted rows read `git` and
  `agent` (through the counters), so a status run re-renders those rows and never re-flattens.
- This satisfies the repo's Map-reactivity rule through explicit version signals rather than a
  `SvelteMap` per entry, which would mean 100k signals. `SvelteSet` and `SvelteMap` are used for
  the small sets: selection and preview tabs.

### 12.3 Virtual ARIA tree

- **Fixed row height** `--ft-row-h`, from density. Rows sit in a spacer of `rows.length × h`, placed
  with `translateY`. The window is `floor(scrollTop / h) ± 10` overscan, so about 60 DOM nodes keyed
  by `row.key`.
- **The focused row is always kept in the rendered set**, even outside the window, so the active descendant exists.
- **Each row is a `{#snippet}`, not a component.**
- **ARIA:**
  - The container is `role="tree"`, `tabindex="0"`, `aria-multiselectable="true"`, using
    **`aria-activedescendant`**. Roving tabindex is not used, because a focused row can be recycled out of the DOM.
  - Rows carry `role="treeitem"`, `aria-level`, `aria-expanded`, `aria-selected`, and `aria-label`
    with status ("store.ts, modified, written by claude this turn").
  - `aria-setsize` and `aria-posinset` are computed only for rendered rows.
- **Sticky ancestors** are computed from `rows`: the parent chain of the first visible row, up to
  `file_tree_sticky_max`, with no IPC. Clicking one scrolls to it and focuses it.

### 12.4 Store and files

```ts
// src/lib/stores/fileTree.svelte.ts — one per window
{
  open: boolean; width: number;                 // mirror of WindowData
  focused: boolean;
  place: TreePlace | null; pinned: boolean; sourceTabId: string | null;
  sub: { id: number; ack: OpenAck } | null;
  model: TreeModel | null;                      // current root
  roots: LRU<string /*place*/, ViewState>;      // 8 entries: model, scrollTop, selection, focus
  scope: 'all' | 'changed' | 'turn' | 'session'; agentFilter: string | null;  // in memory only
  followAgent: string | null;                   // tab id, in memory only
  search: { query: string; seq: number; hits: SearchHit[]; indexing: boolean } | null;
  selection: SvelteSet<string>; focus: string | null; anchor: string | null;
  previewTabs: SvelteMap<string /*paneId*/, string /*tabId*/>;
  health: { watch: string; git: string; note: string | null } | null;
  lastHumanTreeInputMs: number;                 // drives the 3 s rules
}
```

The expanded set per place is kept in `localStorage` under `ft:exp:<place>`. Every access is wrapped
in try/catch, with an LRU of 64 places × 512 paths. It is a per-viewer convenience, and losing it
costs one re-expand. No new state file is created.

New files:
- `src/lib/filetree/{model,paths,icons,keymap,dnd}.ts`
- `src/lib/stores/fileTree.svelte.ts`
- `src/lib/components/filetree/{FilesColumn,FilesHeader,RootSwitcher,TreeBody,StickyAncestors,FindBar,InlineName,ConfirmStrip,AgentStrip,LensBar,EdgeChips}.svelte`
- `src/lib/utils/{openInEditor,insertPaths,shellQuote}.ts`

## 13. Rust layout

```
src-tauri/src/util/proc.rs          hardened Command builder (stdin null, CREATE_NO_WINDOW, git env, timeout, kill_on_drop)
src-tauri/src/filetree/mod.rs       FileTreeState { hubs: Mutex<HashMap<String, Arc<RootHub>>>, subs, linux_inotify: OnceLock<SharedInotify>,
                                                    git_slots: Semaphore(2), fs_slots: Semaphore(32), trash: TrashWorker }
                    commands.rs     every #[tauri::command] above
                    types.rs        wire structs (§5)
                    paths.rs        rel↔native, containment, NFC, case fold
                    place.rs        ft_resolve_place, toplevel LRU
                    hub.rs          RootHub: subs (window label, Channel, paused/stalled, dirty set, acks), snapshots+gens, grace timer
                    list.rs         read_dir + flags + natural sort + chunking + in-flight dedupe
                    ignore_cache.rs per-folder Gitignore stack
                    fsclass.rs      local | network | unknown
                    coalesce.rs     classifier + 75/250 ms windows + overflow/rescan
                    watch/{mod,fsevents,inotify,windows,poll}.rs
                    git.rs          runner, porcelain v2 -z parser, roll-up, slow mode, diff-tree for commits
                    search.rs       index + nucleo
                    ops.rs          §10, trash worker
                    remote/{mod.rs,proto.rs,helper.py}   (phase 6)
src-tauri/src/agent_files/{mod.rs,extract.rs,baseline.rs}
```

`FileTreeState` is managed in `lib.rs`. Window `Destroyed` and `Focused` events, which are already
seen in `on_window_event` (`lib.rs:270`), are forwarded to it.

## 14. Turn baselines: diff against the start of the turn (phase 5)

**Why maiTerm captures its own.** Claude's `file-history` was checked against the real corpus. A
file first touched in turn T appears only in the snapshot at T+1 (`@v2`, no `@v1` on disk). The
store also lives in the per-account root, not `~/.claude` (`accounts/mod.rs:176-195`). So it cannot
supply the pre-turn state of exactly the files an agent just created or touched for the first time,
and it covers Claude only.

PreToolUse is a blocking HTTP hook (`lockfile.rs:540`), and the mod awaits its relay before
`next(e)`, so maiTerm can read a file before the tool runs.

**Rules (`agent_files/baseline.rs`):**
- **When.** Only the **first** write-capable call per `(tab, turn, path)` captures. The first capture
  per `(tab, session, path)` is also kept as the session baseline. Write-capable means Edit,
  MultiEdit, Write, NotebookEdit, and apply_patch paths.
- **How.**
  - `tokio::time::timeout(25 ms, spawn_blocking(read_capped(path, 2 MB)))`, a text-only read using
    the `read_file` NUL sniff.
  - On a timeout, the entry records `unavailable: "timeout"`, and a cancellation flag makes the late
    read **discard** its result, because it may have read post-edit bytes.
  - The hook reply waits at most 25 ms, and typically under 1 ms.
- **New files.** A file that doesn't exist yet records `null`, meaning a new file (diffed against empty).
- **Memory.** An LRU of **64 MB** total, evicting the oldest closed turns first. In memory only, never
  persisted, never on any wire to the phone.
- **Not captured:**
  - local only: **no remote capture in v1**, because a hook must never wait on ssh;
  - binary files or files over 2 MB: `unavailable: "binary"` / `"too_large"`;
  - Bash writes (no Pre path). Those rows offer Compare with HEAD only.
- **Pref.** `file_tree_turn_baselines` (default true) turns the whole thing off.

```rust
pub struct Baseline { pub content: Option<String> /*None = new file*/, pub unavailable: Option<String>,
                      pub turn: u64, pub source: String /*"maiterm"*/ }
```

**Surfaces:**
- **Compare with Turn Start / Session Start** opens a read-only diff through `createDiffTab`
  (`request_id: ''`), named `foo.ts (turn 14 · claude, tab 3)`.
- In the Turn lens, the tooltip's `+N −N` comes from a line diff against the baseline (the `similar`
  crate, added in this phase).
- `showDiff` with `ref: "@turn"|"@session"` is served from the caller's own baselines.

Claude `file-history` as a secondary source after a restart is OUT of v1 (§16). Without it, a turn
that began before maiTerm started shows "baseline unavailable (turn began before maiTerm started)".

## 15. Remote roots (phase 6)

### 15.1 Transport

- **One persistent helper per `host_key`**, refcounted by subscriptions, with 5 min idle grace and
  `kill_on_drop`. It is killed only through its own child handle, never by pattern. Its argv must not
  match the orphan sweep's `ControlPath=<our dir>/` pattern (`ssh_tunnel.rs:820-860`).
- **argv:**

  ```
  ssh <mux_client_args(host_key)> <watch::connect_args(ssh_args)> exec python3 -u -c '<LOADER>'
  ```

  `LOADER` is a **constant** of about 150 characters, valid under Python 3.6. It reads one decimal
  length line from `sys.stdin.buffer`, reads that many bytes, and `exec`s them as the helper (the
  same buffer object then carries the protocol). The helper source, `include_str!("helper.py")`, goes
  over stdin at every start, which is about 40 KB.
- **Consequences of the loader:**
  - No user data ever reaches the remote argv, which the login shell re-parses (`watch/remote.rs:6-8`).
  - Nothing is installed on the host: no content-addressed files, no sweeping, nothing left under
    `~/.maiterm` on shared accounts.
  - Single quotes and `exec` are valid in sh, bash, zsh, fish and csh.
- **Which connection:**
  - With a maiTerm tunnel (bridge prefs on), the helper muxes over the CM socket: ready in ≤250 ms, no new auth.
  - With no tunnel (bridge prefs off) or a Windows client (no ControlMaster, `ssh_tunnel.rs:437-445`),
    `mux_client_args` already gives one direct BatchMode connection, paid about 1 s **once**, never per request.
  - **The tree never creates a tunnel or a ControlMaster of its own** (the ControlPersist leak history).
- **Sessions.** One helper per host is one mux session whatever the tab count, which respects OpenSSH's default `MaxSessions 10`.

### 15.2 Protocol

- **Framing.** Request ids. Each frame is a JSON header line `{"id", "op", …, "len"?}` followed by
  `len` raw bytes, so there is no base64.
- **Ops:**
  - `hello` returns hostname, uid, home, python, git version, inotify limits;
  - `list(dirs[])`: `os.scandir`, no lstat, same `Entry` shape, ignore matching done in the helper
    with the same semantics (a gitignore port tested against the same fixtures as `ignore_cache.rs`);
  - `stat`, `realpath`, `toplevel`;
  - `read(path, cap)`;
  - `write(path, expect{mtime, size})`: temp file in the same folder, fsync, rename, mode preserved;
  - `mkdir`, `rename`, `copy`;
  - `trash`: into `~/.maiterm/trash/<ts>/`, restorable, so remote Undo works on every client OS;
  - `delete`;
  - `files()`: `git ls-files -co --exclude-standard -z`, falling back to a pruned walk;
  - `git_status()`: the same flags as §7.1, `GIT_OPTIONAL_LOCKS=0`;
  - `watch(dirs)`, `unwatch(dirs)`, `ping`.
- The same containment rules apply in the helper. Subprocesses always use argv lists, never a shell.
- Rust maps helper events onto the same `TreeEvent` protocol. The frontend changes only by the host
  chip and the wrong-host warning.

### 15.3 Remote watching, liveness, fallbacks

- **Watching.**
  - inotify via `ctypes` (proven on nova under 3.8.10), only on the root, expanded folders and the git dir.
  - The budget is `min(max_user_watches / 8, 8192)`, because stock hosts have 8,192.
  - On ENOSPC or with no inotify, mtime polling of expanded folders runs only while the sub is unpaused.
  - Events are coalesced for 150 ms and pushed.
- **Liveness.**
  - The helper exits on stdin EOF/EPIPE, when `getppid() == 1`, or after 60 s without a `ping`.
    Rust pings every 30 s, the only timer while idle. ssh without a tty sends no hangup.
  - Reconnect uses backoff that recovers (1 s → 30 s cap), unlike `remote_file_poll_loop`'s permanent
    give-up after 5 failures (`editor.rs:1392-1395`).
- **Fallbacks.**
  - **No `python3`** (exit 127): a stateless `sh -s` per request over the mux, with the script on
    stdin and `find -maxdepth 1` plus stat. The tree is read-only and polled while visible, and
    Health says "limited (no python3)".
  - **Key auth fails:** "needs key-based ssh", with no retry storm.
- **Wrong-host guard.** When `hello.hostname` differs from OSC 7's `cwdHost`, or the agent's cwd
  doesn't exist there, the header warns "Showing ews@nova · this shell is on another machine" (the
  onward-ssh class, `mailink/mod.rs:4112-4119`).
- **Agent marks for remote agents** match on (host_key, absolute remote path). PostToolUse already
  arrives through the tunnel (`lockfile.rs:541`), so rows light and invalidate even with a degraded watcher.
- **Stale-while-revalidate.** The root LRU paints the last listing at once, dimmed until the helper confirms.
- **Python 3.6 floor:** no walrus, `match`, `removeprefix`, `dict |`, built-in generic annotations,
  dataclasses or `functools.cache`. A CI gate parses `helper.py` with `ast` and rejects nodes and
  attribute names from 3.7 onward.

## 16. Settings

There is a new **Files** section in `routes/preferences/+page.svelte`. The fields live on
`Preferences`, with serde defaults. Every value is a bool, a number, or a String/Vec<String>; there
are no enums. **None is in `preference_meta`.** These are the human's view settings, and agents get
the purpose-built tools in §9.6 instead.

| Field | Default | Read by |
|---|---|---|
| `file_tree_enabled` | true | master switch: column, shortcut, MCP tools |
| `file_tree_follow` | true | follow the active tab (§4.2) |
| `file_tree_auto_reveal` | true | reveal the active editor's file |
| `file_tree_preview_tabs` | true | single-click preview |
| `file_tree_show_hidden` | true | client filter |
| `file_tree_show_ignored` | true (dimmed) | client filter |
| `file_tree_exclude` | `[]` (built-ins always apply) | Rust listing |
| `file_tree_git` | true | hub git worker |
| `file_tree_agent_activity` | true | ledger → hub routing, rails |
| `file_tree_turn_baselines` | true | §14 capture |
| `file_tree_agent_tools` | true | MCP gating |
| `file_tree_vim_keys` | false | §11.2 |
| `file_tree_sticky_max` | 5 (0 = off) | sticky ancestors |
| `file_tree_density` | `"normal"` (`"compact"`/`"normal"`/`"comfortable"`) | row height 20/22/24 |
| `file_tree_icons` | `"color"` (`"color"`/`"mono"`) | icons |

## 17. Performance budgets

These are acceptance criteria. Each phase proves its own: Rust budgets by tests on generated
fixtures, frontend budgets by `performance.now()` marks logged on the dev build.

| Path | Budget |
|---|---|
| CPU with no disk changes | **0%**: no timers locally. Exceptions: Linux over-budget polling, network mounts and remote polling, each only while visible and focused, plus the 30 s helper ping |
| Cold open, 100k-file repo, first rows painted | ≤50 ms after the root is resolved (one level listed, nothing walked) |
| Expand a local folder of ≤1k entries | ≤5 ms backend; painted the next frame |
| Folder with 50k entries | first 512 rows ≤30 ms, the rest in 512-entry chunks; ≤80 DOM rows |
| Restore 50 expanded folders (`ft_open` restore) | ≤100 ms |
| Keyboard move | ≤4 ms handler plus render; 60 fps while ↓ is held; a focus move never re-flattens |
| Scroll, 100k rows | 60 fps |
| Expand/collapse splice, 5k-row subtree | ≤2 ms |
| Disk change visible | ≤150 ms local (75/250 ms coalescer), ≤500 ms ssh |
| Git decorations after a save, 10k-file repo | ≤400 ms; ≤1 run per 300 ms per repo; ≤2 git processes app-wide |
| Search keystroke, 100k paths | ≤30 ms; index build ≤400 ms, in the background |
| Agent Pre → "editing now" pulse | ≤150 ms (hook, then 100 ms coalesced, then render) |
| Baseline capture in the hook | ≤25 ms hard deadline; typically <1 ms |
| fds and handles | macOS about 1 per displayed root; Linux ONE inotify instance app-wide; Windows 1 handle per root |
| inotify watches (Linux) | ≤ min(max_user_watches/8, 8192), local and per remote host |
| Memory | snapshots only for subscribed folders; index about 6 MB per 100k paths, evicted after 5 min idle; ledger 2,000 per tab; baselines ≤64 MB |
| Scaling | proportional to **distinct roots on screen**, never to the number of tabs |
| Remote | helper ready ≤250 ms (CM) / about 1 s once (direct); expand ≤1 RTT (about 68 ms on nova); root painted ≤300 ms cold |

## 18. Per-OS summary

| | macOS | Linux | Windows |
|---|---|---|---|
| Watcher | FSEvents, recursive, 1 per root, fixed path set | 1 shared inotify, NonRecursive on expanded folders, budget + visible poll | RDCW recursive, 1 handle per root |
| Live cwd | `proc_pidinfo` (phase 0) | `/proc/<pid>/cwd` | sysinfo; pwsh `Set-Location` is invisible, so prefer agent or saved cwd + pin |
| Paths | NFC compare; case fold per disk | case-sensitive unless probed otherwise | `share::canon` (keeps UNC), drive case, reserved names |
| Trash | NSFileManager; no restore → "Show in Trash" | freedesktop; restore; one thread | Recycle Bin; restore |
| Reveal label | Reveal in Finder | Open Containing Folder | Show in File Explorer |
| Subprocesses | — | — | `CREATE_NO_WINDOW` on every git and ssh spawn |
| ssh | CM mux when bridge on | CM mux when bridge on | no ControlMaster: one direct helper connection |
| Network mounts | statfs fstypename → no watcher | statfs f_type → no watcher | `DRIVE_REMOTE`/UNC → no watcher |

## 19. Explicitly OUT of v1

Each item is listed with its reason.
- **Git writes** (stage, unstage, discard, checkout, commit): `index.lock` races agents, and discard destroys an agent's work.
- **Per-row `+N −N` and folder `+/−` totals:** numstat scales with diff size. Counts appear in the tooltip only.
- **Folder sizes:** unbounded IO, catastrophic on network mounts.
- **Compact folders (`a/b/c`) and file nesting:** both need lookahead listings, and they complicate every per-row target (DnD, rename, menus). v1.1.
- **Multi-root stacked sections:** the root switcher covers v1.
- **Overlay layout:** Loom doctrine.
- **Claude `file-history` as a post-restart baseline source:** undocumented format, a per-account
  store, and corpus-verified gaps. v1.1, behind a parser tested against the real corpus.
- **Remote turn baselines:** a hook must never wait on ssh.
- **Decorations for nested repos and submodules:** shown with a repo badge only.
- **Content search** (find in files), and **QuickOpen on `ft_search`**.
- **Dragging out** to Finder or Explorer (needs a native plugin), and **pasting files from the OS clipboard**.
- **Quick Look**, image thumbnails, and a preview pane in the tree: editor preview tabs cover these.
- **Moving the editor's remote I/O and `watch_file` onto hubs and helpers:** this would retire
  `scp_*`, `remote_file_poll_loop` and one FSEvents stream per editor tab. Follow-up.
- **Windows ssh hosts** for the helper: it needs a POSIX host.
- **maiLink, phone and Loom "turn files" views**, and Overlord collision rules: these need a
  `protocolVersion` bump to 0.24 and are done separately.
- **Automatic selection injection into prompts**, and **any submit from the tree**.

## 20. Build plan

Each phase ships on its own, is tested on its own, and is committed logically, with fixes and prunes
as their own commits. `cargo check --tests` runs after every schema change (exhaustive literals).
Every new command follows the house checklist (`lib.rs`, `types.ts`, `commands.ts`).

### Phase 0: groundwork (no new UI)

**Changes:**
- `pty/manager.rs`: macOS `get_cwd_for_pid` uses `proc_pidinfo`.
- `terminal/osc.rs`: UTF-8 `percent_decode`.
- Create `util/proc.rs` and adopt it in `share/git.rs`.
- `commands/editor.rs`:
  - `git_show_file`: `spawn_blocking`, `/` in `ref:path`, `proc.rs`, a canon strip;
  - `watch_file`: `share::canon` instead of raw `canonicalize` (`:920`).
- Create `src/lib/utils/shellQuote.ts` (POSIX, pwsh and cmd) and drop the private `escapePathForTerminal`.
- Create `src/lib/utils/insertPaths.ts`, and move `TerminalPane.svelte`'s drop path onto it, which
  **adds the `agentDialogOpen` refusal**.
- Create `src/lib/utils/openInEditor.ts`, and adopt it in `claudeCode.handleOpenFile` and `utils/openFile.ts`.
- `agentState.svelte.ts:365`: keep the SessionStart `cwd`.

**Tests:**
- `cargo test`: osc decode with a non-ASCII path; `proc_pidinfo` cwd of the test process equals
  `std::env::current_dir`; `git_show_file` on a path with spaces, against a temp repo.
- Vitest: the quoting tables; the dedupe returns an existing tab.

**Dev check:**
- Drop a file onto a Claude tab while a permission dialog is open: nothing is typed and a toast appears.
- Drop with no dialog: same behaviour as today.

### Phase 1: local read-only Explorer

**Rust:**
- `filetree/{mod,commands,types,paths,place,hub,list,ignore_cache,fsclass,coalesce}.rs`, `watch/*`.
- `WindowData.file_tree_open/width`, `Workspace.file_tree_pin`, and the first batch of `Preferences` fields.
- Setters in `commands/workspace.rs`.
- `relocate/mod.rs`: rebase local `file_tree_pin` values, plus a row in the table in `docs/relocate.md` §2.
- `share/`: confirm `file_tree_pin` stays out of the allowlist.
- `clone_workspace_with_id_mapping` (`commands/window.rs:749`) copies the pin.
- Duplicate window copies open and width.

**Frontend:**
- `filetree/{model,paths,icons,keymap}.ts`, `stores/fileTree.svelte.ts`.
- `components/filetree/{FilesColumn,FilesHeader,RootSwitcher,TreeBody,StickyAncestors}.svelte`.
- `+page.svelte`: the column and the ghost resizer.
- `+layout.svelte`: `isAppShortcut()` extraction, Cmd+Shift+B, `treeFocused` scoping.
- `Icon.svelte`, the titlebar button, "Reveal in Files" in the tab menus.
- The preferences section, `HelpModal.svelte`, and the CLAUDE.md shortcut table plus module-doc line.

**Ships:**
- follow and pin, you-are-here, live watching, shallow mode, network mode, Health footer;
- the virtual ARIA tree, type-ahead, staged Escape, sticky ancestors;
- preview tabs, reveal active file;
- a read-only context menu: Open / Side / Preview / Default app / Reveal / Copy paths / Open Terminal Here / Add to ‹tab›.

**Tests:**
- **Convergence property test** (`filetree/tests/converge.rs`, CI on all three OSes): random
  create/rename/move/delete bursts in a tempdir while a hub serves random subscribed folders. After
  quiescence, client snapshot plus applied deltas must equal a fresh listing. It also runs with the
  event channel forced to overflow, with pause/resume, and with acks withheld (the stall path).
- **Budgets:** a generated 100k-file fixture: root open ≤50 ms, first chunk of a 50k folder ≤30 ms.
  **Idle test:** 10 s with no fs activity gives zero coalescer wakeups.
- **Linux:** watches ≤ budget after expanding 20k folders. An injected low budget falls back to polling and Health reports it.
- **Units:** resolver precedence table (live / stale OSC 7 / agent / fallback / missing → never home /
  service tab skipped); natural sort; ignore stack (nested `.gitignore`, `info/exclude`, negation);
  symlink loop; NFC and case-fold containment.
- **Vitest:** `model.ts` splice and delta algebra (gen mismatch → resync); a 100k-row virtual list smoke test.

**Dev check:**
- With a Claude tab streaming, toggle and resize the column: one resize per gesture and no duplicated scrollback.
- With the tree focused, Cmd+N still opens a window and Cmd+D does nothing.

### Phase 2: git and search

**Changes:**
- `filetree/{git,search}.rs`; `nucleo-matcher = "0.3"`.
- `components/filetree/FindBar.svelte`, the branch row, the Changed lens.
- `ft_diff_base`, and Compare with HEAD via the `openGitDiff` extraction.
- OSC 133 D → git trigger.
- Tooltip numstat in `ft_stat`.

**Tests:**
- porcelain v2 `-z` parser fixtures: renames, conflicts, untracked folders, spaces, newlines, unicode;
- roll-up severity;
- changed-keys-only diffing;
- slow-mode trip and recovery with a stub `git` on PATH;
- a lock test: a concurrent `git commit` loop during refresh storms never sees `index.lock`;
- search ranking fixtures, `seq` cancellation, and a 100k keystroke ≤30 ms.

**Dev check:** a commit typed in a terminal decorates in about 150 ms.

### Phase 3: file operations and DnD

**Changes:**
- `filetree/ops.rs` and the trash worker; `trash = "5"`.
- `ft_op`, `ft_prepare_delete`.
- `components/filetree/{InlineName,ConfirmStrip}.svelte`, `filetree/dnd.ts`.
- Undo stack, editor retargeting, OS drop import, drag to terminal or composer.

**Tests (tempdir):**
- containment escapes (`..`, a symlink to `/etc`, a case-variant prefix, a UNC/verbatim prefix on Windows);
- `create_new` never clobbers;
- case-only rename;
- EXDEV fallback (tmpfs to disk on Linux CI);
- `no_trash` → `needs`, never an rm;
- delete without a token refused, and a reused token refused;
- live-cwd refusal (spawn a child process with cwd inside the folder);
- symlink-not-followed on recursive delete and copy;
- Windows reserved-name validation (Vitest).

**Dev check:**
- Trash then Undo (Linux/Windows), and the "Show in Trash" verb (macOS).
- Renaming a project folder that holds a running agent is refused with Move project.

### Phase 4: agent awareness and MCP

**Changes:**
- `agent_files/{mod,extract}.rs`, fed in the ToolPre/ToolPost/Prompt/Stop arms, with the raw hook
  name read before `normalize_hook_event`.
- Hub routing and `fs_during_tool`.
- `agent_files_remap_tab` in `reloadTab`.
- `components/filetree/{AgentStrip,LensBar,EdgeChips}.svelte`, rails and pulse CSS tokens in `app.css`.
- Turn and Session lenses, follow-agent, conflict badge, the commit flash.
- `protocol.rs`: `ToolGates` refactor (own commit), the three tools.
- `server.rs`: dispatch plus the `OWN_TAB_ONLY_TOOLS` / `PEER_ADDRESSING_TOOLS` entries.
- `claudeCode.svelte.ts` handlers.

**Tests:**
- `extract.rs` against **real captured hook payloads** (Edit, Write, MultiEdit, NotebookEdit,
  Grep/Glob, a Codex `apply_patch` text, a `PostToolUseFailure`), including dedupe by `tool_use_id`;
- 100 ms per-hub coalescing;
- attribution: one in-flight Bash attributes, two don't, ignored paths never do;
- the ledger ring cap; remap;
- tool-list gating;
- `revealInTree` never changes `file_tree_open` and never takes focus;
- own-tab refusal on an inferred identity (the existing test loop at `server.rs:5488-5538` covers the new names).

**Dev check:** a real Claude tab editing files shows the pulse within 150 ms, collapsed folders badge
without expanding, and edge chips appear.

### Phase 5: turn baselines

**Changes:** `agent_files/baseline.rs`, `similar`, `agent_files_baseline`, the Compare with Turn/Session
Start actions, `showDiff` `@turn`/`@session`, `file_tree_turn_baselines`.

**Tests:**
- first-write-only capture per (tab, turn, path);
- a new file gives `None`;
- a 25 ms deadline via an injected slow reader: the hook replies on time and the late result is discarded;
- the 2 MB / binary refusals;
- 64 MB LRU eviction order;
- `@turn` refused for another tab's baseline.

**Dev check:** an agent edits a file twice in one turn, and the turn diff shows the pre-turn content.

### Phase 6: remote roots

**Changes:**
- `filetree/remote/{mod,proto}.rs`, `helper.py`, the loader constant.
- The remote branch of `place.rs`, the host chip, the wrong-host warning, stale-while-revalidate, the sh fallback.

**Tests:**
- the helper runs under python3.6 and 3.8 in CI containers over pipes (frame round trips and fuzzing);
- the AST 3.6 gate;
- the gitignore port shares fixtures with `ignore_cache.rs`;
- the loader under sh, bash, fish and csh quoting.

**Dev check (on `ews@nova`):** cold and warm budgets; inotify degrade at an injected budget; the
helper exits on ssh kill (no orphan in `ps`); bridge off (direct mode); a Windows client against nova.

**Docs:** this file's status block gets an "As built" section per phase, as `docs/stack.md` §11 does.
