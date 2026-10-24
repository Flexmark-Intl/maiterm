//! A queued job's sandbox (docs/hive.md §6.7, §6.8): every command maiTerm runs on a worker's
//! branch runs that worker's code, so it runs under the worker's own write lists
//! (`settings::worker_write_lists`) as a macOS Seatbelt profile, with the network closed but for
//! loopback to the ports the job was given or leases. A job is never a way to do what its worker
//! may not.
//!
//! The line typed into the job's tab is built here, every word `sh_quote`d, and is never a
//! `sh -c` of anything a worker wrote: `cd <worktree> && env -u … K=V … /usr/bin/sandbox-exec -f
//! <profile> <argv…> && /usr/bin/touch <pass mark>; /usr/bin/touch <exit mark>`. The marks are
//! the exit's witnesses: the job's own output reaches the same PTY as the shell's OSC 133, so a
//! job could print any D of its own. The shell writes them, outside the sandbox, into a folder on
//! the worker's and the job's `denyWrite`: the exit mark once the line is over whatever its
//! status (no `$?`, so any shell types it), the pass mark before it only if `sandbox-exec`
//! exited 0. And the job can't type into that shell: TIOCSTI is denied, or a job would push a
//! `touch` of its own marks (or anything else) into the unsandboxed shell's input.
//!
//! Any other OS refuses the job: there is no profile to run it under, and an unsandboxed job
//! would be the escape §6.7 rules out.

use std::path::{Path, PathBuf};

use super::launch::sh_quote;
use super::settings::DenyPath;

pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";
pub const TOUCH: &str = "/usr/bin/touch";
pub const PROFILE_FILE: &str = "profile.sb";
pub const PASS_MARK: &str = "passed";
pub const EXIT_MARK: &str = "exited";
/// maiTerm's own address and credentials: a job is not an agent and never reaches maiTerm.
pub const SCRUBBED: &[&str] = &["MAITERM_AUTH", "MAITERM_PORT", "MAITERM_TAB_ID"];

/// Refuses on every OS but macOS: the job would run with nothing around it.
pub fn supported() -> Result<(), String> {
    if cfg!(target_os = "macos") {
        Ok(())
    } else {
        Err("Queued jobs run only on macOS in this version: there is no sandbox to run them under here.".to_string())
    }
}

/// A path as a Seatbelt string literal. A `"`, a `\` or a control character refuses the job:
/// nothing is escaped in the hope it holds.
pub fn sb_path(p: &Path) -> Result<String, String> {
    let s = p.to_str().ok_or_else(|| format!("{} isn't valid UTF-8, so the job's sandbox can't name it", p.display()))?;
    if !p.is_absolute() {
        return Err(format!("{s} isn't an absolute path, so the job's sandbox can't name it"));
    }
    if s.chars().any(|c| c == '"' || c == '\\' || c.is_control()) {
        return Err(format!("{s:?} holds a quote, a backslash or a control character, so the job's sandbox can't name it"));
    }
    Ok(format!("\"{s}\""))
}

/// `p` with every symlink in its existing part resolved: Seatbelt matches the real path, so a
/// `/var/folders/…` rule would never match the `/private/var/folders/…` the kernel sees. The part
/// that doesn't exist yet is kept as written. Reads the disk.
pub fn real_path(p: &Path) -> PathBuf {
    let mut rest: Vec<&std::ffi::OsStr> = Vec::new();
    let mut at = p;
    loop {
        if let Ok(real) = std::fs::canonicalize(at) {
            return rest.iter().rev().fold(real, |acc, c| acc.join(c));
        }
        match (at.parent(), at.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name);
                at = parent;
            }
            _ => return p.to_path_buf(),
        }
    }
}

