//! Move project (docs/relocate.md): the commands behind "Move project…" and the missing-folder
//! prompt. The rewrite itself is `crate::relocate`; this file is the ordering, which is the part
//! that matters:
//!
//!   1. every window suspends its live tabs in the folder — an agent still running would keep
//!      writing its transcript under the OLD project key while we move it;
//!   2. only when EVERY window confirmed, the folder moves (or, for a folder already moved, is
//!      repointed); a window that doesn't answer calls the whole thing off;
//!   3. each agent runtime's per-project state follows it;
//!   4. maiTerm's saved paths are rebased and saved;
//!   5. each window patches its mirror and wakes the tabs it suspended, which spawn in the new
//!      folder and run their auto-resume command.

use std::path::PathBuf;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::mailink::rpc;
use crate::relocate::{self, agents, detect};
use crate::state::{save_state, AppState};

/// Off the main thread: a stat per saved folder, plus a sibling listing per missing one.
#[tauri::command]
pub async fn find_missing_folders(state: State<'_, Arc<AppState>>) -> Result<Vec<detect::MissingRoot>, String> {
    let app = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let data = app.app_data.read().clone();
        detect::find_missing(&data)
    })
    .await
    .map_err(|e| format!("scan failed: {e}"))
}

/// Does a saved folder still exist? Asked before a tab spawns: a missing one would silently
/// fall back to home, and the tab's resume command must not run there.
#[tauri::command]
pub fn folder_exists(path: String) -> bool {
    relocate::expand_home(&path).is_dir()
}

/// The project a folder belongs to: its git checkout's top level, else the folder itself. What
/// "Move project…" offers to move when opened from a tab sitting somewhere inside it.
#[tauri::command]
pub async fn project_root_of(path: String) -> Result<String, String> {
    let dir = relocate::expand_home(&path);
    if !dir.is_dir() {
        return Err(format!("{} is not a folder.", dir.display()));
    }
    let out = tokio::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(&dir)
        .output()
        .await;
    Ok(match out {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => crate::share::canon(&dir).to_string_lossy().to_string(),
    })
}

/// The OS folder picker, opened in `start_in` (or the nearest folder above it that exists).
#[tauri::command]
pub async fn pick_folder(app: tauri::AppHandle, start_in: Option<String>) -> Result<Option<String>, String> {
    use tauri_plugin_dialog::DialogExt;
    let mut dialog = app.dialog().file();
    if let Some(mut p) = start_in.map(|s| relocate::expand_home(&s)) {
        while !p.is_dir() {
            if !p.pop() {
                break;
            }
        }
        if p.is_dir() {
            dialog = dialog.set_directory(p);
        }
    }
    Ok(dialog.blocking_pick_folder().map(|p| p.to_string()))
}

/// What a relocation would touch — shown before the human confirms. Reads only.
#[derive(Debug, Serialize)]
pub struct RelocatePreview {
    pub old: String,
    pub new: String,
    pub tabs: usize,
    pub services: usize,
    pub agents: agents::AgentPlan,
    /// The volume folds case — the frontend matches live tabs' folders the same way.
    pub fold: bool,
}

/// Async and off the main thread: it reads agent state from disk (every matching Claude
/// transcript's head and tail, Codex's thread db), and the dialog asks again on every pause in
/// typing — a sync command froze the window for each one (review of c96bec9).
#[tauri::command]
pub async fn preview_relocation(
    state: State<'_, Arc<AppState>>,
    old: String,
    new: String,
    move_folder: bool,
) -> Result<RelocatePreview, String> {
    let app = state.inner().clone();
    tokio::task::spawn_blocking(move || {
        let (old, new) = validate(&old, &new, move_folder)?;
        let mut copy = app.app_data.read().clone();
        let report = relocate::relocate_state(&mut copy, &old, &new);
        Ok(RelocatePreview {
            old: old.to_string_lossy().to_string(),
            new: new.to_string_lossy().to_string(),
            tabs: report.tabs.len(),
            services: report.services,
            agents: agents::plan(&old, &new),
            fold: relocate::case_insensitive(&old),
        })
    })
    .await
    .map_err(|e| format!("preview failed: {e}"))?
}

#[derive(Debug, Serialize)]
pub struct RelocateOutcome {
    pub moved: bool,
    pub state: relocate::StateReport,
    pub agents: agents::AgentReport,
    /// Windows that didn't confirm the mirror patch: their saved state is right, but the window
    /// should be reloaded before it writes an old path back.
    pub unconfirmed_windows: Vec<String>,
}

