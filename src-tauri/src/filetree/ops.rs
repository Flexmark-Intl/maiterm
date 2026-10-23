//! File operations from the tree (docs/file-tree.md §10): create, rename, move, copy,
//! duplicate, import, trash, permanent delete and restore.
//!
//! Every rule is enforced here, in Rust, whatever the frontend checked first:
//! - **Containment.** Every rel is checked component by component and joined to the canonical
//!   root; a folder on the way that is a symlink must resolve inside the root; the root itself
//!   is never acted on. Only an import's sources may be outside, never its destination. This
//!   is the first path scope any fs command in the app has.
//! - **Nothing is followed.** A recursive copy, a move's copy fallback and a delete see a
//!   symlink as a link: it is recreated or removed, never walked into.
//! - **Nothing is clobbered.** Creates use `create_new`/`create_dir`; renames ask the OS for a
//!   rename that refuses an existing target, so a file appearing between a check and the
//!   rename is never replaced. A case-only rename on a volume that folds case hops through a
//!   temporary name.
//! - **Trash runs on one thread** (`TrashWorker`), and a failed trash answers `no_trash`:
//!   never a silent permanent delete.
//! - **A permanent delete takes a ticket** minted by `ft_prepare_delete` for exactly that set
//!   of rels, single use, 60 s.
//! - **maiTerm's own guards** (§10.3): a folder a live shell, agent or service is working in
//!   is never moved, renamed, trashed or deleted from here (relocate is the only path that
//!   suspends agents first), and an agent's in-flight work needs a second confirmation.

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use parking_lot::Mutex;

use super::paths::{is_at_or_under, join_rel, name_key, split_parent, split_rel, strip_root};
use super::types::{DeleteTicket, FileOp, Needs, OpFail, OpItem, OpResult};
use crate::util::naming::{unique_name, CopyNames};

/// How long a delete ticket is good for.
pub const TICKET_TTL: Duration = Duration::from_secs(60);

// ── Containment ──────────────────────────────────────────────────────────────────────────────

/// How a rel is used, which decides whether its last component is looked through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// The item itself is acted on (renamed, trashed, copied): a symlink there is the link, so
    /// it is never followed, and the root itself is refused.
    Item,
    /// A folder things go into: a symlink there is followed, so it must land inside the root.
    /// The root itself is allowed.
    Folder,
}

/// The native path of `rel` under `root` (canonical), refused when it could reach outside:
/// a `..`, absolute, empty or (Windows) drive-relative component, or a symlinked folder on
/// the way that resolves outside the root, compared component-wise and case-folded only where
/// the volume folds.
pub fn contained(root: &Path, fold: bool, rel: &str, usage: Use) -> Result<PathBuf, String> {
    let parts = split_rel(rel)?;
    if parts.is_empty() && usage == Use::Item {
        return Err("Files won't act on the root folder itself".into());
    }
    let mut p = root.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        p.push(part);
        if i + 1 == parts.len() && usage == Use::Item {
            break;
        }
        match std::fs::symlink_metadata(&p) {
            Ok(m) if m.file_type().is_symlink() => {
                // A dangling link can't be shown to stay inside, so it is refused like one
                // that leaves.
                let target = std::fs::canonicalize(&p).map_err(|_| format!("{rel}: a link on the way doesn't resolve"))?;
                let target = crate::share::canon(&target);
                if strip_root(&target, root, fold).is_none() {
                    return Err(format!("{rel}: a link on the way leads outside the root"));
                }
            }
            Ok(_) => {}
            // Nothing below a missing component can be a link yet.
            Err(_) => break,
        }
    }
    Ok(p)
}

/// One component of a new name, refused where the platform can't hold it. Separators, `.`,
/// `..` and NUL are already refused by `split_rel`. On Windows a reserved device name would
/// open the device rather than make a file, and a trailing dot or space is silently dropped.
fn check_part(part: &str) -> Result<(), String> {
    if cfg!(windows) {
        if part.ends_with('.') || part.ends_with(' ') {
            return Err(format!("“{part}”: a name can't end with a dot or a space on Windows"));
        }
        if part.chars().any(|c| matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') || (c as u32) < 32) {
            return Err(format!("“{part}”: a name can't contain < > : \" | ? * on Windows"));
        }
        let stem = part.split('.').next().unwrap_or("").trim_end().to_ascii_uppercase();
        let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
            || (stem.len() == 4 && (stem.starts_with("COM") || stem.starts_with("LPT")) && stem.as_bytes()[3].is_ascii_digit() && stem.as_bytes()[3] != b'0');
        if reserved {
            return Err(format!("“{part}” is a reserved name on Windows"));
        }
    }
    Ok(())
}

/// A rename's new name: exactly one component.
fn single_name(name: Option<&str>) -> Result<&str, String> {
    let name = name.filter(|n| !n.is_empty()).ok_or("A name is needed")?;
    let parts = split_rel(name)?;
    if parts.len() != 1 {
        return Err(format!("“{name}”: a name can't contain /"));
    }
    check_part(name)?;
    Ok(name)
}

/// Drop rels inside another rel of the set: trashing `a` already took `a/b`.
fn outermost(rels: &[String], fold: bool) -> Vec<String> {
    let keys: Vec<String> = rels.iter().map(|r| name_key(r, fold)).collect();
    let mut out: Vec<String> = Vec::new();
    for (i, r) in rels.iter().enumerate() {
        let inside = keys.iter().enumerate().any(|(j, k)| j != i && k != &keys[i] && is_at_or_under(&keys[i], k));
        let dup = out.iter().any(|o| name_key(o, fold) == keys[i]);
        if !inside && !dup {
            out.push(r.clone());
        }
    }
    out
}

