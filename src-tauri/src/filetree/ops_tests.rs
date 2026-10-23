//! File operations against a real disk (docs/file-tree.md §20 phase 3). Every test works in its
//! own temp folder, and the trash is a stand-in: a test must never fill the real Trash.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;

use super::*;

fn temp(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("ft-ops-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&t).unwrap();
    crate::share::canon(&t)
}

/// A trash that moves items into a folder of its own, or refuses like a network volume.
struct FakeTrash {
    bin: PathBuf,
    refuse: bool,
    restores: bool,
    held: Arc<Mutex<Vec<(PathBuf, PathBuf)>>>,
}

impl TrashBackend for FakeTrash {
    fn trash(&mut self, path: &Path) -> Result<(), String> {
        if self.refuse {
            return Err("no trash on this volume".into());
        }
        let to = self.bin.join(uuid::Uuid::new_v4().simple().to_string());
        std::fs::rename(path, &to).map_err(|e| e.to_string())?;
        self.held.lock().push((path.to_path_buf(), to));
        Ok(())
    }

    fn can_restore(&self) -> bool {
        self.restores
    }

    fn restore(&mut self, items: &[(PathBuf, i64)]) -> Vec<Result<(), (String, String)>> {
        items
            .iter()
            .map(|(p, _)| {
                let mut held = self.held.lock();
                let i = held.iter().position(|(orig, _)| orig == p).ok_or(("not_in_trash".to_string(), String::new()))?;
                let (orig, binned) = held.remove(i);
                std::fs::rename(binned, orig).map_err(|e| ("io".to_string(), e.to_string()))
            })
            .collect()
    }
}

struct Fx {
    tmp: PathBuf,
    root: PathBuf,
    fold: bool,
    trash: TrashWorker,
    tickets: Tickets,
}

impl Fx {
    fn new(tag: &str) -> Fx {
        Fx::with(tag, false, true, TICKET_TTL)
    }

    fn with(tag: &str, refuse: bool, restores: bool, ttl: Duration) -> Fx {
        let tmp = temp(tag);
        let root = tmp.join("proj");
        let bin = tmp.join("bin");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let fold = crate::relocate::case_insensitive(&root);
        let trash = TrashWorker::start(Box::new(FakeTrash { bin, refuse, restores, held: Arc::default() }));
        Fx { tmp, root, fold, trash, tickets: Tickets::with_ttl(ttl) }
    }

    fn env<'a>(&'a self, holders: &'a dyn Fn() -> Vec<CwdHolder>) -> Env<'a> {
        Env { place: "test", root: &self.root, fold: self.fold, trash: &self.trash, tickets: &self.tickets, holders, rename: rename_noreplace }
    }

    fn run(&self, op: FileOp) -> Result<OpResult, String> {
        let none = || Vec::new();
        run(&self.env(&none), &op)
    }

    fn write(&self, rel: &str, body: &str) {
        let p = self.root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root.join(rel)).unwrap()
    }

    fn names(&self, rel: &str) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(self.root.join(rel)).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
        v.sort();
        v
    }
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.tmp);
    }
}

fn op(kind: &str, src: &[&str]) -> FileOp {
    FileOp { kind: kind.into(), src: src.iter().map(|s| s.to_string()).collect(), ..Default::default() }
}

fn copy_name(stem: &str, ext: &str) -> String {
    crate::util::naming::unique_name(&format!("{stem}{ext}"), false, crate::util::naming::CopyNames::platform(), true, |_| false).unwrap()
}

