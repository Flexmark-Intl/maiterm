//! A worker leaving for good (docs/hive.md §8.4), and its chat read back after (§4.2).
//!
//! Retirement never deletes work: its branches always stay, a worktree goes only when git says it
//! holds nothing uncommitted or untracked, and never one whose `.git` file maiTerm can't vouch for
//! (a worker can rewrite it, and maiTerm's own `git worktree remove` would then follow it into a
//! repo whose config the worker wrote). Its `scratch/` goes only once every file in it is one of
//! its task's gallery assets, byte for byte. The tab goes, the task row stays, and the session list it leaves behind is what
//! `get_task_transcript` reads.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{json, Value};

use super::git::{git_in, git_in_worktree, run, QUERY_TIMEOUT, WORKTREE_TIMEOUT};
use super::paths::HivePaths;
use super::record::{Hive, KIND_GIT};
use super::rpc::{self, Front, HiveWindow};
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::{self, Retired, WorkerFolder, WorkerRecord};
use crate::state::workspace::TaskNote;

/// How long a retiring agent gets to leave after its SIGTERM, so its transcript's last lines are
/// on disk before its tab (and PTY) go.
const EXIT_GRACE: Duration = Duration::from_secs(5);
/// Turns read per recorded session.
const TURNS_PER_SESSION: usize = 200;

// ── Worktrees ──

/// What became of one worktree.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Removal {
    Removed,
    /// Its folder was already gone: only git's entry for it went.
    Gone,
    /// Kept, with the reason in words for the human.
    Kept(String),
}

/// Whether `<wt>/.git` is the file git wrote: a plain file whose one line names `admin`.
pub(crate) fn git_file_names(wt: &Path, admin: &Path) -> bool {
    let p = wt.join(".git");
    let Ok(meta) = fs::symlink_metadata(&p) else { return false };
    if !meta.is_file() || meta.len() > 4096 {
        return false;
    }
    let Ok(text) = fs::read_to_string(&p) else { return false };
    let line = text.trim_end_matches(['\n', '\r']);
    let Some(named) = line.strip_prefix("gitdir:").map(str::trim) else { return false };
    if named.is_empty() || named.contains(['\n', '\r']) {
        return false;
    }
    let named = Path::new(named);
    let named = if named.is_absolute() { named.to_path_buf() } else { wt.join(named) };
    crate::share::canon(&named) == crate::share::canon(admin)
}

/// `git worktree remove`, never forced, of a worktree whose admin dir maiTerm knows (`f.admin_dir`),
/// only when its `.git` file names that dir and git, run on it, reports nothing uncommitted or
/// untracked. Git refuses modified or untracked files again on its own. Blocks.
pub(crate) fn remove_if_clean(repo: &Path, f: &WorkerFolder) -> Removal {
    let Some(wt) = f.worktree.as_deref().map(Path::new) else { return Removal::Gone };
    if fs::symlink_metadata(wt).is_err() {
        drop_entry(repo, wt);
        return Removal::Gone;
    }
    if let Some(why) = keep_reason(f) {
        return Removal::Kept(why);
    }
    let label = &f.name;
    match run(git_in(repo).args(["worktree", "remove"]).arg(wt), WORKTREE_TIMEOUT) {
        Ok(_) => Removal::Removed,
        Err(e) => Removal::Kept(format!("{label}: {} (git kept it: {e})", wt.display())),
    }
}

/// Why an existing worktree must stay, or None when maiTerm may remove it: its `.git` file names
/// the recorded admin dir and git, run on that dir, sees nothing uncommitted or untracked. The
/// `.git` file is checked before any git runs there. Blocks.
pub(crate) fn keep_reason(f: &WorkerFolder) -> Option<String> {
    let wt = Path::new(f.worktree.as_deref()?);
    let label = &f.name;
    let Some(admin) = f.admin_dir.as_deref() else {
        return Some(format!("{label}: {} (maiTerm has no record of its git folder)", wt.display()));
    };
    if !git_file_names(wt, Path::new(admin)) {
        return Some(format!("{label}: {} (its .git file isn't the one git wrote, so maiTerm ran no git there)", wt.display()));
    }
    let status = git_in_worktree(f).and_then(|mut c| run(c.args(["status", "--porcelain", "--untracked-files=all"]), QUERY_TIMEOUT));
    match status {
        Err(e) => Some(format!("{label}: {} (its state couldn't be read: {e})", wt.display())),
        Ok(s) if !s.trim().is_empty() => {
            let n = s.lines().filter(|l| !l.trim().is_empty()).count();
            Some(format!("{label}: {} ({n} uncommitted or untracked file{})", wt.display(), if n == 1 { "" } else { "s" }))
        }
        Ok(_) => None,
    }
}

