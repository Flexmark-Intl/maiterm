//! A worker's way into the run queue (docs/hive.md §6.8, §7.2): `hiveRun`, the coordinator's
//! `hiveRunPriority`, a worker's start, restart and stop of a human-defined service, and what the
//! worker hears back: one `⟦HIVE RUN⟧` per run, through the delivery gate (`deliver`).
//!
//! `hiveRun` is not a free command string. It refuses every shell metacharacter, splits on
//! whitespace into argv (`job_sandbox` runs that under `sandbox-exec`, never `sh -c`), and must
//! start, word by word, with a queued command the human listed for the folder; `needs`, leases,
//! env and extra writes all come from that entry, never from the worker.
//!
//! Every open run and every undelivered result is in the worker's `worker.json` (`pending_runs`),
//! keyed by job: a result is taken before it is typed and put back only if nothing took it, so it
//! reaches the worker exactly once, across a reload, a crash (retried at its next session start)
//! and a maiTerm restart (an open run becomes "interrupted by a restart").

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use tauri::AppHandle;

use super::deliver::{frame_escape, one_line, Sent};
use super::paths::HivePaths;
use super::queue::{self, Job, JobSpec};
use super::record::{Hive, QueuedCommand, RunQueueSettings};
use super::rpc::HiveWindow;
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::{self, PendingRun, WorkerFolder, WorkerRecord, RUN_OPEN, RUN_RESULT};
use crate::state::{AppData, AppState, Service};

/// What `hiveRun` refuses anywhere in a command, line breaks besides: it is never a shell line.
pub const REFUSED: &[char] = &[';', '|', '&', '$', '`', '<', '>', '(', ')', '\\', '\'', '"'];
pub const MAX_COMMAND: usize = 1000;
/// A run that ends within this answers in `hiveRun`'s own result.
pub const INLINE_WAIT: Duration = Duration::from_secs(5);
/// The result's tail: the last lines of the job's screen.
pub const TAIL_LINES: usize = 40;
/// A worker's stack verbs, routed here before the frontend sees them (`claude_code::server`).
pub const SERVICE_VERBS: [&str; 3] = ["startService", "restartService", "stopService"];
const MAX_CHECK_LIST: usize = 50;

// ── The tools' definitions ──

pub fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveRun",
        "description": format!(
            "Run one of this Hive's queued commands (the heavy ones your human listed: a test suite, a build, a dev \
             instance) on your branch. maiTerm queues it behind the machine's other runs, runs it in your worktree under \
             your sandbox, and records its exit code itself: a passing run on a committed tree is a checks receipt for \
             hiveReport. command must start with a queued command, word by word (further arguments are fine), and holds no \
             shell syntax: no ; | & $ ` < > ( ) \\ ' \" or line breaks. Answers at once with the job and its place in the \
             queue (or the result, if it ends within {} s); the result arrives later as a ⟦HIVE RUN⟧ message. Run anything \
             that isn't queued straight in your own shell.",
            INLINE_WAIT.as_secs()
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "command": { "type": "string", "description": "The command, starting with a queued command (`cargo test -p core`)" },
                "folder": { "type": "string", "description": "The Hive folder to run it in (default: the primary folder)" },
                "timeout": { "type": "integer", "description": "Seconds before maiTerm stops it (default 1800, at most 14400)" }
            },
            "required": ["command"]
        }
    })
}

pub fn priority_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveRunPriority",
        "description": "Move a queued run of one of your workers forward: to the top of the queue, or one place up (the run a land waits \
            on, say). Only its place in line changes, never when or with what it runs: maiTerm starts it once what it needs is \
            free. The job id is in the worker's ⟦HIVE RUN⟧ messages and its reports.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "job": { "type": "string", "description": "The job id (j-…)" },
                "move": { "type": "string", "enum": ["top", "up"], "description": "To the top, or one place up (default top)" }
            },
            "required": ["job"]
        }
    })
}

// ── Checking a command ──

fn describe_char(c: char) -> String {
    match c {
        '\n' => "a line break".to_string(),
        '\r' => "a carriage return".to_string(),
        c if c.is_control() => format!("the control character U+{:04X}", c as u32),
        c => format!("`{c}`"),
    }
}

/// A command as argv: whitespace-split, and refused whole if any shell syntax is in it.
pub fn parse_command(cmd: &str) -> Result<Vec<String>, String> {
    if cmd.chars().count() > MAX_COMMAND {
        return Err(format!("The command is over {MAX_COMMAND} characters."));
    }
    if let Some(c) = cmd.chars().find(|c| REFUSED.contains(c) || (c.is_control() && *c != '\t')) {
        return Err(format!(
            "hiveRun refuses {} in a command: it runs as plain words, never through a shell. Leave out ; | & $ ` < > ( ) \\ ' \" \
             and line breaks.",
            describe_char(c)
        ));
    }
    let argv: Vec<String> = cmd.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() {
        return Err("command is required: one of this Hive's queued commands.".to_string());
    }
    Ok(argv)
}

/// The queued entry `argv` starts with, word by word, in `folder`: the longest one, so
/// `cargo test --release` can be listed apart from `cargo test`.
pub fn match_entry<'a>(q: &'a RunQueueSettings, folder: &str, argv: &[String]) -> Option<&'a QueuedCommand> {
    q.commands
        .iter()
        .filter(|c| c.folder == folder)
        .filter(|c| {
            let w = c.words();
            !w.is_empty() && w.len() <= argv.len() && w.iter().zip(argv).all(|(a, b)| *a == b.as_str())
        })
        .max_by_key(|c| c.words().len())
}

/// The queued commands of `folder`, for a refusal.
fn listed(q: &RunQueueSettings, folder: &str) -> String {
    let names: Vec<String> = q.commands.iter().filter(|c| c.folder == folder).map(|c| format!("`{}`", c.words().join(" "))).collect();
    if names.is_empty() {
        format!("nothing is queued in {folder}")
    } else {
        format!("queued in {folder}: {}", names.join(", "))
    }
}

