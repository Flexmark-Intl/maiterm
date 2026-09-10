use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use super::workspace::{AppData, Layout, SplitDirection, SplitNode, TabType, Task, WindowData};

/// Tracks whether the last load_state() successfully parsed a real state file.
/// When false, save_state() will NOT overwrite the backup — preserving the last
/// known-good backup from being clobbered by a default/empty state.
static LOADED_SUCCESSFULLY: AtomicBool = AtomicBool::new(false);

/// Last mtime we observed on the main state file (millis since epoch).
/// Updated on successful load and after every successful save. Used by save_state()
/// to detect when another process has written to the file since we last touched it
/// (e.g. a stale/zombie maiTerm process), and abort rather than clobber newer data.
/// Zero means "no baseline yet" — the guard is skipped on first save after a
/// fresh launch with no existing state file.
static LAST_KNOWN_DISK_MTIME: AtomicU64 = AtomicU64::new(0);

/// Serializes save_state(). Saves are fired from the main thread, from tokio tasks,
/// and from the Claude Code / maiLink / comms HTTP handlers — 78 call sites, none of
/// which held a lock. Two overlapping saves would stomp the shared temp file (the
/// "Failed to rename temp file: No such file or directory" warnings in the log are
/// exactly that), and could interleave the stat-then-store in record_disk_mtime so
/// the baseline ended up BELOW the file's real mtime — permanently tripping the
/// conflict guard against this process's own write. Hold this for the whole
/// write-rename-rebaseline sequence so it is atomic with respect to other saves.
static SAVE_LOCK: Mutex<()> = Mutex::new(());

// Save timing diagnostics (global atomics — no AppState dependency needed)
static SAVE_COUNT: AtomicU64 = AtomicU64::new(0);
static SAVE_LAST_DURATION_US: AtomicU64 = AtomicU64::new(0);
static SAVE_TOTAL_DURATION_US: AtomicU64 = AtomicU64::new(0);
static SAVE_LAST_BYTES: AtomicU64 = AtomicU64::new(0);

fn file_mtime_ms(path: &PathBuf) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
}

fn record_disk_mtime(path: &PathBuf) {
    if let Some(mt) = file_mtime_ms(path) {
        LAST_KNOWN_DISK_MTIME.store(mt, Ordering::Relaxed);
    }
}

/// Our in-memory state, written when we refuse to overwrite someone else's file.
const CONFLICT_PREFIX: &str = "aiterm-state.conflict-";
/// The on-disk file we displaced when taking ownership back from a departed writer.
const SUPERSEDED_PREFIX: &str = "aiterm-state.superseded-";

fn get_conflict_path(timestamp_ms: u64) -> Option<PathBuf> {
    dirs::data_dir().map(|p| {
        p.join(app_data_slug())
            .join(format!("{}{}.json", CONFLICT_PREFIX, timestamp_ms))
    })
}

fn get_superseded_path(timestamp_ms: u64) -> Option<PathBuf> {
    dirs::data_dir().map(|p| {
        p.join(app_data_slug())
            .join(format!("{}{}.json", SUPERSEDED_PREFIX, timestamp_ms))
    })
}

/// How many snapshots to keep per kind. A conflict is retried on every save, so
/// without a cap a guard that stays tripped writes one full state file per second
/// until the disk fills — 644 files (~1.7GB) in the incident that prompted this.
const MAX_SNAPSHOTS: usize = 5;

fn prune_snapshots(prefix: &str) {
    let Some(dir) = dirs::data_dir().map(|p| p.join(app_data_slug())) else { return };
    let Ok(entries) = fs::read_dir(&dir) else { return };
    let mut snapshots: Vec<PathBuf> = entries
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && n.ends_with(".json"))
        })
        .collect();
    if snapshots.len() <= MAX_SNAPSHOTS {
        return;
    }
    // The timestamp is in the name, but sorting by mtime avoids trusting it.
    // Unreadable metadata sorts to the front (None first) and is dropped first.
    snapshots.sort_by_key(|p| fs::metadata(p).and_then(|m| m.modified()).ok());
    let doomed = snapshots.len() - MAX_SNAPSHOTS;
    for path in snapshots.into_iter().take(doomed) {
        let _ = fs::remove_file(path);
    }
}

/// Is another copy of this executable running right now?
///
/// The conflict guard exists to protect against a second maiTerm writing the same
/// state file. Asking the process table directly is what makes the guard
/// *recoverable*: a baseline lost to an instance that has since exited leaves
/// nothing to protect, and must not block this process forever.
///
/// Errs toward "yes" — if we cannot enumerate processes or identify ourselves, keep
/// guarding rather than risk clobbering a live peer.
///
/// Matches on the executable's FILE NAME, not its full path. On macOS sysinfo reads
/// `exe` from KERN_PROCARGS2, which is the path as passed to execve — `cargo run`
/// gives `target/debug/aiterm`, while current_exe() is always absolute, so comparing
/// whole paths finds no peer in the two configurations that actually share a data
/// dir: two `tauri:dev` sessions, and a second bundle copy (LaunchServices refuses to
/// start a second instance of the *same* bundle, so a real duplicate is always a
/// different path). A name match can only over-report, which costs a spurious abort;
/// a path match under-reports, which costs the user's state.
fn another_instance_running() -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let Ok(me_exe) = std::env::current_exe() else { return true };
    let Some(me_name) = me_exe.file_name() else { return true };
    let me_pid = std::process::id();

    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    sys.processes().values().any(|p| {
        p.pid().as_u32() != me_pid
            && p.exe().and_then(|exe| exe.file_name()).is_some_and(|name| name == me_name)
    })
}

