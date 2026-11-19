//! Editing a config file other processes keep writing (`.claude.json`, `history.jsonl`): live
//! agents append and rewrite these while maiTerm edits them, so every edit is change-detecting.
//! Shared by Move project (relocate/agents.rs), the Hive's trust writes (hive/trust.rs) and the
//! account roots' merge (accounts/mod.rs).
//!
//! maiTerm's own writers are serialized (`write_lock`), and each write goes through its own temp
//! file: eight Hive workers restarted at once each wrote trust into the same `.claude.json` from
//! parallel threads through ONE temp name per process, and the 30 s account resync rewrote that
//! file from a copy read before them, so six of eight trust entries were lost (2026-11-18).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::Value;

static WRITES: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
static TMP_SEQ: AtomicU64 = AtomicU64::new(0);

/// Held across a read-edit-replace of a shared config, so no two of maiTerm's own edits
/// interleave. Never held across anything but the edit itself.
pub fn write_lock() -> parking_lot::MutexGuard<'static, ()> {
    WRITES.lock()
}

/// A temp path beside `target` that no other write in this process uses.
fn tmp_beside(target: &Path) -> PathBuf {
    let n = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    target.with_extension(format!("maiterm-{}-{n}", std::process::id()))
}

/// What a writer compares before replacing a file: changed since, and it must read again.
pub fn stamp(p: &Path) -> Option<(u64, std::time::SystemTime)> {
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
    let _serial = write_lock();
    for _ in 0..5 {
        let before = stamp(&target);
        let bytes = fs::read(&target)?;
        let Some(out) = edit(&bytes) else { return Ok(false) };
        if replace_if_unchanged(&target, before, &out)? {
            return Ok(true);
        }
    }
    Err(std::io::Error::other("it kept changing while being updated"))
}

/// Write `bytes` beside `target` and rename it over, unless `target` changed since `before` was
/// taken (Ok(false): read again). Call under `write_lock`.
pub fn replace_if_unchanged(target: &Path, before: Option<(u64, std::time::SystemTime)>, bytes: &[u8]) -> std::io::Result<bool> {
    let tmp = tmp_beside(target);
    fs::write(&tmp, bytes)?;
    if let Ok(m) = fs::metadata(target) {
        let _ = fs::set_permissions(&tmp, m.permissions());
    }
    if stamp(target) != before {
        let _ = fs::remove_file(&tmp);
        return Ok(false);
    }
    fs::rename(&tmp, target).map(|_| true).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Eight Hive workers restarted at once each trust a folder in the same `.claude.json` from
    /// their own thread: every entry lands, and no temp file is left behind.
    #[test]
    fn parallel_edits_of_one_file_keep_every_entry() {
        let dir = std::env::temp_dir().join(format!("json-file-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join(".claude.json");
        fs::write(&f, r#"{"projects":{}}"#).unwrap();
        let threads: Vec<_> = (0..8)
            .map(|i| {
                let f = f.clone();
                std::thread::spawn(move || {
                    update_json(&f, |v| {
                        v["projects"][format!("/w{i}")] = serde_json::json!({"hasTrustDialogAccepted": true});
                        true
                    })
                    .unwrap()
                })
            })
            .collect();
        assert!(threads.into_iter().all(|t| t.join().unwrap()));
        let v: Value = serde_json::from_slice(&fs::read(&f).unwrap()).unwrap();
        assert_eq!(v["projects"].as_object().unwrap().len(), 8, "{v}");
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1, "no temp file left");
        let _ = fs::remove_dir_all(&dir);
    }
}
