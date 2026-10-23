//! The agent ledger (docs/file-tree.md §20 phase 4): recording, dedupe, failures, the ring,
//! remap, the agent-in-flight guard, coalescing, and end to end through a hub: marks reaching
//! a subscriber, shell writes attributed only to a sole shell call, commits credited to a sole
//! open turn.
//!
//! Hook envelopes here follow what `hooks_handler` is sent: the maiterm-tab mod's PreToolUse
//! (`resources/claude-mod/maiterm-tab/hooks/register.ts`: session_id, cwd, tool_name,
//! tool_input, tool_use_id, agent_id), and Claude's PostToolUseFailure as Claude Code documents
//! it (tool_name, tool_input, tool_use_id, error): no failure is captured on disk here.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::json;

use super::extract::extract;
use super::*;
use crate::filetree::hub::Sink;
use crate::filetree::types::{Envelope, TreeEvent};

fn ledger() -> Arc<AgentFiles> {
    AgentFiles::new(Weak::new())
}

/// A record built the way `record_agent_files` builds one from a hook body.
fn rec(event: &serde_json::Value, tab: &str) -> HookRecord {
    let name = event["hook_event_name"].as_str().unwrap();
    let tool = event.get("tool_name").and_then(|v| v.as_str()).unwrap_or("").to_string();
    HookRecord {
        phase: Phase::of(name, event).unwrap(),
        tab_id: tab.into(),
        session_id: event["session_id"].as_str().unwrap_or("s1").into(),
        runtime: "claude".into(),
        cwd: event.get("cwd").and_then(|v| v.as_str()).map(String::from),
        host: None,
        tool: tool.clone(),
        tool_use_id: event.get("tool_use_id").and_then(|v| v.as_str()).map(String::from),
        agent: crate::claude_code::gate::agent_key(event),
        extracted: event.get("tool_input").map(|i| extract(&tool, i)).unwrap_or(Extracted::Nothing),
        existed: None,
    }
}

fn pre(tool: &str, input: serde_json::Value, id: &str, cwd: &str) -> serde_json::Value {
    json!({ "hook_event_name": "PreToolUse", "session_id": "s1", "cwd": cwd, "tool_name": tool, "tool_input": input, "tool_use_id": id })
}

fn post(tool: &str, input: serde_json::Value, id: &str, cwd: &str) -> serde_json::Value {
    json!({ "hook_event_name": "PostToolUse", "session_id": "s1", "cwd": cwd, "tool_name": tool, "tool_input": input, "tool_use_id": id, "tool_response": {} })
}

fn prompt(cwd: &str) -> serde_json::Value {
    json!({ "hook_event_name": "UserPromptSubmit", "session_id": "s1", "cwd": cwd, "prompt": "go" })
}

fn stop(cwd: &str) -> serde_json::Value {
    json!({ "hook_event_name": "Stop", "session_id": "s1", "cwd": cwd })
}

/// The folder the ledger-only tests place their calls in. Built from the temp dir because a
/// POSIX literal like `/repo` is relative on Windows, where the ledger rightly can't place it.
/// Nothing is created there: these tests never touch the disk.
fn repo() -> String {
    std::env::temp_dir().join("af-repo").to_string_lossy().into_owned()
}

/// `rel` (slash-separated) under `repo()`, spelled the native way the ledger stores it.
fn at(rel: &str) -> String {
    let mut p = PathBuf::from(repo());
    p.extend(rel.split('/'));
    p.to_string_lossy().into_owned()
}

#[test]
fn the_raw_failure_event_is_kept_apart_from_success() {
    let none = json!({});
    assert_eq!(Phase::of("PostToolUseFailure", &none), Some(Phase::Post { failed: true }));
    assert_eq!(Phase::of("PostToolUse", &none), Some(Phase::Post { failed: false }));
    assert_eq!(Phase::of("Notification", &json!({ "notification_type": "permission_prompt" })), None);
    assert_eq!(Phase::of("Notification", &json!({ "notification_type": "idle_prompt" })), Some(Phase::Idle));
    assert_eq!(Phase::of("SubagentStop", &none), Some(Phase::SubagentStop));
    assert_eq!(Phase::of("Interrupt", &none), Some(Phase::Interrupt));
}

fn idle(cwd: &str) -> serde_json::Value {
    json!({ "hook_event_name": "Notification", "session_id": "s1", "cwd": cwd, "notification_type": "idle_prompt", "message": "Claude is waiting for your input" })
}

fn subagent_stop(agent: &str, cwd: &str) -> serde_json::Value {
    json!({ "hook_event_name": "SubagentStop", "session_id": "s1", "cwd": cwd, "agent_id": agent, "agent_type": "general-purpose", "stop_hook_active": false })
}

/// A call as a subagent makes it: the parent's session, the subagent's `agent_id`.
fn by_agent(mut event: serde_json::Value, agent: &str) -> serde_json::Value {
    event["agent_id"] = json!(agent);
    event
}

fn status_of(l: &AgentFiles, tab: &str, id: &str) -> String {
    l.activity(tab, "session", true).unwrap().entries.into_iter().find(|e| e.tool_use_id.as_deref() == Some(id)).unwrap().status
}

fn running_shells(l: &AgentFiles) -> Vec<String> {
    l.inner.lock().shells.iter().filter(|s| s.running()).map(|s| s.tool_use_id.clone()).collect()
}

