//! Root-relative POSIX paths and the native paths behind them (docs/file-tree.md §5.1).
//!
//! The wire speaks only `src/lib/x.ts` relative to a hub's canonical root. This module is the
//! one place that turns those into native paths and back:
//! - a rel is checked component by component: no `..`, `.`, empty or absolute parts, so a rel
//!   can never name anything outside the root by its spelling;
//! - an absolute path is matched against the root component-wise, NFC-normalized (APFS keeps a
//!   name in the form it was typed, often NFD, while hooks and typed paths arrive NFC) and
//!   case-folded only where the volume folds (`relocate::case_insensitive`);
//! - a path that doesn't match as spelled is canonicalized, which covers macOS `/tmp` vs
//!   `/private/tmp` and `..` segments.

use std::borrow::Cow;
use std::path::{Component, Path, PathBuf};

use unicode_normalization::{is_nfc_quick, IsNormalized, UnicodeNormalization};

/// `s` in NFC. Borrowed when it already is, which is nearly always.
pub fn nfc(s: &str) -> Cow<'_, str> {
    match is_nfc_quick(s.chars()) {
        IsNormalized::Yes => Cow::Borrowed(s),
        _ => Cow::Owned(s.nfc().collect()),
    }
}

/// macOS volumes (APFS, HFS+) treat NFC and NFD spellings as one name. ext4 and NTFS keep them
/// as two different files, so folding there would match a path to a file it doesn't name.
pub const NORMALIZATION_INSENSITIVE: bool = cfg!(target_os = "macos");

/// `s` in the one spelling a name has here: NFC where the platform's volumes ignore
/// normalization, untouched elsewhere. Also how macOS git spells paths
/// (`core.precomposeunicode`), while a listing gives the name as stored, often NFD.
pub fn norm(s: &str) -> Cow<'_, str> {
    if NORMALIZATION_INSENSITIVE {
        nfc(s)
    } else {
        Cow::Borrowed(s)
    }
}

/// The form two names are compared in: NFC where the platform's volumes ignore normalization,
/// and lowercased where the volume folds case.
pub fn name_key(s: &str, fold: bool) -> String {
    let n = norm(s);
    if fold {
        n.to_lowercase()
    } else {
        n.into_owned()
    }
}

/// The components of a root-relative path, refused if any could step outside the root.
/// `""` is the root itself and has none.
pub fn split_rel(rel: &str) -> Result<Vec<&str>, String> {
    if rel.is_empty() {
        return Ok(Vec::new());
    }
    let bad = |why: &str| Err(format!("not a path inside the root ({why}): {rel}"));
    if rel.starts_with('/') {
        return bad("absolute");
    }
    let mut out = Vec::new();
    for part in rel.split('/') {
        if part.is_empty() {
            return bad("empty component");
        }
        if part == "." || part == ".." {
            return bad("dot component");
        }
        if part.contains('\0') {
            return bad("NUL");
        }
        // On Windows a backslash is a separator and `C:x` is drive-relative: either would let
        // one "component" walk somewhere else.
        if cfg!(windows) && (part.contains('\\') || part.contains(':')) {
            return bad("separator in a name");
        }
        out.push(part);
    }
    Ok(out)
}

/// The native path of `rel` under `root`.
pub fn rel_to_native(root: &Path, rel: &str) -> Result<PathBuf, String> {
    let mut p = root.to_path_buf();
    for part in split_rel(rel)? {
        p.push(part);
    }
    Ok(p)
}

/// `parent/name` as a rel.
pub fn join_rel(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        name.to_string()
    } else {
        format!("{parent}/{name}")
    }
}

/// (parent, name) of a rel. The root has no parent: `("", "")`.
pub fn split_parent(rel: &str) -> (&str, &str) {
    match rel.rfind('/') {
        Some(i) => (&rel[..i], &rel[i + 1..]),
        None => ("", rel),
    }
}

/// Is `rel` the folder `dir` or inside it?
pub fn is_at_or_under(rel: &str, dir: &str) -> bool {
    dir.is_empty() || rel == dir || (rel.len() > dir.len() && rel.starts_with(dir) && rel.as_bytes()[dir.len()] == b'/')
}

fn comp_key(c: &Component<'_>, fold: bool) -> String {
    match c {
        // A drive letter's case never matters, whatever the volume does with names.
        Component::Prefix(p) => p.as_os_str().to_string_lossy().to_lowercase(),
        Component::RootDir => "/".to_string(),
        other => name_key(&other.as_os_str().to_string_lossy(), fold),
    }
}

