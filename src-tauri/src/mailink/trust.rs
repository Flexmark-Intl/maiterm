//! Claude Code's workspace-trust dialog, read off a tab's screen.
//!
//! `claude` in a folder it hasn't been told to trust stops at "Accessing workspace: … Quick
//! safety check: Is this a project you created or one you trust?" before any session exists.
//! No hook fires and nothing registers, so to maiTerm the tab is simply dormant. From the phone
//! that was a dead end. Initialize pasted `/maiterm init` into the dialog, its Enter confirmed the
//! highlighted "No, exit", Claude quit, and the next resume stopped at the same dialog.
//!
//! The screen is the only evidence there is, so this reads it, the way the Codex approval overlay
//! is read (`codex_approval_overlay_open`). Verified against Claude Code 2.1.281 in a fresh
//! `git init` folder on a real PTY (fixtures in `testdata/`, rendered from its actual output):
//! - "❯ No, exit" is first and highlighted; "Yes, I trust this folder" is second. A folder whose
//!   settings pre-approve tools shows "No, continue without these permissions" instead, so the
//!   rows are read off the screen rather than assumed.
//! - Down (`CSI B`) moves the highlight, Enter confirms it, Esc exits.
//! - **The dialog's text STAYS on screen after Claude exits**, with the shell prompt below it.
//!   A match on the words alone would report a dialog for every tab where the human already said
//!   No, and a tap would type into the shell. So the dialog counts as open only while its
//!   "Enter to confirm" line is the LAST non-blank line of the viewport.

/// The rows a trust dialog may offer, exactly as rendered. Anything else on an option line is
/// ignored: an unknown row is one the phone must not offer a button for.
const KNOWN_OPTIONS: &[&str] = &[
    "Yes, I trust this folder",
    "No, exit",
    "No, continue without these permissions",
];

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TrustDialog {
    /// The folder being asked about, re-joined across the rows it wrapped onto.
    pub path: String,
    /// The offered rows, in screen order.
    pub options: Vec<String>,
    /// Which of `options` the highlight (`❯`) is on.
    pub highlighted: usize,
}

impl TrustDialog {
    /// The keys that select `choice` and confirm it: arrows from the current highlight, then
    /// Enter. `None` for a label that isn't one of this dialog's rows.
    pub fn keys_for(&self, choice: &str) -> Option<Vec<&'static str>> {
        let target = self.options.iter().position(|o| o == choice)?;
        let mut keys = Vec::new();
        if target > self.highlighted {
            keys.extend(std::iter::repeat_n("down", target - self.highlighted));
        } else {
            keys.extend(std::iter::repeat_n("up", self.highlighted - target));
        }
        keys.push("enter");
        Some(keys)
    }
}

/// Parse a viewport's text (`terminal::render::viewport_text`) for an OPEN trust dialog.
pub(crate) fn parse(screen: &str) -> Option<TrustDialog> {
    let lines: Vec<&str> = screen.lines().collect();
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    // Open means the footer is the last thing on screen. After an exit the shell prompt sits
    // below it, and the words alone would then describe a dialog that is gone.
    if !lines[last].contains("Enter to confirm") {
        return None;
    }
    let start = lines[..last].iter().rposition(|l| l.trim() == "Accessing workspace:")?;
    let body = &lines[start + 1..last];
    body.iter().position(|l| l.contains("Quick safety check"))?;

    // The path is the block right after the header, one or more rows it wrapped onto, ended by
    // a blank line. Wrapping breaks mid-word, so the rows are joined with nothing between them.
    let path: String = body
        .iter()
        .skip_while(|l| l.trim().is_empty())
        .take_while(|l| !l.trim().is_empty())
        .map(|l| l.trim())
        .collect();
    if path.is_empty() {
        return None;
    }

    let mut options = Vec::new();
    let mut highlighted = None;
    for line in body {
        let t = line.trim_start();
        let (marked, rest) = match t.strip_prefix('❯') {
            Some(r) => (true, r.trim_start()),
            None => (false, t),
        };
        // Tolerate a numbered menu ("1. No, exit") should a release add one.
        let label = rest
            .split_once(". ")
            .filter(|(n, _)| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
            .map_or(rest, |(_, l)| l)
            .trim_end();
        if KNOWN_OPTIONS.contains(&label) {
            if marked {
                highlighted = Some(options.len());
            }
            options.push(label.to_string());
        }
    }
    if options.is_empty() {
        return None;
    }
    Some(TrustDialog { path, options, highlighted: highlighted? })
}

#[cfg(test)]
mod tests {
    use super::*;

    const AT_100: &str = include_str!("testdata/trust_dialog_100.txt");
    const AT_60: &str = include_str!("testdata/trust_dialog_60.txt");
    const AFTER_EXIT: &str = include_str!("testdata/trust_dialog_after_exit.txt");

    #[test]
    fn reads_the_real_dialog_at_two_widths() {
        for screen in [AT_100, AT_60] {
            let d = parse(screen).expect("dialog is open");
            assert_eq!(d.options, vec!["No, exit", "Yes, I trust this folder"]);
            assert_eq!(d.highlighted, 0, "No, exit is highlighted by default");
            assert!(d.path.ends_with("/scratchpad/trustprobe3"), "wrapped path rejoined: {}", d.path);
            assert!(!d.path.contains(' '), "no space inserted at the wrap: {}", d.path);
        }
    }

    /// The load-bearing case: Claude exited and left the dialog's text above the shell prompt.
    #[test]
    fn a_dialog_left_on_screen_after_exit_is_not_open() {
        assert!(AFTER_EXIT.contains("Yes, I trust this folder"), "fixture still shows the words");
        assert_eq!(parse(AFTER_EXIT), None);
    }

    #[test]
    fn keys_walk_from_the_highlight_then_confirm() {
        let d = parse(AT_100).unwrap();
        assert_eq!(d.keys_for("Yes, I trust this folder"), Some(vec!["down", "enter"]));
        assert_eq!(d.keys_for("No, exit"), Some(vec!["enter"]));
        assert_eq!(d.keys_for("Yes"), None, "only an exact row label");
        let moved = TrustDialog { highlighted: 1, ..d };
        assert_eq!(moved.keys_for("No, exit"), Some(vec!["up", "enter"]));
    }

    #[test]
    fn the_permissions_variant_and_a_numbered_menu_are_read() {
        let screen = " Accessing workspace:\n\n /work/repo\n\n Quick safety check: trust?\n\n \
                      ❯ 1. Yes, I trust this folder\n   2. No, continue without these permissions\n\n \
                      Enter to confirm · Esc to cancel\n\n";
        let d = parse(screen).unwrap();
        assert_eq!(d.path, "/work/repo");
        assert_eq!(d.options, vec!["Yes, I trust this folder", "No, continue without these permissions"]);
        assert_eq!(d.highlighted, 0);
    }

    #[test]
    fn ordinary_screens_are_not_dialogs() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("SHELL$ ls\nsrc  docs\nSHELL$"), None);
        // No highlight means we can't tell where the arrows start from.
        assert_eq!(parse(" Accessing workspace:\n\n /a\n\n Quick safety check\n\n   No, exit\n\n Enter to confirm"), None);
    }
}