#[test]
fn a_denied_subagent_call_is_settled_when_the_subagent_stops() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&by_agent(pre("Edit", json!({ "file_path": at("a.ts") }), "e1", &repo()), "a1"), "t1"));
    l.record(rec(&by_agent(pre("Bash", json!({ "command": "make" }), "b1", &repo()), "a2"), "t1"));
    // Denied: no Post. The main agent's turn ending leaves a subagent's calls alone.
    l.record(rec(&stop(&repo()), "t1"));
    assert_eq!(status_of(&l, "t1", "e1"), "pending");
    assert_eq!(running_shells(&l), vec!["b1".to_string()]);
    l.record(rec(&subagent_stop("a1", &repo()), "t1"));
    assert_eq!(status_of(&l, "t1", "e1"), "failed");
    assert!(l.active_tabs(&[PathBuf::from(at("a.ts"))], false).is_empty(), "nothing in flight on it any more");
    assert_eq!(running_shells(&l), vec!["b1".to_string()], "another subagent's shell call runs on");
    l.record(rec(&subagent_stop("a2", &repo()), "t1"));
    assert!(running_shells(&l).is_empty());
}

#[test]
fn an_agent_that_moves_on_has_left_its_denied_call_behind() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&by_agent(pre("Edit", json!({ "file_path": at("a.ts") }), "e1", &repo()), "a1"), "t1"));
    l.record(rec(&by_agent(pre("Bash", json!({ "command": "make" }), "b1", &repo()), "a1"), "t1"));
    assert_eq!(status_of(&l, "t1", "e1"), "failed", "Claude runs an edit on its own: a later call means it never ran");
    // A read may run beside another call, so it is left alone; the Bash before it is over.
    l.record(rec(&by_agent(pre("Read", json!({ "file_path": at("b.ts") }), "r1", &repo()), "a1"), "t1"));
    l.record(rec(&by_agent(pre("Grep", json!({ "pattern": "x", "path": repo() }), "g1", &repo()), "a1"), "t1"));
    assert_eq!(status_of(&l, "t1", "r1"), "pending");
    assert!(running_shells(&l).is_empty());
    // Another agent's call is not this one's evidence.
    l.record(rec(&pre("Edit", json!({ "file_path": at("c.ts") }), "e2", &repo()), "t1"));
    l.record(rec(&by_agent(pre("Edit", json!({ "file_path": at("d.ts") }), "e3", &repo()), "a2"), "t1"));
    assert_eq!(status_of(&l, "t1", "e2"), "pending");
    // A Post that turns up after all still wins.
    l.record(rec(&by_agent(post("Edit", json!({ "file_path": at("a.ts") }), "e1", &repo()), "a1"), "t1"));
    assert_eq!(status_of(&l, "t1", "e1"), "ok");
}

#[test]
fn codex_calls_may_overlap_so_a_new_one_settles_nothing() {
    let l = ledger();
    let mut r = rec(&pre("shell", json!({ "command": ["bash", "-lc", "make"] }), "c1", &repo()), "t1");
    r.runtime = "codex".into();
    l.record(r);
    let mut r = rec(&pre("shell", json!({ "command": ["bash", "-lc", "make test"] }), "c2", &repo()), "t1");
    r.runtime = "codex".into();
    l.record(r);
    assert_eq!(running_shells(&l), vec!["c1".to_string(), "c2".to_string()]);
}

#[test]
fn an_esc_or_a_denial_ends_the_turn_at_idle_prompt() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&pre("Edit", json!({ "file_path": at("a.ts") }), "e1", &repo()), "t1"));
    l.record(rec(&by_agent(pre("Write", json!({ "file_path": at("b.ts"), "content": "x" }), "w1", &repo()), "a1"), "t1"));
    l.record(rec(&pre("Bash", json!({ "command": "git commit" }), "b1", &repo()), "t1"));
    l.record(rec(&by_agent(pre("Bash", json!({ "command": "make" }), "b2", &repo()), "a2"), "t1"));
    // Esc: no Post, no Stop. Claude's idle_prompt is the first word that the turn is over.
    l.record(rec(&idle(&repo()), "t1"));
    let a = l.activity("t1", "session", true).unwrap();
    assert!(!a.turn_open);
    assert!(a.entries.iter().all(|e| e.status == "failed"), "no dialog is open, so nothing is still waiting: {:?}", a.entries);
    assert!(l.sole_open_turn(&|_| true).is_none(), "a later commit is not credited to it");
    assert_eq!(running_shells(&l), vec!["b2".to_string()], "a background subagent's Bash may really still run");
}

#[test]
fn a_codex_interrupt_ends_the_turn_and_every_call() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&pre("Bash", json!({ "command": "make" }), "b1", &repo()), "t1"));
    l.record(rec(&json!({ "hook_event_name": "Interrupt", "session_id": "s1", "cwd": repo() }), "t1"));
    assert!(!l.activity("t1", "turn", true).unwrap().turn_open);
    assert!(running_shells(&l).is_empty());
}

#[test]
fn a_new_session_ends_the_old_ones_shell_calls() {
    let l = ledger();
    l.record(rec(&by_agent(pre("Bash", json!({ "command": "make" }), "b1", &repo()), "a1"), "t1"));
    let mut fresh = prompt(&repo());
    fresh["session_id"] = json!("s2");
    l.record(rec(&fresh, "t1"));
    assert!(running_shells(&l).is_empty());
}

