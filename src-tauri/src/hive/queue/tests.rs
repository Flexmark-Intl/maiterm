use super::*;

const T0: i64 = 1_800_000_000_000;

fn spec(hive: &str, task: &str, worker: &str, needs: &[&str]) -> JobSpec {
    JobSpec {
        hive_id: hive.into(),
        workspace_id: format!("ws-{hive}"),
        window: "main".into(),
        task_id: task.into(),
        worker_slug: worker.into(),
        folder: "app".into(),
        argv: vec!["cargo".into(), "test".into()],
        entry: "cargo test".into(),
        needs: needs.iter().map(|s| s.to_string()).collect(),
        ..JobSpec::default()
    }
}

fn ids(jobs: &[Job]) -> Vec<String> {
    jobs.iter().map(|j| j.id.clone()).collect()
}

/// Queued, started, armed on `pty`, begun: a running job.
fn running(q: &mut Queue, s: JobSpec, pty: &str, now: i64) -> String {
    let id = q.enqueue(s, now).unwrap().id;
    assert!(ids(&q.schedule(T0)).contains(&id), "{id} starts");
    q.arm(&id, &format!("tab-{pty}"), pty).unwrap();
    assert_eq!(q.osc(pty, 'B', None, now), Osc::Began(id.clone()));
    id
}

#[test]
fn ids_and_needs_are_checked_names() {
    let id = new_id(|_| false);
    assert!(is_job_id(&id), "{id}");
    for bad in ["j-1234567", "j-123456789", "x-12345678", "j-1234567G", "j-ABCDEF12"] {
        assert!(!is_job_id(bad), "{bad}");
    }
    assert_eq!(expand_needs(&["cargo".into(), "migrate:{worker}".into(), "cargo".into()], "auth").unwrap(), vec!["cargo", "migrate:auth"]);
    for bad in ["migrate:{branch}", "Cargo", "a b", "x;y", "", "$(id)"] {
        assert!(expand_needs(&[bad.into()], "auth").is_err(), "{bad:?}");
    }
    let mut q = Queue::default();
    assert!(q.enqueue(JobSpec { argv: vec![], ..spec("h1", "t1", "a", &[]) }, T0).is_err(), "no command");
    assert!(q.enqueue(JobSpec { task_id: String::new(), ..spec("h1", "t1", "a", &[]) }, T0).is_err(), "no worker");
    let j = q.enqueue(JobSpec { timeout_s: 10 * MAX_TIMEOUT_S, ..spec("h1", "t1", "a", &[]) }, T0).unwrap();
    assert_eq!((j.state.as_str(), j.timeout_s), (QUEUED, MAX_TIMEOUT_S));
    assert_eq!(q.enqueue(spec("h1", "t1", "a", &[]), T0).unwrap().timeout_s, DEFAULT_TIMEOUT_S);
}

#[test]
fn higher_priority_first_then_first_queued() {
    let mut q = Queue::default();
    let a = q.enqueue(spec("h1", "t1", "a", &["tauri-dev"]), T0).unwrap().id;
    let b = q.enqueue(JobSpec { priority: 5, ..spec("h1", "t2", "b", &["tauri-dev"]) }, T0 + 1).unwrap().id;
    let c = q.enqueue(spec("h1", "t3", "c", &["tauri-dev"]), T0 + 2).unwrap().id;
    assert_eq!(ids(&q.schedule(T0)), vec![b.clone()], "one tauri-dev, the higher priority takes it");
    assert!(q.schedule(T0).is_empty(), "nothing else has room");
    q.fail_start(&b, NEEDS_INTEGRATION, T0 + 3).unwrap();
    assert_eq!(ids(&q.schedule(T0)), vec![a.clone()]);
    q.fail_start(&a, NEEDS_INTEGRATION, T0 + 4).unwrap();
    assert_eq!(ids(&q.schedule(T0)), vec![c]);
}

#[test]
fn jobs_that_dont_compete_run_in_parallel_and_pools_cap_across_hives() {
    let mut q = Queue::default();
    let c1 = q.enqueue(spec("h1", "t1", "a", &["cargo"]), T0).unwrap().id;
    let e1 = q.enqueue(spec("h1", "t2", "b", &["e2e"]), T0 + 1).unwrap().id;
    let c2 = q.enqueue(spec("h2", "t3", "c", &["cargo"]), T0 + 2).unwrap().id;
    let c3 = q.enqueue(spec("h2", "t4", "d", &["cargo"]), T0 + 3).unwrap().id;
    let free = q.enqueue(spec("h1", "t5", "e", &[]), T0 + 4).unwrap().id;
    let started = ids(&q.schedule(T0));
    assert_eq!(started, vec![c1.clone(), e1, c2, free], "cargo's two, the e2e beside them, a job with no needs");
    assert!(!started.contains(&c3), "a third cargo waits, whichever Hive it's from");
    q.fail_start(&c1, "x", T0).unwrap();
    assert_eq!(ids(&q.schedule(T0)), vec![c3]);
}

#[test]
fn a_hive_can_only_lower_its_own_share_of_a_pool() {
    let mut q = Queue::default();
    let shares: BTreeMap<String, u32> = [("cargo".to_string(), 1), ("e2e".to_string(), 9)].into();
    let mine = |task: &str, needs: &[&str]| JobSpec { shares: shares.clone(), ..spec("h1", task, task, needs) };
    let a = q.enqueue(mine("a", &["cargo"]), T0).unwrap().id;
    let b = q.enqueue(mine("b", &["cargo"]), T0 + 1).unwrap().id;
    let other = q.enqueue(spec("h2", "c", "c", &["cargo"]), T0 + 2).unwrap().id;
    let e1 = q.enqueue(mine("d", &["e2e"]), T0 + 3).unwrap().id;
    let e2 = q.enqueue(mine("e", &["e2e"]), T0 + 4).unwrap().id;
    let started = ids(&q.schedule(T0));
    assert_eq!(started, vec![a, other, e1], "h1 holds one cargo; h2 takes the other; 9 e2e is still the pool's one");
    assert!(!started.contains(&b) && !started.contains(&e2));
}

#[test]
fn a_migration_namespace_is_exclusive_and_the_hives_own() {
    let mut q = Queue::default();
    let shares: BTreeMap<String, u32> = [("migrate:auth".to_string(), 3), ("db".to_string(), 2)].into();
    let job = |hive: &str, task: &str, worker: &str| JobSpec { shares: shares.clone(), ..spec(hive, task, worker, &["migrate:{worker}"]) };
    let a1 = q.enqueue(job("h1", "t1", "auth"), T0).unwrap().id;
    let a2 = q.enqueue(job("h1", "t2", "auth"), T0 + 1).unwrap().id;
    let b = q.enqueue(job("h1", "t3", "billing"), T0 + 2).unwrap().id;
    let other_hive = q.enqueue(job("h2", "t4", "auth"), T0 + 3).unwrap().id;
    let started = ids(&q.schedule(T0));
    assert_eq!(started, vec![a1.clone(), b, other_hive], "one migration per namespace, per Hive; settings can't widen it");
    assert!(!started.contains(&a2));
    assert_eq!(q.job(&a1).unwrap().needs, vec!["migrate:auth"]);
    // The Hive's own resources take its settings; an unknown one is one at a time.
    let d1 = q.enqueue(JobSpec { shares: shares.clone(), ..spec("h1", "t5", "x", &["db"]) }, T0 + 4).unwrap().id;
    let d2 = q.enqueue(JobSpec { shares: shares.clone(), ..spec("h1", "t6", "y", &["db"]) }, T0 + 5).unwrap().id;
    let d3 = q.enqueue(JobSpec { shares: shares.clone(), ..spec("h1", "t7", "z", &["db"]) }, T0 + 6).unwrap().id;
    let u1 = q.enqueue(spec("h1", "t8", "u", &["gpu-ish"]), T0 + 7).unwrap().id;
    let u2 = q.enqueue(spec("h1", "t9", "v", &["gpu-ish"]), T0 + 8).unwrap().id;
    let started = ids(&q.schedule(T0));
    assert_eq!(started, vec![d1, d2, u1]);
    assert!(!started.contains(&d3) && !started.contains(&u2));
}

