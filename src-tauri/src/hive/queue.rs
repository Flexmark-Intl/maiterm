//! The run queue (docs/hive.md §6.8): Rust holds the jobs, the resources they need and the
//! leases on the stack copies they depend on, and decides when each starts. Starting one is an
//! rpc to its window's stack store, which owns the PTYs (`stack.job.start`); its end is read here,
//! from the OSC 133 D the PTY reader already parses (`on_osc133`), never reported back by the
//! webview.
//!
//! One `std::sync::Mutex<Queue>`, like the fuses. `Queue` itself is plain data with an injected
//! clock and pressure gate, so the scheduling rules are unit-tested; the driver below it does the
//! I/O outside the lock (the profile and line, the rpc, the run log).
//!
//! - **Resources.** `cargo` (2), `tauri-dev`, `e2e` and `gpu` (1 each) are machine-wide pools
//!   across every Hive; a Hive's `run_queue.resources` can only lower its own share. Anything
//!   else is the Hive's own, capacity 1 unless its settings say otherwise, and a `migrate:`
//!   namespace is always one at a time (`migrate:{worker}` expands per worker). A job starts when
//!   every one of its needs has room, so jobs that don't compete run in parallel.
//! - **Order.** Higher priority first, then first queued.
//! - **Completion.** A D counts only after the B that began the job, on the PTY the window armed
//!   for it, and only once the shell's exit mark is beside it (a D;0 also needs the pass mark,
//!   `job_sandbox`): the job's own output can print any OSC 133 sequence, the shell's marks it
//!   can't write. A job whose shell shows no OSC 133 is `invalid`, never a pass; one whose PTY
//!   closes with no exit is interrupted (`on_pty_closed`), never left holding its resources.
//! - **Memory.** Under pressure nothing new starts (`pressure`); sampled only while a job is
//!   queued or running, so an idle maiTerm reads nothing.
//! - **Memory only.** After a restart the queue is empty and every job row is swept on load
//!   (`sweep_jobs`). The record that outlives it is `logs/runs.jsonl`, append-only.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::paths::HivePaths;
use super::pressure::Gate;
use crate::state::{AppData, AppState, Service};

pub const POOLS: [(&str, u32); 4] = [("cargo", 2), ("tauri-dev", 1), ("e2e", 1), ("gpu", 1)];
pub const MIGRATE: &str = "migrate:";

pub const QUEUED: &str = "queued";
pub const WAITING_WINDOW: &str = "waiting_window";
pub const RUNNING: &str = "running";
pub const DONE: &str = "done";
pub const INTERRUPTED: &str = "interrupted";
pub const INVALID: &str = "invalid";

pub const BY_HUMAN: &str = "human";
pub const BY_TIMEOUT: &str = "timeout";
pub const BY_RELEASE: &str = "release";
pub const BY_RESTART: &str = "restart";

pub const DEFAULT_TIMEOUT_S: u64 = 30 * 60;
pub const MAX_TIMEOUT_S: u64 = 4 * 3600;
/// Ended jobs kept in memory for lookups; the run log keeps every one.
const KEEP_ENDED: usize = 100;
/// What a job whose shell shows no OSC 133 is told (the window's guard says it first).
pub const NEEDS_INTEGRATION: &str = "needs shell integration";

pub const LEASE_FREED_EVENT: &str = "stack-lease-freed";
/// How long a copy whose held actions just went to their window (or whose jobs the human just
/// stopped anyway) takes no new lease, unless the window says it is done first: otherwise the
/// next queued job leases it again before the window's stop lands, and the stop is refused.
pub const LEASE_HANDOFF_MS: i64 = 30_000;

/// What the shell left in the job's run folder, outside the sandbox (`job_sandbox`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Marks {
    /// No exit mark: the line hasn't finished, so a D now is the job's own output.
    None,
    /// The line finished and the job failed.
    Exited,
    /// The line finished and `sandbox-exec` exited 0.
    Passed,
}

pub fn pool_capacity(name: &str, gate: Gate) -> Option<u32> {
    let cap = POOLS.iter().find(|(n, _)| *n == name)?.1;
    Some(if gate == Gate::Critical { 1 } else { cap })
}

/// `^[a-z0-9][a-z0-9._:-]{0,63}$`, after `{worker}` is filled.
pub fn valid_resource(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'.' | b'_' | b':' | b'-'))
}

/// A queued entry's needs for one worker: `{worker}` filled with its slug, any other brace
/// refused, each checked and listed once.
pub fn expand_needs(needs: &[String], worker: &str) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for n in needs {
        let e = n.trim().replace("{worker}", worker);
        if !valid_resource(&e) {
            return Err(format!("{n:?} isn't a resource name (lowercase letters, digits and . _ : -)"));
        }
        if !out.contains(&e) {
            out.push(e);
        }
    }
    Ok(out)
}

/// Pools are one machine-wide count; everything else is counted per Hive.
fn resource_key(hive_id: &str, name: &str) -> String {
    if POOLS.iter().any(|(n, _)| *n == name) {
        name.to_string()
    } else {
        format!("{hive_id}/{name}")
    }
}

/// A job id: `j-` and 8 hex digits. Job rows and their tabs are found by it after a restart.
pub fn is_job_id(s: &str) -> bool {
    s.len() == 10 && s.starts_with("j-") && s[2..].bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// What a caller (`hiveRun`, a worker's service verb) asks the queue to run, already validated
/// against a queued entry: everything but the command's own words comes from that entry.
#[derive(Debug, Clone, Default)]
pub struct JobSpec {
    pub hive_id: String,
    pub workspace_id: String,
    pub window: String,
    pub task_id: String,
    pub worker_slug: String,
    pub folder: String,
    /// The stack target the job runs on (`checkout:<worktree>`), for its row's label.
    pub target: String,
    pub argv: Vec<String>,
    /// The queued entry's prefix it matched.
    pub entry: String,
    /// As the entry names them; `{worker}` expands here.
    pub needs: Vec<String>,
    pub env: Vec<(String, String)>,
    pub extra_write: Vec<String>,
    /// Copy keys it leases while it runs.
    pub leases: Vec<String>,
    /// The Hive's `run_queue.resources` when it was queued.
    pub shares: BTreeMap<String, u32>,
    /// 0 is the default.
    pub timeout_s: u64,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub dirty: bool,
    pub priority: i64,
}

/// One job, as the run log records it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Job {
    pub id: String,
    pub hive_id: String,
    pub workspace_id: String,
    pub window: String,
    pub task_id: String,
    pub worker_slug: String,
    pub folder: String,
    pub target: String,
    pub argv: Vec<String>,
    pub entry: String,
    pub needs: Vec<String>,
    pub priority: i64,
    /// `queued` | `waiting_window` | `running` | `done` | `interrupted` | `invalid`.
    pub state: String,
    pub enqueued_at: i64,
    /// When its B was seen: the timeout runs from here.
    pub started_at: Option<i64>,
    pub ended_at: Option<i64>,
    pub exit_code: Option<i32>,
    /// `human` | `timeout` | `release` | `restart`.
    pub stopped_by: Option<String>,
    pub branch: Option<String>,
    pub commit: Option<String>,
    pub dirty: bool,
    pub tab_id: Option<String>,
    pub pty_id: Option<String>,
    /// The B that began it was seen on its PTY: only a D after it is its exit.
    pub began: bool,
    pub timeout_s: u64,
    pub ports: Vec<u16>,
    pub leases: Vec<String>,
    /// Why it is `invalid` or `interrupted`.
    pub error: Option<String>,
    #[serde(skip)]
    pub env: Vec<(String, String)>,
    #[serde(skip)]
    pub extra_write: Vec<String>,
    #[serde(skip)]
    shares: BTreeMap<String, u32>,
    #[serde(skip)]
    seq: u64,
}

