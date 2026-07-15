use std::fs;
use std::path::PathBuf;

/// Helper scripts for the `/maiterm statusline` subcommand, bundled into the
/// binary at build time from the canonical sources in `src-tauri/resources/`.
/// Shared with the remote (SSH) install path via `get_maiterm_skill_scripts`.
pub const STATUSLINE_SETUP_SCRIPT: &str =
    include_str!("../../resources/maiterm-skill/bin/setup-statusline.sh");
pub const STATUSLINE_PAYLOAD_SCRIPT: &str =
    include_str!("../../resources/maiterm-skill/bin/statusline-command.sh");

/// The `/maiterm` skill body (SKILL.md). Single canonical source bundled at build
/// time and shared with the remote (SSH) install via `get_maiterm_skill_scripts`,
/// so the local and remote skill never drift. Trailing `$ARGUMENTS` is the slash-
/// command argument placeholder Claude substitutes.
pub const MAITERM_SKILL_MD: &str =
    include_str!("../../resources/maiterm-skill/SKILL.md");

/// The Codex hook shim. Codex command-hooks run this script, which forwards the hook
/// event (JSON on stdin) to the local MCP server's /hooks?runtime=codex endpoint.
/// Bundled here and installed by CodexRegistrar; shipped to the remote install too.
#[allow(dead_code)]
pub const AGENT_HOOK_SHIM: &str =
    include_str!("../../resources/maiterm-skill/bin/agent-hook.sh");

fn mcp_server_key() -> &'static str {
    crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude)
}

/// The pre-rebrand server key for THIS build flavor. We strip it from
/// `~/.claude.json` whenever we (re)write our own entry, so updating across the
/// aiterm→maiterm rename leaves no dead duplicate server dialing a stale port.
/// Flavor-specific on purpose: prod removes only `aiterm`, dev only `aiterm-dev`,
/// so the two never clobber each other's registration.
fn legacy_mcp_server_key() -> &'static str {
    if cfg!(debug_assertions) { "aiterm-dev" } else { "aiterm" }
}

/// Check if a process is alive by PID.
#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| {
            let out = String::from_utf8_lossy(&o.stdout);
            !out.contains("No tasks") && out.contains(&pid.to_string())
        })
        .unwrap_or(false)
}

/// A lockfile is live when its owning process is running — or, for tunnel
/// lockfiles written into ~/.claude/ide by a *peer* maiTerm's SSH-bridge
/// remote setup (this machine was the ssh target; the lockfile has `pid: 0`
/// because the listener is sshd's reverse-tunnel port, not a local process),
/// when the port still accepts connections. pid 0 must never take the pid
/// path: kill(0, 0) signals our own process group and always succeeds, which
/// made tunnel lockfiles — and their hook entries — immortal after the
/// tunnel died (the 61720 ECONNREFUSED incident).
fn lockfile_is_live(pid: u64, port: u16) -> bool {
    if pid > 0 {
        return is_process_alive(pid as u32);
    }
    port_is_listening(port)
}

fn port_is_listening(port: u16) -> bool {
    use std::net::{SocketAddr, TcpStream};
    if port == 0 {
        return false;
    }
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(250)).is_ok()
}

fn ide_lock_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("ide"))
}

fn claude_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude.json"))
}

/// Path to Claude Code user settings: ~/.claude/settings.json
fn claude_user_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

pub fn write_lockfile(port: u16, auth: &str, workspace_folders: Vec<String>, hooks_enabled: bool) -> Result<(), String> {
    let dir = ide_lock_dir().ok_or("Could not determine home directory")?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create lock dir: {}", e))?;

    let lock_path = dir.join(format!("{}.lock", port));
    let content = serde_json::json!({
        "pid": std::process::id(),
        "workspaceFolders": workspace_folders,
        "ideName": crate::APP_DISPLAY_NAME,
        "ideVersion": crate::APP_VERSION,
        "transport": "ws",
        "authToken": auth,
        "serverPort": port,
    });

    let json = serde_json::to_string_pretty(&content).map_err(|e| e.to_string())?;
    fs::write(&lock_path, json).map_err(|e| format!("Failed to write lock file: {}", e))?;
    log::info!("Wrote Claude Code lock file at {:?}", lock_path);

    // Also register as a named MCP server so Claude exposes our full tool list
    if let Err(e) = write_mcp_settings(port, auth) {
        log::warn!("Failed to write MCP settings: {}", e);
    }

    // Register hooks in ~/.claude/settings.json (gated on preference)
    if hooks_enabled {
        if let Err(e) = write_hook_settings(port, auth) {
            log::warn!("Failed to write hook settings: {}", e);
        }
    }

    // Install /maiterm skill for fast slash-command access
    if let Err(e) = write_aiterm_skill() {
        log::warn!("Failed to write aiterm skill: {}", e);
    }

    Ok(())
}

