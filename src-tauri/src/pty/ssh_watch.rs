//! Which interactive ssh a terminal is in, and whether it just died of a transport failure.
//!
//! Judged here, on the PTY reader thread, because that thread hears every terminal whether or
//! not the webview has a pane mounted for it. The judgement used to live in `TerminalPane`:
//! panes mount only for workspaces opened since the last launch or reload, so most ssh tabs
//! had no listener at all, and on 2026-11-11 a drop that took ~75 tabs was detected in one.
//!
//! The signal is ssh's exit status: 255 means the transport failed, while a logout forwards
//! the remote shell's own status. Two things make that usable from a byte stream that mixes
//! the local shell's OSC 133 with the remote shell's:
//!
//! * **Rust finds the ssh itself.** A command start (133 B/C) schedules a foreground probe a
//!   moment later; an ssh leading the tty's foreground job is recorded as `SshWatch::live`. Nothing
//!   waits for the webview to ask. A B/C does NOT forget it: with shell integration on
//!   the remote, starting the agent there is a C, and forgetting on it left an agent tab with
//!   no record to credit a later 255 to.
//! * **An exit is confirmed against the process table before it is believed.** A D while
//!   an ssh is on record may be the remote shell's. A fresh probe decides: ssh still in the
//!   foreground means it was remote (nothing happened); the shell holding the tty means the
//!   local command ended, and a 255 is then the drop.
//!
//! The record is TAKEN at the D, before the probe, so a later command can never inherit it —
//! a clean logout followed by something else that exits 255 is not a drop.
//!
//! **A command-start mark right after the exit proves nothing, and must not erase the drop.**
//! Any PROMPT_COMMAND item that runs after maiTerm's hook trips its DEBUG trap and prints a B
//! at every prompt. The commonest case is maiTerm's own: the "Install Shell Integration"
//! snippet in `~/.bashrc` beside the hook maiTerm injects, which makes each prompt read
//! `D;<code> A B D;0 A`. The first design cancelled a confirmation that a B had overtaken —
//! and so recorded nothing, ever, on such a shell. (The detector this replaced lost every drop
//! the same way: the phantom `D;0` overwrote the 255 before it was read.) So the confirmation
//! waits out the prompt, then asks the process table, and the answer stands whatever marks
//! went by in between: a shell holding its terminal is at its prompt, which is the only state
//! a reconnect can be typed into anyway.

use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};

use crate::state::AppState;

/// How long after a command start the foreground is read. The read is served from the shared
/// `ps` snapshot (at most 800ms old), so by then the snapshot postdates the start by 400ms or
/// more — ssh leads the foreground job from the moment the shell execs it, connected or not.
const START_PROBE_DELAY: Duration = Duration::from_millis(1200);

/// A terminal's ssh bookkeeping (`TerminalHandle::ssh`).
#[derive(Default)]
pub struct SshWatch {
    /// The ssh command a foreground probe last saw leading this terminal's foreground job.
    /// Taken when a command ends (see the module doc), put back if the exit was the remote's.
    pub live: Option<String>,
    /// The ssh that died of a transport failure, while the terminal still sits where it left
    /// it. Cleared by the next command start, a reconnect included. Kept in Rust because the
    /// webview that would act on it can be rebuilt, or not listening, while the shell lives on.
    pub dropped: Option<String>,
    /// A start probe is already scheduled; further starts ride on it.
    probe_pending: bool,
    /// When `live` was last taken by a command's end.
    taken_at: Option<Instant>,
}

/// How long after a command's end a foreground probe's sighting of an ssh is not believed.
/// Probes are served from a process snapshot up to 800ms old, and one taken before the exit
/// still shows the ssh that has just died: recorded, it would be credited with the NEXT thing
/// in that terminal to exit 255 — and reconnected to, unattended.
const STALE_SIGHTING: Duration = Duration::from_millis(1000);

impl SshWatch {
    /// A foreground probe saw this ssh leading the terminal's foreground job.
    pub fn note(&mut self, cmd: &str) {
        if self.taken_at.is_some_and(|at| at.elapsed() < STALE_SIGHTING) {
            return;
        }
        self.set_live(cmd);
    }

    /// This ssh IS the terminal's foreground job, on better authority than a cached probe:
    /// the exit confirmation's own fresh sweep, or maiTerm about to type it.
    pub fn set_live(&mut self, cmd: &str) {
        if self.live.as_deref() != Some(cmd) {
            self.live = Some(cmd.to_string());
        }
    }
}

/// How long after an exit mark the process table is asked whose exit it was. Long enough for
/// the prompt to finish drawing — every mark it prints, phantom starts included, is then
/// behind the drop this records — and for the answer to come from a sweep taken after the exit.
const EXIT_CONFIRM_DELAY: Duration = Duration::from_millis(300);

/// What the reader thread must do once it has released the terminal registry lock.
#[derive(Debug, PartialEq)]
pub enum Action {
    /// A command started: read the foreground shortly, and record an ssh found there.
    ProbeStart,
    /// A command ended while an ssh was on record: find out whose exit it was.
    ConfirmExit { ssh: String, exit_code: Option<i32> },
}