/// `p` as a job's profile names it. Seatbelt matches real paths, but a link inside a folder the
/// job (or its worker) can write is theirs to plant: resolved ahead of time, it would turn the
/// grant on `<worker tmp>/jobs` into one on wherever the link points. So symlinks resolve only
/// above the first component inside a `writable` entry (maiTerm's folders and the system's,
/// `/var` → `/private/var`); from there down every component is kept as written and must not be
/// a link. One planted later is never followed: the kernel resolves it and the rule doesn't
/// match. Reads the disk.
pub fn profile_path(p: &Path, writable: &[PathBuf]) -> Result<PathBuf, String> {
    use std::path::Component;
    let comps: Vec<Component> = p.components().collect();
    let mut trusted = PathBuf::new();
    let mut at = 0;
    for c in &comps {
        let next = trusted.join(c);
        if writable.iter().any(|w| next.starts_with(w)) {
            break;
        }
        trusted = next;
        at += 1;
    }
    let mut out = if trusted.as_os_str().is_empty() { trusted } else { real_path(&trusted) };
    for c in &comps[at..] {
        if !matches!(c, Component::Normal(_)) {
            return Err(format!("{} isn't a plain path, so the job's sandbox can't name it", p.display()));
        }
        out.push(c);
        if std::fs::symlink_metadata(&out).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(format!("{} is a symbolic link inside a folder the job can write, so its sandbox won't follow it", out.display()));
        }
    }
    Ok(out)
}

/// What one job's profile holds. Paths are as `profile_path` names them.
#[derive(Debug, Clone, Default)]
pub struct ProfileSpec {
    /// The worker's allow list, its job temp and the queued command's `extra_write`.
    pub allow: Vec<PathBuf>,
    /// The worker's deny list, which wins.
    pub deny: Vec<DenyPath>,
    /// The ports booked for this job: it may listen on these.
    pub own_ports: Vec<u16>,
    /// Its leased copies' ports and its `{port:<service>}` references: it may connect to these
    /// (and to its own).
    pub reach_ports: Vec<u16>,
}

/// The Seatbelt profile, in the order §6.8 gives: everything allowed, then every write denied,
/// then the allow list, then the deny list over it, then the network closed but for loopback to
/// the job's own and leased ports. Seatbelt takes the last rule that matches. The job shares its
/// tab's terminal with the unsandboxed shell, so faking input there (TIOCSTI) is denied too.
pub fn profile(spec: &ProfileSpec) -> Result<String, String> {
    let mut out = String::from("(version 1)\n(allow default)\n(deny file-ioctl (ioctl-command TIOCSTI))\n(deny file-write*)\n(allow file-write*\n");
    out.push_str("    (literal \"/dev/null\")\n    (regex #\"^/dev/tty\")");
    let mut seen: Vec<&Path> = Vec::new();
    for p in &spec.allow {
        if seen.contains(&p.as_path()) {
            continue;
        }
        seen.push(p);
        out.push_str(&format!("\n    (subpath {})", sb_path(p)?));
    }
    out.push_str(")\n");
    if !spec.deny.is_empty() {
        out.push_str("(deny file-write*");
        for d in &spec.deny {
            let form = if d.dir { "subpath" } else { "literal" };
            out.push_str(&format!("\n    ({form} {})", sb_path(&d.path)?));
        }
        out.push_str(")\n");
    }
    out.push_str("(deny network*)\n");
    let mut own = spec.own_ports.clone();
    own.sort_unstable();
    own.dedup();
    let mut reach: Vec<u16> = own.iter().chain(&spec.reach_ports).copied().collect();
    reach.sort_unstable();
    reach.dedup();
    for p in &own {
        out.push_str(&format!("(allow network-bind network-inbound (local ip \"localhost:{p}\"))\n"));
    }
    for p in &reach {
        out.push_str(&format!("(allow network-outbound (remote ip \"localhost:{p}\"))\n"));
    }
    Ok(out)
}

/// What one job's line holds.
#[derive(Debug, Clone, Default)]
pub struct LineSpec {
    pub cwd: PathBuf,
    /// Names removed from the environment besides `SCRUBBED` (`launch::unset_vars`).
    pub unset: Vec<String>,
    /// Set for the job, in order: `TMPDIR`, the worker's `CARGO_TARGET_DIR`, `CARGO_NET_OFFLINE`,
    /// then the queued command's, already expanded.
    pub env: Vec<(String, String)>,
    pub profile: PathBuf,
    pub argv: Vec<String>,
    pub pass_mark: PathBuf,
    pub exit_mark: PathBuf,
}