impl Job {
    pub fn is_active(&self) -> bool {
        matches!(self.state.as_str(), QUEUED | WAITING_WINDOW | RUNNING)
    }

    /// Holds resources: started or being started.
    fn holds(&self) -> bool {
        matches!(self.state.as_str(), WAITING_WINDOW | RUNNING)
    }
}

/// A stop or retarget the human (Run when free) or an agent asked for while a job leased the
/// copy, carried out by the window once the lease is free.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HeldAction {
    pub id: String,
    pub copy_key: String,
    pub workspace_id: String,
    pub window: String,
    /// `stop` | `restart` | `retarget`.
    pub action: String,
    /// The target a retarget moves to.
    pub to: Option<String>,
    /// `human` | `agent`.
    pub by: String,
    /// The job it waited behind.
    pub job: String,
}

/// A job that just left the active set, and what the driver owes it.
#[derive(Debug, Clone, PartialEq)]
pub struct Ended {
    pub job: Job,
    /// Held actions whose copy no job leases any more.
    pub freed: Vec<HeldAction>,
    /// It may still be running in its tab: ^C it under the window's pid guard.
    pub stop: bool,
}

/// What an OSC 133 mark on a PTY means to the queue.
#[derive(Debug, Clone, PartialEq)]
pub enum Osc {
    None,
    Began(String),
    /// A D after the B: the job ends, once its pass mark is read (`complete`).
    Exit { job: String, code: Option<i32> },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LeaseHolder {
    pub id: String,
    pub entry: String,
    pub worker_slug: String,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct LeaseState {
    pub copy_key: String,
    /// The first job holding it, None when it's free.
    pub job: Option<LeaseHolder>,
    pub holders: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Move {
    Top,
    Up,
}

/// A finished job's tab, and the window that holds it.
#[derive(Debug, Clone, PartialEq)]
pub struct OldTab {
    pub window: String,
    pub workspace_id: String,
    pub tab_id: String,
}

#[derive(Default)]
pub struct Queue {
    jobs: Vec<Job>,
    /// resource key → the jobs holding it.
    holders: HashMap<String, Vec<String>>,
    /// copy key → the jobs leasing it. Several jobs on one copy share it (an e2e run each, on
    /// the same dev server); a stop or retarget waits for all of them.
    leases: HashMap<String, Vec<String>>,
    by_pty: HashMap<String, String>,
    held: Vec<HeldAction>,
    /// copy key → when its held actions went to their window: no new lease until it says done
    /// or `LEASE_HANDOFF_MS` passes.
    handoff: HashMap<String, i64>,
    /// task → its finished jobs' tabs, closed when its next job starts or it retires.
    old_tabs: HashMap<String, Vec<OldTab>>,
    seq: u64,
    gate: Gate,
}

fn new_id(taken: impl Fn(&str) -> bool) -> String {
    loop {
        let id = format!("j-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        if !taken(&id) {
            return id;
        }
    }
}

impl Queue {
    pub fn job(&self, id: &str) -> Option<&Job> {
        self.jobs.iter().find(|j| j.id == id)
    }

    fn job_mut(&mut self, id: &str) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }

    /// Anything queued, starting or running: the ticker runs only while this holds.
    pub fn active(&self) -> bool {
        self.jobs.iter().any(Job::is_active)
    }

    pub fn gate(&self) -> Gate {
        self.gate
    }

    /// True when it changed.
    pub fn set_gate(&mut self, gate: Gate) -> bool {
        std::mem::replace(&mut self.gate, gate) != gate
    }

    pub fn enqueue(&mut self, spec: JobSpec, now: i64) -> Result<Job, String> {
        if spec.argv.is_empty() || spec.argv[0].trim().is_empty() {
            return Err("A job needs a command".to_string());
        }
        if spec.task_id.is_empty() || spec.hive_id.is_empty() || spec.window.is_empty() {
            return Err("A job belongs to a Hive worker".to_string());
        }
        let needs = expand_needs(&spec.needs, &spec.worker_slug)?;
        let timeout_s = match spec.timeout_s {
            0 => DEFAULT_TIMEOUT_S,
            t => t.min(MAX_TIMEOUT_S),
        };
        self.seq += 1;
        let id = new_id(|id| self.jobs.iter().any(|j| j.id == id));
        let mut leases = spec.leases.clone();
        leases.dedup();
        let job = Job {
            id,
            hive_id: spec.hive_id,
            workspace_id: spec.workspace_id,
            window: spec.window,
            task_id: spec.task_id,
            worker_slug: spec.worker_slug,
            folder: spec.folder,
            target: spec.target,
            argv: spec.argv,
            entry: spec.entry,
            needs,
            priority: spec.priority,
            state: QUEUED.to_string(),
            enqueued_at: now,
            branch: spec.branch,
            commit: spec.commit,
            dirty: spec.dirty,
            timeout_s,
            leases,
            env: spec.env,
            extra_write: spec.extra_write,
            shares: spec.shares,
            seq: self.seq,
            ..Job::default()
        };
        self.jobs.push(job.clone());
        Ok(job)
    }

    /// Whether every need of `job` has room now.
    fn has_room(&self, job: &Job) -> bool {
        job.needs.iter().all(|n| {
            let key = resource_key(&job.hive_id, n);
            let held = self.holders.get(&key).map_or(&[][..], |v| v.as_slice());
            match pool_capacity(n, self.gate) {
                Some(cap) => {
                    let share = job.shares.get(n.as_str()).map_or(cap, |s| (*s).max(1)).min(cap);
                    let mine = held.iter().filter(|id| self.job(id).is_some_and(|o| o.hive_id == job.hive_id)).count();
                    (held.len() as u32) < cap && (mine as u32) < share
                }
                None => {
                    let cap = if n.starts_with(MIGRATE) { 1 } else { job.shares.get(n.as_str()).map_or(1, |s| (*s).max(1)) };
                    (held.len() as u32) < cap
                }
            }
        })
    }

    /// Queued jobs in the order they're offered a start.
    fn order(&self) -> Vec<usize> {
        let mut idx: Vec<usize> = (0..self.jobs.len()).filter(|&i| self.jobs[i].state == QUEUED).collect();
        idx.sort_by_key(|&i| {
            let j = &self.jobs[i];
            (std::cmp::Reverse(j.priority), j.enqueued_at, j.seq)
        });
        idx
    }

    /// Whether a copy takes no new lease: a stop or move waits on it (Run when free goes before
    /// the next run, never starved behind a stream of them), or one was just handed to its window.
    fn lease_blocked(&self, key: &str, now: i64) -> bool {
        self.held.iter().any(|h| h.copy_key == key) || self.handoff.get(key).is_some_and(|t| now - t < LEASE_HANDOFF_MS)
    }

    /// Every queued job whose needs all have room takes them and its leases, and moves to
    /// `waiting_window`: the driver starts each, outside the lock. Under pressure, none.
    pub fn schedule(&mut self, now: i64) -> Vec<Job> {
        self.handoff.retain(|_, t| now - *t < LEASE_HANDOFF_MS);
        if !self.gate.starts() {
            return Vec::new();
        }
        let mut started = Vec::new();
        for i in self.order() {
            if !self.has_room(&self.jobs[i]) || self.jobs[i].leases.iter().any(|k| self.lease_blocked(k, now)) {
                continue;
            }
            let (id, hive) = (self.jobs[i].id.clone(), self.jobs[i].hive_id.clone());
            for n in self.jobs[i].needs.clone() {
                self.holders.entry(resource_key(&hive, &n)).or_default().push(id.clone());
            }
            for k in self.jobs[i].leases.clone() {
                self.leases.entry(k).or_default().push(id.clone());
            }
            self.jobs[i].state = WAITING_WINDOW.to_string();
            started.push(self.jobs[i].clone());
        }
        started
    }

    /// Ports the driver booked for the job, before its line was typed.
    pub fn set_ports(&mut self, id: &str, ports: Vec<u16>) {
        if let Some(j) = self.job_mut(id) {
            j.ports = ports;
        }
    }

    /// The window has a shell for the job at its prompt and is about to type: from here the
    /// PTY's next B is the job's. Refused unless the job is still waiting on that window, so a
    /// window that wakes after its start was given up types nothing.
    pub fn arm(&mut self, id: &str, tab_id: &str, pty_id: &str) -> Result<(), String> {
        let j = self.jobs.iter_mut().find(|j| j.id == id).ok_or_else(|| format!("No job {id} is waiting to start"))?;
        if j.state != WAITING_WINDOW || j.pty_id.is_some() {
            return Err(format!("Job {id} isn't waiting to start (it is {}), so nothing is typed", j.state));
        }
        j.tab_id = Some(tab_id.to_string());
        j.pty_id = Some(pty_id.to_string());
        self.by_pty.insert(pty_id.to_string(), id.to_string());
        Ok(())
    }

    /// An OSC 133 mark on `pty`. Cheap: a map lookup for every PTY that runs no job.
    pub fn osc(&mut self, pty: &str, cmd: char, code: Option<i32>, now: i64) -> Osc {
        let Some(id) = self.by_pty.get(pty).cloned() else { return Osc::None };
        let Some(j) = self.job_mut(&id) else { return Osc::None };
        match cmd {
            'B' | 'C' if j.state == WAITING_WINDOW && !j.began => {
                j.began = true;
                j.state = RUNNING.to_string();
                j.started_at = Some(now);
                Osc::Began(id)
            }
            'D' if j.began => Osc::Exit { job: id, code },
            _ => Osc::None,
        }
    }

    /// The window answered its start: the B was seen (`confirmed`). The B normally reached
    /// `osc` first; this covers a reader that lost the race.
    pub fn confirm(&mut self, id: &str, tab_id: &str, now: i64) {
        if let Some(j) = self.job_mut(id) {
            if j.state == WAITING_WINDOW && j.pty_id.is_some() {
                j.began = true;
                j.state = RUNNING.to_string();
                j.started_at.get_or_insert(now);
            }
            if j.tab_id.is_none() && !tab_id.is_empty() {
                j.tab_id = Some(tab_id.to_string());
            }
        }
    }

    /// A D after the job's B, with what the shell left beside it. No exit mark: the line is
    /// still running and the job printed the D, ignored. A D;0 with no pass mark is the job's
    /// too (the shell's own exit failed and its D comes after). Otherwise the job ends, and a
    /// pass is exit 0 whatever the D said. On an interrupted job (a ^C cuts the line before its
    /// marks) any D only records the code and frees the PTY.
    pub fn complete(&mut self, id: &str, code: Option<i32>, marks: Marks, now: i64) -> Option<Ended> {
        let j = self.job_mut(id)?;
        if !j.began {
            return None;
        }
        if j.state == INTERRUPTED {
            j.exit_code = j.exit_code.or(code);
            let pty = j.pty_id.clone();
            if let Some(p) = pty {
                self.by_pty.remove(&p);
            }
            return None;
        }
        if j.state != RUNNING {
            return None;
        }
        let code = match marks {
            Marks::None => {
                log::warn!("hive queue: job {id} showed a D {code:?} before its line finished; not its exit");
                return None;
            }
            Marks::Exited if code == Some(0) => {
                log::warn!("hive queue: job {id} showed D;0 with no pass mark; not its exit");
                return None;
            }
            Marks::Exited => code,
            Marks::Passed => Some(0),
        };
        j.state = DONE.to_string();
        j.exit_code = code;
        j.ended_at = Some(now);
        Some(self.end(id, false, now))
    }

    /// The job's PTY closed (its shell died, or its tab was closed) with no exit seen: nothing
    /// will ever send its D, so it ends here rather than holding its resources to the timeout.
    /// Never a pass, and not anyone's stop.
    pub fn pty_closed(&mut self, pty: &str, now: i64) -> Option<Ended> {
        let id = self.by_pty.remove(pty)?;
        let j = self.job_mut(&id)?;
        if !j.holds() {
            return None;
        }
        if j.began {
            j.state = INTERRUPTED.to_string();
            j.error = Some("its tab's shell ended before the job's exit was seen".to_string());
        } else {
            j.state = INVALID.to_string();
            j.error = Some("its tab's shell ended before the job began".to_string());
        }
        j.ended_at = Some(now);
        Some(self.end(&id, false, now))
    }

    /// A start that failed before the job ran: the window refused (no shell integration, a
    /// shell that never ran it), didn't answer, or the line couldn't be built. Never a pass. An
    /// armed one may have had its line typed after all: it is ^C'd like an interrupted job.
    pub fn fail_start(&mut self, id: &str, error: &str, now: i64) -> Option<Ended> {
        let j = self.job_mut(id)?;
        if !j.holds() || j.began {
            return None;
        }
        let stop = j.pty_id.is_some();
        j.state = INVALID.to_string();
        j.error = Some(error.to_string());
        j.ended_at = Some(now);
        Some(self.end(id, stop, now))
    }

    /// Stop a job: dropped if queued, interrupted (never a pass) if started.
    pub fn interrupt(&mut self, id: &str, by: &str, why: &str, now: i64) -> Option<Ended> {
        let j = self.job_mut(id)?;
        if !j.is_active() {
            return None;
        }
        let stop = j.holds() && j.pty_id.is_some();
        j.state = INTERRUPTED.to_string();
        j.stopped_by = Some(by.to_string());
        j.error = Some(why.to_string());
        j.ended_at = Some(now);
        Some(self.end(id, stop, now))
    }

    /// Running jobs past their timeout.
    pub fn timed_out(&self, now: i64) -> Vec<String> {
        self.jobs
            .iter()
            .filter(|j| j.state == RUNNING && j.started_at.is_some_and(|t| now - t >= (j.timeout_s as i64) * 1000))
            .map(|j| j.id.clone())
            .collect()
    }

    /// Everything a worker has in the queue: queued jobs dropped, started ones interrupted.
    pub fn release_worker(&mut self, task_id: &str, by: &str, why: &str, now: i64) -> Vec<Ended> {
        let ids: Vec<String> = self.jobs.iter().filter(|j| j.task_id == task_id && j.is_active()).map(|j| j.id.clone()).collect();
        ids.iter().filter_map(|id| self.interrupt(id, by, why, now)).collect()
    }

    /// A worker retired: what it has in the queue is released, and every job tab it has left
    /// (the ones just ended among them) is taken, to close.
    pub fn retire_worker(&mut self, task_id: &str, why: &str, now: i64) -> (Vec<Ended>, Vec<OldTab>) {
        let ended = self.release_worker(task_id, BY_RELEASE, why, now);
        (ended, self.take_old_tabs(task_id))
    }

    /// Resources and leases go back; held actions whose copy is now free come out, and that
    /// copy takes no new lease while its window acts on them.
    fn end(&mut self, id: &str, stop: bool, now: i64) -> Ended {
        for v in self.holders.values_mut() {
            v.retain(|j| j != id);
        }
        self.holders.retain(|_, v| !v.is_empty());
        let mut freed_keys: Vec<String> = Vec::new();
        for (k, v) in self.leases.iter_mut() {
            if v.iter().any(|j| j == id) {
                v.retain(|j| j != id);
                if v.is_empty() {
                    freed_keys.push(k.clone());
                }
            }
        }
        self.leases.retain(|_, v| !v.is_empty());
        let (freed, kept): (Vec<HeldAction>, Vec<HeldAction>) = std::mem::take(&mut self.held).into_iter().partition(|h| freed_keys.contains(&h.copy_key));
        self.held = kept;
        for h in &freed {
            self.handoff.insert(h.copy_key.clone(), now);
        }
        let job = self.job(id).cloned().unwrap_or_default();
        if !stop {
            if let Some(p) = &job.pty_id {
                self.by_pty.remove(p);
            }
        }
        if let Some(t) = &job.tab_id {
            let tabs = self.old_tabs.entry(job.task_id.clone()).or_default();
            if !tabs.iter().any(|o| &o.tab_id == t) {
                tabs.push(OldTab { window: job.window.clone(), workspace_id: job.workspace_id.clone(), tab_id: t.clone() });
            }
        }
        self.prune();
        Ended { job, freed, stop }
    }

    /// Ended jobs beyond `KEEP_ENDED` go, oldest first, unless a PTY still maps to one (an
    /// interrupted job whose D hasn't come).
    fn prune(&mut self) {
        let ended = self.jobs.iter().filter(|j| !j.is_active()).count();
        if ended <= KEEP_ENDED {
            return;
        }
        let mapped: Vec<String> = self.by_pty.values().cloned().collect();
        let mut drop = ended - KEEP_ENDED;
        self.jobs.retain(|j| {
            if drop > 0 && !j.is_active() && !mapped.contains(&j.id) {
                drop -= 1;
                return false;
            }
            true
        });
    }

    /// A worker's finished job tabs, taken: its next job is starting, or it retired, so they close.
    pub fn take_old_tabs(&mut self, task_id: &str) -> Vec<OldTab> {
        self.old_tabs.remove(task_id).unwrap_or_default()
    }

    pub fn lease_state(&self, keys: &[String]) -> Vec<LeaseState> {
        keys.iter()
            .map(|k| {
                let ids = self.leases.get(k).cloned().unwrap_or_default();
                let job = ids.first().and_then(|id| self.job(id)).map(|j| LeaseHolder {
                    id: j.id.clone(),
                    entry: j.entry.clone(),
                    worker_slug: j.worker_slug.clone(),
                    state: j.state.clone(),
                });
                LeaseState { copy_key: k.clone(), job, holders: ids.len() }
            })
            .collect()
    }

    /// Hold a stop or retarget until the copy is free. Refused when no job leases it: the
    /// caller goes ahead now.
    pub fn hold(&mut self, mut action: HeldAction) -> Result<HeldAction, String> {
        let Some(job) = self.leases.get(&action.copy_key).and_then(|v| v.first()).cloned() else {
            return Err("No job holds that service now, so nothing waits".to_string());
        };
        action.id = format!("h-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
        action.job = job;
        // One wait per copy and action: asking again replaces it.
        self.held.retain(|h| !(h.copy_key == action.copy_key && h.workspace_id == action.workspace_id && h.action == action.action));
        self.held.push(action.clone());
        Ok(action)
    }

    /// The human's Stop anyway: every job leasing the copy is interrupted (never a receipt), and
    /// the copy takes no new lease until their stop has run.
    pub fn override_lease(&mut self, key: &str, now: i64) -> Vec<Ended> {
        let ids = self.leases.get(key).cloned().unwrap_or_default();
        self.handoff.insert(key.to_string(), now);
        ids.iter().filter_map(|id| self.interrupt(id, BY_HUMAN, "the human stopped a service it was using", now)).collect()
    }

    /// The window carried out what waited on the copy: it may be leased again.
    pub fn lease_done(&mut self, key: &str) {
        self.handoff.remove(key);
    }

    /// The coordinator's `hiveRunPriority`: to the front, or one place up.
    pub fn move_job(&mut self, id: &str, mv: Move) -> Result<(), String> {
        let order = self.order();
        let pos = order.iter().position(|&i| self.jobs[i].id == id).ok_or_else(|| format!("Job {id} isn't queued"))?;
        if pos == 0 {
            return Ok(());
        }
        let me = order[pos];
        match mv {
            // The first queued job holds the highest priority there is.
            Move::Top => self.jobs[me].priority = self.jobs[order[0]].priority + 1,
            Move::Up => {
                let other = order[pos - 1];
                let (a, b) = ((self.jobs[me].priority, self.jobs[me].enqueued_at, self.jobs[me].seq), (self.jobs[other].priority, self.jobs[other].enqueued_at, self.jobs[other].seq));
                (self.jobs[me].priority, self.jobs[me].enqueued_at, self.jobs[me].seq) = b;
                (self.jobs[other].priority, self.jobs[other].enqueued_at, self.jobs[other].seq) = a;
            }
        }
        Ok(())
    }
}

// ── The run log ──

/// Every Hive's run log, read once per Hive and kept current by `append_run`: the in-memory
/// index `checks` receipts are verified against (§7.7).
static RUN_INDEX: LazyLock<Mutex<HashMap<PathBuf, HashMap<String, Job>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn read_index(path: &Path) -> HashMap<String, Job> {
    let mut out = HashMap::new();
    let Ok(text) = std::fs::read_to_string(path) else { return out };
    for line in text.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else { continue };
        if v.get("type").and_then(Value::as_str) != Some("run") {
            continue;
        }
        if let Ok(j) = serde_json::from_value::<Job>(v) {
            out.insert(j.id.clone(), j);
        }
    }
    out
}

/// Append one finished job to its Hive's `logs/runs.jsonl`. Blocks on the disk.
pub fn append_run(paths: &HivePaths, job: &Job) -> Result<(), String> {
    let path = paths.run_log();
    let mut v = serde_json::to_value(job).map_err(|e| e.to_string())?;
    if let Value::Object(m) = &mut v {
        m.insert("type".into(), json!("run"));
    }
    append_line(&path, &v)?;
    let mut idx = RUN_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(i) = idx.get_mut(&path) {
        i.insert(job.id.clone(), job.clone());
    }
    Ok(())
}

pub(crate) fn append_line(path: &Path, v: &Value) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    writeln!(f, "{v}").map_err(|e| format!("{}: {e}", path.display()))
}

/// A finished job as the run log has it. Reads the log the first time for each Hive.
pub fn find_run(paths: &HivePaths, job_id: &str) -> Option<Job> {
    let path = paths.run_log();
    let mut idx = RUN_INDEX.lock().unwrap_or_else(|e| e.into_inner());
    idx.entry(path.clone()).or_insert_with(|| read_index(&path)).get(job_id).cloned()
}

// ── Load-time sweep and create-time defaults ──

/// Job rows don't outlive the run that made them, and after a restart none runs: every row and
/// every tab bound to a job id with no row goes. Run on load, before anything reads the stack.
pub fn sweep_jobs(data: &mut AppData) -> usize {
    let mut n = 0;
    for ws in data.windows.iter_mut().flat_map(|w| w.workspaces.iter_mut()) {
        let before = ws.stack.len();
        ws.stack.retain(|s| !s.is_job());
        n += before - ws.stack.len();
        let rows: Vec<String> = ws.stack.iter().map(|s| s.id.clone()).collect();
        for pane in ws.panes.iter_mut() {
            let before = pane.tabs.len();
            pane.tabs.retain(|t| !t.service_id.as_deref().is_some_and(|id| is_job_id(id) && !rows.iter().any(|r| r == id)));
            n += before - pane.tabs.len();
            if pane.active_tab_id.as_ref().is_some_and(|a| !pane.tabs.iter().any(|t| &t.id == a)) {
                pane.active_tab_id = pane.tabs.iter().find(|t| !t.is_strip_hidden()).map(|t| t.id.clone());
            }
        }
    }
    n
}

/// The queued commands a new Hive starts with, read from each git folder (§6.8): `cargo test`
/// and `cargo build` need `cargo`; `npm run tauri:dev` (and `:isolated`) need `tauri-dev` and
/// `cargo`; Playwright needs `e2e`. The human edits them in Hive settings. Reads the disk.
pub fn detect_commands(folders: &[crate::hive::record::HiveFolder]) -> crate::hive::record::RunQueueSettings {
    use crate::hive::record::QueuedCommand;
    let mut commands: Vec<QueuedCommand> = Vec::new();
    let mut add = |folder: &str, prefix: &str, needs: &[&str]| {
        commands.push(QueuedCommand {
            folder: folder.to_string(),
            prefix: prefix.to_string(),
            needs: needs.iter().map(|s| s.to_string()).collect(),
            ..QueuedCommand::default()
        });
    };
    for f in folders.iter().filter(|f| f.is_git()) {
        let root = Path::new(&f.path);
        if has_cargo(root) {
            add(&f.name, "cargo test", &["cargo"]);
            add(&f.name, "cargo build", &["cargo"]);
        }
        let pkg: Value = std::fs::read(root.join("package.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or(Value::Null);
        for script in ["tauri:dev", "tauri:dev:isolated"] {
            if pkg.get("scripts").and_then(|s| s.get(script)).is_some() {
                add(&f.name, &format!("npm run {script}"), &["tauri-dev", "cargo"]);
            }
        }
        let has_dep = |name: &str| ["dependencies", "devDependencies"].iter().any(|k| pkg.get(k).and_then(|d| d.get(name)).is_some());
        let config = ["ts", "js", "mjs", "cjs"].iter().any(|e| root.join(format!("playwright.config.{e}")).is_file());
        if has_dep("@playwright/test") || config {
            add(&f.name, "npx playwright test", &["e2e"]);
        }
    }
    crate::hive::record::RunQueueSettings { commands, resources: BTreeMap::new() }
}

/// A Cargo.toml at the top, or one folder down (`src-tauri/`), outside build and vendor folders.
fn has_cargo(root: &Path) -> bool {
    if root.join("Cargo.toml").is_file() {
        return true;
    }
    let Ok(rd) = std::fs::read_dir(root) else { return false };
    rd.flatten().any(|e| {
        let name = e.file_name().to_string_lossy().to_string();
        !name.starts_with('.') && !["node_modules", "target", "vendor"].contains(&name.as_str()) && e.path().join("Cargo.toml").is_file()
    })
}

// ── The driver: the app's queue, its I/O, its window rpc ──

static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(|| Mutex::new(Queue::default()));

fn queue() -> MutexGuard<'static, Queue> {
    QUEUE.lock().unwrap_or_else(|e| e.into_inner())
}

struct Driver {
    app: Arc<AppState>,
    handle: tauri::AppHandle,
}

static DRIVER: OnceLock<Driver> = OnceLock::new();
static TICKING: AtomicBool = AtomicBool::new(false);

/// At setup: the queue can start jobs from here on.
pub fn install(app: Arc<AppState>, handle: tauri::AppHandle) {
    let _ = DRIVER.set(Driver { app, handle });
}

/// Queue a job and offer it a start. The caller has matched it against a queued entry.
pub fn enqueue(spec: JobSpec) -> Result<Job, String> {
    let job = queue().enqueue(spec, super::fuses::now_ms())?;
    log::info!("hive queue: {} queued for {} ({})", job.id, job.worker_slug, job.argv.join(" "));
    kick();
    Ok(job)
}

/// Every job of a worker that stopped, crashed, retired or was suspended (§6.8).
pub fn release_worker(task_id: &str, why: &str) -> usize {
    let ended = queue().release_worker(task_id, BY_RELEASE, why, super::fuses::now_ms());
    let n = ended.len();
    for e in ended {
        spawn_finish(e);
    }
    n
}

/// A worker retired: its jobs are released as above, and every job tab it left closes, the
/// ones still running included (their jobs were just interrupted; closing the tab ends them).
pub fn retire_worker(task_id: &str, why: &str) -> usize {
    let (ended, tabs) = queue().retire_worker(task_id, why, super::fuses::now_ms());
    let n = ended.len();
    let mut by_window: BTreeMap<(String, String), (Vec<String>, Vec<String>)> = BTreeMap::new();
    for t in tabs {
        by_window.entry((t.window, t.workspace_id)).or_default().0.push(t.tab_id);
    }
    for e in &ended {
        by_window.entry((e.job.window.clone(), e.job.workspace_id.clone())).or_default().1.push(e.job.id.clone());
    }
    for e in ended {
        spawn_finish(e);
    }
    if let Some(d) = DRIVER.get() {
        for ((window, workspace_id), (tab_ids, job_ids)) in by_window {
            cleanup(d, &window, &workspace_id, json!({ "tab_ids": tab_ids, "job_ids": job_ids, "force": true }));
        }
    }
    n
}

pub fn job(id: &str) -> Option<Job> {
    queue().job(id).cloned()
}

/// Called by the PTY's teardown, however it ended: a job still mapped to it ends now.
pub fn on_pty_closed(pty_id: &str) {
    let ended = queue().pty_closed(pty_id, super::fuses::now_ms());
    if let Some(e) = ended {
        log::warn!("hive queue: {} lost its tab's shell: {}", e.job.id, e.job.error.as_deref().unwrap_or_default());
        spawn_finish(e);
    }
}

/// Called by the PTY reader on every OSC 133 mark: for a PTY that runs no job, one lookup.
pub fn on_osc133(pty_id: &str, cmd: char, exit_code: Option<i32>) {
    if !matches!(cmd, 'B' | 'C' | 'D') {
        return;
    }
    let now = super::fuses::now_ms();
    let hit = queue().osc(pty_id, cmd, exit_code, now);
    match hit {
        Osc::None => {}
        Osc::Began(id) => log::info!("hive queue: {id} began on pty {pty_id}"),
        Osc::Exit { job, code } => {
            // The marks are files: read them off the reader thread. The shell writes the pass
            // mark before the exit mark, so with the exit mark there the pass mark is final.
            tauri::async_runtime::spawn_blocking(move || {
                let dir = queue().job(&job).and_then(|j| super::paths::hive_root(&j.hive_id).map(|r| HivePaths::new(r).run(&j.id)));
                let marks = match dir {
                    Some(d) if d.join(super::job_sandbox::EXIT_MARK).is_file() => {
                        if d.join(super::job_sandbox::PASS_MARK).is_file() {
                            Marks::Passed
                        } else {
                            Marks::Exited
                        }
                    }
                    _ => Marks::None,
                };
                let ended = queue().complete(&job, code, marks, super::fuses::now_ms());
                if let Some(e) = ended {
                    log::info!("hive queue: {job} exited {code:?}");
                    spawn_finish(e);
                }
            });
        }
    }
}

fn kick() {
    let Some(d) = DRIVER.get() else { return };
    ensure_ticker(d);
    tauri::async_runtime::spawn(async move { schedule_and_start(d).await });
}

async fn schedule_and_start(d: &'static Driver) {
    // A fresh reading (or one under 5 s old) before anything starts; never on a timer of its own.
    let gate = tauri::async_runtime::spawn_blocking(|| super::pressure::gate_of(&super::pressure::recent())).await.unwrap_or_default();
    let (changed, started) = {
        let mut q = queue();
        let changed = q.set_gate(gate);
        (changed, q.schedule(super::fuses::now_ms()))
    };
    if changed {
        emit_throttled(d, gate);
    }
    for job in started {
        tauri::async_runtime::spawn(start_job(d, job));
    }
}

/// While a job is queued or running: every `SAMPLE_EVERY`, pressure, timeouts, then a schedule.
/// It ends itself once the queue is empty, so nothing ticks for an idle maiTerm.
fn ensure_ticker(d: &'static Driver) {
    if TICKING.swap(true, Ordering::SeqCst) {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(super::pressure::SAMPLE_EVERY).await;
            if !queue().active() {
                TICKING.store(false, Ordering::SeqCst);
                // A job queued between the check and the store found TICKING set; look again.
                if !queue().active() || TICKING.swap(true, Ordering::SeqCst) {
                    return;
                }
            }
            let now = super::fuses::now_ms();
            let ended: Vec<Ended> = {
                let mut q = queue();
                let ids = q.timed_out(now);
                ids.iter().filter_map(|id| q.interrupt(id, BY_TIMEOUT, "it ran past its timeout", now)).collect()
            };
            for e in ended {
                spawn_finish(e);
            }
            schedule_and_start(d).await;
        }
    });
}

