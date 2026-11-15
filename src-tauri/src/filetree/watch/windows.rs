//! Windows: ReadDirectoryChangesW (docs/file-tree.md §6.3), on threads of our own.
//!
//! One handle per hub, recursive on the root. A buffer overflow arrives as a rescan. The root
//! is given in `share::canon` form (no `\\?\` except on UNC), so event paths strip against it.
//! A shallow root (a drive root, the profile folder) watches each opened folder NonRecursive.
//!
//! A handle reports what happens inside its folder, never the folder itself going: a root
//! deleted (with nothing in it to report) or renamed away, the Recycle Bin included, is silent
//! on its own watch. So the root's parent is watched too, NonRecursive, and only an event that
//! names the root appearing, going or moving gets through.
//!
//! **Why not notify's `ReadDirectoryChangesWatcher`.** Its stop closes a folder's handle and
//! then waits, alertably, for the read's completion routine. A read that had already completed
//! (a change landed just before the stop) has its routine run in that wait, and the routine
//! re-arms ReadDirectoryChangesW on the handle VALUE it was given, which is closed by then.
//! Windows hands a freed handle value to the next handle the process opens, so the re-armed
//! read lands on someone else's handle. When that is a synchronous directory handle (every
//! `std::fs::remove_dir_all` opens its folders that way), the read blocks the watcher's thread
//! until the folder changes, and every other call on that handle queues behind it: the thread
//! deleting the folder waits on the read, and the read waits for a change only that thread
//! would make. That is how Files' subscription test hung Windows CI for hours, deleting its
//! temp folder right after dropping a hub (2026-11-13, 2026-11-14).
//!
//! So each watched folder gets a thread that owns its handle outright. The read is overlapped
//! with an event, never a completion routine, and only this thread issues it. Stopping cancels
//! the read and waits (`GetOverlappedResult`) until the kernel is done with it, and only then
//! is the handle closed: nothing touches a handle after it is closed. A stop never waits on the
//! thread, because a hub is dropped on whichever thread lets go of it last, a listing thread or
//! the grace thread holding the hub map's lock among them.

use std::collections::HashMap;
use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::Arc;

use notify::EventKind;
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_NOTIFY_ENUM_DIR, HANDLE, INVALID_HANDLE_VALUE};

use super::rdcw;
use super::{DirWatch, RawEvent, RawSender, WatchError};

/// What a read can return at once (the kernel's limit for a network folder; plenty locally).
const BUF_BYTES: usize = 64 * 1024;

/// The kernel32 calls this file needs, declared here: windows-sys gates them on features
/// (`Win32_System_IO`, `Win32_System_Threading`, `Win32_Security`) this crate doesn't enable.
mod ffi {
    use std::ffi::c_void;

    pub type Handle = *mut c_void;

    pub const FILE_LIST_DIRECTORY: u32 = 0x1;
    pub const FILE_SHARE_ALL: u32 = 0x1 | 0x2 | 0x4;
    pub const OPEN_EXISTING: u32 = 3;
    pub const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    pub const FILE_FLAG_OVERLAPPED: u32 = 0x4000_0000;
    /// FILE_NAME, DIR_NAME, ATTRIBUTES, SIZE, LAST_WRITE, CREATION, SECURITY: what notify asked.
    pub const NOTIFY_FILTER: u32 = 0x1 | 0x2 | 0x4 | 0x8 | 0x10 | 0x40 | 0x100;
    pub const WAIT_OBJECT_0: u32 = 0;
    pub const INFINITE: u32 = u32::MAX;

    /// `OVERLAPPED`, its Offset/Pointer union spelled as the two offsets.
    #[repr(C)]
    pub struct Overlapped {
        pub internal: usize,
        pub internal_high: usize,
        pub offset: u32,
        pub offset_high: u32,
        pub event: Handle,
    }

    #[link(name = "kernel32")]
    extern "system" {
        pub fn CreateFileW(name: *const u16, access: u32, share: u32, security: *const c_void, disposition: u32, flags: u32, template: Handle) -> Handle;
        pub fn CreateEventW(security: *const c_void, manual_reset: i32, initial: i32, name: *const u16) -> Handle;
        pub fn SetEvent(event: Handle) -> i32;
        pub fn WaitForMultipleObjects(count: u32, handles: *const Handle, wait_all: i32, ms: u32) -> u32;
        pub fn ReadDirectoryChangesW(dir: Handle, buf: *mut c_void, len: u32, subtree: i32, filter: u32, returned: *mut u32, overlapped: *mut Overlapped, routine: *const c_void) -> i32;
        pub fn GetOverlappedResult(file: Handle, overlapped: *const Overlapped, bytes: *mut u32, wait: i32) -> i32;
        pub fn CancelIoEx(file: Handle, overlapped: *const Overlapped) -> i32;
    }
}

