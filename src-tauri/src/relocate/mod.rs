//! Move project (docs/relocate.md): a local project folder moved from one absolute path to
//! another, and every saved path under it carried along — tabs (live and archived), stack
//! services, editor/diff tabs, watch-script follow-ups and their approvals — so a restart, a
//! resume or a split lands in the new folder instead of falling back to home.
//!
//! The rewrite is a PREFIX rebase at path-component boundaries: `/a/aiTerm` rebases
//! `/a/aiTerm/src` but never `/a/aiTermX`. A value keeps its form — a `~/…` value stays `~/…`
//! when the new folder is under home too.
//!
//! Remote paths are never touched: a `~` on an SSH tab is the REMOTE home, and `last_cwd` on one
//! can be either side (share/mod.rs `tab_place`), so an SSH tab's cwd fields are left alone.

pub mod agents;
pub mod detect;

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::state::workspace::{AppData, Tab};

/// Expand a leading `~` against home. A relative or empty value comes back unchanged.
pub fn expand_home(value: &str) -> PathBuf {
    let home = dirs::home_dir();
    match (value, home) {
        ("~", Some(h)) => h,
        (v, Some(h)) if v.starts_with("~/") => h.join(&v[2..]),
        (v, _) => PathBuf::from(v),
    }
}

/// Does the volume `p` lives on fold case? Asked of the disk, not the platform: macOS volumes
/// are case-insensitive by default but can be formatted case-sensitive, and there `app` and `App`
/// are two projects. Probed at the nearest existing ancestor whose name has a cased letter —
/// its case-flipped twin resolving to the same file means the volume folds. Nothing testable
/// (a path on no mounted volume): the platform default. Cached per path.
pub fn case_insensitive(p: &Path) -> bool {
    use std::collections::HashMap;
    use std::sync::{Mutex, OnceLock};
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, bool>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(v) = cache.lock().ok().and_then(|c| c.get(p).copied()) {
        return v;
    }
    let v = probe_case_insensitive(p);
    if let Ok(mut c) = cache.lock() {
        c.insert(p.to_path_buf(), v);
    }
    v
}

/// Probed INSIDE the nearest existing folder, on an entry of its own volume: flipping that
/// folder's own name would ask its PARENT's volume — at a mount point (`/Volumes/Data`) that is
/// the boot volume, which folds, and the answer would be wrong for a case-sensitive disk below.
fn probe_case_insensitive(p: &Path) -> bool {
    let fallback = cfg!(target_os = "macos") || cfg!(windows);
    let flip = |name: &str| -> String {
        name.chars().map(|ch| if ch.is_lowercase() { ch.to_ascii_uppercase() } else { ch.to_ascii_lowercase() }).collect()
    };
    // Up from the nearest existing folder, staying on its volume: an empty folder has nothing to
    // flip, so its parent is asked — but never past the mount point.
    let mut dirs = p.ancestors().skip_while(|a| !a.is_dir()).peekable();
    while let Some(dir) = dirs.next() {
        for e in std::fs::read_dir(dir).into_iter().flatten().flatten().take(200) {
            let Some(name) = e.file_name().to_str().map(String::from) else { continue };
            let flipped = flip(&name);
            if flipped == name || !same_device(dir, &e.path()) {
                continue; // nothing to flip, or a mount point of another volume
            }
            return same_file(&e.path(), &dir.join(&flipped));
        }
        match dirs.peek() {
            Some(parent) if same_device(dir, parent) => {}
            _ => break,
        }
    }
    fallback
}