/// A worktree maiTerm made without recording its git folder (the coordinator's read copies, a
/// land's temporary worktree), as `remove_if_clean` takes one: its admin dir found from the repo's
/// side, inside the repo's own `worktrees/`, and naming this worktree back. None when there's no
/// such admin dir. Blocks.
pub(crate) fn found_folder(name: &str, repo: &Path, wt: &Path) -> Option<WorkerFolder> {
    let common = run(git_in(repo).args(["rev-parse", "--path-format=absolute", "--git-common-dir"]), QUERY_TIMEOUT).ok()?;
    let common = crate::share::canon(Path::new(common.trim()));
    let want = crate::share::canon(wt);
    for e in fs::read_dir(common.join("worktrees")).ok()?.flatten() {
        let admin = e.path();
        let Ok(back) = fs::read_to_string(admin.join("gitdir")) else { continue };
        // Its `gitdir` names `<worktree>/.git`; compared by the folder, so no `.git` is followed.
        let names = Path::new(back.trim()).parent().map(crate::share::canon);
        if names.as_deref() == Some(want.as_path()) {
            return Some(WorkerFolder {
                name: name.to_string(),
                kind: KIND_GIT.to_string(),
                worktree: Some(wt.to_string_lossy().to_string()),
                common_dir: Some(common.to_string_lossy().to_string()),
                admin_dir: Some(crate::share::canon(&admin).to_string_lossy().to_string()),
                ..WorkerFolder::default()
            });
        }
    }
    None
}

/// A reviewer's detached copy (§7.7) goes even with changes in it: it holds no work, only what
/// the reviewer tried, and its verdict carries the evidence. Forced, so git runs no status
/// there, and still only once its `.git` file names the recorded admin dir. Blocks.
pub(crate) fn remove_detached(repo: &Path, f: &WorkerFolder) -> Removal {
    let Some(wt) = f.worktree.as_deref().map(Path::new) else { return Removal::Gone };
    if fs::symlink_metadata(wt).is_err() {
        drop_entry(repo, wt);
        return Removal::Gone;
    }
    let label = &f.name;
    let vouched = f.admin_dir.as_deref().is_some_and(|admin| git_file_names(wt, Path::new(admin)));
    if !vouched {
        return Removal::Kept(format!("{label}: {} (its .git file isn't the one git wrote, so maiTerm ran no git there)", wt.display()));
    }
    match run(git_in(repo).args(["worktree", "remove", "--force"]).arg(wt), WORKTREE_TIMEOUT) {
        Ok(_) => Removal::Removed,
        Err(e) => Removal::Kept(format!("{label}: {} (git kept it: {e})", wt.display())),
    }
}

/// Drops git's entry for a worktree whose folder is gone: nothing there to lose.
fn drop_entry(repo: &Path, wt: &Path) {
    if let Err(e) = run(git_in(repo).args(["worktree", "remove", "--force"]).arg(wt), QUERY_TIMEOUT) {
        log::debug!("hive: no worktree entry for {} to drop: {e}", wt.display());
    }
}

