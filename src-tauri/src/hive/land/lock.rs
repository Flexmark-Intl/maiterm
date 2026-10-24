//! The land lock (docs/hive.md §8.1): lands are serialized per repository across steps 1 and 2.
//! An in-process lock wouldn't serialize the dev and prod maiTerms (separate data dirs, one repo),
//! so it is an `O_EXCL` file in the repo's git common dir naming the pid and build that took it.
//! Workers can't write there: their allow lists hold only their own refs, objects and admin corner.
//!
//! A lock whose pid is gone is stale, and so is one naming this process's pid that this process
//! doesn't hold (a land that died with its task, or a pid the OS reused for us). Waiting is a
//! condvar this process's releases wake, plus a re-read of the file every 2 s for the other
//! build's. Everything here blocks: async callers wrap it in `spawn_blocking`.

use std::collections::HashMap;
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

pub const LOCK_FILE: &str = "maiterm-land.lock";
/// How often a waiting land re-reads a lock another process holds.
pub const RECHECK: Duration = Duration::from_secs(2);
/// A lock file nobody finished writing (a crash between create and fsync) is stale after this.
const UNREADABLE_STALE: Duration = Duration::from_secs(30);

/// What a lock file holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LockInfo {
    pub pid: u32,
    /// `dev` | `prod` | an isolated instance's name.
    pub build: String,
    pub hive_id: String,
    pub land_id: String,
    pub at: String,
}

impl LockInfo {
    pub fn ours(hive_id: &str, land_id: &str) -> Self {
        Self {
            pid: std::process::id(),
            build: this_build(),
            hive_id: hive_id.to_string(),
            land_id: land_id.to_string(),
            at: crate::commands::workspace::iso_now(),
        }
    }

    /// Who holds it, for the card and the coordinator.
    pub fn describe(&self) -> String {
        format!("land {} of maiTerm {} (pid {}), since {}", self.land_id, self.build, self.pid, self.at)
    }
}

pub fn this_build() -> String {
    match crate::instance::name() {
        Some(n) => n.to_string(),
        None if cfg!(debug_assertions) => "dev".to_string(),
        None => "prod".to_string(),
    }
}

/// lock path → the land of this process that holds it.
static HELD: LazyLock<Mutex<HashMap<PathBuf, String>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static WAKE: Condvar = Condvar::new();

pub fn lock_path(common_dir: &Path) -> PathBuf {
    common_dir.join(LOCK_FILE)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Taken {
    Taken,
    /// Someone else holds it: what its file says, or None when it can't be read (yet).
    Busy(Option<LockInfo>),
}

#[cfg(unix)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // EPERM is a live process that isn't ours (pid 1 among them); only ESRCH is gone.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM) }
}

#[cfg(windows)]
pub(crate) fn pid_alive(pid: u32) -> bool {
    use sysinfo::{Pid, ProcessesToUpdate, System};
    let target = Pid::from_u32(pid);
    let mut sys = System::new();
    sys.refresh_processes(ProcessesToUpdate::Some(&[target]), true);
    sys.process(target).is_some()
}

fn read_info(path: &Path) -> Option<LockInfo> {
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str::<LockInfo>(&text).ok().filter(|i| i.pid != 0 && !i.land_id.is_empty())
}

fn age(path: &Path) -> Duration {
    fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| SystemTime::now().duration_since(t).ok()).unwrap_or_default()
}

fn stale(path: &Path, info: Option<&LockInfo>, held: &HashMap<PathBuf, String>) -> bool {
    match info {
        None => age(path) >= UNREADABLE_STALE,
        Some(i) if i.pid == std::process::id() => held.get(path) != Some(&i.land_id),
        Some(i) => !pid_alive(i.pid),
    }
}

/// Takes the lock for `info.land_id`, or answers who holds it. Re-taking a lock this land already
/// holds is taken. A stale lock is removed and taken.
pub fn try_take(common_dir: &Path, info: &LockInfo) -> Result<Taken, String> {
    let path = lock_path(common_dir);
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if held.get(&path) == Some(&info.land_id) && read_info(&path).is_some_and(|i| i.land_id == info.land_id) {
        return Ok(Taken::Taken);
    }
    for _ in 0..2 {
        match fs::OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut f) => {
                let body = serde_json::to_vec(info).map_err(|e| e.to_string())?;
                let written = f.write_all(&body).and_then(|_| f.sync_all());
                if let Err(e) = written {
                    let _ = fs::remove_file(&path);
                    return Err(format!("{}: {e}", path.display()));
                }
                held.insert(path, info.land_id.clone());
                return Ok(Taken::Taken);
            }
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                let other = read_info(&path);
                if !stale(&path, other.as_ref(), &held) {
                    return Ok(Taken::Busy(other));
                }
                log::warn!(
                    "hive land: stale lock {} recovered ({})",
                    path.display(),
                    other.as_ref().map_or_else(|| "unreadable".to_string(), LockInfo::describe)
                );
                if let Err(e) = fs::remove_file(&path) {
                    if e.kind() != ErrorKind::NotFound {
                        return Err(format!("{}: {e}", path.display()));
                    }
                }
            }
            Err(e) => return Err(format!("{}: {e}", path.display())),
        }
    }
    Ok(Taken::Busy(read_info(&path)))
}

