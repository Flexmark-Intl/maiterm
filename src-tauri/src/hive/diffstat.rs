//! A worker's diff stat (docs/hive.md §4.4): what it changed against its landing target, summed
//! over its worktrees, recomputed after its edits (debounced per worker) and pushed to its Hive's
//! window as `hive-diff-stat`.
//!
//! The worktree is the worker's: it writes every file there, `.gitattributes` and the `.git` file
//! that names its git dir included, though not the repo's config. So maiTerm's git here names the
//! git dir it recorded at creation (`git::git_in_worktree`), runs no fsmonitor or hook, no diff
//! driver or textconv a worker's attributes could pick, and never writes the worker's index. A
//! filter driver its attributes name still runs: only the human's config can define one.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::json;
use tauri::AppHandle;

use super::feed::{self, Debounce};
use super::git;
use super::paths::HivePaths;
use super::tools::{blocking, Caller};
use super::worker::{self, WorkerFolder, WorkerRecord};
use crate::state::AppState;

pub const EVENT: &str = "hive-diff-stat";
const DEBOUNCE_MS: i64 = 5_000;

/// `git diff --shortstat`, parsed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Shortstat {
    pub files: u32,
    pub insertions: u32,
    pub deletions: u32,
}

impl Shortstat {
    fn add(&mut self, o: Shortstat) {
        self.files = self.files.saturating_add(o.files);
        self.insertions = self.insertions.saturating_add(o.insertions);
        self.deletions = self.deletions.saturating_add(o.deletions);
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct DiffStat {
    pub task_id: String,
    /// Tracked files, committed and not, against the merge base with its target.
    pub total: Shortstat,
    /// Files git doesn't track or ignore, counted apart: `--shortstat` can't see them.
    pub untracked: u32,
    /// Commits only: what a land would take.
    pub committed: Shortstat,
    /// Epoch ms.
    pub computed_at: i64,
    /// A worktree that couldn't be measured, by folder name; the sums leave it out.
    pub error: Option<String>,
}

impl DiffStat {
    /// Equal but for when it was computed: an unchanged stat isn't pushed again.
    fn same_as(&self, o: &DiffStat) -> bool {
        DiffStat { computed_at: o.computed_at, ..self.clone() } == *o
    }
}

/// ` 3 files changed, 212 insertions(+), 40 deletions(-)`; either count is left out when zero,
/// and nothing at all is printed for no change. Run under `LC_ALL=C`: git translates this line.
pub fn parse_shortstat(s: &str) -> Shortstat {
    let mut out = Shortstat::default();
    for part in s.trim().split(',') {
        let mut words = part.split_whitespace();
        let (Some(n), Some(what)) = (words.next().and_then(|n| n.parse::<u32>().ok()), words.next()) else { continue };
        if what.starts_with("file") {
            out.files = n;
        } else if what.starts_with("insertion") {
            out.insertions = n;
        } else if what.starts_with("deletion") {
            out.deletions = n;
        }
    }
    out
}

/// maiTerm's read-only git in a worker's worktree, on the git dir maiTerm recorded for it.
fn git_at(f: &WorkerFolder) -> Result<std::process::Command, String> {
    let mut cmd = git::git_in_worktree(f)?;
    cmd.args(["-c", "core.pager=cat"])
        // `git diff` against the working tree writes back the stat info it refreshed, optional
        // locks or not (git 2.54): this keeps it to memory, and still skips a merely touched file.
        .args(["-c", "diff.autoRefreshIndex=false"])
        .env("LC_ALL", "C")
        .env_remove("GIT_EXTERNAL_DIFF");
    Ok(cmd)
}

fn diff_args(from: &str, to: Option<&str>) -> Vec<String> {
    let mut a: Vec<String> = ["diff", "--shortstat", "--no-ext-diff", "--no-textconv", "--no-color"].map(String::from).to_vec();
    a.push(from.to_string());
    a.extend(to.map(String::from));
    a.push("--".to_string());
    a
}

/// (total, untracked, committed) for one worktree. Blocks: four git calls.
fn folder_stat(f: &WorkerFolder) -> Result<(Shortstat, u32, Shortstat), String> {
    let wt = f.worktree.as_deref().ok_or("no worktree")?;
    if !Path::new(wt).is_dir() {
        return Err("its worktree is gone".to_string());
    }
    // A full ref, so a tag or file the worker named like the target can't stand in for it.
    let base = match (&f.target, &f.base_sha) {
        (Some(t), _) => format!("refs/heads/{t}"),
        (None, Some(sha)) => sha.clone(),
        (None, None) => return Err("no landing target".to_string()),
    };
    let run = |args: Vec<String>| git::run(git_at(f)?.args(args), git::QUERY_TIMEOUT);
    let mb = run(vec!["merge-base".into(), base, "HEAD".into()])?;
    let total = parse_shortstat(&run(diff_args(&mb, None))?);
    // `<target>...HEAD`, with the merge base already in hand.
    let committed = parse_shortstat(&run(diff_args(&mb, Some("HEAD")))?);
    let others = run(["ls-files", "--others", "--exclude-standard", "-z"].map(String::from).to_vec())?;
    let untracked = others.split('\0').filter(|p| !p.is_empty()).count() as u32;
    Ok((total, untracked, committed))
}

/// The worker's stat summed over its git folders; None when it has no worktree to measure (a
/// plain-folder worker). Blocks.
pub fn compute(w: &WorkerRecord) -> Option<DiffStat> {
    let folders: Vec<&WorkerFolder> = w.git_folders().collect();
    if folders.is_empty() {
        return None;
    }
    log::debug!("hive: diff stat for task {} ({} worktrees)", w.task_id, folders.len());
    let mut stat = DiffStat { task_id: w.task_id.clone(), computed_at: super::fuses::now_ms(), ..DiffStat::default() };
    let mut errors: Vec<String> = Vec::new();
    for f in folders {
        match folder_stat(f) {
            Ok((total, untracked, committed)) => {
                stat.total.add(total);
                stat.untracked = stat.untracked.saturating_add(untracked);
                stat.committed.add(committed);
            }
            Err(e) => errors.push(format!("{}: {}", f.name, super::deliver::one_line(&e, 200))),
        }
    }
    stat.error = (!errors.is_empty()).then(|| errors.join("; "));
    Some(stat)
}

static DEBOUNCE: LazyLock<Mutex<Debounce>> = LazyLock::new(|| Mutex::new(Debounce::new(DEBOUNCE_MS)));
/// The last stat per `hive_id:task_id`: pushed only when it changes, served at the view's mount.
static CACHE: LazyLock<Mutex<HashMap<String, DiffStat>>> = LazyLock::new(Default::default);
/// Worker tab → task, so a hook doesn't read every `worker.json` to find its worker. A tab never
/// changes worker: a restart or reload gives the worker a new tab.
static TAB_TASK: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);

fn key(hive_id: &str, task_id: &str) -> String {
    format!("{hive_id}:{task_id}")
}

/// Caches `stat`; true when it differs from the last one but for its time.
fn remember(hive_id: &str, stat: &DiffStat) -> bool {
    let mut cache = CACHE.lock();
    let k = key(hive_id, &stat.task_id);
    let changed = !cache.get(&k).is_some_and(|old| old.same_as(stat));
    cache.insert(k, stat.clone());
    changed
}

/// A worker that's gone (its spawn rolled back): nothing more is computed or served for it.
pub(crate) fn forget(hive_id: &str, task_id: &str) {
    let k = key(hive_id, task_id);
    CACHE.lock().remove(&k);
    DEBOUNCE.lock().forget(&k);
    TAB_TASK.lock().retain(|_, t| t != task_id);
}

/// Something may have changed the worker's files: recompute after the debounce and push the stat
/// to its Hive's window if it moved.
pub(crate) fn touch(handle: &AppHandle, caller: &Caller, task_id: &str) {
    let (handle, label, ws, hive_id, paths, task) = (
        handle.clone(),
        caller.label.clone(),
        caller.workspace_id.clone(),
        caller.hive.id.clone(),
        caller.paths.clone(),
        task_id.to_string(),
    );
    feed::schedule(&DEBOUNCE, key(&hive_id, task_id), move || {
        let (handle, label, ws, hive_id, paths, task) =
            (handle.clone(), label.clone(), ws.clone(), hive_id.clone(), paths.clone(), task.clone());
        async move {
            let t = task.clone();
            let stat = blocking(move || worker::read_worker(&worker::task_dir(&paths, &t)?).map(|w| compute(&w))).await;
            let stat = match stat {
                Ok(Some(s)) => s,
                Ok(None) => return,
                Err(e) => {
                    log::info!("hive {hive_id}: task {task}: no diff stat: {e}");
                    return;
                }
            };
            if remember(&hive_id, &stat) {
                feed::emit(&handle, &label, EVENT, json!({ "workspace_id": ws, "stat": stat }));
            }
        }
    });
}

/// `touch` for the worker in `tab_id`, found once and remembered.
pub(crate) fn touch_tab(handle: &AppHandle, caller: &Caller, tab_id: &str) {
    let known = TAB_TASK.lock().get(tab_id).cloned();
    if let Some(task) = known {
        return touch(handle, caller, &task);
    }
    let (handle, caller, tab) = (handle.clone(), caller.clone(), tab_id.to_string());
    tauri::async_runtime::spawn(async move {
        let (paths, t) = (caller.paths.clone(), tab.clone());
        let Ok(Some(w)) = blocking(move || Ok(worker::worker_for_tab(&paths, &t))).await else { return };
        TAB_TASK.lock().insert(tab, w.task_id.clone());
        touch(&handle, &caller, &w.task_id);
    });
}

/// Every worker's stat for the Hive view's mount: cached, or computed once here for a worker not
/// measured yet. Blocks.
fn stats_for(hive_id: &str, paths: &HivePaths) -> Vec<DiffStat> {
    worker::list_workers(paths)
        .iter()
        .filter_map(|w| {
            let cached = CACHE.lock().get(&key(hive_id, &w.task_id)).cloned();
            cached.or_else(|| {
                let s = compute(w)?;
                remember(hive_id, &s);
                Some(s)
            })
        })
        .collect()
}

/// The board's diff stats at mount; after this it keeps up through `hive-diff-stat`.
#[tauri::command]
pub async fn hive_diff_stats(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    workspace_id: String,
) -> Result<Vec<DiffStat>, String> {
    let hive_id = {
        let data = state.app_data.read();
        let ws = data
            .window(window.label())
            .and_then(|w| w.workspaces.iter().find(|w| w.id == workspace_id))
            .ok_or("Workspace not found")?;
        ws.hive.as_ref().ok_or("That workspace isn't a Hive")?.id.clone()
    };
    let root = super::paths::hive_root(&hive_id).ok_or("No Hive folder")?;
    blocking(move || Ok(stats_for(&hive_id, &HivePaths::new(root)))).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::record::{Hive, HiveFolder, KIND_GIT, KIND_PLAIN};
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;

    #[test]
    fn shortstat_lines_parse() {
        let s = |f, i, d| Shortstat { files: f, insertions: i, deletions: d };
        assert_eq!(parse_shortstat(" 3 files changed, 212 insertions(+), 40 deletions(-)\n"), s(3, 212, 40));
        assert_eq!(parse_shortstat(" 1 file changed, 1 insertion(+)"), s(1, 1, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 2 deletions(-)"), s(1, 0, 2));
        assert_eq!(parse_shortstat(" 2 files changed, 1 insertion(+), 1 deletion(-)"), s(2, 1, 1));
        assert_eq!(parse_shortstat(""), s(0, 0, 0));
        assert_eq!(parse_shortstat(" 1 file changed, 0 insertions(+), 0 deletions(-)"), s(1, 0, 0), "a mode change");
    }

    #[test]
    fn an_unchanged_stat_is_not_news_whenever_it_was_computed() {
        let hive = format!("h-{}", uuid::Uuid::new_v4());
        let a = DiffStat { task_id: "t1".into(), total: Shortstat { files: 1, insertions: 2, deletions: 0 }, computed_at: 1, ..DiffStat::default() };
        assert!(remember(&hive, &a));
        assert!(!remember(&hive, &DiffStat { computed_at: 99, ..a.clone() }));
        assert!(remember(&hive, &DiffStat { untracked: 1, ..a.clone() }));
        assert!(remember(&hive, &DiffStat { error: Some("web: gone".into()), untracked: 1, ..a.clone() }));
        forget(&hive, "t1");
        assert!(CACHE.lock().get(&key(&hive, "t1")).is_none());
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    struct Rig {
        base: PathBuf,
        repo: PathBuf,
        wt: PathBuf,
        worker: WorkerRecord,
    }

    impl Drop for Rig {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.base);
        }
    }

    /// A repo on `main` (a README, a `.gitignore` for `*.log`) and one worker's worktree of it.
    fn rig() -> Rig {
        let base = std::env::temp_dir().join(format!("hive-diffstat-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&base).unwrap();
        let base = crate::share::canon(&base);
        let repo = base.join("web");
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("README.md"), "one\ntwo\nthree\n").unwrap();
        fs::write(repo.join(".gitignore"), "*.log\n").unwrap();
        git(&repo, &["add", "-A"]);
        git(&repo, &["commit", "-q", "--no-verify", "-m", "init"]);
        let folder = HiveFolder {
            name: "web".into(),
            path: repo.to_string_lossy().into(),
            kind: KIND_GIT.into(),
            base: Some("main".into()),
            primary: true,
            ..HiveFolder::default()
        };
        let hive = Hive { id: "h1".into(), slug: "ship".into(), folders: vec![folder], ..Hive::default() };
        let paths = HivePaths::new(base.join("hive/h1"));
        let folders = super::super::worktree::create_worker_worktrees(&hive, &paths, "auth", &["web".to_string()]).unwrap();
        let wt = PathBuf::from(folders[0].worktree.clone().unwrap());
        let worker = WorkerRecord { task_id: "t1".into(), slug: "auth".into(), folders, ..WorkerRecord::default() };
        Rig { base, repo, wt, worker }
    }

    fn commit(wt: &Path, file: &str, text: &str) {
        fs::write(wt.join(file), text).unwrap();
        git(wt, &["add", file]);
        git(wt, &["commit", "-q", "--no-verify", "-m", file]);
    }

    fn stat(r: &Rig) -> DiffStat {
        let s = compute(&r.worker).expect("a git worker");
        assert_eq!(s.error, None);
        assert_eq!(s.task_id, "t1");
        s
    }

    #[test]
    fn committed_work_counts_in_both() {
        let r = rig();
        commit(&r.wt, "a.txt", "x\ny\n");
        let s = stat(&r);
        let want = Shortstat { files: 1, insertions: 2, deletions: 0 };
        assert_eq!((s.total, s.committed, s.untracked), (want, want, 0));
    }

    #[test]
    fn uncommitted_edits_count_only_in_the_total() {
        let r = rig();
        fs::write(r.wt.join("README.md"), "one\n2\nthree\nfour\n").unwrap();
        let s = stat(&r);
        assert_eq!(s.total, Shortstat { files: 1, insertions: 2, deletions: 1 });
        assert_eq!(s.committed, Shortstat::default(), "+0 −0 until a commit");
    }

    #[test]
    fn mixed_work_and_untracked_files() {
        let r = rig();
        commit(&r.wt, "a.txt", "x\n");
        fs::write(r.wt.join("README.md"), "one\nthree\n").unwrap();
        fs::write(r.wt.join("new.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(r.wt.join("dir/sub")).unwrap();
        fs::write(r.wt.join("dir/sub/b.rs"), "b\n").unwrap();
        fs::write(r.wt.join("dir/c.rs"), "c\n").unwrap();
        fs::write(r.wt.join("build.log"), "ignored\n").unwrap();
        // Base moves on after the branch: neither number counts the human's new commit.
        commit(&r.repo, "human.txt", "h\n");
        let s = stat(&r);
        assert_eq!(s.total, Shortstat { files: 2, insertions: 1, deletions: 1 });
        assert_eq!(s.committed, Shortstat { files: 1, insertions: 1, deletions: 0 });
        assert_eq!(s.untracked, 3, "each untracked file, never an ignored one");
    }

    #[test]
    fn measuring_never_writes_the_workers_index() {
        let r = rig();
        let admin = PathBuf::from(r.worker.folders[0].admin_dir.clone().unwrap());
        let index = admin.join("index");
        // A tracked file whose stat changed but whose content didn't: a plain `git diff` refreshes
        // the index and writes it back, GIT_OPTIONAL_LOCKS=0 or not.
        let readme = fs::File::options().write(true).open(r.wt.join("README.md")).unwrap();
        readme.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5)).unwrap();
        drop(readme);
        let before = fs::read(&index).unwrap();
        let s = stat(&r);
        assert_eq!(s.total, Shortstat::default());
        assert_eq!(fs::read(&index).unwrap(), before, "the index is byte for byte the same");
    }

    #[test]
    fn no_target_falls_back_to_the_base_sha() {
        let mut r = rig();
        commit(&r.wt, "a.txt", "x\n");
        r.worker.folders[0].target = None;
        let s = stat(&r);
        assert_eq!(s.committed, Shortstat { files: 1, insertions: 1, deletions: 0 });
        r.worker.folders[0].base_sha = None;
        let e = compute(&r.worker).unwrap().error.unwrap();
        assert!(e.starts_with("web: ") && e.contains("no landing target"), "{e}");
    }

    #[test]
    fn a_forged_git_file_is_not_followed() {
        let r = rig();
        commit(&r.wt, "a.txt", "x\n");
        // The worker points its worktree's `.git` at a repo of its own, whose config it wrote.
        let fake = r.base.join("fake");
        git(&r.base, &["init", "-q", "fake"]);
        fs::write(r.wt.join(".git"), format!("gitdir: {}\n", fake.join(".git").display())).unwrap();
        let s = stat(&r);
        assert_eq!(s.committed, Shortstat { files: 1, insertions: 1, deletions: 0 }, "read through the recorded git dir");
        let mut no_admin = r.worker.clone();
        no_admin.folders[0].admin_dir = Some(fake.join(".git").to_string_lossy().into());
        let e = compute(&no_admin).unwrap().error.unwrap();
        assert!(e.contains("isn't a worktree's"), "{e}");
    }

    #[test]
    fn a_worker_without_a_worktree_has_no_stat() {
        let plain = WorkerRecord {
            task_id: "t".into(),
            folders: vec![WorkerFolder { name: "brand".into(), kind: KIND_PLAIN.into(), ..WorkerFolder::default() }],
            ..WorkerRecord::default()
        };
        assert!(compute(&plain).is_none());
        let gone = WorkerRecord {
            task_id: "t".into(),
            folders: vec![WorkerFolder { name: "web".into(), kind: KIND_GIT.into(), worktree: Some("/nonexistent/wt".into()), ..WorkerFolder::default() }],
            ..WorkerRecord::default()
        };
        let e = compute(&gone).unwrap().error.unwrap();
        assert!(e.contains("worktree is gone"), "{e}");
    }
}
