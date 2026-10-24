//! Shared memory (docs/hive.md §9): many agents writing one folder lose nothing.
//!
//! `index` is the pure rebuild of `MEMORY.md`, `front` reads a file's frontmatter, and `record`
//! is Rust's own record of who wrote each file. Here: the one writer every path goes through (the
//! watcher, an agent's Write/Edit seen by its hook, the Memory view's commands, a session start),
//! serialized per Hive, and the Memory view's commands.
//!
//! The writer's order is what makes a crash safe: `unfiled.md` before `MEMORY.md`, so a crash
//! between them repeats a line and never loses one, and `MEMORY.md` only by compare-and-swap over
//! the text it read, so an agent's Edit in between is read again, not written over.

pub mod front;
pub mod index;
pub mod record;

use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tauri::AppHandle;

use self::index::{MemFile, Opts, GENERATED_MD, MEMORY_MD, UNFILED_MD};
use self::record::{FileProv, MemoryRecord};
use super::paths::{is_component, HivePaths};
use crate::state::AppState;

pub const MEMORY_CHANGED_EVENT: &str = "hive-memory-changed";
/// After the last change in the folder (§9 "debounced").
const DEBOUNCE_MS: u64 = 1_500;
/// Compare-and-swap tries before the writer gives up until the next event.
const CAS_TRIES: usize = 5;
/// The Memory view's editor; a memory file is a note.
const MAX_FILE_BYTES: usize = 256 * 1024;
/// How deep the view lists files in subfolders (plan D6: listed, never indexed).
const VIEW_DEPTH: usize = 3;
const VIEW_MAX_FILES: usize = 1_000;

const UNFILED_FRONT: &str = "---\nname: Unfiled lines\ndescription: Lines found in MEMORY.md that link no memory file\n---\n\n";

/// One lock per Hive: the watcher, an attribution and the commands never interleave.
static LOCKS: LazyLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock_for(hive_id: &str) -> Arc<Mutex<()>> {
    LOCKS.lock().unwrap_or_else(|e| e.into_inner()).entry(hive_id.to_string()).or_default().clone()
}

fn with_lock<T>(hive_id: &str, f: impl FnOnce() -> T) -> T {
    let l = lock_for(hive_id);
    let _held = l.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}

fn is_link(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink())
}

/// A file maiTerm reads in a folder agents write: never through a symlink an agent planted.
fn read_regular(p: &Path) -> Option<Vec<u8>> {
    let m = fs::symlink_metadata(p).ok()?;
    if !m.file_type().is_file() {
        return None;
    }
    fs::read(p).ok()
}

fn read_text(p: &Path) -> String {
    read_regular(p).map(|b| String::from_utf8_lossy(&b).into_owned()).unwrap_or_default()
}

fn mtime_ms(m: &fs::Metadata) -> i64 {
    m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64)
}

/// A top-level file and what it holds now.
struct Listed {
    file: MemFile,
    sha: String,
}

/// The top-level `*.md` files: regular files only, no dot names (the writer's temp files).
fn list_top(dir: &Path) -> Vec<Listed> {
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    let mut out = Vec::new();
    for e in entries.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !name.ends_with(".md") {
            continue;
        }
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        if !m.file_type().is_file() {
            continue;
        }
        let Ok(bytes) = fs::read(e.path()) else { continue };
        let text = String::from_utf8_lossy(&bytes);
        out.push(Listed {
            file: MemFile { name, front: front::parse(&text), mtime_ms: mtime_ms(&m), size: m.len() },
            sha: sha256_hex(&bytes),
        });
    }
    out.sort_by(|a, b| a.file.name.cmp(&b.file.name));
    out
}

/// The record, or (a Hive made before step 7a) a new one that adopts the kept block on disk as
/// the human's (plan D4).
fn load_or_adopt(paths: &HivePaths) -> MemoryRecord {
    if let Some(r) = record::load(paths) {
        return r;
    }
    let kept = index::split(&read_text(&paths.memory_index())).map(|s| s.kept).unwrap_or_default();
    MemoryRecord { v: record::RECORD_VERSION, kept, ..MemoryRecord::default() }
}

/// Provenance by content (plan D7): a file whose bytes aren't what its last known writer left
/// reads `unknown`; a file gone from the folder leaves the record.
fn refresh(rec: &mut MemoryRecord, listed: &[Listed], now: &str) {
    let on_disk: BTreeSet<&str> = listed.iter().map(|l| l.file.name.as_str()).filter(|n| index::is_memory_file(n, rec)).collect();
    rec.files.retain(|n, _| on_disk.contains(n.as_str()));
    for l in listed.iter().filter(|l| on_disk.contains(l.file.name.as_str())) {
        match rec.files.get_mut(&l.file.name) {
            Some(p) if p.sha256 == l.sha => {}
            Some(p) => p.rewritten(record::BY_UNKNOWN, record::KIND_UNKNOWN, None, l.sha.clone(), now),
            None => {
                rec.files.insert(l.file.name.clone(), FileProv::new(record::BY_UNKNOWN, record::KIND_UNKNOWN, None, l.sha.clone(), now));
            }
        }
    }
}

