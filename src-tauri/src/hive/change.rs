//! Changing a Hive's settings after Create (docs/hive.md §3.4): workers at once, the coordinator's
//! and the workers' model and effort, the hosts every builder's sandbox may reach, and (the human
//! only) push after land. The human changes them in Hive settings (`hive_set_settings`). The
//! coordinator asks with `hiveChangeSettings`: under the Settings checkpoint's Ask me (the
//! default) that files a card only the human approves (an agent request of kind `hive_settings`,
//! carried out here from its stored arguments); handed to the coordinator, it applies at once and
//! the human is told. All three go through `check`, against the record as it is when they apply.
//!
//! A change reaches an agent at its next launch, which rewrites its settings (§11.1): a running
//! worker keeps its model and its network until it is relaunched, and a cap lowered under the
//! workers running stops nobody, it only holds new ones back.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::checkpoints::{self, SETTINGS};
use super::deliver::one_line;
use super::fuses::STATUS_PAUSED;
use super::gallery::budget::{host_covers, normalize_host};
use super::record::{Hive, HiveModel, STATUS_ACTIVE};
use super::rpc::{HiveRequest, HiveWindow};
use super::tools::{Caller, Ctx};

/// What `hiveChangeSettings` files and approves: the request kind (`agent_requests::KINDS`).
pub const KIND: &str = "hive_settings";
/// The window's word that the coordinator changed settings on its own: a notice for the human.
pub const SETTINGS_CHANGED_EVENT: &str = "hive-settings-changed";
pub const MODELS: [&str; 3] = ["opus", "sonnet", "haiku"];
pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
/// Workers at once, whoever sets it: past this, no machine runs them.
pub const MAX_CAP: u32 = 32;
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
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub add_hosts: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub remove_hosts: Vec<String>,
    /// The human's alone: a push leaves this computer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub push_after_land: Option<bool>,
    /// The human's alone: which of the Hive's agents are chats on the phone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workers_on_phone: Option<bool>,
}

