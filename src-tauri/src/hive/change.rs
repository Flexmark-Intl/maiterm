//! Changing a Hive's settings after Create (docs/hive.md §3.4): workers at once, the coordinator's
//! and the workers' model and effort, the hosts every builder's sandbox may reach, push after land
//! (switching it on is always the human's card) and the rest. The human changes them in Hive settings (`hive_set_settings`). The
//! coordinator asks with `hiveChangeSettings`: under the Settings checkpoint's Ask me (the
//! default) that files a card only the human approves (an agent request of kind `hive_settings`,
//! carried out here from its stored arguments); handed to the coordinator, it applies at once and
//! the human is told. All three go through `check`, against the record as it is when they apply.
//! What workers may use (hosts, sockets, tools, generator hosts) is under the Access checkpoint
//! instead, the queen's by default (`change_plan::route`), so a worker's refused host goes to her
//! rather than to the human; each grant keeps its reason in the decision log.
//!
//! A change reaches an agent at its next launch, which rewrites its settings (§11.1): a running
//! worker keeps its model and its network until it is relaunched, and a cap lowered under the
//! workers running stops nobody, it only holds new ones back.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::checkpoints;
use super::deliver::one_line;
use super::fuses::STATUS_PAUSED;
use super::gallery::budget::{normalize_host, reaches_remote};
use super::record::{Hive, HiveModel, STATUS_ACTIVE};
use super::rpc::HiveWindow;
use super::tools::{Caller, Ctx};

/// What `hiveChangeSettings` files and approves: the request kind (`agent_requests::KINDS`).
pub const KIND: &str = "hive_settings";
/// The window's word that the coordinator changed settings on its own: a notice for the human.
pub const SETTINGS_CHANGED_EVENT: &str = "hive-settings-changed";
pub const MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// The workers' advisor (`HiveSettings::worker_advisor`): `follow` (stored as `""`) is the
/// human's own Claude settings, `off` none, else the model. Claude refuses an advisor less
/// capable than the worker's model, so haiku isn't offered.
pub const ADVISORS: [&str; 5] = [ADVISOR_FOLLOW, ADVISOR_OFF, "fable", "opus", "sonnet"];
pub const ADVISOR_FOLLOW: &str = "follow";
pub const ADVISOR_OFF: &str = "off";

/// A stored `worker_advisor` as the change tool and the sheet name it.
pub fn advisor_choice(stored: &str) -> &str {
    if stored.is_empty() { ADVISOR_FOLLOW } else { stored }
}

/// How the sheet and a card describe an advisor choice.
pub fn describe_advisor(choice: &str) -> String {
    match choice {
        ADVISOR_FOLLOW | "" => "as in your Claude settings".to_string(),
        ADVISOR_OFF => "none".to_string(),
        m => format!("Claude {m}"),
    }
}
/// Workers at once, whoever sets it: past this, no machine runs them.
pub const MAX_CAP: u32 = 64;
pub const MAX_WORKER_HOSTS: usize = 50;
const MAX_REASON: usize = 500;

/// One change, every field optional: what's left out stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SettingsChange {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cap: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub coordinator_effort: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_effort: Option<String>,
    /// Under the Settings checkpoint: one of `ADVISORS`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub worker_advisor: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_hosts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_hosts: Vec<String>,
    /// Under the Settings checkpoint (`change_plan::route`); a push leaves this computer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_after_land: Option<bool>,
    /// Under the Settings checkpoint: which of the Hive's agents are chats on the phone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers_on_phone: Option<bool>,
    /// Under the Access checkpoint: local database sockets the builders may connect to (`check_socket`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_sockets: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_sockets: Vec<String>,
    /// A git folder's setup or land check, as the project takes shape (§3.6).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<FolderChange>,
    /// Tools (MCP servers, CLIs, skills) detected on this Mac, by id (`change_plan`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_tools: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_tools: Vec<String>,
    /// The gallery's generator hosts and output folders, and the budget (`change_plan`).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_generator_hosts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_generator_hosts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_output_folders: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_output_folders: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub budget_cents: Option<u64>,
    /// Autonomy's prompt timeout and daily digest.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_timeout_secs: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daily_digest: Option<bool>,
    /// How often the queen is shown workers that may be stalled (`health`); 0 is off.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health_check_minutes: Option<u32>,
    /// The run queue, whole: its queued commands and resource limits.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_queue: Option<super::record::RunQueueSettings>,
    /// Checkpoints by key, `coordinator` or `ask`: the queen's change of one is always a card.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub checkpoints: BTreeMap<String, String>,
}

impl SettingsChange {
    /// The fields `check` and `apply_to` handle; the rest are `change_plan`'s.
    pub fn core(&self) -> SettingsChange {
        SettingsChange {
            add_tools: Vec::new(),
            remove_tools: Vec::new(),
            add_generator_hosts: Vec::new(),
            remove_generator_hosts: Vec::new(),
            add_output_folders: Vec::new(),
            remove_output_folders: Vec::new(),
            budget_cents: None,
            ..self.clone()
        }
    }
}

/// One git folder's commands, by its name. `""` clears one; left out, it stays as it is.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FolderChange {
    pub name: String,
    /// Runs outside any sandbox before each worker starts, so the coordinator's change of it is
    /// always a card, whoever holds the Settings checkpoint.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    /// Runs under the workers' sandbox before a land: the checkpoint decides, as for a model.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub land_check: Option<String>,
}

/// A setup or land check, at most: a command line, never a script.
pub const MAX_FOLDER_COMMAND: usize = 1000;
pub const MAX_WORKER_SOCKETS: usize = 10;

/// What a socket a worker may reach is called: a Postgres (`.s.PGSQL.<port>`), MySQL
/// (`mysql*.sock`, `mysqld*.sock`) or Redis (`redis*.sock`) server's. Measured (2026-11-18,
/// Claude 2.1.327): the worker sandbox can't open one local TCP port, since it reaches the network
/// only through its proxy, which database clients don't use; `allowUnixSockets` by path works.
/// Only a database's name is taken: other local sockets hand over far more (Docker's is root, an
/// ssh agent's or maiTerm's ControlMaster's can push, maiTerm's own reach its tools).
pub fn is_database_socket_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    if let Some(port) = name.strip_prefix(".s.PGSQL.") {
        return !port.is_empty() && port.len() <= 5 && port.bytes().all(|b| b.is_ascii_digit());
    }
    let stem = |prefix: &str| n.starts_with(prefix) && n.ends_with(".sock") && n[prefix.len()..n.len() - 5].bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    stem("mysqld") || stem("mysql") || stem("redis")
}

/// A database socket path as it's kept, or why not: absolute, plain, named like one
/// (`is_database_socket_name`). Its realpath is checked again at each launch (`launch_sockets`).
pub fn check_socket(raw: &str) -> Result<String, String> {
    let s = raw.trim();
    let p = std::path::Path::new(s);
    // A quote or backslash can't be named in a job's Seatbelt profile (`job_sandbox::sb_path`),
    // which would fail every builder job, not only its database's. A Unix socket's path starts at
    // `/` on every host: `is_absolute` would also want a drive letter on Windows.
    if !s.starts_with('/') || s.chars().any(|c| c.is_control() || c == '"' || c == '\\') || p.components().any(|c| matches!(c, std::path::Component::ParentDir | std::path::Component::CurDir)) {
        return Err(format!("\u{201c}{}\u{201d} isn\u{2019}t a full path to a socket, such as /tmp/.s.PGSQL.5432.", one_line(s, 80)));
    }
    let name = p.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if !is_database_socket_name(name) {
        return Err(format!(
            "{s} isn\u{2019}t named like a database\u{2019}s socket (.s.PGSQL.<port>, mysql.sock, redis.sock), so workers can\u{2019}t be given it: other local sockets reach far more than a database."
        ));
    }
    Ok(s.to_string())
}

/// The sockets a builder's launch or job may connect to, each as kept and as its realpath (the
/// worker sandbox matches either, a Seatbelt profile only the realpath), checked again: the state
/// file is plain JSON, and a symbolic link at the kept path must lead to a database's socket too.
/// A hard link to another socket under a database's name passes: the grant is only as strong as
/// the socket's folder, which no worker can write (its sandbox, and a job's profile, write only its
/// own folders) but the queen, with her human's access, could. One that doesn't exist now (its
/// server is down) is left out of this launch.
pub fn launch_sockets(hive: &Hive) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for s in &hive.settings.worker_sockets {
        let Ok(kept) = check_socket(s) else {
            log::warn!("hive {}: worker socket left out: {s}", hive.id);
            continue;
        };
        let Ok(real) = std::fs::canonicalize(&kept) else { continue };
        // The resolved path is what reaches both sandboxes, so it passes the same check as the
        // kept one (a link's target may hold a quote a Seatbelt profile can't name, say).
        let real_ok = real.to_str().is_some_and(|r| check_socket(r).is_ok_and(|c| c == r));
        #[cfg(unix)]
        let real_ok = real_ok && std::fs::metadata(&real).is_ok_and(|m| std::os::unix::fs::FileTypeExt::is_socket(&m.file_type()));
        if !real_ok {
            log::warn!("hive {}: {kept} doesn\u{2019}t lead to a database socket now; left out", hive.id);
            continue;
        }
        for p in [kept, real.to_string_lossy().to_string()] {
            if !out.contains(&p) {
                out.push(p);
            }
        }
    }
    out
}

