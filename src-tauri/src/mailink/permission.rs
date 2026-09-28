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
//! - The dialog ends with "Esc to cancel · Tab to amend", and Esc rejects.
//! - Like the trust dialog (`trust.rs`), it counts as open only while that footer is the LAST
//!   non-blank line of the screen, so text left in the scrollback never reads as a live dialog.

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct PermissionDialog {
    /// The row labels, in screen order, wrapped lines rejoined with a space.
    pub options: Vec<String>,
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
pub(crate) fn footer_open(screen: &str) -> bool {
    screen.lines().rev().find(|l| !l.trim().is_empty()).is_some_and(|l| l.contains("Esc to cancel"))
}

/// The key for a fallback answer: row 1 is "Yes" on every dialog seen, and Esc rejects. `None`
/// for anything else, which is refused rather than guessed. Only while `footer_open`.
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
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    if !lines[last].contains("Esc to cancel") {
        return None;
    }
    // Walk up from the footer: continuation lines collect until the row they belong to, and
    // the walk ends at row 1.
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut cont: Vec<&str> = Vec::new();
    for line in lines[..last].iter().rev() {
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
    Some(PermissionDialog { options: rows.into_iter().map(|(_, l)| l).collect() })
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASH_100: &str = include_str!("testdata/permission_bash_100.txt");
    const WRITE_100: &str = include_str!("testdata/permission_write_100.txt");
    const WRITE_60: &str = include_str!("testdata/permission_write_60.txt");
    const TWO_ROWS: &str = include_str!("testdata/permission_two_rows_100.txt");

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
