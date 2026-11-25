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
    /// A tool's dialog, the plan dialog, or one with no footer (WebFetch, sandbox network).
    pub shape: DialogShape,
    /// What the dialog asks, from its rule down to row 1, its lines trimmed and joined.
    pub text: String,
}

/// Which key answers a Claude permission dialog with a choice (`permission_key_for`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PermissionKey {
    /// The row whose label it is: its digit, and the label as drawn.
    Row { key: String, label: String },
    /// Esc on a tool dialog: a reject, never an approval.
    Esc,
    /// The rows can't be read but a tool dialog's footer ends the screen: Yes or No only.
    Fallback(&'static str),
}

impl PermissionKey {
    pub fn bytes(&self) -> String {
        match self {
            PermissionKey::Row { key, .. } => key.clone(),
            PermissionKey::Esc => "\x1b".to_string(),
            PermissionKey::Fallback(k) => k.to_string(),
        }
    }

    /// What it means to the maiterm-tab mod, which takes Yes and No by meaning: a row's label, or
    /// No for Esc. None for a fallback, which goes in as its key.
    pub fn meaning(&self) -> Option<&str> {
        match self {
            PermissionKey::Row { label, .. } => Some(label),
            PermissionKey::Esc => Some("No"),
            PermissionKey::Fallback(_) => None,
        }
    }
}

/// The key a choice presses on the dialog read off the screen (`dialog`), or why not. `Esc` is
/// accepted only on a tool dialog, as its reject; a label must be a row exactly; with no rows
/// read, only Yes and No, and only while a tool dialog's footer ends the screen.
pub(crate) fn permission_key_for(dialog: Option<&PermissionDialog>, footer_open: bool, choice: &str) -> Result<PermissionKey, &'static str> {
    let esc = choice.trim().eq_ignore_ascii_case("esc");
    match dialog {
        Some(d) if esc && d.shape == DialogShape::Tool => Ok(PermissionKey::Esc),
        Some(d) => match d.key_for(choice) {
            Some(key) => {
                let label = key.parse::<usize>().ok().and_then(|n| d.options.get(n.wrapping_sub(1))).cloned().unwrap_or_default();
                Ok(PermissionKey::Row { key, label })
            }
            None => Err("that isn't one of the options the dialog shows"),
        },
        None if !footer_open => Err("that dialog is no longer open in the terminal"),
        None if esc => Ok(PermissionKey::Esc),
        None => fallback_key(choice).map(PermissionKey::Fallback).ok_or("the dialog can't be read off the screen, so only Yes or No can be sent"),
    }
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
    footer(&lines).is_some_and(|(_, kind)| kind == DialogShape::Tool)
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

/// Which kind of dialog it is, by how it ends. In memory only, never persisted or on the wire.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum DialogShape {
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
/// provided nothing between it and the end is blank or a row. Antigravity (`agy`) renders
/// `↑/↓ Navigate · tab Amend` (or no `tab Amend` footer on some command prompts) above 0–2
/// status-bar lines (default `esc to cancel ... auto • ...`, custom `statusLine`, or both when
/// `stack_with_default: true`).
fn footer(lines: &[&str]) -> Option<(usize, DialogShape)> {
    let last = lines.iter().rposition(|l| !l.trim().is_empty())?;
    // "Esc to cancel" is Claude's generic dialog footer. The AskUserQuestion selector
    // ("Enter to select · ↑/↓ to navigate · Esc to cancel"), the trust dialog ("Enter to
    // confirm · Esc to cancel") and pickers like /model draw it too, each after an "Enter to"
    // hint. A tool permission's footer has none ("Esc to cancel · Tab to amend"): read as one,
    // an ask's "No" would press Esc into the agent's question.
    if lines[last].contains("Esc to cancel") && !lines[last].contains("Enter to") {
        return Some((last, DialogShape::Tool));
    }
    // Antigravity (`agy`) tool-permission dialog with `tab Amend`: sits on `last` or up to 2
    // non-blank status-bar lines above `last` (default status bar, custom `statusLine`, or both
    // stacked). Stop walking upward at any rule, prompt, or numbered row so old scrollback above
    // an active input box or shell prompt never matches.
    let mut non_blank_seen = 0usize;
    for (idx, l) in lines[..=last].iter().enumerate().rev() {
        let trimmed = l.trim();
        if trimmed.is_empty() {
            continue;
        }
        if l.contains("tab Amend") {
            return Some((idx, DialogShape::Tool));
        }
        if is_non_status_trailing_line(l) || row(l).is_some() {
            break;
        }
        non_blank_seen += 1;
        if non_blank_seen > 2 {
            break;
        }
    }
    for i in (last.saturating_sub(PLAN_FOOTER_MAX_LINES - 1)..=last).rev() {
        let l = lines[i];
        if l.contains("ctrl+g to edit in") {
            return Some((i, DialogShape::Plan));
        }
        if l.trim().is_empty() || row(l).is_some() {
            break;
        }
    }
    if let Some(bare) = bare_dialog(lines, last) {
        return Some(bare);
    }
    agy_permission_dialog(lines, last)
}