/// Who a change comes from, which sets its limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// Hive settings: anything here.
    Human,
    /// The human's approval of the coordinator's card: the human's limits.
    Approved,
    /// `hiveChangeSettings` applying at once, under a checkpoint handed to the queen
    /// (`change_plan::route`): anything but the checkpoints themselves, a budget raise and
    /// switching push after land on, which are always a card (the approval switch, money, a push).
    Coordinator,
}

/// A change checked against a record: the fields that differ from it, normalised, and one line a
/// human reads for each.
#[derive(Debug, Clone, PartialEq)]
pub struct Checked {
    pub change: SettingsChange,
    pub lines: Vec<String>,
}

fn pick(v: &Option<String>, allowed: &[&str], what: &str) -> Result<Option<String>, String> {
    let Some(s) = v.as_deref().map(str::trim).filter(|s| !s.is_empty()) else { return Ok(None) };
    let s = s.to_ascii_lowercase();
    if !allowed.contains(&s.as_str()) {
        return Err(format!("{what} is one of {}, not \u{201c}{}\u{201d}.", allowed.join(", "), one_line(&s, 40)));
    }
    Ok(Some(s))
}

/// The change as it would land on `hive`, or why not. `remote_hosts` are the hosts the Hive's
/// repos push to, which no worker may reach (§7.3). Only the core fields (`SettingsChange::core`).
pub fn check(hive: &Hive, c: &SettingsChange, by: By, remote_hosts: &[String]) -> Result<Checked, String> {
    let ch = check_core(hive, c, by, remote_hosts)?;
    if ch.lines.is_empty() {
        return Err(NOTHING.to_string());
    }
    Ok(ch)
}

pub const NOTHING: &str = "Nothing would change: every setting named is already so.";

/// `check`, which may find nothing to change: a change whose other fields are `change_plan`'s.
pub fn check_core(hive: &Hive, c: &SettingsChange, by: By, remote_hosts: &[String]) -> Result<Checked, String> {
    if hive.status == super::record::STATUS_DONE {
        return Err("This Hive is done: a message to its queen wakes it first.".to_string());
    }
    let mut out = SettingsChange::default();
    let mut lines = Vec::new();

    if let Some(cap) = c.cap {
        if cap == 0 || cap > MAX_CAP {
            return Err(format!("Workers at once is 1 to {MAX_CAP}."));
        }
        if cap != hive.cap {
            out.cap = Some(cap);
            let note = if cap < hive.cap { " (running workers carry on; no new one starts while more are running)" } else { "" };
            lines.push(format!("Workers at once: {} \u{2192} {cap}{note}", hive.cap));
        }
    }

    let model_line = |who: &str, m: &HiveModel, model: &Option<String>, effort: &Option<String>| -> Option<String> {
        let (nm, ne) = (model.clone().unwrap_or_else(|| m.model.clone()), effort.clone().unwrap_or_else(|| m.effort.clone()));
        (nm != m.model || ne != m.effort).then(|| format!("{who}: {} \u{2192} {}", describe_model(&m.model, &m.effort), describe_model(&nm, &ne)))
    };
    let cm = pick(&c.coordinator_model, &MODELS, "The queen\u{2019}s model")?.filter(|m| *m != hive.models.coordinator.model);
    let ce = pick(&c.coordinator_effort, &EFFORTS, "The queen\u{2019}s effort")?.filter(|e| *e != hive.models.coordinator.effort);
    if let Some(l) = model_line("Queen (from its next launch)", &hive.models.coordinator, &cm, &ce) {
        lines.push(l);
    }
    let wm = pick(&c.worker_model, &MODELS, "The workers\u{2019} model")?.filter(|m| *m != hive.models.worker.model);
    let we = pick(&c.worker_effort, &EFFORTS, "The workers\u{2019} effort")?.filter(|e| *e != hive.models.worker.effort);
    if let Some(l) = model_line("Workers (each from its next launch)", &hive.models.worker, &wm, &we) {
        lines.push(l);
    }
    (out.coordinator_model, out.coordinator_effort, out.worker_model, out.worker_effort) = (cm, ce, wm, we);

    let was = advisor_choice(&hive.settings.worker_advisor).to_string();
    let advisor = pick(&c.worker_advisor, &ADVISORS, "The workers\u{2019} advisor")?.filter(|a| *a != was);
    // Claude skips an advisor less capable than the model it advises, with only a log line: a
    // named advisor below the workers' model would be a setting that silently does nothing.
    let rank = |m: &str| ["haiku", "sonnet", "opus", "fable"].iter().position(|x| *x == m);
    let (a, w) = (advisor.clone().unwrap_or_else(|| was.clone()), out.worker_model.clone().unwrap_or_else(|| hive.models.worker.model.clone()));
    if (advisor.is_some() || out.worker_model.is_some()) && rank(&a).zip(rank(&w)).is_some_and(|(ra, rw)| ra < rw) {
        return Err(format!(
            "Claude {a} can\u{2019}t advise workers on Claude {w}: an advisor must be at least as capable as the model it advises, or Claude skips it. Choose a stronger advisor, follow, or off."
        ));
    }
    if let Some(a) = advisor {
        lines.push(format!("Workers\u{2019} advisor (each from its next launch): {} \u{2192} {}", describe_advisor(&was), describe_advisor(&a)));
        out.worker_advisor = Some(a);
    }

    let have: Vec<String> = hive.settings.worker_hosts.iter().filter_map(|h| normalize_host(h).ok()).collect();
    let mut removed = Vec::new();
    for h in &c.remove_hosts {
        let h = normalize_host(h)?;
        if have.contains(&h) && !removed.contains(&h) {
            removed.push(h);
        }
    }
    let mut added = Vec::new();
    for h in &c.add_hosts {
        let h = normalize_host(h)?;
        if let Some(r) = reaches_remote(&h, remote_hosts) {
            return Err(format!("{h} reaches {r}, where this Hive\u{2019}s repos push: no worker may reach it."));
        }
        if !have.contains(&h) && !added.contains(&h) && !removed.contains(&h) {
            added.push(h);
        }
    }
    if have.len() - removed.len() + added.len() > MAX_WORKER_HOSTS {
        return Err(format!("Workers may reach at most {MAX_WORKER_HOSTS} hosts beyond the package registries."));
    }
    if !added.is_empty() {
        lines.push(format!("Workers may also reach (from each one\u{2019}s next launch): {}", added.join(", ")));
    }
    if !removed.is_empty() {
        lines.push(format!("Workers no longer reach (from each one\u{2019}s next launch): {}", removed.join(", ")));
    }
    (out.add_hosts, out.remove_hosts) = (added, removed);

    if let Some(p) = c.push_after_land {
        if p != hive.push_after_land {
            out.push_after_land = Some(p);
            lines.push(format!("Push after land: {}", if p { "on (maiTerm pushes each land to the remote)" } else { "off" }));
        }
    }

    if let Some(v) = c.workers_on_phone {
        if v != hive.settings.workers_on_phone {
            out.workers_on_phone = Some(v);
            lines.push(format!("On the phone: {} (workers started from now on)", if v { "the queen and its workers" } else { "the queen only" }));
        }
    }

    if !c.add_sockets.is_empty() || !c.remove_sockets.is_empty() {
        let have = &hive.settings.worker_sockets;
        let mut removed: Vec<String> = Vec::new();
        for s in &c.remove_sockets {
            let s = s.trim().to_string();
            if have.contains(&s) && !removed.contains(&s) {
                removed.push(s);
            }
        }
        let mut added: Vec<String> = Vec::new();
        for s in &c.add_sockets {
            let s = check_socket(s)?;
            if !have.contains(&s) && !added.contains(&s) && !removed.contains(&s) {
                added.push(s);
            }
        }
        if have.len() - removed.len() + added.len() > MAX_WORKER_SOCKETS {
            return Err(format!("Workers may reach at most {MAX_WORKER_SOCKETS} local database sockets."));
        }
        if !added.is_empty() {
            lines.push(format!("Workers may also connect to (from each one\u{2019}s next launch): {}", added.join(", ")));
        }
        if !removed.is_empty() {
            lines.push(format!("Workers no longer connect to (from each one\u{2019}s next launch): {}", removed.join(", ")));
        }
        (out.add_sockets, out.remove_sockets) = (added, removed);
    }

    for fc in &c.folders {
        let Some(f) = hive.git_folders().find(|f| f.name == fc.name.trim()) else {
            let names: Vec<&str> = hive.git_folders().map(|f| f.name.as_str()).collect();
            return Err(format!("This Hive has no git folder named \u{201c}{}\u{201d}; its git folders: {}.", one_line(fc.name.trim(), 60), names.join(", ")));
        };
        let mut one = FolderChange { name: f.name.clone(), ..FolderChange::default() };
        if let Some(new) = folder_command(&fc.setup, "setup", &f.name)?.filter(|n| *n != f.setup.clone().unwrap_or_default()) {
            lines.push(format!("Setup of {} (runs outside the sandbox before each worker starts, from the next one on): {} \u{2192} {}", f.name, shown(&f.setup), shown(&Some(new.clone()))));
            one.setup = Some(new);
        }
        if let Some(new) = folder_command(&fc.land_check, "land check", &f.name)?.filter(|n| *n != f.land_check.clone().unwrap_or_default()) {
            lines.push(format!("Land check of {} (runs before each land, in the workers\u{2019} sandbox): {} \u{2192} {}", f.name, shown(&f.land_check), shown(&Some(new.clone()))));
            one.land_check = Some(new);
        }
        if one.setup.is_some() || one.land_check.is_some() {
            if out.folders.iter().any(|o| o.name == one.name) {
                return Err(format!("Name {} once.", f.name));
            }
            out.folders.push(one);
        }
    }

    if let Some(secs) = c.prompt_timeout_secs {
        let secs = checkpoints::clamp_timeout(secs);
        if secs != hive.settings.autonomy.prompt_timeout_secs {
            out.prompt_timeout_secs = Some(secs);
            lines.push(format!("Prompt timeout: {} \u{2192} {} min", hive.settings.autonomy.prompt_timeout_secs.div_ceil(60), secs.div_ceil(60)));
        }
    }
    if let Some(d) = c.daily_digest {
        if d != hive.settings.autonomy.daily_digest {
            out.daily_digest = Some(d);
            lines.push(format!("Daily digest: {}", if d { "on" } else { "off" }));
        }
    }
    if let Some(m) = c.health_check_minutes {
        if m > super::health::MAX_MINUTES {
            return Err(format!("The worker check runs every 1 to {} minutes, or 0 for never.", super::health::MAX_MINUTES));
        }
        if m != hive.settings.autonomy.health_check_minutes {
            out.health_check_minutes = Some(m);
            let say = |m: u32| if m == 0 { "off".to_string() } else { format!("every {m} min") };
            lines.push(format!("Worker check: {} \u{2192} {}", say(hive.settings.autonomy.health_check_minutes), say(m)));
        }
    }

    if let Some(rq) = &c.run_queue {
        let rq = super::run::validate_run_queue(hive, rq)?;
        if rq != hive.settings.run_queue {
            let cmds = |r: &super::record::RunQueueSettings| {
                if r.commands.is_empty() { "none".to_string() } else { r.commands.iter().map(|q| format!("`{}` in {}", q.prefix, q.folder)).collect::<Vec<_>>().join(", ") }
            };
            let res = |r: &super::record::RunQueueSettings| {
                if r.resources.is_empty() { "none".to_string() } else { r.resources.iter().map(|(k, v)| format!("{k} {v}")).collect::<Vec<_>>().join(", ") }
            };
            lines.push(format!("Run queue: queued commands {} \u{2192} {}; resource limits {} \u{2192} {}", cmds(&hive.settings.run_queue), cmds(&rq), res(&hive.settings.run_queue), res(&rq)));
            out.run_queue = Some(rq);
        }
    }

    if !c.checkpoints.is_empty() {
        // The human's approval switch: the queen asks for a change of it on a card, always.
        if by == By::Coordinator {
            return Err("Changing a checkpoint is your human\u{2019}s: it goes on a card.".to_string());
        }
        let before = checkpoints::validate(hive, &BTreeMap::new())?;
        let after = checkpoints::validate(hive, &c.checkpoints)?;
        let changed = checkpoints::describe_changes(&before, &after);
        if !changed.is_empty() {
            out.checkpoints = c.checkpoints.iter().filter(|(k, v)| before.get(*k) != Some(*v)).map(|(k, v)| (k.clone(), v.clone())).collect();
            lines.extend(changed.into_iter().map(|l| format!("Checkpoint, {l}")));
        }
    }

    Ok(Checked { change: out, lines })
}

