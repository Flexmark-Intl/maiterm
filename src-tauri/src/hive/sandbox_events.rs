//! What the sandbox refused, read off a failed tool call (docs/hive.md §6.7, §14.16): the
//! `<sandbox_violations>` note Claude appends to a sandboxed command's error. A worker's
//! `PostToolUseFailure` command hook forwards the event, and each refusal becomes a blocker naming
//! the host or path (wired in step 4e). PostToolUse doesn't fire for the failure, and http hooks
//! don't arrive from a sandboxed tab (spike 10).

use std::path::{Path, PathBuf};

use serde_json::Value;

pub const KIND_HOST: &str = "host";
pub const KIND_PATH: &str = "path";

const OPEN: &str = "<sandbox_violations>";
const CLOSE: &str = "</sandbox_violations>";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// `"host"` | `"path"`.
    pub kind: String,
    /// The host without its port, or the path as the sandbox named it.
    pub target: String,
}

impl Violation {
    /// What `worker.json`'s `violations_seen` holds, so a retry files no second blocker.
    pub fn key(&self) -> String {
        format!("{}:{}", self.kind, self.target)
    }
}

/// What a worker's sandbox lets it write: its cwd and `allowWrite`, less `denyWrite`. A bare
/// `Operation not permitted` is read as the sandbox's only for a path this refuses.
#[derive(Debug, Clone, Default)]
pub struct Writable {
    pub allow: Vec<PathBuf>,
    pub deny: Vec<PathBuf>,
}

impl Writable {
    /// From the worker's `--settings` file as written, and its cwd.
    pub fn from_settings(settings: &Value, cwd: &Path) -> Self {
        let list = |k: &str| -> Vec<PathBuf> {
            settings["sandbox"]["filesystem"][k].as_array().into_iter().flatten().filter_map(Value::as_str).map(PathBuf::from).collect()
        };
        let mut allow = list("allowWrite");
        allow.push(cwd.to_path_buf());
        Self { allow, deny: list("denyWrite") }
    }

    fn refuses(&self, p: &str) -> bool {
        // `/var/…` and `/private/var/…` are one folder on macOS; a program prints either.
        let norm = |p: &Path| -> PathBuf {
            match p.strip_prefix("/private") {
                Ok(rest) if ["var", "tmp", "etc"].iter().any(|d| rest.starts_with(d)) => Path::new("/").join(rest),
                _ => p.to_path_buf(),
            }
        };
        let p = norm(Path::new(p));
        let under = |roots: &[PathBuf]| roots.iter().any(|r| p.starts_with(norm(r)));
        under(&self.deny) || !under(&self.allow)
    }
}

/// Every refusal in the event's failure text, once each, in order. The note's lines read
/// `deny <operation> <target> (<reason>)`: `network-outbound example.com:443` for a host (spike
/// 10's fixtures), a `file-*` operation and a path for a write. A refused WRITE carried no note
/// at all in the 4b probe (macOS Seatbelt, Claude 2.1.310), only the program's own `Operation
/// not permitted`. Most EPERMs aren't the sandbox's (a privacy-protected read, a chmod of another
/// user's file), so a failed Bash call's line counts only in a write's shape and for a path
/// `writable` refuses.
pub fn parse_violations(event: &Value, writable: &Writable) -> Vec<Violation> {
    let mut out: Vec<Violation> = Vec::new();
    let mut add = |v: Violation| {
        if !out.contains(&v) {
            out.push(v);
        }
    };
    let bash = event.get("tool_name").and_then(Value::as_str).is_none_or(|t| matches!(t, "Bash" | "PowerShell" | "Monitor"));
    for text in failure_texts(event) {
        let mut rest = text.as_str();
        while let Some(start) = rest.find(OPEN) {
            let body = &rest[start + OPEN.len()..];
            let (block, after) = body.split_once(CLOSE).unwrap_or((body, ""));
            block.lines().filter_map(parse_line).for_each(&mut add);
            rest = after;
        }
        if bash {
            text.lines().filter_map(refused_write_path).filter(|p| writable.refuses(p)).map(|target| Violation { kind: KIND_PATH.to_string(), target }).for_each(&mut add);
        }
    }
    out
}