/// A real folder (not a link to one), canonical: what a shell inside it reports as its cwd.
fn real_dir(p: &Path) -> Option<PathBuf> {
    let m = std::fs::symlink_metadata(p).ok()?;
    m.is_dir().then(|| crate::share::canon(p))
}

// ── Guards ───────────────────────────────────────────────────────────────────────────────────

/// Where a holder's working directory comes from.
#[derive(Debug, Clone)]
pub enum CwdOf {
    /// A live process: its cwd is read when the guard runs, so a `cd` a moment ago counts.
    Pid(u32),
    /// A recorded folder (an agent session's cwd, a running service's).
    Path(PathBuf),
}

/// A tab with something working in a folder.
#[derive(Debug, Clone)]
pub struct CwdHolder {
    pub tab_id: String,
    pub tab_name: String,
    pub cwd: CwdOf,
}

/// The live-cwd guard (§10.3): the tabs working in any of `dirs` (canonical real folders) or
/// below one. `holders` is asked only when a folder is involved, since gathering it reads
/// every live shell's cwd.
fn live_cwd_refusal(dirs: &[PathBuf], fold: bool, holders: &dyn Fn() -> Vec<CwdHolder>) -> Option<Needs> {
    if dirs.is_empty() {
        return None;
    }
    let mut tabs: Vec<(String, String)> = Vec::new();
    for h in holders() {
        let cwd = match &h.cwd {
            CwdOf::Pid(pid) => match crate::pty::manager::cwd_of_pid(*pid) {
                Some(c) => PathBuf::from(c),
                None => continue,
            },
            CwdOf::Path(p) => p.clone(),
        };
        if !cwd.is_absolute() {
            continue;
        }
        let cwd = crate::share::canon(&cwd);
        if dirs.iter().any(|d| strip_root(&cwd, d, fold).is_some()) && !tabs.iter().any(|(id, _)| *id == h.tab_id) {
            tabs.push((h.tab_id, h.tab_name));
        }
    }
    if tabs.is_empty() {
        return None;
    }
    let names: Vec<String> = tabs.iter().map(|(_, n)| format!("‘{n}’")).collect();
    let detail = match names.as_slice() {
        [one] => format!("Tab {one} is working in this folder. Use Move project…"),
        many => format!("Tabs {} are working in this folder. Use Move project…", many.join(", ")),
    };
    Some(Needs { code: "live_cwd".into(), detail, tabs: tabs.into_iter().map(|(id, _)| id).collect() })
}

/// The tabs whose agent has a write in flight on any of `paths`, or wrote one this turn
/// (§10.3). The ledger that knows this, `agent_files`, arrives in phase 4 (§9.1): until then no
/// tab is ever named, so `needs: agent_active` is never answered and `ack_agents` is accepted
/// but changes nothing.
fn agent_active_tabs(_paths: &[PathBuf]) -> Vec<String> {
    Vec::new()
}

fn agent_refusal(paths: &[PathBuf], ack: bool) -> Option<Needs> {
    if ack {
        return None;
    }
    let tabs = agent_active_tabs(paths);
    (!tabs.is_empty()).then(|| Needs {
        code: "agent_active".into(),
        detail: "An agent is working on this right now. Go ahead anyway?".into(),
        tabs,
    })
}

// ── Primitives ───────────────────────────────────────────────────────────────────────────────

/// A rename that never replaces what is at `to`: the OS refuses atomically, so a file that
/// appears between a check and the rename is safe. Where the volume can't (an old network
/// filesystem), a check-then-rename is the best there is.
pub fn rename_noreplace(from: &Path, to: &Path) -> io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: two NUL-terminated paths that live across the call.
        if unsafe { libc::renamex_np(f.as_ptr(), t.as_ptr(), libc::RENAME_EXCL) } == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        if !matches!(e.raw_os_error(), Some(libc::ENOTSUP) | Some(libc::EOPNOTSUPP)) {
            return Err(e);
        }
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::ffi::OsStrExt;
        const RENAME_NOREPLACE: libc::c_uint = 1;
        let f = std::ffi::CString::new(from.as_os_str().as_bytes())?;
        let t = std::ffi::CString::new(to.as_os_str().as_bytes())?;
        // SAFETY: two NUL-terminated paths that live across the call. The raw syscall, not
        // glibc's wrapper: musl has none.
        let r = unsafe { libc::syscall(libc::SYS_renameat2, libc::AT_FDCWD, f.as_ptr(), libc::AT_FDCWD, t.as_ptr(), RENAME_NOREPLACE) };
        if r == 0 {
            return Ok(());
        }
        let e = io::Error::last_os_error();
        // ENOSYS: a kernel before 3.15; EINVAL: a filesystem without the flag.
        if !matches!(e.raw_os_error(), Some(libc::ENOSYS) | Some(libc::EINVAL)) {
            return Err(e);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let wide = |p: &Path| p.as_os_str().encode_wide().chain(Some(0)).collect::<Vec<u16>>();
        let (f, t) = (wide(from), wide(to));
        // No MOVEFILE_REPLACE_EXISTING: an existing target is refused. No COPY_ALLOWED either:
        // another volume answers ERROR_NOT_SAME_DEVICE, and the caller copies and verifies.
        // SAFETY: two NUL-terminated UTF-16 paths that live across the call.
        if unsafe { windows_sys::Win32::Storage::FileSystem::MoveFileExW(f.as_ptr(), t.as_ptr(), 0) } != 0 {
            return Ok(());
        }
        return Err(io::Error::last_os_error());
    }
    #[allow(unreachable_code)]
    {
        if std::fs::symlink_metadata(to).is_ok() {
            return Err(io::Error::new(io::ErrorKind::AlreadyExists, "the destination exists"));
        }
        std::fs::rename(from, to)
    }
}

