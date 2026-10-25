//! The budget for paid generation (docs/hive.md §10.3, §7.4, §7.8): a rule agents follow and a
//! total the human watches, never a wall. The ledger's integer micro-dollars are the only total
//! (plan D3); recording is never refused, over the budget included, since the money is spent.
//!
//! Crossing it files the human's own `action` blocker on a "Hive budget reached" row of the
//! coordinator's, as the pause does (`fuses::file_row_blocker`), once while that row stands; the
//! human raising the budget past the spend closes it. A worker's "may I spend?" is a `decision`
//! blocker whose question starts `Budget:`, which `is_budget_question` keeps with the human
//! whatever the checkpoints say (plan D4: no new blocker kind, which an older build would drop).
//!
//! Also here, the human's gallery settings (`hive_set_gallery_settings`): the budget, the output
//! folders `galleryAdd` files from, and the generator hosts a worker's sandbox reaches (§14.16,
//! plan D2). Each host is also a channel a file can leave by.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::Gallery;
use crate::hive::paths::{self, HivePaths};
use crate::hive::record::Hive;
use crate::hive::rpc::HiveWindow;
use crate::hive::tools::{blocking, Caller, Ctx};
use crate::hive::ROLE_COORDINATOR;
use crate::state::workspace::{TaskBlocker, TaskNote, TASK_NOTE_CAP};
use crate::state::{AppData, AppState, Task, Workspace, Workstream};

/// The row the human's budget blocker sits on, on the coordinator's tab.
pub const BUDGET_TASK_TITLE: &str = "Hive budget reached";
/// $100,000: past that a typo, not a budget.
pub const MAX_BUDGET_CENTS: u64 = 10_000_000;
pub const MAX_GENERATOR_HOSTS: usize = 50;
pub const MAX_OUTPUT_FOLDERS: usize = 20;
const MICROS_PER_CENT: i64 = 10_000;

/// The Hive's spend as its header shows it (§7.8): this Hive's total and budget, and every
/// Hive's total, which the header shows only when another Hive has spent (plan D26).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SpendView {
    pub spent_micros: i64,
    pub budget_micros: i64,
    pub all_hives_micros: i64,
    pub over: bool,
}

pub fn budget_micros(hive: &Hive) -> i64 {
    i64::try_from(hive.budget_cents).unwrap_or(i64::MAX / MICROS_PER_CENT).saturating_mul(MICROS_PER_CENT)
}

/// Spent to the budget or past it. Nothing spent is never over, even on a $0 budget.
pub fn is_over(spent: i64, budget: i64) -> bool {
    spent > 0 && spent >= budget
}

pub fn view(spent: i64, hive: &Hive, all: i64) -> SpendView {
    let budget = budget_micros(hive);
    SpendView { spent_micros: spent, budget_micros: budget, all_hives_micros: all.max(spent), over: is_over(spent, budget) }
}

/// The Hive's total from its ledger, 0 while nothing was ever filed (no index): a launch or a
/// board read never makes a gallery.
pub fn spent_of(hive_id: &str, paths: &HivePaths) -> i64 {
    spent_in(super::view::existing(hive_id, paths).as_deref(), hive_id)
}

pub(crate) fn spent_in(g: Option<&Gallery>, hive_id: &str) -> i64 {
    match g.map(Gallery::spent_micros) {
        Some(Ok(s)) => s,
        Some(Err(e)) => {
            log::warn!("hive {hive_id}: spend not read: {e}");
            0
        }
        None => 0,
    }
}

/// Every Hive in the state, in every window.
pub fn hive_ids(data: &AppData) -> Vec<String> {
    data.windows.iter().flat_map(|w| w.workspaces.iter()).filter_map(|ws| ws.hive.as_ref().map(|h| h.id.clone())).collect()
}

/// These Hives' ledgers (`hive_ids`), summed. Blocks: one index read per Hive with a gallery.
pub fn all_hives_spent(ids: &[String]) -> i64 {
    ids.iter()
        .filter_map(|id| paths::hive_root(id).map(|r| spent_of(id, &HivePaths::new(r))))
        .fold(0i64, i64::saturating_add)
}

