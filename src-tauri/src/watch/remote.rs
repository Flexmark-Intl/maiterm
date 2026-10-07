//! A watch script that runs on an ssh tab's HOST (docs/follow-ups.md §5.1): the condition an
//! agent on a remote machine waits on is on that machine.
//!
//! One run is one ssh command muxed over the bridge tunnel's maiTerm-owned ControlMaster socket
//! (`ssh_tunnel::mux_client_args`, as the transcript mirror does): no re-authentication, tens of
//! milliseconds. What runs there is a small POSIX `sh` wrapper sent on STDIN to `sh -s` — never
//! on the ssh command line, which the remote login shell would re-parse — carrying the script in a
//! heredoc. The wrapper keeps the local runner's contract on the far side:
//!
//! - the script and its `$MAITERM_WATCH_STATE` file live in `~/.maiterm/watch-scripts/` there,
//!   the script rewritten from the approved text on every run;
//! - it runs in the folder recorded at creation, with a minimal environment and the remote user's
//!   login PATH (cached there for an hour), stdin from /dev/null;
//! - it runs in a process group of its OWN (`setsid`, or job control where there is none), and
//!   the wrapper kills that whole group when it exits or its time is up. The wrapper must do this
//!   itself: ssh without a terminal sends the far side no hangup when the connection drops, so
//!   killing the local ssh would leave the script running there. A host where neither gives the
//!   script its own group runs nothing and says so;
//! - files untouched for 8 days are swept there (a follow-up waits at most 7, and every run
//!   touches its own two).
//!
//! What comes back is framed by a random marker, so nothing a login profile prints is read as
//! the answer. No answer at all — no tunnel, ssh can't connect, nothing back in time — is
//! `Unreachable`: the host, not the script, so it waits rather than counting as a broken run.

use std::time::Duration;

use super::{clean, Candidate, Outcome, OUTPUT_CAP, REPORT_CHARS, STDERR_CHARS};

/// Room past the script's own limit for the connection, the PATH probe and the wrapper.
const SSH_SLACK_SECS: u64 = 20;

/// POSIX single-quoting.
fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The remote folder as a shell word: a leading `~` is the REMOTE home.
fn cwd_word(cwd: &str) -> String {
    if cwd == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = cwd.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sq(rest))
    } else {
        sq(cwd)
    }
}

/// The wrapper `sh -s` runs on the host. `marker` frames everything it reports.
pub(super) fn wrapper(c: &Candidate, marker: &str) -> Option<String> {
    let eof = format!("MAITERM_WATCH_EOF_{marker}");
    if c.script.lines().any(|l| l == eof) {
        return None;
    }
    let body = if c.script.ends_with('\n') { c.script.clone() } else { format!("{}\n", c.script) };
    // With a shebang it runs as itself; without, under sh — as locally.
    let run = if c.script.starts_with("#!") { "\"$s\"" } else { "sh \"$s\"" };
    let id = sq(&c.id);
    Some(format!(
        r#"M={m}
say() {{ printf '\n%s %s\n' "$M" "$1"; }}
umask 077
d="$HOME/.maiterm/watch-scripts"
mkdir -p "$d" 2>/dev/null || {{ say "SETUP could not create $d"; exit 0; }}
find "$d" -type f -mtime +8 -exec rm -f {{}} + </dev/null 2>/dev/null
s="$d/"{id}.sh; st="$d/"{id}.state; o="$d/"{id}.out; e="$d/"{id}.err; to="$d/"{id}.timeout
cat > "$s" <<'{eof}'
{body}{eof}
chmod 700 "$s"
[ -f "$st" ] || : > "$st"
touch "$st"
rm -f "$to"
cd {cwd} 2>/dev/null || {{ say "NOCWD"; exit 0; }}
if [ -n "$(find "$d/.path" -mmin -60 2>/dev/null)" ]; then lp=$(cat "$d/.path"); else
  lp=$("${{SHELL:-/bin/sh}}" -lc 'printf "\n__MAITERM_PATH__%s\n" "$PATH"' </dev/null 2>/dev/null | sed -n 's/^__MAITERM_PATH__//p' | tail -n 1)
  [ -n "$lp" ] && printf '%s' "$lp" > "$d/.path"
fi
[ -n "$lp" ] || lp=/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin
# Its own group: setsid (a background job is never a group leader, so setsid doesn't fork and
# $! is the leader), or else job control, where the shell sets the group before $! is known.
if command -v setsid >/dev/null 2>&1; then iso=setsid; else
  # Job control without a terminal isn't a given: prove it on a sleep before trusting it.
  iso=; set -m 2>/dev/null
  sleep 5 </dev/null >/dev/null 2>&1 &
  q=$!
  me=$(ps -o pgid= -p $$ 2>/dev/null | tr -d ' ')
  pq=$(ps -o pgid= -p "$q" 2>/dev/null | tr -d ' ')
  kill -KILL "$q" 2>/dev/null; wait "$q" 2>/dev/null
  if [ -z "$pq" ] || [ "$pq" = "$me" ]; then say "NOGROUP"; exit 0; fi
fi
$iso env -i HOME="$HOME" USER="${{USER:-}}" LOGNAME="${{LOGNAME:-}}" SHELL="${{SHELL:-}}" LANG="${{LANG:-C.UTF-8}}" TMPDIR="${{TMPDIR:-/tmp}}" PATH="$lp" MAITERM_WATCH_STATE="$st" MAITERM_TAB_ID={tab} {run} </dev/null >"$o" 2>"$e" &
p=$!
$iso sh -c 'sleep "$1"; : > "$2"; kill -KILL -"$3" 2>/dev/null' watchdog {t} "$to" "$p" </dev/null >/dev/null 2>&1 &
w=$!
wait "$p"; rc=$?
kill -KILL -"$p" 2>/dev/null
kill -KILL -"$w" "$w" 2>/dev/null
[ -f "$to" ] && {{ rm -f "$to" "$o" "$e"; say "TIMEOUT"; exit 0; }}
say "RC $rc"
head -c {cap} "$o"
say "ERR"
head -c {cap} "$e"
rm -f "$o" "$e"
"#,
        m = sq(marker),
        id = id,
        eof = eof,
        body = body,
        cwd = cwd_word(&c.cwd),
        tab = sq(&c.tab_id),
        run = run,
        t = c.timeout.as_secs(),
        cap = OUTPUT_CAP,
    ))
}

