//! The human's curation (docs/hive.md §10.4, §10.6, §10.7; step 8e): tagging and status (any
//! status, whatever the Finals checkpoint), collections, the trash and Export…. Each command is
//! checked against the asking window, as the view's are; no MCP call reaches a Tauri command, so
//! nothing here can be an agent's act (plan D11: agents have no trash; D14: only the human
//! overwrites).
//!
//! The human's status changes are the Finals checkpoint's evidence (§7.6): one on an asset the
//! coordinator recommended under Ask me is the human's answer to it, counted toward handing
//! Finals over; one that changes a finals call the coordinator made itself is an override of it,
//! counted toward handing Finals back. Picks with neither are the human curating, not logged.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use tauri::AppHandle;

use super::deliver;
use super::model::*;
use super::tools::{finals_question, is_finals_move, open_recommendation, open_recommendations, status_label, MAX_TAG, MAX_TAGS};
use super::view::{gallery_of, VIEW_LIMIT};
use super::Gallery;
use crate::hive::checkpoints;
use crate::hive::decisions::{self, DecisionLine, BY_HUMAN, EVENT_ANSWERED, SOURCE_GALLERY};
use crate::hive::deliver::one_line;
use crate::hive::rpc::HiveWindow;
use crate::hive::tools::{blocking, Caller, Ctx};
use crate::state::AppState;

/// An override's note, as the decision log and the coordinator read it.
fn override_note(to: &str) -> String {
    format!("Your human set it to {} in the gallery", status_label(to))
}

fn check_ids(ids: &[String]) -> Result<Vec<String>, String> {
    if ids.is_empty() {
        return Err("No assets named.".to_string());
    }
    if ids.len() > VIEW_LIMIT as usize {
        return Err(format!("At most {VIEW_LIMIT} assets at a time."));
    }
    let mut out: Vec<String> = Vec::new();
    for id in ids {
        if !is_asset_id(id) {
            return Err(format!("\"{}\" isn't an asset id.", one_line(id, 40)));
        }
        if !out.contains(id) {
            out.push(id.clone());
        }
    }
    Ok(out)
}

fn check_change(c: &TagChange) -> Result<(), String> {
    if let Some(s) = c.status.as_deref() {
        if !STATUSES.contains(&s) {
            return Err(format!("\"{}\" isn't a status: \"\", picked, final or rejected.", one_line(s, 20)));
        }
    }
    if c.tags.len() > MAX_TAGS || c.untag.len() > MAX_TAGS {
        return Err(format!("At most {MAX_TAGS} tags at a time."));
    }
    if c.tags.iter().chain(&c.untag).any(|t| t.chars().count() > MAX_TAG) {
        return Err(format!("A tag is at most {MAX_TAG} characters."));
    }
    Ok(())
}

/// What a human's status change meant to the decision log.
#[derive(Debug, Default)]
pub(crate) struct Curated {
    pub assets: Vec<Asset>,
    /// Recommendations answered: (asset, its title, recommended, the human's answer).
    pub answered: Vec<(String, String, String, String)>,
    pub overridden: Vec<decisions::Overridden>,
}

/// The answer to an open recommendation: the human's line on the coordinator's decision.
fn answer_line(open: &decisions::Decision, a: &Asset, to: &str, now: &str) -> DecisionLine {
    let r = open.recommended.clone().unwrap_or_default();
    let (question, _, answer) = finals_question(a, to);
    DecisionLine {
        id: open.id.clone(),
        event: EVENT_ANSWERED.to_string(),
        hive_id: r.hive_id.clone(),
        category: checkpoints::FINALS.to_string(),
        source: SOURCE_GALLERY.to_string(),
        task_id: a.task_id.clone(),
        key: Some(a.id.clone()),
        question: if r.question.is_empty() { question } else { r.question.clone() },
        recommended: r.recommended.clone(),
        answer: Some(answer),
        by: BY_HUMAN.to_string(),
        at: now.to_string(),
        ..DecisionLine::default()
    }
}

