//! What crosses the wire between Rust and the Files column (docs/file-tree.md §5).
//!
//! Every path here is ROOT-RELATIVE POSIX (`src/lib/x.ts`, `""` for the root), keyed by the
//! hub's place. Native paths, `\\?\` prefixes, separators and case folding stay in Rust
//! (`paths.rs`); the frontend never splits a path on its own.
//!
//! `TreeEvent` is a serde enum, which the house rule against enums in state does not cover: it
//! is wire-only and never persisted, so an older build can never read it back.

use serde::{Deserialize, Serialize};

/// What the frontend knows about a tab that Rust doesn't keep: terminalsStore's OSC 7 and
/// prompt-derived folders.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct PlaceHints {
    /// terminalsStore `osc.cwd` (OSC 7).
    #[serde(default)]
    pub osc_cwd: Option<String>,
    /// terminalsStore `osc.cwdHost`, the host OSC 7 named.
    #[serde(default)]
    pub osc_host: Option<String>,
    /// terminalsStore `osc.promptCwd`, read off the OSC 0 title by frontend patterns.
    /// Read only once remote folders can be checked for existence (phase 6): until then a fish
    /// `~/D/I/m/x` abbreviation can't be told from a real path, so it is not used.
    #[serde(default)]
    #[allow(dead_code)]
    pub prompt_cwd: Option<String>,
    /// Bypass the 800 ms process-table cache: set on ssh start/exit edges, where a stale
    /// answer is wrong for the whole session.
    #[serde(default)]
    pub fresh: bool,
}

/// Where a tab's tree is rooted, and why.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TreePlace {
    /// The hub key, in `watch::place` form: "/abs" (canonical) or "user@host:/abs".
    pub place: String,
    /// "local" | "remote".
    pub kind: String,
    /// Remote: the ssh destination (`watch::connect_args` form, never a full command line).
    pub host_key: Option<String>,
    /// Remote: the host for a human (`ews@nova`).
    pub host_label: Option<String>,
    /// Native absolute path locally, POSIX absolute (or `~/…`) on a remote host.
    pub root: String,
    /// The tab's own folder relative to the root (the you-are-here tick); None when it IS the
    /// root or isn't under it.
    pub cwd_rel: Option<String>,
    /// Which signal decided: "pty" | "osc7" | "agent" | "last_cwd" | "auto_resume" | "restore"
    /// | "prompt" | "pin".
    pub source: String,
    /// "local" | "network" | "unknown".
    pub fs_class: String,
    /// A home, filesystem or volume root: watched per opened folder only (§6.5).
    pub shallow: bool,
    /// A saved folder that no longer exists. The tree shows it as missing; it is NEVER
    /// replaced by home (docs/relocate.md §4).
    pub missing: Option<String>,
}

/// A folder the client wants, with the generation it already holds.
#[derive(Debug, Clone, Deserialize)]
pub struct DirReq {
    pub rel: String,
    #[serde(default)]
    pub known_gen: Option<u64>,
}

/// The answer to `ft_open`. The events follow on the subscription's Channel.
#[derive(Debug, Clone, Serialize)]
pub struct OpenAck {
    pub sub_id: u64,
    /// The canonical root on this computer, for `relToAbs` in the frontend.
    pub root_native: String,
    /// The native separator, "/" or "\\".
    pub sep: String,
    /// The volume folds case: names compare case-insensitively (validation, dedupe).
    pub case_insensitive: bool,
    /// Names are one name in NFC and NFD here (macOS), and git keys come in NFC while a row's
    /// name is as stored (often NFD): look a row up in the git map by its NFC spelling.
    pub nfc_names: bool,
    pub repo: Option<RepoInfo>,
    pub caps: Caps,
}

/// The git repository the root is in, resolved once per hub.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RepoInfo {
    /// The repo's top level relative to the root: "" when the root IS the top level, `..` steps
    /// when the root is a folder inside a larger repo (a pinned subfolder).
    pub toplevel_rel: String,
    /// Absolute native paths; `.git` can be a file (linked worktree, submodule).
    pub git_dir: String,
    pub common_dir: String,
}

/// What this hub can do.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Caps {
    /// "live" (a watcher covers what is shown), "partial" (some folders polled: watch limit),
    /// "poll" (no watcher: network mount), "none" (no watcher and no poll).
    pub watch: String,
    /// Git decorations: the root is in a repository and `file_tree_git` is on.
    pub git: bool,
    /// Move to Trash is offered. A trash that fails anyway (a network volume, no XDG trash)
    /// answers `needs: no_trash` per op.
    pub trash: bool,
    pub remote: bool,
    /// File operations (`ft_op`) are allowed.
    pub writable: bool,
}