const NOT_PERMITTED: &str = "Operation not permitted";

/// Programs whose `prog: <path>: Operation not permitted` is a write: a shell's is a redirect.
/// Not `ls`, `cat`, `cp` or `chmod`, whose EPERM is as often a read or another user's file.
const WRITING_PROGS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "touch", "mkdir", "tee", "ln", "install"];

/// The path on a line where a write met EPERM, in the shapes the probe saw:
/// `/bin/bash: <path>: Operation not permitted` (a redirect), git's `Unable to create '<path>':
/// Operation not permitted`, Python's `PermissionError: [Errno 1] Operation not permitted:
/// '<path>'`. Absolute paths only.
fn refused_write_path(line: &str) -> Option<String> {
    let at = line.find(NOT_PERMITTED)?;
    let quoted = |s: &str| -> Option<String> {
        let start = s.find("'/")? + 1;
        let end = s[start..].find('\'')?;
        Some(s[start..start + end].to_string())
    };
    let path = if line.contains("PermissionError") {
        quoted(&line[at..])?
    } else if line.contains("Unable to create '") || line.contains("' for writing") {
        quoted(&line[..at])?
    } else {
        // `prog: <path>: Operation not permitted` (bash adds `line 1: `): from the first `: /`.
        let before = line[..at].trim_end().strip_suffix(':')?;
        let prog = before.split(':').next()?.trim();
        let prog = prog.rsplit('/').next().unwrap_or(prog);
        if !WRITING_PROGS.contains(&prog) {
            return None;
        }
        let start = before.find(": /")? + 2;
        before[start..].to_string()
    };
    (path.starts_with('/') && path.len() > 1).then_some(path)
}

/// The failed call's error, and its output when the event carries one.
fn failure_texts(event: &Value) -> Vec<String> {
    let mut texts: Vec<String> = Vec::new();
    let mut push = |v: &Value| match v {
        Value::String(s) => texts.push(s.clone()),
        Value::Object(o) => texts.extend(["stdout", "stderr", "error", "output"].iter().filter_map(|k| o.get(*k).and_then(Value::as_str)).map(String::from)),
        _ => {}
    };
    for k in ["error", "tool_response", "tool_output"] {
        if let Some(v) = event.get(k) {
            push(v);
        }
    }
    texts
}

