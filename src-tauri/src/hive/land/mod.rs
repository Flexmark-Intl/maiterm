//! Landing (docs/hive.md §8, §8.1, §8.2, §8.5): the coordinator's `hiveLand` merges a reported
//! worker into Base, pinned to the commits it reviewed, in two git steps per repo that never touch
//! anyone's uncommitted work (`git`), under a lock per repository (`lock`), with the config rule
//! (`config_rule`) and an append-only log (`log`). `driver` runs it: inline when the land has no
//! check, or across a queued land check whose end (`driver::check_ended`) carries it on.
//!
//! Its state is `worker.json`'s `land`, persisted before every irreversible step, so a restart
//! knows how far it got (`reconcile`). Every kind is a String (§3.2).
//!
//! A land the human must approve (§8.1's config rule, Landing = Ask me, §8.3's integration mode)
//! waits on an agent request card (`approval`), and goes on from its pinned shas once approved.
//!
//! A land is undone by a revert commit landed through the same guarded steps (`undo`, §8.6), and
//! a Base that fails its land check alone makes the lands since it last passed suspect
//! (`base_check`), which holds every further land there but a fix-forward one.
//!
//! Lands that would wait on a busy repo queue up instead and land together (`batch`, §6.8): one
//! land check on a chain of their merges, bisected when it fails.

pub mod approval;
pub mod base_check;
pub mod batch;
pub mod config_rule;
pub mod driver;
pub mod git;
pub mod lock;
pub mod log;
#[cfg(test)]
mod tests;
pub mod undo;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use super::control::named_worker;
use super::deliver::one_line;
use super::paths::HivePaths;
use super::record::Hive;
use super::rpc::HiveWindow;
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::{self, WorkerRecord};

pub const STATE_LOCKING: &str = "locking";
pub const STATE_CHECKING: &str = "checking";
pub const STATE_ADVANCING: &str = "advancing";
pub const STATE_HELD: &str = "held";
pub const STATE_AWAITING_HUMAN: &str = "awaiting_human";
pub const STATE_QUEUED_BATCH: &str = "queued_batch";
/// Every repo landed and the worker retired: the record keeps it as history.
pub const STATE_LANDED: &str = "landed";

/// Step 1 not done, or to be done again (Base moved).
pub const REPO_PENDING: &str = "pending";
/// Its land check runs in the queue.
pub const REPO_CHECKING: &str = "checking";
/// Step 1 and its check passed: `tested` waits for step 2.
pub const REPO_TESTED: &str = "tested";
/// Step 2 under way: a restart reads Base to know whether it happened.
pub const REPO_ADVANCING: &str = "advancing";
pub const REPO_HELD: &str = "held";
pub const REPO_LANDED: &str = "landed";

/// Base moved under step 2 this many times: the land holds rather than loop (§8.1).
pub const MAX_ATTEMPTS: u32 = 3;
pub const MAX_SUMMARY: usize = 2_000;

/// Whose land check a queued job is (`queue::LandRun.kind`): a land's, an undo's, or the
/// post-land check on the target's tip alone (§8.6).
pub const CHECK_LAND: &str = "";
pub const CHECK_UNDO: &str = "undo";
pub const CHECK_BASE: &str = "base_check";
/// A batch's one land check, on the chain of its members' merges (§6.8).
pub const CHECK_BATCH: &str = "batch";

