//! A phone's answers to Claude Code's AskUserQuestion selector, typed in as keys — each key
//! confirmed on the screen before the next one is sent.
//!
//! Used when the maiterm-tab mod isn't behind the selector to take the answers as data
//! (`claude_code/mod_asks.rs`). Navigation is relative (arrow keys), so a key sent before the
//! last one has landed walks from the wrong row, and the row it lands on is recorded as the
//! human's answer. This used to sleep 130/280 ms between keys and hope the TUI had repainted.
//! Now every key has an [`Expect`]ation of what the screen will show once it has landed (the
//! highlight on the next row, the box ticked, the typed text in the Other row, the next question
//! open), and the next key waits until the screen shows it. Waiting is on the terminal's own
//! signal that output reached the grid (`TerminalHandle::screen_seq`), never on a timer; the only
//! clock is a deadline that turns a screen that never repaints into a clear failure. A screen
//! that shows something else (the highlight on a third row, a question opened on the wrong row)
//! fails at once. Either way nothing more is sent, and the caller reports `inject_failed`.
//!
//! RUNTIME-FRAGILE: the selector is a TUI, not a stable API. Mechanics pinned live against Claude
//! Code 2.1.x (docs/mailink-protocol.md §12.3) and read from the 2.1.325 bundle. The form is a row
//! of tabs [Q1..Qn][Submit]:
//!   (a) each question's highlight starts at row 1: no longer assumed, checked on screen before
//!       the first key and again as each question opens;
//!   (b) arrows move the highlight. 2.1.325 also takes digits (a digit picks AND commits a listed
//!       row, or focuses an empty Other row), but it is not used: older builds (2.1.224) are
//!       arrow-only and send the digit `options + 2` to "Chat about this", which throws the
//!       ask away; and a digit commits in one keystroke, leaving no state in between to check.
//!       Arrows with a check after each one are as certain, on every build;
//!   (c) single-select: Enter picks the highlighted row AND advances to the next tab;
//!   (d) multiSelect: Space toggles the highlighted row (live, no Enter); → advances the tab;
//!   (e) a lone single-select question submits on its own Enter; every other form lands on the
//!       review tab ("Ready to submit your answers?", "Submit answers" highlighted), which takes
//!       one more Enter. Which of the two follows the last question is read off the screen;
//!   (f) the free-text Other row ("Type something.", the row after the listed options) is a live
//!       inline input: highlight it and type. Single-select then takes Enter like a listed pick;
//!   (g) multiSelect + Other leaves through the form's own Submit/Next button: Down from the
//!       Other row (the last row) focuses it, Enter presses it. The input swallows Enter and →.
//!       Source-derived from 2.1.240 and still unverified on a device; with the button's focus
//!       now checked on screen, a build where Down doesn't reach it fails cleanly instead.
//!
//! TYPING INTO THE WRONG ROW IS DESTRUCTIVE: the container's onKeyDown (read from 2.1.224) sends
//! the digit `options + 2` to "Chat about this", which dismisses the ask, unless the Other input
//! holds focus. So text is typed only once the screen shows the Other row highlighted.
//!
//! Every answer is mapped to rows BEFORE any key is sent, so a bad answer rejects the whole batch
//! rather than half-answering.

use serde_json::Value;

use super::Answer;

/// The terminal the keys go to, as far as answering needs it. Real: the tab's PTY and grid
/// (`mailink::PtySelector`); tests: a selector that repaints late, out of order or never.
pub(crate) trait SelectorTerm {
    /// Write bytes to the terminal.
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;
    /// The live screen as text. Reading it marks every change so far as seen.
    fn screen(&mut self) -> String;
    /// Resolves once the screen has changed since the last `screen()`, or fails when it can't
    /// (the terminal closed, or the deadline for the last key passed).
    fn repainted(&mut self) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

/// How one question is told apart on screen: its text, and its first option drawn as row 1.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Shown {
    /// The question, whitespace removed, cut to 40 characters (a long one is cut on screen).
    text: String,
    /// Its first option's label, whitespace removed, cut to 12 characters (a label beside an
    /// option preview wraps after its first line).
    first: String,
}

impl Shown {
    fn of(q: &Value) -> Option<Shown> {
        let text: String = squash(q.get("question")?.as_str()?).chars().take(40).collect();
        let first: String = q
            .get("options")
            .and_then(|o| o.get(0))
            .and_then(|o| o.get("label"))
            .and_then(|l| l.as_str())
            .map(|l| squash(l).chars().take(12).collect())
            .unwrap_or_default();
        (!text.is_empty() && !first.is_empty()).then_some(Shown { text, first })
    }
}

