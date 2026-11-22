//! Antigravity CLI (`agy`, mapped to `AgentRuntime::Gemini`) on-disk registration.
//!
//! Manages three clean, non-destructive pieces under `~/.gemini/`:
//!   1. `~/.gemini/config/plugins/maiterm/` — plugin bundle:
//!      - `plugin.json` — manifest (`{"name": "maiterm"}`)
//!      - `mcp-stdio.sh` (Unix) — wrapper that falls back to `~/.aiterm` and execs
//!        `"${MAITERM_BIN:-<exe>}" --mcp-stdio`.
//!      - `rules/AGENTS.md` — compact, always-inlined cheatsheet so `agy` knows maiTerm's
//!        key MCP tools (tasks/decision cards, Overlord, notes, mesh, stack, follow-ups)
//!        from turn 0 without exploring deferred schema files.
//!      - `hooks/agy-hook.sh` + `hooks.json` — lifecycle hooks (`SessionStart`, `PreInvocation`,
//!        `PostInvocation`, `PreToolUse`, `PostToolUse`, `Stop`), gated on `prefs.gemini_hooks`.
//!      - `skills/maiterm/SKILL.md` — full maiTerm `/maiterm` & Mattermost resolve skill.
//!   2. `~/.gemini/config/mcp_config.json` — `mcpServers.maiterm` stdio entry pointing at
//!      `mcp-stdio.sh` (Unix) or `<exe> --mcp-stdio` (Windows). Keeping the MCP server in
//!      `~/.gemini/config/mcp_config.json` rather than inside the plugin gives clean
//!      `maiterm/<tool>` names instead of `maiterm_maiterm/<tool>` (verified on `agy 1.5.13`,
//!      docs/agy-integration-review.md). Safe JSON merge preserves user servers and refuses
//!      to overwrite malformed JSON.
//!   3. `~/.gemini/antigravity-cli/settings.json` — adds `"mcp(maiterm/*)"` to
//!      `permissions.allow` so `agy` does not prompt for approval on every maiTerm tool call
//!      (maiTerm enforces its own human-approval cards for sensitive actions, and `agy`'s
//!      Deny > Ask > Allow precedence still lets users override any tool via `/permissions`).
//!
//! No port or secret is written to disk, so dev and prod share one registration cleanly.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::state::{AgentRuntime, Preferences};
use super::registrar::Registrar;

/// The `agy` hook forwarding script bundled into the binary.
pub const AGY_HOOK_SHIM: &str =
    include_str!("../../resources/maiterm-skill/bin/agy-hook.sh");

/// The bundled maiTerm skill markdown.
const MAITERM_SKILL_MD: &str =
    include_str!("../../resources/maiterm-skill/SKILL.md");

const PLUGIN_NAME: &str = "maiterm";
const MCP_SERVER_KEY: &str = "maiterm";
const MCP_ALLOW_RULE: &str = "mcp(maiterm/*)";

/// Compact, always-inlined rule for Antigravity (`plugins/maiterm/rules/AGENTS.md`).
/// Gives `agy` immediate awareness of maiTerm's common MCP tools and key schemas without
/// burning turns exploring `SKILL.md` and deferred `<tool>.json` files.
const AGY_AGENTS_MD: &str = r#"# maiTerm Terminal Integration

You are running inside a **maiTerm** terminal tab. Your MCP server is **`maiterm`**.

- **Automatic tab identity:** Every `maiterm` tool call automatically carries your tab ID (`$MAITERM_TAB_ID`). Do **not** call `initSession` at startup; call it only if a tool answers that your tab is unknown or when the user asks for `/maiterm init`.
- **Workspace Stack (dev servers, databases, watchers):** ALWAYS check **`maiterm/listStack({})`** *before* running a dev server, database, or background watcher in the shell — maiTerm may already manage it as a service.
  - Manage services: `listStack({})`, `startService({ service, on? })`, `restartService({ service, on? })`, `stopService({ service, on? })`, `waitForService({ service, timeout?, on? })`, `getServiceOutput({ service, lines?, on? })`, `createService({ name, command, cwd?, ready_pattern?, auto_start?, restart? })`, `updateService({ service, url?, port?, ready?, command?, note? })`.
- **Tasks & decision/action cards:** A maiTerm "decision card" or "action card" is a task with a **`blocker`** object (via `createTasks` or `updateTasks`). Do not also ask the same question with `ask_question`.
  - Decision card: `createTasks({ workstream?, tasks: [{ title: "...", blocker: { kind: "decision", question: "...", options: [{ label: "Option A", detail: "...", recommended: true }, { label: "Option B" }], context: "..." } }] })` (add `pick: "many"` for multi-select).
  - Action card (human runs a command): `blocker: { kind: "action", question: "...", command: "...", context: "...", run_in?: "peer name" }`.
  - Task tools: `createTasks({ tasks, workstream? })` (`status`: `"backlog"|"todo"|"active"|"blocked"|"review"|"done"`), `listTasks({ scope?: "tab"|"workspace", status?: [...], ready?, limit? })`, `updateTasks({ updates: [{ id, status?, note?, detail?, blocker?, assign_to?, block_on?, unblock_from? }] })`, `deleteTasks({ ids?, workstreams? })`.
