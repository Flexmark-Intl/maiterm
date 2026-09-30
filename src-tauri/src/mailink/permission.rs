//! Claude Code's tool-permission dialog, read off a tab's screen.
//!
//! The permission hook says a dialog is open but not what it offers, and the rows are not
//! fixed. `/respond` used to map the phone's choice to a fixed digit (Yes 1, "don't ask again"
//! 2, anything else 3), and on a two-row dialog ("1. Yes / 2. No", as Write shows for a file in
//! the project) the phone's "Yes, don't ask again" pressed 2, which is No: an approval the human
//! tapped went to the agent as a rejection. So the rows are read off the screen, the card offers
//! exactly those, and `/respond` presses the digit of the row the human chose.
//!
//! Verified against Claude Code 2.1.283 on a real PTY (fixtures in `testdata/`):
//! - The rows are numbered and a digit selects AND confirms its row.
//! - Row 2 varies with the request ("Yes, and always allow access to <dir> from this project",
//!   "Yes, and switch to accept edits …; Yes, and always allow access to …") and wraps onto
//!   continuation lines indented past the number.
//! - A tool dialog ends with "Esc to cancel · Tab to amend", and Esc rejects. The plan-approval
//!   dialog (ExitPlanMode) ends instead with "ctrl+g to edit in <editor> · <plan path>", which
//!   wraps, and its row 1 is "Yes, auto-accept edits": a mode change, never a plain Yes.
//! - Like the trust dialog (`trust.rs`), it counts as open only while its footer ends the
//!   screen, so text left in the scrollback never reads as a live dialog.
//! - The hooks can't tell one Claude dialog from the next (stacked dialogs, back-to-back asks),
//!   so the screen also gives each its id: a digest of the dialog as drawn.

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PermissionDialog {
    /// The row labels, in screen order, wrapped lines rejoined with a space.
    pub options: Vec<String>,
    /// Digest of the dialog as drawn (what is asked, and the rows), whitespace removed so a
    /// re-wrap at another width doesn't change it. Two dialogs asking the identical thing share
    /// it, which is harmless: an answer means the same on either.
    pub digest: String,
}

impl PermissionDialog {
    /// The key that answers with `choice`: the digit of the row whose label it is, matched
    /// exactly (case aside), or a bare digit naming a row on screen. `None` for anything else:
    /// a label the dialog doesn't show must never be guessed onto a row.
    pub fn key_for(&self, choice: &str) -> Option<String> {
        let c = choice.trim();
        let row = self
            .options
            .iter()
            .position(|o| o.eq_ignore_ascii_case(c))
            .or_else(|| c.parse::<usize>().ok().filter(|n| (1..=self.options.len()).contains(n)).map(|n| n - 1))?;
        Some((row + 1).to_string())
    }
}

/// The card's options when the screen can't be read (a pane too narrow, a dialog shape this
/// build doesn't know): the two answers that mean the same thing on every Claude dialog.
pub(crate) const FALLBACK_OPTIONS: [&str; 2] = ["Yes", "No"];

/// Whether the screen ends in a permission dialog's footer, whatever its rows look like. The
/// fallback keys are sent only then: the hook's permission state outlives the dialog (it holds
/// until PostToolUse, so through the whole approved command), and an Esc typed into a running
/// agent interrupts it.
/// Only a tool dialog's footer counts: on the plan dialog row 1 changes the permission mode.
pub(crate) fn footer_open(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    footer(&lines).is_some_and(|(_, kind)| kind == Footer::Tool)
}

/// Whether any Claude permission dialog (a tool's or the plan's) ends the screen, readable
/// rows or not. The screen is the only prompt-open signal that is both prompt and exact: the
/// hook's permission state arrives 6 s after the dialog opens and holds until the approved
/// tool finishes.
pub(crate) fn dialog_open(screen: &str) -> bool {
    let lines: Vec<&str> = screen.lines().collect();
    footer(&lines).is_some()
}

