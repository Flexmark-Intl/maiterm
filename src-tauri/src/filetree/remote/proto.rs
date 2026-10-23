//! The wire between maiTerm and the Files helper on an ssh host (docs/file-tree.md §15.1–15.2).
//!
//! **Starting it.** The ssh command line carries only a constant: `exec python3 -u -c
//! '<LOADER>'`. The loader reads one decimal length line from stdin, then that many bytes, and
//! runs them as the helper; the same stdin buffer then carries the protocol. So no user data
//! ever reaches the remote argv, which the login shell re-parses (sh, bash, zsh, fish and csh
//! all take the single-quoted loader as one word: it holds no `'`, `\`, `!` or `$`), and
//! nothing is installed on the host. `helper.py` goes over stdin at every start.
//!
//! **Frames.** One JSON header line, then `len` raw bytes when the header has a `len`, both
//! ways. Requests carry an `id` and the reply repeats it; events carry `ev` instead. Listings,
//! file contents and git output travel as raw bytes, never base64.
//!
//! **No python3.** A host without one (or with one older than 3.6) gets a stateless `sh -s`
//! per request, the script on stdin, its answer framed by a random marker so a login profile's
//! chatter is never read as the answer. Read-only: listing and the resolver's existence check.

use std::io::{self, BufRead};

use serde_json::{Map, Value};

use super::super::list::ListError;
use super::super::types::Entry;

/// The helper itself, sent at every start (about 40 KB).
pub const HELPER: &str = include_str!("helper.py");

/// Reads the helper's length and source from stdin and runs it. Python 3.6-valid, one line,
/// and free of `'`, `\`, `!` and `$`, so every login shell passes it through single quotes as
/// it is.
pub const LOADER: &str = r#"import sys;b=sys.stdin.buffer;n=int(b.readline());exec(compile(b.read(n),"maiterm-helper","exec"))"#;

/// A header line longer than this is a broken stream, not a frame.
pub const MAX_HEADER: usize = 1 << 20;

/// The remote command: the loader under python3, unbuffered.
pub fn remote_command() -> String {
    format!("exec python3 -u -c '{LOADER}'")
}

/// What goes on stdin first: the helper's length, then the helper.
pub fn bootstrap() -> Vec<u8> {
    let mut v = format!("{}\n", HELPER.len()).into_bytes();
    v.extend_from_slice(HELPER.as_bytes());
    v
}

/// ssh options that set up a forward or change what the session is: never wanted on the
/// helper's connection. A forward riding a mux client would be asked of the tunnel's master,
/// and `-R` is also what the orphan sweep (`ssh_tunnel::kill_orphaned_tunnels`) looks for.
const DROPPED_WITH_ARG: [&str; 4] = ["-L", "-R", "-D", "-W"];
const DROPPED: [&str; 6] = ["-N", "-f", "-M", "-n", "-t", "-T"];

/// The connect args a helper (or fallback shell) uses: the tab's own, without forwards or
/// session flags.
pub fn clean_connect_args(ssh_args: &str) -> Vec<String> {
    let words: Vec<&str> = ssh_args.split_whitespace().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        if DROPPED_WITH_ARG.contains(&w) {
            i += 2;
            continue;
        }
        if DROPPED_WITH_ARG.iter().any(|f| w.starts_with(f) && w.len() > 2) || DROPPED.contains(&w) || w.chars().skip(1).all(|c| c == 't') && w.starts_with("-t") {
            i += 1;
            continue;
        }
        out.push(w.to_string());
        i += 1;
    }
    out
}

/// `ssh <mux args> -o ClearAllForwardings=yes -o RemoteCommand=none <connect args> <remote
/// command>`: muxed over the tunnel's ControlMaster when there is one, else one direct
/// BatchMode connection. A `RemoteCommand` in the user's ssh_config (`tmux new -A`, a common
/// setup) makes ssh refuse any command of ours before it connects ("Cannot execute
/// command-line and remote command", exit 255), which reads as a dropped connection and was
/// retried forever; a command-line option wins over the config.
pub fn ssh_args(tunnel_key: &str, ssh_args: &str, remote: &str) -> Vec<String> {
    let mut args = crate::commands::ssh_tunnel::mux_client_args(tunnel_key);
    args.push("-o".into());
    args.push("ClearAllForwardings=yes".into());
    args.push("-o".into());
    args.push("RemoteCommand=none".into());
    args.extend(clean_connect_args(ssh_args));
    args.push(remote.to_string());
    args
}

