//! One maiTerm per data directory, decided before anything reads or writes it.
//!
//! A second process on the same data dir does damage in its first milliseconds, long before
//! any window: `run()` re-arms the crash marker, flushes state, prunes scrollback rows for tabs
//! it doesn't know, and then auto-resumes every agent (each one a duplicate Claude on the same
//! session), starts stack services, re-registers hooks and the MCP entry on its own port and
//! opens ssh tunnels that replace the live instance's ControlMaster sockets. That happened on
//! 2026-10-23 from a bare `aiterm --version`.
//!
//! `tauri-plugin-single-instance` can't stand in for this. It is registered on the Builder,
//! after all of the above has already run, and it is off on macOS (LaunchServices keeps one
//! instance only for launches that go THROUGH it; exec'ing the binary bypasses it) and in dev.
//!
//! The guard is an OS advisory lock on a file in the data dir, held for the life of the
//! process. The kernel releases it however the process ends, a crash or SIGKILL included, so
//! a stale lock can't lock maiTerm out. It's per data dir, so dev and an installed build still
//! run side by side.

use std::fs::{File, OpenOptions, TryLockError};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub enum Outcome {
    /// This process is the instance. Keep the File alive until exit: dropping it unlocks.
    Held(File),
    /// Another live process holds it.
    Taken,
    /// No data dir, or the lock file can't be opened or locked. Guarding is impossible,
    /// and refusing to start over that would be worse than the risk it guards.
    Unguarded(String),
}

/// How long a launch waits for the lock before concluding another instance is running.
///
/// A relaunch (`restart()`, the updater, `restartMaiterm`) spawns the new process BEFORE the
/// old one exits (tauri's `process::restart`), so the newcomer routinely finds the lock still
/// held for the old one's shutdown. Waiting is free: nothing has been touched yet. A launch
/// that opens share files is usually a human double-clicking one, which should reach the
/// running instance quickly, so it waits less. Never zero, though: a relaunch reuses the
/// argv of the launch it replaces, so a maiTerm first opened from a share file relaunches
/// looking like a file-open, and its parent is still exiting (it spawns first, then
/// `exit(0)`s, its cleanup already done).
pub fn launch_wait(opens_files: bool) -> Duration {
    Duration::from_secs(if opens_files { 3 } else { 20 })
}

/// How many times a share-file handoff has relaunched itself looking for the instance
/// (lib.rs `second_instance_exit`). Cleared by the process that wins the lock: left set, it
/// would reach every PTY and every later relaunch, spending their budget before they start.
pub const HANDOFF_TRIES_ENV: &str = "MAITERM_HANDOFF_TRIES";

pub fn lock_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(crate::state::persistence::app_data_slug()).join("aiterm-instance.lock"))
}

pub fn acquire(wait: Duration) -> Outcome {
    match lock_path() {
        Some(path) => acquire_at(&path, wait),
        None => Outcome::Unguarded("no data directory".into()),
    }
}

fn acquire_at(path: &Path, wait: Duration) -> Outcome {
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    // Never truncated or written: the lock is the whole message, so there's nothing to
    // misread if a holder dies mid-anything.
    let file = match OpenOptions::new().read(true).write(true).create(true).truncate(false).open(path) {
        Ok(f) => f,
        Err(e) => return Outcome::Unguarded(format!("{}: {e}", path.display())),
    };
    let deadline = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Outcome::Held(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(TryLockError::WouldBlock) => return Outcome::Taken,
            Err(TryLockError::Error(e)) => return Outcome::Unguarded(format!("{}: {e}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("maiterm-instance-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("sub").join("aiterm-instance.lock")
    }

    #[test]
    fn second_holder_is_refused_while_the_first_lives() {
        let path = tmp("refused");
        let Outcome::Held(first) = acquire_at(&path, Duration::ZERO) else { panic!("first launch must hold the lock") };
        assert!(matches!(acquire_at(&path, Duration::from_millis(250)), Outcome::Taken));
        drop(first);
        assert!(matches!(acquire_at(&path, Duration::ZERO), Outcome::Held(_)), "released on drop");
    }

    #[test]
    fn a_relaunch_gets_the_lock_once_the_old_instance_exits() {
        let path = tmp("relaunch");
        let Outcome::Held(old) = acquire_at(&path, Duration::ZERO) else { panic!() };
        let releaser = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            drop(old);
        });
        assert!(matches!(acquire_at(&path, Duration::from_secs(5)), Outcome::Held(_)));
        releaser.join().unwrap();
    }
}
