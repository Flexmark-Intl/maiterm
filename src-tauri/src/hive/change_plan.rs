//! The queen's `hiveChangeSettings` (docs/hive.md §3.4): any Hive setting the human can set
//! (Darryl, 2026-11-21: "queen should be able to set anything"), routed through two checkpoints.
//!
//! - **Access** (`checkpoints::ACCESS`, the queen's by default): what workers may use. That covers
//!   hosts, local database sockets, tools and generator hosts. A Hive left alone for hours must not
//!   stall on a worker that can't reach a website or a local Postgres.
//! - **Settings** (`checkpoints::SETTINGS`, Ask me by default): everything else.
//! - **Always a card**, whoever holds either: a change of a checkpoint (the human's approval switch
//!   itself) and a budget raise (money).
//!
//! A call may mix them: the parts the checkpoints hand the queen apply at once, and the rest goes on
//! one card the human approves (`change::approved_in`). Each half is reduced to what actually differs
//! from the record (`Prepared::change`) before anything else: a card stores only what it shows, so
//! a field that was a no-op at filing can't apply unseen on a later approval (the review). Every
//! limit is checked before anything is saved, so a call that would fail applies nothing; a part
//! that fails anyway (the record changed meanwhile) leaves what already applied logged and told.
//!
//! The core fields go through `change::check_core`/`write`, under the write lock. Tools go through
//! the Tools sheet's own write (`tool_change::write_tools`), with the hosts and output folders the
//! added tools need ticked, since they come with the tool. The gallery (budget, generator hosts,
//! output folders) is a delta merged onto the record as it is after the tools write, just before
//! Budget and generation's write (`gallery::budget::set_checked`), so neither a tool's hosts nor a
//! change the human just made is written over. The rules Rust holds for anyone hold for her: never
//! a repo remote's host, home or a private address, a socket not named like a database's, or a tool
//! not detected on this Mac.

use serde_json::{json, Value};

use super::change::{self, By, SettingsChange};
use super::checkpoints::{self, ACCESS, SETTINGS};
use super::create::DetectedTool;
use super::deliver::one_line;
use super::gallery::budget::{self as gallery, normalize_host, GalleryChange, MAX_GENERATOR_HOSTS, MAX_OUTPUT_FOLDERS};
use super::record::Hive;
use super::rpc::{HiveRequest, HiveWindow};
use super::tool_change::{self, ToolsPreview};
use super::tools::{Caller, Ctx};

const MAX_REASON: usize = 500;

/// Whether a change names nothing at all.
pub fn is_empty(c: &SettingsChange) -> bool {
    *c == SettingsChange::default()
}

/// The queen's change split by who decides each part: (applies at once, goes on a card).
pub fn route(hive: &Hive, c: &SettingsChange) -> (SettingsChange, SettingsChange) {
    let access = !checkpoints::asks(hive, ACCESS);
    let settings = !checkpoints::asks(hive, SETTINGS);
    let mut now = SettingsChange::default();
    let mut card = SettingsChange::default();
    macro_rules! put {
        ($go_now:expr, $($f:ident),+) => {
            $( if $go_now { now.$f = c.$f.clone(); } else { card.$f = c.$f.clone(); } )+
        };
    }
    put!(access, add_hosts, remove_hosts, add_sockets, remove_sockets, add_tools, remove_tools, add_generator_hosts, remove_generator_hosts);
    put!(
        settings,
        cap,
        coordinator_model,
        coordinator_effort,
        worker_model,
        worker_effort,
        push_after_land,
        workers_on_phone,
        folders,
        add_output_folders,
        remove_output_folders,
        prompt_timeout_secs,
        daily_digest,
        run_queue
    );
    // The approval switch itself, and money: the human's, always.
    card.checkpoints = c.checkpoints.clone();
    let raise = c.budget_cents.is_some_and(|b| b > hive.budget_cents);
    put!(settings && !raise, budget_cents);
    (now, card)
}

/// A tools change worked out against a fresh detection, every limit `write_tools` holds checked.
struct ToolsStep {
    detected: Vec<DetectedTool>,
    next: Vec<String>,
    preview: ToolsPreview,
}