/// One frame, encoded.
pub fn encode(header: &Map<String, Value>, payload: &[u8]) -> Vec<u8> {
    let mut h = header.clone();
    if !payload.is_empty() {
        h.insert("len".into(), Value::from(payload.len()));
    }
    let mut out = serde_json::to_vec(&Value::Object(h)).unwrap_or_default();
    out.push(b'\n');
    out.extend_from_slice(payload);
    out
}

#[derive(Debug, Clone)]
pub struct Frame {
    pub header: Map<String, Value>,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn is_ready(&self) -> bool {
        self.header.get("ev").and_then(Value::as_str) == Some("ready")
    }

    pub fn id(&self) -> Option<u64> {
        self.header.get("id").and_then(Value::as_u64)
    }

    pub fn str(&self, key: &str) -> Option<&str> {
        self.header.get(key).and_then(Value::as_str)
    }
}

/// One header line, without its newline; None at the end of the stream.
fn read_line(r: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(if line.is_empty() { None } else { Some(line) });
        }
        match buf.iter().position(|b| *b == b'\n') {
            Some(i) => {
                line.extend_from_slice(&buf[..i]);
                r.consume(i + 1);
                return Ok(Some(line));
            }
            None => {
                let n = buf.len();
                line.extend_from_slice(buf);
                r.consume(n);
            }
        }
        if line.len() > MAX_HEADER {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "a header line past 1 MB"));
        }
    }
}

/// What the stream holds next.
pub enum Read {
    Frame(Frame),
    /// A line that isn't a frame: before the helper's `ready`, a login profile's output.
    Noise,
    End,
}

/// Read the next frame. A line that isn't a JSON object is `Noise`; a frame whose payload
/// is cut short is the end of the stream.
pub fn read_frame(r: &mut impl BufRead) -> io::Result<Read> {
    let Some(line) = read_line(r)? else { return Ok(Read::End) };
    let header = match serde_json::from_slice::<Value>(&line) {
        Ok(Value::Object(m)) => m,
        _ => return Ok(Read::Noise),
    };
    let n = header.get("len").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut payload = vec![0u8; n];
    if n > 0 {
        if let Err(e) = r.read_exact(&mut payload) {
            return if e.kind() == io::ErrorKind::UnexpectedEof { Ok(Read::End) } else { Err(e) };
        }
    }
    Ok(Read::Frame(Frame { header, payload }))
}

/// A listing's payload: per entry its kind letter, `'0' + flags`, the name's bytes, NUL.
pub fn parse_listing(payload: &[u8]) -> Vec<Entry> {
    payload
        .split(|b| *b == 0)
        .filter(|r| r.len() >= 3)
        .filter_map(|r| {
            let k = match r[0] {
                b'f' => "f",
                b'd' => "d",
                b'l' => "l",
                b'L' => "L",
                b'o' => "o",
                _ => return None,
            };
            let f = r[1].checked_sub(b'0').filter(|f| *f < 16)?;
            Some(Entry { n: String::from_utf8_lossy(&r[2..]).into_owned(), k, f })
        })
        .collect()
}

/// A helper error code as a listing error.
pub fn list_error(code: &str, msg: &str) -> ListError {
    match code {
        "gone" => ListError::Gone,
        "denied" => ListError::Denied(msg.to_string()),
        _ => ListError::Other(msg.to_string()),
    }
}

// ── The no-python fallback ───────────────────────────────────────────────────────────────

/// POSIX single-quoting.
pub fn sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A remote path as a shell word: a leading `~` is the REMOTE home.
pub fn path_word(p: &str) -> String {
    if p == "~" {
        "\"$HOME\"".into()
    } else if let Some(rest) = p.strip_prefix("~/") {
        format!("\"$HOME\"/{}", sq(rest))
    } else {
        sq(p)
    }
}

/// A folder of `root` as a shell word; `rel` is checked first like every rel.
fn dir_word(root: &str, rel: &str) -> Result<String, String> {
    let parts = super::super::paths::split_rel(rel)?;
    let base = root.trim_end_matches('/');
    Ok(if parts.is_empty() { path_word(if root.is_empty() { "/" } else { root }) } else { path_word(&format!("{base}/{rel}")) })
}

/// List one folder with `sh`: each entry typed by `test`, NUL-terminated so any name survives.
/// No ignore flags here: that takes the helper.
pub fn sh_list_script(root: &str, rel: &str, marker: &str) -> Result<String, String> {
    let dir = dir_word(root, rel)?;
    Ok(format!(
        r#"M={m}
say() {{ printf '\n%s %s\n' "$M" "$1"; }}
[ -d {dir} ] || {{ say GONE; exit 0; }}
cd {dir} 2>/dev/null || {{ say DENIED; exit 0; }}
say LIST
for f in .* *; do
  case $f in .|..|.git|.DS_Store|Thumbs.db) continue;; esac
  if [ -L "$f" ]; then if [ -d "$f" ]; then t=L; else t=l; fi
  elif [ -d "$f" ]; then t=d
  elif [ -f "$f" ]; then t=f
  elif [ -e "$f" ]; then t=o
  else continue; fi
  printf '%s%s\0' "$t" "$f"
done
say END
"#,
        m = sq(marker),
        dir = dir,
    ))
}