#[test]
fn only_a_d_after_the_armed_b_is_the_exit() {
    let mut q = Queue::default();
    let id = q.enqueue(spec("h1", "t1", "a", &["cargo"]), T0).unwrap().id;
    q.schedule(T0);
    assert_eq!(q.osc("pty-1", 'D', Some(0), T0), Osc::None, "an unarmed PTY is nobody's");
    q.arm(&id, "tab-1", "pty-1").unwrap();
    assert_eq!(q.osc("pty-1", 'D', Some(0), T0), Osc::None, "the fresh shell's first-prompt D;0 comes before the B");
    assert_eq!(q.osc("pty-1", 'A', None, T0), Osc::None);
    assert_eq!(q.osc("pty-1", 'B', None, T0 + 10), Osc::Began(id.clone()));
    let j = q.job(&id).unwrap();
    assert_eq!((j.state.as_str(), j.began, j.started_at), (RUNNING, true, Some(T0 + 10)));
    assert_eq!(q.osc("pty-1", 'B', None, T0 + 11), Osc::None, "only the first B begins it");
    assert_eq!(q.osc("pty-other", 'D', Some(0), T0), Osc::None);
    assert_eq!(q.osc("pty-1", 'D', Some(0), T0 + 20), Osc::Exit { job: id.clone(), code: Some(0) });
    // The job printed a D of its own while its line still ran: no exit mark, so whatever it
    // says, it isn't the exit, and the job keeps its resources.
    for code in [Some(0), Some(1), None] {
        assert!(q.complete(&id, code, Marks::None, T0 + 20).is_none(), "{code:?}");
    }
    assert_eq!(q.job(&id).unwrap().state, RUNNING);
    assert!(q.holders.get("cargo").is_some_and(|v| v.contains(&id)), "it still holds cargo");
    // A D;0 after a failed line (a background child printed it): no pass mark, not the exit.
    assert!(q.complete(&id, Some(0), Marks::Exited, T0 + 20).is_none());
    // A pass is exit 0, whatever the D beside it says.
    let ended = q.complete(&id, Some(7), Marks::Passed, T0 + 30).unwrap();
    assert_eq!((ended.job.state.as_str(), ended.job.exit_code, ended.job.ended_at, ended.stop), (DONE, Some(0), Some(T0 + 30), false));
    assert_eq!(q.osc("pty-1", 'D', Some(0), T0 + 31), Osc::None, "its PTY is free again");
    assert!(q.complete(&id, Some(1), Marks::Passed, T0 + 32).is_none(), "ended once");
}

#[test]
fn a_job_whose_shell_dies_ends_and_frees_what_it_held() {
    let mut q = Queue::default();
    let a = running(&mut q, spec("h1", "t1", "a", &["tauri-dev"]), "p1", T0);
    let b = q.enqueue(spec("h1", "t2", "b", &["tauri-dev"]), T0).unwrap().id;
    assert!(q.schedule(T0).is_empty());
    assert!(q.pty_closed("p-other", T0).is_none(), "a PTY that runs no job");
    let e = q.pty_closed("p1", T0 + 5).unwrap();
    assert_eq!((e.job.state.as_str(), e.job.stopped_by.as_deref(), e.stop), (INTERRUPTED, None, false), "never a pass, nobody's stop, nothing to ^C");
    assert_eq!(e.job.id, a);
    assert_eq!(ids(&q.schedule(T0)), vec![b.clone()]);
    // Armed but never begun: a failed start.
    q.arm(&b, "tab-p2", "p2").unwrap();
    assert_eq!(q.pty_closed("p2", T0).unwrap().job.state, INVALID);
    // An interrupted job waiting on its ^C'd D only drops its mapping.
    let c = running(&mut q, spec("h1", "t3", "c", &[]), "p3", T0);
    q.interrupt(&c, BY_TIMEOUT, "x", T0).unwrap();
    assert!(q.pty_closed("p3", T0).is_none());
    assert_eq!(q.osc("p3", 'D', Some(130), T0), Osc::None);
}

#[test]
fn a_failing_exit_needs_no_mark_and_frees_its_resources() {
    let mut q = Queue::default();
    let a = running(&mut q, spec("h1", "t1", "a", &["tauri-dev"]), "p1", T0);
    let b = q.enqueue(spec("h1", "t2", "b", &["tauri-dev"]), T0).unwrap().id;
    assert!(q.schedule(T0).is_empty());
    let ended = q.complete(&a, Some(101), Marks::Exited, T0 + 5).unwrap();
    assert_eq!((ended.job.state.as_str(), ended.job.exit_code), (DONE, Some(101)));
    assert_eq!(ids(&q.schedule(T0)), vec![b]);
}

#[test]
fn a_job_with_no_shell_integration_is_invalid_never_a_pass() {
    let mut q = Queue::default();
    let id = q.enqueue(spec("h1", "t1", "a", &["cargo"]), T0).unwrap().id;
    q.schedule(T0);
    let ended = q.fail_start(&id, NEEDS_INTEGRATION, T0 + 1).unwrap();
    assert_eq!((ended.job.state.as_str(), ended.job.error.as_deref(), ended.job.exit_code), (INVALID, Some(NEEDS_INTEGRATION), None));
    // A window that wakes after its start was given up is refused before it types.
    assert!(q.arm(&id, "tab", "pty").is_err());
    assert!(!ended.stop, "never armed, so nothing was typed");
    let r = running(&mut q, spec("h1", "t2", "b", &[]), "p2", T0);
    assert!(q.fail_start(&r, "late", T0).is_none(), "a job that began isn't a failed start");
    assert!(q.arm(&r, "tab", "pty-again").is_err(), "armed once");
    // Armed and given up (the window never confirmed its B): its line may be running, so ^C it.
    let s = q.enqueue(spec("h1", "t3", "c", &[]), T0).unwrap().id;
    q.schedule(T0);
    q.arm(&s, "tab-s", "ps").unwrap();
    let e = q.fail_start(&s, "the shell never ran the job's line", T0).unwrap();
    assert!(e.stop && e.job.state == INVALID);
    assert!(q.pty_closed("ps", T0).is_none(), "its tab closing only drops the mapping");
}

#[test]
fn a_job_times_out_from_its_b_and_is_interrupted() {
    let mut q = Queue::default();
    let id = running(&mut q, JobSpec { timeout_s: 60, ..spec("h1", "t1", "a", &["cargo"]) }, "p1", T0);
    assert!(q.timed_out(T0 + 59_999).is_empty());
    assert_eq!(q.timed_out(T0 + 60_000), vec![id.clone()]);
    let ended = q.interrupt(&id, BY_TIMEOUT, "it ran past its timeout", T0 + 60_000).unwrap();
    assert!(ended.stop, "it is still in its tab: ^C it");
    assert_eq!((ended.job.state.as_str(), ended.job.stopped_by.as_deref()), (INTERRUPTED, Some(BY_TIMEOUT)));
    // Its ^C'd D comes later: the code is recorded, nothing ends twice.
    assert_eq!(q.osc("p1", 'D', Some(130), T0 + 61_000), Osc::Exit { job: id.clone(), code: Some(130) });
    // A ^C cuts the line before its marks: an interrupted job's D needs none.
    assert!(q.complete(&id, Some(130), Marks::None, T0 + 61_000).is_none());
    assert_eq!(q.job(&id).unwrap().exit_code, Some(130));
    assert_eq!(q.job(&id).unwrap().state, INTERRUPTED, "never a pass");
    assert_eq!(q.osc("p1", 'D', Some(0), T0 + 62_000), Osc::None, "unmapped after it");
    // A queued job is just dropped.
    let queued = q.enqueue(spec("h1", "t2", "b", &["tauri-dev"]), T0).unwrap().id;
    let blocker = q.enqueue(spec("h1", "t3", "c", &["tauri-dev"]), T0 - 1).unwrap().id;
    q.schedule(T0);
    let e = q.interrupt(&queued, BY_HUMAN, "x", T0).unwrap();
    assert!(!e.stop);
    assert!(q.job(&blocker).unwrap().holds());
}

