//! The remote helper and its Rust side (docs/file-tree.md §15), run against THIS computer:
//! the helper starts under the local python3 through `sh -c` with exactly the remote command
//! (`exec python3 -u -c '<LOADER>'`), so the loader, the bootstrap and every frame cross real
//! pipes. Each test has a scratch HOME (the remote trash, git's global config), never the real
//! one. `on_a_real_host` runs the same against an ssh host when asked.
//!
//! A test that runs the helper's code here (started as above, or loaded by the harness) is
//! `#[cfg(unix)]`: it makes THIS computer stand in for the ssh host, and a host is always a
//! POSIX one (`/bin/sh`, select on pipes, unix symlinks, roots that are `/`-absolute paths).
//! Windows only ever runs the Rust side against a host elsewhere; what of that needs no host
//! (the registry, exit classification, the 3.6 gate) runs everywhere.
#![cfg_attr(not(unix), allow(dead_code, unused_imports))]

use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::proto::{self, Read};
use super::*;
use crate::filetree::hub::Sink;
use crate::filetree::types::{Envelope, TreeEvent, FLAG_HIDDEN, FLAG_IGNORED};
use crate::filetree::FileTreeState;

fn python3() -> bool {
    Command::new("python3").arg("--version").stdout(Stdio::null()).stderr(Stdio::null()).status().is_ok_and(|s| s.success())
}

fn scratch(name: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("ft-remote-{name}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&t).unwrap();
    crate::share::canon(&t)
}

/// The environment a test helper sees: a scratch HOME and config, the caller's PATH.
fn env(c: &mut Command, home: &Path) {
    c.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("ZDOTDIR", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("GIT_CONFIG_NOSYSTEM", "1");
}

/// The helper and the fallback shell, started here instead of over ssh.
struct Local {
    home: PathBuf,
    /// The program in place of python3: a missing one stands for a host without it.
    python: &'static str,
}

impl Transport for Local {
    fn helper(&self, _t: &Target) -> Command {
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(proto::remote_command().replacen("python3", self.python, 1));
        env(&mut c, &self.home);
        c
    }

    fn shell(&self, _t: &Target) -> Command {
        let mut c = Command::new("/bin/sh");
        c.arg("-s");
        env(&mut c, &self.home);
        c
    }
}

fn fast() -> Timing {
    Timing {
        ping_every: Duration::from_secs(5),
        ping_timeout: Duration::from_secs(5),
        ready_timeout: Duration::from_secs(20),
        backoff_first: Duration::from_millis(100),
        backoff_cap: Duration::from_millis(500),
        grace: Duration::from_millis(500),
        poll_every: Duration::from_millis(150),
        limited_poll_every: Duration::from_millis(200),
        put_chunk: Duration::from_secs(60),
    }
}

fn remotes(home: &Path, python: &'static str) -> Remotes {
    let r = Remotes::default();
    r.set_transport(Arc::new(Local { home: home.to_path_buf(), python }), fast());
    r
}

fn ok(r: Reply) -> proto::Frame {
    match r {
        Ok(f) => f,
        Err(e) => panic!("the helper refused: {e}"),
    }
}

fn code(r: Reply) -> String {
    match r {
        Err(CallError::Op { code, .. }) => code,
        Err(e) => panic!("not an op error: {e}"),
        Ok(f) => panic!("answered: {:?}", f.header),
    }
}

fn git(dir: &Path, home: &Path, args: &[&str]) {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "init.defaultBranch=main"]).args(args);
    env(&mut c, home);
    let out = c.output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

const T: Duration = Duration::from_secs(20);

fn shortname(h: &str) -> String {
    h.split('.').next().unwrap_or(h).to_ascii_lowercase()
}

#[test]
#[cfg(unix)]
fn frames_carry_raw_bytes_both_ways() {
    if !python3() {
        return;
    }
    let home = scratch("frames");
    let root = scratch("frames-root");
    let rs = remotes(&home, "python3");
    let c = rs.conn("me@here", None);
    // Held as a root holds it: a helper nobody leases ends after the grace.
    let _lease = Lease::new(c.clone());
    let hello = ok(c.call("hello", json!({}), &[], T));
    assert_eq!(hello.header.get("protocol").and_then(Value::as_u64), Some(1));
    let r = root.to_string_lossy().to_string();
    // Every byte value, a newline and a brace: nothing in a payload is read as framing.
    let bytes: Vec<u8> = (0..=255u8).chain(*b"\n{\"id\":1}\n").cycle().take(300_000).collect();
    ok(c.call("put", json!({ "root": r, "rel": "bin.dat", "mode": 0o640 }), &bytes, T));
    let back = ok(c.call("read", json!({ "root": r, "rel": "bin.dat", "cap": 1 << 20 }), &[], T));
    assert_eq!(back.payload, bytes);
    assert_eq!(code(c.call("read", json!({ "root": r, "rel": "bin.dat", "cap": 10 }), &[], T)), "too_large");
    // Never clobbered: a put onto an existing name is refused.
    assert_eq!(code(c.call("put", json!({ "root": r, "rel": "bin.dat" }), b"x", T)), "exists");
    // A write keeps the mode and refuses when the file moved on since it was read.
    let st = ok(c.call("stat", json!({ "root": r, "rel": "bin.dat" }), &[], T));
    let expect = json!({ "size": st.header["size"], "mtime_ms": st.header["mtime_ms"] });
    ok(c.call("write", json!({ "root": r, "rel": "bin.dat", "expect": expect }), b"new", T));
    assert_eq!(std::fs::read(root.join("bin.dat")).unwrap(), b"new");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(root.join("bin.dat")).unwrap().permissions().mode() & 0o777, 0o640);
    }
    assert_eq!(code(c.call("write", json!({ "root": r, "rel": "bin.dat", "expect": expect }), b"again", T)), "changed");
    // Many requests in flight at once come back to their own callers.
    let handles: Vec<_> = (0..24)
        .map(|i| {
            let c = c.clone();
            let r = r.clone();
            std::thread::spawn(move || {
                let name = format!("f{i}");
                ok(c.call("put", json!({ "root": r, "rel": name }), name.as_bytes(), T));
                ok(c.call("read", json!({ "root": r, "rel": name }), &[], T)).payload == name.as_bytes()
            })
        })
        .collect();
    assert!(handles.into_iter().all(|h| h.join().unwrap()));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[cfg(unix)]
