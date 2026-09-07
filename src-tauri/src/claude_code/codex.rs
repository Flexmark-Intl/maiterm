//! Codex on-disk registration. Mirrors the Claude `lockfile.rs` machinery but for
//! Codex's own config layout under `~/.codex/`:
//!   - `~/.codex/config.toml` — `[mcp_servers.<name>]` with `url` + `http_headers`
//!     (format-preserving via toml_edit so the user's other keys/comments survive).
//!   - `~/.codex/hooks/agent-hook.sh` — the bundled hook shim (executable).
//!   - `~/.codex/hooks.json` — command hooks for every event in `CODEX_HOOK_EVENTS`, merged
//!     with valid existing JSON (malformed-file preservation remains outstanding).
//!   - `~/.codex/prompts/maiterm.md` — a tiny prompt reinforcing the initSession call.
//!
//! The hook COMMAND carries no token and no port locally, so every instance and every launch
//! writes the identical definition — which is what lets Codex's hook trust survive a restart,
//! and lets dev and prod share one entry. `reassert_if_drifted` repairs that definition every
//! 30s; unregister leaves it alone while a sibling instance is still live.
//!
//! Wired into all_registrars(); enabled by prefs.codex_ide (default true).

use std::fs;
use std::path::Path;
use toml_edit::DocumentMut;

use crate::state::{AgentRuntime, Preferences};
use super::lockfile::AGENT_HOOK_SHIM;
use super::registrar::Registrar;

/// The Codex lifecycle events we register a forwarding command hook for.
const CODEX_HOOK_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "Stop",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "UserPromptSubmit",
    "PreCompact",
    "Interrupt",
];