/// A folder command as named: trimmed, `""` for none, one line no longer than the limit.
fn folder_command(v: &Option<String>, what: &str, folder: &str) -> Result<Option<String>, String> {
    let Some(s) = v.as_deref().map(str::trim) else { return Ok(None) };
    if s.chars().any(char::is_control) {
        return Err(format!("The {what} of {folder} has a line break or control character in it: it\u{2019}s one command line."));
    }
    if s.chars().count() > MAX_FOLDER_COMMAND {
        return Err(format!("The {what} of {folder} is over {MAX_FOLDER_COMMAND} characters: put a longer one in a script in the repo."));
    }
    Ok(Some(s.to_string()))
}

/// A command for a card: quoted whole, or "none".
fn shown(v: &Option<String>) -> String {
    match v.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        Some(s) => format!("`{s}`"),
        None => "none".to_string(),
    }
}

pub fn describe_model(model: &str, effort: &str) -> String {
    let m = if model.is_empty() { "default" } else { model };
    if effort.is_empty() { format!("Claude {m}") } else { format!("Claude {m}, {effort} effort") }
}

/// A checked change written onto the record.
pub fn apply_to(hive: &mut Hive, c: &SettingsChange) {
    if let Some(cap) = c.cap {
        hive.cap = cap;
    }
    let set = |m: &mut HiveModel, model: &Option<String>, effort: &Option<String>| {
        if let Some(v) = model {
            m.model = v.clone();
        }
        if let Some(v) = effort {
            m.effort = v.clone();
        }
    };
    set(&mut hive.models.coordinator, &c.coordinator_model, &c.coordinator_effort);
    set(&mut hive.models.worker, &c.worker_model, &c.worker_effort);
    if let Some(a) = &c.worker_advisor {
        hive.settings.worker_advisor = if a == ADVISOR_FOLLOW { String::new() } else { a.clone() };
    }
    let hosts = &mut hive.settings.worker_hosts;
    hosts.retain(|h| normalize_host(h).map_or(true, |n| !c.remove_hosts.contains(&n)));
    hosts.extend(c.add_hosts.iter().cloned());
    if let Some(p) = c.push_after_land {
        hive.push_after_land = p;
    }
    if let Some(v) = c.workers_on_phone {
        hive.settings.workers_on_phone = v;
    }
    let sockets = &mut hive.settings.worker_sockets;
    sockets.retain(|s| !c.remove_sockets.contains(s));
    sockets.extend(c.add_sockets.iter().cloned());
    let none_if_empty = |s: &String| Some(s.clone()).filter(|s| !s.is_empty());
    for fc in &c.folders {
        let Some(f) = hive.folders.iter_mut().find(|f| f.name == fc.name) else { continue };
        if let Some(s) = &fc.setup {
            f.setup = none_if_empty(s);
        }
        if let Some(s) = &fc.land_check {
            f.land_check = none_if_empty(s);
        }
    }
    if let Some(secs) = c.prompt_timeout_secs {
        hive.settings.autonomy.prompt_timeout_secs = checkpoints::clamp_timeout(secs);
    }
    if let Some(d) = c.daily_digest {
        hive.settings.autonomy.daily_digest = d;
    }
    if let Some(m) = c.health_check_minutes {
        hive.settings.autonomy.health_check_minutes = m;
    }
    if let Some(rq) = &c.run_queue {
        hive.settings.run_queue = rq.clone();
    }
    if !c.checkpoints.is_empty() {
        if let Ok(next) = checkpoints::validate(hive, &c.checkpoints) {
            hive.checkpoints = next;
        }
    }
}

/// Checked against the record as it is under the write lock, then saved and announced. The new
/// record and the lines that describe what changed.
/// The Hive's remote hosts (git, and `ssh -G` up to its timeout per alias), read once per request
/// off the async thread (the regression review).
pub(super) async fn remotes_of(hive: &Hive) -> Result<Vec<String>, String> {
    let h = hive.clone();
    super::tools::blocking(move || Ok(super::settings::hive_remote_hosts(&h))).await
}

/// The core fields only (`SettingsChange::core`); the lines may be empty when none of them differ.
pub(super) fn write<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, c: &SettingsChange, by: By, remotes: &[String]) -> Result<(Hive, Vec<String>), String> {
    let mut outcome: Result<Vec<String>, String> = Err("This Hive is gone.".to_string());
    // A refused check leaves the record as it was (saved again, unchanged: harmless).
    let hive = super::finish::write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, |h| {
        outcome = check_core(h, c, by, remotes).map(|ch| {
            apply_to(h, &ch.change);
            ch.lines
        });
    })?;
    Ok((hive, outcome?))
}