fn listings_flag_ignored_and_hidden_entries_like_the_local_stack() {
    if !python3() {
        return;
    }
    // The fixtures of ignore_cache.rs's tests: nested .gitignores with a negation,
    // info/exclude, a nested repository, a directory-only pattern, the global excludes file.
    let home = scratch("ignore");
    let t = scratch("ignore-root");
    for d in [".git/info", "src/gen", "node_modules/x", "vendor/inner/.git"] {
        std::fs::create_dir_all(t.join(d)).unwrap();
    }
    std::fs::write(t.join(".gitignore"), "node_modules/\n*.log\nbuild\n").unwrap();
    std::fs::write(t.join("src/.gitignore"), "gen/\n!keep.log\n").unwrap();
    std::fs::write(t.join(".git/info/exclude"), "secret.txt\n").unwrap();
    std::fs::write(t.join("vendor/inner/.gitignore"), "local.out\n").unwrap();
    std::fs::create_dir_all(home.join(".config/git")).unwrap();
    std::fs::write(home.join(".config/git/ignore"), "*.swp\n").unwrap();
    for f in ["debug.log", "secret.txt", "README.md", "src/debug.log", "src/keep.log", "src/a.swp", "vendor/inner/local.out", "vendor/inner/x.log"] {
        std::fs::write(t.join(f), "").unwrap();
    }
    std::os::unix::fs::symlink(&t, t.join("src/up")).unwrap();
    let rs = remotes(&home, "python3");
    let c = rs.conn("me@here", None);
    // Held as a root holds it: a helper nobody leases ends after the grace.
    let _lease = Lease::new(c.clone());
    let r = t.to_string_lossy().to_string();
    ok(c.call("root", json!({ "root": r, "excludes": ["*.tmp"] }), &[], T));
    std::fs::write(t.join("x.tmp"), "").unwrap();
    let list = |rel: &str| {
        let f = ok(c.call("list", json!({ "root": r, "rel": rel }), &[], T));
        proto::parse_listing(&f.payload).into_iter().map(|e| (e.n.clone(), e)).collect::<std::collections::HashMap<_, _>>()
    };
    let top = list("");
    let ignored = |m: &std::collections::HashMap<String, crate::filetree::types::Entry>, n: &str| m[n].f & FLAG_IGNORED != 0;
    assert!(!top.contains_key(".git"), "never listed");
    assert!(ignored(&top, "node_modules") && ignored(&top, "debug.log") && ignored(&top, "secret.txt") && ignored(&top, "x.tmp"));
    assert!(!ignored(&top, "README.md") && !ignored(&top, "src"));
    assert_eq!(top[".gitignore"].f, FLAG_HIDDEN);
    let src = list("src");
    assert!(ignored(&src, "debug.log"), "a root pattern applies below");
    assert!(!ignored(&src, "keep.log"), "negated by the nearer .gitignore");
    assert!(ignored(&src, "gen") && ignored(&src, "a.swp"));
    assert_eq!((src["up"].k, src["up"].f & 4), ("L", 4), "a link back up is a loop");
    assert!(ignored(&list("node_modules"), "x"), "under an ignored folder");
    let inner = list("vendor/inner");
    assert!(ignored(&inner, "local.out"));
    assert!(!ignored(&inner, "x.log"), "the outer repo's *.log stops at a repository of its own");
    // Outside a repository, a stray .gitignore hides nothing.
    let plain = scratch("ignore-plain");
    std::fs::write(plain.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(plain.join("a.log"), "").unwrap();
    let f = ok(c.call("list", json!({ "root": plain.to_string_lossy(), "rel": "" }), &[], T));
    assert!(proto::parse_listing(&f.payload).iter().all(|e| e.f & FLAG_IGNORED == 0));
    for d in [&home, &t, &plain] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
#[cfg(unix)]
fn every_rel_is_contained_like_ops_rs() {
    if !python3() {
        return;
    }
    let home = scratch("contain");
    let t = scratch("contain-root");
    let outside = scratch("contain-outside");
    std::fs::write(outside.join("keep.txt"), "x").unwrap();
    std::fs::create_dir_all(t.join("in")).unwrap();
    std::os::unix::fs::symlink(&outside, t.join("out")).unwrap();
    std::os::unix::fs::symlink(t.join("nowhere"), t.join("dangling")).unwrap();
    let rs = remotes(&home, "python3");
    let c = rs.conn("me@here", None);
    // Held as a root holds it: a helper nobody leases ends after the grace.
    let _lease = Lease::new(c.clone());
    let r = t.to_string_lossy().to_string();
    for rel in ["../x", "/etc", "in/../..", "in//x", "./in", "out/keep.txt", "dangling/x"] {
        assert_eq!(code(c.call("list", json!({ "root": r, "rel": rel }), &[], T)), "outside", "{rel}");
        assert_eq!(code(c.call("read", json!({ "root": r, "rel": rel }), &[], T)), "outside", "{rel}");
    }
    let op = |file: Value| c.call("fileop", json!({ "root": r, "file": file, "style": "linux" }), &[], T);
    assert_eq!(code(op(json!({ "kind": "trash", "src": [""] }))), "outside", "never the root itself");
    assert_eq!(code(op(json!({ "kind": "create_file", "dest": "out", "name": "x" }))), "outside");
    assert_eq!(code(op(json!({ "kind": "move", "src": ["in"], "dest": "out" }))), "outside");
    // The link itself is an item like any other: trashed, never followed.
    let done = ok(op(json!({ "kind": "trash", "src": ["out"] })));
    assert_eq!(done.header["done"][0]["from"], "out");
    assert!(outside.join("keep.txt").exists());
    // A folder can't go into itself.
    let f = ok(op(json!({ "kind": "move", "src": ["in"], "dest": "in" })));
    assert_eq!(f.header["failed"][0]["code"], "inside_itself");
    for d in [&home, &t, &outside] {
        let _ = std::fs::remove_dir_all(d);
    }
}

#[test]
#[cfg(unix)]
fn file_ops_answer_like_ops_rs_and_undo_through_the_remote_trash() {
    if !python3() {
        return;
    }
    let home = scratch("ops");
    let t = scratch("ops-root");
    std::fs::write(t.join("a.ts"), "a").unwrap();
    let rs = remotes(&home, "python3");
    let c = rs.conn("me@here", None);
    // Held as a root holds it: a helper nobody leases ends after the grace.
    let _lease = Lease::new(c.clone());
    let r = t.to_string_lossy().to_string();
    let op = |file: Value, style: &str| ok(c.call("fileop", json!({ "root": r, "file": file, "style": style }), &[], T)).header;
    let h = op(json!({ "kind": "create_file", "name": "x/y/z.ts" }), "linux");
    assert_eq!(h["done"][0]["from"], "x/y/z.ts");
    assert_eq!(h["undo"], json!({ "kind": "trash", "src": ["x"] }), "undo takes the outermost folder made");
    let h = op(json!({ "kind": "create_file", "name": "a.ts" }), "linux");
    assert_eq!(h["failed"][0]["code"], "exists", "never clobbered");
    // Keep-both names follow the client's file manager.
    let h = op(json!({ "kind": "duplicate", "src": ["a.ts"] }), "mac");
    assert_eq!(h["done"][0]["to"], "a copy.ts");
    let h = op(json!({ "kind": "copy", "src": ["a.ts"], "dest": "", "conflict": "keep_both" }), "windows");
    assert_eq!(h["done"][0]["to"], "a - Copy.ts");
    let h = op(json!({ "kind": "rename", "src": ["a.ts"], "name": "b.ts" }), "linux");
    assert_eq!(h["undo"], json!({ "kind": "rename", "src": ["b.ts"], "name": "a.ts" }));
    let h = op(json!({ "kind": "move", "src": ["b.ts"], "dest": "x" }), "linux");
    assert_eq!(h["undo"], json!({ "kind": "move", "src": ["x/b.ts"], "to": ["b.ts"] }));
    let h = op(json!({ "kind": "rename", "src": ["x/b.ts"], "name": "y" }), "linux");
    assert_eq!(h["failed"][0]["code"], "exists");
    // Trash into ~/.maiterm/trash on the host, then put back: Undo on every client.
    let h = op(json!({ "kind": "trash", "src": ["x", "x/y"] }), "linux");
    assert_eq!(h["done"].as_array().unwrap().len(), 1, "an item inside another goes with it");
    assert!(!t.join("x").exists());
    assert!(home.join(".maiterm/trash").read_dir().unwrap().count() == 1);
    let undo = h["undo"].clone();
    assert_eq!(undo["kind"], "restore");
    let h = op(undo, "linux");
    assert_eq!(h["done"][0]["from"], "x");
    assert!(t.join("x/y/z.ts").exists() && t.join("x/b.ts").exists());
    assert_eq!(home.join(".maiterm/trash").read_dir().unwrap().count(), 0, "an emptied trash entry goes");
    let h = op(json!({ "kind": "restore", "src": ["x"], "trashed_at": 0 }), "linux");
    assert_eq!(h["failed"][0]["code"], "exists");
    let h = op(json!({ "kind": "delete", "src": ["x"] }), "linux");
    assert_eq!(h["done"][0]["from"], "x");
    assert!(!t.join("x").exists());
    let tally = ok(c.call("tally", json!({ "root": r, "rels": ["a copy.ts", "a - Copy.ts"] }), &[], T));
    assert_eq!((tally.header["files"].as_u64(), tally.header["dirs"].as_u64()), (Some(2), Some(0)));
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
#[cfg(unix)]
fn git_runs_on_the_host_with_the_same_parser() {
    if !python3() {
        return;
    }
    let home = scratch("git");
    let t = scratch("git-root");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::write(t.join("src/a.ts"), "one\n").unwrap();
    git(&t, &home, &["init", "-q"]);
    git(&t, &home, &["add", "."]);
    git(&t, &home, &["commit", "-qm", "first"]);
    std::fs::write(t.join("src/a.ts"), "one\ntwo\n").unwrap();
    std::fs::write(t.join("new.txt"), "").unwrap();
    let rs = remotes(&home, "python3");
    let root = RemoteRoot::open(rs.lease("me@here", None), &t.to_string_lossy(), Vec::new()).unwrap();
    assert_eq!(root.repo.as_ref().map(|r| r.toplevel_rel.as_str()), Some(""));
    let core = crate::filetree::git::GitCore::new_with(
        std::sync::Weak::new(),
        &crate::filetree::git::GitConfig::default(),
        Path::new(&root.root),
        Path::new(&root.root),
        false,
        Arc::new(crate::filetree::Slots::new(2)),
        Some(root.clone()),
    );
    let st = core.run_status().unwrap();
    let mut got: Vec<(String, &str)> = st.entries.iter().map(|e| (e.path.clone(), e.letter)).collect();
    got.sort();
    assert_eq!(got, vec![("new.txt".to_string(), "?"), ("src/a.ts".to_string(), "M")]);
    assert_eq!(st.branch.map(|b| b.head), Some("main".to_string()));
    let p = core.apply(core.epoch(), Ok(core.run_status().unwrap()), Duration::ZERO).unwrap();
    assert_eq!(p.diff.dirs, vec![("src".to_string(), "M".to_string())]);
    assert_eq!(core.numstat("src/a.ts", (8, None)), Some((1, 0)));
    assert_eq!(core.diff_base("src/a.ts", "HEAD").unwrap().as_deref(), Some("one\n"));
    // Only read-only subcommands run there.
    assert!(matches!(root.conn.call("git", json!({ "root": root.root, "args": ["commit", "-m", "x"] }), &[], T), Err(CallError::Op { ref code, .. }) if code == "denied"));
    // Find's list comes from ls-files there.
    let mut files = root.files().unwrap();
    files.sort();
    assert_eq!(files, vec!["new.txt".to_string(), "src/a.ts".to_string()]);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&t);
}

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
    fn wait(&self, what: &str, f: impl Fn(&TreeEvent) -> bool) -> TreeEvent {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            if let Some(e) = self.msgs.lock().iter().find(|m| f(&m.ev)) {
                return e.ev.clone();
            }
            assert!(Instant::now() < deadline, "never got {what}: {:?}", self.msgs.lock().iter().map(|m| &m.ev).collect::<Vec<_>>());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

fn dir_names(ev: &TreeEvent) -> Vec<String> {
    match ev {
        TreeEvent::Dir { entries, .. } => entries.iter().map(|e| e.n.clone()).collect(),
        _ => Vec::new(),
    }
}

#[test]
#[cfg(unix)]
fn a_remote_place_serves_the_same_tree_events() {
    if !python3() {
        return;
    }
    let home = scratch("hub");
    let t = scratch("hub-root");
    std::fs::create_dir_all(t.join("src")).unwrap();
    std::fs::write(t.join("src/b.rs"), "fn main() {}\n").unwrap();
    std::fs::write(t.join("a.txt"), "a\n").unwrap();
    std::fs::write(t.join(".gitignore"), "*.log\n").unwrap();
    std::fs::write(t.join("x.log"), "").unwrap();
    git(&t, &home, &["init", "-q"]);
    git(&t, &home, &["add", "."]);
    git(&t, &home, &["commit", "-qm", "first"]);
    std::fs::write(t.join("a.txt"), "a\nb\n").unwrap();

    let ft = FileTreeState::with(Duration::from_millis(200), crate::filetree::watch::RAW_CAPACITY, None);
    ft.remotes.set_transport(Arc::new(Local { home: home.clone(), python: "python3" }), fast());
    let rec = Arc::new(Recorder::default());
    let place = format!("me@here:{}", t.display());
    let ack = ft.open_with("w", &place, vec![], rec.clone(), vec![], None).unwrap();
    assert!(ack.caps.remote && ack.caps.writable && ack.caps.git);
    assert_eq!((ack.root_native.as_str(), ack.sep.as_str(), ack.nfc_names), (t.to_str().unwrap(), "/", false));
    // The root switcher's passive resolve asks only a host whose helper is up.
    assert!(ft.remotes.is_live("me@here") && !ft.remotes.is_live("me@elsewhere"));
    // The ack names the host (the wrong-host guard is off for a subscription without it).
    let here = String::from_utf8(Command::new("hostname").output().unwrap().stdout).unwrap();
    assert_eq!(ack.remote_hostname.as_deref().map(shortname), Some(shortname(here.trim())));
    // Also when the `hello` sent as the helper came up hasn't answered yet, as happens when the
    // open that started it gets its `root` back first: the open asks for it.
    let conn = ft.hub_of(ack.sub_id).unwrap().remote.as_ref().unwrap().conn.clone();
    *conn.hello.lock() = None;
    let second = RemoteRoot::open(ft.remotes.lease("me@here", None), &t.join("src").to_string_lossy(), Vec::new()).unwrap();
    assert!(second.conn.hello().is_some_and(|h| shortname(&h.hostname) == shortname(here.trim())));
    assert!(second.inotify.load(Ordering::SeqCst) == conn.hello().unwrap().inotify);
    drop(second);
    let root = rec.wait("the root's listing", |e| matches!(e, TreeEvent::Dir { rel, last: true, .. } if rel.is_empty()));
    assert_eq!(dir_names(&root), vec!["src", ".gitignore", "a.txt", "x.log"], "sorted here, folders first");
    if let TreeEvent::Dir { entries, .. } = &root {
        assert_eq!(entries.iter().find(|e| e.n == "x.log").unwrap().f & FLAG_IGNORED, FLAG_IGNORED);
    }
    rec.wait("git decorations", |e| matches!(e, TreeEvent::Git(g) if g.files.contains(&("a.txt".to_string(), "M".to_string()))));
    let hub = ft.hub_of(ack.sub_id).unwrap();
    hub.expand(ack.sub_id, vec![crate::filetree::types::DirReq { rel: "src".into(), known_gen: None }]);
    rec.wait("src's listing", |e| matches!(e, TreeEvent::Dir { rel, .. } if rel == "src"));

    // A change on disk arrives as a Delta: by inotify on Linux, by the poll (no inotify here).
    std::fs::write(t.join("src/new.rs"), "").unwrap();
    rec.wait("a delta adding new.rs", |e| matches!(e, TreeEvent::Delta { rel, add, .. } if rel == "src" && add.iter().any(|a| a.n == "new.rs")));

    // Hover, locate and an agent's path on that host.
    let st = hub.stat("a.txt").unwrap();
    assert_eq!((st.size, st.numstat), (4, Some((1, 0))));
    let abs = format!("{}/src/b.rs", t.display());
    assert_eq!(hub.locate(&[abs.clone(), "/elsewhere/x".into()]), vec![Some("src/b.rs".to_string()), None]);
    assert_eq!(hub.locate_agent_path(&format!("me@here:{abs}")).as_deref(), Some("src/b.rs"));
    assert_eq!(hub.locate_agent_path(&abs), None, "a local path is never on the host");
    assert_eq!(hub.locate_agent_path(&format!("other@host:{abs}")), None);
    assert!(!hub.covers(Path::new(&abs)));

    // Find, from the helper's ls-files.
    let first = hub.search(ack.sub_id, "brs", 10, 1, false);
    assert!(first.indexing || !first.items.is_empty());
    if first.items.is_empty() {
        rec.wait("the index", |e| matches!(e, TreeEvent::SearchReady));
    }
    let hits = hub.search(ack.sub_id, "brs", 10, 2, false);
    assert_eq!(hits.items.first().map(|h| h.rel.as_str()), Some("src/b.rs"));

    // File operations, the ticket, and the live-cwd guard on that host.
    let none = Vec::new;
    let op = |file: FileOp, holders: Vec<(String, String, String)>| ft.op_with(ack.sub_id, &file, &none, &move |h: &str| if h == "me@here" { holders.clone() } else { Vec::new() });
    let made = op(FileOp { kind: "create_file".into(), name: Some("src/c.rs".into()), ..Default::default() }, vec![]).unwrap();
    assert_eq!(made.done[0].from, "src/c.rs");
    rec.wait("the new file listed with the op's answer", |e| matches!(e, TreeEvent::Delta { add, .. } if add.iter().any(|a| a.n == "c.rs")));
    let agent_in_src = vec![("tab1".to_string(), "claude".to_string(), format!("{}/src", t.display()))];
    let refused = op(FileOp { kind: "rename".into(), src: vec!["src".into()], name: Some("lib".into()), ..Default::default() }, agent_in_src).unwrap();
    assert_eq!(refused.needs.map(|n| (n.code, n.tabs)), Some(("live_cwd".to_string(), vec!["tab1".to_string()])));
    let trashed = op(FileOp { kind: "trash".into(), src: vec!["src/c.rs".into()], ..Default::default() }, vec![]).unwrap();
    let undo = trashed.undo.expect("remote trash has an undo on every client");
    assert_eq!(undo.kind, "restore");
    assert_eq!(op(undo, vec![]).unwrap().done.len(), 1);
    assert!(t.join("src/c.rs").exists());
    let no_ticket = op(FileOp { kind: "delete".into(), src: vec!["src/c.rs".into()], ..Default::default() }, vec![]);
    assert!(no_ticket.is_err(), "a permanent delete needs a ticket");
    let ticket = ft.prepare_delete(ack.sub_id, &["src/c.rs".to_string()]).unwrap();
    assert_eq!((ticket.files, ticket.dirs), (1, 0));
    let gone = op(FileOp { kind: "delete".into(), src: vec!["src/c.rs".into()], token: Some(ticket.token), ..Default::default() }, vec![]).unwrap();
    assert_eq!(gone.done.len(), 1);
    assert!(!t.join("src/c.rs").exists());
    // An OS drop from this computer lands there, never replacing anything.
    let drop_dir = scratch("hub-drop");
    std::fs::create_dir_all(drop_dir.join("pkg/inner")).unwrap();
    std::fs::write(drop_dir.join("pkg/inner/f.txt"), "hello").unwrap();
    std::os::unix::fs::symlink("inner/f.txt", drop_dir.join("pkg/link")).unwrap();
    let src = drop_dir.join("pkg").to_string_lossy().to_string();
    let imported = op(FileOp { kind: "import".into(), src: vec![src.clone()], dest: Some("src".into()), ..Default::default() }, vec![]).unwrap();
    assert_eq!(imported.done[0].to.as_deref(), Some("src/pkg"));
    assert_eq!(std::fs::read_to_string(t.join("src/pkg/inner/f.txt")).unwrap(), "hello");
    assert_eq!(std::fs::read_link(t.join("src/pkg/link")).unwrap(), Path::new("inner/f.txt"));
    let again = op(FileOp { kind: "import".into(), src: vec![src], dest: Some("src".into()), ..Default::default() }, vec![]).unwrap();
    assert_eq!(again.failed[0].code, "exists");

    // The view goes; the hub after its grace, then the host's helper after its own.
    ft.close(ack.sub_id);
    drop(hub);
    let deadline = Instant::now() + Duration::from_secs(10);
    while ft.remotes.count() > 0 {
        assert!(Instant::now() < deadline, "the helper outlived its grace");
        std::thread::sleep(Duration::from_millis(50));
    }
    for d in [&home, &t, &drop_dir] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// The helper kept each root's repository for its whole life, so `git init` in an open remote
/// root never decorated, not even after reopening it once the hub's grace was over.
#[test]
#[cfg(unix)]
fn git_init_on_the_host_reopens_the_root_with_its_repository() {
    if !python3() {
        return;
    }
    let home = scratch("gitinit");
    let t = scratch("gitinit-root");
    std::fs::write(t.join("a.txt"), "a\n").unwrap();
    let ft = FileTreeState::with(Duration::from_secs(30), crate::filetree::watch::RAW_CAPACITY, None);
    ft.remotes.set_transport(Arc::new(Local { home: home.clone(), python: "python3" }), fast());
    let place = format!("me@here:{}", t.display());
    let rec = Arc::new(Recorder::default());
    let ack = ft.open_with("w", &place, vec![], rec.clone(), vec![], None).unwrap();
    assert!(ack.repo.is_none() && !ack.caps.git);
    rec.wait("the root", |e| matches!(e, TreeEvent::Dir { rel, .. } if rel.is_empty()));
    // Seen live: inotify's `.git` in the root on Linux, the root's poll elsewhere.
    git(&t, &home, &["init", "-q"]);
    rec.wait("a reopen", |e| matches!(e, TreeEvent::Reopen));
    let rec2 = Arc::new(Recorder::default());
    let again = ft.open_with("w", &place, vec![], rec2.clone(), vec![], None).unwrap();
    ft.close(ack.sub_id);
    assert!(again.repo.is_some() && again.caps.git, "the new hub decorates: {:?}", again.repo);
    // Unseen (nobody subscribed, the hub in its grace): the reopen asks the host.
    ft.close(again.sub_id);
    std::fs::remove_dir_all(t.join(".git")).unwrap();
    let plain = ft.open_with("w", &place, vec![], rec2, vec![], None).unwrap();
    assert!(plain.repo.is_none(), "the removed repository is gone from the new hub");
    ft.close(plain.sub_id);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&t);
}

/// Find on a remote root whose file list couldn't be had: it says why rather than answering "no
/// matches" from an empty index taken as valid, and the next query after a while tries again.
#[test]
#[cfg(unix)]
fn a_find_index_that_could_not_be_listed_says_so_and_is_tried_again() {
    if !python3() {
        return;
    }
    let home = scratch("findfail");
    let t = scratch("findfail-root");
    std::fs::write(t.join("alpha.txt"), "").unwrap();
    let ft = FileTreeState::with(Duration::from_millis(200), crate::filetree::watch::RAW_CAPACITY, None);
    ft.remotes.set_transport(Arc::new(Local { home: home.clone(), python: "python3" }), fast());
    let rec = Arc::new(Recorder::default());
    let ack = ft.open_with("w", &format!("me@here:{}", t.display()), vec![], rec.clone(), vec![], None).unwrap();
    rec.wait("the root", |e| matches!(e, TreeEvent::Dir { .. }));
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let mut seq = 0;
    let mut settled = |want: &dyn Fn(&crate::filetree::types::SearchResult) -> bool| {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            seq += 1;
            let r = hub.search(ack.sub_id, "alpha", 10, seq, false);
            if !r.indexing && want(&r) {
                return r;
            }
            assert!(Instant::now() < deadline, "never settled: {r:?}");
            std::thread::sleep(Duration::from_millis(20));
        }
    };
    // The root goes away on the host: its `files` fails.
    let away = t.with_extension("away");
    std::fs::rename(&t, &away).unwrap();
    let r = settled(&|r| r.unavailable.is_some());
    assert!(r.items.is_empty());
    assert!(r.unavailable.as_deref().is_some_and(|u| u.contains("Find couldn't list the files on")), "{r:?}");
    // Back in place: tried again once the wait is over, and found.
    std::fs::rename(&away, &t).unwrap();
    hub.search.retry_failed_ms.store(0, Ordering::SeqCst);
    let r = settled(&|r| !r.items.is_empty());
    assert_eq!(r.items[0].rel, "alpha.txt");
    assert_eq!(r.unavailable, None);
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
#[cfg(unix)]
fn a_dead_helper_comes_back_and_its_roots_reattach() {
    if !python3() {
        return;
    }
    let home = scratch("revive");
    let t = scratch("revive-root");
    let ft = FileTreeState::with(Duration::from_millis(200), crate::filetree::watch::RAW_CAPACITY, None);
    ft.remotes.set_transport(Arc::new(Local { home: home.clone(), python: "python3" }), fast());
    let rec = Arc::new(Recorder::default());
    let ack = ft.open_with("w", &format!("me@here:{}", t.display()), vec![], rec.clone(), vec![], None).unwrap();
    rec.wait("the root", |e| matches!(e, TreeEvent::Dir { .. }));
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let conn = hub.remote.as_ref().unwrap().conn.clone();
    for round in 0..3 {
        conn.kill_current();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !matches!(conn.status(), Status::Down(_)) && conn.status() != Status::Connecting {
            if conn.status() == Status::Live && Instant::now() > deadline {
                panic!("round {round}: never went down");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        while conn.status() != Status::Live {
            assert!(Instant::now() < deadline, "round {round}: never came back ({:?})", conn.status());
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    // What changed while it was gone is caught by the reattach's re-listing.
    std::fs::write(t.join("while-away.txt"), "").unwrap();
    conn.kill_current();
    rec.wait("the change made while it was down", |e| matches!(e, TreeEvent::Delta { add, .. } if add.iter().any(|a| a.n == "while-away.txt")));
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
#[cfg(unix)]
fn a_host_without_python3_is_listed_read_only_by_sh() {
    let home = scratch("nopy");
    let t = scratch("nopy-root");
    std::fs::create_dir_all(t.join("dir")).unwrap();
    std::fs::write(t.join("we'ird name\nnewline"), "").unwrap();
    std::fs::write(t.join(".env"), "").unwrap();
    let ft = FileTreeState::with(Duration::from_millis(200), crate::filetree::watch::RAW_CAPACITY, None);
    ft.remotes.set_transport(Arc::new(Local { home: home.clone(), python: "python3-not-installed-here" }), fast());
    let rec = Arc::new(Recorder::default());
    let ack = ft.open_with("w", &format!("me@here:{}", t.display()), vec![], rec.clone(), vec![], None).unwrap();
    assert!(!ack.caps.writable && !ack.caps.trash && !ack.caps.git);
    let root = rec.wait("the root by sh", |e| matches!(e, TreeEvent::Dir { last: true, .. }));
    assert_eq!(dir_names(&root), vec!["dir", ".env", "we'ird name\nnewline"]);
    rec.wait("the limited health", |e| matches!(e, TreeEvent::Health { note: Some(n), .. } if n.contains("no python3")));
    // Polled while visible: a change shows up without the helper.
    std::fs::write(t.join("later.txt"), "").unwrap();
    rec.wait("a delta by polling", |e| matches!(e, TreeEvent::Delta { add, .. } if add.iter().any(|a| a.n == "later.txt")));
    let r = ft.op_with(ack.sub_id, &FileOp { kind: "create_file".into(), name: Some("x".into()), ..Default::default() }, &Vec::new, &|_| Vec::new());
    assert!(r.unwrap_err().contains("read-only"));
    // The resolver's existence check works there too.
    let probed = ft.probe_remote("me@here", None, &[t.join("dir").to_string_lossy().into(), "/no/such/dir".into()]).unwrap();
    assert!(probed[0].real.is_some() && probed[1].real.is_none());
    ft.close(ack.sub_id);
    let _ = std::fs::remove_dir_all(&home);
    let _ = std::fs::remove_dir_all(&t);
}

#[test]
fn startup_failures_are_told_apart() {
    assert_eq!(classify_exit(Some(127), b"sh: python3: not found\n", false), Status::Limited);
    assert_eq!(classify_exit(Some(1), b"python3: Command not found.\n", false), Status::Limited, "csh says it this way");
    assert_eq!(classify_exit(Some(1), b"  File \"<string>\", line 1\nSyntaxError: invalid syntax\n", false), Status::Limited);
    assert!(matches!(classify_exit(Some(255), b"ews@nova: Permission denied (publickey).\n", false), Status::NeedsKey(_)));
    assert!(matches!(classify_exit(Some(255), b"ssh: connect to host nova port 22: Connection refused\n", false), Status::Down(ref r) if r.contains("Connection refused")));
    // Once it was running, any end is a drop to come back from.
    assert!(matches!(classify_exit(Some(127), b"", true), Status::Down(_)));
}

/// What `ssh host '<remote command>'` does: the login shell runs the command line. Each shell
/// installed here must hand the loader to python3 intact and start the helper.
#[test]
#[cfg(unix)]
fn the_loader_starts_the_helper_under_every_login_shell() {
    if !python3() {
        return;
    }
    let home = scratch("shells");
    let mut tried = Vec::new();
    for sh in ["/bin/sh", "/bin/bash", "/bin/zsh", "/bin/dash", "/bin/ksh", "/bin/csh", "/bin/tcsh", "/opt/homebrew/bin/fish", "/usr/local/bin/fish", "/usr/bin/fish"] {
        if !Path::new(sh).exists() {
            continue;
        }
        tried.push(sh);
        let mut c = Command::new(sh);
        c.arg("-c").arg(proto::remote_command()).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        env(&mut c, &home);
        let mut child = c.spawn().unwrap();
        let mut stdin = child.stdin.take().unwrap();
        stdin.write_all(&proto::bootstrap()).unwrap();
        let mut h = serde_json::Map::new();
        h.insert("id".into(), json!(1));
        h.insert("op".into(), json!("ping"));
        stdin.write_all(&proto::encode(&h, &[])).unwrap();
        stdin.flush().unwrap();
        let mut r = BufReader::new(child.stdout.take().unwrap());
        let mut answered = false;
        for _ in 0..4 {
            match proto::read_frame(&mut r).unwrap() {
                Read::Frame(f) if f.id() == Some(1) => {
                    answered = true;
                    break;
                }
                Read::End => break,
                _ => {}
            }
        }
        // Its stdin ending is its end.
        drop(stdin);
        let status = child.wait().unwrap();
        assert!(answered, "{sh}: the helper didn't start");
        assert!(status.success(), "{sh}: the helper didn't exit cleanly at the end of its input");
    }
    assert!(tried.len() >= 3, "too few shells to say anything: {tried:?}");
    let _ = std::fs::remove_dir_all(&home);
}

/// Python 3.6 is the floor on hosts: the helper is parsed and anything from 3.7 on, syntax or
/// a known attribute, is refused. The gate is itself checked on a file that breaks it.
#[test]
fn the_helper_stays_within_python_3_6() {
    if !python3() {
        return;
    }
    const GATE: &str = r#"
import ast, re, sys
src = open(sys.argv[1], encoding="utf-8").read()
tree = ast.parse(src)
bad = []
def add(node, why):
    bad.append("%s:%d: %s" % (sys.argv[1], getattr(node, "lineno", 0), why))
NODES = {"NamedExpr": "walrus (3.8)", "Match": "match (3.10)", "TryStar": "except* (3.11)", "TypeAlias": "type statement (3.12)"}
ATTRS = {"removeprefix", "removesuffix", "isascii", "reconfigure", "time_ns", "monotonic_ns", "perf_counter_ns",
         "SimpleQueue", "cached_property", "fromisoformat", "waitstatus_to_exitcode", "nullcontext", "memfd_create",
         "is_relative_to", "with_stem", "bit_count", "pidfd_open", "to_thread", "get_running_loop", "isqrt", "readlink_at"}
QUALIFIED = {("functools", "cache"), ("shlex", "join"), ("asyncio", "run"), ("asyncio", "create_task"), ("math", "prod"),
             ("math", "dist"), ("time", "time_ns"), ("os", "readlink_at")}
MODULES = {"dataclasses", "contextvars", "zoneinfo", "graphlib", "tomllib", "importlib.resources", "importlib.metadata"}
KWARGS = {"capture_output", "text", "strict"}
for node in ast.walk(tree):
    name = type(node).__name__
    if name in NODES:
        add(node, NODES[name])
    if isinstance(node, ast.arguments) and getattr(node, "posonlyargs", None):
        add(node, "positional-only parameters (3.8)")
    if isinstance(node, ast.Attribute):
        if node.attr in ATTRS:
            add(node, "." + node.attr + " (3.7+)")
        if isinstance(node.value, ast.Name) and (node.value.id, node.attr) in QUALIFIED:
            add(node, node.value.id + "." + node.attr + " (3.7+)")
    if isinstance(node, ast.Import):
        for a in node.names:
            if a.name in MODULES:
                add(node, "import " + a.name + " (3.7+)")
    if isinstance(node, ast.ImportFrom):
        if node.module in MODULES:
            add(node, "from " + node.module + " (3.7+)")
        if node.module == "__future__" and any(a.name == "annotations" for a in node.names):
            add(node, "from __future__ import annotations (3.7)")
    if isinstance(node, ast.Call):
        for kw in node.keywords:
            if kw.arg in KWARGS:
                add(node, kw.arg + "= (3.7+)")
    if isinstance(node, ast.Subscript) and isinstance(node.value, ast.Name) and node.value.id in ("list", "dict", "tuple", "set", "frozenset", "type"):
        add(node, node.value.id + "[...] (3.9)")
    if isinstance(node, ast.BinOp) and isinstance(node.op, ast.BitOr) and (isinstance(node.left, ast.Dict) or isinstance(node.right, ast.Dict)):
        add(node, "dict | (3.9)")
    if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
        for d in node.decorator_list:
            e = d.func if isinstance(d, ast.Call) else d
            while isinstance(e, ast.Attribute):
                e = e.value
            if not isinstance(e, ast.Name):
                add(d, "a decorator that isn't a dotted name (3.9)")
    if isinstance(node, ast.With) and len(node.items) > 1:
        line = src.splitlines()[node.lineno - 1]
        if re.match(r"\s*(async\s+)?with\s*\(", line):
            add(node, "parenthesized context managers (3.10)")
for i, line in enumerate(src.splitlines(), 1):
    if re.search(r"""\bf["'].*[^=!<>]=(![rsa])?(:[^}]*)?\}""", line):
        bad.append("%s:%d: f-string = specifier (3.8)" % (sys.argv[1], i))
print("\n".join(bad))
sys.exit(1 if bad else 0)
"#;
    let dir = scratch("gate");
    let gate = dir.join("gate.py");
    std::fs::write(&gate, GATE).unwrap();
    let helper = dir.join("helper.py");
    std::fs::write(&helper, proto::HELPER).unwrap();
    let run = |file: &Path| Command::new("python3").arg("-I").arg(&gate).arg(file).output().unwrap();
    let out = run(&helper);
    assert!(out.status.success(), "helper.py uses Python newer than 3.6:\n{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let broken = dir.join("broken.py");
    std::fs::write(
        &broken,
        "import dataclasses\nif (n := 1):\n    pass\nx = 'a'.removeprefix('b')\ndef f(a, /):\n    pass\ny: list[int] = []\nimport subprocess\nsubprocess.run(['x'], capture_output=True)\nprint(f\"{n=}\")\n",
    )
    .unwrap();
    let out = run(&broken);
    let report = String::from_utf8_lossy(&out.stdout);
    assert!(!out.status.success());
    for why in ["walrus", "removeprefix", "dataclasses", "positional-only", "list[...]", "capture_output", "= specifier"] {
        assert!(report.contains(why), "the gate missed {why}:\n{report}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A root opening just as the last one's grace period ends: `Remotes::lease` counts its lease
/// under the registry's lock, and the grace period decides under the same lock, so the helper
/// handed out is never closed under it. The test holds that lock across the grace period's end
/// and leases there, as `lease` does.
#[test]
fn a_helper_leased_as_its_grace_ends_is_not_closed() {
    let home = scratch("grace");
    let rs = remotes(&home, "python3");
    let c = rs.conn("me@here", None);
    let map = rs.map.lock();
    std::thread::sleep(fast().grace + Duration::from_millis(300));
    let lease = Lease::new(map.get("me@here").unwrap().clone());
    drop(map);
    std::thread::sleep(Duration::from_millis(200));
    assert_ne!(c.status(), Status::Closed, "closed under a root that had just leased it");
    assert_eq!(rs.count(), 1);
    assert!(Arc::ptr_eq(lease.conn(), &rs.conn("me@here", None)));
    drop(lease);
    let _ = std::fs::remove_dir_all(&home);
}

/// The helper behind a slow link: everything maiTerm writes is passed on at `rate` bytes a
/// second, and the helper gives up after `deadline` seconds of hearing nothing.
struct Throttled {
    home: PathBuf,
    rate: u32,
    deadline: &'static str,
}

impl Transport for Throttled {
    fn helper(&self, _t: &Target) -> Command {
        let throttle = format!("import os,time\nwhile 1:\n b=os.read(0,8192)\n if not b:\n  break\n time.sleep(len(b)/{})\n while b:\n  b=b[os.write(1,b):]\n", self.rate);
        let mut c = Command::new("/bin/sh");
        c.arg("-c").arg(format!("python3 -c '{throttle}' | {}", proto::remote_command()));
        env(&mut c, &self.home);
        c.env("MAITERM_FT_PING_DEADLINE", self.deadline);
        c
    }

    fn shell(&self, t: &Target) -> Command {
        Local { home: self.home.clone(), python: "python3" }.shell(t)
    }
}

/// An import of a file that takes far longer to carry than a ping may wait: it arrives whole,
/// the helper is never restarted, and pings went on meanwhile. One frame of the whole file used
/// to hold stdin until it was written, so the ping behind it timed out and Rust killed the
/// helper, and the helper itself, reading no ping until the payload was in, gave up too.
#[test]
#[cfg(unix)]
fn a_slow_upload_is_not_taken_for_a_dead_helper() {
    if !python3() {
        return;
    }
    let home = scratch("slow");
    let t = scratch("slow-root");
    let src = scratch("slow-src");
    let rs = Remotes::default();
    // 1.25 MB at 512 KB/s: ~2.5 s to carry, against a 300 ms ping wait and a helper that
    // gives up after a second of silence.
    let timing = Timing { ping_every: Duration::from_millis(200), ping_timeout: Duration::from_millis(300), ..fast() };
    rs.set_transport(Arc::new(Throttled { home: home.clone(), rate: 512 << 10, deadline: "1.0" }), timing);
    let root = RemoteRoot::open(rs.lease("me@here", None), &t.to_string_lossy(), Vec::new()).unwrap();
    let epoch = root.conn.st.lock().epoch;
    let bytes: Vec<u8> = (0..1_310_720u32).map(|i| (i % 251) as u8).collect();
    let file = src.join("big.bin");
    std::fs::write(&file, &bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
    }
    let mut made = false;
    upload(&root, &file, "big.bin", &mut made).unwrap();
    assert!(made);
    assert_eq!(std::fs::read(t.join("big.bin")).unwrap(), bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(t.join("big.bin")).unwrap().permissions().mode() & 0o777, 0o640, "the last frame sets the mode");
    }
    assert_eq!(root.conn.status(), Status::Live);
    assert_eq!(root.conn.st.lock().epoch, epoch, "the helper was restarted during the upload");
    // An append lands only on the file it continues, at the length it expects.
    let r = root.root.clone();
    assert_eq!(code(root.conn.call("append", json!({ "root": r, "rel": "big.bin", "at": 5, "more": false }), b"x", T)), "changed");
    assert_eq!(code(root.conn.call("append", json!({ "root": r, "rel": "nothing.bin", "at": 0 }), b"x", T)), "gone");
    drop(root);
    for d in [&home, &t, &src] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// A first frame that outlasts its wait while being written still reaches the helper, which
/// makes the file from it. The upload must count that file as made, so the import's discard
/// removes it, rather than leave the first megabyte under the real name at 0600.
#[test]
#[cfg(unix)]
fn a_first_frame_that_times_out_is_discarded_not_left_half_made() {
    if !python3() {
        return;
    }
    let home = scratch("late");
    let t = scratch("late-root");
    let src = scratch("late-src");
    let rs = Remotes::default();
    // 1 MB at 512 KB/s is ~2 s to carry, against half a second for the frame's answer.
    let timing = Timing { put_chunk: Duration::from_millis(500), ..fast() };
    rs.set_transport(Arc::new(Throttled { home: home.clone(), rate: 512 << 10, deadline: "30" }), timing);
    let root = RemoteRoot::open(rs.lease("me@here", None), &t.to_string_lossy(), Vec::new()).unwrap();
    let file = src.join("big.bin");
    std::fs::write(&file, vec![7u8; 1_310_720]).unwrap();
    let mut made = false;
    assert!(upload(&root, &file, "big.bin", &mut made).is_err(), "the first frame's answer can't come in time");
    assert!(made, "the helper made the file from a frame that went out");
    ok(root.conn.call("discard", json!({ "root": root.root, "rel": "big.bin" }), &[], T));
    std::thread::sleep(Duration::from_millis(300));
    assert!(!t.join("big.bin").exists(), "a timed-out first frame left a file behind");
    // An answer that is already refused (another writer's file) is not this import's to discard.
    std::fs::write(t.join("theirs.bin"), b"keep").unwrap();
    let mut made = false;
    let small = src.join("small.bin");
    std::fs::write(&small, b"x").unwrap();
    assert_eq!(upload(&root, &small, "theirs.bin", &mut made).unwrap_err().0, "exists");
    assert!(!made);
    drop(root);
    for d in [&home, &t, &src] {
        let _ = std::fs::remove_dir_all(d);
    }
}

/// The helper's own code, loaded under python3 with `send` captured and one op swapped for a
/// version that misbehaves on cue: the cases below can't be made to happen on cue end to end.
fn run_helper_harness(name: &str, script: &str) {
    let dir = scratch(name);
    let helper = dir.join("helper.py");
    std::fs::write(&helper, proto::HELPER).unwrap();
    let harness = dir.join("harness.py");
    let prelude = "import errno, json, os, sys, time\nfrom concurrent.futures import ThreadPoolExecutor\nsrc = open(sys.argv[1], encoding='utf-8').read()\nh = {'__name__': 'helper'}\nexec(compile(src, sys.argv[1], 'exec'), h)\nanswers = []\nh['send'] = lambda header, payload=b'': answers.append(header)\nroot = sys.argv[2]\n";
    std::fs::write(&harness, format!("{prelude}{script}")).unwrap();
    let root = dir.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let mut c = Command::new("python3");
    c.arg("-I").arg(&harness).arg(&helper).arg(&root);
    env(&mut c, &dir);
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    let _ = std::fs::remove_dir_all(&dir);
}

/// A first frame whose write fails after it made the file (a full disk, a quota) takes the
/// file back out: Rust reads any refused put as "not made" and sends no discard after it.
#[test]
#[cfg(unix)]
fn a_put_whose_write_fails_leaves_nothing_behind() {
    if !python3() {
        return;
    }
    run_helper_harness(
        "put-fails",
        r#"
def full_disk(fd, payload):
    os.write(fd, payload[:3])
    raise OSError(errno.ENOSPC, "No space left on device")
h['write_all'] = full_disk
h['handle']({'id': 1, 'op': 'put', 'root': root, 'rel': 'big.bin', 'more': True}, b'x' * 10)
assert answers == [{'id': 1, 'ok': False, 'code': 'io', 'msg': answers[0]['msg']}], answers
assert not os.path.lexists(os.path.join(root, 'big.bin')), 'a failed put left its file'
"#,
    );
}

/// A discard sent while the put it follows is still running waits for it. The helper's workers
/// run jobs in any order, so a discard that ran first removed nothing and the put then made the
/// half-written file it was sent to take away; the put is slowed here so it always would.
#[test]
#[cfg(unix)]
fn a_discard_waits_for_the_put_it_follows() {
    if !python3() {
        return;
    }
    run_helper_harness(
        "discard-order",
        r#"
real_put = h['PAYLOAD_OPS']['put']
def slow_put(req, payload):
    time.sleep(0.4)
    return real_put(req, payload)
h['PAYLOAD_OPS']['put'] = slow_put
os.mkdir(os.path.join(root, 'dir'))
pool = ThreadPoolExecutor(max_workers=6)
h['dispatch'](pool, {'id': 1, 'op': 'put', 'root': root, 'rel': 'big.bin', 'more': True}, b'x' * 10)
h['dispatch'](pool, {'id': 2, 'op': 'put', 'root': root, 'rel': 'dir/inner.bin', 'more': True}, b'x' * 10)
h['dispatch'](pool, {'id': 3, 'op': 'discard', 'root': root, 'rel': 'big.bin'}, b'')
h['dispatch'](pool, {'id': 4, 'op': 'discard', 'root': root, 'rel': 'dir'}, b'')
pool.shutdown(wait=True)
assert all(a['ok'] for a in answers), answers
assert not os.path.lexists(os.path.join(root, 'big.bin')), 'the discard ran before the put'
assert not os.path.lexists(os.path.join(root, 'dir')), 'a folder discard ran before a put inside it'
assert h['MAKING'] == {}, h['MAKING']
"#,
    );
}

#[test]
#[cfg(unix)]
fn the_helper_ends_with_its_input() {
    if !python3() {
        return;
    }
    let home = scratch("eof");
    let mut c = Local { home: home.clone(), python: "python3" }.helper(&Target { tunnel_key: String::new(), ssh_args: String::new() });
    c.stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null());
    let mut child = c.spawn().unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(&proto::bootstrap()).unwrap();
    drop(stdin);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "the helper outlived its stdin");
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = std::fs::remove_dir_all(&home);
}

/// Against a real host, read-only outside a scratch folder it makes under /tmp and removes:
/// `MAITERM_FT_TEST_HOST=ews@nova cargo test --lib filetree::remote -- --ignored`. The helper
/// runs with HOME in that scratch folder (its trash goes there) and a watch budget of 2, so
/// inotify, its budget fallback and the end of the helper with ssh are all seen there.
#[test]
#[ignore]
fn on_a_real_host() {
    let Ok(host) = std::env::var("MAITERM_FT_TEST_HOST") else { return };
    struct Real(String);
    impl Transport for Real {
        fn helper(&self, _t: &Target) -> Command {
            let mut c = Command::new("ssh");
            c.args(["-o", "BatchMode=yes", "-T", &self.0]).arg(format!(
                "h=$(mktemp -d /tmp/maiterm-ft-XXXXXX) && mkdir \"$h/proj\" && HOME=$h MAITERM_FT_WATCH_BUDGET=2 python3 -u -c '{}'; rm -rf \"$h\"",
                proto::LOADER
            ));
            c
        }
        fn shell(&self, _t: &Target) -> Command {
            let mut c = Command::new("ssh");
            c.args(["-o", "BatchMode=yes", "-T", &self.0, "sh -s"]);
            c
        }
    }
    let ft = FileTreeState::with(Duration::from_millis(200), crate::filetree::watch::RAW_CAPACITY, None);
    ft.remotes.set_transport(Arc::new(Real(host.clone())), Timing { grace: Duration::from_millis(500), ..Timing::default() });
    let conn = ft.remotes.conn("test-host", None);
    let lease = Lease::new(conn.clone());
    let hello = ok(conn.call("hello", json!({}), &[], T));
    let home = hello.header["home"].as_str().unwrap().to_string();
    assert!(home.starts_with("/tmp/maiterm-ft-"), "the scratch HOME, never the real one: {home}");
    assert_eq!(hello.header["inotify"]["available"], json!(true));
    let started = Instant::now();
    let rec = Arc::new(Recorder::default());
    let ack = ft.open_with("w", &format!("test-host:{home}/proj"), vec![], rec.clone(), vec![], None).unwrap();
    rec.wait("the root", |e| matches!(e, TreeEvent::Dir { .. }));
    eprintln!("root painted {:?} after open (helper already running)", started.elapsed());
    let none = Vec::new;
    let op = |file: FileOp| ft.op_with(ack.sub_id, &file, &none, &|_| Vec::new()).unwrap();
    for d in ["a", "b", "c"] {
        op(FileOp { kind: "create_dir".into(), name: Some(d.into()), ..Default::default() });
    }
    let hub = ft.hub_of(ack.sub_id).unwrap();
    let reqs = ["a", "b", "c"].map(|r| crate::filetree::types::DirReq { rel: r.into(), known_gen: None }).to_vec();
    let t0 = Instant::now();
    hub.expand(ack.sub_id, reqs);
    rec.wait("c listed", |e| matches!(e, TreeEvent::Dir { rel, .. } if rel == "c"));
    eprintln!("three expands answered in {:?}", t0.elapsed());
    // Budget 2: the root and one folder are watched, the others polled, and Health says so.
    rec.wait("the budget fallback", |e| matches!(e, TreeEvent::Health { watch, .. } if watch == "partial"));
    // A watched folder's change arrives by inotify (no poll is running for it).
    op(FileOp { kind: "create_file".into(), dest: Some("".into()), name: Some("x.txt".into()), ..Default::default() });
    rec.wait("x.txt", |e| matches!(e, TreeEvent::Delta { add, .. } if add.iter().any(|a| a.n == "x.txt")));
    let trashed = op(FileOp { kind: "trash".into(), src: vec!["x.txt".into()], ..Default::default() });
    assert_eq!(op(trashed.undo.unwrap()).done.len(), 1);
    ft.close(ack.sub_id);
    drop(hub);
    drop(lease);
    drop(conn);
    let deadline = Instant::now() + Duration::from_secs(20);
    while ft.remotes.count() > 0 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(100));
    }
    // The helper ended with its ssh, and its scratch folder with it.
    std::thread::sleep(Duration::from_secs(2));
    let check = Command::new("ssh")
        .args(["-o", "BatchMode=yes", "-T", &host, &format!("test -e {home} && echo left || echo gone; pgrep -u \"$(id -u)\" -f 'maiterm[-]helper' >/dev/null && echo helper || echo none")])
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&check.stdout);
    assert!(said.contains("gone"), "the scratch folder was left: {said}");
    assert!(said.contains("none"), "a helper outlived its ssh: {said}");
}