/// Removes a path maiTerm made, never following a link (a link goes, not what it names).
pub(crate) fn remove_path(p: &Path) -> std::io::Result<()> {
    match fs::symlink_metadata(p) {
        Ok(m) if m.is_dir() => fs::remove_dir_all(p),
        Ok(_) => fs::remove_file(p),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether a worker's `scratch/` holds anything but the brief maiTerm writes there.
pub(crate) fn scratch_has_work(scratch: &Path) -> bool {
    fs::read_dir(scratch).into_iter().flatten().flatten().any(|e| e.file_name() != "brief.md")
}

/// Past these a scratch folder is kept without hashing it all (plan D15).
const COVER_MAX_FILES: usize = 10_000;
const COVER_MAX_BYTES: u64 = 20 << 30;

/// Whether everything in a worker's `scratch/` is in the gallery (§8.4, plan D15): every regular
/// file's SHA-256 equals one of this task's assets out of the trash. maiTerm's `brief.md` aside,
/// empty folders count as nothing; a link, anything but a file or folder, an unreadable entry,
/// or more than 10,000 files or 20 GB counts as not covered. Blocks.
pub(crate) fn scratch_covered(scratch: &Path, gallery: &super::gallery::Gallery, task_id: &str) -> bool {
    let Ok(assets) = gallery.task_assets(task_id) else { return false };
    let shas: HashSet<String> = assets.into_iter().filter(|a| !a.in_trash()).map(|a| a.sha256).collect();
    let (mut files, mut bytes) = (0usize, 0u64);
    let mut stack = vec![scratch.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else { return false };
        for e in entries {
            let Ok(e) = e else { return false };
            let p = e.path();
            if dir == scratch && e.file_name() == "brief.md" {
                continue;
            }
            let Ok(m) = fs::symlink_metadata(&p) else { return false };
            if m.is_dir() {
                stack.push(p);
                continue;
            }
            if !m.is_file() {
                return false;
            }
            files += 1;
            bytes += m.len();
            if files > COVER_MAX_FILES || bytes > COVER_MAX_BYTES {
                return false;
            }
            match super::gallery::ingest::hash_file(&p) {
                Ok((sha, _)) if shas.contains(&sha) => {}
                _ => return false,
            }
        }
    }
    true
}

/// What retirement did on disk.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Disk {
    /// The folders whose worktree went.
    pub removed: Vec<String>,
    pub kept: Vec<String>,
}

/// A retiring worker's disk (§8.4): each clean worktree goes (a reviewer's detached copies
/// whatever they hold), maiTerm's own caches go (a warm cargo target is gigabytes, and no work
/// lives in either), its branches stay, and its scratch stays unless the gallery holds everything
/// in it (`scratch_covered`). Blocks.
pub(crate) fn retire_disk(hive: &Hive, paths: &HivePaths, w: &WorkerRecord) -> Disk {
    let mut out = Disk::default();
    for f in w.git_folders() {
        let Some(repo) = hive.folders.iter().find(|h| h.name == f.name).map(|h| PathBuf::from(&h.path)) else {
            out.kept.push(format!("{}: {} (this Hive has no such folder now)", f.name, f.worktree.as_deref().unwrap_or_default()));
            continue;
        };
        let removal = if w.is_reviewer() && f.branch.is_none() { remove_detached(&repo, f) } else { remove_if_clean(&repo, f) };
        match removal {
            Removal::Removed => out.removed.push(f.name.clone()),
            Removal::Gone => {}
            Removal::Kept(why) => out.kept.push(why),
        }
    }
    if super::paths::is_component(&w.slug) && !super::paths::RESERVED_SLUGS.contains(&w.slug.as_str()) {
        for d in [paths.worker_cargo_target(&w.slug), paths.worker_tmp(&w.slug)] {
            if let Err(e) = remove_path(&d) {
                log::warn!("hive: {} left in place: {e}", d.display());
            }
        }
        let scratch = paths.worker_scratch(&w.slug);
        if scratch_has_work(&scratch) {
            let covered = super::gallery::for_paths(&hive.id, paths).is_ok_and(|g| scratch_covered(&scratch, &g, &w.task_id));
            match covered.then(|| remove_path(&scratch)) {
                Some(Ok(())) => log::info!("hive {}: {}'s scratch removed: the gallery holds everything in it", hive.id, worker_label(w)),
                Some(Err(e)) => {
                    log::warn!("hive {}: {} left in place: {e}", hive.id, scratch.display());
                    out.kept.push(format!("scratch: {}", scratch.display()));
                }
                None => out.kept.push(format!("scratch: {}", scratch.display())),
            }
        }
    }
    out
}

// ── Retiring ──

/// What a retirement is: where the worker goes and what its task says.
pub(crate) struct RetireSpec {
    /// `landed` | `retired`.
    pub phase: &'static str,
    /// The phases it may leave from; any other is a change meanwhile, refused.
    pub from: Vec<&'static str>,
    pub reason: String,
    /// The task's lane: `done` lets its waiters launch, `dropped` blocks them.
    pub task_status: &'static str,
    /// The note on its task.
    pub note: String,
    pub landed: Option<String>,
    /// A landing worker may leave too, while its land is held (abandoning it).
    pub held_land: bool,
}

pub(crate) struct Outcome {
    pub record: WorkerRecord,
    pub disk: Disk,
    /// Whether a running agent was sent SIGTERM.
    pub agent_stopped: bool,
    pub runs_released: usize,
}

/// Retire `w` (§8.4): its phase first, so the agent's exit reads as a retirement, never a crash;
/// its agent, given a moment to flush its transcript; its runs; its tab (the window drops it,
/// killing the PTY) and its row's lane, the row kept; then its worktrees, with nothing of it still
/// running in them; and last its record without a tab. The window's half is best effort: once the
/// phase moved, the retirement stands.
pub(crate) async fn retire_worker<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, spec: RetireSpec) -> Result<Outcome, String> {
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let (d, from, phase, now) = (dir.clone(), spec.from.clone(), spec.phase, crate::commands::workspace::iso_now());
    let retired = Retired { reason: spec.reason.clone(), at: now, landed: spec.landed.clone(), ..Retired::default() };
    let label = worker_label(w);
    let held_land = spec.held_land;
    let record = blocking(move || {
        let mut moved = false;
        let r = worker::update_worker(&d, |cur| {
            moved = from.contains(&cur.phase.as_str()) || (held_land && super::land::held(cur));
            if moved {
                super::land::approval::hold_if_partial(cur, "the worker was retired");
                cur.phase = phase.to_string();
                cur.wait_reason = None;
                cur.retired = Some(retired);
            }
            moved
        })?;
        if moved { Ok(r) } else { Err(format!("Worker {label} is {} now: look again with hiveListWorkers.", r.phase)) }
    })
    .await?;
    super::land::approval::withdraw(&ctx.window, caller, w, "the worker was retired, so nothing would land");

    let tab = record.tab_id.clone().filter(|t| super::tools::has_tab(ctx.data, caller, t));
    let mut agent_stopped = false;
    if let Some(t) = &tab {
        if ctx.window.front(t).await == Front::Job {
            match ctx.window.stop_agent(t).await {
                Ok(sent) => {
                    agent_stopped = sent;
                    wait_for_exit(ctx, t).await;
                }
                // Its tab goes anyway, and whatever holds the terminal with it.
                Err(e) => log::info!("hive {}: worker {} retiring: {e}", caller.hive.id, worker_label(w)),
            }
        }
    }
    let runs_released = super::queue::retire_worker(&w.task_id, "you were retired");

    drop_into_history(ctx, caller, &w.task_id, tab.as_deref(), spec.task_status, &spec.note);
    let args = json!({
        "workspace_id": caller.workspace_id,
        "task_id": w.task_id,
        "tab_id": tab,
        "status": spec.task_status,
        "note": spec.note,
    });
    if let Err(e) = rpc::applied(ctx.window.request(rpc::VERB_RETIRE, args).await) {
        log::warn!("hive {}: worker {} retired, its window's half not done: {e}", caller.hive.id, worker_label(w));
    }
    let snapshot = ctx.data.read().clone();
    if let Err(e) = (ctx.save)(&snapshot) {
        log::warn!("hive {}: worker {} retired, the state not saved: {e}", caller.hive.id, worker_label(w));
    }

    let (hive, paths, rec) = (caller.hive.clone(), caller.paths.clone(), record.clone());
    let disk = blocking(move || Ok(retire_disk(&hive, &paths, &rec))).await?;
    let kept = disk.kept.clone();
    let record = blocking(move || {
        worker::update_worker(&dir, |cur| {
            cur.tab_id = None;
            if let Some(r) = cur.retired.as_mut() {
                r.kept = kept;
            }
            true
        })
    })
    .await?;
    super::diffstat::forget(&caller.hive.id, &w.task_id);
    if let Some(t) = &tab {
        super::feed::forget_meta(t);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    log::info!(
        "hive {}: worker {} {} ({}): removed {:?}, kept {:?}",
        caller.hive.id,
        worker_label(w),
        spec.phase,
        spec.reason,
        disk.removed,
        disk.kept
    );
    Ok(Outcome { record, disk, agent_stopped, runs_released })
}

/// Up to `EXIT_GRACE` for the shell's prompt to come back after the SIGTERM.
pub(crate) async fn wait_for_exit<W: HiveWindow>(ctx: &Ctx<'_, W>, tab: &str) {
    let step = Duration::from_millis(250);
    let mut waited = Duration::ZERO;
    while waited < EXIT_GRACE {
        if matches!(ctx.window.front(tab).await, Front::Shell | Front::NoTerminal) {
            return;
        }
        tokio::time::sleep(step).await;
        waited += step;
    }
}

/// The retired worker's tab out of the state and its row moved to `status` with `note`, kept, as
/// the window does in its own copy. Unsaved.
fn drop_into_history<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, tab: Option<&str>, status: &str, note: &str) {
    let mut d = ctx.data.write();
    let Ok(ws) = super::tools::workspace_mut(&mut d, caller) else { return };
    super::tools::drop_tab(ws, tab);
    let now = crate::commands::workspace::iso_now();
    if let Some(row) = ws.tasks.iter_mut().find(|t| t.id == task_id) {
        row.status = status.to_string();
        row.updated_at = now.clone();
        row.notes.push(TaskNote { at: now, text: note.to_string(), by: "agent".to_string() });
        let over = row.notes.len().saturating_sub(crate::state::workspace::TASK_NOTE_CAP);
        row.notes.drain(..over);
    }
}