/// The rename crossed volumes: the move must copy.
fn is_cross_device(e: &io::Error) -> bool {
    #[cfg(unix)]
    {
        e.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE
        e.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = e;
        false
    }
}

/// A case-only (or, on macOS, normalization-only) rename on a volume that sees both spellings
/// as one name: the target "exists" (it is the source), so it goes through a temporary name.
fn hop_rename(from: &Path, to: &Path, rename: fn(&Path, &Path) -> io::Result<()>) -> io::Result<()> {
    let name = from.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let hop = from.with_file_name(format!(".{name}.maiterm-rename-{}", uuid::Uuid::new_v4().simple()));
    rename(from, &hop)?;
    if let Err(e) = rename(&hop, to) {
        let _ = rename(&hop, from);
        return Err(e);
    }
    Ok(())
}

/// Copy `from` to `to`, which must not exist, and on failure remove only what this copy made.
/// The caller checked `to` was free, but something can land there before the copy's first
/// create (another tab's agent, a build); that create then answers `AlreadyExists` and the
/// item belongs to someone else, so it is left exactly as it is.
fn copy_fresh(from: &Path, to: &Path) -> io::Result<()> {
    let mut made = false;
    let r = copy_tree(from, to, &mut made);
    if r.is_err() && made {
        let _ = remove_any(to);
    }
    r
}

/// Copy `from` to `to`, which must not exist. A symlink is recreated as a link, never
/// followed; a folder is walked with `symlink_metadata`, so a link inside it is copied as a
/// link too. Files are created with `create_new`. `made` turns true once `to` itself exists
/// because this call created it.
fn copy_tree(from: &Path, to: &Path, made: &mut bool) -> io::Result<()> {
    let m = std::fs::symlink_metadata(from)?;
    let ft = m.file_type();
    if ft.is_symlink() {
        copy_link(from, to)?;
        *made = true;
        Ok(())
    } else if ft.is_dir() {
        std::fs::create_dir(to)?;
        *made = true;
        for e in std::fs::read_dir(from)? {
            let e = e?;
            copy_tree(&e.path(), &to.join(e.file_name()), &mut false)?;
        }
        // Last, so a read-only folder still takes its contents.
        let _ = std::fs::set_permissions(to, m.permissions());
        Ok(())
    } else if ft.is_file() {
        let mut out = std::fs::OpenOptions::new().write(true).create_new(true).open(to)?;
        *made = true;
        io::copy(&mut std::fs::File::open(from)?, &mut out)?;
        drop(out);
        let _ = std::fs::set_permissions(to, m.permissions());
        Ok(())
    } else {
        Err(io::Error::new(io::ErrorKind::Unsupported, format!("{} isn't a file, folder or link", from.display())))
    }
}

fn copy_link(from: &Path, to: &Path) -> io::Result<()> {
    let target = std::fs::read_link(from)?;
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, to)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if std::fs::symlink_metadata(from)?.file_type().is_symlink_dir() {
            std::os::windows::fs::symlink_dir(target, to)
        } else {
            std::os::windows::fs::symlink_file(target, to)
        }
    }
}

/// Remove a file, a link (never what it points to) or a whole folder. std's `remove_dir_all`
/// doesn't follow symlinks inside the folder.
pub fn remove_any(p: &Path) -> io::Result<()> {
    let m = std::fs::symlink_metadata(p)?;
    if m.is_dir() {
        return std::fs::remove_dir_all(p);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileTypeExt;
        if m.file_type().is_symlink_dir() {
            return std::fs::remove_dir(p);
        }
    }
    std::fs::remove_file(p)
}

/// (files and links, folders, bytes) under `p`, nothing followed.
fn tally(p: &Path) -> io::Result<(u64, u64, u64)> {
    let m = std::fs::symlink_metadata(p)?;
    if !m.is_dir() {
        return Ok((1, 0, m.len()));
    }
    let mut t = (0, 1, 0);
    for e in std::fs::read_dir(p)? {
        let (f, d, b) = tally(&e?.path())?;
        t = (t.0 + f, t.1 + d, t.2 + b);
    }
    Ok(t)
}

