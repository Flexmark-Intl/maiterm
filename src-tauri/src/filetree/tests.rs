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
            TreeEvent::Health { .. } | TreeEvent::RootGone | TreeEvent::Git(_) | TreeEvent::SearchReady => {}
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

/// One random change: create, delete, rename or move a file or a folder.
fn mutate(root: &Path, rng: &mut StdRng, n: &mut u64) {
    *n += 1;
    let dirs = all_dirs(root);
    let files = all_files(root);
    let pick_dir = |rng: &mut StdRng| dirs[rng.gen_range(0..dirs.len())].clone();
    let at = |rel: &str| if rel.is_empty() { root.to_path_buf() } else { root.join(rel) };
    match rng.gen_range(0..10) {
        0..=3 => {
            let d = pick_dir(rng);
            let _ = std::fs::write(at(&d).join(format!("f{n}.txt")), "x");
        }
        4 => {
            let d = pick_dir(rng);
            let _ = std::fs::create_dir(at(&d).join(format!("d{n}")));
        }
        5 | 6 if !files.is_empty() => {
            let f = &files[rng.gen_range(0..files.len())];
            let _ = std::fs::remove_file(at(f));
        }
        7 if !files.is_empty() => {
            let f = &files[rng.gen_range(0..files.len())];
            let to = pick_dir(rng);
            let _ = std::fs::rename(at(f), at(&to).join(format!("m{n}.txt")));
        }
        8 if dirs.len() > 1 => {
            let d = &dirs[rng.gen_range(1..dirs.len())];
            let parent = at(super::paths::split_parent(d).0);
            let _ = std::fs::rename(at(d), parent.join(format!("r{n}")));
        }
        9 if dirs.len() > 6 => {
            let d = &dirs[rng.gen_range(1..dirs.len())];
            let _ = std::fs::remove_dir_all(at(d));
        }
        _ => {
            let d = pick_dir(rng);
            let _ = std::fs::write(at(&d).join(format!("g{n}.txt")), "y");
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
    let ft = FileTreeState::with(Duration::from_millis(200), cap, None);
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

    let mut n = 0u64;
    let mut paused = false;
    let mut max_seq_unacked = 0u64;
    for step in 0..400 {
        mutate(&root, &mut rng, &mut n);
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

    // Quiesce: the watcher's latency, the coalescer's window, the listings.
    std::thread::sleep(Duration::from_millis(800));
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut quiet_rounds = 0;
    while quiet_rounds < 4 {
        assert!(Instant::now() < deadline, "{variant:?}: never went quiet");
        let got = client.pump(&rec, &hub, sub, true);
        std::thread::sleep(Duration::from_millis(250));
        if got == 0 && client.need_refresh.is_empty() && hub.test_idle() && rec.msgs.lock().is_empty() {
            quiet_rounds += 1;
        } else {
            quiet_rounds = 0;
        }
    }

    let fresh_ignore = Mutex::new(IgnoreStack::for_test(&root, None, None, &[]));
    for rel in &client.expanded {
        let abs = if rel.is_empty() { root.clone() } else { root.join(rel) };
        if !abs.is_dir() {
            continue;
        }
        let held = client.dirs.get(rel).unwrap_or_else(|| panic!("{variant:?}: open folder {rel:?} is not held"));
        let mine = held.1.clone();
        let fresh = list::list_dir(&root, rel, &fresh_ignore).unwrap();
        assert_eq!(mine, fresh.entries, "{variant:?} seed {seed}: {rel:?} drifted from the disk");
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

/// Wait until the hub's watcher, started in the background, is running.
fn wait_watching(hub: &RootHub) {
    let end = Instant::now() + Duration::from_secs(30);
    while hub.watch_pending() {
        assert!(Instant::now() < end, "the watcher never started");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_change_arrives_as_a_delta_from_the_held_generation() {
    let root = temp_root("delta");
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    wait_watching(&ft.hub_of(ack.sub_id).unwrap());
    assert!(wait_for(&rec, Duration::from_secs(5), |e| matches!(e, TreeEvent::Dir { rel, .. } if rel.is_empty())).is_some());
    let held = rec.msgs.lock().iter().find_map(|m| match &m.ev {
        TreeEvent::Dir { rel, gen, .. } if rel.is_empty() => Some(*gen),
        _ => None,
    });
    std::fs::write(root.join("x.txt"), "").unwrap();
    assert!(wait_for(&rec, Duration::from_secs(5), |e| matches!(e, TreeEvent::Delta { rel, from, add, .. }
        if rel.is_empty() && Some(*from) == held && add.iter().any(|a| a.n == "x.txt"))).is_some());
    // Saving a file changes nothing the tree shows: no message at all.
    let before = rec.msgs.lock().len();
    std::fs::write(root.join("x.txt"), "more").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(rec.msgs.lock().len(), before);
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
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![DirReq { rel: "a".into(), known_gen: None }], rec.clone(), vec![]).unwrap();
    let gen_of = |rec: &Recorder, rel: &str| {
        rec.msgs.lock().iter().rev().find_map(|m| match &m.ev {
            TreeEvent::Dir { rel: r, gen, .. } if r == rel => Some(*gen),
            _ => None,
        })
    };
    assert!(wait_for(&rec, Duration::from_secs(5), |e| matches!(e, TreeEvent::Dir { rel, .. } if rel == "a")).is_some());
    let gen = gen_of(&rec, "a").unwrap();
    // Close and come back within the grace period: the folder is answered from memory.
    ft.close(ack.sub_id);
    let rec2 = Arc::new(Recorder::default());
    ft.open("w", &root.to_string_lossy(), vec![DirReq { rel: "a".into(), known_gen: Some(gen) }], rec2.clone(), vec![]).unwrap();
    assert!(wait_for(&rec2, Duration::from_secs(5), |e| matches!(e, TreeEvent::NotModified { rel, gen: g } if rel == "a" && *g == gen)).is_some());
    assert_eq!(ft.hub_count(), 1, "one hub per root, shared");
    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn a_deleted_root_says_root_gone() {
    let root = temp_root("rootgone");
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    // The watcher must be what notices: deleted before it runs, the root is only found gone
    // when it starts, which under a loaded fseventsd is later than this test waits.
    wait_watching(&ft.hub_of(ack.sub_id).unwrap());
    assert!(wait_for(&rec, Duration::from_secs(5), |e| matches!(e, TreeEvent::Dir { .. })).is_some());
    std::fs::remove_dir_all(&root).unwrap();
    assert!(wait_for(&rec, Duration::from_secs(10), |e| matches!(e, TreeEvent::RootGone)).is_some());
}

#[test]
fn a_hub_is_reaped_after_its_grace() {
    let root = temp_root("reap");
    let ft = FileTreeState::with(Duration::from_millis(100), super::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    ft.close(ack.sub_id);
    assert_eq!(ft.hub_count(), 1, "kept through the grace period");
    std::thread::sleep(Duration::from_millis(400));
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
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
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
    let first = wait_for(&rec, Duration::from_secs(10), |e| matches!(e, TreeEvent::Dir { rel, chunk: 0, .. } if rel.is_empty())).unwrap();
    let open_ms = (first - t0).as_secs_f64() * 1000.0;
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let t1 = Instant::now();
    hub.expand(ack.sub_id, vec![DirReq { rel: "big".into(), known_gen: None }]);
    let chunk = wait_for(&rec, Duration::from_secs(10), |e| matches!(e, TreeEvent::Dir { rel, chunk: 0, .. } if rel == "big")).unwrap();
    let big_ms = (chunk - t1).as_secs_f64() * 1000.0;
    let full = wait_for(&rec, Duration::from_secs(10), |e| matches!(e, TreeEvent::Dir { rel, last: true, .. } if rel == "big"));
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

#[test]
#[ignore = "waits 10 s; run with --ignored"]
fn idle_costs_no_coalescer_wakeups() {
    let root = temp_root("idle");
    let mut rng = StdRng::seed_from_u64(9);
    seed_tree(&root, 40, &mut rng);
    let ft = FileTreeState::with(Duration::from_secs(30), super::watch::RAW_CAPACITY, None);
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let open: Vec<DirReq> = all_dirs(&root).into_iter().map(|rel| DirReq { rel, known_gen: None }).collect();
    hub.expand(ack.sub_id, open);
    wait_watching(&hub);
    std::thread::sleep(Duration::from_secs(2));
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
    let ctx = super::watch::WatchContext { inotify: Some(shared.clone()) };
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
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    hub.expand(ack.sub_id, (0..8).map(|i| DirReq { rel: format!("d{i}"), known_gen: None }).collect());
    wait_watching(&hub);
    let (watched, polled) = hub.watched_and_polled();
    assert!(shared.watch_count() <= 3, "{} watches over a budget of 3", shared.watch_count());
    assert!(watched <= 3 && polled >= 6, "watched {watched}, polled {polled}");
    assert!(wait_for(&rec, Duration::from_secs(2), |e| matches!(e, TreeEvent::Health { note: Some(n), watch, .. } if n.contains("polling") && watch == "partial")).is_some());
    // A polled folder still converges, through the poller (the window counts as focused).
    std::fs::write(root.join("d0/new.txt"), "").unwrap();
    assert!(wait_for(&rec, Duration::from_secs(8), |e| matches!(e, TreeEvent::Delta { rel, add, .. } if rel == "d0" && add.iter().any(|a| a.n == "new.txt"))).is_some());
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
    let rec = Arc::new(Recorder::default());
    let ack = ft.open("w", &root.to_string_lossy(), vec![], rec.clone(), vec![]).unwrap();
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let all: Vec<DirReq> = all_dirs(&root).into_iter().map(|rel| DirReq { rel, known_gen: None }).collect();
    hub.expand(ack.sub_id, all);
    wait_watching(&hub);
    assert!(shared.watch_count() <= shared.budget(), "{} watches over a budget of {}", shared.watch_count(), shared.budget());
    ft.close(ack.sub_id);
    std::fs::remove_dir_all(&root).unwrap();
}