#[test]
fn leases_hold_a_stop_until_free_and_stop_anyway_interrupts() {
    let mut q = Queue::default();
    let key = "svc-web@checkout:/wt/auth".to_string();
    let lease = |task: &str| JobSpec { leases: vec![key.clone()], ..spec("h1", task, task, &[]) };
    assert_eq!(q.lease_state(&[key.clone()])[0].job, None);
    let held = HeldAction {
        id: String::new(),
        copy_key: key.clone(),
        workspace_id: "ws-h1".into(),
        window: "main".into(),
        action: "retarget".into(),
        to: Some("branch:x".into()),
        by: "agent".into(),
        job: String::new(),
    };
    assert!(q.hold(held.clone()).is_err(), "free: the caller goes ahead");
    let a = running(&mut q, lease("a"), "p1", T0);
    let b = running(&mut q, lease("b"), "p2", T0);
    let st = &q.lease_state(&[key.clone()])[0];
    assert_eq!((st.job.as_ref().map(|j| j.id.clone()), st.holders), (Some(a.clone()), 2), "two e2e runs share one dev server");
    let h = q.hold(held.clone()).unwrap();
    assert_eq!(h.job, a, "queued behind the job holding it");
    assert!(h.id.starts_with("h-"));
    let again = q.hold(held.clone()).unwrap();
    assert_ne!(again.id, h.id, "asking again replaces the wait");
    // While a stop waits on the copy, a new run doesn't join it: Run when free goes first.
    let late = q.enqueue(lease("late"), T0).unwrap().id;
    assert!(q.schedule(T0).is_empty(), "a held stop isn't starved by the next run");
    assert!(q.complete(&a, Some(1), Marks::Exited, T0).unwrap().freed.is_empty(), "b still holds it");
    let ended = q.complete(&b, Some(0), Marks::Passed, T0).unwrap();
    assert_eq!(ended.freed.len(), 1);
    assert_eq!(ended.freed[0].id, again.id);
    assert_eq!(ended.freed[0].to.as_deref(), Some("branch:x"));
    assert_eq!(q.lease_state(&[key.clone()])[0].holders, 0);
    // The held action is out of Rust and on its way to the window: the copy takes no lease
    // until the window says it's done, or the handoff runs out, so the stop isn't refused.
    assert!(q.schedule(T0 + 1).is_empty());
    assert!(q.schedule(T0 + LEASE_HANDOFF_MS - 1).is_empty());
    q.lease_done(&key);
    assert_eq!(ids(&q.schedule(T0 + 2)), vec![late.clone()]);
    let other = q.enqueue(lease("other"), T0).unwrap().id;
    q.arm(&late, "tab-l", "pl").unwrap();
    assert_eq!(q.osc("pl", 'B', None, T0), Osc::Began(late.clone()));
    q.hold(held.clone()).unwrap();
    q.complete(&late, Some(1), Marks::Exited, T0 + 3).unwrap();
    assert!(q.schedule(T0 + 3 + LEASE_HANDOFF_MS - 1).is_empty());
    assert_eq!(ids(&q.schedule(T0 + 3 + LEASE_HANDOFF_MS)), vec![other.clone()], "a window that never answers doesn't wedge the copy");
    q.interrupt(&other, BY_HUMAN, "x", T0).unwrap();

    let c = running(&mut q, lease("c"), "p3", T0);
    let interrupted = q.override_lease(&key, T0 + 5);
    assert_eq!(interrupted.len(), 1);
    let e = &interrupted[0];
    assert_eq!((e.job.id.as_str(), e.job.state.as_str(), e.job.stopped_by.as_deref(), e.stop), (c.as_str(), INTERRUPTED, Some(BY_HUMAN), true));
    assert_eq!(q.lease_state(&[key.clone()])[0].job, None);
    // Stop anyway's own stop runs before anything leases the copy again.
    q.enqueue(lease("d"), T0).unwrap();
    assert!(q.schedule(T0 + 6).is_empty());
    q.lease_done(&key);
    assert_eq!(q.schedule(T0 + 6).len(), 1);
}

#[test]
fn releasing_a_worker_drops_its_queue_and_interrupts_its_runs() {
    let mut q = Queue::default();
    let r = running(&mut q, spec("h1", "t1", "a", &["tauri-dev"]), "p1", T0);
    let w = q.enqueue(spec("h1", "t1", "a", &["tauri-dev"]), T0 + 1).unwrap().id;
    let other = q.enqueue(spec("h1", "t2", "b", &["tauri-dev"]), T0 + 2).unwrap().id;
    let ended = q.release_worker("t1", BY_RELEASE, "its worker stopped", T0 + 3);
    let got: Vec<(String, bool)> = ended.iter().map(|e| (e.job.id.clone(), e.stop)).collect();
    assert_eq!(got, vec![(r, true), (w, false)]);
    assert!(ended.iter().all(|e| e.job.state == INTERRUPTED && e.job.stopped_by.as_deref() == Some(BY_RELEASE)));
    assert_eq!(ids(&q.schedule(T0)), vec![other], "a suspended worker's job never holds tauri-dev");
    assert!(q.active());
}

#[test]
fn memory_pressure_starts_nothing_and_drops_the_heavy_pools_to_one() {
    let mut q = Queue::default();
    let a = q.enqueue(spec("h1", "t1", "a", &["cargo"]), T0).unwrap().id;
    let b = q.enqueue(spec("h1", "t2", "b", &[]), T0 + 1).unwrap().id;
    assert!(q.set_gate(Gate::Hold));
    assert!(!q.set_gate(Gate::Hold), "no change, no event");
    assert!(q.schedule(T0).is_empty(), "level 2 or under 25% free: nothing new, not even a job with no needs");
    q.set_gate(Gate::Critical);
    assert!(q.schedule(T0).is_empty());
    assert_eq!(pool_capacity("cargo", Gate::Critical), Some(1));
    assert_eq!(pool_capacity("cargo", Gate::Open), Some(2));
    assert_eq!(pool_capacity("migrate:x", Gate::Critical), None);
    q.set_gate(Gate::Open);
    assert_eq!(ids(&q.schedule(T0)), vec![a, b]);
}

#[test]
fn the_heavy_pools_drop_to_one_for_jobs_already_counted() {
    // At Critical the capacity is one: a second cargo can't start beside a running one even if
    // starts were allowed (has_room is what the gate's capacity feeds).
    let mut q = Queue::default();
    running(&mut q, spec("h1", "t1", "a", &["cargo"]), "p1", T0);
    let b = q.enqueue(spec("h1", "t2", "b", &["cargo"]), T0).unwrap();
    q.gate = Gate::Critical;
    assert!(!q.has_room(q.job(&b.id).unwrap()));
    q.gate = Gate::Open;
    assert!(q.has_room(q.job(&b.id).unwrap()));
}

#[test]
fn the_coordinator_moves_a_job_up_or_to_the_top() {
    let mut q = Queue::default();
    let j: Vec<String> = (0..4).map(|i| q.enqueue(spec("h1", &format!("t{i}"), "w", &["tauri-dev"]), T0 + i).unwrap().id).collect();
    let order = |q: &Queue| q.order().into_iter().map(|i| q.jobs[i].id.clone()).collect::<Vec<_>>();
    q.move_job(&j[3], Move::Up).unwrap();
    assert_eq!(order(&q), vec![j[0].clone(), j[1].clone(), j[3].clone(), j[2].clone()]);
    q.move_job(&j[2], Move::Top).unwrap();
    assert_eq!(order(&q), vec![j[2].clone(), j[0].clone(), j[1].clone(), j[3].clone()]);
    q.move_job(&j[2], Move::Top).unwrap();
    assert_eq!(order(&q)[0], j[2], "already first");
    assert!(q.move_job("j-00000000", Move::Up).is_err());
    assert_eq!((q.place(&j[2]), q.place(&j[3])), (Some(1), Some(4)), "hiveRun's place in line");
    assert_eq!(ids(&q.schedule(T0)), vec![j[2].clone()]);
    assert_eq!((q.place(&j[2]), q.place(&j[0])), (None, Some(1)), "a started job has left the line");
}

