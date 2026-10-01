//! Is the shell's command line empty? — for bash, read off the grid (docs/follow-ups.md §6.2).
//!
//! maiTerm types an exited agent's resume command into a shell only when the shell's command
//! line is EMPTY. zsh can be asked (the `ZSH_LINE_PROBE` widget in pty/manager.rs). macOS's
//! /bin/bash is 3.2, which has no `READLINE_LINE`, so it can't be asked. Instead maiTerm's bash
//! integration ends PS1 with an invisible mark, and the reader keeps it ONLY while nothing at all
//! has been drawn since: **any output byte after the mark voids it.** An idle, empty prompt
//! draws nothing; every way the line can stop being empty draws something — a typed key's echo,
//! a paste, type-ahead readline redraws after the prompt, a continuation prompt (PS2), the
//! `(reverse-i-search)` banner, vi-mode's bell. A redraw that re-prints PS1 (Ctrl-L, SIGWINCH, a
//! job notice) re-prints the mark too. Erring this way costs a hold (typed, then deleted back to
//! empty, still holds until the next prompt), never a resume glued onto a command.
//!
//! Input that draws NOTHING is caught by the input side (review of 5cd717a): with the tty's echo
//! off readline doesn't redisplay at all, and a pending Esc / Ctrl-X / vi-command prefix leaves
//! the line empty but would eat the resume's first keys. Those are bytes WRITTEN to the PTY, so
//! the line is empty only if nothing has been written since a BASELINE (`InputBaseline`) — and
//! the baseline can't be the mark: a prefix key typed ahead while the previous command ran was
//! written before the mark, draws nothing after it, and still eats our keys (review of 072d75b).
//! So it is the start of the last command (OSC 133 B/C): anything typed after that is either
//! still waiting for the shell or was read by the command. The one command whose input we know
//! was consumed is the agent's own — so when the agent reports its end (Claude's SessionEnd hook,
//! sent while it still owns the terminal), the baseline moves up to that moment: keys typed INTO
//! the agent don't count, keys typed after it let go do. Echo-off is also reported by the bash
//! integration before the prompt (`MaitermNoEcho`), and that prompt is never empty.
//!
//! This replaced comparing the cursor's position with where it stood at the mark (review of
//! e1cac85): once scrollback is at its cap, "history size + screen line" stops naming a row, and
//! a continuation prompt — or a line exactly a multiple of the width long — scrolled the cursor
//! onto the stored coordinates and read as empty. The position is still checked, as a sanity
//! check, but the decision is "nothing drawn since".

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::Term;
use alacritty_terminal::vte::ansi::Processor;

/// Appended to PS1 by the bash integration, inside `\[ \]` so readline counts it as zero width.
pub const PROMPT_END: &[u8] = b"\x1b]1337;MaitermPromptEnd\x07";
/// A command starting (maiTerm's own B, or the spec's C): the prompt it ended is gone.
const COMMAND_BEGIN: [&[u8]; 2] = [b"\x1b]133;B\x07", b"\x1b]133;C\x07"];

fn cursor_at<T: EventListener>(term: &Term<T>) -> (i64, usize) {
    let grid = term.grid();
    (grid.history_size() as i64 + grid.cursor.point.line.0 as i64, grid.cursor.point.column.0)
}

/// The earliest mark at or after `from`: (index just past it, is it a prompt end).
fn next_mark(data: &[u8], from: usize) -> Option<(usize, bool)> {
    let find = |pat: &[u8]| data[from..].windows(pat.len()).position(|w| w == pat).map(|i| from + i + pat.len());
    let mut best: Option<(usize, bool)> = find(PROMPT_END).map(|e| (e, true));
    for pat in COMMAND_BEGIN {
        if let Some(e) = find(pat) {
            if best.is_none_or(|(b, _)| e < b) {
                best = Some((e, false));
            }
        }
    }
    best
}

