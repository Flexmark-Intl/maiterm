//! Listing one folder (docs/file-tree.md §5.5).
//!
//! - `read_dir` with the type from `d_type` (free on unix; already in the FindNextFile data on
//!   Windows, hidden attribute included). No per-entry `lstat`: the only extra syscalls are a
//!   `stat` per symlink (to a folder or not) and the ones std makes itself for `DT_UNKNOWN`.
//! - Size, mtime and the exec bit are hover-only (`ft_stat`), so they are never read here.
//! - Ignored and hidden entries are flagged, never dropped. Only `.git`, `.DS_Store` and
//!   `Thumbs.db` are never listed (`.github` and `.gitignore` are).
//! - Sorted once, here: folders first, then natural, case-insensitive order.
//! - A symlinked folder lists as `L`; one that points back at itself or an ancestor carries the
//!   loop flag and is never expandable.
//! - At most 50,000 entries; past that `truncated` is set and `total` says how many there were.

use std::cmp::Ordering;
use std::fs::FileType;
use std::path::{Path, PathBuf};

use parking_lot::Mutex;

use super::ignore_cache::IgnoreStack;
use super::types::{Entry, FLAG_HIDDEN, FLAG_IGNORED, FLAG_LOOP, FLAG_UNREADABLE};

pub const MAX_ENTRIES: usize = 50_000;
pub const CHUNK: usize = 512;

#[derive(Debug, Clone, PartialEq)]
pub struct Listing {
    pub entries: Vec<Entry>,
    pub total: u32,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ListError {
    Denied(String),
    Gone,
    Other(String),
}

impl ListError {
    pub fn code(&self) -> &'static str {
        match self {
            ListError::Denied(_) => "denied",
            ListError::Gone => "gone",
            // An unexpected IO error is reported as "denied": the folder can't be read, and
            // the wire keeps a closed set of codes.
            ListError::Other(_) => "denied",
        }
    }

    pub fn msg(&self) -> String {
        match self {
            ListError::Denied(m) => format!("Permission denied: {m}"),
            ListError::Gone => "This folder no longer exists".to_string(),
            ListError::Other(m) => m.clone(),
        }
    }
}

fn map_err(e: std::io::Error) -> ListError {
    match e.kind() {
        std::io::ErrorKind::NotFound => ListError::Gone,
        std::io::ErrorKind::PermissionDenied => ListError::Denied(e.to_string()),
        // ENOTDIR: what was a folder is now a file — gone, as far as the tree is concerned.
        _ if e.raw_os_error() == Some(20) => ListError::Gone,
        _ => ListError::Other(e.to_string()),
    }
}

/// Names that are never listed.
pub fn is_builtin_excluded(name: &str) -> bool {
    name == ".git" || name == ".DS_Store" || name == "Thumbs.db"
}

/// The natural sort key of a name: lowercased, with each run of digits written as its length
/// (two digits) followed by the run without leading zeros, so `file9` < `file10` < `File11`
/// by plain byte comparison. A digit compares against any other character the same way
/// whatever its value, so the encoding keeps the order of mixed text too.
pub fn sort_key(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    let bytes = name.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && bytes[i].is_ascii_digit() {
                i += 1;
            }
            let run = &name[start..i];
            let trimmed = run.trim_start_matches('0');
            let digits = if trimmed.is_empty() { "0" } else { trimmed };
            let len = digits.len().min(99);
            out.push((b'0' + (len / 10) as u8) as char);
            out.push((b'0' + (len % 10) as u8) as char);
            out.push_str(digits);
        } else {
            // A char boundary: digits are ASCII, so `i` never lands inside a multibyte char.
            let c = name[i..].chars().next().expect("in bounds");
            if c.is_ascii() {
                out.push(c.to_ascii_lowercase());
            } else {
                out.extend(c.to_lowercase());
            }
            i += c.len_utf8();
        }
    }
    out
}

