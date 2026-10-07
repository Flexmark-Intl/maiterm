//! Follow-up watch scripts (docs/follow-ups.md §5.1): an agent writes the condition it is waiting
//! on as a script, and this runs it on a schedule — out of band, never in the tab — until the
//! script says the condition is met. Then the follow-up is MET here, in Rust, exactly as the
//! frontend meets an event one, and from there it is an ordinary due follow-up: the frontend's
//! tick delivers it through the full gate.
//!
//! Why Rust and not the frontend's tick: an occluded webview is throttled when the screens sleep,
//! and a check that waits hours must not depend on a window being drawn.
//!
//! The script's contract: exit 0 = met (its stdout goes to the agent), exit 1 = not yet, anything
//! else = broken. Three broken runs in a row meet it anyway, saying so — a typo is reported to the
//! agent rather than left to wait out its expiry.
//!
//! What a run gets: its own process group, stdin from /dev/null, the folder the tab was in when it
//! was created, and a minimal environment — never the agent's (no account credentials, no
//! `MAITERM_AUTH`). When it exits, or times out, the WHOLE group is killed, so nothing it started
//! outlives the run: the thing an agent-started watcher leaks, and the reason agent runtimes now
//! kill background jobs.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde::Serialize;
use sha2::Digest;
use tauri::Emitter;

use crate::state::workspace::FollowUp;
use crate::state::{AppData, AppState};

mod remote;

/// The ssh destination args of a live bridge tunnel to `host` (a tunnel's `host_key`): a remote
/// script runs only over one, so the connection is maiTerm's own and already authenticated.
fn tunnel_args(state: &AppState, host: &str) -> Option<String> {
    state.ssh_tunnels.read().values().find(|t| t.host_key == host).map(|t| t.ssh_args.clone())
}

pub const MIN_EVERY_SECS: u32 = 15;
pub const MAX_EVERY_SECS: u32 = 24 * 60 * 60;
pub const MAX_TIMEOUT_SECS: u32 = 60;
pub const MAX_SCRIPT_BYTES: usize = 16 * 1024;
/// Approvals kept. A digest is 64 bytes; the oldest go first.
pub const MAX_APPROVALS: usize = 500;
/// Scripts running at once, across the app. A run still going when its next is due is skipped,
/// not queued, so a slow script can't pile up.
const MAX_RUNNING: usize = 4;
const BROKEN_AFTER: u32 = 3;
const LOOP_SECS: u64 = 5;
const SWEEP_SECS: u64 = 60;
/// Read from each of stdout and stderr. Past it the pipe is dropped, and a script still writing
/// gets SIGPIPE — which reads as broken, the right answer for one that spews.
const OUTPUT_CAP: u64 = 64 * 1024;
const REPORT_CHARS: usize = 2000;
const STDERR_CHARS: usize = 600;

/// The event the frontend mirrors a Rust-side change to a tab's follow-ups from.
pub const CHANGED_EVENT: &str = "follow-ups-changed";

/// What one script's runs have come to, since this launch.
#[derive(Debug, Clone, Default, Serialize)]
pub struct WatchStatus {
    /// RFC 3339.
    pub last_run_at: Option<String>,
    /// "met" | "not_yet" | "broken"
    pub last_result: Option<String>,
    /// Why the last run counted as broken.
    pub detail: Option<String>,
    /// Broken runs in a row.
    pub broken_runs: u32,
    pub running: bool,
}

#[derive(Default)]
struct Run {
    started: Option<Instant>,
    status: WatchStatus,
}

fn runs() -> &'static Mutex<HashMap<String, Run>> {
    static RUNS: OnceLock<Mutex<HashMap<String, Run>>> = OnceLock::new();
    RUNS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn status() -> HashMap<String, WatchStatus> {
    runs().lock().iter().map(|(id, r)| (id.clone(), r.status.clone())).collect()
}

/// Where a script runs, as one string: its folder on this computer ("/repo", "~/repo"), or
/// `user@host:folder` for an ssh tab's. What the approval is keyed by and what a card shows. The
/// two can't collide: a local folder starts with `/` or `~`, a remote place with the user name.
pub fn place(host: Option<&str>, cwd: &str) -> String {
    match host {
        Some(h) => format!("{h}:{cwd}"),
        None => cwd.to_string(),
    }
}

