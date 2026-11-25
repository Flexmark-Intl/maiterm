//! Files, end to end against a real disk and the real watcher (docs/file-tree.md §20 phase 1).
//!
//! The convergence test is the one that matters: a client that follows the protocol (apply a
//! Delta only on a matching generation, refresh otherwise) must end up holding exactly what a
//! fresh listing says, whatever the disk did meanwhile — with the raw channel overflowing, with
//! the subscription paused and resumed, and with acks withheld until it stalls.
//!
//! Budget and idle tests are `#[ignore]`: they build large fixtures or wait 10 s. Run them with
//! `cargo test --lib filetree::tests -- --ignored` (add `--release` for the real budgets).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use super::hub::{RootHub, Sink, STALL_MESSAGES};
use super::ignore_cache::IgnoreStack;
use super::list;
use super::test_support::{hook, prompt_event, tool_event, Probe, HANG};
use super::types::{DirReq, Entry, Envelope, TreeEvent};
use super::FileTreeState;

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

fn temp_root(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("ft-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&t).unwrap();
    crate::share::canon(&t)
}

/// Gives each path an mtime long past, as a folder last changed before the tree opened has. A
/// listing trusts only an mtime at least 2 s old (`hub::trusted_mtime`), so a fixture made a
/// moment ago would be re-listed on every edge. Each call's time is new, so an aged folder's
/// mtime still differs from any it had before.
fn age(paths: &[&Path]) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    for p in paths {
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let t = std::time::UNIX_EPOCH + Duration::from_secs(1_600_000_000 + n);
        #[cfg(unix)]
        let f = std::fs::File::open(p);
        #[cfg(windows)]
        let f = {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_WRITE_ATTRIBUTES; FILE_FLAG_BACKUP_SEMANTICS opens a folder.
            std::fs::OpenOptions::new().access_mode(0x100).custom_flags(0x0200_0000).open(p)
        };
        f.and_then(|f| f.set_modified(t)).unwrap_or_else(|e| panic!("age {}: {e}", p.display()));
    }
}

/// A client that follows the wire protocol, as the frontend's TreeModel will.
#[derive(Default)]
struct Client {
    /// In the order the wire gave: the client never sorts (§12.1), so a wrong `Delta.at`
    /// shows up as a folder out of order.
    dirs: HashMap<String, (u64, Vec<Entry>)>,
    building: HashMap<String, (u64, u32, Vec<Entry>)>,
    expanded: BTreeSet<String>,
    need_refresh: BTreeSet<String>,
    gone: Vec<String>,
    last_seq: u64,
    unacked: u64,
}

impl Client {
    fn apply(&mut self, env: Envelope) {
        assert_eq!(env.seq, self.last_seq + 1, "messages arrive in order with no gaps");
        self.last_seq = env.seq;
        self.unacked += 1;
        match env.ev {
            TreeEvent::Dir { rel, gen, chunk, last, entries, .. } => {
                if !self.expanded.contains(&rel) {
                    return;
                }
                if chunk == 0 {
                    self.building.insert(rel.clone(), (gen, 1, entries));
                } else {
                    match self.building.get_mut(&rel) {
                        Some(b) if b.0 == gen && b.1 == chunk => {
                            b.1 += 1;
                            b.2.extend(entries);
                        }
                        _ => {
                            self.building.remove(&rel);
                            self.need_refresh.insert(rel);
                            return;
                        }
                    }
                }
                if last {
                    let (g, _, es) = self.building.remove(&rel).unwrap();
                    self.dirs.insert(rel, (g, es));
                }
            }
            TreeEvent::NotModified { rel, gen } => {
                if self.expanded.contains(&rel) && self.dirs.get(&rel).map(|d| d.0) != Some(gen) {
                    self.need_refresh.insert(rel);
                }
            }
            TreeEvent::Delta { rel, from, gen, add, del, chg, at } => match self.dirs.get_mut(&rel) {
                Some(d) if d.0 == from => {
                    assert_eq!(at.len(), add.len() + chg.len(), "one position per added or changed entry");
                    let gone: BTreeSet<&str> = del.iter().map(|s| s.as_str()).chain(chg.iter().map(|e| e.n.as_str())).collect();
                    d.1.retain(|e| !gone.contains(e.n.as_str()));
                    let mut place: Vec<(u32, Entry)> = at.into_iter().zip(add.into_iter().chain(chg)).collect();
                    place.sort_by_key(|p| p.0);
                    for (i, e) in place {
                        d.1.insert((i as usize).min(d.1.len()), e);
                    }
                    d.0 = gen;
                }
                _ => {
                    if self.expanded.contains(&rel) {
                        self.need_refresh.insert(rel);
                    }
                }
            },
            TreeEvent::DirError { rel, code, .. } => {
                if code == "gone" {
                    self.dirs.remove(&rel);
                    self.building.remove(&rel);
                    if self.expanded.remove(&rel) {
                        self.gone.push(rel);
                    }
                }
            }
            TreeEvent::Health { .. } | TreeEvent::RootGone | TreeEvent::Reopen | TreeEvent::Git(_) | TreeEvent::SearchReady | TreeEvent::Agent { .. } => {}
        }
    }

    /// Take what arrived, apply it, and do what the protocol says: refresh, collapse, ack.
    fn pump(&mut self, rec: &Recorder, hub: &RootHub, sub: u64, ack: bool) -> usize {
        let msgs = std::mem::take(&mut *rec.msgs.lock());
        let n = msgs.len();
        for m in msgs {
            self.apply(m);
        }
        for rel in std::mem::take(&mut self.need_refresh) {
            hub.refresh(sub, Some(rel));
        }
        let gone = std::mem::take(&mut self.gone);
        if !gone.is_empty() {
            hub.collapse(sub, gone);
        }
        if ack && self.unacked > 0 {
            hub.ack(sub, self.last_seq);
            self.unacked = 0;
        }
        n
    }
}

fn all_dirs(root: &Path) -> Vec<String> {
    fn walk(root: &Path, rel: &str, out: &mut Vec<String>) {
        let abs = if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
        let Ok(rd) = std::fs::read_dir(&abs) else { return };
        for e in rd.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                let r = super::paths::join_rel(rel, &e.file_name().to_string_lossy());
                out.push(r.clone());
                walk(root, &r, out);
            }
        }
    }
    let mut out = vec![String::new()];
    walk(root, "", &mut out);
    out
}

fn all_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for d in all_dirs(root) {
        let abs = if d.is_empty() { root.to_path_buf() } else { root.join(&d) };
        for e in std::fs::read_dir(&abs).into_iter().flatten().flatten() {
            if e.file_type().map(|t| t.is_file()).unwrap_or(false) {
                out.push(super::paths::join_rel(&d, &e.file_name().to_string_lossy()));
            }
        }
    }
    out
}

