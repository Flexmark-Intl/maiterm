//! Codex on-disk registration. Mirrors the Claude `lockfile.rs` machinery but for
//! Codex's own config layout under `~/.codex/`:
//!   - `~/.codex/config.toml` — `[mcp_servers.<name>]` with `url` + `bearer_token`
//!     (format-preserving via toml_edit so the user's other keys/comments survive).
//!   - `~/.codex/hooks/agent-hook.sh` — the bundled hook shim (executable).
//!   - `~/.codex/hooks.json` — command hooks for the 7 lifecycle events, merged with
//!     any existing user hooks (never clobbered).
//!   - `~/.codex/prompts/maiterm.md` — a tiny prompt reinforcing the initSession call.
//!
//! Codex does NOT rewrite its own config, so `reassert_if_drifted` is a no-op.
//!
//! NOTE: This registrar is fully implemented but NOT yet wired into `all_registrars()`
//! / `enabled_registrars()` — a later stage flips it live. Hence `#[allow(dead_code)]`.

use std::fs;
use std::path::Path;
use toml_edit::DocumentMut;

use crate::state::{AgentRuntime, Preferences};
use super::lockfile::AGENT_HOOK_SHIM;
use super::registrar::Registrar;

/// The 7 Codex lifecycle events we register a forwarding command hook for.
const CODEX_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "Stop",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "UserPromptSubmit",
    "PreCompact",
];

/// Marker that identifies *our* hook entry inside a (possibly user-populated) event
/// array. Any command-hook whose command contains this substring is maiTerm's.
const SHIM_MARKER: &str = "agent-hook.sh";

#[allow(dead_code)]
pub struct CodexRegistrar;

#[allow(dead_code)]
impl Registrar for CodexRegistrar {
    fn runtime(&self) -> AgentRuntime {
        AgentRuntime::Codex
    }

    fn enabled(&self, prefs: &Preferences) -> bool {
        prefs.codex_ide
    }

    fn install(&self, port: u16, auth: &str, _workspace_folders: &[String], _prefs: &Preferences) {
        let Some(home) = dirs::home_dir() else {
            log::warn!("Codex install: could not determine home directory");
            return;
        };
        let codex_dir = home.join(".codex");
        if let Err(e) = fs::create_dir_all(&codex_dir) {
            log::warn!("Codex install: failed to create {:?}: {}", codex_dir, e);
            return;
        }

        let name = mcp_name();
        let mut wrote_mcp = false;
        let mut wrote_shim = false;
        let mut wrote_hooks = false;
        let mut wrote_prompt = false;

        // 1. MCP server entry in ~/.codex/config.toml (format-preserving).
        let config_path = codex_dir.join("config.toml");
        match read_document(&config_path) {
            Ok(mut doc) => {
                put_codex_mcp_entry(&mut doc, name, port, auth);
                if let Err(e) = atomic_write(&config_path, &doc.to_string()) {
                    log::warn!("Codex install: failed to write {:?}: {}", config_path, e);
                } else {
                    wrote_mcp = true;
                }
            }
            Err(e) => log::warn!("Codex install: failed to read {:?}: {}", config_path, e),
        }

        // 2. Install the hook shim (executable).
        let shim_path = codex_dir.join("hooks").join("agent-hook.sh");
        if let Some(parent) = shim_path.parent() {
            if let Err(e) = fs::create_dir_all(parent) {
                log::warn!("Codex install: failed to create {:?}: {}", parent, e);
            }
        }
        if let Err(e) = write_executable(&shim_path, AGENT_HOOK_SHIM) {
            log::warn!("Codex install: failed to write shim {:?}: {}", shim_path, e);
        } else {
            wrote_shim = true;
        }

        // 3. Merge our hooks into ~/.codex/hooks.json (don't clobber user hooks).
        let hooks_path = codex_dir.join("hooks.json");
        let shim_str = shim_path.to_string_lossy().to_string();
        match read_json(&hooks_path) {
            Ok(existing) => {
                // Local install: no baked port — the shim uses the per-process
                // $MAITERM_PORT (each tab spawned by the owning maiTerm instance).
                let merged = build_hooks_json(existing, &shim_str, auth, None);
                match serde_json::to_string_pretty(&merged) {
                    Ok(json) => {
                        if let Err(e) = atomic_write(&hooks_path, &json) {
                            log::warn!("Codex install: failed to write {:?}: {}", hooks_path, e);
                        } else {
                            wrote_hooks = true;
                        }
                    }
                    Err(e) => log::warn!("Codex install: failed to serialize hooks.json: {}", e),
                }
            }
            Err(e) => log::warn!("Codex install: failed to read {:?}: {}", hooks_path, e),
        }

        // 4. Minimal prompt reinforcing the MCP initSession instruction. Non-critical.
        let prompts_dir = codex_dir.join("prompts");
        if let Err(e) = fs::create_dir_all(&prompts_dir) {
            log::warn!("Codex install: failed to create {:?}: {}", prompts_dir, e);
        } else {
            let prompt_path = prompts_dir.join("maiterm.md");
            if let Err(e) = atomic_write(&prompt_path, &codex_prompt_body(name)) {
                log::warn!("Codex install: failed to write {:?}: {}", prompt_path, e);
            } else {
                wrote_prompt = true;
            }
        }

        log::info!(
            "Codex install (port {}): mcp_servers.{} in config.toml={}, shim={}, hooks.json={}, prompt={}",
            port, name, wrote_mcp, wrote_shim, wrote_hooks, wrote_prompt
        );
    }