#[test]
fn rels_that_could_step_outside_are_refused_before_anything_happens() {
    let fx = Fx::new("escape");
    fx.write("a.ts", "a");
    for bad in ["../x", "/etc/passwd", "a/../../x", "", "a//b"] {
        assert!(fx.run(op("trash", &[bad])).is_err(), "trash {bad:?}");
        assert!(fx.run(FileOp { kind: "create_file".into(), dest: Some(bad.into()), name: Some("n".into()), ..Default::default() }).is_err() || bad.is_empty(), "create in {bad:?}");
    }
    // The root itself is never acted on.
    assert!(fx.run(op("trash", &[""])).is_err());
    assert!(fx.run(FileOp { kind: "rename".into(), src: vec!["".into()], name: Some("x".into()), ..Default::default() }).is_err());
    // A name can't smuggle a path in.
    assert!(fx.run(FileOp { kind: "rename".into(), src: vec!["a.ts".into()], name: Some("../b.ts".into()), ..Default::default() }).is_err());
    assert!(fx.run(FileOp { kind: "create_file".into(), name: Some("../evil".into()), ..Default::default() }).is_err());
    assert!(!fx.tmp.join("evil").exists());
    // A move's exact destinations are held to the same rule.
    assert!(fx.run(FileOp { kind: "move".into(), src: vec!["a.ts".into()], to: vec!["../a.ts".into()], ..Default::default() }).is_err());
    assert_eq!(fx.read("a.ts"), "a");
}

#[cfg(unix)]
#[test]
fn a_link_on_the_way_out_of_the_root_is_refused() {
    let fx = Fx::new("links");
    let outside = fx.tmp.join("proj2");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("secret"), "s").unwrap();
    std::fs::create_dir_all(fx.root.join("inner")).unwrap();
    let ln = |target: &Path, name: &str| std::os::unix::fs::symlink(target, fx.root.join(name)).unwrap();
    ln(Path::new("/etc"), "etc");
    // A sibling that only shares the root's name as a string prefix is outside.
    ln(&outside, "sib");
    ln(&fx.tmp.join("PROJ2"), "sib_case");
    ln(&fx.root.join("inner"), "inside");
    ln(&fx.tmp.join("PROJ"), "self_case");

    let create_in = |dest: &str| fx.run(FileOp { kind: "create_file".into(), dest: Some(dest.into()), name: Some("n.txt".into()), ..Default::default() });
    for dest in ["etc", "sib", "sib_case"] {
        assert!(create_in(dest).is_err(), "create through {dest}");
        let r = fx.run(FileOp { kind: "copy".into(), src: vec![format!("{dest}/secret")], dest: Some("".into()), ..Default::default() });
        assert!(r.is_err(), "copy out through {dest}");
        assert!(fx.run(op("trash", &[&format!("{dest}/secret")])).is_err(), "trash through {dest}");
    }
    assert!(!outside.join("n.txt").exists());
    assert!(outside.join("secret").exists());
    // A link that stays inside is fine.
    assert_eq!(create_in("inside").unwrap().done.len(), 1);
    // A case variant of the root is the root where the volume folds case; elsewhere it names
    // nothing, and a link that doesn't resolve is refused too.
    assert_eq!(create_in("self_case").is_ok(), fx.fold);
    // The link itself is an item like any other: trashing it trashes the link, not /etc.
    let r = fx.run(op("trash", &["etc"])).unwrap();
    assert_eq!(r.done.len(), 1);
    assert!(Path::new("/etc").exists());
}

#[cfg(windows)]
#[test]
fn verbatim_and_unc_prefixes_never_parse_as_rels() {
    let fx = Fx::new("verbatim");
    for bad in [r"\\?\C:\Windows", r"\\server\share\x", r"C:\x", "C:x", r"a\..\..\x"] {
        assert!(fx.run(op("trash", &[bad])).is_err(), "{bad:?}");
    }
    for name in ["CON", "nul.txt", "a.", "a ", "a?b", "COM1"] {
        assert!(fx.run(FileOp { kind: "create_file".into(), name: Some(name.into()), ..Default::default() }).is_err(), "{name:?}");
    }
    // Win32 would trim these to another name: `...` to the root itself, `foo.` to `foo`.
    fx.write("foo", "sibling");
    for bad in ["...", "foo.", "foo ", "foo./x"] {
        assert!(fx.run(op("trash", &[bad])).is_err(), "trash {bad:?}");
        let none = || Vec::new();
        assert!(prepare_delete(&fx.env(&none), &[bad.to_string()]).is_err(), "delete {bad:?}");
    }
    assert_eq!(fx.read("foo"), "sibling");
}