/// One random change: create, delete, rename or move a file or a folder. Returns the paths a
/// watcher reports for it: the entry, both sides of a rename, a removed folder itself.
fn mutate(root: &Path, rng: &mut StdRng, n: &mut u64) -> Vec<PathBuf> {
    *n += 1;
    let dirs = all_dirs(root);
    let files = all_files(root);
    let pick_dir = |rng: &mut StdRng| dirs[rng.gen_range(0..dirs.len())].clone();
    let at = |rel: &str| if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    match rng.gen_range(0..10) {
        0..=3 => {
            let p = at(&pick_dir(rng)).join(format!("f{n}.txt"));
            let _ = std::fs::write(&p, "x");
            vec![p]
        }
        4 => {
            let p = at(&pick_dir(rng)).join(format!("d{n}"));
            let _ = std::fs::create_dir(&p);
            vec![p]
        }
        5 | 6 if !files.is_empty() => {
            let p = at(&files[rng.gen_range(0..files.len())]);
            let _ = std::fs::remove_file(&p);
            vec![p]
        }
        7 if !files.is_empty() => {
            let from = at(&files[rng.gen_range(0..files.len())]);
            let to = at(&pick_dir(rng)).join(format!("m{n}.txt"));
            let _ = std::fs::rename(&from, &to);
            vec![from, to]
        }
        8 if dirs.len() > 1 => {
            let d = &dirs[rng.gen_range(1..dirs.len())];
            let (from, to) = (at(d), at(super::paths::split_parent(d).0).join(format!("r{n}")));
            let _ = std::fs::rename(&from, &to);
            vec![from, to]
        }
        9 if dirs.len() > 6 => {
            let p = at(&dirs[rng.gen_range(1..dirs.len())]);
            let _ = std::fs::remove_dir_all(&p);
            vec![p]
        }
        _ => {
            let p = at(&pick_dir(rng)).join(format!("g{n}.txt"));
            let _ = std::fs::write(&p, "y");
            vec![p]
        }
    }
}

fn seed_tree(root: &Path, dirs: usize, rng: &mut StdRng) {
    for i in 0..dirs {
        let depth = rng.gen_range(0..3);
        let mut p = root.to_path_buf();
        for d in 0..=depth {
            p.push(format!("s{}_{d}", i % 7));
        }
        let p = p.with_file_name(format!("dir{i}"));
        std::fs::create_dir_all(&p).unwrap();
        for j in 0..rng.gen_range(0..4) {
            std::fs::write(p.join(format!("file{j}.rs")), "").unwrap();
        }
    }
}

#[derive(Clone, Copy, Debug)]
enum Variant {
    Plain,
    /// A raw channel of 4: overflow on nearly every burst.
    Overflow,
    /// The subscription is paused and resumed at random.
    PauseResume,
    /// No acks until the end: the subscription stalls and must catch up on the first ack.
    WithheldAcks,
}

