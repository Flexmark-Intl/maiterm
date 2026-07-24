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
    //
    // Line 2 snapshots the session's remote Claude task board (~/.claude/tasks/<sid>/*.json)
    // as one base64 line — exactly one line even when the dir is absent (the trailing `echo`),
    // so the newline framing stays: line 1 wc, line 2 board, remainder tail delta. `find` (not
    // a glob — zsh's nomatch would abort the whole command line) + base64 are POSIX-universal;
    // a host missing base64 just yields an empty line ⇒ no board. Session ids are UUIDs, so
    // the unquoted (tilde-expanding) dir path is injection-safe.
    let quoted = format!("'{}'", transcript_path.replace('\'', "'\\''"));
    let tasks_dir = format!("~/.claude/tasks/{session_id}");
    let script = format!(
        "wc -c < {quoted}\n[ -d {tasks_dir} ] && find {tasks_dir} -maxdepth 1 -name '*.json' -exec cat {{}} + 2>/dev/null | base64 | tr -d '\\n'; echo\ntail -c +{} {quoted}",
        offset + 1
    );

    // Mux over the bridge tunnel's master — never become one (a dead socket falls
    // through to a plain BatchMode connection, which key-auth hosts still satisfy).
    let mut cmd_args = crate::commands::ssh_tunnel::mux_client_args(host_key);
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

    let Some((remote_size, tasks_b64, delta)) = split_fetch_output(&output.stdout) else {
        log::debug!("transcript mirror: unparseable fetch output for {}", session_id_short(session_id));
        return false;
    };

    // Board shadow first — independent of the transcript delta (a subagent can rewrite board
    // files without the main transcript growing). Never fails the fetch.
    update_tasks_shadow(session_id, tasks_b64);

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

/// Split the fetch script's stdout into (remote file size, base64 task-board line, transcript
/// delta bytes). Line 1 is `wc -c` output (whitespace-padded on some hosts); line 2 is the
/// board dump (empty ⇒ no board); everything after the second newline is the raw `tail`
/// payload, which may itself contain newlines.
fn split_fetch_output(stdout: &[u8]) -> Option<(u64, &[u8], &[u8])> {
    let nl1 = stdout.iter().position(|&b| b == b'\n')?;
    let size = String::from_utf8_lossy(&stdout[..nl1]).trim().parse::<u64>().ok()?;
    let rest = &stdout[nl1 + 1..];
    let nl2 = rest.iter().position(|&b| b == b'\n')?;
    Some((size, &rest[..nl2], &rest[nl2 + 1..]))
}

/// Materialize the remote board dump into the local shadow (`tasks::shadow_path`) that
/// `tasks_for_session` falls back to for sessions with no local board. Content-compared before
/// writing — an unchanged board must not bump the shadow's mtime, or the WS change key would
/// fire a spurious `tasks` event per fetch. Empty/absent board ⇒ shadow removed (the WS diff
/// then emits one clearing `[]`, matching local semantics).
fn update_tasks_shadow(session_id: &str, tasks_b64: &[u8]) {
    use base64::Engine as _;
    let Some(path) = super::tasks::shadow_path(session_id) else { return };
    let board = base64::engine::general_purpose::STANDARD
        .decode(tasks_b64)
        .map(|bytes| super::tasks::parse_concatenated(&bytes))
        .unwrap_or_default();
    if board.is_empty() {
        let _ = std::fs::remove_file(&path);
        return;
    }
    let Ok(json) = serde_json::to_vec(&board) else { return };
    if std::fs::read(&path).is_ok_and(|prev| prev == json) {
        return;
    }
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Err(e) = std::fs::write(&path, &json) {
        log::warn!("transcript mirror: task shadow write failed for {}: {}", session_id_short(session_id), e);
    }
}

#[cfg(test)]
mod tests {
    use super::split_fetch_output;

    #[test]
    fn splits_wc_board_and_delta() {
        // BSD wc pads with spaces; the delta itself contains newlines.
        let out = b"   1234\nQkFTRTY0\n{\"a\":1}\n{\"b\":2}\n";
        let (size, board, delta) = split_fetch_output(out).expect("parses");
        assert_eq!(size, 1234);
        assert_eq!(board, b"QkFTRTY0");
        assert_eq!(delta, b"{\"a\":1}\n{\"b\":2}\n");
    }

    #[test]
    fn empty_board_and_empty_delta_are_valid() {
        // No remote board (bare echo line) and an up-to-date shadow (tail emits nothing).
        let (size, board, delta) = split_fetch_output(b"98\n\n").expect("parses");
        assert_eq!(size, 98);
        assert!(board.is_empty());
        assert!(delta.is_empty());
    }

    #[test]
    fn garbage_and_missing_newlines_are_rejected() {
        assert!(split_fetch_output(b"").is_none());
        assert!(split_fetch_output(b"1234").is_none(), "no newline → can't split");
        assert!(split_fetch_output(b"1234\nB64NOEOL").is_none(), "board line must terminate");
        assert!(split_fetch_output(b"wc: no such file\n\n").is_none());
    }
}

/// Delete shadow files whose transcript hasn't grown in [`PRUNE_AFTER_SECS`] — and their
/// sessions' task-board shadows on the same clock. Called once at startup; keeps the shadow
/// dirs from accumulating one file per remote session forever.
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
            // The board shadow is only ever a companion to a mirrored transcript — an
            // orphaned one has no session left to serve, so it goes on the same sweep.
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                if let Some(tasks) = super::tasks::shadow_path(stem) {
                    let _ = std::fs::remove_file(tasks);
                }
            }
        }
    }
    if pruned > 0 {
        log::info!("transcript mirror: pruned {} stale shadow transcript(s)", pruned);
    }
}