#[cfg(unix)]
fn same_device(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_device(_: &Path, _: &Path) -> bool {
    true
}

#[cfg(unix)]
fn same_file(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (std::fs::metadata(a), std::fs::metadata(b)) {
        (Ok(x), Ok(y)) => x.dev() == y.dev() && x.ino() == y.ino(),
        _ => false,
    }
}

#[cfg(not(unix))]
fn same_file(a: &Path, b: &Path) -> bool {
    b.exists() && std::fs::canonicalize(a).ok() == std::fs::canonicalize(b).ok()
}

/// Component-wise prefix match. A path typed at a prompt (OSC 7 reports `$PWD`) keeps the case
/// it was typed in, so on a volume that folds case the match folds too; elsewhere it is exact.
fn strip_root<'a>(path: &'a Path, root: &Path) -> Option<PathBuf> {
    let fold = case_insensitive(root);
    let mut p = path.components();
    for rc in root.components() {
        let pc = p.next()?;
        let (a, b) = (pc.as_os_str().to_string_lossy(), rc.as_os_str().to_string_lossy());
        let same = if fold { a.to_lowercase() == b.to_lowercase() } else { a == b };
        if !same {
            return None;
        }
    }
    Some(p.as_path().to_path_buf())
}

/// `value` rebased from `old` onto `new`, or `None` when it isn't under `old`. Both roots are
/// absolute. A `~/…` value stays home-relative when the result is under home.
pub fn rebase(value: &str, old: &Path, new: &Path) -> Option<String> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let tilde = value == "~" || value.starts_with("~/");
    let abs = expand_home(value);
    if !abs.is_absolute() {
        return None;
    }
    let rest = strip_root(&abs, old)?;
    let out = if rest.as_os_str().is_empty() { new.to_path_buf() } else { new.join(rest) };
    if tilde {
        if let Some(h) = dirs::home_dir() {
            if let Ok(r) = out.strip_prefix(&h) {
                return Some(if r.as_os_str().is_empty() {
                    "~".to_string()
                } else {
                    format!("~/{}", r.to_string_lossy())
                });
            }
        }
    }
    Some(out.to_string_lossy().to_string())
}

/// `field` rebased in place. True when it changed.
fn rebase_field(field: &mut Option<String>, old: &Path, new: &Path) -> bool {
    match field.as_deref().and_then(|v| rebase(v, old, new)) {
        Some(n) if field.as_deref() != Some(n.as_str()) => {
            *field = Some(n);
            true
        }
        _ => false,
    }
}