fn emit_throttled(d: &Driver, gate: Gate) {
    use tauri::Emitter;
    log::info!("hive queue: memory gate {gate:?}");
    let hives: Vec<(String, String)> = {
        let data = d.app.app_data.read();
        data.windows.iter().flat_map(|w| w.workspaces.iter().filter(|ws| ws.hive.is_some()).map(move |ws| (w.label.clone(), ws.id.clone()))).collect()
    };
    for (label, ws) in hives {
        let payload = json!({ "workspace_id": ws, "throttled": gate.throttled() });
        if let Err(e) = d.handle.emit_to(&label, super::tools::WORKERS_CHANGED_EVENT, payload) {
            log::warn!("hive queue: throttled state not sent to window {label}: {e}");
        }
    }
}

/// The job's row, profile and line, worked out off the lock. Blocks on the disk and git.
struct Prepared {
    line: String,
    row: Service,
    ports: Vec<u16>,
}

fn prepare(app: &AppState, job: &Job) -> Result<Prepared, String> {
    use crate::stack::placeholders::{check, fill, Needs, Values};
    super::job_sandbox::supported()?;
    let (hive, stack, prefs, forbidden) = {
        let data = app.app_data.read();
        let ws = data
            .window(&job.window)
            .and_then(|w| w.workspaces.iter().find(|ws| ws.id == job.workspace_id))
            .ok_or("The Hive's workspace is gone")?;
        let hive = ws.hive.clone().filter(|h| h.id == job.hive_id).ok_or("The workspace is no longer this Hive")?;
        let forbidden = crate::stack::ports::forbidden(*app.mcp_port.read(), app.mailink_info.read().as_ref().map(|(_, p)| *p));
        (hive, ws.stack.clone(), data.preferences.clone(), forbidden)
    };
    let paths = HivePaths::new(super::paths::hive_root(&hive.id).ok_or("The Hive's folder can't be named")?);
    let w = super::worker::read_worker(&paths.task(&job.task_id))?;
    let folder = w
        .folders
        .iter()
        .find(|f| f.name == job.folder && f.worktree.is_some())
        .ok_or_else(|| format!("{} has no worktree of {}", w.slug, job.folder))?;
    let cwd = PathBuf::from(folder.worktree.clone().unwrap_or_default());
    let bin = super::paths::hive_bin_dir().ok_or("Couldn't find maiTerm's data folder")?;
    let ctx = super::settings::WorkerContext::gather(&hive, &paths, &w);
    let lists = super::settings::worker_write_lists(&hive, &w, &paths, &ctx, &bin)?;

    // Placeholders in the entry's env and extra paths, as data: `{checkout}` is fine in both
    // (a path, which the profile and the quoting refuse if it can't be named).
    let as_values: Vec<(String, String)> = job.env.iter().cloned().chain(job.extra_write.iter().map(|p| (String::new(), p.clone()))).collect();
    let needs = Needs::of("", &as_values);
    let mut values = Values {
        branch: folder.branch.clone(),
        worker: Some(w.slug.clone()),
        checkout: Some(cwd.to_string_lossy().to_string()),
        ..Values::default()
    };
    let mut reach: Vec<u16> = Vec::new();
    for name in &needs.port_of {
        let want = Service::normalize_name(name);
        let s = stack.iter().filter(|s| !s.is_job()).find(|s| Service::normalize_name(&s.name) == want);
        let port = s.and_then(|s| match &s.shared_from {
            Some(r) => crate::stack::shared_source(&app.app_data.read(), r).and_then(|(_, src)| src.port),
            None => s.port,
        });
        match port {
            Some(p) => {
                values.port_of.push((name.clone(), p));
                reach.push(p);
            }
            None => return Err(format!("{{port:{name}}}: no service called {name} has a port yet")),
        }
    }
    check(&needs, &values)?;
    let n = if needs.port2 { 2 } else if needs.port { 1 } else { 0 };
    let ports = if n > 0 { crate::stack::ports::take(&job.id, n, crate::stack::ports::DEFAULT_BASE, false, &forbidden)? } else { Vec::new() };
    let release = |e: String| {
        crate::stack::ports::release(&job.id);
        e
    };
    values.port = ports.first().copied();
    values.port2 = ports.get(1).copied();
    let env: Vec<(String, String)> = job.env.iter().map(|(k, v)| Ok((k.clone(), fill(v, &values)?))).collect::<Result<_, String>>().map_err(release)?;
    let home = dirs::home_dir();
    let mut extra: Vec<PathBuf> = Vec::new();
    for p in &job.extra_write {
        let filled = fill(p, &values).map_err(release)?;
        let path = match (filled.strip_prefix("~/"), &home) {
            (Some(rest), Some(h)) => h.join(rest),
            _ => PathBuf::from(&filled),
        };
        if !path.is_absolute() {
            return Err(release(format!("extra write path {p:?} isn't absolute")));
        }
        extra.push(path);
    }
    // The leased copies' ports: what the port book holds for them, and the own folder's port.
    for key in &job.leases {
        reach.extend(crate::stack::ports::held(key));
        if let Some(s) = stack.iter().find(|s| s.id == *key) {
            reach.extend(s.port);
        }
    }

    let run_dir = paths.run(&job.id);
    let job_tmp = paths.worker_job_tmp(&w.slug);
    for d in [&run_dir, &job_tmp] {
        std::fs::create_dir_all(d).map_err(|e| release(format!("{}: {e}", d.display())))?;
    }
    let mark = run_dir.join(super::job_sandbox::PASS_MARK);
    let exit_mark = run_dir.join(super::job_sandbox::EXIT_MARK);
    for m in [&mark, &exit_mark] {
        let _ = std::fs::remove_file(m);
    }
    // Every path the job may write is the worker's to plant a link in: resolved only above it.
    let writable: Vec<PathBuf> = lists.allow.iter().cloned().chain([job_tmp.clone()]).chain(extra.iter().cloned()).collect();
    let named = |p: &Path| super::job_sandbox::profile_path(p, &writable);
    let spec = (|| -> Result<super::job_sandbox::ProfileSpec, String> {
        Ok(super::job_sandbox::ProfileSpec {
            allow: writable.iter().map(|p| named(p)).collect::<Result<_, _>>()?,
            deny: lists.deny.iter().map(|d| Ok(super::settings::DenyPath { path: named(&d.path)?, dir: d.dir })).collect::<Result<_, String>>()?,
            own_ports: ports.clone(),
            reach_ports: reach,
        })
    })()
    .map_err(release)?;
    let profile = super::job_sandbox::profile(&spec).map_err(release)?;
    let profile_path = run_dir.join(super::job_sandbox::PROFILE_FILE);
    super::launch::write_bytes(&profile_path, profile.as_bytes()).map_err(release)?;

    let s = |p: &Path| p.to_string_lossy().to_string();
    let mut line_env = vec![("TMPDIR".to_string(), s(&job_tmp))];
    if super::worktree::worker_uses_own_cargo_target(&hive, &w) {
        line_env.push(("CARGO_TARGET_DIR".to_string(), s(&paths.worker_cargo_target(&w.slug))));
    }
    line_env.push(("CARGO_NET_OFFLINE".to_string(), "true".to_string()));
    for (k, v) in env {
        line_env.retain(|(o, _)| *o != k);
        line_env.push((k, v));
    }
    let line = super::job_sandbox::job_line(&super::job_sandbox::LineSpec {
        cwd: cwd.clone(),
        unset: super::launch::unset_vars(&prefs),
        env: line_env,
        profile: profile_path,
        argv: job.argv.clone(),
        pass_mark: mark,
        exit_mark,
    })
    .map_err(release)?;
    let now = crate::commands::workspace::iso_now();
    let row = Service {
        id: job.id.clone(),
        name: format!("{} · {}", job.entry, w.slug),
        normalized_name: String::new(),
        command: job.argv.join(" "),
        cwd: s(&cwd),
        env: Vec::new(),
        ssh_command: None,
        auto_start: false,
        was_running: false,
        restart: "never".to_string(),
        ready_pattern: None,
        port: ports.first().copied(),
        url: None,
        // Never human-defined: a worker asked for it.
        origin: "agent".to_string(),
        agent_edited: true,
        running_targets: Vec::new(),
        shared_from: None,
        kind: crate::state::workspace::JOB_KIND.to_string(),
        created_at: now.clone(),
        updated_at: now,
    };
    Ok(Prepared { line, row, ports })
}

