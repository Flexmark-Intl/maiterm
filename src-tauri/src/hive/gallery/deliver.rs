//! Assets out of the gallery into folders (docs/hive.md §7.3, §10.6): the coordinator's
//! `galleryDeliver` into the Hive's plain folders, and the human's Export… anywhere.
//!
//! Delivery writes new files only. The target is checked before any byte is written: a plain
//! folder of the Hive or an existing subfolder of one (maiTerm never makes folders there, plan
//! D14), canonical, outside every git work tree. The folder is then opened without following a
//! link and its open descriptor's real path checked again, and each file is an exclusive create
//! in that descriptor (`fclonefileat`, else `O_EXCL`): a name that appears meanwhile gets a
//! suffix, never an overwrite. Overwriting is the human's alone (Export… with Replace, through a
//! temp file renamed over the target).

use std::fs::{self, File};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use super::model::Asset;
use crate::hive::paths::HivePaths;
use crate::hive::record::{Hive, KIND_PLAIN};

/// A name is one component of at most this many bytes.
pub const MAX_NAME_BYTES: usize = 255;
/// `name (2)` … `name (99)`, then a refusal.
pub const MAX_SUFFIX: u32 = 99;
/// Assets one delivery or export takes.
pub const MAX_DELIVER: usize = 500;
const TMP_TAIL: &str = ".maiterm-tmp";

fn canon(p: &Path) -> PathBuf {
    crate::share::canon(p)
}

/// The refusal for a git folder, which names the route that is reviewed (§10.6).
fn in_git(at: &Path) -> String {
    format!(
        "{} is inside a git work tree, which maiTerm doesn't write into. Have a worker put the files in its worktree \
         (hiveSpawnWorker with inputs, or hiveMessageWorker with assets) and land them, so the change is reviewed.",
        at.display()
    )
}

/// Where `folder` delivers to: a plain folder's name with an optional relative subfolder
/// (`brand/icons`), or an absolute path inside a plain folder. Answers the canonical folder and
/// the plain folder's name. It must exist, sit within a plain folder by component, have no
/// hidden folder below the plain folder (`.claude` is agent config), and have no `.git` in it or
/// above it, whatever the folder is called (§7.3). A git folder of the Hive, named or pathed
/// into, is refused with the worktree-and-land route.
pub fn resolve_target(hive: &Hive, folder: &str) -> Result<(PathBuf, String), String> {
    let folder = folder.trim();
    if folder.is_empty() {
        return Err("folder is required: a plain folder's name (with a subfolder: brand/icons), or a path inside one.".to_string());
    }
    let plain: Vec<_> = hive.folders.iter().filter(|f| f.kind == KIND_PLAIN).collect();
    let names = || {
        let n: Vec<&str> = plain.iter().map(|f| f.name.as_str()).collect();
        if n.is_empty() { "This Hive has no plain folders: the human adds them in Hive settings.".to_string() } else { format!("Its plain folders are: {}.", n.join(", ")) }
    };
    let joined = if Path::new(folder).is_absolute() {
        PathBuf::from(folder)
    } else {
        let (name, rest) = folder.split_once(['/', '\\']).unwrap_or((folder, ""));
        let Some(f) = plain.iter().find(|f| f.name == name) else {
            if let Some(g) = hive.folders.iter().find(|f| f.kind != KIND_PLAIN && f.name == name) {
                return Err(in_git(Path::new(&g.path)));
            }
            return Err(format!("No plain folder called \"{}\" in this Hive. {}", crate::hive::deliver::one_line(name, 80), names()));
        };
        let mut p = PathBuf::from(&f.path);
        for c in Path::new(rest).components() {
            match c {
                Component::Normal(s) => p.push(s),
                Component::CurDir => {}
                _ => return Err(format!("\"{}\": a subfolder is a plain relative path, with no \"..\".", crate::hive::deliver::one_line(folder, 200))),
            }
        }
        p
    };
    let dir = canon(&fs::canonicalize(&joined).map_err(|e| format!("{}: {e}. maiTerm doesn't make folders there: deliver into one that exists.", joined.display()))?);
    if !dir.is_dir() {
        return Err(format!("{} isn't a folder.", dir.display()));
    }
    let roots = |kind_plain: bool| {
        hive.folders.iter().filter(move |f| (f.kind == KIND_PLAIN) == kind_plain).filter_map(|f| fs::canonicalize(&f.path).ok().map(|root| (f, canon(&root))))
    };
    let Some((owner, root)) = roots(true).find(|(_, root)| dir.starts_with(root)) else {
        if roots(false).any(|(_, root)| dir.starts_with(&root)) {
            return Err(in_git(&dir));
        }
        return Err(format!("{} isn't in one of this Hive's plain folders. {}", dir.display(), names()));
    };
    if dir.components().any(|c| c.as_os_str() == ".git") || dir.ancestors().any(|a| fs::symlink_metadata(a.join(".git")).is_ok()) {
        return Err(in_git(&dir));
    }
    if dir.strip_prefix(&root).is_ok_and(|rel| rel.components().any(|c| c.as_os_str().to_string_lossy().starts_with('.'))) {
        return Err(format!("{}: a hidden folder isn't delivered into (it may be agent or tool config).", dir.display()));
    }
    Ok((dir, owner.name.clone()))
}