/// Natural, case-insensitive, numeric-aware order; ties broken by the exact spelling so the
/// order is total and the same on every run.
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    sort_key(a).cmp(&sort_key(b)).then_with(|| a.cmp(b))
}

/// Folders (and links to them) first, then natural order.
pub fn entry_cmp(a: &Entry, b: &Entry) -> Ordering {
    b.is_dirish().cmp(&a.is_dirish()).then_with(|| natural_cmp(&a.n, &b.n))
}

type Keyed = (bool, String, Entry);

fn keyed_cmp(a: &Keyed, b: &Keyed) -> Ordering {
    a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)).then_with(|| a.2.n.cmp(&b.2.n))
}

/// Sort, handing the first `CHUNK` entries to `on_head` as soon as they are in place: the first
/// rows of a 50k folder can be painted before the rest is sorted.
fn sort_with_head(entries: &mut Vec<Entry>, on_head: &mut dyn FnMut(&[Entry])) {
    // One key per entry instead of two per comparison, and no copy of the name. Unstable is
    // fine: the order is total (names are unique).
    let mut keyed: Vec<Keyed> = entries.drain(..).map(|e| (!e.is_dirish(), sort_key(&e.n), e)).collect();
    if keyed.len() > CHUNK {
        keyed.select_nth_unstable_by(CHUNK - 1, keyed_cmp);
        keyed[..CHUNK].sort_unstable_by(keyed_cmp);
        let head: Vec<Entry> = keyed[..CHUNK].iter().map(|k| k.2.clone()).collect();
        on_head(&head);
        keyed[CHUNK..].sort_unstable_by(keyed_cmp);
    } else {
        keyed.sort_unstable_by(keyed_cmp);
    }
    entries.extend(keyed.into_iter().map(|(_, _, e)| e));
}

#[cfg(test)]
pub fn sort_entries(entries: &mut Vec<Entry>) {
    sort_with_head(entries, &mut |_| {});
}

/// Classify one entry. `dir_canon` is the canonical path of the folder being listed, computed
/// on the first symlink that needs it.
fn classify(path: &Path, ft: Option<FileType>, dir_abs: &Path, dir_canon: &mut Option<Option<PathBuf>>) -> (&'static str, u8) {
    let ft = match ft {
        Some(ft) => ft,
        None => match std::fs::symlink_metadata(path) {
            Ok(m) => m.file_type(),
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => return ("o", FLAG_UNREADABLE),
            Err(_) => return ("o", 0),
        },
    };
    if ft.is_dir() {
        return ("d", 0);
    }
    if ft.is_file() {
        return ("f", 0);
    }
    if ft.is_symlink() {
        return match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => {
                // A loop: the link's target is this folder or one of its ancestors, so
                // descending would come back here forever.
                let canon = dir_canon.get_or_insert_with(|| std::fs::canonicalize(dir_abs).ok());
                let looped = match (canon.as_ref(), std::fs::canonicalize(path)) {
                    (Some(dir), Ok(target)) => dir.starts_with(&target),
                    _ => false,
                };
                ("L", if looped { FLAG_LOOP } else { 0 })
            }
            Ok(_) => ("l", 0),
            // Broken, or a loop of links: a link that leads to no file.
            Err(_) => ("l", 0),
        };
    }
    ("o", 0)
}

#[cfg(windows)]
fn hidden(name: &str, ent: &std::fs::DirEntry) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
    name.starts_with('.') || ent.metadata().map(|m| m.file_attributes() & FILE_ATTRIBUTE_HIDDEN != 0).unwrap_or(false)
}

#[cfg(not(windows))]
fn hidden(name: &str, _ent: &std::fs::DirEntry) -> bool {
    name.starts_with('.')
}