/// Every queued command's words, once each: what a worker's gate denies run directly (§6.8).
pub fn queued_prefixes(hive: &Hive) -> Vec<Vec<String>> {
    let mut out: Vec<Vec<String>> = Vec::new();
    for c in &hive.settings.run_queue.commands {
        let w: Vec<String> = c.words().into_iter().map(str::to_string).collect();
        if !w.is_empty() && !out.contains(&w) {
            out.push(w);
        }
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct RunArgs {
    pub argv: Vec<String>,
    pub folder: Option<String>,
    /// 0 is the queue's default.
    pub timeout_s: u64,
}

impl RunArgs {
    pub(crate) fn parse(args: &Value) -> Result<Self, String> {
        let cmd = args.get("command").and_then(Value::as_str).unwrap_or_default();
        let argv = parse_command(cmd)?;
        let folder = args.get("folder").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
        let timeout_s = match args.get("timeout") {
            None | Some(Value::Null) => 0,
            Some(v) => v.as_u64().filter(|t| *t > 0).ok_or("timeout is a whole number of seconds")?,
        };
        Ok(Self { argv, folder, timeout_s })
    }
}

/// The worktree's commit, and whether tracked files differ from it. Blocks on git, on the git
/// dir maiTerm recorded.
pub(crate) fn worktree_state(f: &WorkerFolder) -> Result<(String, bool), String> {
    use super::git::{git_in_worktree, run, QUERY_TIMEOUT};
    let git = |args: &[&str]| git_in_worktree(f).and_then(|mut cmd| run(cmd.args(args), QUERY_TIMEOUT));
    let commit = git(&["rev-parse", "--verify", "HEAD"]).map_err(|e| format!("maiTerm couldn't read your {} worktree's commit: {e}", f.name))?;
    let status = git(&["status", "--porcelain", "--untracked-files=no"]).map_err(|e| format!("maiTerm couldn't read your {} worktree's status: {e}", f.name))?;
    Ok((commit, !status.trim().is_empty()))
}

/// The copies an entry leases, on the job's own checkout: its `services` by name. A shared
/// service is another workspace's and never leased (§6.8).
fn leases_for(entry: &QueuedCommand, stack: &[Service], target: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    for name in entry.services.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        let want = Service::normalize_name(name);
        let s = stack.iter().filter(|s| !s.is_job()).find(|s| Service::normalize_name(&s.name) == want).ok_or_else(|| {
            format!(
                "The queued command `{}` uses a service called {}, which this Hive's stack doesn't have. Tell your coordinator: \
                 the human's queued command needs fixing.",
                entry.words().join(" "),
                one_line(name, 60)
            )
        })?;
        if s.shared_from.is_none() {
            out.push(crate::stack::copy_key(&s.id, target));
        }
    }
    Ok(out)
}

/// The worker's folder a run goes to, with its worktree.
fn run_folder<'a>(hive: &Hive, w: &'a WorkerRecord, name: Option<&str>) -> Result<&'a WorkerFolder, String> {
    let name = match name {
        Some(n) => n.to_string(),
        None => hive.primary().map(|f| f.name.clone()).ok_or("This Hive has no primary folder: name one with folder.")?,
    };
    let f = w.folders.iter().find(|f| f.name == name).ok_or_else(|| {
        let mine: Vec<&str> = w.git_folders().map(|f| f.name.as_str()).collect();
        format!("You have no worktree of \"{}\" (yours: {}).", one_line(&name, 60), if mine.is_empty() { "none".to_string() } else { mine.join(", ") })
    })?;
    if f.worktree.is_none() {
        return Err(format!("{} is a plain folder: nothing runs in it.", f.name));
    }
    Ok(f)
}

/// `hiveRun`'s job, matched against the Hive's queued commands. Blocks on git.
pub(crate) fn build_spec(caller: &Caller, w: &WorkerRecord, a: &RunArgs, stack: &[Service]) -> Result<JobSpec, String> {
    let hive = &caller.hive;
    let f = run_folder(hive, w, a.folder.as_deref())?;
    let q = &hive.settings.run_queue;
    let entry = match_entry(q, &f.name, &a.argv).ok_or_else(|| {
        format!(
            "`{}` isn't a queued command ({}). hiveRun runs only those; run anything else straight in your own shell.",
            one_line(&a.argv.join(" "), 120),
            listed(q, &f.name)
        )
    })?;
    let (commit, dirty) = worktree_state(f)?;
    let wt = f.worktree.clone().unwrap_or_default();
    let target = format!("{}{wt}", crate::stack::target::CHECKOUT);
    let leases = leases_for(entry, stack, &target)?;
    Ok(JobSpec {
        hive_id: hive.id.clone(),
        workspace_id: caller.workspace_id.clone(),
        window: caller.label.clone(),
        task_id: w.task_id.clone(),
        worker_slug: w.slug.clone(),
        folder: f.name.clone(),
        target,
        argv: a.argv.clone(),
        entry: entry.words().join(" "),
        needs: entry.needs.clone(),
        env: entry.env.clone(),
        extra_write: entry.extra_write.clone(),
        leases,
        shares: q.resources.clone(),
        timeout_s: a.timeout_s,
        branch: f.branch.clone(),
        commit: Some(commit),
        dirty,
        ..JobSpec::default()
    })
}

// ── pending_runs: Rust's record of what the worker is owed ──

fn now_iso() -> String {
    crate::commands::workspace::iso_now()
}

/// The run is open: told "interrupted" after a restart if it never ends here.
pub(crate) fn record_open(dir: &std::path::Path, job: &str, command: &str) -> Result<(), String> {
    worker::update_worker(dir, |w| {
        if w.pending_runs.iter().any(|r| r.job == job) {
            return false;
        }
        w.pending_runs.push(PendingRun { job: job.to_string(), state: RUN_OPEN.to_string(), text: command.to_string(), at: now_iso(), ..PendingRun::default() });
        true
    })
    .map(|_| ())
}