/// A move across volumes: copy, check the copy holds as many files and bytes as the source,
/// and only then remove the source. A failed or short copy is removed and the source kept;
/// something that took the name before the copy began is someone else's and stays.
fn move_across(from: &Path, to: &Path) -> io::Result<()> {
    copy_fresh(from, to)?;
    let (a, b) = (tally(from)?, tally(to)?);
    if a.0 != b.0 || a.2 != b.2 {
        let _ = remove_any(to);
        return Err(io::Error::other(format!(
            "the copy came out different ({} files, {} bytes of {} files, {} bytes), so the original was left where it was",
            b.0, b.2, a.0, a.2
        )));
    }
    remove_any(from).map_err(|e| io::Error::other(format!("copied, but the original couldn't be removed: {e}")))
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

// ── Trash ────────────────────────────────────────────────────────────────────────────────────

/// What the trash worker drives: the system trash, or a stand-in in tests (which must never
/// fill the real Trash).
pub trait TrashBackend: Send {
    /// Move one path to the trash. A symlink goes as the link.
    fn trash(&mut self, path: &Path) -> Result<(), String>;
    /// The platform's trash can put items back (Linux and Windows; not macOS, §10.4).
    fn can_restore(&self) -> bool;
    /// Put back the newest item trashed from each path at or after its time (unix seconds).
    /// Per item: Ok, or (code, message).
    fn restore(&mut self, items: &[(PathBuf, i64)]) -> Vec<Result<(), (String, String)>>;
}

/// The `trash` crate.
pub struct SystemTrash {
    ctx: trash::TrashContext,
}

impl SystemTrash {
    pub fn new() -> Self {
        #[allow(unused_mut)]
        let mut ctx = trash::TrashContext::default();
        // NSFileManager, not the default Finder script: no automation permission prompt, no
        // sound, and it works with Finder busy. Put Back is lost either way on some systems.
        #[cfg(target_os = "macos")]
        {
            use trash::macos::{DeleteMethod, TrashContextExtMacos};
            ctx.set_delete_method(DeleteMethod::NsFileManager);
        }
        SystemTrash { ctx }
    }
}

impl TrashBackend for SystemTrash {
    fn trash(&mut self, path: &Path) -> Result<(), String> {
        self.ctx.delete(path).map_err(|e| e.to_string())
    }

    fn can_restore(&self) -> bool {
        cfg!(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))
    }

    #[cfg(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android"))))]
    fn restore(&mut self, items: &[(PathBuf, i64)]) -> Vec<Result<(), (String, String)>> {
        let listed = match trash::os_limited::list() {
            Ok(l) => l,
            Err(e) => return items.iter().map(|_| Err(("io".to_string(), e.to_string()))).collect(),
        };
        // The crate records the canonical parent, so compare the same way.
        let canon = |p: &Path| match (p.parent(), p.file_name()) {
            (Some(parent), Some(name)) => crate::share::canon(parent).join(name),
            _ => p.to_path_buf(),
        };
        items
            .iter()
            .map(|(path, since)| {
                let want = canon(path);
                // A second of slack: the time is recorded after the call, ours before it.
                let found = listed
                    .iter()
                    .filter(|it| it.time_deleted >= since - 1 && canon(&it.original_path()) == want)
                    .max_by_key(|it| it.time_deleted)
                    .cloned();
                let Some(item) = found else {
                    return Err(("not_in_trash".to_string(), "It is no longer in the trash".to_string()));
                };
                trash::os_limited::restore_all([item]).map_err(|e| match e {
                    trash::Error::RestoreCollision { .. } => ("exists".to_string(), "Something else now has its name".to_string()),
                    other => ("io".to_string(), other.to_string()),
                })
            })
            .collect()
    }

    #[cfg(not(any(windows, all(unix, not(target_os = "macos"), not(target_os = "ios"), not(target_os = "android")))))]
    fn restore(&mut self, items: &[(PathBuf, i64)]) -> Vec<Result<(), (String, String)>> {
        items.iter().map(|_| Err(("unsupported".to_string(), "This trash can't put items back: open it to drag them out".to_string()))).collect()
    }
}

enum TrashJob {
    Trash(Vec<PathBuf>, mpsc::Sender<Vec<Result<(), String>>>),
    Restore(Vec<(PathBuf, i64)>, mpsc::Sender<Vec<Result<(), (String, String)>>>),
}

/// Every trash call runs on this one thread: the crate warns of undefined behaviour from
/// concurrent mount-point queries on Linux, and two windows trashing at once would be exactly
/// that.
pub struct TrashWorker {
    tx: Mutex<mpsc::Sender<TrashJob>>,
    can_restore: bool,
}

impl TrashWorker {
    pub fn start(mut backend: Box<dyn TrashBackend>) -> Self {
        let can_restore = backend.can_restore();
        let (tx, rx) = mpsc::channel::<TrashJob>();
        let spawned = std::thread::Builder::new().name("ft-trash".into()).spawn(move || {
            // Blocks with no timeout: a tree nobody trashes from costs nothing.
            while let Ok(job) = rx.recv() {
                match job {
                    TrashJob::Trash(paths, reply) => {
                        let _ = reply.send(paths.iter().map(|p| backend.trash(p)).collect());
                    }
                    TrashJob::Restore(items, reply) => {
                        let _ = reply.send(backend.restore(&items));
                    }
                }
            }
        });
        if let Err(e) = spawned {
            // The receiver is gone with the closure: every call answers "unavailable", which
            // reads as no_trash and offers a permanent delete the human confirms.
            log::warn!("filetree: couldn't start the trash thread: {e}");
        }
        TrashWorker { tx: Mutex::new(tx), can_restore }
    }

    pub fn can_restore(&self) -> bool {
        self.can_restore
    }

    fn trash(&self, paths: Vec<PathBuf>) -> Vec<Result<(), String>> {
        let n = paths.len();
        let (reply, rx) = mpsc::channel();
        if self.tx.lock().send(TrashJob::Trash(paths, reply)).is_err() {
            return vec![Err("the trash is unavailable".into()); n];
        }
        rx.recv().unwrap_or_else(|_| vec![Err("the trash is unavailable".into()); n])
    }

    fn restore(&self, items: Vec<(PathBuf, i64)>) -> Vec<Result<(), (String, String)>> {
        let n = items.len();
        let gone = || vec![Err(("io".to_string(), "the trash is unavailable".to_string())); n];
        let (reply, rx) = mpsc::channel();
        if self.tx.lock().send(TrashJob::Restore(items, reply)).is_err() {
            return gone();
        }
        rx.recv().unwrap_or_else(|_| gone())
    }
}

// ── Delete tickets ───────────────────────────────────────────────────────────────────────────

struct Ticket {
    place: String,
    rels: Vec<String>,
    until: Instant,
}

