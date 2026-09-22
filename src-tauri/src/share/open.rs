//! A `.maiterm-workspace` opened from the OS (docs/workspace-share.md §7).
//!
//! Three delivery routes, one queue. macOS hands the running process `RunEvent::Opened`;
//! Windows and Linux launch a second process with the path in argv, which the
//! single-instance plugin forwards here; and a cold launch on those two reads its own argv.
//! A cold launch also delivers before any webview is listening, so every route QUEUES the
//! path and then rings; the frontend drains the queue on mount and on each ring. A ring
//! nobody heard costs nothing, because the path is still queued.

use std::path::PathBuf;

use parking_lot::Mutex;
use tauri::{Emitter, Manager};

static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

/// The ring. Payload-free: the frontend always reads the queue itself.
pub const EVENT: &str = "share-file-opened";

pub fn is_share_path(p: &std::path::Path) -> bool {
    p.extension().is_some_and(|e| e.eq_ignore_ascii_case(super::EXTENSION))
}

/// Share files named on a command line. `cwd` resolves relative ones: a forwarded argv
/// comes from the second process, whose working directory is not ours.
#[cfg_attr(target_os = "macos", allow(dead_code))] // macOS delivers files as RunEvent::Opened
pub fn paths_from_args(args: &[String], cwd: Option<&std::path::Path>) -> Vec<PathBuf> {
    args.iter()
        .skip(1)
        .map(PathBuf::from)
        .filter(|p| is_share_path(p))
        .map(|p| match (p.is_relative(), cwd) {
            (true, Some(c)) => c.join(p),
            _ => p,
        })
        .collect()
}

pub fn deliver(app: &tauri::AppHandle, paths: Vec<PathBuf>) {
    if paths.is_empty() {
        return;
    }
    log::info!("share: {} workspace file(s) opened from the OS", paths.len());
    {
        let mut q = PENDING.lock();
        for p in paths {
            if !q.contains(&p) {
                q.push(p);
            }
        }
    }
    // Bring maiTerm forward — the user just double-clicked something of ours.
    let windows: Vec<_> = app
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label != "preferences" && label != "help")
        .collect();
    let target = windows
        .iter()
        .find(|(_, w)| w.is_focused().unwrap_or(false))
        .or_else(|| windows.iter().find(|(l, _)| l == "main"))
        .or_else(|| windows.first());
    if let Some((label, win)) = target {
        let _ = win.unminimize();
        let _ = win.set_focus();
        // `emit_to` + a label-targeted listen, never `win.emit` (which broadcasts).
        let _ = app.emit_to(label.as_str(), EVENT, ());
    }
}

/// Drain the queue. Whoever calls first gets the files; the rest get nothing, so two windows
/// mounting at once never open two wizards for one file.
#[tauri::command]
pub fn take_pending_share_opens() -> Vec<String> {
    std::mem::take(&mut *PENDING.lock())
        .into_iter()
        .map(|p| p.to_string_lossy().to_string())
        .collect()
}

/// Dev builds only: act as though the OS had opened `path`. Dev builds are never bundled, so
/// they never register the extension — this drives everything downstream of the OS (queue,
/// ring, drain, wizard) for testing.
#[cfg(debug_assertions)]
#[tauri::command]
pub fn share_debug_open(app: tauri::AppHandle, path: String) {
    deliver(&app, vec![PathBuf::from(path)]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_picks_out_share_files_only() {
        let args = vec![
            "/Applications/maiTerm".to_string(),
            "--flag".to_string(),
            "proj.maiterm-workspace".to_string(),
            "/abs/other.MAITERM-WORKSPACE".to_string(),
            "notes.txt".to_string(),
        ];
        let got = paths_from_args(&args, Some(std::path::Path::new("/home/me")));
        assert_eq!(got, vec![PathBuf::from("/home/me/proj.maiterm-workspace"), PathBuf::from("/abs/other.MAITERM-WORKSPACE")]);
    }
}