pub fn delete_lockfile(port: u16, auth: &str) {
    // Clean up hooks first (needs both port and auth)
    if let Err(e) = remove_hook_settings(port, auth) {
        log::warn!("Failed to remove hook settings: {}", e);
    }
    if let Some(dir) = ide_lock_dir() {
        let lock_path = dir.join(format!("{}.lock", port));
        if let Err(e) = fs::remove_file(&lock_path) {
            log::warn!("Failed to delete lock file {:?}: {}", lock_path, e);
        } else {
            log::info!("Deleted Claude Code lock file {:?}", lock_path);
        }
    }

    if let Err(e) = remove_mcp_settings() {
        log::warn!("Failed to remove MCP settings: {}", e);
    }

    remove_aiterm_skill();
}

/// The expected `mcpServers.<key>` value pointing at our live server.
fn expected_mcp_entry(port: u16, auth: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{}/mcp", port),
        "headers": {
            "x-claude-code-ide-authorization": auth
        }
    })
}

/// Atomically set `mcpServers.<key>` to `entry`, preserving the rest of the file.
fn put_mcp_entry(path: &PathBuf, entry: serde_json::Value) -> Result<(), String> {
    let mut settings: serde_json::Value = if path.exists() {
        let raw = fs::read_to_string(path).map_err(|e| format!("Cannot read settings.json: {}", e))?;
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let mcp_servers = settings
        .as_object_mut()
        .ok_or("settings.json is not an object")?
        .entry("mcpServers")
        .or_insert(serde_json::json!({}));

    mcp_servers[mcp_server_key()] = entry;
    // Rebrand migration: drop the legacy aiterm/aiterm-dev key for this flavor so
    // the rename doesn't leave a stale duplicate server behind in ~/.claude.json.
    if let Some(obj) = mcp_servers.as_object_mut() {
        obj.remove(legacy_mcp_server_key());
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    // Atomic write
    let tmp = path.with_extension("json.aiterm-tmp");
    fs::write(&tmp, &json).map_err(|e| format!("Cannot write settings tmp: {}", e))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot update settings.json: {}", e)
    })?;
    Ok(())
}

/// Write an `mcpServers.aiterm` entry into ~/.claude.json so Claude
/// Code CLI exposes our full tool list (not filtered by IDE name).
fn write_mcp_settings(port: u16, auth: &str) -> Result<(), String> {
    let path = claude_settings_path().ok_or("Could not determine home directory")?;
    put_mcp_entry(&path, expected_mcp_entry(port, auth))?;
    log::info!("Registered {} MCP server in ~/.claude.json (port {})", mcp_server_key(), port);
    Ok(())
}

/// Re-assert `mcpServers.<key>` in ~/.claude.json *only if it has drifted* from
/// the live port/transport/token. `~/.claude.json` is co-owned by the `claude`
/// CLI, which rewrites the whole file on its own events — a long-lived session
/// holding a stale in-memory copy can clobber our startup registration, leaving
/// Claude Code dialing a dead port forever (we otherwise only write at startup).
/// Called on a timer so a clobber self-heals within one tick. Returns `Ok(true)`
/// when a repair was written, `Ok(false)` when the entry was already correct.
pub fn ensure_mcp_settings(port: u16, auth: &str) -> Result<bool, String> {
    let path = claude_settings_path().ok_or("Could not determine home directory")?;
    let expected = expected_mcp_entry(port, auth);

    // Cheap read-only check: skip the write (and its disk churn / file-watcher
    // wakeups) when our entry already matches. Object equality is key-order
    // independent, so transport/url/header reordering won't trigger a rewrite.
    if path.exists() {
        if let Ok(raw) = fs::read_to_string(&path) {
            if let Ok(settings) = serde_json::from_str::<serde_json::Value>(&raw) {
                if settings.get("mcpServers").and_then(|m| m.get(mcp_server_key())) == Some(&expected) {
                    return Ok(false);
                }
            }
        }
    }

    put_mcp_entry(&path, expected)?;
    log::info!(
        "Re-asserted {} MCP server in ~/.claude.json (port {}) — entry was missing or had drifted",
        mcp_server_key(),
        port
    );
    Ok(true)
}

/// Remove the MCP server entry from ~/.claude.json on shutdown.
fn remove_mcp_settings() -> Result<(), String> {
    let path = claude_settings_path().ok_or("Could not determine home directory")?;
    if !path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(&path).map_err(|e| format!("Cannot read settings.json: {}", e))?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));

    if let Some(mcp_servers) = settings.get_mut("mcpServers").and_then(|v| v.as_object_mut()) {
        mcp_servers.remove(mcp_server_key());
        // Also strip the legacy aiterm/aiterm-dev key (rebrand migration).
        mcp_servers.remove(legacy_mcp_server_key());
        // Remove the mcpServers key entirely if now empty
        if mcp_servers.is_empty() {
            settings.as_object_mut().unwrap().remove("mcpServers");
        }
    } else {
        return Ok(()); // Nothing to remove
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.aiterm-tmp");
    fs::write(&tmp, &json).map_err(|e| format!("Cannot write settings tmp: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot update settings.json: {}", e)
    })?;

    log::info!("Removed {} MCP server from ~/.claude.json", mcp_server_key());
    Ok(())
}

/// Build the hook URL marker used to identify our hook entries.
fn hook_url_marker(port: u16) -> String {
    format!("http://127.0.0.1:{}/hooks", port)
}