    /// Codex never rewrites its own config, so there is nothing to re-assert.
    fn reassert_if_drifted(&self, _port: u16, _auth: &str, _prefs: &Preferences) {}

    fn unregister(&self, _port: u16, _auth: &str) {
        let Some(home) = dirs::home_dir() else {
            log::warn!("Codex unregister: could not determine home directory");
            return;
        };
        let codex_dir = home.join(".codex");
        let name = mcp_name();

        // 1. Remove the [mcp_servers.<name>] table from config.toml (preserve the rest).
        let config_path = codex_dir.join("config.toml");
        if config_path.exists() {
            match read_document(&config_path) {
                Ok(mut doc) => {
                    if remove_codex_mcp_entry(&mut doc, name) {
                        if let Err(e) = atomic_write(&config_path, &doc.to_string()) {
                            log::warn!("Codex unregister: failed to write {:?}: {}", config_path, e);
                        }
                    }
                }
                Err(e) => log::warn!("Codex unregister: failed to read {:?}: {}", config_path, e),
            }
        }

        // 2. Strip ONLY our entries from hooks.json (command contains agent-hook.sh).
        let hooks_path = codex_dir.join("hooks.json");
        if hooks_path.exists() {
            match read_json(&hooks_path) {
                Ok(Some(existing)) => {
                    let cleaned = strip_maiterm_hooks(existing.clone());
                    // Only rewrite if we actually removed something of ours — never
                    // reformat a user's hooks.json that maiTerm never wrote to.
                    if cleaned != existing {
                        match serde_json::to_string_pretty(&cleaned) {
                            Ok(json) => {
                                if let Err(e) = atomic_write(&hooks_path, &json) {
                                    log::warn!("Codex unregister: failed to write {:?}: {}", hooks_path, e);
                                }
                            }
                            Err(e) => log::warn!("Codex unregister: failed to serialize hooks.json: {}", e),
                        }
                    }
                }
                Ok(None) => {}
                Err(e) => log::warn!("Codex unregister: failed to read {:?}: {}", hooks_path, e),
            }
        }

        // 3. Best-effort removal of the shim and prompt.
        let shim_path = codex_dir.join("hooks").join("agent-hook.sh");
        if shim_path.exists() {
            if let Err(e) = fs::remove_file(&shim_path) {
                log::warn!("Codex unregister: failed to remove {:?}: {}", shim_path, e);
            }
        }
        let prompt_path = codex_dir.join("prompts").join("maiterm.md");
        if prompt_path.exists() {
            if let Err(e) = fs::remove_file(&prompt_path) {
                log::warn!("Codex unregister: failed to remove {:?}: {}", prompt_path, e);
            }
        }

        log::info!("Codex unregister: removed mcp_servers.{} + maiTerm hooks/shim/prompt", name);
    }
}

// ---------------------------------------------------------------------------
// Pure, testable generation helpers (the real logic install()/unregister() call)
// ---------------------------------------------------------------------------

/// The MCP server name for this build flavor (`maiterm` / `maiterm-dev`).
#[allow(dead_code)]
fn mcp_name() -> &'static str {
    crate::state::agent_runtime::mcp_server_name(AgentRuntime::Codex)
}

