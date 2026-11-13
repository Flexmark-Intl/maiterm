//! `getReviewBrief` (docs/overlord.md §9.5): how an agent gets its own work reviewed.
//!
//! The Overlord's "Review after commit" rule types one short line; the method lives here so it
//! can change without migrating anyone's copy of the rule, and so it can carry what a static rule
//! can't — the commits since this tab's last review. The wording follows what made a reviewer
//! better in Anthropic's write-up of its own code review: findings need a file, a line and a
//! failure, noise is cut before it reaches an agent author (who answers a nit with a commit and
//! another round), pre-existing bugs are kept out of the change, and review has to converge.
//!
//! maiTerm runs no git here: an ssh tab's repo is on another host, so the agent passes its repo
//! and HEAD and runs the diff itself. A tab's mark moves on every call that names a new HEAD,
//! whether or not the review then runs — calling the brief is the agent saying it is reviewing.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::state::workspace::ReviewMark;
use crate::state::AppState;

pub const TOOL: &str = "getReviewBrief";

pub fn tool_definition() -> Value {
    json!({
        "name": TOOL,
        "description": "How to get your own work reviewed: rules for you, a brief to hand a reviewer subagent unchanged, and the commit range to review — everything since this tab's last review, not just the newest commit. Call it when you are about to review work (the Overlord asks after a commit). Pass repo and head so the range is right.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                "repo": { "type": "string", "description": "The repo's top level: output of `git rev-parse --show-toplevel`." },
                "head": { "type": "string", "description": "The commit you are reviewing up to: output of `git rev-parse HEAD`." }
            },
            "required": []
        }
    })
}

/// What the call leaves behind and what it answers with. Pure, for the tests.
#[derive(Debug, PartialEq)]
enum Range {
    /// No repo/head given: nothing recorded, the agent works out the range.
    Unknown,
    /// No earlier review in this repo.
    FirstReview { head: String },
    /// HEAD is what was reviewed last time.
    AlreadyReviewed { head: String },
    Since { since: String, head: String },
}

