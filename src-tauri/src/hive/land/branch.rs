//! The integration mode's land into Base (docs/hive.md §8.3, Darryl 2026-11-18): the queen lands
//! each worker into the Hive's branch on her own, and the branch reaches Base only on the human's
//! card. `hiveLandBranch` files it (a `hive_land` request with `branch: true`, so the phone shows
//! and approves it like any land card, with no new kind on the wire); its approval merges, per
//! repo, exactly the branch tip the card named into Base, `--no-ff`.
//!
//! No land check runs here, and none needs to: a card is filed only while the branch already
//! holds Base's tip, so the merge commit's tree is the branch tip's own (`git commit-tree` of that
//! tree, Base and the tip as parents), which the card says was, or wasn't, a tree the branch's
//! last land check passed. Base that moved since the branch took it in is merged into the branch
//! first, by a worker, through a normal land and its check. Base then moves through step 2's
//! `advance` (a checkout of Base by `merge --ff-only`, held over uncommitted work; none, by
//! compare-and-swap), under the repo's land lock. Nothing is pushed: the integration mode never
//! pushes (§8.5). A land into Base is the human's to undo, by hand (`git revert -m 1`): the
//! Hive's Undo runs a check job under a worker's sandbox, and this land has no worker.

use std::path::{Path, PathBuf};
use std::time::Instant;

use serde_json::{json, Value};

use super::approval::{Decision, KIND};
use super::git as lg;
use super::lock::{self, LockInfo, Taken};
use super::log::{self as landlog, LandLogEntry};
use super::git::Step2;
use crate::hive::deliver::one_line;
use crate::hive::git::{run, QUERY_TIMEOUT};
use crate::hive::paths::HivePaths;
use crate::hive::record::Hive;
use crate::hive::rpc::{HiveRequest, HiveWindow};
use crate::hive::tools::{blocking, coordinator_tab, Caller, Ctx};

/// Its land log lines' kind: Base moved by the human's card. Not `moved_target`: it isn't the
/// Hive's to undo, nor a land on the Hive's branch for `fold`'s standing.
pub const KIND_BRANCH: &str = "branch";
pub const ID_PREFIX: &str = "M-";
/// Where a waiting card is kept, so a restart files it again (`refile_in`).
const PENDING_FILE: &str = "branch-land.json";

/// One repo's part of the land, as the card names it.
#[derive(Debug, Clone, PartialEq)]
pub struct RepoPlan {
    pub folder: String,
    pub repo: PathBuf,
    pub base: String,
    pub base_tip: String,
    pub pinned: String,
    pub stat: String,
    /// Why its tip isn't a tree a land check passed; None when one did.
    pub unchecked: Option<String>,
    pub config: Vec<String>,
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(10)]
}

/// The last line that moved `branch` in `folder` passed its check on exactly `tip`'s tree.
fn checked_at(paths: &HivePaths, hive: &Hive, folder: &str, branch: &str, tip: &str) -> Option<String> {
    if !hive.folders.iter().any(|f| f.name == folder && f.land_check.as_deref().is_some_and(|c| !c.trim().is_empty())) {
        return Some(format!("{folder} has no land check"));
    }
    let last = landlog::read(paths).into_iter().filter(|e| e.folder == folder && e.target == branch && e.moved_target()).last();
    match last {
        Some(e) if e.base_after == tip && e.check == landlog::CHECK_PASSED && e.tree_landed == e.tree_tested => None,
        Some(_) => Some(format!("{}'s tip in {folder} isn't the tree its last land check passed", branch)),
        None => Some(format!("nothing has landed into {branch} in {folder} through maiTerm")),
    }
}