/// One numbered row of the selector as drawn.
#[derive(Debug, Clone, PartialEq)]
struct Row {
    focused: bool,
    /// A multiSelect row's box: `Some(true)` ticked.
    ticked: Option<bool>,
    /// The row's text after its number and box, with its wrapped and description lines, whitespace
    /// removed.
    text: String,
}

/// The rows below a question, and whether the form's Submit/Next button holds the highlight.
#[derive(Debug, Clone, PartialEq, Default)]
struct View {
    rows: Vec<Row>,
    button_focused: bool,
}

impl View {
    /// The highlighted row's number (1-based).
    fn focused(&self) -> Option<usize> {
        self.rows.iter().position(|r| r.focused).map(|i| i + 1)
    }
}

const POINTER: char = '\u{276F}'; // ❯

/// `N. rest` → (N, rest).
fn numbered(s: &str) -> Option<(usize, &str)> {
    let digits = s.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = s[digits..].strip_prefix('.')?;
    if !(rest.is_empty() || rest.starts_with(' ')) {
        return None;
    }
    Some((s[..digits].parse().ok()?, rest.trim_start()))
}

/// The selector's footer, or the multiSelect button's label: where the last row's text ends.
fn ends_rows(line: &str) -> bool {
    line.contains("Esc to cancel") || line.contains("Enter to select")
}

fn is_button(rest: &str) -> bool {
    matches!(rest.trim(), "Next" | "Submit")
}

/// Rows numbered 1, 2, 3… in order from the top of `lines`; anything numbered out of turn is
/// text of the row above it.
fn rows_below(lines: &[&str]) -> View {
    let mut view = View::default();
    for line in lines {
        if ends_rows(line) {
            break;
        }
        let t = line.trim_start();
        let (focused, rest) = match t.strip_prefix(POINTER) {
            Some(r) => (true, r.trim_start()),
            None => (false, t),
        };
        if let Some((n, after)) = numbered(rest) {
            if n == view.rows.len() + 1 {
                let chars: Vec<char> = after.chars().collect();
                let (ticked, label) = if chars.len() >= 3 && chars[0] == '[' && chars[2] == ']' {
                    (Some(chars[1] != ' '), chars[3..].iter().collect::<String>())
                } else {
                    (None, after.to_string())
                };
                view.rows.push(Row { focused, ticked, text: squash(&label) });
                continue;
            }
        }
        let Some(last) = view.rows.last_mut() else { continue };
        if is_button(rest) {
            view.button_focused |= focused;
            continue;
        }
        last.text.push_str(&squash(line));
    }
    view
}

/// The screen's lines, and each line's end offset in the whitespace-free text of the screen.
fn squashed_lines(screen: &str) -> (Vec<&str>, String, Vec<usize>) {
    let lines: Vec<&str> = screen.lines().collect();
    let mut cat = String::new();
    let mut ends = Vec::with_capacity(lines.len());
    for l in &lines {
        cat.push_str(&squash(l));
        ends.push(cat.len());
    }
    (lines, cat, ends)
}

/// The selector for question `q`, if it is the one open on screen: the rows under the LAST place
/// its text is drawn, the first of them being its first option. The same text higher up (an
/// earlier ask, the review tab's summary) sits above other rows, and an answered ask shows its
/// choice without row numbers.
fn question_view(screen: &str, q: &Shown) -> Option<View> {
    let (lines, cat, ends) = squashed_lines(screen);
    let at = cat.rfind(&q.text)?;
    let below = &cat[at + q.text.len()..];
    // A permission dialog drawn under the question is not its selector.
    if ["Doyouwant", "Wouldyoulike", "ctrl+gtoeditin"].iter().any(|p| below.contains(p)) {
        return None;
    }
    let j = ends.iter().position(|&e| e >= at + q.text.len())?;
    let view = rows_below(&lines[j + 1..]);
    view.rows.first().filter(|r| r.text.starts_with(&q.first))?;
    Some(view)
}

const REVIEW: &str = "Readytosubmityouranswers?";

/// The review tab, if it is open: `Some(true)` when it is ready to submit (no "You have not
/// answered all questions" warning, and "Submit answers" highlighted), `Some(false)` otherwise.
fn review(screen: &str) -> Option<bool> {
    let (lines, cat, ends) = squashed_lines(screen);
    let at = cat.rfind(REVIEW)?;
    let start = cat[..at].rfind("Reviewyouranswers").unwrap_or(0);
    let warned = cat[start..at].contains("Youhavenotansweredallquestions");
    let j = ends.iter().position(|&e| e >= at + REVIEW.len())?;
    let submit_focused = lines[j + 1..]
        .iter()
        .any(|l| l.trim_start().starts_with(POINTER) && squash(l).contains("Submitanswers"));
    Some(!warned && submit_focused)
}

