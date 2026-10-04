//! Finding saved folders that no longer exist (docs/relocate.md §4).
//!
//! A tab whose folder is gone used to spawn in home without a word (`pty::spawn_pty` falls back
//! when `is_dir()` fails). Here every saved local folder a spawn would use is checked, the
//! missing ones are grouped by the folder that actually went away — `~/IDE/aiTerm` for tabs in
//! `~/IDE/aiTerm/src` and `~/IDE/aiTerm/website` — and for each, likely new homes are suggested.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde::Serialize;

use super::expand_home;
use crate::state::workspace::{AppData, Tab};

/// One folder that went away, and what depended on it.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct MissingRoot {
    /// The highest missing folder: its parent still exists.
    pub root: String,
    /// Paths under `root` that saved state points at, relative ("" = the root itself).
    pub subpaths: Vec<String>,
    /// Tabs (live and archived) that would spawn in a missing folder.
    pub tab_ids: Vec<String>,
    /// Stack services whose cwd is missing.
    pub service_ids: Vec<String>,
    /// Folders that could be where it went, best first. Never the answer on its own: the human
    /// picks, because a wrong pick silently runs every agent in the wrong project.
    pub candidates: Vec<String>,
}

/// The folder a local tab would spawn in — the same choice `TerminalPane` makes (auto-resume
/// context wins over the restore context). `None` for SSH tabs: their folder is remote.
pub fn spawn_cwd(t: &Tab) -> Option<&str> {
    let ssh = |s: &Option<String>| s.as_deref().is_some_and(|s| !s.trim().is_empty());
    if ssh(&t.auto_resume_ssh_command) || ssh(&t.restore_ssh_command) {
        return None;
    }
    let auto = if t.auto_resume_enabled { t.auto_resume_cwd.as_deref() } else { None };
    auto.or(t.restore_cwd.as_deref()).filter(|s| !s.trim().is_empty())
}

/// The highest missing ancestor of a missing `path` (whose parent exists), or `None` when
/// `path` exists or nothing above it does either (a whole volume unmounted is not a move).
fn missing_root(path: &Path) -> Option<PathBuf> {
    if path.exists() {
        return None;
    }
    let mut cur = path;
    while let Some(parent) = cur.parent() {
        if parent.exists() {
            // `/Volumes/x` gone means a disk is unplugged, not a project moved.
            if parent == Path::new("/Volumes") || parent.parent().is_none() {
                return None;
            }
            return Some(cur.to_path_buf());
        }
        cur = parent;
    }
    None
}

/// Every missing folder saved state points at, grouped by what went away.
pub fn find_missing(data: &AppData) -> Vec<MissingRoot> {
    let mut groups: BTreeMap<PathBuf, MissingRoot> = BTreeMap::new();
    let mut add = |path: &str, tab: Option<&str>, service: Option<&str>| {
        let abs = expand_home(path);
        if !abs.is_absolute() {
            return;
        }
        let Some(root) = missing_root(&abs) else { return };
        let g = groups.entry(root.clone()).or_insert_with(|| MissingRoot {
            root: root.to_string_lossy().to_string(),
            subpaths: vec![],
            tab_ids: vec![],
            service_ids: vec![],
            candidates: vec![],
        });
        let sub = abs.strip_prefix(&root).map(|r| r.to_string_lossy().to_string()).unwrap_or_default();
        if !g.subpaths.contains(&sub) {
            g.subpaths.push(sub);
        }
        if let Some(id) = tab {
            if !g.tab_ids.iter().any(|t| t == id) {
                g.tab_ids.push(id.to_string());
            }
        }
        if let Some(id) = service {
            g.service_ids.push(id.to_string());
        }
    };
    for w in &data.windows {
        for ws in &w.workspaces {
            for t in ws.panes.iter().flat_map(|p| p.tabs.iter()).chain(ws.archived_tabs.iter()) {
                if t.service_id.is_some() {
                    continue; // its service's cwd is the one that counts, below
                }
                if let Some(cwd) = spawn_cwd(t) {
                    add(cwd, Some(&t.id), None);
                }
            }
            for s in &ws.stack {
                add(&s.cwd, None, Some(&s.id));
            }
        }
    }
    groups
        .into_values()
        .map(|mut g| {
            g.candidates = candidates(Path::new(&g.root), &g.subpaths, data)
                .into_iter()
                .map(|p| p.to_string_lossy().to_string())
                .collect();
            g
        })
        .collect()
}

