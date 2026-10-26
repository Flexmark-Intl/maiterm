//! A Hive folder's dependencies (docs/hive.md §6.4, §6.8; first real use, 2026-10-26): the install
//! its lockfile names, offered as the folder's setup on the Create sheet, and runnable again
//! mid-run through `hiveRun`, under the worker's own job profile with the package registries
//! reachable through `registry_proxy`.
//!
//! A worker can't install in its own shell. Claude's sandbox denies writes to `.vscode`, `.idea`,
//! `.claude/commands|agents` and git hooks anywhere under the session's working folder
//! (`<cwd>/**/.vscode/**`, read out of 2.1.310's sandbox profile), after every allow, and nothing
//! short of switching its filesystem rules off (`filesystem.disabled`) exempts a subtree. Packages
//! ship `.vscode` folders, so `pnpm install` failed in every worktree. A job's Seatbelt profile is
//! maiTerm's own: it denies those names only at a worktree's top
//! (`settings::worker_deny_write_paths`).

use std::path::Path;

use super::record::{Hive, QueuedCommand};

/// One package manager, as a folder's lockfile names it.
#[derive(Debug, PartialEq, Eq)]
pub struct Installer {
    pub name: &'static str,
    /// Any one of these at the folder's top names it.
    pub lockfiles: &'static [&'static str],
    /// And all of these, beside it.
    pub requires: &'static [&'static str],
    /// The setup the Create sheet offers: installs exactly what the lockfile holds.
    pub setup: &'static str,
    /// What `hiveRun` takes for it: a reinstall once a manifest changed.
    pub prefixes: &'static [&'static str],
    /// Set on its job's line, under the entry's own env: where it would otherwise write outside
    /// its stores.
    pub env: &'static [(&'static str, &'static str)],
}

/// JavaScript's, the first that matches: a pnpm or bun repo may hold a stray `package-lock.json`.
pub const JS: &[Installer] = &[
    Installer {
        name: "pnpm",
        lockfiles: &["pnpm-lock.yaml"],
        requires: &[],
        setup: "pnpm install --frozen-lockfile",
        prefixes: &["pnpm install"],
        env: &[],
    },
    Installer {
        name: "bun",
        lockfiles: &["bun.lock", "bun.lockb"],
        requires: &[],
        setup: "bun install --frozen-lockfile",
        prefixes: &["bun install"],
        env: &[],
    },
    // Yarn 2+ (Berry) refuses classic's flag; its `.yarnrc.yml` tells them apart.
    Installer {
        name: "yarn",
        lockfiles: &["yarn.lock"],
        requires: &[".yarnrc.yml"],
        setup: "yarn install --immutable",
        prefixes: &["yarn install"],
        env: &[],
    },
    Installer {
        name: "yarn",
        lockfiles: &["yarn.lock"],
        requires: &[],
        setup: "yarn install --frozen-lockfile",
        prefixes: &["yarn install"],
        env: &[],
    },
    Installer {
        name: "npm",
        lockfiles: &["package-lock.json", "npm-shrinkwrap.json"],
        requires: &[],
        setup: "npm ci",
        prefixes: &["npm ci", "npm install"],
        env: &[],
    },
];

/// Python's, likewise.
pub const PYTHON: &[Installer] = &[
    Installer {
        name: "uv",
        lockfiles: &["uv.lock"],
        requires: &[],
        setup: "uv sync --frozen",
        prefixes: &["uv sync"],
        // An interpreter goes in uv's data dir, which a job can't write: the setup installed the
        // one the lockfile wants, and a missing one is said as that, not as a refused write.
        env: &[("UV_PYTHON_DOWNLOADS", "never")],
    },
    // The venv in the worktree, never beside the human's projects' in the cache dir. Poetry uses
    // an existing `.venv` unasked, so the worker's own shell finds it too.
    Installer {
        name: "poetry",
        lockfiles: &["poetry.lock"],
        requires: &[],
        setup: "poetry install",
        prefixes: &["poetry install"],
        env: &[("POETRY_VIRTUALENVS_IN_PROJECT", "true")],
    },
];

/// The installers a folder's top names: one per ecosystem at most. Cargo has none: a build
/// fetches through a worker's own sandbox (the registries are on its allowlist), and its warm
/// start is the slim seed (§6.5). Reads the disk.
pub fn detect(dir: &Path) -> Vec<&'static Installer> {
    let named = |i: &&Installer| i.lockfiles.iter().any(|l| dir.join(l).is_file()) && i.requires.iter().all(|r| dir.join(r).is_file());
    [JS, PYTHON].iter().filter_map(|group| group.iter().find(named)).collect()
}

