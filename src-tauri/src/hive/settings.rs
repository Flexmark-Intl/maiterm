//! What a Hive agent launches with (docs/hive.md §6.7, §7.1, §7.3, §11.1): the `--settings`
//! file (sandbox, memory folder, permission rules, command hooks), the `--mcp-config` file, and
//! the hook script those hooks run.
//!
//! The coordinator's settings are the set that passed spike 13 plus the two additions it named
//! (maiTerm's MCP tools, each by name, and `git -C` rules), and the git option denies those
//! rules turned out to need (`GIT_ESCAPE_DENIES`). Each layer covers another's gap: every path
//! on the sandbox's `denyWrite` has a `Write(//…)`/`Edit(//…)` deny rule and the reverse, since
//! the Write tool ignores the sandbox and an Edit deny alone let `python3 -c open(…)` through
//! (spikes 10, 14).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::launch::{sh_quote, LaunchRecord};
use super::paths::{hive_bin_dir, HivePaths};
use super::record::Hive;
use super::ROLE_COORDINATOR;

/// The hook script, bundled at build time; `install_hook` writes it beside the port file.
pub const HOOK_SCRIPT: &str = include_str!("../../resources/hive/maiterm-hive-hook.sh");
pub const HOOK_FILE: &str = "maiterm-hive-hook";
pub const PORT_FILE: &str = "port";

/// Events a Claude older than the one in a tab may not know; an unknown event costs a Settings
/// Warning dialog on every launch, so they go in only where every installed Claude knows them
/// (`lockfile::events_every_claude_knows`).
pub const HIVE_NEWER_EVENTS: &[&str] = &["PreModelSwitch", "StopFailure"];

/// Forwarded to maiTerm, never blocking: the command-hook versions of maiTerm's global http
/// hooks, which fail silently in a sandboxed tab (spike 9), plus StopFailure (§7.8).
pub const OBSERVED_EVENTS: &[&str] = &[
    "SessionStart",
    "SessionEnd",
    "Notification",
    "Stop",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "PostToolUseFailure",
    "PreModelSwitch",
    "SubagentStop",
    "PreCompact",
    "StopFailure",
];

/// The tools the fail-closed gate sees (§6.8). Monitor joins once its sandboxing is proven.
pub const GATE_MATCHER: &str = "Bash|PowerShell|Write|Edit";
/// Under Claude's hook timeout, which the script's own curl deadline (8 s) stays under.
const GATE_TIMEOUT_SECS: u64 = 10;
const OBSERVE_TIMEOUT_SECS: u64 = 5;

/// The push refusals in the space-wildcard form: a mid-pattern `:*` matches nothing, and
/// `Bash(git push:*)` missed `git -C <wt> push` (spike 6). Speed bumps and an audit trail: what
/// stops a push is that no remote is reachable (§7.3).
const PUSH_DENIES: &[&str] = &[
    "Bash(git * push*)",
    "Bash(git push*)",
    "Bash(git * send-pack*)",
    "Bash(git send-pack*)",
    "Bash(git remote*)",
    "Bash(gh pr merge*)",
];

/// The `*` in `git -C * diff` spans any global option, so a config value that runs a command
/// rode a review rule: `git -C <copy> -c core.fsmonitor=<cmd> diff` ran `<cmd>` (the 3e review).
/// The gate refuses these too (`gate::check_git`); these hold them with the gate answering allow.
const GIT_ESCAPE_DENIES: &[&str] = &[
    "Bash(git -c *)",
    "Bash(git * -c *)",
    "Bash(git *--config-env*)",
    "Bash(git *--exec-path=*)",
    "Bash(git *--git-dir*)",
    "Bash(git *--work-tree*)",
    "Bash(git *--ext-diff*)",
    "Bash(git *--textconv*)",
    "Bash(git *--output*)",
    "Bash(git *--no-index*)",
];