/// Codex caps `SessionEnd` and `Interrupt` at 3 seconds and defaults them to 1 (the general
/// default is 600). Registering them with the 5s we use elsewhere would be rejected, so the
/// timeout is per-event rather than one constant.
fn hook_timeout_secs(event: &str) -> u64 {
    match event {
        "SessionEnd" | "Interrupt" => 3,
        _ => 5,
    }
}

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

    fn install(&self, port: u16, auth: &str, _workspace_folders: &[String], prefs: &Preferences) {
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
                // A refusal (a non-table `mcp_servers`) must not be followed by a write:
                // the point of refusing is to leave the user's file exactly as it was.
                if put_codex_mcp_entry(&mut doc, name, port, auth) {
                    if let Err(e) = atomic_write(&config_path, &doc.to_string()) {
                        log::warn!("Codex install: failed to write {:?}: {}", config_path, e);
                    } else {
                        wrote_mcp = true;
                    }
                }
            }
            Err(e) => log::warn!("Codex install: failed to read {:?}: {}", config_path, e),
        }

        // 2 + 3. Hooks: the shim and the hooks.json entries, gated on the preference. The OFF
        // branch actively removes ours rather than skipping the write — a toggle the user turns
        // off has to stop Codex reporting, not merely stop being refreshed.
        let shim_path = codex_dir.join("hooks").join("agent-hook.sh");
        let hooks_path = codex_dir.join("hooks.json");
        if prefs.codex_hooks {
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

            let shim_str = shim_path.to_string_lossy().to_string();
            match read_json(&hooks_path) {
                Ok(existing) => {
                    // Local install: no baked port — the shim uses the per-process
                    // $MAITERM_PORT (each tab spawned by the owning maiTerm instance).
                    let merged = build_hooks_json(existing, &shim_str, None);
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
                // Loud: an unparseable hooks.json means Codex reports nothing to maiTerm for
                // the rest of the session, and we deliberately will not repair it by clobbering.
                Err(e) => log::error!("Codex install: not writing hooks — {}", e),
            }
        } else {
            remove_our_hooks(&hooks_path, &shim_path);
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

    /// Put the shared hooks back if anything moved them.
    ///
    /// Safe to run forever now that the definition carries no token: the merge is idempotent,
    /// so an unchanged file compares equal and nothing is written, and a rewrite produces the
    /// exact bytes Codex already trusts. It was a no-op while the command embedded a per-launch
    /// token, because re-asserting would have invalidated trust on every pass (review C1).
    ///
    /// This is also what makes shared cleanup safe: an instance that quits and strips the
    /// hooks is repaired here within one sweep.
    fn reassert_if_drifted(&self, _port: u16, _auth: &str, prefs: &Preferences) {
        if !prefs.codex_hooks {
            return;
        }
        let Some(home) = dirs::home_dir() else { return };
        let codex_dir = home.join(".codex");
        let shim_path = codex_dir.join("hooks").join("agent-hook.sh");
        let hooks_path = codex_dir.join("hooks.json");

        // The shim itself can go missing (a user cleaning ~/.codex, an unregister from a
        // sibling), and hooks pointing at an absent file fail on every event.
        let shim_current = fs::read_to_string(&shim_path).is_ok_and(|s| s == AGENT_HOOK_SHIM);
        if !shim_current {
            if let Some(parent) = shim_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Err(e) = write_executable(&shim_path, AGENT_HOOK_SHIM) {
                log::warn!("Codex reassert: failed to restore shim {:?}: {}", shim_path, e);
                return;
            }
            log::info!("Codex reassert: restored {:?}", shim_path);
        }

        let existing = match read_json(&hooks_path) {
            Ok(v) => v,
            // Unparseable: the same rule as install — never repair by clobbering.
            Err(e) => {
                log::error!("Codex reassert: not touching hooks — {}", e);
                return;
            }
        };
        let shim_str = shim_path.to_string_lossy().to_string();
        let merged = build_hooks_json(existing.clone(), &shim_str, None);
        // Idempotent merge ⇒ equal means nothing drifted. No write, no trust churn.
        if existing.as_ref() == Some(&merged) {
            return;
        }
        match serde_json::to_string_pretty(&merged) {
            Ok(json) => {
                if let Err(e) = atomic_write(&hooks_path, &json) {
                    log::warn!("Codex reassert: failed to write {:?}: {}", hooks_path, e);
                } else {
                    log::info!("Codex reassert: repaired maiTerm hooks in {:?}", hooks_path);
                }
            }
            Err(e) => log::warn!("Codex reassert: failed to serialize hooks.json: {}", e),
        }
    }

    fn unregister(&self, port: u16, _auth: &str) {
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

        // 2 + 3. Our hooks and the shim they point at — but ONLY if no sibling maiTerm is
        // still running. The hook definition is deliberately identical for every instance, so
        // dev and prod share one trusted entry; tearing it out on quit would leave the survivor
        // with hooks that point at a deleted shim and fail on every event (review C1). This is
        // the `port` argument unregister used to ignore.
        if super::lockfile::another_maiterm_is_live(port) {
            log::info!("Codex unregister: another maiTerm is live — leaving the shared hooks and shim in place");
        } else {
            remove_our_hooks(
                &codex_dir.join("hooks.json"),
                &codex_dir.join("hooks").join("agent-hook.sh"),
            );
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

/// Take maiTerm's hook entries out of `hooks.json` and delete the shim they point at, leaving
/// everything else in the file alone.
///
/// Shared by `unregister` and by an install that finds `codex_hooks` turned off: switching the
/// preference off has to stop Codex reporting, not just stop refreshing the entries.
#[allow(dead_code)]
fn remove_our_hooks(hooks_path: &Path, shim_path: &Path) {
    if hooks_path.exists() {
        match read_json(hooks_path) {
            Ok(Some(existing)) => {
                let cleaned = strip_maiterm_hooks(existing.clone());
                // Only rewrite if we actually removed something of ours — never reformat a
                // user's hooks.json that maiTerm never wrote to.
                if cleaned != existing {
                    match serde_json::to_string_pretty(&cleaned) {
                        Ok(json) => {
                            if let Err(e) = atomic_write(hooks_path, &json) {
                                log::warn!("Codex: failed to write {:?}: {}", hooks_path, e);
                            }
                        }
                        Err(e) => log::warn!("Codex: failed to serialize hooks.json: {}", e),
                    }
                }
            }
            Ok(None) => {}
            Err(e) => log::warn!("Codex: failed to read {:?}: {}", hooks_path, e),
        }
    }
    if shim_path.exists() {
        if let Err(e) = fs::remove_file(shim_path) {
            log::warn!("Codex: failed to remove {:?}: {}", shim_path, e);
        }
    }
}

/// The MCP server name for this build flavor (`maiterm` / `maiterm-dev`).
#[allow(dead_code)]
fn mcp_name() -> &'static str {
    crate::state::agent_runtime::mcp_server_name(AgentRuntime::Codex)
}

/// Set `[mcp_servers.<name>]` with `url`, auth headers, and the tab env header, preserving everything
/// else in the document (format, comments, unrelated tables). Format-preserving via
/// toml_edit: we mutate the existing `DocumentMut` in place.
///
/// toml_edit auto-vivifies missing intermediate tables as INLINE tables
/// (`mcp_servers = { ... }`) when you index-assign through them. We want real
/// `[mcp_servers.<name>]` headers, so we explicitly ensure `mcp_servers` and the
/// per-name child are standard (non-inline) tables before writing the leaf values.
#[allow(dead_code)]
fn put_codex_mcp_entry(doc: &mut DocumentMut, name: &str, port: u16, auth: &str) -> bool {
    // `as_table_like_mut` accepts BOTH representations. `mcp_servers = { … }` and
    // `[mcp_servers.x] … ` are both valid TOML for the same thing, and the previous
    // `as_table_mut().expect(…)` panicked on the inline one — a user who wrote their config
    // that way crashed the install (review C4). An existing inline table keeps its
    // representation; only a table we create ourselves is made a standard one, so the entry
    // still renders as `[mcp_servers.<name>]` in the common case.
    let servers = match doc
        .entry("mcp_servers")
        .or_insert_with(|| toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
    {
        Some(t) => t,
        None => {
            // `mcp_servers` exists and is not a table at all. Overwriting would destroy
            // whatever the user meant by it, so refuse and leave the file alone.
            log::error!("Codex: ~/.codex/config.toml has a non-table `mcp_servers`; refusing to overwrite it");
            return false;
        }
    };
    let entry = match servers
        .entry(name)
        .or_insert(toml_edit::Item::Table(toml_edit::Table::new()))
        .as_table_like_mut()
    {
        Some(t) => t,
        None => {
            log::error!("Codex: ~/.codex/config.toml has a non-table `mcp_servers.{name}`; refusing to overwrite it");
            return false;
        }
    };
    entry.insert("url", toml_edit::value(format!("http://127.0.0.1:{}/mcp", port)));
    // Codex rejects `bearer_token` for streamable_http servers ("bearer_token is not
    // supported for streamable_http"), so pass the auth via http_headers instead — our
    // server's extract_auth accepts the x-maiterm-authorization header (raw token).
    let mut headers = toml_edit::InlineTable::new();
    headers.insert("x-maiterm-authorization", toml_edit::Value::from(auth));
    entry.insert("http_headers", toml_edit::value(headers));
    // Codex's equivalent of Claude's `${VAR}` header expansion: `env_http_headers` maps a
    // header name to an ENV VAR NAME that Codex resolves from the agent's environment. It
    // gives the server the caller's tab on every request, so tool calls target the right
    // tab without waiting for the agent to call initSession (see `server::TAB_ID_HEADER`).
    // Verified against codex-cli 0.149.0: the header is sent on initialize,
    // notifications/initialized and tools/list, carrying the env var's value. Verified safe
    // when the var is unset too — the server stays enabled, it just sends nothing for us to
    // read, which is the pre-header behavior.
    let mut env_headers = toml_edit::InlineTable::new();
    env_headers.insert("x-maiterm-tab", toml_edit::Value::from("MAITERM_TAB_ID"));
    entry.insert("env_http_headers", toml_edit::value(env_headers));
    // Drop any stale bearer_token from a previous (rejected) format.
    entry.remove("bearer_token");
    true
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
fn maiterm_hook_entry(command: &str, matcher: Option<&str>, timeout_secs: u64) -> serde_json::Value {
    let mut group = serde_json::json!({
        "hooks": [{
            "type": "command",
            "command": command,
            "timeout": timeout_secs
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
fn hook_command(shim_path: &str, port: Option<u16>) -> String {
    match port {
        Some(p) => format!("bash \"{}\" \"{}\"", shim_path, p),
        None => format!("bash \"{}\"", shim_path),
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
    port: Option<u16>,
) -> serde_json::Value {
    // Start from the existing doc (preserve other top-level keys) or a fresh object.
    let mut root = match existing {
        Some(v @ serde_json::Value::Object(_)) => v,
        _ => serde_json::json!({}),
    };

    let command = hook_command(shim_path, port);

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
        let our_entry = maiterm_hook_entry(&command, matcher, hook_timeout_secs(event));

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

/// Legacy startup prompt. Still requires initSession despite transport identity;
/// replace alongside missing SessionStart priming (docs/codex-integration-review.md C2).
#[allow(dead_code)]
fn codex_prompt_body(mcp_name: &str) -> String {
    format!(
        "# maiTerm\n\n\
You are running inside a maiTerm terminal tab. Your tab is identified automatically: the \
`x-maiterm-tab` header rides every MCP request you make, and the SessionStart hook registers \
your session. You do NOT need to call `initSession`, and should not spend an opening turn on \
it.\n\n\
`initSession` is a REPAIR tool. Call it only when a `{name}` tool answers that it does not \
know your tab, that your tab was inferred rather than stated, or when the human asks \
(`/maiterm init`). Pass the tabId from `$MAITERM_TAB_ID`, and do not batch that call with \
other `{name}` calls — it would race the registration and can target the wrong tab.\n",
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
    // Always applies: the document is freshly created here, so there is no user content to
    // refuse over. The bool matters only for the local install's read-modify-write.
    let _ = put_codex_mcp_entry(&mut doc, name, remote_port, auth);
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
    let hooks = build_hooks_json(None, REMOTE_SHIM_PLACEHOLDER, Some(remote_port));
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

/// Read a JSON file into a `Value`, or `None` if absent.
///
/// Malformed JSON is an ERROR, not `None`. It used to be swallowed, which made an unparseable
/// `~/.codex/hooks.json` look like an empty one — so the merge started from scratch and the
/// install wrote maiTerm's hooks over every hook the user had (review C4). Every caller treats
/// `Err` as "log it and write nothing", which is the only safe reading of a file we cannot parse.
#[allow(dead_code)]
fn read_json(path: &Path) -> Result<Option<serde_json::Value>, String> {
    if !path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(path).map_err(|e| format!("read {:?}: {}", path, e))?;
    serde_json::from_str(&raw)
        .map(Some)
        .map_err(|e| format!("parse {:?}: {} (leaving the file untouched)", path, e))
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
    fn build_hooks_json_from_empty_produces_every_registered_event() {
        let shim = "/home/u/.codex/hooks/agent-hook.sh";
        let auth = "TOKEN_ABC";
        let v = build_hooks_json(None, shim, None);

        let hooks = v.get("hooks").and_then(|h| h.as_object()).unwrap();
        assert_eq!(hooks.len(), CODEX_HOOK_EVENTS.len(), "every registered event present");
        // Interrupt and SessionEnd were missing until review C5; both fire for real (the C7
        // trace observed SessionEnd), and C7's approval clearing hangs off Interrupt.
        assert!(hooks.contains_key("Interrupt") && hooks.contains_key("SessionEnd"));

        for &event in CODEX_HOOK_EVENTS {
            let arr = hooks.get(event).and_then(|e| e.as_array()).unwrap();
            assert_eq!(arr.len(), 1, "event {} has exactly one entry", event);
            let group = &arr[0];
            let cmd = group["hooks"][0]["command"].as_str().unwrap();
            assert!(cmd.contains("agent-hook.sh"), "{} command has shim", event);
            // The token is deliberately NOT here: it is read from $MAITERM_AUTH at run time
            // so the definition stays byte-identical across launches and stays trusted.
            assert!(!cmd.contains(auth), "{} command must not carry the auth token", event);
            assert_eq!(group["hooks"][0]["type"].as_str(), Some("command"));
            // Codex caps SessionEnd and Interrupt at 3s (default 1); everything else takes
            // our general 5s. Registering those two at 5 would be rejected.
            let want = if matches!(event, "SessionEnd" | "Interrupt") { 3 } else { 5 };
            assert_eq!(group["hooks"][0]["timeout"].as_i64(), Some(want), "{} timeout", event);

            // SessionStart carries the startup|resume matcher; others have none.
            if event == "SessionStart" {
                assert_eq!(group.get("matcher").and_then(|m| m.as_str()), Some("startup|resume"));
            } else {
                assert!(group.get("matcher").is_none(), "{} has no matcher", event);
            }
        }
    }

    #[test]
    fn build_hooks_json_is_idempotent() {
        let shim = "/home/u/.codex/hooks/agent-hook.sh";
        let first = build_hooks_json(None, shim, None);

        // Re-run feeding its own output back in: the result must be unchanged, which is what
        // lets the 30s reassert run forever without ever invalidating hook trust.
        let second = build_hooks_json(Some(first.clone()), shim, None);
        assert_eq!(first, second, "re-running changes nothing");

        let hooks = second.get("hooks").and_then(|h| h.as_object()).unwrap();
        assert_eq!(hooks.len(), CODEX_HOOK_EVENTS.len());
        for &event in CODEX_HOOK_EVENTS {
            let arr = hooks.get(event).unwrap();
            assert_eq!(count_maiterm_entries(arr), 1, "{}: still exactly one maiTerm entry", event);
            let cmd = arr.as_array().unwrap()[0]["hooks"][0]["command"].as_str().unwrap();
            assert_eq!(cmd, format!("bash \"{}\"", shim), "{}: stable definition", event);
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

        let v = build_hooks_json(Some(existing), shim, None);

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
        let mut v = build_hooks_json(None, shim, None);
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
    fn hook_command_is_stable_and_carries_no_secret() {
        let shim = "/h/.codex/hooks/agent-hook.sh";
        // Local form: the shim path and nothing else. Codex records hook trust against the
        // exact command string, so anything per-launch here (the auth token, formerly $1)
        // produced a new untrusted definition every restart and the hooks silently stopped
        // running (review C1). This also lets dev and prod share one trusted definition.
        assert_eq!(hook_command(shim, None), format!("bash \"{}\"", shim));
        // Remote form bakes the tunnel port as $1 — fixed for the life of the install, and
        // not a secret. The token is read from the environment either way.
        assert_eq!(hook_command(shim, Some(40123)), format!("bash \"{}\" \"{}\"", shim, 40123));

        for cmd in [hook_command(shim, None), hook_command(shim, Some(40123))] {
            assert!(!cmd.contains("TOK"), "no token in the definition: {cmd}");
        }
    }

    #[test]
    fn hook_definitions_are_identical_across_launches_with_different_tokens() {
        // The property that makes hook trust survive a restart, stated directly.
        let shim = "/h/.codex/hooks/agent-hook.sh";
        let a = build_hooks_json(None, shim, None);
        let b = build_hooks_json(None, shim, None);
        assert_eq!(a, b);
        let cmd = a["hooks"]["Stop"][0]["hooks"][0]["command"].as_str().unwrap();
        assert_eq!(cmd, format!("bash \"{}\"", shim));
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
        // The remote definition carries no secret either — the tunnel port is fixed for the
        // life of the install, so the remote hook command is stable too.
        assert!(!hooks_json.contains("REMOTE_TOK"), "no auth token in the hook definition");

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
        // Tab identity rides the transport: env_http_headers maps the header to an env var
        // NAME (not a value), so each agent resolves its own tab and one shared config entry
        // stays correct for every tab on the host.
        assert_eq!(
            doc["mcp_servers"][name]["env_http_headers"]["x-maiterm-tab"].as_str(),
            Some("MAITERM_TAB_ID"),
            "tab header maps to the env var name:\n{}",
            rendered
        );
        assert!(!rendered.contains("bearer_token"), "no bearer_token in our entry:\n{}", rendered);
    }

    /// Run the shipped shim with a payload and return its stdout.
    fn run_shim(payload: &str, env: &[(&str, &str)]) -> Option<String> {
        use std::io::Write;
        use std::process::{Command, Stdio};
        let dir = std::env::temp_dir().join(format!(
            "maiterm-shim-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).ok()?;
        let path = dir.join("agent-hook.sh");
        std::fs::write(&path, AGENT_HOOK_SHIM).ok()?;
        let mut cmd = Command::new("bash");
        // HOME points at an empty dir so the ~/.aiterm fallback finds nothing.
        cmd.arg(&path).arg("TOKEN").env("HOME", &dir);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().ok()?;
        child.stdin.take().unwrap().write_all(payload.as_bytes()).ok()?;
        let out = child.wait_with_output().ok()?;
        let _ = std::fs::remove_dir_all(&dir);
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    #[test]
    fn shim_always_emits_valid_json_even_with_nothing_to_talk_to() {
        // The fail-safe that matters: a hook printing anything but JSON is a hook error in the
        // agent's face on every event. No port, and an unreachable port, must both answer {}.
        let start = "{\"hook_event_name\":\"SessionStart\",\"session_id\":\"s\"}";
        let stop = "{\"hook_event_name\":\"Stop\",\"session_id\":\"s\"}";
        for payload in [start, stop] {
            assert_eq!(run_shim(payload, &[]).as_deref(), Some("{}"), "no port configured");
            // 9 is reserved/discard — nothing is listening, so curl fails fast.
            assert_eq!(
                run_shim(payload, &[("MAITERM_PORT", "9"), ("MAITERM_TAB_ID", "tab-1")]).as_deref(),
                Some("{}"),
                "unreachable server"
            );
        }
    }

    #[test]
    fn shim_asks_for_priming_on_session_start_only() {
        // Both query params are load-bearing and were absent entirely until review C2:
        // `prime=1` is what makes the server answer with a body at all, and `format=codex` is
        // what makes that body Codex's SessionStart output shape instead of bare text — which
        // Codex would fail to parse as JSON.
        assert!(AGENT_HOOK_SHIM.contains("prime=1&format=codex"), "SessionStart asks for both");
        assert!(AGENT_HOOK_SHIM.contains("hook_event_name\\\":\\\"SessionStart")
            || AGENT_HOOK_SHIM.contains("\"hook_event_name\":\"SessionStart\""),
            "gated on the event name");
        // Every other event stays observational: its curl still discards the response.
        assert!(AGENT_HOOK_SHIM.contains(">/dev/null 2>&1 || true"), "non-priming branch intact");
    }

    #[test]
    fn put_codex_mcp_entry_accepts_an_inline_parent_table() {
        // `mcp_servers = { … }` is valid TOML for the same thing as `[mcp_servers.x]`. The old
        // `as_table_mut().expect(…)` panicked on it, so a user who wrote their config this way
        // crashed the install (review C4).
        let name = "maiterm-dev";
        let user_toml = "model = \"o3\"\nmcp_servers = { other = { url = \"http://localhost:9999/mcp\" } }\n";
        let mut doc = user_toml.parse::<DocumentMut>().unwrap();
        assert!(put_codex_mcp_entry(&mut doc, name, 7000, "NEWTOK"));

        let out = doc.to_string();
        assert_eq!(doc["mcp_servers"]["other"]["url"].as_str(), Some("http://localhost:9999/mcp"));
        assert_eq!(doc["mcp_servers"][name]["url"].as_str(), Some("http://127.0.0.1:7000/mcp"));
        assert!(out.contains("model = \"o3\""), "user key preserved:\n{}", out);
        // Still valid TOML after the edit.
        assert!(out.parse::<DocumentMut>().is_ok(), "round-trips:\n{}", out);
    }

    #[test]
    fn put_codex_mcp_entry_accepts_an_inline_child_table() {
        let name = "maiterm-dev";
        let user_toml = format!(
            "[mcp_servers]\nother = {{ url = \"http://localhost:9999/mcp\" }}\n{name} = {{ url = \"http://127.0.0.1:1/mcp\" }}\n"
        );
        let mut doc = user_toml.parse::<DocumentMut>().unwrap();
        assert!(put_codex_mcp_entry(&mut doc, name, 7000, "NEWTOK"));

        let out = doc.to_string();
        assert_eq!(doc["mcp_servers"][name]["url"].as_str(), Some("http://127.0.0.1:7000/mcp"));
        assert_eq!(doc["mcp_servers"]["other"]["url"].as_str(), Some("http://localhost:9999/mcp"));
        assert!(out.contains("NEWTOK"), "token written into the inline table:\n{}", out);
        assert!(out.parse::<DocumentMut>().is_ok(), "round-trips:\n{}", out);
    }

    #[test]
    fn put_codex_mcp_entry_refuses_a_non_table_and_changes_nothing() {
        // Overwriting would destroy whatever the user meant by it. Refusing returns false, and
        // the install skips the write on false.
        let name = "maiterm-dev";
        let user_toml = "mcp_servers = \"not a table\"\n";
        let mut doc = user_toml.parse::<DocumentMut>().unwrap();
        assert!(!put_codex_mcp_entry(&mut doc, name, 7000, "TOK"));
        assert_eq!(doc.to_string(), user_toml, "document untouched");
    }

    #[test]
    fn read_json_reports_a_malformed_file_rather_than_calling_it_absent() {
        // `None` meant "no file", so the merge started from scratch and the install wrote over
        // every hook the user had (review C4).
        let dir = std::env::temp_dir().join(format!("maiterm-c4-json-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("hooks.json");

        std::fs::write(&p, "{ not json").unwrap();
        assert!(read_json(&p).is_err(), "malformed is an error, not None");

        std::fs::write(&p, "{\"hooks\":{}}").unwrap();
        assert!(read_json(&p).unwrap().is_some(), "valid parses");

        std::fs::remove_file(&p).unwrap();
        assert!(read_json(&p).unwrap().is_none(), "absent is None");
        let _ = std::fs::remove_dir_all(&dir);
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