/// `unfiled.md`'s body lines, frontmatter left out.
fn unfiled_body(text: &str) -> Vec<String> {
    let start = front::parse(text).body_start;
    text[start..].lines().map(|l| l.trim_end_matches('\r').to_string()).collect()
}

/// Appends the lines `unfiled.md` doesn't already hold: a line moved by an earlier pass that
/// crashed or retried before `MEMORY.md` was written is there once. Returns the new bytes, or
/// None when there was nothing new.
fn append_unfiled(paths: &HivePaths, lines: &[String]) -> Result<Option<Vec<u8>>, String> {
    let p = paths.memory_unfiled();
    let cur = read_text(&p);
    let mut have: BTreeSet<String> = unfiled_body(&cur).into_iter().collect();
    let fresh: Vec<&String> = lines.iter().filter(|l| have.insert((*l).clone())).collect();
    if fresh.is_empty() {
        return Ok(None);
    }
    let mut text = if cur.trim().is_empty() { UNFILED_FRONT.to_string() } else { cur };
    if !text.ends_with('\n') {
        text.push('\n');
    }
    for l in fresh {
        text.push_str(l);
        text.push('\n');
    }
    super::launch::write_bytes(&p, text.as_bytes())?;
    Ok(Some(text.into_bytes()))
}

/// Writes `text` over `MEMORY.md` only if it still holds `t0`. False when an agent wrote first.
fn cas_write(paths: &HivePaths, t0: &str, text: &str, seams: &Seams) -> Result<bool, String> {
    let dir = paths.memory();
    let target = paths.memory_index();
    let tmp = dir.join(format!(".MEMORY.md.maiterm-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let err = |e: std::io::Error| format!("{}: {e}", target.display());
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(&tmp).map_err(err)?;
    let written = f.write_all(text.as_bytes()).and_then(|_| f.sync_all());
    drop(f);
    if let Err(e) = written {
        let _ = fs::remove_file(&tmp);
        return Err(err(e));
    }
    if let Some(hook) = seams.before_rename {
        hook(&target);
    }
    if read_text(&target) != t0 {
        let _ = fs::remove_file(&tmp);
        return Ok(false);
    }
    fs::rename(&tmp, &target).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        err(e)
    })?;
    Ok(true)
}

/// Test seams into the writer.
#[derive(Default)]
pub(crate) struct Seams<'a> {
    /// After `unfiled.md` is written, before anything else: an Err stops the writer there.
    pub after_unfiled: Option<&'a dyn Fn() -> Result<(), String>>,
    /// Between the temp file's write and the re-read that guards the rename.
    pub before_rename: Option<&'a dyn Fn(&Path)>,
}

/// Rebuild `MEMORY.md` now. True when anything on disk changed (the view refetches). Blocks.
pub fn rebuild_now(hive_id: &str, paths: &HivePaths) -> Result<bool, String> {
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        rebuild_locked(paths, &mut rec, false, &Seams::default())
    })
}

/// The writer, under the Hive's lock, with the record as the caller left it (a human action may
/// have changed it). The record is saved whatever happens, so a written archive is never read
/// back as a memory file.
pub(crate) fn rebuild_locked(paths: &HivePaths, rec: &mut MemoryRecord, force_kept: bool, seams: &Seams) -> Result<bool, String> {
    if !paths.memory().is_dir() {
        return Ok(false);
    }
    let before = record::load(paths);
    let mut changed = false;
    let res = write_pass(paths, rec, force_kept, seams, &mut changed);
    if before.as_ref() != Some(rec) {
        record::save(paths, rec)?;
        changed = true;
    }
    res.map(|()| changed)
}

