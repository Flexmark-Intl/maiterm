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
//! a field that was a no-op at filing can't apply unseen on a later approval.
//!
//! **One write.** Everything that needs the disk, the network or a tool detection is gathered first
//! (`gather`: the detection, every added output folder in its canonical form, the hosts looked up).
//! Then `apply_all` works the whole change out on a copy of the record (core fields, tools and the
//! gallery's hosts and folders) and checks every limit on the combined result. The same function is
//! the dry run (`prepare`) and, under the write lock, the write (`commit`), so a change applies whole
//! or not at all. Two review rounds found the earlier shape, three writers each with its own checks,
//! losing a tool's hosts, storing folders as typed and saving half a change; this is the fix of the
//! approach, not of the lines. The budget alone is a second step after it, through Budget and
//! generation's own write (`gallery::budget::set_checked`), because a raise there also closes the
//! budget row and wakes the queen; it is read and written with no await between, so nothing written
//! meanwhile is written over.
//!
//! The rules Rust holds for anyone hold for her: never a repo remote's host, home or a private
//! address, a socket not named like a database's, or a tool not detected on this Mac.

use std::collections::HashMap;

use serde_json::{json, Value};

use super::change::{self, By, SettingsChange};
use super::checkpoints::{self, ACCESS, SETTINGS};
use super::create::DetectedTool;
use super::deliver::one_line;
use super::gallery::budget::{self as gallery, normalize_host, GalleryChange, MAX_GENERATOR_HOSTS, MAX_OUTPUT_FOLDERS};
use super::record::{Hive, STATUS_ACTIVE};
use super::rpc::{HiveRequest, HiveWindow};
use super::tool_change;
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
        health_check_minutes,
        run_queue
    );
    // The approval switch itself, and money: the human's, always.
    card.checkpoints = c.checkpoints.clone();
    let raise = c.budget_cents.is_some_and(|b| b > hive.budget_cents);
    put!(settings && !raise, budget_cents);
    (now, card)
}

/// What the write needs from the disk and a tool detection, gathered off the lock.
#[derive(Debug, Clone, Default)]
pub(crate) struct Gathered {
    /// A fresh detection, when the change names tools.
    detected: Option<Vec<DetectedTool>>,
    /// Each output folder named, as typed (trimmed) → its canonical form; a folder offered by an
    /// added tool too. A folder named to be added that has no entry here wasn't checked: refused.
    canon: HashMap<String, String>,
}

impl Gathered {
    fn canon_of(&self, raw: &str) -> Option<&String> {
        self.canon.get(raw.trim())
    }
}