#[test]
fn a_worker_s_service_copy_is_found_by_its_task_and_service() {
    let mut q = Queue::default();
    let svc = |task: &str, sid: Option<&str>| JobSpec { service_id: sid.map(String::from), ..spec("h1", task, "w", &[]) };
    let a = q.enqueue(svc("t1", Some("web")), T0).unwrap().id;
    q.enqueue(svc("t1", None), T0).unwrap();
    q.enqueue(svc("t2", Some("web")), T0).unwrap();
    assert_eq!(q.active_service_job("t1", "web").map(|j| j.id.clone()), Some(a.clone()));
    assert!(q.active_service_job("t1", "api").is_none());
    q.interrupt(&a, BY_WORKER, "the worker stopped it", T0).unwrap();
    assert!(q.active_service_job("t1", "web").is_none(), "only an active one");
}

#[test]
fn a_worker_s_finished_job_tabs_close_when_its_next_job_starts() {
    let mut q = Queue::default();
    let a = running(&mut q, spec("h1", "t1", "a", &[]), "p1", T0);
    q.complete(&a, Some(1), Marks::Exited, T0);
    let tabs = q.take_old_tabs("t1");
    assert_eq!(tabs, vec![OldTab { window: "main".into(), workspace_id: "ws-h1".into(), tab_id: "tab-p1".into() }]);
    assert!(q.take_old_tabs("t1").is_empty(), "taken once");
}

#[test]
fn a_retiring_worker_s_job_tabs_all_close_the_running_one_s_too() {
    let mut q = Queue::default();
    let a = running(&mut q, spec("h1", "t1", "a", &[]), "p1", T0);
    q.complete(&a, Some(0), Marks::Passed, T0);
    let b = running(&mut q, spec("h1", "t1", "a", &[]), "p2", T0);
    let queued = q.enqueue(spec("h1", "t1", "a", &["tauri-dev"]), T0).unwrap().id;
    let other = running(&mut q, spec("h1", "t2", "o", &[]), "p3", T0);
    let (ended, tabs) = q.retire_worker("t1", "it retired", T0 + 1);
    let got: Vec<(String, bool)> = ended.iter().map(|e| (e.job.id.clone(), e.stop)).collect();
    assert_eq!(got, vec![(b.clone(), true), (queued, false)]);
    assert_eq!(tabs.iter().map(|t| t.tab_id.as_str()).collect::<Vec<_>>(), vec!["tab-p1", "tab-p2"]);
    assert!(q.take_old_tabs("t1").is_empty(), "nothing left behind in memory");
    assert_eq!(q.job(&other).unwrap().state, RUNNING, "another worker's job is untouched");
}

/// An undo's or post-land check borrows a worker's task id: the worker stopping, retiring or
/// crashing after a failed land never cancels it (a lost verdict on Base, §8.6), its tab isn't
/// left filed under a task that may never start or retire again, and a suspend still takes it.
#[test]
fn a_land_profile_job_is_no_part_of_its_worker_s_release_or_its_old_tabs() {
    let land = |kind: &str| JobSpec {
        land: Some(LandRun { land_id: "B-1".into(), profile: PROFILE_LAND.into(), kind: kind.into(), ..LandRun::default() }),
        ..spec("h1", "t1", "a", &[])
    };
    let mut q = Queue::default();
    let check = running(&mut q, land("base_check"), "p1", T0);
    let queued = q.enqueue(land("undo"), T0).unwrap().id;
    let own = q.enqueue(spec("h1", "t1", "a", &["tauri-dev"]), T0).unwrap().id;
    let (ended, tabs) = q.retire_worker("t1", "you were retired", T0 + 1);
    assert_eq!(ids(&ended.iter().map(|e| e.job.clone()).collect::<Vec<_>>()), vec![own], "only the worker's own job");
    assert!(tabs.is_empty());
    assert_eq!((q.job(&check).unwrap().state.as_str(), q.job(&queued).unwrap().state.as_str()), (RUNNING, QUEUED));

    q.complete(&check, Some(1), Marks::Exited, T0 + 2);
    assert_eq!(q.job(&check).unwrap().state, DONE, "it concludes");
    assert!(q.take_old_tabs("t1").is_empty(), "its tab is closed by `finish`, not left under a retired task");

    let ended = q.release_task("t1", true, BY_RELEASE, "your Hive was suspended", T0 + 3);
    assert_eq!(ids(&ended.iter().map(|e| e.job.clone()).collect::<Vec<_>>()), vec![queued], "a suspend releases it");
}

/// A Hive being suspended is released before its PTYs die: its running job is the release's
/// (no "its tab went away" of its own when the PTY then closes), and nothing of it starts or
/// queues until it is resumed.
#[test]
fn a_parked_workspace_starts_nothing_and_its_killed_ptys_end_nothing() {
    let mut q = Queue::default();
    let run = running(&mut q, spec("h1", "t1", "a", &["cargo"]), "p1", T0);
    let waiting = q.enqueue(spec("h1", "t1", "a", &["cargo"]), T0).unwrap().id;
    let elsewhere = q.enqueue(spec("h2", "t2", "b", &["e2e"]), T0).unwrap().id;
    q.park("ws-h1");
    assert_eq!(q.tasks_in("ws-h1"), vec!["t1".to_string()]);
    let ended = q.release_worker("t1", BY_RELEASE, "your Hive was suspended", T0 + 1);
    assert_eq!(ended.iter().map(|e| (e.job.id.clone(), e.stop)).collect::<Vec<_>>(), vec![(run.clone(), true), (waiting, false)]);
    assert!(q.pty_closed("p1", T0 + 2).is_none(), "the kill ends nothing more: the release was its end");
    assert_eq!(q.job(&run).unwrap().stopped_by.as_deref(), Some(BY_RELEASE));

    assert!(q.enqueue(spec("h1", "t1", "a", &[]), T0 + 3).unwrap_err().contains("being suspended"));
    let mut sneaked = q.enqueue(spec("h1", "t9", "c", &[]), T0 + 3);
    assert!(sneaked.is_err(), "no worker of it queues");
    assert_eq!(ids(&q.schedule(T0 + 4)), vec![elsewhere], "another Hive's job runs on");
    q.unpark("ws-h1");
    sneaked = q.enqueue(spec("h1", "t1", "a", &[]), T0 + 5);
    assert!(ids(&q.schedule(T0 + 5)).contains(&sneaked.unwrap().id), "resumed: it queues and starts again");
}

/// A worker's copy of a service runs as a job whose ports are booked under its job id: a run on
/// the same checkout that leases it, or names `{port:web}`, finds them through the queue.
#[test]
fn a_workers_service_copy_is_found_by_its_checkout() {
    let mut q = Queue::default();
    let svc = JobSpec { service_id: Some("svc-web".into()), target: "checkout:/wt/a".into(), ..spec("h1", "t1", "a", &[]) };
    let id = q.enqueue(svc, T0).unwrap().id;
    assert_eq!(q.copy_job_ports("svc-web", "checkout:/wt/a"), Some(vec![]), "queued: no port yet");
    q.schedule(T0);
    q.set_ports(&id, vec![5174]);
    assert_eq!(q.copy_job_ports("svc-web", "checkout:/wt/a"), Some(vec![5174]));
    assert_eq!(q.copy_job_ports("svc-web", "checkout:/wt/b"), None, "another worker's checkout");
    assert_eq!(q.copy_job_ports("svc-api", "checkout:/wt/a"), None);
    q.interrupt(&id, BY_WORKER, "stopped", T0 + 1);
    assert_eq!(q.copy_job_ports("svc-web", "checkout:/wt/a"), None, "ended");
}

