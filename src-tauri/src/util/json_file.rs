//! Editing a config file other processes keep writing (`.claude.json`, `history.jsonl`): live
//! agents append and rewrite these while maiTerm edits them, so every edit is change-detecting.
//! Shared by Move project (relocate/agents.rs) and the Hive's trust writes (hive/trust.rs).

use std::fs;
use std::path::Path;

use serde_json::Value;

fn stamp(p: &Path) -> Option<(u64, std::time::SystemTime)> {
    let m = fs::metadata(p).ok()?;
    Some((m.len(), m.modified().ok()?))
}

/// Read, edit, write beside, rename over. `edit` returns `None` for "nothing to change".
///
/// - Through a symlink to the file it names: a config kept in a dotfiles repo stays a link.
/// - Write-then-rename: a crash mid-write must never leave a runtime's config truncated — a
///   broken `.claude.json` loses every account setting, not just this project's.
/// - Redone when the file changed while it was being edited: agents in other projects keep
///   running and appending, and a rename over their write would drop it.
pub fn update_file(p: &Path, mut edit: impl FnMut(&[u8]) -> Option<Vec<u8>>) -> std::io::Result<bool> {
    let target = match fs::canonicalize(p) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };
    for _ in 0..5 {
        let before = stamp(&target);
        let bytes = fs::read(&target)?;
        let Some(out) = edit(&bytes) else { return Ok(false) };
        let tmp = target.with_extension(format!("maiterm-relocate-{}", std::process::id()));
        fs::write(&tmp, &out)?;
        if let Ok(m) = fs::metadata(&target) {
            let _ = fs::set_permissions(&tmp, m.permissions());
        }
        if stamp(&target) != before {
            let _ = fs::remove_file(&tmp);
            continue;
        }
        return fs::rename(&tmp, &target).map(|_| true).inspect_err(|_| {
            let _ = fs::remove_file(&tmp);
        });
    }
    Err(std::io::Error::other("it kept changing while being updated"))
}

/// `update_file` for a JSON document; `edit` returns whether it changed anything.
pub fn update_json(p: &Path, mut edit: impl FnMut(&mut Value) -> bool) -> std::io::Result<bool> {
    update_file(p, |b| {
        let mut v: Value = serde_json::from_slice(b).ok()?;
        if !edit(&mut v) {
            return None;
        }
        serde_json::to_vec_pretty(&v).ok()
    })
}