/// A raw Win32 rename needs verbatim paths to reach past 260 characters.
#[test]
fn raw_win32_paths_are_made_verbatim() {
    assert_eq!(verbatim(r"C:\proj\a.ts"), r"\\?\C:\proj\a.ts");
    assert_eq!(verbatim("C:/proj/a.ts"), r"\\?\C:\proj\a.ts");
    assert_eq!(verbatim(r"\\server\share\a.ts"), r"\\?\UNC\server\share\a.ts");
    assert_eq!(verbatim(r"\\?\C:\proj"), r"\\?\C:\proj");
    assert_eq!(verbatim(r"\\.\pipe\x"), r"\\.\pipe\x");
}

#[cfg(windows)]
#[test]
fn a_rename_past_the_old_path_limit_works() {
    let fx = Fx::new("long");
    let deep = (0..30).map(|i| format!("folder{i:02}")).collect::<Vec<_>>().join("/");
    fx.write(&format!("{deep}/a.ts"), "a");
    assert!(fx.root.join(&deep).join("a.ts").as_os_str().len() > 260);
    let r = fx.run(FileOp { kind: "rename".into(), src: vec![format!("{deep}/a.ts")], name: Some("b.ts".into()), ..Default::default() }).unwrap();
    assert_eq!(r.done.len(), 1, "{r:?}");
    assert_eq!(fx.read(&format!("{deep}/b.ts")), "a");
}

/// A junction copies as a junction: making a symlink would need Developer Mode or admin.
#[cfg(windows)]
#[test]
fn a_junction_is_copied_as_a_junction() {
    let fx = Fx::new("junction");
    fx.write("pkg/target/index.js", "x");
    junction::create(fx.root.join("pkg/target"), fx.root.join("pkg/link")).unwrap();
    let r = fx.run(op("duplicate", &["pkg"])).unwrap();
    assert_eq!(r.done.len(), 1, "{r:?}");
    let copy = fx.root.join(r.done[0].to.as_deref().unwrap()).join("link");
    assert!(junction::exists(&copy).unwrap());
    assert_eq!(crate::share::canon(&junction::get_target(&copy).unwrap()), crate::share::canon(&fx.root.join("pkg/target")));
}

#[test]
fn create_never_clobbers_and_makes_the_folders_on_the_way() {
    let fx = Fx::new("create");
    fx.write("a.ts", "keep");
    let mk = |name: &str, conflict: &str| fx.run(FileOp { kind: "create_file".into(), name: Some(name.into()), conflict: conflict.into(), ..Default::default() }).unwrap();
    let r = mk("a.ts", "fail");
    assert!(r.done.is_empty());
    assert_eq!(r.failed[0].code, "exists");
    assert_eq!(fx.read("a.ts"), "keep");
    assert_eq!(mk("a.ts", "skip").skipped, vec!["a.ts".to_string()]);
    let r = mk("a.ts", "keep_both");
    assert_eq!(r.done[0].from, copy_name("a", ".ts"));
    assert_eq!(fx.read("a.ts"), "keep");
    assert_eq!(r.undo.unwrap(), FileOp { kind: "trash".into(), src: vec![copy_name("a", ".ts")], ..Default::default() });

    let r = mk("x/y/z.ts", "fail");
    assert_eq!(r.done[0].from, "x/y/z.ts");
    assert!(fx.root.join("x/y/z.ts").is_file());
    // Undo takes the outermost folder this op made.
    assert_eq!(r.undo.unwrap().src, vec!["x".to_string()]);
    // An existing folder on the way is used; undo then takes only what is new.
    let r = mk("x/w.ts", "fail");
    assert_eq!(r.undo.unwrap().src, vec!["x/w.ts".to_string()]);

    let r = fx.run(FileOp { kind: "create_dir".into(), dest: Some("x".into()), name: Some("y".into()), ..Default::default() }).unwrap();
    assert_eq!(r.failed[0].code, "exists");
    // A file on the way is not a folder.
    let r = mk("a.ts/b.ts", "fail");
    assert_eq!(r.failed[0].code, "exists");
}