#[test]
fn the_sweep_takes_job_rows_and_their_tabs_and_nothing_else() {
    use crate::state::{Tab, WindowData, Workspace};
    let svc: Service = serde_json::from_value(json!({
        "id": "svc-1", "name": "web", "command": "npm run dev", "cwd": "/p", "origin": "human", "created_at": "", "updated_at": ""
    }))
    .unwrap();
    let mut ws = Workspace::new("H".into());
    ws.stack = vec![svc.clone(), Service { id: "j-0000abcd".into(), kind: crate::state::workspace::JOB_KIND.into(), ..svc.clone() }];
    let bound = |id: &str, sid: Option<&str>| {
        let mut t = Tab::new(id.into());
        t.id = id.into();
        t.service_id = sid.map(str::to_string);
        t
    };
    ws.panes[0].tabs = vec![
        bound("plain", None),
        bound("web-tab", Some("svc-1")),
        bound("job-tab", Some("j-0000abcd")),
        bound("old-job-tab", Some("j-1111ffff")),
        bound("odd", Some("not-a-job-id")),
    ];
    let mut win = WindowData::new("main".into());
    win.workspaces = vec![ws];
    let mut data = AppData { windows: vec![win], ..AppData::default() };
    assert_eq!(sweep_jobs(&mut data), 3);
    let ws = &data.windows[0].workspaces[0];
    assert_eq!(ws.stack.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(), vec!["svc-1"]);
    assert_eq!(ws.panes[0].tabs.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), vec!["plain", "web-tab", "odd"]);
    assert_eq!(sweep_jobs(&mut data), 0);
}