/// Read what the wrapper reported. `None`: no report at all (the host, not the script).
pub(super) fn parse(stdout: &[u8], marker: &str, c: &Candidate, host: &str) -> Option<Outcome> {
    let tag = format!("\n{marker} ");
    let tag = tag.as_bytes();
    let find = |hay: &[u8], from: usize| hay[from..].windows(tag.len()).position(|w| w == tag).map(|i| i + from);
    let start = find(stdout, 0)?;
    let after = start + tag.len();
    let line_end = stdout[after..].iter().position(|&b| b == b'\n').map_or(stdout.len(), |i| i + after);
    let head = String::from_utf8_lossy(&stdout[after..line_end]).to_string();
    let rest = &stdout[(line_end + 1).min(stdout.len())..];
    Some(match head.as_str() {
        "NOCWD" => Outcome::Broken(format!("its folder {} no longer exists on {host}", c.cwd)),
        "TIMEOUT" => Outcome::Broken(format!("it timed out after {}s", c.timeout.as_secs())),
        "NOGROUP" => Outcome::Broken(format!(
            "{host} can't run it in a process group of its own (no setsid, no job control), so maiTerm couldn't be sure of stopping it"
        )),
        h if h.starts_with("SETUP ") => Outcome::Broken(h["SETUP ".len()..].to_string()),
        h if h.starts_with("RC ") => {
            let code: Option<i32> = h["RC ".len()..].trim().parse().ok();
            let (out, err) = match find(rest, 0) {
                Some(i) => {
                    let e = i + tag.len();
                    let e = rest[e..].iter().position(|&b| b == b'\n').map_or(rest.len(), |j| (j + e + 1).min(rest.len()));
                    (&rest[..i], &rest[e..])
                }
                None => (rest, &rest[rest.len()..]),
            };
            match code {
                Some(0) => Outcome::Met(clean(out, REPORT_CHARS, false)),
                Some(1) => Outcome::NotYet,
                // The shell reports a signal as 128 + n.
                Some(n) => {
                    let why = if n > 128 { "it was killed by a signal".to_string() } else { format!("exit {n}") };
                    let err = clean(err, STDERR_CHARS, true);
                    Outcome::Broken(if err.is_empty() { why } else { format!("{why}: {err}") })
                }
                None => Outcome::Broken(format!("{host} sent back an exit status maiTerm couldn't read")),
            }
        }
        other => Outcome::Broken(format!("{host} sent back something maiTerm couldn't read: {other}")),
    })
}