/// What a change does, every part checked against the record: nothing is saved. `change` is only
/// what differs, normalised: what a card stores and what applies.
pub(crate) struct Prepared {
    pub lines: Vec<String>,
    pub change: SettingsChange,
}

fn tools_next(hive: &Hive, c: &SettingsChange) -> Vec<String> {
    let mut next: Vec<String> = hive.tools.iter().filter(|t| !c.remove_tools.iter().any(|r| r.trim() == t.as_str())).cloned().collect();
    for t in c.add_tools.iter().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        if !next.iter().any(|n| n == t) {
            next.push(t.to_string());
        }
    }
    next
}

async fn tools_step<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, hive: &Hive, c: &SettingsChange, remotes: &[String]) -> Result<Option<ToolsStep>, String> {
    if c.add_tools.is_empty() && c.remove_tools.is_empty() {
        return Ok(None);
    }
    for r in &c.remove_tools {
        if !hive.tools.iter().any(|t| t == r.trim()) {
            return Err(format!("This Hive has no tool \u{201c}{}\u{201d} to remove; its tools: {}.", one_line(r.trim(), 80), if hive.tools.is_empty() { "none".to_string() } else { hive.tools.join(", ") }));
        }
    }
    let next = tools_next(hive, c);
    if next == hive.tools {
        return Ok(None);
    }
    let prefs = ctx.data.read().preferences.clone();
    let detected = super::tools::blocking(move || Ok(super::create::detect_tools(&prefs))).await?;
    let preview = tool_change::preview(hive, &detected, &next, remotes)?;
    // The limits `write_tools` holds, now: nothing may save before one of them refuses.
    let checked = tool_change::check(hive, &detected, &next, &preview.hosts_offered, remotes)?;
    let (h, root, folders) = (hive.clone(), caller.paths.root.clone(), preview.folders_offered.clone());
    super::tools::blocking(move || tool_change::check_folders(&h, &checked, &folders, &root, super::create::app_data_dir().as_deref())).await?;
    Ok(Some(ToolsStep { detected, next, preview }))
}

fn tools_lines(s: &ToolsStep) -> Vec<String> {
    tool_change::change_lines(&tool_change::Checked { preview: s.preview.clone(), hosts_added: s.preview.hosts_offered.clone(), folders_added: s.preview.folders_offered.clone() })
}

/// The gallery part, only what differs: a budget, and hosts and folders added or removed.
#[derive(Debug, Clone, Default, PartialEq)]
struct GalleryDelta {
    budget: Option<u64>,
    add_hosts: Vec<String>,
    remove_hosts: Vec<String>,
    add_folders: Vec<String>,
    remove_folders: Vec<String>,
}

fn usd_of(cents: u64) -> String {
    gallery::usd(i64::try_from(cents).unwrap_or(i64::MAX / 10_000).saturating_mul(10_000))
}

