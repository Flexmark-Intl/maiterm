//! Filing a file into the gallery (docs/hive.md §10.2, §10.3): which paths a caller may file
//! from, and the copy. The race between checking a path and copying it is closed by checking the
//! open file's own real path and copying from that same descriptor, never re-opening by path
//! (plan D18). The rule prevents accidents, not a determined agent (§10.3).

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::model::*;

/// One asset's ceiling (plan D21).
pub const MAX_ASSET_BYTES: u64 = 8 << 30;

/// A file `authorize` opened and checked: copy from `file`, never from the path again.
#[derive(Debug)]
pub struct Source {
    pub canonical: PathBuf,
    pub file: File,
    pub bytes: u64,
    /// Lowercased, `[a-z0-9]{1,8}`, or empty.
    pub ext: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Stored {
    /// Its name under `files/`.
    pub file: String,
    pub sha256: String,
    pub bytes: u64,
}

/// Opens `path` (relative to `base`) for filing when it is a regular file of at most 8 GiB
/// under one of `roots` and not in the gallery itself. Containment is by component on canonical
/// paths, checked again on the open descriptor's real path.
pub fn authorize(path: &Path, base: &Path, roots: &[PathBuf], gallery: &Path) -> Result<Source, String> {
    let joined = if path.is_absolute() { path.to_path_buf() } else { base.join(path) };
    let canonical = crate::share::canon(&fs::canonicalize(&joined).map_err(|e| format!("{}: {e}", joined.display()))?);
    let roots: Vec<PathBuf> = roots.iter().map(|r| crate::share::canon(r)).collect();
    let gallery = crate::share::canon(gallery);
    contained(&canonical, &roots, &gallery)?;
    // Windows refuses to open a folder ("access denied"); say what it is. The descriptor's own
    // metadata below still decides.
    if fs::metadata(&canonical).is_ok_and(|m| m.is_dir()) {
        return Err(format!("{} isn't a file.", canonical.display()));
    }
    let file = open_no_follow(&canonical)?;
    let real = match fd_path(&file)? {
        Some(p) => p,
        None => canonical.clone(),
    };
    contained(&real, &roots, &gallery)?;
    let meta = file.metadata().map_err(|e| format!("{}: {e}", canonical.display()))?;
    if !meta.is_file() {
        return Err(format!("{} isn't a file.", canonical.display()));
    }
    if meta.len() > MAX_ASSET_BYTES {
        return Err(format!("{} is over the gallery's 8 GiB limit for one asset.", canonical.display()));
    }
    let ext = canonical.extension().map(|e| e.to_string_lossy().to_lowercase()).filter(|e| is_ext(e)).unwrap_or_default();
    Ok(Source { canonical, file, bytes: meta.len(), ext })
}

fn contained(p: &Path, roots: &[PathBuf], gallery: &Path) -> Result<(), String> {
    if p.starts_with(gallery) {
        return Err(format!("{} is in the gallery already.", p.display()));
    }
    if !roots.iter().any(|r| p.starts_with(r)) {
        return Err(format!("{} isn't in a folder this caller may file from.", p.display()));
    }
    Ok(())
}

fn is_ext(e: &str) -> bool {
    (1..=8).contains(&e.len()) && e.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

/// `O_NOFOLLOW` refuses a leaf swapped for a link after the canonical check; `O_NONBLOCK` keeps a
/// FIFO from hanging the open (fstat refuses it after).
#[cfg(unix)]
fn open_no_follow(p: &Path) -> Result<File, String> {
    use std::os::unix::fs::OpenOptionsExt;
    fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK).open(p).map_err(|e| {
        if e.raw_os_error() == Some(libc::ELOOP) {
            format!("{} is a link.", p.display())
        } else {
            format!("{}: {e}", p.display())
        }
    })
}

#[cfg(not(unix))]
fn open_no_follow(p: &Path) -> Result<File, String> {
    if fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(format!("{} is a link.", p.display()));
    }
    File::open(p).map_err(|e| format!("{}: {e}", p.display()))
}