/// One file operation (§10.2). Every path is a rel, checked against the root in Rust, except
/// an `import`'s sources (native absolute paths from an OS drop).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct FileOp {
    /// "create_file" | "create_dir" | "rename" | "move" | "copy" | "duplicate" | "trash" |
    /// "delete" | "import" | "restore".
    pub kind: String,
    #[serde(default)]
    pub src: Vec<String>,
    /// The destination folder (create, move, copy, import); "" or None = the root.
    #[serde(default)]
    pub dest: Option<String>,
    /// create: the new name, where "a/b/c.ts" makes the folders on the way; rename: the new
    /// name, one component.
    #[serde(default)]
    pub name: Option<String>,
    /// Exact destination rels, one per `src`, for a move (undo moves each item back to where
    /// it came from, under the name it had: a multi-select spans folders, and keep-both may
    /// have renamed it). When set, `dest` is ignored.
    #[serde(default)]
    pub to: Vec<String>,
    /// "fail" (default) | "keep_both" (pick a fresh name) | "skip".
    #[serde(default)]
    pub conflict: String,
    /// "delete": the ticket from `ft_prepare_delete`.
    #[serde(default)]
    pub token: Option<String>,
    /// The human confirmed a second time over an agent's in-flight work (§10.3).
    #[serde(default)]
    pub ack_agents: bool,
    /// "restore": when the items went to the trash (unix seconds), to tell them from older
    /// trashed items with the same path.
    #[serde(default)]
    pub trashed_at: Option<i64>,
}

/// What `ft_op` did. Refusals of the whole op (a path outside the root, a bad ticket) are the
/// command's error instead; a guard the human can act on comes back as `needs`, with nothing
/// done.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct OpResult {
    pub done: Vec<OpItem>,
    pub failed: Vec<OpFail>,
    /// Sources left alone by `conflict: "skip"`, or already where they were asked to go.
    pub skipped: Vec<String>,
    /// The op that reverses this one, for the tree's undo stack. None when nothing can (a
    /// permanent delete; a trash on macOS, whose trash can't be restored from).
    pub undo: Option<FileOp>,
    pub needs: Option<Needs>,
}

/// One item done. create: `from` is the new rel, `to` None. move/rename/copy/duplicate/import:
/// `from` the source (native for import), `to` the new rel. trash/delete/restore: `from` the
/// rel, `to` None.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpItem {
    pub from: String,
    pub to: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct OpFail {
    pub rel: String,
    /// "exists" | "gone" | "inside_itself" | "no_trash" | "not_in_trash" | "unsupported" |
    /// "io".
    pub code: String,
    pub msg: String,
}

/// Something the human must decide before the op can run.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Needs {
    /// "no_trash" (offer permanent delete) | "agent_active" (confirm again, `ack_agents`) |
    /// "live_cwd" (refused: Use Move project…).
    pub code: String,
    /// The sentence to show.
    pub detail: String,
    /// The tabs concerned (agent_active, live_cwd).
    pub tabs: Vec<String>,
}

/// `ft_prepare_delete`'s answer: a single-use ticket for exactly these rels, good for 60 s,
/// and what deleting them would remove (the ConfirmStrip shows the counts).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeleteTicket {
    pub token: String,
    pub files: u64,
    pub dirs: u64,
}

/// One file's details, fetched only on hover.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct StatInfo {
    pub size: u64,
    pub mtime_ms: i64,
    /// Any execute bit (unix); false on Windows.
    pub exec: bool,
    /// A symlink's target as written in the link.
    pub link_target: Option<String>,
    /// `git diff --numstat HEAD` for this file only, cached per status run: (added, deleted)
    /// lines. None for a clean, untracked or binary file, or outside a repository.
    pub numstat: Option<(u32, u32)>,
}

/// The branch row (§3.2), from the `# branch.*` headers of a status run.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct BranchInfo {
    /// The branch name, or "(detached)".
    pub head: String,
    /// The commit, or "(initial)" before the first one.
    pub oid: String,
    pub upstream: Option<String>,
    pub ahead: i32,
    pub behind: i32,
}

/// Git decorations for one hub (§7.3). Paths are root-relative like everything else.
/// `full`: replace everything held; otherwise drop `del` from both maps, then set `files` and
/// `dirs` (a key can move from one map to the other: an untracked folder that gains a staged
/// file becomes a folder with a roll-up).
#[derive(Debug, Clone, Serialize, PartialEq, Default)]
pub struct GitUpdate {
    pub gen: u64,
    pub full: bool,
    /// rel → "M" | "A" | "D" | "R" | "C" (conflict) | "?" (untracked) | "T" (type change). A
    /// wholly untracked folder's key ends in "/" (`build/`): everything inside it is untracked.
    pub files: Vec<(String, String)>,
    /// Folders' rolled-up severity, the same letters (never "R" or "T").
    pub dirs: Vec<(String, String)>,
    /// Keys that left the map they were in since the last update.
    pub del: Vec<String>,
    pub branch: Option<BranchInfo>,
    /// Past 20k changed files only `dirs` and `count` are sent.
    pub truncated: bool,
    /// How many changed entries the status run found (the branch row's "7 changed").
    pub count: u32,
}

