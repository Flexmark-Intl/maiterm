//! Is there anything typed in the agent's input box? Read off the live screen.
//!
//! Follow-ups (docs/follow-ups.md §6.1) must never paste over a human's half-written prompt: a
//! paste plus CR would submit their draft with the follow-up glued on. Typing into an agent's
//! input fires no hook, so nothing reports a draft. Guessing it from keystroke timestamps was
//! tried twice and failed both ways — it held a relaunched agent's follow-ups forever, then let
//! type-ahead during boot through. The screen is the evidence: this reads the box itself.
//! (Timestamps still guard around this read in `followUps.svelte.ts` — typing in progress, and a
//! key after the gate — and decide where this answers `Unknown`.)
//!
//! Claude Code's layout, from real 2.1.285/2.1.286 screens (see the tests): the input box is a
//! `❯` line with a horizontal rule directly above it, its text running on until the rule
//! below. Submitted prompts in the history above ALSO start with `❯`, but never sit directly
//! under a rule; the trust and permission dialogs use `❯` as a selection cursor, likewise not
//! under a rule. So: the bottom-most `❯` line whose previous line is a rule.
//!
//! Anything else is `Unknown`, and the caller falls back to its weaker evidence. Callers pass
//! text with DIM cells blanked (`screen_text_undimmed`): an empty box can show a dimmed
//! placeholder suggestion, which is not a draft.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputBox {
    Empty,
    HasText,
    /// Not a layout this recognises — another runtime, a dialog, a screen mid-redraw.
    Unknown,
}

const PROMPT: char = '\u{276F}'; // ❯

/// A box rule: `─` from COLUMN 0. Never trimmed — Claude indents every line of a draft by two
/// columns, so a `──` line the human typed or pasted sits at column 2 and must not pass for the
/// box's own rule. Trimming first is what let a pasted screen tail read as an empty box.
fn is_rule(line: &str) -> bool {
    let t = line.trim_end();
    t.starts_with('─') && t.chars().count() >= 10 && t.chars().all(|c| c == '─')
}

/// The box's own prompt line: `❯` at COLUMN 0, for the same reason. A `❯` inside a draft is
/// indented; the dialogs' `❯` cursors are indented too.
fn is_box_prompt(line: &str) -> bool {
    line.starts_with(PROMPT)
}

pub fn parse(screen: &str) -> InputBox {
    let lines: Vec<&str> = screen.lines().collect();
    let Some(start) = (1..lines.len())
        .rev()
        .find(|&i| is_box_prompt(lines[i]) && is_rule(lines[i - 1]))
    else {
        return InputBox::Unknown;
    };
    // The box runs from the prompt line to the next rule. No closing rule means the screen
    // isn't the layout this knows (or is mid-redraw): don't claim either way.
    let Some(end) = (start + 1..lines.len()).find(|&i| is_rule(lines[i])) else {
        return InputBox::Unknown;
    };
    let first = lines[start].trim_start_matches(PROMPT);
    let typed = std::iter::once(first)
        .chain(lines[start + 1..end].iter().copied())
        .any(|l| !l.trim().is_empty());
    if typed { InputBox::HasText } else { InputBox::Empty }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULE: &str = "─────────────────────────────────────────────────────────────────────────────";

    fn screen(body: &[&str]) -> String {
        body.join("\n")
    }

    #[test]
    fn an_empty_box_is_empty() {
        // Claude Code 2.1.285, Haiku, just started (captured 2026-09-30).
        let s = screen(&[
            " ▐▛███▛█   Claude Code v2.1.285",
            "▝▜██████▀  Haiku 4.5 · Claude Max",
            " ▝▝   ▝▝   /Users/Shared/maiterm-demo/web-ui",
            RULE,
            "❯",
            RULE,
            "  DMAC dprusak[…/Shared/maiterm-demo/web-ui] (main) haiku-4-5-20251001 0.0%",
            "  ⏸ manual mode on · ← for agents",
        ]);
        assert_eq!(parse(&s), InputBox::Empty);
    }

    #[test]
    fn a_draft_is_text() {
        let s = screen(&[RULE, "❯ my half-written draft", RULE, "  status line"]);
        assert_eq!(parse(&s), InputBox::HasText);
    }

    #[test]
    fn a_multi_line_draft_counts_its_later_lines() {
        let s = screen(&[RULE, "❯", "  second line of the draft", RULE, "  status"]);
        assert_eq!(parse(&s), InputBox::HasText);
    }

    #[test]
    fn submitted_prompts_in_the_history_are_not_the_box() {
        // After a follow-up was delivered and answered (captured 2026-09-30): the delivered
        // prompt starts with ❯ too, but sits under output, not under a rule.
        let s = screen(&[
            "❯ ⟦FOLLOW-UP⟧ You scheduled this at 13:01 for 13:02 (delivered on time) — your own earlier note, not a new message from your",
            "  human:",
            "  Reply with exactly: FOLLOW-UP RECEIVED. Do nothing else.",
            "⏺ FOLLOW-UP RECEIVED.",
            "✻ Crunched for 1s · done 1:02 PM",
            RULE,
            "❯",
            RULE,
            "  DMAC dprusak[…/Shared/maiterm-demo/web-ui] (main) haiku-4-5-20251001 21.7%",
        ]);
        assert_eq!(parse(&s), InputBox::Empty);
    }

    #[test]
    fn the_trust_dialog_cursor_is_not_the_box() {
        // Claude's workspace-trust dialog (captured 2026-09-30) uses ❯ as a selection cursor.
        let s = screen(&[
            RULE,
            " Accessing workspace:",
            " /Users/Shared/maiterm-demo/web-ui",
            " Security guide",
            " ❯ No, exit",
            "   Yes, I trust this folder",
            " Enter to confirm · Esc to cancel",
        ]);
        assert_eq!(parse(&s), InputBox::Unknown);
    }

    #[test]
    fn no_box_at_all_is_unknown_never_empty() {
        // A shell, another runtime, a half-drawn frame: not evidence of an empty box.
        assert_eq!(parse("dMac[~]# ls\nsrc  package.json\ndMac[~]#"), InputBox::Unknown);
        assert_eq!(parse(&screen(&[RULE, "❯ half a frame"])), InputBox::Unknown);
        assert_eq!(parse(""), InputBox::Unknown);
    }

    #[test]
    fn a_draft_containing_a_rule_and_a_prompt_is_still_text() {
        // Review of 295504a: a pasted screen tail inside the draft. Claude indents a draft's
        // continuation lines, so its `──` and `❯` sit at column 2 — not the box's own. Trimmed
        // matching took them for the box and answered Empty, and the follow-up would have been
        // pasted after the draft and submitted with it.
        let s = screen(&[RULE, "❯ why does it end like this:", "  ──────────────────", "  ❯", RULE, "  status"]);
        assert_eq!(parse(&s), InputBox::HasText);
        // Blank first line, then a user rule: the user's `──` must not close the box.
        let s = screen(&[RULE, "❯", "  ──────────────────", "  notes", RULE, "  status"]);
        assert_eq!(parse(&s), InputBox::HasText);
    }

    #[test]
    fn a_blanked_placeholder_reads_empty() {
        // The caller blanks DIM cells, so a dimmed suggestion arrives as trailing spaces.
        let s = screen(&[RULE, "❯                                  ", RULE]);
        assert_eq!(parse(&s), InputBox::Empty);
    }
}