/// Build the hook entries we register, keyed by event name. Shared by
/// `write_hook_settings` (install) and `hooks_are_current` (reassert drift
/// check) so the two can never disagree about what "our hooks" look like.
fn build_our_hooks(port: u16, auth: &str) -> serde_json::Value {
    let hooks_url = hook_url_marker(port);

    // SessionStart command hook: reads session_id from stdin JSON, echoes tab ID + session ID
    // into Claude's context so Claude passes both to initSession.
    // Gate on $MAITERM_PORT matching our port to prevent dev/prod cross-talk.
    let mcp_key = mcp_server_key();
    let session_start_cmd = format!(
        "{{ [ \"$MAITERM_PORT\" = \"{port}\" ] || [ -z \"$MAITERM_PORT\" ]; }} && \
         [ -n \"$MAITERM_TAB_ID\" ] && \
         MAITERM_SID=$(cat | sed -n 's/.*\"session_id\" *: *\"\\([^\"]*\\)\".*/\\1/p' | head -1) && \
         echo 'Your maiTerm tab ID is '$MAITERM_TAB_ID'. Your session ID is '$MAITERM_SID'. \
You MUST immediately call the {mcp_key} initSession tool with this tabId and sessionId before responding to the user. You can run it in parallel with your other opening tool calls to save a round-trip, but not alongside other maiterm calls. This is required for correct tab targeting.' || true",
        port = port,
        mcp_key = mcp_key,
    );

    let http_hook = |url: &str| -> serde_json::Value {
        serde_json::json!([{
            "matcher": "",
            "hooks": [{
                "type": "http",
                "url": url,
                "headers": {
                    "x-claude-code-ide-authorization": auth
                }
            }]
        }])
    };

    serde_json::json!({
        "SessionStart": [
            // Command hook: echo tab ID into Claude's context + background curl for tab mapping
            {
                "matcher": "",
                "hooks": [{
                    "type": "command",
                    "command": session_start_cmd,
                    "timeout": 5
                }]
            },
            // HTTP hook: sends full event JSON (session_id, cwd) for session tracking
            {
                "matcher": "",
                "hooks": [{
                    "type": "http",
                    "url": &hooks_url,
                    "headers": {
                        "x-claude-code-ide-authorization": auth
                    }
                }]
            }
        ],
        "SessionEnd": http_hook(&hooks_url),
        "Notification": http_hook(&hooks_url),
        "Stop": http_hook(&hooks_url),
        "UserPromptSubmit": http_hook(&hooks_url),
        "PreToolUse": http_hook(&hooks_url),
        "PostToolUse": http_hook(&hooks_url),
        "PreCompact": http_hook(&hooks_url)
    })
}

/// Stable signature of maiTerm's SessionStart *command* hook — the entry that echoes
/// the tab id and tells the agent to call initSession. This exact phrase appears in
/// BOTH the current (`MAITERM_`) command and the legacy (`AITERM_`) one, and is far
/// too specific to collide with a user hook. Matching on content (not the tunnel port
/// it gates on, nor a URL it doesn't have) is what lets re-install both DEDUP our own
/// command hook and SWEEP a stale pre-rename hook. The pre-rename hook matters: once a
/// new-build shell stops exporting `$AITERM_TAB_ID`, that hook's `[ -z "$AITERM_TAB_ID" ]`
/// fallback fires, sources `~/.aiterm`, and emits a phantom tab id into the agent's
/// SessionStart context — a failed initSession followed by a recovery dance.
const MAITERM_CMD_HOOK_MARKER: &str = "initSession tool with this tabId";

/// True when `entry` is a maiTerm SessionStart command hook — from ANY instance
/// or vintage (ours, a dev/prod sibling's, a peer tunnel's, pre-rename AITERM_).
fn is_maiterm_command_hook(entry: &serde_json::Value) -> bool {
    if let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) {
        for hook in hooks {
            if let Some(cmd) = hook.get("command").and_then(|v| v.as_str()) {
                if cmd.contains(MAITERM_CMD_HOOK_MARKER) {
                    return true;
                }
            }
        }
    }
    false
}

/// True when a SessionStart command hook is OURS to manage — swept before
/// re-adding our single copy on install, and counted by the drift check.
/// Every marker-matching hook qualifies EXCEPT a sibling local instance's:
/// one whose gate port is owned by a live pid > 0 lockfile (dev and prod run
/// simultaneously and share ~/.claude/settings.json — if each swept the
/// other's hook, their 30s self-heals would fight forever, and the loser's
/// sessions would boot with no tab id). Port-scoping alone can't replace this
/// check: a tunnel-gated hook (pid-0 lockfile, or no lockfile) must always be
/// ours to sweep, because a TCP probe can't tell an active peer tunnel from a
/// zombie listener whose tunnel died — exactly how the stale pre-rename hook
/// survived every liveness sweep. A swept ACTIVE peer hook is the accepted
/// trade-off; the peer rewrites it on its next bridge connect.
fn command_hook_is_ours_to_sweep(
    entry: &serde_json::Value,
    our_port: u16,
    local_instance_ports: &[u16],
) -> bool {
    if !is_maiterm_command_hook(entry) {
        return false;
    }
    match extract_hook_port(entry) {
        Some(p) => p == our_port || !local_instance_ports.contains(&p),
        None => true,
    }
}