pub fn get_save_stats() -> (u64, u64, u64, u64) {
    (
        SAVE_COUNT.load(Ordering::Relaxed),
        SAVE_LAST_DURATION_US.load(Ordering::Relaxed),
        SAVE_TOTAL_DURATION_US.load(Ordering::Relaxed),
        SAVE_LAST_BYTES.load(Ordering::Relaxed),
    )
}

pub fn app_data_slug() -> &'static str {
    if cfg!(debug_assertions) {
        "com.aiterm.dev"
    } else {
        "com.aiterm.app"
    }
}

pub fn get_state_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("aiterm-state.json"))
}

fn get_backup_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("aiterm-state.bak.json"))
}

/// Per-process temp file. SAVE_LOCK keeps this process's own saves off each other's
/// toes, but a second instance sharing the data dir has its own lock — a shared temp
/// name lets its rename pull the file out from under ours mid-save.
fn get_temp_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| {
        p.join(app_data_slug())
            .join(format!("aiterm-state.tmp-{}.json", std::process::id()))
    })
}

fn get_memory_trend_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("aiterm-memory-trend.json"))
}

fn get_crash_marker_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("aiterm-running.marker"))
}

/// Snapshot of the previous run's exit state, captured at startup.
#[derive(Clone, Default, serde::Serialize)]
pub struct PreviousRunInfo {
    /// True if the marker file existed at startup — meaning the previous run
    /// did not call clear_running_marker() before exiting.
    pub crashed: bool,
    /// Marker file mtime (seconds since epoch). For a crashed run, this is
    /// roughly the wall clock at last write — useful to correlate with
    /// memory_trend's last sample and macOS DiagnosticReports timestamps.
    pub marker_mtime_secs: Option<u64>,
}

use std::sync::OnceLock;
static PREVIOUS_RUN: OnceLock<PreviousRunInfo> = OnceLock::new();

/// Capture the previous run's state and arm the marker for this run.
/// Call ONCE at startup before any other init that might crash. Does NOT log —
/// tauri-plugin-log isn't initialized this early. Call `log_previous_run_status()`
/// from inside the Tauri setup() closure to surface the warning.
pub fn arm_running_marker() -> PreviousRunInfo {
    let info = PREVIOUS_RUN.get_or_init(|| {
        let Some(path) = get_crash_marker_path() else {
            return PreviousRunInfo::default();
        };
        let crashed = path.exists();
        let marker_mtime_secs = if crashed {
            fs::metadata(&path)
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs())
        } else {
            None
        };
        PreviousRunInfo { crashed, marker_mtime_secs }
    }).clone();

    // (Re-)write the marker so this run is now tracked. Done AFTER capturing
    // the previous-run state so we never erase evidence.
    touch_running_marker();

    info
}

/// Emit the previous-run warning if the marker survived. Call from inside the
/// Tauri setup() closure, where tauri-plugin-log is guaranteed to be active.
pub fn log_previous_run_status() {
    let info = previous_run_info();
    if info.crashed {
        log::warn!(
            "Previous run did not exit cleanly (running marker found, mtime_secs={:?})",
            info.marker_mtime_secs
        );
    }
}

/// Read the cached previous-run info captured by arm_running_marker(). Returns
/// default (crashed=false) if arm_running_marker() was never called — should
/// only happen if the diagnostics endpoint is hit before run() finishes init.
pub fn previous_run_info() -> PreviousRunInfo {
    PREVIOUS_RUN.get().cloned().unwrap_or_default()
}

/// Refresh the running-marker mtime so it stays close to "now" while the app
/// is alive. Called by the memory sampler each tick — gives us a tighter
/// upper bound on time-of-crash than just relying on app start time.
pub fn touch_running_marker() {
    let Some(path) = get_crash_marker_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = fs::write(&path, now_secs.to_string());
}

/// Delete the running marker. Call from the graceful-exit path so the next
/// startup knows we shut down cleanly.
pub fn clear_running_marker() {
    let Some(path) = get_crash_marker_path() else { return };
    if path.exists() {
        if let Err(e) = fs::remove_file(&path) {
            log::warn!("Failed to clear running marker: {}", e);
        }
    }
}

/// Load persisted memory samples from disk. Returns empty Vec on any failure
/// (missing file, parse error) — trend data is purely advisory.
pub fn load_memory_trend() -> Vec<super::app_state::MemorySample> {
    let Some(path) = get_memory_trend_path() else { return Vec::new() };
    let Ok(bytes) = fs::read(&path) else { return Vec::new() };
    serde_json::from_slice::<Vec<super::app_state::MemorySample>>(&bytes).unwrap_or_default()
}

