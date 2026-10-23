//! Git decorations and find, end to end through a hub (docs/file-tree.md §20 phase 2).
//!
//! These run real git against temp repositories (with the user's global config switched off
//! for the test's own commits, so no hook or signing setting interferes), or a stub `git`
//! script where the test needs to control how long a run takes.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use super::git::{GitConfig, GitCore, GitTiming, Trigger};
use super::hub::Sink;
use super::types::{Envelope, GitUpdate, TreeEvent};
use super::{FileTreeState, Slots};

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
    fn git_updates(&self) -> Vec<GitUpdate> {
        self.msgs.lock().iter().filter_map(|m| if let TreeEvent::Git(g) = &m.ev { Some(g.clone()) } else { None }).collect()
    }

    fn healths(&self) -> Vec<String> {
        self.msgs.lock().iter().filter_map(|m| if let TreeEvent::Health { git, .. } = &m.ev { Some(git.clone()) } else { None }).collect()
    }
}

fn temp_root(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("ft-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&t).unwrap();
    crate::share::canon(&t)
}

/// git for the test's own setup: no global or system config, an identity, no hooks.
fn git(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new("git")
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap()
}

fn repo(tag: &str) -> PathBuf {
    let r = temp_root(tag);
    git(&r, &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(r.join("src/lib")).unwrap();
    std::fs::write(r.join("src/lib/a.ts"), "a\n").unwrap();
    std::fs::write(r.join("README.md"), "r\n").unwrap();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "--no-verify", "-m", "init"]);
    r
}

