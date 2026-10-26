//! The launch record and the one builder every Hive agent launch goes through (docs/hive.md
//! §11.1).
//!
//! A Hive agent's first launch carries everything that makes it one: its folders as
//! `--add-dir`, the memory folder, deny rules and command hooks in `--settings`, its model and
//! effort, and for a worker its sandbox, only maiTerm's MCP server and its tool set. A generic
//! `claude --resume <id>` drops all of it, so a `hive_role` tab never starts any other way: no
//! record, no launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tauri::State;

use super::paths::{hive_root, HivePaths};
use super::record::Hive;
use super::worker::WorkerRecord;
use super::{ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::AppState;

pub const LAUNCH_VERSION: u32 = 1;
pub const LAUNCH_FILE: &str = "launch.json";
pub const SETTINGS_FILE: &str = "settings.json";
pub const MCP_FILE: &str = "mcp.json";
pub const SESSIONS_FILE: &str = "sessions.json";

/// `onFailure` on a hook, which the fail-closed gate depends on, arrived in 2.1.295 (§6.8).
pub const MIN_CLAUDE: (u32, u32, u32) = (2, 1, 295);

/// Set by a parent Claude for its own children; inherited, they make the new session a child
/// of it, bill the parent's key or override `--effort` (measured running `claude -p` from a
/// maiTerm tab). The PTY spawn scrubs `AGENT_ENV_MARKERS`; the line typed into the shell scrubs
/// these too, after whatever the shell's rc files exported. `ANTHROPIC_API_KEY` goes whatever the
/// account preferences say: a parent session's key gave the coordinator "Invalid API key"
/// (measured), and it outranks the login the tab should run as (docs/login.md §2.2).
const HIVE_EXTRA_UNSET: &[&str] = &[
    "ANTHROPIC_API_KEY",
    "CLAUDE_EFFORT",
    "CLAUDE_PID",
    "CLAUDE_CODE_SESSION_ATTENDED",
    "CLAUDE_CODE_MESSAGING_TOKEN",
    "CLAUDE_CODE_MESSAGING_SOCKET",
];

/// One agent's launch, in `tasks/<task>/launch.json` (`tasks/coordinator/` for the
/// coordinator). Never holds a credential: the account env comes from the tab's own spawn.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LaunchRecord {
    pub version: u32,
    /// `"coordinator"` | `"worker"`.
    pub role: String,
    /// `"claude"`; the only runtime in v1 (§12).
    pub runtime: String,
    pub model: String,
    pub effort: String,
    pub cwd: String,
    pub add_dirs: Vec<String>,
    /// The env maiTerm sets on the launch line, in order.
    pub env: Vec<(String, String)>,
    /// `--setting-sources`, when the launch narrows them: a worker's. The coordinator loads every
    /// source, as a normal tab does.
    pub setting_sources: Option<String>,
    pub settings_path: String,
    /// `--mcp-config`; empty passes none (a coordinator whose account serves maiTerm already).
    pub mcp_config_path: String,
    /// `--permission-mode` for a worker (`"auto"`). The coordinator has none: its human's
    /// settings pick it, as for any tab.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permission_mode: Option<String>,
    /// `--disallowedTools`, by name: WebFetch on every worker, WebSearch unless its role has `web`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub disallowed_tools: Vec<String>,
    /// The worker's task; empty for the coordinator.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub task_id: String,
}

pub const WORKER_PERMISSION_MODE: &str = "auto";
/// A reviewer's `--setting-sources` (§7.7): nothing from the reviewed branch, nor the human's user settings.
pub const REVIEWER_SETTING_SOURCES: &str = "local";

/// One session the task's agent ran, in `tasks/<task>/sessions.json` (§4.2): the last is what a
/// relaunch resumes.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SessionEntry {
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
    pub started_at: String,
}

fn s(p: &Path) -> String {
    p.to_string_lossy().to_string()
}