#[test]
fn rename_refuses_to_replace_and_undoes_itself() {
    let fx = Fx::new("rename");
    fx.write("a.ts", "a");
    fx.write("b.ts", "b");
    let ren = |src: &str, name: &str, conflict: &str| fx.run(FileOp { kind: "rename".into(), src: vec![src.into()], name: Some(name.into()), conflict: conflict.into(), ..Default::default() }).unwrap();
    let r = ren("a.ts", "b.ts", "fail");
    assert_eq!(r.failed[0].code, "exists");
    assert_eq!((fx.read("a.ts"), fx.read("b.ts")), ("a".to_string(), "b".to_string()));
    let r = ren("a.ts", "c.ts", "fail");
    assert_eq!(r.done[0].to.as_deref(), Some("c.ts"));
    let undo = r.undo.unwrap();
    assert_eq!(undo, FileOp { kind: "rename".into(), src: vec!["c.ts".into()], name: Some("a.ts".into()), ..Default::default() });
    fx.run(undo).unwrap();
    assert_eq!(fx.read("a.ts"), "a");
    let r = ren("a.ts", "b.ts", "keep_both");
    assert_eq!(r.done[0].to.as_deref(), Some(copy_name("b", ".ts").as_str()));
    assert_eq!(fx.read("b.ts"), "b");
}

/// The OS-level refusal, not just the check before it: the target appears after any check.
#[test]
fn the_rename_primitive_refuses_an_existing_target() {
    let fx = Fx::new("noreplace");
    fx.write("a", "a");
    fx.write("b", "b");
    let e = rename_noreplace(&fx.root.join("a"), &fx.root.join("b")).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
    assert_eq!(fx.read("b"), "b");
}

#[test]
fn a_case_only_rename_changes_the_spelling() {
    let fx = Fx::new("case");
    fx.write("foo.ts", "body");
    let r = fx.run(FileOp { kind: "rename".into(), src: vec!["foo.ts".into()], name: Some("Foo.ts".into()), ..Default::default() }).unwrap();
    assert_eq!(r.done.len(), 1, "{r:?}");
    assert_eq!(fx.names(""), vec!["Foo.ts".to_string()]);
    assert_eq!(fx.read("Foo.ts"), "body");
    fx.run(r.undo.unwrap()).unwrap();
    assert_eq!(fx.names(""), vec!["foo.ts".to_string()]);
    // A folder too, with what is inside it.
    fx.write("dir/x", "x");
    let r = fx.run(FileOp { kind: "rename".into(), src: vec!["dir".into()], name: Some("DIR".into()), ..Default::default() }).unwrap();
    assert_eq!(r.done.len(), 1);
    assert_eq!(fx.read("DIR/x"), "x");
    assert!(fx.names("").contains(&"DIR".to_string()));
}

#[test]
fn move_goes_back_exactly_on_undo() {
    let fx = Fx::new("move");
    fx.write("a/one.ts", "1");
    fx.write("b/two.ts", "2");
    fx.write("dest/one.ts", "existing");
    std::fs::create_dir_all(fx.root.join("dest")).unwrap();
    let r = fx.run(FileOp { kind: "move".into(), src: vec!["a/one.ts".into(), "b/two.ts".into()], dest: Some("dest".into()), conflict: "keep_both".into(), ..Default::default() }).unwrap();
    assert_eq!(r.done.len(), 2, "{r:?}");
    let one = format!("dest/{}", copy_name("one", ".ts"));
    assert_eq!(r.done[0].to.as_deref(), Some(one.as_str()));
    assert_eq!(fx.read("dest/one.ts"), "existing");
    let undo = r.undo.unwrap();
    assert_eq!(undo.to, vec!["a/one.ts".to_string(), "b/two.ts".to_string()]);
    let back = fx.run(undo).unwrap();
    assert_eq!(back.done.len(), 2, "{back:?}");
    assert_eq!((fx.read("a/one.ts"), fx.read("b/two.ts")), ("1".to_string(), "2".to_string()));
    // Without keep_both nothing is replaced.
    let r = fx.run(FileOp { kind: "move".into(), src: vec!["a/one.ts".into()], dest: Some("dest".into()), ..Default::default() }).unwrap();
    assert_eq!(r.failed[0].code, "exists");
    // A folder can't go into itself.
    fx.write("a/sub/x", "x");
    let r = fx.run(FileOp { kind: "move".into(), src: vec!["a".into()], dest: Some("a/sub".into()), ..Default::default() }).unwrap();
    assert_eq!(r.failed[0].code, "inside_itself");
}

