//! The human's side of the gallery (docs/hive.md §10.4, step 8d): what the gallery view reads,
//! each command checked against the asking window as `hive_set_run_queue` is; the human's drop
//! (`made_by: human`, any path, plan D19); a board card's newest assets (§4.3); and a chat's
//! inline assets (§4.2), matched by who filed them and when, for display only (plan D16): the
//! transcript reader is untouched, so the phone's wire format is too.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::AppHandle;

use super::model::*;
use super::{ingest, Gallery, GALLERY_CHANGED_EVENT, INDEX};
use crate::hive::paths::HivePaths;
use crate::hive::tools::{blocking, caller_for_workspace, Caller};
use crate::hive::{worker, ROLE_COORDINATOR, ROLE_WORKER};
use crate::state::{AppData, AppState};

/// The human's view loads light rows up to this (plan D21).
pub const VIEW_LIMIT: u32 = 5_000;
/// A card shows its task's newest two (§4.3).
pub const CARD_ASSETS: usize = 2;
/// Files one drop takes.
pub const MAX_DROP: usize = 200;
/// A galleryAdd turn claims assets filed from 2 s before it (clocks, the call's own start)…
pub const TURN_BEFORE_MS: i64 = 2_000;
/// …to the next galleryAdd or user turn, or at most this long after it.
pub const TURN_AFTER_MS: i64 = 120_000;
/// A turn shows at most a call's worth.
const TURN_MAX: usize = super::tools::MAX_ITEMS;

/// An asset as a thumbnail somewhere else (a card, a chat): which picture it has, if any.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssetChip {
    pub id: String,
    pub kind: String,
    pub title: String,
    /// The `gallery://` part to draw: `thumb` (a poster) or `wave`; None for the kind's icon.
    pub picture: Option<String>,
}

fn flag(extras: &serde_json::Map<String, Value>, k: &str) -> bool {
    extras.get(k).and_then(Value::as_bool).unwrap_or(false)
}

/// The picture a tile or chip shows: the poster where one was made, else an audio file's
/// waveform. Nothing while thumbnails are pending or failed.
pub fn picture(thumb: &str, extras: &serde_json::Map<String, Value>) -> Option<String> {
    if thumb != THUMB_READY {
        return None;
    }
    if flag(extras, "poster") {
        Some("thumb".into())
    } else if flag(extras, "wave") {
        Some("wave".into())
    } else {
        None
    }
}

impl From<&AssetRow> for AssetChip {
    fn from(r: &AssetRow) -> Self {
        Self { id: r.id.clone(), kind: r.kind.clone(), title: r.title.clone(), picture: picture(&r.thumb, &r.extras) }
    }
}

impl From<&Asset> for AssetChip {
    fn from(a: &Asset) -> Self {
        Self { id: a.id.clone(), kind: a.kind.clone(), title: a.title.clone(), picture: picture(&a.thumb, &a.extras) }
    }
}

/// The Hive's gallery only when something was ever filed (its index exists): a board or a chat
/// read never makes one.
pub(crate) fn existing(hive_id: &str, paths: &HivePaths) -> Option<Arc<Gallery>> {
    if !paths.gallery().join(INDEX).is_file() {
        return None;
    }
    super::for_paths(hive_id, paths).map_err(|e| log::warn!("hive {hive_id}: gallery not opened: {e}")).ok()
}

/// A worker card's newest assets out of the trash, newest first (§4.3).
pub(crate) fn card_assets(g: &Gallery, task_id: &str) -> Vec<AssetChip> {
    let f = ListFilter { task_id: Some(task_id.to_string()), limit: Some(CARD_ASSETS as u32), ..ListFilter::default() };
    match g.list(&f) {
        Ok(rows) => rows.iter().map(AssetChip::from).collect(),
        Err(e) => {
            log::warn!("gallery {}: card assets of {task_id} not read: {e}", g.root().display());
            Vec::new()
        }
    }
}

// ── Inline assets in a chat (§4.2, plan D16) ──

/// Whose chat it is: a worker's by its task (a restart or reload keeps it), the coordinator's by
/// its role (a reload gives it a new tab).
#[derive(Debug, Clone, PartialEq)]
pub enum TurnsBy {
    Task(String),
    Coordinator,
}

