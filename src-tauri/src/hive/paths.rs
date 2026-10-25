//! Where a Hive lives on disk (docs/hive.md §6.2): `<app data>/hive/<id>/`, outside every repo
//! so no watcher in one scans it, and under `app_data_slug()` so dev and prod never share one.

use std::path::{Path, PathBuf};

/// `coordinator` names the coordinator's task dir and read copies; no worker may take it.
pub const COORDINATOR_SLUG: &str = "coordinator";
/// `wt.noindex/land/` holds the temporary land worktrees (§8.1).
pub const LAND_SLUG: &str = "land";
pub const RESERVED_SLUGS: [&str; 2] = [COORDINATOR_SLUG, LAND_SLUG];

const SCRATCH: &str = "scratch";
const CLAUDE_TMP: &str = "claude-tmp";
const CARGO_TARGET: &str = "cargo-target";
/// A worker's own folders, beside its worktrees in `wt.noindex/<slug>/`: no Hive folder may
/// take one of these names, or its worktree would be one of them.
pub const WORKER_OWN_DIRS: [&str; 3] = [SCRATCH, CLAUDE_TMP, CARGO_TARGET];

/// A name no Hive folder may take. Ignoring case: on macOS's default volume `Scratch` IS the
/// worker's `scratch`, and `git worktree add` happily checks out into that empty folder.
pub fn is_reserved_folder_name(name: &str) -> bool {
    name.eq_ignore_ascii_case(COORDINATOR_SLUG) || WORKER_OWN_DIRS.iter().any(|d| d.eq_ignore_ascii_case(name))
}

/// `<app data>/hive`.
pub fn hives_dir() -> Option<PathBuf> {
    crate::state::persistence::app_data_dir().map(|d| d.join("hive"))
}

/// `<app data>/hive/bin`: the hook script and the port file it reads (§6.8). Shared by every
/// Hive, outside every agent's writable paths, and on every `denyWrite`.
pub fn hive_bin_dir() -> Option<PathBuf> {
    hives_dir().map(|d| d.join("bin"))
}

/// The global precedents' folder name, beside every Hive's root (§7.5).
pub const GLOBAL_PRECEDENTS: &str = "precedents";
/// Folders under `<app data>/hive/` that are maiTerm's, not a Hive's (plan D23).
pub const RESERVED_IDS: [&str; 2] = ["bin", GLOBAL_PRECEDENTS];

/// `<app data>/hive/precedents`: precedents the human made global (§7.5), read by every Hive.
pub fn global_precedents_dir() -> Option<PathBuf> {
    hives_dir().map(|d| d.join(GLOBAL_PRECEDENTS))
}

/// `<app data>/hive/<id>`, or None for an id that isn't one safe path component or names one of
/// maiTerm's own folders there. Ids are uuids maiTerm mints, but the state file is hand-editable,
/// and this path gets removed from.
pub fn hive_root(id: &str) -> Option<PathBuf> {
    (is_component(id) && !RESERVED_IDS.iter().any(|r| r.eq_ignore_ascii_case(id))).then(|| hives_dir().map(|d| d.join(id))).flatten()
}

/// One path component a name can safely become: no separator, no `..`, no leading dot (a
/// dot-folder under `wt.noindex/` would hide a worktree from the human's own listing).
pub fn is_component(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && !s.starts_with('.')
        && !s.chars().any(|c| matches!(c, '/' | '\\' | ':' | '\0') || c.is_control())
}

/// The fixed layout under one Hive's root. Pure path arithmetic; `layout::create_layout`
/// makes the folders.
#[derive(Debug, Clone)]
pub struct HivePaths {
    pub root: PathBuf,
}