/// The open file's own path, as the kernel has it. `None` where the platform can't say.
#[cfg(target_os = "macos")]
pub(crate) fn fd_path(f: &File) -> Result<Option<PathBuf>, String> {
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::io::AsRawFd;
    let mut buf = vec![0u8; libc::PATH_MAX as usize];
    // SAFETY: F_GETPATH writes at most MAXPATHLEN (== PATH_MAX) bytes, NUL included, into buf.
    let r = unsafe { libc::fcntl(f.as_raw_fd(), libc::F_GETPATH, buf.as_mut_ptr()) };
    if r == -1 {
        return Err(format!("the opened file's path couldn't be read: {}", std::io::Error::last_os_error()));
    }
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    Ok(Some(PathBuf::from(std::ffi::OsStr::from_bytes(&buf[..n]))))
}

#[cfg(target_os = "linux")]
pub(crate) fn fd_path(f: &File) -> Result<Option<PathBuf>, String> {
    use std::os::unix::io::AsRawFd;
    fs::read_link(format!("/proc/self/fd/{}", f.as_raw_fd())).map(Some).map_err(|e| format!("the opened file's path couldn't be read: {e}"))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
pub(crate) fn fd_path(_f: &File) -> Result<Option<PathBuf>, String> {
    Ok(None)
}

/// Stores `src` as `files_dir/<id>.<ext>`: an APFS clone where it can be, else an exclusive
/// create and a copy; hashed from the stored bytes; read-only, so an edit in place fails (§10.2).
/// Never overwrites: a second store under one id fails.
pub fn store_file(src: &Source, files_dir: &Path, id: &str) -> Result<Stored, String> {
    if !is_asset_id(id) {
        return Err(format!("\"{id}\" isn't an asset id"));
    }
    let name = if src.ext.is_empty() { id.to_string() } else { format!("{id}.{}", src.ext) };
    let dst = files_dir.join(&name);
    // Set once this call has made `dst`: only then is it this call's to remove on a failure.
    let mut created = false;
    let res = (|| {
        let (sha256, bytes) = match clone_into(src, files_dir, &name)? {
            true => {
                created = true;
                clear_flags(&dst)?;
                hash_file(&dst)?
            }
            false => {
                let out = create_new(&dst)?;
                created = true;
                copy_into(src, out, &dst)?
            }
        };
        if bytes > MAX_ASSET_BYTES {
            return Err(format!("{} grew past the gallery's 8 GiB limit while it was filed.", src.canonical.display()));
        }
        read_only(&dst)?;
        Ok(Stored { file: name.clone(), sha256, bytes })
    })();
    if res.is_err() && created {
        discard(&dst);
    }
    res
}

/// Removes a half-made store, clearing any flag first: an immutable leftover would be an orphan
/// in `files/` that no row records and nothing can delete.
fn discard(dst: &Path) {
    let _ = clear_flags(dst);
    if let Err(e) = fs::remove_file(dst) {
        log::warn!("hive gallery: couldn't remove the failed store {}: {e}", dst.display());
    }
}

/// `fclonefileat` carries the source's BSD flags (not its ACLs), so a `uchg` or `uappnd` source
/// would make a clone nothing can chmod, delete or move to the trash. The clone is ours: clear
/// them all, and fail the store when that's refused.
#[cfg(target_os = "macos")]
fn clear_flags(p: &Path) -> Result<(), String> {
    use std::os::macos::fs::MetadataExt;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    let f = fs::OpenOptions::new().read(true).custom_flags(libc::O_NOFOLLOW).open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    if f.metadata().map_err(|e| format!("{}: {e}", p.display()))?.st_flags() == 0 {
        return Ok(());
    }
    // SAFETY: f is open for the call.
    if unsafe { libc::fchflags(f.as_raw_fd(), 0) } != 0 {
        return Err(format!("{}: its file flags couldn't be cleared: {}", p.display(), std::io::Error::last_os_error()));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn clear_flags(_p: &Path) -> Result<(), String> {
    Ok(())
}

const EXISTS: &str = "Already stored:";

/// `fclonefileat` from the checked descriptor: an exclusive create of an instant copy. False
/// when the volume can't clone (another filesystem, another volume), and for a flagged source:
/// the copy carries no flags, and the caller clears any set between this check and the clone.
#[cfg(target_os = "macos")]
fn clone_into(src: &Source, files_dir: &Path, name: &str) -> Result<bool, String> {
    use std::os::macos::fs::MetadataExt;
    use std::os::unix::io::AsRawFd;
    if src.file.metadata().map_err(|e| format!("{}: {e}", src.canonical.display()))?.st_flags() != 0 {
        return Ok(false);
    }
    let dir = File::open(files_dir).map_err(|e| format!("{}: {e}", files_dir.display()))?;
    let cname = std::ffi::CString::new(name).map_err(|e| e.to_string())?;
    // SAFETY: both descriptors are open for the call; cname is NUL-terminated.
    let r = unsafe { libc::fclonefileat(src.file.as_raw_fd(), dir.as_raw_fd(), cname.as_ptr(), 0) };
    if r == 0 {
        return Ok(true);
    }
    let e = std::io::Error::last_os_error();
    match e.raw_os_error() {
        Some(libc::EEXIST) => Err(format!("{EXISTS} {name}")),
        _ => Ok(false),
    }
}

#[cfg(not(target_os = "macos"))]
fn clone_into(_src: &Source, _files_dir: &Path, _name: &str) -> Result<bool, String> {
    Ok(false)
}

fn create_new(dst: &Path) -> Result<File, String> {
    let mut o = fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    o.open(dst).map_err(|e| {
        if e.kind() == std::io::ErrorKind::AlreadyExists {
            format!("{EXISTS} {}", dst.display())
        } else {
            format!("{}: {e}", dst.display())
        }
    })
}

/// Copies from the checked descriptor, hashing as it goes; stops one byte past the limit.
fn copy_into(src: &Source, mut out: File, dst: &Path) -> Result<(String, u64), String> {
    let mut input = &src.file;
    input.seek(SeekFrom::Start(0)).map_err(|e| format!("{}: {e}", src.canonical.display()))?;
    let mut input = input.take(MAX_ASSET_BYTES + 1);
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut n: u64 = 0;
    loop {
        let k = input.read(&mut buf).map_err(|e| format!("{}: {e}", src.canonical.display()))?;
        if k == 0 {
            break;
        }
        h.update(&buf[..k]);
        out.write_all(&buf[..k]).map_err(|e| format!("{}: {e}", dst.display()))?;
        n += k as u64;
    }
    out.sync_all().map_err(|e| format!("{}: {e}", dst.display()))?;
    Ok((hex(&h.finalize()), n))
}

/// SHA-256 and length of a file, streamed.
pub fn hash_file(p: &Path) -> Result<(String, u64), String> {
    let mut f = File::open(p).map_err(|e| format!("{}: {e}", p.display()))?;
    let mut h = Sha256::new();
    let n = std::io::copy(&mut f, &mut h).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok((hex(&h.finalize()), n))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|b| format!("{b:02x}")).collect()
}

fn read_only(p: &Path) -> Result<(), String> {
    #[cfg(unix)]
    let perm = {
        use std::os::unix::fs::PermissionsExt;
        fs::Permissions::from_mode(0o444)
    };
    #[cfg(not(unix))]
    let perm = {
        let mut perm = fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?.permissions();
        perm.set_readonly(true);
        perm
    };
    fs::set_permissions(p, perm).map_err(|e| format!("{}: {e}", p.display()))
}

/// The kind a file's extension suggests; a caller's own `kind` wins over it.
pub fn kind_for(ext: &str) -> &'static str {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "avif" | "heic" | "heif" | "bmp" | "tif" | "tiff" | "svg" => KIND_IMAGE,
        "mp4" | "m4v" | "mov" | "webm" | "mkv" | "avi" => KIND_VIDEO,
        "mp3" | "wav" | "m4a" | "aac" | "flac" | "ogg" | "oga" | "opus" | "aif" | "aiff" => KIND_AUDIO,
        "pdf" | "txt" | "md" | "html" | "htm" | "doc" | "docx" | "ppt" | "pptx" | "key" | "pages" | "xls" | "xlsx" | "numbers" | "csv"
        | "json" | "rtf" | "epub" => KIND_DOCUMENT,
        "glb" | "gltf" | "obj" | "fbx" | "stl" | "usdz" | "ply" | "3mf" | "blend" => KIND_MODEL3D,
        _ => KIND_OTHER,
    }
}