fn wait_for(what: &str, timeout: Duration, mut f: impl FnMut() -> bool) {
    let start = Instant::now();
    while !f() {
        assert!(start.elapsed() < timeout, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The decorations a client holds after applying every update in order.
fn applied(updates: &[GitUpdate]) -> (std::collections::BTreeMap<String, String>, std::collections::BTreeMap<String, String>) {
    let (mut files, mut dirs) = (std::collections::BTreeMap::new(), std::collections::BTreeMap::new());
    for u in updates {
        if u.full {
            files.clear();
            dirs.clear();
        }
        for k in &u.del {
            files.remove(k);
            dirs.remove(k);
        }
        files.extend(u.files.iter().cloned());
        dirs.extend(u.dirs.iter().cloned());
    }
    (files, dirs)
}

/// The watcher starts in the background (FSEvents can take seconds); a change made before it
/// runs is never reported.
fn wait_for_watcher(ft: &FileTreeState, sub: u64) {
    let hub = ft.hub_of(sub).unwrap();
    // Long: with the whole suite making temp folders, fseventsd can keep a stream start
    // waiting for tens of seconds.
    wait_for("the watcher to start", Duration::from_secs(60), || !hub.watch_pending());
    // FSEvents reports from the stream's start; give it a moment past that.
    std::thread::sleep(Duration::from_millis(200));
}

fn fast_timing() -> GitTiming {
    GitTiming { min_gap: Duration::from_millis(20), index_delay: Duration::from_millis(20), worktree_delay: Duration::from_millis(40), ..GitTiming::default() }
}

#[test]
fn decorations_follow_the_disk_and_a_commit_clears_them() {
    let r = repo("git-e2e");
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { timing: fast_timing(), ..GitConfig::default() });
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &r.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    assert!(ack.caps.git);
    wait_for("the first status run", Duration::from_secs(10), || !rec.git_updates().is_empty());
    let (files, _) = applied(&rec.git_updates());
    assert!(files.is_empty(), "a clean repo has no decorations: {files:?}");
    assert_eq!(rec.git_updates()[0].branch.as_ref().map(|b| b.head.as_str()), Some("main"));
    wait_for_watcher(&ft, ack.sub_id);

    // An edit and a new file: the watcher schedules a run (no manual refresh here).
    std::fs::write(r.join("src/lib/a.ts"), "changed\n").unwrap();
    std::fs::write(r.join("src/new.ts"), "n\n").unwrap();
    wait_for("the edit to decorate", Duration::from_secs(15), || applied(&rec.git_updates()).0.len() == 2);
    let (files, dirs) = applied(&rec.git_updates());
    assert_eq!(files.get("src/lib/a.ts").map(String::as_str), Some("M"));
    assert_eq!(files.get("src/new.ts").map(String::as_str), Some("?"));
    assert_eq!(dirs.get("src").map(String::as_str), Some("M"));
    assert_eq!(dirs.get("src/lib").map(String::as_str), Some("M"));
    let last = rec.git_updates().last().cloned().unwrap();
    assert!(!last.full, "a change after the first is sent as changed keys only");

    // A commit made outside the tree (as in a terminal) moves the index and HEAD.
    let n = rec.git_updates().len();
    git(&r, &["add", "-A"]);
    git(&r, &["commit", "-q", "--no-verify", "-m", "two"]);
    wait_for("the commit to clear the decorations", Duration::from_secs(15), || rec.git_updates().len() > n && applied(&rec.git_updates()).0.is_empty());
    let (files, dirs) = applied(&rec.git_updates());
    assert!(files.is_empty() && dirs.is_empty(), "{files:?} {dirs:?}");

    // Hover line counts for a modified file.
    std::fs::write(r.join("README.md"), "r\nmore\nlines\n").unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    hub.git_signal(Trigger::Manual);
    wait_for("README to decorate", Duration::from_secs(10), || applied(&rec.git_updates()).0.contains_key("README.md"));
    assert_eq!(hub.stat("README.md").unwrap().numstat, Some((2, 0)));
    assert_eq!(hub.stat("src/lib/a.ts").unwrap().numstat, None, "a clean file has no counts");

    // Compare with HEAD: the committed content; a new file has none.
    assert_eq!(hub.diff_base("README.md", "HEAD").unwrap().as_deref(), Some("r\n"));
    std::fs::write(r.join("fresh.txt"), "x").unwrap();
    assert_eq!(hub.diff_base("fresh.txt", "HEAD").unwrap(), None);
    assert!(hub.diff_base("../etc/passwd", "HEAD").is_err());
    assert!(hub.diff_base("README.md", "--output=/tmp/x").is_err());

    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_late_subscriber_gets_everything_and_a_paused_one_catches_up_whole() {
    let r = repo("git-subs");
    std::fs::write(r.join("README.md"), "changed\n").unwrap();
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { timing: fast_timing(), ..GitConfig::default() });
    let a = Arc::new(Recorder::default());
    let ack_a = ft.open("w1", &r.to_string_lossy(), vec![], a.clone(), vec![]).unwrap();
    wait_for("the first run", Duration::from_secs(10), || !a.git_updates().is_empty());
    let hub = ft.hub_of(ack_a.sub_id).unwrap();

    let b = Arc::new(Recorder::default());
    let ack_b = ft.open("w2", &r.to_string_lossy(), vec![], b.clone(), vec![]).unwrap();
    let first_b = b.git_updates();
    assert!(first_b.first().is_some_and(|u| u.full), "a new subscriber starts from everything");
    assert_eq!(applied(&first_b).0.get("README.md").map(String::as_str), Some("M"));

    // B pauses; a change happens; B resumes and gets one full update, A only the change.
    hub.pause(ack_b.sub_id, true);
    let (na, nb) = (a.git_updates().len(), b.git_updates().len());
    std::fs::write(r.join("other.txt"), "o").unwrap();
    hub.git_signal(Trigger::Manual);
    wait_for("A to see the change", Duration::from_secs(10), || a.git_updates().len() > na);
    assert_eq!(b.git_updates().len(), nb, "nothing reaches a paused subscriber");
    assert!(!a.git_updates().last().unwrap().full);
    hub.pause(ack_b.sub_id, false);
    let after = b.git_updates();
    assert_eq!(after.len(), nb + 1);
    assert!(after.last().unwrap().full);
    assert_eq!(applied(&after), applied(&a.git_updates()), "both views converge");

    ft.close(ack_a.sub_id);
    ft.close(ack_b.sub_id);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn turning_decorations_off_clears_them_and_runs_nothing() {
    let r = repo("git-off");
    std::fs::write(r.join("README.md"), "changed\n").unwrap();
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { timing: fast_timing(), ..GitConfig::default() });
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &r.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    wait_for("the first run", Duration::from_secs(10), || !applied(&rec.git_updates()).0.is_empty());
    ft.set_git_enabled(false);
    assert!(applied(&rec.git_updates()).0.is_empty(), "off clears what the view shows");
    assert_eq!(rec.healths().last().map(String::as_str), Some("off"));
    let n = rec.git_updates().len();
    ft.hub_of(ack.sub_id).unwrap().git_signal(Trigger::Manual);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(rec.git_updates().len(), n);
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn a_command_finishing_in_the_repo_schedules_a_run() {
    let r = repo("git-cmd");
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { timing: fast_timing(), ..GitConfig::default() });
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &r.join("src").to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    wait_for("the first run", Duration::from_secs(10), || !rec.git_updates().is_empty());
    let hub = ft.hub_of(ack.sub_id).unwrap();
    // A root inside the repo: a command at the repo's top still counts, one elsewhere doesn't.
    assert!(hub.covers(&r));
    assert!(hub.covers(&r.join("src/lib")));
    assert!(!hub.covers(&std::env::temp_dir().join("elsewhere")));
    // Written with the watcher's trailing delay out of the picture: the signal is what runs it.
    std::fs::write(r.join("src/lib/a.ts"), "changed\n").unwrap();
    ft.note_command_in(&r);
    wait_for("the command to decorate", Duration::from_secs(10), || applied(&rec.git_updates()).0.get("lib/a.ts").is_some());
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&r);
}

/// A `git` that records each run and takes as long as a control file says.
#[cfg(unix)]
fn stub_git(dir: &Path) -> (PathBuf, PathBuf, PathBuf) {
    use std::os::unix::fs::PermissionsExt;
    let log = dir.join("runs.log");
    let delay = dir.join("delay");
    std::fs::write(&delay, "0").unwrap();
    let script = dir.join("git");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho run >> '{}'\nsleep \"$(cat '{}')\"\nprintf '# branch.oid abc\\000# branch.head main\\000'\n",
            log.display(),
            delay.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    (script, log, delay)
}

#[cfg(unix)]
fn runs(log: &Path) -> usize {
    std::fs::read_to_string(log).map(|s| s.lines().count()).unwrap_or(0)
}

#[cfg(unix)]
#[test]
fn slow_mode_trips_on_two_slow_runs_skips_worktree_edges_and_recovers() {
    let r = repo("git-slow");
    let bin = temp_root("git-stub");
    let (script, log, delay) = stub_git(&bin);
    let timing = GitTiming { slow_run: Duration::from_millis(250), fast_run: Duration::from_millis(150), ..fast_timing() };
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { program: script.into_os_string(), enabled: true, timing });
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &r.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    wait_for("the open's run", Duration::from_secs(10), || !rec.git_updates().is_empty());

    std::fs::write(&delay, "0.4").unwrap();
    for _ in 0..2 {
        let n = runs(&log);
        hub.git_signal(Trigger::Manual);
        wait_for("a slow run", Duration::from_secs(10), || runs(&log) > n);
        std::thread::sleep(Duration::from_millis(500));
    }
    wait_for("Health to say slow", Duration::from_secs(5), || rec.healths().last().map(String::as_str) == Some("slow"));

    // Slow: worktree changes and finished commands no longer run git.
    let n = runs(&log);
    hub.git_signal(Trigger::Worktree);
    hub.git_signal(Trigger::Command);
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(runs(&log), n, "a slow repo ignores worktree edges");

    // A fast run (on a manual refresh) ends it.
    std::fs::write(&delay, "0").unwrap();
    hub.git_signal(Trigger::Manual);
    wait_for("Health to recover", Duration::from_secs(10), || rec.healths().last().map(String::as_str) == Some("live"));

    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&r);
    let _ = std::fs::remove_dir_all(&bin);
}

