//! The human's curation against the agent tools' scratch Hive (`tools::tests::Rig`): the human's
//! statuses as answers and overrides, Export… and the paths a composer attaches.

use std::fs;

use serde_json::{json, Value};

use super::*;
use crate::hive::autonomy;
use crate::hive::gallery::tools::tests::Rig;
use crate::hive::gallery::tools::TAG;

fn status(s: &str) -> TagChange {
    TagChange { status: Some(s.into()), ..TagChange::default() }
}

async fn human(r: &Rig, ids: &[&String], change: TagChange) -> Result<Vec<Asset>, String> {
    human_tag(&r.ctx(), &r.caller(), r.gallery(), ids.iter().map(|s| s.to_string()).collect(), change).await
}

fn finals_lines(r: &Rig) -> Vec<DecisionLine> {
    decisions::read(&r.paths).into_iter().filter(|l| l.category == checkpoints::FINALS).collect()
}

fn offer_row(r: &Rig) -> Option<crate::state::Task> {
    r.data.read().windows[0].workspaces[0].tasks.iter().find(|t| t.title == autonomy::row_title(checkpoints::FINALS)).cloned()
}

#[tokio::test]
async fn the_human_sets_any_status_whatever_the_checkpoint_and_curating_alone_logs_nothing() {
    let r = Rig::new();
    let a = r.filed("a.png", "A", "a").await;
    r.set_checkpoint("finals", "ask");
    let out = human(&r, &[&a], TagChange { status: Some("final".into()), tags: vec!["Cover".into()], starred: Some(true), ..TagChange::default() }).await.unwrap();
    assert_eq!((out[0].status.as_str(), out[0].tags.clone(), out[0].starred), ("final", vec!["cover".to_string()], true));
    assert!(finals_lines(&r).is_empty(), "no recommendation, no coordinator call: nothing to log");
    assert!(r.win.st().delivered.is_empty());
    assert_eq!(*r.win.st().gallery_changed.last().unwrap(), vec![a.clone()]);

    assert!(human(&r, &[&a], status("best")).await.unwrap_err().contains("isn't a status"));
    assert!(human(&r, &[], status("final")).await.unwrap_err().contains("No assets"));
    assert!(human(&r, &[&"../x".to_string()], status("final")).await.unwrap_err().contains("isn't an asset id"));
}

/// §7.6 under Ask me: the human's pick on an asset the coordinator recommended is the answer to
/// it; five taken in a row offer Finals to the coordinator; one not taken ends the run.
#[tokio::test]
async fn picks_answer_the_coordinators_recommendations_and_five_in_a_row_offer_finals() {
    let r = Rig::new();
    r.set_checkpoint("finals", "ask");
    let mut ids = Vec::new();
    for i in 0..6 {
        ids.push(r.filed(&format!("{i}.png"), &format!("Take {i}"), &format!("bytes {i}")).await);
    }
    // One not taken first: recommended final, the human rejects it.
    r.call(&r.coord, TAG, json!({ "id": ids[5], "status": "final" })).await.unwrap();
    human(&r, &[&ids[5]], status("rejected")).await.unwrap();
    let l = finals_lines(&r).pop().unwrap();
    assert_eq!((l.event.as_str(), l.by.as_str(), l.recommended.as_deref(), l.answer.as_deref()), ("answered", "human", Some("final"), Some("rejected")));
    assert_eq!(autonomy::streak(&decisions::read(&r.paths), checkpoints::FINALS), 0);
    let (_, told) = r.win.st().delivered.last().cloned().unwrap();
    assert!(told.starts_with("[maiTerm] Your human picked finals in the gallery") && told.contains("rejected (you recommended final)"), "{told}");

    for id in &ids[..5] {
        r.call(&r.coord, TAG, json!({ "id": id, "status": "final" })).await.unwrap();
    }
    assert_eq!(open_recommendations(&decisions::read(&r.paths)).len(), 5);
    assert!(offer_row(&r).is_none());
    let four: Vec<&String> = ids[..4].iter().collect();
    human(&r, &four, status("final")).await.unwrap();
    assert_eq!(autonomy::streak(&decisions::read(&r.paths), checkpoints::FINALS), 4);
    assert!(offer_row(&r).is_none(), "four isn't five");
    human(&r, &[&ids[4]], status("final")).await.unwrap();
    assert!(open_recommendations(&decisions::read(&r.paths)).is_empty(), "each answered once");
    let row = offer_row(&r).expect("five taken in a row offer Finals to the coordinator");
    let b = row.blocker.expect("the offer is a blocker on its row");
    assert!(b.question.contains("picking finals in the gallery"), "{}", b.question);
    // Answered already: a second pick on it is the human curating, not a second answer.
    let before = finals_lines(&r).len();
    human(&r, &[&ids[0]], status("picked")).await.unwrap();
    assert_eq!(finals_lines(&r).len(), before);
}

