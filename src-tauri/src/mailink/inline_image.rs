//! Images an agent shows in a message by their path, fetched for the phone (protocol 0.18).
//!
//! An agent writes `![new icon](/Users/…/app-icon-1024.png)`. The desktop terminal shows that as
//! text; the phone renders the Markdown, and its WebView resolved the path against its own origin
//! and drew a broken image. `POST /chats/{tabId}/image {path}` copies the file into the asset
//! store (`assets::store_inline`) and answers its `FileAsset`, so the phone draws it through the
//! `GET /assets/{id}` path it already has.
//!
//! This reads a file off the user's machine (or a host the user ssh's into) for a phone, so it is
//! narrow on purpose:
//! - **Only a path the agent wrote.** The request must EQUAL a path the agent put in one of its
//!   own messages in that chat — not tool output, not the human's turns. Paths are taken out of
//!   the text whole (`agent_paths`), never searched for inside it: substring matching with
//!   boundary rules was tried first and two reviews found ways to fetch the tail of a longer path
//!   the agent never named (a9e5ab8, f598d8a). A RELATIVE path counts only where the agent
//!   delimited it — a Markdown target or backticks — because a bare word can be the tail of a
//!   path with a space in it.
//! - **Images only,** by extension AND by the file's first bytes: png, jpeg, gif, webp, heic. No
//!   svg, which is markup that can carry script. At most 25 MB, enforced at the read.
//! - **The path is on the computer the agent ran on.** Decided by where its session transcript
//!   lives, never by what the tab's terminal runs now; a remote one is read from the host its
//!   transcript was mirrored from (`mirror::session_host`), and only while the tab's bridge is to
//!   that same host.
//! - **No path ever reaches a command line a login shell parses.** The remote read is `sh -s`
//!   with the script, path included, on stdin: fish reads `\'` inside single quotes as an escape
//!   and tcsh forbids a newline in them, so a quoted path on the ssh command line could run code
//!   on those hosts (review of f598d8a).

use crate::state::AppState;
use serde_json::Value;
use sha2::Digest;
use std::sync::Arc;

/// Largest image fetched for a message.
pub const MAX_IMAGE_BYTES: u64 = 25 * 1024 * 1024;

pub(crate) enum Refusal {
    /// No such designated tab: 404, like every other chat route.
    NotFound,
    /// Said to the human verbatim.
    Refused(String),
}

fn refuse(s: impl Into<String>) -> Refusal {
    Refusal::Refused(s.into())
}

/// The image extensions served, lower-case.
fn image_ext(path: &str) -> Option<String> {
    let name = path.rsplit('/').next().unwrap_or(path);
    let ext = name.rsplit_once('.')?.1.to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "heic" | "heif").then_some(ext)
}

/// Do the bytes start like an image of that kind? An extension is only a name.
fn looks_like(ext: &str, b: &[u8]) -> bool {
    match ext {
        "png" => b.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "jpg" | "jpeg" => b.starts_with(&[0xFF, 0xD8, 0xFF]),
        "gif" => b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a"),
        "webp" => b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP",
        // ISO base media: a `ftyp` box naming a HEIF brand.
        "heic" | "heif" => {
            b.len() >= 12
                && &b[4..8] == b"ftyp"
                && matches!(&b[8..12], b"heic" | b"heix" | b"hevc" | b"hevx" | b"heim" | b"heis" | b"mif1" | b"msf1")
        }
        _ => false,
    }
}

fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with("~/") || p.starts_with("file:")
}

/// Every path an agent's message names, whole:
/// - a Markdown link/image target, `](target)` or `](<target with spaces>)`;
/// - a backtick span on one line;
/// - a bare word, only when it is ABSOLUTE (`/…`, `~/…`, `file:…`) — a bare relative word may be
///   the tail of a path with a space in it, which the agent never named.
fn agent_paths(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    // Markdown targets.
    let mut rest = text;
    while let Some(i) = rest.find("](") {
        let after = &rest[i + 2..];
        let target = if let Some(inner) = after.strip_prefix('<') {
            inner.find('>').map(|j| &inner[..j])
        } else {
            let end = after.find(|c: char| c == ')' || c.is_whitespace()).unwrap_or(after.len());
            Some(&after[..end])
        };
        if let Some(t) = target.filter(|t| !t.is_empty()) {
            out.push(t.to_string());
        }
        rest = after;
    }
    // Backtick spans, one line each.
    for line in text.lines() {
        // Odd parts are inside backticks; one with no closing backtick after it is not a span.
        let parts: Vec<&str> = line.split('`').collect();
        for i in (1..parts.len().saturating_sub(1)).step_by(2) {
            if !parts[i].is_empty() {
                out.push(parts[i].to_string());
            }
        }
    }
    // Bare absolute words, with the punctuation prose wraps them in taken off.
    for word in text.split_whitespace() {
        let w = word
            .trim_start_matches(['(', '[', '{', '<', '"', '\'', '`'])
            .trim_end_matches(['.', ',', ';', ':', '!', '?', ')', ']', '}', '>', '"', '\'', '`']);
        if is_absolute(w) {
            out.push(w.to_string());
        }
    }
    out
}