/// Persist memory samples to disk. Errors are logged but not propagated —
/// trend data is purely advisory and we don't want a bad disk to take down the sampler.
pub fn save_memory_trend(samples: &[super::app_state::MemorySample]) {
    let Some(path) = get_memory_trend_path() else { return };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    match serde_json::to_vec(samples) {
        Ok(bytes) => {
            if let Err(e) = fs::write(&path, &bytes) {
                log::warn!("Failed to write memory trend: {}", e);
            }
        }
        Err(e) => log::warn!("Failed to serialize memory trend: {}", e),
    }
}

/// Patch raw JSON to migrate old action_type values before deserialization.
/// "alert" and "question" were briefly used as standalone action types before
/// being consolidated into "set_tab_state" with a separate tab_state field.
pub(crate) fn migrate_json(contents: &str) -> String {
    // Replace "action_type":"alert" with "action_type":"set_tab_state","tab_state":"alert"
    // and same for "question". Only matches inside action entries.
    contents
        .replace(r#""action_type":"alert""#, r#""action_type":"set_tab_state","tab_state":"alert""#)
        .replace(r#""action_type":"question""#, r#""action_type":"set_tab_state","tab_state":"question""#)
}

pub(crate) fn parse_state(contents: &str) -> Result<AppData, serde_json::Error> {
    let migrated = migrate_json(contents);
    serde_json::from_str::<AppData>(&migrated)
}

fn get_corrupt_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("aiterm-state.corrupt.json"))
}

/// Preserve a corrupt state file so the user can recover data manually.
fn preserve_corrupt(source: &PathBuf) {
    if let Some(corrupt_path) = get_corrupt_path() {
        if let Err(e) = fs::copy(source, &corrupt_path) {
            log::warn!("Failed to preserve corrupt state file: {}", e);
        } else {
            log::info!("Preserved corrupt state file at {:?}", corrupt_path);
        }
    }
}

pub fn load_state() -> AppData {
    let Some(path) = get_state_path() else {
        log::warn!("No data directory found");
        return AppData::default();
    };

    log::info!("Loading state from {:?}", path);

    if !path.exists() {
        log::info!("State file does not exist, using defaults");
        return AppData::default();
    }

    match fs::read_to_string(&path) {
        Ok(contents) => match parse_state(&contents) {
            Ok(data) => {
                LOADED_SUCCESSFULLY.store(true, Ordering::Relaxed);
                record_disk_mtime(&path);
                data
            }
            Err(e) => {
                log::error!("Failed to parse state file: {}. Trying backup.", e);
                preserve_corrupt(&path);
                record_disk_mtime(&path);
                load_from_backup()
            }
        },
        Err(e) => {
            log::error!("Failed to read state file: {}. Trying backup.", e);
            record_disk_mtime(&path);
            load_from_backup()
        }
    }
}

fn load_from_backup() -> AppData {
    let Some(backup_path) = get_backup_path() else {
        log::warn!("No backup path available, using defaults");
        return AppData::default();
    };

    if !backup_path.exists() {
        log::info!("No backup file found, using defaults");
        return AppData::default();
    }

    match fs::read_to_string(&backup_path) {
        Ok(contents) => match parse_state(&contents) {
            Ok(data) => {
                log::info!("Successfully loaded from backup");
                LOADED_SUCCESSFULLY.store(true, Ordering::Relaxed);
                data
            }
            Err(e) => {
                log::error!("Backup also corrupt: {}. Using defaults.", e);
                preserve_corrupt(&backup_path);
                AppData::default()
            }
        },
        Err(e) => {
            log::error!("Failed to read backup: {}. Using defaults.", e);
            AppData::default()
        }
    }
}