- **Overlord & review:** If priming mentions an Overlord, call `replyToOverlord({ kind: "ready"|"ack"|"status"|"escalate", state: "working"|"blocked"|"done"|"idle", summary: "...", task?, next?, blockers?, needs_human? })`. After non-trivial commits, call `getReviewBrief({ repo?, head? })`.
- **Notes & workspace UI:** `getTabNotes({})`, `setTabNotes({ notes, mode?: "source"|"render" })`, `editTabNotes({ old_string, new_string } | { edits })`, `openNotesPanel({ open? })`, `listWorkspaceNotes({ workspaceId? })`, `readWorkspaceNote({ noteId, workspaceId? })`, `writeWorkspaceNote({ content, noteId?, mode?: "source"|"render", workspaceId? })`, `openFile({ filePath, startLine?, endLine? })`, `showDiff({ filePath, ref?: "@turn"|"@session"|"<git-ref>" })`, `listWorkspaces({ windowId? })`, `getActiveTab({})`, `switchTab({ tabId })`, `sendNotification({ title, body?, type? })`, `setMuted({ muted, scope?, target? })`.
- **Mesh / Agent Bridge:** `listBridgedPeers({})`, `sendToBridgedAgent({ message, recipient?, topic? })`, `getBridgedAgent({})`, `listTopics({})`, `startTopic({ label })`, `completeTopic({ topicId })`.
- **Follow-ups:** `createFollowUp({ text, in_minutes? | at? | when_script?: { script, label?, every_seconds? } | when_service_ready? | when_service_stopped? | when_task_done?, expires_in_minutes? })`, `listFollowUps({})`, `cancelFollowUp({ id })`.
- **Browser Link (linked Chrome tabs):** `browserStatus({})`, `browserSnapshot({ tab?, max_chars? })`, `browserClick({ ref, tab?, double? })`, `browserType({ text, ref?, tab?, clear?, submit? })`, `browserConsole({ tab?, errors_only?, clear? })`, `browserNetwork({ tab?, failed_only?, filter?, request_id? })`, `browserScreenshot({ tab?, full_page? })`.
- For `/maiterm resolve` (Mattermost threads) or detailed `/maiterm` subcommand mappings, activate the `maiterm` skill.
"#;

pub struct AgyRegistrar;

impl Registrar for AgyRegistrar {
    fn runtime(&self) -> AgentRuntime {
        AgentRuntime::Gemini
    }

    fn enabled(&self, prefs: &Preferences) -> bool {
        prefs.gemini_ide
    }

    fn install(&self, port: u16, _auth: &str, _workspace_folders: &[String], prefs: &Preferences) {
        let Some(paths) = AgyPaths::real() else {
            log::warn!("Agy install: could not determine home directory");
            return;
        };
        let exe = current_exe_string();
        let keep_shared_hooks = super::lockfile::another_maiterm_is_live(port);
        match install_into(&paths, &exe, prefs.gemini_hooks, keep_shared_hooks) {
            Ok(changed) => {
                log::info!(
                    "Agy install (port {}): plugin={} hooks={} changed={}",
                    port,
                    paths.plugin_dir.display(),
                    prefs.gemini_hooks,
                    changed
                );
            }
            Err(e) => log::warn!("Agy install: failed for {}: {}", paths.plugin_dir.display(), e),
        }
    }

    fn reassert_if_drifted(&self, port: u16, _auth: &str, prefs: &Preferences) {
        let Some(paths) = AgyPaths::real() else { return };
        let exe = current_exe_string();
        let keep_shared_hooks = super::lockfile::another_maiterm_is_live(port);
        match install_into(&paths, &exe, prefs.gemini_hooks, keep_shared_hooks) {
            Ok(true) => log::info!("Agy reassert: repaired registration at {}", paths.plugin_dir.display()),
            Ok(false) => {}
            Err(e) => log::warn!("Agy reassert: failed for {}: {}", paths.plugin_dir.display(), e),
        }
    }

    fn unregister(&self, port: u16, _auth: &str) {
        // The registration is shared across dev and prod instances (no baked port or token), so
        // removing it while a sibling maiTerm is live would blind that sibling's agy tabs.
        if super::lockfile::another_maiterm_is_live(port) {
            log::info!("Agy unregister: another maiTerm is live — leaving shared registration alone");
            return;
        }
        let Some(paths) = AgyPaths::real() else { return };
        if let Err(e) = unregister_from(&paths) {
            log::warn!("Agy unregister: failed: {}", e);
        }
    }
}

/// On-disk locations managed by `AgyRegistrar`. Parameterized for unit tests.
struct AgyPaths {
    /// `~/.gemini/config/plugins/maiterm`
    plugin_dir: PathBuf,
    /// `~/.gemini/config/mcp_config.json`
    mcp_config: PathBuf,
    /// `~/.gemini/antigravity-cli/settings.json`
    cli_settings: PathBuf,
}

impl AgyPaths {
    fn real() -> Option<Self> {
        let home = dirs::home_dir()?;
        Some(Self::under(&home))
    }

    fn under(home: &Path) -> Self {
        let gemini = home.join(".gemini");
        Self {
            plugin_dir: gemini.join("config").join("plugins").join(PLUGIN_NAME),
            mcp_config: gemini.join("config").join("mcp_config.json"),
            cli_settings: gemini.join("antigravity-cli").join("settings.json"),
        }
    }
}

/// Global Antigravity plugin path: `~/.gemini/config/plugins/maiterm`.
#[allow(dead_code)]
pub fn plugin_dir() -> Option<PathBuf> {
    AgyPaths::real().map(|p| p.plugin_dir)
}