/// Tags, star and any status, as the human sets them (plan D12), then what each status change
/// meant: an answer to the coordinator's open recommendation, or an override of its own finals
/// call, or nothing.
pub(crate) fn tag_as_human(g: &Gallery, paths: &crate::hive::paths::HivePaths, ids: &[String], change: &TagChange) -> Result<Curated, String> {
    let ids = check_ids(ids)?;
    check_change(change)?;
    let before = ids
        .iter()
        .map(|id| match g.get(id)? {
            None => Err(format!("No asset {id} in this Hive's gallery.")),
            Some(a) if a.purged => Err(format!("{id} was emptied from the trash: only its record is left.")),
            Some(a) => Ok(a),
        })
        .collect::<Result<Vec<_>, String>>()?;
    let assets = g.tag(&ids, change)?;
    let mut out = Curated { assets, ..Curated::default() };
    let Some(to) = change.status.as_deref() else { return Ok(out) };
    let lines = decisions::read(paths);
    let now = crate::commands::workspace::iso_now();
    for a in &before {
        // A status set while a recommendation is open answers it, the status it already had
        // included: that is how the human keeps it ("Keep picked") rather than toggling it off.
        if let Some(open) = open_recommendation(&lines, &a.id) {
            let l = answer_line(&open, a, to, &now);
            decisions::append(paths, &l)?;
            out.answered.push((a.id.clone(), a.title.clone(), l.recommended.unwrap_or_default(), status_label(to)));
            continue;
        }
        if a.status == to || !is_finals_move(&a.status, to) {
            continue;
        }
        // The asset's latest finals call, when it is the coordinator's and says otherwise.
        let mine: Vec<DecisionLine> = lines.iter().filter(|l| l.key.as_deref() == Some(a.id.as_str()) && l.source == SOURCE_GALLERY && l.category == checkpoints::FINALS).cloned().collect();
        let latest = decisions::fold(&mine).into_iter().next();
        if let Some(d) = latest.filter(|d| d.can_override() && d.answered.as_ref().and_then(|l| l.answer.as_deref()) != Some(status_label(to).as_str())) {
            out.overridden.push(decisions::override_decision(paths, &d.id, &override_note(to), &now)?);
        }
    }
    Ok(out)
}

/// The coordinator's word of what the human's picks meant: its recommendations answered, its
/// own calls overridden. Titles are agent text: escaped, on their own lines.
fn curated_notice(c: &Curated) -> Option<String> {
    if c.answered.is_empty() && c.overridden.is_empty() {
        return None;
    }
    const SHOWN: usize = 20;
    let mut s = String::new();
    if !c.answered.is_empty() {
        s.push_str("[maiTerm] Your human picked finals in the gallery, answering your recommendations:");
        for (id, title, rec, answer) in c.answered.iter().take(SHOWN) {
            s.push_str(&format!("\n- \u{201c}{}\u{201d} ({id}): {answer} (you recommended {rec})", one_line(title, 120)));
        }
        if c.answered.len() > SHOWN {
            s.push_str(&format!("\n- and {} more", c.answered.len() - SHOWN));
        }
    }
    if !c.overridden.is_empty() {
        if !s.is_empty() {
            s.push('\n');
        }
        s.push_str("[maiTerm] Your human changed finals you set, in the gallery; each is an override of your decision:");
        for o in c.overridden.iter().take(SHOWN) {
            let yours = o.decision.answered.as_ref().and_then(|l| l.answer.clone()).unwrap_or_default();
            s.push_str(&format!("\n- {} ({}): you answered {yours}; {}", one_line(o.decision.question(), 200), o.decision.id, o.note));
        }
        if c.overridden.len() > SHOWN {
            s.push_str(&format!("\n- and {} more", c.overridden.len() - SHOWN));
        }
    }
    Some(s)
}