/// Set `[mcp_servers.<name>]` with `url` + `bearer_token`, preserving everything
/// else in the document (format, comments, unrelated tables). Format-preserving via
/// toml_edit: we mutate the existing `DocumentMut` in place.
///
/// toml_edit auto-vivifies missing intermediate tables as INLINE tables
/// (`mcp_servers = { ... }`) when you index-assign through them. We want real
/// `[mcp_servers.<name>]` headers, so we explicitly ensure `mcp_servers` and the
/// per-name child are standard (non-inline) tables before writing the leaf values.
#[allow(dead_code)]
fn put_codex_mcp_entry(doc: &mut DocumentMut, name: &str, port: u16, auth: &str) {
    let servers = doc
        .entry("mcp_servers")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .expect("mcp_servers is a standard table");
    let entry = servers
        .entry(name)
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_mut()
        .expect("mcp_servers.<name> is a standard table");
    entry["url"] = toml_edit::value(format!("http://127.0.0.1:{}/mcp", port));
    // Codex rejects `bearer_token` for streamable_http servers ("bearer_token is not
    // supported for streamable_http"), so pass the auth via http_headers instead — our
    // server's extract_auth accepts the x-maiterm-authorization header (raw token).
    let mut headers = toml_edit::InlineTable::new();
    headers.insert("x-maiterm-authorization", toml_edit::Value::from(auth));
    entry["http_headers"] = toml_edit::value(headers);
    // Drop any stale bearer_token from a previous (rejected) format.
    entry.remove("bearer_token");
}

/// Remove `[mcp_servers.<name>]` from the document. Returns true if anything changed.
/// Leaves an empty `[mcp_servers]` table behind if that was the last entry — harmless.
/// Handles both standard and inline `mcp_servers` table representations.
#[allow(dead_code)]
fn remove_codex_mcp_entry(doc: &mut DocumentMut, name: &str) -> bool {
    let Some(servers) = doc.get_mut("mcp_servers") else {
        return false;
    };
    if let Some(table) = servers.as_table_mut() {
        return table.remove(name).is_some();
    }
    if let Some(inline) = servers.as_inline_table_mut() {
        return inline.remove(name).is_some();
    }
    false
}

/// Build one command-hook entry for a single event. Optionally tagged with a matcher
/// group (SessionStart needs `"matcher": "startup|resume"`).
#[allow(dead_code)]
fn maiterm_hook_entry(command: &str, matcher: Option<&str>) -> serde_json::Value {
    let mut group = serde_json::json!({
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": 5
        }]
    });
    if let Some(m) = matcher {
        group["matcher"] = serde_json::Value::String(m.to_string());
    }
    group
}

/// The exact command maiTerm's hook runs: the absolute shim path + the auth token as $1,
/// and (for the SSH-remote install) the MCP port baked as $2 — the reverse-tunnel port
/// is fixed for the bridge and authoritative even when the live shell lacks $MAITERM_PORT.
/// Local installs pass `None` so the shim uses the per-process env port (unchanged bytes).
#[allow(dead_code)]
fn hook_command(shim_path: &str, auth: &str, port: Option<u16>) -> String {
    match port {
        Some(p) => format!("bash \"{}\" \"{}\" \"{}\"", shim_path, auth, p),
        None => format!("bash \"{}\" \"{}\"", shim_path, auth),
    }
}