fn exdev(_: &Path, _: &Path) -> io::Result<()> {
    #[cfg(unix)]
    return Err(io::Error::from_raw_os_error(libc::EXDEV));
    #[cfg(windows)]
    return Err(io::Error::from_raw_os_error(17));
}

/// A rename that crosses volumes copies, checks the copy and only then removes the source;
/// links inside come across as links.
#[test]
fn a_cross_device_move_copies_verifies_then_removes() {
    let fx = Fx::new("exdev");
    fx.write("src/a.txt", "alpha");
    fx.write("src/deep/b.txt", "beta");
    #[cfg(unix)]
    std::os::unix::fs::symlink("a.txt", fx.root.join("src/link")).unwrap();
    std::fs::create_dir_all(fx.root.join("dest")).unwrap();
    let none = || Vec::new();
    let mut env = fx.env(&none);
    env.rename = exdev;
    let r = run(&env, &FileOp { kind: "move".into(), src: vec!["src".into()], dest: Some("dest".into()), ..Default::default() }).unwrap();
    assert_eq!(r.done.len(), 1, "{r:?}");
    assert!(!fx.root.join("src").exists(), "the source goes once the copy checks out");
    assert_eq!(fx.read("dest/src/deep/b.txt"), "beta");
    #[cfg(unix)]
    assert!(std::fs::symlink_metadata(fx.root.join("dest/src/link")).unwrap().file_type().is_symlink());
}

/// Another writer takes the name between the landing check and the copy (Linux answers EXDEV
/// before it looks at the target): its item is someone else's, so the move refuses and the
/// cleanup leaves it alone.
fn exdev_after_someone_writes(_: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir(to)?;
    std::fs::write(to.join("agent-work.txt"), "theirs")?;
    exdev(to, to)
}

#[test]
fn a_cross_device_move_never_removes_what_appeared_at_the_target() {
    let fx = Fx::new("exdev-race");
    fx.write("src/a.txt", "alpha");
    std::fs::create_dir_all(fx.root.join("dest")).unwrap();
    let none = || Vec::new();
    let mut env = fx.env(&none);
    env.rename = exdev_after_someone_writes;
    let r = run(&env, &FileOp { kind: "move".into(), src: vec!["src".into()], dest: Some("dest".into()), ..Default::default() }).unwrap();
    assert!(r.done.is_empty(), "{r:?}");
    assert_eq!(r.failed[0].code, "exists");
    assert_eq!(fx.read("dest/src/agent-work.txt"), "theirs");
    assert_eq!(fx.read("src/a.txt"), "alpha", "the source stays");
}