/// Whether the screen ends in ANY Claude dialog's footer (a permission, a question, a picker,
/// the trust dialog). For paths that must not type: text and its CR go into whatever dialog is
/// up, never to the agent.
pub(crate) fn any_dialog_open(screen: &str) -> bool {
    dialog_open(screen)
        || screen.lines().rev().find(|l| !l.trim().is_empty()).is_some_and(|l| l.contains("Esc to cancel"))
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Footer {
    Tool,
    Plan,
    /// No footer at all: WebFetch's dialog (and, per the 2.1.283 bundle, sandbox-network and
    /// EnterPlanMode's) ends at its last row. The "footer" index is one past the screen's last
    /// line, so the rows are everything above it.
    Bare,
}

/// How many lines the plan footer's path may wrap onto: it is the per-account config dir plus a
/// plan slug, near 175 characters, so three or four lines in a narrow split.
const PLAN_FOOTER_MAX_LINES: usize = 6;

/// Where the dialog's footer starts, when one ends the screen. A tool dialog's footer is the
/// last non-blank line. The plan dialog's wraps with its path, so it may start a few lines up,
/// provided nothing between it and the end is blank or a row.
fn footer(lines: &[&str]) -> Option<(usize, Footer)> {
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    // "Esc to cancel" is Claude's generic dialog footer. The AskUserQuestion selector
    // ("Enter to select · ↑/↓ to navigate · Esc to cancel"), the trust dialog ("Enter to
    // confirm · Esc to cancel") and pickers like /model draw it too, each after an "Enter to"
    // hint. A tool permission's footer has none ("Esc to cancel · Tab to amend"): read as one,
    // an ask's "No" would press Esc into the agent's question.
    if lines[last].contains("Esc to cancel") && !lines[last].contains("Enter to") {
        return Some((last, Footer::Tool));
    }
    for i in (last.saturating_sub(PLAN_FOOTER_MAX_LINES - 1)..=last).rev() {
        let l = lines[i];
        if l.contains("ctrl+g to edit in") {
            return Some((i, Footer::Plan));
        }
        if l.trim().is_empty() || row(l).is_some() {
            break;
        }
    }
    bare_dialog(lines, last)
}

/// A dialog with no footer: the screen ends in its rows (or a row's wrapped continuation),
/// which run unbroken up to row 1, and the line above row 1 asks "Do you want …". The question
/// is required because numbered rows at the bottom of a screen are otherwise ordinary output.
fn bare_dialog(lines: &[&str], last: usize) -> Option<(usize, Footer)> {
    let mut i = last;
    loop {
        let l = lines[i];
        match row(l) {
            Some((1, _)) => {
                // The question may wrap in a narrow pane ("… fetch this" / " content?"), so it
                // is looked for in the few lines above row 1, stopping at the dialog's rule.
                let asks = lines[..i]
                    .iter()
                    .rev()
                    .filter(|l| !l.trim().is_empty())
                    .take(3)
                    .take_while(|l| !l.starts_with('─'))
                    .any(|l| l.contains("Do you want"));
                return asks.then_some((last + 1, Footer::Bare));
            }
            Some(_) => {}
            None if l.starts_with("    ") && !l.trim().is_empty() => {}
            None => return None,
        }
        i = i.checked_sub(1)?;
    }
}

/// The key for a fallback answer: row 1 is "Yes" on every tool dialog seen, and Esc rejects.
/// `None` for anything else, which is refused rather than guessed. Only while `footer_open`.
pub(crate) fn fallback_key(choice: &str) -> Option<&'static str> {
    match choice.trim().to_ascii_lowercase().as_str() {
        "yes" | "1" => Some("1"),
        "no" => Some("\x1b"),
        _ => None,
    }
}

/// A numbered row: at most three columns of indent, an optional `❯` highlight, then `N. label`.
fn row(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let t = line.trim_start();
    let t = t.strip_prefix('❯').map_or(t, str::trim_start);
    let (n, label) = t.split_once(". ")?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((n.parse().ok()?, label.trim()))
}