/// Who a change comes from, which sets its limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum By {
    /// Hive settings: anything here, the cap past the machine's ceiling included.
    Human,
    /// The human's approval of the coordinator's card: the human's limits, on the coordinator's fields.
    Approved,
    /// `hiveChangeSettings` with the checkpoint handed over: never past the ceiling, never push.
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
/// repos push to, which no worker may reach (§7.3); `ceiling` is this computer's cap ceiling.
pub fn check(hive: &Hive, c: &SettingsChange, by: By, ceiling: u32, remote_hosts: &[String]) -> Result<Checked, String> {
    if hive.status == super::record::STATUS_DONE {
        return Err("This Hive is done: a message to its coordinator wakes it first.".to_string());
    }
    let mut out = SettingsChange::default();
    let mut lines = Vec::new();

    if let Some(cap) = c.cap {
        if cap == 0 || cap > MAX_CAP {
            return Err(format!("Workers at once is 1 to {MAX_CAP}."));
        }
        if by == By::Coordinator && cap > ceiling.max(hive.cap) {
            return Err(format!(
                "{cap} workers at once is past this computer\u{2019}s ceiling of {ceiling}: only your human can go past it. With the \
                 Settings checkpoint at Ask me, ask on a card instead."
            ));
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
    let cm = pick(&c.coordinator_model, &MODELS, "The coordinator\u{2019}s model")?.filter(|m| *m != hive.models.coordinator.model);
    let ce = pick(&c.coordinator_effort, &EFFORTS, "The coordinator\u{2019}s effort")?.filter(|e| *e != hive.models.coordinator.effort);
    if let Some(l) = model_line("Coordinator (from its next launch)", &hive.models.coordinator, &cm, &ce) {
        lines.push(l);
    }
    let wm = pick(&c.worker_model, &MODELS, "The workers\u{2019} model")?.filter(|m| *m != hive.models.worker.model);
    let we = pick(&c.worker_effort, &EFFORTS, "The workers\u{2019} effort")?.filter(|e| *e != hive.models.worker.effort);
    if let Some(l) = model_line("Workers (each from its next launch)", &hive.models.worker, &wm, &we) {
        lines.push(l);
    }
    (out.coordinator_model, out.coordinator_effort, out.worker_model, out.worker_effort) = (cm, ce, wm, we);

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
        if let Some(r) = remote_hosts.iter().find(|r| host_covers(&h, &r.to_ascii_lowercase())) {
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
        if by != By::Human {
            return Err("Only your human turns pushing after a land on or off.".to_string());
        }
        if p != hive.push_after_land {
            out.push_after_land = Some(p);
            lines.push(format!("Push after land: {}", if p { "on" } else { "off" }));
        }
    }

    if let Some(v) = c.workers_on_phone {
        if by != By::Human {
            return Err("Only your human chooses what shows on their phone.".to_string());
        }
        if v != hive.settings.workers_on_phone {
            out.workers_on_phone = Some(v);
            lines.push(format!("On the phone: {} (workers started from now on)", if v { "the coordinator and its workers" } else { "the coordinator only" }));
        }
    }

    if lines.is_empty() {
        return Err("Nothing would change: every setting named is already so.".to_string());
    }
    Ok(Checked { change: out, lines })
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
    let hosts = &mut hive.settings.worker_hosts;
    hosts.retain(|h| normalize_host(h).map_or(true, |n| !c.remove_hosts.contains(&n)));
    hosts.extend(c.add_hosts.iter().cloned());
    if let Some(p) = c.push_after_land {
        hive.push_after_land = p;
    }
    if let Some(v) = c.workers_on_phone {
        hive.settings.workers_on_phone = v;
    }
}

/// Checked against the record as it is under the write lock, then saved and announced. The new
/// record and the lines that describe what changed.
fn write<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, c: &SettingsChange, by: By, ceiling: u32) -> Result<(Hive, Vec<String>), String> {
    let remotes = super::settings::hive_remote_hosts(&caller.hive);
    let mut outcome: Result<Vec<String>, String> = Err("This Hive is gone.".to_string());
    // A refused check leaves the record as it was (saved again, unchanged: harmless).
    let hive = super::finish::write_status(ctx, caller, &[STATUS_ACTIVE, STATUS_PAUSED], None, |h| {
        outcome = check(h, c, by, ceiling, &remotes).map(|ch| {
            apply_to(h, &ch.change);
            ch.lines
        });
    })?;
    Ok((hive, outcome?))
}

fn log_line(caller: &Caller, by: &str, lines: &[String], reason: Option<&str>) {
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

fn bulleted(lines: &[String]) -> String {
    lines.iter().map(|l| format!("\n- {l}")).collect()
}

/// "Home", as maiTerm's servers count it (`host_guard::is_home`): what they answer is exactly
/// what no worker may reach.
fn is_private_ip(ip: &std::net::IpAddr) -> bool {
    crate::host_guard::is_home(ip)
}

/// A host added now must not answer this computer or its network (§6.7): a public name for
/// 127.0.0.1 in a worker's allowlist opens every local port, maiTerm's own included. Names on
/// `LOOPBACK_DOMAINS` are refused by `normalize_host`; the rest are looked up here, a `*.` entry
/// by a name under it. A name that doesn't resolve passes: its owner could point it home later,
/// which is why a host is a decision the human makes by default (the Settings checkpoint).
pub(crate) async fn refuse_private_hosts(hosts: &[String]) -> Result<(), String> {
    for h in hosts {
        let Ok(h) = normalize_host(h) else { continue };
        let name = match h.strip_prefix("*.") {
            Some(base) => format!("maiterm-probe.{base}"),
            None => h.clone(),
        };
        let lookup = tokio::time::timeout(std::time::Duration::from_secs(3), tokio::net::lookup_host(format!("{name}:443"))).await;
        let Ok(Ok(addrs)) = lookup else { continue };
        if let Some(ip) = addrs.map(|a| a.ip()).find(is_private_ip) {
            return Err(format!("{h} answers {ip}, this computer or its network: a worker\u{2019}s localhost and LAN stay closed."));
        }
    }
    Ok(())
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
    if change.push_after_land.take().is_some() {
        return Err("Only your human turns pushing after a land on or off.".to_string());
    }
    if change.workers_on_phone.take().is_some() {
        return Err("Only your human chooses what shows on their phone.".to_string());
    }
    let reason = args.get("reason").and_then(Value::as_str).map(str::trim).filter(|r| !r.is_empty()).map(|r| one_line(r, MAX_REASON));
    Ok((change, reason))
}

pub fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveChangeSettings",
        "description": format!(
            "Change this Hive\u{2019}s settings: workers at once (cap), the coordinator\u{2019}s or the workers\u{2019} model and effort, \
             and the hosts every builder\u{2019}s sandbox may reach beyond the package registries (an API, its docs). Name only \
             what changes. Under the Settings checkpoint\u{2019}s Ask me (see your brief) this files a card your human approves or \
             denies, and maiTerm tells you which; handed to you, it applies at once and your human is told. A change reaches each \
             agent at its next launch: a running worker keeps its model and network until it is relaunched. Never past this \
             computer\u{2019}s worker ceiling on your own, and never pushing. Models: {}. Efforts: {}.",
            MODELS.join(", "),
            EFFORTS.join(", ")
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
                "add_hosts": { "type": "array", "items": { "type": "string" }, "description": "Bare host names (api.example.com, *.example.com) every builder may reach" },
                "remove_hosts": { "type": "array", "items": { "type": "string" } },
                "reason": { "type": "string", "description": "Why: your human reads it on the card" }
            },
            "required": ["reason"]
        }
    })
}

