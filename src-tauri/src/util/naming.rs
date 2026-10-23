//! Picking a free name for a copy, the way each file manager does it.
//!
//! Files' "keep both" and Duplicate (docs/file-tree.md §10.2) name copies as the platform's own
//! file manager would, so a copy made in maiTerm looks like one made in Finder or Explorer. The
//! editor's "save to Downloads" keeps its plain numbered style.

use std::path::{Path, PathBuf};

/// How copies of `name.ext` are spelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyNames {
    /// `name (1).ext`, `name (2).ext`: a download that would collide.
    Numbered,
    /// Finder: `name copy.ext`, `name copy 2.ext`.
    Mac,
    /// Explorer: `name - Copy.ext`, `name - Copy (2).ext`.
    Windows,
    /// Nautilus and most Linux file managers: `name (copy).ext`, `name (copy 2).ext`.
    Linux,
}

impl CopyNames {
    /// This platform's file manager.
    pub fn platform() -> Self {
        if cfg!(target_os = "macos") {
            CopyNames::Mac
        } else if cfg!(windows) {
            CopyNames::Windows
        } else {
            CopyNames::Linux
        }
    }

    /// The `n`th copy's name (n ≥ 1) from a stem and an extension (with its dot, or "").
    fn nth(self, stem: &str, ext: &str, n: u32) -> String {
        match (self, n) {
            (CopyNames::Numbered, n) => format!("{stem} ({n}){ext}"),
            (CopyNames::Mac, 1) => format!("{stem} copy{ext}"),
            (CopyNames::Mac, n) => format!("{stem} copy {n}{ext}"),
            (CopyNames::Windows, 1) => format!("{stem} - Copy{ext}"),
            (CopyNames::Windows, n) => format!("{stem} - Copy ({n}){ext}"),
            (CopyNames::Linux, 1) => format!("{stem} (copy){ext}"),
            (CopyNames::Linux, n) => format!("{stem} (copy {n}){ext}"),
        }
    }
}

/// Split a name for numbering. A folder keeps its whole name (`v1.2` is not an extension), and
/// so does a dotfile (`.env`).
fn stem_ext(name: &str, is_dir: bool) -> (&str, &str) {
    if is_dir {
        return (name, "");
    }
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

/// A name for `name` that `taken` says is free. `fresh`: never the name itself, always a copy's
/// name (Duplicate). None only if a thousand candidates are all taken.
pub fn unique_name(name: &str, is_dir: bool, style: CopyNames, fresh: bool, taken: impl Fn(&str) -> bool) -> Option<String> {
    if !fresh && !taken(name) {
        return Some(name.to_string());
    }
    let (stem, ext) = stem_ext(name, is_dir);
    (1..1000).map(|n| style.nth(stem, ext, n)).find(|c| !taken(c))
}

/// A non-colliding path inside `dir` for `name`, numbered in `style`; `dir/name` itself when all
/// candidates are taken (the caller's write then fails on its own terms).
pub fn unique_path(dir: &Path, name: &str, style: CopyNames) -> PathBuf {
    let taken = |n: &str| dir.join(n).symlink_metadata().is_ok();
    match unique_name(name, false, style, false, taken) {
        Some(n) => dir.join(n),
        None => dir.join(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_platform_spells_copies_like_its_file_manager() {
        let taken = |set: &'static [&'static str]| move |n: &str| set.contains(&n);
        let one = &["a.ts"][..];
        let two = &["a.ts", "a copy.ts", "a - Copy.ts", "a (copy).ts", "a (1).ts"][..];
        assert_eq!(unique_name("a.ts", false, CopyNames::Mac, false, taken(one)).unwrap(), "a copy.ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Mac, false, taken(two)).unwrap(), "a copy 2.ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Windows, false, taken(one)).unwrap(), "a - Copy.ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Windows, false, taken(two)).unwrap(), "a - Copy (2).ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Linux, false, taken(one)).unwrap(), "a (copy).ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Linux, false, taken(two)).unwrap(), "a (copy 2).ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Numbered, false, taken(two)).unwrap(), "a (2).ts");
    }

    #[test]
    fn free_names_stay_and_folders_and_dotfiles_keep_their_dots() {
        let none = |_: &str| false;
        assert_eq!(unique_name("a.ts", false, CopyNames::Mac, false, none).unwrap(), "a.ts");
        assert_eq!(unique_name("a.ts", false, CopyNames::Mac, true, none).unwrap(), "a copy.ts", "Duplicate always copies");
        assert_eq!(unique_name("v1.2", true, CopyNames::Mac, true, none).unwrap(), "v1.2 copy");
        assert_eq!(unique_name(".env", false, CopyNames::Linux, true, none).unwrap(), ".env (copy)");
        assert_eq!(unique_name("a.tar.gz", false, CopyNames::Windows, true, none).unwrap(), "a.tar - Copy.gz");
    }
}