/// The maiTerm tools the coordinator is allowed, by name (§7.1: hive, gallery, task, notes). Never
/// the server wildcard: MCP calls are made by Claude's own process, outside the sandbox, and
/// `createService` + `startService` alone would type any command into an unsandboxed shell.
/// The hive and gallery tools arrive in later steps; until then these rules match nothing.
pub const COORDINATOR_MCP_TOOLS: &[&str] = &[
    "initSession",
    "listTasks",
    "createTasks",
    "updateTasks",
    "getTabNotes",
    "setTabNotes",
    "editTabNotes",
    "findNotes",
    "listWorkspaceNotes",
    "readWorkspaceNote",
    "writeWorkspaceNote",
    "hiveSpawnWorker",
    "hiveListWorkers",
    "hiveMessageWorker",
    "hiveLand",
    "hiveUndoLand",
    "hiveStopWorker",
    "hiveRestartWorker",
    "hiveFinish",
    "hiveAnswerWorker",
    "hiveRunPriority",
    "hivePrecedents",
    "hiveRecordPrecedent",
    "galleryList",
    "galleryGet",
    "galleryCollect",
    "galleryTag",
    "galleryAdd",
    "galleryDeliver",
];

/// Denied as well, a backstop to dontAsk's "not listed is denied": the tools that act outside
/// the sandbox (run a command, write a file, drive a browser or another tab, reach out of the
/// machine) or change maiTerm itself.
pub const ESCAPE_MCP_TOOLS: &[&str] = &[
    "createService",
    "updateService",
    "removeService",
    "startService",
    "restartService",
    "startStack",
    "saveDocument",
    "openFile",
    "browserNavigate",
    "browserClick",
    "browserType",
    "browserPressKey",
    "browserEval",
    "setPreference",
    "setAutoResume",
    "setTriggerVariable",
    "driveTab",
    "answerTabPrompt",
    "sendToBridgedAgent",
    "createFollowUp",
    "postCommsReply",
    "startCommsThread",
    "sendFilesToPhone",
    "createTab",
    "restartMaiterm",
    "addMcpServer",
];

/// Where the hooks find their script, and which recent events to install.
#[derive(Debug, Clone)]
pub struct HookEnv {
    pub script: PathBuf,
    pub bin_dir: PathBuf,
    pub known_newer: Vec<&'static str>,
}

impl HookEnv {
    pub fn current() -> Result<Self, String> {
        let bin_dir = hive_bin_dir().ok_or("Couldn't find maiTerm's data folder")?;
        let script = bin_dir.join(HOOK_FILE);
        if !script.is_file() {
            return Err("The Hive hook script isn't installed; restart maiTerm and try again.".to_string());
        }
        Ok(Self { script, bin_dir, known_newer: crate::claude_code::lockfile::events_every_claude_knows(HIVE_NEWER_EVENTS) })
    }
}

/// A path on `denyWrite`, and whether its deny rules cover a folder (`/**`) or one file.
#[derive(Debug, Clone, PartialEq)]
pub struct DenyPath {
    pub path: PathBuf,
    pub dir: bool,
}

/// `Write(//abs/**)`: an absolute path needs the `//` form, `Edit(/abs/**)` matches nothing
/// (spike 14).
pub fn abs_rule(tool: &str, p: &Path, dir: bool) -> String {
    let s = p.to_string_lossy();
    let body = s.trim_start_matches('/');
    format!("{tool}(//{body}{})", if dir { "/**" } else { "" })
}

/// The coordinator's `denyWrite` (§7.1, §6.7): the repos and its read copies, every local-path
/// remote (§7.3), the hook script's folder, the gallery, the Hive's Rust-owned folders, and its
/// home's project settings, which would otherwise load unseen and outside the sandbox on its
/// next launch.
pub fn deny_write_paths(hive: &Hive, paths: &HivePaths, bin_dir: &Path) -> Vec<DenyPath> {
    let mut out: Vec<DenyPath> = Vec::new();
    let mut add = |path: PathBuf, dir: bool| {
        if !out.iter().any(|d| d.path == path) {
            out.push(DenyPath { path, dir });
        }
    };
    for f in &hive.folders {
        add(PathBuf::from(&f.path), true);
        if f.is_git() {
            add(paths.coordinator_worktree(&f.name), true);
        }
        for r in &f.local_remotes {
            add(PathBuf::from(r), true);
        }
    }
    add(bin_dir.to_path_buf(), true);
    add(paths.gallery(), true);
    add(paths.tasks(), true);
    add(paths.logs(), true);
    add(paths.precedents(), true);
    add(paths.coordinator_home().join(".claude"), true);
    add(paths.coordinator_home().join(".mcp.json"), false);
    out
}