/// Does `path` equal a path the agent wrote — as written, or percent-decoded (the phone's
/// Markdown renderer encodes a src and the phone decodes it, so it can't know which form the
/// agent typed)? Each candidate is decoded ON ITS OWN: decoding the whole message let a URL's
/// `%22` become a boundary that made a path inside it look named (review of f598d8a).
pub(crate) fn referenced_by_agent(turns: &[Value], path: &str) -> bool {
    if path.is_empty() || path.chars().any(char::is_control) {
        return false;
    }
    turns
        .iter()
        .filter(|t| t.get("role").and_then(|r| r.as_str()) == Some("agent") && t.get("kind").is_none())
        .filter_map(|t| t.get("text").and_then(|x| x.as_str()))
        .flat_map(agent_paths)
        .any(|c| c == path || percent_decoded(&c).is_some_and(|d| d == path))
}

/// `s` with `%XX` escapes decoded; `None` when there are none, or the result isn't UTF-8.
fn percent_decoded(s: &str) -> Option<String> {
    if !s.contains('%') {
        return None;
    }
    let b = s.as_bytes();
    let hex = |c: u8| (c as char).to_digit(16).map(|d| d as u8);
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h << 4 | l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8(out).ok().filter(|d| d != s)
}

/// A `file:` URL's path: `file:///p`, `file://localhost/p` and `file:/p` are all `/p`. One naming
/// another host is refused — that is another machine's file.
fn strip_file_url(path: &str) -> Result<&str, Refusal> {
    let Some(rest) = path.strip_prefix("file:") else { return Ok(path) };
    if let Some(p) = rest.strip_prefix("//") {
        return match p.strip_prefix("localhost") {
            Some(q) if q.starts_with('/') => Ok(q),
            _ if p.starts_with('/') => Ok(p),
            _ => Err(refuse("A file:// link that names another computer can't be shown here.")),
        };
    }
    if rest.starts_with('/') {
        return Ok(rest);
    }
    Err(refuse("That isn't a path maiTerm can read."))
}

/// Where the agent's session is: its id, and whether its transcript is on this machine.
fn agent_session(app: &AppState, tab_id: &str) -> Option<(String, bool)> {
    let (rt, sid) = super::resolved_session_for_tab(app, tab_id)?;
    let local = super::transcript::session_is_local(rt.as_key(), &sid);
    Some((sid, local))
}

/// The folder a relative path in the agent's words is relative to: its session's.
fn agent_cwd(app: &AppState, sid: &str) -> Option<String> {
    app.agent_sessions.read().get(sid).and_then(|s| s.cwd.clone())
}