/// A script follow-up's place (`place`); None without a folder.
pub fn place_of(f: &FollowUp) -> Option<String> {
    f.due.cwd.as_deref().map(|cwd| place(f.due.host.as_deref(), cwd))
}

/// The approval key: the script and the place it runs. The same script somewhere else is a
/// different thing to approve — `rm -rf build` means what the folder (and the machine) make it
/// mean. A local place is the bare folder, so approvals kept before remote scripts still hold.
pub fn script_hash(place: &str, script: &str) -> String {
    let mut h = sha2::Sha256::new();
    h.update(place.as_bytes());
    h.update([0u8]);
    h.update(script.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Has a HUMAN approved this script, in this place? The preference that waives approval is NOT
/// part of the answer: it is read live, where scripts are picked to run (`collect`), so turning it
/// off withdraws what it let through. Stored as `approved`, it would have approved for good a
/// script no human ever saw (review of afaafbc).
pub fn is_approved(app_data: &AppData, place: &str, script: &str) -> bool {
    app_data.approved_watch_scripts.contains(&script_hash(place, script))
}

pub fn remember_approval(app_data: &mut AppData, place: &str, script: &str) {
    let hash = script_hash(place, script);
    let list = &mut app_data.approved_watch_scripts;
    list.retain(|h| *h != hash);
    list.push(hash);
    if list.len() > MAX_APPROVALS {
        let excess = list.len() - MAX_APPROVALS;
        list.drain(..excess);
    }
}

/// One script due to be considered this pass.
#[derive(Clone)]
struct Candidate {
    id: String,
    tab_id: String,
    script: String,
    cwd: String,
    /// The ssh host it runs on; None: this computer.
    host: Option<String>,
    every: Duration,
    timeout: Duration,
}

enum Outcome {
    Met(String),
    NotYet,
    Broken(String),
    /// A remote script's host can't be reached right now (no tunnel, ssh failed to connect). Not
    /// the script's fault, so not a broken run: it waits, and three of these never "break" it.
    Unreachable(String),
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn is_waiting_script(f: &FollowUp) -> bool {
    f.due.kind == "script" && f.due.met_at.is_none()
}

/// The scripts that should be running, and every script follow-up id there is at all (archived,
/// unapproved and met ones included — what the file sweep and the status map may keep).
fn collect(app_data: &AppData) -> (Vec<Candidate>, HashSet<String>) {
    let now = now_ms();
    // Read here as well as at creation: turning the preference on releases scripts already
    // waiting for approval.
    let unattended = app_data.preferences.follow_ups_scripts_unattended;
    let mut out = Vec::new();
    let mut ids = HashSet::new();
    for ws in app_data.windows.iter().flat_map(|w| w.workspaces.iter()) {
        for t in ws.archived_tabs.iter() {
            ids.extend(t.follow_ups.iter().filter(|f| f.due.kind == "script").map(|f| f.id.clone()));
        }
        // A parked workspace's tabs wait, like their deliveries do (§6.2); an exempt one's are
        // invisible to everything under the Overlord. Archived tabs wait for their restore.
        let skip_ws = ws.suspended || ws.overlord_exempt;
        for tab in ws.panes.iter().flat_map(|p| p.tabs.iter()) {
            for f in &tab.follow_ups {
                if f.due.kind != "script" {
                    continue;
                }
                ids.insert(f.id.clone());
                if skip_ws || tab.overlord_exempt || !is_waiting_script(f) || !(f.due.approved || unattended) {
                    continue;
                }
                let expired = f.expires_at.as_deref().map(crate::mailink::transcript::rfc3339_to_ms)
                    .is_some_and(|t| t > 0 && t < now);
                let (Some(script), Some(cwd)) = (&f.due.script, &f.due.cwd) else { continue };
                if expired {
                    continue;
                }
                out.push(Candidate {
                    id: f.id.clone(),
                    tab_id: tab.id.clone(),
                    script: script.clone(),
                    cwd: cwd.clone(),
                    host: f.due.host.clone(),
                    every: Duration::from_secs(f.due.every_secs.unwrap_or(60).clamp(MIN_EVERY_SECS, MAX_EVERY_SECS) as u64),
                    timeout: Duration::from_secs(f.due.timeout_secs.unwrap_or(10).clamp(1, MAX_TIMEOUT_SECS) as u64),
                });
            }
        }
    }
    (out, ids)
}

fn scripts_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(crate::state::persistence::app_data_slug()).join("watch-scripts"))
}

/// Script and state files whose follow-up is gone — delivered, cancelled, expired and cleared,
/// closed with its tab. Ids are kept for as long as ANY tab holds the follow-up, so a reload
/// (which moves it under the same id) keeps its state.
fn sweep(ids: &HashSet<String>) {
    let Some(dir) = scripts_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        if !ids.contains(stem) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// The loop. Spawned once at setup; idles cheaply when nothing is waiting.
pub async fn watch_loop(state: Arc<AppState>, app: tauri::AppHandle) {
    let mut ticker = tokio::time::interval(Duration::from_secs(LOOP_SECS));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_sweep: Option<Instant> = None;
    loop {
        ticker.tick().await;
        let (live, candidates, ids) = {
            let app_data = state.app_data.read();
            let (c, ids) = collect(&app_data);
            (app_data.preferences.follow_ups_live(), c, ids)
        };
        // A running entry stays until its run ends, so MAX_RUNNING still counts it.
        runs().lock().retain(|id, r| ids.contains(id) || r.status.running);
        // In line, before this pass starts anything: a sweep running beside the runs could take
        // the files of a follow-up created after the snapshot it was given. It is one small
        // directory listing.
        if last_sweep.is_none_or(|t| t.elapsed() >= Duration::from_secs(SWEEP_SECS)) {
            last_sweep = Some(Instant::now());
            sweep(&ids);
        }
        // Off means held (§4): nothing runs, and nothing is lost.
        if !live {
            continue;
        }
        let mut map = runs().lock();
        let mut running = map.values().filter(|r| r.status.running).count();
        for c in candidates {
            if running >= MAX_RUNNING {
                break;
            }
            let r = map.entry(c.id.clone()).or_default();
            if r.status.running || r.started.is_some_and(|t| t.elapsed() < c.every) {
                continue;
            }
            r.started = Some(Instant::now());
            r.status.running = true;
            running += 1;
            let (state, app) = (state.clone(), app.clone());
            tauri::async_runtime::spawn(async move { run_and_record(state, app, c).await });
        }
    }
}

async fn run_and_record(state: Arc<AppState>, app: tauri::AppHandle, c: Candidate) {
    let outcome = match (&c.host, scripts_dir()) {
        (Some(host), _) => match tunnel_args(&state, host) {
            Some(ssh_args) => remote::execute(&c, host, &ssh_args).await,
            None => Outcome::Unreachable(format!("maiTerm has no connection to {host} — it runs once an ssh tab to it is open")),
        },
        (None, Some(dir)) => execute(&c, &dir).await,
        (None, None) => Outcome::Broken("maiTerm has no data folder to run it from".into()),
    };
    let met = {
        let mut map = runs().lock();
        let r = map.entry(c.id.clone()).or_default();
        r.status.running = false;
        r.status.last_run_at = Some(crate::commands::workspace::iso_now());
        match &outcome {
            Outcome::Met(out) => {
                r.status.last_result = Some("met".into());
                r.status.detail = None;
                r.status.broken_runs = 0;
                Some(("it passed".to_string(), Some(out.clone()).filter(|s| !s.is_empty())))
            }
            Outcome::NotYet => {
                r.status.last_result = Some("not_yet".into());
                r.status.detail = None;
                r.status.broken_runs = 0;
                None
            }
            Outcome::Unreachable(why) => {
                // `broken_runs` untouched: a host that comes and goes neither breaks nor clears it.
                r.status.last_result = Some("unreachable".into());
                r.status.detail = Some(why.clone());
                None
            }
            Outcome::Broken(why) => {
                r.status.last_result = Some("broken".into());
                r.status.detail = Some(why.clone());
                r.status.broken_runs += 1;
                (r.status.broken_runs >= BROKEN_AFTER).then(|| {
                    // The frontend's `whenText` matches the start ("it BROKE", `BROKE_PREFIX`).
                    (format!("it BROKE instead — {why}, on {BROKEN_AFTER} runs in a row — so the condition was never checked"), None)
                })
            }
        }
    };
    if let Some((outcome, report)) = met {
        meet(&state, &app, &c, outcome, report);
    }
}

/// Mark it met wherever it now is — the tab may have moved workspace or window since the pass
/// that started the run — and tell that window's frontend.
fn meet(state: &Arc<AppState>, app: &tauri::AppHandle, c: &Candidate, outcome: String, report: Option<String>) {
    let mut app_data = state.app_data.write();
    // (window label, workspace id, tab id, list) of the tab that holds it NOW. Never `c.tab_id`:
    // a reload during the run moved the follow-up to a tab with a new id, and an event naming the
    // old one is mirrored nowhere — the window would never see it met (review of afaafbc).
    let mut hit: Option<(String, String, String, Vec<FollowUp>)> = None;
    'find: for w in app_data.windows.iter_mut() {
        for ws in w.workspaces.iter_mut() {
            let tab = ws.panes.iter_mut().flat_map(|p| p.tabs.iter_mut())
                .chain(ws.archived_tabs.iter_mut())
                .find(|t| t.follow_ups.iter().any(|f| f.id == c.id));
            if let Some(tab) = tab {
                if crate::commands::workspace::meet_follow_up(
                    &mut tab.follow_ups, &c.id, crate::commands::workspace::iso_now(), outcome.clone(), report.clone(),
                ) {
                    hit = Some((w.label.clone(), ws.id.clone(), tab.id.clone(), tab.follow_ups.clone()));
                }
                break 'find;
            }
        }
    }
    let Some((label, workspace_id, tab_id, list)) = hit else { return };
    let data_clone = app_data.clone();
    drop(app_data);
    if let Err(e) = crate::state::save_state(&data_clone) {
        log::error!("follow-ups: saving the met watch script {} failed: {e}", short(&c.id));
    }
    log::info!("follow-ups: watch script {} on tab {} is due — {outcome}", short(&c.id), short(&tab_id));
    let _ = app.emit_to(
        label.as_str(),
        CHANGED_EVENT,
        serde_json::json!({ "workspace_id": workspace_id, "tab_id": tab_id, "follow_ups": list }),
    );
}

fn short(id: &str) -> &str {
    &id[..8.min(id.len())]
}

/// Printable text: control characters out (newline and tab kept), trimmed, capped. It ends up in
/// a bracketed paste (the envelope), where an ESC could end the paste early.
fn clean(bytes: &[u8], max_chars: usize, tail: bool) -> String {
    let s = String::from_utf8_lossy(bytes);
    let s: String = s.chars().filter(|c| *c == '\n' || *c == '\t' || !c.is_control()).collect();
    let s = s.trim();
    let n = s.chars().count();
    if n <= max_chars {
        return s.to_string();
    }
    if tail {
        format!("…{}", s.chars().skip(n - max_chars).collect::<String>())
    } else {
        format!("{}…", s.chars().take(max_chars).collect::<String>())
    }
}

/// The user's login PATH, read once. maiTerm launched from the Finder has only the system PATH,
/// and a watch script calling `gh` or `jq` needs what the user's own terminal has.
async fn login_path() -> String {
    static PATH: tokio::sync::OnceCell<String> = tokio::sync::OnceCell::const_new();
    PATH.get_or_init(|| async {
        const FALLBACK: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut cmd = tokio::process::Command::new(shell);
        // A marker, because a login profile may print things of its own.
        cmd.args(["-l", "-c", "printf '\\n__MAITERM_PATH__%s' \"$PATH\""])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .kill_on_drop(true);
        match tokio::time::timeout(Duration::from_secs(5), cmd.output()).await {
            Ok(Ok(o)) => String::from_utf8_lossy(&o.stdout)
                .rsplit_once("__MAITERM_PATH__")
                .map(|(_, p)| p.trim().to_string())
                .filter(|p| !p.is_empty())
                .unwrap_or_else(|| FALLBACK.into()),
            _ => FALLBACK.into(),
        }
    })
    .await
    .clone()
}

/// A tab's recorded folder is often home-relative (`last_cwd` = "~/repo").
fn expand_home(cwd: &str) -> PathBuf {
    match (cwd.strip_prefix('~'), dirs::home_dir()) {
        (Some(rest), Some(home)) if rest.is_empty() || rest.starts_with('/') => home.join(rest.trim_start_matches('/')),
        _ => PathBuf::from(cwd),
    }
}

async fn read_capped<R: tokio::io::AsyncRead + Unpin>(r: Option<R>) -> Vec<u8> {
    use tokio::io::AsyncReadExt;
    let mut buf = Vec::new();
    if let Some(r) = r {
        let _ = r.take(OUTPUT_CAP).read_to_end(&mut buf).await;
    }
    buf
}

#[cfg(not(unix))]
async fn execute(_c: &Candidate, _dir: &Path) -> Outcome {
    Outcome::Broken("watch scripts run on macOS and Linux only".into())
}

/// One run, its script and state files in `dir`.
#[cfg(unix)]
async fn execute(c: &Candidate, dir: &Path) -> Outcome {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    if let Err(e) = std::fs::create_dir_all(dir) {
        return Outcome::Broken(format!("could not create {}: {e}", dir.display()));
    }
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    let script_path = dir.join(format!("{}.sh", c.id));
    let state_path = dir.join(format!("{}.state", c.id));
    // Written on every run from the stored text — the approved text is the only one that runs.
    if let Err(e) = std::fs::write(&script_path, &c.script)
        .and_then(|_| std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)))
    {
        return Outcome::Broken(format!("could not write the script: {e}"));
    }
    if !state_path.exists() {
        let _ = std::fs::write(&state_path, b"");
        let _ = std::fs::set_permissions(&state_path, std::fs::Permissions::from_mode(0o600));
    }
    let cwd = expand_home(&c.cwd);
    if !cwd.is_dir() {
        return Outcome::Broken(format!("its folder {} no longer exists", c.cwd));
    }

    // With a shebang it runs as itself; without, under sh.
    let mut cmd = if c.script.starts_with("#!") {
        tokio::process::Command::new(&script_path)
    } else {
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg(&script_path);
        cmd
    };
    cmd.current_dir(&cwd)
        .env_clear()
        .env("PATH", login_path().await)
        .env("LANG", std::env::var("LANG").unwrap_or_else(|_| "en_US.UTF-8".into()))
        .env("MAITERM_WATCH_STATE", &state_path)
        .env("MAITERM_TAB_ID", &c.tab_id);
    if let Some(home) = dirs::home_dir() {
        cmd.env("HOME", home);
    }
    for key in ["USER", "LOGNAME", "TMPDIR", "SHELL"] {
        if let Ok(v) = std::env::var(key) {
            cmd.env(key, v);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Outcome::Broken(format!("it could not start: {e}")),
    };
    let pgid = child.id().map(|p| p as libc::pid_t);
    let out_task = tokio::spawn(read_capped(child.stdout.take()));
    let err_task = tokio::spawn(read_capped(child.stderr.take()));
    let kill_group = || {
        if let Some(pg) = pgid {
            // SAFETY: signalling a process group this run created (process_group(0) made the
            // child its leader); a stale id at worst names an empty group.
            unsafe { libc::killpg(pg, libc::SIGKILL) };
        }
    };

    let status = tokio::time::timeout(c.timeout, child.wait()).await;
    // Whatever happened, nothing the script started outlives it.
    kill_group();
    let status = match status {
        Ok(Ok(s)) => Some(s),
        Ok(Err(e)) => return Outcome::Broken(format!("waiting for it failed: {e}")),
        Err(_) => {
            let _ = child.wait().await;
            None
        }
    };
    // A process that left the group (setsid) can still hold the pipes open: don't wait on it.
    let grab = |t: tokio::task::JoinHandle<Vec<u8>>| async move {
        tokio::time::timeout(Duration::from_secs(1), t).await.ok().and_then(|r| r.ok()).unwrap_or_default()
    };
    let (stdout, stderr) = (grab(out_task).await, grab(err_task).await);

    let Some(status) = status else {
        return Outcome::Broken(format!("it timed out after {}s", c.timeout.as_secs()));
    };
    match status.code() {
        Some(0) => Outcome::Met(clean(&stdout, REPORT_CHARS, false)),
        Some(1) => Outcome::NotYet,
        code => {
            let why = code.map_or_else(|| "it was killed by a signal".to_string(), |n| format!("exit {n}"));
            let err = clean(&stderr, STDERR_CHARS, true);
            Outcome::Broken(if err.is_empty() { why } else { format!("{why}: {err}") })
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn candidate(script: &str, cwd: &Path, timeout: u64) -> Candidate {
        Candidate {
            id: format!("test-{}", uuid::Uuid::new_v4()),
            tab_id: "tab".into(),
            script: script.into(),
            cwd: cwd.display().to_string(),
            host: None,
            every: Duration::from_secs(60),
            timeout: Duration::from_secs(timeout),
        }
    }

    /// Never the real data folder: a scratch one per test.
    fn scratch() -> PathBuf {
        std::env::temp_dir().join(format!("maiterm-watch-test-{}", uuid::Uuid::new_v4()))
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
    }

    async fn run(script: &str, timeout: u64) -> Outcome {
        let dir = scratch();
        let c = candidate(script, &std::env::temp_dir(), timeout);
        let o = execute(&c, &dir).await;
        cleanup(&dir);
        o
    }

    #[tokio::test]
    async fn exit_codes_mean_met_not_yet_and_broken() {
        let o = run("echo '3 new files'\nexit 0\n", 5).await;
        assert!(matches!(o, Outcome::Met(ref s) if s == "3 new files"));
        assert!(matches!(run("exit 1", 5).await, Outcome::NotYet));
        let o = run("echo oops >&2; exit 2", 5).await;
        assert!(matches!(o, Outcome::Broken(ref s) if s == "exit 2: oops"), "stderr rides along");
    }

    #[tokio::test]
    async fn a_shebang_runs_as_itself() {
        let o = run("#!/bin/bash\n[[ -n \"$BASH_VERSION\" ]] && echo bash\n", 5).await;
        assert!(matches!(o, Outcome::Met(ref s) if s == "bash"));
    }

    #[tokio::test]
    async fn state_persists_between_runs_and_the_environment_is_minimal() {
        let dir = scratch();
        let c = candidate(
            "n=$(cat \"$MAITERM_WATCH_STATE\"); n=$((${n:-0}+1)); echo $n > \"$MAITERM_WATCH_STATE\"\n\
             [ -z \"$MAITERM_TEST_LEAK\" ] || exit 3\n[ \"$n\" -ge 2 ] && { echo \"run $n\"; exit 0; }; exit 1\n",
            &std::env::temp_dir(),
            5,
        );
        std::env::set_var("MAITERM_TEST_LEAK", "maiTerm's own environment must not reach the script");
        let first = execute(&c, &dir).await;
        let second = execute(&c, &dir).await;
        cleanup(&dir);
        assert!(matches!(first, Outcome::NotYet));
        assert!(matches!(second, Outcome::Met(ref s) if s == "run 2"), "the second run saw the first's state");
    }

    #[tokio::test]
    async fn a_timeout_is_broken_and_kills_what_it_started() {
        let marker = std::env::temp_dir().join(format!("watch-orphan-{}", uuid::Uuid::new_v4()));
        let script = format!("(sleep 2; touch '{}') &\nsleep 30\n", marker.display());
        let started = Instant::now();
        let o = run(&script, 1).await;
        assert!(matches!(o, Outcome::Broken(ref s) if s.contains("timed out")));
        assert!(started.elapsed() < Duration::from_secs(5), "the run was cut off");
        tokio::time::sleep(Duration::from_secs(3)).await;
        assert!(!marker.exists(), "the background child died with the group");
    }

    #[tokio::test]
    async fn a_background_child_does_not_outlive_a_finished_run() {
        let marker = std::env::temp_dir().join(format!("watch-orphan-{}", uuid::Uuid::new_v4()));
        let script = format!("(sleep 1; touch '{}') &\nexit 1\n", marker.display());
        assert!(matches!(run(&script, 5).await, Outcome::NotYet));
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(!marker.exists());
    }

    #[tokio::test]
    async fn a_missing_folder_is_broken() {
        let dir = scratch();
        let c = candidate("exit 0", Path::new("/nonexistent/maiterm-watch"), 5);
        let o = execute(&c, &dir).await;
        cleanup(&dir);
        assert!(matches!(o, Outcome::Broken(ref s) if s.contains("no longer exists")));
    }

    #[test]
    fn a_home_relative_folder_is_expanded() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home("~/repo"), home.join("repo"));
        assert_eq!(expand_home("/abs"), PathBuf::from("/abs"));
        assert_eq!(expand_home("~bob/x"), PathBuf::from("~bob/x"), "another user's home is not ours");
    }

    #[test]
    fn output_is_cleaned_and_capped() {
        assert_eq!(clean(b"\x1b[201~hi\r\n", 100, false), "[201~hi");
        assert_eq!(clean(b"abcdef", 3, false), "abc…");
        assert_eq!(clean(b"abcdef", 3, true), "…def");
    }

    #[test]
    fn approval_is_per_script_and_folder_and_capped() {
        let mut d = AppData::default();
        assert!(!is_approved(&d, "/a", "x"));
        remember_approval(&mut d, "/a", "x");
        assert!(is_approved(&d, "/a", "x"));
        assert!(!is_approved(&d, "/b", "x"), "another folder is another approval");
        assert!(!is_approved(&d, "/a", "x "), "one byte changed is another script");
        // The same folder on another machine is another place.
        assert_eq!(place(None, "/a"), "/a", "a local place is the bare folder, so old approvals hold");
        assert!(!is_approved(&d, &place(Some("ews@nova"), "/a"), "x"), "allowed here is not allowed on a server");
        remember_approval(&mut d, &place(Some("ews@nova"), "/a"), "y");
        assert!(!is_approved(&d, "/a", "y"), "nor the other way");
        assert!(!is_approved(&d, &place(Some("ews@nova2"), "/a"), "y"));
        for i in 0..MAX_APPROVALS + 5 {
            remember_approval(&mut d, "/a", &i.to_string());
        }
        assert_eq!(d.approved_watch_scripts.len(), MAX_APPROVALS);
        assert!(!is_approved(&d, "/a", "x"), "the oldest went first");
        d.preferences.follow_ups_scripts_unattended = true;
        assert!(!is_approved(&d, "/z", "anything"), "the waiver is not an approval");
    }

    fn data_with_script(approved: bool, unattended: bool) -> AppData {
        let mut tab = crate::state::workspace::Tab::new("t".into());
        tab.follow_ups.push(FollowUp {
            id: "f".into(),
            text: "x".into(),
            due: crate::state::workspace::FollowUpDue {
                kind: "script".into(),
                script: Some("exit 1".into()),
                cwd: Some("/tmp".into()),
                approved,
                ..Default::default()
            },
            author: "agent".into(),
            created_at: "2026-10-01T00:00:00Z".into(),
            expires_at: None,
        });
        let mut d = AppData::default();
        let mut win = crate::state::workspace::WindowData::new("main".into());
        let mut ws = crate::state::workspace::Workspace::new("w".into());
        ws.panes[0].tabs.push(tab);
        win.workspaces.push(ws);
        d.windows.push(win);
        d.preferences.follow_ups_scripts_unattended = unattended;
        d
    }

    #[test]
    fn only_an_approval_or_the_live_waiver_runs_a_script() {
        let runs = |approved, unattended| !collect(&data_with_script(approved, unattended)).0.is_empty();
        assert!(!runs(false, false), "nobody said yes");
        assert!(runs(true, false), "the human approved it");
        assert!(runs(false, true), "the waiver is on");
        // ...and turning the waiver off withdraws it: nothing was stored on the script.
        assert!(!runs(false, false));
    }
}