/// The hooks block: every entry `type:"command"`, the gate with `onFailure:"block"` (§6.8).
pub fn hive_hooks(script: &Path, known_newer: &[&str]) -> Value {
    let script = sh_quote(&script.to_string_lossy());
    let mut hooks = serde_json::Map::new();
    for event in OBSERVED_EVENTS {
        if HIVE_NEWER_EVENTS.contains(event) && !known_newer.contains(event) {
            continue;
        }
        let observe = json!({
            "matcher": "",
            "hooks": [{ "type": "command", "command": format!("{script} observe {event}"), "timeout": OBSERVE_TIMEOUT_SECS }]
        });
        let entries = if *event == "PreToolUse" {
            json!([
                {
                    "matcher": GATE_MATCHER,
                    "hooks": [{ "type": "command", "command": format!("{script} gate"), "timeout": GATE_TIMEOUT_SECS, "onFailure": "block" }]
                },
                observe
            ])
        } else {
            json!([observe])
        };
        hooks.insert(event.to_string(), entries);
    }
    Value::Object(hooks)
}

/// The `--settings` file for one launch. Coordinator only until workers arrive (build step 4).
pub fn hive_settings_json(hive: &Hive, record: &LaunchRecord, paths: &HivePaths, hooks: &HookEnv) -> Result<Value, String> {
    if record.role != ROLE_COORDINATOR {
        return Err(format!("No settings for a Hive \"{}\" yet", record.role));
    }
    let home = paths.coordinator_home();
    let deny = deny_write_paths(hive, paths, &hooks.bin_dir);
    let mcp = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);

    // Plain `git diff` from home would find only a repo the agent wrote, whose config runs a
    // command; Claude allows its read-only git verbs with no rule at all (the 3e probe), so it's
    // the gate that refuses git outside the read copies, not the absence of these.
    let mut allow: Vec<String> = ["Bash(git diff *)", "Bash(git log *)", "Bash(git show *)"].map(String::from).to_vec();
    // Its cwd is home, not a repo, so a review names the copy (§7.1). A trailing ` *` is optional
    // only in a pattern with no other wildcard: `git -C * diff *` refused a bare
    // `git -C <copy> diff` (the 3e probe), so each verb also gets its bare form.
    for verb in ["diff", "log", "show"] {
        allow.push(format!("Bash(git -C * {verb})"));
        allow.push(format!("Bash(git -C * {verb} *)"));
    }
    allow.push(abs_rule("Write", &home, true));
    allow.push(abs_rule("Edit", &home, true));
    // dontAsk denies every tool not listed, maiTerm's own included.
    allow.extend(COORDINATOR_MCP_TOOLS.iter().map(|t| format!("mcp__{mcp}__{t}")));

    let mut deny_rules: Vec<String> = vec!["WebFetch".into(), "WebSearch".into()];
    deny_rules.extend(PUSH_DENIES.iter().chain(GIT_ESCAPE_DENIES).map(|s| s.to_string()));
    deny_rules.extend(ESCAPE_MCP_TOOLS.iter().map(|t| format!("mcp__{mcp}__{t}")));
    for d in &deny {
        deny_rules.push(abs_rule("Write", &d.path, d.dir));
        deny_rules.push(abs_rule("Edit", &d.path, d.dir));
    }

    let path_str = |p: &Path| p.to_string_lossy().to_string();
    Ok(json!({
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            // Default true, which ran any sandboxed Bash regardless of the allow rules (spike 13).
            "autoAllowBashIfSandboxed": false,
            "filesystem": {
                "allowWrite": [path_str(&home)],
                "denyWrite": deny.iter().map(|d| path_str(&d.path)).collect::<Vec<_>>(),
            },
            // It reaches generators and maiTerm only as MCP servers, made by Claude's own process.
            "network": { "allowedDomains": [], "strictAllowlist": true },
        },
        // Absolute, the only kind Claude honours; re-passed on every launch or the first
        // compaction moves memory to the human's per-repo folder (§9, spike 1).
        "autoMemoryDirectory": path_str(&paths.memory()),
        "permissions": { "allow": allow, "deny": deny_rules },
        "hooks": hive_hooks(&hooks.script, &hooks.known_newer),
    }))
}