// ── hiveRetireWorker ──

/// The phases the coordinator may retire a worker from: its agent isn't working (§8.4).
pub(crate) const RETIRABLE: [&str; 4] = [worker::PHASE_STOPPED, worker::PHASE_FAILED, worker::PHASE_WAITING, worker::PHASE_REPORTED];

pub(crate) fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveRetireWorker",
        "description": "Retire a worker for good, abandoning its task: its task goes to dropped (workers that depend on it are \
            blocked, and you're told), its agent ends and its tab goes, its run-queue jobs are released. Its branches are \
            always kept; each worktree is removed only if it holds nothing uncommitted or untracked (the answer lists what \
            was kept); its scratch folder is kept unless every file in it is one of its gallery assets. Its chat stays readable in its column. For a stopped, failed, waiting or \
            reported worker; one still working (starting, running, blocked) is refused: hiveStopWorker it first. A landing \
            worker is retired only while its land is held, abandoning the rest of it (what landed stays landed). The reason \
            goes on its task.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "worker": { "type": ["string", "number"], "description": "The worker: badge (3 or \"#3\"), name or task id" },
                "reason": { "type": "string", "description": "Why it's abandoned, in a sentence" }
            },
            "required": ["worker", "reason"]
        }
    })
}

/// Why `phase` can't be retired by the coordinator, or None. `held`: a landing worker's land is
/// held (`land::held`), so retiring it abandons the rest.
pub(crate) fn retire_refusal(label: &str, phase: &str, held: bool) -> Option<String> {
    match phase {
        p if RETIRABLE.contains(&p) => None,
        worker::PHASE_LANDING if held => None,
        worker::PHASE_LAUNCHING | worker::PHASE_RUNNING | worker::PHASE_BLOCKED => Some(format!(
            "Worker {label} is {phase}: its agent may still be working. hiveStopWorker stops it; retire it after."
        )),
        worker::PHASE_LANDING => Some(format!("Worker {label} is landing: retire it once its land is held, or let it finish.")),
        worker::PHASE_LANDED | worker::PHASE_RETIRED => Some(format!("Worker {label} is already {phase}.")),
        other => Some(format!("Worker {label} is {other}: only a stopped, failed, waiting or reported worker is retired.")),
    }
}