/// Write Claude Code hooks into ~/.claude/settings.json.
///
/// Registers:
/// - SessionStart (command) — reads $MAITERM_TAB_ID, POSTs to our server, injects tab ID context
/// - SessionEnd, Notification, Stop, UserPromptSubmit, PreToolUse, PostToolUse, PreCompact (http)
///
/// We identify our entries by matching the hook URL, so we don't clobber user hooks.
fn write_hook_settings(port: u16, auth: &str) -> Result<(), String> {
    let path = claude_user_settings_path().ok_or("Could not determine home directory")?;

    let mut settings: serde_json::Value = if path.exists() {
        let raw = fs::read_to_string(&path).map_err(|e| format!("Cannot read settings: {}", e))?;
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
    } else {
        serde_json::json!({})
    };

    let hooks_url = hook_url_marker(port);
    let our_hooks = build_our_hooks(port, auth);

    // Sweep: remove any maiTerm hook entries whose port has no live lockfile.
    // This catches orphans from crashes where cleanup never ran.
    let live_ports = collect_live_lockfile_ports();
    if let Some(hooks_map) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        for (_event, entries) in hooks_map.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|entry| {
                    // Only filter maiTerm hook entries (contain 127.0.0.1:{port}/hooks)
                    match extract_hook_port(entry) {
                        Some(p) => live_ports.contains(&p) || p == port,
                        None => true, // Not an maiTerm hook — keep it
                    }
                });
                if arr.len() != before {
                    log::info!("Swept {} orphaned hook entries from event '{}'", before - arr.len(), _event);
                }
            }
        }
        hooks_map.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }

    // Also sweep stale allowedHttpHookUrls
    if let Some(arr) = settings.get_mut("allowedHttpHookUrls").and_then(|v| v.as_array_mut()) {
        arr.retain(|v| {
            let url = v.as_str().unwrap_or("");
            match extract_port_from_url(url) {
                Some(p) => live_ports.contains(&p) || p == port,
                None => true,
            }
        });
        if arr.is_empty() {
            settings.as_object_mut().unwrap().remove("allowedHttpHookUrls");
        }
    }

    // Merge our hooks with existing user hooks (don't clobber)
    let local_instance_ports = collect_local_instance_ports();
    let hooks_obj = settings
        .as_object_mut()
        .ok_or("settings.json is not an object")?
        .entry("hooks")
        .or_insert(serde_json::json!({}));

    if let Some(hooks_map) = hooks_obj.as_object_mut() {
        if let Some(our_map) = our_hooks.as_object() {
            for (event_name, our_entries) in our_map {
                let event_array = hooks_map
                    .entry(event_name)
                    .or_insert(serde_json::json!([]));

                if let Some(arr) = event_array.as_array_mut() {
                    // Remove existing maiTerm hook entries before re-adding ours: HTTP
                    // hooks matching our URL, PLUS every maiTerm SessionStart *command*
                    // hook that is ours to manage (see command_hook_is_ours_to_sweep —
                    // a live dev/prod sibling's hook is left alone). The command hook
                    // carries no URL, so URL-matching alone let a fresh copy pile up on
                    // every re-assert; matching it by signature also sweeps stale
                    // pre-rename AITERM_ command hooks that gate on a zombie tunnel
                    // port the liveness sweep can't catch (and that source ~/.aiterm to
                    // emit a phantom tab id into the agent's context).
                    arr.retain(|entry| {
                        !entry_matches_url(entry, &hooks_url)
                            && !command_hook_is_ours_to_sweep(entry, port, &local_instance_ports)
                    });
                    // Add our entries
                    if let Some(our_arr) = our_entries.as_array() {
                        arr.extend(our_arr.iter().cloned());
                    }
                }
            }
        }
    }

    // Also register our hooks URL in allowedHttpHookUrls
    let allowed = settings
        .as_object_mut()
        .unwrap()
        .entry("allowedHttpHookUrls")
        .or_insert(serde_json::json!([]));
    if let Some(arr) = allowed.as_array_mut() {
        let pattern = format!("http://127.0.0.1:{}/*", port);
        if !arr.iter().any(|v| v.as_str() == Some(&pattern)) {
            arr.push(serde_json::Value::String(pattern));
        }
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.aiterm-tmp");
    fs::write(&tmp, &json).map_err(|e| format!("Cannot write settings tmp: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot update settings: {}", e)
    })?;

    log::info!("Registered Claude Code hooks in ~/.claude/settings.json (port {})", port);
    Ok(())
}