pub fn migrate_app_data(data: &mut AppData) {
    // One-time default-on flip for shell integration (Command Completion).
    // Powers the SSH-drop exit-code detection and the completed/failed tab
    // indicators. Runs once per profile; honors a later manual opt-out.
    if !data.preferences.shell_integration_default_migrated {
        data.preferences.shell_integration = true;
        data.preferences.shell_integration_default_migrated = true;
        log::info!("Migration: enabled shell_integration (Command Completion) by default");
    }

    // One-time default-on flip for Restore on Relaunch. Restores terminal
    // sessions/scrollback on launch. Runs once per profile; honors a later
    // manual opt-out.
    if !data.preferences.restore_session_default_migrated {
        data.preferences.restore_session = true;
        data.preferences.restore_session_default_migrated = true;
        log::info!("Migration: enabled restore_session (Restore on Relaunch) by default");
    }

    // Migrate from old single-window format to multi-window format
    if data.windows.is_empty() {
        if let Some(old_workspaces) = data.workspaces.take() {
            if !old_workspaces.is_empty() {
                let mut win = WindowData::new("main".to_string());
                win.workspaces = old_workspaces;
                win.active_workspace_id = data.active_workspace_id.take();
                win.sidebar_width = data.sidebar_width.unwrap_or(215);
                win.sidebar_collapsed = data.sidebar_collapsed.unwrap_or(false);
                data.windows.push(win);
                log::info!("Migration: moved old workspaces into WindowData 'main'");
            }
        }
    }

    // A "0 monitors" geometry key is a phantom layout, never a configuration: macOS
    // reports every screen gone while the displays sleep, and runs that predate that rule
    // saved the sleep-time rect under "0". Launch would then restore it whenever the app
    // came up in the dark. Nothing writes one any more (commands::window), so drop the
    // ones already on disk rather than leaving a trap for the next dark start.
    for win in data.windows.iter_mut() {
        if win.window_geometry.remove("0").is_some() {
            log::info!("Migration: dropped phantom 0-monitor geometry for window '{}'", win.label);
        }
    }

    // Drain the legacy single comms_binding into the comms_bindings list (a tab can
    // now work several threads at once — chat-monitor pickups). Deserialize-only field;
    // the next save writes only the list.
    for win in data.windows.iter_mut() {
        for ws in win.workspaces.iter_mut() {
            for pane in ws.panes.iter_mut() {
                for tab in pane.tabs.iter_mut() {
                    if let Some(b) = tab.comms_binding.take() {
                        tab.comms_bindings.push(b);
                        log::info!("Migration: moved legacy comms_binding into comms_bindings (tab {})", tab.id);
                    }
                }
            }
        }
    }

    // Overlord board rows move from the window to the workspace that owns them
    // (docs/tasks.md §3): tasks belong to a project, not to whichever window happens to
    // be showing it. Drained rather than copied, and `overlord_tasks` is deserialize-only
    // from here on, so the next save clears the old field and this can never run twice.
    // Read as raw JSON so the retired OverlordTask struct doesn't have to be kept alive
    // just to be deleted once.
    for win in data.windows.iter_mut() {
        let legacy = std::mem::take(&mut win.overlord_tasks);
        if legacy.is_empty() {
            continue;
        }
        let (mut moved, mut dropped) = (0u32, 0u32);
        for row in legacy {
            let str_at = |k: &str| row.get(k).and_then(|v| v.as_str()).map(str::to_string);
            // Every one of these was non-optional on the old struct, so a row missing any
            // of them is corrupt rather than merely old. Drop it: inventing a timestamp
            // would hand it a bogus age, and `updated_at` is exactly what the staleness
            // rules read.
            let (Some(id), Some(title), Some(workspace_id), Some(created_at), Some(updated_at)) = (
                str_at("id"),
                str_at("title"),
                str_at("workspace_id"),
                str_at("created_at"),
                str_at("updated_at"),
            ) else {
                dropped += 1;
                continue;
            };
            let Some(ws) = win.workspaces.iter_mut().find(|w| w.id == workspace_id) else {
                // The workspace is gone — so is the project. Nothing to attach to.
                dropped += 1;
                continue;
            };
            ws.tasks.push(Task {
                id,
                normalized_title: Task::normalize_title(&title),
                title,
                detail: None,
                // `state` was the old field name for what is now `status`. The old
                // "backlog" meant "not started", which is `todo` in the six-lane
                // vocabulary — today's `backlog` is a deliberate parking lot, and filing
                // live work there would hide it from every staleness signal.
                status: match str_at("state").as_deref() {
                    Some("backlog") | None => "todo".to_string(),
                    Some(other) => other.to_string(),
                },
                tab_id: str_at("tab_id"),
                blocked_by: Vec::new(),
                workstream_id: None,
                // The old board's "agent" rows were mirrored from Claude's private todo
                // store, which is what "imported" means now; "agent" has been redefined
                // as work an agent created through createTasks. Carrying the old label
                // across would strand those rows: the importer only ever retires
                // `imported` rows, so a mislabeled mirror could never be closed out and
                // would age into a permanent false "stale" card driving task_stale rules.
                origin: match str_at("origin").as_deref() {
                    Some("agent") => "imported".to_string(),
                    Some(other) => other.to_string(),
                    None => "human".to_string(),
                },
                created_at,
                updated_at,
                topic_id: str_at("topic_id"),
                notes: Vec::new(),
            });
            moved += 1;
        }
        if moved > 0 || dropped > 0 {
            log::info!(
                "Migration: moved {} Overlord board rows onto their workspaces ({} dropped as orphaned/malformed) in window '{}'",
                moved, dropped, win.label
            );
        }
    }

    // One-time vocabulary flip (docs/tasks.md §3). `backlog` was the default status for
    // every task written before the six-lane board — it meant "not started", which is now
    // `todo`. Today's `backlog` is a deliberate parking lot that is exempt from staleness
    // signals, so leaving old rows there would silently hide live work from the board and
    // from Overlord. Flag-guarded: re-running it would drag genuinely parked tasks back.
    if !data.preferences.tasks_backlog_vocabulary_migrated {
        let mut moved = 0u32;
        for win in data.windows.iter_mut() {
            for ws in win.workspaces.iter_mut() {
                for t in ws.tasks.iter_mut() {
                    if t.status == "backlog" {
                        t.status = "todo".to_string();
                        moved += 1;
                    }
                }
            }
        }
        data.preferences.tasks_backlog_vocabulary_migrated = true;
        if moved > 0 {
            log::info!("Migration: moved {} tasks from the old default 'backlog' to 'todo'", moved);
        }
    }

    let direction = match data.layout.as_ref() {
        Some(Layout::Vertical) => SplitDirection::Vertical,
        _ => SplitDirection::Horizontal,
    };

    // Per-window / per-workspace migrations
    for window in &mut data.windows {
        for workspace in &mut window.workspaces {
            // Migrate tabs: any tab with a non-default name that lacks custom_name flag
            for pane in &mut workspace.panes {
                for tab in &mut pane.tabs {
                    if !tab.custom_name && tab.name != "Terminal" {
                        tab.custom_name = true;
                        log::info!(
                            "Migration: set custom_name=true for tab '{}' (id={})",
                            tab.name, tab.id
                        );
                    }
                }
            }

            // Migrate split_root from flat pane list
            if workspace.split_root.is_none() && !workspace.panes.is_empty() {
                if workspace.panes.len() == 1 {
                    workspace.split_root = Some(SplitNode::Leaf {
                        pane_id: workspace.panes[0].id.clone(),
                    });
                } else {
                    let mut node = SplitNode::Leaf {
                        pane_id: workspace.panes[0].id.clone(),
                    };
                    for pane in &workspace.panes[1..] {
                        node = SplitNode::Split {
                            id: uuid::Uuid::new_v4().to_string(),
                            direction: direction.clone(),
                            ratio: 0.5,
                            children: Box::new((
                                node,
                                SplitNode::Leaf {
                                    pane_id: pane.id.clone(),
                                },
                            )),
                        };
                    }
                    workspace.split_root = Some(node);
                }
                log::info!(
                    "Migration: converted {} flat panes to split tree for workspace '{}'",
                    workspace.panes.len(),
                    workspace.name
                );
            }
        }
    }
}

