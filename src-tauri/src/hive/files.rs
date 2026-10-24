//! The folder the Files column shows for a Hive agent (docs/hive.md §4.1). A Hive has no active
//! tab, so the column follows the focused column's tab, and the folder that tab's agent works in
//! is known here rather than inferred: its shell cwd, OSC 7 and last folder are a launch
//! detail (a worker's setup may `cd`), and the coordinator's is `coordinator/home/`, which holds
//! nothing worth browsing.

use std::path::PathBuf;

use super::paths::HivePaths;
use super::record::Hive;
use super::worker::WorkerRecord;
use super::{ROLE_COORDINATOR, ROLE_WORKER};

/// A worker: its worktree of the Hive's primary folder, else where it works
/// (`WorkerRecord::cwd`: its first worktree, or `scratch/`). The coordinator: its read copy of a
/// git primary (§6.3), or the plain primary itself. None when there is nothing to say (no
/// primary, no worker record), so the ordinary chain answers.
pub fn files_root(hive: &Hive, role: &str, paths: &HivePaths, worker: Option<&WorkerRecord>) -> Option<PathBuf> {
    let primary = hive.primary();
    match role {
        ROLE_COORDINATOR => {
            let p = primary?;
            Some(if p.is_git() { paths.coordinator_worktree(&p.name) } else { PathBuf::from(&p.path) })
        }
        ROLE_WORKER => {
            let w = worker?;
            let own = primary.and_then(|p| w.folders.iter().find(|f| f.name == p.name)).and_then(|f| f.worktree.as_ref());
            Some(own.map(PathBuf::from).unwrap_or_else(|| w.cwd(paths)))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::record::HiveFolder;
    use crate::hive::worker::WorkerFolder;

    fn folder(name: &str, kind: &str, primary: bool) -> HiveFolder {
        HiveFolder { name: name.into(), path: format!("/src/{name}"), kind: kind.into(), primary, ..HiveFolder::default() }
    }

    fn worktree(name: &str, at: &str) -> WorkerFolder {
        WorkerFolder { name: name.into(), kind: "git".into(), worktree: Some(at.into()), ..WorkerFolder::default() }
    }

    #[test]
    fn a_worker_shows_its_worktree_of_the_primary_else_where_it_works() {
        let paths = HivePaths::new("/x/hive/h");
        let hive = Hive { folders: vec![folder("api", "git", false), folder("web", "git", true)], ..Hive::default() };
        let mut w = WorkerRecord { slug: "auth".into(), folders: vec![worktree("api", "/wt/auth/api"), worktree("web", "/wt/auth/web")], ..WorkerRecord::default() };
        // The primary's worktree, even when another folder comes first (and is its cwd).
        assert_eq!(files_root(&hive, ROLE_WORKER, &paths, Some(&w)), Some(PathBuf::from("/wt/auth/web")));
        // A task without the primary: where it works.
        w.folders = vec![worktree("api", "/wt/auth/api")];
        assert_eq!(files_root(&hive, ROLE_WORKER, &paths, Some(&w)), Some(PathBuf::from("/wt/auth/api")));
        // No git folder at all: its scratch.
        w.folders = vec![WorkerFolder { name: "brand".into(), kind: "plain".into(), ..WorkerFolder::default() }];
        assert_eq!(files_root(&hive, ROLE_WORKER, &paths, Some(&w)), Some(paths.worker_scratch("auth")));
        assert_eq!(files_root(&hive, ROLE_WORKER, &paths, None), None, "no worker.json names the tab");
    }

    #[test]
    fn the_coordinator_shows_its_read_copy_or_the_plain_primary() {
        let paths = HivePaths::new("/x/hive/h");
        let git = Hive { folders: vec![folder("brand", "plain", false), folder("web", "git", true)], ..Hive::default() };
        assert_eq!(files_root(&git, ROLE_COORDINATOR, &paths, None), Some(paths.coordinator_worktree("web")));
        let plain = Hive { folders: vec![folder("footage", "plain", true)], ..Hive::default() };
        assert_eq!(files_root(&plain, ROLE_COORDINATOR, &paths, None), Some(PathBuf::from("/src/footage")));
        assert_eq!(files_root(&Hive::default(), ROLE_COORDINATOR, &paths, None), None, "no primary: the ordinary chain");
        assert_eq!(files_root(&git, "auditor", &paths, None), None, "a role a newer build wrote");
    }
}