pub(crate) async fn retire_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    if reason.is_empty() {
        return Err("reason is required: it goes on the worker's task".to_string());
    }
    if reason.chars().count() > super::control::MAX_REASON {
        return Err(format!("Keep the reason under {} characters.", super::control::MAX_REASON));
    }
    let w = super::control::named_worker(caller, args).await?;
    if let Some(why) = retire_refusal(&worker_label(&w), &w.phase, super::land::held(&w)) {
        return Err(why);
    }
    let spec = RetireSpec {
        phase: worker::PHASE_RETIRED,
        from: RETIRABLE.to_vec(),
        reason: reason.clone(),
        task_status: "dropped",
        note: format!("Retired by the coordinator: {reason}"),
        landed: None,
        held_land: true,
    };
    let out = retire_worker(ctx, caller, &w, spec).await?;
    let branches: Vec<String> = w.git_folders().filter_map(|f| f.branch.as_ref().map(|b| format!("{}: {b}", f.name))).collect();
    let mut answer = json!({
        "worker": w.slug,
        "badge": w.badge,
        "phase": worker::PHASE_RETIRED,
        "task": "dropped",
        "agent": if out.agent_stopped { "its agent was sent SIGTERM" } else { "no agent was running" },
        "runs_released": out.runs_released,
        "worktrees_removed": out.disk.removed,
        "kept": out.disk.kept,
        "branches_kept": branches,
        "note": "Its chat stays readable in its column. Workers that depend on it are blocked: you'll be told which.",
    });
    // A held land's landed repos stay landed: its record and the land log keep them.
    let landed: Vec<String> = w.land.as_ref().map(|l| l.landed().iter().map(|r| r.folder.clone()).collect()).unwrap_or_default();
    if !landed.is_empty() {
        answer["landed"] = json!(landed);
    }
    Ok(answer)
}