/// Free-text commands (`auto_resume_command`): every occurrence of the old root that sits on a
/// component boundary — followed by `/`, a quote, whitespace or the end. Exact case only: this is
/// shell text, and folding case here could rewrite a word that merely looks like the path.
fn rebase_text(text: &str, old: &Path, new: &Path) -> Option<String> {
    let old_s = old.to_string_lossy();
    let new_s = new.to_string_lossy();
    if old_s.len() < 2 || !text.contains(old_s.as_ref()) {
        return None;
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    let mut changed = false;
    while let Some(i) = rest.find(old_s.as_ref()) {
        let after = &rest[i + old_s.len()..];
        let boundary = after.chars().next().map_or(true, |c| c == '/' || c == '\'' || c == '"' || c.is_whitespace() || c == ';' || c == '&' || c == '|' || c == ')');
        out.push_str(&rest[..i]);
        if boundary {
            out.push_str(&new_s);
            changed = true;
        } else {
            out.push_str(&old_s);
        }
        rest = after;
    }
    out.push_str(rest);
    changed.then_some(out)
}

fn is_ssh_tab(t: &Tab) -> bool {
    t.auto_resume_ssh_command.as_deref().is_some_and(|s| !s.trim().is_empty())
        || t.restore_ssh_command.as_deref().is_some_and(|s| !s.trim().is_empty())
}

/// A follow-up's watch script that moved, so its approval can follow it (see `relocate_state`).
struct MovedScript {
    old_cwd: String,
    new_cwd: String,
    script: String,
}

/// Rebase one tab. Returns whether anything changed, and records moved watch scripts.
fn relocate_tab(t: &mut Tab, old: &Path, new: &Path, scripts: &mut Vec<MovedScript>) -> bool {
    let mut changed = false;
    if !is_ssh_tab(t) {
        changed |= rebase_field(&mut t.restore_cwd, old, new);
        changed |= rebase_field(&mut t.auto_resume_cwd, old, new);
        changed |= rebase_field(&mut t.last_cwd, old, new);
        for cmd in [&mut t.auto_resume_command, &mut t.auto_resume_remembered_command] {
            if let Some(n) = cmd.as_deref().and_then(|c| rebase_text(c, old, new)) {
                *cmd = Some(n);
                changed = true;
            }
        }
    }
    if let Some(ef) = t.editor_file.as_mut() {
        if !ef.is_remote {
            if let Some(n) = rebase(&ef.file_path, old, new) {
                changed |= n != ef.file_path;
                ef.file_path = n;
            }
        }
    }
    if let Some(dc) = t.diff_context.as_mut() {
        if let Some(n) = rebase(&dc.file_path, old, new) {
            changed |= n != dc.file_path;
            dc.file_path = n;
        }
    }
    for f in t.follow_ups.iter_mut() {
        let before = f.due.cwd.clone();
        if rebase_field(&mut f.due.cwd, old, new) {
            changed = true;
            if let (Some(o), Some(n), Some(s)) = (before, f.due.cwd.clone(), f.due.script.clone()) {
                scripts.push(MovedScript { old_cwd: o, new_cwd: n, script: s });
            }
        }
    }
    changed
}

/// What a relocation changed, per window — the frontend of each listed window re-reads its state.
#[derive(Debug, Default, Clone, Serialize)]
pub struct StateReport {
    /// (window label, tab id) of every tab whose saved paths moved, archived tabs included.
    pub tabs: Vec<(String, String)>,
    pub services: usize,
    pub approvals_carried: usize,
    pub backup_directory: bool,
}

/// Rebase every saved local path in `data` from `old` to `new`. Pure: touches no disk.
///
/// A watch script's approval is keyed by script AND folder (`watch::script_hash`) on purpose —
/// the same script elsewhere means something else. A moved folder is the SAME folder, so an
/// approval held for a script in it is carried to the new path; only scripts actually held by a
/// follow-up are carried (the digest is one-way, so nothing else could be).
pub fn relocate_state(data: &mut AppData, old: &Path, new: &Path) -> StateReport {
    let mut report = StateReport::default();
    let mut scripts = Vec::new();
    for w in data.windows.iter_mut() {
        for ws in w.workspaces.iter_mut() {
            for t in ws.panes.iter_mut().flat_map(|p| p.tabs.iter_mut()).chain(ws.archived_tabs.iter_mut()) {
                if relocate_tab(t, old, new, &mut scripts) {
                    report.tabs.push((w.label.clone(), t.id.clone()));
                }
            }
            for s in ws.stack.iter_mut() {
                if let Some(n) = rebase(&s.cwd, old, new) {
                    if n != s.cwd {
                        s.cwd = n;
                        report.services += 1;
                    }
                }
            }
        }
    }
    report.backup_directory = rebase_field(&mut data.preferences.backup_directory, old, new);
    for m in scripts {
        if crate::watch::is_approved(data, &m.old_cwd, &m.script) && !crate::watch::is_approved(data, &m.new_cwd, &m.script) {
            crate::watch::remember_approval(data, &m.new_cwd, &m.script);
            report.approvals_carried += 1;
        }
    }
    report
}

/// Every local folder under `old` that saved state points at — the folders agents were started
/// in, as far as maiTerm knows.
pub fn known_paths(data: &AppData, old: &Path) -> Vec<String> {
    let mut out = vec![];
    for w in &data.windows {
        for ws in &w.workspaces {
            for t in ws.panes.iter().flat_map(|p| p.tabs.iter()).chain(ws.archived_tabs.iter()) {
                if is_ssh_tab(t) {
                    continue;
                }
                for c in [&t.restore_cwd, &t.auto_resume_cwd, &t.last_cwd].into_iter().flatten() {
                    if rebase(c, old, old).is_some() && !out.contains(c) {
                        out.push(c.clone());
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

    fn p(s: &str) -> PathBuf {
        PathBuf::from(s)
    }

    #[test]
    fn rebases_at_component_boundaries_only() {
        let (o, n) = (p("/u/IDE/aiTerm"), p("/u/IDE/maiterm"));
        assert_eq!(rebase("/u/IDE/aiTerm", &o, &n).as_deref(), Some("/u/IDE/maiterm"));
        assert_eq!(rebase("/u/IDE/aiTerm/src-tauri", &o, &n).as_deref(), Some("/u/IDE/maiterm/src-tauri"));
        assert_eq!(rebase("/u/IDE/aiTerm/", &o, &n).as_deref(), Some("/u/IDE/maiterm"));
        assert_eq!(rebase("/u/IDE/aiTermX", &o, &n), None);
        assert_eq!(rebase("/u/IDE", &o, &n), None);
        assert_eq!(rebase("relative/aiTerm", &o, &n), None);
        assert_eq!(rebase("", &o, &n), None);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_volume_decides_case_folding() {
        // The temp dir sits on the default (case-insensitive) APFS volume; probed through an
        // existing ancestor even when the path itself is gone.
        let d = std::env::temp_dir().join(format!("maiterm-Case-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&d).unwrap();
        assert!(case_insensitive(&d));
        assert!(case_insensitive(&d.join("Gone/Deeper")));
        std::fs::remove_dir_all(&d).unwrap();
    }

    /// Needs a case-sensitive volume: `MAITERM_CS_MOUNT=<mount point> cargo test -- --ignored
    /// case_sensitive_mount`. Probes a gone path under the mount, an empty folder on it, and the
    /// mount's own child — the cases where flipping the MOUNT's name asked the boot volume.
    #[test]
    #[ignore]
    fn case_sensitive_mount() {
        let m = PathBuf::from(std::env::var("MAITERM_CS_MOUNT").expect("MAITERM_CS_MOUNT"));
        std::fs::create_dir_all(m.join("2024/app")).unwrap();
        std::fs::create_dir_all(m.join("empty")).unwrap();
        assert!(!probe_case_insensitive(&m.join("2024/gone/proj")));
        assert!(!probe_case_insensitive(&m.join("2024")));
        assert!(!probe_case_insensitive(&m.join("empty")));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn folds_case_on_macos() {
        let (o, n) = (p("/u/IDE/aiTerm"), p("/u/IDE/maiterm"));
        assert_eq!(rebase("/u/ide/AITERM/src", &o, &n).as_deref(), Some("/u/IDE/maiterm/src"));
    }

    #[test]
    fn keeps_the_home_relative_form() {
        let h = dirs::home_dir().unwrap();
        let (o, n) = (h.join("DATA/IDE/aiTerm"), h.join("DATA/IDE/maiterm"));
        assert_eq!(rebase("~/DATA/IDE/aiTerm/docs", &o, &n).as_deref(), Some("~/DATA/IDE/maiterm/docs"));
        // Absolute stays absolute.
        let abs = h.join("DATA/IDE/aiTerm").to_string_lossy().to_string();
        assert_eq!(rebase(&abs, &o, &n), Some(h.join("DATA/IDE/maiterm").to_string_lossy().to_string()));
        // Moved out of home: the `~` form can't hold it any more.
        assert_eq!(rebase("~/DATA/IDE/aiTerm", &o, &p("/Volumes/x/maiterm")).as_deref(), Some("/Volumes/x/maiterm"));
    }

    #[test]
    fn rewrites_free_text_on_boundaries() {
        let (o, n) = (p("/u/aiTerm"), p("/u/maiterm"));
        assert_eq!(rebase_text("cd '/u/aiTerm/src' && claude", &o, &n).as_deref(), Some("cd '/u/maiterm/src' && claude"));
        assert_eq!(rebase_text("cd /u/aiTerm; x", &o, &n).as_deref(), Some("cd /u/maiterm; x"));
        assert_eq!(rebase_text("cd /u/aiTermX", &o, &n), None);
        assert_eq!(rebase_text("claude --resume abc", &o, &n), None);
    }

    fn tab(id: &str) -> Tab {
        let mut t = Tab::new(id.into());
        t.id = id.into();
        t
    }

    fn data_with(tabs: Vec<Tab>, archived: Vec<Tab>) -> AppData {
        let mut ws = crate::state::workspace::Workspace::new("ws".into());
        ws.panes[0].tabs = tabs;
        ws.archived_tabs = archived;
        let mut w = crate::state::workspace::WindowData::new("main".into());
        w.workspaces.push(ws);
        let mut d = AppData::default();
        d.windows.push(w);
        d
    }

    #[test]
    fn relocates_local_tabs_and_leaves_ssh_tabs_alone() {
        let (o, n) = (p("/u/aiTerm"), p("/u/maiterm"));
        let mut local = tab("local");
        local.restore_cwd = Some("/u/aiTerm".into());
        local.auto_resume_cwd = Some("/u/aiTerm/src".into());
        local.last_cwd = Some("/u/aiTerm/src".into());
        let mut ssh = tab("ssh");
        ssh.restore_ssh_command = Some("ssh host".into());
        ssh.restore_cwd = Some("/u/aiTerm".into());
        let mut archived = tab("arch");
        archived.restore_cwd = Some("/u/aiTerm/website".into());
        let mut other = tab("other");
        other.restore_cwd = Some("/u/elsewhere".into());

        let mut d = data_with(vec![local, ssh, other], vec![archived]);
        let r = relocate_state(&mut d, &o, &n);
        let ws = &d.windows[0].workspaces[0];
        let t = &ws.panes[0].tabs;
        assert_eq!(t[0].restore_cwd.as_deref(), Some("/u/maiterm"));
        assert_eq!(t[0].auto_resume_cwd.as_deref(), Some("/u/maiterm/src"));
        assert_eq!(t[0].last_cwd.as_deref(), Some("/u/maiterm/src"));
        assert_eq!(t[1].restore_cwd.as_deref(), Some("/u/aiTerm"), "an SSH tab's cwd is not ours to move");
        assert_eq!(t[2].restore_cwd.as_deref(), Some("/u/elsewhere"));
        assert_eq!(ws.archived_tabs[0].restore_cwd.as_deref(), Some("/u/maiterm/website"));
        let ids: Vec<&str> = r.tabs.iter().map(|(_, id)| id.as_str()).collect();
        assert_eq!(ids, vec!["local", "arch"]);
    }

    #[test]
    fn carries_a_held_scripts_approval_and_nothing_else() {
        let (o, n) = (p("/u/aiTerm"), p("/u/maiterm"));
        let mut t = tab("t");
        t.follow_ups.push(crate::state::workspace::FollowUp {
            id: "f".into(),
            text: "x".into(),
            due: crate::state::workspace::FollowUpDue {
                kind: "script".into(),
                script: Some("test -f done".into()),
                cwd: Some("/u/aiTerm".into()),
                ..Default::default()
            },
            author: "agent".into(),
            created_at: "2026-10-04T00:00:00Z".into(),
            expires_at: None,
        });
        let mut d = data_with(vec![t], vec![]);
        crate::watch::remember_approval(&mut d, "/u/aiTerm", "test -f done");
        let r = relocate_state(&mut d, &o, &n);
        assert_eq!(r.approvals_carried, 1);
        assert!(crate::watch::is_approved(&d, "/u/maiterm", "test -f done"));
        assert_eq!(d.windows[0].workspaces[0].panes[0].tabs[0].follow_ups[0].due.cwd.as_deref(), Some("/u/maiterm"));
    }
}