/// Folders beside the missing one that could be it: they hold every subfolder saved state
/// needed (`src-tauri`, `website`…), and no saved state already points into them. A rename in
/// place is the common move, so siblings are all that is searched; anything else is the picker's
/// job. Git repos first, then most recently changed (a rename stamps the folder's ctime).
fn candidates(root: &Path, subpaths: &[String], data: &AppData) -> Vec<PathBuf> {
    let Some(parent) = root.parent() else { return vec![] };
    let Ok(entries) = std::fs::read_dir(parent) else { return vec![] };
    let used = used_folders(data);
    let mut found: Vec<(bool, SystemTime, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .map(|e| e.path())
        .filter(|p| subpaths.iter().all(|s| s.is_empty() || p.join(s).is_dir()))
        .filter(|p| !used.iter().any(|u| u.starts_with(p)))
        .map(|p| {
            let git = p.join(".git").exists();
            (git, renamed_at(&p), p)
        })
        .collect();
    // Without any subfolder to require, every sibling qualifies: keep only git repos then.
    if subpaths.iter().all(|s| s.is_empty()) {
        found.retain(|(git, _, _)| *git);
    }
    // Most recently renamed first — the moved folder, in the common case — then git repos.
    found.sort_by(|a, b| b.1.cmp(&a.1).then(b.0.cmp(&a.0)));
    found.into_iter().take(3).map(|(_, _, p)| p).collect()
}

/// When the folder's own entry last changed. A rename stamps it (the inode's ctime); its
/// modification time is about its CONTENTS and a rename leaves it alone, so sorting by that
/// ranked the moved folder below any sibling someone had saved a file in (live test).
fn renamed_at(p: &Path) -> SystemTime {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let Ok(m) = std::fs::metadata(p) {
            return SystemTime::UNIX_EPOCH + std::time::Duration::new(m.ctime().max(0) as u64, m.ctime_nsec().max(0) as u32);
        }
    }
    std::fs::metadata(p).and_then(|m| m.modified()).unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Every existing local folder saved state already points at — a sibling one of them lives in
/// is another project, not the moved one.
fn used_folders(data: &AppData) -> Vec<PathBuf> {
    let mut out = vec![];
    for w in &data.windows {
        for ws in &w.workspaces {
            for t in ws.panes.iter().flat_map(|p| p.tabs.iter()).chain(ws.archived_tabs.iter()) {
                if let Some(c) = spawn_cwd(t) {
                    let p = expand_home(c);
                    if p.exists() {
                        out.push(p);
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::{Tab, WindowData, Workspace};

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("maiterm-relocate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn data(cwds: &[&Path]) -> AppData {
        let mut ws = Workspace::new("w".into());
        ws.panes[0].tabs = cwds
            .iter()
            .enumerate()
            .map(|(i, c)| {
                let mut t = Tab::new(format!("t{i}"));
                t.id = format!("t{i}");
                t.restore_cwd = Some(c.to_string_lossy().to_string());
                t
            })
            .collect();
        let mut w = WindowData::new("main".into());
        w.workspaces.push(ws);
        let mut d = AppData::default();
        d.windows.push(w);
        d
    }

    #[test]
    fn groups_by_the_folder_that_went_away_and_suggests_the_rename() {
        let base = scratch();
        let old = base.join("aiTerm");
        let new = base.join("maiterm");
        std::fs::create_dir_all(new.join("src")).unwrap();
        std::fs::create_dir_all(new.join("website")).unwrap();
        std::fs::create_dir_all(new.join(".git")).unwrap();
        // A sibling without the subfolders, and one another tab already lives in.
        std::fs::create_dir_all(base.join("unrelated")).unwrap();
        let other = base.join("other");
        std::fs::create_dir_all(other.join("src")).unwrap();
        std::fs::create_dir_all(other.join("website")).unwrap();

        let d = data(&[&old.join("src"), &old.join("website"), &old, &other]);
        let m = find_missing(&d);
        assert_eq!(m.len(), 1);
        assert_eq!(Path::new(&m[0].root), old);
        assert_eq!(m[0].tab_ids, vec!["t0", "t1", "t2"]);
        assert_eq!(m[0].candidates, vec![new.to_string_lossy().to_string()]);
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn present_folders_and_ssh_tabs_are_not_missing() {
        let base = scratch();
        let mut d = data(&[&base, &base.join("gone")]);
        d.windows[0].workspaces[0].panes[0].tabs[1].restore_ssh_command = Some("ssh host".into());
        assert!(find_missing(&d).is_empty());
        std::fs::remove_dir_all(&base).unwrap();
    }
}
