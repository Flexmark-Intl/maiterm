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

/// `<app data>/hive/<id>`, or None for an id that isn't one safe path component. Ids are
/// uuids maiTerm mints, but the state file is hand-editable, and this path gets removed from.
pub fn hive_root(id: &str) -> Option<PathBuf> {
    is_component(id).then(|| hives_dir().map(|d| d.join(id))).flatten()
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

    pub fn gallery(&self) -> PathBuf {
        self.root.join("gallery")
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

    pub fn precedents(&self) -> PathBuf {
        self.root.join("precedents")
    }

    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
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
        for bad in ["", "..", ".hidden", "a/b", "a\\b", "c:x"] {
            assert!(hive_root(bad).is_none(), "{bad:?} must not name a Hive folder");
        }
    }

    #[test]
    fn the_layout_matches_the_spec() {
        let p = HivePaths::new("/x/hive/h");
        assert_eq!(p.coordinator_home(), Path::new("/x/hive/h/coordinator/home"));
        assert_eq!(p.coordinator_worktree("web"), Path::new("/x/hive/h/wt.noindex/coordinator/web"));
        assert_eq!(p.coordinator_task(), Path::new("/x/hive/h/tasks/coordinator"));
        assert_eq!(p.memory_index(), Path::new("/x/hive/h/memory/MEMORY.md"));
        assert_eq!(p.worker_worktree("api-auth", "web"), Path::new("/x/hive/h/wt.noindex/api-auth/web"));
        assert_eq!(p.worker_scratch("api-auth"), Path::new("/x/hive/h/wt.noindex/api-auth/scratch"));
        assert_eq!(p.worker_tmp("api-auth"), Path::new("/x/hive/h/wt.noindex/api-auth/claude-tmp"));
        assert_eq!(p.worker_brief("api-auth"), Path::new("/x/hive/h/wt.noindex/api-auth/scratch/brief.md"));
        assert!(p.worker_cargo_target("api-auth").starts_with(p.worker_dir("api-auth")), "under .noindex, out of Spotlight");
    }
}