/// The run's result, waiting to be typed.
pub(crate) fn record_result(dir: &std::path::Path, job: &str, text: &str) -> Result<(), String> {
    worker::update_worker(dir, |w| {
        w.pending_runs.retain(|r| r.job != job);
        w.pending_runs.push(PendingRun { job: job.to_string(), state: RUN_RESULT.to_string(), text: text.to_string(), at: now_iso(), ..PendingRun::default() });
        true
    })
    .map(|_| ())
}

/// An open run that will get no result of its own (released, or stopped by its worker).
pub(crate) fn forget_open(dir: &std::path::Path, jobs: &[String]) -> Result<(), String> {
    worker::update_worker(dir, |w| {
        let before = w.pending_runs.len();
        w.pending_runs.retain(|r| !(r.state == RUN_OPEN && jobs.contains(&r.job)));
        before != w.pending_runs.len()
    })
    .map(|_| ())
}

/// Results taken out, before they are typed: one job's, or all of them.
pub(crate) fn take_results(dir: &std::path::Path, job: Option<&str>) -> Result<Vec<PendingRun>, String> {
    let mut taken = Vec::new();
    worker::update_worker(dir, |w| {
        let (out, keep): (Vec<PendingRun>, Vec<PendingRun>) =
            std::mem::take(&mut w.pending_runs).into_iter().partition(|r| r.state == RUN_RESULT && job.is_none_or(|j| r.job == j));
        w.pending_runs = keep;
        taken = out;
        !taken.is_empty()
    })?;
    Ok(taken)
}

/// Results nothing took (no agent there): back in front, for the next try.
pub(crate) fn put_back(dir: &std::path::Path, runs: Vec<PendingRun>) -> Result<(), String> {
    worker::update_worker(dir, |w| {
        let fresh: Vec<PendingRun> = runs.into_iter().filter(|r| !w.pending_runs.iter().any(|p| p.job == r.job)).collect();
        if fresh.is_empty() {
            return false;
        }
        let rest = std::mem::take(&mut w.pending_runs);
        w.pending_runs = fresh.into_iter().chain(rest).collect();
        true
    })
    .map(|_| ())
}

/// Every result waiting for this worker, each taken before `send` types it and put back only
/// when nothing took it. Two of these at once deliver each result once. Answers how many went.
pub(crate) async fn deliver_results_with<F, Fut>(dir: PathBuf, send: F) -> usize
where
    F: Fn(String) -> Fut,
    Fut: std::future::Future<Output = Sent>,
{
    let d = dir.clone();
    let taken = match blocking(move || take_results(&d, None)).await {
        Ok(t) => t,
        Err(e) => {
            log::warn!("hive: run results not read from {}: {e}", dir.display());
            return 0;
        }
    };
    let (mut sent, mut back) = (0, Vec::new());
    for r in taken {
        match send(r.text.clone()).await {
            Sent::Refused(why) => {
                log::info!("hive: run result {} kept for later: {why}", r.job);
                back.push(r);
            }
            // Held: the draft-hold queue owns it now and types it once its reason clears.
            Sent::Delivered | Sent::Held(_) => sent += 1,
        }
    }
    if !back.is_empty() {
        if let Err(e) = blocking(move || put_back(&dir, back)).await {
            log::warn!("hive: run results not put back, so they are lost: {e}");
        }
    }
    sent
}

/// Types every result waiting for the worker of `task_id`, if its tab is live. From a job's end,
/// a release, and the worker's session start.
pub(crate) async fn deliver_pending(app: &Arc<AppState>, handle: &AppHandle, paths: &HivePaths, task_id: &str) -> usize {
    let Ok(dir) = worker::task_dir(paths, task_id) else { return 0 };
    let d = dir.clone();
    let Ok(w) = blocking(move || worker::read_worker(&d)).await else { return 0 };
    if !w.pending_runs.iter().any(|r| r.state == RUN_RESULT) {
        return 0;
    }
    let Some(tab) = w.tab_id.clone().filter(|t| super::is_hive_role_tab(&app.app_data.read(), t)) else { return 0 };
    let n = deliver_results_with(dir, |text| {
        let (app, handle, tab) = (app.clone(), handle.clone(), tab.clone());
        async move { super::deliver::deliver(&app, &handle, &tab, &text, "Hive run").await }
    })
    .await;
    if n > 0 {
        log::info!("hive: {n} run result(s) to worker {}", worker_label(&w));
    }
    n
}

/// A worker's session started: whatever it is owed (a result its crash missed, a restart's
/// notice) goes now.
pub async fn on_session_started(app: &Arc<AppState>, handle: &AppHandle, tab_id: &str) {
    let candidates = super::tools::caller_for_tab(&app.app_data.read(), tab_id).into_iter().collect::<Vec<_>>();
    let t = tab_id.to_string();
    let found = blocking(move || Ok(candidates.into_iter().find_map(|c| worker::worker_for_tab(&c.paths, &t).map(|w| (c, w))))).await;
    if let Ok(Some((caller, w))) = found {
        deliver_pending(app, handle, &caller.paths, &w.task_id).await;
    }
}

// ── What the worker is told ──

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(7)]
}

fn duration(job: &Job) -> String {
    let (Some(a), Some(b)) = (job.started_at, job.ended_at) else { return String::new() };
    let s = ((b - a).max(0) / 1000) as u64;
    match s {
        0..=59 => format!(" in {s}s"),
        60..=3599 => format!(" in {}m {}s", s / 60, s % 60),
        _ => format!(" in {}h {}m", s / 3600, (s % 3600) / 60),
    }
}

/// Whether a finished run can stand as a receipt: a `hiveRun` job (never a service copy, whose
/// exit 0 tested nothing), done, begun, exit 0, on a committed tree.
pub fn is_pass(job: &Job) -> bool {
    job.service_id.is_none() && job.state == queue::DONE && job.began && job.exit_code == Some(0) && !job.dirty
}

