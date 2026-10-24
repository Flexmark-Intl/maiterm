use std::cell::Cell;

use super::index::{kept_block, INDEX_BEGIN_LINE, INDEX_END_LINE};
use super::*;

/// A Hive folder with its memory laid out as `create_layout` leaves it.
fn rig(tag: &str) -> (String, HivePaths) {
    let root = std::env::temp_dir().join(format!("hive-memory-{tag}-{}", uuid::Uuid::new_v4()));
    let p = HivePaths::new(crate::share::canon(&{
        fs::create_dir_all(&root).unwrap();
        root
    }));
    fs::create_dir_all(p.memory()).unwrap();
    fs::create_dir_all(p.logs()).unwrap();
    fs::write(p.memory_index(), crate::hive::layout::memory_seed("Ship it")).unwrap();
    let rec = MemoryRecord { v: 1, kept: crate::hive::layout::seed_kept("Ship it"), ..MemoryRecord::default() };
    record::save(&p, &rec).unwrap();
    (format!("h-{tag}-{}", uuid::Uuid::new_v4().simple()), p)
}

fn note(title: &str, desc: &str) -> String {
    format!("---\nname: {title}\ndescription: {desc}\nmetadata:\n  type: project\n---\n\nBody.\n")
}

fn md(p: &HivePaths) -> String {
    fs::read_to_string(p.memory_index()).unwrap()
}

fn rec(p: &HivePaths) -> MemoryRecord {
    record::load(p).unwrap()
}

/// The kept block the view would show: what the human's kept actions say they saw.
fn shown(p: &HivePaths) -> Option<Vec<String>> {
    Some(view(p).kept)
}

fn coordinator() -> Writer {
    Writer { by: record::BY_COORDINATOR.into(), by_kind: record::KIND_COORDINATOR.into(), task_id: None }
}