/// A land in `worker.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LandState {
    /// `L-` and 8 hex digits.
    pub id: String,
    /// `locking` | `checking` | `advancing` | `held` | `awaiting_human` | `queued_batch` | `landed`.
    pub state: String,
    /// In Hive folder order.
    pub repos: Vec<LandRepo>,
    /// The coordinator's words, quoted in the log.
    pub summary: String,
    /// Step 2's order, folder names (default: the Hive's folder order).
    pub order: Vec<String>,
    /// `pass` | `accepted` | `none`: what the log records of its review.
    pub review: String,
    pub held_reason: Option<String>,
    /// Whose lock it waits on.
    pub waiting_for: Option<String>,
    /// Why its latest card asks the human: `config` | `ask` | `integration` (`approval`).
    pub approval: Option<String>,
    /// That card's agent request, and when it was first filed (ms): a restart re-files it under
    /// both, so it keeps its place and its deadline.
    pub request: Option<String>,
    pub request_at: u64,
    /// `desktop` | `phone`, once the human approved it: the land is theirs in the log, and Ask me
    /// or the integration mode doesn't ask again for it.
    pub approved_by: Option<String>,
    /// Passed `fix_forward`: it may land into a repo marked suspect, as its fix (§8.6).
    pub fix_forward: bool,
    /// The batch it lands in (`Q-…`, §6.8), while it is one's member.
    pub batch: Option<String>,
    /// Its place in its repo's batch queue: batches merge their members in `hiveLand` order.
    pub queued_at: u64,
    pub started_at: String,
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// One repo of a land.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LandRepo {
    pub folder: String,
    /// The worker's branch, and the Base it lands on.
    pub branch: String,
    pub target: String,
    /// The sha the coordinator reviewed and named.
    pub pinned: String,
    /// Base's sha step 1 merged: step 2 compare-and-swaps from it.
    pub base_before: Option<String>,
    pub tested: Option<String>,
    /// Whether step 1 made a merge commit (Base wasn't already in the pinned commit).
    pub merged: bool,
    /// Its land check passed on `tested`: the log says so, and a land that lands exactly that
    /// tree proves its target (§8.6, `log::fold`).
    pub checked: bool,
    pub tree_tested: Option<String>,
    pub base_after: Option<String>,
    pub tree_landed: Option<String>,
    /// `pending` | `checking` | `tested` | `advancing` | `held` | `landed`.
    pub status: String,
    pub held_reason: Option<String>,
    /// Times Base moved under step 2.
    pub attempts: u32,
    /// Its land check's job.
    pub job: Option<String>,
    /// The temporary worktree and the git dir git gave it.
    pub tmp: Option<String>,
    pub tmp_admin: Option<String>,
    /// `ok` | `held: <why>` | `off`.
    pub pushed: Option<String>,
    /// Its line is in the land log.
    pub logged: bool,
    /// The config rule's catch (§8.1) from Base to `tested` (or, on a card filed before step 1,
    /// from the merge base to `pinned`), and its fingerprint (`config_rule::Caught`).
    pub config_hits: Vec<String>,
    pub config_fp: Option<String>,
    /// The fingerprint the human approved: the land goes on while the catch still has it.
    pub config_approved: Option<String>,
    /// A batch member's chain tip, the commit its batch's check passed (§6.8). `tested` is then
    /// the member's own merge commit in that chain: it landed once that is in its target, and an
    /// undo reverts it with `revert -m 1`. Its branch isn't moved.
    pub chain_tip: Option<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl LandRepo {
    /// Its config change, as it stands, isn't one the human approved.
    pub fn config_unapproved(&self) -> bool {
        self.status != REPO_LANDED
            && !self.config_hits.is_empty()
            && (self.config_fp.is_none() || self.config_approved.as_deref() != self.config_fp.as_deref())
    }
}

impl LandState {
    pub fn is_held(&self) -> bool {
        self.state == STATE_HELD
    }

    pub fn awaits_human(&self) -> bool {
        self.state == STATE_AWAITING_HUMAN
    }

    /// Running now: a lock, a check or a step 2 under way, or a place in a batch queue.
    pub fn in_progress(&self) -> bool {
        matches!(self.state.as_str(), STATE_LOCKING | STATE_CHECKING | STATE_ADVANCING | STATE_QUEUED_BATCH)
    }

    pub fn repo_mut(&mut self, folder: &str) -> Option<&mut LandRepo> {
        self.repos.iter_mut().find(|r| r.folder == folder)
    }

    /// The repos step 2 takes, in its order.
    pub fn ordered(&self) -> Vec<String> {
        let mut out: Vec<String> = self.order.iter().filter(|f| self.repos.iter().any(|r| &r.folder == *f)).cloned().collect();
        for r in &self.repos {
            if !out.contains(&r.folder) {
                out.push(r.folder.clone());
            }
        }
        out
    }

    pub fn landed(&self) -> Vec<&LandRepo> {
        self.repos.iter().filter(|r| r.status == REPO_LANDED).collect()
    }
}