#[test]
fn a_new_session_numbers_its_turns_afresh_from_its_session_start() {
    let l = ledger();
    for _ in 0..3 {
        l.record(rec(&prompt(&repo()), "t1"));
        l.record(rec(&stop(&repo()), "t1"));
    }
    assert_eq!(l.turn_of("t1"), Some(3));
    // A compaction keeps the session id: the count goes on.
    l.record(rec(&json!({ "hook_event_name": "SessionStart", "session_id": "s1", "cwd": repo(), "source": "compact" }), "t1"));
    assert_eq!(l.turn_of("t1"), Some(3));
    // A /clear mints a new id: before its first prompt, nothing of the old session's count is
    // left for a window opening its Files column to start from.
    l.record(rec(&json!({ "hook_event_name": "SessionStart", "session_id": "s2", "cwd": repo(), "source": "clear" }), "t1"));
    assert_eq!(l.turn_of("t1"), None);
    assert!(l.turns(None).is_empty());
    let mut p = prompt(&repo());
    p["session_id"] = json!("s2");
    l.record(rec(&p, "t1"));
    assert_eq!(l.turn_of("t1"), Some(1));
    assert_eq!(l.turns(None)[0].turn, 1);
}

#[test]
fn a_shell_call_no_hook_ever_ends_stops_counting_after_ten_minutes() {
    let l = ledger();
    l.record(rec(&by_agent(pre("Bash", json!({ "command": "make" }), "b1", &repo()), "a1"), "t1"));
    assert!(l.sole_shell_in(Path::new(&repo()), false).is_some());
    for s in l.inner.lock().shells.iter_mut() {
        s.started -= SHELL_MAX;
    }
    assert!(l.sole_shell_in(Path::new(&repo()), false).is_none());
    assert!(l.inner.lock().shells.is_empty(), "and it is dropped, so the watcher pays nothing for it");
}

#[test]
fn pre_is_recorded_once_whichever_copy_arrives_first() {
    let l = ledger();
    let edit = json!({ "replace_all": false, "file_path": at("src/a.ts"), "old_string": "a", "new_string": "b" });
    l.record(rec(&prompt(&repo()), "t1"));
    // The mod's relay, then the settings hook's copy of the same call.
    l.record(rec(&pre("Edit", edit.clone(), "toolu_01P7vmSN1gCE2Kts811bbr1t", &repo()), "t1"));
    l.record(rec(&pre("Edit", edit.clone(), "toolu_01P7vmSN1gCE2Kts811bbr1t", &repo()), "t1"));
    let a = l.activity("t1", "turn", false).unwrap();
    assert_eq!(a.entries.len(), 1);
    assert_eq!((a.entries[0].op.as_str(), a.entries[0].status.as_str(), a.entries[0].evidence.as_str()), ("edit", "pending", "tool"));
    l.record(rec(&post("Edit", edit, "toolu_01P7vmSN1gCE2Kts811bbr1t", &repo()), "t1"));
    let a = l.activity("t1", "turn", false).unwrap();
    assert_eq!((a.entries.len(), a.entries[0].status.as_str()), (1, "ok"));
    assert!(a.entries[0].t_post_ms.is_some());
}

#[test]
fn a_failed_call_stays_visible_as_failed() {
    let l = ledger();
    let write = json!({ "file_path": at("out.txt"), "content": "x" });
    l.record(rec(&prompt(&repo()), "t1"));
    let mut p = rec(&pre("Write", write.clone(), "toolu_w", &repo()), "t1");
    p.existed = Some(false);
    l.record(p);
    let failure = json!({ "hook_event_name": "PostToolUseFailure", "session_id": "s1", "cwd": repo(), "tool_name": "Write",
        "tool_input": write, "tool_use_id": "toolu_w", "error": "EACCES: permission denied" });
    l.record(rec(&failure, "t1"));
    let a = l.activity("t1", "turn", false).unwrap();
    assert_eq!((a.entries[0].op.as_str(), a.entries[0].status.as_str()), ("create", "failed"), "a Write of a missing file is a create");
}

#[test]
fn reads_are_left_out_unless_asked_and_turns_scope_the_list() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&pre("Read", json!({ "file_path": at("a.ts"), "limit": 22 }), "r1", &repo()), "t1"));
    l.record(rec(&pre("Edit", json!({ "file_path": at("b.ts"), "old_string": "a", "new_string": "b" }), "e1", &repo()), "t1"));
    l.record(rec(&stop(&repo()), "t1"));
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&pre("Grep", json!({ "pattern": "x", "path": at("src") }), "g1", &repo()), "t1"));
    assert_eq!(l.activity("t1", "turn", false).unwrap().entries.len(), 0);
    assert_eq!(l.activity("t1", "turn", true).unwrap().entries.len(), 1);
    assert_eq!(l.activity("t1", "session", false).unwrap().entries.len(), 1);
    assert_eq!(l.activity("t1", "session", true).unwrap().entries.len(), 3);
    // The Edit never got its Post before the turn ended: it stopped pulsing as failed.
    assert_eq!(l.activity("t1", "session", false).unwrap().entries[0].status, "failed");
}

#[test]
fn a_codex_patch_resolves_against_the_session_folder() {
    let l = ledger();
    let patch = "*** Begin Patch\n*** Update File: src/routes/+layout.ts\n@@\n-a\n+b\n*** End Patch";
    l.record(rec(&pre("shell", json!({ "command": ["apply_patch", patch] }), "call_IXwH9ULaOHVmXcQU8KIk9thN", &repo()), "t1"));
    let a = l.activity("t1", "turn", false).unwrap();
    assert_eq!(a.entries[0].path, at("src/routes/+layout.ts"));
}