/// The real thing on Linux CI: /dev/shm is tmpfs, the temp folder usually isn't.
#[cfg(target_os = "linux")]
#[test]
fn a_real_cross_device_move_from_tmpfs() {
    let shm = Path::new("/dev/shm");
    if !shm.is_dir() {
        return;
    }
    let fx = Fx::new("tmpfs");
    let src = shm.join(format!("ft-ops-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(src.join("d")).unwrap();
    std::fs::write(src.join("d/f"), "x").unwrap();
    let to = fx.root.join("moved");
    match rename_noreplace(&src, &to) {
        // The temp folder is on tmpfs here too: nothing crosses.
        Ok(()) => {}
        Err(e) => {
            assert!(is_cross_device(&e), "{e}");
            let moved = move_across(&src, &to);
            let _ = std::fs::remove_dir_all(&src);
            moved.unwrap();
            assert_eq!(std::fs::read_to_string(to.join("d/f")).unwrap(), "x");
        }
    }
    let _ = std::fs::remove_dir_all(&src);
}

#[test]
fn copy_duplicate_and_import_never_replace() {
    let fx = Fx::new("copy");
    fx.write("a.ts", "a");
    fx.write("d/x.ts", "x");
    let r = fx.run(FileOp { kind: "copy".into(), src: vec!["a.ts".into()], dest: Some("d".into()), ..Default::default() }).unwrap();
    assert_eq!(r.done[0].to.as_deref(), Some("d/a.ts"));
    let r = fx.run(FileOp { kind: "copy".into(), src: vec!["a.ts".into()], dest: Some("d".into()), ..Default::default() }).unwrap();
    assert_eq!(r.failed[0].code, "exists");
    let r = fx.run(op("duplicate", &["a.ts", "d"])).unwrap();
    assert_eq!(r.done.len(), 2);
    assert_eq!(r.done[0].to.as_deref(), Some(copy_name("a", ".ts").as_str()));
    let dir_copy = crate::util::naming::unique_name("d", true, crate::util::naming::CopyNames::platform(), true, |_| false).unwrap();
    assert_eq!(fx.read(&format!("{dir_copy}/x.ts")), "x");
    assert_eq!(r.undo.unwrap().src.len(), 2);
    let r = fx.run(FileOp { kind: "copy".into(), src: vec!["d".into()], dest: Some("d".into()), ..Default::default() }).unwrap();
    assert_eq!(r.failed[0].code, "inside_itself");

    // Import: sources anywhere, the destination inside.
    let outside = fx.tmp.join("drop.txt");
    std::fs::write(&outside, "dropped").unwrap();
    let r = fx.run(FileOp { kind: "import".into(), src: vec![outside.to_string_lossy().into_owned()], dest: Some("d".into()), ..Default::default() }).unwrap();
    assert_eq!(r.done[0].to.as_deref(), Some("d/drop.txt"));
    assert_eq!(fx.read("d/drop.txt"), "dropped");
    assert!(outside.exists(), "an import copies");
    assert!(fx.run(FileOp { kind: "import".into(), src: vec!["relative.txt".into()], ..Default::default() }).is_err());
    assert!(fx.run(FileOp { kind: "import".into(), src: vec![outside.to_string_lossy().into_owned()], dest: Some("../".into()), ..Default::default() }).is_err());
    // Importing the root's own parent into the root would never end.
    let r = fx.run(FileOp { kind: "import".into(), src: vec![fx.tmp.to_string_lossy().into_owned()], ..Default::default() }).unwrap();
    assert_eq!(r.failed[0].code, "inside_itself");
}

#[cfg(unix)]
#[test]
fn copy_and_delete_never_follow_a_link() {
    let fx = Fx::new("nofollow");
    let outside = fx.tmp.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("precious"), "p").unwrap();
    fx.write("d/own.txt", "o");
    std::os::unix::fs::symlink(&outside, fx.root.join("d/out")).unwrap();

    let r = fx.run(FileOp { kind: "copy".into(), src: vec!["d".into()], dest: Some("".into()), conflict: "keep_both".into(), ..Default::default() }).unwrap();
    let copy = r.done[0].to.clone().unwrap();
    let link = fx.root.join(&copy).join("out");
    assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink(), "copied as a link");
    assert_eq!(std::fs::read_link(&link).unwrap(), outside);

    let t = fx.run(FileOp { kind: "delete".into(), src: vec!["d".into()], ..Default::default() });
    assert!(t.is_err(), "no ticket");
    let none = || Vec::new();
    let ticket = prepare_delete(&fx.env(&none), &["d".to_string()]).unwrap();
    // d, own.txt and the link (counted as a file: never walked into).
    assert_eq!((ticket.files, ticket.dirs), (2, 1));
    let r = fx.run(FileOp { kind: "delete".into(), src: vec!["d".into()], token: Some(ticket.token), ..Default::default() }).unwrap();
    assert_eq!(r.done.len(), 1);
    assert!(r.undo.is_none(), "a permanent delete can't be undone");
    assert!(!fx.root.join("d").exists());
    assert_eq!(std::fs::read_to_string(outside.join("precious")).unwrap(), "p");
}