/// `hiveChangeSettings` (§3.4).
pub(crate) async fn change_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let (change, reason) = parse_args(args)?;
    refuse_private_hosts(&change.add_hosts).await?;
    let hive = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    let ceiling = super::create::machine_ceiling();
    if !checkpoints::asks(&hive, SETTINGS) {
        let (_, lines) = write(ctx, caller, &change, By::Coordinator, ceiling)?;
        log_line(caller,"coordinator", &lines, reason.as_deref());
        ctx.window.settings_changed(&caller.workspace_id, &format!("The coordinator changed this Hive\u{2019}s settings: {}.", lines.join("; ")));
        ctx.window.poll_waiters(&caller.workspace_id);
        return Ok(json!({ "status": "applied", "changes": lines, "note": "Your human was told. Each agent gets it at its next launch." }));
    }
    // The human's limits apply on approval, but a card that can't apply now isn't filed.
    let checked = check(&hive, &change, By::Approved, ceiling, &super::settings::hive_remote_hosts(&hive))?;
    let mut effects = checked.lines.clone();
    if checked.change.cap.is_some_and(|c| c > ceiling) {
        effects.push(format!("That is past this computer\u{2019}s ceiling of {ceiling} workers at once: expect it to slow down or run short of memory."));
    }
    if !checked.change.add_hosts.is_empty() {
        effects.push("A host a worker can reach can also receive what it sends: only hosts you trust with this project\u{2019}s code.".to_string());
    }
    let req = HiveRequest {
        tab_id: caller.tab_id.clone(),
        kind: KIND,
        args: json!({ "hive_id": hive.id, "change": checked.change }),
        summary: format!("Change the Hive\u{2019}s settings ({})", one_line(&caller.workstream, 80)),
        effects,
        reason,
        keep: None,
    };
    let id = ctx.window.file_request(req)?;
    log::info!("hive {}: settings change {id} waits for the human: {}", hive.id, checked.lines.join("; "));
    Ok(json!({
        "status": "awaiting_approval",
        "request_id": id,
        "changes": checked.lines,
        "note": "Your human approves or denies it on a card; maiTerm tells you which. Carry on meanwhile.",
    }))
}

/// Hive settings: the human's own change. The coordinator is told.
pub(crate) async fn set_by_human<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, change: &SettingsChange) -> Result<Hive, String> {
    refuse_private_hosts(&change.add_hosts).await?;
    let (hive, lines) = write(ctx, caller, change, By::Human, super::create::machine_ceiling())?;
    log_line(caller,"human", &lines, None);
    tell_coordinator(ctx, caller, format!("[maiTerm] Your human changed this Hive\u{2019}s settings:{}", bulleted(&lines))).await;
    ctx.window.poll_waiters(&caller.workspace_id);
    Ok(hive)
}