pub fn save_state(data: &AppData) -> Result<(), String> {
    // Held for the whole sequence — see SAVE_LOCK. A poisoned lock still gives us the
    // guard (a panicking save is not a reason to stop saving), so recover it.
    let _saving = SAVE_LOCK.lock().unwrap_or_else(|e| e.into_inner());

    let save_start = std::time::Instant::now();
    let path = get_state_path().ok_or("Could not determine data directory")?;
    let temp_path = get_temp_path().ok_or("Could not determine temp path")?;
    let backup_path = get_backup_path().ok_or("Could not determine backup path")?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }

    // Conflict guard: if the on-disk file's mtime is newer than what we last
    // recorded, another process (e.g. a stale/zombie maiTerm) wrote since we
    // loaded or last saved. Refuse to clobber it; instead persist our in-memory
    // state to a timestamped conflict file so the user can investigate.
    let known_mtime = LAST_KNOWN_DISK_MTIME.load(Ordering::Relaxed);
    if known_mtime > 0 {
        if let Some(disk_mtime) = file_mtime_ms(&path) {
            // A stale baseline must not outlive the process that invalidated it. The
            // guard only re-records after a *successful* save, so aborting while the
            // other writer is already gone would latch this process out of saving for
            // the rest of its life — every tab, task and pane change silently failing,
            // with a conflict file written every second. Nothing is protected by that,
            // so take ownership instead.
            //
            // The file we are about to overwrite gets its own snapshot first. The
            // rolling .bak.json is NOT enough: it is rewritten on every save, so it
            // would hold the rescued content for about one second. Whoever wrote this
            // — a departed instance, or the user restoring a backup by hand while the
            // app runs — deserves better than that.
            if disk_mtime > known_mtime && !another_instance_running() {
                match get_superseded_path(disk_mtime).map(|dest| (fs::copy(&path, &dest), dest)) {
                    Some((Ok(_), dest)) => {
                        prune_snapshots(SUPERSEDED_PREFIX);
                        log::warn!(
                            "State file changed underneath us (disk mtime {} > known {}), but no other maiTerm is running — the writer has exited. Its copy is preserved at {:?}; re-baselining and saving.",
                            disk_mtime,
                            known_mtime,
                            dest
                        );
                        record_disk_mtime(&path);
                    }
                    // Couldn't preserve it — then don't destroy it. Fall through to the
                    // abort path, which keeps our own state in a conflict file.
                    other => {
                        if let Some((Err(e), dest)) = other {
                            log::error!("Could not snapshot the superseded state file to {:?}: {}. Leaving it alone.", dest, e);
                        }
                    }
                }
            }
        }
    }

    let known_mtime = LAST_KNOWN_DISK_MTIME.load(Ordering::Relaxed);
    if known_mtime > 0 {
        if let Some(disk_mtime) = file_mtime_ms(&path) {
            // Still newer with a live peer: a genuine two-instance conflict.
            if disk_mtime > known_mtime {
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);
                let conflict_path = get_conflict_path(now_ms)
                    .ok_or("Could not determine conflict path")?;
                let mut filtered = data.clone();
                for win in &mut filtered.windows {
                    for ws in &mut win.workspaces {
                        for pane in &mut ws.panes {
                            pane.tabs.retain(|t| t.tab_type != super::workspace::TabType::Diff);
                            for tab in &mut pane.tabs {
                                tab.scrollback = None;
                            }
                        }
                        for tab in &mut ws.archived_tabs {
                            tab.scrollback = None;
                        }
                    }
                }
                let json = serde_json::to_string_pretty(&filtered).map_err(|e| e.to_string())?;
                fs::write(&conflict_path, &json)
                    .map_err(|e| format!("Failed to write conflict file: {}", e))?;
                prune_snapshots(CONFLICT_PREFIX);
                log::error!(
                    "State save aborted: disk mtime {} > known {}. Another maiTerm process likely wrote since this one loaded. In-memory state preserved at {:?}.",
                    disk_mtime,
                    known_mtime,
                    conflict_path
                );
                return Err(format!(
                    "State conflict detected — preserved in-memory copy at {:?}",
                    conflict_path
                ));
            }
        }
    }

    // Clone and filter out ephemeral diff tabs before serializing
    let mut filtered = data.clone();
    for win in &mut filtered.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                pane.tabs.retain(|t| t.tab_type != super::workspace::TabType::Diff);
                // Reset active_tab_id if it pointed to a removed diff tab
                if let Some(ref active_id) = pane.active_tab_id {
                    if !pane.tabs.iter().any(|t| t.id == *active_id) {
                        pane.active_tab_id = pane.tabs.last().map(|t| t.id.clone());
                    }
                }
                // Strip scrollback — it's now persisted in SQLite
                for tab in &mut pane.tabs {
                    tab.scrollback = None;
                }
            }
            for tab in &mut ws.archived_tabs {
                tab.scrollback = None;
            }
        }
    }

    let json = serde_json::to_string_pretty(&filtered).map_err(|e| e.to_string())?;

    // Write to temp file first
    fs::write(&temp_path, &json).map_err(|e| format!("Failed to write temp file: {}", e))?;

    // Only back up the current file if we know it was loaded successfully.
    // This prevents a failed-parse → default-state → save cycle from
    // clobbering the last known-good backup.
    if path.exists() && LOADED_SUCCESSFULLY.load(Ordering::Relaxed) {
        if let Err(e) = fs::copy(&path, &backup_path) {
            log::warn!("Failed to create backup: {}", e);
        }
    }

    // Atomic rename temp -> real path
    fs::rename(&temp_path, &path).map_err(|e| format!("Failed to rename temp file: {}", e))?;

    // Record the new on-disk mtime so subsequent saves from THIS process pass
    // the conflict guard, while saves from any other (stale) process — which
    // still hold the older mtime — get blocked.
    record_disk_mtime(&path);

    // Record save timing
    let elapsed_us = save_start.elapsed().as_micros() as u64;
    SAVE_COUNT.fetch_add(1, Ordering::Relaxed);
    SAVE_LAST_DURATION_US.store(elapsed_us, Ordering::Relaxed);
    SAVE_TOTAL_DURATION_US.fetch_add(elapsed_us, Ordering::Relaxed);
    SAVE_LAST_BYTES.store(json.len() as u64, Ordering::Relaxed);

    Ok(())
}

