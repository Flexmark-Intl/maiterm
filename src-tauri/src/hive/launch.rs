//! The launch record and the one builder every Hive agent launch goes through (docs/hive.md
//! §11.1).
//!
//! A Hive agent's first launch carries everything that makes it one: its folders as
//! `--add-dir`, the sandbox, deny rules and command hooks in `--settings`, only maiTerm's MCP
//! server, its tool set, model and effort. A generic `claude --resume <id>` drops all of it, so
//! a `hive_role` tab never starts any other way: no record, no launch.

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

/// The coordinator's built-in tools, by name: a pattern here silently drops the tool (spike 13).
pub const COORDINATOR_TOOLS: &str = "Read,Grep,Glob,Bash,Write,Edit";

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
    /// `--setting-sources`, when the launch narrows them.
    pub setting_sources: Option<String>,
    pub settings_path: String,
    pub mcp_config_path: String,
    /// `--permission-mode` for a worker (`"auto"`); the coordinator's `dontAsk` is fixed in the
    /// builder, never read from here.
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
/// each git folder, the plain folders and the gallery as `--add-dir`, and its own temp (§6.3,
/// §6.7). `--setting-sources project` keeps the user's own allow rules out: under dontAsk the
/// launch's allow list must be the whole list (§7.1), and its home holds no project settings
/// (they're on its `denyWrite`).
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
        setting_sources: Some("project".to_string()),
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
/// own cargo target. Project settings only, as the coordinator: `--settings` outranks only the
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
    add(s(&paths.worker_scratch(&w.slug)));
    add(s(&paths.gallery()));
    // Also its cwd, so the sandbox and permission scopes name it the same way (spike 13).
    add_dirs.retain(|d| Path::new(d) != cwd);
    add_dirs.push(s(&cwd));

    let mut env = vec![
        ("CLAUDE_CODE_ADDITIONAL_DIRECTORIES_CLAUDE_MD".to_string(), "1".to_string()),
        ("CLAUDE_CODE_TMPDIR".to_string(), s(&paths.worker_tmp(&w.slug))),
    ];
    if super::worktree::worker_uses_own_cargo_target(hive, w) {
        env.push(("CARGO_TARGET_DIR".to_string(), s(&paths.worker_cargo_target(&w.slug))));
    }
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
        setting_sources: Some("project".to_string()),
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
    use std::io::Write as _;
    let is_link = fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if !is_link && fs::read(path).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    let err = |e: std::io::Error| format!("{}: {e}", path.display());
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(err)?;
    let written = f.write_all(bytes).and_then(|_| f.sync_all());
    drop(f);
    written.and_then(|_| fs::rename(&tmp, path)).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        err(e)
    })
}

