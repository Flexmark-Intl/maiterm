//! Images an agent shows in a message by their path, fetched for the phone (protocol 0.18).
//!
//! An agent writes `![new icon](/Users/…/app-icon-1024.png)`. The desktop terminal shows that as
//! text; the phone renders the Markdown, and its WebView resolved the path against its own origin
//! and drew a broken image. `POST /chats/{tabId}/image {path}` copies the file into the asset
//! store (`assets::store_inline`) and answers its `FileAsset`, so the phone draws it through the
//! `GET /assets/{id}` path it already has.
//!
//! This reads a file off the user's machine for a phone, so it is narrow on purpose:
//! - **Only a path the agent already showed.** It must appear, as a whole path, in one of the
//!   AGENT's own messages in that chat's transcript — not a tool's output (an `ls` lists anything)
//!   and not the human's turns. The phone can fetch what the agent put in front of the human,
//!   never browse.
//! - **Images only,** by extension AND by the file's first bytes: png, jpeg, gif, webp, heic. No
//!   svg, which is markup that can carry script. At most 25 MB.
//! - **An SSH tab's path is on the remote host.** It is fetched over the tab's bridge, as
//!   `sendFilesToPhone` does, and never read from a local file that happens to share the name.
//! - **Relative and `~/` paths** resolve against the agent session's folder and the right home.

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

/// Is `path` shown, as a whole path, in one of the agent's own messages? "Whole" means the
/// characters around it can't be part of a longer path: `x.png` inside `x.png.bak` doesn't count.
pub(crate) fn referenced_by_agent(turns: &[Value], path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let edge = |c: Option<char>| match c {
        None => true,
        Some(c) => c.is_whitespace() || matches!(c, '(' | ')' | '<' | '>' | '"' | '\'' | '`' | '[' | ']' | ','),
    };
    turns
        .iter()
        .filter(|t| t.get("role").and_then(|r| r.as_str()) == Some("agent") && t.get("kind").is_none())
        .filter_map(|t| t.get("text").and_then(|x| x.as_str()))
        .any(|text| {
            text.match_indices(path).any(|(i, _)| {
                edge(text[..i].chars().next_back()) && edge(text[i + path.len()..].chars().next())
            })
        })
}

/// The folder a relative path in the agent's words is relative to: its current session's.
fn agent_cwd(app: &AppState, tab_id: &str) -> Option<String> {
    let sid = super::current_session_id(app, tab_id)?;
    app.agent_sessions.read().get(&sid).and_then(|s| s.cwd.clone())
}