fn is_sha(s: &str) -> bool {
    (7..=64).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

fn plan(prev: Option<&ReviewMark>, repo: &str, head: &str) -> Range {
    match prev.filter(|m| m.repo == repo) {
        // Short and full spellings of one commit are the same review.
        Some(m) if m.head.starts_with(head) || head.starts_with(&m.head) => Range::AlreadyReviewed { head: head.to_string() },
        Some(m) => Range::Since { since: m.head.clone(), head: head.to_string() },
        None => Range::FirstReview { head: head.to_string() },
    }
}

const FOR_YOU: &str = "\
1. Size it: `git diff --shortstat {range}`. A few lines of wiring, docs or config with no logic: you may skip the review, and say so. Up to ~1000 changed lines: one reviewer. More than that, or a change to security, persistence, concurrency or anything that deletes data: go deep — two or three reviewers in parallel, each on a different concern, and a separate fresh subagent confirms each Important finding before you act on it.
2. Start each reviewer as a subagent with fresh context. Give it the reviewer brief below unchanged, the range, and one or two sentences on what the change is meant to do. Do not tell it why you think the code is right or what you expect it to find: a reviewer handed your conclusion tends to hand it back.
3. Check every finding against the code before changing anything. One you can't confirm is rejected, with one line saying why. Fix the confirmed Important ones, and every other instance of the same class, not only the line named.
4. Pre-existing bugs (in code this range didn't touch or make worse) are not fixed in this change: put each on the task board, or list it in your summary.
5. After fixing, review only the fix's diff, with a fresh reviewer, the same brief, and the list of what was already reported so it isn't repeated. If that second round still finds an Important bug, stop patching: say what keeps going wrong and reconsider the approach before a third round.
6. Commit the fixes and carry on. End with the counts: found, confirmed, fixed, rejected, sent to the board.";

const REVIEWER_BRIEF: &str = "\
You are reviewing a change for correctness bugs. You have not seen the work, and that is the point.
- Read the change: `git log {range}` and `git diff {range}`. Read the repo's REVIEW.md if there is one, and the CLAUDE.md or contributor docs for the files touched.
- Trace each changed path into its callers and into the state it reads and writes. Look for what breaks: wrong results, crashes, lost or corrupted data, races, security holes, a check that no longer runs, a doc or comment that now says something false.
- A finding needs evidence: file:line and a concrete failure (this input or state leads to this wrong outcome). A suspicion from a name, or \"this might\", is not a finding.
- Skip style, naming, formatting, refactors and missing tests, unless a missing test hides a real bug. At most three nits.
- Tag each finding Important (a bug this change introduces or makes reachable), Nit, or Pre-existing (a real bug the change didn't cause).
- When findings share a cause, name the class (\"every caller of X ignores its error\") and list every instance you found.
- Start your answer with the counts (\"2 Important, 0 Nit, 1 Pre-existing\"). If there is nothing to fix, say so in one line.
- Don't edit files. If you run anything, keep it away from real user data and config: use a scratch directory and a scratch HOME.";

fn answer(range: &Range) -> Value {
    let (range_text, note, recorded) = match range {
        Range::Unknown => (
            "<range>".to_string(),
            "No repo and head given, so nothing was recorded and no range is known. Pass repo and head next time; for now review the commits you made for this piece of work since your last review.".to_string(),
            false,
        ),
        Range::FirstReview { head } => (
            format!("<start>..{head}"),
            format!("No earlier review is recorded for this tab in this repo. Review the commits you made for this piece of work: find where they start with `git log`, and use the commit before the first one as <start>. If you can't tell, review `{head}~1..{head}`."),
            true,
        ),
        Range::AlreadyReviewed { head } => {
            return json!({
                "skip": true,
                "head": head,
                "note": "This commit was already reviewed from this tab, and nothing has been committed since. There is nothing new to review: carry on.",
            });
        }
        Range::Since { since, head } => (
            format!("{since}..{head}"),
            format!("Everything committed since this tab's last review. If `git merge-base --is-ancestor {since} {head}` fails, history was rewritten or you changed branch: review the commits you made instead."),
            true,
        ),
    };
    json!({
        "range": range_text,
        "rangeNote": note,
        "recorded": recorded,
        "forYou": FOR_YOU.replace("{range}", &range_text),
        "reviewerBrief": REVIEWER_BRIEF.replace("{range}", &range_text),
    })
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn handle(arguments: &Value, state: &Arc<AppState>) -> Value {
    let Some(tab_id) = arguments.get("tabId").and_then(|v| v.as_str()).filter(|t| !t.is_empty()) else {
        return json!({ "error": "Missing tabId — call initSession first" });
    };
    let text = |k: &str| arguments.get(k).and_then(|v| v.as_str()).map(str::trim).filter(|s| !s.is_empty());
    let (repo, head) = match (text("repo"), text("head")) {
        (Some(repo), Some(head)) => {
            if !is_sha(head) {
                return json!({ "error": "head must be a commit hash: the output of `git rev-parse HEAD`." });
            }
            if repo.len() > 4096 {
                return json!({ "error": "repo is too long: pass the output of `git rev-parse --show-toplevel`." });
            }
            (repo, head.to_ascii_lowercase())
        }
        (None, None) => return answer(&Range::Unknown),
        _ => return json!({ "error": "Pass both repo and head, or neither." }),
    };

    let (range, data) = {
        let mut app_data = state.app_data.write();
        let Some(tab) = app_data
            .windows
            .iter_mut()
            .flat_map(|w| &mut w.workspaces)
            .flat_map(|ws| &mut ws.panes)
            .flat_map(|p| &mut p.tabs)
            .find(|t| t.id == tab_id)
        else {
            return json!({ "error": format!("Tab '{tab_id}' not found") });
        };
        let range = plan(tab.last_review.as_ref(), repo, &head);
        if matches!(range, Range::AlreadyReviewed { .. }) {
            (range, None)
        } else {
            tab.last_review = Some(ReviewMark { repo: repo.to_string(), head: head.clone(), at: now_secs() });
            (range, Some(app_data.clone()))
        }
    };
    if let Some(data) = data {
        if let Err(e) = crate::state::save_state(&data) {
            log::warn!("[review] failed to persist the review mark for tab {tab_id}: {e}");
        }
    }
    answer(&range)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(repo: &str, head: &str) -> ReviewMark {
        ReviewMark { repo: repo.into(), head: head.into(), at: 1 }
    }

    #[test]
    fn the_range_runs_from_the_last_review_in_the_same_repo() {
        assert_eq!(
            plan(Some(&mark("/r", "aaaaaaa")), "/r", "bbbbbbb"),
            Range::Since { since: "aaaaaaa".into(), head: "bbbbbbb".into() }
        );
        // Another repo's SHA means nothing here.
        assert_eq!(plan(Some(&mark("/other", "aaaaaaa")), "/r", "bbbbbbb"), Range::FirstReview { head: "bbbbbbb".into() });
        assert_eq!(plan(None, "/r", "bbbbbbb"), Range::FirstReview { head: "bbbbbbb".into() });
    }

    #[test]
    fn the_same_commit_twice_is_not_a_second_review() {
        let full = "aaaaaaa1234567890aaaaaaa1234567890aaaaaa";
        assert_eq!(plan(Some(&mark("/r", full)), "/r", "aaaaaaa"), Range::AlreadyReviewed { head: "aaaaaaa".into() });
        assert_eq!(plan(Some(&mark("/r", "aaaaaaa")), "/r", full), Range::AlreadyReviewed { head: full.into() });
        assert_eq!(answer(&Range::AlreadyReviewed { head: "aaaaaaa".into() })["skip"], true);
    }

    #[test]
    fn the_brief_names_the_range_everywhere_it_is_used() {
        let out = answer(&Range::Since { since: "aaaaaaa".into(), head: "bbbbbbb".into() });
        for key in ["forYou", "reviewerBrief"] {
            let s = out[key].as_str().unwrap();
            assert!(s.contains("aaaaaaa..bbbbbbb"), "{key}");
            assert!(!s.contains("{range}"), "{key} left a placeholder");
        }
        assert_eq!(out["recorded"], true);
        assert_eq!(answer(&Range::Unknown)["recorded"], false);
    }

    #[test]
    fn only_a_hash_is_a_head() {
        assert!(is_sha("abc1234"));
        assert!(!is_sha("HEAD"));
        assert!(!is_sha("abc12"));
        assert!(!is_sha("abc1234; rm -rf ~"));
    }
}