/// Migrate scrollback data from JSON state to SQLite on first load.
pub fn migrate_scrollback_to_db(data: &mut AppData, db: &super::scrollback_db::ScrollbackDb) {
    let mut migrated = 0u32;
    for win in &mut data.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if let Some(ref scrollback) = tab.scrollback {
                        if !scrollback.is_empty() {
                            if let Err(e) = db.save(&tab.id, scrollback, None) {
                                log::error!("Failed to migrate scrollback for tab {}: {}", tab.id, e);
                            } else {
                                migrated += 1;
                            }
                        }
                        tab.scrollback = None;
                    }
                }
            }
            for tab in &mut ws.archived_tabs {
                if let Some(ref scrollback) = tab.scrollback {
                    if !scrollback.is_empty() {
                        if let Err(e) = db.save(&tab.id, scrollback, None) {
                            log::error!("Failed to migrate archived scrollback for tab {}: {}", tab.id, e);
                        } else {
                            migrated += 1;
                        }
                        tab.scrollback = None;
                    }
                }
            }
        }
    }
    if migrated > 0 {
        log::info!("Migration: moved {} tab scrollbacks from JSON to SQLite", migrated);
    }
}

/// One-time reconcile of the `pty_id` high-watermark. `pty_id` is set on spawn and
/// cleared only on explicit suspend, and several paths historically leaked it (a
/// restart that didn't re-spawn a tab, "suspend other tabs", workspace suspend), so
/// on an old state file nearly every tab looks live. This runs ONCE: a tab that
/// looks live (pty_id set, no suspend marker) but wasn't genuinely running at the
/// last shutdown — judged by scrollback recency, the only surviving record of the
/// real live set — has its pty_id cleared and a proper `suspended_at` stamped (its
/// last-active time). Afterward `pty_id` is authoritative and steady-state restore
/// uses it directly; the leak paths are plugged so it stays that way.
pub fn reconcile_tab_liveness(data: &mut AppData, db: &super::scrollback_db::ScrollbackDb) {
    // tab_id -> last scrollback save time, to stamp a meaningful "idle" age.
    let times: std::collections::HashMap<String, String> =
        db.tab_times().unwrap_or_default().into_iter().collect();

    // Part 1 — ONE-TIME high-watermark clear. On an old state file nearly every
    // tab looks live (pty_id was write-once). Clear pty_id on tabs that look live
    // but weren't running at last shutdown (judged by scrollback recency).
    if !data.tab_liveness_reconciled {
        // 24h relative to the newest save. The window is relative, so an app left
        // off for weeks still resolves the last shutdown batch correctly.
        const RECONCILE_WINDOW_MINUTES: i64 = 24 * 60;
        // If the recency query fails, skip WITHOUT setting the flag so it retries
        // next boot — better than a destructive clean-slate on a transient error.
        match db.recent_tab_ids(RECONCILE_WINDOW_MINUTES) {
            Ok(recent) => {
                let mut kept = 0usize;
                let mut suspended = 0usize;
                for win in &mut data.windows {
                    for ws in &mut win.workspaces {
                        for pane in &mut ws.panes {
                            for tab in &mut pane.tabs {
                                if !matches!(tab.tab_type, TabType::Terminal) {
                                    continue;
                                }
                                // Only the high-watermark: looks live (pty_id set,
                                // no suspend marker). Leave genuine suspended /
                                // never-spawned tabs alone.
                                if tab.pty_id.is_none() || tab.suspended_at.is_some() {
                                    continue;
                                }
                                if recent.contains(&tab.id) {
                                    kept += 1; // genuinely live at last shutdown
                                    continue;
                                }
                                tab.pty_id = None;
                                tab.suspended_at = times
                                    .get(&tab.id)
                                    .map(|t| t.replacen(' ', "T", 1) + "Z"); // SQLite UTC -> RFC3339
                                suspended += 1;
                            }
                        }
                    }
                }
                data.tab_liveness_reconciled = true;
                log::info!(
                    "Tab-liveness reconcile: kept {} live, marked {} stale tabs suspended",
                    kept, suspended
                );
            }
            Err(e) => {
                log::warn!("Tab-liveness reconcile skipped (scrollback query failed): {}", e);
            }
        }
    }

    // Part 2 — EVERY boot: normalize "limbo" terminal tabs (pty_id=None AND
    // suspended_at=None) to properly-suspended. These already render and resume
    // like suspended tabs, but a missing suspended_at means no "suspended Xd ago"
    // age and they get miscounted as "uninitialized" in diagnostics. They come
    // from PTYs that ended without going through a suspend path (legacy state,
    // pre-discipline versions). Stamp the scrollback save time (real idle age)
    // where we have it, else now. Idempotent once stamped; restore is unaffected
    // (pty_id is still None, so the tab was never restore-eligible).
    let mut normalized = 0usize;
    for win in &mut data.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if !matches!(tab.tab_type, TabType::Terminal) {
                        continue;
                    }
                    if tab.pty_id.is_some() || tab.suspended_at.is_some() {
                        continue;
                    }
                    tab.suspended_at = Some(
                        times
                            .get(&tab.id)
                            .map(|t| t.replacen(' ', "T", 1) + "Z")
                            .unwrap_or_else(crate::commands::workspace::iso_now),
                    );
                    normalized += 1;
                }
            }
        }
    }
    if normalized > 0 {
        log::info!("Tab-liveness: normalized {} limbo tab(s) to suspended", normalized);
    }

    backfill_agent_runtimes(data);
}

