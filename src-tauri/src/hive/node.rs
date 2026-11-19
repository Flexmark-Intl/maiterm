//! The Node a Hive folder asks for (docs/hive.md §6.8), and the installed one that answers it.
//!
//! A job's line runs in a shell at the human's default PATH, so `npm ci` ran on whatever `node`
//! came first there (Node 24 on the first real Hive, 2026-11-18) while the repo's `engines` wanted
//! 22: the install was refused, or forced past the check and added a package that broke `npm ci`
//! on 22 for everyone after. The folder says which Node it wants (`.nvmrc`, `.node-version`,
//! `.tool-versions`, else `package.json` `engines.node`); the newest installed Node that fits
//! goes first on the job's PATH. One that nothing installed fits changes nothing, and is logged.

use std::path::{Path, PathBuf};

use semver::{Version, VersionReq};

/// What a folder asks for, and where it said so.
#[derive(Debug, Clone, PartialEq)]
pub struct Want {
    /// Any one of these.
    pub any_of: Vec<VersionReq>,
    /// `.nvmrc`, `package.json engines.node`, …
    pub from: String,
    /// As written there.
    pub said: String,
}

impl Want {
    pub fn fits(&self, v: &Version) -> bool {
        self.any_of.iter().any(|r| r.matches(v))
    }
}

/// The nearest folder from `dir` up to `root` (both included) that says which Node it wants.
pub fn wanted(dir: &Path, root: &Path) -> Option<Want> {
    let mut at = Some(dir);
    while let Some(d) = at {
        if let Some(w) = wanted_in(d) {
            return Some(w);
        }
        if d == root || !d.starts_with(root) {
            break;
        }
        at = d.parent();
    }
    None
}

/// What one folder's own files say: a version file before `engines`, as nvm and its kin read it.
pub fn wanted_in(dir: &Path) -> Option<Want> {
    for name in [".nvmrc", ".node-version"] {
        if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
            let said = text.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with('#')).unwrap_or_default().to_string();
            if let Some(r) = version_file_req(&said) {
                return Some(Want { any_of: vec![r], from: name.to_string(), said });
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(dir.join(".tool-versions")) {
        let line = text.lines().map(str::trim).find(|l| l.split_whitespace().next() == Some("nodejs"));
        if let Some(said) = line.and_then(|l| l.split_whitespace().nth(1)) {
            if let Some(r) = version_file_req(said) {
                return Some(Want { any_of: vec![r], from: ".tool-versions".to_string(), said: said.to_string() });
            }
        }
    }
    let pkg: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(dir.join("package.json")).ok()?).ok()?;
    let said = pkg.get("engines")?.get("node")?.as_str()?.trim().to_string();
    let any_of = npm_range(&said)?;
    Some(Want { any_of, from: "package.json engines.node".to_string(), said })
}

/// A version file's line (`22`, `v22.14.0`, `22.14`): that release line, at least what it names.
/// `lts/*`, `node` and other aliases name no version here, so nothing.
fn version_file_req(said: &str) -> Option<VersionReq> {
    let v = said.trim().trim_start_matches('v');
    let parts: Vec<&str> = v.split('.').collect();
    if parts.is_empty() || parts.len() > 3 || !parts.iter().all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit())) {
        return None;
    }
    let major: u64 = parts[0].parse().ok()?;
    let req = if parts.len() == 1 { format!(">={major}.0.0, <{}", major + 1) } else { format!(">={}, <{}", fill(&parts), major + 1) };
    VersionReq::parse(&req).ok()
}

/// `22.14` → `22.14.0`.
fn fill(parts: &[&str]) -> String {
    let mut p: Vec<&str> = parts.to_vec();
    while p.len() < 3 {
        p.push("0");
    }
    p.join(".")
}

/// An npm range (`>=22.14.0 <23`, `^22 || ^24`, `22.x`, `20 - 22`) as the semver crate reads
/// ranges. A bare version is exact in npm, so it gets `=` (which the crate reads as npm does for
/// a partial one: `=22` is any 22). Anything it can't read is no range at all.
pub fn npm_range(said: &str) -> Option<Vec<VersionReq>> {
    let mut out = Vec::new();
    for alt in said.split("||") {
        let alt = alt.trim();
        if alt.is_empty() || alt == "*" || alt.eq_ignore_ascii_case("x") {
            out.push(VersionReq::STAR);
            continue;
        }
        let words: Vec<&str> = alt.split_whitespace().collect();
        let comparators: Vec<String> = match words.as_slice() {
            [lo, "-", hi] => vec![format!(">={}", partial_low(lo)), upper_of(hi)],
            _ => {
                // `>= 22` is one comparator written as two words.
                let mut joined: Vec<String> = Vec::new();
                let mut pending: Option<&str> = None;
                for w in words {
                    match pending.take() {
                        Some(op) => joined.push(format!("{op}{w}")),
                        None if matches!(w, ">=" | "<=" | ">" | "<" | "=" | "^" | "~") => pending = Some(w),
                        None => joined.push(w.to_string()),
                    }
                }
                if pending.is_some() {
                    return None;
                }
                joined
                    .into_iter()
                    .map(|c| if c.starts_with(|ch: char| ch.is_ascii_digit() || ch == 'v') { format!("={}", c.trim_start_matches('v')) } else { c.replace(">=v", ">=").replace("<v", "<").replace("^v", "^").replace("~v", "~") })
                    .collect()
            }
        };
        out.push(VersionReq::parse(&comparators.join(", ")).ok()?);
    }
    (!out.is_empty()).then_some(out)
}