#[test]
fn an_ssh_tabs_paths_carry_its_host() {
    assert_eq!(entry_path("src/x.ts", Some("/home/ews/p"), Some("ews@nova")).as_deref(), Some("ews@nova:/home/ews/p/src/x.ts"));
    assert_eq!(entry_path("/home/ews/p/../q/y.ts", None, Some("ews@nova")).as_deref(), Some("ews@nova:/home/ews/q/y.ts"));
    assert_eq!(entry_path("rel.ts", None, None), None, "a relative path with no folder can't be placed");
}

#[test]
fn the_ring_keeps_the_newest_two_thousand() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    for i in 0..(RING + 150) {
        l.record(rec(&pre("Edit", json!({ "file_path": at(&format!("f{i}.ts")), "old_string": "a", "new_string": "b" }), &format!("id{i}"), &repo()), "t1"));
    }
    let a = l.activity("t1", "session", true).unwrap();
    assert_eq!(a.entries.len(), RING);
    assert_eq!(a.entries[0].path, at("f150.ts"));
}

#[test]
fn a_reload_moves_the_ledger_and_a_closed_tab_is_forgotten_after_a_minute() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "old"));
    l.record(rec(&pre("Edit", json!({ "file_path": at("a.ts") }), "e1", &repo()), "old"));
    // The reload deletes the old tab first: a prune now must not lose it.
    l.prune(&HashSet::new());
    l.remap_tab("old", "new");
    assert!(l.activity("old", "turn", false).is_none());
    assert_eq!(l.activity("new", "turn", false).unwrap().entries.len(), 1);
    l.prune(&HashSet::new());
    assert!(l.activity("new", "turn", false).is_some(), "within the grace");
    l.age_missing(MISSING_GRACE);
    l.prune(&HashSet::new());
    assert!(l.activity("new", "turn", false).is_none());
}

#[test]
fn a_turn_that_only_read_is_still_the_tabs_current_one() {
    let l = ledger();
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&pre("Edit", json!({ "file_path": at("a.ts"), "old_string": "a", "new_string": "b" }), "e1", &repo()), "t1"));
    l.record(rec(&stop(&repo()), "t1"));
    l.record(rec(&prompt(&repo()), "t1"));
    l.record(rec(&pre("Read", json!({ "file_path": at("b.ts") }), "r1", &repo()), "t1"));
    // The standing marks know only turn 1 (the edit); the window must hear turn 2 from here.
    assert!(l.standing_marks(None).iter().all(|m| m.turn == 1));
    let t = l.turns(None);
    assert_eq!(t.len(), 1);
    assert_eq!((t[0].turn, t[0].open, t[0].end_ms), (2, true, None));
    assert!(t[0].start_ms > 0);
    // Stop ends it, and a later idle notification does not move the end.
    l.record(rec(&stop(&repo()), "t1"));
    let ended = l.turns(None)[0].end_ms.expect("ended at the Stop");
    std::thread::sleep(Duration::from_millis(5));
    l.record(rec(&idle(&repo()), "t1"));
    assert_eq!(l.turns(None)[0].end_ms, Some(ended));
    // Only the asked tabs; a new session starts at no turn and is left out until it has one.
    assert!(l.turns(Some(&HashSet::from(["t2".to_string()]))).is_empty());
    let mut fresh = prompt(&repo());
    fresh["session_id"] = json!("s2");
    l.record(rec(&fresh, "t1"));
    assert_eq!(l.turns(None)[0].turn, 1);
}

#[test]
fn a_new_session_starts_its_own_history() {
    let l = ledger();
    l.record(rec(&pre("Edit", json!({ "file_path": at("a.ts") }), "e1", &repo()), "t1"));
    let mut fresh = prompt(&repo());
    fresh["session_id"] = json!("s2");
    l.record(rec(&fresh, "t1"));
    assert!(l.activity("t1", "session", true).unwrap().entries.is_empty());
}