/// Every boot, idempotent: tag a terminal tab as an agent tab when its own persisted record
/// proves an agent has run in it, and nothing has said so.
///
/// `Tab.runtime` had exactly one writer for Claude — `initSession` — because the SessionStart
/// hook deliberately skipped it ("None defaults to claude"). That default holds on the frontend
/// and NOT in maiLink, where `designated_tabs` reads `runtime.is_some()` as "this is an agent
/// tab": a null runtime hides the tab from the phone completely. It stayed invisible only while
/// `/maiterm init` was still required of every agent; once the tab id started riding the wire,
/// nothing made an agent call init, and the writes stopped happening.
///
/// The hook now writes it for every runtime, which fixes this going forward and heals any tab
/// whose agent starts again. It cannot heal a DORMANT tab — no agent, so no SessionStart, ever —
/// and on the machine this was found on that was ~85% of the affected tabs. Their evidence is
/// already persisted: a `<runtime>SessionId` trigger variable is written only by a real
/// registration. Read it back rather than stranding them.
///
/// Conservative on purpose: it only ever fills a `None`, never corrects a runtime already set,
/// and a tab with no session variable is left alone (a plain shell tab must not become an agent
/// tab). Idempotent — the second boot finds nothing to do.
fn backfill_agent_runtimes(data: &mut AppData) {
    use crate::state::AgentRuntime;
    const RUNTIMES: [AgentRuntime; 3] =
        [AgentRuntime::Claude, AgentRuntime::Codex, AgentRuntime::Gemini];

    let mut tagged = 0usize;
    for win in &mut data.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if !matches!(tab.tab_type, TabType::Terminal) || tab.runtime.is_some() {
                        continue;
                    }
                    let found = RUNTIMES.into_iter().find(|rt| {
                        let var = crate::state::agent_runtime::descriptor(*rt).session_id_var;
                        tab.trigger_variables.get(var).is_some_and(|v| !v.trim().is_empty())
                    });
                    if let Some(rt) = found {
                        tab.runtime = Some(rt);
                        tagged += 1;
                    }
                }
            }
        }
    }
    if tagged > 0 {
        log::info!(
            "Agent-runtime backfill: tagged {} tab(s) from a persisted session id — they were \
             invisible to maiLink",
            tagged
        );
    }
}

#[cfg(test)]
mod migration_tests {
    use super::*;
    use crate::state::workspace::Workspace;

