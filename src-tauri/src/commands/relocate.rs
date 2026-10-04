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

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use serde_json::{json, Value};
use tauri::State;

use crate::mailink::rpc;
use crate::relocate::{self, agents, detect};
use crate::state::{save_state, AppState};

#[tauri::command]
pub fn find_missing_folders(state: State<'_, Arc<AppState>>) -> Vec<detect::MissingRoot> {
    detect::find_missing(&state.app_data.read())
}

/// What a relocation would touch — shown before the human confirms. Reads only.
#[derive(Debug, Serialize)]
pub struct RelocatePreview {
    pub old: String,
    pub new: String,
    pub tabs: usize,
    pub services: usize,
    pub agents: agents::AgentPlan,
}

#[tauri::command]
pub fn preview_relocation(
    state: State<'_, Arc<AppState>>,
    old: String,
    new: String,
    move_folder: bool,
) -> Result<RelocatePreview, String> {
    let (old, new) = validate(&old, &new, move_folder)?;
    let mut copy = state.app_data.read().clone();
    let report = relocate::relocate_state(&mut copy, &old, &new);
    Ok(RelocatePreview {
        old: old.to_string_lossy().to_string(),
        new: new.to_string_lossy().to_string(),
        tabs: report.tabs.len(),
        services: report.services,
        agents: agents::plan(&old, &new),
    })
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

/// Ask every window to resume the tabs it suspended — the undo for a move called off.
async fn wake(app: &Arc<AppState>, handle: &tauri::AppHandle, suspended: &[(String, Vec<String>)]) {
    for (label, ids) in suspended {
        if ids.is_empty() {
            continue;
        }
        let _ = rpc::request(app, Some(handle), label, "relocate.apply", json!({ "tabs": [], "stacks": [], "wake": ids })).await;
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
    log::info!("relocate: {} {old_s} → {new_s}", if move_folder { "moving" } else { "repointing" });

    // 1. Suspend. Both roots: a folder moved under a running shell is still that shell's cwd,
    //    now reported at the new path.
    let mut suspended: Vec<(String, Vec<String>)> = vec![];
    for label in window_labels(&app) {
        let out = rpc::request(&app, Some(&handle), &label, "relocate.suspend", json!({ "roots": [old_s, new_s] })).await;
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
                wake(&app, &handle, &suspended).await;
                return Err(format!("Nothing was moved: a window couldn't stop its tabs in the folder ({why})."));
            }
        };
        suspended.push((label, ids));
    }

    // 2. Move.
    if move_folder {
        if let Err(e) = std::fs::rename(&old, &new) {
            wake(&app, &handle, &suspended).await;
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
    let agents = agents::apply(&old, &new, &known);

    // 4. maiTerm state.
    let (report, patches) = {
        let mut data = app.app_data.write();
        let report = relocate::relocate_state(&mut data, &old, &new);
        let patches = window_patches(&data, &report);
        let clone = data.clone();
        drop(data);
        save_state(&clone)?;
        (report, patches)
    };

    // 5. Mirrors + wake.
    let mut unconfirmed = vec![];
    for (label, patch) in patches_with_wake(patches, &suspended) {
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