/// A passing run's worktree, read again at its end: if HEAD moved or tracked files changed while
/// it ran (or maiTerm can't tell), it tested no one commit, so it is marked dirty, no receipt.
/// Blocks on git.
pub(crate) fn recheck_tree(paths: &HivePaths, job: &mut Job) {
    if !is_pass(job) {
        return;
    }
    let now = worker::read_worker(&paths.task(&job.task_id)).and_then(|w| {
        let f = w.folders.iter().find(|f| f.name == job.folder && f.worktree.is_some()).ok_or_else(|| format!("no worktree of {}", job.folder))?;
        worktree_state(f)
    });
    let why = match now {
        Ok((head, _)) if job.commit.as_deref() != Some(head.as_str()) => Some(format!("your branch moved to {} while it ran", short(&head))),
        Ok((_, true)) => Some("tracked files in your worktree changed while it ran".to_string()),
        Ok(_) => None,
        Err(e) => Some(format!("maiTerm couldn't read your worktree when it ended ({})", one_line(&e, 120))),
    };
    if let Some(why) = why {
        log::info!("hive: {} is no receipt: {why}", job.id);
        job.dirty = true;
        job.error = Some(why);
    }
}

/// The `⟦HIVE RUN⟧` message: maiTerm's own account of the run, then its output quoted as data.
pub fn result_message(job: &Job, tail: &str) -> String {
    let at = match (&job.branch, &job.commit) {
        (Some(b), Some(c)) => format!("{}@{}", one_line(b, 120), short(c)),
        (None, Some(c)) => short(c).to_string(),
        _ => format!("your {} worktree", job.folder),
    };
    let reason = job.error.as_deref().map(|e| one_line(e, 200)).unwrap_or_default();
    let head = match job.state.as_str() {
        queue::DONE if job.exit_code == Some(0) => "passed (exit 0)".to_string(),
        queue::DONE => format!("failed (exit {})", job.exit_code.map_or("unknown".to_string(), |c| c.to_string())),
        queue::INTERRUPTED => {
            let by = match job.stopped_by.as_deref() {
                Some(queue::BY_HUMAN) => "your human stopped it",
                Some(queue::BY_TIMEOUT) => "it ran past its timeout",
                Some(queue::BY_RESTART) => "maiTerm restarted",
                Some(queue::BY_RELEASE) => "you were released",
                Some(queue::BY_WORKER) => "you stopped it",
                _ => "its tab went away",
            };
            format!("interrupted: {by}{}", if reason.is_empty() { String::new() } else { format!(" ({reason})") })
        }
        _ => format!("didn't run: {}", if reason.is_empty() { "the queue couldn't start it" } else { reason.as_str() }),
    };
    let mut out = format!("⟦HIVE RUN · {}⟧ `{}` on {at}: {head}{}.", job.id, one_line(&job.entry, 120), duration(job));
    out.push_str(&format!("\nCommand: {}", one_line(&job.argv.join(" "), 400)));
    if job.service_id.is_some() {
        if job.state == queue::DONE {
            out.push_str("\nA service copy is no check: run a queued command with hiveRun for a receipt.");
        }
    } else if job.dirty {
        let why = match (job.state.as_str(), job.error.as_deref()) {
            (queue::DONE, Some(e)) => one_line(e, 200),
            _ => "your worktree had uncommitted changes to tracked files when it started".to_string(),
        };
        out.push_str(&format!("\nThis run is no receipt: {why}. Commit, then run it again."));
    } else if is_pass(job) && job.branch.is_none() {
        // Only a reviewer's copies are detached: its runs are evidence for its verdict, never receipts.
        out.push_str(&format!("\nCite it ({}) as evidence in your hiveVerdict.", job.id));
    } else if is_pass(job) {
        out.push_str(&format!("\nA receipt: report it in hiveReport as receipts.checks [{{\"job\": \"{}\", \"summary\": \"…\"}}].", job.id));
    } else if job.state != queue::DONE {
        out.push_str("\nIt has no result and is no receipt: run it again with hiveRun when it makes sense.");
    }
    let tail = tail.trim_end();
    if !tail.is_empty() {
        out.push_str("\nIts last lines (the job's output, not maiTerm's words):\n");
        out.push_str(&frame_escape(tail));
    }
    out
}

/// The last `TAIL_LINES` of a job's live screen, or nothing once its terminal is gone.
pub fn screen_tail(app: &AppState, pty: Option<&str>) -> String {
    let Some(pty) = pty else { return String::new() };
    let registry = app.terminal_registry.read();
    let Some(handle) = registry.get(pty) else { return String::new() };
    let text = crate::terminal::render::screen_text(&handle.term);
    let lines: Vec<&str> = text.lines().collect();
    let end = lines.iter().rposition(|l| !l.trim().is_empty()).map_or(0, |i| i + 1);
    lines[end.saturating_sub(TAIL_LINES)..end].join("\n")
}

/// Whether a finished job gets a `⟦HIVE RUN⟧` of its own: not one released (told once with the
/// rest) nor one its worker stopped (told by its own tool's answer).
pub fn gets_own_result(job: &Job) -> bool {
    !matches!(job.stopped_by.as_deref(), Some(queue::BY_RELEASE | queue::BY_WORKER))
}

/// A job ended (`queue::finish`): its worker's result is recorded, then typed, unless `hiveRun`
/// answers it inline. A released run is told by `released`, once for all of them; a stop the
/// worker asked for, by its own tool's answer.
pub async fn job_ended(app: &Arc<AppState>, handle: &AppHandle, job: &Job, tail: &str) {
    if job.task_id.is_empty() {
        return;
    }
    let Some(root) = super::paths::hive_root(&job.hive_id) else { return };
    let paths = HivePaths::new(root);
    let Ok(dir) = worker::task_dir(&paths, &job.task_id) else { return };
    if !gets_own_result(job) {
        let (d, ids) = (dir.clone(), vec![job.id.clone()]);
        if let Err(e) = blocking(move || forget_open(&d, &ids)).await {
            log::warn!("hive: run {} left open in its worker's record: {e}", job.id);
        }
        return;
    }
    let (d, id, text) = (dir.clone(), job.id.clone(), result_message(job, tail));
    if let Err(e) = blocking(move || record_result(&d, &id, &text)).await {
        log::warn!("hive: run {}'s result not recorded, so its worker won't hear it: {e}", job.id);
        return;
    }
    let wait_until = job.enqueued_at + INLINE_WAIT.as_millis() as i64 + 750;
    let now = super::fuses::now_ms();
    if now < wait_until {
        tokio::time::sleep(Duration::from_millis((wait_until - now) as u64)).await;
    }
    deliver_pending(app, handle, &paths, &job.task_id).await;
}

