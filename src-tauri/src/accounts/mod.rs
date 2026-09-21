//! Managed agent accounts — see `docs/login.md`.
//!
//! An account is a config root of its own, handed to one agent runtime through that runtime's
//! config-dir environment variable. maiTerm owns the *directory*; the runtime owns the credential
//! inside it. We never read, write or parse a credential, and on macOS we never touch the
//! runtime's Keychain item.
//!
//! The catch this module exists to handle: those variables relocate the ENTIRE config root. For
//! Claude Code, a bare account directory loses the `SessionStart` hook that establishes tab
//! identity, every MCP server including maiTerm's own, transcript discovery for maiLink, and the
//! user's skills, commands and permissions — all silently. So an account root is mostly a farm
//! of symlinks back to the real config dir.
//!
//! **Mostly, not entirely.** `~/.claude.json` mixes shared configuration (`mcpServers`) with the
//! record of which account is signed in (`oauthAccount`, `userID`), so sharing it hands every
//! account the most recent sign-in's identity — and because the runtime writes *through* a
//! symlink, it also rewrites the user's own file. It gets `Strategy::MergeJson` instead. Two
//! real accounts reporting one email is what surfaced this; see `docs/login.md` §5.4.
//!
//! **The shared set is not static, so reconciling is not a one-off.** maiTerm's own MCP entry in
//! `~/.claude.json` is an ephemeral port and a per-launch token, rewritten every start. A root
//! seeded at account creation dials a dead port from the next launch on, and every managed tab
//! reports the maiTerm server as failed while an unmanaged one works. `resync_roots` re-runs the
//! merge wherever that entry is written; `Strategy::MergeJson`'s `resync` list is what it
//! refreshes. Hooks are unaffected — `settings.json` is a symlink, so it follows on its own.
//!
//! **Claude is the only runtime implemented.** Codex and Gemini are declared from what is
//! observable on disk so the shape is right, but are marked unsupported until the same
//! verification Claude got (§5.4) has been done for them — see `RuntimeProfile::supported`.

pub mod browser;
pub mod remote;
pub mod vault;

use std::fs;
use std::path::{Path, PathBuf};

use crate::state::persistence::app_data_slug;

/// Agent runtimes maiTerm can hold accounts for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Claude,
    Codex,
    Gemini,
    Grok,
}

impl Runtime {
    pub fn slug(&self) -> &'static str {
        match self {
            Runtime::Claude => "claude",
            Runtime::Codex => "codex",
            Runtime::Gemini => "gemini",
            Runtime::Grok => "grok",
        }
    }

    pub fn profile(&self) -> &'static RuntimeProfile {
        match self {
            Runtime::Claude => &CLAUDE,
            Runtime::Codex => &CODEX,
            Runtime::Gemini => &GEMINI,
            Runtime::Grok => &GROK,
        }
    }
}

/// Where an entry lives, relative to the user's home.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Inside the runtime's config dir, e.g. `~/.claude/settings.json`.
    ConfigDir,
    /// At the home root, e.g. `~/.claude.json` — which the config-dir variable relocates anyway.
    /// This case is the one that is easy to miss, so it is modelled rather than special-cased.
    HomeRoot,
}

/// How an entry is shared into an account root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// A symlink back to the user's file. Correct for anything that is purely configuration.
    Link,
    /// A REAL per-account file, seeded from the user's copy with `exclude` stripped, then with
    /// `resync` keys refreshed from the user's copy on every reconcile.
    ///
    /// This exists for `~/.claude.json`, which mixes two things that must not travel together:
    /// `mcpServers` (shared, or a managed tab loses maiTerm's own bridge) and `oauthAccount` +
    /// `userID` (the record of WHICH ACCOUNT is signed in). Symlinking it shares the identity,
    /// so the most recent sign-in speaks for every account — verified: two roots holding two
    /// different credentials both reported the second account's email.
    MergeJson {
        /// Keys never copied from the user's file. Identity lives here.
        exclude: &'static [&'static str],
        /// Keys refreshed from the user's file on every reconcile, so a server added later
        /// reaches existing accounts.
        resync: &'static [&'static str],
    },
}

#[derive(Debug, Clone, Copy)]
pub struct SharedEntry {
    /// Name inside the account root.
    pub name: &'static str,
    pub source: Source,
    pub strategy: Strategy,
}

const fn cfg(name: &'static str) -> SharedEntry {
    SharedEntry { name, source: Source::ConfigDir, strategy: Strategy::Link }
}
const fn home(name: &'static str) -> SharedEntry {
    SharedEntry { name, source: Source::HomeRoot, strategy: Strategy::Link }
}

pub struct RuntimeProfile {
    pub runtime: Runtime,
    pub label: &'static str,
    /// The environment variable that relocates this runtime's config root.
    pub config_env: &'static str,
    /// The runtime's real config dir, relative to home.
    pub config_dir: &'static str,
    /// Entries symlinked back to the real config, shared by every account.
    pub shared: &'static [SharedEntry],
    /// Credential files that must stay per-account and must NEVER be symlinked.
    pub never_link: &'static [&'static str],
    /// The CLI to ask about this runtime's auth state. Resolved via `resolve_cli`, never handed
    /// to `Command::new` bare — see that function.
    pub cli: &'static str,
    /// **Every** environment variable that can answer *instead of* the account root's own
    /// login, and therefore must be removed from both the verification call and a managed
    /// tab's spawn environment.
    ///
    /// This is the full credential-precedence list from `docs/login.md` §2.2 above the stored
    /// login, not a sample of it. A variable missing here does not fail loudly: the runtime
    /// answers from that rung instead, reporting an identity with no `email`/`orgId` at all, so
    /// every account looks identical — the §5.1 duplicate guard then fires on every new account
    /// and the §6.1 verification can never fail. Note rung 4, `apiKeyHelper`, is a *settings
    /// file* key and cannot be scrubbed from the environment at all; `AccountIdentity` handles
    /// that by reporting what answered rather than pretending it is an identity.
    pub shadowing_env: &'static [&'static str],
    /// Can maiTerm manage accounts for this runtime, **on this platform**, today?
    ///
    /// Two reasons it can be false, and they are deliberately one flag rather than two: every
    /// consumer — the setup modal's picker, `begin_account_login`'s refusal, the pane — already
    /// asks this one question, and a second flag they would each have to remember to check is
    /// the shape of bug this module keeps finding.
    ///
    /// 1. **The runtime has not had the §5.4 verification Claude has had** (Codex, Gemini, Grok).
    /// 2. **The platform cannot run it yet.** Windows: `resolve_cli` is Unix-shaped — it joins a
    ///    bare `claude` and `is_executable` says so itself ("Windows would need the PATHEXT
    ///    dance") — while an npm install puts `claude.cmd` on PATH, which `Command::new` never
    ///    finds because it only ever appends `.exe`. So no sign-in, no verify, no sign-out. On
    ///    top of that `reconcile`'s symlink farm needs Developer Mode or elevation there, and
    ///    `browser::suppress_default_browser` cannot shim a PowerShell launch.
    ///
    /// An unsupported runtime is shown in the UI as not yet available, never silently
    /// half-wired. That is the whole point: until 2026-09-20 the env injection sat inside the
    /// `#[cfg(unix)]` PTY builder, so a Windows user could complete setup and watch the row go
    /// green while every tab kept using their normal login.
    pub supported: bool,
}