#[test]
fn the_run_log_is_appended_and_indexed() {
    let root = std::env::temp_dir().join(format!("hive-runlog-{}", uuid::Uuid::new_v4()));
    let paths = HivePaths::new(&root);
    let mut q = Queue::default();
    let id = running(&mut q, JobSpec { branch: Some("hive/x--a".into()), commit: Some("abc1234".into()), ..spec("h1", "t1", "a", &["cargo"]) }, "p1", T0);
    let ended = q.complete(&id, Some(0), Marks::Passed, T0 + 5).unwrap();
    append_run(&paths, &ended.job).unwrap();
    append_line(&paths.run_log(), &json!({"type": "deny", "command": "cargo test"})).unwrap();
    let text = std::fs::read_to_string(paths.run_log()).unwrap();
    assert_eq!(text.lines().count(), 2);
    let first: Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    assert_eq!(first["type"], "run");
    assert_eq!(first["state"], DONE);
    assert!(first.get("env").is_none(), "the expanded env stays out of the log");
    let found = find_run(&paths, &id).unwrap();
    assert_eq!((found.exit_code, found.began, found.commit.as_deref()), (Some(0), true, Some("abc1234")));
    // A second job after the index was read is found too.
    let id2 = running(&mut q, spec("h1", "t1", "a", &[]), "p2", T0);
    let e2 = q.complete(&id2, Some(3), Marks::Exited, T0).unwrap();
    append_run(&paths, &e2.job).unwrap();
    assert_eq!(find_run(&paths, &id2).unwrap().exit_code, Some(3));
    assert!(find_run(&paths, "j-ffffffff").is_none());
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn land_checks_go_first_one_per_repo_per_hive_and_keep_their_place() {
    let land = |hive: &str, task: &str, id: &str| JobSpec {
        priority: crate::hive::land::driver::LAND_PRIORITY,
        land: Some(LandRun { land_id: id.into(), worktree: format!("/h/wt.noindex/land/{id}/web"), admin: "/r/.git/worktrees/web".into(), profile: "worker".into(), kind: String::new() }),
        ..spec(hive, task, "w", &["land:web", "cargo"])
    };
    let mut q = Queue::default();
    let run = q.enqueue(spec("h1", "t0", "r", &["cargo"]), T0).unwrap().id;
    let a = q.enqueue(land("h1", "t1", "L-aaaaaaaa"), T0 + 1).unwrap();
    let b = q.enqueue(land("h1", "t2", "L-bbbbbbbb"), T0 + 2).unwrap().id;
    let other = q.enqueue(land("h2", "t3", "L-cccccccc"), T0 + 3).unwrap().id;
    assert_eq!(a.land.as_ref().map(|l| l.land_id.as_str()), Some("L-aaaaaaaa"), "a land check keeps its place");
    assert_eq!(ids(&q.schedule(T0)), vec![a.id.clone(), other.clone()], "ahead of the run queued first; one per repo in a Hive, the other Hive's beside it");
    assert_eq!(q.place(&b), Some(1));
    assert_eq!(q.place(&run), Some(2));
    q.fail_start(&a.id, "x", T0).unwrap();
    assert_eq!(ids(&q.schedule(T0)), vec![b], "the next land check of that repo once the first ends");

    // The run log carries where it ran.
    let root = std::env::temp_dir().join(format!("hive-landrun-{}", uuid::Uuid::new_v4()));
    let paths = HivePaths::new(&root);
    let ended = q.interrupt(&other, BY_RELEASE, "its land ended", T0).unwrap();
    append_run(&paths, &ended.job).unwrap();
    assert_eq!(find_run(&paths, &other).unwrap().land.unwrap().land_id, "L-cccccccc");
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn create_detects_the_usual_queued_commands() {
    use crate::hive::record::{HiveFolder, KIND_GIT, KIND_PLAIN};
    let base = std::env::temp_dir().join(format!("hive-detect-{}", uuid::Uuid::new_v4()));
    let app = base.join("app");
    std::fs::create_dir_all(app.join("src-tauri")).unwrap();
    std::fs::write(app.join("src-tauri/Cargo.toml"), "[package]").unwrap();
    std::fs::write(
        app.join("package.json"),
        r#"{"scripts":{"tauri:dev":"tauri dev","tauri:dev:isolated":"node x"},"devDependencies":{"@playwright/test":"1"}}"#,
    )
    .unwrap();
    let web = base.join("web");
    std::fs::create_dir_all(web.join("node_modules/x")).unwrap();
    std::fs::write(web.join("node_modules/x/Cargo.toml"), "").unwrap();
    std::fs::write(web.join("playwright.config.ts"), "").unwrap();
    let plain = base.join("assets");
    std::fs::create_dir_all(&plain).unwrap();
    std::fs::write(plain.join("Cargo.toml"), "").unwrap();
    let folder = |name: &str, p: &Path, kind: &str| HiveFolder { name: name.into(), path: p.to_string_lossy().to_string(), kind: kind.into(), ..HiveFolder::default() };
    let got = detect_commands(&[folder("app", &app, KIND_GIT), folder("web", &web, KIND_GIT), folder("assets", &plain, KIND_PLAIN)]);
    let rows: Vec<(String, String, Vec<String>)> = got.commands.iter().map(|c| (c.folder.clone(), c.prefix.clone(), c.needs.clone())).collect();
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(
        rows,
        vec![
            ("app".into(), "cargo test".into(), v(&["cargo"])),
            ("app".into(), "cargo build".into(), v(&["cargo"])),
            ("app".into(), "npm run tauri:dev".into(), v(&["tauri-dev", "cargo"])),
            ("app".into(), "npm run tauri:dev:isolated".into(), v(&["tauri-dev", "cargo"])),
            ("app".into(), "npx playwright test".into(), v(&["e2e"])),
            ("web".into(), "npx playwright test".into(), v(&["e2e"])),
        ],
        "a vendored crate isn't the folder's; a plain folder runs nothing"
    );
    assert!(got.resources.is_empty());
    let _ = std::fs::remove_dir_all(&base);
}

/// maiTerm as its own Hive's primary folder (the 5b pilot): recognised by its isolated-instance
/// script, it queues that instance per worker on its own ports and folders, and never the
/// human's `tauri:dev`. Read off this repo's own package.json too, so a renamed script fails here.
#[test]
fn maiterm_is_detected_as_the_pilot_repo() {
    use crate::hive::record::{Hive, HiveFolder, KIND_GIT};
    use crate::stack::placeholders::{fill, Values};
    let base = std::env::temp_dir().join(format!("hive-pilot-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(base.join("src-tauri")).unwrap();
    std::fs::write(base.join("src-tauri/Cargo.toml"), "[package]").unwrap();
    std::fs::write(
        base.join("package.json"),
        r#"{"scripts":{"tauri:dev":"lsof -ti :1420 | xargs kill -9; tauri dev","tauri:dev:isolated":"node scripts/dev-isolated.mjs"}}"#,
    )
    .unwrap();
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf();
    let folder = |p: &Path| HiveFolder { name: "maiterm".into(), path: p.to_string_lossy().to_string(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() };
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    for root in [base.clone(), repo] {
        let got = detect_commands(&[folder(&root)]);
        let prefixes: Vec<&str> = got.commands.iter().map(|c| c.prefix.as_str()).collect();
        assert_eq!(prefixes, vec!["cargo test", "cargo build", "npm run tauri:dev:isolated"], "{}: never the human's tauri:dev", root.display());
        let iso = &got.commands[2];
        assert_eq!(iso.needs, v(&["tauri-dev", "cargo"]));
        assert_eq!(iso.env, PILOT_ENV.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<Vec<_>>());
        assert_eq!(got.commands[0].needs, v(&["cargo"]));
        assert!(got.commands[0].env.is_empty() && got.commands[0].extra_write.is_empty(), "cargo test is the plain entry");
        // The human's settings check takes them as detected.
        let hive = Hive { id: "h".into(), folders: vec![folder(&root)], ..Hive::default() };
        assert_eq!(crate::hive::run::validate_run_queue(&hive, &got).unwrap(), got);
    }
    // Filled per worker: its own data folder, WebKit storage, caches and logs, and its two ports.
    let iso = &detect_commands(&[folder(&base)]).commands[2];
    let values = Values { worker: Some("auth".into()), port: Some(5174), port2: Some(5175), ..Values::default() };
    let filled: Vec<String> = iso.extra_write.iter().map(|p| fill(p, &values).unwrap()).collect();
    assert_eq!(
        filled,
        vec![
            "~/Library/Application Support/com.aiterm.dev.auth",
            "~/Library/WebKit/com.aiterm.dev.auth",
            "~/Library/Caches/com.aiterm.dev.auth",
            "~/Library/Logs/com.aiterm.dev.auth",
        ]
    );
    let env: Vec<(String, String)> = iso.env.iter().map(|(k, x)| (k.clone(), fill(x, &values).unwrap())).collect();
    assert_eq!(env, vec![("MAITERM_INSTANCE".into(), "auth".into()), ("MAITERM_VITE_PORT".into(), "5174".into()), ("MAITERM_MCP_PORT".into(), "5175".into())]);
    // A slug over 32 (`slugify` allows 60) still names an instance `dev-isolated.mjs` takes, and
    // its extra writes are that same instance's folders.
    let long = Values { worker: Some("move-session-persistence-into-redis".into()), port: Some(5174), port2: Some(5175), ..Values::default() };
    let name = fill(&iso.env[0].1, &long).unwrap();
    assert!(crate::instance::valid_name(&name), "{name}");
    let ids: Vec<String> = iso.extra_write.iter().map(|p| fill(p, &long).unwrap()).collect();
    assert!(ids.iter().all(|p| p.ends_with(&format!("/{}", crate::instance::slug_for(true, &crate::instance::parse_name(true, Some(&name)))))), "{ids:?}");
    // Another Tauri app's isolated script is just a script.
    std::fs::write(base.join("package.json"), r#"{"scripts":{"tauri:dev":"tauri dev","tauri:dev:isolated":"node x"}}"#).unwrap();
    let other = detect_commands(&[folder(&base)]);
    assert_eq!(other.commands.iter().map(|c| c.prefix.as_str()).collect::<Vec<_>>(), vec!["cargo test", "cargo build", "npm run tauri:dev", "npm run tauri:dev:isolated"]);
    assert!(other.commands.iter().all(|c| c.env.is_empty() && c.extra_write.is_empty()));
    let _ = std::fs::remove_dir_all(&base);
}

/// The job's sandbox, for real (macOS only): a profile drawn from a worker's write lists over
/// scratch folders, and the line typed through `zsh`. Nothing here touches maiTerm's data folder
/// or any running maiTerm.
///
/// `cargo test --lib job_sandbox_probe -- --ignored --nocapture`
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn job_sandbox_probe() {
    use crate::hive::job_sandbox::{job_line, profile, profile_path, real_path, LineSpec, ProfileSpec, EXIT_MARK, PASS_MARK, PROFILE_FILE};
    use crate::hive::record::{Hive, HiveFolder, KIND_GIT};
    use crate::hive::settings::{worker_write_lists, DenyPath, WorkerContext};
    use crate::hive::worker::{WorkerFolder, WorkerRecord};
    use std::net::TcpListener;
    use std::process::Command;

    let base = real_path(&std::env::temp_dir().join(format!("hive-job-probe-{}", uuid::Uuid::new_v4())));
    let repo = base.join("repo");
    let hive_root = base.join("hive");
    let paths = HivePaths::new(&hive_root);
    let wt = paths.worker_worktree("auth", "app");
    for d in [repo.join(".git/worktrees/app"), repo.join(".git/objects"), repo.join(".git/refs/heads/hive"), wt.clone(), paths.runs(), paths.worker_job_tmp("auth")] {
        std::fs::create_dir_all(&d).unwrap();
    }
    std::fs::write(repo.join(".git/config"), "[core]\n").unwrap();
    let hive = Hive { id: "probe".into(), slug: "probe".into(), folders: vec![HiveFolder { name: "app".into(), path: repo.to_string_lossy().to_string(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() }], ..Hive::default() };
    let w = WorkerRecord {
        task_id: "t-auth".into(),
        slug: "auth".into(),
        folders: vec![WorkerFolder {
            name: "app".into(),
            kind: KIND_GIT.into(),
            worktree: Some(wt.to_string_lossy().to_string()),
            branch: Some("hive/probe--auth".into()),
            common_dir: Some(repo.join(".git").to_string_lossy().to_string()),
            admin_dir: Some(repo.join(".git/worktrees/app").to_string_lossy().to_string()),
            ..WorkerFolder::default()
        }],
        ..WorkerRecord::default()
    };
    let lists = worker_write_lists(&hive, &w, &paths, &WorkerContext::default(), &base.join("bin")).unwrap();
    assert!(lists.deny.iter().any(|d| d.path == paths.runs()), "the runs folder is the worker's denyWrite");

    let leased = TcpListener::bind("127.0.0.1:0").unwrap();
    let stranger = TcpListener::bind("127.0.0.1:0").unwrap();
    let (leased_port, stranger_port) = (leased.local_addr().unwrap().port(), stranger.local_addr().unwrap().port());
    let run_dir = paths.run("j-0000beef");
    std::fs::create_dir_all(&run_dir).unwrap();
    // As `prepare` names them.
    let writable: Vec<PathBuf> = lists.allow.iter().cloned().chain([paths.worker_job_tmp("auth")]).collect();
    let spec = ProfileSpec {
        allow: writable.iter().map(|p| profile_path(p, &writable).unwrap()).collect(),
        deny: lists.deny.iter().map(|d| DenyPath { path: profile_path(&d.path, &writable).unwrap(), dir: d.dir }).collect(),
        own_ports: vec![],
        reach_ports: vec![leased_port],
    };
    let profile_file = run_dir.join(PROFILE_FILE);
    std::fs::write(&profile_file, profile(&spec).unwrap()).unwrap();
    println!("{}", std::fs::read_to_string(&profile_file).unwrap());

    let sandboxed = |script: &str| -> bool {
        let out = Command::new("/usr/bin/sandbox-exec").arg("-f").arg(&profile_file).args(["/bin/sh", "-c", script]).output().unwrap();
        println!("{script:?} → {:?} {}", out.status.code(), String::from_utf8_lossy(&out.stderr).trim());
        out.status.success()
    };
    let q = |p: &Path| crate::hive::launch::sh_quote(&p.to_string_lossy());
    let home_probe = dirs::home_dir().unwrap().join(format!(".maiterm-job-probe-{}", uuid::Uuid::new_v4()));

    assert!(sandboxed(&format!("echo ok > {}", q(&wt.join("written")))), "a write in the worktree");
    assert!(wt.join("written").is_file());
    assert!(sandboxed(&format!("echo ok > {}", q(&repo.join(".git/objects/probe")))), "the worker's corner of .git");
    assert!(!sandboxed(&format!("echo x >> {}", q(&repo.join(".git/config")))), ".git/config is refused");
    assert_eq!(std::fs::read_to_string(repo.join(".git/config")).unwrap(), "[core]\n");
    assert!(!sandboxed(&format!("echo x > {}", q(&home_probe))), "$HOME is refused");
    let _ = std::fs::remove_file(&home_probe);
    assert!(!sandboxed(&format!("echo x > {}", q(&run_dir.join(PASS_MARK)))), "a job can't plant its own pass mark");
    assert!(!run_dir.join(PASS_MARK).exists());
    assert!(!sandboxed("/usr/bin/curl -s -m 5 -o /dev/null https://example.com"), "no network");
    assert!(sandboxed(&format!("/usr/bin/nc -z -G 2 127.0.0.1 {leased_port}")), "a leased listener");
    assert!(!sandboxed(&format!("/usr/bin/nc -z -G 2 127.0.0.1 {stranger_port}")), "a listener it doesn't lease");

    // The whole line, typed through a shell as the job tab would: it passes and marks.
    let line = |argv: Vec<String>| {
        job_line(&LineSpec {
            cwd: wt.clone(),
            unset: vec!["CLAUDECODE".into()],
            env: vec![("TMPDIR".into(), paths.worker_job_tmp("auth").to_string_lossy().to_string()), ("CARGO_NET_OFFLINE".into(), "true".into())],
            profile: profile_file.clone(),
            argv,
            pass_mark: run_dir.join(PASS_MARK),
            exit_mark: run_dir.join(EXIT_MARK),
        })
        .unwrap()
    };
    let typed = |l: &str| Command::new("/bin/zsh").args(["-f", "-c", l]).env("MAITERM_AUTH", "secret").status().unwrap();
    let st = typed(&line(vec!["/bin/sh".into(), "-c".into(), "test -z \"$MAITERM_AUTH\" && echo $CARGO_NET_OFFLINE > ./env-seen".into()]));
    assert!(st.success(), "the job ran, without maiTerm's credentials");
    assert_eq!(std::fs::read_to_string(wt.join("env-seen")).unwrap().trim(), "true");
    assert!(run_dir.join(PASS_MARK).is_file() && run_dir.join(EXIT_MARK).is_file(), "the shell marked the pass and the exit");
    for m in [PASS_MARK, EXIT_MARK] {
        std::fs::remove_file(run_dir.join(m)).unwrap();
    }
    let st = typed(&line(vec!["/usr/bin/touch".into(), repo.join(".git/config.lock").to_string_lossy().to_string()]));
    assert!(st.success(), "the line itself ends with the exit mark");
    assert!(!run_dir.join(PASS_MARK).exists(), "no pass mark for a failed job");
    assert!(run_dir.join(EXIT_MARK).is_file(), "an exit mark whatever the job returned");
    assert!(!sandboxed(&format!("echo x > {}", q(&run_dir.join("exited-forged")))), "a job can't plant its own exit mark");

    // A link the worker planted in its job temp is refused, never resolved into a grant.
    let jobs = paths.worker_job_tmp("auth");
    std::fs::remove_dir_all(&jobs).unwrap();
    std::os::unix::fs::symlink(dirs::home_dir().unwrap(), &jobs).unwrap();
    assert!(profile_path(&jobs, &writable).is_err(), "a planted link refuses the job");
    std::fs::remove_file(&jobs).unwrap();

    // The job shares a terminal with the unsandboxed shell: it can't type into it (TIOCSTI).
    // With the profile's deny taken out, the same injection does reach the shell, so a pass
    // here is the deny's doing.
    if Command::new("/usr/bin/python3").arg("-c").arg("pass").status().is_ok_and(|s| s.success()) {
        let inj = base.join("inject.py");
        std::fs::write(&inj, "import fcntl, sys, termios\nfor c in (sys.argv[1] + '\\n').encode():\n    fcntl.ioctl(0, termios.TIOCSTI, bytes([c]))\n").unwrap();
        let open = run_dir.join("open.sb");
        std::fs::write(&open, std::fs::read_to_string(&profile_file).unwrap().replace("(deny file-ioctl (ioctl-command TIOCSTI))\n", "")).unwrap();
        let injected = |prof: &Path, tag: &str| -> bool {
            use portable_pty::{native_pty_system, CommandBuilder, PtySize};
            use std::io::{Read, Write};
            let escaped = base.join(format!("escaped-{tag}"));
            let pair = native_pty_system().openpty(PtySize { rows: 24, cols: 80, pixel_width: 0, pixel_height: 0 }).unwrap();
            let mut cmd = CommandBuilder::new("/bin/zsh");
            cmd.arg("-f");
            cmd.cwd(&base);
            let mut child = pair.slave.spawn_command(cmd).unwrap();
            drop(pair.slave);
            let mut reader = pair.master.try_clone_reader().unwrap();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while reader.read(&mut buf).is_ok_and(|n| n > 0) {}
            });
            let mut w = pair.master.take_writer().unwrap();
            let typed = format!("/usr/bin/sandbox-exec -f {} /usr/bin/python3 {} {}\n", q(prof), q(&inj), q(Path::new(&format!("touch {}", q(&escaped)))));
            w.write_all(typed.as_bytes()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(3));
            w.write_all(b"exit\n").unwrap();
            let _ = child.wait();
            let hit = escaped.exists();
            println!("TIOCSTI with {tag}: {}", if hit { "reached the shell" } else { "refused" });
            hit
        };
        assert!(injected(&open, "no-deny"), "control: without the deny the injection works, so the probe can see it");
        assert!(!injected(&profile_file, "profile"), "the job can't type into its tab's shell");
    } else {
        println!("TIOCSTI case skipped: no /usr/bin/python3");
    }
    drop((leased, stranger));
    let _ = std::fs::remove_dir_all(&base);
}

/// A Hive suspended when maiTerm stopped is parked again at startup: nothing of it queues until
/// its resume unparks it (docs/hive.md §6.8, plan 9a).
#[test]
fn a_suspended_hive_is_parked_again_after_a_restart() {
    use crate::hive::Hive;
    use crate::state::{WindowData, Workspace};
    let tag = uuid::Uuid::new_v4().simple().to_string();
    let ws = |name: &str, hive: bool, suspended: bool| {
        let mut w = Workspace::new(name.into());
        w.id = format!("{name}-{tag}");
        w.hive = hive.then(|| Hive { id: format!("h-{name}-{tag}"), ..Hive::default() });
        w.suspended = suspended;
        w
    };
    let mut win = WindowData::new("main".into());
    win.workspaces = vec![ws("asleep", true, true), ws("awake", true, false), ws("plain", false, true)];
    let data = parking_lot::RwLock::new(AppData { windows: vec![win], ..AppData::default() });
    assert_eq!(suspended_hive_workspaces(&data.read()), vec![format!("asleep-{tag}")], "a suspended plain workspace isn't a Hive's");

    assert_eq!(park_suspended(&data), 1);
    let job = |name: &str| JobSpec { workspace_id: format!("{name}-{tag}"), ..spec(&format!("h-{name}-{tag}"), &format!("t-{name}-{tag}"), "w", &[]) };
    assert!(enqueue(job("asleep")).unwrap_err().contains("being suspended"));
    let awake = enqueue(job("awake")).expect("an awake Hive queues");
    unpark_workspace(&format!("asleep-{tag}"));
    let resumed = enqueue(job("asleep")).expect("queues once resumed");
    release_worker(&awake.task_id, "test over");
    release_worker(&resumed.task_id, "test over");
}

/// The first real use: `pnpm install` failed in every worktree on the `.vscode` folders packages
/// ship, which Claude's sandbox refuses under a worker's cwd. A job's profile, drawn from the same
/// lists, lets an install write them inside `node_modules` and still refuses the worktree's own
/// agent and editor config; and an install job reaches only maiTerm's registry proxy, which
/// tunnels to a granted host alone. For real, under `sandbox-exec` (macOS only), over scratch
/// folders and loopback.
#[cfg(target_os = "macos")]
#[test]
fn an_install_job_writes_node_modules_config_and_reaches_only_the_registry_proxy() {
    use crate::hive::job_sandbox::{profile, profile_path, real_path, ProfileSpec, PROFILE_FILE};
    use crate::hive::record::{Hive, HiveFolder, KIND_GIT};
    use crate::hive::settings::{worker_write_lists, DenyPath, WorkerContext};
    use crate::hive::worker::{WorkerFolder, WorkerRecord};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;

    let base = real_path(&std::env::temp_dir().join(format!("hive-install-job-{}", uuid::Uuid::new_v4())));
    let repo = base.join("repo");
    let paths = HivePaths::new(base.join("hive"));
    let wt = paths.worker_worktree("auth", "app");
    for d in [repo.join(".git/worktrees/app"), repo.join(".git/objects"), wt.clone(), paths.runs(), paths.worker_job_tmp("auth")] {
        std::fs::create_dir_all(&d).unwrap();
    }
    let hive = Hive { id: "install".into(), slug: "install".into(), folders: vec![HiveFolder { name: "app".into(), path: repo.to_string_lossy().to_string(), kind: KIND_GIT.into(), primary: true, ..HiveFolder::default() }], ..Hive::default() };
    let w = WorkerRecord {
        task_id: "t-auth".into(),
        slug: "auth".into(),
        folders: vec![WorkerFolder {
            name: "app".into(),
            kind: KIND_GIT.into(),
            worktree: Some(wt.to_string_lossy().to_string()),
            branch: Some("hive/install--auth".into()),
            common_dir: Some(repo.join(".git").to_string_lossy().to_string()),
            admin_dir: Some(repo.join(".git/worktrees/app").to_string_lossy().to_string()),
            ..WorkerFolder::default()
        }],
        ..WorkerRecord::default()
    };
    let lists = worker_write_lists(&hive, &w, &paths, &WorkerContext::default(), &base.join("bin")).unwrap();

    // A registry stand-in on loopback, granted to this job through the proxy.
    let registry = TcpListener::bind("127.0.0.1:0").unwrap();
    let registry_port = registry.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for s in registry.incoming().flatten() {
            let mut s = s;
            let mut buf = [0u8; 1024];
            let _ = s.read(&mut buf);
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nhi");
        }
    });
    let job = format!("j-install-{}", uuid::Uuid::new_v4().simple());
    let (proxy_port, url) = crate::hive::registry_proxy::open_on(&job, vec!["127.0.0.1".into()], registry_port).unwrap();

    let writable: Vec<PathBuf> = lists.allow.iter().cloned().chain([paths.worker_job_tmp("auth")]).collect();
    let spec = ProfileSpec {
        allow: writable.iter().map(|p| profile_path(p, &writable).unwrap()).collect(),
        deny: lists.deny.iter().map(|d| DenyPath { path: profile_path(&d.path, &writable).unwrap(), dir: d.dir }).collect(),
        own_ports: vec![],
        reach_ports: vec![proxy_port],
    };
    let profile_file = paths.run(&job).join(PROFILE_FILE);
    std::fs::create_dir_all(profile_file.parent().unwrap()).unwrap();
    std::fs::write(&profile_file, profile(&spec).unwrap()).unwrap();
    let sandboxed = |script: &str| -> (bool, String) {
        let out = Command::new("/usr/bin/sandbox-exec").arg("-f").arg(&profile_file).args(["/bin/sh", "-c", script]).output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stdout).to_string())
    };
    let q = |p: &Path| crate::hive::launch::sh_quote(&p.to_string_lossy());

    for ok in ["node_modules/x/.vscode/settings.json", "node_modules/.pnpm/y@1/node_modules/y/.idea/z.xml", "packages/a/node_modules/b/.vscode/f"] {
        let f = wt.join(ok);
        assert!(sandboxed(&format!("mkdir -p {} && echo x > {}", q(f.parent().unwrap()), q(&f))).0, "an install writes {ok}");
        assert!(f.is_file(), "{ok}");
    }
    for refused in [".vscode/tasks.json", ".idea/workspace.xml", ".claude/settings.json", ".mcp.json"] {
        let f = wt.join(refused);
        assert!(!sandboxed(&format!("mkdir -p {} ; echo x > {}", q(f.parent().unwrap()), q(&f))).0, "{refused} is refused");
        assert!(!f.exists(), "{refused}");
    }

    let through = format!("/usr/bin/curl -s -m 10 -p -x '{url}' http://127.0.0.1:{registry_port}/");
    assert_eq!(sandboxed(&through), (true, "hi".to_string()), "the registry, through the proxy");
    assert!(!sandboxed(&format!("/usr/bin/curl -s -m 5 http://127.0.0.1:{registry_port}/")).0, "never straight to it");
    assert!(!sandboxed(&format!("/usr/bin/curl -s -m 5 -p -x '{url}' http://localhost:{registry_port}/")).0, "nor through the proxy to a host it wasn't granted");
    crate::hive::registry_proxy::close(&job);
    assert!(!sandboxed(&through).0, "the grant ends with its job");
    let _ = std::fs::remove_dir_all(&base);
}

/// A real `pnpm install` as an install job (macOS, network): the job's profile with only the
/// registry proxy's port open, the proxy granting the npm registry, and a scratch `HOME` and
/// store, so nothing of the human's is written. Passed 2026-10-26 (pnpm 11.1.2): the package
/// resolved, downloaded and linked through the proxy; `curl` straight to the registry refused.
///
/// `cargo test --lib real_pnpm_install_probe -- --ignored --nocapture`
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn real_pnpm_install_probe() {
    use crate::hive::job_sandbox::{profile, real_path, ProfileSpec};
    use std::process::Command;

    let base = real_path(&std::env::temp_dir().join(format!("hive-pnpm-probe-{}", uuid::Uuid::new_v4())));
    let (wt, home, cache) = (base.join("wt"), base.join("home"), base.join("deps-cache"));
    for d in [&wt, &home, &cache] {
        std::fs::create_dir_all(d).unwrap();
    }
    std::fs::write(wt.join("package.json"), r#"{"name":"probe","version":"0.0.0","private":true,"dependencies":{"is-number":"7.0.0"}}"#).unwrap();
    let job = format!("j-probe-{}", uuid::Uuid::new_v4().simple());
    let (proxy_port, url) = crate::hive::registry_proxy::open(&job, vec!["registry.npmjs.org".into()]).unwrap();
    // HOME isn't writable: pnpm must keep everything in the worker's cache, as `deps::cache_env` says.
    let spec = ProfileSpec { allow: vec![wt.clone(), cache.clone()], deny: vec![], own_ports: vec![], reach_ports: vec![proxy_port] };
    let profile_file = base.join("profile.sb");
    std::fs::write(&profile_file, profile(&spec).unwrap()).unwrap();
    let mut cmd = Command::new("/usr/bin/sandbox-exec");
    cmd.arg("-f").arg(&profile_file).args(["pnpm", "install", "--no-frozen-lockfile"]).current_dir(&wt).env("HOME", &home);
    for (k, v) in crate::hive::registry_proxy::env(&url).into_iter().chain(crate::hive::deps::cache_env(&cache)) {
        cmd.env(k, v);
    }
    let out = cmd.output().unwrap();
    println!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    assert!(out.status.success(), "pnpm install through the proxy");
    assert!(wt.join("node_modules/is-number/package.json").is_file());
    assert!(cache.join("pnpm-store").is_dir(), "the store is the worker's own");
    // And never straight out: the profile has no route but the proxy.
    let direct = Command::new("/usr/bin/sandbox-exec").arg("-f").arg(&profile_file).args(["/usr/bin/curl", "-s", "-m", "5", "https://registry.npmjs.org/"]).output().unwrap();
    assert!(!direct.status.success());
    crate::hive::registry_proxy::close(&job);
    let _ = std::fs::remove_dir_all(&base);
}