/// Fold one OSC 133 mark into the handle's ssh bookkeeping. Pure: the probes it asks for run
/// off the lock, in `run`.
pub fn on_shell_event(w: &mut SshWatch, cmd: char, exit_code: Option<i32>) -> Option<Action> {
    match cmd {
        // maiTerm's own hooks mark a command start with B; C is the spec's.
        'B' | 'C' => {
            // Something was run at the local prompt — the human's own reconnect, or anything
            // else. Either way the drop is no longer waiting on maiTerm. (A remote shell cannot
            // send this while a drop is on record: there is no ssh to carry it. A phantom start
            // from the prompt that reported the exit comes BEFORE the record — see `run`.)
            w.dropped = None;
            if w.probe_pending {
                return None;
            }
            w.probe_pending = true;
            Some(Action::ProbeStart)
        }
        'D' => {
            let ssh = w.live.take()?;
            w.taken_at = Some(Instant::now());
            Some(Action::ConfirmExit { ssh, exit_code })
        }
        _ => None,
    }
}

/// What a confirmed exit means, given what the process table says now.
#[derive(Debug, PartialEq)]
enum Verdict {
    /// ssh still leads the foreground job: the exit was a command in the REMOTE shell.
    StillConnected,
    /// The local ssh ended with a transport failure.
    Dropped,
    /// The local ssh ended some other way (a logout), or we cannot tell.
    Ended,
}

fn verdict(ssh_in_foreground: bool, shell_holds_tty: Option<bool>, exit_code: Option<i32>) -> Verdict {
    if ssh_in_foreground {
        return Verdict::StillConnected;
    }
    // `Some(false)`: another job owns the terminal, so this was not the prompt returning.
    // `None` is a platform without the signal (Windows), where the exit code is all there is.
    if exit_code == Some(255) && shell_holds_tty != Some(false) {
        Verdict::Dropped
    } else {
        Verdict::Ended
    }
}

/// Carry out an `Action`. Spawns its own thread: every path here runs a process sweep, and
/// the reader thread must not wait on one.
pub fn run(action: Action, state: &Arc<AppState>, app: &AppHandle, pty_id: &str, tab_id: &str) {
    let state = Arc::clone(state);
    let app = app.clone();
    let pty_id = pty_id.to_string();
    let tab_id = tab_id.to_string();
    thread::spawn(move || match action {
        Action::ProbeStart => {
            thread::sleep(START_PROBE_DELAY);
            if let Some(handle) = state.terminal_registry.write().get_mut(&pty_id) {
                handle.ssh.probe_pending = false;
            }
            // Records an ssh it finds (`note_live_ssh`). Finding none changes nothing: a failed
            // sweep must not cost an idle agent tab the only record a later drop is credited to.
            let _ = super::get_pty_foreground(&state, &pty_id, false);
        }
        Action::ConfirmExit { ssh, exit_code } => {
            thread::sleep(EXIT_CONFIRM_DELAY);
            // Fresh: a snapshot from before the exit still shows the ssh, and would read every
            // drop as "still connected".
            let Ok(foreground) = super::get_pty_foreground(&state, &pty_id, true) else { return };
            let holds = super::pty_child_pid_of(&state, &pty_id).and_then(super::shell_holds_tty);
            match verdict(foreground.is_some(), holds, exit_code) {
                // Put back by hand: this soon after the exit mark a probe's own note is not
                // believed (`STALE_SIGHTING`), but this sweep was taken after it.
                Verdict::StillConnected => {
                    if let (Some(cmd), Some(handle)) = (foreground, state.terminal_registry.write().get_mut(&pty_id)) {
                        handle.ssh.set_live(&cmd);
                    }
                }
                Verdict::Ended => {}
                Verdict::Dropped => {
                    {
                        let mut registry = state.terminal_registry.write();
                        let Some(handle) = registry.get_mut(&pty_id) else { return };
                        handle.ssh.dropped = Some(ssh.clone());
                    }
                    log::info!("SSH drop: tab {tab_id} (pty {pty_id}) lost its ssh (exit 255)");
                    let _ = app.emit(
                        "ssh-dropped",
                        DroppedSsh { pty_id: pty_id.clone(), tab_id: tab_id.clone(), ssh },
                    );
                }
            }
        }
    });
}

/// One terminal sitting at its local prompt because its ssh died.
#[derive(serde::Serialize, Clone)]
pub struct DroppedSsh {
    pub pty_id: String,
    /// The tab the PTY was spawned for. A reload keeps the PTY and may not keep the id, so
    /// the frontend matches on `pty_id` first.
    pub tab_id: String,
    /// The raw ssh command line, as `ps` reported it.
    pub ssh: String,
}