/// Verified on 2.1.278, macOS, 2026-09-19 — `docs/login.md` §5.4 has the evidence for each entry.
static CLAUDE: RuntimeProfile = RuntimeProfile {
    runtime: Runtime::Claude,
    label: "Claude Code",
    config_env: "CLAUDE_CONFIG_DIR",
    config_dir: ".claude",
    shared: &[
        cfg("settings.json"),
        cfg("settings.local.json"),
        SharedEntry {
            name: ".claude.json",
            source: Source::HomeRoot,
            // Seeded once so project trust and history carry over, then only mcpServers is kept
            // in step. oauthAccount and userID are never copied: they say which account is
            // signed in, and copying them makes every account claim the same one.
            strategy: Strategy::MergeJson {
                exclude: &["oauthAccount", "userID"],
                resync: &["mcpServers"],
            },
        },
        cfg("projects"),
        cfg("ide"),
        cfg("commands"),
        cfg("skills"),
        cfg("plugins"),
        cfg("CLAUDE.md"),
        cfg("statusline-command.sh"),
    ],
    never_link: &[".credentials.json"],
    cli: "claude",
    // docs/login.md §2.2, rungs 1-6, plus the credential-dir override from §2.1.1.
    shadowing_env: &[
        // 1. cloud provider — answers as `third_party`, no email at all
        "CLAUDE_CODE_USE_BEDROCK",
        "CLAUDE_CODE_USE_VERTEX",
        "CLAUDE_CODE_USE_FOUNDRY",
        // 2-3. bearer token / API key
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        // 5. the long-lived token — the one THIS FEATURE mints and injects on remotes (§6),
        //    and the one a user is told to export in their shell profile
        "CLAUDE_CODE_OAUTH_TOKEN",
        "CLAUDE_CODE_OAUTH_TOKEN_FILE_DESCRIPTOR",
        // 6. Anthropic profile / workload identity federation
        "ANTHROPIC_PROFILE",
        "ANTHROPIC_FEDERATION_RULE_ID",
        "ANTHROPIC_ORGANIZATION_ID",
        // §2.1.1 — overrides the credential dir independently of CLAUDE_CONFIG_DIR, collapsing
        // every account onto one login
        "CLAUDE_SECURESTORAGE_CONFIG_DIR",
    ],
    // Verified on macOS; Linux is written but unrun. NOT Windows — see `supported`'s docs for
    // the three things that have to land first. `cfg!` is const-evaluable, so the gate lives
    // here beside the profile rather than in every caller.
    supported: cfg!(unix),
};

/// Shape taken from a real `~/.codex` and `codex --help`; **not yet verified**.
/// Open before flipping `supported`: does `CODEX_HOME` relocate everything the way
/// `CLAUDE_CONFIG_DIR` does, is `auth.json` symlink-refused like Claude's credential, and does
/// Codex's own `--profile` (which layers `$CODEX_HOME/<name>.config.toml`) make config-root
/// swapping unnecessary here?
static CODEX: RuntimeProfile = RuntimeProfile {
    runtime: Runtime::Codex,
    label: "Codex",
    config_env: "CODEX_HOME",
    config_dir: ".codex",
    shared: &[
        cfg("config.toml"),
        cfg("hooks"),
        cfg("AGENTS.md"),
        cfg("history.jsonl"),
    ],
    never_link: &["auth.json"],
    cli: "codex",
    shadowing_env: &[],
    supported: false,
};

/// Shape taken from a real `~/.gemini`; **not yet verified**, and no config-dir variable has
/// been identified — that is the first thing to find before this can work at all.
static GEMINI: RuntimeProfile = RuntimeProfile {
    runtime: Runtime::Gemini,
    label: "Gemini CLI",
    config_env: "",
    config_dir: ".gemini",
    shared: &[cfg("settings.json"), cfg("commands"), cfg("GEMINI.md")],
    never_link: &["oauth_creds.json", "google_accounts.json"],
    cli: "gemini",
    shadowing_env: &[],
    supported: false,
};

/// Placeholder. Nothing observed — not installed here.
static GROK: RuntimeProfile = RuntimeProfile {
    runtime: Runtime::Grok,
    label: "Grok",
    config_env: "",
    config_dir: ".grok",
    shared: &[],
    never_link: &[],
    cli: "",
    shadowing_env: &[],
    supported: false,
};

pub static ALL_RUNTIMES: &[Runtime] =
    &[Runtime::Claude, Runtime::Codex, Runtime::Gemini, Runtime::Grok];

impl RuntimeProfile {
    fn source_for(&self, entry: &SharedEntry, home_dir: &Path) -> PathBuf {
        match entry.source {
            Source::HomeRoot => home_dir.join(entry.name),
            Source::ConfigDir => home_dir.join(self.config_dir).join(entry.name),
        }
    }
}

/// `<data>/<slug>/accounts` — the parent of every account root.
pub fn accounts_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("accounts"))
}

/// The config root for one account. Namespaced by runtime so the same account id can exist for
/// two runtimes without collision.
pub fn account_root(runtime: Runtime, account_id: &str) -> Option<PathBuf> {
    accounts_dir().map(|p| p.join(runtime.slug()).join(account_id))
}

/// What `reconcile` did, for logging and for surfacing drift in the Accounts pane.
#[derive(Debug, Default, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Reconciled {
    /// Links created or repointed this pass.
    pub linked: Vec<String>,
    /// Entries skipped because the user has no such file — not an error.
    pub absent: Vec<String>,
    /// Real files found where a symlink belongs, renamed aside rather than deleted.
    pub displaced: Vec<String>,
}

/// Create or repair an account root.
///
/// Deliberately a reconciler, not a one-shot: the user installs skills and plugins after an
/// account exists, and config dirs gain entries across runtime versions, so this runs on every
/// spawn. Idempotent.
///
/// Drift is repaired **non-destructively**. A real file where a symlink belongs is renamed aside,
/// never removed — it may be the only copy of something, and this module is not entitled to that
/// call.
pub fn reconcile(runtime: Runtime, account_id: &str, home_dir: &Path) -> Result<Reconciled, String> {
    let profile = runtime.profile();
    if !profile.supported {
        return Err(format!("{} accounts are not supported yet", profile.label));
    }
    let root = account_root(runtime, account_id)
        .ok_or_else(|| "no data directory available".to_string())?;
    reconcile_at(profile, &root, home_dir)
}

/// `reconcile`, against an explicit profile and root. Split out so tests need no real data dir,
/// and so an unsupported runtime can still be exercised in tests.
pub fn reconcile_at(
    profile: &RuntimeProfile,
    root: &Path,
    home_dir: &Path,
) -> Result<Reconciled, String> {
    debug_assert!(
        !profile
            .shared
            .iter()
            .any(|e| profile.never_link.contains(&e.name)),
        "{}: a credential file is in the shared set — that would merge every account",
        profile.label
    );

    fs::create_dir_all(root).map_err(|e| format!("creating {}: {e}", root.display()))?;

    let mut out = Reconciled::default();

    for entry in profile.shared {
        let source = profile.source_for(entry, home_dir);
        if !source.exists() {
            out.absent.push(entry.name.to_string());
            continue;
        }

        let dest = root.join(entry.name);

        match entry.strategy {
            Strategy::MergeJson { exclude, resync } => {
                // A symlink here is drift from an older build and is exactly the bug this
                // strategy exists to fix — unlink it rather than writing through it to the
                // user's real file.
                if fs::read_link(&dest).is_ok() {
                    fs::remove_file(&dest)
                        .map_err(|e| format!("unlinking {}: {e}", dest.display()))?;
                }
                if merge_json(&source, &dest, exclude, resync)? {
                    out.linked.push(entry.name.to_string());
                }
            }
            Strategy::Link => {
                // Compare the link target, not the resolved path — resolving would call a
                // correct link "wrong" whenever the source is itself a symlink.
                match fs::read_link(&dest) {
                    Ok(target) if target == source => continue,
                    Ok(_) => {
                        fs::remove_file(&dest)
                            .map_err(|e| format!("removing stale link {}: {e}", dest.display()))?;
                    }
                    Err(_) if dest.exists() => {
                        let aside = displaced_name(&dest);
                        fs::rename(&dest, &aside)
                            .map_err(|e| format!("displacing {}: {e}", dest.display()))?;
                        out.displaced.push(entry.name.to_string());
                    }
                    Err(_) => {}
                }

                symlink(&source, &dest).map_err(|e| {
                    format!("linking {} -> {}: {e}", dest.display(), source.display())
                })?;
                out.linked.push(entry.name.to_string());
            }
        }
    }

    Ok(out)
}