fn write_pass(paths: &HivePaths, rec: &mut MemoryRecord, force_kept: bool, seams: &Seams, changed: &mut bool) -> Result<(), String> {
    let dir = paths.memory();
    let mut cas_failed = 0;
    // Each pass that appends to `unfiled.md` starts again (that file is now the newest); the
    // second can append nothing new, so this bound is never reached by a healthy folder.
    for _pass in 0..(CAS_TRIES + 4) {
        let t0 = read_text(&paths.memory_index());
        let listed = list_top(&dir);
        let now = crate::commands::workspace::iso_now();
        refresh(rec, &listed, &now);
        let files: Vec<MemFile> = listed.iter().map(|l| l.file.clone()).collect();
        let r = index::rebuild_with(&t0, &files, rec, &now[..10.min(now.len())], Opts { force_kept });
        rec.status = r.status.clone();

        if !r.unfiled_append.is_empty() {
            if let Some(bytes) = append_unfiled(paths, &r.unfiled_append)? {
                let sha = sha256_hex(&bytes);
                rec.files
                    .entry(UNFILED_MD.to_string())
                    .and_modify(|p| p.rewritten(record::BY_UNKNOWN, record::KIND_UNKNOWN, None, sha.clone(), &now))
                    .or_insert_with(|| FileProv::new(record::BY_UNKNOWN, record::KIND_UNKNOWN, None, sha, &now));
                *changed = true;
                if let Some(hook) = seams.after_unfiled {
                    hook()?;
                }
                continue;
            }
        }
        if let Some((name, body)) = &r.archive {
            let p = dir.join(name);
            if read_regular(&p).as_deref() != Some(body.as_bytes()) {
                super::launch::write_bytes(&p, body.as_bytes())?;
                *changed = true;
            }
            if !rec.maiterm_files.contains(name) {
                rec.maiterm_files.push(name.clone());
            }
        }
        if let Some(text) = &r.generated_md {
            if read_regular(&paths.memory_generated()).as_deref() != Some(text.as_bytes()) {
                super::launch::write_bytes(&paths.memory_generated(), text.as_bytes())?;
                *changed = true;
            }
        }
        if let Some(text) = &r.memory_md {
            if !cas_write(paths, &t0, text, seams)? {
                cas_failed += 1;
                if cas_failed >= CAS_TRIES {
                    log::warn!("hive memory: {} kept changing under the rebuild; trying again at the next change", paths.memory_index().display());
                    return Ok(());
                }
                continue;
            }
            *changed = true;
        }
        rec.generated = r.generated;
        // Only now that MEMORY.md no longer links them.
        for stale in &r.stale_archives {
            let p = dir.join(stale);
            if fs::symlink_metadata(&p).is_ok() {
                if let Err(e) = fs::remove_file(&p) {
                    log::warn!("hive memory: {} not removed: {e}", p.display());
                    continue;
                }
                *changed = true;
            }
            rec.maiterm_files.retain(|m| m != stale);
        }
        if r.generated_md.is_none() && fs::symlink_metadata(paths.memory_generated()).is_ok() {
            fs::remove_file(paths.memory_generated()).map_err(|e| format!("{GENERATED_MD}: {e}"))?;
            *changed = true;
        }
        return Ok(());
    }
    log::warn!("hive memory: {} needed too many passes; trying again at the next change", paths.memory_index().display());
    Ok(())
}

// ── Attribution: an agent's Write/Edit, seen by its PostToolUse hook ──

/// The tools whose PostToolUse names the file they wrote.
const WRITE_TOOLS: [&str; 4] = ["Write", "Edit", "MultiEdit", "NotebookEdit"];

/// The path a hook event says a tool wrote, when it is one `observed` attributes.
pub fn tool_write_target<'a>(event_name: &str, event: &'a Value) -> Option<&'a str> {
    if event_name != "PostToolUse" || !event.get("tool_name").and_then(Value::as_str).is_some_and(|t| WRITE_TOOLS.contains(&t)) {
        return None;
    }
    let input = event.get("tool_input")?;
    input.get("file_path").or_else(|| input.get("notebook_path")).and_then(Value::as_str)
}

/// The memory file `path` names: directly in the Hive's `memory/` (by its canonical parent, the
/// leaf never resolved), a `.md` maiTerm doesn't generate. `MEMORY.md` is skipped: agent lines
/// there are absorbed by the rebuild.
pub fn memory_name_of(paths: &HivePaths, path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?.to_string();
    let parent = fs::canonicalize(path.parent()?).ok()?;
    if parent != fs::canonicalize(paths.memory()).ok()? {
        return None;
    }
    if !name.ends_with(".md") || name == MEMORY_MD || name == GENERATED_MD || !is_component(&name) {
        return None;
    }
    Some(name)
}

/// Who an attribution names.
#[derive(Debug, Clone, PartialEq)]
pub struct Writer {
    pub by: String,
    pub by_kind: String,
    pub task_id: Option<String>,
}

/// A Hive tab's writer: `coordinator`, or a worker as `#<badge> <title>`. Blocks on a small read.
pub fn writer_for(role: &str, paths: &HivePaths, tab_id: &str) -> Option<Writer> {
    match role {
        super::ROLE_COORDINATOR => Some(Writer { by: record::BY_COORDINATOR.into(), by_kind: record::KIND_COORDINATOR.into(), task_id: None }),
        super::ROLE_WORKER => {
            let w = super::worker::worker_for_tab(paths, tab_id)?;
            let title: String = w.title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(40).collect();
            Some(Writer { by: format!("#{} {title}", w.badge), by_kind: record::KIND_WORKER.into(), task_id: Some(w.task_id) })
        }
        _ => None,
    }
}

/// `name` was just written by `who`: its provenance is theirs at the bytes it holds now, then the
/// index is rebuilt. A later change nothing explains fails the hash and reads `unknown`. Blocks.
pub fn attribute(hive_id: &str, paths: &HivePaths, name: &str, who: &Writer) -> Result<bool, String> {
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        if rec.maiterm_files.iter().any(|m| m == name) {
            return rebuild_locked(paths, &mut rec, false, &Seams::default());
        }
        if let Some(bytes) = read_regular(&paths.memory().join(name)) {
            let sha = sha256_hex(&bytes);
            // A rebuild between the Write and this hook (another agent's, a session start, the
            // open view's) saw these exact bytes first and credited nobody: this writer was first.
            let first = rec.files.get(name).is_some_and(|p| p.created_by == record::BY_UNKNOWN && p.by == record::BY_UNKNOWN && p.sha256 == sha);
            set_prov(&mut rec, name, &who.by, &who.by_kind, who.task_id.clone(), sha);
            if let Some(p) = rec.files.get_mut(name).filter(|_| first) {
                p.created_by = who.by.clone();
            }
        }
        rebuild_locked(paths, &mut rec, false, &Seams::default())
    })
}