/// `tag_as_human` carried out: the gallery and the decision log told, the coordinator told what
/// its calls came to, and an offer due on Finals filed (§7.6).
pub(crate) async fn human_tag<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, g: Arc<Gallery>, ids: Vec<String>, change: TagChange) -> Result<Vec<Asset>, String> {
    let paths = caller.paths.clone();
    let c = blocking(move || tag_as_human(&g, &paths, &ids, &change)).await?;
    let ids: Vec<String> = c.assets.iter().map(|a| a.id.clone()).collect();
    ctx.window.gallery_changed(caller, &ids);
    if c.answered.is_empty() && c.overridden.is_empty() {
        return Ok(c.assets);
    }
    log::info!("hive {}: the human answered {} finals recommendation(s), overrode {} finals decision(s)", caller.hive.id, c.answered.len(), c.overridden.len());
    if let (Some(text), Some(coord)) = (curated_notice(&c), crate::hive::tools::coordinator_tab(ctx.data, caller)) {
        let sent = ctx.window.deliver(&coord, text, "Hive finals").await;
        log::info!("hive {}: finals notice to the coordinator: {}", caller.hive.id, sent.describe());
    }
    ctx.window.decisions_changed(&caller.workspace_id);
    crate::hive::autonomy::review(ctx, caller).await;
    Ok(c.assets)
}

/// What an Export… wrote.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Exported {
    pub folder: PathBuf,
    pub names: Vec<String>,
    /// A collection's members in the trash, not written.
    pub skipped: usize,
}

/// The human's Export… (§10.6): the assets or a collection (in order, numbered) into any folder
/// that exists, as new files with suffixes, or with `replace` over files of the same name.
pub fn export(g: &Gallery, paths: &crate::hive::paths::HivePaths, ids: &[String], collection: Option<&str>, folder: &str, replace: bool) -> Result<Exported, String> {
    let ids = if ids.is_empty() { Vec::new() } else { check_ids(ids)? };
    if ids.len() > deliver::MAX_DELIVER {
        return Err(format!("At most {} assets at a time.", deliver::MAX_DELIVER));
    }
    let dir = crate::share::canon(&std::fs::canonicalize(folder).map_err(|e| format!("{folder}: {e}"))?);
    if !dir.is_dir() {
        return Err(format!("{} isn't a folder.", dir.display()));
    }
    let (assets, skipped, numbered) = super::tools::delivery_assets(g, &ids, collection)?;
    let names = deliver::derived_names(&assets, numbered);
    let items: Vec<(PathBuf, String)> = assets.iter().zip(&names).map(|(a, n)| (g.file_path(a), n.clone())).collect();
    let run = if replace { deliver::export_replacing(&dir, &items) } else { deliver::deliver_files(&dir, &items) };
    let names = deliver::log_run(paths, if replace { "human (replace)" } else { BY_HUMAN }, &dir, &assets, run)?;
    Ok(Exported { folder: dir, names, skipped })
}

/// Stored paths of assets out of the trash, in order: what Iterate, Send to worker and a drag
/// onto a composer attach (the agent opens the gallery's read-only copy).
pub fn stored_paths(g: &Gallery, ids: &[String]) -> Result<Vec<String>, String> {
    check_ids(ids)?
        .iter()
        .map(|id| match g.get(id)? {
            Some(a) if !a.in_trash() => Ok(g.file_path(&a).to_string_lossy().to_string()),
            Some(_) => Err(format!("{id} is in the trash: restore it to attach it.")),
            None => Err(format!("No asset {id} in this Hive's gallery.")),
        })
        .collect()
}

// ── The window's commands ──

#[tauri::command]
pub async fn gallery_tag(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    ids: Vec<String>,
    change: TagChange,
) -> Result<Vec<Asset>, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let ctx = crate::hive::tools::app_ctx(state.inner(), &app, &caller);
    human_tag(&ctx, &caller, g, ids, change).await
}

#[tauri::command]
pub async fn gallery_collect(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    change: CollectChange,
) -> Result<Collection, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    for list in [&change.add, &change.remove] {
        if !list.is_empty() {
            check_ids(list)?;
        }
    }
    let touched: Vec<String> = change.add.iter().chain(&change.remove).cloned().collect();
    let c = blocking(move || g.collect(&change)).await?;
    crate::hive::tools::app_ctx(state.inner(), &app, &caller).window.gallery_changed(&caller, &touched);
    Ok(c)
}