/// True when every hook entry we'd write for (port, auth) is already present
/// in ~/.claude/settings.json and our URL pattern is in allowedHttpHookUrls.
/// serde_json object equality is key-order independent, so a CLI rewrite that
/// only reorders keys won't read as drift.
fn hooks_are_current(path: &std::path::Path, port: u16, auth: &str) -> bool {
    let Ok(raw) = fs::read_to_string(path) else { return false };
    let Ok(settings) = serde_json::from_str::<serde_json::Value>(&raw) else { return false };

    let expected = build_our_hooks(port, auth);
    let Some(hooks) = settings.get("hooks").and_then(|v| v.as_object()) else { return false };
    let all_present = expected.as_object().unwrap().iter().all(|(event, ours)| {
        let Some(arr) = hooks.get(event).and_then(|v| v.as_array()) else { return false };
        ours.as_array().unwrap().iter().all(|entry| arr.contains(entry))
    });
    if !all_present {
        return false;
    }

    // Exactly one SessionStart command hook that we manage may exist. Two or more
    // means duplicates piled up (the merge historically deduped only URL-bearing HTTP
    // hooks, never the command hook) or a stale pre-rename AITERM_ command hook still
    // lingers. Both are drift the write path must sweep — left in place, the stale hook
    // sources ~/.aiterm and feeds a phantom tab id into the agent's SessionStart
    // context. A live dev/prod sibling's hook is neither counted nor swept — see
    // command_hook_is_ours_to_sweep. Mirror the write path's rule exactly, or the
    // self-heal loops forever on a state the write can't change.
    let session_start_cmd_hooks: Vec<&serde_json::Value> = hooks
        .get("SessionStart")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter(|e| is_maiterm_command_hook(e)).collect())
        .unwrap_or_default();
    // Reading the lockfile dir every 30s is avoidable in the common steady state:
    // only foreign-gated command hooks need the sibling-instance lookup.
    let local_instance_ports = if session_start_cmd_hooks
        .iter()
        .any(|e| extract_hook_port(e) != Some(port))
    {
        collect_local_instance_ports()
    } else {
        Vec::new()
    };
    let cmd_hook_count = session_start_cmd_hooks
        .iter()
        .filter(|e| command_hook_is_ours_to_sweep(e, port, &local_instance_ports))
        .count();
    if cmd_hook_count != 1 {
        return false;
    }

    // Claude refuses HTTP hooks whose URL isn't allowlisted, so a missing
    // pattern is drift even when the hook entries themselves survived.
    let pattern = format!("http://127.0.0.1:{}/*", port);
    let allowed = settings
        .get("allowedHttpHookUrls")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().any(|v| v.as_str() == Some(&pattern)))
        .unwrap_or(false);
    if !allowed {
        return false;
    }

    // Stale foreign maiTerm entries are drift too. A deploy overlap leaves the
    // old instance's hooks behind (its lockfile was still live when we swept at
    // startup, then it died without clean exit) — every hook then also dials
    // the dead port, and every session shows ECONNREFUSED noise. The sweep that
    // removes them lives in write_hook_settings, which only runs on drift, so
    // stale ports must *count* as drift.
    let mut foreign_ports: Vec<u16> = Vec::new();
    for entries in hooks.values() {
        if let Some(arr) = entries.as_array() {
            for entry in arr {
                if let Some(p) = extract_hook_port(entry) {
                    if p != port && !foreign_ports.contains(&p) {
                        foreign_ports.push(p);
                    }
                }
            }
        }
    }
    if let Some(arr) = settings.get("allowedHttpHookUrls").and_then(|v| v.as_array()) {
        for v in arr {
            if let Some(p) = v.as_str().and_then(extract_port_from_url) {
                if p != port && !foreign_ports.contains(&p) {
                    foreign_ports.push(p);
                }
            }
        }
    }
    if !foreign_ports.is_empty() {
        let live = collect_live_lockfile_ports();
        if foreign_ports.iter().any(|p| !live.contains(p)) {
            return false;
        }
    }

    true
}

/// Re-assert our hooks in ~/.claude/settings.json *only if they have drifted*.
/// Same rationale as `ensure_mcp_settings`: settings.json is co-owned (the
/// `claude` CLI rewrites it, and an SSH-bridge setup script that lands in a
/// local shell clobbers it with remote-tunnel ports) — without this, every
/// hook dials a dead port until the next app restart, killing session
/// tracking, Claude state indicators, and notifications. Called on the same
/// timer as the MCP re-assert. Returns `Ok(true)` when a repair was written.
pub fn ensure_hook_settings(port: u16, auth: &str) -> Result<bool, String> {
    let path = claude_user_settings_path().ok_or("Could not determine home directory")?;

    // Cheap read-only check: skip the write (and its disk churn) when our
    // entries are already present and correct.
    if hooks_are_current(&path, port, auth) {
        return Ok(false);
    }

    write_hook_settings(port, auth)?;
    log::info!(
        "Re-asserted Claude Code hooks in ~/.claude/settings.json (port {}) — entries were missing or had drifted",
        port
    );
    Ok(true)
}

/// Collect ports from all live lockfiles (PIDs that are still running).
fn collect_live_lockfile_ports() -> Vec<u16> {
    let Some(dir) = ide_lock_dir() else { return vec![] };
    let Ok(entries) = fs::read_dir(&dir) else { return vec![] };
    let mut ports = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else { continue };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&contents) else { continue };

        let pid = data.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
        if let Some(port) = data.get("serverPort").and_then(|v| v.as_u64()) {
            if lockfile_is_live(pid, port as u16) {
                ports.push(port as u16);
            }
        }
    }
    ports
}