pub fn new_land_id() -> String {
    format!("L-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

/// A land as a card shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LandView {
    pub id: String,
    pub state: String,
    pub held_reason: Option<String>,
    pub waiting_for: Option<String>,
    /// The card it waits on (`awaiting_human`): the worker's column shows that request.
    pub request: Option<String>,
    /// The batch it lands in (§6.8).
    pub batch: Option<String>,
    pub repos: Vec<LandRepoView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LandRepoView {
    pub folder: String,
    pub status: String,
    /// `ok` | `held: <why>` | `off`, once it landed: a Landed card says a held push (§8.5).
    pub pushed: Option<String>,
}

impl LandView {
    pub fn of(l: &LandState) -> Self {
        Self {
            id: l.id.clone(),
            state: l.state.clone(),
            held_reason: l.held_reason.clone(),
            waiting_for: l.waiting_for.clone(),
            request: l.request.clone().filter(|_| l.awaits_human()),
            batch: l.batch.clone(),
            repos: l.repos.iter().map(|r| LandRepoView { folder: r.folder.clone(), status: r.status.clone(), pushed: r.pushed.clone() }).collect(),
        }
    }
}

// ── hiveLand ──

pub fn tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": "hiveLand",
        "description": format!(
            "Land a worker that reported done into its Base, pinned to the commit you reviewed in each repo (commits: {{folder: \
             full sha}}, exactly its repos). maiTerm merges Base into that commit in a temporary worktree, runs the folder's land \
             check there if it has one, and fast-forwards Base to exactly the tested commit; nobody's uncommitted work is touched. \
             Refused if a branch moved since you reviewed it, its worktree has uncommitted or untracked files, its agent is mid-turn, \
             or a review of it didn't pass at exactly these commits (pass accept_review after hiveResolve accept). A conflict \
             answers the files and the worker goes back to Reported: tell it to merge Base and report again. If Base moves under \
             it, it tries again {MAX_ATTEMPTS} times, then holds; an uncommitted change in Base's checkout holds it too. Several \
             repos: every repo's merge and check pass before any lands, then they land in order (order lists folders first), \
             stopping at the first held one. hiveLand again with the same commits resumes a held land. A held worker may also \
             report again (after merging Base, say): then land its new commits, reviewed, and what already landed stays as it \
             landed; hiveRetireWorker abandons a held land. Without a land check the answer is the result; with one it says \
             checking, and the result arrives as ⟦HIVE LAND⟧. When it lands, the worker is retired and workers waiting on it \
             launch from the new Base. Some lands wait for the human's approval on a card first: every land in integration \
             mode, every land while the Landing checkpoint is Ask me, and any land whose diff changes agent or execution \
             config (.claude/, CLAUDE.md, git hooks, CI, package.json scripts…). The answer then says awaiting_human; the \
             result arrives as ⟦HIVE LAND⟧ once they answer, and a denial, or no answer within 24 hours, puts the worker back \
             in Reported. Don't ask them again yourself. If a land check fails, maiTerm also runs it on Base alone: when Base \
             fails too, that repo is suspect, the lands since it last passed are named, and every land there needs \
             fix_forward: true until a land or an undo there passes its check (undo the land that broke it with \
             hiveUndoLand, or land a fix). A one-repo land that finds its repo busy with another land answers queued_batch: \
             the lands waiting there are merged in turn and checked once together, a failing batch split until the culprit is \
             found; each one's result arrives as ⟦HIVE LAND⟧. A member that conflicts with another or changes config lands \
             alone. Its branch isn't moved: its own merge commit is what landed."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "worker": { "type": ["string", "number"], "description": "The worker: badge (3 or \"#3\"), name or task id" },
                "summary": { "type": "string", "description": "What lands, in a sentence or two: it goes in the land log" },
                "commits": { "type": "object", "additionalProperties": { "type": "string" }, "description": "folder → the full sha you reviewed there, for every repo of the worker" },
                "order": { "type": "array", "items": { "type": "string" }, "description": "Which repos land first (default: the Hive's folder order)" },
                "accept_review": { "type": "boolean", "description": "Land on hiveResolve accept, past a review that didn't pass" },
                "fix_forward": { "type": "boolean", "description": "Land into a repo marked suspect (its Base fails its land check alone), as the fix" }
            },
            "required": ["worker", "summary", "commits"]
        }
    })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct LandArgs {
    pub summary: String,
    pub commits: BTreeMap<String, String>,
    pub order: Vec<String>,
    pub accept_review: bool,
    pub fix_forward: bool,
}