/// Names Claude or a build reads as instructions or runs (§8.1's list, `package.json`,
/// `CLAUDE.local.md`, and the human's own patterns), at `dir` under its plain folder: those reach
/// the human's folder only through a land the human approves, never a delivery.
pub fn check_config_names(hive: &Hive, dir: &Path, names: &[String]) -> Result<(), String> {
    let rel = hive
        .folders
        .iter()
        .filter(|f| f.kind == KIND_PLAIN)
        .filter_map(|f| fs::canonicalize(&f.path).ok().map(|r| canon(&r)))
        .find_map(|root| dir.strip_prefix(&root).ok().map(|r| r.to_string_lossy().replace('\\', "/")))
        .unwrap_or_default();
    let extra = &hive.settings.land.config_paths;
    for n in names {
        let at = if rel.is_empty() { n.clone() } else { format!("{rel}/{n}") };
        if crate::hive::land::config_rule::listed(&at, extra) || crate::hive::land::config_rule::is_package_json(&at) || n.eq_ignore_ascii_case("claude.local.md") {
            return Err(format!(
                "\"{}\" is agent or build config, which a delivery doesn't write. Have a worker put it in its worktree and land it, \
                 so your human approves it.",
                crate::hive::deliver::one_line(n, 80)
            ));
        }
    }
    Ok(())
}

/// Unicode format characters (category Cf): invisible, and the bidi overrides among them make
/// `hero\u{202E}gnp.terminal` show as an image's name in Finder.
fn is_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{06DD}' | '\u{070F}' | '\u{180E}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}' | '\u{2066}'..='\u{206F}' | '\u{FEFF}' | '\u{FFF9}'..='\u{FFFB}' | '\u{110BD}' | '\u{E0001}' | '\u{E0020}'..='\u{E007F}')
}

/// Each name is one path component: no separator, no `..`, no leading dot, no control or
/// format character, at most 255 bytes, and no two alike (in any case: most volumes ignore it).
pub fn check_names(names: &[String]) -> Result<(), String> {
    let mut seen: Vec<String> = Vec::new();
    for n in names {
        let shown = crate::hive::deliver::one_line(n, 80);
        if n.trim().is_empty() {
            return Err("A name is empty.".to_string());
        }
        if n.contains(['/', '\\']) || n == "." || n == ".." {
            return Err(format!("\"{shown}\" isn't one file name: no folders in a name."));
        }
        if n.starts_with('.') {
            return Err(format!("\"{shown}\" starts with a dot: a hidden file isn't delivered."));
        }
        if n.chars().any(|c| c.is_control() || is_format(c)) || n.len() > MAX_NAME_BYTES {
            return Err(format!("\"{shown}\" isn't a usable file name (one line, at most {MAX_NAME_BYTES} bytes)."));
        }
        let key = n.to_lowercase();
        if seen.contains(&key) {
            return Err(format!("\"{shown}\" is named twice."));
        }
        seen.push(key);
    }
    Ok(())
}