/// A tool marker naming `galleryAdd`, under whichever MCP server it was reached by.
pub fn is_gallery_add(turn: &Value) -> bool {
    if turn.get("role").and_then(Value::as_str) != Some("tool") {
        return false;
    }
    let text = turn.get("text").and_then(Value::as_str).unwrap_or_default();
    let name = text.split('(').next().unwrap_or_default().trim();
    name == super::tools::ADD || name.ends_with(&format!("__{}", super::tools::ADD))
}

fn ts(turn: &Value) -> i64 {
    turn.get("ts").and_then(Value::as_i64).unwrap_or(0)
}

/// A single-path call files one asset; `galleryAdd` with `items` (or no path) shows bare.
fn expected(turn: &Value) -> Option<usize> {
    turn.get("text").and_then(Value::as_str).is_some_and(|t| t.contains('(')).then_some(1)
}

/// How a batch's assets (oldest first) split across its calls, in the order they were sent: a
/// single-path call takes one, a call of unknown size what the later single-path calls leave,
/// and the last call the rest.
pub fn split_batch(n: usize, expect: &[Option<usize>]) -> Vec<usize> {
    let mut left = n;
    let mut out = Vec::with_capacity(expect.len());
    for (p, e) in expect.iter().enumerate() {
        let reserved: usize = expect[p + 1..].iter().flatten().sum();
        let take = if p + 1 == expect.len() { left } else { e.map_or(left.saturating_sub(reserved), |c| c.min(left)) };
        let take = take.min(TURN_MAX);
        out.push(take);
        left -= take;
    }
    out
}

/// Gives each `galleryAdd` tool turn `gallery_assets`: the assets this chat's agent filed from
/// 2 s before the turn to the next galleryAdd or user turn (at most 120 s on), oldest first,
/// each claimed by one turn only. Display only: an asset filed some other way in that window
/// shows there too.
///
/// Other tool turns never close a window: Claude Code writes each tool_use of one message as
/// its own line, seconds apart, and runs the calls only after the last, so an asset lands after
/// its message's later turns. For the same reason calls sent together form a batch: a call whose
/// window ends at the next galleryAdd with nothing filed yet was sent with it, and the batch's
/// assets are shared out in order (`split_batch`).
pub fn annotate_turns(g: &Gallery, by: &TurnsBy, turns: &mut [Value]) -> Result<(), String> {
    let calls: Vec<usize> = turns.iter().enumerate().filter(|(_, t)| is_gallery_add(t)).map(|(i, _)| i).collect();
    let Some(&first) = calls.first() else { return Ok(()) };
    let mut f = ListFilter { since_ms: Some(ts(&turns[first]) - TURN_BEFORE_MS), ..ListFilter::default() };
    match by {
        TurnsBy::Task(t) => f.task_id = Some(t.clone()),
        TurnsBy::Coordinator => f.made_by = Some(BY_COORDINATOR.to_string()),
    }
    let mut rows = g.list(&f)?;
    rows.reverse();
    let mut claimed = vec![false; rows.len()];
    // Where each call's window closes, and whether the next galleryAdd is what closed it.
    let stops: Vec<(i64, bool)> = calls
        .iter()
        .map(|&i| {
            let at = ts(&turns[i]);
            let cap = at + TURN_AFTER_MS;
            let next = turns[i + 1..].iter().find(|t| ts(t) > at && (is_gallery_add(t) || t.get("role").and_then(Value::as_str) == Some("user")));
            match next {
                Some(t) if ts(t) <= cap => (ts(t), is_gallery_add(t)),
                _ => (cap, false),
            }
        })
        .collect();
    let starts: Vec<i64> = calls.iter().map(|&i| ts(&turns[i]) - TURN_BEFORE_MS).collect();
    let expects: Vec<Option<usize>> = calls.iter().map(|&i| expected(&turns[i])).collect();
    let unclaimed_in = |claimed: &[bool], lo: i64, hi: i64| rows.iter().zip(claimed).filter(|(r, c)| !**c && (lo..hi).contains(&r.created_ms)).count();
    let mut k = 0;
    while k < calls.len() {
        let lo = starts[k];
        let mut last = k;
        while stops[last].1 && last + 1 < calls.len() && unclaimed_in(&claimed, lo, stops[last].0) < last + 1 - k {
            last += 1;
        }
        let end = stops[last].0;
        let batch = &calls[k..=last];
        let picked: Vec<usize> = (0..rows.len()).filter(|&j| !claimed[j] && (lo..=end).contains(&rows[j].created_ms)).collect();
        let counts = if batch.len() == 1 { vec![picked.len().min(TURN_MAX)] } else { split_batch(picked.len(), &expects[k..=last]) };
        let mut at = 0;
        for (&i, n) in batch.iter().zip(counts) {
            let chips: Vec<AssetChip> = picked[at..at + n].iter().map(|&j| AssetChip::from(&rows[j])).collect();
            for &j in &picked[at..at + n] {
                claimed[j] = true;
            }
            at += n;
            if !chips.is_empty() {
                turns[i]["gallery_assets"] = json!(chips);
            }
        }
        k = last + 1;
    }
    Ok(())
}