/// The coordinator's record, all of it derived from the Hive: its home as cwd, a read copy of
/// each git folder, the plain folders and the gallery as `--add-dir`, and its own temp (§6.3).
/// Every setting source, as a plain tab: by the human's decision (2026-10-26) it runs with their
/// own settings, MCP servers and permission mode (§7.1). `prepare_coordinator` drops the MCP
/// config when the account already serves maiTerm.
pub fn coordinator_record(hive: &Hive, paths: &HivePaths) -> LaunchRecord {
    let task = paths.coordinator_task();
    let mut add_dirs: Vec<String> = Vec::new();
    for f in &hive.folders {
        let dir = if f.is_git() { paths.coordinator_worktree(&f.name) } else { PathBuf::from(&f.path) };
        add_dirs.push(s(&dir));
    }
    add_dirs.push(s(&paths.gallery()));
    // Also its cwd, so the sandbox and permission scopes name it the same way (spike 13).
    add_dirs.push(s(&paths.coordinator_home()));
    let model = &hive.models.coordinator;
    LaunchRecord {
        version: LAUNCH_VERSION,
        role: ROLE_COORDINATOR.to_string(),
        runtime: if model.runtime.is_empty() { "claude".to_string() } else { model.runtime.clone() },
        model: model.model.clone(),
        effort: model.effort.clone(),
        cwd: s(&paths.coordinator_home()),
        add_dirs,
        env: vec![
            ("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_string(), "1".to_string()),
            ("CLAUDE_CODE_TMPDIR".to_string(), s(&paths.coordinator_tmp())),
        ],
        setting_sources: None,
        settings_path: s(&task.join(SETTINGS_FILE)),
        mcp_config_path: s(&task.join(MCP_FILE)),
        permission_mode: None,
        disallowed_tools: Vec::new(),
        task_id: String::new(),
    }
}

/// A worker's record, derived from the Hive and its `worker.json` on every launch (§6.3, §6.7):
/// its first worktree (or `scratch/`) as cwd; its other worktrees, the plain folders it was
/// given, `scratch/` and the gallery as `--add-dir`; its own temp and, for a Rust worker, its
/// own cargo target. Project settings only, never the user's: `--settings` outranks only the
/// keys it sets, and Claude merges the sandbox's lists across sources and honours a user
/// setting's `allowLocalBinding` or `allowedDomains`, so the human's own settings would widen
/// every worker's sandbox. Under `allowUnsandboxedCommands: false` and `strictAllowlist` from
/// `--settings`, Claude ignores a project file's sandbox values (2.1.310's settings schema).
pub fn worker_record(hive: &Hive, paths: &HivePaths, w: &WorkerRecord) -> LaunchRecord {
    let task = paths.task(&w.task_id);
    let cwd = w.cwd(paths);
    let mut add_dirs: Vec<String> = Vec::new();
    let mut add = |p: String| {
        if !add_dirs.contains(&p) {
            add_dirs.push(p);
        }
    };
    for f in &w.folders {
        match &f.worktree {
            Some(wt) if Path::new(wt) != cwd => add(wt.clone()),
            Some(_) => {}
            None => {
                if let Some(h) = hive.folders.iter().find(|h| h.name == f.name && !h.is_git()) {
                    add(h.path.clone());
                }
            }
        }
    }
    // A reviewer reads the builder's scratch too (its brief, its made files); its sandbox's
    // denyWrite of other workers' folders keeps that read-only.
    if let Some(builder) = w.reviews.as_deref().and_then(|id| super::worker::task_dir(paths, id).ok()).and_then(|d| super::worker::read_worker(&d).ok()) {
        add(s(&paths.worker_scratch(&builder.slug)));
    }
    add(s(&paths.worker_scratch(&w.slug)));
    add(s(&paths.gallery()));
    // Also its cwd, so the sandbox and permission scopes name it the same way (spike 13).
    add_dirs.retain(|d| Path::new(d) != cwd);
    add_dirs.push(s(&cwd));

    let mut env = Vec::new();
    // A reviewer's added folders are the builder's branch: its CLAUDE.md is the builder's words.
    if !w.is_reviewer() {
        env.push(("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_string(), "1".to_string()));
    }
    env.push(("CLAUDE_CODE_TMPDIR".to_string(), s(&paths.worker_tmp(&w.slug))));
    if super::worktree::worker_uses_own_cargo_target(hive, w) {
        env.push(("CARGO_TARGET_DIR".to_string(), s(&paths.worker_cargo_target(&w.slug))));
    }
    // A reviewer's sources leave out `project`: under it Claude 2.1.310 loads `.claude/skills`,
    // `commands` and `agents` from every `--add-dir`, and the builder commits what it likes into
    // the pinned copies (and writes its own scratch). `local` reads only the cwd's
    // `settings.local.json`, and its scratch's `.claude` is on denyWrite; `--settings` still
    // carries the sandbox, permissions and maiTerm's hooks.
    let sources = if w.is_reviewer() { REVIEWER_SETTING_SOURCES } else { "project" };
    let web = super::roles::role(&w.role).is_some_and(|r| r.web);
    let mut disallowed = vec!["WebFetch".to_string()];
    if !web {
        disallowed.push("WebSearch".to_string());
    }
    let model = &hive.models.worker;
    let or = |mine: &str, hive: &str| if mine.is_empty() { hive.to_string() } else { mine.to_string() };
    LaunchRecord {
        version: LAUNCH_VERSION,
        role: ROLE_WORKER.to_string(),
        runtime: if model.runtime.is_empty() { "claude".to_string() } else { model.runtime.clone() },
        model: or(&w.model, &model.model),
        effort: or(&w.effort, &model.effort),
        cwd: s(&cwd),
        add_dirs,
        env,
        setting_sources: Some(sources.to_string()),
        settings_path: s(&task.join(SETTINGS_FILE)),
        mcp_config_path: s(&task.join(MCP_FILE)),
        permission_mode: Some(WORKER_PERMISSION_MODE.to_string()),
        disallowed_tools: disallowed,
        task_id: w.task_id.clone(),
    }
}

pub fn read_record(task_dir: &Path) -> Result<LaunchRecord, String> {
    let p = task_dir.join(LAUNCH_FILE);
    let bytes = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn write_record(task_dir: &Path, rec: &LaunchRecord) -> Result<(), String> {
    write_json(&task_dir.join(LAUNCH_FILE), &serde_json::to_value(rec).map_err(|e| e.to_string())?)
}

/// Atomic, and skipped when the file already holds exactly this: a relaunch rewrites three files
/// and most launches change none of them.
pub(crate) fn write_json(path: &Path, v: &serde_json::Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    write_bytes(path, &bytes)
}

/// Some of these folders are an agent's to write (the coordinator's home holds brief.md), and
/// maiTerm writes unsandboxed: a symlink planted at the destination or the temp name must never
/// be written through. So the destination is replaced, never followed, and the temp file is a
/// fresh random name opened O_EXCL, which refuses anything already there, a symlink included.
pub(crate) fn write_bytes(path: &Path, bytes: &[u8]) -> Result<(), String> {
    write_bytes_as(path, bytes, false)
}

/// `write_json` readable by its owner only (0600 on Unix), from the first byte: an MCP config
/// carrying a generator's entry carries its key.
pub(crate) fn write_private_json(path: &Path, v: &serde_json::Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    write_bytes_as(path, &bytes, true)
}

fn write_bytes_as(path: &Path, bytes: &[u8], private: bool) -> Result<(), String> {
    use std::io::Write as _;
    let meta = fs::symlink_metadata(path).ok();
    let is_link = meta.as_ref().is_some_and(|m| m.file_type().is_symlink());
    #[cfg(unix)]
    let mode_ok = !private || meta.as_ref().is_some_and(|m| std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o777 == 0o600);
    #[cfg(not(unix))]
    let mode_ok = true;
    if !is_link && mode_ok && fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    if private {
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    }
    #[cfg(not(unix))]
    let _ = private;
    let mut f = opts.open(&tmp).map_err(err)?;
    let written = f.write_all(bytes).and_then(|_| f.sync_all());
    drop(f);
    written.and_then(|_| fs::rename(&tmp, path)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        err(e)
    })
}

/// The whole command line after `claude`. `--model` and `--effort` on every launch: on resume
/// Claude keeps the model but drops effort to medium (spike 11), and the record, not the resumed
/// session, decides both. A worker gets only the MCP servers its config names; the coordinator
/// the account's own as well, like any tab (§7.1).
pub fn hive_launch_args(record: &LaunchRecord, resume: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |k: &str, v: &str| {
        a.push(k.to_string());
        a.push(v.to_string());
    };
    push("--settings", &record.settings_path);
    if !record.mcp_config_path.is_empty() {
        push("--mcp-config", &record.mcp_config_path);
    }
    if record.role != ROLE_COORDINATOR {
        a.push("--strict-mcp-config".to_string());
    }
    if let Some(src) = record.setting_sources.as_deref() {
        a.push("--setting-sources".to_string());
        a.push(src.to_string());
    }
    for d in &record.add_dirs {
        a.push("--add-dir".to_string());
        a.push(d.clone());
    }
    if !record.model.is_empty() {
        a.push("--model".to_string());
        a.push(record.model.clone());
    }
    if !record.effort.is_empty() {
        a.push("--effort".to_string());
        a.push(record.effort.clone());
    }
    if let Some(mode) = record.permission_mode.as_deref().filter(|m| !m.is_empty()) {
        a.push("--permission-mode".to_string());
        a.push(mode.to_string());
    }
    // One value, comma-joined: the flag is variadic, and a second word could swallow what follows.
    if !record.disallowed_tools.is_empty() {
        a.push("--disallowedTools".to_string());
        a.push(record.disallowed_tools.join(","));
    }
    if let Some(sid) = resume.filter(|s| !s.is_empty()) {
        a.push("--resume".to_string());
        a.push(sid.to_string());
    }
    a
}

pub(crate) fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The line typed into the tab's shell: `cd` to the record's cwd, then `env` removing `unset`
/// and setting `set` (the account's config root, `account_set`) and the record's env, then
/// `claude` and the arguments. Every argument is quoted (the paths hold "Application Support"),
/// and the line is typed raw, never through trigger interpolation.
pub fn hive_launch_line(record: &LaunchRecord, args: &[String], unset: &[String], set: &[(String, String)]) -> String {
    let mut line = format!("cd {} && env", sh_quote(&record.cwd));
    let sets = |u: &String| record.env.iter().chain(set).any(|(k, _)| k == u);
    for u in unset {
        if !sets(u) {
            line.push_str(" -u ");
            line.push_str(u);
        }
    }
    for (k, v) in set.iter().filter(|(k, _)| !record.env.iter().any(|(r, _)| r == k)).chain(&record.env) {
        line.push_str(&format!(" {k}={}", sh_quote(v)));
    }
    line.push_str(" claude");
    for a in args {
        line.push(' ');
        line.push_str(&sh_quote(a));
    }
    line
}

/// The Claude config root a Hive agent launches under (§6.6, docs/login.md §6.1): the one a plain
/// tab's `claude` gets, so a Hive never runs as another account than the human's tabs. That is
/// the active managed Claude account's, else the `CLAUDE_CONFIG_DIR` the user exported themselves
/// (`accounts::user_claude_root`, what the startup scrub kept), else None for Claude's default
/// (`~/.claude`, trust in `~/.claude.json`). Never what the tab's shell inherited: the launch
/// line sets or unsets `CLAUDE_CONFIG_DIR` from this (`account_set`, `unset_vars`), and the trust
/// writes and checks, the Create sheet's trust read and its login check read the same root, so
/// the env an agent starts under and the trust maiTerm wrote can't disagree.
pub fn launch_config_root(prefs: &crate::state::Preferences) -> Option<PathBuf> {
    config_root_for(prefs, crate::accounts::user_claude_root())
}

/// `launch_config_root` with the user's own root given.
pub(crate) fn config_root_for(prefs: &crate::state::Preferences, users_own: Option<&Path>) -> Option<PathBuf> {
    crate::accounts::active_accounts(prefs)
        .into_iter()
        .find(|(rt, _)| *rt == crate::accounts::Runtime::Claude)
        .and_then(|(rt, id)| crate::accounts::account_root(rt, &id))
        .or_else(|| users_own.map(Path::to_path_buf))
}

/// Whose trust the launch root holds, as the human knows it (`config_root_for`'s three cases),
/// for the untrusted refusal and the Create sheet: naming `~/.claude` for a user's own
/// `CLAUDE_CONFIG_DIR` sends the human to trust a root this launch never reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TrustRoot {
    Account(String),
    Dir(PathBuf),
    Default,
}

pub(crate) fn trust_root(prefs: &crate::state::Preferences) -> TrustRoot {
    trust_root_for(prefs, crate::accounts::user_claude_root())
}

pub(crate) fn trust_root_for(prefs: &crate::state::Preferences, users_own: Option<&Path>) -> TrustRoot {
    // Without the user's own root, a root is a managed account's.
    if let Some(dir) = config_root_for(prefs, None) {
        return super::create::account_label(prefs).map_or(TrustRoot::Dir(dir), TrustRoot::Account);
    }
    users_own.map_or(TrustRoot::Default, |d| TrustRoot::Dir(d.to_path_buf()))
}

/// The `.claude.json` that launch root reads trust from.
pub fn launch_claude_json(prefs: &crate::state::Preferences) -> Option<PathBuf> {
    claude_json_for(launch_config_root(prefs).as_deref())
}

fn claude_json_for(root: Option<&Path>) -> Option<PathBuf> {
    match root {
        Some(root) => Some(root.join(".claude.json")),
        None => dirs::home_dir().map(|h| h.join(".claude.json")),
    }
}

const CONFIG_ENV: &str = "CLAUDE_CONFIG_DIR";

/// What the launch line sets: `CLAUDE_CONFIG_DIR` to the launch root, when there is one.
pub fn account_set(prefs: &crate::state::Preferences) -> Vec<(String, String)> {
    account_set_for(launch_config_root(prefs).as_deref())
}

fn account_set_for(root: Option<&Path>) -> Vec<(String, String)> {
    root.map(|r| (CONFIG_ENV.to_string(), r.to_string_lossy().into_owned())).into_iter().collect()
}

/// What the launch line removes: the agent markers the PTY spawn scrubs, the parent-session
/// vars above, and, under a managed account, every variable that would answer instead of the
/// account's login (docs/login.md §6.1). Names only, never values. Never `CLAUDE_CONFIG_DIR`:
/// with a launch root the line sets it, and without one a value in the tab's shell is one
/// maiTerm can't see (a shell rc's export, when maiTerm was opened from the Dock), which plain
/// tabs run under. Removing it would run the Hive as `~/.claude`'s account instead, silently, so
/// `config_guard` refuses the launch then.
pub fn unset_vars(prefs: &crate::state::Preferences) -> Vec<String> {
    unset_vars_for(prefs)
}

fn unset_vars_for(prefs: &crate::state::Preferences) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let names = crate::state::agent_runtime::AGENT_ENV_MARKERS
        .iter()
        .chain(HIVE_EXTRA_UNSET)
        .map(|s| s.to_string())
        .chain(crate::accounts::spawn_env_for(prefs).1)
        .filter(|n| n != CONFIG_ENV);
    for n in names {
        let is_name = !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if is_name && !out.contains(&n) {
            out.push(n);
        }
    }
    out
}

/// What the line says, around the shell's value, when `config_guard` refuses. No `\`, no `%`:
/// they go through `sh_quote` (fish reads `\\` inside single quotes) and `printf`'s `%s`.
const GUARD_BEFORE: &str = "maiTerm: this shell sets CLAUDE_CONFIG_DIR=";
const GUARD_AFTER: &str = ", a Claude config root maiTerm can't verify, so this Hive agent was not started: it could run, and \
     bill, as a different Claude account. Turn on managed accounts (Preferences > Accounts), or start maiTerm with \
     CLAUDE_CONFIG_DIR set so it knows that root, then restart the agent.";

/// What runs ahead of the launch line when maiTerm names no config root (`launch_config_root`
/// is None, so the agent should run under Claude's default, `~/.claude`, where the trust went):
/// a check that the tab's shell exports no `CLAUDE_CONFIG_DIR` either. One it does came from
/// somewhere maiTerm can't read (a shell rc), and plain tabs run under it; the line neither
/// removes it (another account, silently) nor runs under it (a root maiTerm never verified or
/// wrote trust into), and says so instead. Its own `sh -c`, so it reads exactly what `claude`
/// would inherit and parses the same in bash, zsh and fish, whose `&&` the line already uses.
/// Empty with a root: the line sets it then (`account_set`). Windows never gets here
/// (`prepare_launch` refuses it).
pub(crate) fn config_guard(root: Option<&Path>) -> String {
    if root.is_some() {
        return String::new();
    }
    let script = format!("[ -z \"${{{CONFIG_ENV}+x}}\" ] || {{ printf '%s%s%s\\n' \"$1\" \"${CONFIG_ENV}\" \"$2\" >&2; exit 1; }}");
    format!("sh -c {} maiterm {} {} && ", sh_quote(&script), sh_quote(GUARD_BEFORE), sh_quote(GUARD_AFTER))
}

/// The environment a check run from maiTerm itself (the Create sheet's login check) gives Claude
/// to match a launch under `root` that gets past `config_guard`: the line's removals and its
/// `CLAUDE_CONFIG_DIR` (None removes). Without a root that is removed: the guard runs a launch
/// only when the shell has none, and maiTerm's own env could still hold a relative one
/// (`accounts::users_own_root` drops those).
pub fn check_env(prefs: &crate::state::Preferences, root: Option<&Path>) -> Vec<(String, Option<String>)> {
    let mut out: Vec<(String, Option<String>)> = unset_vars_for(prefs).into_iter().map(|n| (n, None)).collect();
    match root {
        Some(_) => out.extend(account_set_for(root).into_iter().map(|(k, v)| (k, Some(v)))),
        None => out.push((CONFIG_ENV.to_string(), None)),
    }
    out
}

/// The whole typed line for a launch under `root`: the guard, the worker's first-launch setup,
/// then `hive_launch_line` with what that root removes and sets. One root, read once, for all three.
fn launch_text(prefs: &crate::state::Preferences, root: Option<&Path>, setup: &str, record: &LaunchRecord, args: &[String]) -> String {
    format!("{}{setup}{}", config_guard(root), hive_launch_line(record, args, &unset_vars_for(prefs), &account_set_for(root)))
}

static SESSIONS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn read_sessions(task_dir: &Path) -> Vec<SessionEntry> {
    fs::read(task_dir.join(SESSIONS_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn last_session(task_dir: &Path) -> Option<String> {
    read_sessions(task_dir).pop().map(|e| e.session_id).filter(|s| !s.is_empty())
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SessionAppend {
    pub added: bool,
    /// The list was empty before: the agent's first launch.
    pub first: bool,
}

/// Record a session the task's agent started, once per session id: a resume reports the same
/// id again, and two hooks can report one start.
pub fn append_session(task_dir: &Path, entry: SessionEntry) -> Result<SessionAppend, String> {
    let _held = SESSIONS_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut list = read_sessions(task_dir);
    let first = list.is_empty();
    if entry.session_id.is_empty() || list.iter().any(|e| e.session_id == entry.session_id) {
        return Ok(SessionAppend { added: false, first: false });
    }
    list.push(entry);
    write_json(&task_dir.join(SESSIONS_FILE), &serde_json::to_value(&list).map_err(|e| e.to_string())?)?;
    Ok(SessionAppend { added: true, first })
}

pub const KICKOFF_FILE: &str = "kickoff.json";
pub const KICKOFF_DELIVERED: &str = "delivered";

/// Whether the coordinator's one-time kickoff reached it, in `tasks/coordinator/kickoff.json`.
/// Its own record, never read off the session list: a kickoff held or refused at the first start
/// must still go out on a later one. No file is pending: a new Hive's coordinator hasn't been told.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct KickoffState {
    /// `"pending"` | `"delivered"`.
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delivered_at: Option<String>,
}

pub fn kickoff_pending(task_dir: &Path) -> bool {
    let read = fs::read(task_dir.join(KICKOFF_FILE)).ok().and_then(|b| serde_json::from_slice::<KickoffState>(&b).ok());
    read.is_none_or(|k| k.state != KICKOFF_DELIVERED)
}

pub fn mark_kickoff_delivered(task_dir: &Path) -> Result<(), String> {
    let k = KickoffState { state: KICKOFF_DELIVERED.to_string(), delivered_at: Some(crate::commands::workspace::iso_now()) };
    write_json(&task_dir.join(KICKOFF_FILE), &serde_json::to_value(&k).map_err(|e| e.to_string())?)
}

/// A fresh session is told again: its context starts empty.
pub fn reset_kickoff(task_dir: &Path) -> Result<(), String> {
    match fs::remove_file(task_dir.join(KICKOFF_FILE)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", task_dir.join(KICKOFF_FILE).display())),
        _ => Ok(()),
    }
}

fn parse_version(text: &str) -> Option<(u32, u32, u32)> {
    let v = text.split_whitespace().next()?;
    let mut it = v.split('.').map(|p| p.parse::<u32>().ok());
    Some((it.next()??, it.next()??, it.next()??))
}

/// The verdict over every Claude's probed version, and whether it may be cached. Only a definite
/// one is: a version that couldn't be read (a timeout on the first run of a freshly updated
/// binary, a loaded machine) is asked again on the next launch, never remembered as a refusal.
/// A too-old Claude wins over an unreadable one, since that refusal is certain.
fn version_verdict(probes: &[(PathBuf, Option<(u32, u32, u32)>)]) -> (Result<(), String>, bool) {
    if let Some((path, Some((a, b, c)))) = probes.iter().find(|(_, v)| v.is_some_and(|v| v < MIN_CLAUDE)) {
        let msg = format!("Claude Code at {} is {a}.{b}.{c}; a Hive needs 2.1.295 or newer, where a hook can fail closed.", path.display());
        return (Err(msg), true);
    }
    if let Some((path, _)) = probes.iter().find(|(_, v)| v.is_none()) {
        return (Err(format!("Couldn't read the version of Claude Code at {}.", path.display())), false);
    }
    (Ok(()), true)
}

/// Every Claude maiTerm can find is at least `MIN_CLAUDE`: a tab runs whichever its shell finds
/// (the `events_installed_claude_knows` rule). None found is a refusal, since an older Claude
/// ignores `onFailure` and the gate would fail open. A definite verdict is cached by path, size
/// and mtime.
pub fn claude_new_enough() -> Result<(), String> {
    type Key = Vec<(PathBuf, u64, Option<std::time::SystemTime>)>;
    static CACHE: std::sync::Mutex<Option<(Key, Result<(), String>)>> = std::sync::Mutex::new(None);
    let mut key: Key = Vec::new();
    for p in crate::accounts::cli_candidates(crate::accounts::Runtime::Claude.profile()) {
        let Ok(real) = fs::canonicalize(&p) else { continue };
        let Ok(meta) = fs::metadata(&real) else { continue };
        let e = (real, meta.len(), meta.modified().ok());
        if !key.contains(&e) {
            key.push(e);
        }
    }
    if key.is_empty() {
        return Err("maiTerm can't find Claude Code to check its version. A Hive needs Claude Code 2.1.295 or newer.".to_string());
    }
    if let Some((k, r)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
        if *k == key {
            return r.clone();
        }
    }
    let probes: Vec<(PathBuf, Option<(u32, u32, u32)>)> = key
        .iter()
        .map(|(path, _, _)| {
            let out = crate::util::proc::output_timeout(crate::util::proc::command(path).arg("--version"), Duration::from_secs(15));
            let v = out.ok().filter(|o| o.status.success()).and_then(|o| parse_version(&String::from_utf8_lossy(&o.stdout)));
            (path.clone(), v)
        })
        .collect();
    let (result, definite) = version_verdict(&probes);
    if definite {
        *CACHE.lock().unwrap_or_else(|e| e.into_inner()) = Some((key, result.clone()));
    }
    result
}

/// Everything a launch writes, from the current Hive and record: the settings file, the MCP
/// config and the brief. Pure of trust and version checks, which `prepare_launch` does first.
pub fn write_launch_files(hive: &Hive, paths: &HivePaths, record: &LaunchRecord, hooks: &super::settings::HookEnv) -> Result<(), String> {
    let settings = super::settings::hive_settings_json(hive, record, paths, hooks)?;
    write_json(Path::new(&record.settings_path), &settings)?;
    if !record.mcp_config_path.is_empty() {
        write_private_json(Path::new(&record.mcp_config_path), &super::settings::hive_mcp_config())?;
    }
    if record.role == ROLE_COORDINATOR {
        let counts = super::precedents::counts(paths);
        write_bytes(&paths.coordinator_brief(), super::brief::brief_md(hive, paths, &counts).as_bytes())?;
    }
    Ok(())
}

/// A worker's launch files: its settings (§6.7), the MCP config (with the Hive's generators for a
/// media role, from `claude_json`, the launching account's `.claude.json`), and
/// `scratch/brief.md` (§7.1).
#[allow(clippy::too_many_arguments)]
pub fn write_worker_launch_files(
    hive: &Hive,
    paths: &HivePaths,
    record: &LaunchRecord,
    w: &WorkerRecord,
    ctx: &super::settings::WorkerContext,
    task_brief: &str,
    hooks: &super::settings::HookEnv,
    claude_json: Option<&serde_json::Value>,
) -> Result<(), String> {
    let settings = super::settings::worker_settings_json(hive, record, w, paths, ctx, hooks)?;
    write_json(Path::new(&record.settings_path), &settings)?;
    write_private_json(Path::new(&record.mcp_config_path), &super::settings::worker_mcp_config(hive, w, claude_json))?;
    let builder = w.reviews.as_deref().and_then(|id| super::worker::task_dir(paths, id).ok()).and_then(|d| super::worker::read_worker(&d).ok());
    let inputs = super::gallery::tools::brief_inputs(&hive.id, paths, &w.inputs);
    write_bytes(&paths.worker_brief(&w.slug), super::brief::worker_brief_md(hive, paths, w, task_brief, builder.as_ref(), &inputs).as_bytes())
}

const NO_CLAUDE_YET: &str = "Claude Code hasn't run under the account this Hive's agents use, so there's nowhere to record folder \
     trust. Run claude once in a tab of that account, then try again.";

/// Every root maiTerm writes trust into, with the one the launch reads (`launch_json`) among them.
fn trust_homes(launch_json: &Path) -> Result<crate::relocate::agents::Homes, String> {
    let mut homes = crate::relocate::agents::Homes::real().ok_or("Couldn't find the home folder")?;
    if !homes.claude_json.iter().any(|f| f == launch_json) {
        homes.claude_json.push(launch_json.to_path_buf());
    }
    Ok(homes)
}

/// Trust a folder maiTerm made itself (the coordinator's home, a scratch-only worker's scratch)
/// in every root, re-verified before every launch: the tab may come up under another account
/// than the one this was last written for, and nothing may answer the dialog (§6.6). The root the
/// launch reads (`launch_json`, `launch_claude_json`) must hold it afterwards, whatever the rest do.
pub(crate) fn ensure_trust_in(homes: &crate::relocate::agents::Homes, launch_json: &Path, paths: &HivePaths, dir: &Path) -> Result<(), String> {
    let trust = super::trust::write_trust(homes, paths, dir)?;
    if !trust.failed.is_empty() {
        return Err(format!("Couldn't record Claude's folder trust: {}", trust.failed.join("; ")));
    }
    if !launch_json.is_file() {
        return Err(NO_CLAUDE_YET.to_string());
    }
    if !super::trust::trusted_in_all(&[launch_json.to_path_buf()], dir) {
        return Err(format!("Couldn't record Claude's folder trust in {}", launch_json.display()));
    }
    Ok(())
}

fn ensure_trust(launch_json: &Path, paths: &HivePaths, dir: &Path) -> Result<(), String> {
    ensure_trust_in(&trust_homes(launch_json)?, launch_json, paths, dir)
}

/// A worktree inherits its main repo's trust (§6.6), so maiTerm writes none for a worker's: a
/// repo the human hasn't trusted is theirs to trust, and the launch is refused instead of
/// raising the dialog. Checked in the one root the launch reads (`launch_json`), the root the
/// Create sheet reads too: an entry in another root (`~/.claude.json` under a managed account)
/// is no trust this launch gets, and a missing one there refuses nothing. `under` names that
/// root for the refusal.
fn check_worker_trust(homes: &crate::relocate::agents::Homes, launch_json: &Path, under: &TrustRoot, paths: &HivePaths, w: &WorkerRecord) -> Result<(), String> {
    // A reviewer's cwd is its scratch, a folder maiTerm made: trusted like a scratch-only worker's.
    if w.git_folders().next().is_none() || w.is_reviewer() {
        return ensure_trust_in(homes, launch_json, paths, &paths.worker_scratch(&w.slug));
    }
    if !launch_json.is_file() {
        return Err(NO_CLAUDE_YET.to_string());
    }
    let root = [launch_json.to_path_buf()];
    for f in w.git_folders() {
        let wt = f.worktree.as_deref().unwrap_or_default();
        let common = f.common_dir.as_deref().map(Path::new);
        let candidates = super::trust::worktree_candidates(Path::new(wt), common);
        if !super::trust::keys_trusted_in_all(&root, &candidates) {
            let repo = common.filter(|c| c.file_name().is_some_and(|n| n == ".git")).and_then(Path::parent).map(|p| p.display().to_string());
            return Err(untrusted_refusal(&f.name, repo.as_deref(), under));
        }
    }
    Ok(())
}

/// Why a worker in an untrusted repo doesn't start, and exactly how its human trusts it: under
/// the account the launch uses, in the human's own checkout, never a copy maiTerm made.
pub(crate) fn untrusted_refusal(folder: &str, repo: Option<&str>, under: &TrustRoot) -> String {
    let under = match under {
        TrustRoot::Account(a) => format!("the Claude account \"{a}\""),
        TrustRoot::Dir(d) => format!("the Claude config at {} (CLAUDE_CONFIG_DIR)", d.display()),
        TrustRoot::Default => "Claude's default account (~/.claude)".to_string(),
    };
    let place = repo.map_or_else(|| format!("the \"{folder}\" folder"), |r| format!("{r} (the \"{folder}\" folder)"));
    format!(
        "Claude Code doesn't trust the repo at {place} under {under}, the account this Hive's agents run as, so starting this \
         worker would raise Claude's trust dialog, which nothing may answer for you. Only your human can trust it: in a \
         maiTerm tab running under that account, run `claude` once in {} and accept its question. A worktree or read copy \
         doesn't count: its trust comes from that repo. Then restart the worker.",
        repo.unwrap_or("that repo")
    )
}

/// What a spawn checks once its worktrees exist and before its tab does: a launch the builder
/// would refuse (an old Claude, a repo nobody has trusted) refuses the spawn instead, rather than
/// leaving a worker in `launching`, holding a slot it never uses. Blocks: the version probe and
/// the trust reads (and a scratch-only worker's trust write).
pub fn worker_preflight(paths: &HivePaths, w: &WorkerRecord, prefs: &crate::state::Preferences) -> Result<(), String> {
    if cfg!(windows) {
        return Err("A Hive needs Claude's sandbox, which native Windows doesn't have.".to_string());
    }
    crate::instance::hive_refusal()?;
    claude_new_enough()?;
    let launch_json = launch_claude_json(prefs).ok_or("Couldn't find the home folder")?;
    check_worker_trust(&trust_homes(&launch_json)?, &launch_json, &trust_root(prefs), paths, w)
}

/// What runs before a worker's first session (§6.4, §6.5): the slim cargo seed, then each git
/// folder's setup command in its worktree, typed ahead of the agent so a failure shows in the
/// raw terminal and stops the launch. Setup is the human's command, run on a fresh worktree at
/// the landing target's tip, before the agent has written anything, so it runs Base's code. It
/// isn't under the worker's sandbox profile yet: a job's profile closes the network, which `npm
/// ci` needs (docs/hive.md §13.1, step 5b's acceptance).
fn first_launch_setup(hive: &Hive, paths: &HivePaths, w: &WorkerRecord) -> Result<String, String> {
    let seed = super::worktree::worker_uses_own_cargo_target(hive, w).then(|| super::worktree::worker_seed_source(hive, w)).flatten();
    if let Some(src) = seed {
        let target = paths.worker_cargo_target(&w.slug);
        // maiTerm copies unsandboxed into a folder that becomes the worker's: never through a link.
        if fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
            log::warn!("hive {}: {} is a symlink; not seeded", hive.id, target.display());
        } else {
            match super::worktree::seed_cargo_target(&src, &target) {
                Ok(true) => log::info!("hive {}: seeded {} from {}", hive.id, target.display(), src.display()),
                Ok(false) => {}
                Err(e) => log::warn!("hive {}: cargo seed failed, worker {} builds cold: {e}", hive.id, w.slug),
            }
        }
    }
    let mut prefix = String::new();
    // A reviewer's worktrees hold the builder's unlanded code, and setup runs unsandboxed: an
    // `npm ci` there would run the builder's install scripts as the human.
    if w.is_reviewer() {
        return Ok(prefix);
    }
    // Setup's package caches are the worker's own, as its later install jobs' are: one store, so
    // a reinstall finds what setup put there. Emptied first: setup runs unsandboxed, so it must
    // never read a cache a worker's job could have written (`deps::cache_env`). Corepack's stays
    // the human's (`deps::COREPACK_HOME_SHELL`).
    let cache = paths.worker_deps_cache(&w.slug);
    let exports: String = super::deps::setup_env(&cache).iter().map(|(k, v)| format!(" {k}={}", sh_quote(v))).collect();
    let exports = format!(" {}{exports}", super::deps::COREPACK_HOME_SHELL);
    let mut emptied = false;
    for f in w.git_folders() {
        let setup = hive.folders.iter().find(|h| h.name == f.name).and_then(|h| h.setup.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let (Some(setup), Some(wt)) = (setup, f.worktree.as_deref()) else { continue };
        if setup.chars().any(char::is_control) {
            return Err(format!("The setup command of \"{}\" has a line break in it, so it can't be typed as one line. Fix it in the Hive's settings.", f.name));
        }
        if !emptied {
            emptied = true;
            if fs::symlink_metadata(&cache).is_ok() {
                if let Err(e) = fs::remove_dir_all(&cache).or_else(|_| fs::remove_file(&cache)) {
                    return Err(format!("maiTerm couldn't empty {}'s package cache ({e}), so its setup didn't run.", w.slug));
                }
            }
        }
        prefix.push_str(&format!("( cd {} && export{exports} && {setup} ) && ", sh_quote(wt)));
    }
    Ok(prefix)
}

/// Every launch in a Done Hive (§11.4). `src/lib/hive/launch.ts` matches its start
/// (`HIVE_DONE_REFUSAL`): keep the two in step.
pub const DONE_REFUSAL: &str = "This Hive is done: it starts no agent until your human wakes it with a message.";

/// The launch line for a Hive tab (§11.1), or why it can't start. `tasks` is the workspace's
/// task list, where a worker's brief is its task's detail. Blocks: trust writes, the version
/// probe, file writes, and before a worker's first session its cargo seed.
pub fn prepare_launch(hive: &Hive, role: &str, tab_id: &str, tasks: &[crate::state::Task], prefs: &crate::state::Preferences) -> Result<String, String> {
    // Before anything touches a home or probes Claude: a restart's session restore and a Hive
    // view's remount of a Done Hive type nothing, and the frontend reads this string as quiet.
    if hive.status == super::record::STATUS_DONE {
        return Err(DONE_REFUSAL.to_string());
    }
    if cfg!(windows) {
        return Err("A Hive needs Claude's sandbox, which native Windows doesn't have.".to_string());
    }
    crate::instance::hive_refusal()?;
    let root = hive_root(&hive.id).ok_or_else(|| format!("\"{}\" can't name a Hive folder", hive.id))?;
    let paths = HivePaths::new(root);
    match role {
        ROLE_COORDINATOR => prepare_coordinator(hive, &paths, prefs),
        ROLE_WORKER => prepare_worker(hive, &paths, tab_id, tasks, prefs),
        other => Err(format!("A Hive has no \"{other}\" agent to launch.")),
    }
}

fn prepare_worker(hive: &Hive, paths: &HivePaths, tab_id: &str, tasks: &[crate::state::Task], prefs: &crate::state::Preferences) -> Result<String, String> {
    // No record, no launch: without worker.json and launch.json the fallback is the generic
    // launch, unsandboxed in the human's folder.
    let missing = |why: String| {
        log::warn!("hive {}: tab {tab_id}: {why}", hive.id);
        "launch record missing".to_string()
    };
    let mut w = super::worker::worker_for_tab(paths, tab_id).ok_or_else(|| missing("no worker.json names this tab".to_string()))?;
    // A mount (session restore, the Hive view, a resume) never brings back an agent the Hive has
    // stopped or that failed: only `hiveRestartWorker` does, which takes a slot first.
    if !w.is_live() {
        return Err(format!(
            "Worker {} is {}, so it doesn't start on its own. Its coordinator restarts it with hiveRestartWorker.",
            w.slug, w.phase
        ));
    }
    // A reviewer that gave its verdict has nothing left to do: its retirement is owed (the waiters
    // poll does it), and a relaunch would only hold its slot.
    if let Some(b) = w.reviews.as_deref().filter(|_| w.is_reviewer()).and_then(|id| super::worker::task_dir(paths, id).ok()).and_then(|d| super::worker::read_worker(&d).ok()) {
        if super::review::verdict_given(&b, &w).is_some() {
            return Err(format!("Reviewer {} has given its verdict, so it doesn't start again: maiTerm retires it.", w.slug));
        }
    }
    let task = super::worker::task_dir(paths, &w.task_id).map_err(missing)?;
    let existing = read_record(&task).map_err(missing)?;
    claude_new_enough()?;
    if w.cargo_target.is_empty() {
        let own = super::worktree::worker_uses_own_cargo_target(hive, &w);
        w = super::worker::update_worker(&task, |w| {
            let first = w.cargo_target.is_empty();
            if first {
                w.cargo_target = if own { super::worker::CARGO_TARGET_OWN } else { super::worker::CARGO_TARGET_NONE }.to_string();
            }
            first
        })?;
    }

    let record = worker_record(hive, paths, &w);
    if record != existing {
        write_record(&task, &record)?;
    }
    let root = launch_config_root(prefs);
    let launch_json = claude_json_for(root.as_deref()).ok_or("Couldn't find the home folder")?;
    check_worker_trust(&trust_homes(&launch_json)?, &launch_json, &trust_root(prefs), paths, &w)?;

    let hooks = super::settings::HookEnv::current()?;
    let ctx = super::settings::WorkerContext::gather(hive, paths, &w);
    let brief = tasks.iter().find(|t| t.id == w.task_id).and_then(|t| t.detail.clone()).unwrap_or_default();
    // The account the tab starts under: its generator entries, keys and all, go in mcp.json.
    let claude_json = fs::read(&launch_json).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    write_worker_launch_files(hive, paths, &record, &w, &ctx, &brief, &hooks, claude_json.as_ref())?;
    // A fresh start (`hiveRestartWorker` fresh) is a new session, told its kickoff again; the
    // flag clears when that session starts.
    let resume = if w.fresh_next {
        reset_kickoff(&task)?;
        None
    } else {
        last_session(&task)
    };
    let setup = if w.setup_done { String::new() } else { first_launch_setup(hive, paths, &w)? };
    let args = hive_launch_args(&record, resume.as_deref());
    log::info!("hive {}: launching worker {} ({})", hive.id, w.slug, if resume.is_some() { "resume" } else { "new session" });
    Ok(launch_text(prefs, root.as_deref(), &setup, &record, &args))
}

fn prepare_coordinator(hive: &Hive, paths: &HivePaths, prefs: &crate::state::Preferences) -> Result<String, String> {
    let task = paths.coordinator_task();
    // No record, no launch: the fallback would be exactly the generic launch this replaces.
    let existing = read_record(&task).map_err(|e| {
        log::warn!("hive {}: {e}", hive.id);
        "launch record missing".to_string()
    })?;
    claude_new_enough()?;

    let root = launch_config_root(prefs);
    let launch_json = claude_json_for(root.as_deref()).ok_or("Couldn't find the home folder")?;
    // The coordinator's record is all derived from the Hive, so a folder added since the last
    // launch reaches this one; and its MCP servers from the account it starts under, which serves
    // maiTerm's to every tab while the Claude Code integration is on.
    let mut record = coordinator_record(hive, paths);
    let claude_json = fs::read(&launch_json).ok().and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    if super::settings::has_maiterm_server(claude_json.as_ref()) {
        record.mcp_config_path.clear();
    }
    if record != existing {
        write_record(&task, &record)?;
    }
    ensure_trust(&launch_json, paths, &paths.coordinator_home())?;

    let hooks = super::settings::HookEnv::current()?;
    write_launch_files(hive, paths, &record, &hooks)?;
    let resume = last_session(&task);
    let args = hive_launch_args(&record, resume.as_deref());
    log::info!("hive {}: launching the coordinator ({})", hive.id, if resume.is_some() { "resume" } else { "first launch" });
    Ok(launch_text(prefs, root.as_deref(), "", &record, &args))
}

/// The line a Hive tab's shell runs to start (or resume) its agent. The frontend types it as
/// is; an error is shown, and nothing else is ever typed in its place.
#[tauri::command]
pub async fn hive_launch_command(state: State<'_, Arc<AppState>>, tab_id: String) -> Result<String, String> {
    let (hive, role, tasks, prefs) = {
        let data = state.app_data.read();
        let (ws, tab) = super::find_hive_tab(&data, &tab_id).ok_or("Not a Hive tab")?;
        let hive = ws.hive.clone().ok_or("Not a Hive tab")?;
        let role = tab.hive_role.clone().unwrap_or_default();
        // Only a worker's brief comes from the task list.
        let tasks = if role == ROLE_WORKER { ws.tasks.clone() } else { Vec::new() };
        (hive, role, tasks, data.preferences.clone())
    };
    tauri::async_runtime::spawn_blocking(move || prepare_launch(&hive, &role, &tab_id, &tasks, &prefs))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hive::record::{HiveFolder, KIND_GIT, KIND_PLAIN};
    use crate::hive::worker::WorkerFolder;

    pub(crate) fn sample_hive() -> Hive {
        Hive {
            id: "h1".into(),
            slug: "ship-it".into(),
            goal: "Ship the thing".into(),
            folders: vec![
                HiveFolder { name: "web".into(), path: "/r/web".into(), kind: KIND_GIT.into(), primary: true, local_remotes: vec!["/srv/web.git".into()], ..HiveFolder::default() },
                HiveFolder { name: "brand".into(), path: "/r/brand assets".into(), kind: KIND_PLAIN.into(), ..HiveFolder::default() },
            ],
            ..Hive::default()
        }
    }

    pub(crate) fn sample_paths() -> HivePaths {
        HivePaths::new("/Users/x/Library/Application Support/com.aiterm.dev/hive/h1")
    }

    fn scratch(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("hive-launch-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        t
    }

    fn flag_values<'a>(args: &'a [String], flag: &str) -> Vec<&'a str> {
        args.windows(2).filter(|w| w[0] == flag).map(|w| w[1].as_str()).collect()
    }

    #[test]
    fn the_coordinator_record_comes_from_the_hive() {
        let (h, p) = (sample_hive(), sample_paths());
        let r = coordinator_record(&h, &p);
        assert_eq!(r.role, ROLE_COORDINATOR);
        assert_eq!(r.cwd, s(&p.coordinator_home()));
        assert_eq!((r.model.as_str(), r.effort.as_str(), r.runtime.as_str()), ("opus", "medium", "claude"));
        assert_eq!(
            r.add_dirs,
            vec![s(&p.coordinator_worktree("web")), "/r/brand assets".to_string(), s(&p.gallery()), s(&p.coordinator_home())],
            "a read copy for git, the folder itself for plain, the gallery, home"
        );
        assert!(r.env.contains(&("CLAUDE_CODE_TMPDIR".into(), s(&p.coordinator_tmp()))));
        assert!(r.env.contains(&("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".into(), "1".into())));
        assert!(r.settings_path.starts_with(&s(&p.coordinator_task())));
        let text = serde_json::to_string(&r).unwrap();
        assert!(!text.to_lowercase().contains("token") && !text.contains("ANTHROPIC"), "never a credential");
    }

    #[test]
    fn effort_and_model_are_passed_with_and_without_resume() {
        let r = coordinator_record(&sample_hive(), &sample_paths());
        for resume in [None, Some("sid-1")] {
            let a = hive_launch_args(&r, resume);
            assert_eq!(flag_values(&a, "--effort"), vec!["medium"]);
            assert_eq!(flag_values(&a, "--model"), vec!["opus"]);
            assert_eq!(flag_values(&a, "--settings"), vec![r.settings_path.as_str()]);
            assert_eq!(flag_values(&a, "--add-dir").len(), r.add_dirs.len());
            assert_eq!(flag_values(&a, "--resume"), resume.into_iter().collect::<Vec<_>>());
        }
    }

    // POSIX-shell only: `sh -n` parses the rendered line, as the SessionStart hook's test does.
    #[cfg(unix)]
    #[test]
    fn the_launch_line_is_valid_shell_with_spaces_and_quotes() {
        let h = sample_hive();
        let p = HivePaths::new("/Users/o'brien/Library/Application Support/com.aiterm.dev/hive/h1");
        let r = coordinator_record(&h, &p);
        let args = hive_launch_args(&r, Some("abc-123"));
        let unset = vec!["CLAUDECODE".to_string(), "ANTHROPIC_API_KEY".to_string(), "CLAUDE_CODE_TMPDIR".to_string()];
        let line = hive_launch_line(&r, &args, &unset, &[]);
        assert!(line.starts_with("cd '/Users/o'\\''brien/Library/Application Support/"), "{line}");
        assert!(line.contains(" -u CLAUDECODE -u ANTHROPIC_API_KEY "), "{line}");
        assert!(!line.contains("-u CLAUDE_CODE_TMPDIR"), "a var the record sets isn't also unset");
        let out = std::process::Command::new("sh").arg("-n").arg("-c").arg(&line).output().expect("sh is available");
        assert!(out.status.success(), "not valid shell:\n{line}\n{}", String::from_utf8_lossy(&out.stderr));

        // And it says what it means: echo each word back through the real shell.
        let echo = line.replacen(" && env", " && printf '%s\\n'", 1).replacen("cd ", ": ", 1);
        let out = std::process::Command::new("sh").arg("-c").arg(&echo).output().unwrap();
        let words: Vec<String> = String::from_utf8_lossy(&out.stdout).lines().map(String::from).collect();
        assert!(words.contains(&p.coordinator_worktree("web").to_string_lossy().to_string()), "{words:?}");
        assert!(words.contains(&"/r/brand assets".to_string()));
        assert_eq!(words.last().map(String::as_str), Some("abc-123"));
    }

    #[test]
    fn the_markers_are_unset_and_nothing_but_names_is() {
        let u = unset_vars(&crate::state::Preferences::default());
        for m in crate::state::agent_runtime::AGENT_ENV_MARKERS {
            assert!(u.iter().any(|x| x == m), "{m}");
        }
        assert!(u.iter().any(|x| x == "CLAUDE_EFFORT"));
        // With managed accounts off (the default), the one source that shadows a login is empty:
        // a parent session's key must go anyway.
        assert!(u.iter().any(|x| x == "ANTHROPIC_API_KEY"), "{u:?}");
        assert!(u.iter().all(|n| n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')));
        let line = hive_launch_line(&coordinator_record(&sample_hive(), &sample_paths()), &[], &u, &[]);
        assert!(line.contains(" -u ANTHROPIC_API_KEY "), "{line}");
    }

    fn managed(id: &str) -> crate::state::Preferences {
        let mut p = crate::state::Preferences { accounts_setup_complete: true, accounts_enabled: true, ..Default::default() };
        p.managed_accounts.push(crate::state::ManagedAccount {
            id: id.into(),
            runtime: "claude".into(),
            label: "Work".into(),
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
        p.active_account_ids.insert("claude".into(), id.into());
        p
    }

    /// Runs a whole typed launch line (`launch_text`) through a real `sh`, with a stand-in
    /// `claude` first on PATH that prints the root it got, the tab's shell exporting
    /// `CLAUDE_CONFIG_DIR=shell` or not. Returns (stdout, stderr, success).
    #[cfg(unix)]
    fn run_launch(prefs: &crate::state::Preferences, root: Option<&Path>, setup: &str, shell: Option<&str>) -> (String, String, bool) {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("guard");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let fake = bin.join("claude");
        fs::write(&fake, "#!/bin/sh\nprintf 'RAN:%s' \"${CLAUDE_CONFIG_DIR-unset}\"\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let mut r = coordinator_record(&sample_hive(), &sample_paths());
        r.cwd = dir.to_string_lossy().into_owned();
        let line = launch_text(prefs, root, setup, &r, &[]);
        let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap_or_default());
        let mut cmd = std::process::Command::new("sh");
        cmd.arg("-c").arg(&line).env("PATH", path);
        match shell {
            Some(v) => cmd.env(CONFIG_ENV, v),
            None => cmd.env_remove(CONFIG_ENV),
        };
        let out = cmd.output().unwrap();
        let _ = fs::remove_dir_all(&dir);
        (String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned(), out.status.success())
    }

    /// The second live test: a dev maiTerm started from a prod tab carried the prod root, and the
    /// coordinator ran under it, at a trust dialog. With a root maiTerm chose, the line names
    /// exactly it, whatever the shell has: `env` applies the assignment over it.
    #[cfg(unix)]
    #[test]
    fn a_managed_root_is_named_exactly_whatever_the_shell_has() {
        let work = managed("acct-1");
        let root = crate::accounts::account_root(crate::accounts::Runtime::Claude, "acct-1").unwrap();
        assert_eq!(config_root_for(&work, None).as_deref(), Some(root.as_path()));
        assert_eq!(account_set(&work), vec![(CONFIG_ENV.to_string(), root.to_string_lossy().into_owned())]);
        assert!(!unset_vars(&work).iter().any(|u| u == CONFIG_ENV));
        assert!(config_guard(Some(&root)).is_empty(), "nothing to check: the line sets it");
        let want = format!("RAN:{}", root.display());
        for shell in [None, Some("/prod/accounts/claude/x")] {
            let (out, err, ok) = run_launch(&work, Some(&root), "", shell);
            assert!(ok && out == want, "shell {shell:?}: {out} {err}");
        }
        assert_eq!(launch_claude_json(&work), Some(root.join(".claude.json")));
        let env = check_env(&work, Some(&root));
        assert!(env.contains(&(CONFIG_ENV.to_string(), Some(root.to_string_lossy().into_owned()))), "the sheet's check names it too");
    }

    /// No root maiTerm knows (managed accounts off, maiTerm opened from the Dock while the shell rc
    /// exports `CLAUDE_CONFIG_DIR`): plain tabs run under the rc's root, so removing it would run
    /// the Hive as `~/.claude`'s account, silently. The line never removes it: with none in the
    /// shell Claude starts under its default, where the trust went; with one it refuses, says
    /// why, and `claude` never runs, nor a worker's setup ahead of it.
    #[cfg(unix)]
    #[test]
    fn without_a_root_the_line_refuses_a_shell_config_dir_it_cant_verify() {
        let none = crate::state::Preferences::default();
        assert_eq!(config_root_for(&none, None), None);
        assert!(!unset_vars(&none).iter().any(|u| u == CONFIG_ENV), "never removed");
        assert!(account_set_for(None).is_empty());
        assert_eq!(claude_json_for(None), dirs::home_dir().map(|h| h.join(".claude.json")));
        assert!(check_env(&none, None).contains(&(CONFIG_ENV.to_string(), None)), "the sheet checks the default root");

        let (out, err, ok) = run_launch(&none, None, "echo SETUP-RAN && ", None);
        assert!(ok && out == "SETUP-RAN\nRAN:unset", "unset in the shell: claude runs under its default: {out} {err}");

        for v in ["/Users/u/.claude-work", "/Users/o'brien/my claude", ""] {
            let (out, err, ok) = run_launch(&none, None, "echo SETUP-RAN && ", Some(v));
            assert!(!ok, "{v:?}: {out}");
            assert!(!out.contains("RAN"), "neither setup nor claude ran: {out}");
            assert_eq!(err, format!("{GUARD_BEFORE}{v}{GUARD_AFTER}\n"), "one clear line, the value exactly");
        }
        assert!(!GUARD_BEFORE.contains(['\\', '%']) && !GUARD_AFTER.contains(['\\', '%']));
    }

    /// Managed accounts off and maiTerm started with the user's own `CLAUDE_CONFIG_DIR` (from
    /// another terminal, no maiTerm markers): the scrub keeps it and every plain tab runs under it,
    /// so the launch line, the trust root and the login check name it too, never `~/.claude`.
    #[cfg(unix)]
    #[test]
    fn the_users_own_root_is_the_one_plain_tabs_and_hive_tabs_share() {
        let own = Path::new("/Users/u/.claude-work");
        let none = crate::state::Preferences::default();
        let inherited = |k: &str| (k == CONFIG_ENV).then(|| own.to_string_lossy().into_owned());
        assert!(crate::accounts::inherited_account_env(inherited, None).is_empty(), "plain tabs keep it");
        assert!(crate::accounts::spawn_env_for(&none).0.is_empty(), "and nothing per tab replaces it");

        let root = config_root_for(&none, Some(own));
        assert_eq!(root.as_deref(), Some(own));
        assert_eq!(account_set_for(root.as_deref()), vec![(CONFIG_ENV.to_string(), own.to_string_lossy().into_owned())]);
        assert!(!unset_vars_for(&none).iter().any(|u| u == CONFIG_ENV));
        assert_eq!(claude_json_for(root.as_deref()), Some(own.join(".claude.json")), "trust and the sheet read it");
        assert_eq!(trust_root_for(&none, Some(own)), TrustRoot::Dir(own.to_path_buf()), "and the refusal names it");
        assert_eq!(trust_root_for(&none, None), TrustRoot::Default);

        assert!(config_guard(root.as_deref()).is_empty());
        let (out, err, ok) = run_launch(&none, root.as_deref(), "", Some(&own.to_string_lossy()));
        assert!(ok && out == format!("RAN:{}", own.display()), "{out} {err}");

        // A managed account still wins: every tab's spawn sets its own then.
        let work = managed("acct-2");
        let acct = crate::accounts::account_root(crate::accounts::Runtime::Claude, "acct-2").unwrap();
        assert_eq!(config_root_for(&work, Some(own)), Some(acct));
        assert_eq!(trust_root_for(&work, Some(own)), TrustRoot::Account("Work".into()));
    }

    /// Trust goes into the root the launch reads, and a launch whose root holds none is refused
    /// rather than shown Claude's dialog.
    #[cfg(unix)]
    #[test]
    fn trust_is_written_to_the_launchs_own_root() {
        let base = scratch("launch-root");
        let (home_json, acct_json) = (base.join("home/.claude.json"), base.join("accounts/claude/a1/.claude.json"));
        for f in [&home_json, &acct_json] {
            fs::create_dir_all(f.parent().unwrap()).unwrap();
            fs::write(f, "{}").unwrap();
        }
        let homes = |files: Vec<PathBuf>| crate::relocate::agents::Homes {
            claude_projects: base.join("home/.claude/projects"),
            claude_json: files,
            claude_history: vec![],
            codex: base.join("codex"),
            gemini: base.join("gemini"),
        };
        let p = HivePaths::new(base.join("hive"));
        let dir = p.coordinator_home();
        fs::create_dir_all(&dir).unwrap();

        ensure_trust_in(&homes(vec![home_json.clone(), acct_json.clone()]), &acct_json, &p, &dir).unwrap();
        assert!(crate::hive::trust::trusted_in_all(&[acct_json.clone()], &dir), "in the account's own root");
        assert!(crate::hive::trust::trusted_in_all(&[home_json.clone()], &dir), "and every other");

        // A root no Claude ran under yet: refused, though another root got the entry.
        let fresh = base.join("accounts/claude/a2/.claude.json");
        let e = ensure_trust_in(&homes(vec![home_json.clone(), fresh.clone()]), &fresh, &p, &dir).unwrap_err();
        assert_eq!(e, NO_CLAUDE_YET);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn only_a_definite_version_verdict_is_cached() {
        let p = |v: Option<(u32, u32, u32)>| (PathBuf::from("/bin/claude"), v);
        assert_eq!(version_verdict(&[p(Some((2, 1, 310)))]), (Ok(()), true));
        let (old, cached) = version_verdict(&[p(Some((2, 1, 200)))]);
        assert!(old.unwrap_err().contains("2.1.200") && cached, "too old is certain");
        let (unread, cached) = version_verdict(&[p(None)]);
        assert!(unread.is_err() && !cached, "a timeout or bad output is asked again next launch");
        let (both, cached) = version_verdict(&[p(None), p(Some((2, 0, 1)))]);
        assert!(both.unwrap_err().contains("2.0.1") && cached, "a too-old one wins over an unreadable one");
    }

    /// maiTerm writes unsandboxed into an agent's home: a symlink planted at the destination or
    /// at the old pid-named temp is replaced or ignored, never written through.
    #[cfg(unix)]
    #[test]
    fn writes_never_follow_a_planted_symlink() {
        let dir = scratch("symlink");
        let outside = dir.join("outside");
        fs::write(&outside, b"the human's file").unwrap();
        let home = dir.join("home");
        fs::create_dir_all(&home).unwrap();
        let brief = home.join("brief.md");
        std::os::unix::fs::symlink(&outside, home.join(format!("brief.tmp-{}", std::process::id()))).unwrap();
        std::os::unix::fs::symlink(&outside, &brief).unwrap();
        write_bytes(&brief, b"the brief").unwrap();
        assert_eq!(fs::read(&outside).unwrap(), b"the human's file");
        assert!(!fs::symlink_metadata(&brief).unwrap().file_type().is_symlink());
        assert_eq!(fs::read(&brief).unwrap(), b"the brief");

        // Equal bytes behind a symlink are still no reason to leave the link in place.
        let twin = home.join("twin.md");
        fs::write(&outside, b"same").unwrap();
        std::os::unix::fs::symlink(&outside, &twin).unwrap();
        write_bytes(&twin, b"same").unwrap();
        assert!(!fs::symlink_metadata(&twin).unwrap().file_type().is_symlink());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn sessions_are_listed_once_each_and_the_last_is_resumed() {
        let dir = scratch("sessions");
        let e = |id: &str| SessionEntry { session_id: id.into(), transcript_path: None, started_at: "t".into() };
        assert_eq!(last_session(&dir), None);
        assert_eq!(append_session(&dir, e("a")).unwrap(), SessionAppend { added: true, first: true });
        assert_eq!(append_session(&dir, e("a")).unwrap(), SessionAppend { added: false, first: false }, "a resume reports the same id");
        assert_eq!(append_session(&dir, e("b")).unwrap(), SessionAppend { added: true, first: false });
        assert_eq!(append_session(&dir, e("")).unwrap().added, false);
        assert_eq!(read_sessions(&dir).iter().map(|e| e.session_id.as_str()).collect::<Vec<_>>(), vec!["a", "b"]);
        assert_eq!(last_session(&dir).as_deref(), Some("b"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_record_round_trips_and_a_missing_one_is_an_error() {
        let dir = scratch("record");
        assert!(read_record(&dir).is_err());
        let r = coordinator_record(&sample_hive(), &sample_paths());
        write_record(&dir, &r).unwrap();
        assert_eq!(read_record(&dir).unwrap(), r);
        fs::write(dir.join(LAUNCH_FILE), b"{not json").unwrap();
        assert!(read_record(&dir).is_err(), "unreadable is missing, never a default");
        let _ = fs::remove_dir_all(&dir);
    }

    /// Refused before anything is written: no generic fallback, no trust write, no files.
    #[test]
    fn no_record_no_launch() {
        let prefs = crate::state::Preferences::default();
        let mut h = sample_hive();
        h.id = uuid::Uuid::new_v4().to_string();
        let err = prepare_launch(&h, ROLE_COORDINATOR, "t", &[], &prefs).unwrap_err();
        assert!(err == "launch record missing" || cfg!(windows), "{err}");
        assert!(!hive_root(&h.id).unwrap().exists(), "nothing was created");
        let err = prepare_launch(&h, ROLE_WORKER, "t", &[], &prefs).unwrap_err();
        assert!(err == "launch record missing" || cfg!(windows), "no worker.json names the tab: {err}");
        assert!(!hive_root(&h.id).unwrap().exists());
        assert!(prepare_launch(&h, "reviewer", "t", &[], &prefs).is_err());
        h.id = "../escape".into();
        assert!(prepare_launch(&h, ROLE_COORDINATOR, "t", &[], &prefs).is_err());
    }

    /// A worker of `sample_hive` plus an `api` repo: worktrees of web and api, the plain brand
    /// folder, as `create_worker_worktrees` would record them.
    pub(crate) fn sample_worker_hive() -> (Hive, HivePaths, WorkerRecord) {
        let mut h = sample_hive();
        h.folders.push(HiveFolder { name: "api".into(), path: "/r/api".into(), kind: KIND_GIT.into(), base: Some("main".into()), ..HiveFolder::default() });
        let p = sample_paths();
        let git = |name: &str| WorkerFolder {
            name: name.into(),
            kind: KIND_GIT.into(),
            worktree: Some(s(&p.worker_worktree("auth", name))),
            branch: Some("hive/ship-it--auth".into()),
            target: Some("main".into()),
            base_sha: Some("abc".into()),
            common_dir: Some(format!("/r/{name}/.git")),
            admin_dir: Some(format!("/r/{name}/.git/worktrees/{name}")),
            ..WorkerFolder::default()
        };
        let w = WorkerRecord {
            task_id: "t-auth".into(),
            slug: "auth".into(),
            title: "Auth tokens".into(),
            role: "backend".into(),
            badge: 3,
            phase: crate::hive::worker::PHASE_LAUNCHING.into(),
            tab_id: Some("tab-w".into()),
            folders: vec![git("web"), git("api"), WorkerFolder { name: "brand".into(), kind: KIND_PLAIN.into(), ..WorkerFolder::default() }],
            ..WorkerRecord::default()
        };
        (h, p, w)
    }

    /// The human's decision (2026-10-26): the coordinator launches as a normal agent tab, every
    /// setting source and the account's MCP servers, no sandbox, tool list or permission mode of
    /// maiTerm's. What stays is the Hive's: its settings file (hooks, memory, push denies), its
    /// folders, model and effort.
    #[test]
    fn the_coordinator_launches_like_a_normal_agent_tab() {
        let mut r = coordinator_record(&sample_hive(), &sample_paths());
        let p = sample_paths();
        let mut want: Vec<String> = vec!["--settings".into(), r.settings_path.clone(), "--mcp-config".into(), r.mcp_config_path.clone()];
        for d in [s(&p.coordinator_worktree("web")), "/r/brand assets".to_string(), s(&p.gallery()), s(&p.coordinator_home())] {
            want.push("--add-dir".into());
            want.push(d);
        }
        want.extend(["--model", "opus", "--effort", "medium", "--resume", "sid"].map(String::from));
        assert_eq!(hive_launch_args(&r, Some("sid")), want, "an account without maiTerm's server gets it from the Hive");
        for absent in ["--strict-mcp-config", "--setting-sources", "--tools", "--permission-mode", "--disallowedTools"] {
            assert!(!hive_launch_args(&r, None).iter().any(|a| a == absent), "{absent}");
        }
        // The account serves maiTerm (`prepare_coordinator`): no second copy of it.
        r.mcp_config_path.clear();
        assert!(flag_values(&hive_launch_args(&r, None), "--mcp-config").is_empty());
        let text = serde_json::to_string(&r).unwrap();
        for absent in ["permission_mode", "disallowed_tools", "task_id"] {
            assert!(!text.contains(absent), "the coordinator's launch.json gains no {absent}");
        }
        let old: LaunchRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(old, r);
    }

    /// Workers and reviewers launch exactly as contained as before: their own MCP servers only,
    /// narrowed setting sources, auto mode.
    #[test]
    fn workers_keep_the_strict_mcp_config_and_their_narrowed_sources() {
        let (h, p, w) = sample_worker_hive();
        let mut reviewer = w.clone();
        reviewer.reviews = Some("t-other".into());
        for (who, rec) in [("worker", worker_record(&h, &p, &w)), ("reviewer", worker_record(&h, &p, &reviewer))] {
            let a = hive_launch_args(&rec, None);
            assert!(a.contains(&"--strict-mcp-config".to_string()), "{who}");
            assert_eq!(flag_values(&a, "--mcp-config"), vec![rec.mcp_config_path.as_str()], "{who}");
            assert_eq!(flag_values(&a, "--permission-mode"), vec![WORKER_PERMISSION_MODE], "{who}");
            assert_eq!(flag_values(&a, "--setting-sources").len(), 1, "{who}");
        }
    }

    #[test]
    fn the_coordinator_reads_maiterm_from_its_account_only_when_it_is_there() {
        use crate::hive::settings::has_maiterm_server;
        let key = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
        let served = serde_json::json!({ "mcpServers": { key: { "type": "http", "url": "http://127.0.0.1:1/mcp" }, "railway": { "command": "x" } } });
        assert!(has_maiterm_server(Some(&served)));
        let other = if key == "maiterm" { "maiterm-dev" } else { "maiterm" };
        let other_build = serde_json::json!({ "mcpServers": { other: { "type": "http", "url": "http://127.0.0.1:1/mcp" } } });
        assert!(!has_maiterm_server(Some(&other_build)), "another build's server is no maiTerm for this one");
        assert!(!has_maiterm_server(Some(&serde_json::json!({ "mcpServers": { key: {} } }))));
        assert!(!has_maiterm_server(Some(&serde_json::json!({}))));
        assert!(!has_maiterm_server(None));
    }

    #[test]
    fn the_worker_record_comes_from_its_worker_json() {
        let (h, p, w) = sample_worker_hive();
        let r = worker_record(&h, &p, &w);
        let web = s(&p.worker_worktree("auth", "web"));
        assert_eq!((r.role.as_str(), r.cwd.as_str(), r.task_id.as_str()), (ROLE_WORKER, web.as_str(), "t-auth"));
        assert_eq!(
            r.add_dirs,
            vec![s(&p.worker_worktree("auth", "api")), "/r/brand assets".to_string(), s(&p.worker_scratch("auth")), s(&p.gallery()), web.clone()],
            "the other worktree, the plain folder, scratch, the gallery, then the cwd"
        );
        assert_eq!((r.model.as_str(), r.effort.as_str()), ("sonnet", "high"), "the Hive's worker model when the worker names none");
        assert!(r.env.contains(&("CLAUDE_CODE_TMPDIR".into(), s(&p.worker_tmp("auth")))));
        assert!(r.env.contains(&("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".into(), "1".into())));
        assert!(!r.env.iter().any(|(k, _)| k == "CARGO_TARGET_DIR"), "no Rust folder, no target of its own");
        assert_eq!(r.setting_sources.as_deref(), Some("project"), "never the human's user settings");
        assert_eq!(r.permission_mode.as_deref(), Some(WORKER_PERMISSION_MODE));
        assert_eq!(r.disallowed_tools, vec!["WebFetch", "WebSearch"]);
        assert!(r.settings_path.starts_with(&s(&p.task("t-auth"))));

        let mut docs = w.clone();
        docs.role = "docs".into();
        docs.model = "opus".into();
        docs.effort = "max".into();
        let r = worker_record(&h, &p, &docs);
        assert_eq!(r.disallowed_tools, vec!["WebFetch"], "a web role keeps WebSearch, never WebFetch");
        assert_eq!((r.model.as_str(), r.effort.as_str()), ("opus", "max"));

        let mut scratch_only = w.clone();
        scratch_only.folders.retain(|f| f.worktree.is_none());
        let r = worker_record(&h, &p, &scratch_only);
        assert_eq!(r.cwd, s(&p.worker_scratch("auth")));
        assert_eq!(r.add_dirs, vec!["/r/brand assets".to_string(), s(&p.gallery()), s(&p.worker_scratch("auth"))], "scratch once, as the cwd");
    }

    /// Plan D1: a reviewer runs from its empty scratch under `local` sources only, so the reviewed
    /// branch's settings, hooks, MCP servers, skills, commands and agents never load, nor (with the
    /// env var gone) its CLAUDE.md.
    #[test]
    fn a_reviewer_launches_from_its_scratch_without_the_branchs_claude_md_or_setup() {
        let base = scratch("reviewer");
        let (mut h, _, builder) = sample_worker_hive();
        let p = HivePaths::new(base.join("hive/h1"));
        crate::hive::worker::write_worker(&p.task("t-auth"), &builder).unwrap();
        let mut r = builder.clone();
        r.task_id = "t-rev".into();
        r.slug = "auth-review".into();
        r.reviews = Some("t-auth".into());
        for f in r.folders.iter_mut().filter(|f| f.worktree.is_some()) {
            f.worktree = Some(s(&p.worker_worktree("auth-review", &f.name)));
            f.branch = None;
        }
        let rec = worker_record(&h, &p, &r);
        let scratch_dir = s(&p.worker_scratch("auth-review"));
        assert_eq!(rec.cwd, scratch_dir, "never the reviewed tree");
        assert_eq!(
            rec.add_dirs,
            vec![
                s(&p.worker_worktree("auth-review", "web")),
                s(&p.worker_worktree("auth-review", "api")),
                "/r/brand assets".to_string(),
                s(&p.worker_scratch("auth")),
                s(&p.gallery()),
                scratch_dir.clone(),
            ],
            "its review worktrees, the plain folder, the builder's scratch to read, the gallery, then its own scratch as cwd"
        );
        assert!(!rec.env.iter().any(|(k, _)| k == "CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD"), "{:?}", rec.env);
        assert_eq!(rec.setting_sources.as_deref(), Some("local"), "project loads the added folders' skills, commands and agents");
        let a = hive_launch_args(&rec, None);
        assert_eq!(flag_values(&a, "--setting-sources"), vec!["local"]);
        assert_eq!(worker_record(&h, &p, &builder).setting_sources.as_deref(), Some("project"), "a builder keeps project sources");
        h.folders[0].setup = Some("npm ci".into());
        assert_eq!(first_launch_setup(&h, &p, &r).unwrap(), "", "no setup over the builder's code, unsandboxed");
        assert!(first_launch_setup(&h, &p, &builder).unwrap().contains("npm ci"), "the builder's own launch runs it");
        // Corepack's home is read before the caches move it, and a Poetry venv lands in the worktree.
        let line = first_launch_setup(&h, &p, &builder).unwrap();
        let (corepack, xdg) = (line.find("COREPACK_HOME=\"${COREPACK_HOME:-").unwrap(), line.find("XDG_CACHE_HOME=").unwrap());
        assert!(corepack < xdg, "{line}");
        assert!(line.contains("COREPACK_ENABLE_DOWNLOAD_PROMPT='0'") && line.contains("POETRY_VIRTUALENVS_IN_PROJECT='true'"), "{line}");

        // A reviewer whose verdict is in never launches again (a restore, a mount): it is owed its retirement.
        r.tab_id = Some("tab-rev".into());
        crate::hive::worker::write_worker(&p.task("t-rev"), &r).unwrap();
        let mut b = builder.clone();
        b.review = Some(crate::hive::worker::Review {
            verdicts: vec![crate::hive::worker::Verdict { reviewer_task: "t-rev".into(), verdict: "fail".into(), ..Default::default() }],
            ..Default::default()
        });
        crate::hive::worker::write_worker(&p.task("t-auth"), &b).unwrap();
        let prefs = crate::state::Preferences::default();
        let e = prepare_worker(&h, &p, "tab-rev", &[], &prefs).unwrap_err();
        assert!(e.contains("has given its verdict"), "{e}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn worker_arguments_carry_auto_mode_and_the_removed_tools() {
        let (h, p, w) = sample_worker_hive();
        let r = worker_record(&h, &p, &w);
        let a = hive_launch_args(&r, Some("sid-9"));
        assert_eq!(flag_values(&a, "--permission-mode"), vec!["auto"]);
        assert_eq!(flag_values(&a, "--disallowedTools"), vec!["WebFetch,WebSearch"]);
        assert!(flag_values(&a, "--tools").is_empty());
        assert_eq!(flag_values(&a, "--setting-sources"), vec!["project"]);
        assert_eq!(flag_values(&a, "--add-dir").len(), r.add_dirs.len());
        assert_eq!(a.last().map(String::as_str), Some("sid-9"), "--resume after the variadic flags");
    }

    /// The cargo target is decided once: a Rust worker gets its own whether or not the human
    /// has built, and the record keeps it across a human's `cargo build` or `cargo clean`.
    #[test]
    fn the_cargo_target_stays_put_across_relaunches() {
        let base = scratch("cargo");
        fs::create_dir_all(base.join("web/src-tauri")).unwrap();
        fs::write(base.join("web/src-tauri/Cargo.toml"), "[package]\n").unwrap();
        let (mut h, p, mut w) = sample_worker_hive();
        h.folders[0].path = s(&base.join("web"));
        w.folders.retain(|f| f.name == "web");
        let target = |w: &WorkerRecord| worker_record(&h, &p, w).env.iter().find(|(k, _)| k == "CARGO_TARGET_DIR").map(|(_, v)| v.clone());
        assert_eq!(target(&w), Some(s(&p.worker_cargo_target("auth"))), "a Rust folder with nothing built yet");
        assert!(crate::hive::worktree::worker_seed_source(&h, &w).is_none());
        fs::create_dir_all(base.join("web/src-tauri/target/debug/.fingerprint")).unwrap();
        assert_eq!(target(&w), Some(s(&p.worker_cargo_target("auth"))), "the human's build changes nothing");

        w.cargo_target = crate::hive::worker::CARGO_TARGET_NONE.into();
        assert_eq!(target(&w), None, "decided at its first launch: it keeps building where it did");
        w.cargo_target = crate::hive::worker::CARGO_TARGET_OWN.into();
        fs::remove_dir_all(base.join("web")).unwrap();
        assert_eq!(target(&w), Some(s(&p.worker_cargo_target("auth"))), "a cargo clean, even the folder gone");
        let _ = fs::remove_dir_all(&base);
    }

    // POSIX-shell only, like the coordinator's.
    #[cfg(unix)]
    #[test]
    fn the_worker_line_with_setup_is_valid_shell() {
        let (mut h, _, mut w) = sample_worker_hive();
        let p = HivePaths::new("/Users/o'brien/Library/Application Support/com.aiterm.dev/hive/h1");
        for f in &mut w.folders {
            if let Some(wt) = f.worktree.as_mut() {
                *wt = s(&p.worker_worktree("auth", &f.name));
            }
        }
        h.folders[0].setup = Some("npm ci --no-audit".into());
        h.folders[2].setup = Some("  ".into());
        let r = worker_record(&h, &p, &w);
        let line = format!("{}{}", first_launch_setup(&h, &p, &w).unwrap(), hive_launch_line(&r, &hive_launch_args(&r, None), &["CLAUDECODE".to_string()], &[]));
        assert!(line.starts_with("( cd '/Users/o'\\''brien/Library/Application Support/"), "{line}");
        assert_eq!(line.matches("( cd ").count(), 1, "a blank setup is none: {line}");
        assert!(line.contains(" && npm ci --no-audit ) && cd "), "{line}");
        let out = std::process::Command::new("sh").arg("-n").arg("-c").arg(&line).output().expect("sh is available");
        assert!(out.status.success(), "not valid shell:\n{line}\n{}", String::from_utf8_lossy(&out.stderr));

        h.folders[0].setup = Some("npm ci\nrm -rf ~".into());
        assert!(first_launch_setup(&h, &p, &w).unwrap_err().contains("line break"));
    }

    #[test]
    fn a_fresh_start_resets_the_kickoff() {
        let dir = scratch("kickoff");
        assert!(kickoff_pending(&dir));
        reset_kickoff(&dir).unwrap();
        mark_kickoff_delivered(&dir).unwrap();
        assert!(!kickoff_pending(&dir));
        reset_kickoff(&dir).unwrap();
        assert!(kickoff_pending(&dir), "told again in the new session");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_untrusted_worktree_repo_refuses_the_launch() {
        let base = scratch("trust");
        let root = base.join(".claude.json");
        fs::write(&root, r#"{"projects":{}}"#).unwrap();
        let (_, p, w) = sample_worker_hive();
        let homes = |files: Vec<PathBuf>| crate::relocate::agents::Homes {
            claude_projects: base.join("projects"),
            claude_json: files,
            claude_history: vec![],
            codex: base.join("codex"),
            gemini: base.join("gemini"),
        };
        let work = TrustRoot::Account("Work".into());
        let err = check_worker_trust(&homes(vec![root.clone()]), &root, &work, &p, &w).unwrap_err();
        assert!(err.contains("trust dialog") && err.contains("\"web\""), "{err}");
        // How to trust it: the account, the human's repo, a maiTerm tab; never a copy.
        assert!(err.contains("the Claude account \"Work\"") && err.contains("run `claude` once in /r/web") && err.contains("maiTerm tab"), "{err}");
        assert!(err.contains("A worktree or read copy doesn't count"), "{err}");
        let missing = base.join("missing.json");
        assert_eq!(check_worker_trust(&homes(vec![missing.clone()]), &missing, &TrustRoot::Default, &p, &w).unwrap_err(), NO_CLAUDE_YET, "no root a Claude ran under");
        // The main checkouts, read from the recorded common dirs, cover the worktrees.
        fs::write(&root, r#"{"projects":{"/r/web":{"hasTrustDialogAccepted":true},"/r/api":{"hasTrustDialogAccepted":true}}}"#).unwrap();
        assert_eq!(check_worker_trust(&homes(vec![root.clone()]), &root, &TrustRoot::Default, &p, &w), Ok(()));
        // Trusted elsewhere, but the launch's own root has never run a Claude: refused.
        assert_eq!(check_worker_trust(&homes(vec![root.clone(), missing.clone()]), &missing, &TrustRoot::Default, &p, &w).unwrap_err(), NO_CLAUDE_YET);
        // The live test's case: trusted in the launch's root only, untrusted in `~/.claude.json`,
        // which a managed account's launch never reads. It starts.
        let plain_home = base.join("home.claude.json");
        fs::write(&plain_home, r#"{"projects":{}}"#).unwrap();
        assert_eq!(check_worker_trust(&homes(vec![plain_home.clone(), root.clone()]), &root, &work, &p, &w), Ok(()));
        // And the reverse: trusted only in `~/.claude.json` is no trust for this launch.
        let err = check_worker_trust(&homes(vec![root.clone(), plain_home.clone()]), &plain_home, &TrustRoot::Default, &p, &w).unwrap_err();
        assert!(err.contains("Claude's default account (~/.claude)"), "{err}");
        // The user's own `CLAUDE_CONFIG_DIR` is the root: the refusal names it, never `~/.claude`.
        let own = TrustRoot::Dir(PathBuf::from("/Users/u/.claude-work"));
        let err = check_worker_trust(&homes(vec![plain_home.clone()]), &plain_home, &own, &p, &w).unwrap_err();
        assert!(err.contains("the Claude config at /Users/u/.claude-work") && !err.contains("~/.claude)"), "{err}");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn a_done_hive_launches_nothing_and_touches_no_folder() {
        let id = format!("done-{}", uuid::Uuid::new_v4());
        let h = Hive { id: id.clone(), status: crate::hive::record::STATUS_DONE.into(), ..sample_hive() };
        let prefs = crate::state::Preferences::default();
        for role in [ROLE_COORDINATOR, ROLE_WORKER, "visitor"] {
            assert_eq!(prepare_launch(&h, role, "tab", &[], &prefs), Err(DONE_REFUSAL.to_string()), "{role}: before the role is even read");
        }
        assert!(!hive_root(&id).unwrap().exists(), "no record rewritten, no trust written, no Claude probed");
        assert!(DONE_REFUSAL.starts_with("This Hive is done"), "launch.ts matches this start");
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("2.1.310 (Claude Code)\n"), Some((2, 1, 310)));
        assert_eq!(parse_version("garbage"), None);
        assert!(parse_version("2.1.294 (Claude Code)").unwrap() < MIN_CLAUDE);
        assert!(parse_version("2.2.0").unwrap() >= MIN_CLAUDE);
    }
}