fn set_prov(rec: &mut MemoryRecord, name: &str, by: &str, kind: &str, task_id: Option<String>, sha: String) {
    let now = crate::commands::workspace::iso_now();
    match rec.files.get_mut(name) {
        Some(p) => p.rewritten(by, kind, task_id, sha, &now),
        None => {
            rec.files.insert(name.to_string(), FileProv::new(by, kind, task_id, sha, &now));
        }
    }
}

/// `events::observed`'s hook into this: any Hive tab's Write/Edit into `memory/`, the
/// coordinator's included. Off the hook's path.
pub fn attribute_tool_write(state: &Arc<AppState>, handle: &AppHandle, tab_id: &str, path: &str) {
    let (agent, board) = {
        let data = state.app_data.read();
        (super::hive_agent(&data, tab_id), super::hive_board_of(&data, tab_id))
    };
    let Some((hive, role, paths)) = agent else { return };
    let (handle, tab, path) = (handle.clone(), tab_id.to_string(), PathBuf::from(path));
    tauri::async_runtime::spawn_blocking(move || {
        let Some(name) = memory_name_of(&paths, &path) else { return };
        let Some(who) = writer_for(&role, &paths, &tab) else { return };
        match attribute(&hive.id, &paths, &name, &who) {
            Ok(true) => {
                if let Some((label, ws)) = &board {
                    emit_changed(&handle, label, ws);
                }
            }
            Ok(false) => {}
            Err(e) => log::warn!("hive {}: memory file {name} by {} not recorded: {e}", hive.id, who.by),
        }
    });
}

pub(crate) fn emit_changed(handle: &AppHandle, label: &str, workspace_id: &str) {
    use tauri::Emitter;
    let payload = serde_json::json!({ "workspace_id": workspace_id });
    if let Err(e) = handle.emit_to(label, MEMORY_CHANGED_EVENT, payload) {
        log::warn!("hive: memory-changed not sent to window {label}: {e}");
    }
}

// ── The watcher ──

type Watcher = notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>;

static WATCHERS: LazyLock<Mutex<HashMap<String, Watcher>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// A debounced, non-recursive watch on `dir`: `on_change` runs after the last change settles.
pub(crate) fn start_watcher(dir: &Path, on_change: impl Fn() + Send + 'static) -> Result<Watcher, String> {
    let mut debouncer = notify_debouncer_mini::new_debouncer(
        std::time::Duration::from_millis(DEBOUNCE_MS),
        move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| match res {
            Ok(events) if !events.is_empty() => on_change(),
            Ok(_) => {}
            Err(e) => log::warn!("hive memory: watch error: {e}"),
        },
    )
    .map_err(|e| format!("couldn't watch {}: {e}", dir.display()))?;
    debouncer.watcher().watch(dir, notify::RecursiveMode::NonRecursive).map_err(|e| format!("couldn't watch {}: {e}", dir.display()))?;
    Ok(debouncer)
}

/// The window and workspace a Hive is in now, by its id.
fn board_of_hive(state: &AppState, hive_id: &str) -> Option<(String, String)> {
    let data = state.app_data.read();
    data.windows.iter().find_map(|w| {
        w.workspaces.iter().find(|ws| ws.hive.as_ref().is_some_and(|h| h.id == hive_id)).map(|ws| (w.label.clone(), ws.id.clone()))
    })
}

/// Watch a Hive's memory folder: each settled change rebuilds, and a rebuild that changed
/// something tells its window. Replaces an older watch of the same Hive.
pub fn watch(state: &Arc<AppState>, handle: &AppHandle, hive_id: &str, paths: &HivePaths) {
    let (st, h, id, p) = (state.clone(), handle.clone(), hive_id.to_string(), paths.clone());
    let started = start_watcher(&paths.memory(), move || {
        let (st, h, id, p) = (st.clone(), h.clone(), id.clone(), p.clone());
        tauri::async_runtime::spawn_blocking(move || match rebuild_now(&id, &p) {
            Ok(true) => {
                if let Some((label, ws)) = board_of_hive(&st, &id) {
                    emit_changed(&h, &label, &ws);
                }
            }
            Ok(false) => {}
            Err(e) => log::warn!("hive {id}: memory not rebuilt: {e}"),
        });
    });
    match started {
        Ok(w) => {
            WATCHERS.lock().unwrap_or_else(|e| e.into_inner()).insert(hive_id.to_string(), w);
        }
        Err(e) => log::warn!("hive {hive_id}: {e}; MEMORY.md is rebuilt only on agent writes and session starts"),
    }
}