/// Whether `pinned` already holds everything in Base's tip: Base is in its history, or Base's tip
/// is a land of this branch maiTerm logged (`KIND_BRANCH`, whose files were exactly the branch tip
/// it named, built on a Base that branch tip held), and that tip is in `pinned`'s history. The
/// second case is every land after the first: the merge commit itself is never on the branch.
fn holds_base(paths: &HivePaths, repo: &Path, folder: &str, base: &str, base_tip: &str, pinned: &str) -> bool {
    if lg::is_ancestor(repo, base_tip, pinned) {
        return true;
    }
    // Checked in git, not taken from the line: Base's tip really is a merge whose second parent is
    // that land's branch tip, with exactly its files. A line the log shouldn't hold then lets
    // nothing in that isn't on the branch.
    let merge_of = |r: &str| lg::rev_parse(repo, &format!("{base_tip}^2")).as_deref() == Some(r) && lg::tree_of(repo, base_tip).is_some_and(|t| lg::tree_of(repo, r).as_deref() == Some(t.as_str()));
    landlog::read(paths).into_iter().any(|e| {
        e.kind == KIND_BRANCH && e.folder == folder && e.target == base && e.base_after == base_tip && merge_of(&e.reviewed_sha) && lg::is_ancestor(repo, &e.reviewed_sha, pinned)
    })
}

/// What a land of the Hive's branch into Base would take, repo by repo, or why it can't go now.
/// Blocks on git.
pub fn plan(hive: &Hive, paths: &HivePaths) -> Result<Vec<RepoPlan>, String> {
    if !hive.is_integration() {
        return Err("This Hive lands straight into Base: there's no branch of its own to land.".to_string());
    }
    let branch = hive.integration_branch();
    let mut out = Vec::new();
    for f in hive.git_folders() {
        let repo = PathBuf::from(&f.path);
        let Some(base) = f.base.clone().filter(|b| !b.is_empty()) else {
            return Err(format!("{}'s Base was a detached HEAD when this Hive was made, so its branch has nowhere to land.", f.name));
        };
        let pinned = lg::branch_tip(&repo, &branch).ok_or_else(|| format!("{} has no branch {branch}.", f.name))?;
        let base_tip = lg::branch_tip(&repo, &base).ok_or_else(|| format!("{} has no branch {base}.", f.name))?;
        if lg::is_ancestor(&repo, &pinned, &base_tip) {
            continue;
        }
        if !holds_base(paths, &repo, &f.name, &base, &base_tip, &pinned) {
            return Err(format!(
                "{base} in {} moved since {branch} last took it in. Have a worker merge {base} into {branch} first (a land like any \
                 other, through its land check), then ask again: what lands in {base} is then exactly what was checked.",
                f.name
            ));
        }
        let stat = lg::shortstat(&repo, &base_tip, &pinned)?.trim().to_string();
        let config = super::config_rule::hits(&repo, &base_tip, &pinned, &hive.settings.land.config_paths)?;
        out.push(RepoPlan { folder: f.name.clone(), unchecked: checked_at(paths, hive, &f.name, &branch, &pinned), repo, base, base_tip, pinned, stat, config });
    }
    if out.is_empty() {
        return Err(format!("{branch} has nothing Base doesn't already have."));
    }
    Ok(out)
}

/// The card: what lands, what was checked, what it changes of the agents' config.
pub(crate) fn card(hive: &Hive, land_id: &str, plans: &[RepoPlan], summary: &str) -> HiveRequest {
    let branch = hive.integration_branch();
    let mut bases: Vec<&str> = Vec::new();
    for p in plans {
        if !bases.contains(&p.base.as_str()) {
            bases.push(p.base.as_str());
        }
    }
    let folders: Vec<&str> = plans.iter().map(|p| p.folder.as_str()).collect();
    let mut effects = vec![format!(
        "maiTerm merges exactly the tip of {branch} named below into {}, --no-ff, one merge commit per repo. Its files are \
         the tip\u{2019}s own: {} already holds everything in {}. Nothing is pushed.",
        bases.join(", "),
        branch,
        bases.join(", ")
    )];
    for p in plans {
        let check = match &p.unchecked {
            None => "its files are exactly what its last land check passed".to_string(),
            Some(why) => format!("not checked as a whole: {why}"),
        };
        effects.push(format!("{}: {}, at {}; {check}.", p.folder, if p.stat.is_empty() { "no file changes" } else { &p.stat }, short(&p.pinned)));
        if !p.config.is_empty() {
            effects.push(format!("{} changes agent or execution config: {}.", p.folder, super::driver::files_line(&p.config)));
        }
    }
    effects.push(format!("{} moves only if it hasn\u{2019}t moved since this card; undoing it is yours (git revert -m 1 of its merge commit).", bases.join(", ")));
    let repos: serde_json::Map<String, Value> =
        plans.iter().map(|p| (p.folder.clone(), json!({ "pinned": p.pinned, "base": p.base, "base_tip": p.base_tip }))).collect();
    HiveRequest {
        tab_id: String::new(),
        kind: KIND,
        args: json!({ "hive_id": hive.id, "branch": true, "land_id": land_id, "repos": repos, "summary": summary }),
        summary: format!("Land {branch} into {} in {}", bases.join(", "), folders.join(", ")),
        effects,
        reason: Some(summary.to_string()).filter(|s| !s.is_empty()),
        keep: None,
    }
}