/// What the file is served as. Never `text/html`: every document but a PDF is opaque bytes, so
/// an agent's page can't run in the webview (plan D7).
pub fn mime_for(ext: &str) -> &'static str {
    match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "avif" => "image/avif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        "bmp" => "image/bmp",
        "tif" | "tiff" => "image/tiff",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "m4v" => "video/x-m4v",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "avi" => "video/x-msvideo",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" => "audio/mp4",
        "aac" => "audio/aac",
        "flac" => "audio/flac",
        "ogg" | "oga" => "audio/ogg",
        "opus" => "audio/opus",
        "aif" | "aiff" => "audio/aiff",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Scratch {
        base: PathBuf,
        root: PathBuf,
        gallery: PathBuf,
        files: PathBuf,
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            for e in [&self.root, &self.files].into_iter().flat_map(|d| fs::read_dir(d).into_iter().flatten().flatten()) {
                #[cfg(target_os = "macos")]
                set_flags(&e.path(), 0);
            }
            for e in fs::read_dir(&self.files).into_iter().flatten().flatten() {
                let mut p = fs::metadata(e.path()).unwrap().permissions();
                #[allow(clippy::permissions_set_readonly_false)]
                p.set_readonly(false);
                let _ = fs::set_permissions(e.path(), p);
            }
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    fn scratch() -> Scratch {
        let t = std::env::temp_dir().join(format!("hive-gallery-ingest-{}", uuid::Uuid::new_v4()));
        let base = crate::share::canon(&{
            fs::create_dir_all(&t).unwrap();
            t
        });
        let (root, gallery) = (base.join("scratch"), base.join("gallery"));
        let files = gallery.join("files");
        for d in [&root, &files] {
            fs::create_dir_all(d).unwrap();
        }
        Scratch { base, root, gallery, files }
    }

    fn refused(s: &Scratch, p: &Path) -> String {
        authorize(p, &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap_err()
    }

    #[test]
    fn a_file_under_a_root_opens_by_absolute_or_relative_path() {
        let s = scratch();
        fs::create_dir_all(s.root.join("out")).unwrap();
        fs::write(s.root.join("out/Hero.PNG"), b"png").unwrap();
        let a = authorize(&s.root.join("out/Hero.PNG"), Path::new("/nowhere"), std::slice::from_ref(&s.root), &s.gallery).unwrap();
        assert_eq!((a.bytes, a.ext.as_str()), (3, "png"));
        assert_eq!(a.canonical, s.root.join("out/Hero.PNG"));
        let b = authorize(Path::new("out/../out/Hero.PNG"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap();
        assert_eq!(b.canonical, a.canonical, "a relative path is joined to the caller's cwd");
        fs::write(s.root.join("notes.tar.verylongext"), b"x").unwrap();
        assert_eq!(authorize(Path::new("notes.tar.verylongext"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap().ext, "");
    }

    #[test]
    fn outside_a_prefix_sibling_a_folder_and_the_gallery_are_refused() {
        let s = scratch();
        let sibling = s.base.join("scratch2");
        fs::create_dir_all(&sibling).unwrap();
        fs::write(sibling.join("a.png"), b"x").unwrap();
        assert!(refused(&s, &sibling.join("a.png")).contains("isn't in a folder"), "a shared prefix isn't containment");
        fs::write(s.base.join("b.png"), b"x").unwrap();
        assert!(refused(&s, &s.base.join("b.png")).contains("isn't in a folder"));
        fs::create_dir_all(s.root.join("dir")).unwrap();
        assert!(refused(&s, &s.root.join("dir")).contains("isn't a file"));
        assert!(refused(&s, &s.root.join("missing.png")).contains("missing.png"));
        fs::write(s.files.join("a-0123456789ab.png"), b"x").unwrap();
        let roots = [s.base.clone()];
        let e = authorize(&s.files.join("a-0123456789ab.png"), &s.root, &roots, &s.gallery).unwrap_err();
        assert!(e.contains("in the gallery already"), "{e}");
    }

    #[cfg(unix)]
    #[test]
    fn links_out_of_a_root_are_refused_at_the_leaf_and_midway() {
        use std::os::unix::fs::symlink;
        let s = scratch();
        let outside = s.base.join("outside");
        fs::create_dir_all(&outside).unwrap();
        fs::write(outside.join("secret.png"), b"x").unwrap();
        symlink(outside.join("secret.png"), s.root.join("leaf.png")).unwrap();
        assert!(refused(&s, &s.root.join("leaf.png")).contains("isn't in a folder"), "the leaf link resolves outside");
        symlink(&outside, s.root.join("mid")).unwrap();
        assert!(refused(&s, &s.root.join("mid/secret.png")).contains("isn't in a folder"), "a link midway resolves outside");
        // A link to a file inside the root is fine: canonical is what's filed.
        fs::write(s.root.join("real.png"), b"y").unwrap();
        symlink(s.root.join("real.png"), s.root.join("alias.png")).unwrap();
        assert_eq!(authorize(&s.root.join("alias.png"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap().canonical, s.root.join("real.png"));
        assert!(open_no_follow(&s.root.join("alias.png")).unwrap_err().contains("is a link"), "a raced leaf link is refused by the open");
    }

    #[cfg(unix)]
    #[test]
    fn an_oversized_file_and_a_fifo_are_refused() {
        let s = scratch();
        let big = File::create(s.root.join("big.mov")).unwrap();
        big.set_len(MAX_ASSET_BYTES + 1).unwrap(); // sparse: no disk spent
        assert!(refused(&s, &s.root.join("big.mov")).contains("8 GiB"));
        let fifo = s.root.join("pipe");
        let c = std::ffi::CString::new(fifo.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert!(refused(&s, &fifo).contains("isn't a file"), "opened without blocking, then refused");
    }

    #[test]
    fn a_stored_file_is_hashed_read_only_and_never_overwritten() {
        let s = scratch();
        let body = b"hello gallery".repeat(1000);
        fs::write(s.root.join("clip.MP4"), &body).unwrap();
        let src = authorize(Path::new("clip.MP4"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap();
        let id = mint(ASSET_PREFIX);
        let st = store_file(&src, &s.files, &id).unwrap();
        assert_eq!(st.file, format!("{id}.mp4"));
        assert_eq!(st.bytes, body.len() as u64);
        assert_eq!(st.sha256, hex(&Sha256::digest(&body)));
        let stored = s.files.join(&st.file);
        assert_eq!(fs::read(&stored).unwrap(), body);
        assert!(fs::metadata(&stored).unwrap().permissions().readonly());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(&stored).unwrap().permissions().mode() & 0o777, 0o444);
        }
        assert!(fs::OpenOptions::new().write(true).open(&stored).is_err(), "an edit in place fails");

        fs::write(s.root.join("other.mp4"), b"different").unwrap();
        let other = authorize(Path::new("other.mp4"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap();
        let e = store_file(&other, &s.files, &id).unwrap_err();
        assert!(e.starts_with(EXISTS), "{e}");
        assert_eq!(fs::read(&stored).unwrap(), body, "the first store is untouched");
        assert!(store_file(&other, &s.files, "../escape").is_err());
    }

    #[cfg(target_os = "macos")]
    fn set_flags(p: &Path, flags: libc::c_uint) {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        // SAFETY: a NUL-terminated path.
        unsafe { libc::chflags(c.as_ptr(), flags) };
    }

    #[cfg(target_os = "macos")]
    fn flags_of(p: &Path) -> u32 {
        use std::os::macos::fs::MetadataExt;
        fs::symlink_metadata(p).unwrap().st_flags()
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn an_immutable_source_stores_an_unflagged_file() {
        let s = scratch();
        fs::write(s.root.join("x.png"), b"locked").unwrap();
        set_flags(&s.root.join("x.png"), libc::UF_IMMUTABLE | libc::UF_APPEND);
        assert_ne!(flags_of(&s.root.join("x.png")), 0);
        let src = authorize(Path::new("x.png"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap();
        let st = store_file(&src, &s.files, &mint(ASSET_PREFIX)).unwrap();
        let stored = s.files.join(&st.file);
        assert_eq!(flags_of(&stored), 0, "a clone would have carried uchg");
        assert_eq!(fs::read(&stored).unwrap(), b"locked");
        assert!(fs::metadata(&stored).unwrap().permissions().readonly());
        fs::remove_file(&stored).expect("the stored file can be deleted");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_flag_set_after_the_check_is_cleared_and_a_failed_store_leaves_nothing() {
        let s = scratch();
        let p = s.files.join("raced.png");
        fs::write(&p, b"x").unwrap();
        set_flags(&p, libc::UF_IMMUTABLE);
        clear_flags(&p).unwrap();
        assert_eq!(flags_of(&p), 0);
        set_flags(&p, libc::UF_IMMUTABLE);
        assert!(fs::remove_file(&p).is_err(), "uchg refuses a plain delete");
        discard(&p);
        assert!(!p.exists(), "the cleanup clears the flag before it deletes");
    }

    #[test]
    fn the_copy_fallback_hashes_what_it_wrote() {
        let s = scratch();
        fs::write(s.root.join("a.wav"), b"RIFF....").unwrap();
        let src = authorize(Path::new("a.wav"), &s.root, std::slice::from_ref(&s.root), &s.gallery).unwrap();
        let dst = s.files.join("copy.wav");
        let (sha, n) = copy_into(&src, create_new(&dst).unwrap(), &dst).unwrap();
        assert_eq!((sha, n), hash_file(&dst).unwrap());
        assert!(create_new(&dst).unwrap_err().starts_with(EXISTS), "exclusive create");
    }

    #[test]
    fn kinds_and_mimes_never_serve_html() {
        assert_eq!((kind_for("png"), mime_for("png")), (KIND_IMAGE, "image/png"));
        assert_eq!((kind_for("mov"), mime_for("mov")), (KIND_VIDEO, "video/quicktime"));
        assert_eq!((kind_for("flac"), mime_for("flac")), (KIND_AUDIO, "audio/flac"));
        assert_eq!((kind_for("pdf"), mime_for("pdf")), (KIND_DOCUMENT, "application/pdf"));
        assert_eq!((kind_for("glb"), kind_for("zip")), (KIND_MODEL3D, KIND_OTHER));
        for doc in ["html", "htm", "md", "txt", "docx", "json"] {
            assert_eq!(mime_for(doc), "application/octet-stream", "{doc}");
        }
    }
}