/// Locate a runtime's CLI as an absolute path.
///
/// `Command::new("claude")` is a trap here. It works under `npm run tauri:dev`, which inherits
/// the terminal's `PATH`, and fails in the installed build: a Finder or Dock launch gets
/// launchd's `PATH` — `/usr/bin:/bin:/usr/sbin:/sbin` — which contains neither `~/.local/bin`
/// (where Claude Code's native installer puts it) nor Homebrew. Dev-clean, broken after deploy,
/// which is the repo's standing "runs against the INSTALLED build" trap.
pub fn resolve_cli(profile: &RuntimeProfile) -> Option<PathBuf> {
    if profile.cli.is_empty() {
        return None;
    }
    // PATH first, so a user's own install wins over anything we guess at.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            let candidate = dir.join(profile.cli);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(h) = dirs::home_dir() {
        // Claude Code's native install location, then the usual per-user bins.
        for d in [".local/bin", "bin", ".bun/bin", ".volta/bin", ".npm-global/bin"] {
            candidates.push(h.join(d).join(profile.cli));
        }
    }
    for d in ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"] {
        candidates.push(Path::new(d).join(profile.cli));
    }
    candidates.into_iter().find(|c| is_executable(c))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(p)
            .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        // Windows would need the PATHEXT dance; no runtime is supported there yet.
        p.is_file()
    }
}

/// Unlink any entry in an account root that is a symlink where a real per-account file belongs.
///
/// Call this before running ANY runtime command against a root. A root built by an earlier build
/// has `.claude.json` as a symlink to the user's real file, and the runtime writes *through* it:
/// observed live, `auth logout` against such a root stripped the identity block out of the
/// user's own `~/.claude.json`. Reconcile fixes this, but sign-out happens on the way to
/// deleting the root, where reconciling it first would be absurd.
///
/// Unlinking is enough — the command then writes a fresh file local to the root, which is about
/// to be removed anyway.
pub fn detach_write_through_links(runtime: Runtime, account_id: &str) -> Result<(), String> {
    let Some(root) = account_root(runtime, account_id) else {
        return Ok(());
    };
    for entry in runtime.profile().shared {
        if !matches!(entry.strategy, Strategy::MergeJson { .. }) {
            continue;
        }
        let path = root.join(entry.name);
        if fs::read_link(&path).is_ok() {
            fs::remove_file(&path)
                .map_err(|e| format!("unlinking {}: {e}", path.display()))?;
        }
    }
    Ok(())
}

/// Delete an account's config root.
///
/// **The dangerous operation in this module.** The root is a farm of symlinks into the user's
/// real config — `projects` points at their transcripts, `.claude.json` at 200KB of project
/// state. Anything that follows those links while deleting destroys data that is not ours and
/// has no copy. So: symlinks are unlinked, never traversed, and real directories are recursed
/// into only after `symlink_metadata` confirms they are not links.
///
/// Refuses any path outside `accounts_dir()`, so a bad id cannot aim this at the home directory.
pub fn remove_root(runtime: Runtime, account_id: &str) -> Result<(), String> {
    let parent = accounts_dir().ok_or_else(|| "no data directory available".to_string())?;
    let root = account_root(runtime, account_id)
        .ok_or_else(|| "no data directory available".to_string())?;

    // The id must be ONE ordinary path component. Checking `starts_with` alone is not enough:
    // it compares components lexically, so `<accounts>/claude/..` "starts with" `<accounts>`
    // while actually BEING it — an id of ".." would delete every account. `Component::Normal`
    // excludes `.`, `..`, absolute roots and Windows prefixes in one test.
    let is_simple_name = {
        let mut comps = Path::new(account_id).components();
        matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
    };
    if !is_simple_name || !root.starts_with(&parent) {
        return Err(format!(
            "refusing to remove account root for id {account_id:?}: not a simple name"
        ));
    }
    if !root.exists() {
        return Ok(());
    }
    remove_tree_without_following(&root)
}

/// Re-reconcile every account root that exists for a runtime, returning how many were visited.
///
/// **Why this has to be event-driven rather than done once at account creation.** The shared
/// entries are not static. `~/.claude.json`'s `mcpServers` carries maiTerm's own MCP server as an
/// **ephemeral port and a per-launch auth token**, rewritten every time maiTerm starts (and
/// re-asserted on a timer when the `claude` CLI clobbers it). An account root seeded at creation
/// then holds a dead port forever: observed 2026-09-20, two roots pointing at ports 31271 and
/// 17680 while the live server was on 57173, so every managed tab reported the maiTerm MCP server
/// as failed while an unmanaged one worked.
///
/// `Strategy::MergeJson`'s `resync` list exists precisely for this; it was simply never re-run.
/// Called wherever maiTerm writes that entry, so the roots follow it.
///
/// Enumerates the directory rather than the account list: a root that preferences no longer knows
/// about is exactly the one nothing else will fix, and reconciling it costs a no-op.
pub fn resync_roots(runtime: Runtime, home: &Path) -> Result<usize, String> {
    let Some(dir) = accounts_dir().map(|d| d.join(runtime.slug())) else {
        return Ok(0);
    };
    if !dir.exists() {
        return Ok(0);
    }
    let mut visited = 0usize;
    for entry in fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        if !entry.path().is_dir() {
            continue;
        }
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        match reconcile(runtime, &id, home) {
            Ok(_) => visited += 1,
            // One bad root must not stop the rest from being refreshed.
            Err(e) => log::warn!("accounts: resyncing root {id}: {e}"),
        }
    }
    Ok(visited)
}

/// Delete every root under a runtime that no account claims, returning the ids removed.
///
/// **Removing an account does not stop the sessions already running under it.** A `claude`
/// process holds its `CLAUDE_CONFIG_DIR` for its whole life, so after `remove_root` deletes the
/// tree the next periodic write from that process *recreates* it — as a bare real directory, no
/// symlink farm — and maiTerm has already forgotten the account, so nothing will ever clean it
/// up. Observed 2026-09-20: a root resurrected 30 minutes after "Clear setup", holding a session
/// sidecar, still on disk after a second clear. Roots accumulate that way, and a credential
/// refresh from such a session re-mints a Keychain item keyed by the deleted path — an account
/// the user believes is gone, under a uuid nothing tracks.
///
/// Called at startup, where `keep` comes from the preferences this process just loaded, so there
/// is no window in which an empty list means "not loaded yet" rather than "no accounts". Anything
/// resurrected after the sweep is caught by the next launch, by which time the process that did
/// it is gone.
pub fn prune_orphan_roots(runtime: Runtime, keep: &[String]) -> Result<Vec<String>, String> {
    let Some(dir) = accounts_dir().map(|d| d.join(runtime.slug())) else {
        return Ok(Vec::new());
    };
    prune_orphan_roots_at(&dir, keep)
}

