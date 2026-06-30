use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::workspace::{AppData, Layout, SplitDirection, SplitNode, TabType, WindowData};

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

fn get_conflict_path(timestamp_ms: u64) -> Option<PathBuf> {
    dirs::data_dir().map(|p| {
        p.join(app_data_slug())
            .join(format!("aiterm-state.conflict-{}.json", timestamp_ms))
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

fn get_temp_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(app_data_slug()).join("aiterm-state.tmp.json"))
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
    if data.tab_liveness_reconciled {
        return;
    }
    // 24h relative to the newest save. The window is relative, so an app left off
    // for weeks still resolves the last shutdown batch correctly.
    const RECONCILE_WINDOW_MINUTES: i64 = 24 * 60;
    // If the recency query fails, skip WITHOUT setting the flag so it retries next
    // boot — better than a destructive clean-slate on a transient error.
    let recent = match db.recent_tab_ids(RECONCILE_WINDOW_MINUTES) {
        Ok(set) => set,
        Err(e) => {
            log::warn!("Tab-liveness reconcile skipped (scrollback query failed): {}", e);
            return;
        }
    };
    // tab_id -> last save time, to stamp a meaningful "idle" age.
    let times: std::collections::HashMap<String, String> =
        db.tab_times().unwrap_or_default().into_iter().collect();

    let mut kept = 0usize;
    let mut suspended = 0usize;
    for win in &mut data.windows {
        for ws in &mut win.workspaces {
            for pane in &mut ws.panes {
                for tab in &mut pane.tabs {
                    if !matches!(tab.tab_type, TabType::Terminal) {
                        continue;
                    }
                    // Only the high-watermark: looks live (pty_id set, no suspend
                    // marker). Leave genuine suspended / never-spawned tabs alone.
                    if tab.pty_id.is_none() || tab.suspended_at.is_some() {
                        continue;
                    }
                    if recent.contains(&tab.id) {
                        kept += 1; // genuinely live at last shutdown — keep pty_id
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