/// True when a trailing line below a candidate `agy` dialog clearly belongs to an input box,
/// spinner/tool output, another dialog type, or a shell prompt rather than `agy`'s hint/status bar.
fn is_non_status_trailing_line(line: &str) -> bool {
    let t = line.trim();
    if t.starts_with('─')
        || t.starts_with('❯')
        || t.starts_with('>')
        || t.chars().next().is_some_and(|c| {
            ('\u{2800}'..='\u{28ff}').contains(&c) || matches!(c, '✻' | '●' | '○' | '⏺' | '⎿' | '└')
        })
        || t.contains("Esc to cancel")
        || t.contains("Enter to confirm")
        || t.contains("enter Confirm")
        || t.contains("Enter to select")
    {
        return true;
    }
    // Shell prompts (`SHELL$ `, `root# `, `host% `) end with a prompt sigil; distinguish a zsh
    // `%` prompt from a status-line percentage like `12.3%` (digit before `%`).
    if t.ends_with('$') || t.ends_with('#') || t.ends_with('❯') {
        return true;
    }
    if let Some(before_pct) = t.strip_suffix('%') {
        if !before_pct.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            return true;
        }
    }
    false
}

/// Try to parse an unbroken `agy` permission block ending at `end_idx` (last option row or its
/// wrapped continuation). Returns `Some((end_idx + 1, DialogShape::Tool))` when rows `1..=N`
/// start with `Yes` / include `No` and sit directly below an `agy` permission header.
fn try_agy_rows_ending_at(lines: &[&str], end_idx: usize) -> Option<(usize, DialogShape)> {
    let mut rows: Vec<(usize, String)> = Vec::new();
    let mut cont: Vec<&str> = Vec::new();
    let mut first_row = end_idx;
    let mut walked_to_one = false;
    for (j, rl) in lines[..=end_idx].iter().enumerate().rev() {
        first_row = j;
        if rl.trim().is_empty() {
            return None;
        }
        if let Some((n, label)) = row(rl) {
            let mut text = label.to_string();
            for c in cont.drain(..).rev() {
                text.push(' ');
                text.push_str(c);
            }
            rows.push((n, text));
            if n == 1 {
                walked_to_one = true;
                break;
            }
        } else if rl.starts_with("    ") {
            cont.push(rl.trim());
        } else {
            return None;
        }
    }
    if !walked_to_one || !cont.is_empty() || rows.len() < 2 {
        return None;
    }
    rows.reverse();
    if !rows.iter().enumerate().all(|(i, (n, _))| *n == i + 1) {
        return None;
    }
    let has_yes = rows[0].1.starts_with("Yes");
    let has_no = rows.iter().skip(1).any(|(_, l)| l.starts_with("No"));
    if !has_yes || !has_no {
        return None;
    }
    let header_lines: Vec<&str> = lines[..first_row]
        .iter()
        .rev()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .take_while(|s| {
            !s.starts_with('─')
                && !s.chars().next().is_some_and(|c| {
                    ('\u{2800}'..='\u{28ff}').contains(&c) || matches!(c, '○' | '●' | '└')
                })
        })
        .take(8)
        .collect();
    let asks = header_lines.iter().enumerate().any(|(offset, h)| {
        h.contains("Run this command?")
            || h.contains("Allow calling this tool?")
            || h.contains("Requesting permission for:")
            || (offset < 3 && h.starts_with("Allow ") && h.ends_with('?'))
    });
    asks.then_some((end_idx + 1, DialogShape::Tool))
}