/// §7.6 at Coordinator: the human moving an asset off the coordinator's finals call overrides
/// that decision (once), the coordinator is told, and two in a day offer Finals back.
#[tokio::test]
async fn changing_the_coordinators_finals_overrides_it_and_two_in_a_day_offer_it_back() {
    let r = Rig::new();
    let a = r.filed("a.png", "Hero", "a").await;
    let b = r.filed("b.png", "Logo", "b").await;
    r.call(&r.coord, TAG, json!({ "ids": [a.clone(), b.clone()], "status": "final" })).await.unwrap();
    human(&r, &[&a], status("final")).await.unwrap();
    assert_eq!(finals_lines(&r).iter().filter(|l| l.event == "overridden").count(), 0, "the same call isn't an override");

    human(&r, &[&a], status("rejected")).await.unwrap();
    let over: Vec<DecisionLine> = finals_lines(&r).into_iter().filter(|l| l.event == "overridden").collect();
    assert_eq!(over.len(), 1);
    assert_eq!((over[0].by.as_str(), over[0].key.as_deref(), over[0].reason.as_deref()), ("human", Some(a.as_str()), Some("Your human set it to rejected in the gallery")));
    let (_, told) = r.win.st().delivered.last().cloned().unwrap();
    assert!(told.contains("Your human changed finals you set") && told.contains("you answered final"), "{told}");
    assert!(offer_row(&r).is_none(), "one isn't two");
    human(&r, &[&a], status("final")).await.unwrap();
    assert_eq!(finals_lines(&r).iter().filter(|l| l.event == "overridden").count(), 1, "a decision is overridden once");

    human(&r, &[&b], status("")).await.unwrap();
    let row = offer_row(&r).expect("two overrides in a day offer Finals back");
    assert!(row.blocker.unwrap().question.contains("overridden the coordinator twice"));
}

/// Under Ask me a recommendation the coordinator then decides itself (Finals handed over) is
/// withdrawn: the human's later move is an override of the decision, not an answer.
#[tokio::test]
async fn a_recommendation_the_coordinator_then_decides_is_withdrawn() {
    let r = Rig::new();
    let a = r.filed("a.png", "Hero", "a").await;
    r.set_checkpoint("finals", "ask");
    r.call(&r.coord, TAG, json!({ "id": a, "status": "final" })).await.unwrap();
    r.set_checkpoint("finals", "coordinator");
    r.call(&r.coord, TAG, json!({ "id": a, "status": "final" })).await.unwrap();
    let lines = decisions::read(&r.paths);
    assert!(open_recommendations(&lines).is_empty());
    assert!(lines.iter().any(|l| l.event == "lapsed" && l.key.as_deref() == Some(a.as_str())), "{lines:?}");
    human(&r, &[&a], status("rejected")).await.unwrap();
    let last = finals_lines(&r).pop().unwrap();
    assert_eq!((last.event.as_str(), last.by.as_str()), ("overridden", "human"));
}

#[tokio::test]
async fn export_writes_anywhere_numbered_for_a_collection_and_replaces_only_when_asked() {
    let r = Rig::new();
    let a = r.filed("a.png", "Hero", "aaa").await;
    let b = r.filed("b.png", "Logo", "bbb").await;
    let out = r.base.join("Desktop");
    fs::create_dir_all(&out).unwrap();
    let g = r.gallery();
    g.collect(&CollectChange { collection: "Deck".into(), create: true, add: vec![b.clone(), a.clone()], ..CollectChange::default() }).unwrap();

    let e = export(&g, &r.paths, std::slice::from_ref(&a),None, &out.to_string_lossy(), false).unwrap();
    assert_eq!(e.names, vec!["hero.png"]);
    let e = export(&g, &r.paths, std::slice::from_ref(&a),None, &out.to_string_lossy(), false).unwrap();
    assert_eq!(e.names, vec!["hero (2).png"], "without Replace, a new file");
    fs::write(out.join("hero.png"), "edited by hand").unwrap();
    let e = export(&g, &r.paths, std::slice::from_ref(&a),None, &out.to_string_lossy(), true).unwrap();
    assert_eq!(e.names, vec!["hero.png"]);
    assert_eq!(fs::read_to_string(out.join("hero.png")).unwrap(), "aaa", "Replace overwrites");
    let e = export(&g, &r.paths, &[], Some("deck"), &out.to_string_lossy(), false).unwrap();
    assert_eq!(e.names, vec!["01-logo.png", "02-hero.png"]);
    assert!(export(&g, &r.paths, std::slice::from_ref(&a),None, &out.join("nope").to_string_lossy(), false).is_err());

    let log: Vec<Value> = fs::read_to_string(r.paths.delivery_log()).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(log.iter().map(|l| l["by"].as_str().unwrap()).collect::<Vec<_>>(), vec!["human", "human", "human (replace)", "human"]);
}