fn parse_line(line: &str) -> Option<Violation> {
    let rest = line.trim().strip_prefix("deny")?;
    // Seatbelt can number a repeated denial: `deny(1) …`.
    let rest = rest.strip_prefix('(').map_or(rest, |r| r.split_once(')').map_or(r, |(_, after)| after));
    let (op, target) = rest.trim_start().split_once(char::is_whitespace)?;
    // The reason is a trailing parenthesis; a path may hold " (" itself, so cut at the last one.
    let target = target.trim();
    let target = match target.rfind(" (") {
        Some(i) if target.ends_with(')') => &target[..i],
        _ => target,
    }
    .trim();
    if target.is_empty() {
        return None;
    }
    if op.starts_with("network") {
        let host = match target.strip_prefix('[') {
            Some(v6) => v6.split(']').next()?,
            None => target.rsplit_once(':').filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit())).map_or(target, |(h, _)| h),
        };
        (!host.is_empty()).then(|| Violation { kind: KIND_HOST.to_string(), target: host.to_ascii_lowercase() })
    } else if op.starts_with("file") {
        Some(Violation { kind: KIND_PATH.to_string(), target: target.to_string() })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The 4b probe's worker, as its settings drew it: the web worktree is its cwd, api's hooks
    /// and its own `.claude` are denied, and the whole web checkout was (the Edit deny Claude
    /// folded into the sandbox, which refused the commit).
    fn probe_writable() -> Writable {
        Writable {
            allow: ["/T/p/wt.noindex/w/web", "/T/p/wt.noindex/w/scratch", "/T/p/repo/web/.git/worktrees/web1"].map(PathBuf::from).to_vec(),
            deny: ["/T/p/repo/api/.git/hooks", "/T/p/wt.noindex/w/web/.claude", "/T/p/repo/web"].map(PathBuf::from).to_vec(),
        }
    }

    fn parse(event: &Value) -> Vec<Violation> {
        parse_violations(event, &probe_writable())
    }

    /// Spike 10's PostToolUseFailure, verbatim but for the paths.
    const SPIKE_10: &str = r#"{"session_id":"d301393a","cwd":"/w","permission_mode":"auto","hook_event_name":"PostToolUseFailure","tool_name":"Bash","tool_input":{"command":"curl -sS -m 8 -o /dev/null -w '%{http_code}\\n' https://example.com/"},"tool_use_id":"toolu_01","error":"Exit code 56\ncurl: (56) CONNECT tunnel failed, response 403\n000\n\n<sandbox_violations>\ndeny network-outbound example.com:443 (host is not on the allow list)\n</sandbox_violations>","is_interrupt":false,"duration_ms":395}"#;

    #[test]
    fn spike_10s_blocked_host_is_named() {
        let v = parse(&serde_json::from_str(SPIKE_10).unwrap());
        assert_eq!(v, vec![Violation { kind: KIND_HOST.into(), target: "example.com".into() }]);
        assert_eq!(v[0].key(), "host:example.com");
    }

    #[test]
    fn repeats_are_named_once_and_every_block_is_read() {
        // Spike 10's runs saw the same line twice in one note, and a deny-list wording.
        let e = json!({"error": "x\n<sandbox_violations>\ndeny network-outbound example.com:443 (host is not on the allow list)\n\
            deny network-outbound example.com:443 (host is not on the allow list)\n</sandbox_violations>\nmore\n\
            <sandbox_violations>\ndeny network-outbound API.fal.ai:443 (host is on the deny list)\n\
            deny network-outbound 127.0.0.1:47120 (host is not on the allow list)\n</sandbox_violations>"});
        let targets: Vec<String> = parse(&e).into_iter().map(|v| v.target).collect();
        assert_eq!(targets, vec!["example.com", "api.fal.ai", "127.0.0.1"]);
    }

    /// A write refusal's shape wasn't captured in spike 10; Seatbelt names a `file-write-*`
    /// operation and the path. The worker probe prints what a real one looks like.
    #[test]
    fn a_refused_write_names_its_path() {
        let e = json!({"tool_response": {"stderr": "Operation not permitted\n<sandbox_violations>\n\
            deny(1) file-write-create /Users/x/Library/Application Support/hive/wt.noindex/w/web/.claude (not allowed)\n\
            deny file-write-data /r/web/.git/hooks/x\n</sandbox_violations>"}});
        let v = parse(&e);
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|v| v.kind == KIND_PATH));
        assert_eq!(v[0].target, "/Users/x/Library/Application Support/hive/wt.noindex/w/web/.claude");
        assert_eq!(v[1].target, "/r/web/.git/hooks/x");
    }

    /// The 4b probe's refused writes, verbatim but for the scratch prefix: no note, only EPERM.
    #[test]
    fn a_write_refused_without_a_note_is_read_from_the_programs_own_error() {
        let fail = |error: &str| json!({"hook_event_name": "PostToolUseFailure", "tool_name": "Bash", "error": error});
        let cases = [
            ("Exit code 1\n/bin/bash: /T/p/repo/api/.git/hooks/x: Operation not permitted", "/T/p/repo/api/.git/hooks/x"),
            ("Exit code 128\nfatal: Unable to create '/T/p/repo/web/.git/worktrees/web1/index.lock': Operation not permitted", "/T/p/repo/web/.git/worktrees/web1/index.lock"),
            (
                "Exit code 1\nTraceback (most recent call last):\n  File \"<string>\", line 1, in <module>\nPermissionError: [Errno 1] Operation not permitted: '/T/p/wt.noindex/w/web/.claude'",
                "/T/p/wt.noindex/w/web/.claude",
            ),
        ];
        for (error, path) in cases {
            assert_eq!(parse(&fail(error)), vec![Violation { kind: KIND_PATH.into(), target: path.into() }], "{error}");
        }
        // A Write tool failure is the permission system's, not the sandbox's.
        let write = json!({"tool_name": "Write", "error": "/bin/bash: /x/y: Operation not permitted"});
        assert!(parse(&write).is_empty());
        assert!(parse(&fail("touch: x: Operation not permitted")).is_empty(), "relative: no path to name");
        let outside = "Exit code 1\nbash: line 1: /Users/x/.config/tool/state: Operation not permitted";
        assert_eq!(parse(&fail(outside))[0].target, "/Users/x/.config/tool/state", "outside every writable path: the sandbox's");
        let var = Writable { allow: vec![PathBuf::from("/var/folders/w")], deny: vec![PathBuf::from("/private/var/folders/w/.git")] };
        assert!(!var.refuses("/private/var/folders/w/x") && var.refuses("/var/folders/w/.git/config"), "/private/var is /var");
    }

    /// Most EPERMs aren't the sandbox's: a privacy-protected read, a chmod or touch -t of another
    /// user's file, a kill. Read as refusals, each would become a blocker the coordinator can't
    /// answer by widening anything.
    #[test]
    fn an_eperm_that_isnt_a_refused_write_is_nothing() {
        let fail = |error: &str| json!({"tool_name": "Bash", "error": format!("Exit code 1\n{error}")});
        for error in [
            "ls: /Users/x/Library/Mail: Operation not permitted",
            "cat: /Users/x/Library/Messages/chat.db: Operation not permitted",
            "cp: /Users/x/Library/Safari/History.db: Operation not permitted",
            "chmod: /etc/hosts: Operation not permitted",
            "chown: /T/p/wt.noindex/w/web/x: Operation not permitted",
            "kill: 1: Operation not permitted",
            // In its own folders, a refusal the sandbox didn't make (touch -t of a file it doesn't own).
            "touch: /T/p/wt.noindex/w/web/README.md: Operation not permitted",
            "/bin/bash: /T/p/wt.noindex/w/scratch/out.txt: Operation not permitted",
            "PermissionError: [Errno 1] Operation not permitted: '/T/p/wt.noindex/w/web/x'",
        ] {
            assert!(parse(&fail(error)).is_empty(), "{error}");
        }
        let settings = json!({"sandbox": {"filesystem": {"allowWrite": ["/T/a"], "denyWrite": ["/T/a/.git"]}}});
        let w = Writable::from_settings(&settings, Path::new("/T/cwd"));
        assert!(!w.refuses("/T/a/x") && !w.refuses("/T/cwd/y") && w.refuses("/T/a/.git/config") && w.refuses("/T/b"));
    }

    #[test]
    fn anything_else_is_nothing() {
        for e in [json!({}), json!({"error": "Exit code 1\nno note here"}), json!({"error": "<sandbox_violations>\nallow x\ndeny\ndeny mach-lookup com.apple.x (y)\n</sandbox_violations>"})] {
            assert!(parse(&e).is_empty(), "{e}");
        }
        let unterminated = json!({"error": "<sandbox_violations>\ndeny network-outbound [::1]:8765 (host is not on the allow list)"});
        assert_eq!(parse(&unterminated)[0].target, "::1");
        // The sandbox's own note is believed whatever the paths: only the bare EPERM is filtered.
        let note = json!({"error": "<sandbox_violations>\ndeny file-write-create /T/p/wt.noindex/w/web/x (y)\n</sandbox_violations>"});
        assert_eq!(parse(&note).len(), 1);
    }
}