/// What `c` changes in the gallery as it is on `hive`, and its lines: None when nothing differs.
fn gallery_delta(hive: &Hive, c: &SettingsChange, by: By) -> Result<Option<(GalleryDelta, Vec<String>)>, String> {
    let g = &hive.settings.gallery;
    let mut d = GalleryDelta::default();
    let mut lines = Vec::new();
    if let Some(b) = c.budget_cents.filter(|b| *b != hive.budget_cents) {
        gallery::check_budget(b)?;
        if b > hive.budget_cents && by == By::Coordinator {
            return Err("Raising the budget is your human\u{2019}s: it goes on a card.".to_string());
        }
        lines.push(format!("Budget: {} \u{2192} {}", usd_of(hive.budget_cents), usd_of(b)));
        d.budget = Some(b);
    }
    let have: Vec<String> = g.generator_hosts.iter().filter_map(|h| normalize_host(h).ok()).collect();
    for h in &c.remove_generator_hosts {
        let n = normalize_host(h)?;
        if have.contains(&n) && !d.remove_hosts.contains(&n) {
            d.remove_hosts.push(n);
        }
    }
    for h in &c.add_generator_hosts {
        let n = normalize_host(h)?;
        if !have.contains(&n) && !d.add_hosts.contains(&n) && !d.remove_hosts.contains(&n) {
            d.add_hosts.push(n);
        }
    }
    for f in c.remove_output_folders.iter().map(|f| f.trim()) {
        if g.output_folders.iter().any(|x| x == f) && !d.remove_folders.iter().any(|x| x == f) {
            d.remove_folders.push(f.to_string());
        }
    }
    for f in c.add_output_folders.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        if !g.output_folders.iter().any(|x| x == f) && !d.add_folders.iter().any(|x| x == f) && !d.remove_folders.iter().any(|x| x == f) {
            d.add_folders.push(f.to_string());
        }
    }
    if !d.add_hosts.is_empty() {
        lines.push(format!("Media workers may also reach (from each one\u{2019}s next launch): {}", d.add_hosts.join(", ")));
    }
    if !d.remove_hosts.is_empty() {
        lines.push(format!("Media workers no longer reach (from each one\u{2019}s next launch): {}", d.remove_hosts.join(", ")));
    }
    if !d.add_folders.is_empty() {
        lines.push(format!("galleryAdd may also file from: {}", d.add_folders.join(", ")));
    }
    if !d.remove_folders.is_empty() {
        lines.push(format!("galleryAdd no longer files from: {}", d.remove_folders.join(", ")));
    }
    if lines.is_empty() {
        return Ok(None);
    }
    Ok(Some((d, lines)))
}

/// The gallery with `d` merged onto it, or why not: within the limits Budget and generation holds.
fn merged(hive: &Hive, d: &GalleryDelta) -> Result<GalleryChange, String> {
    let g = &hive.settings.gallery;
    let mut hosts: Vec<String> = g.generator_hosts.iter().filter(|h| normalize_host(h).map_or(true, |n| !d.remove_hosts.contains(&n))).cloned().collect();
    for h in &d.add_hosts {
        if !hosts.iter().any(|x| normalize_host(x).ok().as_deref() == Some(h.as_str())) {
            hosts.push(h.clone());
        }
    }
    let mut folders: Vec<String> = g.output_folders.iter().filter(|f| !d.remove_folders.contains(f)).cloned().collect();
    for f in &d.add_folders {
        if !folders.contains(f) {
            folders.push(f.clone());
        }
    }
    if hosts.len() > MAX_GENERATOR_HOSTS {
        return Err(format!("At most {MAX_GENERATOR_HOSTS} generator hosts."));
    }
    if folders.len() > MAX_OUTPUT_FOLDERS {
        return Err(format!("At most {MAX_OUTPUT_FOLDERS} output folders."));
    }
    Ok(GalleryChange { budget_cents: d.budget.unwrap_or(hive.budget_cents), output_folders: folders, generator_hosts: hosts })
}

/// The added hosts and folders checked as Budget and generation checks them (folders on disk, hosts
/// against the remotes), off the async thread. A removal needs no check.
async fn gallery_checked(caller: &Caller, d: &GalleryDelta, remotes: &[String]) -> Result<(), String> {
    let (d, remotes, root) = (d.clone(), remotes.to_vec(), caller.paths.root.clone());
    super::tools::blocking(move || {
        gallery::check_hosts(&d.add_hosts, &remotes)?;
        gallery::check_output_folders(&d.add_folders, &root, super::create::app_data_dir().as_deref())?;
        Ok(())
    })
    .await
}