#[test]
fn the_guard_names_a_tab_writing_inside_a_folder_until_its_turn_ends() {
    let root = std::env::temp_dir().join(format!("af-guard-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(root.join("src")).unwrap();
    let root = crate::share::canon(&root);
    let file = root.join("src/a.ts");
    let cwd = root.to_string_lossy().into_owned();
    let l = ledger();
    l.record(rec(&prompt(&cwd), "t1"));
    l.record(rec(&pre("Read", json!({ "file_path": root.join("README.md") }), "r1", &cwd), "t1"));
    assert!(l.active_tabs(&[root.clone()], false).is_empty(), "a read is no write");
    l.record(rec(&pre("Edit", json!({ "file_path": file }), "e1", &cwd), "t1"));
    assert_eq!(l.active_tabs(&[root.join("src")], false), vec!["t1".to_string()], "in flight inside the folder");
    assert!(l.active_tabs(&[root.join("other")], false).is_empty());
    l.record(rec(&post("Edit", json!({ "file_path": file }), "e1", &cwd), "t1"));
    assert_eq!(l.active_tabs(&[file.clone()], false), vec!["t1".to_string()], "written this turn");
    l.record(rec(&stop(&cwd), "t1"));
    assert!(l.active_tabs(&[file], false).is_empty(), "the turn is over");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn coalescing_sends_at_most_one_batch_per_tree_per_window() {
    let mut co: Coalescer<&'static str> = Coalescer::default();
    let mark = |rel: &str, phase: &str| AgentMark { rel: rel.into(), op: "edit".into(), phase: phase.into(), tab_id: "t".into(), agent: String::new(), turn: 1, evidence: "tool".into(), at_ms: 0 };
    let t0 = Instant::now();
    co.push("/a", &"a", mark("x", "pending"));
    // The first mark after a quiet spell goes at once.
    let first = co.take_due(t0);
    assert_eq!(first.len(), 1);
    co.push("/a", &"a", mark("x", "ok"));
    co.push("/a", &"a", mark("y", "pending"));
    co.push("/b", &"b", mark("z", "pending"));
    let early = co.take_due(t0 + Duration::from_millis(50));
    assert_eq!(early.iter().map(|(h, _)| *h).collect::<Vec<_>>(), vec!["b"], "another tree has its own window");
    assert!(co.next_due().is_some());
    let late = co.take_due(t0 + COALESCE);
    assert_eq!(late.len(), 1);
    let marks = &late[0].1;
    assert_eq!(marks.len(), 2, "one mark per path: the latest");
    assert_eq!(marks.iter().find(|m| m.rel == "x").unwrap().phase, "ok");
    assert!(co.next_due().is_none(), "nothing waiting");
}

// ── End to end through a hub ────────────────────────────────────────────────────────────────

#[derive(Default)]
struct Recorder {
    msgs: Mutex<Vec<Envelope>>,
}

impl Sink for Recorder {
    fn send(&self, env: Envelope) -> bool {
        self.msgs.lock().push(env);
        true
    }
}

impl Recorder {
    fn marks(&self) -> Vec<AgentMark> {
        self.msgs.lock().iter().filter_map(|m| if let TreeEvent::Agent { marks } = &m.ev { Some(marks.clone()) } else { None }).flatten().collect()
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("af-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&t).unwrap();
    crate::share::canon(&t)
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// A tree on `root` whose watcher is running and whose root listing has arrived.
fn open_tree(root: &Path, excludes: Vec<String>) -> (Arc<FileTreeState>, Arc<Recorder>, u64) {
    let ft = FileTreeState::with(Duration::from_secs(30), crate::filetree::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), excludes).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    wait_for("the watcher", Duration::from_secs(30), || !hub.watch_pending());
    wait_for("the root listing", Duration::from_secs(10), || rec.msgs.lock().iter().any(|m| matches!(&m.ev, TreeEvent::Dir { rel, last: true, .. } if rel.is_empty())));
    // FSEvents may report what happened just before the stream started: let that settle.
    std::thread::sleep(Duration::from_millis(400));
    (ft, rec, ack.sub_id)
}

/// One tree for every end-to-end check: each FSEvents stream start is queued behind the
/// process's others (about 2 s apiece on a busy machine), and the suite's other watcher tests
/// wait at most 30 s for theirs.
#[test]
fn end_to_end_through_one_tree() {
    let root = temp_root("e2e");
    git(&root, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("build")).unwrap();
    std::fs::write(root.join("src/a.ts"), "a").unwrap();
    git(&root, &["add", "src"]);
    git(&root, &["commit", "-q", "--no-verify", "-m", "init"]);
    let (ft, rec, sub) = open_tree(&root, vec!["build/".into()]);
    let cwd = root.to_string_lossy().into_owned();
    a_tool_call_lights_its_row(&ft, &rec, &root, &cwd);
    one_shell_call_is_credited_two_are_not_and_ignored_paths_never(&ft, &root, &cwd);
    a_commit_during_the_sole_open_turn_is_credited_to_it(&ft, &rec, &root, &cwd);
    ft.close(sub);
    std::fs::remove_dir_all(&root).unwrap();
}

fn a_tool_call_lights_its_row(ft: &Arc<FileTreeState>, rec: &Recorder, root: &Path, cwd: &str) {
    let cwd = cwd.to_string();
    let input = json!({ "file_path": root.join("src/a.ts"), "old_string": "a", "new_string": "b" });
    let t0 = Instant::now();
    ft.agents.record(rec_for(&pre("Edit", input.clone(), "e1", &cwd), "t1"));
    wait_for("the pending mark", Duration::from_secs(2), || rec.marks().iter().any(|m| m.rel == "src/a.ts" && m.phase == "pending"));
    let took = t0.elapsed();
    ft.agents.record(rec_for(&post("Edit", input, "e1", &cwd), "t1"));
    wait_for("the ok mark", Duration::from_secs(2), || rec.marks().iter().any(|m| m.rel == "src/a.ts" && m.phase == "ok"));
    // The budget is a pulse within 150 ms of the hook (§17); allow a loaded test machine.
    assert!(took < Duration::from_millis(500), "the first mark took {took:?}");
    let m = rec.marks().into_iter().find(|m| m.phase == "ok").unwrap();
    assert_eq!((m.tab_id.as_str(), m.evidence.as_str(), m.op.as_str()), ("t1", "tool", "edit"));
    // A path outside the root reaches nobody.
    let before = rec.marks().len();
    ft.agents.record(rec_for(&pre("Read", json!({ "file_path": std::env::temp_dir().join("af-elsewhere").join("x.ts") }), "r1", &cwd), "t1"));
    std::thread::sleep(Duration::from_millis(250));
    assert_eq!(rec.marks().len(), before);
    // A subscriber that arrives later is primed with what still stands.
    let late = Arc::new(Recorder::default());
    let ack = ft.open("w2", &root.to_string_lossy(), vec![], late.clone(), vec![]).unwrap();
    assert!(late.marks().iter().any(|m| m.rel == "src/a.ts" && m.phase == "ok"));
    ft.close(ack.sub_id);
    ft.agents.record(rec_for(&stop(&cwd), "t1"));
}

/// `rec` with the tab's cwd as the event names it, for the hub tests.
fn rec_for(event: &serde_json::Value, tab: &str) -> HookRecord {
    rec(event, tab)
}

fn bash(id: &str, cwd: &str) -> serde_json::Value {
    pre("Bash", json!({ "command": "make", "description": "Build" }), id, cwd)
}

fn changed(ft: &FileTreeState, tab: &str) -> Vec<String> {
    ft.agents
        .activity(tab, "session", false)
        .map(|a| a.entries.into_iter().filter(|e| e.evidence == "fs_during_tool").map(|e| e.path).collect())
        .unwrap_or_default()
}

fn one_shell_call_is_credited_two_are_not_and_ignored_paths_never(ft: &Arc<FileTreeState>, root: &Path, cwd: &str) {
    let cwd = cwd.to_string();
    // One tab's Bash running: a new file and a write under an ignored folder.
    ft.agents.record(rec_for(&prompt(&cwd), "t1"));
    ft.agents.record(rec_for(&bash("b1", &cwd), "t1"));
    std::fs::write(root.join("out.txt"), "x").unwrap();
    std::fs::write(root.join("build/obj.o"), "x").unwrap();
    let want = paths::norm(&root.join("out.txt").to_string_lossy()).into_owned();
    wait_for("the attribution", Duration::from_secs(5), || changed(&ft, "t1").contains(&want));
    std::thread::sleep(Duration::from_millis(500));
    let got = changed(&ft, "t1");
    assert!(!got.iter().any(|p| p.contains("build")), "an ignored path is never attributed: {got:?}");
    assert_eq!(ft.agents.activity("t1", "session", false).unwrap().entries.iter().find(|e| e.path == want).unwrap().op, "changed");
    ft.agents.record(rec_for(&post("Bash", json!({ "command": "make" }), "b1", &cwd), "t1"));

    // Two tabs' shell calls at once: nobody is credited.
    std::thread::sleep(Duration::from_millis(500));
    ft.agents.record(rec_for(&prompt(&cwd), "t2"));
    ft.agents.record(rec_for(&bash("b2", &cwd), "t1"));
    ft.agents.record(rec_for(&bash("b3", &cwd), "t2"));
    std::fs::write(root.join("both.txt"), "x").unwrap();
    std::thread::sleep(Duration::from_millis(1200));
    let both = paths::norm(&root.join("both.txt").to_string_lossy()).into_owned();
    assert!(!changed(ft, "t1").contains(&both) && !changed(ft, "t2").contains(&both), "two shells: unattributed");
    ft.agents.record(rec_for(&post("Bash", json!({ "command": "make" }), "b2", &cwd), "t1"));
    ft.agents.record(rec_for(&post("Bash", json!({ "command": "make" }), "b3", &cwd), "t2"));
    ft.agents.record(rec_for(&stop(&cwd), "t1"));
    ft.agents.record(rec_for(&stop(&cwd), "t2"));
}

fn git(dir: &Path, args: &[&str]) {
    let o = Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
}

fn a_commit_during_the_sole_open_turn_is_credited_to_it(ft: &Arc<FileTreeState>, rec: &Recorder, root: &Path, cwd: &str) {
    let cwd = cwd.to_string();
    wait_for("the first status run", Duration::from_secs(10), || rec.msgs.lock().iter().any(|m| matches!(m.ev, TreeEvent::Git(_))));
    ft.agents.record(rec_for(&prompt(&cwd), "t1"));
    std::fs::write(root.join("b.txt"), "b\n").unwrap();
    git(root, &["add", "b.txt"]);
    git(root, &["commit", "-q", "--no-verify", "-m", "two"]);
    let want = paths::norm(&root.join("b.txt").to_string_lossy()).into_owned();
    let commits = || ft.agents.activity("t1", "turn", false).unwrap().entries.into_iter().filter(|e| e.evidence == "commit").map(|e| e.path).collect::<Vec<_>>();
    wait_for("the commit credit", Duration::from_secs(10), || commits().contains(&want));
    assert_eq!(commits(), vec![want], "only what the commit changed");
    wait_for("the commit mark", Duration::from_secs(2), || rec.marks().iter().any(|m| m.rel == "b.txt" && m.op == "commit"));

    // With two turns open nobody is credited.
    ft.agents.record(rec_for(&prompt(&cwd), "t2"));
    std::fs::write(root.join("c.txt"), "c\n").unwrap();
    git(root, &["add", "c.txt"]);
    git(root, &["commit", "-q", "--no-verify", "-m", "three"]);
    std::thread::sleep(Duration::from_millis(1500));
    let c = paths::norm(&root.join("c.txt").to_string_lossy()).into_owned();
    assert!(!commits().contains(&c));
}

/// The hook reads a baseline BEFORE it records the Pre, in the turn `turn_for_pre` predicts;
/// that has to be the turn `record` then files the call under, or the baseline would sit in a
/// turn no lookup asks for.
#[test]
fn turn_for_pre_predicts_the_turn_record_files_the_call_in() {
    let l = ledger();
    let cwd = repo();
    let edit = |id: &str| pre("Edit", json!({ "file_path": at("a.ts"), "old_string": "a", "new_string": "b" }), id, &cwd);
    let check = |l: &AgentFiles, event: serde_json::Value, id: &str| {
        let predicted = l.turn_for_pre("t1", event["session_id"].as_str().unwrap());
        l.record(rec(&event, "t1"));
        let filed = l.activity("t1", "session", true).unwrap().entries.into_iter().find(|e| e.tool_use_id.as_deref() == Some(id)).unwrap().turn;
        assert_eq!(predicted, filed, "{id}");
    };
    // No prompt seen: the call opens turn 1.
    check(&l, edit("e1"), "e1");
    l.record(rec(&stop(&cwd), "t1"));
    l.record(rec(&prompt(&cwd), "t1"));
    check(&l, edit("e2"), "e2");
    // After the turn's Stop, a background call carries the same turn on.
    l.record(rec(&stop(&cwd), "t1"));
    check(&l, edit("e3"), "e3");
    // A new session numbers afresh.
    let mut fresh = edit("e4");
    fresh["session_id"] = json!("s2");
    check(&l, fresh, "e4");
    assert_eq!(l.turn_and_session("t1"), Some((1, "s2".to_string())));
    // That was a headless `claude -p` the agent ran (it inherits the tab's id). The parent's next
    // call carries its own turn on rather than starting a turn 1 its baselines already have.
    check(&l, edit("e5"), "e5");
    assert_eq!(l.turn_and_session("t1"), Some((2, "s1".to_string())));
    l.record(rec(&prompt(&cwd), "t1"));
    assert_eq!(l.turn_of("t1"), Some(3));
}

/// A prompt landing between `turn_for_pre` and `record` (a background subagent's call racing the
/// human's next prompt) moves the call to the next turn; `record` says so, and the hook moves the
/// baseline it read to match.
#[tokio::test]
async fn record_answers_the_turn_a_call_was_really_filed_in() {
    let l = ledger();
    let d = std::env::temp_dir().join(format!("af-race-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    let cwd = d.to_string_lossy().into_owned();
    let file = d.join("a.ts").to_string_lossy().into_owned();
    std::fs::write(&file, "before\n").unwrap();
    l.record(rec(&prompt(&cwd), "t1"));
    let call = pre("Edit", json!({ "file_path": file, "old_string": "b", "new_string": "c" }), "e1", &cwd);
    let predicted = l.turn_for_pre("t1", "s1");
    l.baselines.capture("t1", "s1", "claude", predicted, vec![file.clone()]).await;
    l.record(rec(&prompt(&cwd), "t1"));
    let filed = l.record(rec(&call, "t1")).unwrap();
    assert_eq!((predicted, filed), (1, 2));
    l.baselines.rekey("t1", "s1", predicted, filed, std::slice::from_ref(&file));
    assert_eq!(l.baselines.lookup("t1", "s1", &file, "turn", 2).content.as_deref(), Some("before\n"));
    // The second copy of the same Pre is dropped and says so.
    assert_eq!(l.record(rec(&call, "t1")), None);
    let _ = std::fs::remove_dir_all(&d);
}

/// The hook's own order for a Pre: predict the turn, capture, record, move the baseline.
async fn hook_pre(l: &AgentFiles, event: &serde_json::Value, file: &str) {
    let session = event["session_id"].as_str().unwrap();
    let turn = l.turn_for_pre("t1", session);
    let seen = l.baselines.capture("t1", session, "claude", turn, vec![file.to_string()]).await;
    if let Some(filed) = l.record(rec(event, "t1")) {
        let paths: Vec<String> = seen.into_iter().map(|(p, _)| p).collect();
        l.baselines.rekey("t1", session, turn, filed, &paths);
    }
}

/// A parent turn's Bash runs headless `claude -p` children, which inherit the tab's id. However
/// many there are, and whether they write or only read, the parent's edits of a file it already
/// changed this turn never become the turn's start: either the real start survives, or the
/// lookup says it is gone.
#[tokio::test]
async fn headless_children_never_make_the_parents_edits_its_baseline() {
    let d = std::env::temp_dir().join(format!("af-children-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    let cwd = d.to_string_lossy().into_owned();
    let (a, x) = (d.join("a.ts").to_string_lossy().into_owned(), d.join("x.ts").to_string_lossy().into_owned());
    std::fs::write(&x, "x\n").unwrap();
    let edit = |file: &str, id: &str, session: &str| {
        let mut e = pre("Edit", json!({ "file_path": file, "old_string": "v", "new_string": "w" }), id, &cwd);
        e["session_id"] = json!(session);
        e
    };
    let child_turn = |session: &str| {
        let mut p = prompt(&cwd);
        p["session_id"] = json!(session);
        let mut s = stop(&cwd);
        s["session_id"] = json!(session);
        (p, s)
    };
    // Writers: `claude -p 'fix lint' && claude -p 'add tests'`.
    let l = ledger();
    std::fs::write(&a, "v0\n").unwrap();
    l.record(rec(&prompt(&cwd), "t1"));
    hook_pre(&l, &edit(&a, "p1", "s1"), &a).await;
    std::fs::write(&a, "v1\n").unwrap();
    for c in ["c1", "c2"] {
        let (p, s) = child_turn(c);
        l.record(rec(&p, "t1"));
        hook_pre(&l, &edit(&x, &format!("{c}-e"), c), &x).await;
        l.record(rec(&s, "t1"));
    }
    hook_pre(&l, &edit(&a, "p2", "s1"), &a).await;
    assert_eq!(l.turn_and_session("t1"), Some((1, "s1".to_string())));
    for scope in ["turn", "session"] {
        assert_eq!(l.baselines.lookup("t1", "s1", &a, scope, 1).content.as_deref(), Some("v0\n"), "{scope}");
    }
    // As many writers as the ledger remembers sessions for: with `left` full, the parent coming
    // back must not be the one pushed out to make room for the child it leaves.
    let l = ledger();
    std::fs::write(&a, "v0\n").unwrap();
    for _ in 0..3 {
        l.record(rec(&prompt(&cwd), "t1"));
    }
    hook_pre(&l, &edit(&a, "w1", "s1"), &a).await;
    std::fs::write(&a, "v1\n").unwrap();
    for i in 0..LEFT_SESSIONS {
        let c = format!("w{i}");
        let (p, s) = child_turn(&c);
        l.record(rec(&p, "t1"));
        hook_pre(&l, &edit(&x, &format!("{c}-e"), &c), &x).await;
        l.record(rec(&s, "t1"));
    }
    hook_pre(&l, &edit(&a, "w2", "s1"), &a).await;
    assert_eq!(l.turn_and_session("t1"), Some((3, "s1".to_string())));
    for scope in ["turn", "session"] {
        assert_eq!(l.baselines.lookup("t1", "s1", &a, scope, 3).content.as_deref(), Some("v0\n"), "{scope}");
    }
    // Readers, more than the ledger keeps turns for: the parent comes back numbering from 1.
    let l = ledger();
    std::fs::write(&a, "v0\n").unwrap();
    l.record(rec(&prompt(&cwd), "t1"));
    hook_pre(&l, &edit(&a, "q1", "s1"), &a).await;
    l.record(rec(&stop(&cwd), "t1"));
    std::fs::write(&a, "v5\n").unwrap();
    for i in 2..=7 {
        l.record(rec(&prompt(&cwd), "t1"));
        l.record(rec(&stop(&cwd), "t1"));
        assert_eq!(l.turn_of("t1"), Some(i));
    }
    l.record(rec(&prompt(&cwd), "t1"));
    for i in 0..=LEFT_SESSIONS {
        let (p, s) = child_turn(&format!("r{i}"));
        l.record(rec(&p, "t1"));
        l.record(rec(&s, "t1"));
    }
    hook_pre(&l, &edit(&a, "q2", "s1"), &a).await;
    assert_eq!(l.turn_and_session("t1"), Some((1, "s1".to_string())));
    for scope in ["turn", "session"] {
        let got = l.baselines.lookup("t1", "s1", &a, scope, 1);
        assert_eq!((got.content, got.unavailable.as_deref()), (None, Some("too_many_sessions")), "{scope}");
    }
    // Its next turn opens with a prompt after it came back: a clean turn, read as usual.
    l.record(rec(&stop(&cwd), "t1"));
    l.record(rec(&prompt(&cwd), "t1"));
    hook_pre(&l, &edit(&a, "q3", "s1"), &a).await;
    assert_eq!(l.baselines.lookup("t1", "s1", &a, "turn", 2).content.as_deref(), Some("v5\n"));
    let _ = std::fs::remove_dir_all(&d);
}

/// A session the ledger forgot comes back with a prompt, reads for a turn or two, and only then
/// edits: none of those turns began before it came back, so the edit's turn is read as usual and
/// the read-only turns are "not captured", not lost. Only the session's start stays gone.
#[tokio::test]
async fn a_forgotten_session_back_with_a_prompt_starts_clean() {
    let d = std::env::temp_dir().join(format!("af-back-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&d).unwrap();
    let cwd = d.to_string_lossy().into_owned();
    let a = d.join("a.ts").to_string_lossy().into_owned();
    std::fs::write(&a, "v0\n").unwrap();
    let l = ledger();
    l.record(rec(&prompt(&cwd), "t1"));
    hook_pre(&l, &pre("Edit", json!({ "file_path": a, "old_string": "v", "new_string": "w" }), "e1", &cwd), &a).await;
    l.record(rec(&stop(&cwd), "t1"));
    std::fs::write(&a, "v1\n").unwrap();
    for i in 0..=LEFT_SESSIONS {
        let mut p = prompt(&cwd);
        p["session_id"] = json!(format!("r{i}"));
        l.record(rec(&p, "t1"));
    }
    for turn in 1..=2 {
        l.record(rec(&prompt(&cwd), "t1"));
        assert_eq!(l.turn_and_session("t1"), Some((turn, "s1".to_string())));
        assert_eq!(l.baselines.lookup("t1", "s1", &a, "turn", turn).unavailable.as_deref(), Some("not_captured"));
        l.record(rec(&stop(&cwd), "t1"));
    }
    l.record(rec(&prompt(&cwd), "t1"));
    hook_pre(&l, &pre("Edit", json!({ "file_path": a, "old_string": "v", "new_string": "w" }), "e2", &cwd), &a).await;
    assert_eq!(l.baselines.lookup("t1", "s1", &a, "turn", 3).content.as_deref(), Some("v1\n"));
    assert_eq!(l.baselines.lookup("t1", "s1", &a, "session", 3).unavailable.as_deref(), Some("too_many_sessions"));
    let _ = std::fs::remove_dir_all(&d);
}