/// A handle closed when this is dropped, and only then.
struct Owned(HANDLE);

// SAFETY: a kernel handle is usable from any thread; each `Owned` is closed exactly once.
unsafe impl Send for Owned {}
unsafe impl Sync for Owned {}

impl Owned {
    fn event() -> std::io::Result<Owned> {
        // Manual reset: the read clears it when issued, the kernel sets it when done.
        // SAFETY: no security attributes, no name.
        let h = unsafe { ffi::CreateEventW(std::ptr::null(), 1, 0, std::ptr::null()) };
        if h.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Owned(h))
    }
}

impl Drop for Owned {
    fn drop(&mut self) {
        // SAFETY: a handle this value owns, closed once.
        unsafe { CloseHandle(self.0) };
    }
}

type Emit = Box<dyn FnMut(notify::Result<notify::Event>) + Send>;

/// One watched folder's thread. Dropping it asks the thread to stop, without waiting for it.
struct DirThread {
    stop: Arc<Owned>,
}

impl Drop for DirThread {
    fn drop(&mut self) {
        // SAFETY: the stop event is alive while either side holds it.
        unsafe { ffi::SetEvent(self.stop.0) };
    }
}

fn notify_err(e: std::io::Error, path: &Path) -> notify::Error {
    notify::Error::io(e).add_path(path.to_path_buf())
}

/// Watch `dir`, calling `emit` for each change. Returns once the first read is issued, so a
/// change made after this returns is reported (as notify's `watch()` promised).
fn watch_dir(dir: &Path, subtree: bool, emit: Emit) -> notify::Result<DirThread> {
    if !dir.is_dir() {
        return Err(notify::Error::path_not_found().add_path(dir.to_path_buf()));
    }
    let wide: Vec<u16> = dir.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: a NUL-terminated path; the handle is owned at once.
    let h = unsafe {
        ffi::CreateFileW(wide.as_ptr(), ffi::FILE_LIST_DIRECTORY, ffi::FILE_SHARE_ALL, std::ptr::null(), ffi::OPEN_EXISTING, ffi::FILE_FLAG_BACKUP_SEMANTICS | ffi::FILE_FLAG_OVERLAPPED, std::ptr::null_mut())
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(notify_err(std::io::Error::last_os_error(), dir));
    }
    let handle = Owned(h);
    let done = Owned::event().map_err(|e| notify_err(e, dir))?;
    let stop = Arc::new(Owned::event().map_err(|e| notify_err(e, dir))?);
    let (armed_tx, armed_rx) = mpsc::channel();
    let thread_stop = stop.clone();
    let path = dir.to_path_buf();
    // The read is issued on the thread that waits on it: a thread's pending reads are
    // cancelled when it exits, so the caller (often a short-lived thread) can't issue it.
    std::thread::Builder::new()
        .name("ft-rdcw".into())
        .stack_size(256 * 1024)
        .spawn(move || run(handle, done, thread_stop, path, subtree, emit, armed_tx))
        .map_err(|e| notify_err(e, dir))?;
    // Answered as soon as the thread has issued its first read (an overlapped call returns at
    // once); an error drops the sender, which ends this wait too.
    match armed_rx.recv() {
        Ok(Ok(())) => Ok(DirThread { stop }),
        Ok(Err(e)) => Err(notify_err(e, dir)),
        Err(_) => Err(notify::Error::generic("the watch thread ended before it started").add_path(dir.to_path_buf())),
    }
}