/// Fetch `path` as the agent in `tab_id` meant it, and store it for the phone.
pub(crate) async fn fetch(app: &Arc<AppState>, tab_id: &str, path: &str) -> Result<super::assets::AssetRecord, Refusal> {
    if !super::is_designated(app, tab_id) {
        return Err(Refusal::NotFound);
    }
    let path = path.trim();
    if image_ext(path).is_none() {
        return Err(refuse("Only png, jpeg, gif, webp and heic images can be shown here."));
    }
    // Transcript reads and transcript lookup are file I/O: off the async workers.
    let (referenced, session) = {
        let (app, tab, p) = (app.clone(), tab_id.to_string(), path.to_string());
        tokio::task::spawn_blocking(move || {
            (referenced_by_agent(&super::tab_transcript(&app, &tab), &p), agent_session(&app, &tab))
        })
        .await
        .unwrap_or((false, None))
    };
    if !referenced {
        return Err(refuse("That image isn't in any of the agent's recent messages in this chat."));
    }
    let Some((sid, local)) = session else {
        return Err(refuse("maiTerm can't tell which computer this agent runs on."));
    };
    // Matched as written (scheme and all); read as a path.
    let path = strip_file_url(path)?;
    let Some(ext) = image_ext(path) else {
        return Err(refuse("Only png, jpeg, gif, webp and heic images can be shown here."));
    };
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let cwd = agent_cwd(app, &sid);

    let (resolved, bytes) = if local {
        let full = local_path(path, cwd.as_deref()).ok_or_else(|| {
            refuse("maiTerm doesn't know which folder this agent is in, so it can't find a relative path.")
        })?;
        tokio::task::spawn_blocking(move || read_local(&full))
            .await
            .map_err(|e| refuse(format!("read task failed: {e}")))?
            .map_err(Refusal::Refused)?
    } else {
        // The host the agent ran on — and the tab's bridge must be to that host NOW. The tab may
        // since have ssh'd somewhere else, where the same path is a different file.
        let host = crate::mailink::mirror::session_host(&sid);
        let bridge = match crate::comms::staging_target_for_tab(app, tab_id) {
            crate::comms::StagingTarget::Remote { host_key, ssh_args } if Some(&host_key) == host.as_ref() => {
                Some((host_key, ssh_args))
            }
            _ => None,
        };
        let Some((host_key, ssh_args)) = bridge else {
            return Err(refuse("This chat ran on another computer, and maiTerm isn't connected to it right now."));
        };
        let expr = remote_path_expr(path, cwd.as_deref()).ok_or_else(|| {
            refuse("maiTerm doesn't know which folder this agent is in, so it can't find a relative path.")
        })?;
        let bytes = fetch_remote_capped(&host_key, &ssh_args, &expr).await.map_err(Refusal::Refused)?;
        (format!("{host_key}:{expr}"), bytes)
    };
    if !looks_like(&ext, &bytes) {
        return Err(refuse(format!("{name} isn't a {} image.", ext.to_ascii_uppercase())));
    }
    let key = {
        let mut h = sha2::Sha256::new();
        h.update(tab_id.as_bytes());
        h.update([0u8]);
        h.update(resolved.as_bytes());
        h.update([0u8]);
        h.update(&bytes);
        h.finalize().iter().map(|b| format!("{b:02x}")).collect::<String>()
    };
    let tab = tab_id.to_string();
    tokio::task::spawn_blocking(move || super::assets::store_inline(&tab, &name, bytes, &key))
        .await
        .map_err(|e| refuse(format!("store task failed: {e}")))?
        .map_err(Refusal::Refused)
}

/// A local path: `~/` from the home folder, a relative one from the agent's folder.
fn local_path(path: &str, cwd: Option<&str>) -> Option<std::path::PathBuf> {
    if let Some(rest) = path.strip_prefix("~/") {
        return dirs::home_dir().map(|h| h.join(rest));
    }
    if path.starts_with('/') {
        return Some(path.into());
    }
    let cwd = cwd?;
    let base = match cwd.strip_prefix("~/") {
        Some(rest) => dirs::home_dir()?.join(rest),
        None => cwd.into(),
    };
    Some(base.join(path))
}