/// A hyphen range's low end: missing parts are zeros.
fn partial_low(v: &str) -> String {
    let parts: Vec<&str> = v.trim_start_matches('v').split('.').collect();
    fill(&parts)
}

/// A hyphen range's high end: a full version is included; a partial one takes its whole line.
fn upper_of(v: &str) -> String {
    let v = v.trim_start_matches('v');
    let parts: Vec<&str> = v.split('.').collect();
    let n = |i: usize| parts.get(i).and_then(|p| p.parse::<u64>().ok()).unwrap_or(0);
    match parts.len() {
        3 => format!("<={v}"),
        2 => format!("<{}.{}.0", n(0), n(1) + 1),
        _ => format!("<{}.0.0", n(0) + 1),
    }
}

/// One installed Node: its version and the folder its `node` is in.
#[derive(Debug, Clone, PartialEq)]
pub struct Installed {
    pub version: Version,
    pub bin: PathBuf,
}

/// Where version managers and Homebrew keep Nodes: each a folder of version-named folders, and
/// the path from one of those to its `bin`.
fn stores(home: Option<&Path>) -> Vec<(PathBuf, &'static str)> {
    let mut out: Vec<(PathBuf, &'static str)> = Vec::new();
    for cellar in ["/opt/homebrew/Cellar", "/usr/local/Cellar", "/home/linuxbrew/.linuxbrew/Cellar"] {
        let Ok(rd) = std::fs::read_dir(cellar) else { continue };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name == "node" || name.starts_with("node@") {
                out.push((e.path(), "bin"));
            }
        }
    }
    if let Some(nvm) = std::env::var_os("NVM_DIR") {
        out.push((PathBuf::from(nvm).join("versions/node"), "bin"));
    }
    if let Some(h) = home {
        out.push((h.join(".nvm/versions/node"), "bin"));
        out.push((h.join(".local/share/fnm/node-versions"), "installation/bin"));
        out.push((h.join("Library/Application Support/fnm/node-versions"), "installation/bin"));
        out.push((h.join(".volta/tools/image/node"), "bin"));
        out.push((h.join(".asdf/installs/nodejs"), "bin"));
        out.push((h.join(".local/share/mise/installs/node"), "bin"));
        out.push((h.join(".nodenv/versions"), "bin"));
    }
    out
}

/// A version folder's name as a version: `v22.14.0`, Homebrew's `22.23.5_1` (a rebuild).
fn version_of(name: &str) -> Option<Version> {
    let v = name.trim_start_matches('v');
    let v = v.split_once('_').map_or(v, |(a, _)| a);
    Version::parse(v).ok()
}

/// Every Node the stores hold that has a `node` to run.
pub fn installed(home: Option<&Path>) -> Vec<Installed> {
    let mut out: Vec<Installed> = Vec::new();
    for (store, rel) in stores(home) {
        let Ok(rd) = std::fs::read_dir(&store) else { continue };
        for e in rd.flatten() {
            let Some(version) = version_of(&e.file_name().to_string_lossy()) else { continue };
            let bin = e.path().join(rel);
            if bin.join("node").is_file() && !out.iter().any(|i| i.bin == bin) {
                out.push(Installed { version, bin });
            }
        }
    }
    out
}

/// The newest of `installed` that `want` fits (a pre-release only if nothing else does).
pub fn pick<'a>(want: &Want, installed: &'a [Installed]) -> Option<&'a Installed> {
    let fit = installed.iter().filter(|i| want.fits(&i.version));
    fit.max_by(|a, b| (a.version.pre.is_empty(), &a.version).cmp(&(b.version.pre.is_empty(), &b.version)))
}