#[test]
fn a_delete_takes_a_ticket_for_exactly_its_items_once() {
    let fx = Fx::new("ticket");
    fx.write("a", "a");
    fx.write("b", "b");
    let none = || Vec::new();
    let env = fx.env(&none);
    let del = |src: &[&str], token: Option<String>| run(&env, &FileOp { kind: "delete".into(), src: src.iter().map(|s| s.to_string()).collect(), token, ..Default::default() });

    assert!(del(&["a"], None).is_err());
    assert!(del(&["a"], Some("made-up".into())).is_err());
    let t = prepare_delete(&env, &["a".to_string()]).unwrap();
    assert!(del(&["a", "b"], Some(t.token.clone())).is_err(), "a ticket covers only its own set");
    assert!(fx.root.join("b").exists());
    assert_eq!(del(&["a"], Some(t.token.clone())).unwrap().done.len(), 1);
    fx.write("a", "again");
    assert!(del(&["a"], Some(t.token)).is_err(), "single use");
    assert_eq!(fx.read("a"), "again");

    let expired = Fx::with("expired", false, true, Duration::ZERO);
    expired.write("a", "a");
    let env = expired.env(&none);
    let t = prepare_delete(&env, &["a".to_string()]).unwrap();
    assert!(run(&env, &FileOp { kind: "delete".into(), src: vec!["a".into()], token: Some(t.token), ..Default::default() }).is_err());
    assert!(expired.root.join("a").exists());
}

#[test]
fn a_failed_trash_asks_and_never_removes() {
    let fx = Fx::with("notrash", true, true, TICKET_TTL);
    fx.write("a.ts", "a");
    fx.write("d/b.ts", "b");
    let r = fx.run(op("trash", &["a.ts", "d"])).unwrap();
    assert!(r.done.is_empty());
    assert_eq!(r.needs.as_ref().map(|n| n.code.as_str()), Some("no_trash"));
    assert!(r.failed.iter().all(|f| f.code == "no_trash"));
    assert!(r.undo.is_none());
    assert_eq!((fx.read("a.ts"), fx.read("d/b.ts")), ("a".to_string(), "b".to_string()));
}

#[test]
fn trash_undoes_by_restoring_where_the_platform_can() {
    let fx = Fx::new("trash");
    fx.write("a.ts", "a");
    fx.write("d/b.ts", "b");
    // `d/b.ts` is inside `d`: trashed once, with its folder.
    let r = fx.run(op("trash", &["a.ts", "d", "d/b.ts"])).unwrap();
    assert_eq!(r.done.len(), 2, "{r:?}");
    assert!(!fx.root.join("a.ts").exists());
    let undo = r.undo.unwrap();
    assert_eq!(undo.kind, "restore");
    assert!(undo.trashed_at.is_some());
    let back = fx.run(undo).unwrap();
    assert_eq!(back.done.len(), 2, "{back:?}");
    assert_eq!(fx.read("d/b.ts"), "b");
    assert_eq!(back.undo.unwrap().kind, "trash");

    // macOS: no restore, so no undo, and asking for one is refused.
    let mac = Fx::with("mac", false, false, TICKET_TTL);
    mac.write("a", "a");
    assert!(mac.run(op("trash", &["a"])).unwrap().undo.is_none());
    assert!(mac.run(FileOp { kind: "restore".into(), src: vec!["a".into()], trashed_at: Some(0), ..Default::default() }).is_err());
}

#[test]
fn a_folder_a_tab_works_in_is_refused_with_move_project() {
    let fx = Fx::new("cwd");
    fx.write("app/src/x.ts", "x");
    fx.write("other/y.ts", "y");
    let holders = || vec![CwdHolder { tab_id: "t1".into(), tab_name: "claude".into(), cwd: CwdOf::Path(fx.root.join("app/src")) }];
    let env = fx.env(&holders);
    for o in [op("trash", &["app"]), FileOp { kind: "rename".into(), src: vec!["app".into()], name: Some("app2".into()), ..Default::default() }] {
        let r = run(&env, &o).unwrap();
        let needs = r.needs.unwrap();
        assert_eq!(needs.code, "live_cwd");
        assert_eq!(needs.tabs, vec!["t1".to_string()]);
        assert!(needs.detail.contains("‘claude’") && needs.detail.contains("Move project"), "{}", needs.detail);
        assert!(r.done.is_empty());
    }
    assert!(fx.root.join("app/src/x.ts").exists());
    // A file, an unrelated folder, and a copy are all fine.
    assert_eq!(run(&env, &op("trash", &["other"])).unwrap().done.len(), 1);
    assert_eq!(run(&env, &op("duplicate", &["app"])).unwrap().done.len(), 1);
}