/// Before the Hive's folder is removed.
pub fn unwatch(hive_id: &str) {
    WATCHERS.lock().unwrap_or_else(|e| e.into_inner()).remove(hive_id);
}

/// At startup: one rebuild per Hive (an agent may have written while maiTerm was down), then a
/// watch on each. Off the setup thread.
pub fn watch_all(state: &Arc<AppState>, handle: &AppHandle) {
    let hives: Vec<(String, HivePaths)> = {
        let data = state.app_data.read();
        data.windows
            .iter()
            .flat_map(|w| w.workspaces.iter())
            .filter_map(|ws| ws.hive.as_ref())
            .filter_map(|h| Some((h.id.clone(), HivePaths::new(super::paths::hive_root(&h.id)?))))
            .collect()
    };
    if hives.is_empty() {
        return;
    }
    let (state, handle) = (state.clone(), handle.clone());
    std::thread::spawn(move || {
        for (id, paths) in hives {
            if let Err(e) = rebuild_now(&id, &paths) {
                log::warn!("hive {id}: memory not rebuilt at startup: {e}");
            }
            watch(&state, &handle, &id, &paths);
        }
    });
}

// ── The Memory view (§9) ──

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryFileView {
    /// Relative to `memory/`, `/`-separated for a file in a subfolder.
    pub name: String,
    pub title: String,
    pub description: String,
    #[serde(rename = "type")]
    pub type_: String,
    pub modified_ms: i64,
    pub size: u64,
    pub by: String,
    pub by_kind: String,
    pub created_by: String,
    pub indexed: bool,
    pub folded: bool,
    pub pinned: bool,
    pub in_subfolder: bool,
    /// One of maiTerm's archive folds: read-only.
    pub archive: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryBudget {
    pub lines: usize,
    pub bytes: usize,
    pub kept_lines: usize,
    pub kept_bytes: usize,
    pub max_lines: usize,
    pub max_bytes: usize,
    pub kept_max_lines: usize,
    pub kept_max_bytes: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryView {
    pub files: Vec<MemoryFileView>,
    /// The kept block as it is on disk (the record's when the fences are gone).
    pub kept: Vec<String>,
    /// `unfiled.md`'s lines the human hasn't chosen to keep there.
    pub unfiled: Vec<String>,
    pub status: Option<String>,
    pub kept_changed: bool,
    pub budget: MemoryBudget,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MemoryFileText {
    pub text: String,
    pub sha256: String,
}

fn sub_files(dir: &Path, rel: &str, depth: usize, out: &mut Vec<(String, fs::Metadata, PathBuf)>) {
    if depth == 0 || out.len() >= VIEW_MAX_FILES {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else { return };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let Ok(m) = fs::symlink_metadata(e.path()) else { continue };
        let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        if m.is_dir() {
            sub_files(&e.path(), &path, depth - 1, out);
        } else if m.file_type().is_file() && name.ends_with(".md") && !rel.is_empty() {
            out.push((path, m, e.path()));
        }
        if out.len() >= VIEW_MAX_FILES {
            return;
        }
    }
}

pub fn view(paths: &HivePaths) -> MemoryView {
    let rec = load_or_adopt(paths);
    let text = read_text(&paths.memory_index());
    let listed = list_top(&paths.memory());
    let files: Vec<MemFile> = listed.iter().map(|l| l.file.clone()).collect();
    let today = crate::commands::workspace::iso_now();
    let r = index::rebuild(&text, &files, &rec, &today[..10.min(today.len())]);
    let kept = shown_kept(&text, &rec);
    let mut out: Vec<MemoryFileView> = Vec::new();
    for l in &listed {
        let n = &l.file.name;
        if n == MEMORY_MD || n == GENERATED_MD {
            continue;
        }
        let archive = rec.maiterm_files.contains(n);
        let prov = rec.files.get(n);
        let (by, by_kind) = if archive {
            (record::BY_MAITERM.to_string(), record::KIND_MAITERM.to_string())
        } else {
            prov.map_or((record::BY_UNKNOWN.to_string(), record::KIND_UNKNOWN.to_string()), |p| (p.by.clone(), p.by_kind.clone()))
        };
        out.push(MemoryFileView {
            name: n.clone(),
            title: l.file.front.name.clone().unwrap_or_else(|| n.trim_end_matches(".md").to_string()),
            description: l.file.front.description.clone().unwrap_or_default(),
            type_: l.file.front.type_.clone().unwrap_or_default(),
            modified_ms: l.file.mtime_ms,
            size: l.file.size,
            created_by: prov.map_or_else(|| by.clone(), |p| if p.created_by.is_empty() { p.by.clone() } else { p.created_by.clone() }),
            by,
            by_kind,
            indexed: r.indexed.contains(n),
            folded: r.folded.contains(n),
            pinned: r.pinned.contains(n),
            in_subfolder: false,
            archive,
        });
    }
    let mut subs = Vec::new();
    sub_files(&paths.memory(), "", VIEW_DEPTH + 1, &mut subs);
    for (name, m, p) in subs {
        let f = front::parse(&read_text(&p));
        out.push(MemoryFileView {
            title: f.name.unwrap_or_else(|| name.rsplit('/').next().unwrap_or(&name).trim_end_matches(".md").to_string()),
            description: f.description.unwrap_or_default(),
            type_: f.type_.unwrap_or_default(),
            modified_ms: mtime_ms(&m),
            size: m.len(),
            by: record::BY_UNKNOWN.into(),
            by_kind: record::KIND_UNKNOWN.into(),
            created_by: record::BY_UNKNOWN.into(),
            indexed: false,
            folded: false,
            pinned: false,
            in_subfolder: true,
            archive: false,
            name,
        });
    }
    out.sort_by(|a, b| a.in_subfolder.cmp(&b.in_subfolder).then(b.modified_ms.cmp(&a.modified_ms)).then_with(|| a.name.cmp(&b.name)));
    let unfiled = unfiled_body(&read_text(&paths.memory_unfiled()))
        .into_iter()
        .filter(|l| !l.trim().is_empty() && !rec.unfiled_kept.contains(l))
        .collect();
    MemoryView {
        files: out,
        budget: MemoryBudget {
            lines: text.matches('\n').count() + usize::from(!text.is_empty() && !text.ends_with('\n')),
            bytes: text.len(),
            kept_lines: kept.len(),
            kept_bytes: index::kept_bytes(&kept),
            max_lines: index::CLAUDE_LINES,
            max_bytes: index::CLAUDE_BYTES,
            kept_max_lines: index::KEPT_MAX_LINES,
            kept_max_bytes: index::KEPT_MAX_BYTES,
        },
        kept,
        unfiled,
        status: rec.status.clone(),
        kept_changed: r.kept_changed,
    }
}

/// A path relative to `memory/` the view may name: components only, nothing hidden, no symlink on
/// the way, a `.md` file.
fn resolve(paths: &HivePaths, name: &str) -> Result<PathBuf, String> {
    let parts: Vec<&str> = name.split('/').collect();
    if parts.len() > VIEW_DEPTH + 1 || !parts.iter().all(|p| is_component(p) && !p.contains(['\\', ':'])) || !name.ends_with(".md") {
        return Err(format!("\"{name}\" isn't a memory file"));
    }
    let mut p = paths.memory();
    for part in parts {
        p = p.join(part);
        if is_link(&p) {
            return Err(format!("\"{name}\" goes through a symlink: maiTerm doesn't follow those here"));
        }
    }
    Ok(p)
}

pub fn read_file(paths: &HivePaths, name: &str) -> Result<MemoryFileText, String> {
    let p = resolve(paths, name)?;
    let bytes = read_regular(&p).ok_or_else(|| format!("{name} isn't there any more"))?;
    Ok(MemoryFileText { sha256: sha256_hex(&bytes), text: String::from_utf8_lossy(&bytes).into_owned() })
}

/// A file only the human's view may create or edit: top level, not maiTerm's own.
fn editable_name(rec: &MemoryRecord, name: &str) -> Result<(), String> {
    if !is_component(name) || !name.ends_with(".md") || name.contains(['\\', ':']) {
        return Err(format!("\"{name}\" can't name a memory file: one name ending in .md, in the memory folder itself"));
    }
    if name == MEMORY_MD || name == GENERATED_MD {
        return Err(format!("{name} is maiTerm's index: edit the kept block, or the files it lists"));
    }
    if name.starts_with(index::ARCHIVE_PREFIX) || rec.maiterm_files.iter().any(|m| m == name) {
        return Err(format!("\"{name}\" is a name maiTerm uses for its archives"));
    }
    Ok(())
}

/// The human's create or edit (§9: `human`). Provenance is recorded at the new bytes BEFORE they
/// are written, so the watcher's rebuild already finds them explained. `expect_sha` is what the
/// view opened (None: a new file).
pub fn write_file(hive_id: &str, paths: &HivePaths, name: &str, text: &str, expect_sha: Option<&str>) -> Result<(), String> {
    if text.len() > MAX_FILE_BYTES {
        return Err(format!("A memory file holds at most {} KB", MAX_FILE_BYTES / 1024));
    }
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        editable_name(&rec, name)?;
        let p = paths.memory().join(name);
        if is_link(&p) {
            return Err(format!("{name} is a symlink: maiTerm doesn't write through those here"));
        }
        let cur = read_regular(&p).map(|b| sha256_hex(&b));
        match (expect_sha, cur.as_deref()) {
            (None, Some(_)) => return Err(format!("{name} already exists: open it to edit it")),
            (Some(want), Some(have)) if want != have => return Err(format!("{name} changed since you opened it: reopen it to see the change")),
            (Some(_), None) => return Err(format!("{name} was deleted since you opened it")),
            _ => {}
        }
        set_prov(&mut rec, name, record::BY_HUMAN, record::KIND_HUMAN, None, sha256_hex(text.as_bytes()));
        record::save(paths, &rec)?;
        super::launch::write_bytes(&p, text.as_bytes())?;
        rebuild_locked(paths, &mut rec, false, &Seams::default()).map(|_| ())
    })
}

pub fn delete_file(hive_id: &str, paths: &HivePaths, name: &str, expect_sha: &str) -> Result<(), String> {
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        if name == MEMORY_MD || name == GENERATED_MD {
            return Err(format!("{name} is maiTerm's index, not a memory file"));
        }
        if rec.maiterm_files.iter().any(|m| m == name) {
            return Err(format!("{name} is maiTerm's archive of older lines: it goes when nothing needs it"));
        }
        let p = resolve(paths, name)?;
        let have = read_regular(&p).map(|b| sha256_hex(&b)).ok_or_else(|| format!("{name} isn't there any more"))?;
        if have != expect_sha {
            return Err(format!("{name} changed since you opened it: reopen it to see the change"));
        }
        fs::remove_file(&p).map_err(|e| format!("{name}: {e}"))?;
        rec.files.remove(name);
        rebuild_locked(paths, &mut rec, false, &Seams::default()).map(|_| ())
    })
}