fn set_row(app: &AppState, job: &Job, row: Option<Service>) -> bool {
    let mut data = app.app_data.write();
    let Some(ws) = data.window_mut(&job.window).and_then(|w| w.workspaces.iter_mut().find(|ws| ws.id == job.workspace_id)) else {
        return false;
    };
    ws.stack.retain(|s| s.id != job.id);
    if let Some(r) = row {
        ws.stack.push(r);
    }
    true
}

async fn start_job(d: &'static Driver, job: Job) {
    use super::rpc::Outcome;
    let now = super::fuses::now_ms;
    let (app, j) = (d.app.clone(), job.clone());
    let prepared = tauri::async_runtime::spawn_blocking(move || prepare(&app, &j)).await.unwrap_or_else(|e| Err(e.to_string()));
    let p = match prepared {
        Ok(p) => p,
        Err(e) => {
            log::warn!("hive queue: {} not started: {e}", job.id);
            let ended = queue().fail_start(&job.id, &e, now());
            if let Some(e) = ended {
                spawn_finish(e);
            }
            return;
        }
    };
    queue().set_ports(&job.id, p.ports.clone());
    // Rust's row first: `create_tab` binds the job's tab to it, which a Hive allows only for a
    // row of its own stack (`refuse_tab_create`). Never saved: a job row is swept on load.
    if !set_row(&d.app, &job, Some(p.row.clone())) {
        if let Some(e) = queue().fail_start(&job.id, "The Hive's workspace is gone", now()) {
            spawn_finish(e);
        }
        return;
    }
    let old = queue().take_old_tabs(&job.task_id);
    if !old.is_empty() {
        let tabs: Vec<String> = old.into_iter().map(|t| t.tab_id).collect();
        cleanup(d, &job.window, &job.workspace_id, json!({ "tab_ids": tabs }));
    }
    let args = json!({
        "workspace_id": job.workspace_id,
        "job_id": job.id,
        "row": p.row,
        "target": job.target,
        "line": p.line,
    });
    let outcome = crate::mailink::rpc::request_on(
        &d.app,
        Some(&d.handle),
        super::rpc::EVENT,
        &job.window,
        super::rpc::VERB_JOB_START,
        args,
        super::rpc::JOB_START_TIMEOUT,
    )
    .await;
    let failed = match outcome {
        Outcome::Answered(v) => match v.get("error").and_then(Value::as_str) {
            Some(err) => Some(err.to_string()),
            None if v.get("confirmed").and_then(Value::as_bool) == Some(true) => {
                let tab = v.get("tab_id").and_then(Value::as_str).unwrap_or_default().to_string();
                queue().confirm(&job.id, &tab, now());
                None
            }
            None => Some("the shell never ran the job's line".to_string()),
        },
        other => Some(super::rpc::accepted(other).err().unwrap_or_else(|| "the window didn't answer".to_string())),
    };
    if let Some(err) = failed {
        let ended = queue().fail_start(&job.id, &err, now());
        match ended {
            Some(e) => {
                log::warn!("hive queue: {} invalid: {err}", job.id);
                spawn_finish(e);
                // The tab it made, if any, goes. A window that wakes after a timeout and tries
                // anyway has its arm refused and closes the tab it made itself (`startJob`).
                cleanup(d, &job.window, &job.workspace_id, json!({ "job_ids": [job.id] }));
            }
            // Its B reached the PTY reader though the window gave up waiting: it runs, and its
            // D (or its timeout) ends it. Or it was released mid-start, and the window, refused
            // its arm, closes what it made.
            None => log::info!("hive queue: {} began or ended though its window answered {err:?}", job.id),
        }
    }
}