/// Parse a screen's text (`terminal::render::screen_text`) for an OPEN permission dialog.
pub(crate) fn parse(screen: &str) -> Option<PermissionDialog> {
    let lines: Vec<&str> = screen.lines().collect();
    let (foot, kind) = footer(&lines)?;
    // Walk up from the footer: continuation lines collect until the row they belong to, and
    // the walk ends at row 1.
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut cont: Vec<&str> = Vec::new();
    let mut first_row = foot;
    for (i, line) in lines[..foot].iter().enumerate().rev() {
        first_row = i;
        if line.trim().is_empty() {
            if rows.is_empty() && cont.is_empty() {
                continue;
            }
            return None;
        }
        if let Some((n, label)) = row(line) {
            let mut text = label.to_string();
            for c in cont.drain(..).rev() {
                text.push(' ');
                text.push_str(c);
            }
            rows.push((n, text));
            if n == 1 {
                break;
            }
        } else if line.starts_with("    ") {
            cont.push(line.trim());
        } else {
            return None;
        }
    }
    rows.reverse();
    let numbered_in_order = rows.iter().enumerate().all(|(i, (n, _))| *n == i + 1);
    if rows.len() < 2 || !cont.is_empty() || !numbered_in_order {
        return None;
    }
    // The dialog as drawn: from the rule above it through the last row. A tool dialog has one
    // full-width `─` rule, above what it asks. The plan dialog has two, and the plan sits
    // between them: without it every plan would share one id, and a tap on an old plan's card
    // would approve the next. Only what the dialog SAYS goes in: whitespace and the rules
    // (both change with the width) and the `❯` highlight (arrows at the desk move it) are
    // dropped, so a resize or a moved highlight keeps the id.
    let rule_above = |end: usize| lines[..end].iter().rposition(|l| l.starts_with('─'));
    let mut top = rule_above(first_row).unwrap_or(0);
    if kind == Footer::Plan {
        top = rule_above(top).unwrap_or(0);
    }
    // Also dropped: the unattended-session countdown ("Claude Code will automatically deny this
    // request in 1:31"). It ticks every second, so with it in, the id changed under every card:
    // each answer was refused as stale, and the card's fresh-prompt guard restarted every poll.
    let drawn: String = lines[top..foot]
        .iter()
        .filter(|l| !l.contains("automatically deny this request"))
        .copied()
        .collect::<String>()
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '─' | '╌' | '❯'))
        .collect();
    let digest = super::sha256_hex(drawn.as_bytes())[..12].to_string();
    Some(PermissionDialog { options: rows.into_iter().map(|(_, l)| l).collect(), digest })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASH_100: &str = include_str!("testdata/permission_bash_100.txt");
    const WRITE_100: &str = include_str!("testdata/permission_write_100.txt");
    const WRITE_60: &str = include_str!("testdata/permission_write_60.txt");
    const TWO_ROWS: &str = include_str!("testdata/permission_two_rows_100.txt");
    const PLAN_100: &str = include_str!("testdata/permission_plan_100.txt");
    const WEBFETCH_100: &str = include_str!("testdata/permission_webfetch_100.txt");

    /// WebFetch's dialog has no footer: it ends at its last row.
    #[test]
    fn a_dialog_with_no_footer_is_read_from_its_question_and_rows() {
        let d = parse(WEBFETCH_100).expect("open");
        assert_eq!(d.options, vec!["Yes", "Yes, and don't ask again for example.com", "No, and tell Claude what to do differently (esc)"]);
        assert_eq!(d.key_for("No, and tell Claude what to do differently (esc)").as_deref(), Some("3"));
        assert!(dialog_open(WEBFETCH_100));
        // In a narrow pane the question wraps.
        let narrow = WEBFETCH_100.replace("Do you want to allow Claude to fetch this content?", "Do you want to allow Claude to fetch\n this content?");
        assert!(parse(&narrow).is_some());
        // Numbered rows at the bottom of ordinary output are not a dialog.
        assert!(!dialog_open("Steps:\n 1. build\n 2. test\n"));
        assert_eq!(parse("Steps:\n 1. build\n 2. test\n"), None);
    }

    #[test]
    fn reads_the_real_three_row_dialogs() {
        let d = parse(BASH_100).expect("open");
        assert_eq!(d.options, vec!["Yes", "Yes, and always allow access to /private/tmp/work/repo from this project", "No"]);
        for screen in [WRITE_100, WRITE_60] {
            let d = parse(screen).expect("open");
            assert_eq!(d.options.len(), 3);
            assert_eq!(d.options[0], "Yes");
            assert_eq!(d.options[2], "No");
            assert!(d.options[1].starts_with("Yes, and switch to accept edits"), "{}", d.options[1]);
            assert!(d.options[1].ends_with("for this session (shift+tab)"), "wrapped rows rejoined: {}", d.options[1]);
        }
    }

    /// The dialog behind the phantom rejections: the phone's "Yes, don't ask again" pressed 2.
    #[test]
    fn a_two_row_dialog_offers_two_rows_and_refuses_the_rest() {
        let d = parse(TWO_ROWS).expect("open");
        assert_eq!(d.options, vec!["Yes", "No"]);
        assert_eq!(d.key_for("Yes").as_deref(), Some("1"));
        assert_eq!(d.key_for("No").as_deref(), Some("2"));
        assert_eq!(d.key_for("Yes, don't ask again"), None);
        assert_eq!(d.key_for("3"), None, "a digit names a row on screen or nothing");
        assert_eq!(d.key_for("2").as_deref(), Some("2"));
    }

    /// ExitPlanMode's dialog: a different footer that wraps, and a row 1 that changes the mode.
    #[test]
    fn the_plan_dialog_offers_its_own_rows_and_no_fallback() {
        let d = parse(PLAN_100).expect("open");
        assert_eq!(d.options[0], "Yes, auto-accept edits");
        assert_eq!(d.options[1], "Yes, manually approve edits");
        assert!(d.options[2].starts_with("Tell Claude what to change"));
        assert_eq!(d.key_for("Yes"), None, "a bare Yes is not a row here");
        assert_eq!(d.key_for("Yes, manually approve edits").as_deref(), Some("2"));
        // If its rows couldn't be read, a fallback Yes would press 1 and change the mode.
        assert!(!footer_open(PLAN_100));
    }

    /// Each dialog carries its own id, stable across a re-wrap, so an answered card's id never
    /// hides the next dialog.
    #[test]
    fn the_digest_tells_dialogs_apart_but_not_widths() {
        let ids: Vec<String> = [BASH_100, WRITE_100, TWO_ROWS, PLAN_100].iter().map(|s| parse(s).unwrap().digest).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        let edited = TWO_ROWS.replace("notes.md", "other.md");
        assert_ne!(parse(&edited).unwrap().digest, parse(TWO_ROWS).unwrap().digest);
        // The same dialog at 100 and 60 columns: rules and wraps differ, the id doesn't.
        assert_eq!(parse(WRITE_100).unwrap().digest, parse(WRITE_60).unwrap().digest);
        // A human moving the highlight at the desk keeps the id.
        let moved = TWO_ROWS.replace(" ❯ 1. Yes\n   2. No", "   1. Yes\n ❯ 2. No");
        assert_ne!(moved, TWO_ROWS);
        assert_eq!(parse(&moved).unwrap().digest, parse(TWO_ROWS).unwrap().digest);
    }

    /// The unattended-session countdown ticks every second; the dialog it sits in is the same one.
    #[test]
    fn the_auto_deny_countdown_does_not_change_the_id() {
        let at = |t: &str| TWO_ROWS.replace(
            " Do you want to create notes.md?",
            &format!(" ⚠ Claude Code will automatically deny this request in {t}, to avoid blocking progress on an unattended session\n\n Do you want to create notes.md?"),
        );
        assert_ne!(at("1:31"), TWO_ROWS);
        assert_eq!(parse(&at("1:31")).unwrap().digest, parse(&at("1:30")).unwrap().digest);
        assert_eq!(parse(&at("1:31")).unwrap().options, vec!["Yes", "No"]);
    }

    /// The plan is what a plan dialog asks, so two plans are two ids: a tap on an old plan's
    /// card must never approve the next one.
    #[test]
    fn two_plans_are_two_dialogs() {
        let other = PLAN_100.replace("Create an empty file named hello.txt", "Delete the whole repository");
        assert_ne!(parse(&other).unwrap().digest, parse(PLAN_100).unwrap().digest);
    }

    /// In a narrow split the plan footer's path wraps onto several lines.
    #[test]
    fn a_plan_footer_wrapped_onto_three_lines_is_read() {
        let narrow = PLAN_100.replace(
            " ctrl+g to edit in nano · ~/Library/Application Support/com.example.app/accounts/claude/0000000-0000-0\n",
            " ctrl+g to edit in nano · ~/Library/Application Support/com.example\n .app/accounts/claude/0000000-0000-0\n",
        );
        assert_ne!(narrow, PLAN_100);
        assert_eq!(parse(&narrow).unwrap().options, parse(PLAN_100).unwrap().options);
    }

    /// Positions carry no meaning: a dialog that defaults to No lists it first, and a Bash dialog
    /// can put "Yes, and switch to auto mode" where the fixed map pressed 3 for No.
    #[test]
    fn keys_follow_the_label_not_the_position() {
        let no_first = " Do you want to proceed?\n ❯ 1. No\n   2. Yes\n   3. Yes, and switch to auto mode\n\n Esc to cancel\n";
        let d = parse(no_first).unwrap();
        assert_eq!(d.key_for("Yes").as_deref(), Some("2"));
        assert_eq!(d.key_for("No").as_deref(), Some("1"));
        let four = " Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for: npm test\n   3. Yes, and switch to auto mode\n   4. No\n\n Esc to cancel\n";
        assert_eq!(parse(four).unwrap().key_for("No").as_deref(), Some("4"));
    }

    #[test]
    fn a_row_is_chosen_by_its_exact_label() {
        let d = parse(BASH_100).unwrap();
        assert_eq!(d.key_for("yes").as_deref(), Some("1"));
        assert_eq!(d.key_for(&d.options[1].clone()).as_deref(), Some("2"));
        assert_eq!(d.key_for("Yes, and always allow access"), None, "no prefix matching");
    }

    #[test]
    fn text_left_above_a_prompt_is_not_a_dialog() {
        let gone = format!("{}\nSHELL$ ", TWO_ROWS.trim_end());
        assert_eq!(parse(&gone), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("  1 x\n Esc to cancel"), None, "a diff line is not a row");
    }

    /// What the composer and the responder ask before typing: any dialog, rows readable or not.
    #[test]
    fn a_dialog_is_open_while_either_footer_ends_the_screen() {
        for s in [TWO_ROWS, BASH_100, PLAN_100] {
            assert!(dialog_open(s));
        }
        assert!(dialog_open(" rows this build can't read\n Esc to cancel · Tab to amend\n"));
        // Other Claude dialogs share "Esc to cancel", after an "Enter to" hint.
        let ask = " ❯ 1. Blue\n   2. Green\n ────\n   3. Chat about this\n\n Enter to select · ↑/↓ to navigate · Esc to cancel\n";
        assert!(!dialog_open(ask));
        assert!(!footer_open(ask));
        assert_eq!(parse(ask), None);
        assert!(!dialog_open(" ❯ 1. No, exit\n   2. Yes, I trust this folder\n\n Enter to confirm · Esc to cancel\n"));
        assert!(!dialog_open("⏺ Bash(npm test)\n  ⎿  Running…\n\n✻ Working… (esc to interrupt)\n"));
        assert!(!dialog_open(&format!("{}\nSHELL$ ", TWO_ROWS.trim_end())));
    }

    #[test]
    fn the_fallback_needs_a_dialog_footer_on_screen() {
        assert!(footer_open(TWO_ROWS));
        assert!(footer_open(" something unreadable\n Esc to cancel · Tab to amend\n\n"));
        // An approved command running under a permission state that hasn't cleared yet.
        assert!(!footer_open("⏺ Bash(npm test)\n  ⎿  Running…\n\n✻ Working… (esc to interrupt)\n"));
        assert!(!footer_open(""));
    }

    #[test]
    fn the_fallback_only_knows_yes_and_no() {
        assert_eq!(fallback_key("Yes"), Some("1"));
        assert_eq!(fallback_key("No"), Some("\x1b"));
        assert_eq!(fallback_key("Yes, don't ask again"), None);
        assert_eq!(fallback_key("3"), None);
    }
}