fn check_kept(lines: &[String]) -> Result<(), String> {
    if index::kept_over_cap(lines) {
        return Err(format!(
            "The kept block holds at most {} lines and {} KB, since Claude reads only the first {} lines of MEMORY.md: this is {} lines, {} bytes",
            index::KEPT_MAX_LINES,
            index::KEPT_MAX_BYTES / 1000,
            index::CLAUDE_LINES,
            lines.len(),
            index::kept_bytes(lines)
        ));
    }
    Ok(())
}

/// A kept block changed by someone else must be settled (Keep as is, Restore mine) before the
/// human's pin can add to it: adding to Rust's copy would write over theirs.
fn refuse_drift(paths: &HivePaths, rec: &MemoryRecord) -> Result<(), String> {
    match index::split(&read_text(&paths.memory_index())) {
        Some(s) if s.kept != rec.kept => Err("The kept block was changed outside this view: keep it as is or restore yours first".into()),
        _ => Ok(()),
    }
}

/// The kept block the view shows: MEMORY.md's, or the record's when the fences are gone.
fn shown_kept(text: &str, rec: &MemoryRecord) -> Vec<String> {
    index::split(text).map(|s| s.kept).unwrap_or_else(|| rec.kept.clone())
}

pub const KEPT_SET: &str = "set";
pub const KEPT_ACCEPT: &str = "accept";
pub const KEPT_RESTORE: &str = "restore";