/// Merge maiTerm's command hooks into an existing (or absent) hooks.json value.
///
/// Shape produced:
/// ```json
/// { "hooks": { "<Event>": [ { "hooks": [ { "type": "command", "command": "...", "timeout": 5 } ] } ] } }
/// ```
/// MERGE rule, per event:
///   - identify maiTerm's entry by its command containing `agent-hook.sh`;
///   - if one already exists, REPLACE it (so the token updates) — exactly one survives;
///   - otherwise APPEND ours;
///   - leave any NON-maiTerm entries in that event's array untouched.
/// Other top-level keys in hooks.json are preserved.
#[allow(dead_code)]
fn build_hooks_json(
    existing: Option<serde_json::Value>,
    shim_path: &str,
    auth: &str,
    port: Option<u16>,
) -> serde_json::Value {
    // Start from the existing doc (preserve other top-level keys) or a fresh object.
    let mut root = match existing {
        Some(v @ serde_json::Value::Object(_)) => v,
        _ => serde_json::json!({}),
    };

    let command = hook_command(shim_path, auth, port);

    // Ensure root.hooks is an object.
    let root_obj = root.as_object_mut().expect("root is an object");
    let hooks = root_obj
        .entry("hooks")
        .or_insert_with(|| serde_json::json!({}));
    if !hooks.is_object() {
        *hooks = serde_json::json!({});
    }
    let hooks_obj = hooks.as_object_mut().expect("hooks is an object");

    for &event in CODEX_HOOK_EVENTS {
        let matcher = if event == "SessionStart" {
            Some("startup|resume")
        } else {
            None
        };
        let our_entry = maiterm_hook_entry(&command, matcher);

        let arr = hooks_obj
            .entry(event)
            .or_insert_with(|| serde_json::json!([]));
        if !arr.is_array() {
            *arr = serde_json::json!([]);
        }
        let arr = arr.as_array_mut().expect("event value is an array");

        // Replace an existing maiTerm entry in place; else append. Exactly one ours.
        match arr.iter().position(is_maiterm_entry) {
            Some(idx) => arr[idx] = our_entry,
            None => arr.push(our_entry),
        }
    }

    root
}

/// Remove ONLY maiTerm's entries (command contains `agent-hook.sh`) from every event
/// array; drop now-empty event arrays. If no events remain, leaves `{"hooks":{}}`.
/// Preserves other top-level keys.
#[allow(dead_code)]
fn strip_maiterm_hooks(mut root: serde_json::Value) -> serde_json::Value {
    if let Some(hooks) = root.get_mut("hooks").and_then(|h| h.as_object_mut()) {
        // Strip our entries from each event array.
        for (_event, entries) in hooks.iter_mut() {
            if let Some(arr) = entries.as_array_mut() {
                arr.retain(|e| !is_maiterm_entry(e));
            }
        }
        // Drop now-empty event arrays.
        hooks.retain(|_event, entries| {
            entries.as_array().map(|a| !a.is_empty()).unwrap_or(true)
        });
    }
    root
}