#[tauri::command]
pub async fn gallery_rename_collection(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    collection: String,
    name: String,
) -> Result<Collection, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let c = blocking(move || g.rename_collection(&collection, &name)).await?;
    crate::hive::tools::app_ctx(state.inner(), &app, &caller).window.gallery_changed(&caller, &[]);
    Ok(c)
}

/// The grouping goes; its assets stay.
#[tauri::command]
pub async fn gallery_delete_collection(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    collection: String,
) -> Result<Collection, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let c = blocking(move || g.delete_collection(&collection)).await?;
    log::info!("hive {}: the human deleted collection {} ({} assets kept)", caller.hive.id, c.id, c.asset_ids.len());
    crate::hive::tools::app_ctx(state.inner(), &app, &caller).window.gallery_changed(&caller, &c.asset_ids);
    Ok(c)
}

/// Into the trash (plan D11): hidden, its file kept until the human empties the trash.
#[tauri::command]
pub async fn gallery_trash(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, app: AppHandle, workspace_id: String, ids: Vec<String>) -> Result<usize, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let ids = check_ids(&ids)?;
    let (moved, touched) = (ids.clone(), ids);
    let n = blocking(move || g.trash(&moved)).await?;
    log::info!("hive {}: the human moved {n} asset(s) to the trash", caller.hive.id);
    crate::hive::tools::app_ctx(state.inner(), &app, &caller).window.gallery_changed(&caller, &touched);
    Ok(n)
}

#[tauri::command]
pub async fn gallery_restore(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, app: AppHandle, workspace_id: String, ids: Vec<String>) -> Result<usize, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let ids = check_ids(&ids)?;
    let back = ids.clone();
    let (n, pending) = blocking(move || {
        let n = g.restore(&back)?;
        // A thumbnail the trash interrupted is made now.
        let pending: Vec<String> = g.pending_thumbs()?.into_iter().filter(|p| back.contains(p)).collect();
        Ok((n, pending))
    })
    .await?;
    super::thumbs::enqueue(&caller.hive.id, pending);
    crate::hive::tools::app_ctx(state.inner(), &app, &caller).window.gallery_changed(&caller, &ids);
    Ok(n)
}

/// The whole trash's count, for the confirmation: emptying takes all of it, not what a filter shows.
#[tauri::command]
pub async fn gallery_trash_count(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<usize, String> {
    let (_, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    blocking(move || g.trash_count()).await
}

/// Empties the trash: files and thumbnails go, each row stays as a tombstone (plan D11).
#[tauri::command]
pub async fn gallery_empty_trash(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, app: AppHandle, workspace_id: String) -> Result<usize, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let n = blocking(move || g.purge_trash()).await?;
    log::info!("hive {}: the human emptied the gallery's trash ({n} asset(s))", caller.hive.id);
    crate::hive::tools::app_ctx(state.inner(), &app, &caller).window.gallery_changed(&caller, &[]);
    Ok(n)
}

#[tauri::command]
pub async fn gallery_export(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    workspace_id: String,
    ids: Vec<String>,
    collection: Option<String>,
    folder: String,
    replace: bool,
) -> Result<Exported, String> {
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let paths = caller.paths.clone();
    let out = blocking(move || export(&g, &paths, &ids, collection.as_deref().filter(|c| !c.trim().is_empty()), &folder, replace)).await?;
    log::info!("hive {}: the human exported {} file(s) to {}{}", caller.hive.id, out.names.len(), out.folder.display(), if replace { ", replacing" } else { "" });
    Ok(out)
}

#[tauri::command]
pub async fn gallery_paths(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String, ids: Vec<String>) -> Result<Vec<String>, String> {
    let (_, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    blocking(move || stored_paths(&g, &ids)).await
}

/// The coordinator's finals recommendations waiting on the human: asset id → status.
#[tauri::command]
pub async fn gallery_recommendations(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<BTreeMap<String, String>, String> {
    let (caller, _) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let paths = caller.paths.clone();
    blocking(move || Ok(open_recommendations(&decisions::read(&paths)))).await
}

#[cfg(test)]
mod tests;