/// The human's kept block: `set` writes theirs, `accept` takes the block on disk as theirs,
/// `restore` writes their recorded one back over a block someone else changed. `seen` is the
/// block the view showed when they acted: one changed since (an agent's edit inside the watcher's
/// debounce) is refused, so nothing is written over or adopted as the human's unseen.
pub fn kept(hive_id: &str, paths: &HivePaths, action: &str, lines: Option<Vec<String>>, seen: Option<Vec<String>>) -> Result<(), String> {
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        let now = shown_kept(&read_text(&paths.memory_index()), &rec);
        if seen.as_ref() != Some(&now) {
            return Err("The kept block changed since you opened it: look at it once more".into());
        }
        match action {
            KEPT_SET => {
                let lines: Vec<String> = lines
                    .ok_or("Set needs the lines")?
                    .iter()
                    .map(|l| index::defuse(l.trim_end_matches(['\r', '\n'])))
                    .collect();
                if lines.iter().any(|l| l.contains(['\n', '\r'])) {
                    return Err("Each kept line is one line".into());
                }
                check_kept(&lines)?;
                rec.kept = lines;
                rebuild_locked(paths, &mut rec, true, &Seams::default())
            }
            KEPT_ACCEPT => {
                let s = index::split(&read_text(&paths.memory_index())).ok_or("MEMORY.md has no kept block to accept")?;
                // An over-cap block never becomes the human's copy: that copy is Restore's way back.
                check_kept(&s.kept)?;
                rec.kept = s.kept;
                rebuild_locked(paths, &mut rec, false, &Seams::default())
            }
            KEPT_RESTORE => rebuild_locked(paths, &mut rec, true, &Seams::default()),
            other => Err(format!("Unknown kept action \"{other}\"")),
        }
        .map(|_| ())
    })
}

/// A file's index line into the kept block: the human pinned it.
pub fn pin(hive_id: &str, paths: &HivePaths, name: &str) -> Result<(), String> {
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        refuse_drift(paths, &rec)?;
        let listed = list_top(&paths.memory());
        let f = listed.iter().find(|l| l.file.name == name && index::is_memory_file(name, &rec)).ok_or_else(|| format!("{name} isn't a memory file here"))?;
        let line = index::index_line(&f.file, &rec);
        if !rec.kept.contains(&line) {
            let mut next = rec.kept.clone();
            next.push(line);
            check_kept(&next)?;
            rec.kept = next;
        }
        rebuild_locked(paths, &mut rec, true, &Seams::default()).map(|_| ())
    })
}