/// Global Antigravity MCP config path: `~/.gemini/config/mcp_config.json`.
pub fn mcp_config_path() -> Option<PathBuf> {
    AgyPaths::real().map(|p| p.mcp_config)
}

/// Path to the stdio wrapper script inside the global Antigravity plugin.
pub fn mcp_stdio_wrapper_path() -> Option<PathBuf> {
    AgyPaths::real().map(|p| p.plugin_dir.join("mcp-stdio.sh"))
}

/// Artifacts rendered for remote SSH hosts (reused by `commands::ssh_tunnel::build_agy_setup_script`
/// so remote and local Antigravity registrations never drift).
pub struct AgyRemoteArtifacts {
    pub plugin_json: String,
    pub agents_md: &'static str,
    pub skill_md: &'static str,
    pub hook_shim: &'static str,
    pub hooks_json: String,
    pub mcp_stdio_py: &'static str,
    pub mcp_stdio_sh: &'static str,
}

/// Stdlib-only Python 3 stdio-to-HTTP MCP bridge for remote SSH hosts (where the local
/// `maiTerm` binary is not installed). Mirrors `claude_code/mcp_stdio.rs`: reads NDJSON
/// JSON-RPC lines from stdin, forwards to `http://127.0.0.1:$MAITERM_PORT<path>` with
/// `x-maiterm-authorization`, `x-maiterm-tab`, and sticky `mcp-session-id`, and unpacks
/// SSE `data:` frames or plain JSON back to stdout.
pub const REMOTE_MCP_STDIO_PY: &str = r#"#!/usr/bin/env python3
import json
import os
import sys
import urllib.error
import urllib.request


def err_reply(raw, msg):
    try:
        v = json.loads(raw)
        rid = v.get("id")
        if rid is None:
            return None
        return json.dumps(
            {"jsonrpc": "2.0", "id": rid, "error": {"code": -32000, "message": msg}},
            separators=(",", ":"),
        )
    except Exception:
        return None


def main():
    path = "/mcp"
    args = sys.argv[1:]
    i = 0
    while i < len(args):
        if args[i] == "--path" and i + 1 < len(args):
            if args[i + 1].startswith("/"):
                path = args[i + 1]
            i += 2
        else:
            i += 1
    port = os.environ.get("MAITERM_PORT", "").strip()
    auth = os.environ.get("MAITERM_AUTH", "").strip()
    tab_id = os.environ.get("MAITERM_TAB_ID", "").strip()
    if not port or not auth:
        return
    url = f"http://127.0.0.1:{port}{path}"
    sid = None
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    for raw in sys.stdin:
        line = raw.strip()
        if not line:
            continue
        headers = {
            "x-maiterm-authorization": auth,
            "content-type": "application/json",
            "accept": "application/json, text/event-stream",
        }
        if tab_id:
            headers["x-maiterm-tab"] = tab_id
        if sid:
            headers["mcp-session-id"] = sid
        req = urllib.request.Request(
            url, data=line.encode("utf-8"), headers=headers, method="POST"
        )
        out = None
        try:
            with opener.open(req, timeout=180) as resp:
                nsid = resp.headers.get("mcp-session-id")
                if nsid:
                    sid = nsid
                if resp.status != 202:
                    ct = resp.headers.get("content-type", "")
                    body = resp.read().decode("utf-8", errors="replace").strip()
                    if body:
                        if (
                            "text/event-stream" in ct
                            or body.startswith("event:")
                            or body.startswith("data:")
                        ):
                            data_lines = [
                                l[5:].lstrip()
                                for l in body.splitlines()
                                if l.startswith("data:")
                            ]
                            joined = "\n".join(data_lines)
                            if joined:
                                try:
                                    out = json.dumps(
                                        json.loads(joined), separators=(",", ":")
                                    )
                                except Exception:
                                    out = joined.replace("\n", " ")
                        else:
                            try:
                                out = json.dumps(
                                    json.loads(body), separators=(",", ":")
                                )
                            except Exception:
                                out = body.replace("\n", " ")
        except urllib.error.HTTPError as e:
            out = err_reply(line, f"maiTerm MCP HTTP {e.code}")
        except Exception as e:
            out = err_reply(line, f"maiTerm MCP unreachable: {e}")
        if out:
            sys.stdout.write(out + "\n")
            sys.stdout.flush()


if __name__ == "__main__":
    main()
"#;

const REMOTE_MCP_STDIO_SH: &str = r#"#!/usr/bin/env bash
if [ -z "${MAITERM_PORT:-}" ] || [ -z "${MAITERM_AUTH:-}" ] || [ -z "${MAITERM_TAB_ID:-}" ]; then
  [ -f "$HOME/.aiterm" ] && . "$HOME/.aiterm" 2>/dev/null || true
fi
exec python3 "$HOME/.gemini/config/plugins/maiterm/mcp-stdio.py" "$@"
"#;

/// Render all Antigravity plugin/hook/bridge files for installation on a remote SSH host.
pub fn render_agy_remote_artifacts() -> AgyRemoteArtifacts {
    let plugin_json = serde_json::to_string_pretty(&json!({ "name": PLUGIN_NAME }))
        .unwrap_or_else(|_| "{\"name\":\"maiterm\"}".to_string());
    let hooks_json = serde_json::to_string_pretty(&build_hooks_json(None))
        .unwrap_or_else(|_| "{}".to_string());
    AgyRemoteArtifacts {
        plugin_json,
        agents_md: AGY_AGENTS_MD,
        skill_md: MAITERM_SKILL_MD,
        hook_shim: AGY_HOOK_SHIM,
        hooks_json,
        mcp_stdio_py: REMOTE_MCP_STDIO_PY,
        mcp_stdio_sh: REMOTE_MCP_STDIO_SH,
    }
}