/// Fetch `path` as the agent in `tab_id` meant it, and store it for the phone.
pub(crate) async fn fetch(app: &Arc<AppState>, tab_id: &str, path: &str) -> Result<super::assets::AssetRecord, Refusal> {
    if !super::is_designated(app, tab_id) {
        return Err(Refusal::NotFound);
    }
    let path = path.trim();
    let Some(ext) = image_ext(path) else {
        return Err(refuse("Only png, jpeg, gif, webp and heic images can be shown here."));
    };
    // The transcript read is file I/O on a large file: off the async workers.
    let referenced = {
        let (app, tab, p) = (app.clone(), tab_id.to_string(), path.to_string());
        tokio::task::spawn_blocking(move || referenced_by_agent(&super::tab_transcript(&app, &tab), &p))
            .await
            .unwrap_or(false)
    };
    if !referenced {
        return Err(refuse("That image isn't in any of the agent's recent messages in this chat."));
    }
    let name = path.rsplit('/').next().unwrap_or(path).to_string();
    let cwd = agent_cwd(app, tab_id);

    let (resolved, bytes) = match crate::comms::staging_target_for_tab(app, tab_id) {
        crate::comms::StagingTarget::Unavailable => {
            return Err(refuse("This chat runs on another computer, and maiTerm's connection to it is down."));
        }
        crate::comms::StagingTarget::Remote { host_key, ssh_args } => {
            let expr = remote_path_expr(path, cwd.as_deref()).ok_or_else(|| {
                refuse("maiTerm doesn't know which folder this agent is in, so it can't find a relative path.")
            })?;
            let bytes = fetch_remote_capped(&host_key, &ssh_args, &expr).await.map_err(Refusal::Refused)?;
            (format!("{host_key}:{expr}"), bytes)
        }
        crate::comms::StagingTarget::Local => {
            let full = local_path(path, cwd.as_deref()).ok_or_else(|| {
                refuse("maiTerm doesn't know which folder this agent is in, so it can't find a relative path.")
            })?;
            tokio::task::spawn_blocking(move || read_local(&full))
                .await
                .map_err(|e| refuse(format!("read task failed: {e}")))?
                .map_err(Refusal::Refused)?
        }
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

/// Read a local image: a regular file (after following links), under the cap, still an image name.
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
    let bytes = std::fs::read(&real).map_err(|e| format!("Could not read it: {e}"))?;
    Ok((real.to_string_lossy().to_string(), bytes))
}

fn too_big(n: u64) -> String {
    format!(
        "That image is {}, over the {} limit for showing it here.",
        super::assets::human_bytes(n),
        super::assets::human_bytes(MAX_IMAGE_BYTES)
    )
}

fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// The remote shell expression for the path: `~/` as `"$HOME"/…`, a relative path from the
/// agent's (remote) folder. `None` for a relative path with no known folder.
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

/// Read a remote file over the tab's bridge, refusing a non-file and stopping one byte past the
/// cap — a remote `cat` of anything would be read whole before its size could be judged.
async fn fetch_remote_capped(host_key: &str, ssh_args: &str, expr: &str) -> Result<Vec<u8>, String> {
    let mut args = crate::commands::ssh_tunnel::mux_client_args(host_key);
    args.extend(ssh_args.split_whitespace().map(str::to_string));
    args.push(format!(
        "f={expr}; if [ ! -e \"$f\" ]; then echo missing >&2; exit 3; fi; if [ ! -f \"$f\" ]; then echo notfile >&2; exit 4; fi; head -c {} < \"$f\"",
        MAX_IMAGE_BYTES + 1
    ));
    let out = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::process::Command::new("ssh")
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    .map_err(|_| "Reading it from the other computer took too long.".to_string())?
    .map_err(|e| format!("ssh failed to start: {e}"))?;
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

    #[test]
    fn only_a_whole_path_in_the_agents_own_words_counts() {
        let p = "/Users/me/app/brand/icon.png";
        let agent = |t: &str| json!({ "role": "agent", "text": t });
        assert!(referenced_by_agent(&[agent(&format!("Here: ![icon]({p})"))], p));
        assert!(referenced_by_agent(&[agent(&format!("Saved to `{p}`."))], p));
        assert!(referenced_by_agent(&[agent(&format!("Saved to {p}"))], p));
        assert!(!referenced_by_agent(&[agent(&format!("Saved to {p}.bak"))], p), "part of a longer name");
        assert!(!referenced_by_agent(&[agent(&format!("see /x{p}"))], p), "part of a longer path");
        assert!(!referenced_by_agent(&[json!({ "role": "tool", "text": format!("ls: {p}") })], p), "a tool's output");
        assert!(!referenced_by_agent(&[json!({ "role": "user", "text": p })], p), "the human's words");
        assert!(!referenced_by_agent(&[json!({ "role": "agent", "kind": "peer_message", "text": p })], p));
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
    fn remote_paths_keep_the_remote_home_and_folder() {
        assert_eq!(remote_path_expr("~/x/y.png", None).unwrap(), "\"$HOME\"/'x/y.png'");
        assert_eq!(remote_path_expr("/srv/a b.png", None).unwrap(), "'/srv/a b.png'");
        assert_eq!(remote_path_expr("img/a.png", Some("/srv/app")).unwrap(), "'/srv/app'/'img/a.png'");
        assert!(remote_path_expr("img/a.png", None).is_none());
        assert_eq!(remote_path_expr("it's.png", Some("/s")).unwrap(), "'/s'/'it'\\''s.png'");
    }
}