pub fn new_id() -> String {
    format!("{ID_PREFIX}{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

fn pending_path(paths: &HivePaths) -> PathBuf {
    paths.root.join(PENDING_FILE)
}

fn save_pending(paths: &HivePaths, request: &str, at: u64, req: &HiveRequest) -> Result<(), String> {
    let v = json!({ "request": request, "at": at, "args": req.args, "summary": req.summary, "effects": req.effects, "reason": req.reason });
    let path = pending_path(paths);
    let tmp = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, serde_json::to_vec_pretty(&v).map_err(|e| e.to_string())?).and_then(|_| std::fs::rename(&tmp, &path)).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("couldn't keep the card: {e}")
    })
}

fn clear_pending(paths: &HivePaths, request: &str) {
    let path = pending_path(paths);
    let ours = std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()).is_some_and(|v| v["request"] == request);
    if ours {
        let _ = std::fs::remove_file(&path);
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

/// `hiveLandBranch {summary}`: the card asking the human to land the Hive's branch into Base.
pub(crate) async fn tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let summary = one_line(args.get("summary").and_then(Value::as_str).unwrap_or_default().trim(), 1000);
    if summary.is_empty() {
        return Err("summary is required: what the branch holds, in a few lines, for your human to approve.".to_string());
    }
    let (hive, paths) = (caller.hive.clone(), caller.paths.clone());
    let plans = blocking(move || plan(&hive, &paths)).await?;
    let id = new_id();
    let mut req = card(&caller.hive, &id, &plans, &summary);
    req.tab_id = caller.tab_id.clone();
    let request = ctx.window.file_request(req.clone())?;
    let (paths, at) = (caller.paths.clone(), now_ms());
    blocking(move || save_pending(&paths, &request, at, &req)).await?;
    Ok(json!({
        "land": id,
        "state": "awaiting_human",
        "result": format!("Your human has a card to land {} into Base. Their answer arrives as \u{27e6}HIVE LAND\u{27e7}.", caller.hive.integration_branch()),
    }))
}