fn current_exe_string() -> String {
    std::env::current_exe()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Build the Unix stdio wrapper script. Reads `$MAITERM_BIN` (set per-PTY by the owning
/// maiTerm instance) so dev and prod invoke their own binary; falls back to `fallback_exe`
/// for env-less shells that only sourced `~/.aiterm`.
fn build_mcp_stdio_wrapper(fallback_exe: &str) -> String {
    let escaped = fallback_exe.replace('\'', "'\"'\"'");
    format!(
        r#"#!/usr/bin/env bash
if [ -z "${{MAITERM_PORT:-}}" ] || [ -z "${{MAITERM_AUTH:-}}" ] || [ -z "${{MAITERM_TAB_ID:-}}" ]; then
  [ -f "$HOME/.aiterm" ] && . "$HOME/.aiterm" 2>/dev/null || true
fi
bin="${{MAITERM_BIN:-}}"
[ -n "$bin" ] || bin='{escaped}'
[ -n "$bin" ] && [ -x "$bin" ] || exit 0
exec "$bin" --mcp-stdio "$@"
"#
    )
}

/// Build the `mcpServers.maiterm` entry for `~/.gemini/config/mcp_config.json`.
fn build_mcp_server_entry(plugin_dir: &Path, exe: &str) -> Value {
    if cfg!(windows) {
        json!({
            "command": exe,
            "args": ["--mcp-stdio"],
        })
    } else {
        let wrapper_path = plugin_dir.join("mcp-stdio.sh");
        json!({
            "command": "bash",
            "args": [wrapper_path.to_string_lossy()],
        })
    }
}

/// True if an `mcpServers.maiterm` entry is maiTerm's own stdio bridge entry (so unregister
/// never deletes a user-authored custom `maiterm` server).
fn is_our_mcp_entry(entry: &Value) -> bool {
    let Some(args) = entry.get("args").and_then(|v| v.as_array()) else {
        return false;
    };
    args.iter().filter_map(|a| a.as_str()).any(|a| {
        a == "--mcp-stdio" || a.ends_with("/mcp-stdio.sh") || a == "./mcp-stdio.sh"
    })
}

/// Build `hooks.json` in Antigravity's named-hook format (verified on agy 1.5.13).
/// Flat handler arrays for SessionStart / PreInvocation / PostInvocation / Stop;
/// `{ matcher, hooks }` groups for PreToolUse / PostToolUse. Passes `--event <Name>` so
/// `agy-hook.sh` classifies each event authoritatively without scanning untrusted tool args.
pub fn build_hooks_json(baked_port: Option<u16>) -> Value {
    let handler = |event: &str, timeout: u64| {
        let cmd = match baked_port {
            Some(p) => format!("bash ./hooks/agy-hook.sh --event {event} {p}"),
            None => format!("bash ./hooks/agy-hook.sh --event {event}"),
        };
        json!({
            "type": "command",
            "command": cmd,
            "timeout": timeout,
        })
    };
    let tool_group = |event: &str| {
        json!([{
            "matcher": "*",
            "hooks": [handler(event, 5)],
        }])
    };
    json!({
        "maiterm": {
            "SessionStart": [handler("SessionStart", 5)],
            "PreInvocation": [handler("PreInvocation", 5)],
            "PostInvocation": [handler("PostInvocation", 5)],
            "PreToolUse": tool_group("PreToolUse"),
            "PostToolUse": tool_group("PostToolUse"),
            "Stop": [handler("Stop", 5)],
        }
    })
}

/// Safely read-modify-write a shared JSON object file (`~/.gemini/config/mcp_config.json` or
/// `~/.gemini/antigravity-cli/settings.json`) under `util::json_file::write_lock()` with
/// `stamp` + `replace_if_unchanged` (preserving symlinks, concurrent `mcp_proxy::entries`
/// edits, and `agy` writes). Refuses malformed or non-object JSON.
fn edit_json_object(
    path: &Path,
    create_if_absent: bool,
    f: impl FnMut(&mut Value) -> Result<bool, String>,
) -> Result<bool, String> {
    crate::util::json_file::edit_json_object(path, create_if_absent, f)
}

/// Merge our `mcpServers.maiterm` entry into `~/.gemini/config/mcp_config.json`.
fn upsert_mcp_config(path: &Path, entry: Value) -> Result<bool, String> {
    edit_json_object(path, true, |doc| {
        let obj = doc.as_object_mut().unwrap();
        let servers = obj.entry("mcpServers").or_insert_with(|| json!({}));
        let Some(servers_obj) = servers.as_object_mut() else {
            return Err(format!("refusing to edit {}: mcpServers is not an object", path.display()));
        };
        if servers_obj.get(MCP_SERVER_KEY) == Some(&entry) {
            return Ok(false);
        }
        // Don't clobber a non-maiterm user entry under the same key.
        if let Some(existing) = servers_obj.get(MCP_SERVER_KEY) {
            if !is_our_mcp_entry(existing) {
                log::warn!(
                    "Agy install: leaving existing custom mcpServers.{} in {} untouched",
                    MCP_SERVER_KEY,
                    path.display()
                );
                return Ok(false);
            }
        }
        servers_obj.insert(MCP_SERVER_KEY.to_string(), entry.clone());
        Ok(true)
    })
}

fn remove_mcp_config(path: &Path) -> Result<bool, String> {
    edit_json_object(path, false, |doc| {
        let Some(servers) = doc.get_mut("mcpServers").and_then(|v| v.as_object_mut()) else {
            return Ok(false);
        };
        if !servers.get(MCP_SERVER_KEY).is_some_and(is_our_mcp_entry) {
            return Ok(false);
        }
        servers.remove(MCP_SERVER_KEY);
        Ok(true)
    })
}

/// Ensure `"mcp(maiterm/*)"` is in `~/.gemini/antigravity-cli/settings.json` → `permissions.allow`.
/// Preserves all other settings and user permission rules (and `agy`'s Deny > Ask > Allow order
/// still lets the user override any specific tool).
fn upsert_allow_permission(path: &Path) -> Result<bool, String> {
    edit_json_object(path, true, |doc| {
        let obj = doc.as_object_mut().unwrap();
        let perms = obj.entry("permissions").or_insert_with(|| json!({}));
        let Some(perms_obj) = perms.as_object_mut() else {
            return Err(format!("refusing to edit {}: permissions is not an object", path.display()));
        };
        let allow = perms_obj.entry("allow").or_insert_with(|| json!([]));
        let Some(allow_arr) = allow.as_array_mut() else {
            return Err(format!("refusing to edit {}: permissions.allow is not an array", path.display()));
        };
        if allow_arr.iter().any(|v| v.as_str() == Some(MCP_ALLOW_RULE)) {
            return Ok(false);
        }
        allow_arr.push(Value::String(MCP_ALLOW_RULE.to_string()));
        Ok(true)
    })
}

fn remove_allow_permission(path: &Path) -> Result<bool, String> {
    edit_json_object(path, false, |doc| {
        let Some(allow_arr) = doc
            .get_mut("permissions")
            .and_then(|v| v.as_object_mut())
            .and_then(|p| p.get_mut("allow"))
            .and_then(|v| v.as_array_mut())
        else {
            return Ok(false);
        };
        let before = allow_arr.len();
        allow_arr.retain(|v| v.as_str() != Some(MCP_ALLOW_RULE));
        Ok(allow_arr.len() != before)
    })
}

/// Write or update all files managed by `AgyRegistrar`. Returns `Ok(true)` if any file changed.
fn install_into(
    paths: &AgyPaths,
    exe: &str,
    hooks_enabled: bool,
    keep_shared_hooks: bool,
) -> Result<bool, String> {
    fs::create_dir_all(&paths.plugin_dir).map_err(|e| format!("create_dir_all: {e}"))?;
    let mut changed = false;

    let manifest = serde_json::to_string_pretty(&json!({ "name": PLUGIN_NAME }))
        .map_err(|e| format!("serialize plugin.json: {e}"))?;
    changed |= write_if_changed(&paths.plugin_dir.join("plugin.json"), &manifest, false)?;

    // Remove legacy plugin-scoped mcp_config.json (which caused `maiterm_maiterm/<tool>` naming).
    let legacy_plugin_mcp = paths.plugin_dir.join("mcp_config.json");
    if legacy_plugin_mcp.exists() {
        let _ = fs::remove_file(&legacy_plugin_mcp);
        changed = true;
    }

    #[cfg(unix)]
    {
        let wrapper_path = paths.plugin_dir.join("mcp-stdio.sh");
        let wrapper = build_mcp_stdio_wrapper(exe);
        // When a sibling maiTerm (dev + prod) is live, avoid 30s reassert ping-pong over
        // fallback_exe as long as a valid executable wrapper using `[ -n "$bin" ] || bin=` exists.
        let keep_sibling_wrapper = keep_shared_hooks
            && fs::read_to_string(&wrapper_path)
                .ok()
                .is_some_and(|s| s.contains("exec \"$bin\" --mcp-stdio \"$@\"") && s.contains("[ -n \"$bin\" ] || bin='"));
        if !keep_sibling_wrapper {
            changed |= write_if_changed(&wrapper_path, &wrapper, true)?;
        }
    }

    // Always-inlined plugin rule giving agy immediate awareness of maiTerm tools from turn 0.
    let rules_path = paths.plugin_dir.join("rules").join("AGENTS.md");
    changed |= write_if_changed(&rules_path, AGY_AGENTS_MD, false)?;

    let skill_path = paths.plugin_dir.join("skills").join("maiterm").join("SKILL.md");
    changed |= write_if_changed(&skill_path, MAITERM_SKILL_MD, false)?;

    let hooks_json_path = paths.plugin_dir.join("hooks.json");
    let shim_path = paths.plugin_dir.join("hooks").join("agy-hook.sh");
    if hooks_enabled {
        changed |= write_if_changed(&shim_path, AGY_HOOK_SHIM, true)?;
        let hooks_json = serde_json::to_string_pretty(&build_hooks_json(None))
            .map_err(|e| format!("serialize hooks.json: {e}"))?;
        changed |= write_if_changed(&hooks_json_path, &hooks_json, false)?;
    } else if !keep_shared_hooks {
        if hooks_json_path.exists() {
            fs::remove_file(&hooks_json_path).map_err(|e| format!("remove hooks.json: {e}"))?;
            changed = true;
        }
        if shim_path.exists() {
            let _ = fs::remove_file(&shim_path);
            changed = true;
        }
    }

    // Register global MCP entry (`maiterm/<tool>`) and wildcard allow rule (`mcp(maiterm/*)`).
    // Log-and-continue on malformed user JSON so the rest of the plugin still installs cleanly.
    match upsert_mcp_config(&paths.mcp_config, build_mcp_server_entry(&paths.plugin_dir, exe)) {
        Ok(c) => changed |= c,
        Err(e) => log::error!("Agy install: not updating mcp_config.json — {}", e),
    }
    match upsert_allow_permission(&paths.cli_settings) {
        Ok(c) => changed |= c,
        Err(e) => log::error!("Agy install: not updating settings.json — {}", e),
    }

    Ok(changed)
}

fn unregister_from(paths: &AgyPaths) -> Result<(), String> {
    if let Err(e) = remove_mcp_config(&paths.mcp_config) {
        log::warn!("Agy unregister: mcp_config.json: {}", e);
    }
    if let Err(e) = remove_allow_permission(&paths.cli_settings) {
        log::warn!("Agy unregister: settings.json: {}", e);
    }
    if paths.plugin_dir.exists() {
        fs::remove_dir_all(&paths.plugin_dir).map_err(|e| format!("remove_dir_all: {e}"))?;
    }
    Ok(())
}

fn write_if_changed(path: &Path, content: &str, executable: bool) -> Result<bool, String> {
    if let Ok(existing) = fs::read_to_string(path) {
        if existing == content {
            #[cfg(unix)]
            if executable {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(meta) = fs::metadata(path) {
                    if meta.permissions().mode() & 0o111 == 0 {
                        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
                        return Ok(true);
                    }
                }
            }
            return Ok(false);
        }
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| format!("mkdir {}: {e}", parent.display()))?;
    }
    static TMP_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = TMP_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = path.with_extension(format!("tmp-{}-{}", std::process::id(), seq));
    fs::write(&tmp, content).map_err(|e| format!("write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("chmod {}: {e}", tmp.display()))?;
    }
    fs::rename(&tmp, path).map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_home() -> PathBuf {
        std::env::temp_dir().join(format!("maiterm-agy-test-{}", uuid::Uuid::new_v4()))
    }

    #[test]
    fn install_is_idempotent_merges_configs_and_cleans_up_on_unregister() {
        let home = temp_home();
        let paths = AgyPaths::under(&home);

        // Pre-seed user configs to confirm we preserve existing MCP servers and settings.
        fs::create_dir_all(paths.mcp_config.parent().unwrap()).unwrap();
        fs::write(
            &paths.mcp_config,
            r#"{"mcpServers":{"user-db":{"command":"sqlite-mcp"}}}"#,
        )
        .unwrap();
        fs::create_dir_all(paths.cli_settings.parent().unwrap()).unwrap();
        fs::write(
            &paths.cli_settings,
            r#"{"colorScheme":"dark","permissions":{"allow":["read_file(*)"],"deny":["command(rm -rf)"]}}"#,
        )
        .unwrap();
        // Seed a legacy plugin mcp_config.json to confirm migration deletes it.
        fs::create_dir_all(&paths.plugin_dir).unwrap();
        fs::write(paths.plugin_dir.join("mcp_config.json"), "{}").unwrap();

        let changed1 = install_into(&paths, "/usr/local/bin/aiterm", true, false).unwrap();
        assert!(changed1);
        assert!(paths.plugin_dir.join("plugin.json").exists());
        assert!(!paths.plugin_dir.join("mcp_config.json").exists(), "legacy plugin mcp_config.json removed");
        assert!(paths.plugin_dir.join("rules/AGENTS.md").exists());
        let agents_md = fs::read_to_string(paths.plugin_dir.join("rules/AGENTS.md")).unwrap();
        assert!(agents_md.contains("maiterm/listStack({})"));
        assert!(agents_md.contains("replyToOverlord({ kind:"));
        assert!(agents_md.contains("sendToBridgedAgent({ message,"));
        assert!(agents_md.contains("createFollowUp({ text,"));
        assert!(!agents_md.contains("in_progress"), "createTasks status enum uses active, not in_progress");
        assert!(!agents_md.contains("replace\"|\"append"), "setTabNotes mode is source|render, not replace|append");
        assert!(agents_md.contains("setTabNotes({ notes, mode?: \"source\"|\"render\" })"));
        assert!(agents_md.contains("switchTab({ tabId })"));
        assert!(agents_md.contains("status?: [...]"));
        assert!(paths.plugin_dir.join("hooks.json").exists());
        assert!(paths.plugin_dir.join("hooks/agy-hook.sh").exists());
        assert!(paths.plugin_dir.join("skills/maiterm/SKILL.md").exists());

        // Global mcp_config.json has both user-db and maiterm.
        let mcp: Value = serde_json::from_str(&fs::read_to_string(&paths.mcp_config).unwrap()).unwrap();
        assert_eq!(mcp["mcpServers"]["user-db"]["command"], "sqlite-mcp");
        assert!(mcp["mcpServers"]["maiterm"].is_object());

        // Global settings.json added mcp(maiterm/*) while preserving everything else.
        let settings: Value = serde_json::from_str(&fs::read_to_string(&paths.cli_settings).unwrap()).unwrap();
        assert_eq!(settings["colorScheme"], "dark");
        let allow: Vec<&str> = settings["permissions"]["allow"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(allow, vec!["read_file(*)", "mcp(maiterm/*)"]);
        assert_eq!(settings["permissions"]["deny"][0], "command(rm -rf)");

        // Second pass writes nothing.
        let changed2 = install_into(&paths, "/usr/local/bin/aiterm", true, false).unwrap();
        assert!(!changed2, "reassert must not rewrite unchanged files");

        // Turning hooks off with no sibling removes hooks.json and shim.
        let changed3 = install_into(&paths, "/usr/local/bin/aiterm", false, false).unwrap();
        assert!(changed3);
        assert!(!paths.plugin_dir.join("hooks.json").exists());
        assert!(!paths.plugin_dir.join("hooks/agy-hook.sh").exists());

        // Turning hooks back on, then off with a sibling live preserves hooks.json.
        install_into(&paths, "/usr/local/bin/aiterm", true, false).unwrap();
        let changed4 = install_into(&paths, "/usr/local/bin/aiterm", false, true).unwrap();
        assert!(!changed4);
        assert!(paths.plugin_dir.join("hooks.json").exists());

        // Unregister removes plugin dir, our mcp entry, and our allow rule, preserving user entries.
        unregister_from(&paths).unwrap();
        assert!(!paths.plugin_dir.exists());
        let mcp_after: Value = serde_json::from_str(&fs::read_to_string(&paths.mcp_config).unwrap()).unwrap();
        assert_eq!(mcp_after["mcpServers"]["user-db"]["command"], "sqlite-mcp");
        assert!(mcp_after["mcpServers"].get("maiterm").is_none());
        let settings_after: Value = serde_json::from_str(&fs::read_to_string(&paths.cli_settings).unwrap()).unwrap();
        let allow_after: Vec<&str> = settings_after["permissions"]["allow"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
        assert_eq!(allow_after, vec!["read_file(*)"]);

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn refuses_to_clobber_malformed_user_json() {
        let home = temp_home();
        let paths = AgyPaths::under(&home);
        fs::create_dir_all(paths.mcp_config.parent().unwrap()).unwrap();
        fs::write(&paths.mcp_config, "{ not valid json").unwrap();
        fs::create_dir_all(paths.cli_settings.parent().unwrap()).unwrap();
        fs::write(&paths.cli_settings, "{ broken").unwrap();

        install_into(&paths, "/usr/local/bin/aiterm", true, false).unwrap();
        assert_eq!(fs::read_to_string(&paths.mcp_config).unwrap(), "{ not valid json");
        assert_eq!(fs::read_to_string(&paths.cli_settings).unwrap(), "{ broken");

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn hooks_json_matches_agy_named_group_schema() {
        let v = build_hooks_json(None);
        let group = v.get("maiterm").expect("named hook group");
        assert!(group["SessionStart"].is_array());
        assert!(group["PreInvocation"].is_array());
        assert!(group["PostInvocation"].is_array());
        assert!(group["Stop"].is_array());
        assert_eq!(group["PreToolUse"][0]["matcher"], "*");
        assert_eq!(group["PostToolUse"][0]["matcher"], "*");
        assert_eq!(
            group["PreToolUse"][0]["hooks"][0]["command"],
            "bash ./hooks/agy-hook.sh --event PreToolUse"
        );
        assert_eq!(
            group["SessionStart"][0]["command"],
            "bash ./hooks/agy-hook.sh --event SessionStart"
        );
    }

    #[cfg(unix)]
    #[test]
    fn agy_hook_shim_outputs_empty_for_pre_tool_use_and_empty_json_for_stop() {
        use std::io::Write;
        use std::process::{Command, Stdio};

        let home = temp_home();
        let paths = AgyPaths::under(&home);
        install_into(&paths, "/bin/true", true, false).unwrap();
        let shim = paths.plugin_dir.join("hooks/agy-hook.sh");

        let run = |extra_args: &[&str], payload: &str| {
            let mut child = Command::new("bash")
                .arg(&shim)
                .args(extra_args)
                .env_remove("MAITERM_PORT")
                .env_remove("MAITERM_AUTH")
                .env_remove("MAITERM_TAB_ID")
                .env("HOME", &home)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(payload.as_bytes()).unwrap();
            let out = child.wait_with_output().unwrap();
            assert!(out.status.success());
            String::from_utf8(out.stdout).unwrap()
        };

        // CRITICAL: PreToolUse MUST print nothing (printing {} causes agy to hard-deny the call),
        // even if tool args contain `"hookEventName":"SessionStart"`.
        assert_eq!(run(&["--event", "PreToolUse"], r#"{"hookEventName":"PreToolUse","toolCall":{"args":{"text":"\"hookEventName\":\"SessionStart\""}}}"#), "");
        assert_eq!(run(&[], r#"{"hookEventName":"PreToolUse","toolCall":{"args":{"text":"\"hookEventName\":\"SessionStart\""}}}"#), "");
        assert_eq!(run(&["--event", "Stop"], r#"{"hookEventName":"Stop","conversationId":"c1"}"#), "{}");
        assert_eq!(run(&["--event", "SessionStart"], r#"{"hookEventName":"SessionStart","conversationId":"c1"}"#), "{}");

        // Verify mcp-stdio.sh fallback_exe works when MAITERM_BIN is unset (no literal single quotes!).
        let fake_bin = home.join("fake-aiterm.sh");
        fs::write(&fake_bin, "#!/usr/bin/env bash\nprintf 'FALLBACK_OK:%s' \"$*\"\n").unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&fake_bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        install_into(&paths, &fake_bin.to_string_lossy(), true, false).unwrap();
        let wrapper = paths.plugin_dir.join("mcp-stdio.sh");
        let out = Command::new("bash")
            .arg(&wrapper)
            .args(["--path", "/mcp/x/linear"])
            .env_remove("MAITERM_BIN")
            .env("HOME", &home)
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout).unwrap(),
            "FALLBACK_OK:--mcp-stdio --path /mcp/x/linear"
        );

        let _ = fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn install_preserves_symlinked_json_configs() {
        use std::os::unix::fs::symlink;

        let home = temp_home();
        let paths = AgyPaths::under(&home);
        let dotfiles = home.join("dotfiles");
        fs::create_dir_all(&dotfiles).unwrap();
        let real_mcp = dotfiles.join("mcp_config.json");
        let real_settings = dotfiles.join("settings.json");
        fs::write(&real_mcp, r#"{"mcpServers":{}}"#).unwrap();
        fs::write(&real_settings, r#"{"colorScheme":"dark"}"#).unwrap();

        fs::create_dir_all(paths.mcp_config.parent().unwrap()).unwrap();
        fs::create_dir_all(paths.cli_settings.parent().unwrap()).unwrap();
        symlink(&real_mcp, &paths.mcp_config).unwrap();
        symlink(&real_settings, &paths.cli_settings).unwrap();

        install_into(&paths, "/usr/local/bin/aiterm", true, false).unwrap();

        assert!(fs::symlink_metadata(&paths.mcp_config).unwrap().file_type().is_symlink());
        assert!(fs::symlink_metadata(&paths.cli_settings).unwrap().file_type().is_symlink());
        let mcp: Value = serde_json::from_str(&fs::read_to_string(&real_mcp).unwrap()).unwrap();
        assert!(mcp["mcpServers"]["maiterm"].is_object());
        let settings: Value = serde_json::from_str(&fs::read_to_string(&real_settings).unwrap()).unwrap();
        assert_eq!(settings["permissions"]["allow"][0], "mcp(maiterm/*)");

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn agy_agents_md_signatures_match_protocol_schemas() {
        use std::collections::HashMap;
        use super::super::protocol::{tool_list_response, ToolGates};

        let tools = tool_list_response(ToolGates {
            tasks: true,
            stack: true,
            follow_ups_live: true,
            escalations_to_overlord: true,
            browser_link: true,
            file_tree: true,
            hive: None,
        });
        let by_name: HashMap<&str, &Value> = tools["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| Some((t["name"].as_str()?, t)))
            .collect();

        // Check every backticked `toolName({ ... })` (and `maiterm/toolName({ ... })`) signature in
        // AGY_AGENTS_MD against the real MCP inputSchema in protocol.rs: tool must exist, every
        // required argument must be named in the signature, and every top-level key named in the
        // signature must exist in inputSchema.properties.
        let mut checked = 0usize;
        for chunk in AGY_AGENTS_MD.split('`').skip(1).step_by(2) {
            let Some((head, rest)) = chunk.split_once("({") else { continue };
            let tool = head.strip_prefix("maiterm/").unwrap_or(head);
            if tool.is_empty() || !tool.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                continue;
            }
            let schema = by_name
                .get(tool)
                .unwrap_or_else(|| panic!("AGY_AGENTS_MD references unknown MCP tool `{tool}`"));
            let props = schema["inputSchema"]["properties"].as_object().unwrap();
            let required: Vec<&str> = schema["inputSchema"]["required"]
                .as_array()
                .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
                .unwrap_or_default();

            // Extract top-level parameter identifiers from inside `{ ... }` (depth 1).
            let mut depth = 1usize;
            let mut in_str = false;
            let mut top_keys: Vec<String> = Vec::new();
            let mut cur = String::new();
            let mut expect_key = true;
            for ch in rest.chars() {
                if ch == '"' {
                    in_str = !in_str;
                    continue;
                }
                if in_str {
                    continue;
                }
                match ch {
                    '{' | '[' => {
                        depth += 1;
                    }
                    '}' | ']' => {
                        depth = depth.saturating_sub(1);
                        if depth == 0 {
                            if expect_key && !cur.is_empty() {
                                top_keys.push(std::mem::take(&mut cur));
                            }
                            break;
                        }
                    }
                    ',' | '|' if depth == 1 => {
                        if expect_key && !cur.is_empty() {
                            top_keys.push(std::mem::take(&mut cur));
                        }
                        expect_key = true;
                    }
                    ':' if depth == 1 => {
                        if expect_key && !cur.is_empty() {
                            top_keys.push(std::mem::take(&mut cur));
                        }
                        expect_key = false;
                    }
                    '?' | ' ' | '\t' if depth == 1 => {}
                    c if depth == 1 && expect_key && (c.is_ascii_alphanumeric() || c == '_') => {
                        cur.push(c);
                    }
                    _ => {}
                }
            }

            for req in &required {
                assert!(
                    top_keys.iter().any(|k| k == req),
                    "AGY_AGENTS_MD signature for `{tool}` (`{chunk}`) is missing required param `{req}`"
                );
            }
            for key in &top_keys {
                assert!(
                    props.contains_key(key.as_str()),
                    "AGY_AGENTS_MD signature for `{tool}` (`{chunk}`) names unknown param `{key}`"
                );
            }
            checked += 1;
        }
        assert!(checked >= 40, "expected to verify >=40 tool signatures in AGY_AGENTS_MD, checked {checked}");
    }
}
