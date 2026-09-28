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

#[derive(Clone, Copy, PartialEq, Debug)]
enum Footer {
    Tool,
    Plan,
}

/// Where the dialog's footer starts, when one ends the screen. The plan dialog's wraps onto a
/// second line with its path, so the footer may start up to two lines above the last.
fn footer(lines: &[&str]) -> Option<(usize, Footer)> {
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    (last.saturating_sub(1)..=last).rev().find_map(|i| {
        let l = lines[i];
        if l.contains("Esc to cancel") {
            Some((i, Footer::Tool))
        } else if l.contains("ctrl+g to edit in") {
            Some((i, Footer::Plan))
        } else {
            None
        }
    }).filter(|(i, kind)| *kind == Footer::Plan || *i == last)
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
    let (foot, _) = footer(&lines)?;
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
    // The dialog as drawn: from the rule above its question (the last full-width `─` line
    // before the rows) through the last row.
    let top = lines[..first_row].iter().rposition(|l| l.starts_with('─')).unwrap_or(0);
    let drawn: String = lines[top..foot].concat().chars().filter(|c| !c.is_whitespace()).collect();
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
        // 100 and 60 columns wrap the rows differently; the header path is truncated to the
        // width, so only the rows region is compared here.
        let rows = |s: &str| parse(s).unwrap().options.join("");
        assert_eq!(rows(WRITE_100).replace(' ', ""), rows(WRITE_60).replace(' ', ""));
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