/// A full commit id: 40 hex digits (SHA-1) or 64 (SHA-256).
pub fn is_full_sha(s: &str) -> bool {
    matches!(s.len(), 40 | 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

impl LandArgs {
    pub(crate) fn parse(args: &Value) -> Result<Self, String> {
        let summary = args.get("summary").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
        if summary.is_empty() {
            return Err("summary is required: what lands, for the land log".to_string());
        }
        if summary.chars().count() > MAX_SUMMARY {
            return Err(format!("Keep the summary under {MAX_SUMMARY} characters."));
        }
        let Some(obj) = args.get("commits").and_then(Value::as_object) else {
            return Err("commits is required: {folder: the full sha you reviewed}, for every repo of the worker".to_string());
        };
        let mut commits = BTreeMap::new();
        for (folder, sha) in obj {
            let sha = sha.as_str().map(str::trim).unwrap_or_default().to_ascii_lowercase();
            if !is_full_sha(&sha) {
                return Err(format!("commits.{}: give the full sha you reviewed (40 hex digits), not a short one or a branch name.", one_line(folder, 60)));
            }
            commits.insert(folder.clone(), sha);
        }
        let order = match args.get("order") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a.iter().map(|v| v.as_str().map(str::to_string).ok_or("order is a list of folder names")).collect::<Result<_, _>>()?,
            Some(_) => return Err("order is a list of folder names".to_string()),
        };
        let flag = |k: &str| args.get(k).and_then(Value::as_bool).unwrap_or(false);
        Ok(Self { summary, commits, order, accept_review: flag("accept_review"), fix_forward: flag("fix_forward") })
    }
}

/// What the worker's review says about landing these commits (plan D3): `pass` at exactly them,
/// `accepted` by `hiveResolve accept` (with `accept_review`), `none` when no verdict exists, or
/// why it can't land.
pub(crate) fn review_standing(w: &WorkerRecord, commits: &BTreeMap<String, String>, accept_review: bool, workers: &[WorkerRecord]) -> Result<&'static str, String> {
    let label = worker_label(w);
    if let Some(r) = workers.iter().find(|r| {
        r.reviews.as_deref() == Some(w.task_id.as_str())
            && (r.is_live() || r.phase == worker::PHASE_WAITING)
            && super::review::verdict_given(w, r).is_none()
    }) {
        return Err(format!("Reviewer {} is still reviewing worker {label}: land it after its verdict.", worker_label(r)));
    }
    let Some(review) = w.review.as_ref().filter(|r| !r.verdicts.is_empty()) else { return Ok(log::REVIEW_NONE) };
    let latest = review.latest().expect("verdicts isn't empty");
    // Every named commit is one the pass reviewed: a held land's repos still to land are named
    // without the ones that landed.
    if latest.verdict == "pass" && commits.iter().all(|(f, s)| latest.shas.get(f) == Some(s)) {
        return Ok(log::REVIEW_PASS);
    }
    let accepted = review.resolution.as_ref().is_some_and(|r| r.kind == "accept");
    if accepted && accept_review {
        return Ok(log::REVIEW_ACCEPTED);
    }
    let why = if latest.verdict == "pass" { "passed at other commits than these".to_string() } else { "failed".to_string() };
    Err(if accepted {
        format!("Worker {label}'s last review {why}; you resolved its escalation with accept, so pass accept_review: true to land it as it is.")
    } else {
        format!("Worker {label}'s last review {why}: a land needs a review that passed at exactly these commits. Spawn a reviewer with reviews: {}.", w.slug)
    })
}

