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
//! user's skills, commands and permissions — all silently. So an account root is a farm of
//! symlinks back to the real config dir, and only the credential is per-account.
//!
//! **Claude is the only runtime implemented.** Codex and Gemini are declared from what is
//! observable on disk so the shape is right, but are marked unsupported until the same
//! verification Claude got (§5.4) has been done for them — see `RuntimeProfile::supported`.

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

#[derive(Debug, Clone, Copy)]
pub struct SharedEntry {
    /// Name inside the account root.
    pub name: &'static str,
    pub source: Source,
}

const fn cfg(name: &'static str) -> SharedEntry {
    SharedEntry { name, source: Source::ConfigDir }
}
const fn home(name: &'static str) -> SharedEntry {
    SharedEntry { name, source: Source::HomeRoot }
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
    /// False until this runtime has had the §5.4 verification Claude has had. An unsupported
    /// runtime is shown in the UI as not yet available, never silently half-wired.
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
        home(".claude.json"),
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
    supported: true,
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

        let link = root.join(entry.name);

        // Compare the link target, not the resolved path — resolving would call a correct link
        // "wrong" whenever the source is itself a symlink, and relink on every pass.
        match fs::read_link(&link) {
            Ok(target) if target == source => continue,
            Ok(_) => {
                fs::remove_file(&link)
                    .map_err(|e| format!("removing stale link {}: {e}", link.display()))?;
            }
            Err(_) if link.exists() => {
                let aside = displaced_name(&link);
                fs::rename(&link, &aside)
                    .map_err(|e| format!("displacing {}: {e}", link.display()))?;
                out.displaced.push(entry.name.to_string());
            }
            Err(_) => {}
        }

        symlink(&source, &link)
            .map_err(|e| format!("linking {} -> {}: {e}", link.display(), source.display()))?;
        out.linked.push(entry.name.to_string());
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
        assert_eq!(
            fs::read_link(root.join(".claude.json")).unwrap(),
            home.join(".claude.json"),
            "home-root entry must link to ~/.claude.json, not ~/.claude/.claude.json"
        );
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
        // The runtime wrote its own .claude.json here before the farm was built. It may be the
        // only copy of that state.
        fs::write(root.join(".claude.json"), "irreplaceable").unwrap();

        let r = reconcile_at(&CLAUDE, &root, &home).unwrap();

        assert!(r.displaced.contains(&".claude.json".to_string()));
        let parked: Vec<_> = fs::read_dir(&root)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with(".claude.json.displaced-"))
            .collect();
        assert_eq!(parked.len(), 1, "the real file must survive, parked aside");
        assert_eq!(fs::read_to_string(parked[0].path()).unwrap(), "irreplaceable");

        fs::remove_dir_all(&base).ok();
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
        let (set, scrub) = spawn_env(Runtime::Claude, "acct-1").unwrap();
        assert_eq!(set[0].0, "CLAUDE_CONFIG_DIR");
        assert!(set[0].1.ends_with("accounts/claude/acct-1"));

        // The spawn scrub and the verifier's scrub must be the SAME list. If they diverge, the
        // pane reports the identity the root holds while the tab runs as whatever a leftover
        // variable resolves to — a green "signed in as alice" over a session billed elsewhere.
        let expected: Vec<String> =
            CLAUDE.shadowing_env.iter().map(|s| s.to_string()).collect();
        assert_eq!(scrub, expected);
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