// ── A retired worker's chat ──

/// The Claude `projects/` folders a recorded transcript may be read from: the home's and each
/// managed account root's, as they really are (an account's is a link to the home's).
pub(crate) fn transcript_roots() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let mut add = |p: PathBuf| {
        if let Ok(real) = fs::canonicalize(&p) {
            let real = crate::share::canon(&real);
            if !out.contains(&real) {
                out.push(real);
            }
        }
    };
    if let Some(home) = dirs::home_dir() {
        add(home.join(".claude").join("projects"));
    }
    if let Some(dir) = crate::accounts::accounts_dir().map(|d| d.join("claude")) {
        for e in fs::read_dir(dir).into_iter().flatten().flatten() {
            add(e.path().join("projects"));
        }
    }
    out
}

/// A recorded transcript path as it really is, only when that is a `.jsonl` file under one of
/// `roots` (D16): `sessions.json` sits in the Hive folder, and a path written there must not
/// become a read of any file on the machine.
pub(crate) fn readable_transcript(path: &str, roots: &[PathBuf]) -> Option<PathBuf> {
    let real = crate::share::canon(&fs::canonicalize(path).ok()?);
    let is_jsonl = real.extension().is_some_and(|e| e == "jsonl");
    let is_file = fs::metadata(&real).is_ok_and(|m| m.is_file());
    (is_jsonl && is_file && roots.iter().any(|r| real.starts_with(r) && real != *r)).then_some(real)
}

/// The task's recorded sessions in order, each as its last turns, a divider between them. A turn
/// already shown (a fork copies its parent's lines) is shown once. Blocks.
pub(crate) fn task_transcript(task_dir: &Path, roots: &[PathBuf]) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut shown = 0;
    for s in super::launch::read_sessions(task_dir) {
        let Some(path) = s.transcript_path.as_deref().and_then(|p| readable_transcript(p, roots)) else {
            log::info!("hive: session {} of {} has no readable transcript; skipped", s.session_id, task_dir.display());
            continue;
        };
        let Some(turns) = crate::mailink::transcript::turns_from_path(&path, TURNS_PER_SESSION, crate::mailink::transcript::ToolRender::Marker) else {
            continue;
        };
        let turns: Vec<Value> = turns
            .into_iter()
            .filter(|t| t.get("msg_id").and_then(Value::as_str).is_none_or(|id| seen.insert(id.to_string())))
            .collect();
        let Some(first_ts) = turns.first().and_then(|t| t.get("ts")).cloned() else { continue };
        if shown > 0 {
            out.push(json!({
                "msg_id": format!("session:{}", s.session_id),
                "role": "system",
                "kind": "session_divider",
                "text": "New session",
                "ts": first_ts,
            }));
        }
        out.extend(turns);
        shown += 1;
    }
    out
}