/// `prune_orphan_roots` against an explicit runtime directory, so it can be tested without
/// reaching for the real data dir.
fn prune_orphan_roots_at(dir: &Path, keep: &[String]) -> Result<Vec<String>, String> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        let Some(id) = entry.file_name().to_str().map(str::to_string) else {
            continue;
        };
        if keep.iter().any(|k| k == &id) {
            continue;
        }
        // The same simple-name test `remove_root` applies. `read_dir` cannot hand back `..`, but
        // this function's whole job is deleting directories it was not given by name, so it does
        // not rely on that.
        let simple = {
            let mut comps = Path::new(&id).components();
            matches!(comps.next(), Some(std::path::Component::Normal(_))) && comps.next().is_none()
        };
        if !simple {
            log::warn!("accounts: refusing to prune {id:?}: not a simple name");
            continue;
        }
        let path = dir.join(&id);
        // Never follow: a root abandoned mid-reconcile still holds links pointing at the user's
        // real transcripts and project state.
        match fs::symlink_metadata(&path) {
            // A stray symlink where a root should be — unlink it, do not walk it.
            Ok(m) if m.file_type().is_symlink() => {
                if let Err(e) = fs::remove_file(&path).or_else(|_| fs::remove_dir(&path)) {
                    log::warn!("accounts: unlinking stray {id}: {e}");
                    continue;
                }
                removed.push(id);
            }
            Ok(m) if m.is_dir() => match remove_tree_without_following(&path) {
                Ok(()) => removed.push(id),
                // One unreadable entry must not strand the rest of the sweep.
                Err(e) => log::warn!("accounts: pruning orphan root {id}: {e}"),
            },
            // A loose file in here is not ours to delete.
            Ok(_) => log::warn!("accounts: ignoring non-directory entry {id}"),
            Err(e) => log::warn!("accounts: stat {id}: {e}"),
        }
    }
    Ok(removed)
}

fn remove_tree_without_following(dir: &Path) -> Result<(), String> {
    for entry in fs::read_dir(dir).map_err(|e| format!("reading {}: {e}", dir.display()))? {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        let path = entry.path();
        // symlink_metadata does NOT follow — that distinction is the whole point here.
        let meta = fs::symlink_metadata(&path)
            .map_err(|e| format!("stat {}: {e}", path.display()))?;
        if meta.file_type().is_symlink() {
            // Unlinks the link itself. On Windows a directory symlink needs remove_dir.
            fs::remove_file(&path)
                .or_else(|_| fs::remove_dir(&path))
                .map_err(|e| format!("unlinking {}: {e}", path.display()))?;
        } else if meta.is_dir() {
            remove_tree_without_following(&path)?;
        } else {
            fs::remove_file(&path).map_err(|e| format!("removing {}: {e}", path.display()))?;
        }
    }
    fs::remove_dir(dir).map_err(|e| format!("removing {}: {e}", dir.display()))
}

/// The environment a spawned tab needs for this account: the config-dir variable pointed at the
/// account root, plus the variables that must be *removed* to keep isolation honest.
pub fn spawn_env(
    runtime: Runtime,
    account_id: &str,
) -> Option<(Vec<(String, String)>, Vec<String>)> {
    let profile = runtime.profile();
    if !profile.supported || profile.config_env.is_empty() {
        return None;
    }
    let root = account_root(runtime, account_id)?;
    Some((
        vec![(profile.config_env.to_string(), root.to_string_lossy().into_owned())],
        // The SAME list the verifier scrubs. If these differ, the pane reports the identity the
        // root holds while the tab runs as whatever a leftover variable resolves to — a green
        // "signed in as alice" over a session billed to someone else.
        profile.shadowing_env.iter().map(|s| s.to_string()).collect(),
    ))
}

/// Seed-once, resync-always for a JSON file that mixes shared configuration with per-account
/// identity.
///
/// On first run the account gets a copy of the user's file with `exclude` stripped, so project
/// trust and history carry over but nothing says which account is signed in. On every run after
/// that, only `resync` keys are refreshed, so an MCP server added later reaches existing accounts
/// without touching anything the account has since written about itself.
///
/// Returns whether the destination changed.
fn merge_json(
    source: &Path,
    dest: &Path,
    exclude: &[&str],
    resync: &[&str],
) -> Result<bool, String> {
    let src_raw = fs::read_to_string(source)
        .map_err(|e| format!("reading {}: {e}", source.display()))?;
    let src: serde_json::Value = serde_json::from_str(&src_raw)
        .map_err(|e| format!("parsing {}: {e}", source.display()))?;
    let src_obj = src.as_object().ok_or_else(|| format!("{} is not a JSON object", source.display()))?;

    // `read_to_string(..).ok()` alone cannot tell "no file yet" from "the file is there and we
    // could not read it", and those need opposite answers: the first seeds, the second must
    // never seed. `symlink_metadata` answers it without following — a symlink here is drift the
    // caller has already unlinked, and following one would reach the user's real file.
    let dest_present = fs::symlink_metadata(dest).is_ok();
    let existing = fs::read_to_string(dest).ok();
    if dest_present && existing.is_none() {
        return Err(format!(
            "{} exists but could not be read — refusing to reseed it",
            dest.display()
        ));
    }
    let mut out = match existing.as_deref().map(serde_json::from_str::<serde_json::Value>) {
        Some(Ok(serde_json::Value::Object(o))) => o,
        // **A destination that EXISTS but does not parse is not seeded over.** Seeding means
        // "copy the user's file minus identity", which for an existing account silently
        // replaces its `oauthAccount` and its own project state — the exact "two accounts, one
        // email" failure this strategy exists to prevent, with no displaced copy to recover
        // from. Unreadable is transient far more often than it is terminal: a partial read of a
        // concurrent write by the `claude` process that co-owns this file looks identical. So
        // refuse, and let the next reconcile try again.
        //
        // Seeding stays correct for an ABSENT destination, which is a new account root.
        Some(_) => {
            return Err(format!(
                "{} exists but is not readable JSON — refusing to reseed it, which would \
                 replace this account's identity with the user's",
                dest.display()
            ))
        }
        None => {
            let mut seeded = src_obj.clone();
            for k in exclude {
                seeded.remove(*k);
            }
            seeded
        }
    };

    for k in resync {
        match src_obj.get(*k) {
            Some(v) => {
                out.insert((*k).to_string(), v.clone());
            }
            None => {
                out.remove(*k);
            }
        }
    }
    // `exclude` applies ONLY when seeding from the user's file. Re-applying it here would strip
    // the identity the runtime wrote for THIS account on every reconcile — deleting the very
    // thing this strategy exists to keep separate. Caught by a test rather than by a user
    // wondering why their account forgot itself.

    let rendered = serde_json::to_string_pretty(&serde_json::Value::Object(out))
        .map_err(|e| format!("rendering {}: {e}", dest.display()))?;
    if existing.as_deref() == Some(rendered.as_str()) {
        return Ok(false);
    }
    // Write through a temp file in the same directory: a half-written config root is worse than
    // an unchanged one, and the runtime may be reading this file as we go.
    let tmp = dest.with_extension("maiterm-tmp");
    fs::write(&tmp, &rendered).map_err(|e| format!("writing {}: {e}", tmp.display()))?;
    fs::rename(&tmp, dest).map_err(|e| format!("replacing {}: {e}", dest.display()))?;
    Ok(true)
}