pub(super) fn log_line(caller: &Caller, by: &str, lines: &[String], reason: Option<&str>) {
    let line = json!({ "v": 1, "at": crate::mailink::board::iso_now(), "by": by, "changes": lines, "reason": reason });
    if let Err(e) = super::queue::append_line(&caller.paths.settings_log(), &line) {
        log::warn!("hive {}: settings changed but not logged: {e}", caller.hive.id);
    }
    log::info!("hive {}: settings changed by {by}: {}", caller.hive.id, lines.join("; "));
}

async fn tell_coordinator<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) {
    let Some(c) = super::tools::coordinator_tab(ctx.data, caller) else { return };
    let sent = ctx.window.deliver(&c, text, "Hive settings").await;
    log::info!("hive {}: settings notice to the coordinator: {}", caller.hive.id, sent.describe());
}

/// Whether a change reaches workers only from their next launch: their model, effort or hosts.
fn needs_worker_relaunch(c: &SettingsChange) -> bool {
    c.worker_model.is_some()
        || c.worker_effort.is_some()
        || c.worker_advisor.is_some()
        || !c.add_hosts.is_empty()
        || !c.remove_hosts.is_empty()
        // A running builder's shell keeps the sockets it launched with, a removed one included.
        || !c.add_sockets.is_empty()
        || !c.remove_sockets.is_empty()
        // Its MCP servers and network come from the Hive's tools and generator hosts at launch.
        || !c.add_tools.is_empty()
        || !c.remove_tools.is_empty()
        || !c.add_generator_hosts.is_empty()
        || !c.remove_generator_hosts.is_empty()
}

/// For a change that reaches workers only at their next launch: which running workers still
/// have the old settings, and what to do. None when none is running, or the change isn't one.
fn relaunch_sentence(c: &SettingsChange, workers: &[super::worker::WorkerRecord]) -> Option<String> {
    if !needs_worker_relaunch(c) {
        return None;
    }
    running_on_old(workers)
}

/// The running workers a next-launch change hasn't reached yet, and what to do: None when none
/// is running. A tools change (`tool_change`) says it too.
pub(super) fn running_on_old(workers: &[super::worker::WorkerRecord]) -> Option<String> {
    let live: Vec<String> = workers.iter().filter(|w| w.is_live()).map(super::tools::worker_label).collect();
    if live.is_empty() {
        return None;
    }
    Some(format!(
        "Running on the old settings until relaunched: {}. Relaunch each with hiveRestartWorker when it is at a good point (its \
         session resumes).",
        live.join(", ")
    ))
}

/// `relaunch_sentence` from the workers on disk now.
pub(super) async fn relaunch_note(caller: &Caller, c: &SettingsChange) -> Option<String> {
    if !needs_worker_relaunch(c) {
        return None;
    }
    let p = caller.paths.clone();
    let workers = super::tools::blocking(move || Ok(super::worker::list_workers(&p))).await.unwrap_or_default();
    relaunch_sentence(c, &workers)
}

fn bulleted(lines: &[String]) -> String {
    lines.iter().map(|l| format!("\n- {l}")).collect()
}

/// "Home", as maiTerm's servers count it (`host_guard::is_home`): what they answer is exactly
/// what no worker may reach.
fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    crate::host_guard::is_home(ip)
}

/// Names a test treats as resolving to a public address, so it needs no network.
#[cfg(test)]
pub(crate) static TEST_PUBLIC: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// A host added now must not answer this computer or its network (§6.7): a public name for
/// 127.0.0.1 in a worker's allowlist opens every local port, maiTerm's own included. Names on
/// `LOOPBACK_DOMAINS` are refused by `normalize_host`; the rest are looked up here, a `*.` entry
/// by a name under it. For the human, a name that doesn't resolve passes: its owner could point it
/// home later, and the human chose it. The queen grants alone (2026-10-31), so for her
/// (`must_resolve`) a name that doesn't resolve now is refused (the review).
pub(crate) async fn refuse_private_hosts(hosts: &[String], must_resolve: bool, remote_ips: &[std::net::IpAddr]) -> Result<(), String> {
    for h in hosts {
        let Ok(h) = normalize_host(h) else { continue };
        let name = match h.strip_prefix("*.") {
            Some(base) => format!("maiterm-probe.{base}"),
            None => h.clone(),
        };
        let lookup = tokio::time::timeout(std::time::Duration::from_secs(3), tokio::net::lookup_host(format!("{name}:443"))).await;
        let addrs: Vec<std::net::IpAddr> = match lookup {
            Ok(Ok(a)) => a.map(|a| a.ip()).collect(),
            _ => Vec::new(),
        };
        #[cfg(test)]
        let addrs = if addrs.is_empty() && TEST_PUBLIC.lock().unwrap().contains(&h) { vec![std::net::IpAddr::from([8, 8, 8, 8])] } else { addrs };
        if addrs.is_empty() {
            if must_resolve {
                return Err(format!("{h} doesn\u{2019}t resolve yet, so maiTerm can\u{2019}t check where it points: ask again once it does."));
            }
            continue;
        }
        if let Some(ip) = addrs.iter().find(|ip| is_private_ip(ip)) {
            return Err(format!("{h} answers {ip}, this computer or its network: a worker\u{2019}s localhost and LAN stay closed."));
        }
        if let Some(ip) = addrs.iter().find(|ip| remote_ips.contains(ip)) {
            return Err(format!("{h} answers {ip}, a git remote of this Hive: no worker may reach it."));
        }
    }
    Ok(())
}

/// The remotes given as IP addresses: a name that answers one reaches the remote (the final review).
pub(super) fn ip_remotes(remote_hosts: &[String]) -> Vec<std::net::IpAddr> {
    remote_hosts.iter().filter_map(|r| r.trim_start_matches(super::gallery::budget::UNRESOLVED_ALIAS).parse().ok()).collect()
}

/// `refuse_private_hosts` for one host, from a blocking caller (a worker's launch): whether it
/// answers this computer or its network now. At most 2 s; a lookup that fails or runs out passes,
/// as when it is added.
pub fn points_home_blocking(host: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok(h) = normalize_host(host) else { return false };
    let name = match h.strip_prefix("*.") {
        Some(base) => format!("maiterm-probe.{base}:443"),
        None => format!("{h}:443"),
    };
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(name.to_socket_addrs().map(|a| a.map(|s| s.ip()).any(|ip| is_private_ip(&ip))).unwrap_or(false));
    });
    rx.recv_timeout(std::time::Duration::from_secs(2)).unwrap_or(false)
}

/// The tool's arguments: the change, and the coordinator's reason.
pub fn parse_args(args: &Value) -> Result<(SettingsChange, Option<String>), String> {
    let mut change: SettingsChange = serde_json::from_value(args.clone()).map_err(|e| format!("The settings don\u{2019}t read: {e}"))?;
    // The tool speaks dollars and minutes; the record keeps cents and seconds.
    if args.get("budget_cents").is_some() || args.get("prompt_timeout_secs").is_some() {
        return Err("Name the budget as budget_usd and the prompt timeout as prompt_timeout_minutes.".to_string());
    }
    if let Some(v) = args.get("budget_usd") {
        let usd = v.as_f64().filter(|u| u.is_finite() && *u >= 0.0).ok_or("budget_usd is a number of dollars, 0 or more.")?;
        change.budget_cents = Some((usd * 100.0).round() as u64);
    }
    if let Some(v) = args.get("prompt_timeout_minutes") {
        let m = v.as_u64().ok_or("prompt_timeout_minutes is a whole number of minutes.")?;
        change.prompt_timeout_secs = Some(m.saturating_mul(60));
    }
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).map(|r| one_line(r, MAX_REASON));
    Ok((change, reason))
}

/// `run_queue` in the tool's schema (its own `json!`: one would pass the macro's recursion limit).
fn run_queue_schema() -> Value {
    let command = json!({ "type": "object", "properties": {
        "folder": { "type": "string" }, "prefix": { "type": "string" },
        "needs": { "type": "array", "items": { "type": "string" } },
        "services": { "type": "array", "items": { "type": "string" } },
        "env": { "type": "array", "items": { "type": "array", "items": { "type": "string" }, "minItems": 2, "maxItems": 2 } },
        "extra_write": { "type": "array", "items": { "type": "string" } }
    }, "required": ["folder", "prefix"] });
    json!({
        "type": "object",
        "description": "The whole run queue, replacing it: commands workers run only through hiveRun, and resource limits",
        "properties": {
            "commands": { "type": "array", "items": command },
            "resources": { "type": "object", "additionalProperties": { "type": "integer", "minimum": 1, "maximum": 64 } }
        }
    })
}