/// Two variants titled alike, exported with Replace: each its own file, neither written over the
/// other (Replace overwrites what was in the folder, never the export's own files).
#[tokio::test]
async fn export_with_replace_keeps_two_assets_titled_alike_apart() {
    let r = Rig::new();
    let a = r.filed("a.png", "Hero", "aaa").await;
    let b = r.filed("b.png", "Hero", "bbb").await;
    let out = r.base.join("Desktop");
    fs::create_dir_all(&out).unwrap();
    let e = export(&r.gallery(), &r.paths, &[a, b], None, &out.to_string_lossy(), true).unwrap();
    assert_eq!(e.names, vec!["hero.png", "hero (2).png"]);
    assert_eq!((fs::read_to_string(out.join("hero.png")).unwrap(), fs::read_to_string(out.join("hero (2).png")).unwrap()), ("aaa".to_string(), "bbb".to_string()));
}

/// Under Ask me the human can keep the status an asset has as the answer: the status buttons
/// toggle, so setting it again is the "Keep" answer, logged as that status and nothing else.
#[tokio::test]
async fn keeping_the_status_it_has_answers_a_recommendation() {
    let r = Rig::new();
    let a = r.filed("a.png", "Hero", "a").await;
    human(&r, &[&a], status("picked")).await.unwrap();
    r.set_checkpoint("finals", "ask");
    r.call(&r.coord, TAG, json!({ "id": a, "status": "final" })).await.unwrap();
    assert_eq!(open_recommendations(&decisions::read(&r.paths)).len(), 1);
    human(&r, &[&a], status("picked")).await.unwrap();
    let l = finals_lines(&r).pop().unwrap();
    assert_eq!((l.event.as_str(), l.recommended.as_deref(), l.answer.as_deref()), ("answered", Some("final"), Some("picked")));
    assert!(open_recommendations(&decisions::read(&r.paths)).is_empty());
    assert_eq!(r.gallery().get(&a).unwrap().unwrap().status, "picked", "the asset keeps its pick");
    assert_eq!(autonomy::streak(&decisions::read(&r.paths), checkpoints::FINALS), 0, "a recommendation not taken");
}

#[tokio::test]
async fn a_composer_attaches_stored_paths_of_assets_out_of_the_trash_and_emptying_keeps_a_tombstone() {
    let r = Rig::new();
    let a = r.filed("a.png", "Hero", "aaa").await;
    let b = r.filed("b.png", "Logo", "bbb").await;
    let g = r.gallery();
    let p = stored_paths(&g, &[a.clone(), b.clone()]).unwrap();
    assert_eq!(p.len(), 2);
    assert!(std::path::Path::new(&p[0]).is_file() && p[0].ends_with(&format!("{a}.png")));

    assert_eq!(g.trash(std::slice::from_ref(&b)).unwrap(), 1);
    assert!(stored_paths(&g, std::slice::from_ref(&b)).unwrap_err().contains("in the trash"));
    let file = std::path::PathBuf::from(stored_paths(&g, std::slice::from_ref(&a)).unwrap().remove(0)).with_file_name(format!("{b}.png"));
    assert!(file.is_file(), "the trash keeps the file");
    assert_eq!(g.purge_trash().unwrap(), 1);
    assert!(!file.exists(), "emptying deletes it");
    let tomb = g.get(&b).unwrap().unwrap();
    assert!(tomb.purged && tomb.title == "Logo", "the row stays");
    assert!(human(&r, &[&b], status("final")).await.unwrap_err().contains("emptied from the trash"));
}