/// The whole command line after `claude`. `--model` and `--effort` on every launch: on resume
/// Claude keeps the model but drops effort to medium (spike 11), and the record, not the resumed
/// session, decides both.
pub fn hive_launch_args(record: &LaunchRecord, resume: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = Vec::new();
    let mut push = |k: &str, v: &str| {
        a.push(k.to_string());
        a.push(v.to_string());
    };
    push("--settings", &record.settings_path);
    push("--mcp-config", &record.mcp_config_path);
    a.push("--strict-mcp-config".to_string());
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
    if record.role == ROLE_COORDINATOR {
        a.extend(["--tools", COORDINATOR_TOOLS, "--permission-mode", "dontAsk"].map(String::from));
    } else if let Some(mode) = record.permission_mode.as_deref().filter(|m| !m.is_empty()) {
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
/// and setting the record's env, then `claude` and the arguments. Every argument is quoted
/// (the paths hold "Application Support"), and the line is typed raw, never through trigger
/// interpolation.
pub fn hive_launch_line(record: &LaunchRecord, args: &[String], unset: &[String]) -> String {
    let mut line = format!("cd {} && env", sh_quote(&record.cwd));
    for u in unset {
        if !record.env.iter().any(|(k, _)| k == u) {
            line.push_str(" -u ");
            line.push_str(u);
        }
    }
    for (k, v) in &record.env {
        line.push_str(&format!(" {k}={}", sh_quote(v)));
    }
    line.push_str(" claude");
    for a in args {
        line.push(' ');
        line.push_str(&sh_quote(a));
    }
    line
}

/// What the launch line removes: the agent markers the PTY spawn scrubs, the parent-session
/// vars above, and, under a managed account, every variable that would answer instead of the
/// account's login (docs/login.md §6.1). Names only, never values.
pub fn unset_vars(prefs: &crate::state::Preferences) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let names = crate::state::agent_runtime::AGENT_ENV_MARKERS
        .iter()
        .chain(HIVE_EXTRA_UNSET)
        .map(|s| s.to_string())
        .chain(crate::accounts::spawn_env_for(prefs).1);
    for n in names {
        let is_name = !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if is_name && !out.contains(&n) {
            out.push(n);
        }
    }
    out
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
    write_json(Path::new(&record.mcp_config_path), &super::settings::hive_mcp_config())?;
    if record.role == ROLE_COORDINATOR {
        write_bytes(&paths.coordinator_brief(), super::brief::brief_md(hive, paths).as_bytes())?;
    }
    Ok(())
}

/// A worker's launch files: its settings (§6.7), the MCP config, and `scratch/brief.md` (§7.1).
pub fn write_worker_launch_files(
    hive: &Hive,
    paths: &HivePaths,
    record: &LaunchRecord,
    w: &WorkerRecord,
    ctx: &super::settings::WorkerContext,
    task_brief: &str,
    hooks: &super::settings::HookEnv,
) -> Result<(), String> {
    let settings = super::settings::worker_settings_json(hive, record, w, paths, ctx, hooks)?;
    write_json(Path::new(&record.settings_path), &settings)?;
    write_json(Path::new(&record.mcp_config_path), &super::settings::hive_mcp_config())?;
    write_bytes(&paths.worker_brief(&w.slug), super::brief::worker_brief_md(hive, paths, w, task_brief).as_bytes())
}

const NO_CLAUDE_YET: &str =
    "Claude Code hasn't run on this computer yet, so there's nowhere to record folder trust. Run claude once in any tab, then try again.";

/// Trust a folder maiTerm made itself (the coordinator's home, a scratch-only worker's scratch)
/// in every root, re-verified before every launch: the tab may come up under another account
/// than the one this was last written for, and nothing may answer the dialog (§6.6).
fn ensure_trust(paths: &HivePaths, dir: &Path) -> Result<(), String> {
    let homes = crate::relocate::agents::Homes::real().ok_or("Couldn't find the home folder")?;
    let trust = super::trust::write_trust(&homes, paths, dir)?;
    if !trust.failed.is_empty() {
        return Err(format!("Couldn't record Claude's folder trust: {}", trust.failed.join("; ")));
    }
    if trust.written.is_empty() && trust.already.is_empty() {
        return Err(NO_CLAUDE_YET.to_string());
    }
    Ok(())
}

/// A worktree inherits its main repo's trust (§6.6), so maiTerm writes none for a worker's: a
/// repo the human hasn't trusted is theirs to trust, and the launch is refused instead of
/// raising the dialog. Checked in every root a Claude has run under.
fn check_worker_trust(claude_json: &[PathBuf], paths: &HivePaths, w: &WorkerRecord) -> Result<(), String> {
    if w.git_folders().next().is_none() {
        return ensure_trust(paths, &paths.worker_scratch(&w.slug));
    }
    let roots: Vec<PathBuf> = claude_json.iter().filter(|f| f.is_file()).cloned().collect();
    if roots.is_empty() {
        return Err(NO_CLAUDE_YET.to_string());
    }
    for f in w.git_folders() {
        let wt = f.worktree.as_deref().unwrap_or_default();
        if !super::trust::trusted_in_all(&roots, Path::new(wt)) {
            return Err(format!(
                "Claude Code doesn't trust the repo behind \"{}\", so starting this worker would raise its trust dialog, which \
                 nothing may answer for you. Open that repo in Claude Code once and trust it, then restart the worker.",
                f.name
            ));
        }
    }
    Ok(())
}

/// What runs before a worker's first session (§6.4, §6.5): the slim cargo seed, then each git
/// folder's setup command in its worktree, typed ahead of the agent so a failure shows in the
/// raw terminal and stops the launch. Setup is the human's command, run on a fresh worktree at
/// the landing target's tip, before the agent has written anything; it moves under the worker's
/// sandbox profile with the run queue (step 5b).
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
    for f in w.git_folders() {
        let setup = hive.folders.iter().find(|h| h.name == f.name).and_then(|h| h.setup.as_deref()).map(str::trim).filter(|s| !s.is_empty());
        let (Some(setup), Some(wt)) = (setup, f.worktree.as_deref()) else { continue };
        if setup.chars().any(char::is_control) {
            return Err(format!("The setup command of \"{}\" has a line break in it, so it can't be typed as one line. Fix it in the Hive's settings.", f.name));
        }
        prefix.push_str(&format!("( cd {} && {setup} ) && ", sh_quote(wt)));
    }
    Ok(prefix)
}