fn cleanup(d: &'static Driver, window: &str, workspace_id: &str, what: Value) {
    let (label, mut args) = (window.to_string(), what);
    args["workspace_id"] = json!(workspace_id);
    tauri::async_runtime::spawn(async move {
        let out = crate::mailink::rpc::request_on(&d.app, Some(&d.handle), super::rpc::EVENT, &label, super::rpc::VERB_JOB_CLEANUP, args, super::rpc::TIMEOUT).await;
        if let Err(e) = super::rpc::applied(out) {
            log::info!("hive queue: job tabs not closed: {e}");
        }
    });
}

fn spawn_finish(e: Ended) {
    let Some(d) = DRIVER.get() else { return };
    tauri::async_runtime::spawn(finish(d, e));
}

/// A job left the active set: its row and ports go, the run log gets its line, a held action
/// whose copy is free goes to its window, a job still in its tab is ^C'd, and the queue looks
/// for the next start.
async fn finish(d: &'static Driver, e: Ended) {
    use tauri::Emitter;
    let job = e.job.clone();
    set_row(&d.app, &job, None);
    crate::stack::ports::release(&job.id);
    let logged = job.clone();
    let wrote = tauri::async_runtime::spawn_blocking(move || match super::paths::hive_root(&logged.hive_id) {
        Some(root) => append_run(&HivePaths::new(root), &logged),
        None => Err("no Hive folder".to_string()),
    })
    .await;
    let wrote = wrote.map_err(|e| e.to_string()).and_then(|r| r);
    if let Err(err) = wrote {
        log::warn!("hive queue: {} not in the run log: {err}", job.id);
    }
    for h in &e.freed {
        if let Err(err) = d.handle.emit_to(&h.window, LEASE_FREED_EVENT, h) {
            log::warn!("hive queue: {} not sent to window {}: {err}", LEASE_FREED_EVENT, h.window);
        }
    }
    if e.stop {
        let args = json!({ "workspace_id": job.workspace_id, "job_id": job.id, "tab_id": job.tab_id });
        let out = crate::mailink::rpc::request_on(&d.app, Some(&d.handle), super::rpc::EVENT, &job.window, super::rpc::VERB_JOB_STOP, args, super::rpc::TIMEOUT).await;
        if let Err(err) = super::rpc::applied(out) {
            log::warn!("hive queue: {} not stopped in its tab: {err}", job.id);
        }
    }
    super::rpc::emit_workers_changed(&d.handle, &job.window, &job.workspace_id);
    kick();
}