impl HivePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn memory(&self) -> PathBuf {
        self.root.join("memory")
    }

    pub fn memory_index(&self) -> PathBuf {
        self.memory().join("MEMORY.md")
    }

    /// Where the index goes when `MEMORY.md` can't take it (the kept block over its cap, §9).
    pub fn memory_generated(&self) -> PathBuf {
        self.memory().join("MEMORY.generated.md")
    }

    /// Lines found in `MEMORY.md` that link no memory file: moved here, never dropped (§9).
    pub fn memory_unfiled(&self) -> PathBuf {
        self.memory().join("unfiled.md")
    }

    /// Rust's record of the memory folder: who wrote each file, and the human's kept block. Under
    /// `logs/`, which no Hive agent may write (§9 "Provenance comes from maiTerm").
    pub fn memory_record(&self) -> PathBuf {
        self.logs().join("memory.json")
    }

    pub fn gallery(&self) -> PathBuf {
        self.root.join("gallery")
    }

    /// Stored assets, `<id>.<ext>`, read-only (§10.2).
    pub fn gallery_files(&self) -> PathBuf {
        self.gallery().join(super::gallery::FILES)
    }

    pub fn gallery_thumbs(&self) -> PathBuf {
        self.gallery().join(super::gallery::THUMBS)
    }

    pub fn gallery_trash(&self) -> PathBuf {
        self.gallery().join(super::gallery::TRASH)
    }

    /// The gallery's SQLite index, Rust's alone.
    pub fn gallery_index(&self) -> PathBuf {
        self.gallery().join(super::gallery::INDEX)
    }

    /// The thumbnailer's output before it moves into `thumbs/`.
    pub fn gallery_tmp(&self) -> PathBuf {
        self.gallery().join(super::gallery::TMP)
    }

    pub fn tasks(&self) -> PathBuf {
        self.root.join("tasks")
    }

    /// A task's launch record, settings file and session list (§11.1).
    pub fn task(&self, task_id: &str) -> PathBuf {
        self.tasks().join(task_id)
    }

    /// The coordinator has no task, so its launch record takes the reserved slug.
    pub fn coordinator_task(&self) -> PathBuf {
        self.task(COORDINATOR_SLUG)
    }

    /// The plan the human sees first (§7.4 "Show me the plan first"), Rust's: under `tasks/`, on
    /// every agent's deny lists, so no agent can approve its own plan.
    pub fn plan_record(&self) -> PathBuf {
        self.coordinator_task().join("plan.json")
    }

    pub fn precedents(&self) -> PathBuf {
        self.root.join("precedents")
    }

    /// The global precedents' folder: beside this Hive's root, as `global_precedents_dir()` is
    /// beside every Hive's. A test's scratch Hive gets its own.
    pub fn global_precedents(&self) -> PathBuf {
        self.root.parent().unwrap_or(&self.root).join(GLOBAL_PRECEDENTS)
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }

    /// The run log: one line per finished job and per refusal, append-only (§6.8).
    pub fn run_log(&self) -> PathBuf {
        self.logs().join("runs.jsonl")
    }

    /// The land log: one line per repo per land, undo or base check, append-only (§8.6).
    pub fn land_log(&self) -> PathBuf {
        self.logs().join("lands.jsonl")
    }

    /// The delivery log: one line per `galleryDeliver` and per Export… (§10.6), append-only.
    pub fn delivery_log(&self) -> PathBuf {
        self.logs().join("deliveries.jsonl")
    }

    /// The decision log: every prompt and question routed, recommended, answered or lapsed (§7.6).
    pub fn decision_log(&self) -> PathBuf {
        self.logs().join("decisions.jsonl")
    }

    /// Every change of the completion criteria (§11.4), append-only.
    pub fn criteria_log(&self) -> PathBuf {
        self.logs().join("criteria.jsonl")
    }

    /// Done's reports and wakes (§11.4), append-only: maiTerm's record of what finished the Hive,
    /// under `logs/`, which no Hive agent may write.
    pub fn finishes(&self) -> PathBuf {
        self.logs().join("finishes.jsonl")
    }

    /// Undos while they run, wait or hold (§8.6), one file each: under `logs/`, which no Hive
    /// agent may write.
    pub fn undos(&self) -> PathBuf {
        self.logs().join("undos")
    }

    /// One land's temporary worktrees and its check's temp folder (§8.1).
    pub fn land_dir(&self, land_id: &str) -> PathBuf {
        self.worktrees().join(LAND_SLUG).join(land_id)
    }

    pub fn land_worktree(&self, land_id: &str, folder: &str) -> PathBuf {
        self.land_dir(land_id).join(folder)
    }

    /// A land check's `TMPDIR`: beside its worktrees, never inside the tree it tests. A dot name,
    /// which no Hive folder can have (`is_component`), so it never is one of those worktrees.
    pub fn land_tmp(&self, land_id: &str) -> PathBuf {
        self.land_dir(land_id).join(".tmp")
    }

    /// Each job's sandbox profile and pass mark, Rust's alone: on every worker's `denyWrite`, so
    /// neither a worker nor its job can plant a mark or rewrite a profile.
    pub fn runs(&self) -> PathBuf {
        self.root.join("runs")
    }

    pub fn run(&self, job_id: &str) -> PathBuf {
        self.runs().join(job_id)
    }

    /// The coordinator's working directory: never the root, whose Grep would walk every
    /// worktree and the gallery (§6.3).
    pub fn coordinator_home(&self) -> PathBuf {
        self.root.join("coordinator").join("home")
    }

    /// The rest of the coordinator's brief, which its invariant rules tell it to read (§7.1).
    pub fn coordinator_brief(&self) -> PathBuf {
        self.coordinator_home().join("brief.md")
    }

    /// The coordinator's `CLAUDE_CODE_TMPDIR` (§6.7).
    pub fn coordinator_tmp(&self) -> PathBuf {
        self.root.join("coordinator").join("claude-tmp")
    }

    /// Every worktree lives under a `.noindex` folder: Spotlight indexed a fresh worktree at
    /// ~100% CPU otherwise (spike 4).
    pub fn worktrees(&self) -> PathBuf {
        self.root.join("wt.noindex")
    }

    /// The coordinator's detached read copy of one git folder.
    pub fn coordinator_worktree(&self, folder: &str) -> PathBuf {
        self.worktrees().join(COORDINATOR_SLUG).join(folder)
    }

    /// Everything one worker has on disk but its task folder: worktrees, scratch, temp.
    pub fn worker_dir(&self, slug: &str) -> PathBuf {
        self.worktrees().join(slug)
    }

    /// The worker's worktree of one git folder.
    pub fn worker_worktree(&self, slug: &str, folder: &str) -> PathBuf {
        self.worker_dir(slug).join(folder)
    }

    /// The worker's own working space, and its cwd when no git folder is its task's (§6.3).
    pub fn worker_scratch(&self, slug: &str) -> PathBuf {
        self.worker_dir(slug).join(SCRATCH)
    }

    /// The worker's `CLAUDE_CODE_TMPDIR` (§6.7).
    pub fn worker_tmp(&self, slug: &str) -> PathBuf {
        self.worker_dir(slug).join(CLAUDE_TMP)
    }

    /// A queued job's `TMPDIR`: inside the worker's own temp, which its sandbox already writes,
    /// so a job never writes anywhere its worker can't.
    pub fn worker_job_tmp(&self, slug: &str) -> PathBuf {
        self.worker_tmp(slug).join("jobs")
    }

    /// The worker's own `CARGO_TARGET_DIR`: one shared target serializes every build on its
    /// lock (§6.5).
    pub fn worker_cargo_target(&self, slug: &str) -> PathBuf {
        self.worker_dir(slug).join(CARGO_TARGET)
    }

    /// The rest of a worker's brief, rewritten on every launch (§7.1).
    pub fn worker_brief(&self, slug: &str) -> PathBuf {
        self.worker_scratch(slug).join("brief.md")
    }

    /// Which `.claude.json` trust keys maiTerm itself wrote, so removal touches only those.
    pub fn trust_record(&self) -> PathBuf {
        self.root.join("trust-written.json")
    }

    pub fn contains(&self, p: &Path) -> bool {
        p.starts_with(&self.root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_root_is_under_the_app_data_slug_and_refuses_unsafe_ids() {
        let root = hive_root("0b6f-1").unwrap();
        assert!(root.ends_with(Path::new(crate::state::persistence::app_data_slug()).join("hive").join("0b6f-1")));
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "c:x", "bin", "precedents", "Precedents"] {
            assert!(hive_root(bad).is_none(), "{bad:?} must not name a Hive folder");
        }
        assert_eq!(HivePaths::new(root).global_precedents(), global_precedents_dir().unwrap(), "every Hive reads the one global folder");
    }

    #[test]
    fn the_layout_matches_the_spec() {
        let p = HivePaths::new("/x/hive/h");
        assert_eq!(p.coordinator_home(), Path::new("/x/hive/h/coordinator/home"));
        assert_eq!(p.coordinator_worktree("web"), Path::new("/x/hive/h/wt.noindex/coordinator/web"));
        assert_eq!(p.coordinator_task(), Path::new("/x/hive/h/tasks/coordinator"));
        assert!(p.plan_record().starts_with(p.tasks()), "Rust's, under a folder on every deny list");
        assert_eq!(p.memory_index(), Path::new("/x/hive/h/memory/MEMORY.md"));
        assert_eq!(p.memory_record(), Path::new("/x/hive/h/logs/memory.json"));
        assert!(p.finishes().starts_with(p.logs()) && p.criteria_log().starts_with(p.logs()), "Rust's, under a folder no agent writes");
        assert_eq!(p.memory_generated(), Path::new("/x/hive/h/memory/MEMORY.generated.md"));
        assert_eq!(p.memory_unfiled(), Path::new("/x/hive/h/memory/unfiled.md"));
        assert_eq!(p.gallery_files(), Path::new("/x/hive/h/gallery/files"));
        assert_eq!(p.gallery_thumbs(), Path::new("/x/hive/h/gallery/thumbs"));
        assert_eq!(p.gallery_trash(), Path::new("/x/hive/h/gallery/trash"));
        assert_eq!(p.gallery_index(), Path::new("/x/hive/h/gallery/index.sqlite"));
        assert_eq!(p.gallery_tmp(), Path::new("/x/hive/h/gallery/.tmp"));
        assert_eq!(p.worker_worktree("api-auth", "web"), Path::new("/x/hive/h/wt.noindex/api-auth/web"));
        assert_eq!(p.worker_scratch("api-auth"), Path::new("/x/hive/h/wt.noindex/api-auth/scratch"));
        assert_eq!(p.worker_tmp("api-auth"), Path::new("/x/hive/h/wt.noindex/api-auth/claude-tmp"));
        assert_eq!(p.worker_brief("api-auth"), Path::new("/x/hive/h/wt.noindex/api-auth/scratch/brief.md"));
        assert!(p.worker_cargo_target("api-auth").starts_with(p.worker_dir("api-auth")), "under .noindex, out of Spotlight");
    }
}