/// An Antigravity (`agy`) permission prompt rendered without a `tab Amend` line (e.g.
/// `Run this command?` above `> 1. Yes, run command` … `4. No, cancel`, followed by 0–3 trailing
/// hint/status-bar lines). Requires strict row numbering (`1..=N`), a `Yes…` first row, at least
/// one `No…` row, and an `agy` permission question directly above row 1 so ordinary lists and
/// non-permission pickers never match.
fn agy_permission_dialog(lines: &[&str], last: usize) -> Option<(usize, DialogShape)> {
    let mut skipped = 0usize;
    for (idx, l) in lines[..=last].iter().enumerate().rev() {
        if l.trim().is_empty() {
            continue;
        }
        // Check non-status lines first so indented prompts/rules (`    SHELL$ `, `    ────`)
        // can never be mistaken for row continuations.
        if row(l).is_some() {
            return try_agy_rows_ending_at(lines, idx);
        }
        if is_non_status_trailing_line(l) {
            return None;
        }
        // A wrapped continuation of the last row has modest indent (4–8 spaces) and is not a
        // status bar; a right-aligned status bar (`    ... auto • model • high`) fails this
        // check or fails `try_agy_rows_ending_at` and falls through to be skipped as trailing.
        let indent = l.len() - l.trim_start_matches(' ').len();
        let looks_like_status = l.contains("esc to cancel") || l.contains(" • ") || l.contains(" · ");
        if (4..=8).contains(&indent) && !looks_like_status {
            if let Some(found) = try_agy_rows_ending_at(lines, idx) {
                return Some(found);
            }
        }
        skipped += 1;
        if skipped > 3 {
            return None;
        }
    }
    None
}

/// A dialog with no footer: the screen ends in its rows (or a row's wrapped continuation),
/// which run unbroken up to row 1, and the line above row 1 asks "Do you want …". The question
/// is required because numbered rows at the bottom of a screen are otherwise ordinary output.
fn bare_dialog(lines: &[&str], last: usize) -> Option<(usize, DialogShape)> {
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
                return asks.then_some((last + 1, DialogShape::Bare));
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

/// A numbered row: at most three columns of indent, an optional `❯` (Claude) or `>` (Antigravity)
/// highlight, then `N. label`.
fn row(line: &str) -> Option<(usize, &str)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let t = line.trim_start();
    let t = t
        .strip_prefix('❯')
        .or_else(|| t.strip_prefix('>'))
        .map_or(t, str::trim_start);
    let (n, label) = t.split_once(". ")?;
    if n.is_empty() || !n.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some((n.parse().ok()?, label.trim()))
}