/// The stored file's extension, lowercased at filing.
pub fn ext_of(a: &Asset) -> &str {
    a.file.rsplit_once('.').map_or("", |(_, e)| e)
}

/// A title as a file name's stem: lowercase letters and digits, dashes between, at most 80.
pub fn slug(title: &str) -> String {
    let mut out = String::new();
    for c in title.chars().flat_map(char::to_lowercase) {
        if c.is_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let out: String = out.trim_matches('-').chars().take(80).collect();
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { "asset".to_string() } else { out }
}

/// Names from titles: `slug.ext`, and with `numbered` (a collection, in order) a two-digit (or
/// wider) order in front, `01-hero.png`. Unique within the batch in any case (two variants titled
/// "Hero" are `hero.png` and `hero (2).png`), so Export… with Replace never writes one of its own
/// files over another.
pub fn derived_names(assets: &[Asset], numbered: bool) -> Vec<String> {
    let width = assets.len().to_string().len().max(2);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    assets
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let stem = slug(&a.title);
            let ext = ext_of(a);
            let base = if ext.is_empty() { stem } else { format!("{stem}.{ext}") };
            let name = if numbered { format!("{:0width$}-{base}", i + 1) } else { base };
            let mut n = 1;
            while seen.contains(&suffixed(&name, n).to_lowercase()) {
                n += 1;
            }
            let unique = suffixed(&name, n);
            seen.insert(unique.to_lowercase());
            unique
        })
        .collect()
}

/// Names the caller gave, one per asset and checked; a name with no extension takes the stored
/// file's.
pub fn given_names(assets: &[Asset], names: &[String]) -> Result<Vec<String>, String> {
    if names.len() != assets.len() {
        return Err(format!("names has {} names for {} assets: give one each, in order, or none.", names.len(), assets.len()));
    }
    let out: Vec<String> = names
        .iter()
        .zip(assets)
        .map(|(n, a)| {
            let n = n.trim();
            match ext_of(a) {
                e if !e.is_empty() && !n.contains('.') => format!("{n}.{e}"),
                _ => n.to_string(),
            }
        })
        .collect();
    check_names(&out)?;
    Ok(out)
}

/// `name` with ` (n)` before its extension; 1 is the name itself.
pub fn suffixed(name: &str, n: u32) -> String {
    if n <= 1 {
        return name.to_string();
    }
    match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => format!("{stem} ({n}).{ext}"),
        _ => format!("{name} ({n})"),
    }
}

enum Place {
    Done,
    Exists,
}

/// The folder, opened without following a link at its last part, its descriptor's real path
/// checked against `dir` as it was checked (already canonical, so never canonicalized again: a
/// fresh canonicalize would follow a part swapped for a link just as the open did): a folder or
/// any folder above it swapped for a link after `resolve_target` is refused.
#[cfg(unix)]
fn open_dir(dir: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    let f = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?;
    if let Some(real) = super::ingest::fd_path(&f)? {
        if canon(&real) != dir {
            return Err(format!("{} changed while it was being checked: nothing was written.", dir.display()));
        }
    }
    Ok(f)
}

#[cfg(unix)]
fn cname(name: &str) -> Result<std::ffi::CString, String> {
    std::ffi::CString::new(name).map_err(|_| format!("\"{name}\" can't be a file name."))
}