/// Every terminal with a drop on record. The webview reads this to catch up on what it did
/// not hear: it was rebuilt (Reload Window), or stalled behind dark displays.
pub fn list_dropped(state: &AppState) -> Vec<DroppedSsh> {
    let tab_of: std::collections::HashMap<String, String> = state
        .tab_pty_map
        .read()
        .iter()
        .map(|(tab, pty)| (pty.clone(), tab.clone()))
        .collect();
    state
        .terminal_registry
        .read()
        .iter()
        .filter_map(|(pty_id, handle)| {
            let ssh = handle.ssh.dropped.clone()?;
            Some(DroppedSsh {
                pty_id: pty_id.clone(),
                tab_id: tab_of.get(pty_id).cloned().unwrap_or_default(),
                ssh,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn handle() -> SshWatch {
        SshWatch::default()
    }

    #[test]
    fn a_remote_command_start_does_not_forget_the_ssh() {
        let mut h = handle();
        h.live = Some("ssh ews@nova".into());
        // The agent being launched in the remote shell is a C in this stream.
        assert_eq!(on_shell_event(&mut h, 'C', None), Some(Action::ProbeStart));
        assert_eq!(h.live.as_deref(), Some("ssh ews@nova"));
    }

    #[test]
    fn an_exit_takes_the_ssh_so_a_later_command_cannot_inherit_it() {
        let mut h = handle();
        h.live = Some("ssh ews@nova".into());
        // A clean logout…
        let logout = on_shell_event(&mut h, 'D', Some(0));
        assert!(matches!(logout, Some(Action::ConfirmExit { exit_code: Some(0), .. })));
        assert_eq!(h.live, None);
        // …then some unrelated local command that exits 255: no ssh on record, no question asked.
        on_shell_event(&mut h, 'B', None);
        assert_eq!(on_shell_event(&mut h, 'D', Some(255)), None);
        assert_eq!(h.dropped, None);
    }

    #[test]
    fn a_sighting_just_after_an_exit_is_not_believed() {
        // A cached process snapshot from before the exit still shows the ssh that died.
        let mut h = handle();
        h.live = Some("ssh ews@nova".into());
        on_shell_event(&mut h, 'D', Some(0));
        h.note("ssh ews@nova");
        assert_eq!(h.live, None);
        // The confirmation's own sweep, and an ssh maiTerm is about to type, are believed.
        h.set_live("ssh ews@nova");
        assert_eq!(h.live.as_deref(), Some("ssh ews@nova"));
    }

    #[test]
    fn a_255_without_an_ssh_on_record_is_not_a_drop() {
        let mut h = handle();
        assert_eq!(on_shell_event(&mut h, 'D', Some(255)), None);
    }

    #[test]
    fn a_command_start_clears_a_recorded_drop() {
        let mut h = handle();
        h.live = Some("ssh ews@nova".into());
        assert!(on_shell_event(&mut h, 'D', Some(255)).is_some());
        h.dropped = Some("ssh ews@nova".into());
        on_shell_event(&mut h, 'B', None);
        assert_eq!(h.dropped, None);
    }

    #[test]
    fn a_doubled_prompt_hook_asks_one_question_and_takes_nothing_else() {
        // What a shell with two integrations prints when its ssh dies: D;255 A B D;0 A.
        let mut h = handle();
        h.live = Some("ssh ews@nova".into());
        let asked = on_shell_event(&mut h, 'D', Some(255));
        assert_eq!(asked, Some(Action::ConfirmExit { ssh: "ssh ews@nova".into(), exit_code: Some(255) }));
        assert_eq!(on_shell_event(&mut h, 'A', None), None);
        assert_eq!(on_shell_event(&mut h, 'B', None), Some(Action::ProbeStart));
        // The phantom exit has no ssh to ask about, so it cannot answer "0" for the real one.
        assert_eq!(on_shell_event(&mut h, 'D', Some(0)), None);
    }

    #[test]
    fn command_starts_share_one_pending_probe() {
        let mut h = handle();
        assert_eq!(on_shell_event(&mut h, 'B', None), Some(Action::ProbeStart));
        assert_eq!(on_shell_event(&mut h, 'C', None), None);
        h.probe_pending = false;
        assert_eq!(on_shell_event(&mut h, 'B', None), Some(Action::ProbeStart));
    }

    #[test]
    fn the_verdict_separates_a_remote_exit_a_logout_and_a_drop() {
        // The remote shell's own command exited 255 — ssh is still there.
        assert_eq!(verdict(true, Some(false), Some(255)), Verdict::StillConnected);
        // The local shell is back at its prompt after a 255.
        assert_eq!(verdict(false, Some(true), Some(255)), Verdict::Dropped);
        // A logout forwards the remote shell's status.
        assert_eq!(verdict(false, Some(true), Some(0)), Verdict::Ended);
        assert_eq!(verdict(false, Some(true), Some(130)), Verdict::Ended);
        // Another job owns the terminal: not a prompt return, whatever the code.
        assert_eq!(verdict(false, Some(false), Some(255)), Verdict::Ended);
        // No tty signal on this platform: the exit code decides.
        assert_eq!(verdict(false, None, Some(255)), Verdict::Dropped);
    }
}