/// A worker's runs were released (it stopped, crashed, retired, its task was dropped, or it was
/// suspended): it is told once, now or at its next session start, and none of them is a
/// receipt. A suspend's notice is only recorded (`tell_now` false): its agent is about to go.
pub async fn released(app: &Arc<AppState>, handle: &AppHandle, hive_id: &str, task_id: &str, jobs: &[Job], why: &str, tell_now: bool) {
    if jobs.is_empty() {
        return;
    }
    let Some(root) = super::paths::hive_root(hive_id) else { return };
    let paths = HivePaths::new(root);
    let Ok(dir) = worker::task_dir(&paths, task_id) else { return };
    let (jobs, why) = (jobs.to_vec(), why.to_string());
    if let Err(e) = blocking(move || record_release(&dir, &jobs, &why)).await {
        log::warn!("hive: the release of {task_id}'s runs not recorded for it: {e}");
        return;
    }
    if tell_now {
        deliver_pending(app, handle, &paths, task_id).await;
    }
}

/// One notice for every run released at once, in place of their open entries.
pub(crate) fn record_release(dir: &std::path::Path, jobs: &[Job], why: &str) -> Result<(), String> {
    let Some(first) = jobs.first() else { return Ok(()) };
    let one = jobs.len() == 1;
    let list: Vec<String> = jobs.iter().map(|j| format!("{} (`{}`)", j.id, one_line(&j.argv.join(" "), 80))).collect();
    let text = format!(
        "⟦HIVE RUN · released⟧ Your run{} {} {} stopped: {}. {}; run {} again with hiveRun once you're back at work.",
        if one { "" } else { "s" },
        list.join(", "),
        if one { "was" } else { "were" },
        one_line(why, 200),
        if one { "It is no receipt" } else { "None of them is a receipt" },
        if one { "it" } else { "what you need" },
    );
    let ids: Vec<String> = jobs.iter().map(|j| j.id.clone()).collect();
    forget_open(dir, &ids)?;
    record_result(dir, &first.id, &text)
}

/// A worker's tab was suspended: its runs are released, found by its record off the caller's
/// thread.
pub fn release_for_tab(state: Arc<AppState>, tab_id: String, why: &'static str) {
    let candidates = super::tools::caller_for_tab(&state.app_data.read(), &tab_id).into_iter().collect::<Vec<_>>();
    tauri::async_runtime::spawn_blocking(move || {
        let found = candidates.into_iter().find_map(|c| worker::worker_for_tab(&c.paths, &tab_id));
        if let Some(w) = found {
            let n = queue::release_worker_later(&w.task_id, why);
            if n > 0 {
                log::info!("hive: {} run(s) of {} released: {why}", n, worker_label(&w));
            }
        }
    });
}

/// At startup, before any tab is up: every run a worker had open when maiTerm stopped is
/// interrupted, on the run log and in a notice it gets at its next session start (§6.8). The
/// queue starts empty, so every open entry is the last run's. Blocks on the disk.
pub fn restart_notices(data: &parking_lot::RwLock<AppData>) -> usize {
    let callers = super::tools::hive_callers(&data.read());
    let n: usize = callers.iter().map(|c| restart_notices_in(&c.paths, &c.hive.id, &c.workspace_id)).sum();
    if n > 0 {
        log::info!("hive: {n} run(s) interrupted by the restart; their workers are told at their next session start");
    }
    n
}

/// One Hive's half of `restart_notices`. Blocks on the disk.
pub(crate) fn restart_notices_in(paths: &HivePaths, hive_id: &str, workspace_id: &str) -> usize {
    let mut n = 0;
    for w in worker::list_workers(paths) {
        if !w.pending_runs.iter().any(|r| r.state == RUN_OPEN) {
            continue;
        }
        let Ok(dir) = worker::task_dir(paths, &w.task_id) else { continue };
        let mut open: Vec<PendingRun> = Vec::new();
        let res = worker::update_worker(&dir, |rec| {
            for r in rec.pending_runs.iter_mut().filter(|r| r.state == RUN_OPEN) {
                open.push(r.clone());
                r.state = RUN_RESULT.to_string();
                r.text = format!(
                    "⟦HIVE RUN · {}⟧ Your run `{}` was interrupted by a maiTerm restart, so it has no result and is no receipt: run it \
                     again with hiveRun.",
                    r.job,
                    one_line(&r.text, 200)
                );
            }
            !open.is_empty()
        });
        if let Err(e) = res {
            log::warn!("hive {hive_id}: worker {}'s open runs not marked interrupted: {e}", worker_label(&w));
            continue;
        }
        for r in &open {
            let mut job = Job::default();
            job.id = r.job.clone();
            job.hive_id = hive_id.to_string();
            job.workspace_id = workspace_id.to_string();
            job.task_id = w.task_id.clone();
            job.worker_slug = w.slug.clone();
            job.argv = r.text.split_whitespace().map(str::to_string).collect();
            job.state = queue::INTERRUPTED.to_string();
            job.stopped_by = Some(queue::BY_RESTART.to_string());
            job.error = Some("maiTerm restarted".to_string());
            // Unless it ended and was logged before the restart, the run log says so too.
            if queue::find_run(paths, &job.id).is_none() {
                if let Err(e) = queue::append_run(paths, &job) {
                    log::warn!("hive {hive_id}: {} not in the run log: {e}", job.id);
                }
            }
        }
        n += open.len();
    }
    n
}

// ── The tools ──

fn workspace_stack(data: &parking_lot::RwLock<AppData>, caller: &Caller) -> Vec<Service> {
    let d = data.read();
    d.window(&caller.label).and_then(|w| w.workspaces.iter().find(|ws| ws.id == caller.workspace_id)).map(|ws| ws.stack.clone()).unwrap_or_default()
}