#[test]
fn a_new_file_is_indexed_once_and_the_rebuild_is_idempotent() {
    let (id, p) = rig("idem");
    fs::write(p.memory().join("store.md"), note("Session store", "Redis, not Postgres")).unwrap();
    // Claude's own pointer line, appended after the fences.
    let mut text = md(&p);
    text.push_str("- [Session store](store.md) — Redis\n");
    fs::write(p.memory_index(), &text).unwrap();
    assert!(rebuild_now(&id, &p).unwrap());
    let after = md(&p);
    assert_eq!(after.matches("(store.md)").count(), 1, "{after}");
    assert!(after.contains("- [Session store](store.md) — Redis, not Postgres · unknown\n"));
    assert!(!rebuild_now(&id, &p).unwrap(), "nothing changes the second time: the watcher can't loop");
    assert_eq!(md(&p), after);
    assert!(!p.memory_unfiled().exists(), "an absorbed pointer is no unfiled line");
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn unfiled_is_written_before_memory_md_and_a_rerun_after_a_crash_duplicates_nothing() {
    let (_, p) = rig("crash");
    let mut text = md(&p);
    text.push_str("remember: the API is v2\n");
    fs::write(p.memory_index(), &text).unwrap();
    let fail = || Err("crash".to_string());
    let seams = Seams { after_unfiled: Some(&fail), ..Seams::default() };
    let mut r = load_or_adopt(&p);
    assert!(rebuild_locked(&p, &mut r, false, &seams).is_err());
    assert!(fs::read_to_string(p.memory_unfiled()).unwrap().contains("remember: the API is v2\n"), "moved first");
    assert_eq!(md(&p), text, "MEMORY.md still holds it: a line is repeated, never lost");

    let mut r = load_or_adopt(&p);
    rebuild_locked(&p, &mut r, false, &Seams::default()).unwrap();
    let unfiled = fs::read_to_string(p.memory_unfiled()).unwrap();
    assert_eq!(unfiled.matches("remember: the API is v2").count(), 1, "{unfiled}");
    assert!(unfiled.starts_with("---\nname: Unfiled lines\n"));
    let m = md(&p);
    assert!(!m.contains("remember: the API"), "{m}");
    assert!(m.contains("- [Unfiled lines](unfiled.md) — Lines found in MEMORY.md that link no memory file · unknown"));
    assert_eq!(view(&p).unfiled, vec!["remember: the API is v2".to_string()]);
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn an_agent_write_between_read_and_rename_is_kept() {
    let (_, p) = rig("cas");
    fs::write(p.memory().join("a.md"), note("A", "first")).unwrap();
    let fired = Cell::new(false);
    let agent = |target: &Path| {
        if !fired.replace(true) {
            let mut t = fs::read_to_string(target).unwrap();
            t.push_str("- [A](a.md) — an agent's pointer\nan agent's loose note\n");
            fs::write(target, t).unwrap();
        }
    };
    let seams = Seams { before_rename: Some(&agent), ..Seams::default() };
    let mut r = load_or_adopt(&p);
    assert!(rebuild_locked(&p, &mut r, false, &seams).unwrap());
    assert!(fired.get());
    let m = md(&p);
    assert_eq!(m.matches("(a.md)").count(), 1, "the pointer is absorbed: {m}");
    assert!(!m.contains("loose note"));
    assert!(fs::read_to_string(p.memory_unfiled()).unwrap().contains("an agent's loose note\n"), "the retry moved it, never dropped it");
    assert!(fs::read_dir(p.memory()).unwrap().flatten().all(|e| !e.file_name().to_string_lossy().starts_with(".MEMORY.md.maiterm-")), "no temp file left");
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn provenance_follows_tool_events_and_reads_unknown_for_anything_else() {
    let (id, p) = rig("prov");
    let f = p.memory().join("plan.md");
    fs::write(&f, note("Plan", "the plan")).unwrap();
    assert!(attribute(&id, &p, "plan.md", &coordinator()).unwrap());
    assert!(md(&p).contains("— the plan · coordinator\n"));
    assert_eq!(rec(&p).files["plan.md"].created_by, "coordinator");

    // A worker's Edit of the same file, named by its badge and title.
    let (_, _, w) = crate::hive::launch::tests::sample_worker_hive();
    crate::hive::worker::write_worker(&p.task(&w.task_id), &w).unwrap();
    let who = writer_for(crate::hive::ROLE_WORKER, &p, "tab-w").unwrap();
    assert_eq!(who, Writer { by: "#3 Auth tokens".into(), by_kind: "worker".into(), task_id: Some("t-auth".into()) });
    assert_eq!(writer_for(crate::hive::ROLE_COORDINATOR, &p, "any").unwrap(), coordinator());
    assert!(writer_for(crate::hive::ROLE_WORKER, &p, "no-such-tab").is_none());
    fs::write(&f, note("Plan", "the plan, revised")).unwrap();
    attribute(&id, &p, "plan.md", &who).unwrap();
    let r = rec(&p);
    assert_eq!((r.files["plan.md"].by.as_str(), r.files["plan.md"].created_by.as_str()), ("#3 Auth tokens", "coordinator"));
    assert!(md(&p).contains("— the plan, revised · #3 Auth tokens\n"));

    // A Bash `>` no tool event explains.
    fs::write(&f, note("Plan", "rewritten by a shell")).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert_eq!(rec(&p).files["plan.md"].by, "unknown");
    assert!(md(&p).contains("— rewritten by a shell · unknown\n"));

    // Deleted: it leaves the record and the index.
    fs::remove_file(&f).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert!(!rec(&p).files.contains_key("plan.md"));
    assert!(!md(&p).contains("plan.md"));
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn the_humans_edit_is_theirs_and_a_later_rebuild_keeps_it() {
    let (id, p) = rig("human");
    write_file(&id, &p, "style.md", &note("Style", "short sentences"), None).unwrap();
    assert_eq!(rec(&p).files["style.md"].by, "human");
    // The watcher's rebuild after the write finds the bytes explained.
    rebuild_now(&id, &p).unwrap();
    assert_eq!(rec(&p).files["style.md"].by, "human");
    assert!(md(&p).contains("— short sentences · human\n"));

    let opened = read_file(&p, "style.md").unwrap();
    assert!(write_file(&id, &p, "style.md", "x", None).unwrap_err().contains("already exists"));
    fs::write(p.memory().join("style.md"), note("Style", "an agent changed it")).unwrap();
    let err = write_file(&id, &p, "style.md", "mine", Some(&opened.sha256)).unwrap_err();
    assert!(err.contains("changed since you opened it"), "{err}");
    let now = read_file(&p, "style.md").unwrap();
    write_file(&id, &p, "style.md", &note("Style", "mine again"), Some(&now.sha256)).unwrap();
    assert_eq!(rec(&p).files["style.md"].by, "human");

    for bad in ["MEMORY.md", "MEMORY.generated.md", "archive-2026-01-01.md", "../x.md", "sub/x.md", "x.txt", ".hidden.md"] {
        assert!(write_file(&id, &p, bad, "x", None).is_err(), "{bad}");
    }
    let sha = read_file(&p, "style.md").unwrap().sha256;
    assert!(delete_file(&id, &p, "style.md", "stale").unwrap_err().contains("changed since"));
    delete_file(&id, &p, "style.md", &sha).unwrap();
    assert!(!p.memory().join("style.md").exists() && !rec(&p).files.contains_key("style.md"));
    assert!(!md(&p).contains("style.md"));
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn writes_outside_the_memory_folder_or_to_the_index_are_not_attributed() {
    let (_, p) = rig("names");
    fs::write(p.memory().join("a.md"), "x").unwrap();
    assert_eq!(memory_name_of(&p, &p.memory().join("a.md")).as_deref(), Some("a.md"));
    // Through `..`: the canonical parent decides.
    assert_eq!(memory_name_of(&p, &p.logs().join("../memory/a.md")).as_deref(), Some("a.md"));
    for not in [p.memory_index(), p.memory_generated(), p.memory().join("a.txt"), p.logs().join("a.md"), p.memory().join("sub/a.md")] {
        assert!(memory_name_of(&p, &not).is_none(), "{}", not.display());
    }
    let ev = serde_json::json!({ "tool_name": "Write", "tool_input": { "file_path": "/m/a.md" } });
    assert_eq!(tool_write_target("PostToolUse", &ev), Some("/m/a.md"));
    assert_eq!(tool_write_target("PreToolUse", &ev), None, "only once it is written");
    assert_eq!(tool_write_target("PostToolUseFailure", &ev), None);
    let bash = serde_json::json!({ "tool_name": "Bash", "tool_input": { "command": "echo > /m/a.md" } });
    assert_eq!(tool_write_target("PostToolUse", &bash), None);
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn kept_block_actions_and_drift() {
    let (id, p) = rig("kept");
    fs::write(p.memory().join("a.md"), note("A", "aa")).unwrap();
    kept(&id, &p, KEPT_SET, Some(vec!["- Goal: Ship it".into(), "- Use pnpm <!-- x -->".into()]), shown(&p)).unwrap();
    assert_eq!(rec(&p).kept, vec!["- Goal: Ship it", "- Use pnpm <! -- x -- >"]);
    assert!(md(&p).starts_with(&kept_block(&rec(&p).kept)));
    let too_many: Vec<String> = (0..61).map(|i| format!("- {i}")).collect();
    assert!(kept(&id, &p, KEPT_SET, Some(too_many), shown(&p)).unwrap_err().contains("at most 60 lines"));
    assert!(kept(&id, &p, KEPT_SET, Some(vec!["a\nb".into()]), shown(&p)).is_err());

    // An agent edits the kept block: left alone, flagged, and a pin waits for the human's call.
    let drifted = md(&p).replace("- Use pnpm", "- Use npm");
    fs::write(p.memory_index(), &drifted).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert!(md(&p).contains("- Use npm"), "never rewritten by a rebuild");
    assert!(view(&p).kept_changed);
    assert!(pin(&id, &p, "a.md").unwrap_err().contains("changed outside this view"));
    kept(&id, &p, KEPT_RESTORE, None, shown(&p)).unwrap();
    assert!(md(&p).contains("- Use pnpm") && !md(&p).contains("- Use npm"));
    assert!(!view(&p).kept_changed);

    fs::write(p.memory_index(), md(&p).replace("- Use pnpm", "- Use yarn")).unwrap();
    kept(&id, &p, KEPT_ACCEPT, None, shown(&p)).unwrap();
    assert!(rec(&p).kept.iter().any(|l| l.starts_with("- Use yarn")));
    assert!(!view(&p).kept_changed);

    // A pin moves the file's line into the kept block, and the index stops listing it twice.
    pin(&id, &p, "a.md").unwrap();
    let m = md(&p);
    assert_eq!(m.matches("(a.md)").count(), 1, "{m}");
    assert!(index::split(&m).unwrap().kept.iter().any(|l| l.contains("(a.md)")));
    assert!(view(&p).files.iter().any(|f| f.name == "a.md" && f.pinned));
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn a_kept_action_on_a_block_that_changed_since_the_view_is_refused() {
    let (id, p) = rig("kept-cas");
    let before = shown(&p);
    // An agent adds a line inside the fences after the view loaded, before it refetched.
    fs::write(p.memory_index(), md(&p).replace("- Goal: Ship it\n", "- Goal: Ship it\n- Decided: Redis\n")).unwrap();
    for (action, lines) in [(KEPT_SET, Some(vec!["- Goal: Ship it".to_string(), "- mine".into()])), (KEPT_ACCEPT, None), (KEPT_RESTORE, None)] {
        let e = kept(&id, &p, action, lines, before.clone()).unwrap_err();
        assert!(e.contains("changed since you opened it"), "{action}: {e}");
        assert!(kept(&id, &p, action, None, None).is_err(), "{action}: no seen block, no action");
    }
    assert!(md(&p).contains("- Decided: Redis"), "nothing written over");
    assert!(!rec(&p).kept.iter().any(|l| l.contains("Redis")), "nothing adopted unseen");
    // Seen once more, it's the human's call.
    kept(&id, &p, KEPT_ACCEPT, None, shown(&p)).unwrap();
    assert!(rec(&p).kept.iter().any(|l| l.contains("Redis")));
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn accepting_an_over_cap_kept_block_is_refused_and_keeps_the_humans_copy() {
    let (id, p) = rig("kept-cap");
    let mine = rec(&p).kept;
    let big: Vec<String> = (0..61).map(|i| format!("- agent {i}")).collect();
    fs::write(p.memory_index(), md(&p).replace(&kept_block(&mine), &kept_block(&big))).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert!(view(&p).kept_changed);
    assert!(kept(&id, &p, KEPT_ACCEPT, None, shown(&p)).unwrap_err().contains("at most 60 lines"));
    assert_eq!(rec(&p).kept, mine);
    kept(&id, &p, KEPT_RESTORE, None, shown(&p)).unwrap();
    assert!(md(&p).starts_with(&kept_block(&mine)) && view(&p).status.is_none());
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn a_rebuild_between_a_write_and_its_hook_leaves_the_writer_first() {
    let (id, p) = rig("first");
    let f = p.memory().join("b.md");
    fs::write(&f, note("B", "bb")).unwrap();
    // Another agent's attribution, a session start or the open view rebuilds before b.md's hook.
    rebuild_now(&id, &p).unwrap();
    assert_eq!(rec(&p).files["b.md"].created_by, "unknown");
    attribute(&id, &p, "b.md", &coordinator()).unwrap();
    let r = rec(&p);
    assert_eq!((r.files["b.md"].by.as_str(), r.files["b.md"].created_by.as_str()), ("coordinator", "coordinator"));

    // A file a shell made stays first written by unknown when a tool edits it later.
    let g = p.memory().join("c.md");
    fs::write(&g, note("C", "by a shell")).unwrap();
    rebuild_now(&id, &p).unwrap();
    fs::write(&g, note("C", "edited")).unwrap();
    attribute(&id, &p, "c.md", &coordinator()).unwrap();
    assert_eq!(rec(&p).files["c.md"].created_by, "unknown");
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn unfiled_lines_keep_pin_and_delete() {
    let (id, p) = rig("unfiled");
    let mut text = md(&p);
    text.push_str("one\ntwo\nthree\n");
    fs::write(p.memory_index(), text).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert_eq!(view(&p).unfiled, vec!["one", "two", "three"]);
    unfiled(&id, &p, "one", UNFILED_KEEP).unwrap();
    assert_eq!(view(&p).unfiled, vec!["two", "three"], "kept there, no longer offered");
    assert!(fs::read_to_string(p.memory_unfiled()).unwrap().contains("one\n"));
    unfiled(&id, &p, "two", UNFILED_PIN).unwrap();
    assert!(index::split(&md(&p)).unwrap().kept.contains(&"two".to_string()));
    unfiled(&id, &p, "three", UNFILED_DELETE).unwrap();
    let u = fs::read_to_string(p.memory_unfiled()).unwrap();
    assert!(!u.contains("two\n") && !u.contains("three\n") && u.contains("one\n"), "{u}");
    assert!(view(&p).unfiled.is_empty());
    assert!(unfiled(&id, &p, "three", UNFILED_DELETE).is_err(), "gone already");
    // The removals were maiTerm's own writes: nothing new reads as unfiled.
    assert!(!rebuild_now(&id, &p).unwrap());
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn a_hive_made_before_7a_adopts_its_kept_block() {
    let (id, p) = rig("adopt");
    fs::remove_file(p.memory_record()).unwrap();
    let text = format!("{}{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n", kept_block(&["- Goal: old".into(), "- hand-kept".into()]));
    fs::write(p.memory_index(), &text).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert_eq!(rec(&p).kept, vec!["- Goal: old", "- hand-kept"]);
    assert!(!view(&p).kept_changed);
    assert_eq!(md(&p), text);
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn the_view_lists_files_with_their_provenance_and_tags() {
    let (id, p) = rig("view");
    write_file(&id, &p, "a.md", &note("A", "aa"), None).unwrap();
    fs::create_dir_all(p.memory().join("deep")).unwrap();
    fs::write(p.memory().join("deep/b.md"), note("B", "bb")).unwrap();
    rebuild_now(&id, &p).unwrap();
    let v = view(&p);
    let a = v.files.iter().find(|f| f.name == "a.md").unwrap();
    assert_eq!((a.by.as_str(), a.indexed, a.in_subfolder, a.type_.as_str()), ("human", true, false, "project"));
    let b = v.files.iter().find(|f| f.name == "deep/b.md").unwrap();
    assert!(b.in_subfolder && !b.indexed, "listed, never indexed (plan D6)");
    assert_eq!(read_file(&p, "deep/b.md").unwrap().text, note("B", "bb"));
    assert!(!md(&p).contains("b.md"));
    assert_eq!((v.budget.kept_lines, v.budget.max_lines), (1, 200));
    assert!(v.budget.lines >= 5);
    assert!(read_file(&p, "../logs/memory.json").is_err());
    let _ = fs::remove_dir_all(&p.root);
}

#[cfg(unix)]
#[test]
fn symlinks_are_never_read_or_written_through() {
    let (id, p) = rig("links");
    let outside = p.root.join("secret.md");
    fs::write(&outside, note("Secret", "not memory")).unwrap();
    std::os::unix::fs::symlink(&outside, p.memory().join("planted.md")).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert!(!md(&p).contains("Secret"), "a symlink is no memory file");
    assert!(read_file(&p, "planted.md").is_err());
    assert!(write_file(&id, &p, "planted.md", "x", None).is_err());
    assert_eq!(fs::read_to_string(&outside).unwrap(), note("Secret", "not memory"));
    let _ = fs::remove_dir_all(&p.root);
}

#[test]
fn the_kept_block_over_its_cap_leaves_memory_md_and_writes_the_generated_file() {
    let (id, p) = rig("cap");
    let big: Vec<String> = (0..61).map(|i| format!("- kept {i}")).collect();
    let text = format!("{}{INDEX_BEGIN_LINE}\n{INDEX_END_LINE}\n", kept_block(&big));
    fs::write(p.memory_index(), &text).unwrap();
    record::save(&p, &MemoryRecord { v: 1, kept: big, ..MemoryRecord::default() }).unwrap();
    fs::write(p.memory().join("a.md"), note("A", "aa")).unwrap();
    rebuild_now(&id, &p).unwrap();
    assert_eq!(md(&p), text, "untouched");
    assert!(fs::read_to_string(p.memory_generated()).unwrap().contains("(a.md)"));
    assert_eq!(view(&p).status.as_deref(), Some(index::OVER_CAP));
    // Back under the cap, a normal rebuild removes the overflow file.
    kept(&id, &p, KEPT_SET, Some(vec!["- Goal: Ship it".into()]), shown(&p)).unwrap();
    assert!(!p.memory_generated().exists());
    assert!(md(&p).contains("(a.md)") && view(&p).status.is_none());
    let _ = fs::remove_dir_all(&p.root);
}

/// FSEvents only: the watcher end to end, as `watch` drives it.
#[cfg(target_os = "macos")]
#[test]
fn a_file_written_in_memory_is_indexed_within_three_seconds() {
    let (id, p) = rig("watch");
    let (wid, wp) = (id.clone(), p.clone());
    let _w = start_watcher(&p.memory(), move || {
        let _ = rebuild_now(&wid, &wp);
    })
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    fs::write(p.memory().join("watched.md"), note("Watched", "seen")).unwrap();
    let start = std::time::Instant::now();
    while start.elapsed() < std::time::Duration::from_secs(3) && !md(&p).contains("(watched.md)") {
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    assert!(md(&p).contains("- [Watched](watched.md) — seen · unknown"), "{}", md(&p));
    let _ = fs::remove_dir_all(&p.root);
}