// ── Commands: the store's half ──

/// The window has a shell at its prompt for `job_id` and is about to type its line.
#[tauri::command]
pub fn stack_job_arm(job_id: String, tab_id: String, pty_id: String) -> Result<(), String> {
    queue().arm(&job_id, &tab_id, &pty_id)
}

/// Which of these copies a job leases now: checked before every stop and retarget (§6.8).
#[tauri::command]
pub fn stack_lease_state(copy_keys: Vec<String>) -> Result<Vec<LeaseState>, String> {
    Ok(queue().lease_state(&copy_keys))
}

/// Run when free: the stop or retarget waits in Rust, and `stack-lease-freed` tells this window
/// when the copy is free.
#[tauri::command]
pub fn stack_lease_hold(
    window: tauri::Window,
    workspace_id: String,
    copy_key: String,
    action: String,
    to: Option<String>,
    by: Option<String>,
) -> Result<HeldAction, String> {
    if !matches!(action.as_str(), "stop" | "restart" | "retarget") {
        return Err(format!("Nothing to hold for {action:?}"));
    }
    let by = by.filter(|b| b == "agent").unwrap_or_else(|| BY_HUMAN.to_string());
    queue().hold(HeldAction {
        id: String::new(),
        copy_key,
        workspace_id,
        window: window.label().to_string(),
        action,
        to,
        by,
        job: String::new(),
    })
}

/// The window carried out what waited on a copy (a Run when free, a Stop anyway): the copy may
/// be leased again before `LEASE_HANDOFF_MS` runs out.
#[tauri::command]
pub fn stack_lease_done(copy_key: String) -> Result<(), String> {
    queue().lease_done(&copy_key);
    kick();
    Ok(())
}

/// Stop anyway: the jobs leasing the copy are interrupted, never a receipt, and their workers
/// are told (5b-5). Answers the interrupted job ids.
#[tauri::command]
pub fn hive_lease_override(copy_key: String) -> Result<Vec<String>, String> {
    let ended = queue().override_lease(&copy_key, super::fuses::now_ms());
    let ids = ended.iter().map(|e| e.job.id.clone()).collect();
    for e in ended {
        log::info!("hive queue: {} interrupted: the human stopped {copy_key}", e.job.id);
        spawn_finish(e);
    }
    Ok(ids)
}

#[cfg(test)]
mod tests;