/// `old` and `new` checked for the mode and made absolute. Moving: `old` is a folder, `new`
/// does not exist and its parent does. Repointing (the folder was moved outside maiTerm): `old`
/// is gone and `new` is a folder.
fn validate(old: &str, new: &str, move_folder: bool) -> Result<(PathBuf, PathBuf), String> {
    let old_p = relocate::expand_home(old.trim().trim_end_matches('/'));
    let new_p = relocate::expand_home(new.trim().trim_end_matches('/'));
    if !old_p.is_absolute() || !new_p.is_absolute() {
        return Err("Both folders must be absolute paths.".into());
    }
    if old_p.parent().is_none() || new_p.parent().is_none() {
        return Err("A filesystem root can't be moved.".into());
    }
    if dirs::home_dir().is_some_and(|h| h.starts_with(&old_p)) {
        return Err("That's your home folder or above it — move a project inside it instead.".into());
    }
    if new_p.starts_with(&old_p) || old_p.starts_with(&new_p) {
        return Err("A folder can't be moved into itself or onto its own parent.".into());
    }
    if move_folder {
        if !old_p.is_dir() {
            return Err(format!("{} is not a folder.", old_p.display()));
        }
        if new_p.exists() {
            return Err(format!("{} already exists — pick a name that doesn't.", new_p.display()));
        }
        let parent = new_p.parent().unwrap();
        if !parent.is_dir() {
            return Err(format!("{} doesn't exist.", parent.display()));
        }
        let old_c = crate::share::canon(&old_p);
        let new_c = crate::share::canon(parent).join(new_p.file_name().unwrap());
        Ok((old_c, new_c))
    } else {
        if old_p.exists() {
            return Err(format!("{} still exists — use Move project… to move it.", old_p.display()));
        }
        if !new_p.is_dir() {
            return Err(format!("{} is not a folder.", new_p.display()));
        }
        Ok((old_p, crate::share::canon(&new_p)))
    }
}

fn window_labels(state: &AppState) -> Vec<String> {
    state.app_data.read().windows.iter().map(|w| w.label.clone()).collect()
}

/// How long a window gets to suspend its tabs: each one saves scrollback, kills a PTY and writes
/// state, one after another, and a throttled background webview runs slowly.
const SUSPEND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(90);
const ABORT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Call the move off in EVERY window, not only those that answered: a window that timed out
/// still has the suspend request queued, and its answer will go nowhere — so it is told the
/// session is over, and wakes whatever it suspended for it, now or when its suspend finishes.
async fn abort(app: &Arc<AppState>, handle: &tauri::AppHandle, session: &str) {
    for label in window_labels(app) {
        let _ = rpc::request_with_timeout(app, Some(handle), &label, "relocate.abort", json!({ "session": session }), ABORT_TIMEOUT).await;
    }
}