/// `ft_search`'s answer (§8).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchResult {
    /// The query this answers; an older one than the client's newest is dropped there.
    pub seq: u64,
    /// The index is being built (or rebuilt): `search_ready` follows on the Channel.
    pub indexing: bool,
    pub items: Vec<SearchHit>,
    /// Why there is no index: a home or volume root is indexed only when the human asks.
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SearchHit {
    pub rel: String,
    pub score: u32,
    /// Matched character positions (chars, not bytes) in `rel`, ascending.
    pub idx: Vec<u32>,
}

/// One row of a listing. The short keys matter: 50k entries are ~1.2 MB instead of ~3 MB.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Entry {
    /// The name, as the disk spells it.
    pub n: String,
    /// "f" file, "d" folder, "l" link to a file (or broken), "L" link to a folder, "o" other.
    pub k: &'static str,
    /// Bits: 1 ignored, 2 hidden, 4 link loop (never expandable), 8 unreadable.
    pub f: u8,
}

pub const FLAG_IGNORED: u8 = 1;
pub const FLAG_HIDDEN: u8 = 2;
pub const FLAG_LOOP: u8 = 4;
pub const FLAG_UNREADABLE: u8 = 8;

impl Entry {
    /// A folder or a link to one: sorted first, and expandable unless it loops.
    pub fn is_dirish(&self) -> bool {
        self.k == "d" || self.k == "L"
    }
}

/// Every message on a subscription's Channel carries a sequence number, which the client acks
/// (§5.4).
#[derive(Debug, Clone, Serialize)]
pub struct Envelope {
    pub seq: u64,
    #[serde(flatten)]
    pub ev: TreeEvent,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum TreeEvent {
    /// A full listing, in chunks of 512. Chunk 0 replaces whatever the client held for `rel`.
    Dir { rel: String, gen: u64, chunk: u32, last: bool, total: u32, truncated: bool, entries: Vec<Entry> },
    /// The client's `known_gen` is current.
    NotModified { rel: String, gen: u64 },
    /// Apply only when holding `gen == from`; otherwise `ft_refresh(rel)`. `at` is where each
    /// entry of `add` then `chg` sits in the folder AFTER the delta, so the client places rows
    /// without a comparator of its own (listings are sorted only here, §12.1): drop `del` and
    /// the `chg` names, then insert in ascending `at`.
    Delta { rel: String, from: u64, gen: u64, add: Vec<Entry>, del: Vec<String>, chg: Vec<Entry>, at: Vec<u32> },
    /// "denied" | "gone" | "timeout" (a network folder not answering yet; its listing still
    /// arrives when it does).
    DirError { rel: String, code: String, msg: String },
    /// The hub's health. The footer shows `note` when it is set; absent note = healthy.
    /// `git`: "off" | "live" | "slow" | "error".
    Health { watch: String, git: String, note: Option<String> },
    /// Git decorations changed (or, `full`, everything this subscription should hold).
    Git(GitUpdate),
    /// The search index finished building: a query answered `indexing` should be asked again.
    SearchReady,
    // No `Resync` (§5.3): when events were lost the hub re-lists every folder itself and sends
    // ordinary deltas, which costs the client no round trip.
    /// The root itself was deleted or renamed away. Never followed by a re-root to home.
    RootGone,
}

impl TreeEvent {
    /// The folder this event is about, for the dirty set of a paused subscription.
    pub fn rel(&self) -> Option<&str> {
        match self {
            TreeEvent::Dir { rel, .. }
            | TreeEvent::NotModified { rel, .. }
            | TreeEvent::Delta { rel, .. }
            | TreeEvent::DirError { rel, .. } => Some(rel),
            _ => None,
        }
    }

    /// Roughly what the message costs the webview, for the unacked-bytes stall limit. An
    /// estimate, not a second serialization.
    pub fn approx_bytes(&self) -> usize {
        let entries = |v: &[Entry]| v.iter().map(|e| e.n.len() + 20).sum::<usize>();
        match self {
            TreeEvent::Dir { entries: e, rel, .. } => 80 + rel.len() + entries(e),
            TreeEvent::Delta { add, del, chg, rel, .. } => {
                80 + rel.len() + entries(add) + entries(chg) + del.iter().map(|d| d.len() + 3).sum::<usize>()
            }
            TreeEvent::Git(g) => {
                let pairs = |v: &[(String, String)]| v.iter().map(|(k, _)| k.len() + 10).sum::<usize>();
                160 + pairs(&g.files) + pairs(&g.dirs) + g.del.iter().map(|d| d.len() + 3).sum::<usize>()
            }
            _ => 120,
        }
    }
}