/// A retired worker's chat (§4.2, §8.4): only the sessions maiTerm recorded for that task, never a
/// path from the caller.
#[tauri::command]
pub async fn get_task_transcript(hive_id: String, task_id: String) -> Result<Vec<Value>, String> {
    let root = super::paths::hive_root(&hive_id).ok_or("That isn't a Hive")?;
    let dir = worker::task_dir(&HivePaths::new(root), &task_id)?;
    blocking(move || Ok(task_transcript(&dir, &transcript_roots()))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::launch::{append_session, SessionEntry};

    fn scratch(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("hive-retire-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        crate::share::canon(&t)
    }

    fn row(id: &str, text: &str, ts: &str) -> String {
        format!(
            "{}\n",
            json!({ "type": "user", "uuid": id, "timestamp": ts, "message": { "role": "user", "content": text } })
        )
    }

    #[test]
    fn a_retired_chat_joins_its_sessions_and_reads_only_recorded_transcripts_under_projects() {
        let base = scratch("chat");
        let projects = base.join("home/.claude/projects/-wt");
        fs::create_dir_all(&projects).unwrap();
        let roots = vec![crate::share::canon(&base.join("home/.claude/projects"))];
        let (a, b) = (projects.join("s1.jsonl"), projects.join("s2.jsonl"));
        fs::write(&a, row("u1", "first", "2026-10-24T10:00:00Z") + &row("u2", "second", "2026-10-24T10:01:00Z")).unwrap();
        // A resume or fork carries the last turn over: shown once.
        fs::write(&b, row("u2", "second", "2026-10-24T10:01:00Z") + &row("u3", "third", "2026-10-24T11:00:00Z")).unwrap();
        let outside = base.join("elsewhere/secret.jsonl");
        fs::create_dir_all(outside.parent().unwrap()).unwrap();
        fs::write(&outside, row("x1", "not yours", "2026-10-24T09:00:00Z")).unwrap();
        let not_jsonl = projects.join("notes.txt");
        fs::write(&not_jsonl, "x").unwrap();

        let task = base.join("hive/tasks/t1");
        fs::create_dir_all(&task).unwrap();
        let entry = |sid: &str, p: &Path| SessionEntry { session_id: sid.into(), transcript_path: Some(p.to_string_lossy().into()), started_at: "now".into() };
        append_session(&task, entry("s0", &outside)).unwrap();
        append_session(&task, entry("s1", &a)).unwrap();
        append_session(&task, entry("sx", &not_jsonl)).unwrap();
        append_session(&task, SessionEntry { session_id: "sn".into(), transcript_path: None, started_at: "now".into() }).unwrap();
        append_session(&task, entry("s2", &b)).unwrap();
        // A path through `..` that lands outside is outside.
        append_session(&task, entry("s3", &projects.join("../../../../elsewhere/secret.jsonl"))).unwrap();

        let turns = task_transcript(&task, &roots);
        let ids: Vec<&str> = turns.iter().map(|t| t["msg_id"].as_str().unwrap()).collect();
        assert_eq!(ids, vec!["u1", "u2", "session:s2", "u3"], "{turns:?}");
        assert_eq!(turns[2]["kind"], "session_divider");
        assert!(!turns.iter().any(|t| t["text"] == "not yours"), "nothing outside the projects folder");
        assert!(readable_transcript(&a.to_string_lossy(), &roots).is_some());
        assert!(readable_transcript(&outside.to_string_lossy(), &roots).is_none());
        assert!(readable_transcript(&roots[0].to_string_lossy(), &roots).is_none(), "the folder itself is no transcript");
        assert!(task_transcript(&base.join("hive/tasks/none"), &roots).is_empty(), "no sessions, no turns");
        let _ = fs::remove_dir_all(&base);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_inside_projects_to_a_file_outside_is_not_read() {
        let base = scratch("link");
        let projects = base.join("projects/-wt");
        fs::create_dir_all(&projects).unwrap();
        let outside = base.join("secret.jsonl");
        fs::write(&outside, row("x1", "not yours", "2026-10-24T09:00:00Z")).unwrap();
        let link = projects.join("evil.jsonl");
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        let roots = vec![crate::share::canon(&base.join("projects"))];
        assert!(readable_transcript(&link.to_string_lossy(), &roots).is_none());
        let _ = fs::remove_dir_all(&base);
    }

    /// Files `rel` from `sc` into the gallery for `task`, as galleryAdd stores it.
    fn file_into(g: &crate::hive::gallery::Gallery, sc: &Path, rel: &str, task: &str) -> String {
        use crate::hive::gallery::{ingest, model};
        let src = ingest::authorize(Path::new(rel), sc, &[sc.to_path_buf()], g.root()).unwrap();
        let id = model::mint(model::ASSET_PREFIX);
        let stored = ingest::store_file(&src, &g.files_dir(), &id).unwrap();
        g.insert_asset(model::NewAsset { id: id.clone(), file: stored.file, sha256: stored.sha256, bytes: stored.bytes, title: rel.into(), task_id: Some(task.into()), ..Default::default() })
            .unwrap();
        id
    }

    #[test]
    fn a_scratch_goes_only_when_the_gallery_holds_every_file_in_it() {
        let base = scratch("cover");
        let paths = HivePaths::new(base.join("hive"));
        let hive = Hive { id: format!("h-{}", uuid::Uuid::new_v4()), ..Hive::default() };
        let w = WorkerRecord { task_id: "t1".into(), slug: "auth".into(), badge: 1, ..WorkerRecord::default() };
        let sc = paths.worker_scratch("auth");
        fs::create_dir_all(sc.join("renders/empty")).unwrap();
        fs::write(sc.join("brief.md"), "maiTerm's").unwrap();
        fs::write(sc.join("renders/hero.png"), "png").unwrap();
        let g = crate::hive::gallery::for_paths(&hive.id, &paths).unwrap();
        assert!(!scratch_covered(&sc, &g, "t1"), "nothing filed yet");
        let hero = file_into(&g, &sc, "renders/hero.png", "t1");
        assert!(scratch_covered(&sc, &g, "t1"), "the brief and an empty folder count as nothing");
        assert!(!scratch_covered(&sc, &g, "t2"), "another task's assets cover nothing");

        fs::write(sc.join("notes.txt"), "x").unwrap();
        assert!(!scratch_covered(&sc, &g, "t1"), "an extra file");
        fs::remove_file(sc.join("notes.txt")).unwrap();
        fs::write(sc.join("renders/hero.png"), "png, edited").unwrap();
        assert!(!scratch_covered(&sc, &g, "t1"), "a file changed after it was filed");
        fs::write(sc.join("renders/hero.png"), "png").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(sc.join("renders/hero.png"), sc.join("link.png")).unwrap();
            assert!(!scratch_covered(&sc, &g, "t1"), "a link");
            fs::remove_file(sc.join("link.png")).unwrap();
        }
        g.trash(std::slice::from_ref(&hero)).unwrap();
        assert!(!scratch_covered(&sc, &g, "t1"), "an asset in the trash isn't in the gallery for this");
        g.restore(std::slice::from_ref(&hero)).unwrap();

        // Another worker's scratch with one file unfiled stays, listed.
        let other = WorkerRecord { task_id: "t2".into(), slug: "db".into(), badge: 2, ..WorkerRecord::default() };
        let osc = paths.worker_scratch("db");
        fs::create_dir_all(&osc).unwrap();
        fs::write(osc.join("a.wav"), "wav").unwrap();
        fs::write(osc.join("b.wav"), "wav two").unwrap();
        file_into(&g, &osc, "a.wav", "t2");
        let kept = retire_disk(&hive, &paths, &other);
        assert!(osc.join("b.wav").is_file() && kept.kept.iter().any(|k| k.starts_with("scratch: ")), "{:?}", kept.kept);

        let disk = retire_disk(&hive, &paths, &w);
        assert!(!sc.exists(), "covered: removed");
        assert!(disk.kept.is_empty(), "{:?}", disk.kept);
        assert!(g.get(&hero).unwrap().is_some_and(|a| g.file_path(&a).is_file()), "the gallery's copy outlives it");
        crate::hive::gallery::forget(&hive.id);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn the_coordinator_retires_only_a_worker_whose_agent_isnt_working() {
        for p in RETIRABLE {
            assert!(retire_refusal("#1 a", p, false).is_none(), "{p}");
        }
        assert!(retire_refusal("#1 a", worker::PHASE_LANDING, true).is_none(), "a held land is abandoned");
        assert!(retire_refusal("#1 a", worker::PHASE_RUNNING, true).is_some());
        for (p, says) in [
            (worker::PHASE_LAUNCHING, "hiveStopWorker"),
            (worker::PHASE_RUNNING, "hiveStopWorker"),
            (worker::PHASE_BLOCKED, "hiveStopWorker"),
            (worker::PHASE_LANDING, "landing"),
            (worker::PHASE_LANDED, "already"),
            (worker::PHASE_RETIRED, "already"),
            ("reviewing", "only a stopped"),
        ] {
            let e = retire_refusal("#1 a", p, false).unwrap();
            assert!(e.contains(says), "{p}: {e}");
        }
    }
}