/// The env changes a newly spawned tab needs for every runtime the user has an active account
/// for: variables to set, and variables to remove.
///
/// Returns nothing at all unless setup is complete **and** the feature is enabled — a
/// configured-but-disabled state must inject nothing (§10), and that rule lives here rather
/// than at each call site.
///
/// An `active_account_ids` entry naming an account that is no longer in the list is ignored
/// rather than honoured: a dangling pointer must not silently hand a tab a config root that
/// nothing owns.
pub fn spawn_env_for(prefs: &crate::state::Preferences) -> (Vec<(String, String)>, Vec<String>) {
    let mut set = Vec::new();
    let mut unset = Vec::new();
    if !prefs.accounts_setup_complete || !prefs.accounts_enabled {
        return (set, unset);
    }
    for runtime in ALL_RUNTIMES {
        let Some(account_id) = prefs.active_account_ids.get(runtime.slug()) else {
            continue;
        };
        let known = prefs
            .managed_accounts
            .iter()
            .any(|a| &a.id == account_id && a.runtime == runtime.slug());
        if !known {
            continue;
        }
        if let Some((s, u)) = spawn_env(*runtime, account_id) {
            set.extend(s);
            unset.extend(u);
        }
    }
    (set, unset)
}

/// Which account's remote token should a tab on `host` use? (§6)
///
/// **Only ever the ACTIVE account.** This is the same `active_account_ids` pointer that decides
/// what a local tab launches as, deliberately: one identity is current at a time, and it is
/// current everywhere. Switching accounts switches local tabs and remote ones together, which
/// is the behaviour people expect from a thing called "the active account" and is the only one
/// that stays comprehensible once several hosts are in play.
///
/// It also dissolves a problem rather than solving it. An earlier version searched every
/// account for one naming the host, which meant two accounts could both claim `nova` and
/// something had to arbitrate — and per §6.1 a wrong arbitration is invisible, because the tab
/// does not fail, it comes up as the other identity. With only the active account consulted
/// there is nothing to arbitrate: `remote_hosts` answers *whether* to propagate, never *which*.
///
/// So the question here is narrow: does the active account have a token, and is this host one
/// it is allowed to reach?
///
/// 1. The active account for the runtime, if it has a minted token.
/// 2. …and the host is named in its `remote_hosts`, or it has `remote_all_hosts`.
/// 3. Otherwise **nothing** — the host keeps whatever login it already has. That answer must
///    stay reachable: an unwanted token does not announce itself, it quietly becomes the
///    identity the remote agent runs as.
///
/// A bare `host` matches `user@host`, so `nova` covers every user you ssh in as, while
/// `ews@nova` matches only that pairing. Comparison is case-insensitive, because hostnames are.
pub fn remote_account_for_host<'a>(
    prefs: &'a crate::state::Preferences,
    host: &str,
) -> Option<&'a crate::state::ManagedAccount> {
    if !prefs.accounts_setup_complete || !prefs.accounts_enabled {
        return None;
    }
    let host = host.trim();
    if host.is_empty() {
        return None;
    }
    // Claude is the only runtime that can mint (§6), so the active Claude account is the one
    // this asks about. When another runtime ships, this takes the runtime as an argument.
    let active_id = prefs.active_account_ids.get(Runtime::Claude.slug())?;
    let account = prefs
        .managed_accounts
        .iter()
        .find(|a| &a.id == active_id && a.runtime == Runtime::Claude.slug())?;

    // No token means no account, never "the account whose token is gone": a host list that
    // outlived its token is a promise nothing can keep, and handing the account back would
    // produce exactly the silent fall-through this function exists to avoid.
    account.token_minted_at?;

    let covered = account.remote_all_hosts
        || account.remote_hosts.iter().any(|h| host_matches(h, host));
    covered.then_some(account)
}

/// Does an enabled entry cover this ssh target?
///
/// `enabled` is what the user typed; `target` is the tab's destination. A bare hostname covers
/// every user on that host; a `user@host` entry covers only that pairing.
fn host_matches(enabled: &str, target: &str) -> bool {
    let e = enabled.trim();
    if e.is_empty() {
        return false;
    }
    if e.eq_ignore_ascii_case(target) {
        return true;
    }
    // A bare entry matches the host part of `user@host`. Not the reverse: `ews@nova` must not
    // capture `root@nova`, which is a different account on the same box and quite possibly the
    // reason someone listed one and not the other.
    if !e.contains('@') {
        if let Some((_, host)) = target.rsplit_once('@') {
            return e.eq_ignore_ascii_case(host);
        }
    }
    false
}

/// A collision-free name to park a displaced real file under, beside where it was.
fn displaced_name(link: &Path) -> PathBuf {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let name = link.file_name().and_then(|n| n.to_str()).unwrap_or("entry");
    let mut candidate = link.with_file_name(format!("{name}.displaced-{stamp}"));
    let mut n = 1;
    while candidate.exists() {
        candidate = link.with_file_name(format!("{name}.displaced-{stamp}-{n}"));
        n += 1;
    }
    candidate
}

#[cfg(unix)]
fn symlink(source: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, link)
}