/// The detection and the canonical folders, and every check that needs the disk: an added folder
/// as Budget and generation checks one (it exists, holds neither the Hive nor maiTerm's data), a
/// folder an added tool offers likewise. Blocks off the async thread.
async fn gather<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, hive: &Hive, c: &SettingsChange, remotes: &[String]) -> Result<Gathered, String> {
    let mut g = Gathered::default();
    let mut offered: Vec<String> = Vec::new();
    if !c.add_tools.is_empty() || !c.remove_tools.is_empty() {
        let prefs = ctx.data.read().preferences.clone();
        let detected = super::tools::blocking(move || Ok(super::create::detect_tools(&prefs))).await?;
        let next = tools_next(hive, c);
        if next != hive.tools {
            let preview = tool_change::preview(hive, &detected, &next, remotes)?;
            offered = preview.folders_offered.clone();
            // A host an added tool brings must not answer home either.
            change::refuse_private_hosts(&preview.hosts_offered, false, &change::ip_remotes(remotes)).await?;
        }
        g.detected = Some(detected);
    }
    let adds: Vec<String> = c.add_output_folders.iter().chain(offered.iter()).map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
    let removes: Vec<String> = c.remove_output_folders.iter().map(|f| f.trim().to_string()).filter(|f| !f.is_empty()).collect();
    let root = caller.paths.root.clone();
    let canon = super::tools::blocking(move || {
        let mut m = HashMap::new();
        let app = super::create::app_data_dir();
        for f in adds {
            let c = gallery::check_output_folders(std::slice::from_ref(&f), &root, app.as_deref())?;
            m.insert(f, c.into_iter().next().unwrap_or_default());
        }
        // A removal needs no check, only its canonical form when it still exists.
        for f in removes {
            // A folder already deleted can't be canonicalized: its parent's canonical form plus
            // its name finds it as stored (`/tmp/x` for a kept `/private/tmp/x`).
            let p = crate::share::expand_tilde(&f);
            let canon = std::fs::canonicalize(&p).ok().or_else(|| Some(crate::share::canon(p.parent()?).join(p.file_name()?)));
            if let Some(c) = canon {
                m.entry(f).or_insert_with(|| c.to_string_lossy().to_string());
            }
        }
        Ok(m)
    })
    .await?;
    g.canon = canon;
    Ok(g)
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

fn usd_of(cents: u64) -> String {
    gallery::usd(i64::try_from(cents).unwrap_or(i64::MAX / 10_000).saturating_mul(10_000))
}

/// The whole change worked out: what it says and only what differs, normalised.
#[derive(Debug, Clone, Default)]
pub(crate) struct Prepared {
    pub lines: Vec<String>,
    pub change: SettingsChange,
}

/// `c` applied to `h` in full, or `h` left as it was and why not: the core fields
/// (`change::check_core`), tools (`tool_change::check`, their offered hosts and folders with them),
/// and the gallery's generator hosts and output folders, every limit checked on the combined
/// result. The budget is only checked and named here: `commit` writes it after. Pure: the disk and
/// the detection come in `g`.
pub(crate) fn apply_all(h: &mut Hive, c: &SettingsChange, by: By, remotes: &[String], g: &Gathered) -> Result<Prepared, String> {
    let core = change::check_core(h, &c.core(), by, remotes)?;
    let mut next = h.clone();
    change::apply_to(&mut next, &core.change);
    let mut out = Prepared { lines: core.lines, change: core.change };

    if let Some(detected) = &g.detected {
        for r in &c.remove_tools {
            if !h.tools.iter().any(|t| t == r.trim()) {
                return Err(format!("This Hive has no tool \u{201c}{}\u{201d} to remove; its tools: {}.", one_line(r.trim(), 80), if h.tools.is_empty() { "none".to_string() } else { h.tools.join(", ") }));
            }
        }
        let tools = tools_next(h, c);
        if tools != h.tools {
            let preview = tool_change::preview(&next, detected, &tools, remotes)?;
            let mut checked = tool_change::check(&next, detected, &tools, &preview.hosts_offered, remotes)?;
            checked.folders_added = Vec::new();
            for f in &preview.folders_offered {
                let canon = g.canon_of(f).ok_or_else(|| format!("{} wasn\u{2019}t checked: try again.", one_line(f, 200)))?;
                if !next.settings.gallery.output_folders.contains(canon) && !checked.folders_added.contains(canon) {
                    checked.folders_added.push(canon.clone());
                }
            }
            out.lines.extend(tool_change::change_lines(&checked));
            (out.change.add_tools, out.change.remove_tools) = (checked.preview.added.clone(), checked.preview.removed.clone());
            tool_change::apply_to(&mut next, &checked);
        }
    } else if !c.add_tools.is_empty() || !c.remove_tools.is_empty() {
        return Err("The tools weren\u{2019}t detected: try again.".to_string());
    }

    // The gallery's hosts and folders, as a delta on the record as the tools left it.
    let gal = &mut next.settings.gallery;
    let have: Vec<String> = gal.generator_hosts.iter().filter_map(|x| normalize_host(x).ok()).collect();
    let mut removed_hosts = Vec::new();
    for x in &c.remove_generator_hosts {
        let n = normalize_host(x)?;
        if have.contains(&n) && !removed_hosts.contains(&n) {
            removed_hosts.push(n);
        }
    }
    let mut added_hosts = Vec::new();
    for n in gallery::check_hosts(&c.add_generator_hosts, remotes)? {
        if !have.contains(&n) && !removed_hosts.contains(&n) {
            added_hosts.push(n);
        }
    }
    gal.generator_hosts.retain(|x| normalize_host(x).map_or(true, |n| !removed_hosts.contains(&n)));
    gal.generator_hosts.extend(added_hosts.iter().cloned());
    let mut removed_folders = Vec::new();
    for f in c.remove_output_folders.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        let canon = g.canon_of(f);
        if let Some(x) = gal.output_folders.iter().find(|x| x.as_str() == f || Some(*x) == canon) {
            if !removed_folders.contains(x) {
                removed_folders.push(x.clone());
            }
        }
    }
    let mut added_folders = Vec::new();
    for f in c.add_output_folders.iter().map(|f| f.trim()).filter(|f| !f.is_empty()) {
        let canon = g.canon_of(f).ok_or_else(|| format!("{} wasn\u{2019}t checked: try again.", one_line(f, 200)))?.clone();
        if !gal.output_folders.contains(&canon) && !added_folders.contains(&canon) && !removed_folders.contains(&canon) {
            added_folders.push(canon);
        }
    }
    gal.output_folders.retain(|x| !removed_folders.contains(x));
    gal.output_folders.extend(added_folders.iter().cloned());
    if gal.generator_hosts.len() > MAX_GENERATOR_HOSTS {
        return Err(format!("At most {MAX_GENERATOR_HOSTS} generator hosts, the ones added tools bring included."));
    }
    if gal.output_folders.len() > MAX_OUTPUT_FOLDERS {
        return Err(format!("At most {MAX_OUTPUT_FOLDERS} output folders, the ones added tools write into included."));
    }
    if !added_hosts.is_empty() {
        out.lines.push(format!("Media workers may also reach (from each one\u{2019}s next launch): {}", added_hosts.join(", ")));
    }
    if !removed_hosts.is_empty() {
        out.lines.push(format!("Media workers no longer reach (from each one\u{2019}s next launch): {}", removed_hosts.join(", ")));
    }
    if !added_folders.is_empty() {
        out.lines.push(format!("galleryAdd may also file from: {}", added_folders.join(", ")));
    }
    if !removed_folders.is_empty() {
        out.lines.push(format!("galleryAdd no longer files from: {}", removed_folders.join(", ")));
    }
    (out.change.add_generator_hosts, out.change.remove_generator_hosts) = (added_hosts, removed_hosts);
    (out.change.add_output_folders, out.change.remove_output_folders) = (added_folders, removed_folders);

    if let Some(b) = c.budget_cents.filter(|b| *b != h.budget_cents) {
        gallery::check_budget(b)?;
        if b > h.budget_cents && by == By::Coordinator {
            return Err("Raising the budget is your human\u{2019}s: it goes on a card.".to_string());
        }
        out.lines.push(format!("Budget: {} \u{2192} {}", usd_of(h.budget_cents), usd_of(b)));
        out.change.budget_cents = Some(b);
    }

    // The budget is `commit`'s second step; everything else lands here, whole.
    next.budget_cents = h.budget_cents;
    *h = next;
    Ok(out)
}

