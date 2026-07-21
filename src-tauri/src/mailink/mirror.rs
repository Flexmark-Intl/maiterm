//! SSH transcript mirror — real per-turn chat for SSH Claude tabs (maiLink v2).
//!
//! An SSH tab's Claude session writes its JSONL on the REMOTE host, so the local transcript
//! distiller can't see it and maiLink falls back to a PTY scrape. This module mirrors the
//! remote file into a local *shadow file* and changes nothing downstream: `locate_jsonl`
//! searches [`shadow_dir`] alongside `~/.claude/projects`, so the distiller, the WS message
//! streamer's mtime gate, the context gauge, and last-turn recency all light up unmodified.
//!
//! How the pieces line up:
//! * **What to fetch** — every Claude hook payload carries `transcript_path` verbatim;
//!   `hooks_handler` captures it onto the session and calls [`schedule_fetch`] per event
//!   (a hook event IS the "something appended" signal). While a phone WS is connected,
//!   `ws_event_loop` also calls [`refresh_tabs`] on a slow tick to cover appends between
//!   hook events (long assistant turns emit no hooks until Stop).
//! * **How to fetch** — `tail -c +<offset+1>` over ssh, mux'd through the bridge tunnel's
//!   maiTerm-owned ControlMaster socket (`cm_socket_path`): no re-auth, tens of ms. The
//!   tunnel is alive exactly when remote hooks flow, so a working mirror and a working
//!   trigger have the same lifetime. Socket dead → BatchMode direct attempt → on failure a
//!   short backoff and the tab simply stays on the snapshot fallback (today's floor).
//! * **Offset tracking** — the shadow file's byte length IS the offset (crash-safe, no
//!   sidecar state). Each fetch also gets the remote size (`wc -c`) in the same round trip;
//!   remote-shorter-than-shadow means the file was replaced → shadow resets and refetches.
//!
//! Scope: Claude runtime only (Codex/Gemini keep the snapshot), gated on the maiLink
//! listener running — no phone bridge, no ssh traffic.

use std::path::PathBuf;
use std::sync::Arc;

use crate::state::app_state::RemoteMirrorEntry;
use crate::state::AppState;

/// Where shadow files live: `<data_dir>/<slug>/remote-transcripts/<session_id>.jsonl`.
/// Session ids are globally unique, so files are flat and unambiguous.
pub fn shadow_dir() -> Option<PathBuf> {
    Some(dirs::data_dir()?.join(crate::state::persistence::app_data_slug()).join("remote-transcripts"))
}

/// Backoff after a failed fetch. Long enough not to hammer a dead tunnel from the hook
/// stream, short enough that a recovered bridge resumes mirroring within a turn or two.
const FAILURE_BACKOFF_MS: u64 = 30_000;

/// Shadow files whose sessions haven't appended in this long are pruned at startup.
const PRUNE_AFTER_SECS: u64 = 30 * 24 * 3600;

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Fetch the remote transcript delta for one session, coalesced: one ssh in flight per
/// session, events landing mid-fetch fold into a single follow-up pass. No-ops unless the
/// maiLink listener is running AND the tab rides a live SSH bridge tunnel (local tabs and
/// bridge-down tabs fall through to the existing snapshot path untouched).
pub fn schedule_fetch(app: &Arc<AppState>, tab_id: &str, session_id: &str, transcript_path: &str) {
    if app.mailink_info.read().is_none() {
        return;
    }
    // The tab's tunnel gives the ssh destination; its absence is the "not an SSH tab" gate.
    let Some((host_key, ssh_args)) = ({
        let tunnels = app.ssh_tunnels.read();
        tunnels
            .values()
            .find(|t| t.tab_ids.contains(tab_id))
            .map(|t| (t.host_key.clone(), t.ssh_args.clone()))
    }) else {
        return;
    };

    {
        let mut mirrors = app.remote_mirrors.write();
        let entry = mirrors.entry(session_id.to_string()).or_insert_with(RemoteMirrorEntry::default);
        if now_ms() < entry.backoff_until_ms {
            return;
        }
        if entry.in_flight {
            entry.dirty = true;
            return;
        }
        entry.in_flight = true;
    }

    let app = app.clone();
    let session_id = session_id.to_string();
    let transcript_path = transcript_path.to_string();
    tauri::async_runtime::spawn(async move {
        loop {
            let ok = fetch_once(&host_key, &ssh_args, &session_id, &transcript_path).await;
            let mut mirrors = app.remote_mirrors.write();
            let entry = mirrors.entry(session_id.clone()).or_insert_with(RemoteMirrorEntry::default);
            if !ok {
                entry.backoff_until_ms = now_ms() + FAILURE_BACKOFF_MS;
                entry.in_flight = false;
                entry.dirty = false;
                return;
            }
            if entry.dirty {
                entry.dirty = false;
                continue; // another hook landed mid-fetch — pull its delta too
            }
            entry.in_flight = false;
            return;
        }
    });
}