/// Single-use tokens for a permanent delete, each bound to one hub and one exact set of rels.
/// A stray IPC can't delete anything: it has no token, and a token deletes only what the human
/// saw counted.
pub struct Tickets {
    ttl: Duration,
    map: Mutex<HashMap<String, Ticket>>,
}

impl Default for Tickets {
    fn default() -> Self {
        Tickets::with_ttl(TICKET_TTL)
    }
}

fn rel_set(rels: &[String]) -> Vec<String> {
    let mut v = rels.to_vec();
    v.sort();
    v.dedup();
    v
}

impl Tickets {
    pub fn with_ttl(ttl: Duration) -> Self {
        Tickets { ttl, map: Mutex::new(HashMap::new()) }
    }

    fn mint(&self, place: &str, rels: &[String]) -> String {
        let token = uuid::Uuid::new_v4().simple().to_string();
        let mut map = self.map.lock();
        let now = Instant::now();
        map.retain(|_, t| t.until > now);
        map.insert(token.clone(), Ticket { place: place.to_string(), rels: rel_set(rels), until: now + self.ttl });
        token
    }

    /// Whether `token` deletes exactly `rels` in `place`, without using it up: a delete that
    /// stops at a guard can come back with the same ticket after the human's second yes.
    fn check(&self, token: Option<&str>, place: &str, rels: &[String]) -> Result<(), String> {
        let token = token.ok_or("A permanent delete needs a ticket from ft_prepare_delete")?;
        let map = self.map.lock();
        let t = map.get(token).ok_or("This delete's ticket is used up or unknown: confirm it again")?;
        if t.until <= Instant::now() {
            return Err("This delete's ticket expired: confirm it again".into());
        }
        if t.place != place || t.rels != rel_set(rels) {
            return Err("This delete's ticket was for other items: confirm it again".into());
        }
        Ok(())
    }

    fn take(&self, token: Option<&str>, place: &str, rels: &[String]) -> Result<(), String> {
        self.check(token, place, rels)?;
        if let Some(t) = token {
            self.map.lock().remove(t);
        }
        Ok(())
    }
}

// ── Operations ───────────────────────────────────────────────────────────────────────────────

/// What an op runs against: one hub's root, and the app's trash, tickets and live folders.
pub struct Env<'a> {
    pub place: &'a str,
    /// Canonical.
    pub root: &'a Path,
    pub fold: bool,
    pub trash: &'a TrashWorker,
    pub tickets: &'a Tickets,
    /// Every tab working in some folder, gathered only when a folder is moved or removed.
    pub holders: &'a dyn Fn() -> Vec<CwdHolder>,
    /// `rename_noreplace`; a test forces a cross-device answer through here.
    pub rename: fn(&Path, &Path) -> io::Result<()>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Conflict {
    Fail,
    KeepBoth,
    Skip,
}

/// One item's outcome inside an op.
enum Step {
    Done(OpItem),
    Skipped(String),
    Failed(OpFail),
}

fn fail(rel: &str, code: &str, msg: impl Into<String>) -> Step {
    Step::Failed(OpFail { rel: rel.to_string(), code: code.to_string(), msg: msg.into() })
}

fn collect(steps: Vec<Step>) -> OpResult {
    let mut r = OpResult::default();
    for s in steps {
        match s {
            Step::Done(i) => r.done.push(i),
            Step::Skipped(s) => r.skipped.push(s),
            Step::Failed(f) => r.failed.push(f),
        }
    }
    r
}

fn trash_undo(rels: Vec<String>) -> Option<FileOp> {
    (!rels.is_empty()).then(|| FileOp { kind: "trash".into(), src: rels, ..Default::default() })
}

impl Env<'_> {
    fn exists(&self, p: &Path) -> bool {
        std::fs::symlink_metadata(p).is_ok()
    }

    /// The name an item lands under in `dir`, or why it doesn't.
    fn landing(&self, dir: &Path, name: &str, is_dir: bool, conflict: Conflict, fresh: bool) -> Result<String, &'static str> {
        if !fresh && !self.exists(&dir.join(name)) {
            return Ok(name.to_string());
        }
        match (conflict, fresh) {
            (_, true) | (Conflict::KeepBoth, _) => {
                unique_name(name, is_dir, CopyNames::platform(), fresh, |n| self.exists(&dir.join(n))).ok_or("exists")
            }
            (Conflict::Skip, _) => Err("skip"),
            (Conflict::Fail, _) => Err("exists"),
        }
    }

    /// The live-cwd and agent guards over the items about to be moved or removed.
    fn guards(&self, items: &[PathBuf], ack_agents: bool) -> Option<Needs> {
        let dirs: Vec<PathBuf> = items.iter().filter_map(|p| real_dir(p)).collect();
        live_cwd_refusal(&dirs, self.fold, self.holders).or_else(|| agent_refusal(items, ack_agents))
    }
}

/// Run one op. An error refuses the whole op before anything changed: a path outside the root,
/// the root itself, a bad ticket, a malformed request.
pub fn run(env: &Env, op: &FileOp) -> Result<OpResult, String> {
    let conflict = match op.conflict.as_str() {
        "" | "fail" => Conflict::Fail,
        "keep_both" => Conflict::KeepBoth,
        "skip" => Conflict::Skip,
        other => return Err(format!("Unknown conflict rule: {other}")),
    };
    match op.kind.as_str() {
        "create_file" => create(env, op, false, conflict),
        "create_dir" => create(env, op, true, conflict),
        "rename" => rename(env, op, conflict),
        "move" => move_items(env, op, conflict),
        "copy" => copy_items(env, op, conflict),
        "duplicate" => duplicate(env, op),
        "import" => import(env, op, conflict),
        "trash" => trash_items(env, op),
        "delete" => delete_items(env, op),
        "restore" => restore_items(env, op),
        other => Err(format!("Unknown file operation: {other}")),
    }
}