/// The launch line for a Hive tab (§11.1), or why it can't start. `tasks` is the workspace's
/// task list, where a worker's brief is its task's detail. Blocks: trust writes, the version
/// probe, file writes, and before a worker's first session its cargo seed.
pub fn prepare_launch(hive: &Hive, role: &str, tab_id: &str, tasks: &[crate::state::Task], prefs: &crate::state::Preferences) -> Result<String, String> {
    if cfg!(windows) {
        return Err("A Hive needs Claude's sandbox, which native Windows doesn't have.".to_string());
    }
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
    let homes = crate::relocate::agents::Homes::real().ok_or("Couldn't find the home folder")?;
    check_worker_trust(&homes.claude_json, paths, &w)?;

    let hooks = super::settings::HookEnv::current()?;
    let ctx = super::settings::WorkerContext::gather(hive, paths, &w);
    let brief = tasks.iter().find(|t| t.id == w.task_id).and_then(|t| t.detail.clone()).unwrap_or_default();
    write_worker_launch_files(hive, paths, &record, &w, &ctx, &brief, &hooks)?;
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
    Ok(format!("{setup}{}", hive_launch_line(&record, &args, &unset_vars(prefs))))
}

fn prepare_coordinator(hive: &Hive, paths: &HivePaths, prefs: &crate::state::Preferences) -> Result<String, String> {
    let task = paths.coordinator_task();
    // No record, no launch: the fallback would be exactly the generic launch this replaces.
    let existing = read_record(&task).map_err(|e| {
        log::warn!("hive {}: {e}", hive.id);
        "launch record missing".to_string()
    })?;
    claude_new_enough()?;

    // The coordinator's record is all derived from the Hive, so a folder added since the last
    // launch reaches this one.
    let record = coordinator_record(hive, paths);
    if record != existing {
        write_record(&task, &record)?;
    }

    ensure_trust(paths, &paths.coordinator_home())?;

    let hooks = super::settings::HookEnv::current()?;
    write_launch_files(hive, paths, &record, &hooks)?;
    let resume = last_session(&task);
    let args = hive_launch_args(&record, resume.as_deref());
    log::info!("hive {}: launching the coordinator ({})", hive.id, if resume.is_some() { "resume" } else { "first launch" });
    Ok(hive_launch_line(&record, &args, &unset_vars(prefs)))
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
            assert!(a.contains(&"--strict-mcp-config".to_string()));
            assert_eq!(flag_values(&a, "--add-dir").len(), r.add_dirs.len());
            assert_eq!(flag_values(&a, "--permission-mode"), vec!["dontAsk"]);
            let tools = flag_values(&a, "--tools");
            assert_eq!(tools.len(), 1);
            assert!(!tools[0].contains('(') && !tools[0].contains(' '), "names only: {}", tools[0]);
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
        let line = hive_launch_line(&r, &args, &unset);
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
        let line = hive_launch_line(&coordinator_record(&sample_hive(), &sample_paths()), &[], &u);
        assert!(line.contains(" -u ANTHROPIC_API_KEY "), "{line}");
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

    #[test]
    fn the_coordinators_arguments_are_unchanged() {
        let r = coordinator_record(&sample_hive(), &sample_paths());
        let p = sample_paths();
        let mut want: Vec<String> = vec!["--settings".into(), r.settings_path.clone(), "--mcp-config".into(), r.mcp_config_path.clone()];
        want.extend(["--strict-mcp-config", "--setting-sources", "project"].map(String::from));
        for d in [s(&p.coordinator_worktree("web")), "/r/brand assets".to_string(), s(&p.gallery()), s(&p.coordinator_home())] {
            want.push("--add-dir".into());
            want.push(d);
        }
        want.extend(["--model", "opus", "--effort", "medium", "--tools", COORDINATOR_TOOLS, "--permission-mode", "dontAsk", "--resume", "sid"].map(String::from));
        assert_eq!(hive_launch_args(&r, Some("sid")), want);
        let text = serde_json::to_string(&r).unwrap();
        for absent in ["permission_mode", "disallowed_tools", "task_id"] {
            assert!(!text.contains(absent), "the coordinator's launch.json gains no {absent}");
        }
        // A record written before these fields reads as it always did.
        let old: LaunchRecord = serde_json::from_str(&text).unwrap();
        assert_eq!(old, r);
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
        let line = format!("{}{}", first_launch_setup(&h, &p, &w).unwrap(), hive_launch_line(&r, &hive_launch_args(&r, None), &["CLAUDECODE".to_string()]));
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
        let err = check_worker_trust(&[root.clone()], &p, &w).unwrap_err();
        assert!(err.contains("trust dialog") && err.contains("\"web\""), "{err}");
        assert_eq!(check_worker_trust(&[base.join("missing.json")], &p, &w).unwrap_err(), NO_CLAUDE_YET, "no root a Claude ran under");
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("2.1.310 (Claude Code)\n"), Some((2, 1, 310)));
        assert_eq!(parse_version("garbage"), None);
        assert!(parse_version("2.1.294 (Claude Code)").unwrap() < MIN_CLAUDE);
        assert!(parse_version("2.2.0").unwrap() >= MIN_CLAUDE);
    }
}