pub const UNFILED_KEEP: &str = "keep";
pub const UNFILED_PIN: &str = "pin";
pub const UNFILED_DELETE: &str = "delete";

/// One unfiled line: leave it in `unfiled.md` (and stop offering it), pin it into the kept block,
/// or delete it from `unfiled.md`.
pub fn unfiled(hive_id: &str, paths: &HivePaths, line: &str, action: &str) -> Result<(), String> {
    with_lock(hive_id, || {
        let mut rec = load_or_adopt(paths);
        let p = paths.memory_unfiled();
        let text = read_text(&p);
        let body_at = front::parse(&text).body_start;
        let lines: Vec<&str> = text[body_at..].split_inclusive('\n').collect();
        let hit = lines.iter().position(|l| l.trim_end_matches(['\n', '\r']) == line).ok_or("That line isn't in unfiled.md any more")?;
        match action {
            UNFILED_KEEP => {
                if !rec.unfiled_kept.iter().any(|l| l == line) {
                    rec.unfiled_kept.push(line.to_string());
                }
                return rebuild_locked(paths, &mut rec, false, &Seams::default()).map(|_| ());
            }
            UNFILED_PIN => {
                refuse_drift(paths, &rec)?;
                let mut next = rec.kept.clone();
                next.push(index::defuse(line));
                check_kept(&next)?;
                rec.kept = next;
            }
            UNFILED_DELETE => {}
            other => return Err(format!("Unknown action \"{other}\"")),
        }
        let mut out = text[..body_at].to_string();
        for (i, l) in lines.iter().enumerate() {
            if i != hit {
                out.push_str(l);
            }
        }
        super::launch::write_bytes(&p, out.as_bytes())?;
        set_prov(&mut rec, UNFILED_MD, record::BY_UNKNOWN, record::KIND_UNKNOWN, None, sha256_hex(out.as_bytes()));
        rec.unfiled_kept.retain(|l| l != line);
        rebuild_locked(paths, &mut rec, action == UNFILED_PIN, &Seams::default()).map(|_| ())
    })
}

// ── Tauri commands: the human's, window-scoped ──

type Cmd = Result<MemoryView, String>;

/// Runs `f` for the Hive in this window's workspace, then answers the view and, when `emit`,
/// tells the window (its other views refetch).
async fn act(
    window: &tauri::Window,
    state: &Arc<AppState>,
    app: &AppHandle,
    workspace_id: &str,
    emit: bool,
    f: impl FnOnce(&str, &HivePaths) -> Result<(), String> + Send + 'static,
) -> Cmd {
    let caller = super::land::caller_in(state, window.label(), workspace_id)?;
    let (id, paths) = (caller.hive.id.clone(), caller.paths.clone());
    let v = super::tools::blocking(move || {
        f(&id, &paths)?;
        Ok(view(&paths))
    })
    .await?;
    if emit {
        emit_changed(app, window.label(), workspace_id);
    }
    Ok(v)
}

/// The Memory view's list: the folder rebuilt first, so it reads what an agent starting now reads.
#[tauri::command]
pub async fn hive_memory_list(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, app: AppHandle, workspace_id: String) -> Cmd {
    act(&window, state.inner(), &app, &workspace_id, false, |id, p| rebuild_now(id, p).map(|_| ())).await
}

#[tauri::command]
pub async fn hive_memory_read(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, workspace_id: String, name: String) -> Result<MemoryFileText, String> {
    let caller = super::land::caller_in(state.inner(), window.label(), &workspace_id)?;
    super::tools::blocking(move || read_file(&caller.paths, &name)).await
}

#[tauri::command]
pub async fn hive_memory_write(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    name: String,
    text: String,
    expect_sha: Option<String>,
) -> Cmd {
    act(&window, state.inner(), &app, &workspace_id, true, move |id, p| write_file(id, p, &name, &text, expect_sha.as_deref())).await
}

#[tauri::command]
pub async fn hive_memory_delete(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    name: String,
    expect_sha: String,
) -> Cmd {
    act(&window, state.inner(), &app, &workspace_id, true, move |id, p| delete_file(id, p, &name, &expect_sha)).await
}

#[tauri::command]
pub async fn hive_memory_kept(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    action: String,
    lines: Option<Vec<String>>,
    seen: Option<Vec<String>>,
) -> Cmd {
    act(&window, state.inner(), &app, &workspace_id, true, move |id, p| kept(id, p, &action, lines, seen)).await
}

#[tauri::command]
pub async fn hive_memory_pin(window: tauri::Window, state: tauri::State<'_, Arc<AppState>>, app: AppHandle, workspace_id: String, name: String) -> Cmd {
    act(&window, state.inner(), &app, &workspace_id, true, move |id, p| pin(id, p, &name)).await
}

#[tauri::command]
pub async fn hive_memory_unfiled(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    line: String,
    action: String,
) -> Cmd {
    act(&window, state.inner(), &app, &workspace_id, true, move |id, p| unfiled(id, p, &line, &action)).await
}

#[cfg(test)]
mod tests;