/// Collect ports of live *local maiTerm instances*: lockfiles with pid > 0 whose
/// process is running. Unlike `collect_live_lockfile_ports`, this deliberately
/// excludes pid-0 tunnel lockfiles — their port-probe liveness can't tell an
/// active peer tunnel from a zombie listener, which is exactly how a stale
/// tunnel-gated command hook once survived every sweep. Used only to recognize
/// a concurrently-running dev/prod sibling's SessionStart command hook, where
/// the process check is reliable.
fn collect_local_instance_ports() -> Vec<u16> {
    let Some(dir) = ide_lock_dir() else { return vec![] };
    let Ok(entries) = fs::read_dir(&dir) else { return vec![] };
    let mut ports = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }
        let Ok(contents) = fs::read_to_string(&path) else { continue };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&contents) else { continue };

        let pid = data.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
        if pid == 0 {
            continue;
        }
        if let Some(port) = data.get("serverPort").and_then(|v| v.as_u64()) {
            if is_process_alive(pid as u32) {
                ports.push(port as u16);
            }
        }
    }
    ports
}

/// Extract the port number from an maiTerm hook entry's URL/command.
fn extract_hook_port(entry: &serde_json::Value) -> Option<u16> {
    if let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) {
        for hook in hooks {
            // HTTP hooks
            if let Some(url) = hook.get("url").and_then(|v| v.as_str()) {
                if let Some(port) = extract_port_from_url(url) {
                    return Some(port);
                }
            }
            // Command hooks — look for our URL or the MAITERM_PORT/legacy AITERM_PORT pattern
            if let Some(cmd) = hook.get("command").and_then(|v| v.as_str()) {
                if let Some(port) = extract_port_from_url(cmd) {
                    return Some(port);
                }
                // Also match the MAITERM_PORT = "NNNNN" pattern in command hooks
                // (and legacy AITERM_PORT — see extract_port_from_aiterm_var).
                if let Some(port) = extract_port_from_aiterm_var(cmd) {
                    return Some(port);
                }
            }
        }
    }
    None
}

/// Extract port from a `MAITERM_PORT = "12345"` (or `MAITERM_PORT" = "12345"`) pattern in a
/// command hook. The marker is the legacy `"AITERM_PORT"` substring, which also appears inside
/// `"MAITERM_PORT"` — so a single match recognizes BOTH our current hooks and any old ones left
/// on already-provisioned remotes, which is what lets hook re-install replace them.
fn extract_port_from_aiterm_var(s: &str) -> Option<u16> {
    // "AITERM_PORT" is a substring of "MAITERM_PORT", so this matches new and legacy alike.
    let marker = "AITERM_PORT";
    let idx = s.find(marker)? + marker.len();
    let rest = &s[idx..];
    // Skip past quotes, spaces, equals, backslashes
    let digits_start = rest.find(|c: char| c.is_ascii_digit())?;
    let rest = &rest[digits_start..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    if end == 0 { return None; }
    rest[..end].parse::<u16>().ok()
}

/// Extract port from a string like "http://127.0.0.1:12345/hooks" or "...127.0.0.1:12345/*"
fn extract_port_from_url(s: &str) -> Option<u16> {
    let marker = "127.0.0.1:";
    let idx = s.find(marker)? + marker.len();
    let rest = &s[idx..];
    let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    rest[..end].parse::<u16>().ok()
}

/// Check if a hook entry contains a URL matching our server.
fn entry_matches_url(entry: &serde_json::Value, url_pattern: &str) -> bool {
    if let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) {
        for hook in hooks {
            // HTTP hooks: check url field
            if let Some(url) = hook.get("url").and_then(|v| v.as_str()) {
                if url.starts_with(url_pattern) {
                    return true;
                }
            }
            // Command hooks: check command field for our URL
            if let Some(cmd) = hook.get("command").and_then(|v| v.as_str()) {
                if cmd.contains(url_pattern) {
                    return true;
                }
            }
        }
    }
    false
}

/// Remove our hook entries from ~/.claude/settings.json on shutdown.
fn remove_hook_settings(port: u16, auth: &str) -> Result<(), String> {
    let path = claude_user_settings_path().ok_or("Could not determine home directory")?;
    if !path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(&path).map_err(|e| format!("Cannot read settings: {}", e))?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));

    let mut changed = false;

    // Remove hook entries that contain our auth token (identifies our server instance)
    if let Some(hooks_map) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        for (_event, entries) in hooks_map.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|entry| !entry_contains_auth(entry, auth));
                if arr.len() != before {
                    changed = true;
                }
            }
        }
        // Clean up empty event arrays
        hooks_map.retain(|_, v| {
            v.as_array().map(|a| !a.is_empty()).unwrap_or(true)
        });
    }

    // Clean up empty hooks object
    if let Some(hooks) = settings.get("hooks").and_then(|v| v.as_object()) {
        if hooks.is_empty() {
            settings.as_object_mut().unwrap().remove("hooks");
            changed = true;
        }
    }

    // Clean up our port-specific allowedHttpHookUrls entry
    let our_url_pattern = format!("http://127.0.0.1:{}/*", port);
    if let Some(arr) = settings.get_mut("allowedHttpHookUrls").and_then(|v| v.as_array_mut()) {
        let before = arr.len();
        arr.retain(|v| v.as_str() != Some(&our_url_pattern));
        if arr.len() != before {
            changed = true;
        }
        if arr.is_empty() {
            settings.as_object_mut().unwrap().remove("allowedHttpHookUrls");
        }
    }

    if !changed {
        return Ok(());
    }

    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.aiterm-tmp");
    fs::write(&tmp, &json).map_err(|e| format!("Cannot write settings tmp: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot update settings: {}", e)
    })?;

    log::info!("Removed Claude Code hooks from ~/.claude/settings.json");
    Ok(())
}

