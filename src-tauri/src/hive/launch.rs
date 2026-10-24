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
use super::ROLE_COORDINATOR;
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
}

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

/// The launch line for a Hive tab (§11.1), or why it can't start. Blocks: trust writes, the
/// version probe, file writes.
pub fn prepare_launch(hive: &Hive, role: &str, prefs: &crate::state::Preferences) -> Result<String, String> {
    if cfg!(windows) {
        return Err("A Hive needs Claude's sandbox, which native Windows doesn't have.".to_string());
    }
    if role != ROLE_COORDINATOR {
        return Err("Only a Hive's coordinator can be launched in this version.".to_string());
    }
    let root = hive_root(&hive.id).ok_or_else(|| format!("\"{}\" can't name a Hive folder", hive.id))?;
    let paths = HivePaths::new(root);
    let task = paths.coordinator_task();
    // No record, no launch: the fallback would be exactly the generic launch this replaces.
    let existing = read_record(&task).map_err(|e| {
        log::warn!("hive {}: {e}", hive.id);
        "launch record missing".to_string()
    })?;
    claude_new_enough()?;

    // The coordinator's record is all derived from the Hive, so a folder added since the last
    // launch reaches this one.
    let record = coordinator_record(hive, &paths);
    if record != existing {
        write_record(&task, &record)?;
    }

    // Re-verified before every launch, in every root: the tab may come up under another account
    // than the one this was last written for, and nothing may answer the dialog (§6.6).
    let homes = crate::relocate::agents::Homes::real().ok_or("Couldn't find the home folder")?;
    let trust = super::trust::write_trust(&homes, &paths, &paths.coordinator_home())?;
    if !trust.failed.is_empty() {
        return Err(format!("Couldn't record Claude's folder trust: {}", trust.failed.join("; ")));
    }
    if trust.written.is_empty() && trust.already.is_empty() {
        return Err("Claude Code hasn't run on this computer yet, so there's nowhere to record folder trust. Run claude once in any tab, then try again.".to_string());
    }

    let hooks = super::settings::HookEnv::current()?;
    write_launch_files(hive, &paths, &record, &hooks)?;
    let resume = last_session(&task);
    let args = hive_launch_args(&record, resume.as_deref());
    log::info!("hive {}: launching the coordinator ({})", hive.id, if resume.is_some() { "resume" } else { "first launch" });
    Ok(hive_launch_line(&record, &args, &unset_vars(prefs)))
}

/// The line a Hive tab's shell runs to start (or resume) its agent. The frontend types it as
/// is; an error is shown, and nothing else is ever typed in its place.
#[tauri::command]
pub async fn hive_launch_command(state: State<'_, Arc<AppState>>, tab_id: String) -> Result<String, String> {
    let (hive, role, prefs) = {
        let data = state.app_data.read();
        let (ws, tab) = super::find_hive_tab(&data, &tab_id).ok_or("Not a Hive tab")?;
        let hive = ws.hive.clone().ok_or("Not a Hive tab")?;
        (hive, tab.hive_role.clone().unwrap_or_default(), data.preferences.clone())
    };
    tauri::async_runtime::spawn_blocking(move || prepare_launch(&hive, &role, &prefs))
        .await
        .map_err(|e| e.to_string())?
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::hive::record::{HiveFolder, KIND_GIT, KIND_PLAIN};

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
        let err = prepare_launch(&h, ROLE_COORDINATOR, &prefs).unwrap_err();
        assert!(err == "launch record missing" || cfg!(windows), "{err}");
        assert!(!hive_root(&h.id).unwrap().exists(), "nothing was created");
        assert!(prepare_launch(&h, crate::hive::ROLE_WORKER, &prefs).is_err());
        h.id = "../escape".into();
        assert!(prepare_launch(&h, ROLE_COORDINATOR, &prefs).is_err());
    }

    #[test]
    fn versions_parse_and_compare() {
        assert_eq!(parse_version("2.1.310 (Claude Code)\n"), Some((2, 1, 310)));
        assert_eq!(parse_version("garbage"), None);
        assert!(parse_version("2.1.294 (Claude Code)").unwrap() < MIN_CLAUDE);
        assert!(parse_version("2.2.0").unwrap() >= MIN_CLAUDE);
    }
}