/// Dollars as the header and maiTerm's messages say them: "$10.40", "$1,250.00"; under a cent to
/// the tenth of a cent, "$0.003", so a run of cheap frames doesn't read as nothing.
pub fn usd(micros: i64) -> String {
    let sign = if micros < 0 { "-" } else { "" };
    let m = micros.unsigned_abs();
    if m > 0 && m < MICROS_PER_CENT as u64 {
        let mills = ((m + 500) / 1000).max(1);
        return format!("{sign}$0.{mills:03}");
    }
    let cents = (m + MICROS_PER_CENT as u64 / 2) / MICROS_PER_CENT as u64;
    let dollars = (cents / 100).to_string();
    let mut grouped = String::new();
    for (i, c) in dollars.chars().enumerate() {
        if i > 0 && (dollars.len() - i).is_multiple_of(3) {
            grouped.push(',');
        }
        grouped.push(c);
    }
    format!("{sign}${grouped}.{:02}", cents % 100)
}

/// A question about money (plan D4): it starts `Budget:`, in any case. Mislabelling only ever
/// sends a question to the human, the safe direction (§7.4).
pub fn is_budget_question(text: &str) -> bool {
    text.trim_start().get(..7).is_some_and(|p| p.eq_ignore_ascii_case("budget:"))
}

pub fn is_budget_blocker(b: &TaskBlocker) -> bool {
    is_budget_question(&b.question)
}

// ── Crossing ──

fn is_budget_row(t: &Task, coordinator: Option<&str>) -> bool {
    t.tab_id.as_deref() == coordinator && t.normalized_title == Task::normalize_title(BUDGET_TASK_TITLE)
}

fn is_open(t: &Task) -> bool {
    t.status == "blocked" && t.blocker.is_some()
}

pub fn crossing_blocker(spent: i64, budget: i64, now: &str) -> TaskBlocker {
    TaskBlocker {
        kind: "action".into(),
        question: format!(
            "This Hive has spent {} of its {} budget for paid generation. Raise the budget (Hive header \u{2192} Budget) or tell the \
             coordinator to stop paid generation.",
            usd(spent),
            usd(budget)
        ),
        context: Some(
            "maiTerm counts the costs agents record with their renders, so the total is self-reported, and renders already under \
             way can carry it further. The provider account's own credit is the only hard limit."
                .into(),
        ),
        options: Vec::new(),
        command: None,
        asked_at: now.into(),
        asked_by: "agent".into(),
    }
}

/// Files the crossing on the coordinator's row when no budget row is open already: one question
/// however many spends cross. The tasks to send the store, or None when nothing was written.
pub fn file_crossing(ws: &mut Workspace, coordinator: Option<&str>, spent: i64, budget: i64, now: &str) -> Option<(Vec<Task>, Vec<Workstream>)> {
    if !is_over(spent, budget) || ws.tasks.iter().any(|t| is_budget_row(t, coordinator) && is_open(t)) {
        return None;
    }
    let b = crossing_blocker(spent, budget, now);
    let note = format!("Needs you: {}", b.question);
    crate::hive::fuses::file_row_blocker(ws, coordinator, BUDGET_TASK_TITLE, b, &note, now);
    Some((ws.tasks.clone(), ws.workstreams.clone()))
}

/// A raise past the spend answers the crossing: its row is done, which clears the question
/// everywhere. Any row not yet done, not only a Blocked one: the card's generic "I've done it"
/// moves it to Active without a raise. False when no such row stands or the budget still doesn't
/// cover the spend.
pub fn close_on_raise(ws: &mut Workspace, coordinator: Option<&str>, spent: i64, budget: i64, now: &str) -> bool {
    if is_over(spent, budget) {
        return false;
    }
    let Some(t) = ws.tasks.iter_mut().find(|t| is_budget_row(t, coordinator) && !matches!(t.status.as_str(), "done" | "dropped")) else {
        return false;
    };
    t.status = "done".into();
    t.settle_blocker();
    t.notes.push(TaskNote { at: now.into(), text: format!("Budget raised by the human to {}", usd(budget)), by: "human".into() });
    let over = t.notes.len().saturating_sub(TASK_NOTE_CAP);
    t.notes.drain(..over);
    t.updated_at = now.into();
    true
}