/// The guard reads a live process's cwd at the moment of the op.
#[cfg(unix)]
#[test]
fn a_live_process_in_the_folder_blocks_a_move() {
    let fx = Fx::new("pid");
    fx.write("work/deep/f", "f");
    std::fs::create_dir_all(fx.root.join("dest")).unwrap();
    let mut child = std::process::Command::new("sleep").arg("30").current_dir(fx.root.join("work/deep")).spawn().unwrap();
    let pid = child.id();
    let holders = move || vec![CwdHolder { tab_id: "shell".into(), tab_name: "zsh".into(), cwd: CwdOf::Pid(pid) }];
    let env = fx.env(&holders);
    let r = run(&env, &FileOp { kind: "move".into(), src: vec!["work".into()], dest: Some("dest".into()), ..Default::default() });
    // Only ever our own child, by its handle.
    let _ = child.kill();
    let _ = child.wait();
    let r = r.unwrap();
    assert_eq!(r.needs.map(|n| n.code), Some("live_cwd".to_string()));
    assert!(fx.root.join("work/deep/f").exists());
    // Gone, it no longer holds anything.
    assert_eq!(run(&env, &FileOp { kind: "move".into(), src: vec!["work".into()], dest: Some("dest".into()), ..Default::default() }).unwrap().done.len(), 1);
}

/// End to end through a subscription: the op's folder is re-listed at once, so the new row
/// arrives without waiting on the watcher.
#[test]
fn an_op_through_a_subscription_shows_up_on_the_channel() {
    use super::super::hub::Sink;
    use super::super::types::{Envelope, TreeEvent};
    #[derive(Default)]
    struct Rec(Mutex<Vec<Envelope>>);
    impl Sink for Rec {
        fn send(&self, env: Envelope) -> bool {
            self.0.lock().push(env);
            true
        }
    }
    let fx = Fx::new("sub");
    let ft = super::super::FileTreeState::with(Duration::from_millis(50), super::super::watch::RAW_CAPACITY, None);
    ft.set_trash_backend(Box::new(FakeTrash { bin: fx.tmp.join("bin"), refuse: false, restores: true, held: Arc::default() }));
    let rec = Arc::new(Rec::default());
    let ack = ft.open("w", &fx.root.to_string_lossy(), Vec::new(), rec.clone(), Vec::new()).unwrap();
    assert!(ack.caps.writable && ack.caps.trash);
    let has = |name: &str| {
        rec.0.lock().iter().any(|e| match &e.ev {
            TreeEvent::Dir { rel, entries, .. } if rel.is_empty() => entries.iter().any(|x| x.n == name),
            TreeEvent::Delta { rel, add, .. } if rel.is_empty() => add.iter().any(|x| x.n == name),
            _ => false,
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !rec.0.lock().iter().any(|e| matches!(&e.ev, TreeEvent::Dir { rel, last: true, .. } if rel.is_empty())) {
        assert!(std::time::Instant::now() < deadline, "the root lists");
        std::thread::sleep(Duration::from_millis(10));
    }
    let none = || Vec::new();
    let r = ft.op(ack.sub_id, &FileOp { kind: "create_file".into(), name: Some("new.ts".into()), ..Default::default() }, &none).unwrap();
    assert_eq!(r.done.len(), 1);
    while !has("new.ts") {
        assert!(std::time::Instant::now() < deadline, "the new file arrives");
        std::thread::sleep(Duration::from_millis(10));
    }
    let t = ft.prepare_delete(ack.sub_id, &["new.ts".to_string()]).unwrap();
    assert_eq!((t.files, t.dirs), (1, 0));
    let r = ft.op(ack.sub_id, &FileOp { kind: "delete".into(), src: vec!["new.ts".into()], token: Some(t.token), ..Default::default() }, &none).unwrap();
    assert_eq!(r.done.len(), 1);
    ft.close(ack.sub_id);
}