/// A waiting card filed again after a restart (agent requests live in memory).
pub(crate) fn refile_in<W: HiveWindow>(window: &W, caller: &Caller) -> usize {
    let Some(v) = std::fs::read(pending_path(&caller.paths)).ok().and_then(|b| serde_json::from_slice::<Value>(&b).ok()) else { return 0 };
    let (Some(id), Some(at)) = (v["request"].as_str(), v["at"].as_u64()) else { return 0 };
    let req = HiveRequest {
        tab_id: caller.tab_id.clone(),
        kind: KIND,
        args: v["args"].clone(),
        summary: v["summary"].as_str().unwrap_or_default().to_string(),
        effects: v["effects"].as_array().map(|a| a.iter().filter_map(|e| e.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        reason: v["reason"].as_str().map(str::to_string),
        keep: Some((id.to_string(), at)),
    };
    match window.file_request(req) {
        Ok(_) => 1,
        Err(e) => {
            log::warn!("hive {}: the land of its branch into Base not put to the human again: {e}", caller.hive.id);
            0
        }
    }
}

pub(crate) fn is_branch(d: &Decision) -> bool {
    d.args.get("branch").and_then(Value::as_bool) == Some(true)
}

/// One repo landed, held, or not reached.
#[derive(Debug, Clone, PartialEq)]
enum RepoOutcome {
    Landed { merge: String },
    Stopped(String),
}

/// One repo's land: the lock, Base still where the card saw it, the merge commit, step 2. Blocks.
#[allow(clippy::too_many_arguments)]
fn land_repo(hive: &Hive, paths: &HivePaths, land_id: &str, folder: &str, pinned: &str, base: &str, base_tip: &str, request: &str, summary: &str) -> RepoOutcome {
    let Some(f) = hive.git_folders().find(|f| f.name == folder) else {
        return RepoOutcome::Stopped(format!("{folder} is no longer one of this Hive's git folders"));
    };
    let repo = PathBuf::from(&f.path);
    let common = match lg::common_dir(&repo) {
        Ok(c) => c,
        Err(e) => return RepoOutcome::Stopped(e),
    };
    match lock::take(&common, &LockInfo::ours(&hive.id, land_id), Instant::now() + super::driver::LOCK_INLINE_WAIT, |_| {}) {
        Ok(Taken::Taken) => {}
        Ok(Taken::Busy(who)) => return RepoOutcome::Stopped(format!("{folder} is busy with {}", who.map_or_else(|| "another land".to_string(), |i| i.describe()))),
        Err(e) => return RepoOutcome::Stopped(e),
    }
    let out = land_locked(hive, paths, &repo, land_id, folder, pinned, base, base_tip, request, summary);
    lock::release(&common, land_id);
    out
}

#[allow(clippy::too_many_arguments)]
fn land_locked(hive: &Hive, paths: &HivePaths, repo: &Path, land_id: &str, folder: &str, pinned: &str, base: &str, base_tip: &str, request: &str, summary: &str) -> RepoOutcome {
    if lg::branch_tip(repo, base).as_deref() != Some(base_tip) {
        return RepoOutcome::Stopped(format!("{base} in {folder} moved since the card: ask again"));
    }
    if !holds_base(paths, repo, folder, base, base_tip, pinned) {
        return RepoOutcome::Stopped(format!("{} doesn't hold {base}'s tip in {folder}", hive.integration_branch()));
    }
    let Some(tree) = lg::tree_of(repo, pinned) else { return RepoOutcome::Stopped(format!("{} isn't a commit in {folder}", short(pinned))) };
    let message = format!("Land {} into {base} [maiTerm {land_id}]\n\n{summary}", hive.integration_branch());
    let mut cmd = lg::lgit(repo);
    cmd.args(lg::identity(repo)).args(["commit-tree", &tree, "-p", base_tip, "-p", pinned, "-m", &message]);
    let merge = match run(&mut cmd, QUERY_TIMEOUT) {
        Ok(s) => s.trim().to_string(),
        Err(e) => return RepoOutcome::Stopped(format!("git commit-tree: {e}")),
    };
    match lg::advance(repo, base, base_tip, &merge) {
        Ok(Step2::Advanced) => {}
        Ok(Step2::Held(why)) => return RepoOutcome::Stopped(why),
        Ok(Step2::BaseMoved) => return RepoOutcome::Stopped(format!("{base} in {folder} moved just now: ask again")),
        Err(e) => return RepoOutcome::Stopped(e),
    }
    let entry = LandLogEntry {
        id: format!("{land_id}/{folder}"),
        land: land_id.to_string(),
        kind: KIND_BRANCH.to_string(),
        hive_id: hive.id.clone(),
        title: format!("{} into {base}", hive.integration_branch()),
        folder: folder.to_string(),
        target: base.to_string(),
        reviewed_sha: pinned.to_string(),
        tested_sha: pinned.to_string(),
        merge_commit: Some(merge.clone()),
        base_before: base_tip.to_string(),
        base_after: merge.clone(),
        tree_tested: tree.clone(),
        tree_landed: lg::tree_of(repo, &format!("refs/heads/{base}")).unwrap_or_default(),
        summary: summary.to_string(),
        by: landlog::BY_HUMAN.to_string(),
        request: Some(request.to_string()).filter(|r| !r.is_empty()),
        review: landlog::REVIEW_NONE.to_string(),
        pushed: landlog::PUSHED_OFF.to_string(),
        at: crate::commands::workspace::iso_now(),
        ..LandLogEntry::default()
    };
    if let Err(e) = landlog::append(paths, &entry) {
        log::warn!("hive {}: {base} in {folder} moved to {} but the land log didn't take it: {e}", hive.id, short(&merge));
    }
    RepoOutcome::Landed { merge }
}

/// The human approved: each repo in order, stopping at the first that can't go.
pub(crate) async fn approved<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, d: &Decision) {
    let land_id = d.args.get("land_id").and_then(Value::as_str).unwrap_or_default().to_string();
    let summary = d.args.get("summary").and_then(Value::as_str).unwrap_or_default().to_string();
    let repos: Vec<(String, String, String, String)> = d
        .args
        .get("repos")
        .and_then(Value::as_object)
        .map(|m| {
            m.iter()
                .map(|(folder, v)| {
                    let s = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
                    (folder.clone(), s("pinned"), s("base"), s("base_tip"))
                })
                .collect()
        })
        .unwrap_or_default();
    let (hive, paths, id, request) = (caller.hive.clone(), caller.paths.clone(), land_id.clone(), d.id.clone());
    let lines = blocking(move || {
        let mut lines = Vec::new();
        for (folder, pinned, base, base_tip) in &repos {
            match land_repo(&hive, &paths, &id, folder, pinned, base, base_tip, &request, &summary) {
                RepoOutcome::Landed { merge } => lines.push((true, format!("{folder}: {base} is at {}", short(&merge)))),
                RepoOutcome::Stopped(why) => {
                    lines.push((false, format!("{folder}: not landed: {why}")));
                    break;
                }
            }
        }
        clear_pending(&paths, &request);
        Ok(lines)
    })
    .await
    .unwrap_or_else(|e| vec![(false, e)]);
    let ok = !lines.is_empty() && lines.iter().all(|(ok, _)| *ok);
    let text: Vec<String> = lines.into_iter().map(|(_, l)| l).collect();
    let outcome = text.join("; ");
    log::info!("hive {}: land {land_id} of its branch into Base: {outcome}", caller.hive.id);
    ctx.window.settle_request(&d.id, ok, outcome.clone());
    tell(ctx, caller, format!("\u{27e6}HIVE LAND\u{27e7} Your human approved landing {} into Base{}. {outcome}.", caller.hive.integration_branch(), d.where_from())).await;
}

/// Denied, or lapsed: the queen is told; nothing moved.
pub(crate) async fn closed<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, d: &Decision, status: &str) {
    let (paths, request) = (caller.paths.clone(), d.id.clone());
    let _ = blocking(move || {
        clear_pending(&paths, &request);
        Ok(())
    })
    .await;
    let what = if status == "denied" { "denied" } else { "didn't answer (the card lapsed)" };
    tell(ctx, caller, format!("\u{27e6}HIVE LAND\u{27e7} Your human {what} landing {} into Base. Nothing moved.", caller.hive.integration_branch())).await;
}

async fn tell<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) {
    match coordinator_tab(ctx.data, caller) {
        Some(c) => {
            let sent = ctx.window.deliver(&c, text.clone(), "Hive land").await;
            crate::hive::owed::owe_if_refused(caller, &sent, "Hive land", text).await;
        }
        None => log::info!("hive {}: the branch land's result not delivered: no coordinator tab", caller.hive.id),
    }
}

pub fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveLandBranch",
        "description": "Integration mode only: ask your human to land this Hive's branch into Base, on a card. Call it when the \
            goal's work has landed into the branch and passed its checks. It's refused while Base has moved since the branch \
            last took it in: have a worker merge Base into the branch first (a normal land, through its land check), so what \
            lands in Base is exactly what was checked. maiTerm merges exactly the branch tip the card names, one --no-ff merge \
            commit per repo, and pushes nothing. The answer arrives as \u{27e6}HIVE LAND\u{27e7}. Undoing it is your human's.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "summary": { "type": "string", "description": "What the branch holds and how it was checked, for your human, in a few lines." },
                "tabId": tab_id,
            },
            "required": ["summary"],
        },
    })
}