async fn calling_worker(caller: &Caller) -> Result<WorkerRecord, String> {
    let (paths, tab) = (caller.paths.clone(), caller.tab_id.clone());
    let w = blocking(move || worker::worker_for_tab(&paths, &tab).ok_or_else(|| "No worker record names your tab.".to_string())).await?;
    if w.phase == worker::PHASE_STOPPED {
        return Err("Your coordinator has stopped you: nothing more runs for you. Finish here.".to_string());
    }
    Ok(w)
}

/// What a queued job's answer says about it now.
fn job_view(job: &Job) -> Value {
    let now = queue::job(&job.id).unwrap_or_else(|| job.clone());
    let place = queue::place(&job.id);
    json!({
        "job": now.id,
        "state": now.state,
        "place": place,
        "entry": now.entry,
        "needs": now.needs,
        "commit": now.commit.as_deref().map(short),
        "dirty": now.dirty,
    })
}

pub(crate) async fn run_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let a = RunArgs::parse(args)?;
    let w = calling_worker(caller).await?;
    let stack = workspace_stack(ctx.data, caller);
    let (c, rec) = (caller.clone(), w.clone());
    let spec = blocking(move || build_spec(&c, &rec, &a, &stack)).await?;
    let job = queue::enqueue(spec)?;
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let (d, id, cmd) = (dir.clone(), job.id.clone(), job.argv.join(" "));
    if let Err(e) = blocking(move || record_open(&d, &id, &cmd)).await {
        log::warn!("hive {}: {} not recorded as open for {}: {e}", caller.hive.id, job.id, worker_label(&w));
    }
    log::info!("hive {}: {} queued {} ({})", caller.hive.id, worker_label(&w), job.id, job.argv.join(" "));

    // A short run answers here; its result is taken so it isn't typed as well.
    let deadline = tokio::time::Instant::now() + INLINE_WAIT;
    while tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(250)).await;
        if queue::job(&job.id).is_some_and(|j| j.is_active()) {
            continue;
        }
        let (d, id) = (dir.clone(), job.id.clone());
        if let Ok(mut taken) = blocking(move || take_results(&d, Some(&id))).await {
            if let Some(r) = taken.pop() {
                let mut v = job_view(&job);
                v["result"] = json!(r.text);
                return Ok(v);
            }
        }
    }
    let mut v = job_view(&job);
    v["note"] = json!(if job.dirty {
        "Queued. Your worktree has uncommitted changes to tracked files: a run is a receipt only if your tree is committed when it starts and unchanged until it ends. The result arrives as a ⟦HIVE RUN⟧ message."
    } else {
        "Queued. It runs on your worktree as it is when it starts: a commit or an edit to tracked files before it ends makes it no receipt. The result arrives as a ⟦HIVE RUN⟧ message between your turns."
    });
    Ok(v)
}

/// `hiveRunPriority`: a job of the coordinator's own Hive, to the top or one place up.
pub(crate) async fn priority_tool(caller: &Caller, args: &Value) -> Result<Value, String> {
    let id = args.get("job").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    if !queue::is_job_id(id) {
        return Err("job is required: a job id (j- and 8 hex digits).".to_string());
    }
    let mv = match args.get("move").and_then(Value::as_str).unwrap_or("top") {
        "top" => queue::Move::Top,
        "up" => queue::Move::Up,
        other => return Err(format!("move must be top or up, not {}.", one_line(other, 20))),
    };
    let place = queue::move_job(id, mv, &caller.hive.id)?;
    log::info!("hive {}: the coordinator moved {id} to place {place}", caller.hive.id);
    Ok(json!({ "job": id, "place": place }))
}

/// The worker tab among a request's identities, when `tool` is one of the stack verbs a worker
/// runs only through the queue: such a call never reaches the frontend's stack store.
pub fn queued_verb_caller(data: &AppData, callers: &[&str], tool: &str) -> Option<String> {
    if !SERVICE_VERBS.contains(&tool) {
        return None;
    }
    callers
        .iter()
        .find(|id| super::find_hive_tab(data, id).is_some_and(|(_, t)| t.hive_role.as_deref() == Some(super::ROLE_WORKER)))
        .map(|s| s.to_string())
}

/// Whether `on` names the worker's own branch: empty, its name or badge, or one of its branches.
pub(crate) fn own_branch(w: &WorkerRecord, on: &str) -> bool {
    let on = on.trim();
    on.is_empty()
        || on == w.slug
        || on.strip_prefix('#').unwrap_or(on).parse::<u32>().is_ok_and(|b| b == w.badge)
        || w.git_folders().any(|f| f.branch.as_deref() == Some(on))
}

/// The service a worker's verb names: a definition the human made, used here, not shared.
pub(crate) fn worker_service<'a>(stack: &'a [Service], name: &str) -> Result<&'a Service, String> {
    let want = Service::normalize_name(name);
    let s = stack
        .iter()
        .filter(|s| !s.is_job())
        .find(|s| s.id == name || Service::normalize_name(&s.name) == want)
        .ok_or_else(|| format!("This Hive's stack has no service \"{}\".", one_line(name, 60)))?;
    if s.shared_from.is_some() {
        return Err(format!("{} is shared from another workspace: it is started there, never from here.", s.name));
    }
    if s.origin == "agent" || s.agent_edited {
        return Err(format!(
            "{} was made or changed by an agent: a worker runs only services your human defined. Tell your coordinator if you need it.",
            s.name
        ));
    }
    Ok(s)
}

/// A service whose command takes no `{port}` hard-codes its port and runs one copy at a time
/// (§6.9): a second one on the worker's branch could bind nothing (its profile opens only the
/// ports booked for it) and would collide with the human's. Moving the one copy onto the branch
/// takes the human's away, which is the human's call, so a worker is refused.
pub(crate) fn one_copy_refusal(s: &Service) -> Result<(), String> {
    if crate::stack::placeholders::Needs::of(&s.command, &s.env).one_copy() {
        return Err(format!(
            "{} hard-codes its port, so it runs one copy at a time and can't have one on your branch beside your human's. Ask \
             your coordinator: your human can move it onto your branch, or give its command a {{port}}.",
            s.name
        ));
    }
    Ok(())
}