/// Every refusal `hiveLand` makes before anything is written, in the plan's order. `tips` are the
/// branch tips read from the repos, `dirty` each worktree's uncommitted files, `busy` whether its
/// agent is mid-turn, `suspect` the folders whose Base fails its land check alone (§8.6).
/// Answers what the log records of its review.
#[allow(clippy::too_many_arguments)]
pub(crate) fn refusal(
    hive: &Hive,
    w: &WorkerRecord,
    a: &LandArgs,
    tips: &Result<BTreeMap<String, String>, String>,
    dirty: &[String],
    busy: bool,
    workers: &[WorkerRecord],
    suspect: &[String],
) -> Result<&'static str, String> {
    let label = worker_label(w);
    let done = w.last_report.as_ref().is_some_and(|r| r.status == "done");
    match (w.phase.as_str(), w.land.as_ref()) {
        (worker::PHASE_REPORTED, _) if done => {}
        (worker::PHASE_LANDING, Some(l)) if l.is_held() => {}
        (worker::PHASE_LANDING, Some(l)) if l.awaits_human() => {
            return Err(format!("Worker {label}'s land {} waits for the human's approval: its result comes as ⟦HIVE LAND⟧ once they answer.", l.id))
        }
        (worker::PHASE_LANDING, Some(l)) => return Err(format!("Worker {label}'s land {} is {} now: its result comes as ⟦HIVE LAND⟧.", l.id, l.state)),
        (worker::PHASE_REPORTED, _) => return Err(format!("Worker {label} reported {}, not done: only done work lands.", w.last_report.as_ref().map_or("nothing", |r| r.status.as_str()))),
        (p, _) => return Err(format!("Worker {label} is {p}: only a worker that has reported done lands.")),
    }
    if w.is_reviewer() {
        return Err(format!("Worker {label} is a reviewer: a review has nothing to land."));
    }
    let mine: Vec<&str> = w.git_folders().map(|f| f.name.as_str()).collect();
    let named: Vec<&str> = a.commits.keys().map(String::as_str).collect();
    if mine.iter().any(|f| !named.contains(f)) || named.iter().any(|f| !mine.contains(f)) {
        return Err(format!(
            "commits must name exactly worker {label}'s repos ({}), each with the full sha you reviewed.",
            if mine.is_empty() { "none: pass {}".to_string() } else { mine.join(", ") }
        ));
    }
    if let Some(f) = a.order.iter().find(|f| !mine.contains(&f.as_str())) {
        return Err(format!("order names \"{}\", which isn't one of worker {label}'s repos.", one_line(f, 60)));
    }
    // Once part of a held land has landed, what landed can't change: those repos keep their pins.
    // A repo still to land keeps its pin (landing as pinned even if the branch moved on since) or
    // takes a new, reviewed commit: the worker reported again after merging Base, say.
    let partial = w.land.as_ref().filter(|l| l.is_held() && !l.landed().is_empty());
    let mut left = a.commits.clone();
    if let Some(l) = partial {
        for r in l.landed() {
            if a.commits.get(&r.folder) != Some(&r.pinned) {
                return Err(format!(
                    "Worker {label}'s held land {} already landed {} at {}: name that commit for it.",
                    l.id,
                    r.folder,
                    &r.pinned[..r.pinned.len().min(10)]
                ));
            }
            left.remove(&r.folder);
        }
    }
    let repinned = |folder: &str, sha: &str| partial.is_none_or(|l| l.repos.iter().find(|r| r.folder == folder).is_none_or(|r| r.pinned != sha));
    let tips = tips.as_ref().map_err(|e| format!("maiTerm couldn't read worker {label}'s branches: {e}"))?;
    for (folder, sha) in left.iter().filter(|(f, s)| repinned(f, s)) {
        let tip = tips.get(folder).map(String::as_str).unwrap_or_default();
        if tip != sha {
            return Err(format!(
                "Worker {label}'s branch in {folder} moved since you reviewed it: it is at {}, not {}. Review again.",
                &tip[..tip.len().min(10)],
                &sha[..10]
            ));
        }
    }
    // Everything landed (the worker's retirement is what's left): nothing to check or review.
    let review = match partial.filter(|_| left.is_empty()) {
        Some(l) => [log::REVIEW_PASS, log::REVIEW_ACCEPTED].into_iter().find(|r| *r == l.review).unwrap_or(log::REVIEW_NONE),
        None => {
            if let Some(d) = dirty.first() {
                return Err(format!(
                    "Worker {label}'s worktree has uncommitted or untracked files ({d}): they would count toward a check but never land. Tell it to commit or remove them, then review what it committed."
                ));
            }
            if busy {
                return Err(format!("Worker {label}'s agent is mid-turn: land it once it's idle, so nothing it commits slips under the land."));
            }
            review_standing(w, &left, a.accept_review, workers)?
        }
    };
    // A Base that fails its land check alone holds every land there but its fix (§8.6).
    if let Some(f) = left.keys().find(|f| suspect.contains(f)).filter(|_| !a.fix_forward) {
        return Err(format!(
            "{f} is suspect: its {} fails its land check alone since an earlier land, so lands there are held. Undo the land \
             that broke it (hiveUndoLand; the land log names the suspects), or land the fix with fix_forward: true.",
            land_target(hive, f)
        ));
    }
    // The integration mode lands on `hive/<slug>`, whatever Base was.
    for f in w.git_folders().filter(|_| !hive.is_integration()) {
        let base = hive.folders.iter().find(|h| h.name == f.name).and_then(|h| h.base.clone());
        if base.is_none() {
            return Err(format!("{}'s Base was a detached HEAD when this Hive was made, so a land there has nowhere to go.", f.name));
        }
    }
    Ok(review)
}

/// The branch a land in this folder moves: Base, or `hive/<slug>` in the integration mode (§8.3).
pub fn land_target(hive: &Hive, folder: &str) -> String {
    if hive.is_integration() {
        return hive.integration_branch();
    }
    hive.folders.iter().find(|f| f.name == folder).and_then(|f| f.base.clone()).unwrap_or_default()
}

/// The worktrees' uncommitted or untracked files, one line per dirty worktree. Blocks on git.
pub(crate) fn dirty_worktrees(w: &WorkerRecord) -> Vec<String> {
    let mut out = Vec::new();
    for f in w.git_folders() {
        let status = super::git::git_in_worktree(f).and_then(|mut c| super::git::run(c.args(["status", "--porcelain", "--untracked-files=all"]), super::git::QUERY_TIMEOUT));
        match status {
            Ok(s) if s.trim().is_empty() => {}
            Ok(s) => {
                let n = s.lines().filter(|l| !l.trim().is_empty()).count();
                out.push(format!("{}: {n} file{}", f.name, if n == 1 { "" } else { "s" }));
            }
            Err(e) => out.push(format!("{}: its state couldn't be read ({})", f.name, one_line(&e, 120))),
        }
    }
    out
}

pub(crate) async fn land_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    land_tool_with(ctx, caller, args, driver::Opts::production()).await
}