fn converge(variant: Variant, seed: u64) {
    let mut rng = StdRng::seed_from_u64(seed);
    let root = temp_root("converge");
    let extra = if matches!(variant, Variant::WithheldAcks) { 320 } else { 30 };
    seed_tree(&root, extra, &mut rng);
    let cap = if matches!(variant, Variant::Overflow) { 4 } else { super::watch::RAW_CAPACITY };
    let ft = FileTreeState::for_test(Duration::from_millis(200), cap);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let sub = ack.sub_id;
    let hub = ft.hub_of(sub).unwrap();
    let mut client = Client::default();
    client.expanded.insert(String::new());
    let acks = !matches!(variant, Variant::WithheldAcks);

    // Open a random half of the folders (all of them when the point is to exceed the window).
    let mut open: Vec<String> = all_dirs(&root).into_iter().filter(|d| !d.is_empty()).collect();
    if !matches!(variant, Variant::WithheldAcks) {
        open.retain(|_| rng.gen_bool(0.5));
    }
    client.expanded.extend(open.iter().cloned());
    hub.expand(sub, open.into_iter().map(|rel| DirReq { rel, known_gen: None }).collect());
    // From here the hub hears only what the test hands it: every change, as a watcher would
    // report it. What the OS reports is late, out of order or lost under load (docs/file-tree.md
    // §6.1), so it decides nothing here; this tests what the hub does with what it is told.
    probe.watching(&hub);
    probe.idle(&hub);
    hub.go_deaf();

    let mut n = 0u64;
    let mut paused = false;
    let mut max_seq_unacked = 0u64;
    for step in 0..400 {
        let changed = mutate(&root, &mut rng, &mut n);
        probe.push(&hub, &changed);
        if step % 10 == 0 {
            // The view moves too: open or close a folder.
            let dirs = all_dirs(&root);
            let d = dirs[rng.gen_range(0..dirs.len())].clone();
            if !d.is_empty() && client.expanded.contains(&d) && rng.gen_bool(0.3) {
                client.expanded.remove(&d);
                client.dirs.remove(&d);
                hub.collapse(sub, vec![d]);
            } else if !client.expanded.contains(&d) {
                client.expanded.insert(d.clone());
                hub.expand(sub, vec![DirReq { rel: d, known_gen: None }]);
            }
        }
        if matches!(variant, Variant::PauseResume) && step % 25 == 0 {
            paused = !paused;
            hub.pause(sub, paused);
        }
        client.pump(&rec, &hub, sub, acks);
        if !acks {
            max_seq_unacked = max_seq_unacked.max(client.last_seq);
        }
        if rng.gen_bool(0.2) {
            std::thread::sleep(Duration::from_millis(rng.gen_range(1..20)));
        }
    }
    if paused {
        hub.pause(sub, false);
    }
    if !acks {
        assert!(max_seq_unacked > STALL_MESSAGES, "the test must actually fill the ack window ({max_seq_unacked})");
        assert!(max_seq_unacked <= STALL_MESSAGES + 1, "a stalled subscription is sent nothing ({max_seq_unacked})");
    }

    // Quiesce: every change handed over has been processed (the channel and its batches are in
    // order) and listed and sent; then the client takes what was sent and asks for what it lacks
    // until the hub has nothing more to send. Nothing here waits on the clock.
    probe.deliver(&hub, &[]);
    let quiet = |client: &mut Client| {
        let mut rounds = 0;
        loop {
            rounds += 1;
            assert!(rounds < 1000, "{variant:?}: never went quiet");
            let got = client.pump(&rec, &hub, sub, true);
            probe.idle(&hub);
            if got == 0 && client.need_refresh.is_empty() && rec.msgs.lock().is_empty() {
                break;
            }
        }
    };
    quiet(&mut client);

    let fresh_ignore = super::ignore_cache::IgnoreLock::new(IgnoreStack::for_test(&root, None, None, &[]));
    // The events alone brought the hub to the disk: every open folder it holds is what a fresh
    // listing says, before any edge could re-list one.
    for rel in &client.expanded {
        let abs = if rel.is_empty() { root.clone() } else { root.join(rel) };
        if !abs.is_dir() {
            continue;
        }
        let fresh = list::list_dir(&root, rel, &fresh_ignore).unwrap();
        let names = |v: &[Entry]| v.iter().map(|e| e.n.clone()).collect::<BTreeSet<_>>();
        let held = hub.dirs.lock().get(rel).map(|d| (d.gen, d.entries.clone()));
        let Some((gen, held)) = held else { panic!("{variant:?} seed {seed}: the hub holds no listing of open folder {rel:?}") };
        assert!(
            held == fresh.entries,
            "{variant:?} seed {seed}: from the events alone the hub's {rel:?} (gen {gen}) drifted from the disk. Lost batches seen: {}. Missing: {:?}. Extra: {:?}",
            probe.lost(),
            names(&fresh.entries).difference(&names(&held)).collect::<Vec<_>>(),
            names(&held).difference(&names(&fresh.entries)).collect::<Vec<_>>(),
        );
    }
    // An edge then re-lists what the events patched (their folders' mtimes moved) and finds
    // nothing new: nothing is sent.
    let relisted = probe.reconcile(&hub, "");
    let after: Vec<TreeEvent> = rec.msgs.lock().iter().map(|m| m.ev.clone()).collect();
    assert!(after.is_empty(), "{variant:?} seed {seed}: the edge after the events (re-listed {relisted:?}) sent {after:?}");
    quiet(&mut client);

    for rel in &client.expanded {
        let abs = if rel.is_empty() { root.clone() } else { root.join(rel) };
        if !abs.is_dir() {
            continue;
        }
        let held = client.dirs.get(rel).unwrap_or_else(|| panic!("{variant:?}: open folder {rel:?} is not held"));
        let mine = held.1.clone();
        let fresh = list::list_dir(&root, rel, &fresh_ignore).unwrap();
        if mine != fresh.entries {
            // Which side is wrong, last in the message so a cut-off log still shows it.
            let names = |v: &[Entry]| v.iter().map(|e| e.n.clone()).collect::<BTreeSet<_>>();
            let (m, f) = (names(&mine), names(&fresh.entries));
            let hub_snap = hub.dirs.lock().get(rel).map(|d| (d.gen, d.entries == fresh.entries));
            let lost = probe.lost();
            // A hub that is merely behind catches up after one more edge; one that stays wrong
            // missed a change its folder's mtime didn't show.
            let again = probe.reconcile(&hub, "");
            let fresh2 = list::list_dir(&root, rel, &fresh_ignore).unwrap();
            let caught_up = hub.dirs.lock().get(rel).map(|d| (d.gen, d.entries == fresh2.entries));
            panic!(
                "{variant:?} seed {seed}: {rel:?} drifted from the disk. Lost batches seen: {lost}. After another edge (re-listed {again:?}), hub (gen, matches disk): {caught_up:?}. Client holds gen {}, {} entries; disk {}. Missing: {:?}. Extra: {:?}. Same names, other order or flags: {}. Hub snapshot (gen, matches disk): {hub_snap:?}. Client still building: {:?}",
                held.0,
                mine.len(),
                fresh.entries.len(),
                f.difference(&m).collect::<Vec<_>>(),
                m.difference(&f).collect::<Vec<_>>(),
                m == f,
                client.building.get(rel).map(|b| (b.0, b.1, b.2.len())),
            );
        }
    }
    for rel in client.dirs.keys() {
        let abs = if rel.is_empty() { root.clone() } else { root.join(rel) };
        assert!(abs.is_dir(), "{variant:?}: still holding {rel:?}, which is gone");
    }
    ft.close(sub);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn converges_under_random_bursts() {
    converge(Variant::Plain, 1);
}

#[test]
fn converges_when_the_raw_channel_overflows() {
    converge(Variant::Overflow, 2);
}

#[test]
fn converges_across_pause_and_resume() {
    converge(Variant::PauseResume, 3);
}

#[test]
fn converges_after_a_stall_from_withheld_acks() {
    converge(Variant::WithheldAcks, 4);
}

#[test]
#[ignore = "a few minutes of random bursts across many seeds; run with --ignored"]
fn converges_across_many_seeds() {
    for seed in 10..16 {
        for v in [Variant::Plain, Variant::Overflow, Variant::PauseResume, Variant::WithheldAcks] {
            converge(v, seed);
        }
    }
}


/// A delivered event is applied: the hub patches the folder and sends a Delta from the
/// generation the client holds. The event is handed over by the test (`Probe::deliver`), so the
/// test holds however late, out of order or lost FSEvents is (docs/file-tree.md §6.1).
#[test]
fn a_change_arrives_as_a_delta_from_the_held_generation() {
    let root = temp_root("delta");
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    let held = hub.dirs.lock().get("").map(|d| d.gen).expect("the root is listed");
    std::fs::write(root.join("x.txt"), "").unwrap();
    probe.deliver(&hub, &[root.join("x.txt")]);
    assert!(
        rec.msgs.lock().iter().any(|m| matches!(&m.ev, TreeEvent::Delta { rel, from, add, .. }
            if rel.is_empty() && *from == held && add.iter().any(|a| a.n == "x.txt"))),
        "x.txt arrives as a Delta from the held generation"
    );
    // Saving a file changes nothing the tree shows: no message about it.
    let before = rec.msgs.lock().len();
    std::fs::write(root.join("x.txt"), "more").unwrap();
    probe.deliver(&hub, &[root.join("x.txt")]);
    let after: Vec<TreeEvent> = rec.msgs.lock()[before..].iter().map(|m| m.ev.clone()).collect();
    assert!(after.is_empty(), "the save sent {after:?}");
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Wait for the first message matching `f`, pumping the recorder.
fn wait_for(rec: &Recorder, timeout: Duration, f: impl Fn(&TreeEvent) -> bool) -> Option<Instant> {
    let end = Instant::now() + timeout;
    let mut seen = 0;
    while Instant::now() < end {
        {
            let msgs = rec.msgs.lock();
            if let Some(_) = msgs[seen..].iter().find(|m| f(&m.ev)) {
                return Some(Instant::now());
            }
            seen = msgs.len();
        }
        std::thread::sleep(Duration::from_micros(200));
    }
    None
}

#[test]
fn a_known_generation_is_answered_not_modified() {
    let root = temp_root("notmod");
    std::fs::create_dir_all(root.join("a")).unwrap();
    std::fs::write(root.join("a/x"), "").unwrap();
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![DirReq { rel: "a".into(), known_gen: None }], rec.clone(), vec![]).unwrap();
    // The generation the client holds: its latest listing OR delta.
    let gen_of = |rec: &Recorder, rel: &str| {
        rec.msgs.lock().iter().rev().find_map(|m| match &m.ev {
            TreeEvent::Dir { rel: r, gen, .. } | TreeEvent::Delta { rel: r, gen, .. } if r == rel => Some(*gen),
            _ => None,
        })
    };
    let hub = ft.hub_of(ack.sub_id).unwrap();
    // Taken from the hub itself once nothing is being listed (the watcher's start re-lists every
    // open folder), never by waiting on an OS event. A watcher may still report the setup's own
    // writes later (Windows did, CI 2026-10-31; FSEvents can be 20 s late), but a re-list that
    // finds the same entries keeps the generation, so this one stays the folder's.
    probe.watching(&hub);
    probe.idle(&hub);
    let gen = hub.dirs.lock().get("a").map(|d| d.gen).expect("a is listed");
    assert_eq!(gen_of(&rec, "a"), Some(gen), "the client holds what the hub has");
    drop(hub);
    // Close and come back within the grace period: the folder is answered from memory.
    ft.close(ack.sub_id);
    let rec2 = Arc::new(Recorder::default());
    ft.open("w", &root.to_string_lossy(), vec![DirReq { rel: "a".into(), known_gen: Some(gen) }], rec2.clone(), vec![]).unwrap();
    assert!(wait_for(&rec2, HANG, |e| matches!(e, TreeEvent::NotModified { rel, gen: g } if rel == "a" && *g == gen)).is_some());
    assert_eq!(ft.hub_count(), 1, "one hub per root, shared");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_deleted_root_says_root_gone() {
    let root = temp_root("rootgone");
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    // The watcher's report of the deletion is what is tested, handed over by the test: FSEvents
    // may report it late or not at all (§6.1). Deleted before the watcher runs, the root would
    // be found gone by the start's own re-list instead.
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    std::fs::remove_dir_all(&root).unwrap();
    probe.deliver(&hub, &[root.clone()]);
    assert!(rec.msgs.lock().iter().any(|m| m.ev == TreeEvent::RootGone));
}

/// Close a test's last subscription to its hub and wait until nothing of the hub runs in the
/// root, before the test deletes it. The git worker runs `git -C root`, and on Windows a folder
/// that is some process's working directory can't be deleted (OS error 32): so the hub is
/// reaped (its grace ended by the test, not slept through) and its worker waited out. `hub` is
/// the test's last handle on it.
fn close_and_settle(ft: &FileTreeState, sub: u64, hub: Arc<RootHub>) {
    let git = hub.git.clone();
    ft.hold_graces();
    ft.close(sub);
    ft.end_graces();
    drop(hub);
    if let Some(g) = git {
        assert!(g.wait_exited(super::test_support::HANG), "the hub's git worker never stopped: something still holds the hub");
    }
}

fn git_init(dir: &std::path::Path) {
    let ok = std::process::Command::new("git").args(["init", "-q"]).current_dir(dir).status().is_ok_and(|s| s.success());
    assert!(ok, "git init");
}

#[test]
fn git_init_in_an_open_root_reopens_it_with_its_repository() {
    // A hub reads its repository once; `git init` in a plain root used to leave it without
    // decorations for good, and a reopen within the grace period got the same hub back.
    let root = temp_root("gitinit");
    std::fs::write(root.join("a.ts"), "x").unwrap();
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    ft.hold_graces();
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    assert!(ack.repo.is_none());
    let old = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&old);
    git_init(&root);
    // The watcher's report of the new `.git`, handed over by the test (FSEvents may be late or
    // lose it, §6.1; an edge catches it then, `a_lost_git_init_reopens_on_an_edge`).
    probe.deliver(&old, &[root.join(".git")]);
    assert!(rec.msgs.lock().iter().any(|m| m.ev == TreeEvent::Reopen), "the view is told to reopen");
    // What the client does: close, open the same place again.
    let rec2 = Arc::new(Recorder::default());
    let again = ft.open("w", &root.to_string_lossy(), vec![], rec2, vec![]).unwrap();
    ft.close(ack.sub_id);
    assert!(again.repo.is_some(), "the new hub decorates by the new repository");
    let new = ft.hub_of(again.sub_id).unwrap();
    assert!(!Arc::ptr_eq(&old, &new));
    // The new hub's git worker runs `git -C root`: waited out before the root is deleted.
    let git = new.git.clone().expect("the new hub has a repository");
    drop((old, new));
    // The old hub's reap (its last subscriber just left) must not take its successor.
    ft.end_graces();
    assert!(ft.hub_of(again.sub_id).is_ok());
    assert_eq!(ft.hub_count(), 1);
    // In its grace period, with nobody to tell: the repository removed is caught when the
    // place is opened again (the reopen reads the disk; the watcher may not have run).
    ft.close(again.sub_id);
    std::fs::remove_dir_all(root.join(".git")).unwrap();
    let plain = ft.open("w", &root.to_string_lossy(), vec![], Arc::new(Recorder::default()), vec![]).unwrap();
    assert!(plain.repo.is_none(), "a hub in its grace is not reused for another repository");
    ft.close(plain.sub_id);
    // The repository's hub was retired for `plain`; whatever still held it is gone with the
    // graces ended, and its worker with it.
    ft.end_graces();
    assert!(git.wait_exited(super::test_support::HANG), "the repository hub's git worker never stopped");
    std::fs::remove_dir_all(&root).unwrap();
}

/// FSEvents under load loses changes without saying so (docs/file-tree.md §6.1). A hub that
/// heard nothing at all (`go_deaf`) still shows the disk after each kind of edge: the open
/// folders whose mtime moved are listed again, and one that didn't move sends nothing.
#[test]
fn an_edge_relists_what_the_watcher_never_reported() {
    let root = temp_root("edge");
    for d in ["a", "b"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
        std::fs::write(root.join(d).join("one.txt"), "").unwrap();
    }
    age(&[&root, &root.join("a"), &root.join("b")]);
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let open = vec![DirReq { rel: "a".into(), known_gen: None }, DirReq { rel: "b".into(), known_gen: None }];
    let ack = ft.open("w", &root.to_string_lossy(), open, rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    hub.go_deaf();
    let ignore = super::ignore_cache::IgnoreLock::new(IgnoreStack::for_test(&root, None, None, &[]));
    let disk = |rel: &str| list::list_dir(&root, rel, &ignore).unwrap().entries;
    let held = |rel: &str| hub.dirs.lock().get(rel).map(|d| d.entries.clone()).unwrap();
    let about = |rel: &str, from: usize| -> Vec<TreeEvent> {
        rec.msgs.lock()[from..]
            .iter()
            .filter(|m| matches!(&m.ev, TreeEvent::Dir { rel: r, .. } | TreeEvent::Delta { rel: r, .. } | TreeEvent::NotModified { rel: r, .. } if r == rel))
            .map(|m| m.ev.clone())
            .collect()
    };
    let a = root.join("a");
    type Edge<'e> = (&'e str, Box<dyn Fn(u32) + 'e>, Box<dyn Fn() + 'e>);
    let edges: Vec<Edge> = vec![
        // A command finished in a terminal whose shell is in a/ (OSC 133 D).
        ("a command", Box::new(|n| std::fs::write(a.join(format!("cmd{n}.txt")), "").unwrap()), Box::new(|| ft.note_command_in(&a))),
        // The window came back.
        ("focus", Box::new(|n| std::fs::rename(a.join("one.txt"), a.join(format!("moved{n}.txt"))).unwrap()), Box::new(|| {
            ft.window_focus("w", false);
            ft.window_focus("w", true);
        })),
        // An agent's shell call in a/ ended.
        ("an agent's Bash", Box::new(|n| std::fs::create_dir(a.join(format!("dir{n}"))).unwrap()), Box::new(|| {
            ft.agents.record(hook(tool_event("PostToolUse", "Bash", serde_json::json!({ "command": "make" }), "b1", &a), "t1"));
        })),
        // An agent wrote a file in a/ (its own write lands through the ledger's dispatcher).
        ("an agent's Write", Box::new(|n| std::fs::write(a.join(format!("w{n}.txt")), "x").unwrap()), Box::new(|| {
            let input = serde_json::json!({ "file_path": a.join("w3.txt"), "content": "x" });
            ft.agents.record(hook(tool_event("PostToolUse", "Write", input, "w1", &a), "t1"));
        })),
    ];
    // The Bash call's start, so its end is a call that ran (the ledger keeps local calls only).
    ft.agents.record(hook(prompt_event(&a), "t1"));
    ft.agents.record(hook(tool_event("PreToolUse", "Bash", serde_json::json!({ "command": "make" }), "b1", &a), "t1"));
    for (n, (what, change, fire)) in edges.iter().enumerate() {
        let before = rec.msgs.lock().len();
        change(n as u32);
        assert_ne!(held("a"), disk("a"), "{what}: the deaf hub can't have heard of the change");
        let asked = hub.reconcile_asked();
        fire();
        let relisted = probe.reconciled(&hub, asked);
        assert!(relisted.contains("a") && !relisted.contains("b"), "{what}: re-listed {relisted:?}");
        assert_eq!(held("a"), disk("a"), "{what}: the hub shows the disk");
        assert!(about("a", before).iter().any(|e| matches!(e, TreeEvent::Delta { .. })), "{what}: the client got a Delta");
        assert!(about("b", before).is_empty(), "{what}: nothing about an unchanged folder");
    }

    // Expanding a folder whose mtime moved lists it again rather than answering from the
    // snapshot; one that didn't is answered from the snapshot.
    // The edges above left a/ listed young, so untrusted: age it and let an edge list it again,
    // so its snapshot carries a trusted mtime that late.txt then moves.
    age(&[&a]);
    let asked = hub.reconcile_asked();
    ft.note_command_in(&a);
    assert!(probe.reconciled(&hub, asked).contains("a"));
    std::fs::write(a.join("late.txt"), "").unwrap();
    // Aged before this listing, so the one it records is trusted and the idle edge below has
    // nothing to re-list.
    age(&[&a]);
    let gen = |rel: &str| hub.dirs.lock().get(rel).map(|d| d.gen).unwrap();
    let rec2 = Arc::new(Recorder::default());
    let reqs = vec![DirReq { rel: "a".into(), known_gen: Some(gen("a")) }, DirReq { rel: "b".into(), known_gen: Some(gen("b")) }];
    let second = ft.open("w2", &root.to_string_lossy(), reqs, rec2.clone(), vec![]).unwrap();
    probe.idle(&hub);
    let got: Vec<TreeEvent> = rec2.msgs.lock().iter().map(|m| m.ev.clone()).collect();
    assert!(got.iter().any(|e| matches!(e, TreeEvent::Dir { rel, entries, .. } if rel == "a" && entries.iter().any(|x| x.n == "late.txt"))), "{got:?}");
    assert!(got.iter().any(|e| matches!(e, TreeEvent::NotModified { rel, .. } if rel == "b")), "{got:?}");
    ft.close(second.sub_id);

    // Nothing changed: an edge re-lists nothing and sends nothing.
    let before = rec.msgs.lock().len();
    let asked = hub.reconcile_asked();
    ft.note_command_in(&root);
    assert!(probe.reconciled(&hub, asked).is_empty());
    assert!(about("a", before).is_empty() && about("b", before).is_empty() && about("", before).is_empty());

    // The root deleted unseen is found gone on the next edge.
    std::fs::remove_dir_all(&root).unwrap();
    let asked = hub.reconcile_asked();
    ft.window_focus("w", false);
    ft.window_focus("w", true);
    probe.reconciled(&hub, asked);
    assert!(rec.msgs.lock().iter().any(|m| m.ev == TreeEvent::RootGone));
    ft.close(ack.sub_id);
}

/// `git init` whose `.git` the watcher never reported: the command finishing is the edge, and
/// the views reopen with the new repository (docs/file-tree.md §6.1).
#[test]
fn a_lost_git_init_reopens_on_an_edge() {
    let root = temp_root("gitedge");
    std::fs::write(root.join("a.ts"), "x").unwrap();
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    assert!(ack.repo.is_none());
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    hub.go_deaf();
    git_init(&root);
    let asked = hub.reconcile_asked();
    ft.note_command_in(&root);
    probe.reconciled(&hub, asked);
    assert!(rec.msgs.lock().iter().any(|m| m.ev == TreeEvent::Reopen), "the view is told to reopen");
    close_and_settle(&ft, ack.sub_id, hub);
    std::fs::remove_dir_all(&root).unwrap();
}

/// Which rows are flagged ignored follows `.gitignore` and `info/exclude`. A change to one whose
/// event was lost shows after an edge, even an edit in place, which moves no folder's mtime
/// (docs/file-tree.md §6.1).
#[test]
fn a_lost_ignore_rule_change_shows_after_an_edge() {
    let root = temp_root("ignedge");
    std::fs::create_dir_all(root.join("dist")).unwrap();
    std::fs::write(root.join("dist/bundle.js"), "").unwrap();
    git_init(&root);
    std::fs::create_dir_all(root.join(".git/info")).unwrap();
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    hub.go_deaf();
    let ignored = || hub.dirs.lock().get("").and_then(|d| d.entries.iter().find(|e| e.n == "dist").map(|e| e.f & super::types::FLAG_IGNORED != 0));
    let edge = || {
        let asked = hub.reconcile_asked();
        ft.note_command_in(&root);
        probe.reconciled(&hub, asked)
    };
    assert_eq!(ignored(), Some(false));
    std::fs::write(root.join(".gitignore"), "dist/\n").unwrap();
    let relisted = edge();
    assert_eq!(ignored(), Some(true), "a new .gitignore (re-listed {relisted:?})");
    std::fs::write(root.join(".gitignore"), "other/\n").unwrap();
    let relisted = edge();
    assert_eq!(ignored(), Some(false), "a .gitignore edited in place (re-listed {relisted:?})");
    std::fs::write(root.join(".git/info/exclude"), "dist/\n").unwrap();
    let relisted = edge();
    assert_eq!(ignored(), Some(true), "the repository's info/exclude (re-listed {relisted:?})");
    let flipped = rec.msgs.lock().iter().filter(|m| matches!(&m.ev, TreeEvent::Delta { rel, chg, .. } if rel.is_empty() && chg.iter().any(|e| e.n == "dist"))).count();
    assert_eq!(flipped, 3, "the client saw each flip as a Delta");
    close_and_settle(&ft, ack.sub_id, hub);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A `.gitignore` whose whole-second mtime is in the future (a zip made in a later timezone)
/// can't be told unchanged by its mtime. Idle edges re-read it and do nothing else: no re-list,
/// no git run (docs/file-tree.md §6.1).
#[test]
fn idle_edges_over_a_future_dated_gitignore_do_nothing() {
    let root = temp_root("futureignore");
    std::fs::create_dir_all(root.join("a/b")).unwrap();
    git_init(&root);
    std::fs::write(root.join(".gitignore"), "dist/\n").unwrap();
    let future = std::time::UNIX_EPOCH + Duration::from_secs(1_900_000_000);
    std::fs::File::options().write(true).open(root.join(".gitignore")).unwrap().set_modified(future).unwrap();
    age(&[&root, &root.join("a"), &root.join("a/b")]);
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let open = vec![DirReq { rel: "a".into(), known_gen: None }, DirReq { rel: "a/b".into(), known_gen: None }];
    let ack = ft.open("w", &root.to_string_lossy(), open, rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    hub.go_deaf();
    let core = hub.git.clone().expect("a repository");
    let signals = core.test_signals();
    for i in 0..3 {
        let relisted = probe.reconcile(&hub, "a/b");
        assert!(relisted.is_empty(), "idle edge {i} re-listed {relisted:?}");
    }
    assert_eq!(core.test_signals(), signals, "idle edges asked git for nothing");
    close_and_settle(&ft, ack.sub_id, hub);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A folder that was open, then closed: its `.gitignore` changes while it is closed and the
/// event is lost. An edge comes while it is closed (and checks only the open folders' rules),
/// then the user opens it again: it is listed with the rules on disk (docs/file-tree.md §6.1).
/// And an open folder whose rules moved with no edge yet is listed again when another folder
/// is opened.
#[test]
fn a_lost_gitignore_in_a_closed_folder_shows_when_reopened() {
    let root = temp_root("reopenignore");
    git_init(&root);
    std::fs::create_dir_all(root.join("pkg/gen")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    std::fs::write(root.join("pkg/gen/x.js"), "").unwrap();
    age(&[&root, &root.join("pkg"), &root.join("pkg/gen"), &root.join("other")]);
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    hub.expand(ack.sub_id, vec![DirReq { rel: "pkg".into(), known_gen: None }]);
    probe.idle(&hub);
    let flag = |rel: &str, name: &str| hub.dirs.lock().get(rel).and_then(|d| d.entries.iter().find(|e| e.n == name).map(|e| e.f & super::types::FLAG_IGNORED != 0));
    assert_eq!(flag("pkg", "gen"), Some(false));
    hub.collapse(ack.sub_id, vec!["pkg".into()]);
    probe.idle(&hub);
    hub.go_deaf();
    std::fs::write(root.join("pkg/.gitignore"), "gen/\n").unwrap();
    // An edge while pkg is closed (the window regains focus, a command ends).
    let asked = hub.reconcile_asked();
    ft.note_command_in(&root);
    let relisted = probe.reconciled(&hub, asked);
    assert!(relisted.is_empty(), "nothing open moved: {relisted:?}");
    hub.expand(ack.sub_id, vec![DirReq { rel: "pkg".into(), known_gen: None }]);
    probe.idle(&hub);
    assert_eq!(flag("pkg", "gen"), Some(true), "pkg reopened with the rules on disk");
    // The root's own rules change, lost, and no edge comes: opening another folder lists the
    // root again too, since it was listed with the old rules.
    assert_eq!(flag("", "pkg"), Some(false));
    std::fs::write(root.join(".gitignore"), "pkg/\n").unwrap();
    hub.expand(ack.sub_id, vec![DirReq { rel: "other".into(), known_gen: None }]);
    probe.idle(&hub);
    assert_eq!(flag("", "pkg"), Some(true), "the open root, listed again with the rules on disk");
    close_and_settle(&ft, ack.sub_id, hub);
    std::fs::remove_dir_all(&root).unwrap();
}

/// A folder the ignore stack loaded while it was closed, for a worktree event checked for git
/// or for Find's index, then opened: its `.gitignore` changed with the event lost, and it is
/// listed with the rules on disk, not the copy loaded earlier (docs/file-tree.md §6.1).
#[test]
fn a_folder_loaded_for_git_or_find_opens_with_the_rules_on_disk() {
    for find in [false, true] {
        let root = temp_root(if find { "loadfind" } else { "loadgit" });
        git_init(&root);
        std::fs::create_dir_all(root.join("pkg/gen")).unwrap();
        std::fs::write(root.join("pkg/x.ts"), "").unwrap();
        let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
        let probe = Probe::new(&ft);
        let rec = Arc::new(Recorder::default());
        let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
        let hub = ft.hub_of(ack.sub_id).unwrap();
        probe.watching(&hub);
        probe.idle(&hub);
        hub.go_deaf();
        if find {
            let _ = hub.search(ack.sub_id, "x", 10, 1, false);
        }
        std::fs::write(root.join("pkg/x.ts"), "y").unwrap();
        probe.deliver(&hub, &[root.join("pkg/x.ts")]);
        let pkg = ["pkg".to_string()];
        assert_eq!(hub.ignore.lock().stamps(&pkg).len(), 2, "find={find}: the stack holds pkg's rules while pkg is closed");
        std::fs::write(root.join("pkg/.gitignore"), "gen/\n").unwrap();
        hub.expand(ack.sub_id, vec![DirReq { rel: "pkg".into(), known_gen: None }]);
        probe.idle(&hub);
        let gen = hub.dirs.lock().get("pkg").and_then(|d| d.entries.iter().find(|e| e.n == "gen").map(|e| e.f & super::types::FLAG_IGNORED != 0));
        assert_eq!(gen, Some(true), "find={find}: pkg opened with the rules on disk");
        close_and_settle(&ft, ack.sub_id, hub);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// The Files view is closed and opened again within the hub's grace (switching tabs back to
/// a root): the kept snapshot of pkg answers the restore. pkg/.gitignore appeared while the
/// view was closed and its event was lost: pkg reopens with the rules on disk (§6.1).
#[test]
fn a_lost_gitignore_shows_when_reopened_within_the_grace() {
    for in_place in [false, true] {
        let root = temp_root("rv134grace");
        git_init(&root);
        std::fs::create_dir_all(root.join("pkg/gen")).unwrap();
        std::fs::write(root.join("pkg/gen/x.js"), "").unwrap();
        if in_place {
            std::fs::write(root.join("pkg/.gitignore"), "# nothing yet\n").unwrap();
        }
        let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
        ft.hold_graces();
        let probe = Probe::new(&ft);
        let rec = Arc::new(Recorder::default());
        let restore = || vec![DirReq { rel: "".into(), known_gen: None }, DirReq { rel: "pkg".into(), known_gen: None }];
        let ack = ft.open("w", &root.to_string_lossy(), restore(), rec.clone(), vec![]).unwrap();
        let hub = ft.hub_of(ack.sub_id).unwrap();
        probe.watching(&hub);
        probe.idle(&hub);
        let flag = |rel: &str, name: &str| hub.dirs.lock().get(rel).and_then(|d| d.entries.iter().find(|e| e.n == name).map(|e| e.f & super::types::FLAG_IGNORED != 0));
        assert_eq!(flag("pkg", "gen"), Some(false));
        // The view closes (tab switch): the hub keeps its snapshots for the grace.
        ft.close(ack.sub_id);
        hub.go_deaf();
        std::fs::write(root.join("pkg/.gitignore"), "gen/\n").unwrap();
        // The view comes back with what it had open.
        let rec2 = Arc::new(Recorder::default());
        let again = ft.open("w", &root.to_string_lossy(), restore(), rec2.clone(), vec![]).unwrap();
        let hub2 = ft.hub_of(again.sub_id).unwrap();
        assert!(Arc::ptr_eq(&hub, &hub2), "the same hub, back from its grace");
        probe.idle(&hub);
        assert_eq!(flag("pkg", "gen"), Some(true), "in_place={in_place}: pkg reopened within the grace with the rules on disk");
        drop(hub2);
        close_and_settle(&ft, again.sub_id, hub);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// The user presses Refresh (ft_refresh, every open folder) after pkg/.gitignore changed with
/// its event lost and no edge yet: the refreshed listing uses the rules on disk (§6.1).
#[test]
fn refresh_lists_an_open_folder_with_the_rules_on_disk() {
    for one in [false, true] {
        let root = temp_root("rv134refresh");
        git_init(&root);
        std::fs::create_dir_all(root.join("pkg/gen")).unwrap();
        std::fs::write(root.join("pkg/gen/x.js"), "").unwrap();
        let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
        let probe = Probe::new(&ft);
        let rec = Arc::new(Recorder::default());
        let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
        let hub = ft.hub_of(ack.sub_id).unwrap();
        probe.watching(&hub);
        probe.idle(&hub);
        hub.expand(ack.sub_id, vec![DirReq { rel: "pkg".into(), known_gen: None }]);
        probe.idle(&hub);
        let flag = |rel: &str, name: &str| hub.dirs.lock().get(rel).and_then(|d| d.entries.iter().find(|e| e.n == name).map(|e| e.f & super::types::FLAG_IGNORED != 0));
        assert_eq!(flag("pkg", "gen"), Some(false));
        hub.go_deaf();
        std::fs::write(root.join("pkg/.gitignore"), "gen/\n").unwrap();
        hub.refresh(ack.sub_id, if one { Some("pkg".into()) } else { None });
        probe.idle(&hub);
        assert_eq!(flag("pkg", "gen"), Some(true), "refresh one={one}: pkg listed with the rules on disk");
        close_and_settle(&ft, ack.sub_id, hub);
        std::fs::remove_dir_all(&root).unwrap();
    }
}

/// The rules files are read off the disk with the hub's ignore lock released: a slow disk under
/// a refresh, an edge or a fresh open must not hold up every listing and event that matches
/// rows (docs/file-tree.md §6.1).
#[test]
fn rules_files_are_read_without_the_ignore_lock() {
    let root = temp_root("rulesnolock");
    git_init(&root);
    std::fs::create_dir_all(root.join("pkg/gen")).unwrap();
    std::fs::create_dir_all(root.join("other")).unwrap();
    std::fs::write(root.join("pkg/.gitignore"), "gen/\n").unwrap();
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let restore = vec![DirReq { rel: "".into(), known_gen: None }, DirReq { rel: "pkg".into(), known_gen: None }];
    let ack = ft.open("w", &root.to_string_lossy(), restore, rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    // Whether the thread reading the rules files holds the ignore lock, each time. Another
    // thread holding it meanwhile (a listing matching its rows) is no fault.
    let seen: Arc<Mutex<Vec<bool>>> = Arc::new(Mutex::new(Vec::new()));
    let key = PathBuf::from(root.file_name().unwrap());
    {
        let seen = seen.clone();
        super::ignore_cache::MOVED_HOOKS.lock().push((key.clone(), Arc::new(move || {
            seen.lock().push(super::ignore_cache::held_here());
        })));
    }
    let take = |what: &str| {
        let got: Vec<bool> = seen.lock().drain(..).collect();
        assert!(!got.is_empty(), "{what}: the rules files were read");
        assert!(got.iter().all(|held| !held), "{what}: the ignore lock was held while the rules files were read: {got:?}");
    };
    hub.refresh(ack.sub_id, None);
    probe.idle(&hub);
    take("refresh");
    let asked = hub.reconcile_asked();
    hub.reconcile("");
    probe.reconciled(&hub, asked);
    probe.idle(&hub);
    take("an edge");
    hub.expand(ack.sub_id, vec![DirReq { rel: "other".into(), known_gen: None }]);
    probe.idle(&hub);
    take("a fresh open");
    super::ignore_cache::MOVED_HOOKS.lock().retain(|(r, _)| r != &key);
    close_and_settle(&ft, ack.sub_id, hub);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_hub_is_reaped_after_its_grace() {
    let root = temp_root("reap");
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    ft.hold_graces();
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    ft.close(ack.sub_id);
    assert_eq!(ft.hub_count(), 1, "kept through the grace period");
    ft.end_graces();
    assert_eq!(ft.hub_count(), 0);
    // A window closing drops what it held.
    let ack = ft.open("w2", &root.to_string_lossy(), vec![], rec, vec![]).unwrap();
    ft.window_destroyed("w2");
    assert!(ft.hub_of(ack.sub_id).is_err());
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn locate_spells_names_as_the_disk_does() {
    use unicode_normalization::UnicodeNormalization;
    let root = temp_root("locate");
    let nfd: String = "café".nfd().collect();
    std::fs::create_dir_all(root.join(&nfd).join("Src")).unwrap();
    let ft = FileTreeState::for_test(Duration::from_secs(30), super::watch::RAW_CAPACITY);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec, vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    // A hook path arrives NFC. On macOS it names the NFD folder on disk, and the rel must use
    // the disk's spelling (the tree's row key); elsewhere it is a different, missing folder.
    let nfc: String = if super::paths::NORMALIZATION_INSENSITIVE { "café".nfc().collect() } else { nfd.clone() };
    let asked = root.join(&nfc).join("Src").join("a.ts").to_string_lossy().to_string();
    let got = hub.locate(&[asked, "/definitely/elsewhere".into()]);
    assert_eq!(got[0].as_deref(), Some(format!("{nfd}/Src/a.ts").as_str()));
    assert_eq!(got[1], None);
    if hub.fold {
        let upper = root.join(&nfc).join("SRC").join("a.ts").to_string_lossy().to_string();
        assert_eq!(hub.locate(&[upper])[0].as_deref(), Some(format!("{nfd}/Src/a.ts").as_str()));
    }
    let stat = hub.stat(&format!("{nfd}/Src")).unwrap();
    assert!(!stat.exec && stat.numstat.is_none());
    std::fs::remove_dir_all(&root).unwrap();
}

// ── Budgets (§17) ────────────────────────────────────────────────────────────────────────

/// 20 top-level folders × 50 × 100 files = 100k files, plus a 50k-entry folder.
fn big_fixture() -> PathBuf {
    let root = temp_root("budget");
    for a in 0..20 {
        for b in 0..50 {
            let d = root.join(format!("pkg{a}")).join(format!("mod{b}"));
            std::fs::create_dir_all(&d).unwrap();
            for c in 0..100 {
                std::fs::write(d.join(format!("f{c}.ts")), "").unwrap();
            }
        }
    }
    let big = root.join("big");
    std::fs::create_dir_all(&big).unwrap();
    for i in 0..50_000 {
        std::fs::write(big.join(format!("item{i}.json")), "").unwrap();
    }
    root
}

#[test]
#[ignore = "builds a 150k-file fixture; run with --ignored (and --release for the real numbers)"]
fn budgets_on_a_generated_fixture() {
    let root = big_fixture();
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let t0 = Instant::now();
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let first = wait_for(&rec, HANG, |e| matches!(e, TreeEvent::Dir { rel, chunk: 0, .. } if rel.is_empty())).unwrap();
    let open_ms = (first - t0).as_secs_f64() * 1000.0;
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let t1 = Instant::now();
    hub.expand(ack.sub_id, vec![DirReq { rel: "big".into(), known_gen: None }]);
    let chunk = wait_for(&rec, HANG, |e| matches!(e, TreeEvent::Dir { rel, chunk: 0, .. } if rel == "big")).unwrap();
    let big_ms = (chunk - t1).as_secs_f64() * 1000.0;
    let full = wait_for(&rec, HANG, |e| matches!(e, TreeEvent::Dir { rel, last: true, .. } if rel == "big"));
    assert!(full.is_some());
    // The floor no listing can beat: the bare directory read, measured on this machine now.
    let t2 = Instant::now();
    let n = std::fs::read_dir(root.join("big")).unwrap().count();
    let floor_ms = (Instant::now() - t2).as_secs_f64() * 1000.0;
    eprintln!("root open {open_ms:.1} ms, first chunk of 50k {big_ms:.1} ms (bare read_dir of {n}: {floor_ms:.1} ms)");
    assert!(open_ms <= 50.0, "root open took {open_ms:.1} ms (budget 50)");
    // The budget is 30 ms. A loaded or slow disk can spend that on read_dir alone, which no
    // listing can avoid; what this code adds on top of it must stay small either way.
    assert!(big_ms <= 30.0 || big_ms <= floor_ms + 8.0, "first chunk of a 50k folder took {big_ms:.1} ms (budget 30; bare read {floor_ms:.1})");
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn the_repository_is_found_on_disk_worktrees_included() {
    let t = temp_root("repo");
    // Native paths, joined a component at a time: `t.join("main/.git")` keeps the '/' on
    // Windows, while repo_info answers canonical paths with native separators.
    let at = |rel: &str| rel.split('/').fold(t.clone(), |p, c| p.join(c));
    std::fs::create_dir_all(at("main/.git/worktrees/wt")).unwrap();
    std::fs::create_dir_all(at("main/src")).unwrap();
    std::fs::create_dir_all(at("wt")).unwrap();
    std::fs::write(at("wt/.git"), format!("gitdir: {}\n", at("main/.git/worktrees/wt").display())).unwrap();
    std::fs::write(at("main/.git/worktrees/wt/commondir"), "../..\n").unwrap();
    let (info, git_dir) = super::hub::repo_info(&at("main")).unwrap();
    assert_eq!(info.toplevel_rel, "");
    assert_eq!(git_dir, at("main/.git"));
    let (info, _) = super::hub::repo_info(&at("main/src")).unwrap();
    assert_eq!(info.toplevel_rel, "..", "a root inside a repo points up to its top level");
    let (info, git_dir) = super::hub::repo_info(&at("wt")).unwrap();
    assert_eq!((info.toplevel_rel.as_str(), git_dir.clone()), ("", at("main/.git/worktrees/wt")));
    assert_eq!(info.common_dir, at("main/.git").to_string_lossy());
    assert!(super::hub::repo_info(&t).is_none(), "a plain folder is in no repository");
    std::fs::remove_dir_all(&t).unwrap();
}

/// Smoke test of the real OS watcher (FSEvents on macOS, inotify on Linux, RDCW on Windows):
/// starts cleanly and reports a real filesystem write to the hub. Kept as the sole always-on
/// real-watcher delivery test so dozens of synthetic-event tests do not queue `FSEventStreamStart`
/// RPCs behind each other in `fseventsd` under parallel `cargo test --lib`.
#[test]
fn the_native_watcher_starts_and_reports_a_change() {
    let root = temp_root("nativewatch");
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    probe.watching(&hub);
    probe.idle(&hub);
    let names = probe.settle(&hub, &root);
    let entries: BTreeSet<String> = hub
        .dirs
        .lock()
        .get("")
        .map(|d| d.entries.iter().map(|e| e.n.clone()).collect())
        .unwrap_or_default();
    for name in &names {
        assert!(entries.contains(name), "native watcher missed {name:?} in {entries:?}");
    }
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
#[ignore = "waits 10 s; run with --ignored"]
fn idle_costs_no_coalescer_wakeups() {
    let root = temp_root("idle");
    let mut rng = StdRng::seed_from_u64(9);
    seed_tree(&root, 40, &mut rng);
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let open: Vec<DirReq> = all_dirs(&root).into_iter().map(|rel| DirReq { rel, known_gen: None }).collect();
    hub.expand(ack.sub_id, open);
    probe.watching(&hub);
    probe.settle(&hub, &root);
    let before = hub.wakeups.load(std::sync::atomic::Ordering::Relaxed);
    let msgs = rec.msgs.lock().len();
    std::thread::sleep(Duration::from_secs(10));
    assert_eq!(hub.wakeups.load(std::sync::atomic::Ordering::Relaxed), before, "the coalescer woke with nothing changing");
    assert_eq!(rec.msgs.lock().len(), msgs, "nothing was sent with nothing changing");
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}

// ── Linux: one inotify instance, a budget, a polling fallback (§6.2) ─────────────────────

#[cfg(target_os = "linux")]
fn linux_state(budget: Option<usize>) -> (Arc<FileTreeState>, Arc<super::watch::inotify::SharedInotify>) {
    let shared = match budget {
        Some(b) => super::watch::inotify::SharedInotify::with_budget(b),
        None => super::watch::inotify::SharedInotify::new(),
    };
    let ctx = super::watch::WatchContext { inotify: Some(shared.clone()), ..Default::default() };
    (FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, Some(ctx)), shared)
}

#[cfg(target_os = "linux")]
#[test]
fn a_low_inotify_budget_falls_back_to_polling_and_says_so() {
    let root = temp_root("budget-low");
    for i in 0..8 {
        std::fs::create_dir_all(root.join(format!("d{i}"))).unwrap();
    }
    let (ft, shared) = linux_state(Some(3));
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    hub.expand(ack.sub_id, (0..8).map(|i| DirReq { rel: format!("d{i}"), known_gen: None }).collect());
    probe.watching(&hub);
    let (watched, polled) = hub.watched_and_polled();
    assert!(shared.watch_count() <= 3, "{} watches over a budget of 3", shared.watch_count());
    assert!(watched <= 3 && polled >= 6, "watched {watched}, polled {polled}");
    assert!(wait_for(&rec, HANG, |e| matches!(e, TreeEvent::Health { note: Some(n), watch, .. } if n.contains("polling") && watch == "partial")).is_some());
    // A polled folder still converges, through the poller (the window counts as focused).
    std::fs::write(root.join("d0/new.txt"), "").unwrap();
    assert!(wait_for(&rec, HANG, |e| matches!(e, TreeEvent::Delta { rel, add, .. } if rel == "d0" && add.iter().any(|a| a.n == "new.txt"))).is_some());
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "creates and opens 20k folders; run with --ignored"]
fn watches_stay_within_budget_after_expanding_20k_folders() {
    let root = temp_root("budget-20k");
    for i in 0..20_000 {
        std::fs::create_dir_all(root.join(format!("g{}", i / 200)).join(format!("d{i}"))).unwrap();
    }
    let (ft, shared) = linux_state(None);
    let probe = Probe::new(&ft);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let all: Vec<DirReq> = all_dirs(&root).into_iter().map(|rel| DirReq { rel, known_gen: None }).collect();
    hub.expand(ack.sub_id, all);
    probe.watching(&hub);
    assert!(shared.watch_count() <= shared.budget(), "{} watches over a budget of {}", shared.watch_count(), shared.budget());
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}