/// One folder's watch loop. Owns `handle`, `done` and the buffer and OVERLAPPED the kernel
/// writes into; returns only when no read is outstanding, and the handle closes after that.
fn run(handle: Owned, done: Owned, stop: Arc<Owned>, dir: PathBuf, subtree: bool, mut emit: Emit, armed: mpsc::Sender<std::io::Result<()>>) {
    // u32s: the buffer must be DWORD aligned.
    let mut buf = vec![0u32; BUF_BYTES / 4];
    let mut ov = ffi::Overlapped { internal: 0, internal_high: 0, offset: 0, offset_high: 0, event: done.0 };
    let issue = |buf: &mut Vec<u32>, ov: &mut ffi::Overlapped| -> std::io::Result<()> {
        // SAFETY: `buf` and `ov` outlive the read: this function doesn't return while one is
        // outstanding (a stop cancels it and waits for it to finish). `unused` is written only
        // by a synchronous read; given anyway, as notify did.
        let mut unused = 0u32;
        let ok = unsafe { ffi::ReadDirectoryChangesW(handle.0, buf.as_mut_ptr().cast(), BUF_BYTES as u32, subtree as i32, ffi::NOTIFY_FILTER, &mut unused, ov, std::ptr::null()) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    };
    let first = issue(&mut buf, &mut ov);
    let started = first.is_ok();
    let _ = armed.send(first);
    drop(armed);
    if !started {
        return;
    }
    loop {
        // Stop first: with both signalled the lower index wins, so a stop isn't put off while
        // changes keep arriving.
        let waits = [stop.0, done.0];
        // SAFETY: two live event handles.
        let woke = unsafe { ffi::WaitForMultipleObjects(2, waits.as_ptr(), 0, ffi::INFINITE) };
        if woke != ffi::WAIT_OBJECT_0 + 1 {
            // Stopped (or the wait itself failed): cancel the read and wait until the kernel is
            // done with it. A read that already finished makes both calls return at once.
            let mut n = 0u32;
            // SAFETY: the handle and the OVERLAPPED of the read this thread issued.
            unsafe {
                ffi::CancelIoEx(handle.0, &ov);
                ffi::GetOverlappedResult(handle.0, &ov, &mut n, 1);
            }
            return;
        }
        let mut n = 0u32;
        // SAFETY: the read signalled `done`, so it is finished.
        let ok = unsafe { ffi::GetOverlappedResult(handle.0, &ov, &mut n, 0) } != 0;
        if !ok {
            // SAFETY: reads this thread's last error.
            let code = unsafe { GetLastError() };
            if code == ERROR_NOTIFY_ENUM_DIR {
                emit(Ok(rdcw::rescan(dir.clone())));
            } else {
                // The folder went, or can't be read any more: nothing else will come from it.
                emit(Err(notify_err(std::io::Error::from_raw_os_error(code as i32), &dir)));
                return;
            }
        } else if n == 0 {
            // The changes didn't fit in the buffer: lost, so everything under the folder is
            // re-listed.
            emit(Ok(rdcw::rescan(dir.clone())));
        } else {
            let len = (n as usize).min(BUF_BYTES);
            // SAFETY: the kernel wrote `n` bytes into `buf`, which holds `BUF_BYTES`.
            let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), len) };
            for (action, name) in rdcw::records(bytes) {
                if let Some(ev) = rdcw::event(action, dir.join(OsString::from_wide(&name))) {
                    emit(Ok(ev));
                }
            }
        }
        if let Err(e) = issue(&mut buf, &mut ov) {
            emit(Err(notify_err(e, &dir)));
            return;
        }
    }
}

pub fn recursive(root: &Path, sender: RawSender) -> notify::Result<Box<dyn Send>> {
    let own = sender.clone();
    let w = watch_dir(root, true, Box::new(move |res| own.push_notify(res)))?;
    Ok(Box::new((w, root_watch(root, sender))))
}

/// The root's own entry in its parent. None (a drive root, or a parent that can't be watched)
/// leaves only the relisting a deleted root's children cause.
fn root_watch(root: &Path, sender: RawSender) -> Option<DirThread> {
    let parent = root.parent()?;
    let name = root.file_name()?.to_os_string();
    watch_dir(
        parent,
        false,
        Box::new(move |res: notify::Result<notify::Event>| {
            // The parent's own errors are not the root's: the recursive watch reports those.
            let Ok(ev) = res else { return };
            if !matches!(ev.kind, EventKind::Create(_) | EventKind::Remove(_) | EventKind::Modify(notify::event::ModifyKind::Name(_))) {
                return;
            }
            let paths: Vec<PathBuf> = ev.paths.into_iter().filter(|p| p.file_name().is_some_and(|n| n.eq_ignore_ascii_case(&name))).collect();
            if !paths.is_empty() {
                sender.push(RawEvent { paths, rescan: false, content_only: false });
            }
        }),
    )
    .ok()
}

struct PerDir {
    sender: RawSender,
    dirs: HashMap<PathBuf, DirThread>,
}

impl DirWatch for PerDir {
    fn add(&mut self, dir: &Path) -> Result<(), WatchError> {
        if self.dirs.contains_key(dir) {
            return Ok(());
        }
        let own = self.sender.clone();
        let w = watch_dir(dir, false, Box::new(move |res| own.push_notify(res))).map_err(|e| WatchError::Other(e.to_string()))?;
        self.dirs.insert(dir.to_path_buf(), w);
        Ok(())
    }