fn plain_word(what: &str, s: &str) -> Result<(), String> {
    // A newline typed into a PTY ends the line whatever quotes are around it.
    if s.chars().any(char::is_control) {
        return Err(format!("{what} {s:?} holds a control character, so it can't be typed"));
    }
    Ok(())
}

fn abs(what: &str, p: &Path) -> Result<String, String> {
    let s = p.to_str().ok_or_else(|| format!("{what} {} isn't valid UTF-8", p.display()))?;
    if !p.is_absolute() {
        return Err(format!("{what} {s} isn't an absolute path"));
    }
    plain_word(what, s)?;
    Ok(sh_quote(s))
}

/// The line typed into a job's tab. Never through `sh -c`: each argv word is its own quoted
/// word, run by `sandbox-exec` directly.
pub fn job_line(l: &LineSpec) -> Result<String, String> {
    supported()?;
    line_text(l)
}

fn line_text(l: &LineSpec) -> Result<String, String> {
    if l.argv.is_empty() || l.argv[0].is_empty() {
        return Err("A job needs a command".to_string());
    }
    let mut line = format!("cd {} && env", abs("The job's folder", &l.cwd)?);
    let mut removed: Vec<&str> = Vec::new();
    for u in SCRUBBED.iter().copied().chain(l.unset.iter().map(String::as_str)) {
        if !crate::stack::placeholders::valid_env_key(u) {
            return Err(format!("{u:?} isn't a variable name"));
        }
        if removed.contains(&u) || l.env.iter().any(|(k, _)| k == u) {
            continue;
        }
        removed.push(u);
        line.push_str(" -u ");
        line.push_str(u);
    }
    for (k, v) in &l.env {
        if !crate::stack::placeholders::valid_env_key(k) || SCRUBBED.contains(&k.as_str()) {
            return Err(format!("{k:?} can't be set for a job"));
        }
        plain_word(&format!("{k}'s value"), v)?;
        line.push_str(&format!(" {k}={}", sh_quote(v)));
    }
    line.push_str(&format!(" {SANDBOX_EXEC} -f {}", abs("The job's profile", &l.profile)?));
    for a in &l.argv {
        plain_word("The word", a)?;
        line.push(' ');
        line.push_str(&sh_quote(a));
    }
    // `a && b && c; d`: the exit mark follows the whole list, whatever it returned.
    line.push_str(&format!(" && {TOUCH} {}", abs("The job's pass mark", &l.pass_mark)?));
    line.push_str(&format!("; {TOUCH} {}", abs("The job's exit mark", &l.exit_mark)?));
    Ok(line)
}