/// The text after `\n<marker> <word>\n`, up to the next marker line.
fn section<'a>(out: &'a [u8], marker: &str, word: &str) -> Option<&'a [u8]> {
    let tag = format!("\n{marker} {word}\n");
    let start = find(out, tag.as_bytes())? + tag.len();
    let rest = &out[start..];
    let end = find(rest, format!("\n{marker} ").as_bytes()).unwrap_or(rest.len());
    Some(&rest[..end])
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// What `sh_list_script` printed. None: no answer at all (the connection, not the folder).
pub fn parse_sh_list(out: &[u8], marker: &str) -> Option<Result<Vec<Entry>, ListError>> {
    if find(out, format!("\n{marker} GONE\n").as_bytes()).is_some() {
        return Some(Err(ListError::Gone));
    }
    if find(out, format!("\n{marker} DENIED\n").as_bytes()).is_some() {
        return Some(Err(ListError::Denied("this folder can't be read".into())));
    }
    let body = section(out, marker, "LIST")?;
    // The END marker must be there too: a listing cut short is no listing.
    find(out, format!("\n{marker} END\n").as_bytes())?;
    let mut entries = Vec::new();
    for rec in body.split(|b| *b == 0).filter(|r| r.len() >= 2) {
        let k = match rec[0] {
            b'f' => "f",
            b'd' => "d",
            b'l' => "l",
            b'L' => "L",
            b'o' => "o",
            _ => continue,
        };
        let n = String::from_utf8_lossy(&rec[1..]).into_owned();
        let f = if n.starts_with('.') { super::super::types::FLAG_HIDDEN } else { 0 };
        entries.push(Entry { n, k, f });
    }
    Some(Ok(entries))
}

/// The resolver's question without the helper: for each candidate, is it a folder, its real
/// path, and (when git is there) its repository's top level.
pub fn sh_probe_script(paths: &[String], marker: &str) -> String {
    let mut s = format!("M={}\nprintf '\\n%s PROBE\\n' \"$M\"\n", sq(marker));
    for p in paths {
        let w = path_word(p);
        s.push_str(&format!(
            "if [ -d {w} ]; then r=$(cd {w} 2>/dev/null && pwd -P); t=$(cd {w} 2>/dev/null && git rev-parse --show-toplevel 2>/dev/null); printf 'D%s\\0%s\\0' \"$r\" \"$t\"; else printf 'N\\0\\0'; fi\n"
        ));
    }
    s.push_str("printf '\\n%s END\\n' \"$M\"\n");
    s
}

/// One probed candidate: its real path when it is a folder there, and its top level.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Probed {
    pub real: Option<String>,
    pub top: Option<String>,
}