/// Every entry of `dir_abs`, typed and hidden-flagged, not yet ignore-flagged or sorted.
pub fn read_raw(dir_abs: &Path) -> Result<Vec<Entry>, ListError> {
    let rd = std::fs::read_dir(dir_abs).map_err(map_err)?;
    let mut dir_canon = None;
    let mut out = Vec::new();
    for ent in rd {
        let Ok(ent) = ent else { continue };
        let name = ent.file_name().into_string().unwrap_or_else(|os| os.to_string_lossy().into_owned());
        if is_builtin_excluded(&name) {
            continue;
        }
        // Files and folders are typed by d_type alone; only links and unknowns need a path.
        let ft = ent.file_type().ok();
        let (k, mut f) = match ft {
            Some(t) if t.is_dir() => ("d", 0),
            Some(t) if t.is_file() => ("f", 0),
            _ => classify(&dir_abs.join(&name), ft, dir_abs, &mut dir_canon),
        };
        if hidden(&name, &ent) {
            f |= FLAG_HIDDEN;
        }
        out.push(Entry { n: name, k, f });
    }
    Ok(out)
}

/// One entry of `dir_abs` by name, for the coalescer's small updates: None when it is gone.
pub fn stat_entry(dir_abs: &Path, name: &str) -> Option<Entry> {
    if is_builtin_excluded(name) {
        return None;
    }
    let path = dir_abs.join(name);
    let ft = std::fs::symlink_metadata(&path).ok()?.file_type();
    let (k, mut f) = classify(&path, Some(ft), dir_abs, &mut None);
    if name.starts_with('.') {
        f |= FLAG_HIDDEN;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if std::fs::symlink_metadata(&path).map(|m| m.file_attributes() & 0x2 != 0).unwrap_or(false) {
            f |= FLAG_HIDDEN;
        }
    }
    Some(Entry { n: name.to_string(), k, f })
}

/// Set the ignore flag on `entries` of folder `rel`.
pub fn flag_ignored(entries: &mut [Entry], rel: &str, ignore: &Mutex<IgnoreStack>) {
    let mut ig = ignore.lock();
    let m = ig.for_dir(rel);
    for e in entries.iter_mut() {
        if m.is_ignored(&e.n, e.is_dirish()) {
            e.f |= FLAG_IGNORED;
        } else {
            e.f &= !FLAG_IGNORED;
        }
    }
}

/// The folder `rel` under `root`, ready to send. The disk is read before the ignore stack is
/// locked, so a folder that hangs on a dead mount never holds up another folder's listing.
#[cfg(test)]
pub fn list_dir(root: &Path, rel: &str, ignore: &Mutex<IgnoreStack>) -> Result<Listing, ListError> {
    list_dir_with(root, rel, ignore, &mut |_, _, _| {})
}