/// Check if a hook entry contains a specific auth token (used for cleanup).
fn entry_contains_auth(entry: &serde_json::Value, auth: &str) -> bool {
    if let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) {
        for hook in hooks {
            if let Some(headers) = hook.get("headers").and_then(|v| v.as_object()) {
                if let Some(h) = headers.get("x-claude-code-ide-authorization").and_then(|v| v.as_str()) {
                    if h == auth {
                        return true;
                    }
                }
            }
            if let Some(cmd) = hook.get("command").and_then(|v| v.as_str()) {
                if cmd.contains(auth) {
                    return true;
                }
            }
        }
    }
    false
}

/// Directory for the /maiterm skill: ~/.claude/skills/maiterm/
fn maiterm_skill_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("skills").join("maiterm"))
}

/// Legacy pre-rebrand skill dir (~/.claude/skills/aiterm/). Removed on
/// startup and exit so users don't keep a stale duplicate /aiterm command.
fn legacy_aiterm_skill_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("skills").join("aiterm"))
}

/// Remove the legacy /aiterm skill dir if present (rebrand migration).
fn remove_legacy_aiterm_skill() {
    if let Some(dir) = legacy_aiterm_skill_dir() {
        if dir.exists() {
            if let Err(e) = fs::remove_dir_all(&dir) {
                log::warn!("Failed to remove legacy aiterm skill dir: {}", e);
            } else {
                log::info!("Removed legacy /aiterm skill");
            }
        }
    }
}

/// Install the /maiterm slash command skill globally.
/// This gives all Claude Code sessions fast access to maiTerm operations
/// without LLM reasoning about which tool to use.
fn write_aiterm_skill() -> Result<(), String> {
    // Rebrand migration: drop the old /aiterm skill so it doesn't shadow /maiterm.
    remove_legacy_aiterm_skill();
    let dir = maiterm_skill_dir().ok_or("Could not determine home directory")?;
    fs::create_dir_all(&dir).map_err(|e| format!("Failed to create skill dir: {}", e))?;

    let skill = MAITERM_SKILL_MD;

    let path = dir.join("SKILL.md");
    fs::write(&path, skill).map_err(|e| format!("Failed to write skill: {}", e))?;
    log::info!("Wrote /maiterm skill at {:?}", path);

    // Bundle the helper scripts for `/maiterm statusline` into bin/.
    let bin_dir = dir.join("bin");
    fs::create_dir_all(&bin_dir).map_err(|e| format!("Failed to create skill bin dir: {}", e))?;
    write_executable(
        &bin_dir.join("setup-statusline.sh"),
        STATUSLINE_SETUP_SCRIPT,
    )?;
    write_executable(
        &bin_dir.join("statusline-command.sh"),
        STATUSLINE_PAYLOAD_SCRIPT,
    )?;

    Ok(())
}

/// Write a file and mark it executable (no-op chmod on non-unix).
fn write_executable(path: &std::path::Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("Failed to write {:?}: {}", path, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Failed to chmod {:?}: {}", path, e))?;
    }
    Ok(())
}

/// Remove the /maiterm skill directory (and any legacy /aiterm one).
fn remove_aiterm_skill() {
    remove_legacy_aiterm_skill();
    if let Some(dir) = maiterm_skill_dir() {
        if dir.exists() {
            if let Err(e) = fs::remove_dir_all(&dir) {
                log::warn!("Failed to remove maiterm skill dir: {}", e);
            } else {
                log::info!("Removed /maiterm skill");
            }
        }
    }
}

pub fn cleanup_stale_lockfiles() {
    let Some(dir) = ide_lock_dir() else { return };
    let Ok(entries) = fs::read_dir(&dir) else { return };

    let mut stale_ports: Vec<(u16, String)> = Vec::new(); // (port, auth) for dead servers

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("lock") {
            continue;
        }

        let Ok(contents) = fs::read_to_string(&path) else { continue };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&contents) else {
            // Invalid JSON — remove it
            let _ = fs::remove_file(&path);
            continue;
        };

        let pid = data.get("pid").and_then(|v| v.as_u64()).unwrap_or(0);
        let port = data.get("serverPort").and_then(|v| v.as_u64()).unwrap_or(0) as u16;
        if !lockfile_is_live(pid, port) {
            // Collect port/auth before deleting so we can clean their hooks
            let auth = data.get("authToken").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if port > 0 && !auth.is_empty() {
                stale_ports.push((port, auth));
            }
            log::info!(
                "Removing stale lock file {:?} ({})",
                path,
                if pid > 0 { format!("pid {} dead", pid) } else { format!("tunnel port {} not listening", port) }
            );
            let _ = fs::remove_file(&path);
        }
    }

    // Clean up hooks and allowedHttpHookUrls for all dead servers
    if !stale_ports.is_empty() {
        if let Err(e) = cleanup_stale_hooks(&stale_ports) {
            log::warn!("Failed to clean stale hooks: {}", e);
        }
    }
}