/// One exclusive create of `name` in the open folder, a copy of `src` at 0644.
#[cfg(unix)]
fn place(dir: &File, src: &File, name: &str) -> Result<Place, String> {
    use std::os::unix::io::{AsRawFd, FromRawFd};
    let c = cname(name)?;
    #[cfg(target_os = "macos")]
    {
        // SAFETY: both descriptors are open; c is NUL-terminated.
        let r = unsafe { libc::fclonefileat(src.as_raw_fd(), dir.as_raw_fd(), c.as_ptr(), 0) };
        if r == 0 {
            // A clone carries the stored file's 0444.
            // SAFETY: dir is open; c is NUL-terminated.
            let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC) };
            if fd >= 0 {
                // SAFETY: fd was just opened and is owned here.
                let f = unsafe { File::from_raw_fd(fd) };
                // SAFETY: f's descriptor is open.
                unsafe { libc::fchmod(f.as_raw_fd(), 0o644) };
            }
            return Ok(Place::Done);
        }
        if std::io::Error::last_os_error().raw_os_error() == Some(libc::EEXIST) {
            return Ok(Place::Exists);
        }
    }
    // SAFETY: dir is open; c is NUL-terminated; the mode is passed as the variadic third argument.
    let fd = unsafe { libc::openat(dir.as_raw_fd(), c.as_ptr(), libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC, 0o644 as libc::c_uint) };
    if fd < 0 {
        let e = std::io::Error::last_os_error();
        return if e.raw_os_error() == Some(libc::EEXIST) { Ok(Place::Exists) } else { Err(format!("{name}: {e}")) };
    }
    // SAFETY: fd was just opened and is owned here.
    let mut out = unsafe { File::from_raw_fd(fd) };
    let copied = (|| -> std::io::Result<()> {
        use std::io::{Seek, SeekFrom};
        let mut input = src;
        input.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut input, &mut out)?;
        out.sync_all()
    })();
    // The umask may have narrowed it.
    // SAFETY: out's descriptor is open.
    unsafe { libc::fchmod(out.as_raw_fd(), 0o644) };
    if let Err(e) = copied {
        drop(out);
        // SAFETY: dir is open; c is NUL-terminated; unlinkat doesn't follow a link at the leaf.
        unsafe { libc::unlinkat(dir.as_raw_fd(), c.as_ptr(), 0) };
        return Err(format!("{name}: {e}"));
    }
    Ok(Place::Done)
}

#[cfg(not(unix))]
fn open_dir(dir: &Path) -> Result<PathBuf, String> {
    if fs::symlink_metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?.file_type().is_symlink() {
        return Err(format!("{} changed while it was being checked: nothing was written.", dir.display()));
    }
    Ok(dir.to_path_buf())
}

#[cfg(not(unix))]
fn place(dir: &Path, src: &File, name: &str) -> Result<Place, String> {
    use std::io::{Seek, SeekFrom};
    let dst = dir.join(name);
    let mut out = match fs::OpenOptions::new().write(true).create_new(true).open(&dst) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(Place::Exists),
        Err(e) => return Err(format!("{name}: {e}")),
    };
    let copied = (|| -> std::io::Result<()> {
        let mut input = src;
        input.seek(SeekFrom::Start(0))?;
        std::io::copy(&mut input, &mut out)?;
        out.sync_all()
    })();
    if let Err(e) = copied {
        drop(out);
        let _ = fs::remove_file(&dst);
        return Err(format!("{name}: {e}"));
    }
    Ok(Place::Done)
}