/// What `c` would do to `hive`, every part checked, nothing saved.
pub(crate) async fn prepare<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, hive: &Hive, c: &SettingsChange, by: By, remotes: &[String]) -> Result<Prepared, String> {
    let core = change::check_core(hive, &c.core(), by, remotes)?;
    let mut lines = core.lines;
    let mut change = core.change;
    if let Some(s) = tools_step(ctx, caller, hive, c, remotes).await? {
        lines.extend(tools_lines(&s));
        (change.add_tools, change.remove_tools) = (s.preview.added.clone(), s.preview.removed.clone());
    }
    if let Some((d, l)) = gallery_delta(hive, c, by)? {
        merged(hive, &d)?;
        gallery_checked(caller, &d, remotes).await?;
        lines.extend(l);
        change.budget_cents = d.budget;
        (change.add_generator_hosts, change.remove_generator_hosts, change.add_output_folders, change.remove_output_folders) = (d.add_hosts, d.remove_hosts, d.add_folders, d.remove_folders);
    }
    Ok(Prepared { lines, change })
}

/// Hosts added now must not answer home: a name the queen grants alone must resolve too.
async fn refuse_home(c: &SettingsChange, by: By, remotes: &[String]) -> Result<(), String> {
    let hosts: Vec<String> = c.add_hosts.iter().chain(c.add_generator_hosts.iter()).cloned().collect();
    change::refuse_private_hosts(&hosts, by == By::Coordinator, &change::ip_remotes(remotes)).await
}

/// A change applied: the record after, what changed, and the part that failed after others had
/// applied, if one did.
pub(crate) struct Committed {
    pub hive: Hive,
    pub lines: Vec<String>,
    pub failed: Option<String>,
}

/// `c` applied, each part checked again against the record as it is now, every limit before any
/// write: the core under the write lock, then tools, then the gallery merged onto the record as
/// the tools write left it. Err only when nothing applied.
pub(crate) async fn commit<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, c: &SettingsChange, by: By, remotes: &[String]) -> Result<Committed, String> {
    refuse_home(c, by, remotes).await?;
    let current = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    let tools = tools_step(ctx, caller, &current, c, remotes).await?;
    let gallery_part = gallery_delta(&current, c, by)?;
    if let Some((d, _)) = &gallery_part {
        merged(&current, d)?;
        gallery_checked(caller, d, remotes).await?;
    }
    change::check_core(&current, &c.core(), by, remotes)?;

    let (mut hive, mut lines) = change::write(ctx, caller, &c.core(), by, remotes)?;
    let mut failed = None;
    if let Some(s) = tools {
        match tool_change::write_tools(ctx, caller, &s.detected, remotes, &s.next, &s.preview.hosts_offered, &s.preview.folders_offered).await {
            Ok((h, checked)) => {
                hive = h;
                lines.extend(tool_change::change_lines(&checked));
            }
            Err(e) => failed = Some(e),
        }
    }
    if let (None, Some((d, _))) = (&failed, gallery_part) {
        let p = caller.paths.clone();
        let id = caller.hive.id.clone();
        let spent = super::tools::blocking(move || Ok(gallery::spent_of(&id, &p))).await?;
        // Re-read and merged with no await before the write, so nothing written meanwhile (the
        // tools write's hosts, the human's own budget) is written over.
        let fresh = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
        let applied = match (gallery_delta(&fresh, &delta_change(&d), by), merged(&fresh, &d)) {
            (Ok(Some((_, l))), Ok(g)) => gallery::set_checked(ctx, &caller.label, &caller.workspace_id, &caller.hive.id, g, spent).await.map(|h| (h, l)),
            (Ok(None), _) => Ok((hive.clone(), Vec::new())),
            (Err(e), _) | (_, Err(e)) => Err(e),
        };
        match applied {
            Ok((h, l)) => {
                hive = h;
                lines.extend(l);
            }
            Err(e) => failed = Some(e),
        }
    }
    match (lines.is_empty(), failed) {
        (true, Some(e)) => Err(e),
        (true, None) => Err(change::NOTHING.to_string()),
        (false, failed) => Ok(Committed { hive, lines, failed }),
    }
}

/// A gallery delta as the change fields that name it.
fn delta_change(d: &GalleryDelta) -> SettingsChange {
    SettingsChange {
        budget_cents: d.budget,
        add_generator_hosts: d.add_hosts.clone(),
        remove_generator_hosts: d.remove_hosts.clone(),
        add_output_folders: d.add_folders.clone(),
        remove_output_folders: d.remove_folders.clone(),
        ..Default::default()
    }
}