fn coordinator_of(ws: &Workspace) -> Option<String> {
    ws.panes.iter().flat_map(|p| p.tabs.iter()).find(|t| t.hive_role.as_deref() == Some(ROLE_COORDINATOR)).map(|t| t.id.clone())
}

pub fn crossing_notice(spent: i64, budget: i64) -> String {
    format!(
        "[maiTerm] Budget reached: {} of {}. No paid render until your human raises it; maiTerm has asked them, don't ask again.",
        usd(spent),
        usd(budget)
    )
}

/// After a call that recorded a spend: at or past the budget, the human is asked (once while the
/// question stands) and the coordinator told, unless it made the spend, whose answer says so.
/// Checked against the budget as the state has it now, under its lock, so two crossings at once
/// file one row. Whether a row was filed.
pub(crate) async fn after_spend<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, by_coordinator: bool, spent: i64) -> bool {
    let now = crate::mailink::board::iso_now();
    let filed = {
        let mut d = ctx.data.write();
        let Ok(ws) = crate::hive::tools::workspace_mut(&mut d, caller) else { return false };
        let Some(budget) = ws.hive.as_ref().map(budget_micros) else { return false };
        let coordinator = coordinator_of(ws);
        file_crossing(ws, coordinator.as_deref(), spent, budget, &now).map(|rows| (coordinator, budget, rows))
    };
    let Some((coordinator, budget, (tasks, workstreams))) = filed else { return false };
    let snapshot = ctx.data.read().clone();
    if let Err(e) = (ctx.save)(&snapshot) {
        log::warn!("hive {}: budget row not saved: {e}", caller.hive.id);
    }
    ctx.window.tasks_changed(&caller.workspace_id, &tasks, &workstreams);
    log::info!("hive {}: budget reached, {} of {}: the human is asked", caller.hive.id, usd(spent), usd(budget));
    if let Some(c) = coordinator.filter(|_| !by_coordinator) {
        let sent = ctx.window.deliver(&c, crossing_notice(spent, budget), "Hive budget").await;
        log::info!("hive {}: budget notice to the coordinator: {}", caller.hive.id, sent.describe());
    }
    true
}

// ── The human's gallery settings ──

/// What the gallery settings sheet sends.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct GalleryChange {
    pub budget_cents: u64,
    pub output_folders: Vec<String>,
    pub generator_hosts: Vec<String>,
}

pub fn check_budget(cents: u64) -> Result<(), String> {
    if cents > MAX_BUDGET_CENTS {
        return Err(format!("The budget can be at most {}.", usd(MAX_BUDGET_CENTS as i64 * MICROS_PER_CENT)));
    }
    Ok(())
}

/// One allowlist entry, as Claude's sandbox takes it: a lowercase DNS name with a dot, an
/// optional leading `*.`, no scheme, port, path or IP literal, never localhost.
pub fn normalize_host(raw: &str) -> Result<String, String> {
    let h = raw.trim().trim_end_matches('.').to_ascii_lowercase();
    let shown = crate::hive::deliver::one_line(raw, 80);
    let refuse = |why: &str| Err(format!("\u{201c}{shown}\u{201d} {why}"));
    if h.is_empty() {
        return Err("A host can't be empty.".to_string());
    }
    if h.contains("://") || h.contains(['/', ':', '@', '?', '#', '[', ']']) {
        return refuse("isn't a bare host name: no scheme, port or path.");
    }
    let name = h.strip_prefix("*.").unwrap_or(&h);
    let labels: Vec<&str> = name.split('.').collect();
    if labels.len() < 2 {
        return refuse("needs a dot: a whole top-level domain is too wide.");
    }
    let label_ok = |l: &&str| !l.is_empty() && l.len() <= 63 && !l.starts_with('-') && !l.ends_with('-') && l.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if name.len() > 253 || !labels.iter().all(label_ok) {
        return refuse("isn't a host name (letters, digits and hyphens between dots).");
    }
    if labels.last().is_some_and(|tld| tld.bytes().all(|b| b.is_ascii_digit())) {
        return refuse("is an IP address: list host names only.");
    }
    if labels.last() == Some(&"localhost") || labels.last() == Some(&"local") {
        return refuse("is this computer or its network: a worker's localhost stays closed.");
    }
    Ok(h)
}

