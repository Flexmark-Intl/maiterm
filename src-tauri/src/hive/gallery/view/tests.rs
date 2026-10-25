//! The human's gallery commands, card assets and a chat's inline assets, on a scratch gallery.

use std::fs;

use super::*;
use crate::hive::record::Hive;
use crate::state::{WindowData, Workspace};

fn scratch(tag: &str) -> PathBuf {
    let t = std::env::temp_dir().join(format!("hive-gallery-view-{tag}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&t).unwrap();
    crate::share::canon(&t)
}

fn worker_by(task: &str) -> MadeBy {
    MadeBy { kind: BY_WORKER.into(), tab_id: format!("tab-{task}"), role: "image".into(), worker_title: task.into(), badge: 1 }
}

/// An index row (its file needn't exist to be listed), filed at `ms`.
fn filed(g: &Gallery, title: &str, made_by: MadeBy, task: Option<&str>, ms: i64) -> String {
    let id = mint(ASSET_PREFIX);
    g.insert_asset(NewAsset {
        id: id.clone(),
        kind: KIND_IMAGE.into(),
        file: format!("{id}.png"),
        title: title.into(),
        made_by,
        task_id: task.map(str::to_string),
        ..NewAsset::default()
    })
    .unwrap();
    let conn = rusqlite::Connection::open(g.root().join(INDEX)).unwrap();
    conn.execute("UPDATE assets SET created_ms = ?2 WHERE id = ?1", rusqlite::params![id, ms]).unwrap();
    id
}

fn turn(id: &str, role: &str, text: &str, ts: i64) -> Value {
    json!({ "msg_id": id, "role": role, "text": text, "ts": ts })
}

fn claimed(t: &Value) -> Vec<String> {
    t.get("gallery_assets").and_then(Value::as_array).map(|a| a.iter().map(|c| c["id"].as_str().unwrap().to_string()).collect()).unwrap_or_default()
}

#[test]
fn a_gallery_add_marker_is_recognised_under_any_server_name() {
    assert!(is_gallery_add(&turn("a", "tool", "mcp__maiterm__galleryAdd(/tmp/x.png)", 0)));
    assert!(is_gallery_add(&turn("a", "tool", "mcp__maiterm-dev__galleryAdd", 0)));
    assert!(is_gallery_add(&turn("a", "tool", "galleryAdd", 0)));
    assert!(!is_gallery_add(&turn("a", "tool", "mcp__maiterm__galleryAddLater(x)", 0)));
    assert!(!is_gallery_add(&turn("a", "tool", "Bash(echo galleryAdd)", 0)));
    assert!(!is_gallery_add(&turn("a", "agent", "mcp__maiterm__galleryAdd", 0)), "only a tool turn");
}

#[test]
fn two_gallery_add_turns_split_their_assets_each_claimed_once_within_the_window() {
    let base = scratch("turns");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let t0 = 1_800_000_000_000_i64;
    let early = filed(&g, "too early", worker_by("t1"), Some("t1"), t0 - 5_000);
    let a1 = filed(&g, "a1", worker_by("t1"), Some("t1"), t0 - 1_000);
    let a2 = filed(&g, "a2", worker_by("t1"), Some("t1"), t0 + 3_000);
    let other_task = filed(&g, "theirs", worker_by("t2"), Some("t2"), t0 + 3_500);
    let b1 = filed(&g, "b1", worker_by("t1"), Some("t1"), t0 + 11_000);
    let late = filed(&g, "too late", worker_by("t1"), Some("t1"), t0 + 10_000 + TURN_AFTER_MS + 5_000);
    let mut turns = vec![
        turn("u", "user", "make two heroes", t0 - 60_000),
        turn("c1", "tool", "mcp__maiterm__galleryAdd(scratch/a.png)", t0),
        // Another call of the same message is its own line, stamped later, before either runs:
        // it doesn't close the window, nor does the agent's next text.
        turn("c1b", "tool", "Read(scratch/a.png)", t0 + 1_500),
        turn("m", "agent", "Filed the first two.", t0 + 8_000),
        turn("c2", "tool", "mcp__maiterm__galleryAdd(scratch/b.png)", t0 + 10_000),
    ];
    annotate_turns(&g, &TurnsBy::Task("t1".into()), &mut turns).unwrap();
    assert_eq!(claimed(&turns[1]), vec![a1.clone(), a2.clone()]);
    assert_eq!(claimed(&turns[4]), vec![b1.clone()]);
    assert!(claimed(&turns[0]).is_empty() && claimed(&turns[2]).is_empty() && claimed(&turns[3]).is_empty());
    let all: Vec<String> = turns.iter().flat_map(claimed).collect();
    for absent in [&early, &other_task, &late] {
        assert!(!all.contains(absent), "{absent} is outside the window or another task's");
    }
    assert_eq!(turns[1]["gallery_assets"][0]["title"], "a1");
    assert_eq!(turns[1]["gallery_assets"][0]["picture"], Value::Null, "no picture while pending");

    // Overlapping windows: a2 is in both, and is the first turn's only.
    let mut close = vec![turn("x1", "tool", "galleryAdd", t0 + 1_500), turn("x2", "tool", "galleryAdd", t0 + 4_000)];
    annotate_turns(&g, &TurnsBy::Task("t1".into()), &mut close).unwrap();
    assert_eq!(claimed(&close[0]), vec![a2.clone()]);
    assert_eq!(claimed(&close[1]), vec![b1.clone()]);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_gallery_add_sent_with_other_calls_keeps_its_assets_filed_after_their_lines() {
    let base = scratch("batch");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let t0 = 1_800_000_000_000_i64;
    // galleryAdd then Bash in one message, 4 ms apart; the asset is filed when the call runs.
    let a = filed(&g, "a", worker_by("t1"), Some("t1"), t0 + 500);
    let mut turns = vec![turn("g", "tool", "mcp__maiterm__galleryAdd(scratch/a.png)", t0), turn("b", "tool", "Bash(ls)", t0 + 4)];
    annotate_turns(&g, &TurnsBy::Task("t1".into()), &mut turns).unwrap();
    assert_eq!(claimed(&turns[0]), vec![a.clone()]);

    // Two galleryAdds and a Read in one message, the lines seconds apart, both files filed after
    // the last line: each call shows its own.
    let t1 = t0 + 600_000;
    let x = filed(&g, "x", worker_by("t1"), Some("t1"), t1 + 5_000);
    let y = filed(&g, "y", worker_by("t1"), Some("t1"), t1 + 5_100);
    let mut par = vec![
        turn("p1", "tool", "mcp__maiterm__galleryAdd(scratch/x.png)", t1),
        turn("p2", "tool", "mcp__maiterm__galleryAdd(scratch/y.png)", t1 + 1_500),
        turn("p3", "tool", "Read(notes.md)", t1 + 3_000),
    ];
    annotate_turns(&g, &TurnsBy::Task("t1".into()), &mut par).unwrap();
    assert_eq!(claimed(&par[0]), vec![x.clone()]);
    assert_eq!(claimed(&par[1]), vec![y.clone()]);

    // Sent one after the other (the first ran before the second was written): no batch.
    let t2 = t0 + 1_200_000;
    let s1 = filed(&g, "s1", worker_by("t1"), Some("t1"), t2 + 800);
    let s2 = filed(&g, "s2", worker_by("t1"), Some("t1"), t2 + 900);
    let s3 = filed(&g, "s3", worker_by("t1"), Some("t1"), t2 + 4_000);
    let mut seq = vec![turn("q1", "tool", "galleryAdd", t2), turn("q2", "tool", "mcp__maiterm__galleryAdd(scratch/s3.png)", t2 + 3_000)];
    annotate_turns(&g, &TurnsBy::Task("t1".into()), &mut seq).unwrap();
    assert_eq!(claimed(&seq[0]), vec![s1, s2]);
    assert_eq!(claimed(&seq[1]), vec![s3]);

    // A user turn closes a window: the agent's turn was over.
    let t3 = t0 + 1_800_000;
    let _later = filed(&g, "after the human spoke", worker_by("t1"), Some("t1"), t3 + 20_000);
    let mut spoke = vec![turn("r", "tool", "galleryAdd", t3), turn("h", "user", "now something else", t3 + 10_000)];
    annotate_turns(&g, &TurnsBy::Task("t1".into()), &mut spoke).unwrap();
    assert!(claimed(&spoke[0]).is_empty());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_batch_shares_its_assets_out_in_the_order_its_calls_were_sent() {
    assert_eq!(split_batch(2, &[Some(1), Some(1)]), vec![1, 1]);
    assert_eq!(split_batch(3, &[Some(1), Some(1)]), vec![1, 2], "the last takes the rest");
    assert_eq!(split_batch(5, &[None, Some(1)]), vec![4, 1], "items leave the single-path call its one");
    assert_eq!(split_batch(5, &[Some(1), None, None]), vec![1, 4, 0]);
    assert_eq!(split_batch(1, &[Some(1), Some(1)]), vec![1, 0]);
    assert_eq!(split_batch(0, &[None, None]), vec![0, 0]);
}

#[test]
fn the_coordinators_chat_takes_only_the_coordinators_assets() {
    let base = scratch("coord");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let t0 = 1_800_000_000_000_i64;
    let mine = filed(&g, "mine", MadeBy { kind: BY_COORDINATOR.into(), tab_id: "c-old".into(), role: ROLE_COORDINATOR.into(), ..MadeBy::default() }, None, t0 + 500);
    let _human = filed(&g, "dropped", MadeBy::human(), None, t0 + 600);
    let _worker = filed(&g, "worker's", worker_by("t1"), Some("t1"), t0 + 700);
    let mut turns = vec![turn("c", "tool", "mcp__maiterm__galleryAdd(home/x.png)", t0)];
    annotate_turns(&g, &TurnsBy::Coordinator, &mut turns).unwrap();
    assert_eq!(claimed(&turns[0]), vec![mine]);
    // No galleryAdd turn: nothing read, nothing added.
    let mut plain = vec![turn("b", "tool", "Bash(ls)", t0)];
    annotate_turns(&g, &TurnsBy::Coordinator, &mut plain).unwrap();
    assert!(plain[0].get("gallery_assets").is_none());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_card_shows_its_tasks_newest_two_out_of_the_trash_with_their_pictures() {
    let base = scratch("card");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let a = filed(&g, "a", worker_by("t1"), Some("t1"), 1_000);
    let b = filed(&g, "b", worker_by("t1"), Some("t1"), 2_000);
    let c = filed(&g, "c", worker_by("t1"), Some("t1"), 3_000);
    let _other = filed(&g, "d", worker_by("t2"), Some("t2"), 4_000);
    g.set_meta(&b, &MediaMeta { thumb: THUMB_READY.into(), poster: true, ..MediaMeta::default() }).unwrap();
    g.trash(std::slice::from_ref(&c)).unwrap();
    let chips = card_assets(&g, "t1");
    assert_eq!(chips.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), vec![b.as_str(), a.as_str()]);
    assert_eq!(chips[0].picture.as_deref(), Some("thumb"));
    assert_eq!(chips[1].picture, None);
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_picture_is_the_poster_else_the_waveform_and_only_once_ready() {
    let extras = |poster: bool, wave: bool| {
        let mut m = serde_json::Map::new();
        m.insert("poster".into(), json!(poster));
        m.insert("wave".into(), json!(wave));
        m
    };
    assert_eq!(picture(THUMB_READY, &extras(true, true)).as_deref(), Some("thumb"));
    assert_eq!(picture(THUMB_READY, &extras(false, true)).as_deref(), Some("wave"));
    assert_eq!(picture(THUMB_READY, &extras(false, false)), None);
    assert_eq!(picture(THUMB_PENDING, &extras(true, true)), None);
    assert_eq!(picture(THUMB_FAILED, &serde_json::Map::new()), None);
}

#[test]
fn a_hive_never_filed_into_has_no_gallery_opened_for_its_board() {
    let base = scratch("none");
    let paths = HivePaths::new(base.join("hive"));
    fs::create_dir_all(&paths.root).unwrap();
    assert!(existing("h-none", &paths).is_none());
    assert!(!paths.gallery().exists(), "a board read makes no gallery");
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn the_humans_list_shows_the_trash_only_when_asked_and_caps_its_rows() {
    let base = scratch("list");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let a = filed(&g, "a", MadeBy::human(), None, 1_000);
    let b = filed(&g, "b", MadeBy::human(), None, 2_000);
    g.trash(std::slice::from_ref(&a)).unwrap();
    let ids = |f: ListFilter| view_list(&g, f).unwrap().into_iter().map(|r| r.id).collect::<Vec<_>>();
    assert_eq!(ids(ListFilter::default()), vec![b.clone()]);
    assert_eq!(ids(ListFilter { include_trash: true, ..ListFilter::default() }), vec![b.clone(), a.clone()]);
    assert_eq!(ids(ListFilter { include_trash: true, limit: Some(1), ..ListFilter::default() }), vec![b.clone()]);
    assert_eq!(view_list(&g, ListFilter { limit: Some(999_999), ..ListFilter::default() }).unwrap().len(), 1, "capped, not refused");
    let d = detail(&g, &a).unwrap();
    assert!(d.asset.deleted_at.is_some(), "the human's detail reads the trash");
    assert!(detail(&g, "a-000000000000").is_err());
    assert!(detail(&g, "../etc").is_err());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_detail_carries_the_stored_path_and_its_lineage() {
    let base = scratch("detail");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let parent = filed(&g, "original", MadeBy::human(), None, 1_000);
    let child_id = mint(ASSET_PREFIX);
    g.insert_asset(NewAsset { id: child_id.clone(), kind: KIND_IMAGE.into(), file: format!("{child_id}.png"), title: "warmer".into(), parents: vec![parent.clone()], ..NewAsset::default() }).unwrap();
    let d = detail(&g, &child_id).unwrap();
    assert_eq!(d.path, g.files_dir().join(format!("{child_id}.png")));
    assert_eq!(d.parent_chips.iter().map(|c| c.id.clone()).collect::<Vec<_>>(), vec![parent.clone()]);
    let p = detail(&g, &parent).unwrap();
    assert_eq!(p.child_chips.iter().map(|c| c.title.clone()).collect::<Vec<_>>(), vec!["warmer".to_string()]);
    // Flattened: the view reads the asset's own fields at the top level.
    let v = serde_json::to_value(&p).unwrap();
    assert_eq!(v["id"], json!(parent));
    assert_eq!(v["children"], json!(1));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_drop_files_as_the_human_from_anywhere_and_never_merges_a_duplicate() {
    let base = scratch("drop");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let elsewhere = base.join("Downloads/mood board");
    fs::create_dir_all(&elsewhere).unwrap();
    let file = elsewhere.join("Reference Shot.JPG");
    fs::write(&file, b"jpeg bytes").unwrap();
    let out = add_human_files(&g, &[file.to_string_lossy().to_string(), file.to_string_lossy().to_string()]);
    assert!(out.refused.is_empty(), "{:?}", out.refused);
    assert_eq!(out.assets.len(), 2, "each drop is its own asset");
    let a = &out.assets[0];
    assert_eq!(a.made_by, MadeBy::human());
    assert_eq!(a.task_id, None);
    assert_eq!((a.kind.as_str(), a.title.as_str(), a.thumb.as_str()), (KIND_IMAGE, "Reference Shot", THUMB_PENDING));
    assert_eq!(fs::read(g.file_path(a)).unwrap(), b"jpeg bytes");
    assert_ne!(out.assets[0].id, out.assets[1].id);

    let refused = add_human_files(&g, &["relative/x.png".to_string(), elsewhere.to_string_lossy().to_string(), base.join("missing.png").to_string_lossy().to_string()]);
    assert!(refused.assets.is_empty());
    assert_eq!(refused.refused.len(), 3, "a relative path, a folder and a missing file: {:?}", refused.refused);
    // The gallery's own files are never filed again.
    let stored = g.file_path(a).to_string_lossy().to_string();
    let again = add_human_files(&g, &[stored]);
    assert!(again.refused[0].error.contains("in the gallery already"), "{:?}", again.refused);
    let _ = fs::remove_dir_all(&base);
}

#[cfg(unix)]
#[test]
fn a_dropped_link_files_what_it_points_at() {
    let base = scratch("link");
    let g = Gallery::open(&base.join("gallery")).unwrap();
    let real = base.join("elsewhere/clip.wav");
    fs::create_dir_all(real.parent().unwrap()).unwrap();
    fs::write(&real, b"RIFF").unwrap();
    let link = base.join("desk/clip-link.wav");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let a = add_human_file(&g, &link).unwrap();
    assert_eq!(a.kind, KIND_AUDIO);
    assert_eq!(a.source_path, real.to_string_lossy());
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn the_commands_refuse_a_workspace_the_asking_window_doesnt_hold() {
    let mut ws = Workspace::new("Launch art".into());
    ws.hive = Some(Hive { id: format!("h-{}", uuid::Uuid::new_v4().simple()), ..Hive::default() });
    let ws_id = ws.id.clone();
    let plain = Workspace::new("Plain".into());
    let plain_id = plain.id.clone();
    let mut main = WindowData::new("main".into());
    main.workspaces = vec![ws, plain];
    let other = WindowData::new("other".into());
    let data = AppData { windows: vec![main, other], ..AppData::default() };
    assert!(hive_in_window(&data, "main", &ws_id).is_ok());
    assert!(hive_in_window(&data, "other", &ws_id).is_err(), "another window's Hive");
    assert!(hive_in_window(&data, "main", &plain_id).is_err(), "not a Hive");
    assert!(hive_in_window(&data, "main", "nope").is_err());
}
