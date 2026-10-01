//! Is the shell's command line empty? — for bash, read off the grid (docs/follow-ups.md §6.2).
//!
//! maiTerm types an exited agent's resume command into a shell only when the shell's command
//! line is EMPTY. zsh can be asked (the `ZSH_LINE_PROBE` widget in pty/manager.rs). macOS's
//! /bin/bash is 3.2, which has no `READLINE_LINE`, so it can't be asked. Instead maiTerm's bash
//! integration ends PS1 with an invisible mark, and the reader notes where the cursor stood the
//! instant the prompt finished drawing. The line is empty iff the cursor is still exactly there,
//! with nothing after it on that row.
//!
//! That catches what outside signals missed: type-ahead entered while the previous command ran is
//! drawn by readline AFTER the prompt (cursor moves on); a paste is drawn after it; a continuation
//! prompt (PS2) carries no mark, so the cursor is no longer at the last one. Anything unexpected
//! — a mark split across two reads, scrollback trimmed under it — reads as not empty.

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

/// Feed `data` to the terminal, noting the cursor at each prompt-end mark and forgetting it at
/// each command start — in byte order, so a mark and a B in the same read settle correctly.
pub fn advance<T: EventListener>(
    processor: &mut Processor,
    term: &mut Term<T>,
    prompt_end: &mut Option<(i64, usize)>,
    data: &[u8],
) {
    let mut from = 0;
    while let Some((end, is_prompt)) = next_mark(data, from) {
        processor.advance(term, &data[from..end]);
        *prompt_end = is_prompt.then(|| cursor_at(term));
        from = end;
    }
    processor.advance(term, &data[from..]);
}

/// Is the command line empty? `None` when there is no mark to judge by.
pub fn line_is_empty<T: EventListener>(term: &Term<T>, prompt_end: Option<(i64, usize)>) -> Option<bool> {
    let mark = prompt_end?;
    if cursor_at(term) != mark {
        return Some(false);
    }
    // The cursor is at the mark; anything after it on the row (text left behind by a cursor
    // moved back with Home or ←) is still on the line.
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

    fn term() -> (Processor, Term<VoidListener>, Option<(i64, usize)>) {
        (Processor::new(), Term::new(Config::default(), &TermSize::new(40, 5), VoidListener), None)
    }

    const PROMPT: &[u8] = b"\x1b]133;A\x07me@host$ \x1b]1337;MaitermPromptEnd\x07";

    #[test]
    fn a_fresh_prompt_is_empty_and_typing_makes_it_not() {
        let (mut p, mut t, mut m) = term();
        advance(&mut p, &mut t, &mut m, PROMPT);
        assert_eq!(line_is_empty(&t, m), Some(true));
        advance(&mut p, &mut t, &mut m, b"git comm");
        assert_eq!(line_is_empty(&t, m), Some(false));
    }

    #[test]
    fn type_ahead_drawn_after_the_prompt_is_seen() {
        // readline draws the prompt, then the keys typed while the last command ran.
        let (mut p, mut t, mut m) = term();
        let mut bytes = PROMPT.to_vec();
        bytes.extend_from_slice(b"git commit -am wip");
        advance(&mut p, &mut t, &mut m, &bytes);
        assert_eq!(line_is_empty(&t, m), Some(false));
    }

    #[test]
    fn text_behind_a_cursor_moved_back_to_the_mark_still_counts() {
        let (mut p, mut t, mut m) = term();
        advance(&mut p, &mut t, &mut m, PROMPT);
        advance(&mut p, &mut t, &mut m, b"rm -rf out\x1b[10D"); // typed, then Home-ish
        assert_eq!(line_is_empty(&t, m), Some(false));
    }

    #[test]
    fn a_continuation_prompt_is_not_the_marked_prompt() {
        let (mut p, mut t, mut m) = term();
        advance(&mut p, &mut t, &mut m, PROMPT);
        advance(&mut p, &mut t, &mut m, b"echo a \\\r\n> ");
        assert_eq!(line_is_empty(&t, m), Some(false));
    }

    #[test]
    fn a_command_start_forgets_the_mark_even_in_the_same_read() {
        let (mut p, mut t, mut m) = term();
        let mut bytes = PROMPT.to_vec();
        bytes.extend_from_slice(b"\r\n\x1b]133;B\x07");
        advance(&mut p, &mut t, &mut m, &bytes);
        assert_eq!(line_is_empty(&t, m), None);
        // ...and the next prompt is judged on its own mark.
        advance(&mut p, &mut t, &mut m, b"output\r\n\x1b]133;D;0\x07");
        advance(&mut p, &mut t, &mut m, PROMPT);
        assert_eq!(line_is_empty(&t, m), Some(true));
    }

    #[test]
    fn real_bash_3_2_output_reads_right() {
        // Captured from macOS /bin/bash 3.2.57 under maiTerm's PROMPT_COMMAND (2026-09-30): a
        // fresh prompt, then `sleep 1` with `git commit -am wip` typed while it ran — readline
        // redraws the type-ahead after the next prompt's mark.
        let (mut p, mut t, mut m) = term();
        advance(&mut p, &mut t, &mut m, b"\x1b]133;D;0\x07\x1b]133;A\x07\x1b]1337;MaitermPromptMarks=23558\x07me@host$ \x1b]1337;MaitermPromptEnd\x07");
        assert_eq!(line_is_empty(&t, m), Some(true));
        advance(&mut p, &mut t, &mut m, b"sleep 1\r\n\x1b]133;B\x07git commit -am wip\x1b]133;D;0\x07\x1b]133;A\x07me@host$ \x1b]1337;MaitermPromptEnd\x07git commit -am wip");
        assert_eq!(line_is_empty(&t, m), Some(false));
    }

    #[test]
    fn a_redrawn_prompt_moves_the_mark_with_it() {
        // Ctrl-L / SIGWINCH: readline redraws PS1, mark included.
        let (mut p, mut t, mut m) = term();
        advance(&mut p, &mut t, &mut m, PROMPT);
        let mut bytes = b"\x1b[H\x1b[2J".to_vec();
        bytes.extend_from_slice(b"me@host$ \x1b]1337;MaitermPromptEnd\x07");
        advance(&mut p, &mut t, &mut m, &bytes);
        assert_eq!(line_is_empty(&t, m), Some(true));
    }
}