/// Read a local image: a regular file (after following links) still named as an image, capped AT
/// THE READ — the file can grow between a size check and the read.
fn read_local(full: &std::path::Path) -> Result<(String, Vec<u8>), String> {
    let real = std::fs::canonicalize(full).map_err(|_| "That file isn't there any more.".to_string())?;
    let meta = std::fs::metadata(&real).map_err(|e| format!("Could not read it: {e}"))?;
    if !meta.is_file() {
        return Err("That isn't a file.".to_string());
    }
    // A link may point anywhere; what it points at must still be named as an image. (Its bytes
    // are checked against the shown name's kind afterwards, on every path.)
    if image_ext(&real.to_string_lossy()).is_none() {
        return Err("That path leads to a file that isn't an image.".to_string());
    }
    if meta.len() > MAX_IMAGE_BYTES {
        return Err(too_big(meta.len()));
    }
    use std::io::Read;
    let mut bytes = Vec::new();
    std::fs::File::open(&real)
        .and_then(|f| f.take(MAX_IMAGE_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|e| format!("Could not read it: {e}"))?;
    if bytes.len() as u64 > MAX_IMAGE_BYTES {
        return Err(too_big(bytes.len() as u64));
    }
    Ok((real.to_string_lossy().to_string(), bytes))
}

fn too_big(n: u64) -> String {
    format!(
        "That image is {}, over the {} limit for showing it here.",
        super::assets::human_bytes(n),
        super::assets::human_bytes(MAX_IMAGE_BYTES)
    )
}

/// POSIX single quotes. Only ever read by `sh` (the script goes on stdin to `sh -s`), never by a
/// login shell, whose quoting rules vary.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The path as a POSIX `sh` expression: `~/` as `"$HOME"/…`, a relative path from the agent's
/// (remote) folder. `None` for a relative path with no known folder.
fn remote_path_expr(path: &str, cwd: Option<&str>) -> Option<String> {
    if let Some(rest) = path.strip_prefix("~/") {
        return Some(format!("\"$HOME\"/{}", sh_quote(rest)));
    }
    if path.starts_with('/') {
        return Some(sh_quote(path));
    }
    let cwd = cwd?;
    Some(match cwd.strip_prefix("~/") {
        Some(rest) => format!("\"$HOME\"/{}/{}", sh_quote(rest), sh_quote(path)),
        None => format!("{}/{}", sh_quote(cwd), sh_quote(path)),
    })
}

/// The remote script: refuse a missing path or a non-file, then stop one byte past the cap.
fn remote_script(expr: &str) -> String {
    format!(
        "f={expr}\nif [ ! -e \"$f\" ]; then echo missing >&2; exit 3; fi\nif [ ! -f \"$f\" ]; then echo notfile >&2; exit 4; fi\nexec head -c {} < \"$f\"\n",
        MAX_IMAGE_BYTES + 1
    )
}

/// Read a remote file over the tab's bridge. The login shell is handed only `sh -s`; the script —
/// and with it the path — arrives on stdin, where only POSIX `sh` reads it.
async fn fetch_remote_capped(host_key: &str, ssh_args: &str, expr: &str) -> Result<Vec<u8>, String> {
    use tokio::io::AsyncWriteExt;
    let mut args = crate::commands::ssh_tunnel::mux_client_args(host_key);
    args.extend(ssh_args.split_whitespace().map(str::to_string));
    args.push("sh -s".to_string());
    let mut child = tokio::process::Command::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        // A timeout drops the future; without this the ssh (and the remote read) live on.
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("ssh failed to start: {e}"))?;
    let script = remote_script(expr);
    let mut stdin = child.stdin.take().ok_or("ssh has no stdin")?;
    let out = tokio::time::timeout(std::time::Duration::from_secs(30), async move {
        stdin.write_all(script.as_bytes()).await.map_err(|e| format!("could not send the request: {e}"))?;
        drop(stdin);
        child.wait_with_output().await.map_err(|e| format!("ssh failed: {e}"))
    })
    .await
    .map_err(|_| "Reading it from the other computer took too long.".to_string())??;
    match out.status.code() {
        Some(0) => {}
        Some(3) => return Err("That file isn't there any more.".to_string()),
        Some(4) => return Err("That isn't a file.".to_string()),
        _ => return Err(format!("Could not read it from the other computer: {}", String::from_utf8_lossy(&out.stderr).trim())),
    }
    if out.stdout.len() as u64 > MAX_IMAGE_BYTES {
        return Err(format!("That image is over the {} limit for showing it here.", super::assets::human_bytes(MAX_IMAGE_BYTES)));
    }
    Ok(out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn agent(t: &str) -> Value {
        json!({ "role": "agent", "text": t })
    }

    #[test]
    fn only_a_path_the_agent_wrote_counts() {
        let p = "/Users/me/app/brand/icon.png";
        assert!(referenced_by_agent(&[agent(&format!("Here: ![icon]({p})"))], p));
        assert!(referenced_by_agent(&[agent(&format!("Saved to `{p}`."))], p));
        assert!(referenced_by_agent(&[agent(&format!("Saved to {p}."))], p));
        assert!(!referenced_by_agent(&[agent(&format!("Saved to {p}.bak"))], p), "part of a longer name");
        assert!(!referenced_by_agent(&[agent(&format!("see /x{p}"))], p), "part of a longer path");
        assert!(!referenced_by_agent(&[json!({ "role": "tool", "text": format!("ls: {p}") })], p), "a tool's output");
        assert!(!referenced_by_agent(&[json!({ "role": "user", "text": p })], p), "the human's words");
        assert!(!referenced_by_agent(&[json!({ "role": "agent", "kind": "peer_message", "text": p })], p));
    }

    #[test]
    fn a_relative_path_counts_only_where_the_agent_delimited_it() {
        assert!(referenced_by_agent(&[agent("![plot](out/plot.png)")], "out/plot.png"));
        assert!(referenced_by_agent(&[agent("Saved to `out/plot.png`")], "out/plot.png"));
        // A bare relative word may be the tail of a path with a space in it.
        assert!(!referenced_by_agent(&[agent("Saved to out/plot.png")], "out/plot.png"));
        for (text, tail) in [
            ("Look at /Users/me/My Project Files/img/a.png", "Files/img/a.png"),
            ("Shot: /Users/me/Desktop/Screenshot 2026-10-05 at 16.54.55.png", "16.54.55.png"),
            ("Look at /Users/me/Library/Application Support/x/icon.png", "Support/x/icon.png"),
        ] {
            assert!(!referenced_by_agent(&[agent(text)], tail), "{tail}");
        }
        // The whole spaced path counts where it is delimited.
        assert!(referenced_by_agent(
            &[agent("`/Users/me/My Project Files/img/a.png`")],
            "/Users/me/My Project Files/img/a.png"
        ));
        assert!(referenced_by_agent(&[agent("![a](</Users/me/a b.png>)")], "/Users/me/a b.png"));
    }

    #[test]
    fn decoding_is_per_path_and_control_characters_never_match() {
        let encoded = agent("![x](file:///Users/me/a%20b.png)");
        assert!(referenced_by_agent(&[encoded.clone()], "file:///Users/me/a b.png"));
        assert!(referenced_by_agent(&[encoded], "file:///Users/me/a%20b.png"));
        // A path inside an encoded URL was never named on its own.
        let url = agent("See https://x/r?u=%22/Users/me/secret.png%22");
        assert!(!referenced_by_agent(&[url], "/Users/me/secret.png"));
        // A request spanning lines is refused outright.
        let two = agent("/tmp/out\necho hi; echo plot.png");
        assert!(!referenced_by_agent(&[two], "/tmp/out\necho hi; echo plot.png"));
        assert_eq!(percent_decoded("a%20b%E2%80%AFc").as_deref(), Some("a b\u{202F}c"));
        assert_eq!(percent_decoded("100%"), None, "a lone % is text");
    }

    #[test]
    fn file_urls_resolve_to_their_path_and_never_another_host() {
        assert_eq!(strip_file_url("file:///Users/me/a.png").ok(), Some("/Users/me/a.png"));
        assert_eq!(strip_file_url("file://localhost/Users/me/a.png").ok(), Some("/Users/me/a.png"));
        assert_eq!(strip_file_url("file:/Users/me/a.png").ok(), Some("/Users/me/a.png"));
        assert_eq!(strip_file_url("/Users/me/a.png").ok(), Some("/Users/me/a.png"));
        assert!(strip_file_url("file://server/share/a.png").is_err());
        assert!(strip_file_url("file:a.png").is_err());
    }

    #[test]
    fn images_by_name_and_by_bytes() {
        assert_eq!(image_ext("/a/b.PNG").as_deref(), Some("png"));
        assert!(image_ext("/a/b.svg").is_none(), "svg is markup");
        assert!(image_ext("/a/b").is_none());
        assert!(looks_like("png", &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0]));
        assert!(!looks_like("png", b"#!/bin/sh\n"), "a script renamed .png");
        assert!(looks_like("jpg", &[0xFF, 0xD8, 0xFF, 0xE0]));
        assert!(looks_like("webp", b"RIFF\0\0\0\0WEBPVP8 "));
        assert!(looks_like("heic", b"\0\0\0\x18ftypheic\0\0\0\0"));
    }

    #[test]
    fn the_remote_script_quotes_any_path_for_posix_sh() {
        assert_eq!(remote_path_expr("~/x/y.png", None).unwrap(), "\"$HOME\"/'x/y.png'");
        assert_eq!(remote_path_expr("/srv/a b.png", None).unwrap(), "'/srv/a b.png'");
        assert_eq!(remote_path_expr("img/a.png", Some("/srv/app")).unwrap(), "'/srv/app'/'img/a.png'");
        assert!(remote_path_expr("img/a.png", None).is_none());
        // Run the script's first line under the local sh: the path comes back byte for byte.
        for p in ["/tmp/a';touch /tmp/P;'.png", "/tmp/$(id)`id`\\x.png", "/tmp/a!b \" c.png"] {
            let expr = remote_path_expr(p, None).unwrap();
            let out = std::process::Command::new("sh")
                .arg("-c")
                .arg(format!("f={expr}\nprintf %s \"$f\""))
                .output()
                .unwrap();
            assert_eq!(String::from_utf8_lossy(&out.stdout), p);
        }
    }
}