/// The setup a folder's lockfiles name, for the Create sheet to show and the human to edit. One
/// typed line: `launch::first_launch_setup` runs it in a fresh worktree before the agent starts.
pub fn setup_for(dir: &Path) -> Option<String> {
    let setups: Vec<&str> = detect(dir).iter().map(|i| i.setup).collect();
    (!setups.is_empty()).then(|| setups.join(" && "))
}

/// Whether a queued command is a package install, whose job reaches the registries
/// (`registry_proxy`) and writes its manager's caches: a human's entry as much as a detected one.
pub fn is_install(words: &[&str]) -> bool {
    JS.iter().chain(PYTHON).any(|i| runs(i, words))
}

/// Whether `words` start with one of `i`'s prefixes, word by word.
fn runs(i: &Installer, words: &[&str]) -> bool {
    i.prefixes.iter().any(|p| {
        let p: Vec<&str> = p.split_whitespace().collect();
        words.len() >= p.len() && words[..p.len()] == p[..]
    })
}

/// Where every package manager keeps its downloads and caches during a worker's install: the
/// worker's own cache folder (`HivePaths::worker_deps_cache`), never the human's. Package
/// caches hold code that runs unpacked and unchecked (pnpm's `dlx` bins, uv's tool environments,
/// yarn's and bun's unpacked packages), so a worker that could write the human's would run code
/// as the human the next time they typed `pnpm dlx` or `uvx`. Each manager is pointed by its own
/// variable, and the XDG ones catch what reads those; one that ignores both writes `~`, which
/// the job's profile refuses: the install fails, it never escapes.
pub fn cache_env(dir: &Path) -> Vec<(String, String)> {
    let at = |sub: &str| dir.join(sub).to_string_lossy().to_string();
    [
        ("XDG_CACHE_HOME", "xdg-cache"),
        ("XDG_DATA_HOME", "xdg-data"),
        ("XDG_STATE_HOME", "xdg-state"),
        ("npm_config_cache", "npm"),
        ("npm_config_store_dir", "pnpm-store"),
        ("npm_config_cache_dir", "pnpm-cache"),
        ("pnpm_config_store_dir", "pnpm-store"),
        ("pnpm_config_cache_dir", "pnpm-cache"),
        ("YARN_CACHE_FOLDER", "yarn"),
        ("YARN_GLOBAL_FOLDER", "yarn-global"),
        ("BUN_INSTALL_CACHE_DIR", "bun"),
        ("UV_CACHE_DIR", "uv"),
        ("POETRY_CACHE_DIR", "poetry"),
    ]
    .iter()
    .map(|(k, sub)| (k.to_string(), at(sub)))
    .collect()
}

/// The folders an install of `words` writes beyond the worktree: the worker's cache folder.
pub fn install_writes(words: &[&str], cache: &Path) -> Vec<String> {
    if is_install(words) { vec![cache.to_string_lossy().to_string()] } else { Vec::new() }
}

/// The env an install of `words` sets on its job's line: its caches in `cache`, then its
/// installers' own, in order.
pub fn install_env(words: &[&str], cache: &Path) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = if is_install(words) { cache_env(cache) } else { Vec::new() };
    for i in JS.iter().chain(PYTHON).filter(|i| runs(i, words)) {
        for (k, v) in i.env {
            out.retain(|(o, _)| o != k);
            out.push((k.to_string(), v.to_string()));
        }
    }
    out
}

/// Each of `folders`' installs as the words a worker types, read from the human's checkouts'
/// lockfiles whether or not an entry of the human's covers it: what a brief tells a worker.
/// Reads the disk.
pub fn installs_in(hive: &Hive, folders: &[&str]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for f in hive.git_folders().filter(|f| folders.contains(&f.name.as_str())) {
        for i in detect(Path::new(&f.path)) {
            out.extend(i.prefixes.iter().map(|p| (f.name.clone(), p.to_string())));
        }
    }
    out
}