/// What `c` would do to `hive`, every part checked, nothing saved.
pub(crate) async fn prepare<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, hive: &Hive, c: &SettingsChange, by: By, remotes: &[String]) -> Result<Prepared, String> {
    let g = gather(ctx, caller, hive, c, remotes).await?;
    apply_all(&mut hive.clone(), c, by, remotes, &g)
}

/// A change applied: the record after, what changed, and the budget step's refusal when it failed
/// after the rest had applied.
pub(crate) struct Committed {
    pub hive: Hive,
    pub lines: Vec<String>,
    pub failed: Option<String>,
}

/// `c` applied whole, checked again against the record under the write lock (`apply_all`), then
/// its budget. Err when nothing applied.
pub(crate) async fn commit<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, c: &SettingsChange, by: By, remotes: &[String]) -> Result<Committed, String> {
    // A host the queen grants alone must resolve, and nowhere home; the human's may not resolve yet.
    let hosts: Vec<String> = c.add_hosts.iter().chain(c.add_generator_hosts.iter()).cloned().collect();
    change::refuse_private_hosts(&hosts, by == By::Coordinator, &change::ip_remotes(remotes)).await?;
    let current = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    let g = gather(ctx, caller, &current, c, remotes).await?;
    let mut outcome: Result<Prepared, String> = Err("This Hive is gone.".to_string());
    // A refused change leaves the record as it was (saved again, unchanged: harmless).
    let mut hive = super::finish::write_status(ctx, caller, &[STATUS_ACTIVE, super::fuses::STATUS_PAUSED], None, |h| {
        outcome = apply_all(h, c, by, remotes, &g);
    })?;
    let done = outcome?;
    let mut lines = done.lines;
    let mut failed = None;
    if let Some(b) = done.change.budget_cents {
        let (p, id) = (caller.paths.clone(), caller.hive.id.clone());
        match super::tools::blocking(move || Ok(gallery::spent_of(&id, &p))).await {
            Ok(spent) => {
                // Read and written with no await between: the lists stay as they are now.
                let res = match super::autonomy::current_hive(ctx, caller) {
                    Some(fresh) => {
                        let gc = GalleryChange { budget_cents: b, output_folders: fresh.settings.gallery.output_folders.clone(), generator_hosts: fresh.settings.gallery.generator_hosts.clone() };
                        gallery::set_checked(ctx, &caller.label, &caller.workspace_id, &caller.hive.id, gc, spent).await
                    }
                    None => Err("This Hive is gone.".to_string()),
                };
                match res {
                    Ok(h) => hive = h,
                    Err(e) => failed = Some(e),
                }
            }
            Err(e) => failed = Some(e),
        }
        if failed.is_some() {
            lines.retain(|l| !l.starts_with("Budget: "));
        }
    }
    match (lines.is_empty(), failed) {
        (true, Some(e)) => Err(e),
        (true, None) => Err(change::NOTHING.to_string()),
        (false, failed) => Ok(Committed { hive, lines, failed }),
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
    let now = if is_empty(&now) { Prepared::default() } else { prepare(ctx, caller, &hive, &now, By::Coordinator, &remotes).await? };
    let card = if is_empty(&card) { Prepared::default() } else { prepare(ctx, caller, &hive, &card, By::Approved, &remotes).await? };
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
            note.push_str(&format!(" The budget wasn\u{2019}t changed: {e}"));
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
            health_check_minutes: Some(30),
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

    fn gathered(pairs: &[(&str, &str)]) -> Gathered {
        Gathered { detected: None, canon: pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect() }
    }

    /// The gallery part is only what differs, folders in their canonical form (a folder already
    /// listed under its canonical name isn't added twice), and her raise is refused.
    #[test]
    fn the_gallery_part_is_only_what_differs_in_canonical_form() {
        let mut h = hive();
        h.settings.gallery.generator_hosts = vec!["old.example.com".into()];
        h.settings.gallery.output_folders = vec!["/private/tmp/a".into(), "/Users/u/renders".into()];
        let g = gathered(&[("/tmp/a", "/private/tmp/a"), ("~/renders", "/Users/u/renders"), ("/tmp/new", "/private/tmp/new")]);
        let c = SettingsChange {
            budget_cents: Some(500),
            add_generator_hosts: vec!["New.Example.com".into(), "old.example.com".into()],
            add_output_folders: vec!["~/renders".into(), "/tmp/new".into()],
            remove_output_folders: vec!["/tmp/a".into(), "/tmp/never".into()],
            ..Default::default()
        };
        let mut after = h.clone();
        let p = apply_all(&mut after, &c, By::Coordinator, &[], &g).unwrap();
        assert_eq!(p.change.add_generator_hosts, vec!["new.example.com".to_string()], "only what differs");
        assert_eq!(p.change.add_output_folders, vec!["/private/tmp/new".to_string()], "canonical, and ~/renders is already there");
        assert_eq!(p.change.remove_output_folders, vec!["/private/tmp/a".to_string()], "found by its canonical form");
        assert_eq!(p.change.budget_cents, Some(500));
        assert_eq!(after.settings.gallery.output_folders, vec!["/Users/u/renders".to_string(), "/private/tmp/new".to_string()]);
        assert_eq!(after.budget_cents, 1000, "the budget is commit's second step");
        assert_eq!(p.lines.len(), 4, "{:?}", p.lines);

        let unchecked = SettingsChange { add_output_folders: vec!["/elsewhere".into()], ..Default::default() };
        assert!(apply_all(&mut h.clone(), &unchecked, By::Coordinator, &[], &g).unwrap_err().contains("checked"));
        let raise = SettingsChange { budget_cents: Some(5000), ..Default::default() };
        assert!(apply_all(&mut h.clone(), &raise, By::Coordinator, &[], &g).unwrap_err().contains("card"));
        assert!(apply_all(&mut h.clone(), &raise, By::Approved, &[], &g).is_ok());
        let same = SettingsChange { budget_cents: Some(1000), add_generator_hosts: vec!["old.example.com".into()], ..Default::default() };
        assert!(apply_all(&mut h.clone(), &same, By::Approved, &[], &g).unwrap().lines.is_empty(), "a no-op can't ride on a card");
    }

    /// A refused part leaves the whole record as it was: nothing applies by halves.
    #[test]
    fn a_change_applies_whole_or_not_at_all() {
        let mut h = hive();
        h.settings.gallery.generator_hosts = (0..MAX_GENERATOR_HOSTS).map(|i| format!("h{i}.example.com")).collect();
        let before = h.clone();
        let c = SettingsChange { cap: Some(5), add_generator_hosts: vec!["one-more.example.com".into()], ..Default::default() };
        assert!(apply_all(&mut h, &c, By::Approved, &[], &Gathered::default()).unwrap_err().contains("At most"));
        assert_eq!(h, before, "the cap didn't apply either");
        let tools = SettingsChange { add_tools: vec!["mcp:x".into()], ..Default::default() };
        assert!(apply_all(&mut h, &tools, By::Approved, &[], &Gathered::default()).unwrap_err().contains("detected"));
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