pub(crate) async fn land_tool_with<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value, opts: driver::Opts) -> Result<Value, String> {
    let a = LandArgs::parse(args)?;
    let w = named_worker(caller, args).await?;
    let (hive, paths, rec) = (caller.hive.clone(), caller.paths.clone(), w.clone());
    let (tips, dirty, workers, suspect) = blocking(move || {
        let tips = super::worktree::builder_tips(&hive, &rec);
        Ok((tips, dirty_worktrees(&rec), worker::list_workers(&paths), log::folded(&paths).suspect_folders()))
    })
    .await?;
    let busy = w.tab_id.as_deref().filter(|t| super::tools::has_tab(ctx.data, caller, t)).is_some_and(|t| ctx.window.agent_busy(t));
    let review = refusal(&caller.hive, &w, &a, &tips, &dirty, busy, &workers, &suspect)?;

    if w.git_folders().next().is_none() {
        return driver::land_without_repos(ctx, caller, &w, &a).await;
    }
    let land = start_state(&caller.hive, &w, &a, review);
    let id = land.id.clone();
    let dir = worker::task_dir(&caller.paths, &w.task_id)?;
    let (d, l, label) = (dir.clone(), land.clone(), worker_label(&w));
    blocking(move || {
        let mut ok = false;
        let r = worker::update_worker(&d, |cur| {
            let done = cur.last_report.as_ref().is_some_and(|r| r.status == "done");
            ok = (cur.phase == worker::PHASE_REPORTED && done) || (cur.phase == worker::PHASE_LANDING && cur.land.as_ref().is_some_and(LandState::is_held));
            if ok {
                cur.phase = worker::PHASE_LANDING.to_string();
                cur.land = Some(l);
            }
            ok
        })?;
        if ok { Ok(()) } else { Err(format!("Worker {label} is {} now: look again with hiveListWorkers.", r.phase)) }
    })
    .await?;
    if let Some(t) = &w.tab_id {
        super::deliver::hold_for_land(t);
    }
    ctx.window.workers_changed(&caller.workspace_id);
    ::log::info!("hive {}: land {id} of worker {} started ({} repos)", caller.hive.id, worker_label(&w), a.commits.len());
    // Integration mode and Landing = Ask me: the card comes first, before step 1 (§8.3), pinned.
    if let Some(why) = approval::asks_first(&caller.hive).filter(|_| land.approved_by.is_none()) {
        return approval::await_human(ctx, caller, &w, land, why).await.answer();
    }
    // A busy repo: it waits in that repo's batch queue rather than for the lock alone (§6.8).
    if let Some(out) = batch::try_join(ctx, caller, &w, &land).await {
        return out.answer();
    }
    driver::drive(ctx, caller, &w.task_id, &opts, false).await.answer()
}

/// A new land, or a held one resumed: its landed repos and their pins kept, a repo still to land
/// kept while its pin is, everything else from these arguments. A held land whose pins changed
/// with nothing of it landed starts over.
fn start_state(hive: &Hive, w: &WorkerRecord, a: &LandArgs, review: &str) -> LandState {
    let now = crate::commands::workspace::iso_now();
    let pinned = |r: &LandRepo| a.commits.get(&r.folder) == Some(&r.pinned);
    let prior = w.land.clone().filter(|l| l.is_held() && l.landed().iter().all(|r| pinned(r)) && (!l.landed().is_empty() || l.repos.iter().all(pinned)));
    let mut land = prior.unwrap_or_else(|| LandState { id: new_land_id(), started_at: now.clone(), ..LandState::default() });
    let mut repos: Vec<LandRepo> = Vec::new();
    let mut repinned = false;
    for hf in &hive.folders {
        let Some(f) = w.git_folders().find(|f| f.name == hf.name) else { continue };
        let kept = land.repos.iter().find(|r| r.folder == f.name).filter(|r| r.status == REPO_LANDED || pinned(r)).cloned();
        repinned |= kept.is_none();
        let mut r = kept.unwrap_or_else(|| LandRepo {
            folder: f.name.clone(),
            pinned: a.commits.get(&f.name).cloned().unwrap_or_default(),
            status: REPO_PENDING.to_string(),
            ..LandRepo::default()
        });
        r.branch = f.branch.clone().unwrap_or_default();
        r.target = land_target(hive, &f.name);
        if r.status != REPO_LANDED {
            // A resumed repo starts its step 2 attempts again, and its tested commit is reused only
            // while Base is where step 1 found it (`driver::prepare`).
            r.attempts = 0;
            r.held_reason = None;
            r.job = None;
            let reusable = r.tested.is_some() && r.base_before.is_some() && matches!(r.status.as_str(), REPO_TESTED | REPO_HELD | REPO_ADVANCING);
            r.status = if reusable { REPO_TESTED } else { REPO_PENDING }.to_string();
        }
        repos.push(r);
    }
    land.repos = repos;
    // The human approved these pins: a held land resumed on them keeps that, a new pin doesn't.
    if repinned {
        land.approved_by = None;
        land.request = None;
        land.approval = None;
    }
    land.state = STATE_LOCKING.to_string();
    land.summary = a.summary.clone();
    land.order = a.order.clone();
    land.fix_forward = a.fix_forward;
    land.review = review.to_string();
    land.held_reason = None;
    land.waiting_for = None;
    land.updated_at = now;
    land
}