/// The `bin` folder to put first on a job's PATH in `dir` (inside `root`), or None: the folder
/// asks for no Node, or none installed fits (logged). Reads the disk.
pub fn bin_for(dir: &Path, root: &Path) -> Option<PathBuf> {
    let want = wanted(dir, root)?;
    let all = installed(dirs::home_dir().as_deref());
    match pick(&want, &all) {
        Some(i) => Some(i.bin.clone()),
        None => {
            log::warn!(
                "hive: {} wants Node {} ({}), and no installed Node fits; its job runs on the default node",
                dir.display(),
                want.said,
                want.from
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    fn fits(range: &str, version: &str) -> bool {
        let any_of = npm_range(range).unwrap_or_else(|| panic!("{range} unread"));
        Want { any_of, from: String::new(), said: range.into() }.fits(&v(version))
    }

    #[test]
    fn npm_ranges_read_as_npm_reads_them() {
        assert!(fits(">=22.14.0 <23", "22.23.5"));
        assert!(!fits(">=22.14.0 <23", "24.3.0"));
        assert!(!fits(">=22.14.0 <23", "22.13.9"));
        assert!(fits(">= 22", "24.3.0"), "an operator written apart from its version");
        assert!(fits("^22 || ^24", "24.1.0") && !fits("^22 || ^24", "23.0.0"));
        assert!(fits("22.x", "22.9.0") && !fits("22.x", "23.0.0"));
        assert!(fits("22", "22.1.0") && !fits("22", "21.9.0"), "a bare partial is its whole line");
        assert!(fits("22.14.0", "22.14.0") && !fits("22.14.0", "22.14.1"), "a bare full version is exact");
        assert!(fits("20 - 22", "22.99.0") && fits("20 - 22", "20.0.0") && !fits("20 - 22", "23.0.0"));
        assert!(fits("20.1.0 - 22.3.0", "22.3.0") && !fits("20.1.0 - 22.3.0", "22.3.1"));
        assert!(fits("*", "18.0.0"));
        assert!(npm_range(">=").is_none() && npm_range("lts/*").is_none());
    }

    #[test]
    fn a_version_file_names_a_release_line() {
        let r = |s: &str| version_file_req(s).unwrap();
        assert!(r("22").matches(&v("22.23.5")) && !r("22").matches(&v("24.3.0")));
        assert!(r("v22.14.0").matches(&v("22.23.5")) && !r("v22.14.0").matches(&v("22.13.0")));
        assert!(r("22.14").matches(&v("22.14.0")));
        assert!(version_file_req("lts/*").is_none() && version_file_req("node").is_none() && version_file_req("").is_none());
    }

    #[test]
    fn the_folder_s_own_files_say_which_node() {
        let base = std::env::temp_dir().join(format!("maiterm-node-{}", uuid::Uuid::new_v4().simple()));
        let sub = base.join("packages/web");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::write(base.join("package.json"), r#"{"engines":{"node":">=22.14.0 <23"}}"#).unwrap();
        let w = wanted(&sub, &base).expect("found above the job's folder");
        assert_eq!((w.from.as_str(), w.said.as_str()), ("package.json engines.node", ">=22.14.0 <23"));
        std::fs::write(base.join(".nvmrc"), "# pinned\nv20\n").unwrap();
        assert_eq!(wanted(&sub, &base).unwrap().from, ".nvmrc", "a version file before engines");
        std::fs::write(sub.join(".tool-versions"), "python 3.12\nnodejs 24.1.0\n").unwrap();
        assert_eq!(wanted(&sub, &base).unwrap().said, "24.1.0", "the nearest folder wins");
        assert!(wanted(&base.join("packages"), &base.join("packages")).is_none(), "never above the root");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_newest_installed_node_that_fits_is_picked() {
        let base = std::env::temp_dir().join(format!("maiterm-nodes-{}", uuid::Uuid::new_v4().simple()));
        for (dir, ver) in [(".nvm/versions/node", "v22.14.0"), (".nvm/versions/node", "v24.3.0"), (".volta/tools/image/node", "22.23.5"), (".nvm/versions/node", "v22.99.0")] {
            let bin = base.join(dir).join(ver).join("bin");
            std::fs::create_dir_all(&bin).unwrap();
            if ver != "v22.99.0" {
                std::fs::write(bin.join("node"), "").unwrap();
            }
        }
        let all = installed(Some(&base));
        let want = Want { any_of: npm_range(">=22.14.0 <23").unwrap(), from: String::new(), said: String::new() };
        let got = pick(&want, &all).expect("one fits");
        assert_eq!(got.version, v("22.23.5"), "the newest that fits; a folder with no node isn't a Node");
        assert!(got.bin.ends_with(".volta/tools/image/node/22.23.5/bin"));
        let none = Want { any_of: npm_range("^18").unwrap(), from: String::new(), said: String::new() };
        assert!(pick(&none, &all).is_none());
        assert_eq!(version_of("22.23.5_1"), Some(v("22.23.5")), "a Homebrew rebuild");
        let _ = std::fs::remove_dir_all(&base);
    }
}