    #[test]
    fn a_tab_that_has_run_an_agent_is_tagged_as_one() {
        use crate::state::workspace::Tab;
        use crate::state::AgentRuntime;
        let mut ws = Workspace::new("EWS Mesh".to_string());
        ws.panes[0].tabs.clear(); // drop the auto-created Terminal tab so indices are the cases
        let mut mk = |name: &str, var: Option<(&str, &str)>, rt: Option<AgentRuntime>| {
            let mut t = Tab::new(name.to_string());
            t.runtime = rt;
            if let Some((k, v)) = var {
                t.trigger_variables.insert(k.to_string(), v.to_string());
            }
            ws.panes[0].tabs.push(t);
        };
        // The reported shape: Claude has unmistakably run here, but nothing ever wrote runtime
        // because this agent never happened to call initSession.
        mk("Backoffice Social Campaigns", Some(("claudeSessionId", "b5e41230")), None);
        mk("codex tab", Some(("codexSessionId", "c-1")), None);
        // A plain shell tab must NOT become an agent tab.
        mk("just a shell", None, None);
        // An empty variable is not evidence.
        mk("empty var", Some(("claudeSessionId", "  ")), None);
        // An already-tagged tab is never re-decided, even if a stale sibling var disagrees.
        mk("already codex", Some(("claudeSessionId", "x")), Some(AgentRuntime::Codex));

        let mut win = WindowData::new("main".to_string());
        win.workspaces.push(ws);
        let mut data = AppData::default();
        data.windows.push(win);

        backfill_agent_runtimes(&mut data);
        let tabs = &data.windows[0].workspaces[0].panes[0].tabs;
        assert_eq!(tabs[0].runtime, Some(AgentRuntime::Claude));
        assert_eq!(tabs[1].runtime, Some(AgentRuntime::Codex));
        assert_eq!(tabs[2].runtime, None, "a shell tab stays a shell tab");
        assert_eq!(tabs[3].runtime, None, "an empty session id proves nothing");
        assert_eq!(tabs[4].runtime, Some(AgentRuntime::Codex), "never re-decided");

        // Idempotent: a second boot changes nothing.
        let before = data.clone();
        backfill_agent_runtimes(&mut data);
        let after = &data.windows[0].workspaces[0].panes[0].tabs;
        for (i, t) in before.windows[0].workspaces[0].panes[0].tabs.iter().enumerate() {
            assert_eq!(t.runtime, after[i].runtime);
        }
    }

    fn window_with_legacy(workspace_id: &str, rows: serde_json::Value) -> AppData {
        let mut ws = Workspace::new("Project".to_string());
        ws.id = workspace_id.to_string();
        let mut win = WindowData::new("main".to_string());
        win.workspaces.push(ws);
        win.overlord_tasks = rows.as_array().unwrap().clone();
        let mut data = AppData::default();
        data.windows.push(win);
        data
    }

    fn legacy_row(id: &str, workspace_id: &str, origin: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "title": "  Wire   the parser. ",
            "workspace_id": workspace_id,
            "tab_id": "tab-1",
            "state": "active",
            "origin": origin,
            "created_at": "2026-08-01T00:00:00Z",
            "updated_at": "2026-08-02T00:00:00Z",
        })
    }

    #[test]
    fn moves_board_rows_onto_their_workspace_and_normalizes() {
        let mut data = window_with_legacy("ws-1", serde_json::json!([legacy_row("t1", "ws-1", "human")]));
        migrate_app_data(&mut data);
        let ws = &data.windows[0].workspaces[0];
        assert_eq!(ws.tasks.len(), 1);
        let t = &ws.tasks[0];
        assert_eq!(t.id, "t1");
        assert_eq!(t.status, "active", "the old `state` field becomes `status`");
        assert_eq!(t.tab_id.as_deref(), Some("tab-1"));
        assert_eq!(t.normalized_title, "wire the parser");
        assert_eq!(t.updated_at, "2026-08-02T00:00:00Z", "age must survive — staleness reads it");
        assert!(data.windows[0].overlord_tasks.is_empty(), "drained, so it cannot run twice");
    }

    #[test]
    fn relabels_legacy_agent_rows_as_imported() {
        // The old board's "agent" rows were mirrored from Claude's private store, which is
        // what "imported" means now. Left as "agent" the importer could never retire them
        // and they would age into permanent false stale cards.
        let mut data = window_with_legacy("ws-1", serde_json::json!([legacy_row("t1", "ws-1", "agent")]));
        migrate_app_data(&mut data);
        assert_eq!(data.windows[0].workspaces[0].tasks[0].origin, "imported");
    }

    #[test]
    fn drops_orphans_and_malformed_rows_rather_than_inventing_data() {
        let mut malformed = legacy_row("t2", "ws-1", "human");
        malformed.as_object_mut().unwrap().remove("updated_at");
        let mut data = window_with_legacy(
            "ws-1",
            serde_json::json!([
                legacy_row("t1", "ws-gone", "human"), // workspace no longer exists
                malformed,                            // no timestamp to age it by
                legacy_row("t3", "ws-1", "human"),
            ]),
        );
        migrate_app_data(&mut data);
        let tasks = &data.windows[0].workspaces[0].tasks;
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t3");
    }

    #[test]
    fn is_a_no_op_with_nothing_to_migrate() {
        let mut data = window_with_legacy("ws-1", serde_json::json!([]));
        migrate_app_data(&mut data);
        assert!(data.windows[0].workspaces[0].tasks.is_empty());
    }
}