/// What is known about the current prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptEnd {
    /// The prompt finished drawing with the cursor here (absolute row, column), and nothing has
    /// been drawn since.
    Marked(i64, usize),
    /// Something was drawn after the mark, or the prompt was drawn with the tty's echo off: the
    /// line is not known to be empty.
    Drawn,
}

/// The PTY's input byte count at the points that bound "input nobody has read yet".
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct InputBaseline {
    /// At the last command start (OSC 133 B/C) — keys before it were the command line itself.
    pub at_begin: u64,
    /// When the agent last reported its end (Claude's SessionEnd), if that came after the last
    /// command start — keys before it went to the agent.
    pub at_agent_end: Option<u64>,
}

impl InputBaseline {
    fn count(&self) -> u64 {
        self.at_agent_end.unwrap_or(self.at_begin)
    }
}

/// Feed `data` to the terminal, noting the cursor at each prompt-end mark and forgetting it at
/// each command start — in byte order, so a mark and a B in the same read settle correctly — and
/// voiding it on ANY byte drawn after it.
///
/// `written`: the PTY's input byte count as this read began — becomes the baseline at a command
/// start. `no_echo`: the shell reported, before this prompt, that the tty's echo is off — keys
/// typed ahead then sit on the line invisibly, so that prompt is never empty (review of 5cd717a).
#[allow(clippy::too_many_arguments)]
pub fn advance<T: EventListener>(
    processor: &mut Processor,
    term: &mut Term<T>,
    prompt_end: &mut Option<PromptEnd>,
    no_echo: &mut bool,
    baseline: &mut InputBaseline,
    written: u64,
    data: &[u8],
) {
    let mut from = 0;
    while let Some((end, is_prompt)) = next_mark(data, from) {
        processor.advance(term, &data[from..end]);
        *prompt_end = if !is_prompt {
            *no_echo = false;
            // Read at the START of this read: a key written during it lands after, and counts.
            *baseline = InputBaseline { at_begin: written, at_agent_end: None };
            None
        } else if std::mem::take(no_echo) {
            Some(PromptEnd::Drawn)
        } else {
            let (row, col) = cursor_at(term);
            Some(PromptEnd::Marked(row, col))
        };
        from = end;
    }
    if from < data.len() && prompt_end.is_some() {
        // Something was drawn after the last mark: whatever it is, the prompt is no longer known
        // to be untouched.
        *prompt_end = Some(PromptEnd::Drawn);
    }
    processor.advance(term, &data[from..]);
}