/// Whether an allowlist entry reaches `host`: itself, or under a `*.` entry.
pub fn host_covers(entry: &str, host: &str) -> bool {
    match entry.strip_prefix("*.") {
        Some(base) => host.len() > base.len() && host.ends_with(base) && host.as_bytes()[host.len() - base.len() - 1] == b'.',
        None => entry == host,
    }
}

/// The human's hosts, normalized, deduplicated and capped. A repo's remote host stays denied: a
/// host allowlist can't tell a fetch from a push (§7.3).
pub fn check_hosts(raw: &[String], remote_hosts: &[String]) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    for r in raw {
        let h = normalize_host(r)?;
        if let Some(remote) = remote_hosts.iter().find(|rh| host_covers(&h, &rh.to_ascii_lowercase())) {
            return Err(format!("{h} reaches {remote}, a git remote of this Hive: no agent may reach a remote, so maiTerm keeps it denied."));
        }
        if !out.contains(&h) {
            out.push(h);
        }
    }
    if out.len() > MAX_GENERATOR_HOSTS {
        return Err(format!("At most {MAX_GENERATOR_HOSTS} generator hosts."));
    }
    Ok(out)
}

/// The human's output folders, canonical: each one exists, holds neither the Hive nor maiTerm's
/// data folder and sits in neither, at most `MAX_OUTPUT_FOLDERS`.
pub fn check_output_folders(raw: &[String], hive_root: &Path, app_data: Option<&Path>) -> Result<Vec<String>, String> {
    let mut out: Vec<String> = Vec::new();
    let hive_root = crate::share::canon(hive_root);
    for r in raw.iter().map(|r| r.trim()).filter(|r| !r.is_empty()) {
        let p = crate::share::expand_tilde(r);
        if !p.is_absolute() || !p.is_dir() {
            return Err(format!("No folder at {r}."));
        }
        let c = crate::share::canon(&p);
        if c.starts_with(&hive_root) || hive_root.starts_with(&c) {
            return Err(format!("{} is the Hive's own folder or holds it.", c.display()));
        }
        if let Some(why) = app_data.and_then(|a| crate::hive::create::app_data_refusal(&c, a)) {
            return Err(format!("{} {why}.", c.display()));
        }
        let s = c.to_string_lossy().to_string();
        if !out.contains(&s) {
            out.push(s);
        }
    }
    if out.len() > MAX_OUTPUT_FOLDERS {
        return Err(format!("At most {MAX_OUTPUT_FOLDERS} output folders."));
    }
    Ok(out)
}

/// A settings change, checked against the disk and the Hive's remotes. Blocks: git, the disk.
pub fn check_change(hive: &Hive, hive_root: &Path, app_data: Option<&Path>, c: &GalleryChange) -> Result<GalleryChange, String> {
    check_budget(c.budget_cents)?;
    Ok(GalleryChange {
        budget_cents: c.budget_cents,
        output_folders: check_output_folders(&c.output_folders, hive_root, app_data)?,
        generator_hosts: check_hosts(&c.generator_hosts, &crate::hive::settings::hive_remote_hosts(hive))?,
    })
}

/// A checked change written into the state, the window's own workspace only.
pub struct Applied {
    pub hive: Hive,
    pub coordinator: Option<String>,
    /// The workspace's rows when a raise closed the budget row.
    pub closed: Option<(Vec<Task>, Vec<Workstream>)>,
    /// The spend was over the budget and no longer is. The coordinator was told to wait for this,
    /// so it hears it whatever became of the row (the human may have answered the card already).
    pub raised: bool,
}

pub fn apply_in(data: &mut AppData, label: &str, workspace_id: &str, hive_id: &str, c: GalleryChange, spent: i64, now: &str) -> Result<Applied, String> {
    let ws = data
        .window_mut(label)
        .and_then(|w| w.workspaces.iter_mut().find(|w| w.id == workspace_id))
        .filter(|ws| ws.hive.as_ref().is_some_and(|h| h.id == hive_id))
        .ok_or("Workspace not found")?;
    let coordinator = coordinator_of(ws);
    let hive = ws.hive.as_mut().ok_or("That workspace isn't a Hive")?;
    let was_over = is_over(spent, budget_micros(hive));
    hive.budget_cents = c.budget_cents;
    hive.settings.gallery.output_folders = c.output_folders;
    hive.settings.gallery.generator_hosts = c.generator_hosts;
    let hive = hive.clone();
    let budget = budget_micros(&hive);
    let closed = close_on_raise(ws, coordinator.as_deref(), spent, budget, now).then(|| (ws.tasks.clone(), ws.workstreams.clone()));
    let raised = closed.is_some() || (was_over && !is_over(spent, budget));
    Ok(Applied { hive, coordinator, closed, raised })
}