/// Where a service's copy on the worker's branch runs: the worker's folder of its repo, and its
/// folder re-rooted in that worktree. Blocks on git.
fn service_place(paths: &HivePaths, s: &Service, w: &WorkerRecord) -> Result<(String, String, PathBuf), String> {
    use crate::stack::target;
    let ctx = crate::stack::context_for(Some(&paths.root));
    let repo = target::repo_of(std::path::Path::new(&s.cwd), &ctx)?;
    for f in w.git_folders() {
        let wt = f.worktree.clone().unwrap_or_default();
        if let Ok(place) = target::checkout_place(&repo, std::path::Path::new(&wt), &ctx) {
            let cwd = place.root.join(&repo.rel);
            if !cwd.is_dir() {
                let rel = repo.rel.display().to_string().replace('\\', "/");
                return Err(format!("{rel}/ doesn't exist on your branch, so {} can't run there.", s.name));
            }
            return Ok((f.name.clone(), wt, cwd));
        }
    }
    Err(format!("{} runs in a repo you have no worktree of, so it can't run on your branch.", s.name))
}

/// The job a worker's start of human-defined service `s` queues: its own command under the
/// worker's sandbox, in `folder`'s worktree at `cwd` (`service_place`), needing what a queued
/// entry for that command needs, else nothing. Blocks on git.
pub(crate) fn service_spec(caller: &Caller, w: &WorkerRecord, s: &Service, stack: &[Service], folder: &str, cwd: &std::path::Path) -> Result<JobSpec, String> {
    let f = w.folders.iter().find(|f| f.name == folder && f.worktree.is_some()).ok_or("Your folder of it is gone")?;
    let target = format!("{}{}", crate::stack::target::CHECKOUT, f.worktree.as_deref().unwrap_or_default());
    let key = crate::stack::copy_key(&s.id, &target);
    let q = &caller.hive.settings.run_queue;
    let words: Vec<String> = s.command.split_whitespace().map(str::to_string).collect();
    let entry = match_entry(q, folder, &words);
    let (commit, dirty) = worktree_state(f)?;
    let leases = match entry {
        Some(e) => leases_for(e, stack, &target)?.into_iter().filter(|k| *k != key).collect(),
        None => Vec::new(),
    };
    let mut env = s.env.clone();
    for (k, v) in entry.iter().flat_map(|e| e.env.iter()) {
        env.retain(|(o, _)| o != k);
        env.push((k.clone(), v.clone()));
    }
    Ok(JobSpec {
        hive_id: caller.hive.id.clone(),
        workspace_id: caller.workspace_id.clone(),
        window: caller.label.clone(),
        task_id: w.task_id.clone(),
        worker_slug: w.slug.clone(),
        folder: folder.to_string(),
        target,
        argv: super::job_sandbox::service_argv(&s.command),
        entry: s.name.clone(),
        needs: entry.map(|e| e.needs.clone()).unwrap_or_default(),
        env,
        extra_write: entry.map(|e| e.extra_write.clone()).unwrap_or_default(),
        leases,
        shares: q.resources.clone(),
        // A service runs until it's stopped: the longest a job may.
        timeout_s: queue::MAX_TIMEOUT_S,
        branch: f.branch.clone(),
        commit: Some(commit),
        dirty,
        service_id: Some(s.id.clone()),
        cwd: Some(cwd.to_string_lossy().to_string()),
        command: Some(s.command.clone()),
        ..JobSpec::default()
    })
}

/// A worker's `startService`, `restartService` or `stopService` (§6.8, §7.2): only a human-defined
/// service, only on its own branch, as a queued job under its sandbox; a stop takes only its own
/// copy. Called for any request a worker tab makes, whatever `tabId` it names.
pub async fn worker_service_verb(tool: &str, args: &Value, worker_tab: &str, state: &Arc<AppState>, _handle: &AppHandle) -> Value {
    let caller = super::tools::caller_for_tab(&state.app_data.read(), worker_tab).map(|mut c| {
        c.tab_id = worker_tab.to_string();
        c
    });
    let res = match caller {
        Some(c) => service_verb(tool, args, &c, &state.app_data).await,
        None => Err("Only a Hive's worker can call this, from its own tab.".to_string()),
    };
    res.unwrap_or_else(|e| {
        log::info!("hive: {tool} from worker tab {worker_tab} refused: {e}");
        json!({ "error": e })
    })
}