fn hosts_only(c: &SettingsChange) -> bool {
    (!c.add_hosts.is_empty() || !c.remove_hosts.is_empty()) && SettingsChange { add_hosts: c.add_hosts.clone(), remove_hosts: c.remove_hosts.clone(), ..Default::default() } == *c
}

/// A line in the decision log for what the queen changed on her own: hers, auditable, undone in
/// Hive settings. `c` is the normalised change (`Prepared::change`).
pub(crate) fn decision_line(hive_id: &str, c: &SettingsChange, lines: &[String], reason: &str) -> super::decisions::DecisionLine {
    use super::decisions::{self, DecisionLine};
    let (category, question) = if hosts_only(c) {
        (decisions::CATEGORY_HOSTS, "Which hosts the workers\u{2019} sandbox may reach")
    } else {
        (decisions::CATEGORY_OTHER, "This Hive\u{2019}s settings")
    };
    DecisionLine {
        id: decisions::new_id(),
        event: decisions::EVENT_ANSWERED.to_string(),
        hive_id: hive_id.to_string(),
        category: category.to_string(),
        source: decisions::SOURCE_SETTINGS.to_string(),
        question: question.to_string(),
        answer: Some(sentence(c, lines, reason)),
        reason: Some(one_line(reason, MAX_REASON)),
        by: decisions::BY_COORDINATOR.to_string(),
        at: crate::commands::workspace::iso_now(),
        ..DecisionLine::default()
    }
}

/// What the queen did, as the human reads it in a notice and the decision log.
fn sentence(c: &SettingsChange, lines: &[String], reason: &str) -> String {
    if hosts_only(c) {
        let mut parts = Vec::new();
        if !c.add_hosts.is_empty() {
            parts.push(format!("allowed {} for workers", c.add_hosts.join(", ")));
        }
        if !c.remove_hosts.is_empty() {
            parts.push(format!("stopped workers reaching {}", c.remove_hosts.join(", ")));
        }
        return format!("The queen {}: {}", parts.join(" and "), one_line(reason, MAX_REASON));
    }
    format!("The queen changed this Hive\u{2019}s settings: {}. Why: {}", lines.join("; "), one_line(reason, MAX_REASON))
}