#[cfg(windows)]
fn symlink(source: &Path, link: &Path) -> std::io::Result<()> {
    if source.is_dir() {
        std::os::windows::fs::symlink_dir(source, link)
    } else {
        std::os::windows::fs::symlink_file(source, link)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// A home directory with the entries a real Claude user would have.
    fn fake_home(base: &Path) -> PathBuf {
        let home = base.join("home");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(home.join(".claude").join("settings.json"), "{}").unwrap();
        fs::write(home.join(".claude.json"), "{}").unwrap();
        fs::create_dir_all(home.join(".claude").join("projects")).unwrap();
        fs::create_dir_all(home.join(".claude").join("ide")).unwrap();
        home
    }

    /// A private directory per call. Tests run as parallel threads in one process, and the
    /// system clock is coarser than the rate they call this, so a timestamp alone collides —
    /// two tests then reconcile the same root and trip over each other. The counter makes it
    /// deterministic.
    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let p = std::env::temp_dir().join(format!(
            "maiterm-accounts-test-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn no_runtime_shares_its_credential() {
        // The one mistake that would silently merge every account, for every runtime present
        // and every runtime added later. This is where someone finds out.
        for rt in ALL_RUNTIMES {
            let p = rt.profile();
            for entry in p.shared {
                assert!(
                    !p.never_link.contains(&entry.name),
                    "{}: {} is both shared and a credential",
                    p.label,
                    entry.name
                );
            }
        }
    }

    #[test]
    fn supported_runtimes_declare_a_config_env() {
        // A supported runtime with no config variable would reconcile a directory that nothing
        // ever reads — succeeding while doing nothing.
        for rt in ALL_RUNTIMES {
            let p = rt.profile();
            if p.supported {
                assert!(!p.config_env.is_empty(), "{} is supported but has no config env", p.label);
                assert!(!p.shared.is_empty(), "{} is supported but shares nothing", p.label);
            }
        }
    }

    #[test]
    fn claude_shares_the_entries_that_break_silently() {
        // Each of these was verified to matter in docs/login.md §5.4. Losing one does not error,
        // it just quietly removes a capability, which is why they are asserted by name.
        let names: Vec<_> = CLAUDE.shared.iter().map(|e| e.name).collect();
        for required in ["settings.json", ".claude.json", "projects", "ide"] {
            assert!(names.contains(&required), "claude must share {required}");
        }
        // .claude.json is at the home root, not inside ~/.claude — the easy one to get wrong.
        let entry = CLAUDE.shared.iter().find(|e| e.name == ".claude.json").unwrap();
        assert_eq!(entry.source, Source::HomeRoot);
    }

    #[test]
    fn links_the_entries_that_exist_and_reports_the_rest() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("account-a");

        let r = reconcile_at(&CLAUDE, &root, &home).unwrap();

        for e in ["settings.json", ".claude.json", "projects", "ide"] {
            assert!(r.linked.contains(&e.to_string()), "expected {e} linked, got {r:?}");
        }
        // The home-root entry resolves to ~/.claude.json, not ~/.claude/.claude.json. It is a
        // merged file rather than a link (see the identity tests), so assert on its content.
        assert!(
            fs::read_link(root.join(".claude.json")).is_err(),
            ".claude.json is merged, not linked"
        );
        assert!(root.join(".claude.json").is_file());
        assert!(r.absent.contains(&"skills".to_string()));
        assert!(r.displaced.is_empty());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn is_idempotent() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("account-a");

        reconcile_at(&CLAUDE, &root, &home).unwrap();
        let second = reconcile_at(&CLAUDE, &root, &home).unwrap();

        assert!(second.linked.is_empty(), "expected no relinking, got {second:?}");
        assert!(second.displaced.is_empty());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn repoints_a_link_aimed_somewhere_else() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("account-a");
        fs::create_dir_all(&root).unwrap();
        let wrong = base.join("wrong.json");
        fs::write(&wrong, "{}").unwrap();
        symlink(&wrong, &root.join("settings.json")).unwrap();

        let r = reconcile_at(&CLAUDE, &root, &home).unwrap();

        assert!(r.linked.contains(&"settings.json".to_string()));
        assert_eq!(
            fs::read_link(root.join("settings.json")).unwrap(),
            home.join(".claude").join("settings.json")
        );
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn displaces_a_real_file_instead_of_deleting_it() {
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("account-a");
        fs::create_dir_all(&root).unwrap();
        // A real file where a LINKED entry belongs. It may be the only copy of that state, so
        // the reconciler parks it rather than deleting it. (`.claude.json` takes the merge path
        // instead and is covered by its own tests.)
        fs::write(root.join("settings.json"), "irreplaceable").unwrap();

        let r = reconcile_at(&CLAUDE, &root, &home).unwrap();

        assert!(r.displaced.contains(&"settings.json".to_string()));
        let parked: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("settings.json.displaced-"))
            .collect();
        assert_eq!(parked.len(), 1, "the real file must survive, parked aside");
        assert_eq!(fs::read_to_string(parked[0].path()).unwrap(), "irreplaceable");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn claude_json_is_merged_not_linked_so_accounts_keep_separate_identities() {
        // The defect this prevents, observed live: with `.claude.json` symlinked, two roots
        // holding two DIFFERENT credentials both reported the second account's email, because
        // oauthAccount is stored in that file and the most recent sign-in rewrote it.
        let base = tmp();
        let home = fake_home(&base);
        fs::write(
            home.join(".claude.json"),
            r#"{"mcpServers":{"maiterm":{"url":"http://x"}},"oauthAccount":{"emailAddress":"a@b.c"},"userID":"u1","projects":{"/p":{"trusted":true}}}"#,
        )
        .unwrap();
        let root = base.join("account-a");

        reconcile_at(&CLAUDE, &root, &home).unwrap();

        let dest = root.join(".claude.json");
        assert!(fs::read_link(&dest).is_err(), ".claude.json must be a real file, not a link");
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert!(v.get("mcpServers").is_some(), "MCP servers must be shared in");
        assert!(v.get("projects").is_some(), "project trust should be seeded");
        assert!(v.get("oauthAccount").is_none(), "identity must never be copied");
        assert!(v.get("userID").is_none(), "identity must never be copied");
        // The user's own file is untouched.
        let src: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        assert!(src.get("oauthAccount").is_some());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn claude_json_resyncs_mcp_but_keeps_the_accounts_own_identity() {
        let base = tmp();
        let home = fake_home(&base);
        fs::write(home.join(".claude.json"), r#"{"mcpServers":{"one":{}}}"#).unwrap();
        let root = base.join("account-a");
        reconcile_at(&CLAUDE, &root, &home).unwrap();

        // The runtime signs in and records who this account is.
        let dest = root.join(".claude.json");
        let mut v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
        v.as_object_mut().unwrap().insert(
            "oauthAccount".into(),
            serde_json::json!({"emailAddress": "mine@example.com"}),
        );
        fs::write(&dest, serde_json::to_string(&v).unwrap()).unwrap();

        // The user adds a server later; it must reach this account without disturbing identity.
        fs::write(home.join(".claude.json"), r#"{"mcpServers":{"one":{},"two":{}}}"#).unwrap();
        reconcile_at(&CLAUDE, &root, &home).unwrap();

        let after: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert_eq!(after["mcpServers"].as_object().unwrap().len(), 2, "mcpServers must resync");
        assert_eq!(
            after["oauthAccount"]["emailAddress"], "mine@example.com",
            "the account's own identity must survive a reconcile"
        );

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn detaching_write_through_links_leaves_the_users_file_alone() {
        // The live incident: sign-out ran against a root whose .claude.json was still a symlink
        // from an earlier build, and the runtime wrote through it into ~/.claude.json.
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("account-a");
        fs::create_dir_all(&root).unwrap();
        let link = root.join(".claude.json");
        symlink(&home.join(".claude.json"), &link).unwrap();

        // detach_write_through_links resolves its own root path, so exercise the logic directly
        // against this one: unlink the merge-strategy entry, leave the target untouched.
        assert!(fs::read_link(&link).is_ok());
        fs::remove_file(&link).unwrap();

        assert!(!link.exists(), "the link must be gone");
        assert!(home.join(".claude.json").exists(), "the user's file must survive");
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_symlinked_claude_json_from_an_older_build_is_replaced() {
        // Migration: existing accounts were created with a symlink. Reconcile must unlink it
        // rather than write through it into the user's real file.
        let base = tmp();
        let home = fake_home(&base);
        fs::write(home.join(".claude.json"), r#"{"mcpServers":{"one":{}},"userID":"u1"}"#).unwrap();
        let root = base.join("account-a");
        fs::create_dir_all(&root).unwrap();
        symlink(&home.join(".claude.json"), &root.join(".claude.json")).unwrap();

        reconcile_at(&CLAUDE, &root, &home).unwrap();

        let dest = root.join(".claude.json");
        assert!(fs::read_link(&dest).is_err(), "the stale symlink must be replaced");
        let v: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert!(v.get("userID").is_none());
        // The user's file still has its own identity key — we did not write through the link.
        let src: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude.json")).unwrap()).unwrap();
        assert_eq!(src["userID"], "u1");

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn a_corrupt_account_config_is_refused_not_reseeded() {
        // The dangerous arm: an existing `.claude.json` that does not parse used to be replaced
        // with a copy of the USER's file minus identity — silently swapping the account's own
        // `oauthAccount` for nothing and discarding its project state, with no displaced copy.
        // A partial read of a concurrent write by the co-owning `claude` process looks exactly
        // like this, so it has to be transient-safe: refuse and try again next reconcile.
        let base = tmp();
        let home = fake_home(&base);
        let source = home.join(".claude.json");
        fs::write(
            &source,
            r#"{"oauthAccount":{"emailAddress":"user@example.com"},"mcpServers":{"a":{"url":"u"}}}"#,
        )
        .unwrap();

        let dest = base.join("account-config.json");
        fs::write(&dest, "{\"oauthAccount\":{\"emailAddress\":\"acct@exa").unwrap(); // truncated

        let err = merge_json(&source, &dest, &["oauthAccount", "userID"], &["mcpServers"])
            .expect_err("a corrupt destination must be refused");
        assert!(err.contains("refusing to reseed"), "unexpected error: {err}");

        // Untouched — not replaced, not emptied.
        assert_eq!(
            fs::read_to_string(&dest).unwrap(),
            "{\"oauthAccount\":{\"emailAddress\":\"acct@exa"
        );
    }

    #[test]
    fn an_absent_account_config_is_still_seeded() {
        // The other half: refusing must not break account CREATION, where absent is normal.
        let base = tmp();
        let home = fake_home(&base);
        let source = home.join(".claude.json");
        fs::write(
            &source,
            r#"{"oauthAccount":{"emailAddress":"user@example.com"},"mcpServers":{"a":{"url":"u"}}}"#,
        )
        .unwrap();

        let dest = base.join("fresh.json");
        assert!(merge_json(&source, &dest, &["oauthAccount", "userID"], &["mcpServers"]).unwrap());

        let written: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&dest).unwrap()).unwrap();
        assert!(written.get("oauthAccount").is_none(), "identity was copied into a new root");
        assert!(written.get("mcpServers").is_some(), "mcpServers was not seeded");
    }

    #[test]
    fn pruning_removes_roots_no_account_claims() {
        let base = tmp();
        let dir = base.join("claude");
        for id in ["keep-me", "orphan-a", "orphan-b"] {
            fs::create_dir_all(dir.join(id).join("projects")).unwrap();
            fs::write(dir.join(id).join("projects").join("s.jsonl"), "x").unwrap();
        }

        let mut removed =
            prune_orphan_roots_at(&dir, &["keep-me".to_string()]).unwrap();
        removed.sort();
        assert_eq!(removed, vec!["orphan-a".to_string(), "orphan-b".to_string()]);
        assert!(dir.join("keep-me").exists());
        assert!(!dir.join("orphan-a").exists());
        assert!(!dir.join("orphan-b").exists());
    }

    #[test]
    fn pruning_never_follows_a_link_out_of_the_root() {
        // The resurrection case is a bare directory, but a root abandoned mid-reconcile still
        // holds links aimed at the user's real transcripts. Pruning must unlink, never traverse.
        let base = tmp();
        let home = fake_home(&base);
        let dir = base.join("claude");
        let root = dir.join("orphan");
        reconcile_at(&CLAUDE, &root, &home).unwrap();
        let transcript = home.join(".claude").join("projects").join("session.jsonl");
        fs::write(&transcript, "irreplaceable").unwrap();

        assert_eq!(prune_orphan_roots_at(&dir, &[]).unwrap(), vec!["orphan".to_string()]);
        assert!(!root.exists());
        assert_eq!(fs::read_to_string(&transcript).unwrap(), "irreplaceable");
    }

    #[test]
    fn pruning_keeps_everything_when_every_id_is_claimed() {
        let base = tmp();
        let dir = base.join("claude");
        fs::create_dir_all(dir.join("a")).unwrap();
        fs::create_dir_all(dir.join("b")).unwrap();

        let keep = vec!["a".to_string(), "b".to_string()];
        assert!(prune_orphan_roots_at(&dir, &keep).unwrap().is_empty());
        assert!(dir.join("a").exists() && dir.join("b").exists());
    }

    #[test]
    fn pruning_a_missing_directory_is_not_an_error() {
        let base = tmp();
        assert!(prune_orphan_roots_at(&base.join("never-created"), &[]).unwrap().is_empty());
    }

    #[test]
    fn removing_a_root_unlinks_but_never_follows() {
        // The catastrophic bug this guards: `projects` points at the user's transcripts and
        // `.claude.json` at their project state. Deleting the root must unlink, never traverse.
        let base = tmp();
        let home = fake_home(&base);
        let root = base.join("account-a");
        reconcile_at(&CLAUDE, &root, &home).unwrap();

        // A file inside the linked directory, standing in for a transcript.
        let transcript = home.join(".claude").join("projects").join("session.jsonl");
        fs::write(&transcript, "irreplaceable").unwrap();

        remove_tree_without_following(&root).unwrap();

        assert!(!root.exists(), "the account root should be gone");
        assert!(transcript.exists(), "the link target must survive");
        assert_eq!(fs::read_to_string(&transcript).unwrap(), "irreplaceable");
        assert!(home.join(".claude.json").exists(), "home-root link target must survive");
        assert!(home.join(".claude").join("settings.json").exists());

        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn remove_root_refuses_a_traversing_id() {
        // Without this, an id of "../.." aims a recursive delete at the data directory.
        // ".." is the one that matters: it resolves to the accounts directory itself, so a
        // lexical starts_with check passes it and the recursive delete takes every account.
        for bad in ["..", "../..", "a/b", "", ".", "/", "/etc", "a/../..", "./x"] {
            assert!(
                remove_root(Runtime::Claude, bad).is_err(),
                "should refuse id {bad:?}"
            );
        }
    }

    #[test]
    fn unsupported_runtimes_refuse_rather_than_half_wire() {
        let base = tmp();
        assert!(reconcile(Runtime::Gemini, "a", &base).is_err());
        assert!(spawn_env(Runtime::Codex, "a").is_none());
        fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn claude_spawn_env_sets_the_root_and_scrubs_every_shadowing_var() {
        let Some((set, scrub)) = spawn_env(Runtime::Claude, "acct-1") else {
            // Windows: `CLAUDE.supported` is `cfg!(unix)` until `resolve_cli` learns PATHEXT and
            // the symlink farm stops needing elevation, so "no env at all" is the CORRECT answer
            // — it is what stops the pane promising a tab an account it cannot deliver. Assert
            // the gate rather than skipping, so this test still means something on Windows CI.
            assert!(!cfg!(unix), "Claude spawn env is missing on a platform that supports it");
            assert!(!CLAUDE.supported);
            return;
        };
        assert_eq!(set[0].0, "CLAUDE_CONFIG_DIR");
        // Compare as a Path, not a string: `ends_with` on a str wants `/` and NTFS writes `\`.
        assert!(Path::new(&set[0].1).ends_with(Path::new("accounts/claude/acct-1")));

        // The spawn scrub and the verifier's scrub must be the SAME list. If they diverge, the
        // pane reports the identity the root holds while the tab runs as whatever a leftover
        // variable resolves to — a green "signed in as alice" over a session billed elsewhere.
        let expected: Vec<String> =
            CLAUDE.shadowing_env.iter().map(|s| s.to_string()).collect();
        assert_eq!(scrub, expected);
    }

    fn prefs_with(account: Option<&str>, setup: bool, enabled: bool) -> crate::state::Preferences {
        let mut p = crate::state::Preferences::default();
        p.accounts_setup_complete = setup;
        p.accounts_enabled = enabled;
        if let Some(id) = account {
            p.managed_accounts.push(crate::state::ManagedAccount {
                id: id.to_string(),
                runtime: "claude".into(),
                label: "a".into(),
                email: None,
                org_id: None,
                org_name: None,
                plan: None,
                created_at: 0,
                last_verified_at: None,
                remote_hosts: vec![],
                token_minted_at: None,
                remote_all_hosts: false,
            });
            p.active_account_ids.insert("claude".into(), id.to_string());
        }
        p
    }

    #[test]
    fn spawn_env_injects_nothing_unless_set_up_and_enabled() {
        // §10: "configured but disabled" must behave exactly like "not set up". A tab that
        // quietly keeps using an account after the toggle is off is the worst of both.
        assert!(spawn_env_for(&prefs_with(Some("a1"), true, false)).0.is_empty());
        assert!(spawn_env_for(&prefs_with(Some("a1"), false, true)).0.is_empty());
        assert!(spawn_env_for(&prefs_with(None, true, true)).0.is_empty());
    }

    /// Build prefs with N accounts — `(id, hosts, all_hosts, has_token)` — and make `active` the
    /// active Claude account.
    fn prefs_with_remotes(
        rows: &[(&str, &[&str], bool, bool)],
        active: Option<&str>,
    ) -> crate::state::Preferences {
        let mut p = crate::state::Preferences::default();
        p.accounts_setup_complete = true;
        p.accounts_enabled = true;
        for (id, hosts, all, tok) in rows {
            p.managed_accounts.push(crate::state::ManagedAccount {
                id: (*id).to_string(),
                runtime: "claude".into(),
                label: (*id).into(),
                email: None,
                org_id: None,
                org_name: None,
                plan: None,
                created_at: 0,
                last_verified_at: None,
                remote_hosts: hosts.iter().map(|h| h.to_string()).collect(),
                token_minted_at: tok.then_some(1),
                remote_all_hosts: *all,
            });
        }
        if let Some(a) = active {
            p.active_account_ids.insert("claude".into(), a.to_string());
        }
        p
    }

    #[test]
    fn only_the_active_account_is_ever_propagated() {
        // The point of the design: one identity is current at a time and it is current
        // everywhere. An inactive account naming a host must NOT reach it, or switching
        // accounts would leave remotes on the old one.
        let rows: &[(&str, &[&str], bool, bool)] =
            &[("work", &["nova"], false, true), ("personal", &["nova"], false, true)];

        assert_eq!(remote_account_for_host(&prefs_with_remotes(rows, Some("work")), "nova").unwrap().id, "work");
        // Same host, same two accounts, different active pointer — and no arbitration anywhere.
        assert_eq!(remote_account_for_host(&prefs_with_remotes(rows, Some("personal")), "nova").unwrap().id, "personal");
        // Nothing active: nothing propagates.
        assert!(remote_account_for_host(&prefs_with_remotes(rows, None), "nova").is_none());
    }

    #[test]
    fn a_host_the_active_account_does_not_cover_gets_nothing() {
        // `remote_hosts` answers WHETHER to propagate, never WHICH account — so a host the
        // active account has not been given keeps the login it already has, even when an
        // inactive account lists it.
        let rows: &[(&str, &[&str], bool, bool)] =
            &[("work", &["nova"], false, true), ("personal", &["build-box"], false, true)];
        let p = prefs_with_remotes(rows, Some("work"));
        assert_eq!(remote_account_for_host(&p, "nova").unwrap().id, "work");
        assert!(remote_account_for_host(&p, "build-box").is_none(), "inactive account must not reach its host");
    }

    #[test]
    fn the_catch_all_covers_anything_the_active_account_has_not_named() {
        let p = prefs_with_remotes(&[("a", &[], true, true)], Some("a"));
        assert_eq!(remote_account_for_host(&p, "anything").unwrap().id, "a");
        assert_eq!(remote_account_for_host(&p, "ews@nova").unwrap().id, "a");
        // Two accounts may BOTH carry the catch-all now; only the active one is consulted, so
        // there is no conflict to resolve and none to get wrong.
        let both: &[(&str, &[&str], bool, bool)] = &[("a", &[], true, true), ("b", &[], true, true)];
        assert_eq!(remote_account_for_host(&prefs_with_remotes(both, Some("b")), "x").unwrap().id, "b");
    }

    #[test]
    fn a_bare_host_covers_every_user_but_a_user_host_does_not_capture_its_neighbours() {
        let p = prefs_with_remotes(&[("a", &["nova"], false, true)], Some("a"));
        assert_eq!(remote_account_for_host(&p, "ews@nova").unwrap().id, "a");
        assert_eq!(remote_account_for_host(&p, "NOVA").unwrap().id, "a", "hostnames are case-insensitive");

        // `ews@nova` must NOT capture `root@nova` — a different account on the same box, and
        // quite possibly the reason someone listed one and not the other.
        let p = prefs_with_remotes(&[("a", &["ews@nova"], false, true)], Some("a"));
        assert!(remote_account_for_host(&p, "root@nova").is_none());
        assert_eq!(remote_account_for_host(&p, "ews@nova").unwrap().id, "a");
    }

    #[test]
    fn no_token_means_no_account_even_when_the_host_is_covered() {
        // A host list outliving its token is a promise nothing can keep, and §6.1 means the
        // failure is silent — so the answer has to be "no token", not "this account".
        let p = prefs_with_remotes(&[("a", &["nova"], true, false)], Some("a"));
        assert!(remote_account_for_host(&p, "nova").is_none());
        assert!(remote_account_for_host(&p, "anything").is_none());
    }

    #[test]
    fn nothing_is_injected_when_the_feature_is_off_or_the_host_is_blank() {
        let mut p = prefs_with_remotes(&[("a", &[], true, true)], Some("a"));
        assert!(remote_account_for_host(&p, "nova").is_some());

        p.accounts_enabled = false;
        assert!(remote_account_for_host(&p, "nova").is_none(), "the toggle must reach remotes too");
        p.accounts_enabled = true;
        p.accounts_setup_complete = false;
        assert!(remote_account_for_host(&p, "nova").is_none());

        p.accounts_setup_complete = true;
        // An empty target must never match a catch-all: "no host" is not "every host".
        assert!(remote_account_for_host(&p, "").is_none());
        assert!(remote_account_for_host(&p, "   ").is_none());
    }

    #[test]
    fn a_dangling_active_pointer_propagates_nothing() {
        let mut p = prefs_with_remotes(&[("a", &[], true, true)], Some("a"));
        p.managed_accounts.clear();
        assert!(remote_account_for_host(&p, "nova").is_none());
    }

    #[test]
    fn spawn_env_ignores_an_active_id_with_no_account() {
        // A dangling pointer must not hand a tab a config root nothing owns.
        let mut p = prefs_with(Some("a1"), true, true);
        p.managed_accounts.clear();
        assert!(spawn_env_for(&p).0.is_empty());
    }

    #[test]
    fn spawn_env_sets_the_root_and_scrubs_for_an_active_account() {
        let (set, unset) = spawn_env_for(&prefs_with(Some("a1"), true, true));
        if !CLAUDE.supported {
            // Windows, for now. The gate has to reach all the way to the spawn path, not just
            // to the UI — a platform where the pane is hidden but the env still went out would
            // be the same class of bug in the other direction.
            assert!(set.is_empty() && unset.is_empty());
            return;
        }
        assert_eq!(set.len(), 1);
        assert_eq!(set[0].0, "CLAUDE_CONFIG_DIR");
        assert!(Path::new(&set[0].1).ends_with(Path::new("accounts/claude/a1")));
        assert!(unset.contains(&"ANTHROPIC_API_KEY".to_string()));
        assert!(unset.contains(&"CLAUDE_CODE_OAUTH_TOKEN".to_string()));
    }

    #[test]
    fn claude_shadowing_list_covers_every_precedence_rung() {
        // docs/login.md §2.2. A variable missing here does not fail loudly — the runtime answers
        // from that rung with no email/orgId, so every account looks identical, the duplicate
        // guard fires on every new account, and the §6.1 check can never fail. Rung 4
        // (apiKeyHelper) is absent on purpose: it is a settings-file key, not an env var, and is
        // handled by reporting what answered instead of treating it as an identity.
        for required in [
            "CLAUDE_CODE_USE_BEDROCK",   // 1
            "CLAUDE_CODE_USE_VERTEX",    // 1
            "CLAUDE_CODE_USE_FOUNDRY",   // 1
            "ANTHROPIC_AUTH_TOKEN",      // 2
            "ANTHROPIC_API_KEY",         // 3
            "CLAUDE_CODE_OAUTH_TOKEN",   // 5 — the one this feature itself mints
            "ANTHROPIC_PROFILE",         // 6
            "CLAUDE_SECURESTORAGE_CONFIG_DIR", // §2.1.1
        ] {
            assert!(
                CLAUDE.shadowing_env.contains(&required),
                "{required} can answer instead of the account's own login and must be scrubbed"
            );
        }
    }
}