pub fn parse_sh_probe(out: &[u8], marker: &str, n: usize) -> Option<Vec<Probed>> {
    let body = section(out, marker, "PROBE")?;
    find(out, format!("\n{marker} END\n").as_bytes())?;
    let fields: Vec<&[u8]> = body.split(|b| *b == 0).collect();
    let mut out = Vec::new();
    for i in 0..n {
        let a = fields.get(i * 2)?;
        let b = fields.get(i * 2 + 1).copied().unwrap_or(&[]);
        let text = |f: &[u8]| (!f.is_empty()).then(|| String::from_utf8_lossy(f).into_owned());
        out.push(match a.first() {
            Some(b'D') => Probed { real: text(&a[1..]), top: text(b) },
            _ => Probed::default(),
        });
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_loader_is_one_word_in_every_shell() {
        for c in ['\'', '\\', '!', '$', '\n', '`'] {
            assert!(!LOADER.contains(c), "the loader holds {c:?}");
        }
        assert!(remote_command().starts_with("exec python3 -u -c '"));
        assert!(LOADER.len() < 160);
    }

    #[test]
    fn frames_round_trip_with_raw_payloads() {
        let mut h = Map::new();
        h.insert("id".into(), Value::from(7));
        h.insert("op".into(), Value::from("put"));
        let payload: Vec<u8> = (0..=255u8).chain([b'\n', 0, b'{']).collect();
        let mut wire = b"motd: hello\n".to_vec();
        wire.extend(encode(&h, &payload));
        wire.extend(encode(&h, &[]));
        let mut r = io::BufReader::new(&wire[..]);
        assert!(matches!(read_frame(&mut r).unwrap(), Read::Noise));
        let Read::Frame(f) = read_frame(&mut r).unwrap() else { panic!("a frame") };
        assert_eq!((f.id(), f.str("op"), f.payload.as_slice()), (Some(7), Some("put"), payload.as_slice()));
        let Read::Frame(f) = read_frame(&mut r).unwrap() else { panic!("a frame") };
        assert!(f.payload.is_empty() && !f.header.contains_key("len"));
        assert!(matches!(read_frame(&mut r).unwrap(), Read::End));
        // A payload cut short is the end, never a short frame.
        let mut cut = encode(&h, b"abcdef");
        cut.truncate(cut.len() - 2);
        assert!(matches!(read_frame(&mut io::BufReader::new(&cut[..])).unwrap(), Read::End));
    }

    #[test]
    fn listing_payloads_parse_with_flags_and_any_name() {
        let p = b"d0src\0f2.env\0L4up\0l9weird\nname\0x0bad\0f\0";
        let e = parse_listing(p);
        assert_eq!(
            e,
            vec![
                Entry { n: "src".into(), k: "d", f: 0 },
                Entry { n: ".env".into(), k: "f", f: 2 },
                Entry { n: "up".into(), k: "L", f: 4 },
                Entry { n: "weird\nname".into(), k: "l", f: 9 },
            ]
        );
    }

    #[test]
    fn helper_args_drop_forwards_and_session_flags() {
        let a = clean_connect_args("-p 2222 -L 8080:localhost:80 -R 9000:x:1 -N -Dproxy -tt -x -C ews@nova");
        assert_eq!(a, vec!["-p", "2222", "-x", "-C", "ews@nova"]);
        let argv = ssh_args("ews@nova", "-x -C ews@nova", &remote_command()).join(" ");
        // Never one of the orphan sweep's tunnels (`-R` plus our ControlPath).
        assert!(!argv.contains(" -R "));
        assert!(argv.contains("ClearAllForwardings=yes"));
        // A RemoteCommand in ssh_config would otherwise refuse ours before connecting.
        assert!(argv.contains("-o RemoteCommand=none"));
        assert!(argv.ends_with(&format!("ews@nova {}", remote_command())));
    }

    /// The real ssh, against a config that sets `RemoteCommand`: without our override it
    /// refuses before connecting. The host can't resolve, so nothing leaves this computer.
    #[test]
    fn a_remote_command_in_ssh_config_does_not_refuse_the_helper() {
        let Ok(probe) = std::process::Command::new("ssh").arg("-V").output() else { return };
        if !probe.status.success() && probe.stderr.is_empty() {
            return;
        }
        let dir = std::env::temp_dir().join(format!("ft-sshcfg-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let cfg = dir.join("config");
        std::fs::write(&cfg, "Host *\n  RemoteCommand tmux new -A -s main\n").unwrap();
        let connect = format!("-F {} nohost.invalid", cfg.display());
        let run = |args: Vec<String>| {
            let out = std::process::Command::new("ssh").args(args).stdin(std::process::Stdio::null()).output().unwrap();
            String::from_utf8_lossy(&out.stderr).to_lowercase()
        };
        let ours = run(ssh_args("nohost.invalid", &connect, "true"));
        // Without the override this OpenSSH (7.6 or later) says exactly that and exits 255.
        let plain = run(vec!["-F".into(), cfg.display().to_string(), "-o".into(), "BatchMode=yes".into(), "nohost.invalid".into(), "true".into()]);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!ours.contains("cannot execute command-line and remote command"), "{ours}");
        if plain.contains("cannot execute command-line and remote command") {
            assert!(ours.contains("nohost.invalid"), "ours got as far as connecting: {ours}");
        }
    }

    #[test]
    fn sh_listing_parses_and_refuses_an_escape() {
        let m = "__M__";
        let out = format!("noise\n\n{m} LIST\nd.git-like\0f.env\0lname with\nnewline\0\n{m} END\n");
        let e = parse_sh_list(out.as_bytes(), m).unwrap().unwrap();
        assert_eq!(e[1], Entry { n: ".env".into(), k: "f", f: 2 });
        assert_eq!(e[2].n, "name with\nnewline");
        assert!(matches!(parse_sh_list(format!("\n{m} GONE\n").as_bytes(), m), Some(Err(ListError::Gone))));
        assert!(parse_sh_list(b"Permission denied (publickey).\n", m).is_none());
        assert!(parse_sh_list(format!("\n{m} LIST\nf1\0").as_bytes(), m).is_none(), "cut short");
        assert!(sh_list_script("/srv", "../etc", m).is_err());
    }
}