pub fn tool_definition(tab_id: &Value) -> Value {
    let mut def = json!({
        "name": "hiveChangeSettings",
        "description": format!(
            "Change any of this Hive\u{2019}s settings; name only what changes, and say why. Two checkpoints decide what applies at \
             once (your human is told, and the decision log keeps it) and what goes on a card your human approves or denies \
             (maiTerm tells you which): ACCESS covers what workers may use (add_hosts/remove_hosts, add_sockets/remove_sockets, \
             add_tools/remove_tools, add_generator_hosts/remove_generator_hosts), SETTINGS covers the rest (cap, models and \
             efforts, worker_advisor, folders\u{2019} setup and land check, push_after_land, workers_on_phone, run_queue, budget_usd lowered, \
             output folders, prompt_timeout_minutes, daily_digest, health_check_minutes). Your brief says who holds each. A call may mix them: what \
             its checkpoints hand you applies, the rest goes on one card. Raising budget_usd, switching push_after_land on and any \
             change of checkpoints always go on a card. When a worker is stuck on a host, a database or a tool, grant it here rather than waiting: \
             a host answers each open sandbox blocker it settles. A change reaches each agent at its next launch: a running \
             worker keeps its model, network, sockets and tools until hiveRestartWorker relaunches it. Refused for anyone: a \
             repo remote\u{2019}s host, localhost or a private address; a socket not named like a database\u{2019}s \
             (.s.PGSQL.<port>, mysql.sock, redis.sock); a tool not detected on this Mac. Models: {}. Efforts: {}. Checkpoints: {}.",
            MODELS.join(", "),
            EFFORTS.join(", "),
            checkpoints::CHECKPOINTS.join(", ")
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "cap": { "type": "integer", "minimum": 1, "maximum": MAX_CAP, "description": "Workers at once" },
                "coordinator_model": { "type": "string", "enum": MODELS },
                "coordinator_effort": { "type": "string", "enum": EFFORTS },
                "worker_model": { "type": "string", "enum": MODELS },
                "worker_effort": { "type": "string", "enum": EFFORTS },
                "worker_advisor": { "type": "string", "enum": ADVISORS, "description": "The model workers and reviewers consult before substantive work: follow (your human's own Claude settings, the default), off, or a model. Each consultation costs; it can't be less capable than the workers' model" },
                "add_hosts": { "type": "array", "items": { "type": "string" }, "description": "Bare host names (api.example.com, *.example.com) every builder may reach" },
                "remove_hosts": { "type": "array", "items": { "type": "string" } },
                "checkpoints": { "type": "object", "additionalProperties": { "type": "string", "enum": ["coordinator", "ask"] }, "description": "Checkpoint by key: always a card" },
                "folders": {
                    "type": "array",
                    "description": "Git folders whose setup or land check changes, by name. \"\" clears one; leave a field out to keep it.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "name": { "type": "string" },
                            "setup": { "type": "string", "description": "One command line, run in each new worker's copy before it starts (e.g. `npm ci`)" },
                            "land_check": { "type": "string", "description": "One command line a land must pass (e.g. `npm test`)" }
                        },
                        "required": ["name"]
                    }
                },
                "reason": { "type": "string", "description": "Why: your human reads it on the card" }
            },
            "required": ["reason"]
        }
    });
    if let Some(p) = def.pointer_mut("/inputSchema/properties").and_then(Value::as_object_mut) {
        p.extend(more_properties());
    }
    def
}

/// The rest of the tool's properties (their own `json!`: one would pass the macro's recursion limit).
fn more_properties() -> serde_json::Map<String, Value> {
    let strings = json!({ "type": "array", "items": { "type": "string" } });
    let mut m = json!({
        "add_sockets": { "type": "array", "items": { "type": "string" }, "description": "Full paths of local database sockets every builder may connect to (/tmp/.s.PGSQL.5432)" },
        "add_tools": { "type": "array", "items": { "type": "string" }, "description": "Tool ids detected on this Mac (mcp:<server>, cli:<program>, skill:<name>): workers get the MCP servers and CLIs, and the hosts and output folders they need come with them" },
        "add_generator_hosts": { "type": "array", "items": { "type": "string" }, "description": "Hosts paid generators reach, for media workers" },
        "add_output_folders": { "type": "array", "items": { "type": "string" }, "description": "Absolute folders galleryAdd may file from" },
        "push_after_land": { "type": "boolean", "description": "maiTerm pushes each land to its remote" },
        "workers_on_phone": { "type": "boolean", "description": "Workers are chats on your human's phone too" },
        "budget_usd": { "type": "number", "minimum": 0, "description": "The generation budget in dollars; a raise always goes on a card" },
        "prompt_timeout_minutes": { "type": "integer", "minimum": 1, "maximum": 60, "description": "How long you have to answer a worker's prompt before your human is told" },
        "daily_digest": { "type": "boolean" },
        "health_check_minutes": { "type": "integer", "minimum": 0, "maximum": super::health::MAX_MINUTES, "description": "How often maiTerm shows you workers that may be stalled (⟦HIVE CHECK⟧); 0 is never" }
    });
    let m = m.as_object_mut().expect("an object");
    for k in ["remove_sockets", "remove_tools", "remove_generator_hosts", "remove_output_folders"] {
        m.insert(k.to_string(), strings.clone());
    }
    m.insert("run_queue".to_string(), run_queue_schema());
    std::mem::take(m)
}

/// Hive settings: the human's own change (the core fields: tools, the gallery and autonomy have
/// their own commands). The coordinator is told.
pub(crate) async fn set_by_human<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, change: &SettingsChange) -> Result<Hive, String> {
    let change = &change.core();
    let remotes = remotes_of(&caller.hive).await?;
    refuse_private_hosts(&change.add_hosts, false, &ip_remotes(&remotes)).await?;
    // Checked first, so a change with nothing in it is refused before anything is saved.
    check(&super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?, change, By::Human, &remotes)?;
    let (hive, lines) = write(ctx, caller, change, By::Human, &remotes)?;
    if lines.is_empty() {
        return Err(NOTHING.to_string());
    }
    log_line(caller, "human", &lines, None);
    let running = relaunch_note(caller, change).await.map(|s| format!("\n{s}")).unwrap_or_default();
    tell_coordinator(ctx, caller, format!("[maiTerm] Your human changed this Hive\u{2019}s settings:{}{running}", bulleted(&lines))).await;
    ctx.window.poll_waiters(&caller.workspace_id);
    Ok(hive)
}

/// The human approved the coordinator's card: applied from the stored arguments, re-checked
/// against the record as it is now. The record after, when the change moved a checkpoint, so the
/// caller can hand the prompts it now covers to the human.
pub(crate) async fn approved_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, request_id: &str, args: &Value) -> Option<(Hive, Vec<String>)> {
    let change: Result<SettingsChange, String> =
        args.get("change").cloned().ok_or_else(|| "it had no change".to_string()).and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()));
    let res = match change {
        Ok(c) => match remotes_of(&caller.hive).await {
            Ok(remotes) => super::change_plan::commit(ctx, caller, &c, By::Approved, &remotes).await.map(|done| (done, c)),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    match res {
        Ok((done, c)) => {
            let (hive, lines) = (done.hive, done.lines);
            log_line(caller, "coordinator, approved by the human", &lines, None);
            // What applied stands; the budget, `commit`'s one later step, is said when it failed.
            let not = done.failed.as_ref().map(|e| format!("; the budget wasn\u{2019}t changed: {e}")).unwrap_or_default();
            ctx.window.settle_request(request_id, true, format!("applied: {}{not}", lines.join("; ")));
            let running = relaunch_note(caller, &c).await.map(|s| format!("\n{s}")).unwrap_or_default();
            let not = done.failed.map(|e| format!("\nThe budget wasn\u{2019}t changed: {e}")).unwrap_or_default();
            tell_coordinator(ctx, caller, format!("[maiTerm] Your human approved your settings change:{}{running}{not}", bulleted(&lines))).await;
            ctx.window.poll_waiters(&caller.workspace_id);
            let moved: Vec<String> = lines.iter().filter_map(|l| l.strip_prefix("Checkpoint, ").map(str::to_string)).collect();
            (!moved.is_empty()).then_some((hive, moved))
        }
        Err(e) => {
            ctx.window.settle_request(request_id, false, e.clone());
            tell_coordinator(ctx, caller, format!("[maiTerm] Your human approved your settings change, but it couldn\u{2019}t apply: {e}")).await;
            None
        }
    }
}

/// Denied or lapsed: the coordinator is told.
pub(crate) async fn closed_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, status: &str) {
    let how = if status == "denied" { "denied" } else { "didn\u{2019}t answer in time; it lapsed" };
    tell_coordinator(ctx, caller, format!("[maiTerm] Your human {how} your settings change: nothing changed.")).await;
}

fn app_and_caller(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) -> Option<(std::sync::Arc<crate::state::AppState>, Caller)> {
    use tauri::Manager;
    let state = app.try_state::<std::sync::Arc<crate::state::AppState>>()?.inner().clone();
    let hive_id = r.args.get("hive_id").and_then(Value::as_str)?.to_string();
    let caller = super::tools::hive_callers(&state.app_data.read()).into_iter().find(|c| c.hive.id == hive_id)?;
    Some((state, caller))
}