/// The ask is gone: none of its questions open, no review tab, and Claude's input box drawn
/// where the selector was. The box is the positive evidence: a frame caught mid-redraw shows
/// neither.
fn closed(screen: &str, all: &[Shown]) -> bool {
    review(screen).is_none()
        && all.iter().all(|q| question_view(screen, q).is_none())
        && super::input_box::parse(screen) != super::input_box::InputBox::Unknown
}

/// What the screen shows once a key has landed.
#[derive(Debug, Clone)]
pub(crate) enum Expect<'a> {
    /// Question `q` with the highlight moved from row `from` to row `row`.
    Focus { q: &'a Shown, row: usize, from: usize },
    /// Row `row` of question `q` ticked, the highlight still on it.
    Ticked { q: &'a Shown, row: usize },
    /// The Other row `row` highlighted and holding `text`.
    Typed { q: &'a Shown, row: usize, text: String },
    /// The multiSelect Submit/Next button highlighted, moved there from row `from`.
    Button { q: &'a Shown, from: usize },
    /// Question `q` open, its highlight on row 1.
    Opens { q: &'a Shown },
    /// After the last question: the review tab ready to submit, or the ask closed.
    Finish { all: &'a [Shown] },
    /// The ask closed.
    Closed { all: &'a [Shown] },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Verdict {
    Met,
    /// Not yet: the screen still shows the state before the key, or a frame mid-redraw.
    Pending,
    /// The screen shows something the key can't have produced from the state it was sent in.
    Wrong(String),
}

impl Expect<'_> {
    pub(crate) fn check(&self, screen: &str) -> Verdict {
        use Verdict::*;
        let elsewhere = |at: usize| Wrong(format!("the highlight is on row {at}"));
        match self {
            Expect::Focus { q, row, from } => match question_view(screen, q).and_then(|v| v.focused()) {
                Some(f) if f == *row => Met,
                Some(f) if f == *from => Pending,
                Some(f) => elsewhere(f),
                None => Pending,
            },
            Expect::Ticked { q, row } => match question_view(screen, q) {
                Some(v) => match (v.focused(), v.rows.get(row - 1).and_then(|r| r.ticked)) {
                    (Some(f), _) if f != *row => elsewhere(f),
                    (Some(_), Some(true)) => Met,
                    _ => Pending,
                },
                None => Pending,
            },
            Expect::Typed { q, row, text } => match question_view(screen, q) {
                Some(v) => match v.focused() {
                    Some(f) if f == *row => {
                        let typed = squash(text);
                        let head: String = typed.chars().take(12).collect();
                        let tail: String = typed.chars().rev().take(12).collect::<Vec<_>>().into_iter().rev().collect();
                        let shown = &v.rows[row - 1].text;
                        if !typed.is_empty() && (shown.starts_with(&head) || shown.ends_with(&tail)) { Met } else { Pending }
                    }
                    Some(f) => elsewhere(f),
                    None => Pending,
                },
                None => Pending,
            },
            Expect::Button { q, from } => match question_view(screen, q) {
                Some(v) if v.button_focused => Met,
                Some(v) => match v.focused() {
                    Some(f) if f != *from => elsewhere(f),
                    _ => Pending,
                },
                None => Pending,
            },
            Expect::Opens { q } => match question_view(screen, q).map(|v| v.focused()) {
                Some(Some(1)) => Met,
                Some(Some(f)) => Wrong(format!("the question opened with its highlight on row {f}, not row 1")),
                _ => Pending,
            },
            Expect::Finish { all } => match review(screen) {
                Some(true) => Met,
                Some(false) if !squash(screen).contains("Youhavenotansweredallquestions") => Pending,
                Some(false) => Wrong("the form says not every question was answered".into()),
                None if closed(screen, all) => Met,
                None => Pending,
            },
            Expect::Closed { all } => if closed(screen, all) { Met } else { Pending },
        }
    }
}

/// Send `bytes`, then wait until the screen shows `expect`. Returns the screen that showed it.
///
/// The expectation must NOT already hold before the key is sent (or a key that never landed
/// would pass for one that did), and must not be contradicted before it either. Every failure
/// leaves the caller to send nothing more.
async fn press<T: SelectorTerm>(term: &mut T, bytes: &[u8], what: &str, expect: &Expect<'_>) -> Result<String, String> {
    match expect.check(&term.screen()) {
        Verdict::Pending => {}
        Verdict::Met => return Err(format!("{what}: the screen already showed the result before the key, so it couldn't be confirmed")),
        Verdict::Wrong(why) => return Err(format!("{what}: before the key, {why}")),
    }
    term.send(bytes).map_err(|e| format!("{what}: {e}"))?;
    loop {
        let screen = term.screen();
        match expect.check(&screen) {
            Verdict::Met => return Ok(screen),
            Verdict::Wrong(why) => return Err(format!("{what}: {why}")),
            Verdict::Pending => term.repainted().await.map_err(|e| format!("{what}: {e}"))?,
        }
    }
}

const DOWN: &[u8] = b"\x1b[B";
const UP: &[u8] = b"\x1b[A";
const RIGHT: &[u8] = b"\x1b[C";
const ENTER: &[u8] = b"\r";
const SPACE: &[u8] = b" ";

/// Move question `q`'s highlight from row `from` to row `to`, one confirmed row at a time.
async fn nav<T: SelectorTerm>(term: &mut T, q: &Shown, from: usize, to: usize) -> Result<usize, String> {
    let mut cur = from;
    while cur != to {
        let (key, next) = if to > cur { (DOWN, cur + 1) } else { (UP, cur - 1) };
        press(term, key, &format!("moving to row {next}"), &Expect::Focus { q, row: next, from: cur }).await?;
        cur = next;
    }
    Ok(cur)
}

/// Replay the phone's per-question answers into the open selector, in question order, each key
/// confirmed on screen before the next (module doc). `Err` says which key wasn't confirmed and
/// why; nothing was sent after it.
pub(crate) async fn drive_question_answers<T: SelectorTerm>(
    term: &mut T,
    tool_input: &Value,
    answers: &[Answer],
) -> Result<(), String> {
    let questions = tool_input
        .get("questions")
        .and_then(|v| v.as_array())
        .ok_or("pending ask has no questions[]")?;
    if answers.len() != questions.len() {
        return Err(format!(
            "answer count {} != question count {}",
            answers.len(),
            questions.len()
        ));
    }

    struct Plan {
        multi: bool,
        option_count: usize,   // # of listed options; the "Type something" (Other) row follows them
        rows: Vec<usize>,      // listed-option rows (1-based) to select/toggle
        other: Option<String>, // free-text for the "Other" row, if the phone chose it
    }

    // Resolve every answer to concrete selector actions up front (fail-closed on any bad map).
    let mut plans: Vec<Plan> = Vec::with_capacity(questions.len());
    let mut shown: Vec<Shown> = Vec::with_capacity(questions.len());
    for (qi, (q, ans)) in questions.iter().zip(answers).enumerate() {
        let labels: Vec<&str> = q
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|o| o.get("label").and_then(|v| v.as_str()))
                    .collect()
            })
            .unwrap_or_default();
        let multi = q.get("multiSelect").and_then(|v| v.as_bool()).unwrap_or(false);

        let mut rows = Vec::new();
        for sel in &ans.selected {
            match labels.iter().position(|l| l == sel) {
                Some(idx) => rows.push(idx + 1),
                None => return Err(format!("question {qi}: label {sel:?} not among options")),
            }
        }
        // Text only: every control character becomes a space. In the Other input a newline or
        // CR submits early, ESC cancels the whole ask, and Tab moves focus to the Next button;
        // all of it goes in one write, after the last screen check.
        let other = ans.other.as_ref().map(|s| s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect::<String>());
        let other = other.filter(|s| !s.trim().is_empty());
        if rows.is_empty() && other.is_none() {
            return Err(format!("question {qi}: no option selected and no Other text"));
        }
        // single-select accepts exactly one pick total (one listed option XOR Other text).
        if !multi && rows.len() + other.is_some() as usize > 1 {
            return Err(format!("question {qi}: single-select received multiple picks"));
        }
        rows.sort_unstable();
        rows.dedup();
        plans.push(Plan { multi, option_count: labels.len(), rows, other });
        shown.push(Shown::of(q).ok_or_else(|| format!("question {qi} has no text or no options to find it by on screen"))?);
    }
    // Each step is confirmed by the NEXT question opening; two that read the same can't confirm it.
    for (i, a) in shown.iter().enumerate() {
        if shown[i + 1..].contains(a) {
            return Err("two of the questions read the same on screen, so the keys couldn't be confirmed".into());
        }
    }

    // (a) The first question is open with its highlight on row 1, or nothing is sent.
    if (Expect::Opens { q: &shown[0] }).check(&term.screen()) != Verdict::Met {
        return Err("the question isn't open on screen with its highlight on the first row; nothing was sent".into());
    }

    let last = plans.len() - 1;
    let mut after_last = String::new();
    for (qi, plan) in plans.iter().enumerate() {
        let q = &shown[qi];
        let advance = if qi < last { Expect::Opens { q: &shown[qi + 1] } } else { Expect::Finish { all: &shown } };
        let n = qi + 1;
        let mut cur = 1usize;
        let after = if plan.multi {
            for &row in &plan.rows {
                cur = nav(term, q, cur, row).await?;
                press(term, SPACE, &format!("question {n}: ticking row {row}"), &Expect::Ticked { q, row }).await?;
            }
            if let Some(text) = &plan.other {
                let row = plan.option_count + 1;
                cur = nav(term, q, cur, row).await?;
                // Typing fills the Other row AND ticks it: nothing to commit (f).
                press(term, text.as_bytes(), &format!("question {n}: typing the Other answer"), &Expect::Typed { q, row, text: text.clone() }).await?;
                // (g) Down to the form's own Submit/Next button, Enter to press it.
                press(term, DOWN, &format!("question {n}: moving to the Next button"), &Expect::Button { q, from: cur }).await?;
                press(term, ENTER, &format!("question {n}: pressing Next"), &advance).await?
            } else {
                // (d) Toggles are live; → moves to the next tab. Only from a listed row: the
                // Other input would swallow it.
                press(term, RIGHT, &format!("question {n}: moving on"), &advance).await?
            }
        } else if let Some(text) = &plan.other {
            let row = plan.option_count + 1;
            nav(term, q, cur, row).await?;
            press(term, text.as_bytes(), &format!("question {n}: typing the Other answer"), &Expect::Typed { q, row, text: text.clone() }).await?;
            press(term, ENTER, &format!("question {n}: choosing the Other answer"), &advance).await?
        } else {
            let row = plan.rows[0];
            nav(term, q, cur, row).await?;
            // (c) Enter picks the row AND advances.
            press(term, ENTER, &format!("question {n}: choosing row {row}"), &advance).await?
        };
        if qi == last {
            after_last = after;
        }
    }

    // (e) What follows the last question is read, not assumed: the review tab takes one Enter on
    // its highlighted "Submit answers"; a closed ask needs nothing more.
    if review(&after_last) == Some(true) {
        press(term, ENTER, "submitting the answers", &Expect::Closed { all: &shown }).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::VecDeque;

    /// Claude's AskUserQuestion selector, reduced to what the keys do and what it draws.
    #[derive(Clone)]
    struct Model {
        qs: Vec<(String, Vec<String>, bool)>,
        /// The open tab; `qs.len()` is the review tab.
        tab: usize,
        /// Highlighted row, 1-based; `labels + 1` is Other, `labels + 2` the multiSelect button.
        focus: usize,
        ticked: Vec<Vec<bool>>,
        other: Vec<String>,
        chosen: Vec<Option<String>>,
        submitted: bool,
        /// Set by "Chat about this": the ask thrown away.
        dismissed: bool,
    }

    impl Model {
        fn new(qs: &[(&str, &[&str], bool)]) -> Model {
            Model {
                qs: qs.iter().map(|(t, l, m)| (t.to_string(), l.iter().map(|s| s.to_string()).collect(), *m)).collect(),
                tab: 0,
                focus: 1,
                ticked: qs.iter().map(|(_, l, _)| vec![false; l.len() + 1]).collect(),
                other: vec![String::new(); qs.len()],
                chosen: vec![None; qs.len()],
                submitted: false,
                dismissed: false,
            }
        }

        fn lone_single(&self) -> bool {
            self.qs.len() == 1 && !self.qs[0].2
        }

        fn advance(&mut self) {
            self.tab += 1;
            self.focus = 1;
            if self.lone_single() {
                self.submitted = true;
            }
        }

        fn key(&mut self, bytes: &[u8]) {
            if self.submitted || self.dismissed {
                return;
            }
            if self.tab == self.qs.len() {
                if bytes == ENTER && self.focus == 1 {
                    self.submitted = true;
                }
                return;
            }
            let n = self.qs[self.tab].1.len();
            let multi = self.qs[self.tab].2;
            let other_row = n + 1;
            let in_other = self.focus == other_row;
            match bytes {
                DOWN if in_other && multi => self.focus = n + 2,
                DOWN if self.focus < other_row => self.focus += 1,
                UP if self.focus > 1 && !in_other => self.focus -= 1,
                SPACE if multi && self.focus <= n => {
                    let t = &mut self.ticked[self.tab][self.focus - 1];
                    *t = !*t;
                }
                RIGHT if multi && !in_other => self.advance(),
                ENTER if !multi && self.focus <= n => {
                    self.chosen[self.tab] = Some(self.qs[self.tab].1[self.focus - 1].clone());
                    self.advance();
                }
                ENTER if !multi && in_other && !self.other[self.tab].is_empty() => {
                    self.chosen[self.tab] = Some(format!("Other: {}", self.other[self.tab]));
                    self.advance();
                }
                ENTER if multi && self.focus == n + 2 => self.advance(),
                _ if in_other => {
                    self.other[self.tab].push_str(std::str::from_utf8(bytes).unwrap());
                    self.ticked[self.tab][n] = true;
                }
                // The digit `options + 2` outside the Other input: "Chat about this".
                _ if bytes.iter().any(|d| d.is_ascii_digit() && (d - b'0') as usize == n + 2) => self.dismissed = true,
                _ => {}
            }
        }

        fn render(&self) -> String {
            let input_box = "\n──────────────────────────────\n❯ \n──────────────────────────────\n";
            if self.dismissed {
                return format!("● Chatting about it…\n{input_box}");
            }
            if self.submitted {
                let mut s = String::from("● User answered Claude's questions:\n");
                for (i, (t, _, _)) in self.qs.iter().enumerate() {
                    s += &format!("  · {t} → {}\n", self.chosen[i].clone().unwrap_or_default());
                }
                return s + input_box;
            }
            let mut s = String::from("● Asking…\n\n");
            if self.tab == self.qs.len() {
                s += " Review your answers\n\n";
                for (t, _, _) in &self.qs {
                    s += &format!(" ● {t}\n   → answered\n");
                }
                s += "\n Ready to submit your answers?\n\n ❯ 1. Submit answers\n   2. Cancel\n";
                return s;
            }
            let (text, labels, multi) = &self.qs[self.tab];
            s += &format!(" {text}\n\n");
            let row = |i: usize, label: &str, ticked: bool| {
                let p = if self.focus == i { "❯" } else { " " };
                let b = if *multi { if ticked { "[✓] " } else { "[ ] " } } else { "" };
                format!(" {p} {i}. {b}{label}\n")
            };
            for (i, l) in labels.iter().enumerate() {
                s += &row(i + 1, l, self.ticked[self.tab][i]);
            }
            let typed = &self.other[self.tab];
            let other = if typed.is_empty() { "Type something." } else { typed };
            s += &row(labels.len() + 1, other, self.ticked[self.tab][labels.len()]);
            if *multi {
                let label = if self.tab + 1 == self.qs.len() { "Submit" } else { "Next" };
                let p = if self.focus == labels.len() + 2 { "❯" } else { " " };
                s += &format!(" {p}    {label}\n");
            }
            s + "\n Enter to select · ↑/↓ to navigate · Esc to cancel\n"
        }
    }

    /// How the fake terminal repaints after a key.
    #[derive(Clone, Copy, PartialEq)]
    enum Paint {
        /// One frame, the new state.
        Prompt,
        /// `n` repaints of the OLD screen (a spinner tick, a status line) before the new one.
        Late(usize),
        /// A half-drawn frame, then the old screen redrawn, then the new one.
        OutOfOrder,
        /// No repaint at all.
        Never,
        /// The human presses Down in the terminal at the same moment.
        HumanDown,
    }

    struct Fake {
        model: Model,
        shown: String,
        queued: VecDeque<String>,
        sent: Vec<Vec<u8>>,
        /// The paint for each key sent, in order; `Prompt` once the list runs out.
        paints: VecDeque<Paint>,
    }

    impl Fake {
        fn new(model: Model, paints: &[Paint]) -> Fake {
            let shown = model.render();
            Fake { model, shown, queued: VecDeque::new(), sent: Vec::new(), paints: paints.iter().copied().collect() }
        }
    }

    impl SelectorTerm for Fake {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            self.sent.push(bytes.to_vec());
            let old = self.shown.clone();
            self.model.key(bytes);
            let paint = self.paints.pop_front().unwrap_or(Paint::Prompt);
            if paint == Paint::HumanDown {
                self.model.key(DOWN);
            }
            let new = self.model.render();
            match paint {
                Paint::Prompt | Paint::HumanDown => self.queued.push_back(new),
                Paint::Late(n) => {
                    for i in 0..n {
                        self.queued.push_back(old.replace("Asking…", &format!("Asking… ({i})")));
                    }
                    self.queued.push_back(new);
                }
                Paint::OutOfOrder => {
                    self.queued.push_back(new.lines().take(3).collect::<Vec<_>>().join("\n"));
                    self.queued.push_back(old);
                    self.queued.push_back(new);
                }
                Paint::Never => {}
            }
            Ok(())
        }

        fn screen(&mut self) -> String {
            self.shown.clone()
        }

        fn repainted(&mut self) -> impl std::future::Future<Output = Result<(), String>> + Send {
            let r = match self.queued.pop_front() {
                Some(frame) => {
                    self.shown = frame;
                    Ok(())
                }
                None => Err("the screen never showed it".to_string()),
            };
            std::future::ready(r)
        }
    }

    fn input(qs: &[(&str, &[&str], bool)]) -> Value {
        json!({ "questions": qs.iter().map(|(t, l, m)| json!({
            "question": t, "header": "H", "multiSelect": m,
            "options": l.iter().map(|x| json!({ "label": x, "description": "" })).collect::<Vec<_>>(),
        })).collect::<Vec<_>>() })
    }

    fn pick(labels: &[&str]) -> Answer {
        Answer { selected: labels.iter().map(|s| s.to_string()).collect(), other: None }
    }

    fn other(text: &str) -> Answer {
        Answer { selected: vec![], other: Some(text.to_string()) }
    }

    fn block_on<F: std::future::Future>(f: F) -> F::Output {
        tokio::runtime::Builder::new_current_thread().build().unwrap().block_on(f)
    }

    const DB: (&str, &[&str], bool) = ("Which database should the new service use for its queue?", &["Postgres", "Redis", "SQLite"], false);
    const CACHE: (&str, &[&str], bool) = ("Which caches should it warm on start?", &["Users", "Sessions", "Prices"], true);

    fn run(qs: &[(&str, &[&str], bool)], answers: &[Answer], paints: &[Paint]) -> (Result<(), String>, Fake) {
        let mut fake = Fake::new(Model::new(qs), paints);
        let r = block_on(drive_question_answers(&mut fake, &input(qs), answers));
        (r, fake)
    }

    #[test]
    fn a_lone_question_picks_the_row_asked_for() {
        let (r, fake) = run(&[DB], &[pick(&["SQLite"])], &[]);
        assert_eq!(r, Ok(()));
        assert_eq!(fake.model.chosen, vec![Some("SQLite".to_string())]);
        assert!(fake.model.submitted);
        assert_eq!(fake.sent, vec![DOWN.to_vec(), DOWN.to_vec(), ENTER.to_vec()]);
    }

    #[test]
    fn a_late_repaint_is_waited_for_not_overrun() {
        // Every key's effect arrives only after two repaints of the old screen: a fixed sleep
        // sent the next Down into the stale highlight. Each key now waits for its own row.
        let paints = [Paint::Late(2); 8];
        let (r, fake) = run(&[DB], &[pick(&["SQLite"])], &paints);
        assert_eq!(r, Ok(()));
        assert_eq!(fake.model.chosen, vec![Some("SQLite".to_string())]);
        assert_eq!(fake.sent.len(), 3, "one key per step, none sent twice");
    }

    #[test]
    fn an_out_of_order_repaint_is_waited_through() {
        let paints = [Paint::OutOfOrder; 8];
        let (r, fake) = run(&[DB], &[pick(&["Redis"])], &paints);
        assert_eq!(r, Ok(()));
        assert_eq!(fake.model.chosen, vec![Some("Redis".to_string())]);
    }

    #[test]
    fn a_repaint_that_never_comes_fails_and_sends_nothing_more() {
        // The first Down lands in the model but is never drawn.
        let (r, fake) = run(&[DB], &[pick(&["SQLite"])], &[Paint::Never]);
        let e = r.unwrap_err();
        assert!(e.contains("moving to row 2") && e.contains("never showed"), "{e}");
        assert_eq!(fake.sent, vec![DOWN.to_vec()], "no key after the unconfirmed one");
        assert_eq!(fake.model.chosen, vec![None], "nothing was picked");
        assert!(!fake.model.submitted);
    }

    #[test]
    fn a_highlight_that_lands_on_another_row_fails_at_once() {
        // The human presses Down too: the highlight lands on row 3, not row 2. The old code
        // would have sent the second Down and an Enter, and recorded the wrong row.
        let (r, fake) = run(&[DB], &[pick(&["Redis"])], &[Paint::HumanDown]);
        let e = r.unwrap_err();
        assert!(e.contains("row 3"), "{e}");
        assert_eq!(fake.sent, vec![DOWN.to_vec()]);
        assert_eq!(fake.model.chosen, vec![None]);
    }

    #[test]
    fn nothing_is_sent_unless_the_question_is_open_at_row_one() {
        let mut model = Model::new(&[DB]);
        model.focus = 2; // the human moved it first
        let mut fake = Fake::new(model, &[]);
        let r = block_on(drive_question_answers(&mut fake, &input(&[DB]), &[pick(&["Postgres"])]));
        assert!(r.unwrap_err().contains("nothing was sent"));
        assert!(fake.sent.is_empty());
    }

    #[test]
    fn other_text_is_typed_only_once_the_other_row_is_highlighted() {
        // "5" is the digit `options + 2`: typed with the highlight anywhere but the Other row,
        // it is "Chat about this" and the ask is thrown away. Late repaints made that happen.
        let paints = [Paint::Late(1); 8];
        let (r, fake) = run(&[DB], &[other("Use 5 shards")], &paints);
        assert_eq!(r, Ok(()));
        assert!(!fake.model.dismissed);
        assert_eq!(fake.model.chosen, vec![Some("Other: Use 5 shards".to_string())]);
    }

    #[test]
    fn other_text_never_follows_an_unconfirmed_move() {
        // The last Down toward the Other row is never drawn: the text must not be typed.
        let (r, fake) = run(&[DB], &[other("5")], &[Paint::Prompt, Paint::Prompt, Paint::Never]);
        assert!(r.is_err());
        assert_eq!(fake.sent, vec![DOWN.to_vec(); 3]);
        assert!(!fake.model.dismissed);
    }

    #[test]
    fn a_multi_question_form_with_ticks_and_other_submits_from_the_review() {
        let qs = [CACHE, DB];
        let answers = [
            Answer { selected: vec!["Users".into(), "Prices".into()], other: Some("Carts".into()) },
            pick(&["Redis"]),
        ];
        let paints = [Paint::Late(1), Paint::OutOfOrder, Paint::Prompt, Paint::Late(3)];
        let (r, fake) = run(&qs, &answers, &paints);
        assert_eq!(r, Ok(()));
        assert_eq!(fake.model.ticked[0], vec![true, false, true, true]);
        assert_eq!(fake.model.other[0], "Carts");
        assert_eq!(fake.model.chosen[1], Some("Redis".to_string()));
        assert!(fake.model.submitted);
        assert_eq!(fake.sent.last(), Some(&ENTER.to_vec()));
    }

    #[test]
    fn a_lone_multiselect_moves_on_with_right_then_submits() {
        let (r, fake) = run(&[CACHE], &[pick(&["Sessions"])], &[]);
        assert_eq!(r, Ok(()));
        assert_eq!(fake.model.ticked[0], vec![false, true, false, false]);
        assert!(fake.model.submitted);
        assert_eq!(fake.sent, vec![DOWN.to_vec(), SPACE.to_vec(), RIGHT.to_vec(), ENTER.to_vec()]);
    }

    #[test]
    fn a_review_that_never_appears_is_not_submitted_blind() {
        // The → after the last tick is never drawn: no final Enter.
        let (r, fake) = run(&[CACHE], &[pick(&["Users"])], &[Paint::Prompt, Paint::Never]);
        assert!(r.is_err());
        assert_eq!(fake.sent, vec![SPACE.to_vec(), RIGHT.to_vec()]);
        assert!(!fake.model.submitted);
    }

    #[test]
    fn questions_that_read_the_same_are_refused_before_any_key() {
        let qs = [DB, DB];
        let mut fake = Fake::new(Model::new(&qs), &[]);
        let r = block_on(drive_question_answers(&mut fake, &input(&qs), &[pick(&["Redis"]), pick(&["Redis"])]));
        assert!(r.unwrap_err().contains("read the same"));
        assert!(fake.sent.is_empty());
    }

    #[test]
    fn other_text_goes_in_as_text_with_no_control_keys() {
        // A Tab would move a multiSelect's focus to the Next button, an ESC would cancel the ask.
        let answers = [Answer { selected: vec![], other: Some("Carts\tand\u{1b}more\u{7f}\n".into()) }];
        let (r, fake) = run(&[CACHE], &answers, &[]);
        assert_eq!(r, Ok(()));
        for key in &fake.sent {
            if key.len() > 1 && key != DOWN && key != UP && key != RIGHT {
                assert!(!key.iter().any(|b| b.is_ascii_control()), "control byte in typed text: {key:?}");
            }
        }
        assert!(!fake.sent.iter().flatten().any(|&b| b == b'\t'));
        assert_eq!(fake.model.other[0], "Carts and more  ");
        assert!(fake.model.submitted);
    }

    #[test]
    fn a_bad_answer_rejects_the_batch_before_any_key() {
        let (r, fake) = run(&[DB], &[pick(&["Mongo"])], &[]);
        assert!(r.unwrap_err().contains("not among options"));
        assert!(fake.sent.is_empty());
    }

    #[test]
    fn the_selector_is_read_off_a_real_layout() {
        // From the question_on_screen fixtures (mailink/mod.rs tests): rows, highlight, boxes.
        let multi = " Which database should the new service use for its queue?\n\n ❯ 1. [ ] Postgres\n   2. [✓] Redis\n   3. [ ] Type something.\n       Next\n\n Enter to select · Esc to cancel\n";
        let q = Shown::of(&json!({ "question": "Which database should the new service use for its queue?", "options": [{ "label": "Postgres" }] })).unwrap();
        let v = question_view(multi, &q).unwrap();
        assert_eq!(v.focused(), Some(1));
        assert_eq!(v.rows.iter().map(|r| r.ticked).collect::<Vec<_>>(), vec![Some(false), Some(true), Some(false)]);
        assert!(!v.button_focused);
        // The same question text above a permission dialog is not its selector.
        let perm = " Which database should the new service use for its queue?\n Bash command\n Do you want to proceed?\n ❯ 1. Yes\n   2. No\n";
        assert!(question_view(perm, &q).is_none());
    }
}
