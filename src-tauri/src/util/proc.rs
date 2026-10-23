//! One way to run a helper process (git, mostly) from a GUI app (docs/file-tree.md §7.1).
//!
//! Every subprocess maiTerm runs on its own behalf needs the same four things, and each one
//! was missing somewhere at least once:
//! - stdin is null, so a child that wants to ask something reads EOF instead of hanging on a
//!   terminal nobody can see;
//! - on Windows, `CREATE_NO_WINDOW`, or every spawn flashes a console window;
//! - for git, an environment that can never prompt or take an optional lock (an agent's own
//!   `git commit` in the same repo must never meet our `index.lock`);
//! - a deadline, after which the child is killed rather than left to pin a blocking thread.
//!
//! Everything here blocks. Callers run it under `spawn_blocking`.

use std::ffi::OsStr;
use std::io::Read;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A GUI app's console child opens a console window unless told not to.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// `program` with stdin closed and, on Windows, no console window.
pub fn command(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = Command::new(program);
    cmd.stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

/// A git invocation that can never stop to ask a question and never takes an optional lock.
/// A prompt nobody can see is a hang, and everything that calls this runs where nobody can
/// answer one.
pub fn git() -> Command {
    let mut cmd = command("git");
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_SSH_COMMAND", "ssh -o BatchMode=yes -o ConnectTimeout=10");
    cmd
}

#[derive(Debug)]
pub enum RunError {
    /// The program couldn't be started (not installed, not executable).
    Spawn(std::io::Error),
    /// The deadline passed; the child was killed.
    TimedOut,
    Io(std::io::Error),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Spawn(e) => write!(f, "couldn't run it: {e}"),
            RunError::TimedOut => write!(f, "timed out"),
            RunError::Io(e) => write!(f, "{e}"),
        }
    }
}

/// Kills and reaps the child if it is dropped before it finished: on the timeout path, on an
/// error, or on a panic in between. A child we stopped waiting for must not outlive us.
struct KillOnDrop(Option<Child>);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Read a pipe to the end on its own thread. Both pipes are drained while the child runs:
/// waiting first and reading after deadlocks as soon as the output outgrows the pipe buffer.
fn drain(pipe: Option<impl Read + Send + 'static>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// Run `cmd` to completion with stdout and stderr captured, killing it at `timeout`.
pub fn output_timeout(cmd: &mut Command, timeout: Duration) -> Result<Output, RunError> {
    let deadline = Instant::now() + timeout;
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut guard = KillOnDrop(Some(cmd.spawn().map_err(RunError::Spawn)?));
    let child = guard.0.as_mut().expect("just spawned");
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());

    // Poll quickly at first, so a 5 ms git command isn't charged a long sleep, then back off.
    let mut nap = Duration::from_millis(1);
    let status: ExitStatus = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => return Err(RunError::TimedOut),
            Ok(None) => {
                std::thread::sleep(nap);
                nap = (nap * 2).min(Duration::from_millis(50));
            }
            Err(e) => return Err(RunError::Io(e)),
        }
    };
    guard.0 = None; // reaped by try_wait

    // A grandchild (ssh under git) can hold the pipes open after the child exits, so even
    // reading the output is bounded by the same deadline.
    let left = || deadline.saturating_duration_since(Instant::now());
    let stdout = stdout.recv_timeout(left()).map_err(|_| RunError::TimedOut)?;
    let stderr = stderr.recv_timeout(left()).map_err(|_| RunError::TimedOut)?;
    Ok(Output { status, stdout, stderr })
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn output_is_captured_and_stdin_is_closed() {
        // `cat` with a null stdin ends at once instead of waiting for input.
        let out = output_timeout(&mut command("cat"), Duration::from_secs(5)).unwrap();
        assert!(out.status.success());
        assert!(out.stdout.is_empty());
        let out = output_timeout(command("sh").args(["-c", "printf hi; printf err >&2"]), Duration::from_secs(5)).unwrap();
        assert_eq!(out.stdout, b"hi");
        assert_eq!(out.stderr, b"err");
    }

    #[test]
    fn output_larger_than_a_pipe_buffer_does_not_deadlock() {
        let out = output_timeout(command("sh").args(["-c", "head -c 1000000 /dev/zero"]), Duration::from_secs(10)).unwrap();
        assert_eq!(out.stdout.len(), 1_000_000);
    }

    #[test]
    fn a_child_past_its_deadline_is_killed() {
        let start = Instant::now();
        let r = output_timeout(command("sleep").arg("30"), Duration::from_millis(200));
        assert!(matches!(r, Err(RunError::TimedOut)));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let r = output_timeout(&mut command("definitely-not-a-real-binary-xyz"), Duration::from_secs(1));
        assert!(matches!(r, Err(RunError::Spawn(_))));
    }
}