pub(super) async fn execute(c: &Candidate, host: &str, ssh_args: &str) -> Outcome {
    let marker = format!("__MAITERM_WATCH_{}__", uuid::Uuid::new_v4().simple());
    let Some(input) = wrapper(c, &marker) else {
        return Outcome::Broken("the script contains maiTerm's own end-of-script line".into());
    };
    // Mux over the bridge tunnel's master — never become one (mirror.rs does the same).
    let mut args = crate::commands::ssh_tunnel::mux_client_args(host);
    args.extend(ssh_args.split_whitespace().map(str::to_string));
    args.push("sh -s".into());

    let mut cmd = tokio::process::Command::new("ssh");
    cmd.args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    let mut child = match cmd.spawn() {
        Ok(ch) => ch,
        Err(e) => return Outcome::Unreachable(format!("ssh could not start: {e}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        use tokio::io::AsyncWriteExt;
        let _ = stdin.write_all(input.as_bytes()).await;
        // Dropped here: EOF ends `sh -s`'s input.
    }
    let limit = c.timeout + Duration::from_secs(SSH_SLACK_SECS);
    let output = match tokio::time::timeout(limit, child.wait_with_output()).await {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => return Outcome::Unreachable(format!("ssh to {host} failed: {e}")),
        // The wrapper ends the script itself at its own limit; past this it is the connection.
        Err(_) => return Outcome::Unreachable(format!("no answer from {host} within {}s", limit.as_secs())),
    };
    match parse(&output.stdout, &marker, c, host) {
        Some(o) => o,
        None if output.status.code() == Some(255) || output.stdout.is_empty() => {
            let err = clean(&output.stderr, STDERR_CHARS, true);
            Outcome::Unreachable(if err.is_empty() { format!("ssh to {host} failed") } else { format!("ssh to {host} failed: {err}") })
        }
        None => Outcome::Broken(format!("{host} ran it but sent back no result")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn candidate(script: &str, cwd: &str, timeout: u64) -> Candidate {
        Candidate {
            id: format!("test-{}", uuid::Uuid::new_v4()),
            tab_id: "tab".into(),
            script: script.into(),
            cwd: cwd.into(),
            host: Some("me@here".into()),
            every: Duration::from_secs(60),
            timeout: Duration::from_secs(timeout),
        }
    }

    /// Runs the wrapper with THIS machine's `sh -s` standing in for the host, under a scratch
    /// HOME, with no terminal — as ssh -T gives it.
    async fn run_local(c: &Candidate) -> (Outcome, PathBuf) {
        let home = std::env::temp_dir().join(format!("maiterm-remote-watch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&home).unwrap();
        let marker = format!("__MAITERM_WATCH_{}__", uuid::Uuid::new_v4().simple());
        let input = wrapper(c, &marker).unwrap();
        let mut child = tokio::process::Command::new("/bin/sh")
            .arg("-s")
            .env_clear()
            .env("HOME", &home)
            .env("ZDOTDIR", &home)
            .env("SHELL", "/bin/sh")
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        {
            use tokio::io::AsyncWriteExt;
            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(input.as_bytes()).await.unwrap();
        }
        let out = child.wait_with_output().await.unwrap();
        (parse(&out.stdout, &marker, c, "me@here").expect("a report"), home)
    }

    #[tokio::test]
    async fn exit_codes_output_and_state_cross_over() {
        let tmp = std::env::temp_dir().display().to_string();
        let c = candidate("echo found it\necho \"$MAITERM_WATCH_STATE\" >&2\nexit 0", &tmp, 5);
        let (o, home) = run_local(&c).await;
        assert!(matches!(o, Outcome::Met(ref s) if s == "found it"), "{:?}", matches!(o, Outcome::Met(_)));
        assert!(home.join(".maiterm/watch-scripts").join(format!("{}.state", c.id)).exists());
        let (o, _) = run_local(&candidate("exit 1", &tmp, 5)).await;
        assert!(matches!(o, Outcome::NotYet));
        let (o, _) = run_local(&candidate("echo nope >&2; exit 3", &tmp, 5)).await;
        assert!(matches!(o, Outcome::Broken(ref s) if s == "exit 3: nope"));
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn quotes_and_a_tilde_folder_survive() {
        let (o, home) = run_local(&candidate("printf '%s' \"it's $(pwd)\"", "~", 5)).await;
        // The temp folder can sit behind a symlink (/var → /private/var): compare real paths.
        let want = format!("it's {}", home.canonicalize().unwrap().display());
        let got = match o {
            Outcome::Met(s) => s.strip_prefix("it's ").map(|p| format!("it's {}", std::fs::canonicalize(p).unwrap().display())),
            _ => None,
        };
        assert_eq!(got.as_deref(), Some(want.as_str()));
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn a_missing_folder_is_broken() {
        let (o, home) = run_local(&candidate("exit 0", "/no/such/folder/anywhere", 5)).await;
        assert!(matches!(o, Outcome::Broken(ref s) if s.contains("no longer exists on me@here")));
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn a_timeout_kills_the_whole_group() {
        let tmp = std::env::temp_dir();
        let flag = tmp.join(format!("maiterm-remote-orphan-{}", uuid::Uuid::new_v4()));
        // A grandchild that would outlive the script if only the script were killed.
        let script = format!("(sleep 3; touch {}) &\nsleep 30\n", flag.display());
        let started = std::time::Instant::now();
        let (o, home) = run_local(&candidate(&script, &tmp.display().to_string(), 1)).await;
        assert!(matches!(o, Outcome::Broken(ref s) if s.contains("timed out")));
        assert!(started.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!flag.exists(), "the grandchild was killed with its group");
        let _ = std::fs::remove_dir_all(home);
    }

    #[tokio::test]
    async fn a_finished_script_leaves_nothing_running() {
        let tmp = std::env::temp_dir();
        let flag = tmp.join(format!("maiterm-remote-orphan-{}", uuid::Uuid::new_v4()));
        let script = format!("(sleep 3; touch {}) &\nexit 1\n", flag.display());
        let (o, home) = run_local(&candidate(&script, &tmp.display().to_string(), 5)).await;
        assert!(matches!(o, Outcome::NotYet));
        tokio::time::sleep(Duration::from_secs(4)).await;
        assert!(!flag.exists(), "what it left behind was killed with its group");
        let _ = std::fs::remove_dir_all(home);
    }

    /// Against a real host: `MAITERM_WATCH_TEST_HOST=ews@nova cargo test --lib remote -- --ignored`.
    /// Runs under a scratch HOME there (removed after), never the host user's own.
    #[tokio::test]
    #[ignore]
    async fn on_a_real_host() {
        let Ok(host) = std::env::var("MAITERM_WATCH_TEST_HOST") else { return };
        let run = |script: &str, cwd: &str, t: u64| {
            let c = candidate(script, cwd, t);
            let host = host.clone();
            async move {
                let marker = format!("__MAITERM_WATCH_{}__", uuid::Uuid::new_v4().simple());
                let input = wrapper(&c, &marker).unwrap();
                let mut child = tokio::process::Command::new("ssh")
                    .args(["-o", "BatchMode=yes", "-T", &host, "h=$(mktemp -d) && HOME=$h sh -s; rc=$?; rm -rf \"$h\"; exit $rc"])
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .unwrap();
                {
                    use tokio::io::AsyncWriteExt;
                    child.stdin.take().unwrap().write_all(input.as_bytes()).await.unwrap();
                }
                let out = child.wait_with_output().await.unwrap();
                parse(&out.stdout, &marker, &c, &host).expect("a report")
            }
        };
        assert!(matches!(run("echo \"on $(uname -s)\"", "/tmp", 5).await, Outcome::Met(ref s) if s == "on Linux"));
        assert!(matches!(run("exit 1", "/tmp", 5).await, Outcome::NotYet));
        assert!(matches!(run("command -v setsid >/dev/null && exit 0; exit 1", "~", 5).await, Outcome::Met(_)));
        let flag = format!("/tmp/maiterm-remote-orphan-{}", uuid::Uuid::new_v4());
        let o = run(&format!("(sleep 3; touch {flag}) &\nsleep 30"), "/tmp", 1).await;
        assert!(matches!(o, Outcome::Broken(ref s) if s.contains("timed out")));
        tokio::time::sleep(Duration::from_secs(4)).await;
        let check = format!("test -e {flag} && echo present || echo absent");
        let o = run(&check, "/tmp", 5).await;
        assert!(matches!(o, Outcome::Met(ref s) if s == "absent"), "the grandchild was killed with its group");
        assert!(matches!(run("exit 0", "/no/such/dir", 5).await, Outcome::Broken(ref s) if s.contains("no longer exists")));
    }

    #[test]
    fn noise_before_the_marker_is_ignored_and_no_marker_is_no_report() {
        let c = candidate("x", "/", 5);
        let m = "__MAITERM_WATCH_abc__";
        let out = format!("motd says hi\n\n{m} RC 0\nall good\n{m} ERR\n");
        assert!(matches!(parse(out.as_bytes(), m, &c, "h"), Some(Outcome::Met(ref s)) if s == "all good"));
        assert!(parse(b"Permission denied (publickey).\n", m, &c, "h").is_none());
    }

    #[test]
    fn a_script_holding_the_end_line_is_refused() {
        let m = "abc";
        let c = candidate("echo\nMAITERM_WATCH_EOF_abc\nrm -rf /", "/", 5);
        assert!(wrapper(&c, m).is_none());
    }
}