/// `try_take` until `deadline`, waking on this process's releases and re-reading every
/// `RECHECK`. `on_busy` hears each holder seen, so a long wait can be told. Busy at the deadline
/// answers the holder.
pub fn take(common_dir: &Path, info: &LockInfo, deadline: Instant, mut on_busy: impl FnMut(Option<&LockInfo>)) -> Result<Taken, String> {
    loop {
        let busy = match try_take(common_dir, info)? {
            Taken::Taken => return Ok(Taken::Taken),
            Taken::Busy(other) => other,
        };
        on_busy(busy.as_ref());
        let now = Instant::now();
        if now >= deadline {
            return Ok(Taken::Busy(busy));
        }
        let wait = RECHECK.min(deadline - now);
        let g = HELD.lock().unwrap_or_else(|e| e.into_inner());
        let _ = WAKE.wait_timeout(g, wait).unwrap_or_else(|e| e.into_inner());
    }
}

/// Removes the lock only if `land_id` holds it, and wakes this process's waiters.
pub fn release(common_dir: &Path, land_id: &str) {
    let path = lock_path(common_dir);
    let mut held = HELD.lock().unwrap_or_else(|e| e.into_inner());
    if read_info(&path).is_some_and(|i| i.land_id == land_id) {
        if let Err(e) = fs::remove_file(&path) {
            log::warn!("hive land: lock {} not removed: {e}", path.display());
        }
    }
    if held.get(&path).is_some_and(|l| l == land_id) {
        held.remove(&path);
    }
    drop(held);
    WAKE.notify_all();
}

/// Whether this process holds the lock for `land_id`.
pub fn holds(common_dir: &Path, land_id: &str) -> bool {
    HELD.lock().unwrap_or_else(|e| e.into_inner()).get(&lock_path(common_dir)).is_some_and(|l| l == land_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let t = std::env::temp_dir().join(format!("hive-lock-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&t).unwrap();
        t
    }

    #[test]
    fn one_land_holds_it_and_only_its_release_frees_it() {
        let d = scratch();
        let a = LockInfo::ours("h", "L-aaaaaaaa");
        let b = LockInfo::ours("h", "L-bbbbbbbb");
        assert_eq!(try_take(&d, &a).unwrap(), Taken::Taken);
        assert_eq!(try_take(&d, &a).unwrap(), Taken::Taken, "re-taking its own lock");
        assert!(holds(&d, "L-aaaaaaaa"));
        match try_take(&d, &b).unwrap() {
            Taken::Busy(Some(i)) => assert_eq!(i.land_id, "L-aaaaaaaa"),
            other => panic!("{other:?}"),
        }
        release(&d, "L-bbbbbbbb");
        assert!(lock_path(&d).exists(), "another land's release leaves it");
        release(&d, "L-aaaaaaaa");
        assert!(!lock_path(&d).exists());
        assert_eq!(try_take(&d, &b).unwrap(), Taken::Taken);
        release(&d, "L-bbbbbbbb");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_dead_pid_is_recovered_and_a_live_one_waits() {
        let d = scratch();
        // A pid no process has (above pid_max on macOS and Linux) left by a crash.
        let dead = LockInfo { pid: 2_147_483_000, build: "prod".into(), hive_id: "h".into(), land_id: "L-dead0000".into(), at: "then".into() };
        fs::write(lock_path(&d), serde_json::to_vec(&dead).unwrap()).unwrap();
        let me = LockInfo::ours("h", "L-11111111");
        assert_eq!(try_take(&d, &me).unwrap(), Taken::Taken, "a dead pid's lock is stale");
        release(&d, "L-11111111");

        #[cfg(unix)]
        {
            let live = LockInfo { pid: 1, build: "prod".into(), hive_id: "h".into(), land_id: "L-init0000".into(), at: "now".into() };
            fs::write(lock_path(&d), serde_json::to_vec(&live).unwrap()).unwrap();
            let mut seen = 0;
            let start = Instant::now();
            let out = take(&d, &me, Instant::now() + Duration::from_millis(300), |h| {
                assert_eq!(h.map(|i| i.pid), Some(1));
                seen += 1;
            })
            .unwrap();
            assert!(matches!(out, Taken::Busy(Some(ref i)) if i.pid == 1), "{out:?}");
            assert!(seen >= 1 && start.elapsed() >= Duration::from_millis(300));
            assert!(lock_path(&d).exists(), "pid 1's lock stays");
        }
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn our_own_pid_without_our_hold_is_stale() {
        let d = scratch();
        let orphan = LockInfo::ours("h", "L-orphan00");
        fs::write(lock_path(&d), serde_json::to_vec(&orphan).unwrap()).unwrap();
        let me = LockInfo::ours("h", "L-22222222");
        assert_eq!(try_take(&d, &me).unwrap(), Taken::Taken, "this process never took it");
        release(&d, "L-22222222");
        // A fresh unreadable file (a writer mid-create) isn't stale yet.
        fs::write(lock_path(&d), "").unwrap();
        assert_eq!(try_take(&d, &me).unwrap(), Taken::Busy(None));
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn a_release_wakes_a_waiter_in_this_process() {
        let d = scratch();
        let a = LockInfo::ours("h", "L-33333333");
        assert_eq!(try_take(&d, &a).unwrap(), Taken::Taken);
        let (d2, start) = (d.clone(), Instant::now());
        let waiter = std::thread::spawn(move || take(&d2, &LockInfo::ours("h", "L-44444444"), Instant::now() + Duration::from_secs(10), |_| {}).unwrap());
        std::thread::sleep(Duration::from_millis(150));
        release(&d, "L-33333333");
        assert_eq!(waiter.join().unwrap(), Taken::Taken);
        assert!(start.elapsed() < RECHECK + Duration::from_secs(1), "woken, not left to its re-check: {:?}", start.elapsed());
        release(&d, "L-44444444");
        let _ = fs::remove_dir_all(&d);
    }
}