/// Is the command line empty? `None` when there is no prompt to judge by (no mark yet, or a
/// command has started since). `written`: the PTY's input byte count now — anything written
/// since the baseline (a key, a paste, the phone, a focus report) means not known to be empty.
pub fn line_is_empty<T: EventListener>(
    term: &Term<T>,
    prompt_end: Option<PromptEnd>,
    baseline: InputBaseline,
    written: u64,
) -> Option<bool> {
    let mark = match prompt_end? {
        PromptEnd::Drawn => return Some(false),
        PromptEnd::Marked(..) if written != baseline.count() => return Some(false),
        PromptEnd::Marked(row, col) => (row, col),
    };
    // Nothing has been drawn since the mark, so the cursor can only have moved by input the shell
    // didn't echo. Still check: a mismatch is never "empty".
    if cursor_at(term) != mark {
        return Some(false);
    }
    let grid = term.grid();
    let row = &grid[grid.cursor.point.line];
    let rest_blank = (mark.1..grid.columns()).all(|c| matches!(row[Column(c)].c, ' ' | '\0'));
    Some(rest_blank)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::{test::TermSize, Config};

    fn term() -> (Processor, Term<VoidListener>, Option<PromptEnd>) {
        (Processor::new(), Term::new(Config::default(), &TermSize::new(40, 5), VoidListener), None)
    }

    const PROMPT: &[u8] = b"\x1b]133;A\x07me@host$ \x1b]1337;MaitermPromptEnd\x07";

    /// The reader's call with echo on and nothing written to the PTY.
    fn adv(p: &mut Processor, t: &mut Term<VoidListener>, m: &mut Option<PromptEnd>, data: &[u8]) {
        advance(p, t, m, &mut false, &mut InputBaseline::default(), 0, data);
    }

    #[test]
    fn input_that_draws_nothing_still_counts() {
        // Review of 5cd717a: with the tty's echo off, or a pending Esc / Ctrl-X / vi-command
        // prefix, keys change the line (or will eat ours) and draw nothing. They were WRITTEN.
        let (mut p, mut t, mut m) = term();
        let mut base = InputBaseline::default();
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 100, b"make\r\n\x1b]133;B\x07");
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 100, PROMPT);
        assert_eq!(line_is_empty(&t, m, base, 100), Some(true));
        assert_eq!(line_is_empty(&t, m, base, 101), Some(false), "Esc typed at the prompt");
    }

    #[test]
    fn a_prefix_key_typed_ahead_during_the_last_command_counts() {
        // Review of 072d75b: Esc typed while `sleep` ran is written BEFORE the mark and draws
        // nothing after it — but it was written after the command started.
        let (mut p, mut t, mut m) = term();
        let mut base = InputBaseline::default();
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 10, b"sleep 1\r\n\x1b]133;B\x07");
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 11, b"^[\x1b]133;D;0\x07");
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 11, PROMPT);
        assert_eq!(line_is_empty(&t, m, base, 11), Some(false));
    }

    #[test]
    fn keys_typed_into_the_agent_do_not_count_once_it_reports_its_end() {
        let (mut p, mut t, mut m) = term();
        let mut base = InputBaseline::default();
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 30, b"claude\r\n\x1b]133;B\x07");
        // ...the human types 500 bytes into the agent, then /exit; SessionEnd arrives:
        base.at_agent_end = Some(530);
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 530, b"\x1b]133;D;0\x07");
        advance(&mut p, &mut t, &mut m, &mut false, &mut base, 530, PROMPT);
        assert_eq!(line_is_empty(&t, m, base, 530), Some(true));
        assert_eq!(line_is_empty(&t, m, base, 531), Some(false), "a key after the agent let go");
        // Without the agent's end, those keys count: a killed agent holds.
        let killed = InputBaseline { at_begin: 30, at_agent_end: None };
        assert_eq!(line_is_empty(&t, m, killed, 530), Some(false));
    }

    #[test]
    fn a_prompt_drawn_with_echo_off_is_never_empty() {
        // Type-ahead with echo off sits on the line unseen, written before the mark.
        let (mut p, mut t, mut m) = term();
        let mut no_echo = true;
        let mut base = InputBaseline::default();
        advance(&mut p, &mut t, &mut m, &mut no_echo, &mut base, 0, PROMPT);
        assert_eq!(line_is_empty(&t, m, base, 0), Some(false));
        assert!(!no_echo, "consumed by the prompt it was reported for");
        advance(&mut p, &mut t, &mut m, &mut no_echo, &mut base, 0, b"\r\n\x1b]133;B\x07out\r\n");
        advance(&mut p, &mut t, &mut m, &mut no_echo, &mut base, 0, PROMPT);
        assert_eq!(line_is_empty(&t, m, base, 0), Some(true), "the next prompt, echo back on, is judged afresh");
    }

    #[test]
    fn a_fresh_prompt_is_empty_and_typing_makes_it_not() {
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,PROMPT);
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(true));
        adv(&mut p, &mut t, &mut m,b"git comm");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn type_ahead_drawn_after_the_prompt_is_seen() {
        // readline draws the prompt, then the keys typed while the last command ran.
        let (mut p, mut t, mut m) = term();
        let mut bytes = PROMPT.to_vec();
        bytes.extend_from_slice(b"git commit -am wip");
        adv(&mut p, &mut t, &mut m,&bytes);
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn text_behind_a_cursor_moved_back_to_the_mark_still_counts() {
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,PROMPT);
        adv(&mut p, &mut t, &mut m,b"rm -rf out\x1b[10D"); // typed, then Home-ish
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn a_continuation_prompt_is_not_the_marked_prompt() {
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,PROMPT);
        adv(&mut p, &mut t, &mut m,b"echo a \\\r\n> ");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn a_command_start_forgets_the_mark_even_in_the_same_read() {
        let (mut p, mut t, mut m) = term();
        let mut bytes = PROMPT.to_vec();
        bytes.extend_from_slice(b"\r\n\x1b]133;B\x07");
        adv(&mut p, &mut t, &mut m,&bytes);
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), None);
        // ...and the next prompt is judged on its own mark.
        adv(&mut p, &mut t, &mut m,b"output\r\n\x1b]133;D;0\x07");
        adv(&mut p, &mut t, &mut m,PROMPT);
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(true));
    }

    /// Review of e1cac85: scrollback at its cap (none here), prompt on the bottom row — a scroll
    /// lands the cursor back on the stored coordinates.
    fn capped_at_bottom() -> (Processor, Term<VoidListener>, Option<PromptEnd>) {
        let config = Config { scrolling_history: 0, ..Config::default() };
        let (mut p, mut t, mut m) = (Processor::new(), Term::new(config, &TermSize::new(20, 5), VoidListener), None);
        adv(&mut p, &mut t, &mut m,b"1\r\n2\r\n3\r\n4\r\n");
        adv(&mut p, &mut t, &mut m,b"$ \x1b]1337;MaitermPromptEnd\x07");
        (p, t, m)
    }

    #[test]
    fn a_continuation_prompt_on_a_full_scrollback_is_not_empty() {
        let (mut p, mut t, mut m) = capped_at_bottom();
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(true));
        adv(&mut p, &mut t, &mut m,b"make deploy \\\r\n> ");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn a_line_exactly_the_width_long_on_a_full_scrollback_is_not_empty() {
        let (mut p, mut t, mut m) = capped_at_bottom();
        adv(&mut p, &mut t, &mut m,b"aaaaaaaaaaaaaaaaaa \raa");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn reverse_search_and_the_vi_bell_are_not_an_empty_line() {
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,b"0123456789abcdefghij$ \x1b]1337;MaitermPromptEnd\x07");
        adv(&mut p, &mut t, &mut m,b"\r(reverse-i-search)`': ");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,PROMPT);
        adv(&mut p, &mut t, &mut m,b"\x07");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn real_bash_3_2_output_reads_right() {
        // Captured from macOS /bin/bash 3.2.57 under maiTerm's PROMPT_COMMAND (2026-09-30): a
        // fresh prompt, then `sleep 1` with `git commit -am wip` typed while it ran — readline
        // redraws the type-ahead after the next prompt's mark.
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]1337;MaitermPromptMarks=23558\x07me@host$ \x1b]1337;MaitermPromptEnd\x07");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(true));
        adv(&mut p, &mut t, &mut m,b"sleep 1\r\n\x1b]133;B\x07git commit -am wip\x1b]133;D;0\x07\x1b]133;A\x07me@host$ \x1b]1337;MaitermPromptEnd\x07git commit -am wip");
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(false));
    }

    #[test]
    fn a_redrawn_prompt_moves_the_mark_with_it() {
        // Ctrl-L / SIGWINCH: readline redraws PS1, mark included.
        let (mut p, mut t, mut m) = term();
        adv(&mut p, &mut t, &mut m,PROMPT);
        let mut bytes = b"\x1b[H\x1b[2J".to_vec();
        bytes.extend_from_slice(b"me@host$ \x1b]1337;MaitermPromptEnd\x07");
        adv(&mut p, &mut t, &mut m,&bytes);
        assert_eq!(line_is_empty(&t, m, InputBaseline::default(), 0), Some(true));
    }
}
