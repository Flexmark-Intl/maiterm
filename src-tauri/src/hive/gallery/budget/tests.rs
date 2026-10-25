//! The budget against the gallery tools' scratch Hive (`tools::tests::Rig`, budget $1.00) and the
//! human's settings, pure where they can be.

use std::fs;

use serde_json::json;

use super::*;
use crate::hive::gallery::model::{usd_to_micros, NewSpend};
use crate::hive::gallery::tools::tests::Rig;
use crate::hive::gallery::tools::ADD;

#[test]
fn dollars_read_as_people_write_them() {
    assert_eq!(usd(10_400_000), "$10.40");
    assert_eq!(usd(0), "$0.00");
    assert_eq!(usd(3_000), "$0.003");
    assert_eq!(usd(400), "$0.001", "a spend under a tenth of a cent still shows");
    assert_eq!(usd(9_999), "$0.010");
    assert_eq!(usd(10_000), "$0.01");
    assert_eq!(usd(1_250_005_000), "$1,250.01");
    assert_eq!(usd(100_000 * 1_000_000), "$100,000.00");
    assert_eq!(usd(-2_500_000), "-$2.50");
}

#[test]
fn the_ledger_sums_micros_without_float_drift() {
    let dir = std::env::temp_dir().join(format!("hive-budget-ledger-{}", uuid::Uuid::new_v4().simple()));
    let g = Gallery::open(&dir).unwrap();
    for _ in 0..1_000 {
        g.record_spend(NewSpend { cost_micros: usd_to_micros(0.003), tool: "fal".into(), ..NewSpend::default() }).unwrap();
    }
    assert_eq!(g.spent_micros().unwrap(), 3_000_000, "1,000 renders at $0.003 are exactly $3.00");
    assert_eq!(usd(g.spent_micros().unwrap()), "$3.00");
    drop(g);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_budget_question_starts_budget_in_any_case() {
    for yes in ["Budget: 40 more frames at $0.05?", "budget: go on?", "  BUDGET:spend $3?"] {
        assert!(is_budget_question(yes), "{yes}");
    }
    for no in ["Which budget?", "Budget for the hero shot", "Budgets: x", "", "Bud"] {
        assert!(!is_budget_question(no), "{no}");
    }
    let b = crossing_blocker(1, 1, "t");
    assert!(!is_budget_blocker(&b), "maiTerm's own action row isn't a worker's question");
}

#[test]
fn nothing_spent_is_never_over_and_the_view_carries_every_hive() {
    let hive = Hive { budget_cents: 0, ..Hive::default() };
    assert!(!view(0, &hive, 0).over, "a $0 budget with nothing spent");
    assert!(view(1, &hive, 0).over);
    let hive = Hive { budget_cents: 1_000, ..Hive::default() };
    let v = view(2_000_000, &hive, 5_100_000);
    assert_eq!(v, SpendView { spent_micros: 2_000_000, budget_micros: 10_000_000, all_hives_micros: 5_100_000, over: false });
    assert_eq!(view(10_000_000, &hive, 0).all_hives_micros, 10_000_000, "every Hive's total includes this one's");
    assert!(view(10_000_000, &hive, 0).over, "reaching the budget is crossing it");
}

/// A paid render that wasn't kept: a spend only.
fn spend(usd: f64) -> serde_json::Value {
    json!({ "title": format!("take at ${usd}"), "recipe": { "tool": "fal", "cost_usd": usd } })
}

fn budget_rows(r: &Rig) -> Vec<crate::state::Task> {
    r.data.read().windows[0].workspaces[0].tasks.iter().filter(|t| t.title == BUDGET_TASK_TITLE).cloned().collect()
}

fn notices(r: &Rig) -> Vec<String> {
    r.win.st().delivered.iter().filter(|(t, m)| *t == r.coord && m.to_lowercase().contains("budget")).map(|(_, m)| m.clone()).collect()
}

#[tokio::test]
async fn crossing_asks_the_human_once_and_a_raise_closes_it() {
    let r = Rig::new();
    let v = r.call(&r.w1, ADD, spend(0.6)).await.unwrap();
    assert_eq!(v["budget"]["over"], json!(false));
    assert!(budget_rows(&r).is_empty() && notices(&r).is_empty(), "under the budget, nothing is asked");

    // The crossing: one row on the coordinator's tab, the store told, the coordinator told.
    let v = r.call(&r.w1, ADD, spend(0.6)).await.unwrap();
    assert_eq!(v["budget"]["over"], json!(true));
    assert!(v["budget"]["rule"].as_str().unwrap().contains("stop paid generation now"), "{v}");
    let rows = budget_rows(&r);
    assert_eq!(rows.len(), 1);
    let (row, b) = (&rows[0], rows[0].blocker.as_ref().unwrap());
    assert_eq!((row.status.as_str(), row.tab_id.as_deref(), b.kind.as_str()), ("blocked", Some(r.coord.as_str()), "action"));
    assert!(b.question.contains("$1.20 of its $1.00 budget") && b.question.contains("Raise the budget"), "{}", b.question);
    assert!(r.win.st().tasks.iter().any(|t| t.title == BUDGET_TASK_TITLE), "the tasks store got the row");
    let told = notices(&r);
    assert_eq!(told.len(), 1);
    assert!(told[0].starts_with("[maiTerm] Budget reached: $1.20 of $1.00.") && told[0].contains("don't ask again"), "{told:?}");

    // A second spend over the budget is recorded and asks nothing more.
    let v = r.call(&r.w1, ADD, spend(0.3)).await.unwrap();
    assert_eq!(v["budget"]["spent_usd"], json!(1.5), "recording is never refused");
    assert_eq!((budget_rows(&r).len(), notices(&r).len()), (1, 1));
    assert_eq!(budget_rows(&r)[0].notes.len(), 1, "the row wasn't filed again");

    // The human raises it past the spend: the row closes and the coordinator hears what's left.
    let ws_id = r.data.read().windows[0].workspaces[0].id.clone();
    let spent = r.gallery().spent_micros().unwrap();
    let change = GalleryChange { budget_cents: 200, ..GalleryChange::default() };
    let hive = set_checked(&r.ctx(), "main", &ws_id, &r.hive_id, change, spent).await.unwrap();
    assert_eq!(hive.budget_cents, 200);
    let row = budget_rows(&r).pop().unwrap();
    assert_eq!((row.status.as_str(), row.blocker.is_none()), ("done", true));
    assert_eq!(row.notes.last().unwrap().text, "Budget raised by the human to $2.00");
    assert_eq!(notices(&r).last().unwrap(), "[maiTerm] Your human raised the budget to $2.00; $0.50 left.");

    // Spending past the raised budget asks again, on the same row.
    r.call(&r.w1, ADD, spend(0.6)).await.unwrap();
    let rows = budget_rows(&r);
    assert_eq!(rows.len(), 1, "the same row, reused");
    assert_eq!(rows[0].status, "blocked");
    assert!(rows[0].blocker.as_ref().unwrap().question.contains("$2.10 of its $2.00"));
    assert_eq!(notices(&r).iter().filter(|m| m.contains("Budget reached")).count(), 2);
}

#[tokio::test]
async fn a_raise_that_still_leaves_it_over_keeps_the_question_and_the_coordinator_isnt_told_its_own_crossing() {
    let r = Rig::new();
    // The coordinator's own paid render crosses it: its answer says so, so nothing is typed to it.
    let v = r.call(&r.coord, ADD, spend(1.5)).await.unwrap();
    assert_eq!(v["budget"]["over"], json!(true));
    assert_eq!(budget_rows(&r).len(), 1, "the human is still asked");
    assert!(notices(&r).is_empty(), "{:?}", notices(&r));

    let ws_id = r.data.read().windows[0].workspaces[0].id.clone();
    let change = GalleryChange { budget_cents: 120, ..GalleryChange::default() };
    set_checked(&r.ctx(), "main", &ws_id, &r.hive_id, change, 1_500_000).await.unwrap();
    assert_eq!(budget_rows(&r)[0].status, "blocked", "$1.20 doesn't cover $1.50");
    assert!(notices(&r).is_empty());
}

#[tokio::test]
async fn a_raise_after_the_human_answered_the_card_still_reaches_the_coordinator() {
    let r = Rig::new();
    r.call(&r.w1, ADD, spend(1.2)).await.unwrap();
    assert_eq!(notices(&r).len(), 1);
    // BlockerCard's "I've done it" without a raise: the row goes to Active, the blocker settled.
    {
        let mut d = r.data.write();
        let t = d.windows[0].workspaces[0].tasks.iter_mut().find(|t| t.title == BUDGET_TASK_TITLE).unwrap();
        t.status = "in_progress".into();
        t.settle_blocker();
    }
    let ws_id = r.data.read().windows[0].workspaces[0].id.clone();
    let change = GalleryChange { budget_cents: 500, ..GalleryChange::default() };
    set_checked(&r.ctx(), "main", &ws_id, &r.hive_id, change, 1_200_000).await.unwrap();
    assert_eq!(budget_rows(&r)[0].status, "done", "the answered row doesn't stay Active");
    assert_eq!(notices(&r).last().unwrap(), "[maiTerm] Your human raised the budget to $5.00; $3.80 left.");

    // Saving again with nothing over tells it nothing more.
    let change = GalleryChange { budget_cents: 600, ..GalleryChange::default() };
    set_checked(&r.ctx(), "main", &ws_id, &r.hive_id, change, 1_200_000).await.unwrap();
    assert_eq!(notices(&r).len(), 2, "{:?}", notices(&r));
}

#[tokio::test]
async fn a_raise_past_the_spend_is_announced_even_with_no_row_left() {
    let r = Rig::new();
    r.call(&r.w1, ADD, spend(1.2)).await.unwrap();
    r.data.write().windows[0].workspaces[0].tasks.retain(|t| t.title != BUDGET_TASK_TITLE);
    let ws_id = r.data.read().windows[0].workspaces[0].id.clone();
    let change = GalleryChange { budget_cents: 200, ..GalleryChange::default() };
    set_checked(&r.ctx(), "main", &ws_id, &r.hive_id, change, 1_200_000).await.unwrap();
    assert_eq!(notices(&r).last().unwrap(), "[maiTerm] Your human raised the budget to $2.00; $0.80 left.");
}

#[tokio::test]
async fn the_settings_are_the_asking_windows_only() {
    let r = Rig::new();
    let ws_id = r.data.read().windows[0].workspaces[0].id.clone();
    let e = set_gallery_settings(&r.ctx(), "other-window", &ws_id, GalleryChange::default(), None).await.unwrap_err();
    assert_eq!(e, "Workspace not found");
    let e = set_checked(&r.ctx(), "other-window", &ws_id, &r.hive_id, GalleryChange::default(), 0).await.unwrap_err();
    assert_eq!(e, "Workspace not found");
    let e = set_checked(&r.ctx(), "main", &ws_id, "h-another", GalleryChange::default(), 0).await.unwrap_err();
    assert_eq!(e, "Workspace not found", "a Hive replaced since the check");
}

#[test]
fn the_budget_and_hosts_are_checked() {
    assert!(check_budget(0).is_ok() && check_budget(MAX_BUDGET_CENTS).is_ok());
    assert!(check_budget(MAX_BUDGET_CENTS + 1).unwrap_err().contains("$100,000.00"));

    assert_eq!(normalize_host("  Queue.FAL.run. ").unwrap(), "queue.fal.run");
    assert_eq!(normalize_host("*.fal.media").unwrap(), "*.fal.media");
    let refused = |h: &str, says: &str| {
        let e = normalize_host(h).unwrap_err();
        assert!(e.contains(says), "{h}: {e}");
    };
    refused("", "empty");
    refused("https://fal.run", "bare host");
    refused("fal.run:443", "bare host");
    refused("fal.run/x", "bare host");
    refused("[::1]", "bare host");
    refused("ai", "needs a dot");
    refused("*.ai", "needs a dot");
    refused("10.0.0.7", "IP address");
    refused("printer.local", "localhost");
    refused("api.localhost", "localhost");
    refused("fal_run.com", "isn't a host name");
    refused("-x.com", "isn't a host name");
    refused("*.*.x.com", "isn't a host name");

    assert!(host_covers("*.fal.media", "v3.fal.media") && host_covers("fal.media", "fal.media"));
    assert!(!host_covers("*.fal.media", "fal.media") && !host_covers("*.fal.media", "evilfal.media") && !host_covers("fal.media", "v3.fal.media"));

    let remotes = vec!["github.com".to_string(), "git.fal.ai".to_string()];
    let ok = check_hosts(&["fal.run".into(), "FAL.run".into(), "api.kie.ai".into()], &remotes).unwrap();
    assert_eq!(ok, vec!["fal.run", "api.kie.ai"], "normalized and once each");
    assert!(check_hosts(&["github.com".into()], &remotes).unwrap_err().contains("git remote"));
    assert!(check_hosts(&["*.fal.ai".into()], &remotes).unwrap_err().contains("reaches git.fal.ai"), "a wildcard over a remote");
    let many: Vec<String> = (0..=MAX_GENERATOR_HOSTS).map(|i| format!("h{i}.example.com")).collect();
    assert!(check_hosts(&many, &[]).unwrap_err().contains("At most 50"));
}

#[test]
fn output_folders_exist_and_stay_clear_of_the_hive_and_app_data() {
    let base = crate::share::canon(&std::env::temp_dir()).join(format!("hive-budget-folders-{}", uuid::Uuid::new_v4().simple()));
    let (comfy, app_data) = (base.join("ComfyUI").join("output"), base.join("appdata"));
    let hive_root = app_data.join("hive").join("h1");
    for d in [&comfy, &hive_root] {
        fs::create_dir_all(d).unwrap();
    }
    let s = |p: &std::path::Path| p.to_string_lossy().to_string();
    let ok = check_output_folders(&[s(&comfy), format!("{}/", s(&comfy)), "  ".into()], &hive_root, Some(&app_data)).unwrap();
    assert_eq!(ok, vec![s(&comfy)], "canonical, once, blanks dropped");
    assert!(check_output_folders(&[s(&base.join("gone"))], &hive_root, Some(&app_data)).unwrap_err().contains("No folder"));
    assert!(check_output_folders(&["relative/out".into()], &hive_root, Some(&app_data)).unwrap_err().contains("No folder"));
    assert!(check_output_folders(&[s(&hive_root)], &hive_root, Some(&app_data)).unwrap_err().contains("Hive's own folder"));
    assert!(check_output_folders(&[s(&base)], &hive_root, Some(&app_data)).unwrap_err().contains("Hive's own folder"), "one holding it");
    let beside = app_data.join("other");
    fs::create_dir_all(&beside).unwrap();
    assert!(check_output_folders(&[s(&beside)], &hive_root, Some(&app_data)).unwrap_err().contains("maiTerm's own data folder"));
    let many: Vec<String> = (0..=MAX_OUTPUT_FOLDERS)
        .map(|i| {
            let d = base.join(format!("out{i}"));
            fs::create_dir_all(&d).unwrap();
            s(&d)
        })
        .collect();
    assert!(check_output_folders(&many, &hive_root, Some(&app_data)).unwrap_err().contains("At most 20"));
    let _ = fs::remove_dir_all(&base);
}

#[test]
fn a_row_is_filed_once_while_open_and_closed_only_by_a_covering_raise() {
    let mut ws = crate::state::Workspace::new("H".into());
    let c = Some("coord");
    assert!(file_crossing(&mut ws, c, 900_000, 1_000_000, "t0").is_none(), "under the budget");
    assert!(file_crossing(&mut ws, c, 1_000_000, 1_000_000, "t1").is_some());
    assert!(file_crossing(&mut ws, c, 2_000_000, 1_000_000, "t2").is_none(), "one question while it stands");
    assert!(!close_on_raise(&mut ws, c, 2_000_000, 1_500_000, "t3"), "still over");
    assert!(close_on_raise(&mut ws, c, 2_000_000, 2_500_000, "t4"));
    assert!(!close_on_raise(&mut ws, c, 2_000_000, 3_000_000, "t5"), "nothing open");
    ws.tasks[0].status = "in_progress".into();
    assert!(close_on_raise(&mut ws, c, 2_000_000, 3_000_000, "t5"), "an answered row not yet done closes too");
    assert_eq!(ws.tasks[0].status, "done");
    assert!(file_crossing(&mut ws, c, 3_000_000, 2_500_000, "t6").is_some(), "crossing again asks again");
    assert_eq!(ws.tasks.len(), 1);
    assert!(file_crossing(&mut ws, Some("another-coord"), 3_000_000, 2_500_000, "t7").is_some(), "rows are per coordinator tab");
}