/// Is this hook group one of maiTerm's? True if any of its command hooks references
/// the shim (`agent-hook.sh`).
#[allow(dead_code)]
fn is_maiterm_entry(entry: &serde_json::Value) -> bool {
    entry
        .get("hooks")
        .and_then(|h| h.as_array())
        .map(|hooks| {
            hooks.iter().any(|h| {
                h.get("command")
                    .and_then(|c| c.as_str())
                    .map(|c| c.contains(SHIM_MARKER))
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// The minimal Codex prompt body. Short on purpose; reinforces the MCP `instructions`.
#[allow(dead_code)]
fn codex_prompt_body(mcp_name: &str) -> String {
    format!(
        "# maiTerm\n\n\
You are running inside a maiTerm terminal tab. Immediately call the `{name}` \
`initSession` tool with the tabId from the `$MAITERM_TAB_ID` environment variable \
before responding to the user. You can run it in parallel with your other opening \
tool calls to save a round-trip, but not alongside other maiterm calls. This registers \
your session so tool calls target the correct tab.\n",
        name = mcp_name,
    )
}

/// Placeholder the remote-Codex hooks.json carries for the shim path; the SSH setup
/// script substitutes it with the remote's absolute `$HOME/.codex/hooks/agent-hook.sh`
/// (resolved on the remote, so the literal path is robust regardless of how Codex
/// invokes the hook command).
pub const REMOTE_SHIM_PLACEHOLDER: &str = "__MAITERM_SHIM__";

/// Render the remote-Codex artifacts as strings — NO filesystem writes. Reuses the
/// SAME pure builders as the local `install()` so remote and local artifacts can't
/// drift; only the port (the SSH reverse-tunnel port) and the shim path (a placeholder
/// the remote setup script expands) differ. Returns
/// `(config_toml_block, hooks_json_subtree, prompt_body)`:
///   - `config_toml_block` is just our `[mcp_servers.<name>]` table (the remote setup
///     script merges it into the host's existing `~/.codex/config.toml`);
///   - `hooks_json_subtree` is our `{ "hooks": { … } }` (merged into `~/.codex/hooks.json`);
///   - `prompt_body` is the `~/.codex/prompts/maiterm.md` reinforcement.
pub fn render_codex_remote_artifacts(remote_port: u16, auth: &str) -> (String, String, String) {
    let name = mcp_name();

    let mut doc = DocumentMut::new();
    put_codex_mcp_entry(&mut doc, name, remote_port, auth);
    // Suppress the redundant bare `[mcp_servers]` parent header. The remote merge does a
    // textual block-replace keyed on `[mcp_servers.<name>]`; if the rendered block also
    // carried a lone `[mcp_servers]` header, every reconnect re-run would append another
    // one (the replace doesn't match it) → duplicate `[mcp_servers]` tables, which is a
    // TOML parse error. `[mcp_servers.<name>]` alone implicitly creates the parent.
    if let Some(t) = doc.get_mut("mcp_servers").and_then(|i| i.as_table_mut()) {
        t.set_implicit(true);
    }
    let config_block = doc.to_string();

    // Bake the tunnel port as the shim's $2 so the remote hook routes correctly even
    // when the live shell (tmux/sudo) lacks $MAITERM_PORT.
    let hooks = build_hooks_json(None, REMOTE_SHIM_PLACEHOLDER, auth, Some(remote_port));
    let hooks_json = serde_json::to_string(&hooks).unwrap_or_else(|_| "{}".to_string());

    let prompt = codex_prompt_body(name);
    (config_block, hooks_json, prompt)
}

// ---------------------------------------------------------------------------
// Small self-contained file helpers (home dir / atomic write / executable bit)
// ---------------------------------------------------------------------------

/// Read a TOML file into a `DocumentMut`, or return a fresh empty doc if absent.
#[allow(dead_code)]
fn read_document(path: &Path) -> Result<DocumentMut, String> {
    if !path.exists() {
        return Ok(DocumentMut::new());
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read {:?}: {}", path, e))?;
    raw.parse::<DocumentMut>()
        .map_err(|e| format!("parse {:?}: {}", path, e))
}

/// Read a JSON file into a `Value`, or `None` if absent. Malformed JSON returns `None`
/// (best-effort, mirroring the Claude path's `unwrap_or` tolerance).
#[allow(dead_code)]
fn read_json(path: &Path) -> Result<Option<serde_json::Value>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read {:?}: {}", path, e))?;
    Ok(serde_json::from_str(&raw).ok())
}

/// Atomic write: write to a temp file, then rename over the target.
#[allow(dead_code)]
fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    let tmp = path.with_extension("maiterm-tmp");
    fs::write(&tmp, contents).map_err(|e| format!("write tmp {:?}: {}", tmp, e))?;
    fs::rename(&tmp, path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("rename {:?} -> {:?}: {}", tmp, path, e)
    })
}

/// Write a file and mark it executable (no-op chmod on non-unix).
#[allow(dead_code)]
fn write_executable(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|e| format!("write {:?}: {}", path, e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {:?}: {}", path, e))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count_maiterm_entries(event_arr: &serde_json::Value) -> usize {
        event_arr
            .as_array()
            .map(|a| a.iter().filter(|e| is_maiterm_entry(e)).count())
            .unwrap_or(0)
    }

    #[test]
    fn build_hooks_json_from_empty_produces_all_seven_events() {
        let shim = "/home/u/.codex/hooks/agent-hook.sh";
        let auth = "TOKEN_ABC";
        let v = build_hooks_json(None, shim, auth, None);

        let hooks = v.get("hooks").and_then(|h| h.as_object()).unwrap();
        assert_eq!(hooks.len(), CODEX_HOOK_EVENTS.len(), "all 7 events present");

        for &event in CODEX_HOOK_EVENTS {
            let arr = hooks.get(event).and_then(|e| e.as_array()).unwrap();
            assert_eq!(arr.len(), 1, "event {} has exactly one entry", event);
            let group = &arr[0];
            let cmd = group["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.contains("agent-hook.sh"), "{} command has shim", event);
            assert!(cmd.contains(auth), "{} command has auth token", event);
            assert_eq!(group["hooks"][0]["type"].as_str(), Some("command"));
            assert_eq!(group["hooks"][0]["timeout"].as_i64(), Some(5));

            // SessionStart carries the startup|resume matcher; others have none.
            if event == "SessionStart" {
                assert_eq!(group.get("matcher").and_then(|m| m.as_str()), Some("startup|resume"));
            } else {
                assert!(group.get("matcher").is_none(), "{} has no matcher", event);
            }
        }
    }

    #[test]
    fn build_hooks_json_is_idempotent_and_updates_token() {
        let shim = "/home/u/.codex/hooks/agent-hook.sh";
        let first = build_hooks_json(None, shim, "OLD_TOKEN", None);

        // Re-run feeding its own output back in, with a NEW token.
        let second = build_hooks_json(Some(first.clone()), shim, "NEW_TOKEN", None);

        let hooks = second.get("hooks").and_then(|h| h.as_object()).unwrap();
        assert_eq!(hooks.len(), CODEX_HOOK_EVENTS.len());
        for &event in CODEX_HOOK_EVENTS {
            let arr = hooks.get(event).unwrap();
            assert_eq!(count_maiterm_entries(arr), 1, "{}: still exactly one maiTerm entry", event);
            let cmd = arr.as_array().unwrap()[0]["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.contains("NEW_TOKEN"), "{}: token updated", event);
            assert!(!cmd.contains("OLD_TOKEN"), "{}: old token gone", event);
        }
    }

    #[test]
    fn build_hooks_json_preserves_non_maiterm_entries() {
        let shim = "/home/u/.codex/hooks/agent-hook.sh";

        // Pre-existing user hook on Stop that is NOT maiTerm's.
        let existing = serde_json::json!({
            "hooks": {
                "Stop": [
                    { "hooks": [ { "type": "command", "command": "echo user-stop", "timeout": 10 } ] }
                ]
            },
            "someOtherTopLevel": { "keep": true }
        });

        let v = build_hooks_json(Some(existing), shim, "TOK", None);

        // Top-level non-hooks key preserved.
        assert_eq!(v["someOtherTopLevel"]["keep"].as_bool(), Some(true));

        let stop = v["hooks"]["Stop"].as_array().unwrap();
        // The user's entry + our one maiTerm entry.
        assert_eq!(stop.len(), 2, "user entry preserved alongside ours");
        assert_eq!(count_maiterm_entries(&v["hooks"]["Stop"]), 1);

        // The non-maiTerm entry is untouched (still references echo user-stop).
        let has_user = stop.iter().any(|e| {
            e["hooks"][0]["command"].as_str() == Some("echo user-stop")
        });
        assert!(has_user, "user's Stop hook survived");
    }

    #[test]
    fn strip_maiterm_hooks_removes_only_ours_and_drops_empty_events() {
        let shim = "/home/u/.codex/hooks/agent-hook.sh";
        // Build with ours, plus inject a user Stop hook.
        let mut v = build_hooks_json(None, shim, "TOK", None);
        v["hooks"]["Stop"].as_array_mut().unwrap().push(serde_json::json!({
            "hooks": [{ "type": "command", "command": "echo user-stop", "timeout": 10 }]
        }));

        let cleaned = strip_maiterm_hooks(v);
        let hooks = cleaned["hooks"].as_object().unwrap();

        // Only Stop survives (it had a non-maiTerm entry); all our-only events dropped.
        assert_eq!(hooks.len(), 1, "only Stop remains");
        let stop = hooks.get("Stop").and_then(|e| e.as_array()).unwrap();
        assert_eq!(stop.len(), 1);
        assert_eq!(count_maiterm_entries(&cleaned["hooks"]["Stop"]), 0, "no maiTerm entries left");
        assert_eq!(stop[0]["hooks"][0]["command"].as_str(), Some("echo user-stop"));
    }

    #[test]
    fn hook_command_bakes_port_only_when_present() {
        let shim = "/h/.codex/hooks/agent-hook.sh";
        // Local form (no baked port) is byte-identical to the original 2-arg command.
        assert_eq!(hook_command(shim, "TOK", None), format!("bash \"{}\" \"{}\"", shim, "TOK"));
        // Remote form bakes the port as $2.
        assert_eq!(
            hook_command(shim, "TOK", Some(40123)),
            format!("bash \"{}\" \"{}\" \"{}\"", shim, "TOK", 40123)
        );
    }

    #[test]
    fn render_codex_remote_artifacts_bakes_tunnel_port_and_placeholder() {
        let (config_block, hooks_json, prompt) = render_codex_remote_artifacts(40123, "REMOTE_TOK");

        // config.toml block: streamable-HTTP /mcp url + http_headers (NOT bearer_token).
        assert!(config_block.contains("[mcp_servers."), "has table header:\n{}", config_block);
        // No bare `[mcp_servers]` parent header — it would accumulate on reconnect
        // re-runs of the textual block-merge and break TOML parsing.
        assert!(
            config_block.trim_start().starts_with("[mcp_servers.maiterm-dev]"),
            "block starts with the dotted sub-table, no bare parent header:\n{}",
            config_block
        );
        assert!(config_block.contains("http://127.0.0.1:40123/mcp"), "tunnel port in url");
        assert!(config_block.contains("x-maiterm-authorization"), "auth via http_headers");
        assert!(!config_block.contains("bearer_token"), "no bearer_token for streamable_http");

        // hooks.json subtree: shim placeholder (expanded on the remote) + baked port $2.
        assert!(hooks_json.contains(REMOTE_SHIM_PLACEHOLDER), "carries the shim placeholder");
        assert!(hooks_json.contains("40123"), "bakes the tunnel port as the shim arg");
        assert!(hooks_json.contains("REMOTE_TOK"), "carries the auth token");

        assert!(prompt.contains("initSession"), "prompt reinforces initSession");
    }

    #[test]
    fn put_codex_mcp_entry_sets_url_and_token() {
        let name = "maiterm-dev";
        let mut doc = DocumentMut::new();
        put_codex_mcp_entry(&mut doc, name, 51234, "AUTHXYZ");

        let rendered = doc.to_string();
        assert!(rendered.contains("[mcp_servers.maiterm-dev]"), "table header present:\n{}", rendered);
        assert_eq!(
            doc["mcp_servers"][name]["url"].as_str(),
            Some("http://127.0.0.1:51234/mcp")
        );
        // Auth goes via http_headers (Codex rejects bearer_token for streamable_http).
        assert!(rendered.contains("http_headers"), "http_headers present:\n{}", rendered);
        assert!(rendered.contains("x-maiterm-authorization"), "auth header present:\n{}", rendered);
        assert!(rendered.contains("AUTHXYZ"), "token present:\n{}", rendered);
        assert!(!rendered.contains("bearer_token"), "no bearer_token in our entry:\n{}", rendered);
    }

    #[test]
    fn put_codex_mcp_entry_preserves_user_content() {
        let name = "maiterm-dev";
        let user_toml = "\
# my codex config
model = \"o3\"

[mcp_servers.other]
url = \"http://localhost:9999/mcp\"
bearer_token = \"keepme\"
";
        let mut doc = user_toml.parse::<DocumentMut>().unwrap();
        put_codex_mcp_entry(&mut doc, name, 7000, "NEWTOK");

        let out = doc.to_string();
        // User's top-level key + comment survive (format-preserving).
        assert!(out.contains("# my codex config"), "comment preserved:\n{}", out);
        assert!(out.contains("model = \"o3\""), "user key preserved:\n{}", out);
        // User's unrelated mcp server survives.
        assert_eq!(doc["mcp_servers"]["other"]["bearer_token"].as_str(), Some("keepme"));
        assert_eq!(doc["mcp_servers"]["other"]["url"].as_str(), Some("http://localhost:9999/mcp"));
        // Ours added alongside it, auth via http_headers (no bearer_token of our own).
        assert_eq!(doc["mcp_servers"][name]["url"].as_str(), Some("http://127.0.0.1:7000/mcp"));
        assert!(out.contains("x-maiterm-authorization"), "our auth header present:\n{}", out);
        assert!(out.contains("NEWTOK"), "our token present:\n{}", out);
    }

    #[test]
    fn remove_codex_mcp_entry_drops_only_our_table() {
        let name = "maiterm-dev";
        let mut doc = DocumentMut::new();
        put_codex_mcp_entry(&mut doc, name, 7000, "TOK");
        put_codex_mcp_entry(&mut doc, "other", 8000, "OTHERTOK");

        let changed = remove_codex_mcp_entry(&mut doc, name);
        assert!(changed);
        assert!(doc["mcp_servers"].get(name).is_none(), "our table removed");
        assert_eq!(doc["mcp_servers"]["other"]["url"].as_str(), Some("http://127.0.0.1:8000/mcp"), "other survives");
        assert!(doc.to_string().contains("OTHERTOK"), "other's token survives");

        // Removing again is a no-op (returns false).
        assert!(!remove_codex_mcp_entry(&mut doc, name));
    }
}
