//! One way to run a helper process (git, mostly) from a GUI app (docs/file-tree.md §7.1).
//!
//! Every subprocess maiTerm runs on its own behalf needs the same four things, and each one
//! was missing somewhere at least once:
//! - stdin is null, so a child that wants to ask something reads EOF instead of hanging on a
//!   terminal nobody can see;
//! - on Windows, `CREATE_NO_WINDOW`, or every spawn flashes a console window;
//! - for git, an environment that can never prompt or take an optional lock (an agent's own
//!   `git commit` in the same repo must never meet our `index.lock`);
//! - a deadline, after which the child is killed and reaped rather than left running.
//!
//! The std functions block, and callers run them under `spawn_blocking`. `output_timeout_async`
//! and `wait_output_timeout` are the same deadline for tokio children.

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
    git_at("git")
}

/// `git()` with the program named: the Files git worker's tests run a stub in its place.
pub fn git_at(program: impl AsRef<OsStr>) -> Command {
    let mut cmd = command(program);
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

// ── The same deadline for tokio children ─────────────────────────────────────────────────
//
// `tokio::time::timeout(d, cmd.output())` looks like a bounded run and is not one: when the
// deadline fires the `output()` future is dropped, and tokio does not kill on drop unless
// asked, so the child goes on running with nobody holding it (an ssh to a hung host stayed up
// for its whole session). These two kill the child at the deadline and wait for it, so it is
// gone — reaped, not a zombie — by the time the caller sees `TimedOut`.

/// Run `cmd` with stdin closed and stdout and stderr captured, killing it at `timeout`.
pub async fn output_timeout_async(cmd: &mut tokio::process::Command, timeout: Duration) -> Result<Output, RunError> {
    wait_output_timeout(spawn_captured(cmd)?, timeout).await
}

/// `cmd` spawned the way `output_timeout_async` runs it.
fn spawn_captured(cmd: &mut tokio::process::Command) -> Result<tokio::process::Child, RunError> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // `wait_output_timeout` kills at the deadline; this covers the caller's future being
        // dropped (a cancelled command) before the deadline.
        .kill_on_drop(true)
        .spawn()
        .map_err(RunError::Spawn)
}

/// Wait for a child the caller already spawned (one it had to write to first), collecting
/// whatever of stdout and stderr it piped, and kill and reap it at `timeout`.
///
/// The pipes are read while the child runs: waiting first and reading after deadlocks as soon
/// as the output outgrows the pipe buffer. A grandchild holding a pipe open past the child's
/// exit is bounded by the same deadline.
pub async fn wait_output_timeout(mut child: tokio::process::Child, timeout: Duration) -> Result<Output, RunError> {
    use tokio::io::AsyncReadExt;

    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let result = tokio::time::timeout(timeout, async {
        let read_out = async {
            let mut buf = Vec::new();
            if let Some(pipe) = stdout.as_mut() {
                pipe.read_to_end(&mut buf).await?;
            }
            Ok::<_, std::io::Error>(buf)
        };
        let read_err = async {
            let mut buf = Vec::new();
            if let Some(pipe) = stderr.as_mut() {
                pipe.read_to_end(&mut buf).await?;
            }
            Ok::<_, std::io::Error>(buf)
        };
        let (status, stdout, stderr) = tokio::try_join!(child.wait(), read_out, read_err)?;
        Ok::<_, std::io::Error>(Output { status, stdout, stderr })
    })
    .await;
    match result {
        Ok(output) => output.map_err(RunError::Io),
        Err(_) => {
            // `kill` is start_kill + wait: when it returns the child is reaped. An error here
            // means it had already exited and been waited on (a grandchild held the pipes).
            let _ = child.kill().await;
            Err(RunError::TimedOut)
        }
    }
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

    /// Is `pid` still a child of this process, running or a zombie? `waitpid(WNOHANG)` answers
    /// about our own children only, so another process that reuses the pid can't make it yes.
    fn still_our_child(pid: u32) -> bool {
        let mut status = 0;
        let r = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
        // 0: still running. >0 would mean WE just reaped it, i.e. it was a zombie.
        r >= 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ECHILD)
    }

    /// A stub that never exits on its own: it waits on a stdin nobody writes or closes.
    fn never_exits() -> tokio::process::Command {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "read never"]).stdin(Stdio::piped());
        cmd
    }

    #[tokio::test]
    async fn a_timed_out_tokio_child_is_killed_and_reaped() {
        let mut cmd = never_exits();
        let mut child = cmd.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let pid = child.id().unwrap();
        // Held, never written or closed: nothing but the kill can end the child.
        let _stdin = child.stdin.take();
        assert!(still_our_child(pid));

        let r = wait_output_timeout(child, Duration::from_millis(50)).await;
        assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
        assert!(!still_our_child(pid), "the timed-out child {pid} is still running or unreaped");
    }

    #[tokio::test]
    async fn a_timed_out_command_is_killed_and_reaped() {
        // `spawn_captured` closes stdin, so this stub is one that doesn't read it.
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("100000");
        let child = spawn_captured(&mut cmd).unwrap();
        let pid = child.id().unwrap();
        let r = wait_output_timeout(child, Duration::from_millis(50)).await;
        assert!(matches!(r, Err(RunError::TimedOut)), "{r:?}");
        assert!(!still_our_child(pid), "the timed-out child {pid} is still running or unreaped");
    }

    #[tokio::test]
    async fn a_tokio_child_that_finishes_gives_its_output() {
        let mut cmd = tokio::process::Command::new("sh");
        cmd.args(["-c", "printf hi; printf err >&2; exit 3"]);
        let out = output_timeout_async(&mut cmd, Duration::from_secs(30)).await.unwrap();
        assert_eq!(out.status.code(), Some(3));
        assert_eq!(out.stdout, b"hi");
        assert_eq!(out.stderr, b"err");
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let r = output_timeout(&mut command("definitely-not-a-real-binary-xyz"), Duration::from_secs(1));
        assert!(matches!(r, Err(RunError::Spawn(_))));
    }
}