/// What a card's line says of a land.
pub fn describe(l: &LandState) -> String {
    match l.state.as_str() {
        STATE_HELD => l.held_reason.clone().unwrap_or_else(|| "held".to_string()),
        STATE_LOCKING => l.waiting_for.as_ref().map_or_else(|| "taking the land lock".to_string(), |w| format!("waiting for {w}")),
        STATE_CHECKING if l.batch.is_some() => "batch land check running".to_string(),
        STATE_CHECKING => "land check running".to_string(),
        STATE_ADVANCING => "advancing Base".to_string(),
        STATE_AWAITING_HUMAN => "waiting for the human's approval".to_string(),
        STATE_QUEUED_BATCH => "waiting to land in a batch".to_string(),
        other => other.to_string(),
    }
}

/// At startup, before any tab is up: every land a stopped maiTerm left running is settled from
/// the repos. A repo caught mid-step-2 landed if Base now contains its tested commit (its log line
/// is written then), and goes back to waiting otherwise; the land holds, its temporary worktrees
/// and its locks go, and the coordinator's next `hiveLand` resumes it. Blocks on git and the disk.
pub fn reconcile(data: &parking_lot::RwLock<crate::state::AppData>) -> usize {
    let callers = super::tools::hive_callers(&data.read());
    let n: usize = callers.iter().map(|c| reconcile_in(&c.hive, &c.paths)).sum();
    if n > 0 {
        ::log::info!("hive: {n} land(s) interrupted by the restart held; the coordinator's hiveLand resumes each");
    }
    let u: usize = callers.iter().map(|c| undo::reconcile_in(&c.hive, &c.paths)).sum();
    if u > 0 {
        ::log::info!("hive: {u} undo(s) interrupted by the restart settled");
    }
    // A post-land check and a batch run are in memory only: what one left on disk goes.
    for c in &callers {
        base_check::sweep(&c.hive, &c.paths);
        batch::sweep(&c.hive, &c.paths);
    }
    n + u
}

pub(crate) fn reconcile_in(hive: &Hive, paths: &HivePaths) -> usize {
    // No batch run outlives a restart: the members held here join new ones.
    batch::forget_runs(&hive.id);
    let mut n = 0;
    for w in worker::list_workers(paths) {
        // A worker that already retired keeps its land only as history. One still landing whose
        // land reads `landed` (written before its retirement, which then didn't happen) holds like
        // any other, and the coordinator's hiveLand retires it.
        if w.phase != worker::PHASE_LANDING {
            continue;
        }
        let Some(land) = w.land.clone().filter(|l| l.in_progress() || l.state == STATE_LANDED) else { continue };
        let Ok(dir) = worker::task_dir(paths, &w.task_id) else { continue };
        let mut land = land;
        let request = driver::approved_request(&land);
        for r in land.repos.iter_mut() {
            let Some(repo) = hive.folders.iter().find(|f| f.name == r.folder).map(|f| std::path::PathBuf::from(&f.path)) else { continue };
            if let Some(tmp) = r.tmp.take() {
                git::remove_temp(&repo, std::path::Path::new(&tmp));
                r.tmp_admin = None;
            }
            if let Ok(common) = git::common_dir(&repo) {
                lock::release(&common, &land.id);
            }
            match r.status.as_str() {
                REPO_ADVANCING => {
                    let tested = r.tested.clone().unwrap_or_default();
                    if !tested.is_empty() && git::branch_tip(&repo, &r.target).is_some_and(|t| git::is_ancestor(&repo, &tested, &t)) {
                        // A batch member landed once its merge is in the target; Base went to its chain's tip.
                        let after = r.chain_tip.clone().unwrap_or(tested);
                        r.status = REPO_LANDED.to_string();
                        r.tree_landed = git::tree_of(&repo, &after);
                        r.base_after = Some(after);
                        r.pushed.get_or_insert_with(|| "held: maiTerm restarted before its push".to_string());
                        driver::log_repo(paths, hive, &w, &land.id, &land.summary, &land.review, request.clone(), r);
                    } else {
                        r.status = REPO_TESTED.to_string();
                    }
                }
                REPO_CHECKING => {
                    r.status = REPO_PENDING.to_string();
                    r.job = None;
                }
                _ => {}
            }
            // A batch member's merge sits on its chain's earlier members: never landed alone.
            if r.status != REPO_LANDED && r.chain_tip.is_some() {
                batch::detach_repo(r);
            }
        }
        if land.landed().is_empty() {
            land.batch = None;
        }
        land.state = STATE_HELD.to_string();
        land.waiting_for = None;
        land.held_reason = Some(match land.landed().len() {
            0 => "interrupted by a maiTerm restart: hiveLand again to resume".to_string(),
            k if k == land.repos.len() => "landed, but maiTerm restarted before it finished: hiveLand again to retire the worker".to_string(),
            _ => {
                let done: Vec<&str> = land.landed().iter().map(|r| r.folder.as_str()).collect();
                format!("landed {}; interrupted by a maiTerm restart: hiveLand again to land the rest", done.join(", "))
            }
        });
        land.updated_at = crate::commands::workspace::iso_now();
        let id = land.id.clone();
        let res = worker::update_worker(&dir, |cur| {
            let ours = cur.land.as_ref().is_some_and(|l| l.id == id);
            if ours {
                cur.land = Some(land);
            }
            ours
        });
        match res {
            Ok(_) => n += 1,
            Err(e) => ::log::warn!("hive {}: land {id} of worker {} not reconciled: {e}", hive.id, worker_label(&w)),
        }
    }
    n
}