/// `worker_service_verb` for the worker whose tab is `caller.tab_id`.
pub(crate) async fn service_verb(tool: &str, args: &Value, caller: &Caller, data: &parking_lot::RwLock<AppData>) -> Result<Value, String> {
    let caller = caller.clone();
    let w = calling_worker(&caller).await?;
    let name = args.get("service").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).ok_or("service is required")?;
    let on = args.get("on").and_then(Value::as_str).unwrap_or_default();
    if !own_branch(&w, on) {
        return Err(format!(
            "A worker runs services only on its own branch: \"{}\" isn't yours. Leave out on, or name your branch.",
            one_line(on, 60)
        ));
    }
    let stack = workspace_stack(data, &caller);
    let s = worker_service(&stack, name)?.clone();
    if tool != "stopService" {
        one_copy_refusal(&s)?;
    }
    let (paths, svc, rec) = (caller.paths.clone(), s.clone(), w.clone());
    let (folder, wt, cwd) = blocking(move || service_place(&paths, &svc, &rec)).await?;
    let target = format!("{}{wt}", crate::stack::target::CHECKOUT);
    let key = crate::stack::copy_key(&s.id, &target);
    let mine = queue::active_service_job(&w.task_id, &s.id);
    if tool != "startService" {
        let lease = queue::lease_of(&key);
        if let Some(j) = lease.job.filter(|j| mine.as_ref().is_none_or(|m| m.id != j.id)) {
            return Err(format!("{} on your branch is in use by run {} (`{}`): wait for it to end.", s.name, j.id, one_line(&j.entry, 80)));
        }
    }
    match (tool, &mine) {
        ("stopService", None) => {
            return Err(format!("You have no copy of {} running: stopService stops only your own copies.", s.name));
        }
        ("stopService", Some(j)) => {
            queue::interrupt(&j.id, queue::BY_WORKER, "the worker stopped it");
            log::info!("hive {}: {} stopped its copy of {} ({})", caller.hive.id, worker_label(&w), s.name, j.id);
            return Ok(json!({ "service": s.name, "on": w.slug, "job": j.id, "stopped": true }));
        }
        ("startService", Some(j)) => {
            return Err(format!("Your copy of {} is already {} as {}: restartService restarts it.", s.name, j.state, j.id));
        }
        ("restartService", Some(j)) => {
            queue::interrupt(&j.id, queue::BY_WORKER, "the worker restarted it");
        }
        _ => {}
    }
    let (c, rec, svc) = (caller.clone(), w.clone(), s.clone());
    let spec = blocking(move || service_spec(&c, &rec, &svc, &stack, &folder, &cwd)).await?;
    let job = queue::enqueue(spec)?;
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let (d, id, cmd) = (dir, job.id.clone(), s.name.clone());
    if let Err(e) = blocking(move || record_open(&d, &id, &cmd)).await {
        log::warn!("hive {}: {} not recorded as open: {e}", caller.hive.id, job.id);
    }
    log::info!("hive {}: {} queued its copy of {} ({})", caller.hive.id, worker_label(&w), s.name, job.id);
    let mut v = job_view(&job);
    v["service"] = json!(s.name);
    v["on"] = json!(w.slug);
    v["note"] = json!(
        "Your copy runs as a queued job on your branch, under your sandbox, until stopService (or its timeout). It ends as a ⟦HIVE RUN⟧ message."
    );
    Ok(v)
}

// ── The human's settings: which commands are queued ──

/// A run queue as the human edited it, checked and tidied: a prefix of plain words, needs that
/// name resources, env a shell takes, extra writes that are absolute.
pub fn validate_run_queue(hive: &Hive, rq: &RunQueueSettings) -> Result<RunQueueSettings, String> {
    if rq.commands.len() > MAX_CHECK_LIST {
        return Err(format!("At most {MAX_CHECK_LIST} queued commands."));
    }
    let mut out = RunQueueSettings::default();
    for c in &rq.commands {
        let f = hive.folders.iter().find(|f| f.name == c.folder).ok_or_else(|| format!("This Hive has no folder \"{}\".", c.folder))?;
        if !f.is_git() {
            return Err(format!("{} is a plain folder: nothing runs in it.", f.name));
        }
        let words = parse_command(&c.prefix).map_err(|e| format!("`{}`: {e}", c.prefix.trim()))?;
        let prefix = words.join(" ");
        if out.commands.iter().any(|o| o.folder == c.folder && o.prefix == prefix) {
            return Err(format!("`{prefix}` is listed twice for {}.", c.folder));
        }
        let needs: Vec<String> = c.needs.iter().map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).collect();
        queue::expand_needs(&needs, "w").map_err(|e| format!("`{prefix}` needs {e}"))?;
        let services: Vec<String> = c.services.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
        if let Some(s) = services.iter().find(|s| s.chars().count() > 100 || s.chars().any(char::is_control)) {
            return Err(format!("\"{}\" isn't a service name.", one_line(s, 60)));
        }
        for (k, v) in &c.env {
            if !crate::stack::placeholders::valid_env_key(k) || super::job_sandbox::SCRUBBED.contains(&k.as_str()) {
                return Err(format!("`{prefix}`: {k:?} can't be set for a job."));
            }
            if v.chars().any(char::is_control) {
                return Err(format!("`{prefix}`: {k}'s value holds a control character."));
            }
        }
        let mut extra = Vec::new();
        for p in c.extra_write.iter().map(|p| p.trim()).filter(|p| !p.is_empty()) {
            if !(p.starts_with('/') || p.starts_with("~/")) || p.chars().any(|ch| ch == '"' || ch == '\\' || ch.is_control()) {
                return Err(format!("`{prefix}`: {} isn't an absolute path the sandbox can name.", one_line(p, 120)));
            }
            extra.push(p.to_string());
        }
        out.commands.push(QueuedCommand { folder: c.folder.clone(), prefix, needs, services, env: c.env.clone(), extra_write: extra });
    }
    for (name, cap) in &rq.resources {
        if !queue::valid_resource(name) {
            return Err(format!("\"{}\" isn't a resource name (lowercase letters, digits and . _ : -).", one_line(name, 60)));
        }
        if !(1..=64).contains(cap) {
            return Err(format!("{name}: a capacity is 1 to 64."));
        }
        out.resources.insert(name.clone(), *cap);
    }
    Ok(out)
}

/// Hive settings → Run queue: the human's list of queued commands and resource limits. Answers
/// the Hive as saved; its window's mirror follows `hive-changed`.
#[tauri::command]
pub fn hive_set_run_queue(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    run_queue: RunQueueSettings,
) -> Result<Hive, String> {
    use tauri::Emitter;
    let (hive, snapshot) = {
        let mut data = state.app_data.write();
        let ws = data
            .window_mut(window.label())
            .and_then(|w| w.workspaces.iter_mut().find(|w| w.id == workspace_id))
            .ok_or("Workspace not found")?;
        let hive = ws.hive.as_mut().ok_or("That workspace isn't a Hive")?;
        hive.settings.run_queue = validate_run_queue(hive, &run_queue)?;
        (hive.clone(), data.clone())
    };
    crate::state::save_state(&snapshot)?;
    log::info!("hive {}: run queue set: {} queued command(s)", hive.id, hive.settings.run_queue.commands.len());
    let payload = json!({ "workspace_id": workspace_id, "hive": hive });
    if let Err(e) = app.emit_to(window.label(), super::fuses::HIVE_CHANGED_EVENT, payload) {
        log::warn!("hive {}: run queue change not sent to its window: {e}", hive.id);
    }
    Ok(hive)
}

#[cfg(test)]
mod tests;