#[cfg(unix)]
#[test]
fn signals_during_a_run_collapse_into_one_rerun() {
    let r = repo("git-collapse");
    let bin = temp_root("git-stub2");
    let (script, log, delay) = stub_git(&bin);
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { program: script.into_os_string(), enabled: true, timing: fast_timing() });
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &r.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    wait_for("the open's run", Duration::from_secs(10), || runs(&log) >= 1);
    std::thread::sleep(Duration::from_millis(100));
    std::fs::write(&delay, "0.3").unwrap();
    let n = runs(&log);
    hub.git_signal(Trigger::Manual);
    wait_for("the run to start", Duration::from_secs(5), || runs(&log) > n);
    for _ in 0..20 {
        hub.git_signal(Trigger::Manual);
        hub.git_signal(Trigger::Index);
    }
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(runs(&log), n + 2, "twenty signals during one run make exactly one more run");
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&r);
    let _ = std::fs::remove_dir_all(&bin);
}

/// The reason for `--no-optional-locks`: an agent committing in the same repo must never meet
/// our `index.lock`, however hard the tree refreshes.
#[test]
fn a_refresh_storm_never_takes_index_lock_from_concurrent_commits() {
    let r = repo("git-lock");
    let core = GitCore::new(std::sync::Weak::new(), &GitConfig::default(), &r, &r, false, Arc::new(Slots::new(2)));
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let storm = {
        let (core, stop) = (core.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut n = 0;
            while !stop.load(std::sync::atomic::Ordering::SeqCst) {
                core.run_status().expect("status runs");
                n += 1;
            }
            n
        })
    };
    for i in 0..25 {
        std::fs::write(r.join(format!("f{i}.txt")), format!("{i}\n")).unwrap();
        std::fs::write(r.join("README.md"), format!("rev {i}\n")).unwrap();
        let add = git(&r, &["add", "-A"]);
        assert!(add.status.success(), "add {i}: {}", String::from_utf8_lossy(&add.stderr));
        let commit = git(&r, &["commit", "-q", "--no-verify", "-m", &format!("c{i}")]);
        let err = String::from_utf8_lossy(&commit.stderr);
        assert!(commit.status.success() && !err.contains("index.lock"), "commit {i}: {err}");
    }
    stop.store(true, std::sync::atomic::Ordering::SeqCst);
    let n = storm.join().unwrap();
    assert!(n > 5, "the storm ran ({n} status runs)");
    let _ = std::fs::remove_dir_all(&r);
}