/// The human's gallery settings for the Hive in `workspace_id`, which must be window `label`'s:
/// checked, saved, sent to the window's mirror; a raise past the spend closes the budget row and
/// tells the coordinator what's left.
pub(crate) async fn set_gallery_settings<W: HiveWindow>(ctx: &Ctx<'_, W>, label: &str, workspace_id: &str, change: GalleryChange, app_data: Option<PathBuf>) -> Result<Hive, String> {
    let hive = {
        let d = ctx.data.read();
        let ws = d.window(label).and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id)).ok_or("Workspace not found")?;
        ws.hive.clone().ok_or("That workspace isn't a Hive")?
    };
    let root = paths::hive_root(&hive.id).ok_or("No Hive folder")?;
    let h = hive.clone();
    let (checked, spent) = blocking(move || {
        let paths = HivePaths::new(&root);
        Ok((check_change(&h, &paths.root, app_data.as_deref(), &change)?, spent_of(&h.id, &paths)))
    })
    .await?;
    set_checked(ctx, label, workspace_id, &hive.id, checked, spent).await
}

pub(crate) async fn set_checked<W: HiveWindow>(ctx: &Ctx<'_, W>, label: &str, workspace_id: &str, hive_id: &str, checked: GalleryChange, spent: i64) -> Result<Hive, String> {
    let now = crate::mailink::board::iso_now();
    let applied = apply_in(&mut ctx.data.write(), label, workspace_id, hive_id, checked, spent, &now)?;
    let snapshot = ctx.data.read().clone();
    (ctx.save)(&snapshot)?;
    let Applied { hive, coordinator, closed, raised } = applied;
    log::info!(
        "hive {}: gallery settings saved: budget {}, {} output folder(s), {} generator host(s)",
        hive.id,
        usd(budget_micros(&hive)),
        hive.settings.gallery.output_folders.len(),
        hive.settings.gallery.generator_hosts.len()
    );
    ctx.window.hive_changed(workspace_id, &hive);
    if let Some((tasks, workstreams)) = closed {
        ctx.window.tasks_changed(workspace_id, &tasks, &workstreams);
    }
    if let Some(c) = coordinator.filter(|_| raised) {
        let budget = budget_micros(&hive);
        let text = format!("[maiTerm] Your human raised the budget to {}; {} left.", usd(budget), usd((budget - spent).max(0)));
        let sent = ctx.window.deliver(&c, text, "Hive budget").await;
        log::info!("hive {}: budget raise to the coordinator: {}", hive.id, sent.describe());
    }
    Ok(hive)
}

fn window_ctx<'a>(state: &'a Arc<AppState>, app: &tauri::AppHandle, label: &str) -> Ctx<'a, crate::hive::rpc::AppWindow> {
    Ctx {
        data: &state.app_data,
        fuses: crate::hive::fuses::book(),
        window: crate::hive::rpc::AppWindow { app: state.clone(), handle: app.clone(), label: label.to_string() },
        save: crate::state::save_state,
        preflight: crate::hive::launch::worker_preflight,
    }
}

/// Hive header → Budget: the budget, the output folders and the generator hosts (§10.3, §14.16).
/// A running worker gets new hosts at its next launch, which rewrites its settings.
#[tauri::command]
pub async fn hive_set_gallery_settings(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    workspace_id: String,
    settings: GalleryChange,
) -> Result<Hive, String> {
    let label = window.label().to_string();
    let ctx = window_ctx(state.inner(), &app, &label);
    set_gallery_settings(&ctx, &label, &workspace_id, settings, crate::hive::create::app_data_dir()).await
}

#[cfg(test)]
mod tests;