/// The components of `path` below `root`, matched component-wise (never as a string prefix:
/// `/a/app` is not under `/a/ap`). None when `path` isn't at or under `root`, or when what
/// follows the root has a `.`/`..` the caller must canonicalize first.
pub fn strip_root(path: &Path, root: &Path, fold: bool) -> Option<Vec<String>> {
    let mut p = path.components();
    for rc in root.components() {
        let pc = p.next()?;
        if comp_key(&pc, fold) != comp_key(&rc, fold) {
            return None;
        }
    }
    let mut out = Vec::new();
    for c in p {
        match c {
            Component::Normal(s) => out.push(s.to_string_lossy().into_owned()),
            _ => return None,
        }
    }
    Some(out)
}

/// `path` with `.` dropped and `..` folded, lexically. Only for the part of a path that doesn't
/// exist yet: an existing prefix is canonicalized by the disk instead, because under a symlink
/// a lexical `..` names a different folder.
fn lexical_normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` canonicalized as far as it exists: the longest existing prefix through the disk
/// (symlinks resolved, `\\?\` stripped by `share::canon`), the rest appended lexically.
pub fn canon_existing(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    while !existing.exists() {
        match (existing.file_name().map(|n| n.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return lexical_normalize(path),
        }
    }
    let mut out = crate::share::canon(&existing);
    for name in rest.into_iter().rev() {
        out.push(name);
    }
    lexical_normalize(&out)
}

/// The components of an absolute path below `root` (already canonical): as spelled first, then
/// canonicalized. The spelled match comes first on purpose: under a symlinked folder inside
/// the root, the tree keys rows by the LINK path, which canonicalizing would leave behind.
pub fn locate_components(root: &Path, fold: bool, abs: &str) -> Option<Vec<String>> {
    let p = Path::new(abs);
    if !p.is_absolute() {
        return None;
    }
    let plain = p.components().all(|c| !matches!(c, Component::CurDir | Component::ParentDir));
    if plain {
        if let Some(rest) = strip_root(p, root, fold) {
            return Some(rest);
        }
    }
    strip_root(&canon_existing(p), root, fold)
}

/// Whether `rel`, which a lookup below `root` (canonical) just found, is stored spelled the
/// way `rel` spells it. On a volume that folds case (`fold`) a lookup of `Foo.ts` finds
/// `foo.ts`, so after a case-only rename the old name still looks present, and anything
/// indexed by name (Find) would list the file twice. The stored spelling comes from
/// canonicalizing, which on macOS and Windows answers each component as stored; a symlink is
/// canonicalized only up to its folder (resolving it would leave its own name behind) and its
/// name read from the folder's listing. Only a definite mismatch says no: a symlinked folder
/// on the way, or a lookup that fails, keeps the answer the first lookup gave.
pub fn spelled_on_disk(root: &Path, rel: &str, fold: bool) -> bool {
    if !fold {
        return true;
    }
    let Ok(parts) = split_rel(rel) else { return true };
    let Some((leaf, dirs)) = parts.split_last() else { return true };
    let Ok(abs) = rel_to_native(root, rel) else { return true };
    let link = std::fs::symlink_metadata(&abs).is_ok_and(|m| m.file_type().is_symlink());
    let (target, want) = match (link, abs.parent()) {
        (true, Some(parent)) => (parent.to_path_buf(), dirs),
        _ => (abs.clone(), &parts[..]),
    };
    let canon = crate::share::canon(&target);
    let Ok(tail) = canon.strip_prefix(root) else { return true };
    let got: Vec<String> = tail.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    if got.len() != want.len() {
        return true;
    }
    if got.iter().zip(want).any(|(g, w)| norm(g) != norm(w)) {
        return false;
    }
    if !link {
        return true;
    }
    let Some(parent) = abs.parent() else { return true };
    match std::fs::read_dir(parent) {
        Ok(rd) => rd.flatten().any(|e| norm(&e.file_name().to_string_lossy()) == norm(leaf)),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The source of a case-only rename still resolves where the volume folds case; only the
    /// stored spelling tells the two names apart. A volume that keeps case answers yes without
    /// asking (both names really are there, or the first lookup failed).
    #[test]
    fn a_case_only_renames_source_is_not_spelled_on_disk() {
        let tmp = std::env::temp_dir().join(format!("ft-spell-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("Dir")).unwrap();
        let root = crate::share::canon(&tmp);
        std::fs::write(root.join("Dir/foo.ts"), "").unwrap();
        let fold = crate::relocate::case_insensitive(&root);
        assert!(spelled_on_disk(&root, "Dir/foo.ts", fold));
        assert!(spelled_on_disk(&root, "Dir/Foo.ts", false), "unfolded: nothing to ask");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("foo.ts", root.join("Dir/link.ts")).unwrap();
            assert!(spelled_on_disk(&root, "Dir/link.ts", fold), "a symlink keeps its own name");
        }
        if fold {
            assert!(!spelled_on_disk(&root, "Dir/Foo.ts", fold), "the leaf's case differs");
            assert!(!spelled_on_disk(&root, "dir/foo.ts", fold), "a folder's case differs");
            #[cfg(unix)]
            assert!(!spelled_on_disk(&root, "Dir/LINK.ts", fold), "a symlink's case differs");
        }
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rel_can_never_step_outside() {
        assert_eq!(split_rel("").unwrap(), Vec::<&str>::new());
        assert_eq!(split_rel("src/lib/x.ts").unwrap(), vec!["src", "lib", "x.ts"]);
        for bad in ["/etc", "../x", "a/../../b", "a//b", "a/./b", "a/", "a\0b"] {
            assert!(split_rel(bad).is_err(), "{bad:?}");
        }
        assert_eq!(rel_to_native(Path::new("/r"), "a/b").unwrap(), PathBuf::from("/r/a/b"));
    }

    #[test]
    fn parents_and_containment_are_component_wise() {
        assert_eq!(split_parent("a/b/c"), ("a/b", "c"));
        assert_eq!(split_parent("a"), ("", "a"));
        assert!(is_at_or_under("a/b", "a"));
        assert!(is_at_or_under("a", "a"));
        assert!(is_at_or_under("anything", ""));
        assert!(!is_at_or_under("ab", "a"));
        assert_eq!(join_rel("", "x"), "x");
        assert_eq!(join_rel("a", "x"), "a/x");
    }

    #[cfg(unix)]
    #[test]
    fn containment_is_nfc_and_folds_case_only_where_the_disk_does() {
        let nfd: String = "café".nfd().collect();
        let nfc_name: String = "café".nfc().collect();
        assert_ne!(nfd, nfc_name);
        let root = PathBuf::from(format!("/u/{nfd}"));
        // A hook path arrives NFC; the root came off an APFS listing in NFD. One folder on
        // macOS; two different folders on ext4.
        let hook = PathBuf::from(format!("/u/{nfc_name}/src/a.ts"));
        let want = if NORMALIZATION_INSENSITIVE { Some(vec!["src".to_string(), "a.ts".to_string()]) } else { None };
        assert_eq!(strip_root(&hook, &root, false), want);
        let root = PathBuf::from(format!("/u/{nfc_name}"));
        // Case: folded only when the volume folds.
        let upper = PathBuf::from(format!("/U/{nfc_name}/x"));
        assert_eq!(strip_root(&upper, &root, true), Some(vec!["x".to_string()]));
        assert_eq!(strip_root(&upper, &root, false), None);
        // A sibling that merely shares a prefix is not inside.
        assert_eq!(strip_root(Path::new("/u/caféX/x"), Path::new("/u/café"), true), None);
        // The root itself is "".
        assert_eq!(strip_root(&root, &root, false), Some(vec![]));
    }

    #[cfg(unix)]
    #[test]
    fn locate_canonicalizes_dot_segments_and_symlinked_roots() {
        let tmp = std::env::temp_dir().join(format!("ft-locate-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("proj/src")).unwrap();
        std::os::unix::fs::symlink(tmp.join("proj"), tmp.join("alias")).unwrap();
        let root = crate::share::canon(&tmp.join("proj"));
        let fold = crate::relocate::case_insensitive(&root);
        let s = |p: PathBuf| p.to_string_lossy().to_string();
        assert_eq!(locate_components(&root, fold, &s(tmp.join("proj/src/../src/a.ts"))), Some(vec!["src".into(), "a.ts".into()]));
        // Through a symlink to the root: canonicalized onto it.
        assert_eq!(locate_components(&root, fold, &s(tmp.join("alias/src/b.ts"))), Some(vec!["src".into(), "b.ts".into()]));
        assert_eq!(locate_components(&root, fold, &s(tmp.clone())), None);
        assert_eq!(locate_components(&root, fold, "relative/x"), None);
        std::fs::remove_dir_all(&tmp).unwrap();
    }
}