#[tauri::command]
pub async fn relocate_project(
    handle: tauri::AppHandle,
    state: State<'_, Arc<AppState>>,
    old: String,
    new: String,
    move_folder: bool,
) -> Result<RelocateOutcome, String> {
    let app = state.inner().clone();
    let (old, new) = validate(&old, &new, move_folder)?;
    let (old_s, new_s) = (old.to_string_lossy().to_string(), new.to_string_lossy().to_string());
    let fold = relocate::case_insensitive(&old);
    let session = uuid::Uuid::new_v4().to_string();
    log::info!("relocate: {} {old_s} → {new_s}", if move_folder { "moving" } else { "repointing" });

    // 1. Suspend: tabs whose shell is in the folder, and tabs that spawned in home because it
    //    was missing (the window remembers those — relocateFallback.ts). When repointing, also
    //    tabs in the NEW folder that run an agent: one started there before the folder was
    //    located resumed its session from the OLD project key and keeps writing there (live
    //    test, 2026-10-04). A plain shell in the new folder is left alone.
    let agent_roots: Vec<&str> = if move_folder { vec![] } else { vec![new_s.as_str()] };
    let mut suspended: Vec<(String, Vec<String>)> = vec![];
    for label in window_labels(&app) {
        let args = json!({ "roots": [old_s], "session": session, "fold": fold, "agent_roots": agent_roots });
        let out = rpc::request_with_timeout(&app, Some(&handle), &label, "relocate.suspend", args, SUSPEND_TIMEOUT).await;
        let ids = match out {
            rpc::Outcome::Answered(v) if v.get("error").is_none() => v["suspended"]
                .as_array()
                .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect::<Vec<_>>())
                .unwrap_or_default(),
            rpc::Outcome::NoWindow => continue, // closed meanwhile
            other => {
                let why = match other {
                    rpc::Outcome::Answered(v) => v["error"].as_str().unwrap_or("refused").to_string(),
                    rpc::Outcome::Timeout => "it didn't answer — its screen may be asleep".into(),
                    _ => "it dropped the request".into(),
                };
                abort(&app, &handle, &session).await;
                return Err(format!("Nothing was moved: a window couldn't stop its tabs in the folder ({why}). Any tab it stopped is being started again."));
            }
        };
        suspended.push((label, ids));
    }

    // 2. Move.
    if move_folder {
        if let Err(e) = std::fs::rename(&old, &new) {
            abort(&app, &handle, &session).await;
            let cross = e.raw_os_error() == Some(18); // EXDEV
            return Err(if cross {
                "Nothing was moved: that's a different disk, and maiTerm only renames within one. Move it yourself, then point maiTerm at the new folder.".into()
            } else {
                format!("Nothing was moved: {e}")
            });
        }
    }

    // 3. Agents. Failures here are reported, not fatal: the folder has already moved, and
    //    maiTerm's own paths must follow it regardless.
    let known = relocate::known_paths(&app.app_data.read(), &old);
    let mut agents = agents::apply(&old, &new, &known);

    // 4. maiTerm state. A failed save is reported, not returned: the folder has moved and the
    //    rebased state is live in memory (the next save writes it), and returning here would
    //    leave every suspended tab down and every mirror on the old paths.
    let (report, patches) = {
        let mut data = app.app_data.write();
        let report = relocate::relocate_state(&mut data, &old, &new);
        let patches = window_patches(&data, &report);
        let clone = data.clone();
        drop(data);
        if let Err(e) = save_state(&clone) {
            log::error!("relocate: saving state failed: {e}");
            agents.warnings.push(format!("maiTerm couldn't save its state ({e}) — it will retry on the next save; quit normally to be sure."));
        }
        (report, patches)
    };

    // 5. Mirrors + wake.
    let mut unconfirmed = vec![];
    for (label, mut patch) in patches_with_wake(patches, &suspended) {
        patch["session"] = json!(session);
        match rpc::request(&app, Some(&handle), &label, "relocate.apply", patch).await {
            rpc::Outcome::Answered(v) if v.get("error").is_none() => {}
            rpc::Outcome::NoWindow => {}
            _ => unconfirmed.push(label),
        }
    }
    log::info!(
        "relocate: done — {} tab(s), {} service(s), {} approval(s) carried; agents: {:?}",
        report.tabs.len(), report.services, report.approvals_carried, agents
    );
    Ok(RelocateOutcome { moved: move_folder, state: report, agents, unconfirmed_windows: unconfirmed })
}

/// Per window: the rebased tab records and stacks its mirror must take.
fn window_patches(data: &crate::state::workspace::AppData, report: &relocate::StateReport) -> Vec<(String, Value)> {
    let mut out = vec![];
    for w in &data.windows {
        let mut tabs = vec![];
        let mut stacks = vec![];
        for ws in &w.workspaces {
            for t in ws.panes.iter().flat_map(|p| p.tabs.iter()).chain(ws.archived_tabs.iter()) {
                if report.tabs.iter().any(|(l, id)| *l == w.label && *id == t.id) {
                    tabs.push(json!({ "workspace_id": ws.id, "tab": t }));
                }
            }
            if !ws.stack.is_empty() {
                stacks.push(json!({ "workspace_id": ws.id, "stack": ws.stack }));
            }
        }
        out.push((w.label.clone(), json!({ "tabs": tabs, "stacks": stacks, "wake": [] })));
    }
    out
}

fn patches_with_wake(patches: Vec<(String, Value)>, suspended: &[(String, Vec<String>)]) -> Vec<(String, Value)> {
    patches
        .into_iter()
        .map(|(label, mut p)| {
            if let Some((_, ids)) = suspended.iter().find(|(l, _)| *l == label) {
                p["wake"] = json!(ids);
            }
            (label, p)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn validation_refuses_the_dangerous_shapes() {
        let base = std::env::temp_dir().join(format!("maiterm-validate-{}", uuid::Uuid::new_v4()));
        let a = base.join("a");
        std::fs::create_dir_all(&a).unwrap();
        let s = |p: &Path| p.to_string_lossy().to_string();
        assert!(validate(&s(&a), &s(&a.join("inner")), true).is_err(), "into itself");
        assert!(validate(&s(&a), &s(&base), true).is_err(), "onto its parent");
        assert!(validate(&s(&a), &s(&a), true).is_err());
        assert!(validate(&s(&base.join("nope")), &s(&base.join("b")), true).is_err(), "nothing to move");
        assert!(validate(&s(&a), &s(&base.join("x/y")), true).is_err(), "no parent to move into");
        assert!(validate(&s(&a), &s(&base.join("b")), true).is_ok());
        // Repointing: the old one must really be gone.
        assert!(validate(&s(&a), &s(&a), false).is_err());
        assert!(validate(&s(&base.join("gone")), &s(&a), false).is_ok());
        assert!(validate("relative", &s(&a), false).is_err());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