/// The `--mcp-config` file: maiTerm's server only, under `--strict-mcp-config`. Nothing baked:
/// the port, token and tab expand from the tab's environment in the url and headers (the
/// expansion table in claude_code/CLAUDE.md), and a sandboxed Claude reached it this way.
pub fn hive_mcp_config() -> Value {
    let key = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
    json!({
        "mcpServers": {
            key: {
                "type": "http",
                "url": "http://127.0.0.1:${MAITERM_PORT}/mcp",
                "headers": {
                    "x-claude-code-ide-authorization": "${MAITERM_AUTH}",
                    "x-maiterm-tab": "${MAITERM_TAB_ID}"
                }
            }
        }
    })
}

/// Write the hook script (only when it changed) and the port it reads, on every server bind.
/// The script finds maiTerm's port here on every call, so a restart can't strand a tab (§6.8).
pub fn install_hook(bin_dir: &Path, port: u16) -> Result<(), String> {
    fs::create_dir_all(bin_dir).map_err(|e| format!("{}: {e}", bin_dir.display()))?;
    let script = bin_dir.join(HOOK_FILE);
    super::launch::write_bytes(&script, HOOK_SCRIPT.as_bytes())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).map_err(|e| format!("{}: {e}", script.display()))?;
    }
    super::launch::write_bytes(&bin_dir.join(PORT_FILE), port.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::launch::coordinator_record;

    fn env() -> HookEnv {
        HookEnv {
            script: PathBuf::from("/Users/x/Library/Application Support/com.aiterm.dev/hive/bin/maiterm-hive-hook"),
            bin_dir: PathBuf::from("/Users/x/Library/Application Support/com.aiterm.dev/hive/bin"),
            known_newer: HIVE_NEWER_EVENTS.to_vec(),
        }
    }

    fn settings() -> Value {
        let (h, p) = (crate::hive::launch::tests::sample_hive(), crate::hive::launch::tests::sample_paths());
        hive_settings_json(&h, &coordinator_record(&h, &p), &p, &env()).unwrap()
    }

    fn strings(v: &Value) -> Vec<String> {
        v.as_array().unwrap().iter().map(|s| s.as_str().unwrap().to_string()).collect()
    }

    #[test]
    fn every_hook_is_a_command_and_the_gate_blocks_on_failure() {
        let s = settings();
        let hooks = s["hooks"].as_object().unwrap();
        for event in OBSERVED_EVENTS {
            assert!(hooks.contains_key(*event), "{event}");
        }
        for (event, entries) in hooks {
            for e in entries.as_array().unwrap() {
                for h in e["hooks"].as_array().unwrap() {
                    assert_eq!(h["type"], "command", "{event}");
                    assert!(h["command"].as_str().unwrap().starts_with("'/Users/x/Library/Application Support/"), "quoted script path");
                }
            }
        }
        let pre = hooks["PreToolUse"].as_array().unwrap();
        let gate = pre.iter().find(|e| e["matcher"] == GATE_MATCHER).expect("a gate entry");
        assert_eq!(gate["hooks"][0]["onFailure"], "block");
        assert!(gate["hooks"][0]["command"].as_str().unwrap().ends_with(" gate"));
        assert!(gate["hooks"][0]["timeout"].as_u64().unwrap() <= 10);
        // The observing copy never blocks: only the gate may.
        let blocking = hooks.values().flat_map(|v| v.as_array().unwrap()).filter(|e| e["hooks"][0].get("onFailure").is_some()).count();
        assert_eq!(blocking, 1);
    }

    #[test]
    fn newer_events_go_in_only_when_every_claude_knows_them() {
        let h = hive_hooks(Path::new("/s"), &["PreModelSwitch"]);
        assert!(h.get("PreModelSwitch").is_some());
        assert!(h.get("StopFailure").is_none());
        assert!(h.get("Stop").is_some());
        assert!(hive_hooks(Path::new("/s"), &[]).get("PreModelSwitch").is_none());
    }

    #[test]
    fn the_sandbox_holds_the_three_settings_spike_13_named() {
        let s = settings();
        let sb = &s["sandbox"];
        assert_eq!(sb["enabled"], true);
        assert_eq!(sb["failIfUnavailable"], true);
        assert_eq!(sb["allowUnsandboxedCommands"], false);
        assert_eq!(sb["autoAllowBashIfSandboxed"], false);
        assert_eq!(sb["network"]["strictAllowlist"], true);
        assert_eq!(sb["network"]["allowedDomains"], json!([]));
        let p = crate::hive::launch::tests::sample_paths();
        assert_eq!(strings(&sb["filesystem"]["allowWrite"]), vec![p.coordinator_home().to_string_lossy().to_string()]);
        let mem = s["autoMemoryDirectory"].as_str().unwrap();
        assert!(Path::new(mem).is_absolute() && mem.ends_with("/memory"), "{mem}");
    }

    #[test]
    fn permissions_are_the_whole_list_and_use_forms_claude_matches() {
        let s = settings();
        let allow = strings(&s["permissions"]["allow"]);
        let deny = strings(&s["permissions"]["deny"]);
        for r in ["Bash(git diff *)", "Bash(git -C * show *)", "Bash(git -C * diff)", "Bash(git -C * log)", "WebFetch", "WebSearch", "Bash(git * push*)", "Bash(git remote*)", "Bash(gh pr merge*)"] {
            assert!(allow.contains(&r.to_string()) || deny.contains(&r.to_string()), "{r}");
        }
        assert!(deny.contains(&"WebFetch".to_string()) && !allow.contains(&"WebFetch".to_string()));
        // A review rule's `*` spans `-c core.fsmonitor=<cmd>`; the denies take it back.
        for r in ["Bash(git * -c *)", "Bash(git -c *)", "Bash(git *--config-env*)", "Bash(git *--ext-diff*)", "Bash(git *--no-index*)", "Bash(git *--output*)"] {
            assert!(deny.contains(&r.to_string()), "{r}");
        }
        let mcp = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
        assert!(allow.contains(&format!("mcp__{mcp}__listTasks")));
        // Named tools only: a server wildcard hands it createService + startService, a command
        // typed into an unsandboxed shell.
        for r in allow.iter().filter(|r| r.starts_with("mcp__")) {
            assert!(!r.contains('*'), "a wildcard MCP allow: {r}");
        }
        for t in ESCAPE_MCP_TOOLS {
            let rule = format!("mcp__{mcp}__{t}");
            assert!(deny.contains(&rule) && !allow.contains(&rule), "{rule}");
        }
        assert!(!COORDINATOR_MCP_TOOLS.iter().any(|t| ESCAPE_MCP_TOOLS.contains(t)));
        for r in deny.iter().chain(&allow) {
            let inner = r.split_once('(').map(|(_, i)| i.trim_end_matches(')')).unwrap_or("");
            assert!(!inner.trim_end_matches(":*").contains(":*"), "a mid-pattern :* matches nothing: {r}");
            assert!(!inner.ends_with(":*"), "the colon form missed real pushes: {r}");
            if r.starts_with("Write(") || r.starts_with("Edit(") {
                assert!(inner.starts_with("//"), "absolute paths need the // form: {r}");
            }
        }
    }

    #[test]
    fn deny_write_and_the_write_edit_deny_rules_mirror_each_other() {
        let s = settings();
        let deny_write = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
        let deny = strings(&s["permissions"]["deny"]);
        let (h, p) = (crate::hive::launch::tests::sample_hive(), crate::hive::launch::tests::sample_paths());
        let listed = deny_write_paths(&h, &p, &env().bin_dir);
        assert_eq!(listed.len(), deny_write.len());
        for d in &listed {
            assert!(deny_write.contains(&d.path.to_string_lossy().to_string()));
            for tool in ["Write", "Edit"] {
                assert!(deny.contains(&abs_rule(tool, &d.path, d.dir)), "{tool} rule for {}", d.path.display());
            }
        }
        // The reverse: every Write/Edit deny rule names a denyWrite path.
        for r in deny.iter().filter(|r| r.starts_with("Write(") || r.starts_with("Edit(")) {
            let inner = r.split_once("(/").unwrap().1.trim_end_matches(')').trim_end_matches("/**");
            assert!(deny_write.iter().any(|d| d == inner), "{r} has no denyWrite twin");
        }
        for must in ["/r/web", "/r/brand assets", "/srv/web.git"] {
            assert!(deny_write.contains(&must.to_string()), "{must}");
        }
        for must in [p.coordinator_worktree("web"), p.gallery(), env().bin_dir, p.coordinator_home().join(".claude")] {
            assert!(deny_write.contains(&must.to_string_lossy().to_string()), "{}", must.display());
        }
        assert!(!deny_write.iter().any(|d| *d == p.coordinator_home().to_string_lossy()), "home stays writable");
    }

    #[test]
    fn the_mcp_config_names_only_maiterm_and_bakes_nothing() {
        let v = hive_mcp_config();
        let servers = v["mcpServers"].as_object().unwrap();
        assert_eq!(servers.len(), 1);
        let entry = servers.values().next().unwrap();
        assert_eq!(entry["url"], "http://127.0.0.1:${MAITERM_PORT}/mcp");
        assert_eq!(entry["headers"]["x-claude-code-ide-authorization"], "${MAITERM_AUTH}");
        assert_eq!(entry["headers"]["x-maiterm-tab"], "${MAITERM_TAB_ID}");
    }

    /// The automated stand-in for spike 9's fail-closed check: the real script, through a real
    /// socket. `sh` and `curl` are POSIX-only, as the hooks are.
    #[cfg(unix)]
    mod hook_script {
        use super::super::*;
        use std::io::{Read, Write};
        use std::net::TcpListener;
        use std::process::{Command, Stdio};

        fn bin() -> PathBuf {
            let t = std::env::temp_dir().join(format!("hive-hook-{}/hive/bin", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        }

        /// Answer one request with `status` and `body`; the request comes back as text.
        fn serve_once(status: &'static str, body: &'static str) -> (u16, std::thread::JoinHandle<String>) {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = l.local_addr().unwrap().port();
            let h = std::thread::spawn(move || {
                let (mut s, _) = l.accept().unwrap();
                let mut req = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = s.read(&mut buf).unwrap();
                    req.extend_from_slice(&buf[..n]);
                    let text = String::from_utf8_lossy(&req).to_string();
                    if let Some(end) = text.find("\r\n\r\n") {
                        let len = text
                            .lines()
                            .find_map(|l| {
                                let lower = l.to_ascii_lowercase();
                                lower.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())
                            })
                            .unwrap_or(0);
                        if req.len() >= end + 4 + len || n == 0 {
                            break;
                        }
                    }
                    if n == 0 {
                        break;
                    }
                }
                let reply = format!("HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = s.write_all(reply.as_bytes());
                String::from_utf8_lossy(&req).to_string()
            });
            (port, h)
        }

        fn closed_port() -> u16 {
            TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
        }

        fn run(bin: &Path, port: u16, args: &[&str]) -> (i32, String, String) {
            install_hook(bin, port).unwrap();
            let mut child = Command::new(bin.join(HOOK_FILE))
                .args(args)
                .env("MAITERM_TAB_ID", "tab-1")
                .env("MAITERM_AUTH", "tok-xyz")
                .env_remove("MAITERM_PORT")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("the installed script runs");
            child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"git push"}}"#).unwrap();
            let out = child.wait_with_output().unwrap();
            (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).to_string(), String::from_utf8_lossy(&out.stderr).to_string())
        }

        #[test]
        fn a_closed_port_blocks() {
            let b = bin();
            let (code, out, err) = run(&b, closed_port(), &["gate"]);
            assert_eq!(code, 2, "{err}");
            assert!(out.is_empty());
            assert!(err.contains("fails closed"), "{err}");
        }

        #[test]
        fn a_server_error_blocks() {
            let b = bin();
            let (port, h) = serve_once("500 Internal Server Error", "allow");
            let (code, _, err) = run(&b, port, &["gate"]);
            h.join().unwrap();
            assert_eq!(code, 2, "a 500 is a failure even with an allow body: {err}");
        }

        #[test]
        fn a_deny_blocks_with_its_reason() {
            let b = bin();
            let (port, h) = serve_once("200 OK", "deny:No agent in a Hive pushes.");
            let (code, _, err) = run(&b, port, &["gate"]);
            h.join().unwrap();
            assert_eq!(code, 2);
            assert_eq!(err.trim(), "No agent in a Hive pushes.");
        }

        #[test]
        fn only_an_exact_allow_lets_the_call_through() {
            let b = bin();
            let (port, h) = serve_once("200 OK", "allowed");
            assert_eq!(run(&b, port, &["gate"]).0, 2, "not exactly allow");
            h.join().unwrap();

            let (port, h) = serve_once("200 OK", "allow");
            let (code, out, err) = run(&b, port, &["gate"]);
            let req = h.join().unwrap();
            assert_eq!(code, 0, "{err}");
            assert!(out.is_empty(), "an allow prints nothing, so Claude's own permission rules still decide");
            assert!(req.starts_with("POST /hive/gate?tab_id=tab-1 "), "{req}");
            assert!(req.to_ascii_lowercase().contains("x-claude-code-ide-authorization: tok-xyz"));
            assert!(req.contains(r#""command":"git push""#), "the event is forwarded whole");
        }

        #[test]
        fn observing_never_blocks_and_prints_only_the_priming() {
            let b = bin();
            let (port, h) = serve_once("200 OK", "PRIMING");
            let (code, out, _) = run(&b, port, &["observe", "SessionStart"]);
            let req = h.join().unwrap();
            assert_eq!((code, out.trim()), (0, "PRIMING"));
            assert!(req.starts_with("POST /hooks?tab_id=tab-1&via=hive&prime=1 "), "{req}");

            let (port, h) = serve_once("200 OK", "ignored");
            let (code, out, _) = run(&b, port, &["observe", "Stop"]);
            let req = h.join().unwrap();
            assert_eq!((code, out.as_str()), (0, ""));
            assert!(req.starts_with("POST /hooks?tab_id=tab-1&via=hive "), "{req}");

            let (code, out, _) = run(&b, closed_port(), &["observe", "PostToolUse"]);
            assert_eq!((code, out.as_str()), (0, ""), "maiTerm down: observation is lost, never a block");
        }

        #[test]
        fn a_bad_port_file_falls_back_to_the_env() {
            let b = bin();
            let (port, h) = serve_once("200 OK", "allow");
            install_hook(&b, port).unwrap();
            fs::write(b.join(PORT_FILE), "not-a-port").unwrap();
            let mut child = Command::new(b.join(HOOK_FILE))
                .arg("gate")
                .env("MAITERM_TAB_ID", "tab-1")
                .env("MAITERM_AUTH", "tok-xyz")
                .env("MAITERM_PORT", port.to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(b"{}").unwrap();
            assert_eq!(child.wait().unwrap().code(), Some(0));
            h.join().unwrap();
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(fs::metadata(b.join(HOOK_FILE)).unwrap().permissions().mode() & 0o777, 0o755);
        }
    }

    #[test]
    fn workers_have_no_settings_yet() {
        let (h, p) = (crate::hive::launch::tests::sample_hive(), crate::hive::launch::tests::sample_paths());
        let mut r = coordinator_record(&h, &p);
        r.role = crate::hive::ROLE_WORKER.into();
        assert!(hive_settings_json(&h, &r, &p, &env()).is_err());
    }
}