/// Remove hook entries and allowedHttpHookUrls for a list of dead (port, auth) pairs.
fn cleanup_stale_hooks(stale_ports: &[(u16, String)]) -> Result<(), String> {
    let path = claude_user_settings_path().ok_or("Could not determine home directory")?;
    if !path.exists() {
        return Ok(());
    }

    let raw = fs::read_to_string(&path).map_err(|e| format!("Cannot read settings: {}", e))?;
    let mut settings: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
    let mut changed = false;

    // Remove hook entries by auth token
    if let Some(hooks_map) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) {
        for (_event, entries) in hooks_map.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                let before = arr.len();
                arr.retain(|entry| {
                    !stale_ports.iter().any(|(_, auth)| entry_contains_auth(entry, auth))
                });
                if arr.len() != before {
                    changed = true;
                }
            }
        }
        hooks_map.retain(|_, v| v.as_array().map(|a| !a.is_empty()).unwrap_or(true));
    }

    // Clean up empty hooks object
    if settings.get("hooks").and_then(|v| v.as_object()).map(|h| h.is_empty()).unwrap_or(false) {
        settings.as_object_mut().unwrap().remove("hooks");
        changed = true;
    }

    // Remove allowedHttpHookUrls entries for dead ports
    if let Some(arr) = settings.get_mut("allowedHttpHookUrls").and_then(|v| v.as_array_mut()) {
        let before = arr.len();
        arr.retain(|v| {
            let url = v.as_str().unwrap_or("");
            !stale_ports.iter().any(|(port, _)| url == format!("http://127.0.0.1:{}/*", port))
        });
        if arr.len() != before {
            changed = true;
        }
        if arr.is_empty() {
            settings.as_object_mut().unwrap().remove("allowedHttpHookUrls");
        }
    }

    if !changed {
        return Ok(());
    }

    let removed_count: usize = stale_ports.len();
    let json = serde_json::to_string_pretty(&settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.aiterm-tmp");
    fs::write(&tmp, &json).map_err(|e| format!("Cannot write settings tmp: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot update settings: {}", e)
    })?;

    log::info!("Cleaned {} stale hook entries from ~/.claude/settings.json", removed_count);
    Ok(())
}

#[cfg(test)]
mod command_hook_sweep_tests {
    use super::*;

    fn cmd_hook(gate: &str) -> serde_json::Value {
        serde_json::json!({
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": format!(
                    "{{ [ \"$MAITERM_PORT\" = \"{gate}\" ] || [ -z \"$MAITERM_PORT\" ]; }} && \
                     echo 'call the maiterm initSession tool with this tabId and sessionId'"
                ),
                "timeout": 5
            }]
        })
    }

    fn http_hook(port: u16) -> serde_json::Value {
        serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "http", "url": format!("http://127.0.0.1:{port}/hooks") }]
        })
    }

    #[test]
    fn our_own_hook_is_swept_even_when_our_lockfile_is_live() {
        // Dedup depends on this: our port may be in local_instance_ports.
        assert!(command_hook_is_ours_to_sweep(&cmd_hook("56819"), 56819, &[56819]));
    }

    #[test]
    fn live_sibling_instance_hook_is_kept() {
        // Dev + prod share ~/.claude/settings.json — never sweep the other's hook.
        assert!(!command_hook_is_ours_to_sweep(&cmd_hook("40001"), 56819, &[40001]));
    }

    #[test]
    fn tunnel_or_dead_gated_hook_is_swept() {
        // Foreign port with no pid>0 lockfile: zombie tunnel or dead instance.
        assert!(command_hook_is_ours_to_sweep(&cmd_hook("59184"), 56819, &[]));
        assert!(command_hook_is_ours_to_sweep(&cmd_hook("59184"), 56819, &[40001]));
    }

    #[test]
    fn legacy_aiterm_hook_is_swept() {
        // Pre-rename hooks gate on AITERM_PORT; extract_hook_port still finds it.
        let legacy = serde_json::json!({
            "matcher": "",
            "hooks": [{
                "type": "command",
                "command": "{ [ -z \"$AITERM_TAB_ID\" ] && [ -f ~/.aiterm ] && . ~/.aiterm; } 2>/dev/null; \
                            { [ \"$AITERM_PORT\" = \"59184\" ] || [ -z \"$AITERM_PORT\" ]; } && \
                            echo 'call the aiterm initSession tool with this tabId and sessionId'"
            }]
        });
        assert!(command_hook_is_ours_to_sweep(&legacy, 56819, &[]));
    }

    #[test]
    fn marker_hook_without_extractable_port_is_swept() {
        let portless = serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": "echo 'initSession tool with this tabId'" }]
        });
        assert!(command_hook_is_ours_to_sweep(&portless, 56819, &[40001]));
    }

    #[test]
    fn non_maiterm_entries_are_never_swept() {
        // User command hooks and our own HTTP hooks don't match the signature.
        let user_hook = serde_json::json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": "echo hello" }]
        });
        assert!(!command_hook_is_ours_to_sweep(&user_hook, 56819, &[]));
        assert!(!command_hook_is_ours_to_sweep(&http_hook(56819), 56819, &[]));
    }
}