fn dest_folder(env: &Env, dest: Option<&str>) -> Result<(String, PathBuf), String> {
    let rel = dest.unwrap_or("").to_string();
    let native = contained(env.root, env.fold, &rel, Use::Folder)?;
    if !native.is_dir() {
        return Err(format!("{}: not a folder", if rel.is_empty() { "the root" } else { &rel }));
    }
    Ok((rel, native))
}

fn items(env: &Env, src: &[String]) -> Result<Vec<(String, PathBuf)>, String> {
    if src.is_empty() {
        return Err("No items given".into());
    }
    outermost(src, env.fold).into_iter().map(|r| contained(env.root, env.fold, &r, Use::Item).map(|p| (r, p))).collect()
}

/// New file or folder. "a/b/c.ts" makes `a` and `b` first; an existing folder on the way is
/// used (after the same containment check as any destination).
fn create(env: &Env, op: &FileOp, dir: bool, conflict: Conflict) -> Result<OpResult, String> {
    let (mut parent_rel, mut parent) = dest_folder(env, op.dest.as_deref())?;
    let name = op.name.as_deref().filter(|n| !n.is_empty()).ok_or("A name is needed")?;
    let parts = split_rel(name)?;
    for p in &parts {
        check_part(p)?;
    }
    let (leaf, folders) = parts.split_last().ok_or("A name is needed")?;
    let mut first_new: Option<String> = None;
    let mut steps = Vec::new();
    let mut broke = false;
    for part in folders {
        let rel = join_rel(&parent_rel, part);
        let p = parent.join(part);
        match std::fs::create_dir(&p) {
            Ok(()) => {
                first_new.get_or_insert_with(|| rel.clone());
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                contained(env.root, env.fold, &rel, Use::Folder)?;
                if !p.is_dir() {
                    steps.push(fail(&rel, "exists", format!("{rel} exists and isn't a folder")));
                    broke = true;
                    break;
                }
            }
            Err(e) => {
                steps.push(fail(&rel, "io", e.to_string()));
                broke = true;
                break;
            }
        }
        parent_rel = rel;
        parent = p;
    }
    if !broke {
        match env.landing(&parent, leaf, dir, conflict, false) {
            Err("skip") => steps.push(Step::Skipped(join_rel(&parent_rel, leaf))),
            Err(_) => steps.push(fail(&join_rel(&parent_rel, leaf), "exists", "Something with that name is already there")),
            Ok(n) => {
                let rel = join_rel(&parent_rel, &n);
                let p = parent.join(&n);
                let made = if dir { std::fs::create_dir(&p) } else { std::fs::OpenOptions::new().write(true).create_new(true).open(&p).map(drop) };
                match made {
                    Ok(()) => {
                        first_new.get_or_insert_with(|| rel.clone());
                        steps.push(Step::Done(OpItem { from: rel, to: None }));
                    }
                    Err(e) if e.kind() == io::ErrorKind::AlreadyExists => steps.push(fail(&rel, "exists", "Something with that name is already there")),
                    Err(e) => steps.push(fail(&rel, "io", e.to_string())),
                }
            }
        }
    }
    let mut r = collect(steps);
    // Undo removes the outermost thing this op made, folders on the way included.
    r.undo = trash_undo(first_new.into_iter().collect());
    Ok(r)
}

fn rename(env: &Env, op: &FileOp, conflict: Conflict) -> Result<OpResult, String> {
    let [rel] = op.src.as_slice() else { return Err("Rename takes exactly one item".into()) };
    let from = contained(env.root, env.fold, rel, Use::Item)?;
    let name = single_name(op.name.as_deref())?;
    let (parent_rel, old) = split_parent(rel);
    let Ok(meta) = std::fs::symlink_metadata(&from) else {
        return Ok(collect(vec![fail(rel, "gone", "It is no longer there")]));
    };
    if name == old {
        return Ok(collect(vec![Step::Skipped(rel.clone())]));
    }
    if let Some(needs) = env.guards(std::slice::from_ref(&from), op.ack_agents) {
        return Ok(OpResult { needs: Some(needs), ..Default::default() });
    }
    let parent = from.parent().ok_or("Files won't act on the root folder itself")?.to_path_buf();
    // Two spellings of one name on this volume: the target "exists" because it is the source.
    let same_name = name_key(name, env.fold) == name_key(old, env.fold);
    let target = if same_name {
        name.to_string()
    } else {
        match env.landing(&parent, name, meta.is_dir(), conflict, false) {
            Ok(n) => n,
            Err("skip") => return Ok(collect(vec![Step::Skipped(rel.clone())])),
            Err(_) => return Ok(collect(vec![fail(rel, "exists", format!("“{name}” is already there"))])),
        }
    };
    let to = parent.join(&target);
    let done = if same_name { hop_rename(&from, &to, env.rename) } else { (env.rename)(&from, &to) };
    let new_rel = join_rel(parent_rel, &target);
    Ok(match done {
        Ok(()) => OpResult {
            done: vec![OpItem { from: rel.clone(), to: Some(new_rel.clone()) }],
            undo: Some(FileOp { kind: "rename".into(), src: vec![new_rel], name: Some(old.to_string()), ..Default::default() }),
            ..Default::default()
        },
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => collect(vec![fail(rel, "exists", format!("“{target}” is already there"))]),
        Err(e) => collect(vec![fail(rel, "io", e.to_string())]),
    })
}