/// Schedule a fetch for every given tab that has a live Claude session with a known
/// transcript path. Called from the maiLink WS loop on a slow tick (phone connected) so
/// appends between hook events still stream.
pub fn refresh_tabs(app: &Arc<AppState>, tab_ids: &[String]) {
    let targets: Vec<(String, String, String)> = {
        let sessions = app.agent_sessions.read();
        tab_ids
            .iter()
            .filter_map(|tab| {
                sessions
                    .iter()
                    .find(|(_, s)| {
                        s.tab_id == *tab
                            && s.runtime == crate::state::AgentRuntime::Claude
                            && s.transcript_path.is_some()
                    })
                    .map(|(sid, s)| (tab.clone(), sid.clone(), s.transcript_path.clone().unwrap()))
            })
            .collect()
    };
    for (tab, sid, path) in targets {
        schedule_fetch(app, &tab, &sid, &path);
    }
}

/// One ssh round trip: remote size + the byte delta past our shadow length, appended to the
/// shadow file. Returns false on any failure (caller applies backoff).
async fn fetch_once(host_key: &str, ssh_args: &str, session_id: &str, transcript_path: &str) -> bool {
    let Some(dir) = shadow_dir() else { return false };
    let shadow = dir.join(format!("{session_id}.jsonl"));
    let offset = std::fs::metadata(&shadow).map(|m| m.len()).unwrap_or(0);

    // POSIX-quote the remote path; `tail -c +N` is 1-based. `wc -c` rides the same round
    // trip so a replaced/shorter remote file is detected instead of stalling the mirror.
    let quoted = format!("'{}'", transcript_path.replace('\'', "'\\''"));
    let script = format!("wc -c < {quoted}\ntail -c +{} {quoted}", offset + 1);

    let mut cmd_args: Vec<String> = Vec::new();
    #[cfg(unix)]
    if let Some(sock) = crate::commands::ssh_tunnel::cm_socket_path(host_key) {
        // Mux over the bridge tunnel's master — never become one (a dead socket falls
        // through to a plain BatchMode connection, which key-auth hosts still satisfy).
        cmd_args.push("-o".into());
        cmd_args.push("ControlMaster=no".into());
        cmd_args.push("-o".into());
        cmd_args.push(format!("ControlPath={}", sock.display()));
    }
    #[cfg(not(unix))]
    let _ = host_key;
    cmd_args.push("-o".into());
    cmd_args.push("BatchMode=yes".into());
    cmd_args.push("-o".into());
    cmd_args.push("ConnectTimeout=5".into());
    cmd_args.push("-T".into());
    for arg in ssh_args.split_whitespace() {
        cmd_args.push(arg.to_string());
    }
    cmd_args.push(script);

    let output = match tokio::time::timeout(
        tokio::time::Duration::from_secs(20),
        tokio::process::Command::new("ssh")
            .args(&cmd_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output(),
    )
    .await
    {
        Ok(Ok(o)) => o,
        Ok(Err(e)) => {
            log::debug!("transcript mirror: ssh spawn failed for {}: {}", session_id_short(session_id), e);
            return false;
        }
        Err(_) => {
            log::debug!("transcript mirror: fetch timed out for {}", session_id_short(session_id));
            return false;
        }
    };
    if !output.status.success() {
        log::debug!(
            "transcript mirror: fetch failed for {} (exit {:?}): {}",
            session_id_short(session_id),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
        return false;
    }

    let Some((remote_size, delta)) = split_size_and_delta(&output.stdout) else {
        log::debug!("transcript mirror: unparseable fetch output for {}", session_id_short(session_id));
        return false;
    };

    if remote_size < offset {
        // Remote file replaced/shorter than our shadow (shouldn't happen for append-only
        // JSONL, but never stall): reset and refetch from zero on the next pass.
        log::info!(
            "transcript mirror: remote transcript shrank for {} ({} < {}), resetting shadow",
            session_id_short(session_id), remote_size, offset
        );
        let _ = std::fs::remove_file(&shadow);
        return true;
    }
    if delta.is_empty() {
        return true;
    }

    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    use std::io::Write;
    let appended = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&shadow)
        .and_then(|mut f| f.write_all(delta));
    match appended {
        Ok(()) => {
            log::debug!(
                "transcript mirror: +{} bytes for {} (shadow now {})",
                delta.len(), session_id_short(session_id), offset + delta.len() as u64
            );
            true
        }
        Err(e) => {
            log::warn!("transcript mirror: shadow append failed for {}: {}", session_id_short(session_id), e);
            false
        }
    }
}

fn session_id_short(sid: &str) -> &str {
    &sid[..sid.len().min(8)]
}

/// Split the fetch script's stdout into (remote file size, transcript delta bytes).
/// First line is `wc -c` output (whitespace-padded on some hosts); everything after the
/// first newline is the raw `tail` payload, which may itself contain newlines.
fn split_size_and_delta(stdout: &[u8]) -> Option<(u64, &[u8])> {
    let nl = stdout.iter().position(|&b| b == b'\n')?;
    let size = String::from_utf8_lossy(&stdout[..nl]).trim().parse::<u64>().ok()?;
    Some((size, &stdout[nl + 1..]))
}

#[cfg(test)]
mod tests {
    use super::split_size_and_delta;

    #[test]
    fn splits_wc_line_from_delta() {
        // BSD wc pads with spaces; the delta itself contains newlines.
        let out = b"   1234\n{\"a\":1}\n{\"b\":2}\n";
        let (size, delta) = split_size_and_delta(out).expect("parses");
        assert_eq!(size, 1234);
        assert_eq!(delta, b"{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn empty_delta_is_valid() {
        // Up-to-date shadow: wc reports the size, tail emits nothing.
        let (size, delta) = split_size_and_delta(b"98\n").expect("parses");
        assert_eq!(size, 98);
        assert!(delta.is_empty());
    }

    #[test]
    fn garbage_and_missing_newline_are_rejected() {
        assert!(split_size_and_delta(b"").is_none());
        assert!(split_size_and_delta(b"1234").is_none(), "no newline → can't split");
        assert!(split_size_and_delta(b"wc: no such file\n").is_none());
    }
}

/// Delete shadow files whose transcript hasn't grown in [`PRUNE_AFTER_SECS`]. Called once at
/// startup; keeps the shadow dir from accumulating one file per remote session forever.
pub fn prune_stale_shadows() {
    let Some(dir) = shadow_dir() else { return };
    let Ok(entries) = std::fs::read_dir(&dir) else { return };
    let now = std::time::SystemTime::now();
    let mut pruned = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|m| now.duration_since(m).ok())
            .is_some_and(|age| age.as_secs() > PRUNE_AFTER_SECS);
        if stale && std::fs::remove_file(&path).is_ok() {
            pruned += 1;
        }
    }
    if pruned > 0 {
        log::info!("transcript mirror: pruned {} stale shadow transcript(s)", pruned);
    }
}