/// Each git folder's installs as queued commands of their own, read from the human's checkout's
/// lockfiles, for a worker that changed a manifest (§6.8): beside the land checks, never over an
/// entry of the human's that already matches. No needs or leases; the job gets its caches and the
/// registries as any install does (`run::build_spec`). Reads the disk.
pub fn install_commands(hive: &Hive, taken: &dyn Fn(&str, &[String]) -> bool) -> Vec<QueuedCommand> {
    let mut out = Vec::new();
    for f in hive.git_folders() {
        for i in detect(Path::new(&f.path)) {
            for p in i.prefixes {
                let argv: Vec<String> = p.split_whitespace().map(str::to_string).collect();
                if taken(&f.name, &argv) {
                    continue;
                }
                out.push(QueuedCommand { folder: f.name.clone(), prefix: p.to_string(), ..QueuedCommand::default() });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn folder(files: &[&str]) -> PathBuf {
        let d = std::env::temp_dir().join(format!("hive-deps-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&d).unwrap();
        for f in files {
            fs::write(d.join(f), "").unwrap();
        }
        d
    }

    /// The Create sheet's setup per lockfile, one per ecosystem, the JS one first.
    #[test]
    fn setup_is_read_from_the_lockfiles() {
        for (files, want) in [
            (&["pnpm-lock.yaml", "package.json"][..], Some("pnpm install --frozen-lockfile")),
            (&["pnpm-lock.yaml", "package-lock.json"], Some("pnpm install --frozen-lockfile")),
            (&["package-lock.json"], Some("npm ci")),
            (&["npm-shrinkwrap.json"], Some("npm ci")),
            (&["yarn.lock"], Some("yarn install --frozen-lockfile")),
            (&["yarn.lock", ".yarnrc.yml"], Some("yarn install --immutable")),
            (&["bun.lockb"], Some("bun install --frozen-lockfile")),
            (&["bun.lock"], Some("bun install --frozen-lockfile")),
            (&["uv.lock"], Some("uv sync --frozen")),
            (&["poetry.lock"], Some("poetry install")),
            (&["pnpm-lock.yaml", "uv.lock"], Some("pnpm install --frozen-lockfile && uv sync --frozen")),
            (&["Cargo.lock", "Cargo.toml"], None),
            (&["package.json"], None),
            (&[], None),
        ] {
            let d = folder(files);
            assert_eq!(setup_for(&d).as_deref(), want, "{files:?}");
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn an_install_is_told_by_its_first_words() {
        for (cmd, install) in [
            ("pnpm install", true),
            ("pnpm install --frozen-lockfile", true),
            ("npm ci", true),
            ("npm install left-pad", true),
            ("uv sync --frozen", true),
            ("pnpm add x", false),
            ("npm run build", false),
            ("npm", false),
            ("cargo test", false),
        ] {
            let words: Vec<&str> = cmd.split_whitespace().collect();
            assert_eq!(is_install(&words), install, "{cmd}");
        }
        let cache = Path::new("/h/wt.noindex/w1/deps-cache");
        assert_eq!(install_writes(&["pnpm", "install"], cache), vec!["/h/wt.noindex/w1/deps-cache".to_string()]);
        assert!(install_writes(&["npm", "run", "x"], cache).is_empty());
        let poetry = install_env(&["poetry", "install"], cache);
        assert_eq!(poetry.last(), Some(&("POETRY_VIRTUALENVS_IN_PROJECT".to_string(), "true".to_string())));
        assert!(poetry.contains(&("POETRY_CACHE_DIR".to_string(), "/h/wt.noindex/w1/deps-cache/poetry".to_string())));
        assert!(install_env(&["npm", "run", "x"], cache).is_empty());
    }

    /// An install writes nothing of the human's: every cache it is pointed at, and the one
    /// folder it may write, are inside the worker's own cache folder.
    #[test]
    fn an_install_writes_no_folder_of_the_human_s() {
        let cache = Path::new("/h/wt.noindex/w1/deps-cache");
        for i in JS.iter().chain(PYTHON) {
            let words: Vec<&str> = i.prefixes[0].split_whitespace().collect();
            for w in install_writes(&words, cache) {
                assert!(Path::new(&w).starts_with(cache), "{}: {w}", i.name);
            }
            for (k, v) in install_env(&words, cache) {
                assert!(!v.starts_with('/') || Path::new(&v).starts_with(cache), "{}: {k}={v}", i.name);
            }
        }
    }

    #[test]
    fn each_git_folder_s_installs_are_queued_unless_the_human_queued_one() {
        let (web, api) = (folder(&["pnpm-lock.yaml"]), folder(&["package-lock.json", "uv.lock"]));
        let mut h = crate::hive::launch::tests::sample_hive();
        h.folders[0].path = web.to_string_lossy().to_string();
        h.folders.push(crate::hive::record::HiveFolder { name: "api".into(), path: api.to_string_lossy().to_string(), kind: crate::hive::record::KIND_GIT.into(), ..Default::default() });
        let none = |_: &str, _: &[String]| false;
        let got: Vec<(String, String)> = install_commands(&h, &none).into_iter().map(|c| (c.folder, c.prefix)).collect();
        assert_eq!(got, vec![("web".into(), "pnpm install".into()), ("api".into(), "npm ci".into()), ("api".into(), "npm install".into()), ("api".into(), "uv sync".into())]);
        let human_npm = |f: &str, argv: &[String]| f == "api" && argv[0] == "npm";
        let got: Vec<String> = install_commands(&h, &human_npm).into_iter().map(|c| c.prefix).collect();
        assert_eq!(got, vec!["pnpm install", "uv sync"]);
        let c = &install_commands(&h, &none)[0];
        assert!(c.needs.is_empty() && c.services.is_empty() && c.env.is_empty());
        let _ = fs::remove_dir_all(&web);
        let _ = fs::remove_dir_all(&api);
    }
}