/// `annotate_turns` for a live tab's chat when it is a Hive agent's (`get_tab_transcript`).
/// Blocks: a worker record and the index.
pub fn annotate_tab(app: &AppState, tab_id: &str, turns: &mut [Value]) {
    if !turns.iter().any(is_gallery_add) {
        return;
    }
    let found = {
        let data = app.app_data.read();
        crate::hive::find_hive_tab(&data, tab_id).and_then(|(ws, tab)| {
            let hive = ws.hive.as_ref()?;
            Some((hive.id.clone(), tab.hive_role.clone()?, crate::hive::paths::hive_root(&hive.id)?))
        })
    };
    let Some((hive_id, role, root)) = found else { return };
    let paths = HivePaths::new(root);
    let by = match role.as_str() {
        ROLE_COORDINATOR => TurnsBy::Coordinator,
        ROLE_WORKER => match worker::worker_for_tab(&paths, tab_id) {
            Some(w) => TurnsBy::Task(w.task_id),
            None => return,
        },
        _ => return,
    };
    if let Some(g) = existing(&hive_id, &paths) {
        if let Err(e) = annotate_turns(&g, &by, turns) {
            log::warn!("hive {hive_id}: inline assets of tab {tab_id} not read: {e}");
        }
    }
}

// ── The human's drop (plan D19) ──

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Refused {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AddResult {
    pub assets: Vec<Asset>,
    pub refused: Vec<Refused>,
}

/// Files one dropped file as the human's: any path (the drop is their act), checked and copied
/// as an agent's is, from the descriptor `authorize` opened, under the file's own folder. A
/// link is filed as what it points at. Never a duplicate: each drop is a new asset (plan D6).
pub fn add_human_file(g: &Gallery, path: &Path) -> Result<Asset, String> {
    if !path.is_absolute() {
        return Err(format!("{} isn't an absolute path.", path.display()));
    }
    let real = crate::share::canon(&std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?);
    let folder = real.parent().map(Path::to_path_buf).ok_or_else(|| format!("{} has no folder.", real.display()))?;
    let src = ingest::authorize(&real, &folder, std::slice::from_ref(&folder), g.root())?;
    let id = mint(ASSET_PREFIX);
    let stored = ingest::store_file(&src, &g.files_dir(), &id)?;
    let stem = real.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let title: String = stem.chars().filter(|c| !c.is_control()).take(super::tools::MAX_TITLE).collect();
    let inserted = g.insert_asset(NewAsset {
        id: id.clone(),
        kind: ingest::kind_for(&src.ext).to_string(),
        file: stored.file.clone(),
        mime: ingest::mime_for(&src.ext).to_string(),
        bytes: stored.bytes,
        sha256: stored.sha256,
        title: if title.trim().is_empty() { stored.file.clone() } else { title },
        made_by: MadeBy::human(),
        source_path: src.canonical.to_string_lossy().to_string(),
        thumb: THUMB_PENDING.to_string(),
        ..NewAsset::default()
    });
    inserted.inspect_err(|_| g.discard_file(&stored.file))
}

pub fn add_human_files(g: &Gallery, paths: &[String]) -> AddResult {
    let mut out = AddResult { assets: Vec::new(), refused: Vec::new() };
    for p in paths {
        match add_human_file(g, Path::new(p)) {
            Ok(a) => out.assets.push(a),
            Err(error) => out.refused.push(Refused { path: p.clone(), error }),
        }
    }
    out
}

// ── The window's commands ──

/// The Hive in workspace `workspace_id`, only when window `label` holds it.
pub(crate) fn hive_in_window(data: &AppData, label: &str, workspace_id: &str) -> Result<Caller, String> {
    caller_for_workspace(data, workspace_id).filter(|c| c.label == label).ok_or_else(|| "That workspace isn't a Hive in this window.".to_string())
}

async fn gallery_of(state: &Arc<AppState>, label: &str, workspace_id: &str) -> Result<(Caller, Arc<Gallery>), String> {
    let caller = hive_in_window(&state.app_data.read(), label, workspace_id)?;
    let (id, paths) = (caller.hive.id.clone(), caller.paths.clone());
    let g = blocking(move || super::for_paths(&id, &paths)).await?;
    Ok((caller, g))
}

/// The human's list: the trash only when asked for, up to `VIEW_LIMIT` rows, newest first.
pub fn view_list(g: &Gallery, mut filter: ListFilter) -> Result<Vec<AssetRow>, String> {
    filter.limit = Some(filter.limit.unwrap_or(VIEW_LIMIT).clamp(1, VIEW_LIMIT));
    g.list(&filter)
}

#[tauri::command]
pub async fn gallery_list(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String, filter: ListFilter) -> Result<Vec<AssetRow>, String> {
    let (_, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    blocking(move || view_list(&g, filter)).await
}

/// One asset with what the detail view draws beside it: its stored path (Reveal, Copy path;
/// never launched), and its parents and children as chips.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AssetDetail {
    #[serde(flatten)]
    pub asset: Asset,
    pub path: PathBuf,
    pub parent_chips: Vec<AssetChip>,
    pub child_chips: Vec<AssetChip>,
}

pub fn detail(g: &Gallery, id: &str) -> Result<AssetDetail, String> {
    if !is_asset_id(id) {
        return Err("That isn't an asset id.".to_string());
    }
    let a = g.get(id)?.ok_or_else(|| format!("No asset {id} in this Hive's gallery."))?;
    let chip = |id: &str| -> Result<Option<AssetChip>, String> { Ok(g.get(id)?.filter(|p| !p.purged).map(|p| AssetChip::from(&p))) };
    let parent_chips = a.parents.iter().map(|p| chip(p)).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect();
    let child_chips = g.children_of(id)?.iter().map(|(c, _)| chip(c)).collect::<Result<Vec<_>, _>>()?.into_iter().flatten().collect();
    Ok(AssetDetail { path: g.file_path(&a), asset: a, parent_chips, child_chips })
}

#[tauri::command]
pub async fn gallery_get(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String, id: String) -> Result<AssetDetail, String> {
    let (_, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    blocking(move || detail(&g, &id)).await
}

#[tauri::command]
pub async fn gallery_collections(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String) -> Result<Vec<Collection>, String> {
    let (_, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    blocking(move || g.collections()).await
}

/// Files dropped on the gallery (§10.3): the human's, from anywhere.
#[tauri::command]
pub async fn gallery_add_files(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    paths: Vec<String>,
) -> Result<AddResult, String> {
    use tauri::Emitter;
    if paths.is_empty() || paths.len() > MAX_DROP {
        return Err(format!("Drop 1 to {MAX_DROP} files at a time."));
    }
    let (caller, g) = gallery_of(state.inner(), window.label(), &workspace_id).await?;
    let out = blocking(move || Ok(add_human_files(&g, &paths))).await?;
    let ids: Vec<String> = out.assets.iter().map(|a| a.id.clone()).collect();
    log::info!("hive {}: the human filed {} asset(s), {} refused", caller.hive.id, ids.len(), out.refused.len());
    for r in &out.refused {
        log::info!("hive {}: drop of {} refused: {}", caller.hive.id, r.path, r.error);
    }
    if !ids.is_empty() {
        super::thumbs::enqueue(&caller.hive.id, ids.clone());
        let payload = json!({ "workspace_id": workspace_id, "hive_id": caller.hive.id, "ids": ids });
        if let Err(e) = app.emit_to(window.label(), GALLERY_CHANGED_EVENT, payload) {
            log::warn!("hive {}: gallery change not sent to window {}: {e}", caller.hive.id, window.label());
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