/// Refused while a land holds a lock or is advancing (plan D13): a held land is over for now, one
/// waiting on the human holds no lock (plan D4), and one every repo of which landed holds nothing
/// either.
pub fn in_flight(w: &WorkerRecord) -> bool {
    w.phase == worker::PHASE_LANDING && !w.land.as_ref().is_some_and(|l| l.is_held() || l.awaits_human() || l.state == STATE_LANDED)
}

/// A landing worker whose land is held, or waits on the human: nothing of it runs, so the worker
/// may report again, stop or be retired (its card then withdrawn), and a held one resumed.
pub fn held(w: &WorkerRecord) -> bool {
    w.phase == worker::PHASE_LANDING && w.land.as_ref().is_some_and(|l| l.is_held() || l.awaits_human())
}

// ── The human's commands ──

/// The Hive in workspace `workspace_id` of this window, as maiTerm acts in it for the human.
pub(crate) fn caller_in(state: &std::sync::Arc<crate::state::AppState>, label: &str, workspace_id: &str) -> Result<Caller, String> {
    super::tools::caller_for_workspace(&state.app_data.read(), workspace_id)
        .filter(|c| c.label == label)
        .ok_or_else(|| "That workspace isn't a Hive in this window.".to_string())
}

/// The arguments a held land resumes with (plan D6, the column menu's Retry): its own pins,
/// summary, order and flags, so nothing the coordinator didn't name lands.
pub(crate) fn retry_args(w: &WorkerRecord) -> Result<Value, String> {
    let label = worker_label(w);
    let Some(l) = w.land.as_ref().filter(|l| w.phase == worker::PHASE_LANDING && l.is_held()) else {
        return Err(format!("Worker {label} has no held land to retry."));
    };
    let commits: Map<String, Value> = l.repos.iter().map(|r| (r.folder.clone(), json!(r.pinned))).collect();
    Ok(json!({
        "worker": w.task_id,
        "summary": l.summary,
        "commits": commits,
        "order": l.order,
        "accept_review": l.review == log::REVIEW_ACCEPTED,
        "fix_forward": l.fix_forward,
    }))
}

/// Retry a held land (plan D6): the human's click in the worker's column, the same as the
/// coordinator's `hiveLand` with the same commits. The coordinator hears what it came to.
pub(crate) async fn retry<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, task_id: &str, opts: driver::Opts) -> Result<Value, String> {
    let dir = worker::task_dir(&caller.paths, task_id)?;
    let w = blocking(move || worker::read_worker(&dir)).await?;
    let args = retry_args(&w)?;
    let label = worker_label(&w);
    let land_id = w.land.as_ref().map(|l| l.id.clone()).unwrap_or_default();
    let res = land_tool_with(ctx, caller, &args, opts).await;
    let says = match &res {
        Ok(v) if v.get("state").and_then(Value::as_str) == Some(STATE_CHECKING) => Some("Its land check runs now; the result follows as ⟦HIVE LAND⟧.".to_string()),
        Ok(v) => v.get("result").and_then(Value::as_str).map(str::to_string),
        Err(e) => Some(e.clone()),
    };
    if let Some(says) = says {
        driver::tell_text(ctx, caller, &w, format!("⟦HIVE LAND · {}⟧ The human retried worker {label}'s held land {land_id}. {says}", one_line(&w.title, 120))).await;
    }
    res
}

#[tauri::command]
pub async fn hive_retry_land(
    window: tauri::Window,
    app: tauri::AppHandle,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    workspace_id: String,
    task_id: String,
) -> Result<Value, String> {
    let caller = caller_in(&state, window.label(), &workspace_id)?;
    let ctx = super::tools::app_ctx(state.inner(), &app, &caller);
    let res = retry(&ctx, &caller, &task_id, driver::Opts::production()).await;
    super::waiters::poll_soon(state.inner().clone(), app.clone(), caller.workspace_id.clone());
    res
}