/// Whether `dest` (a folder) is `src` or inside it: a folder can't go into itself.
fn inside_itself(src: &Path, dest: &Path, fold: bool) -> bool {
    match real_dir(src) {
        Some(s) => strip_root(&crate::share::canon(dest), &s, fold).is_some(),
        None => false,
    }
}

fn move_items(env: &Env, op: &FileOp, conflict: Conflict) -> Result<OpResult, String> {
    // Where each item goes: (source rel, source, destination folder rel, folder, name).
    let mut plan: Vec<(String, PathBuf, String, PathBuf, String)> = Vec::new();
    if op.to.is_empty() {
        let (dest_rel, dest) = dest_folder(env, op.dest.as_deref())?;
        for (rel, p) in items(env, &op.src)? {
            let name = split_parent(&rel).1.to_string();
            plan.push((rel, p, dest_rel.clone(), dest.clone(), name));
        }
    } else {
        if op.to.len() != op.src.len() {
            return Err("A move's `to` names one destination per item".into());
        }
        for (rel, to) in op.src.iter().zip(&op.to) {
            let p = contained(env.root, env.fold, rel, Use::Item)?;
            contained(env.root, env.fold, to, Use::Item)?;
            let (dest_rel, name) = split_parent(to);
            check_part(name)?;
            let (dest_rel, dest) = dest_folder(env, Some(dest_rel))?;
            plan.push((rel.clone(), p, dest_rel, dest, name.to_string()));
        }
    }
    let sources: Vec<PathBuf> = plan.iter().map(|p| p.1.clone()).collect();
    if let Some(needs) = env.guards(&sources, op.ack_agents) {
        return Ok(OpResult { needs: Some(needs), ..Default::default() });
    }
    let mut steps = Vec::new();
    let (mut moved, mut back) = (Vec::new(), Vec::new());
    for (rel, from, dest_rel, dest, name) in plan {
        let Ok(meta) = std::fs::symlink_metadata(&from) else {
            steps.push(fail(&rel, "gone", "It is no longer there"));
            continue;
        };
        if inside_itself(&from, &dest, env.fold) {
            steps.push(fail(&rel, "inside_itself", "A folder can't be moved into itself"));
            continue;
        }
        let (src_parent, src_name) = split_parent(&rel);
        if name_key(src_parent, env.fold) == name_key(&dest_rel, env.fold) && name_key(src_name, env.fold) == name_key(&name, env.fold) {
            steps.push(Step::Skipped(rel));
            continue;
        }
        let target = match env.landing(&dest, &name, meta.is_dir(), conflict, false) {
            Ok(n) => n,
            Err("skip") => {
                steps.push(Step::Skipped(rel));
                continue;
            }
            Err(_) => {
                steps.push(fail(&rel, "exists", format!("“{name}” is already there")));
                continue;
            }
        };
        let to = dest.join(&target);
        let done = match (env.rename)(&from, &to) {
            Err(e) if is_cross_device(&e) => move_across(&from, &to),
            other => other,
        };
        let new_rel = join_rel(&dest_rel, &target);
        match done {
            Ok(()) => {
                moved.push(new_rel.clone());
                back.push(rel.clone());
                steps.push(Step::Done(OpItem { from: rel, to: Some(new_rel) }));
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => steps.push(fail(&rel, "exists", format!("“{target}” is already there"))),
            Err(e) => steps.push(fail(&rel, "io", e.to_string())),
        }
    }
    let mut r = collect(steps);
    // Each item goes back to exactly where it was, under the name it had.
    r.undo = (!moved.is_empty()).then(|| FileOp { kind: "move".into(), src: moved, to: back, ..Default::default() });
    Ok(r)
}

/// Copy one item into `dest`. `label` is what `OpItem.from` reports.
fn copy_one(env: &Env, label: &str, from: &Path, dest_rel: &str, dest: &Path, conflict: Conflict, fresh: bool) -> Step {
    let Ok(meta) = std::fs::symlink_metadata(from) else { return fail(label, "gone", "It is no longer there") };
    if inside_itself(from, dest, env.fold) {
        return fail(label, "inside_itself", "A folder can't be copied into itself");
    }
    let Some(name) = from.file_name().map(|n| n.to_string_lossy().into_owned()) else {
        return fail(label, "io", "It has no name to copy under");
    };
    let target = match env.landing(dest, &name, meta.is_dir(), conflict, fresh) {
        Ok(n) => n,
        Err("skip") => return Step::Skipped(label.to_string()),
        Err(_) => return fail(label, "exists", format!("“{name}” is already there")),
    };
    let to = dest.join(&target);
    match copy_fresh(from, &to) {
        Ok(()) => Step::Done(OpItem { from: label.to_string(), to: Some(join_rel(dest_rel, &target)) }),
        Err(e) => fail(label, if e.kind() == io::ErrorKind::AlreadyExists { "exists" } else { "io" }, e.to_string()),
    }
}

fn with_copy_undo(steps: Vec<Step>) -> OpResult {
    let mut r = collect(steps);
    r.undo = trash_undo(r.done.iter().filter_map(|i| i.to.clone()).collect());
    r
}

fn copy_items(env: &Env, op: &FileOp, conflict: Conflict) -> Result<OpResult, String> {
    let (dest_rel, dest) = dest_folder(env, op.dest.as_deref())?;
    let steps = items(env, &op.src)?.into_iter().map(|(rel, p)| copy_one(env, &rel, &p, &dest_rel, &dest, conflict, false)).collect();
    Ok(with_copy_undo(steps))
}

/// A copy beside each item, always under a fresh name ("a copy.ts").
fn duplicate(env: &Env, op: &FileOp) -> Result<OpResult, String> {
    let mut steps = Vec::new();
    for (rel, p) in items(env, &op.src)? {
        let parent_rel = split_parent(&rel).0;
        let (parent_rel, parent) = dest_folder(env, Some(parent_rel))?;
        steps.push(copy_one(env, &rel, &p, &parent_rel, &parent, Conflict::KeepBoth, true));
    }
    Ok(with_copy_undo(steps))
}

/// Files dropped from the OS: native absolute sources, anywhere; the destination still inside.
fn import(env: &Env, op: &FileOp, conflict: Conflict) -> Result<OpResult, String> {
    let (dest_rel, dest) = dest_folder(env, op.dest.as_deref())?;
    if op.src.is_empty() {
        return Err("No items given".into());
    }
    for s in &op.src {
        if !Path::new(s).is_absolute() {
            return Err(format!("{s}: an import takes absolute paths"));
        }
    }
    let steps = op.src.iter().map(|s| copy_one(env, s, Path::new(s), &dest_rel, &dest, conflict, false)).collect();
    Ok(with_copy_undo(steps))
}

fn trash_items(env: &Env, op: &FileOp) -> Result<OpResult, String> {
    let items = items(env, &op.src)?;
    let paths: Vec<PathBuf> = items.iter().map(|(_, p)| p.clone()).collect();
    if let Some(needs) = env.guards(&paths, op.ack_agents) {
        return Ok(OpResult { needs: Some(needs), ..Default::default() });
    }
    let (present, missing): (Vec<_>, Vec<_>) = items.into_iter().partition(|(_, p)| env.exists(p));
    let mut steps: Vec<Step> = missing.iter().map(|(rel, _)| fail(rel, "gone", "It is no longer there")).collect();
    let since = now_secs();
    let results = env.trash.trash(present.iter().map(|(_, p)| p.clone()).collect());
    let mut no_trash = Vec::new();
    for ((rel, _), res) in present.into_iter().zip(results) {
        match res {
            Ok(()) => steps.push(Step::Done(OpItem { from: rel, to: None })),
            Err(msg) => {
                no_trash.push(rel.clone());
                steps.push(fail(&rel, "no_trash", msg));
            }
        }
    }
    let mut r = collect(steps);
    if !no_trash.is_empty() {
        let what = if no_trash.len() == 1 { "This item" } else { "These items" };
        r.needs = Some(Needs { code: "no_trash".into(), detail: format!("{what} can't go to the trash here. Delete permanently?"), tabs: Vec::new() });
    }
    let trashed: Vec<String> = r.done.iter().map(|i| i.from.clone()).collect();
    if env.trash.can_restore() && !trashed.is_empty() {
        r.undo = Some(FileOp { kind: "restore".into(), src: trashed, trashed_at: Some(since), ..Default::default() });
    }
    Ok(r)
}

fn delete_items(env: &Env, op: &FileOp) -> Result<OpResult, String> {
    // The ticket is checked first and used up only once the guards pass, so the human's
    // second yes over an agent's work can reuse it.
    env.tickets.check(op.token.as_deref(), env.place, &op.src)?;
    let items = items(env, &op.src)?;
    let paths: Vec<PathBuf> = items.iter().map(|(_, p)| p.clone()).collect();
    if let Some(needs) = env.guards(&paths, op.ack_agents) {
        return Ok(OpResult { needs: Some(needs), ..Default::default() });
    }
    env.tickets.take(op.token.as_deref(), env.place, &op.src)?;
    let steps = items
        .into_iter()
        .map(|(rel, p)| match remove_any(&p) {
            Ok(()) => Step::Done(OpItem { from: rel, to: None }),
            Err(e) if e.kind() == io::ErrorKind::NotFound => fail(&rel, "gone", "It is no longer there"),
            Err(e) => fail(&rel, "io", e.to_string()),
        })
        .collect();
    Ok(collect(steps))
}

/// Undo of a trash: put the items back where they were (Linux and Windows).
fn restore_items(env: &Env, op: &FileOp) -> Result<OpResult, String> {
    if !env.trash.can_restore() {
        return Err("This platform's trash can't put items back from here: open the Trash to drag them out".into());
    }
    let since = op.trashed_at.ok_or("A restore needs the time the items were trashed")?;
    let items = items(env, &op.src)?;
    let (taken, free): (Vec<_>, Vec<_>) = items.into_iter().partition(|(_, p)| env.exists(p));
    let mut steps: Vec<Step> = taken.iter().map(|(rel, _)| fail(rel, "exists", "Something else now has its name")).collect();
    let results = env.trash.restore(free.iter().map(|(_, p)| (p.clone(), since)).collect());
    for ((rel, _), res) in free.into_iter().zip(results) {
        steps.push(match res {
            Ok(()) => Step::Done(OpItem { from: rel, to: None }),
            Err((code, msg)) => fail(&rel, &code, msg),
        });
    }
    let mut r = collect(steps);
    r.undo = trash_undo(r.done.iter().map(|i| i.from.clone()).collect());
    Ok(r)
}

/// `ft_prepare_delete`: count what deleting `rels` would remove and mint a ticket for exactly
/// that set. Counting walks without following links, like the delete itself.
pub fn prepare_delete(env: &Env, rels: &[String]) -> Result<DeleteTicket, String> {
    let items = items(env, rels)?;
    let (mut files, mut dirs) = (0, 0);
    for (rel, p) in &items {
        let (f, d, _) = tally(p).map_err(|e| format!("{rel}: {e}"))?;
        files += f;
        dirs += d;
    }
    Ok(DeleteTicket { token: env.tickets.mint(env.place, rels), files, dirs })
}

#[cfg(test)]
#[path = "ops_tests.rs"]
mod tests;