    fn remove(&mut self, dir: &Path) {
        self.dirs.remove(dir);
    }
}

pub fn per_dir(sender: RawSender) -> notify::Result<Box<dyn DirWatch>> {
    Ok(Box::new(PerDir { sender, dirs: HashMap::new() }))
}

#[cfg(test)]
mod tests {
    use super::super::channel;
    use super::*;
    use std::time::{Duration, Instant};

    fn temp(tag: &str) -> PathBuf {
        let t = std::env::temp_dir().join(format!("ft-rdcw-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&t).unwrap();
        crate::share::canon(&t)
    }

    /// Every raw event until `want` holds of what arrived. The deadline only turns a hang
    /// into a failure: each event wakes the wait.
    fn until(rx: &mpsc::Receiver<RawEvent>, what: &str, want: impl Fn(&[RawEvent]) -> bool) -> Vec<RawEvent> {
        let deadline = Instant::now() + crate::filetree::test_support::HANG;
        let mut got = Vec::new();
        while !want(&got) {
            match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(ev) => got.push(ev),
                Err(e) => panic!("{what} ({e}): got {got:?}"),
            }
        }
        got
    }

    fn names(evs: &[RawEvent]) -> Vec<String> {
        evs.iter().flat_map(|e| e.paths.iter()).filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).collect()
    }

    #[test]
    fn a_recursive_watch_reports_changes_below_the_root_and_the_root_going() {
        let tmp = temp("rec");
        let root = tmp.join("proj");
        std::fs::create_dir_all(root.join("src")).unwrap();
        let (tx, rx, _) = channel(1024);
        let w = recursive(&root, tx).unwrap();
        std::fs::write(root.join("src").join("a.ts"), "a").unwrap();
        until(&rx, "a file made in a subfolder", |e| names(e).contains(&"a.ts".to_string()));
        std::fs::rename(root.join("src").join("a.ts"), root.join("b.ts")).unwrap();
        until(&rx, "a rename", |e| names(e).contains(&"b.ts".to_string()));
        // The root renamed away is heard through its parent.
        std::fs::rename(&root, tmp.join("moved")).unwrap();
        // (A rescan naming the root would be the recursive watch's own error, not this.)
        until(&rx, "the root moving away", |e| e.iter().any(|ev| !ev.rescan && ev.paths.iter().any(|p| p == &root)));
        drop(w);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn per_folder_watches_come_and_go() {
        let tmp = temp("per");
        std::fs::create_dir_all(tmp.join("a")).unwrap();
        let (tx, rx, _) = channel(1024);
        let mut w = per_dir(tx).unwrap();
        w.add(&tmp.join("a")).unwrap();
        assert!(w.add(&tmp.join("missing")).is_err());
        std::fs::write(tmp.join("a").join("x"), "x").unwrap();
        until(&rx, "a file in a watched folder", |e| names(e).contains(&"x".to_string()));
        w.remove(&tmp.join("a"));
        drop(w);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    /// The hang this backend exists to prevent: a watcher dropped while changes are still
    /// arriving, and its folders deleted at once on the same thread. With notify's watcher a
    /// read re-armed on a closed handle could land on `remove_dir_all`'s own and block it
    /// forever, so a watchdog turns a hang into an abort that says so.
    #[test]
    fn dropping_a_busy_watcher_and_deleting_its_folder_never_hangs() {
        // Dropped on any exit, a panic's included, which ends the watchdog's wait at once: a
        // failure must not turn into an abort later. Only a hang reaches the 60 s.
        let (_finished, over) = mpsc::channel::<()>();
        std::thread::spawn(move || {
            if let Err(mpsc::RecvTimeoutError::Timeout) = over.recv_timeout(Duration::from_secs(60)) {
                use std::io::Write;
                let _ = writeln!(std::io::stderr(), "dropping_a_busy_watcher_and_deleting_its_folder_never_hangs: still running after 60 s, aborting");
                std::process::abort();
            }
        });
        for i in 0..100 {
            let tmp = temp(&format!("busy{i}"));
            let root = tmp.join("proj");
            std::fs::create_dir_all(root.join("sub")).unwrap();
            let (tx, _rx, _) = channel(65_536);
            let w = recursive(&root, tx).unwrap();
            for n in 0..20 {
                let p = root.join("sub").join(format!("f{n}"));
                std::fs::write(&p, "x").unwrap();
                if n % 2 == 0 {
                    std::fs::remove_file(&p).unwrap();
                }
            }
            drop(w);
            let _ = std::fs::remove_dir_all(&tmp);
        }
    }
}
