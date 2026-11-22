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

/// Read-edit-replace `path` under `write_lock`, for every maiTerm writer of an agent's settings
/// file (`~/.claude/settings.json`, `~/.codex/config.toml`, `hooks.json`…): live agents, the
/// hook installer's reassert timer, Agent Defaults and Deshittify all rewrite the same files, and
/// a rename from a copy read before another's write drops that write.
///
/// `edit` gets the current text (None: no file) and returns the new text, or None for "nothing to
/// change"; it runs again when the file changed while it was being edited. With
/// `create_if_absent` false a missing file is left missing (`Ok(false)`, `edit` not called);
/// otherwise it is created with its folder. Through a symlink to the file it names, and a link
/// whose file is gone is refused: writing would put a plain file where the user's dotfiles link
/// was, and the agent would stop reading their real one. The file's mode is kept.
pub fn edit_text(
    path: &Path,
    create_if_absent: bool,
    mut edit: impl FnMut(Option<&str>) -> Result<Option<String>, String>,
) -> Result<bool, String> {
    let _serial = write_lock();
    let target = match fs::canonicalize(path) {
        Ok(t) => t,
        Err(_) if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) => {
            return Err(format!("{} is a link to a file that isn't there. maiTerm won't replace the link.", path.display()));
        }
        Err(_) if !create_if_absent => return Ok(false),
        Err(_) => path.to_path_buf(),
    };
    for _ in 0..5 {
        let before = stamp(&target);
        let raw = match fs::read_to_string(&target) {
            Ok(r) => Some(r),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if !create_if_absent {
                    return Ok(false);
                }
                None
            }
            Err(e) => return Err(format!("Cannot read {}: {e}", target.display())),
        };
        let Some(out) = edit(raw.as_deref())? else { return Ok(false) };
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        if replace_if_unchanged(&target, before, out.as_bytes()).map_err(|e| format!("Cannot write {}: {e}", target.display()))? {
            return Ok(true);
        }
    }
    Err(format!("{} kept changing while being updated", target.display()))
}

/// `edit_text` for a JSON settings object: `{}` for no file or an empty one; a file that doesn't
/// parse, or isn't an object, is an error and is never written (treating it as `{}` would replace
/// the user's whole file with ours). `f` returns whether it changed anything.
pub fn edit_json_object(
    path: &Path,
    create_if_absent: bool,
    mut f: impl FnMut(&mut Value) -> Result<bool, String>,
) -> Result<bool, String> {
    edit_text(path, create_if_absent, |raw| {
        let mut doc: Value = match raw.filter(|r| !r.trim().is_empty()) {
            None => serde_json::json!({}),
            Some(r) => serde_json::from_str(r).map_err(|e| {
                format!("{} could not be parsed ({e}). maiTerm won't overwrite settings it can't read.", path.display())
            })?,
        };
        if !doc.is_object() {
            return Err(format!("{} is not a JSON object. maiTerm won't overwrite it.", path.display()));
        }
        if !f(&mut doc)? {
            return Ok(None);
        }
        serde_json::to_string_pretty(&doc).map(|s| Some(s + "\n")).map_err(|e| e.to_string())
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

    fn scratch_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("json-file-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// One trailing comma must not cost the user every setting they have: the file is refused,
    /// not read as `{}` and written over (the hook installer did that until 2026-11-22).
    #[test]
    fn an_unparseable_settings_file_is_never_written() {
        let d = scratch_dir("bad");
        let f = d.join("settings.json");
        fs::write(&f, "{\n  \"model\": \"opus\",\n}\n").unwrap();
        let err = edit_json_object(&f, true, |v| {
            v["hooks"] = serde_json::json!({});
            Ok(true)
        })
        .unwrap_err();
        assert!(err.contains("could not be parsed"), "{err}");
        assert_eq!(fs::read_to_string(&f).unwrap(), "{\n  \"model\": \"opus\",\n}\n");
        fs::write(&f, "[1]").unwrap();
        assert!(edit_json_object(&f, true, |_| Ok(true)).is_err(), "not an object");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_file_is_created_only_when_asked() {
        let d = scratch_dir("missing");
        let f = d.join("sub").join("settings.json");
        assert!(!edit_json_object(&f, false, |_| Ok(true)).unwrap());
        assert!(!f.exists());
        assert!(edit_json_object(&f, true, |v| {
            v["a"] = serde_json::json!(1);
            Ok(true)
        })
        .unwrap());
        assert_eq!(serde_json::from_str::<Value>(&fs::read_to_string(&f).unwrap()).unwrap()["a"], 1);
        let _ = fs::remove_dir_all(&d);
    }

    #[cfg(unix)]
    #[test]
    fn a_link_stays_a_link_a_dangling_one_is_refused_and_the_mode_is_kept() {
        use std::os::unix::fs::PermissionsExt;
        let d = scratch_dir("link");
        let real = d.join("dotfiles").join("config.toml");
        fs::create_dir_all(real.parent().unwrap()).unwrap();
        fs::write(&real, "a = 1\n").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o600)).unwrap();
        let link = d.join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(edit_text(&link, true, |raw| Ok(Some(format!("{}b = 2\n", raw.unwrap())))).unwrap());
        assert!(fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(&real).unwrap(), "a = 1\nb = 2\n");
        assert_eq!(fs::metadata(&real).unwrap().permissions().mode() & 0o777, 0o600);

        let dangling = d.join("gone.json");
        std::os::unix::fs::symlink(d.join("nowhere.json"), &dangling).unwrap();
        for create in [true, false] {
            assert!(edit_json_object(&dangling, create, |_| Ok(true)).is_err());
        }
        assert!(fs::symlink_metadata(&dangling).unwrap().file_type().is_symlink());
        assert!(!d.join("nowhere.json").exists());
        let _ = fs::remove_dir_all(&d);
    }

}
