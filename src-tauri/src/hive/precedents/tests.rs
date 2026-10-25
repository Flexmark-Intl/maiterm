//! Precedents against a scratch Hive folder: its global folder is its own, beside it.

use serde_json::json;

use super::*;
use crate::hive::decisions::{append, DecisionLine, BY_COORDINATOR, BY_HUMAN, EVENT_ANSWERED, EVENT_OFFER};

struct Scratch {
    base: PathBuf,
    paths: HivePaths,
}

impl Scratch {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!("hive-precedents-{}", uuid::Uuid::new_v4()));
        let paths = HivePaths::new(base.join("hive/h1"));
        Self { base, paths }
    }

    /// A coordinator's blocker the human answered: its asked line and the human's answer.
    fn human_answered(&self, id: &str, task: &str, category: &str) {
        let asked = DecisionLine {
            id: id.into(),
            hive_id: "h1".into(),
            category: category.into(),
            source: "blocker".into(),
            task_id: Some(task.into()),
            key: Some(format!("b_{task}_2026-10-24T10:00:00Z")),
            question: "Dark or light grade?".into(),
            context: Some("The brand guide allows both.".into()),
            options: vec![DecisionOption { label: "Dark".into(), recommended: true }, DecisionOption { label: "Light".into(), recommended: false }],
            recommended: Some("Dark".into()),
            at: "2026-10-24T10:00:00Z".into(),
            ..DecisionLine::default()
        };
        append(&self.paths, &asked).unwrap();
        let answered = DecisionLine {
            event: EVENT_ANSWERED.into(),
            answer: Some("Dark".into()),
            reason: Some("moody suits the brand".into()),
            by: BY_HUMAN.into(),
            at: "2026-10-24T10:05:00Z".into(),
            context: None,
            ..asked
        };
        append(&self.paths, &answered).unwrap();
    }

    fn record(&self, blocker_id: &str, rule: &str) -> Result<Precedent, String> {
        let a = parse_record_args(&json!({ "blocker_id": blocker_id, "rule": rule }))?;
        record_in(&self.paths, "h1", "Ship it", &a, "2026-10-24T11:00:00Z")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

#[test]
fn a_precedent_from_the_humans_answer_copies_every_field_and_the_rule_is_the_coordinators() {
    let s = Scratch::new();
    s.human_answered("D-00000001", "t-coord", "other");
    let p = s.record("t-coord", "When grading for this brand, choose dark, because moody is the brand").unwrap();
    assert!(is_id(&p.id), "{}", p.id);
    assert_eq!((p.hive_id.as_str(), p.hive_name.as_str(), p.decision_id.as_str()), ("h1", "Ship it", "D-00000001"));
    assert_eq!((p.category.as_str(), p.question.as_str(), p.context.as_deref()), ("other", "Dark or light grade?", Some("The brand guide allows both.")));
    assert_eq!(p.options.iter().map(|o| (o.label.as_str(), o.recommended)).collect::<Vec<_>>(), vec![("Dark", true), ("Light", false)]);
    assert_eq!((p.recommended.as_deref(), p.answer.as_str(), p.reason.as_deref()), (Some("Dark"), "Dark", Some("moody suits the brand")));
    assert_eq!((p.asked_at.as_str(), p.answered_at.as_str()), ("2026-10-24T10:00:00Z", "2026-10-24T10:05:00Z"));
    assert_eq!(p.blocker_key.as_deref(), Some("b_t-coord_2026-10-24T10:00:00Z"));
    assert_eq!((p.rule_by.as_str(), p.scope.as_str(), p.status.as_str(), p.cited, p.overridden), ("coordinator", "hive", "active", 0, 0));
    assert_eq!(get(&s.paths, &p.id), Some(p.clone()), "stored in the Hive's own folder");
    assert!(s.paths.precedents().join(format!("{}.json", p.id)).is_file());

    // The same answer named by its decision id: one precedent per decision.
    let e = s.record("D-00000001", "Another wording").unwrap_err();
    assert!(e.contains(&format!("already recorded as {}", p.id)), "{e}");
}

#[test]
fn only_an_answer_the_human_gave_can_be_recorded() {
    let s = Scratch::new();
    // The coordinator's own answer to a worker's prompt.
    let coord = DecisionLine { id: "D-c0000001".into(), event: EVENT_ANSWERED.into(), category: "permissions".into(), task_id: Some("t-w".into()), answer: Some("Yes".into()), by: BY_COORDINATOR.into(), ..DecisionLine::default() };
    append(&s.paths, &coord).unwrap();
    let e = s.record("D-c0000001", "When tests, allow").unwrap_err();
    assert!(e.contains("your own answer") && e.contains("only an answer your human gave"), "{e}");
    let e = s.record("t-w", "When tests, allow").unwrap_err();
    assert!(e.contains("No answer your human gave is logged for task t-w"), "{e}");
    // Asked and never answered.
    append(&s.paths, &DecisionLine { id: "D-a0000001".into(), question: "?".into(), ..DecisionLine::default() }).unwrap();
    assert!(s.record("D-a0000001", "x").unwrap_err().contains("isn\u{2019}t answered"));
    assert!(s.record("D-nope0000", "x").unwrap_err().contains("no decision"));
    // maiTerm's offer, answered by the human: never a precedent.
    let offer = DecisionLine { id: "D-0ffe0001".into(), event: EVENT_OFFER.into(), category: "offer".into(), task_id: Some("t-coord".into()), ..DecisionLine::default() };
    append(&s.paths, &offer).unwrap();
    append(&s.paths, &DecisionLine { event: EVENT_ANSWERED.into(), answer: Some("No".into()), by: BY_HUMAN.into(), ..offer }).unwrap();
    assert!(s.record("D-0ffe0001", "x").unwrap_err().contains("never becomes a precedent"));
    assert!(list_in(&s.paths.precedents()).is_empty());
}

#[test]
fn the_arguments_are_one_line_and_never_global() {
    let parse = |v: Value| parse_record_args(&v);
    assert!(parse(json!({ "blocker_id": "t", "rule": "x", "scope": "global" })).unwrap_err().contains("your human\u{2019}s click in the Memory view"));
    assert!(parse(json!({ "blocker_id": "t", "rule": "x", "scope": "hive" })).is_ok());
    assert!(parse(json!({ "blocker_id": "t", "rule": "When A,\nchoose B" })).unwrap_err().contains("one line"));
    assert!(parse(json!({ "blocker_id": "t", "rule": "x\u{2028}y" })).unwrap_err().contains("one line"));
    assert!(parse(json!({ "blocker_id": "t", "rule": "  " })).unwrap_err().contains("rule is required"));
    assert!(parse(json!({ "blocker_id": "t", "rule": "r".repeat(MAX_RULE + 1) })).unwrap_err().contains("under 400"));
    assert!(parse(json!({ "blocker_id": "t", "rule": "x", "label": "l".repeat(MAX_LABEL + 1) })).unwrap_err().contains("under 60"));
    assert!(parse(json!({ "rule": "x" })).unwrap_err().contains("blocker_id is required"));
    let a = parse(json!({ "blocker_id": " t-1 ", "rule": " When ⟦A⟧, choose B ", "label": " style " })).unwrap();
    assert_eq!((a.blocker_id.as_str(), a.label.as_deref()), ("t-1", Some("style")));
    assert!(!a.rule.contains('⟦'), "escaped like every agent string: {}", a.rule);
}

fn stored(s: &Scratch, dir: &Path, id: &str, category: &str, rule: &str, at: &str) -> Precedent {
    let p = Precedent { id: id.into(), hive_id: "h1".into(), hive_name: "Ship it".into(), category: category.into(), question: format!("Q {rule}"), answer: "A".into(), rule: rule.into(), created_at: at.into(), scope: if dir == s.paths.global_precedents() { SCOPE_GLOBAL.into() } else { SCOPE_HIVE.into() }, ..Precedent::default() };
    save_in(dir, &p).unwrap();
    p
}

#[test]
fn the_list_filters_by_category_and_words_this_hives_first() {
    let s = Scratch::new();
    let (local, global) = (s.paths.precedents(), s.paths.global_precedents());
    stored(&s, &local, "P-00000001", "questions", "When asked about caching, choose Redis", "2026-10-20");
    stored(&s, &local, "P-00000002", "landing", "When tests pass, land", "2026-10-22");
    let mut retired = stored(&s, &local, "P-00000003", "questions", "When asked about caching, choose Memcached", "2026-10-23");
    retired.status = STATUS_RETIRED.into();
    save_in(&local, &retired).unwrap();
    let mut other = stored(&s, &global, "P-00000004", "questions", "When CACHING matters, measure first", "2026-10-24");
    other.hive_name = "Brand site".into();
    other.hive_id = "h2".into();
    save_in(&global, &other).unwrap();

    let all = query_in(&s.paths, "h1", None, None);
    let ids: Vec<&str> = all["precedents"].as_array().unwrap().iter().map(|p| p["cite_as"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["P-00000002", "P-00000001", "P-00000004"], "this Hive's newest first, then global; never a retired one");
    assert_eq!(all["categories"], json!({ "landing": 1, "questions": 2 }));
    let g = &all["precedents"][2];
    assert_eq!((g["from_hive"].as_str(), g["rule_wording"].as_str(), g["answer_by"].as_str()), (Some("Brand site"), Some("the coordinator\u{2019}s wording"), Some("your human\u{2019}s answer")));
    assert!(all["precedents"][0].get("from_hive").is_none(), "this Hive's own name no source");

    let q = query_in(&s.paths, "h1", Some("questions"), Some("caching"));
    let ids: Vec<&str> = q["precedents"].as_array().unwrap().iter().map(|p| p["cite_as"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["P-00000001", "P-00000004"], "category and case-blind words");
    assert!(query_in(&s.paths, "h1", Some("plan"), None)["precedents"].as_array().unwrap().is_empty());

    // More than the cap: the cap.
    for i in 0..30 {
        stored(&s, &local, &format!("P-1{i:07}"), "other", "When x, y", &format!("2026-11-{i:02}"));
    }
    assert_eq!(query_in(&s.paths, "h1", None, None)["precedents"].as_array().unwrap().len(), MAX_LISTED);

    let c = counts(&s.paths);
    assert_eq!((c.hive.get("questions"), c.hive.get("other"), c.global.get("questions")), (Some(&1), Some(&30), Some(&1)), "active ones only");
}

#[test]
fn a_citation_needs_an_active_one_and_two_overrides_retire_it() {
    let s = Scratch::new();
    stored(&s, &s.paths.precedents(), "P-0000000a", "questions", "r", "1");
    stored(&s, &s.paths.global_precedents(), "P-0000000b", "questions", "g", "1");
    assert!(check_citation(&s.paths, "P-0000000c").unwrap_err().contains("no precedent P-0000000c"));
    assert!(check_citation(&s.paths, "nonsense").unwrap_err().contains("no precedent"));
    assert_eq!(check_citation(&s.paths, " P-0000000b ").unwrap().scope, SCOPE_GLOBAL, "a global one cites too");
    assert_eq!(cite(&s.paths, "P-0000000b").unwrap().cited, 1);
    assert_eq!(get_in(&s.paths.global_precedents(), "P-0000000b").unwrap().cited, 1, "counted where it lives");

    assert!(!override_(&s.paths, "P-0000000a").unwrap(), "one override: still active");
    assert!(check_citation(&s.paths, "P-0000000a").is_ok());
    assert!(override_(&s.paths, "P-0000000a").unwrap(), "the second retires it");
    let p = get(&s.paths, "P-0000000a").unwrap();
    assert_eq!((p.status.as_str(), p.overridden), (STATUS_RETIRED, 2));
    assert!(check_citation(&s.paths, "P-0000000a").unwrap_err().contains("retired"));
    assert!(!override_(&s.paths, "P-0000000a").unwrap(), "retired once");
    assert!(get(&s.paths, "P-0000000a").is_some(), "kept, never deleted by maiTerm");
}

#[test]
fn the_human_edits_deletes_and_makes_one_global() {
    let s = Scratch::new();
    s.human_answered("D-00000001", "t-coord", "questions");
    let p = s.record("D-00000001", "When grading, choose dark").unwrap();

    let e = edit(&s.paths, &p.id, "When grading this brand,\nchoose dark").unwrap_err();
    assert!(e.contains("one line"), "{e}");
    let edited = edit(&s.paths, &p.id, "Dark grades for this brand").unwrap();
    assert_eq!((edited.rule.as_str(), edited.rule_by.as_str()), ("Dark grades for this brand", "human"));

    let g = promote(&s.paths, &p.id, "2026-10-25").unwrap();
    assert_ne!(g.id, p.id);
    assert_eq!((g.scope.as_str(), g.hive_id.as_str(), g.hive_name.as_str(), g.rule.as_str()), (SCOPE_GLOBAL, "h1", "Ship it", "Dark grades for this brand"));
    assert_eq!(get_in(&s.paths.global_precedents(), &g.id), Some(g.clone()));
    assert_eq!(get_in(&s.paths.precedents(), &p.id).unwrap().promoted_to.as_deref(), Some(g.id.as_str()));
    assert!(promote(&s.paths, &p.id, "x").unwrap_err().contains("already global"));
    // Offered to the coordinator once, as the global copy.
    let ids: Vec<String> = query_in(&s.paths, "h1", None, None)["precedents"].as_array().unwrap().iter().map(|x| x["cite_as"].as_str().unwrap().to_string()).collect();
    assert_eq!(ids, vec![g.id.clone()]);
    // A global one is edited and deleted from any Hive's view; the Hive's own stays.
    edit(&s.paths, &g.id, "Dark, everywhere").unwrap();
    assert_eq!(get_in(&s.paths.global_precedents(), &g.id).unwrap().rule_by, "human");
    delete(&s.paths, &g.id).unwrap();
    assert!(get(&s.paths, &g.id).is_none());
    assert!(get(&s.paths, &p.id).is_some());
    // Its copy gone, it can go global again.
    assert!(promote(&s.paths, &p.id, "y").is_ok());
    delete(&s.paths, &p.id).unwrap();
    assert!(delete(&s.paths, &p.id).unwrap_err().contains("no precedent"));
    let v = view(&s.paths);
    assert!(v.hive.is_empty() && v.global.len() == 1);

    let mut retired = stored(&s, &s.paths.precedents(), "P-0000000e", "questions", "r", "1");
    retired.status = STATUS_RETIRED.into();
    save_in(&s.paths.precedents(), &retired).unwrap();
    assert!(promote(&s.paths, "P-0000000e", "z").unwrap_err().contains("retired"));
}

#[test]
fn a_broken_or_misnamed_file_is_skipped_and_unknown_fields_survive() {
    let s = Scratch::new();
    let dir = s.paths.precedents();
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("P-0000000f.json"), "not json").unwrap();
    std::fs::write(dir.join("P-00000010.json"), r#"{"id":"P-00000011","rule":"wrong file"}"#).unwrap();
    std::fs::write(dir.join("P-00000012.json"), r#"{"id":"P-00000012","rule":"r","later":{"x":1}}"#).unwrap();
    let all = list_in(&dir);
    assert_eq!(all.len(), 1);
    assert_eq!((all[0].status.as_str(), all[0].scope.as_str()), (STATUS_ACTIVE, SCOPE_HIVE), "missing fields default");
    cite(&s.paths, "P-00000012").unwrap();
    let raw: Value = serde_json::from_slice(&std::fs::read(dir.join("P-00000012.json")).unwrap()).unwrap();
    assert_eq!(raw["later"], json!({ "x": 1 }));
    assert!(save_in(&dir, &Precedent { id: "../x".into(), ..Precedent::default() }).is_err(), "an id is never a path");
}

#[test]
fn once_made_global_the_global_copy_is_the_only_live_one() {
    let s = Scratch::new();
    s.human_answered("D-00000001", "t-coord", "questions");
    let p = s.record("D-00000001", "When grading, choose dark").unwrap();
    let g = promote(&s.paths, &p.id, "2026-10-25").unwrap();
    // Counted once, as the brief and hivePrecedents both see it.
    let c = counts(&s.paths);
    assert!(c.hive.is_empty() && c.global.get("questions") == Some(&1), "{c:?}");
    assert_eq!(query_in(&s.paths, "h1", None, None)["categories"], json!({ "questions": 1 }));

    // The Hive's own id, from the coordinator's history, cites the global copy.
    assert_eq!(check_citation(&s.paths, &p.id).unwrap().id, g.id);
    cite(&s.paths, &p.id).unwrap();
    assert_eq!((get_in(&s.paths.global_precedents(), &g.id).unwrap().cited, get_in(&s.paths.precedents(), &p.id).unwrap().cited), (1, 0));

    // The human's rewording of the Hive's own reaches the copy the coordinator is offered.
    edit(&s.paths, &p.id, "Dark grades, always").unwrap();
    assert_eq!(query_in(&s.paths, "h1", None, None)["precedents"][0]["rule"], "Dark grades, always");

    // Two overrides, through either id, retire the copy; the Hive's own doesn't come back.
    assert!(!override_(&s.paths, &p.id).unwrap());
    assert!(override_(&s.paths, &g.id).unwrap());
    assert_eq!(get_in(&s.paths.global_precedents(), &g.id).unwrap().status, STATUS_RETIRED);
    assert!(query_in(&s.paths, "h1", None, None)["precedents"].as_array().unwrap().is_empty());
    assert!(counts(&s.paths).is_empty());
    let e = check_citation(&s.paths, &p.id).unwrap_err();
    assert!(e.contains("retired") && e.contains(&g.id), "{e}");
    assert!(promote(&s.paths, &p.id, "x").unwrap_err().contains("already global"));

    // The Hive's own deleted, the answer is still recorded: as the global copy.
    delete(&s.paths, &p.id).unwrap();
    let e = s.record("D-00000001", "Again").unwrap_err();
    assert!(e.contains(&format!("already recorded as {}", g.id)), "{e}");
    // Another Hive's global copy of a decision with the same id is no bar.
    let mut other = get_in(&s.paths.global_precedents(), &g.id).unwrap();
    other.hive_id = "h2".into();
    save_in(&s.paths.global_precedents(), &other).unwrap();
    assert!(s.record("D-00000001", "Again").is_ok());
}