/// `list_dir`, calling `on_head(first 512 entries, total, truncated)` before the rest is
/// sorted when the folder is bigger than one chunk.
pub fn list_dir_with(root: &Path, rel: &str, ignore: &Mutex<IgnoreStack>, on_head: &mut dyn FnMut(&[Entry], u32, bool)) -> Result<Listing, ListError> {
    let abs = super::paths::rel_to_native(root, rel).map_err(ListError::Other)?;
    let mut entries = read_raw(&abs)?;
    flag_ignored(&mut entries, rel, ignore);
    let total = entries.len();
    let truncated = total > MAX_ENTRIES;
    let total32 = total.min(u32::MAX as usize) as u32;
    sort_with_head(&mut entries, &mut |head| on_head(head, total32, truncated));
    entries.truncate(MAX_ENTRIES);
    Ok(Listing { entries, total: total32, truncated })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn natural_sort_is_numeric_case_insensitive_and_total() {
        let mut v = vec!["file10", "File2", "file1", "file01", "a", "B", "file2", "img12.png", "img2.png", "x-1", "x1", "10", "9"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["9", "10", "a", "B", "file01", "file1", "File2", "file2", "file10", "img2.png", "img12.png", "x-1", "x1"]);
        // Very long digit runs still compare by value, not by their first digit.
        assert_eq!(natural_cmp("v99999999999999999999999", "v123456789012345678901234"), Ordering::Less);
        assert_eq!(natural_cmp("a", "a"), Ordering::Equal);
    }

    #[test]
    fn folders_first_and_sort_entries_matches_entry_cmp() {
        let mk = |n: &str, k: &'static str| Entry { n: n.into(), k, f: 0 };
        let mut v = vec![mk("b.txt", "f"), mk("Zeta", "d"), mk("alpha", "L"), mk("a.txt", "f"), mk("link", "l")];
        let mut w = v.clone();
        sort_entries(&mut v);
        w.sort_by(entry_cmp);
        assert_eq!(v, w);
        let names: Vec<&str> = v.iter().map(|e| e.n.as_str()).collect();
        assert_eq!(names, vec!["alpha", "Zeta", "a.txt", "b.txt", "link"]);
    }

    #[cfg(unix)]
    #[test]
    fn lists_types_flags_and_builtins() {
        let t = std::env::temp_dir().join(format!("ft-list-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(t.join(".git")).unwrap();
        fs::create_dir_all(t.join("src/deep")).unwrap();
        fs::write(t.join(".gitignore"), "*.log\n").unwrap();
        fs::write(t.join("a.log"), "").unwrap();
        fs::write(t.join(".env"), "").unwrap();
        fs::write(t.join(".DS_Store"), "").unwrap();
        fs::write(t.join("file.txt"), "").unwrap();
        std::os::unix::fs::symlink(t.join("file.txt"), t.join("to-file")).unwrap();
        std::os::unix::fs::symlink(t.join("src"), t.join("to-src")).unwrap();
        std::os::unix::fs::symlink(t.join("nope"), t.join("broken")).unwrap();
        // A link back to an ancestor: expanding it would never end.
        std::os::unix::fs::symlink(&t, t.join("src/deep/up")).unwrap();
        let ig = Mutex::new(IgnoreStack::for_test(&t, Some(&t.join(".git")), None, &[]));
        let l = list_dir(&t, "", &ig).unwrap();
        let get = |n: &str| l.entries.iter().find(|e| e.n == n).cloned();
        assert!(get(".git").is_none() && get(".DS_Store").is_none());
        assert_eq!(get("src").unwrap().k, "d");
        assert_eq!(get("to-src").unwrap().k, "L");
        assert_eq!(get("to-src").unwrap().f & FLAG_LOOP, 0);
        assert_eq!(get("to-file").unwrap().k, "l");
        assert_eq!(get("broken").unwrap().k, "l");
        assert_eq!(get("a.log").unwrap().f & FLAG_IGNORED, FLAG_IGNORED);
        assert_eq!(get(".env").unwrap().f & FLAG_HIDDEN, FLAG_HIDDEN);
        assert_eq!(get(".gitignore").unwrap().f, FLAG_HIDDEN);
        assert!(l.entries[0].is_dirish() && l.entries[1].is_dirish());
        let deep = list_dir(&t, "src/deep", &ig).unwrap();
        let up = deep.entries.iter().find(|e| e.n == "up").unwrap();
        assert_eq!((up.k, up.f & FLAG_LOOP), ("L", FLAG_LOOP));
        // The loop is seen through the link too: listing to-src/deep finds the same loop.
        let via = list_dir(&t, "to-src/deep", &ig).unwrap();
        assert_eq!(via.entries.iter().find(|e| e.n == "up").unwrap().f & FLAG_LOOP, FLAG_LOOP);
        assert_eq!(list_dir(&t, "missing", &ig), Err(ListError::Gone));
        assert_eq!(stat_entry(&t, "file.txt"), Some(Entry { n: "file.txt".into(), k: "f", f: 0 }));
        assert_eq!(stat_entry(&t, "gone.txt"), None);
        fs::remove_dir_all(&t).unwrap();
    }
}