/// A human-defined service run on a worker's branch: its own command, the human's text, run by
/// `/bin/sh` inside the same profile (§6.8).
pub fn service_argv(command: &str) -> Vec<String> {
    vec!["/bin/sh".to_string(), "-c".to_string(), command.to_string()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> ProfileSpec {
        ProfileSpec {
            allow: vec![
                PathBuf::from("/Users/x/Library/Application Support/com.aiterm.dev/hive/h1/wt.noindex/auth/web"),
                PathBuf::from("/r/web/.git/worktrees/web"),
                PathBuf::from("/r/web/.git/objects"),
                PathBuf::from("/r/web/.git/objects"),
            ],
            deny: vec![
                DenyPath { path: PathBuf::from("/r/web/.git/config"), dir: false },
                DenyPath { path: PathBuf::from("/r/web/.git/hooks"), dir: true },
            ],
            own_ports: vec![5175, 5174, 5174],
            reach_ports: vec![5432, 5174],
        }
    }

    // The golden and refusal tests use Unix paths, which aren't absolute on Windows (CI).
    #[cfg(unix)]
    #[test]
    fn the_profile_is_the_worker_s_lists_in_the_spec_s_order() {
        let got = profile(&spec()).unwrap();
        let want = r#"(version 1)
(allow default)
(deny file-ioctl (ioctl-command TIOCSTI))
(deny file-write*)
(allow file-write*
    (literal "/dev/null")
    (regex #"^/dev/tty")
    (subpath "/Users/x/Library/Application Support/com.aiterm.dev/hive/h1/wt.noindex/auth/web")
    (subpath "/r/web/.git/worktrees/web")
    (subpath "/r/web/.git/objects"))
(deny file-write*
    (literal "/r/web/.git/config")
    (subpath "/r/web/.git/hooks"))
(deny network*)
(allow network-bind network-inbound (local ip "localhost:5174"))
(allow network-bind network-inbound (local ip "localhost:5175"))
(allow network-outbound (remote ip "localhost:5174"))
(allow network-outbound (remote ip "localhost:5175"))
(allow network-outbound (remote ip "localhost:5432"))
"#;
        assert_eq!(got, want);
    }

    #[cfg(unix)]
    #[test]
    fn no_port_no_network() {
        let got = profile(&ProfileSpec { allow: vec![PathBuf::from("/w")], ..Default::default() }).unwrap();
        assert!(got.ends_with("(deny network*)\n"), "{got}");
        assert!(!got.contains("network-bind") && !got.contains("network-outbound"));
        assert!(!got.contains("(deny file-write*\n"), "no deny block when nothing is denied");
    }

    #[cfg(unix)]
    #[test]
    fn a_path_seatbelt_would_misread_refuses_the_job() {
        for bad in ["/w/a\"b", "/w/a\\b", "/w/a\nb", "/w/a\u{7}b", "relative/path"] {
            let s = ProfileSpec { allow: vec![PathBuf::from(bad)], ..Default::default() };
            assert!(profile(&s).is_err(), "{bad:?}");
            let s = ProfileSpec { deny: vec![DenyPath { path: PathBuf::from(bad), dir: true }], ..Default::default() };
            assert!(profile(&s).is_err(), "{bad:?} in deny");
        }
        // Spaces and an apostrophe are ordinary path characters inside a Seatbelt string.
        assert!(sb_path(Path::new("/Users/x/Library/Application Support/it's")).is_ok());
    }

    fn line() -> LineSpec {
        LineSpec {
            cwd: PathBuf::from("/Users/x/Library/Application Support/com.aiterm.dev/hive/h1/wt.noindex/auth/web"),
            unset: vec!["CLAUDECODE".into(), "MAITERM_PORT".into()],
            env: vec![
                ("TMPDIR".into(), "/h/wt.noindex/auth/claude-tmp/jobs".into()),
                ("CARGO_TARGET_DIR".into(), "/h/wt.noindex/auth/cargo-target".into()),
                ("CARGO_NET_OFFLINE".into(), "true".into()),
                ("DATABASE_URL".into(), "postgres://localhost/app_auth".into()),
            ],
            profile: PathBuf::from("/h/runs/j-0000abcd/profile.sb"),
            argv: vec!["cargo".into(), "test".into(), "--".into(), "it's".into()],
            pass_mark: PathBuf::from("/h/runs/j-0000abcd/passed"),
            exit_mark: PathBuf::from("/h/runs/j-0000abcd/exited"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_line_quotes_every_word_and_never_runs_a_shell() {
        let got = line_text(&line()).unwrap();
        assert_eq!(
            got,
            "cd '/Users/x/Library/Application Support/com.aiterm.dev/hive/h1/wt.noindex/auth/web' && env \
             -u MAITERM_AUTH -u MAITERM_PORT -u MAITERM_TAB_ID -u CLAUDECODE \
             TMPDIR='/h/wt.noindex/auth/claude-tmp/jobs' CARGO_TARGET_DIR='/h/wt.noindex/auth/cargo-target' \
             CARGO_NET_OFFLINE='true' DATABASE_URL='postgres://localhost/app_auth' \
             /usr/bin/sandbox-exec -f '/h/runs/j-0000abcd/profile.sb' 'cargo' 'test' '--' 'it'\\''s' \
             && /usr/bin/touch '/h/runs/j-0000abcd/passed'; /usr/bin/touch '/h/runs/j-0000abcd/exited'"
        );
        assert!(!got.contains("sh -c"));
    }

    #[cfg(unix)]
    #[test]
    fn the_line_refuses_what_it_cant_type_safely() {
        let mut l = line();
        l.argv = vec!["cargo".into(), "test\nrm -rf ~".into()];
        assert!(line_text(&l).is_err(), "a newline ends the line whatever the quotes");
        let mut l = line();
        l.env.push(("BAD-NAME".into(), "x".into()));
        assert!(line_text(&l).is_err());
        let mut l = line();
        l.env.push(("MAITERM_AUTH".into(), "stolen".into()));
        assert!(line_text(&l).is_err(), "maiTerm's credentials are never set for a job");
        let mut l = line();
        l.env.push(("X".into(), "a\rb".into()));
        assert!(line_text(&l).is_err());
        let mut l = line();
        l.argv.clear();
        assert!(line_text(&l).is_err());
        let mut l = line();
        l.unset.push("A;B".into());
        assert!(line_text(&l).is_err());
        let mut l = line();
        l.cwd = PathBuf::from("relative");
        assert!(line_text(&l).is_err());
        // Metacharacters in a word are data once quoted: `$(…)` and backticks reach argv verbatim.
        let mut l = line();
        l.argv = vec!["echo".into(), "$(touch /tmp/p)".into(), "`id`".into()];
        assert!(line_text(&l).unwrap().contains("'echo' '$(touch /tmp/p)' '`id`'"));
    }

    #[test]
    fn only_macos_runs_a_job() {
        assert_eq!(job_line(&line()).is_ok(), cfg!(target_os = "macos"));
        assert_eq!(service_argv("npm run dev -- --port 5174"), vec!["/bin/sh", "-c", "npm run dev -- --port 5174"]);
    }

    #[cfg(unix)]
    #[test]
    fn real_paths_resolve_the_part_that_exists() {
        let base = std::env::temp_dir().join(format!("job-real-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(base.join("a")).unwrap();
        std::os::unix::fs::symlink(base.join("a"), base.join("link")).unwrap();
        let real_base = std::fs::canonicalize(&base).unwrap();
        assert_eq!(real_path(&base.join("link/not/yet")), real_base.join("a/not/yet"));
        assert_eq!(real_path(Path::new("/definitely-not-here-xyz/q")), PathBuf::from("/definitely-not-here-xyz/q"));
        let _ = std::fs::remove_dir_all(&base);
    }

    /// A worker that swapped its job temp (or anything under a folder it writes) for a link to
    /// somewhere else must not have the profile grant wherever the link points.
    #[cfg(unix)]
    #[test]
    fn a_link_the_worker_planted_is_never_resolved_into_a_grant() {
        let base = real_path(&std::env::temp_dir().join(format!("job-plant-{}", uuid::Uuid::new_v4())));
        let (tmp, home) = (base.join("hive/tmp/auth"), base.join("home"));
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        // maiTerm's own folders above the worker's resolve: `/var/folders` is a link.
        std::os::unix::fs::symlink(base.join("hive"), base.join("hive-link")).unwrap();
        let writable = vec![base.join("hive-link/tmp/auth"), base.join("hive-link/tmp/auth/jobs")];
        assert_eq!(profile_path(&writable[1], &writable).unwrap(), tmp.join("jobs"), "nothing planted yet: named, not created");
        std::os::unix::fs::symlink(&home, tmp.join("jobs")).unwrap();
        let err = profile_path(&writable[1], &writable).unwrap_err();
        assert!(err.contains("symbolic link"), "{err}");
        // The worker's own folder swapped whole is refused the same way.
        std::fs::remove_file(tmp.join("jobs")).unwrap();
        std::fs::remove_dir(&tmp).unwrap();
        std::os::unix::fs::symlink(&home, &tmp).unwrap();
        assert!(profile_path(&writable[1], &writable).is_err());
        assert!(profile_path(&writable[0], &writable).is_err());
        let _ = std::fs::remove_dir_all(&base);
    }
}