/// The human approved the coordinator's card: applied from the stored arguments, re-checked
/// against the record as it is now.
pub(crate) async fn approved_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, request_id: &str, args: &Value) {
    let change: Result<SettingsChange, String> =
        args.get("change").cloned().ok_or_else(|| "it had no change".to_string()).and_then(|v| serde_json::from_value(v).map_err(|e| e.to_string()));
    let res = match change {
        Ok(c) => match refuse_private_hosts(&c.add_hosts).await {
            Ok(()) => write(ctx, caller, &c, By::Approved, super::create::machine_ceiling()),
            Err(e) => Err(e),
        },
        Err(e) => Err(e),
    };
    match res {
        Ok((_, lines)) => {
            log_line(caller,"coordinator, approved by the human", &lines, None);
            ctx.window.settle_request(request_id, true, format!("applied: {}", lines.join("; ")));
            tell_coordinator(ctx, caller, format!("[maiTerm] Your human approved your settings change:{}", bulleted(&lines))).await;
            ctx.window.poll_waiters(&caller.workspace_id);
        }
        Err(e) => {
            ctx.window.settle_request(request_id, false, e.clone());
            tell_coordinator(ctx, caller, format!("[maiTerm] Your human approved your settings change, but it couldn\u{2019}t apply: {e}")).await;
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
        approved_in(&ctx, &caller, &r.id, &r.args).await;
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
        let ch = check(&hive(), &c, By::Coordinator, 5, &[]).unwrap();
        assert_eq!(ch.change.cap, None, "the cap is already 3");
        assert_eq!(ch.change.worker_model.as_deref(), Some("opus"), "lower-cased");
        assert_eq!(ch.change.worker_effort, None, "workers are already at high effort");
        assert_eq!(ch.lines, vec!["Workers (each from its next launch): Claude sonnet, high effort \u{2192} Claude opus, high effort".to_string()]);
        let none = SettingsChange { cap: Some(3), ..Default::default() };
        assert!(check(&hive(), &none, By::Human, 5, &[]).unwrap_err().starts_with("Nothing would change"));
    }

    #[test]
    fn the_coordinator_stays_under_the_ceiling_and_never_pushes() {
        let up = SettingsChange { cap: Some(8), ..Default::default() };
        assert!(check(&hive(), &up, By::Coordinator, 5, &[]).unwrap_err().contains("ceiling of 5"));
        assert_eq!(check(&hive(), &up, By::Approved, 5, &[]).unwrap().change.cap, Some(8), "the human may go past it");
        assert_eq!(check(&hive(), &up, By::Human, 5, &[]).unwrap().change.cap, Some(8));
        assert!(check(&hive(), &SettingsChange { cap: Some(MAX_CAP + 1), ..Default::default() }, By::Human, 5, &[]).is_err());
        assert!(check(&hive(), &SettingsChange { cap: Some(0), ..Default::default() }, By::Human, 5, &[]).is_err());
        let push = SettingsChange { push_after_land: Some(true), ..Default::default() };
        assert!(check(&hive(), &push, By::Coordinator, 5, &[]).is_err());
        assert!(check(&hive(), &push, By::Approved, 5, &[]).is_err());
        assert!(check(&hive(), &push, By::Human, 5, &[]).is_ok());
        let args = json!({ "push_after_land": true, "reason": "ship it" });
        assert!(parse_args(&args).is_err(), "the tool refuses push outright");
        let phone = SettingsChange { workers_on_phone: Some(true), ..Default::default() };
        assert!(check(&hive(), &phone, By::Approved, 5, &[]).is_err());
        let mut h = hive();
        apply_to(&mut h, &check(&hive(), &phone, By::Human, 5, &[]).unwrap().change);
        assert!(h.settings.workers_on_phone);
    }

    #[test]
    fn hosts_are_normalised_deduplicated_and_never_a_remote() {
        let c = SettingsChange {
            add_hosts: vec!["API.Example.com".into(), "api.example.com".into(), "docs.example.com".into()],
            remove_hosts: vec!["docs.example.com".into(), "never.there".into()],
            ..Default::default()
        };
        let ch = check(&hive(), &c, By::Coordinator, 5, &[]).unwrap();
        assert_eq!(ch.change.add_hosts, vec!["api.example.com".to_string()], "one copy; a host also removed isn't re-added");
        assert_eq!(ch.change.remove_hosts, vec!["docs.example.com".to_string()], "only a host it has");
        let mut h = hive();
        apply_to(&mut h, &ch.change);
        assert_eq!(h.settings.worker_hosts, vec!["api.example.com".to_string()]);
        let remote = SettingsChange { add_hosts: vec!["github.com".into()], ..Default::default() };
        assert!(check(&hive(), &remote, By::Human, 5, &["github.com".into()]).unwrap_err().contains("push"));
        let bad = SettingsChange { add_hosts: vec!["https://x.com/path".into()], ..Default::default() };
        assert!(check(&hive(), &bad, By::Human, 5, &[]).is_err());
        for home in ["lvh.me", "*.nip.io", "127.0.0.1.nip.io", "app.localtest.me"] {
            let c = SettingsChange { add_hosts: vec![home.into()], ..Default::default() };
            assert!(check(&hive(), &c, By::Human, 5, &[]).unwrap_err().contains("127.0.0.1"), "{home}");
        }
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
        assert!(check(&hive(), &c, By::Human, 5, &[]).is_err());
        let c = SettingsChange { coordinator_effort: Some("max".into()), ..Default::default() };
        let ch = check(&hive(), &c, By::Coordinator, 5, &[]).unwrap();
        let mut h = hive();
        apply_to(&mut h, &ch.change);
        assert_eq!((h.models.coordinator.model.as_str(), h.models.coordinator.effort.as_str()), ("opus", "max"));
        let mut done = hive();
        done.status = super::super::record::STATUS_DONE.into();
        assert!(check(&done, &c, By::Human, 5, &[]).is_err());
    }

    #[test]
    fn the_stored_change_round_trips_through_a_card() {
        let c = SettingsChange { cap: Some(4), add_hosts: vec!["api.example.com".into()], ..Default::default() };
        let v = json!({ "hive_id": "h1", "change": c });
        assert_eq!(v["change"], json!({ "cap": 4, "add_hosts": ["api.example.com"] }));
        let back: SettingsChange = serde_json::from_value(v["change"].clone()).unwrap();
        assert_eq!(back, c);
    }
}