fn open_stored(p: &Path) -> Result<File, String> {
    let mut o = fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    o.open(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// A run that stopped partway: the names it wrote before the failure, in order (the caller logs
/// them: they are in the folder), and the failure.
#[derive(Debug)]
pub struct Stopped {
    pub written: Vec<String>,
    pub error: String,
}

impl From<String> for Stopped {
    fn from(error: String) -> Self {
        Stopped { written: Vec::new(), error }
    }
}

/// Writes each stored file into `dir` under its name as a new file: a name taken (before or
/// meanwhile) gets ` (2)`, ` (3)` … up to 99. Answers the names written, in order. Nothing is
/// ever overwritten.
pub fn deliver_files(dir: &Path, items: &[(PathBuf, String)]) -> Result<Vec<String>, Stopped> {
    let d = open_dir(dir)?;
    let mut written: Vec<String> = Vec::new();
    for (src, name) in items {
        let res = open_stored(src).and_then(|s| {
            for n in 1..=MAX_SUFFIX {
                let candidate = suffixed(name, n);
                if let Place::Done = place(&d, &s, &candidate)? {
                    return Ok(candidate);
                }
            }
            Err(format!("\"{name}\" and \"{}\" to \"{}\" are all taken in {}.", suffixed(name, 2), suffixed(name, MAX_SUFFIX), dir.display()))
        });
        match res {
            Ok(n) => written.push(n),
            Err(e) => return Err(stopped(written, e)),
        }
    }
    Ok(written)
}

fn stopped(written: Vec<String>, e: String) -> Stopped {
    let error = if written.is_empty() { e } else { format!("{e} (written before it: {})", written.join(", ")) };
    Stopped { written, error }
}

/// The human's Export… with Replace: each file goes to a hidden temp name in `dir`, then is
/// renamed over `name`, so a reader sees the old file or the new one, never half of one.
pub fn export_replacing(dir: &Path, items: &[(PathBuf, String)]) -> Result<Vec<String>, Stopped> {
    let d = open_dir(dir)?;
    let mut written: Vec<String> = Vec::new();
    for (src, name) in items {
        let tmp = {
            let t = format!(".{name}{TMP_TAIL}");
            if t.len() <= MAX_NAME_BYTES { t } else { format!(".maiterm-{}{TMP_TAIL}", &uuid::Uuid::new_v4().simple().to_string()[..8]) }
        };
        let res = open_stored(src).and_then(|s| {
            if let Place::Exists = place(&d, &s, &tmp)? {
                // A crash's leftover: ours to remove, then once more.
                remove_in(&d, dir, &tmp)?;
                if let Place::Exists = place(&d, &s, &tmp)? {
                    return Err(format!("{tmp} keeps reappearing in {}.", dir.display()));
                }
            }
            rename_in(&d, dir, &tmp, name).inspect_err(|_| {
                let _ = remove_in(&d, dir, &tmp);
            })
        });
        match res {
            Ok(()) => written.push(name.clone()),
            Err(e) => return Err(stopped(written, e)),
        }
    }
    Ok(written)
}

#[cfg(unix)]
fn remove_in(d: &File, dir: &Path, name: &str) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let c = cname(name)?;
    // SAFETY: d is open; c is NUL-terminated.
    if unsafe { libc::unlinkat(d.as_raw_fd(), c.as_ptr(), 0) } != 0 {
        return Err(format!("{}: {}", dir.join(name).display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(unix)]
fn rename_in(d: &File, dir: &Path, from: &str, to: &str) -> Result<(), String> {
    use std::os::unix::io::AsRawFd;
    let (f, t) = (cname(from)?, cname(to)?);
    // SAFETY: d is open for both names; both are NUL-terminated. A link at `to` is replaced, never followed.
    if unsafe { libc::renameat(d.as_raw_fd(), f.as_ptr(), d.as_raw_fd(), t.as_ptr()) } != 0 {
        return Err(format!("{}: {}", dir.join(to).display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(unix))]
fn remove_in(_d: &Path, dir: &Path, name: &str) -> Result<(), String> {
    fs::remove_file(dir.join(name)).map_err(|e| format!("{}: {e}", dir.join(name).display()))
}

#[cfg(not(unix))]
fn rename_in(_d: &Path, dir: &Path, from: &str, to: &str) -> Result<(), String> {
    fs::rename(dir.join(from), dir.join(to)).map_err(|e| format!("{}: {e}", dir.join(to).display()))
}

/// One delivery or export, as `logs/deliveries.jsonl` keeps it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeliveryItem {
    pub asset: String,
    pub name: String,
}

pub fn log_delivery(paths: &HivePaths, by: &str, folder: &Path, items: &[DeliveryItem]) {
    let line = json!({ "at": crate::commands::workspace::iso_now(), "by": by, "folder": folder, "items": items });
    if let Err(e) = crate::hive::queue::append_line(&paths.delivery_log(), &line) {
        log::warn!("hive: delivery into {} not logged: {e}", folder.display());
    }
}

/// A run's line, whole or partway (a stopped run's files are in the folder all the same), then
/// its names or its failure. A run that wrote nothing leaves no line.
pub fn log_run(paths: &HivePaths, by: &str, folder: &Path, assets: &[Asset], run: Result<Vec<String>, Stopped>) -> Result<Vec<String>, String> {
    let written = match &run {
        Ok(w) => w,
        Err(s) => &s.written,
    };
    if run.is_ok() || !written.is_empty() {
        let items: Vec<DeliveryItem> = assets.iter().zip(written).map(|(a, n)| DeliveryItem { asset: a.id.clone(), name: n.clone() }).collect();
        log_delivery(paths, by, folder, &items);
    }
    run.map_err(|s| s.error)
}

#[cfg(test)]
mod tests;