/// The unattended-session countdown ("⚠ Claude Code will automatically deny this request in
/// 1:31, to avoid blocking progress on an unattended session") ticks every second, so with it in
/// the id changed under every card: each answer was refused as stale and the dialog timed out.
/// Cut from the joined, whitespace-free text, not by line: in a narrow pane the sentence wraps
/// and the ticking time lands on a row that doesn't hold the phrase.
fn without_countdown(mut drawn: String) -> String {
    const START: &str = "ClaudeCodewillautomaticallydenythisrequest";
    const END: &str = "unattendedsession";
    while let Some(s) = drawn.find(START) {
        let Some(e) = drawn[s..].find(END).map(|e| s + e + END.len()) else { break };
        let start = if drawn[..s].ends_with('⚠') { s - '⚠'.len_utf8() } else { s };
        drawn.replace_range(start..e, "");
    }
    drawn
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
    let is_agy = kind == DialogShape::Tool
        && !lines.get(foot).is_some_and(|l| l.contains("Esc to cancel"));
    let mut top = rule_above(first_row).unwrap_or(0);
    if is_agy {
        // Antigravity draws no `─` rule directly above its permission prompt; cap the header
        // window to the block immediately above row 1 so animated spinner lines from earlier
        // output above an old rule never destabilize the prompt digest.
        let floor = first_row.saturating_sub(10);
        if top < floor {
            top = floor;
        }
        // Stop below any braille spinner line (`⠋`..`⣿`) or status circle (`○`/`●`) above the prompt.
        if let Some(rel) = lines[top..first_row].iter().rposition(|l| {
            l.trim_start().chars().next().is_some_and(|c| {
                ('\u{2800}'..='\u{28ff}').contains(&c) || matches!(c, '○' | '●' | '└')
            })
        }) {
            top = top + rel + 1;
        }
    } else if kind == DialogShape::Plan {
        top = rule_above(top).unwrap_or(0);
    }
    let drawn: String = lines[top..foot]
        .concat()
        .chars()
        .filter(|c| !c.is_whitespace() && !matches!(c, '─' | '╌' | '❯' | '>'))
        .collect();
    let drawn = without_countdown(drawn);
    let digest = super::sha256_hex(drawn.as_bytes())[..12].to_string();
    // What it asks, above its rows: the words a Hive's allowlist reads (hive/prompts.rs).
    let text = lines[top..first_row]
        .iter()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with(['─', '╌']))
        .collect::<Vec<_>>()
        .join(" ");
    Some(PermissionDialog { options: rows.into_iter().map(|(_, l)| l).collect(), digest, shape: kind, text })
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
        // And the dialog is the same one with the countdown gone.
        assert_eq!(parse(&at("1:31")).unwrap().digest, parse(TWO_ROWS).unwrap().digest);
    }

    /// In a narrow pane the sentence wraps: the time lands on a row without the phrase, or the
    /// phrase itself splits. Neither may tick the id.
    #[test]
    fn a_wrapped_countdown_does_not_change_the_id() {
        let wrapped = |rows: &str| TWO_ROWS.replace(" Do you want to create notes.md?", &format!("{rows}\n\n Do you want to create notes.md?"));
        let at55 = |t: &str| wrapped(&format!(
            " ⚠ Claude Code will automatically deny this request in\n {t}, to avoid blocking progress on an unattended\n session"));
        let at45 = |t: &str| wrapped(&format!(
            " ⚠ Claude Code will automatically deny this\n request in {t}, to avoid blocking progress on\n an unattended session"));
        for (a, b) in [(at55("1:31"), at55("1:30")), (at45("1:31"), at45("about 45 seconds"))] {
            assert_eq!(parse(&a).unwrap().digest, parse(&b).unwrap().digest);
            assert_eq!(parse(&a).unwrap().digest, parse(TWO_ROWS).unwrap().digest);
        }
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
    fn each_dialog_says_its_shape_and_what_it_asks() {
        let bash = parse(BASH_100).unwrap();
        assert_eq!(bash.shape, DialogShape::Tool);
        assert!(bash.text.contains("Do you want to proceed?"), "{}", bash.text);
        assert!(!bash.text.contains('─') && !bash.text.contains("1. Yes"), "the rule and the rows aren't the question: {}", bash.text);
        assert_eq!(parse(PLAN_100).unwrap().shape, DialogShape::Plan);
        let fetch = parse(WEBFETCH_100).unwrap();
        assert_eq!(fetch.shape, DialogShape::Bare);
        assert!(fetch.text.contains("Do you want to allow Claude to fetch this content?"), "{}", fetch.text);
    }

    /// Esc is a reject on a tool dialog only; a label must be a row; unread rows take Yes or No.
    #[test]
    fn the_key_for_a_choice_follows_the_dialog() {
        let bash = parse(BASH_100).unwrap();
        assert_eq!(permission_key_for(Some(&bash), true, "Esc"), Ok(PermissionKey::Esc));
        assert_eq!(PermissionKey::Esc.bytes(), "\x1b");
        assert_eq!(PermissionKey::Esc.meaning(), Some("No"), "the mod takes it as a deny");
        let yes = permission_key_for(Some(&bash), true, "Yes").unwrap();
        assert_eq!(yes, PermissionKey::Row { key: "1".into(), label: "Yes".into() });
        assert_eq!(permission_key_for(Some(&bash), true, "No").unwrap().bytes(), "3");
        assert!(permission_key_for(Some(&bash), true, "Yes, and don't ask again").is_err());
        for not_tool in [PLAN_100, WEBFETCH_100] {
            assert!(permission_key_for(Some(&parse(not_tool).unwrap()), false, "esc").is_err(), "no Esc on a plan or bare dialog");
        }
        assert_eq!(permission_key_for(None, true, "esc"), Ok(PermissionKey::Esc));
        assert_eq!(permission_key_for(None, true, "No"), Ok(PermissionKey::Fallback("\x1b")));
        assert_eq!(PermissionKey::Fallback("1").meaning(), None);
        assert!(permission_key_for(None, false, "Esc").unwrap_err().contains("no longer open"));
        assert!(permission_key_for(None, true, "Yes, and don't ask again").unwrap_err().contains("only Yes or No"));
    }

    #[test]
    fn the_fallback_only_knows_yes_and_no() {
        assert_eq!(fallback_key("Yes"), Some("1"));
        assert_eq!(fallback_key("No"), Some("\x1b"));
        assert_eq!(fallback_key("Yes, don't ask again"), None);
        assert_eq!(fallback_key("3"), None);
    }

    #[test]
    fn reads_agy_six_option_tool_permission_dialog_and_ignores_normal_working_screen() {
        // Real layout captured live from `agy 1.5.13` on a 100×35 PTY.
        let agy_mcp = concat!(
            "○ Calling 1 MCP tool (tauri-mcp/ipc_execute_command) (ctrl+o to expand)\n",
            "● Calling MCP tool...\n",
            "\n",
            "MCP\n",
            "tauri-mcp/ipc_execute_command\n",
            "\n",
            "Allow calling this tool?\n",
            "> 1. Yes, allow tool call\n",
            "  2. Yes, and always allow tool 'tauri-mcp/ipc_execute_command' in this conversation\n",
            "  3. Yes, and always allow tool 'tauri-mcp/ipc_execute_command' (Persist to settings.json)\n",
            "  4. No, deny tool call\n",
            "  5. No, and always deny tool 'tauri-mcp/ipc_execute_command' in this conversation\n",
            "  6. No, and always deny tool 'tauri-mcp/ipc_execute_command' (Persist to settings.json)\n",
            "\n",
            "  ↑/↓ Navigate · tab Amend\n",
            "esc to cancel                                                            auto · Gemini 4 Neon · high\n",
        );

        assert!(dialog_open(agy_mcp));
        assert!(footer_open(agy_mcp));
        assert!(any_dialog_open(agy_mcp));

        let d = parse(agy_mcp).expect("agy 6-option permission dialog parsed");
        assert_eq!(d.shape, DialogShape::Tool);
        assert_eq!(d.options.len(), 6);
        assert_eq!(d.options[0], "Yes, allow tool call");
        assert_eq!(d.options[1], "Yes, and always allow tool 'tauri-mcp/ipc_execute_command' in this conversation");
        assert_eq!(d.options[3], "No, deny tool call");
        assert_eq!(d.options[5], "No, and always deny tool 'tauri-mcp/ipc_execute_command' (Persist to settings.json)");
        assert!(d.text.contains("tauri-mcp/ipc_execute_command"), "{}", d.text);
        assert!(d.text.contains("Allow calling this tool?"), "{}", d.text);
        assert!(!d.text.contains("Calling MCP tool"), "spinner line excluded from header: {}", d.text);

        // Moving the `>` cursor to another row keeps the digest stable.
        let moved = agy_mcp
            .replace("> 1. Yes, allow tool call", "  1. Yes, allow tool call")
            .replace("  4. No, deny tool call", "> 4. No, deny tool call");
        assert_eq!(parse(&moved).unwrap().digest, d.digest);

        // Key mapping: exact label, bare digit, and Esc all resolve cleanly.
        assert_eq!(permission_key_for(Some(&d), true, "Yes, allow tool call").unwrap().bytes(), "1");
        assert_eq!(permission_key_for(Some(&d), true, "No, deny tool call").unwrap().bytes(), "4");
        assert_eq!(permission_key_for(Some(&d), true, "6").unwrap().bytes(), "6");
        assert_eq!(permission_key_for(Some(&d), true, "Esc"), Ok(PermissionKey::Esc));
        assert!(permission_key_for(Some(&d), true, "Yes").is_err(), "non-verbatim label refused");

        // Normal working agy screen (with `esc to cancel   auto · ...`) is NOT a permission dialog.
        let working = concat!(
            "⣟  Generating...\n",
            "────────────────────────────────────────────────────────────────────────────────────────────────────\n",
            ">\n",
            "────────────────────────────────────────────────────────────────────────────────────────────────────\n",
            "esc to cancel                                                            auto · Gemini 4 Neon · high\n",
        );
        assert!(!dialog_open(working));
        assert!(!any_dialog_open(working));
        assert_eq!(parse(working), None);
    }

    #[test]
    fn reads_agy_command_prompt_and_custom_or_stacked_statusline_variants() {
        // Real command-permission dialog from agy 1.5.17 (no `tab Amend` line, or with custom/bullet status bar).
        let agy_cmd_bare = concat!(
            "Reason: The command triggers domain verification on the remote Resend service, which mutates remote account state in a way that cannot be directly reverted.\n",
            "Requesting permission for:\n",
            "  resend domains verify 3b0d3940-81c3-4e24-9bdc-e5538cbe9931 --json\n",
            "\n",
            "Run this command?\n",
            "> 1. Yes, run command\n",
            "  2. Yes, and always allow in this conversation for commands that start with 'resend'\n",
            "  3. Yes, and always allow for commands that start with 'resend' (Persist to settings.json)\n",
            "  4. No, cancel\n",
        );
        let d_bare = parse(agy_cmd_bare).expect("bare agy command permission dialog parsed");
        assert_eq!(d_bare.shape, DialogShape::Tool);
        assert_eq!(d_bare.options.len(), 4);
        assert_eq!(d_bare.options[0], "Yes, run command");
        assert_eq!(d_bare.options[3], "No, cancel");
        assert!(d_bare.text.contains("resend domains verify"), "{}", d_bare.text);
        assert!(d_bare.text.contains("Run this command?"), "{}", d_bare.text);
        assert!(dialog_open(agy_cmd_bare));
        assert!(footer_open(agy_cmd_bare));
        assert_eq!(permission_key_for(Some(&d_bare), true, "Esc"), Ok(PermissionKey::Esc));

        // Same command prompt above custom statusLine (`stack_with_default: false` — no `esc to cancel` or ` · `).
        let agy_cmd_custom_sl = format!(
            "{agy_cmd_bare}\nMACSTUDIO dprusak[~/projects/maiterm] (main) gemini-3-pro high 12.3%\n"
        );
        let d_sl = parse(&agy_cmd_custom_sl).expect("agy command prompt above custom statusLine");
        assert_eq!(d_sl.options, d_bare.options);
        assert_eq!(d_sl.digest, d_bare.digest);

        // Same command prompt above `tab Amend` + custom statusLine, and above stacked 2-line status bar.
        let agy_cmd_tab_custom = format!(
            "{agy_cmd_bare}\n  ↑/↓ Navigate • tab Amend\nMACSTUDIO dprusak[~/projects/maiterm] (main) gemini-3-pro high 12.3%\n"
        );
        assert_eq!(parse(&agy_cmd_tab_custom).expect("tab Amend + custom statusLine").options, d_bare.options);

        let agy_cmd_stacked = format!(
            "{agy_cmd_bare}\n  ↑/↓ Navigate · tab Amend\nesc to cancel                                             auto • gemini-3-pro • high\nMACSTUDIO dprusak[~/projects/maiterm] (main) gemini-3-pro high 12.3%\n"
        );
        assert_eq!(parse(&agy_cmd_stacked).expect("tab Amend + stacked 2-line statusLine").options, d_bare.options);

        // Dismissed dialog left in scrollback above a shell prompt or input box must NOT match.
        let after_exit = format!("{agy_cmd_bare}\nSHELL$ ");
        assert_eq!(parse(&after_exit), None);
        assert!(!dialog_open(&after_exit));

        let after_input_box = format!(
            "{agy_cmd_bare}\n────────────────────────────────────────\n> \n────────────────────────────────────────\nMACSTUDIO dprusak[~/projects/maiterm] (main) gemini-3-pro high 12.3%\n"
        );
        assert_eq!(parse(&after_input_box), None);
        assert!(!dialog_open(&after_input_box));

        // 2-line stacked status bar below `tab Amend` must match `tab Amend` directly (even when
        // rows are unreadable / header does not match `agy_permission_dialog`).
        let stacked_tab_only = concat!(
            "unreadable narrow rows\n",
            "  ↑/↓ Navigate · tab Amend\n",
            "esc to cancel                                             auto • gemini-3-pro • high\n",
            "MACSTUDIO dprusak[~/projects/maiterm] (main) gemini-3-pro high 12.3%\n",
        );
        assert!(dialog_open(stacked_tab_only));
        assert!(footer_open(stacked_tab_only));

        // Right-aligned (4+-space-indented) status bar — both with and without a blank line —
        // must be skipped as trailing rather than aborting the scan or corrupting the last row.
        let right_aligned_blank = format!(
            "{agy_cmd_bare}\n                                        auto • gemini-3-pro • high\n"
        );
        let d_ra1 = parse(&right_aligned_blank).expect("right-aligned status bar after blank line");
        assert_eq!(d_ra1.options, d_bare.options);
        assert_eq!(d_ra1.digest, d_bare.digest);

        let right_aligned_adjacent = format!(
            "{agy_cmd_bare}                                        auto • gemini-3-pro • high\n"
        );
        let d_ra2 = parse(&right_aligned_adjacent).expect("right-aligned status bar adjacent to last row");
        assert_eq!(d_ra2.options, d_bare.options);
        assert_eq!(d_ra2.digest, d_bare.digest);

        // Indented non-status line (`    SHELL$ `, `    ────`) directly below a dismissed dialog
        // must NOT be folded as a row continuation.
        let indented_shell = format!("{agy_cmd_bare}    SHELL$ \n");
        assert_eq!(parse(&indented_shell), None);
        assert!(!dialog_open(&indented_shell));

        // Real wrapped continuation of row 4 (5-space indent) above a custom statusLine still works.
        let wrapped_last_row = concat!(
            "Run this command?\n",
            "> 1. Yes, run command\n",
            "  2. No, cancel and tell agent\n",
            "     what to do instead\n",
            "\n",
            "MACSTUDIO dprusak[~/projects/maiterm] (main) gemini-3-pro high 12.3%\n",
        );
        let d_wrap = parse(wrapped_last_row).expect("wrapped last row above custom statusLine");
        assert_eq!(d_wrap.options, vec!["Yes, run command", "No, cancel and tell agent what to do instead"]);
    }
}