/// `hiveChangeSettings` (§3.4).
pub(crate) async fn change_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let (change, reason) = change::parse_args(args)?;
    let Some(reason) = reason else {
        return Err("A settings change needs a reason: your human reads it with the change.".to_string());
    };
    let hive = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    let remotes = change::remotes_of(&hive).await?;
    let (now, card) = route(&hive, &change);
    // Both halves checked, and reduced to what differs, before either applies.
    let none = || Prepared { lines: Vec::new(), change: SettingsChange::default() };
    let now = if is_empty(&now) { none() } else { prepare(ctx, caller, &hive, &now, By::Coordinator, &remotes).await? };
    let card = if is_empty(&card) { none() } else { prepare(ctx, caller, &hive, &card, By::Approved, &remotes).await? };
    if now.lines.is_empty() && card.lines.is_empty() {
        return Err(change::NOTHING.to_string());
    }

    let mut out = serde_json::Map::new();
    if !now.lines.is_empty() {
        let done = commit(ctx, caller, &now.change, By::Coordinator, &remotes).await?;
        let lines = done.lines.clone();
        change::log_line(caller, "coordinator", &lines, Some(&reason));
        let line = decision_line(&done.hive.id, &now.change, &lines, &reason);
        let text = format!("{}. Undo it in Hive settings.", sentence(&now.change, &lines, &reason));
        let p = caller.paths.clone();
        let _ = super::tools::blocking(move || {
            super::decisions::record(&p, &line);
            Ok(())
        })
        .await;
        ctx.window.decisions_changed(&caller.workspace_id);
        ctx.window.settings_changed(&caller.workspace_id, &text);
        let settled = if now.change.add_hosts.is_empty() { Vec::new() } else { super::prompts::settle_host_blockers(ctx, caller, &done.hive, &reason).await };
        ctx.window.poll_waiters(&caller.workspace_id);
        let answered: Vec<Value> = settled.iter().map(|w| json!({ "worker": w.badge, "title": w.title })).collect();
        let mut note = if answered.is_empty() {
            "Applied; your human was told. Each agent gets it at its next launch: hiveRestartWorker relaunches a running worker (its session resumes).".to_string()
        } else {
            "Applied; your human was told, and maiTerm answered these workers\u{2019} sandbox blockers as yours. Each gets it at its next \
             launch: relaunch it with hiveRestartWorker when it is at a good point (its session resumes)."
                .to_string()
        };
        if let Some(running) = change::relaunch_note(caller, &now.change).await {
            note.push(' ');
            note.push_str(&running);
        }
        if let Some(e) = &done.failed {
            note.push_str(&format!(" Not applied, as the Hive changed meanwhile: {e}"));
            out.insert("not_applied".into(), json!(e));
        }
        out.insert("applied".into(), json!(lines));
        out.insert("answered_blockers".into(), json!(answered));
        out.insert("note".into(), json!(note));
    }
    if !card.lines.is_empty() {
        let mut effects = card.lines.clone();
        if !card.change.add_hosts.is_empty() || !card.change.add_generator_hosts.is_empty() {
            effects.push("A host a worker can reach can also receive what it sends: only hosts you trust with this project\u{2019}s code.".to_string());
        }
        if card.change.folders.iter().any(|f| f.setup.is_some()) {
            effects.push("A folder\u{2019}s setup runs outside the sandbox, as you, before each worker starts.".to_string());
        }
        let req = HiveRequest {
            tab_id: caller.tab_id.clone(),
            kind: change::KIND,
            // Only what the card shows: a field equal to the record now can't apply unseen later.
            args: json!({ "hive_id": hive.id, "change": card.change }),
            summary: format!("Change the Hive\u{2019}s settings ({})", one_line(&caller.workstream, 80)),
            effects,
            reason: Some(reason.clone()),
            keep: None,
        };
        let id = ctx.window.file_request(req)?;
        log::info!("hive {}: settings change {id} waits for the human: {}", hive.id, card.lines.join("; "));
        out.insert("request_id".into(), json!(id));
        out.insert("awaiting_approval".into(), json!(card.lines));
        let why = if !card.change.checkpoints.is_empty() || card.change.budget_cents.is_some_and(|b| b > hive.budget_cents) {
            "A checkpoint change and a budget raise are always your human\u{2019}s."
        } else {
            "Your human holds that checkpoint."
        };
        let note = format!("{why} They approve or deny the card; maiTerm tells you which. Carry on meanwhile.");
        let note = match out.get("note").and_then(Value::as_str) {
            Some(n) => format!("{n} The rest is on a card: {note}"),
            None => note,
        };
        out.insert("note".into(), json!(note));
    }
    let status = match (out.contains_key("applied"), out.contains_key("request_id")) {
        (true, true) => "partly_applied",
        (true, false) => "applied",
        _ => "awaiting_approval",
    };
    out.insert("status".into(), json!(status));
    Ok(Value::Object(out))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::record::STATUS_ACTIVE;

    fn hive() -> Hive {
        let mut h = Hive { id: "h1".into(), cap: 3, budget_cents: 1000, status: STATUS_ACTIVE.into(), checkpoints: super::super::create::default_checkpoints("base"), ..Hive::default() };
        h.tools = vec!["mcp:fal".into()];
        h
    }

    /// By default the queen grants access at once and asks for the rest; checkpoints and a budget
    /// raise are a card whoever holds Settings.
    #[test]
    fn a_change_splits_by_who_holds_each_checkpoint() {
        let c = SettingsChange {
            add_hosts: vec!["api.example.com".into()],
            add_sockets: vec!["/tmp/.s.PGSQL.5432".into()],
            add_tools: vec!["mcp:x".into()],
            cap: Some(5),
            push_after_land: Some(true),
            budget_cents: Some(2000),
            checkpoints: [("plan".to_string(), "ask".to_string())].into(),
            ..Default::default()
        };
        let (now, card) = route(&hive(), &c);
        assert_eq!((now.add_hosts.len(), now.add_sockets.len(), now.add_tools.len()), (1, 1, 1), "access is the queen's by default");
        assert_eq!((now.cap, now.push_after_land, now.budget_cents), (None, None, None));
        assert_eq!((card.cap, card.push_after_land, card.budget_cents), (Some(5), Some(true), Some(2000)), "settings ask by default");
        assert_eq!(card.checkpoints.len(), 1);

        let mut both = hive();
        both.checkpoints.insert(SETTINGS.into(), "coordinator".into());
        let (now, card) = route(&both, &c);
        assert_eq!((now.cap, now.push_after_land), (Some(5), Some(true)), "handed Settings, she sets anything");
        assert_eq!((card.budget_cents, card.checkpoints.len()), (Some(2000), 1), "but a raise and a checkpoint stay a card");
        assert!(card.cap.is_none() && card.add_hosts.is_empty());
        let lower = SettingsChange { budget_cents: Some(500), ..Default::default() };
        assert_eq!(route(&both, &lower).0.budget_cents, Some(500), "lowering it follows Settings");

        let mut asks = hive();
        asks.checkpoints.insert(ACCESS.into(), "ask".into());
        let (now, card) = route(&asks, &c);
        assert!(now.add_hosts.is_empty() && now.add_sockets.is_empty() && now.add_tools.is_empty());
        assert_eq!((card.add_hosts.len(), card.add_sockets.len(), card.add_tools.len()), (1, 1, 1), "Ask me on Access puts grants on a card");
    }

    /// Every field of the change lands in exactly one half.
    #[test]
    fn no_field_is_lost_in_the_split() {
        let c = SettingsChange {
            cap: Some(2),
            coordinator_model: Some("opus".into()),
            coordinator_effort: Some("high".into()),
            worker_model: Some("haiku".into()),
            worker_effort: Some("low".into()),
            add_hosts: vec!["a.example.com".into()],
            remove_hosts: vec!["b.example.com".into()],
            push_after_land: Some(false),
            workers_on_phone: Some(true),
            add_sockets: vec!["/tmp/redis.sock".into()],
            remove_sockets: vec!["/tmp/mysql.sock".into()],
            folders: vec![change::FolderChange { name: "web".into(), setup: Some("npm ci".into()), land_check: None }],
            add_tools: vec!["mcp:a".into()],
            remove_tools: vec!["mcp:fal".into()],
            add_generator_hosts: vec!["g.example.com".into()],
            remove_generator_hosts: vec!["h.example.com".into()],
            add_output_folders: vec!["/tmp/out".into()],
            remove_output_folders: vec!["/tmp/old".into()],
            budget_cents: Some(900),
            prompt_timeout_secs: Some(600),
            daily_digest: Some(false),
            run_queue: Some(Default::default()),
            checkpoints: [("access".to_string(), "ask".to_string())].into(),
        };
        for settings in ["ask", "coordinator"] {
            for access in ["ask", "coordinator"] {
                let mut h = hive();
                h.checkpoints.insert(SETTINGS.into(), settings.into());
                h.checkpoints.insert(ACCESS.into(), access.into());
                let (now, card) = route(&h, &c);
                let a = serde_json::to_value(&now).unwrap();
                let b = serde_json::to_value(&card).unwrap();
                let mut merged = a.as_object().unwrap().clone();
                for (k, v) in b.as_object().unwrap() {
                    assert!(merged.insert(k.clone(), v.clone()).is_none(), "{k} in both halves");
                }
                assert_eq!(serde_json::Value::Object(merged), serde_json::to_value(&c).unwrap(), "settings {settings}, access {access}");
            }
        }
    }

    /// The gallery is a delta of only what differs, merged onto the record as it is when it
    /// applies: a host a tools write added meanwhile is kept, and her raise is refused.
    #[test]
    fn the_gallery_part_is_a_delta_merged_onto_the_record_now() {
        let mut h = hive();
        h.settings.gallery.generator_hosts = vec!["old.example.com".into()];
        h.settings.gallery.output_folders = vec!["/tmp/a".into()];
        let c = SettingsChange {
            budget_cents: Some(500),
            add_generator_hosts: vec!["New.Example.com".into(), "old.example.com".into()],
            remove_output_folders: vec!["/tmp/a".into(), "/tmp/never".into()],
            ..Default::default()
        };
        let (d, lines) = gallery_delta(&h, &c, By::Coordinator).unwrap().unwrap();
        assert_eq!(d, GalleryDelta { budget: Some(500), add_hosts: vec!["new.example.com".into()], remove_folders: vec!["/tmp/a".into()], ..Default::default() }, "only what differs");
        assert_eq!(lines.len(), 3, "{lines:?}");

        // A tools write added a host meanwhile: the merge keeps it.
        let mut later = h.clone();
        later.settings.gallery.generator_hosts.push("tool.example.com".into());
        let g = merged(&later, &d).unwrap();
        assert_eq!(g.generator_hosts, vec!["old.example.com".to_string(), "tool.example.com".to_string(), "new.example.com".to_string()]);
        assert!(g.output_folders.is_empty());
        // The human lowered the budget meanwhile: a delta that names none keeps theirs.
        let hosts_only = GalleryDelta { add_hosts: vec!["x.example.com".into()], ..Default::default() };
        later.budget_cents = 200;
        assert_eq!(merged(&later, &hosts_only).unwrap().budget_cents, 200);

        assert!(gallery_delta(&h, &SettingsChange { budget_cents: Some(5000), ..Default::default() }, By::Coordinator).unwrap_err().contains("card"));
        assert!(gallery_delta(&h, &SettingsChange { budget_cents: Some(5000), ..Default::default() }, By::Approved).unwrap().is_some());
        assert!(gallery_delta(&h, &SettingsChange { budget_cents: Some(1000), ..Default::default() }, By::Approved).unwrap().is_none(), "the same budget changes nothing, so a card can't carry it");
        assert!(gallery_delta(&h, &SettingsChange { add_generator_hosts: vec!["old.example.com".into()], ..Default::default() }, By::Approved).unwrap().is_none());
        let mut full = h.clone();
        full.settings.gallery.generator_hosts = (0..MAX_GENERATOR_HOSTS).map(|i| format!("h{i}.example.com")).collect();
        assert!(merged(&full, &hosts_only).unwrap_err().contains("At most"));
    }

    #[test]
    fn tools_add_and_remove_by_id() {
        let h = hive();
        let c = SettingsChange { add_tools: vec![" mcp:x ".into(), "mcp:fal".into()], remove_tools: vec!["mcp:fal".into()], ..Default::default() };
        assert_eq!(tools_next(&h, &c), vec!["mcp:x".to_string(), "mcp:fal".to_string()], "named in both, it stays");
        let c = SettingsChange { remove_tools: vec!["mcp:fal".into()], ..Default::default() };
        assert!(tools_next(&h, &c).is_empty());
    }

    #[test]
    fn the_queens_own_change_is_hers_in_the_decision_log() {
        let hosts = SettingsChange { add_hosts: vec!["shadcn-svelte.com".into()], ..Default::default() };
        let l = decision_line("h1", &hosts, &[], "the components registry");
        assert_eq!((l.by.as_str(), l.category.as_str(), l.source.as_str()), ("coordinator", "hosts", "settings"));
        assert_eq!(l.answer.as_deref(), Some("The queen allowed shadcn-svelte.com for workers: the components registry"));
        let db = SettingsChange { add_sockets: vec!["/tmp/.s.PGSQL.5432".into()], ..Default::default() };
        let l = decision_line("h1", &db, &["Workers may also connect to: /tmp/.s.PGSQL.5432".into()], "the shop's database");
        assert_eq!(l.category, "other");
        assert!(l.answer.as_deref().unwrap().contains("/tmp/.s.PGSQL.5432") && l.answer.as_deref().unwrap().contains("the shop's database"));
    }
}