/// `agent_requests::decide`'s approval of a `hive_settings` card.
pub fn on_approved(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) {
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let Some((state, caller)) = app_and_caller(&app, &r) else {
            crate::agent_requests::settle(&app, &r.id, false, "its Hive is gone".to_string());
            return;
        };
        let ctx = super::tools::app_ctx(&state, &app, &caller);
        if let Some((hive, moved)) = approved_in(&ctx, &caller, &r.id, &r.args).await {
            // As Hive settings → Autonomy does: a prompt the queen holds under a checkpoint now
            // Ask me becomes the human's, and she is told.
            super::prompts::checkpoints_changed(&state, &app, &caller.label, &caller.workspace_id, &hive, moved).await;
        }
    });
}

/// A `hive_settings` card denied or lapsed.
pub fn on_closed(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) {
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let Some((state, caller)) = app_and_caller(&app, &r) else { return };
        let ctx = super::tools::app_ctx(&state, &app, &caller);
        closed_in(&ctx, &caller, &r.status).await;
    });
}

/// Hive settings → Workers and access (§3.4): the human's change, checked like any other.
#[tauri::command]
pub async fn hive_set_settings(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    change: SettingsChange,
) -> Result<Hive, String> {
    let caller = super::tools::caller_for_workspace(&state.app_data.read(), &workspace_id)
        .filter(|c| c.label == window.label())
        .ok_or("That workspace isn't a Hive in this window")?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    set_by_human(&ctx, &caller, &change).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hive() -> Hive {
        let mut h = Hive { id: "h1".into(), cap: 3, status: STATUS_ACTIVE.into(), ..Hive::default() };
        h.settings.worker_hosts = vec!["docs.example.com".into()];
        h
    }

    #[test]
    fn a_change_keeps_only_what_differs_and_says_each_line() {
        let c = SettingsChange { cap: Some(3), worker_model: Some("Opus".into()), worker_effort: Some("high".into()), ..Default::default() };
        let ch = check(&hive(), &c, By::Coordinator, &[]).unwrap();
        assert_eq!(ch.change.cap, None, "the cap is already 3");
        assert_eq!(ch.change.worker_model.as_deref(), Some("opus"), "lower-cased");
        assert_eq!(ch.change.worker_effort, None, "workers are already at high effort");
        assert_eq!(ch.lines, vec!["Workers (each from its next launch): Claude sonnet, high effort \u{2192} Claude opus, high effort".to_string()]);
        let none = SettingsChange { cap: Some(3), ..Default::default() };
        assert!(check(&hive(), &none, By::Human, &[]).unwrap_err().starts_with("Nothing would change"));
    }

    /// A host or worker model approved mid-run reaches no running worker until it is relaunched:
    /// the queen is told which ones, by name, and how.
    #[test]
    fn a_next_launch_change_names_the_running_workers_to_relaunch() {
        use super::super::worker::{WorkerRecord, PHASE_REPORTED, PHASE_RETIRED, PHASE_RUNNING};
        let w = |badge: u32, slug: &str, phase: &str| WorkerRecord { badge, slug: slug.into(), phase: phase.into(), ..WorkerRecord::default() };
        let workers = [w(1, "login", PHASE_RUNNING), w(2, "api", PHASE_RETIRED), w(3, "screens", PHASE_REPORTED)];
        let hosts = SettingsChange { add_hosts: vec!["api.example.com".into()], ..Default::default() };
        let s = relaunch_sentence(&hosts, &workers).unwrap();
        assert!(s.contains("#1 login, #3 screens") && !s.contains("#2 api") && s.contains("hiveRestartWorker"), "{s}");
        let model = SettingsChange { worker_model: Some("opus".into()), ..Default::default() };
        assert!(relaunch_sentence(&model, &workers).is_some());
        let cap = SettingsChange { cap: Some(2), ..Default::default() };
        assert!(relaunch_sentence(&cap, &workers).is_none(), "the cap needs no relaunch");
        assert!(relaunch_sentence(&hosts, &workers[1..2]).is_none(), "nothing running, nothing to say");
        let sockets = SettingsChange { remove_sockets: vec!["/tmp/.s.PGSQL.5432".into()], ..Default::default() };
        assert!(relaunch_sentence(&sockets, &workers).is_some_and(|s| s.contains("#1 login")), "a removed socket stays in a running shell");
    }

    #[test]
    /// Darryl, 2026-11-21: the queen sets anything the checkpoints hand her (`change_plan::route`
    /// decides what applies at once); `check` keeps only the limits that hold for anyone, and a
    /// checkpoint change, which is always the human's.
    fn the_queen_may_set_anything_but_a_checkpoint() {
        let up = SettingsChange { cap: Some(8), ..Default::default() };
        assert_eq!(check(&hive(), &up, By::Coordinator, &[]).unwrap().change.cap, Some(8));
        assert_eq!(check(&hive(), &SettingsChange { cap: Some(2), ..Default::default() }, By::Coordinator, &[]).unwrap().change.cap, Some(2));
        assert!(check(&hive(), &SettingsChange { cap: Some(MAX_CAP + 1), ..Default::default() }, By::Coordinator, &[]).is_err());
        assert!(check(&hive(), &SettingsChange { cap: Some(0), ..Default::default() }, By::Human, &[]).is_err());
        let push = SettingsChange { push_after_land: Some(true), ..Default::default() };
        for by in [By::Coordinator, By::Approved, By::Human] {
            assert!(check(&hive(), &push, by, &[]).is_ok());
        }
        let args = json!({ "push_after_land": true, "budget_usd": 12.5, "prompt_timeout_minutes": 10, "reason": "ship it" });
        let (c, reason) = parse_args(&args).unwrap();
        assert_eq!((c.push_after_land, c.budget_cents, c.prompt_timeout_secs, reason.as_deref()), (Some(true), Some(1250), Some(600), Some("ship it")));
        assert!(parse_args(&json!({ "budget_cents": 5 })).is_err(), "the tool speaks dollars");
        let cp = SettingsChange { checkpoints: [("landing".to_string(), "ask".to_string())].into(), ..Default::default() };
        assert!(check(&hive(), &cp, By::Coordinator, &[]).unwrap_err().contains("card"));
        let ok = check(&hive(), &cp, By::Approved, &[]).unwrap();
        assert_eq!(ok.lines, vec!["Checkpoint, landing: Ask me".to_string()]);
        let mut h = hive();
        apply_to(&mut h, &ok.change);
        assert!(checkpoints::asks(&h, "landing"));
        assert!(check(&hive(), &SettingsChange { checkpoints: [("nope".to_string(), "ask".to_string())].into(), ..Default::default() }, By::Approved, &[]).is_err());
        let t = SettingsChange { prompt_timeout_secs: Some(5), daily_digest: Some(false), ..Default::default() };
        let ch = check(&hive(), &t, By::Coordinator, &[]).unwrap();
        assert_eq!((ch.change.prompt_timeout_secs, ch.change.daily_digest), (Some(60), Some(false)), "clamped to a minute");
        let phone = SettingsChange { workers_on_phone: Some(true), ..Default::default() };
        let mut h = hive();
        apply_to(&mut h, &check(&hive(), &phone, By::Human, &[]).unwrap().change);
        assert!(h.settings.workers_on_phone);
    }

    /// Darryl, 2026-11-18: workers' shells may reach a local database. Only by its socket, only a
    /// database's, and only the human grants it.
    #[test]
    fn database_sockets_are_the_humans_and_only_a_databases() {
        for ok in ["/tmp/.s.PGSQL.5432", "/opt/homebrew/var/mysql/mysql.sock", "/tmp/mysqld.sock", "/tmp/redis.sock", "/var/run/redis/redis-server.sock"] {
            assert_eq!(check_socket(ok).as_deref(), Ok(ok), "{ok}");
        }
        for bad in [
            "/var/run/docker.sock",
            "/private/tmp/com.apple.launchd.x/Listeners",
            "/Users/u/.maiterm/cm/host.sock",
            "/tmp/.s.PGSQL.",
            "/tmp/.s.PGSQL.5432x",
            "tmp/.s.PGSQL.5432",
            "/tmp/../tmp/.s.PGSQL.5432",
            "/tmp/redis/../agent.sock",
            "/tmp/a\"b/.s.PGSQL.5432",
            "/tmp/a\\b/.s.PGSQL.5432",
        ] {
            assert!(check_socket(bad).is_err(), "{bad}");
        }
        let add = SettingsChange { add_sockets: vec![" /tmp/.s.PGSQL.5432 ".into(), "/tmp/.s.PGSQL.5432".into()], ..Default::default() };
        // The queen grants one too now (2026-11-21), under the Access checkpoint.
        assert!(check(&hive(), &add, By::Coordinator, &[]).is_ok());
        assert_eq!(parse_args(&json!({ "add_sockets": ["/tmp/.s.PGSQL.5432"] })).unwrap().0.add_sockets.len(), 1);
        let ch = check(&hive(), &add, By::Human, &[]).unwrap();
        assert_eq!(ch.change.add_sockets, vec!["/tmp/.s.PGSQL.5432".to_string()], "trimmed, one copy");
        let mut h = hive();
        apply_to(&mut h, &ch.change);
        assert_eq!(h.settings.worker_sockets, vec!["/tmp/.s.PGSQL.5432".to_string()]);
        let rm = SettingsChange { remove_sockets: vec!["/tmp/.s.PGSQL.5432".into()], ..Default::default() };
        let rm = check(&h, &rm, By::Human, &[]).unwrap().change;
        apply_to(&mut h, &rm);
        assert!(h.settings.worker_sockets.is_empty());
        let docker = SettingsChange { add_sockets: vec!["/var/run/docker.sock".into()], ..Default::default() };
        assert!(check(&hive(), &docker, By::Human, &[]).unwrap_err().contains("isn\u{2019}t named like a database"));
    }

    /// At launch a kept path must still lead to a database's socket: a link to another socket, or
    /// a plain file, is left out, and a socket is given as kept and as its realpath.
    #[cfg(unix)]
    #[test]
    fn a_launch_takes_a_socket_only_while_it_leads_to_a_database() {
        let base = std::env::temp_dir().join(format!("hs-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
        std::fs::create_dir_all(&base).unwrap();
        let base = std::fs::canonicalize(&base).unwrap();
        let real = base.join(".s.PGSQL.5999");
        let _listener = std::os::unix::net::UnixListener::bind(&real).unwrap();
        let other = base.join("agent.sock");
        let _other = std::os::unix::net::UnixListener::bind(&other).unwrap();
        let link = base.join("l");
        std::fs::create_dir_all(&link).unwrap();
        std::os::unix::fs::symlink(&other, link.join(".s.PGSQL.6000")).unwrap();
        std::fs::write(base.join("redis.sock"), "not a socket").unwrap();
        // A link whose target a Seatbelt profile can't name: left out, not passed on to fail a job.
        let quoted = base.join("q\"x");
        std::fs::create_dir_all(&quoted).unwrap();
        let _quoted = std::os::unix::net::UnixListener::bind(quoted.join(".s.PGSQL.6100")).unwrap();
        std::os::unix::fs::symlink(&quoted, base.join("ql")).unwrap();
        let mut h = hive();
        h.settings.worker_sockets = vec![
            real.to_string_lossy().into(),
            link.join(".s.PGSQL.6000").to_string_lossy().into(),
            base.join("redis.sock").to_string_lossy().into(),
            base.join(".s.PGSQL.7000").to_string_lossy().into(),
            base.join("ql").join(".s.PGSQL.6100").to_string_lossy().into(),
        ];
        assert_eq!(launch_sockets(&h), vec![real.to_string_lossy().to_string()], "the real one once; the link, the file, the missing one and the unnameable one left out");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn hosts_are_normalised_deduplicated_and_never_a_remote() {
        let c = SettingsChange {
            add_hosts: vec!["API.Example.com".into(), "api.example.com".into(), "docs.example.com".into()],
            remove_hosts: vec!["docs.example.com".into(), "never.there".into()],
            ..Default::default()
        };
        let ch = check(&hive(), &c, By::Coordinator, &[]).unwrap();
        assert_eq!(ch.change.add_hosts, vec!["api.example.com".to_string()], "one copy; a host also removed isn't re-added");
        assert_eq!(ch.change.remove_hosts, vec!["docs.example.com".to_string()], "only a host it has");
        let mut h = hive();
        apply_to(&mut h, &ch.change);
        assert_eq!(h.settings.worker_hosts, vec!["api.example.com".to_string()]);
        let remote = SettingsChange { add_hosts: vec!["github.com".into()], ..Default::default() };
        assert!(check(&hive(), &remote, By::Human, &["github.com".into()]).unwrap_err().contains("push"));
        let bad = SettingsChange { add_hosts: vec!["https://x.com/path".into()], ..Default::default() };
        assert!(check(&hive(), &bad, By::Human, &[]).is_err());
        for home in ["lvh.me", "*.nip.io", "127.0.0.1.nip.io", "app.localtest.me"] {
            let c = SettingsChange { add_hosts: vec![home.into()], ..Default::default() };
            assert!(check(&hive(), &c, By::Human, &[]).unwrap_err().contains("127.0.0.1"), "{home}");
        }
    }

    /// The queen grants hosts herself now (2026-10-31), within the limits Rust holds for anyone.
    #[test]
    fn the_queens_host_grant_keeps_every_hard_limit() {
        let add = |h: &str| SettingsChange { add_hosts: vec![h.into()], ..Default::default() };
        assert_eq!(check(&hive(), &add("shadcn-svelte.com"), By::Coordinator, &[]).unwrap().change.add_hosts, vec!["shadcn-svelte.com".to_string()]);
        assert!(check(&hive(), &add("github.com"), By::Coordinator, &["github.com".into()]).unwrap_err().contains("push"), "a repo remote's host");
        for refused in ["localhost", "app.localhost", "printer.local", "10.0.0.1", "127.0.0.1", "lvh.me", "x.com:8765", "https://x.com"] {
            assert!(check(&hive(), &add(refused), By::Coordinator, &[]).is_err(), "{refused}");
        }
        let mut full = hive();
        full.settings.worker_hosts = (0..MAX_WORKER_HOSTS).map(|i| format!("h{i}.example.com")).collect();
        assert!(check(&full, &add("one-more.example.com"), By::Coordinator, &[]).unwrap_err().contains("at most"));
    }

    #[test]
    fn a_private_address_is_this_computer_or_its_network() {
        for ip in ["127.0.0.1", "10.1.2.3", "192.168.1.5", "172.16.0.1", "169.254.1.1", "0.0.0.0", "100.101.1.2", "::1", "::", "fd00::1", "fe80::1", "::ffff:127.0.0.1"] {
            assert!(is_private_ip(&ip.parse().unwrap()), "{ip}");
        }
        for ip in ["8.8.8.8", "142.250.1.1", "2606:4700::1111"] {
            assert!(!is_private_ip(&ip.parse().unwrap()), "{ip}");
        }
    }

    #[test]
    fn models_and_efforts_are_from_the_lists_and_a_done_hive_refuses() {
        let c = SettingsChange { coordinator_model: Some("gpt".into()), ..Default::default() };
        assert!(check(&hive(), &c, By::Human, &[]).is_err());
        let c = SettingsChange { coordinator_effort: Some("max".into()), ..Default::default() };
        let ch = check(&hive(), &c, By::Coordinator, &[]).unwrap();
        let mut h = hive();
        apply_to(&mut h, &ch.change);
        assert_eq!((h.models.coordinator.model.as_str(), h.models.coordinator.effort.as_str()), ("opus", "max"));
        let mut done = hive();
        done.status = super::super::record::STATUS_DONE.into();
        assert!(check(&done, &c, By::Human, &[]).is_err());
    }

    #[test]
    fn the_workers_advisor_is_one_of_the_choices_and_follow_is_stored_empty() {
        let set = |a: &str| SettingsChange { worker_advisor: Some(a.into()), ..Default::default() };
        assert!(check(&hive(), &set("haiku"), By::Coordinator, &[]).is_err(), "never less capable than a worker");
        assert!(check(&hive(), &set("follow"), By::Human, &[]).is_err(), "a new Hive already follows: nothing changes");
        let mut h = hive();
        let ch = check(&h, &set("Fable"), By::Coordinator, &[]).unwrap();
        assert!(ch.lines[0].contains("as in your Claude settings \u{2192} Claude fable"), "{:?}", ch.lines);
        apply_to(&mut h, &ch.change);
        assert_eq!(h.settings.worker_advisor, "fable");
        assert!(needs_worker_relaunch(&ch.change));
        let ch = check(&h, &set("follow"), By::Human, &[]).unwrap();
        apply_to(&mut h, &ch.change);
        assert_eq!(h.settings.worker_advisor, "", "follow is stored as the default");

        // Workers are on sonnet: a sonnet advisor is fine, and opus workers would outrank it.
        let ch = check(&h, &set("sonnet"), By::Human, &[]).unwrap();
        apply_to(&mut h, &ch.change);
        let opus = SettingsChange { worker_model: Some("opus".into()), ..Default::default() };
        assert!(check(&h, &opus, By::Coordinator, &[]).unwrap_err().contains("can\u{2019}t advise"), "an advisor that would be skipped");
        let both = SettingsChange { worker_model: Some("opus".into()), worker_advisor: Some("fable".into()), ..Default::default() };
        assert!(check(&h, &both, By::Coordinator, &[]).is_ok());
    }

    /// §3.6: the queen sets a new project's commands as it grows. A land check follows the
    /// checkpoint; a setup runs outside the sandbox, so only the human's approval applies it.
    #[test]
    fn a_folders_commands_change_and_only_the_human_approves_a_setup() {
        let mut h = hive();
        h.folders = vec![
            super::super::record::HiveFolder { name: "web".into(), kind: super::super::record::KIND_GIT.into(), land_check: Some("npm test".into()), ..Default::default() },
            super::super::record::HiveFolder { name: "brand".into(), kind: super::super::record::KIND_PLAIN.into(), ..Default::default() },
        ];
        let fc = |setup: Option<&str>, land: Option<&str>| SettingsChange {
            folders: vec![FolderChange { name: "web".into(), setup: setup.map(Into::into), land_check: land.map(Into::into) }],
            ..Default::default()
        };

        let land = check(&h, &fc(None, Some(" cargo test ")), By::Coordinator, &[]).unwrap();
        assert_eq!(land.change.folders[0].land_check.as_deref(), Some("cargo test"), "trimmed");
        assert!(land.lines[0].contains("`npm test` \u{2192} `cargo test`"), "{:?}", land.lines);

        let setup = fc(Some("npm ci"), None);
        // Under Settings handed to her, the queen sets a setup too (2026-11-21); the card says it
        // runs outside the sandbox when she asks.
        assert!(check(&h, &setup, By::Coordinator, &[]).is_ok());
        let ok = check(&h, &setup, By::Approved, &[]).unwrap();
        assert!(ok.lines[0].contains("outside the sandbox") && ok.lines[0].contains("none \u{2192} `npm ci`"), "{:?}", ok.lines);
        apply_to(&mut h, &ok.change);
        assert_eq!(h.folders[0].setup.as_deref(), Some("npm ci"));

        assert!(check(&h, &fc(Some("npm ci"), Some("npm test")), By::Human, &[]).unwrap_err().starts_with("Nothing would change"));
        let cleared = check(&h, &fc(None, Some("")), By::Coordinator, &[]).unwrap();
        apply_to(&mut h, &cleared.change);
        assert_eq!((h.folders[0].land_check.clone(), h.folders[0].setup.as_deref()), (None, Some("npm ci")), "\"\" clears; left out stays");

        assert!(check(&h, &fc(Some("npm ci\nrm -rf ~"), None), By::Human, &[]).unwrap_err().contains("line break"));
        assert!(check(&h, &fc(None, Some(&"x".repeat(MAX_FOLDER_COMMAND + 1))), By::Human, &[]).is_err());
        let plain = SettingsChange { folders: vec![FolderChange { name: "brand".into(), land_check: Some("true".into()), ..Default::default() }], ..Default::default() };
        assert!(check(&h, &plain, By::Human, &[]).unwrap_err().contains("its git folders: web"), "a plain folder has no commands");
    }

    #[test]
    fn the_stored_change_round_trips_through_a_card() {
        let c = SettingsChange { cap: Some(4), add_hosts: vec!["api.example.com".into()], ..Default::default() };
        let v = json!({ "hive_id": "h1", "change": c });
        assert_eq!(v["change"], json!({ "cap": 4, "add_hosts": ["api.example.com"] }));
        let back: SettingsChange = serde_json::from_value(v["change"].clone()).unwrap();
        assert_eq!(back, c);
    }

    /// The review of the queen's own host grant (2026-10-31): `coordinator_hosts` runs `check`
    /// and `write` as the human's change does, so every hard limit holds for her: loopback in
    /// every spelling, private ranges, loopback DNS, a port and a repo remote's host are refused;
    /// a trailing dot and capitals are normalised.
    #[test]
    fn the_queens_grant_refuses_every_home_spelling_and_a_remote() {
        let add = |h: &str| SettingsChange { add_hosts: vec![h.into()], ..Default::default() };
        let remotes = ["github.com".to_string()];
        for h in ["localhost", "127.0.0.1", "[::1]", "::ffff:127.0.0.1", "2130706433", "0x7f000001", "10.0.0.5", "192.168.1.2", "169.254.1.1", "localtest.me", "lvh.me", "x.nip.io", "example.com:8080", "github.com", "GitHub.com."] {
            assert!(check(&hive(), &add(h), By::Coordinator, &remotes).is_err(), "{h}");
        }
        for h in ["example.com.", "EXAMPLE.com"] {
            assert_eq!(check(&hive(), &add(h), By::Coordinator, &remotes).unwrap().change.add_hosts, vec!["example.com".to_string()], "{h}");
        }
        // A name under the remote, a wildcard over it, or its registrable domain: ssh.github.com:443
        // is GitHub's SSH push endpoint.
        for h in ["ssh.github.com", "*.github.com", "api.github.com"] {
            assert!(check(&hive(), &add(h), By::Coordinator, &remotes).unwrap_err().contains("push"), "{h}");
        }
        let uk = ["git.example.co.uk".to_string()];
        assert!(check(&hive(), &add("ssh.example.co.uk"), By::Coordinator, &uk).is_err(), "a two-part suffix keeps its registrable domain");
        assert!(check(&hive(), &add("other.co.uk"), By::Coordinator, &uk).is_ok(), "but not every .co.uk");
        assert!(check(&hive(), &add("raw.githubusercontent.com"), By::Coordinator, &remotes).is_ok(), "another domain");
        // An ssh alias ssh didn't resolve (`gh:owner/repo`): no forge is grantable.
        let alias = ["gh".to_string()];
        for h in ["github.com", "ssh.github.com", "gitlab.com"] {
            assert!(check(&hive(), &add(h), By::Coordinator, &alias).is_err(), "{h}");
        }
        assert!(check(&hive(), &add("registry.npmjs.org"), By::Coordinator, &alias).is_ok());
        // The final review: a remote maiTerm can't name exactly leaves no forge grantable.
        for (remote, h) in [
            ("140.82.112.3", "github.com"),
            ("github.com-work", "github.com"),
            ("github.com-work", "ssh.github.com"),
            ("corp", "git.corp.example.com"),
            ("?gitserver.example.org", "gitlab.com"),
        ] {
            assert!(check(&hive(), &add(h), By::Coordinator, &[remote.to_string()]).is_err(), "{h} with remote {remote}");
        }
        for (remote, h) in [("github.com-work", "registry.npmjs.org"), ("corp", "api.example.org"), ("140.82.112.3", "pypi.org")] {
            assert!(check(&hive(), &add(h), By::Coordinator, &[remote.to_string()]).is_ok(), "{h} with remote {remote}");
        }
        // One rule wherever hosts are allowed: the gallery's generator hosts too.
        assert!(super::super::gallery::budget::check_hosts(&["ssh.github.com".into()], &["github.com".into()]).is_err());
        assert!(super::super::gallery::budget::check_hosts(&["github.com".into()], &["140.82.112.3".into()]).is_err());
    }

    /// The queen grants alone, so a name that doesn't resolve now is refused for her; the human's
    /// own change may still add one.
    #[tokio::test]
    async fn the_queens_grant_needs_a_name_that_resolves() {
        let name = "nothing-here.maiterm-review.invalid".to_string();
        assert!(refuse_private_hosts(&[name.clone()], true, &[]).await.unwrap_err().contains("doesn\u{2019}t resolve yet"));
        assert!(refuse_private_hosts(&[name], false, &[]).await.is_ok());
        // A name that answers a remote given as an IP reaches that remote.
        let public = "answers-the-remote.maiterm-review.invalid".to_string();
        TEST_PUBLIC.lock().unwrap().push(public.clone());
        let remote_ip = ip_remotes(&["8.8.8.8".to_string(), "github.com".to_string()]);
        assert_eq!(remote_ip, vec![std::net::IpAddr::from([8, 8, 8, 8])]);
        assert!(refuse_private_hosts(&[public.clone()], true, &remote_ip).await.unwrap_err().contains("git remote"));
        assert!(refuse_private_hosts(&[public], true, &[]).await.is_ok());
    }

    /// The review of the remote-host fix (2026-10-31): forms it closes.
    #[test]
    fn review_remote_host_forms_it_closes() {
        let add = |h: &str| SettingsChange { add_hosts: vec![h.into()], ..Default::default() };
        let gh = ["github.com".to_string()];
        // A unicode spelling isn't a host name at all; a remote's port never reaches url_host.
        for h in ["gïthub.com", "github.com:22", "*.com", "com"] {
            assert!(check(&hive(), &add(h), By::Coordinator, &gh).is_err(), "{h}");
        }
        assert_eq!(super::super::settings::url_host("ssh://git@GitHub.com:2222/o/r").as_deref(), Some("github.com"));
        assert_eq!(super::super::settings::url_host("git@github.com:o/r").as_deref(), Some("github.com"));
        // A forge's sibling under an unrelated label is another domain.
        assert!(check(&hive(), &add("github.io"), By::Coordinator, &gh).is_ok());
    }
}