#[test]
fn find_builds_its_index_on_the_first_query_and_follows_the_disk() {
    let r = repo("find");
    std::fs::write(r.join(".gitignore"), "dist/\n").unwrap();
    std::fs::create_dir_all(r.join("dist")).unwrap();
    std::fs::write(r.join("dist/bundle.js"), "").unwrap();
    std::fs::write(r.join("src/untracked_helper.ts"), "").unwrap();
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    ft.set_git_config(GitConfig { timing: fast_timing(), ..GitConfig::default() });
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &r.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();

    let first = hub.search("helper", 500, 1, false);
    assert!(first.indexing, "the first query starts the build");
    wait_for("search_ready", Duration::from_secs(10), || rec.msgs.lock().iter().any(|m| m.ev == TreeEvent::SearchReady));
    let hits = hub.search("helper", 500, 2, false);
    assert!(!hits.indexing);
    assert_eq!(hits.items.first().map(|h| h.rel.as_str()), Some("src/untracked_helper.ts"), "untracked files are indexed");
    assert!(hub.search("bundle", 500, 3, false).items.is_empty(), "ignored files are not");
    wait_for_watcher(&ft, ack.sub_id);

    // A new file is found once the watcher reports it; a deleted one disappears.
    std::fs::write(r.join("src/lib/brand_new_widget.ts"), "").unwrap();
    let mut seq = 10;
    wait_for("the new file in the index", Duration::from_secs(15), || {
        seq += 1;
        hub.search("brand_new_widget", 500, seq, false).items.iter().any(|h| h.rel == "src/lib/brand_new_widget.ts")
    });
    std::fs::remove_file(r.join("src/untracked_helper.ts")).unwrap();
    wait_for("the deleted file gone from the index", Duration::from_secs(15), || {
        seq += 1;
        !hub.search("untracked_helper", 500, seq, false).items.iter().any(|h| h.rel == "src/untracked_helper.ts")
    });
    assert!(hub.search.len().is_some());
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&r);
}
