//! What a Hive agent launches with (docs/hive.md §6.7, §7.1, §7.3, §11.1): the `--settings`
//! file (sandbox, memory folder, permission rules, command hooks), the `--mcp-config` file, and
//! the hook script those hooks run.
//!
//! The coordinator's settings are the set that passed spike 13 plus the two additions it named
//! (maiTerm's MCP tools, each by name, and `git -C` rules), and the git option denies those
//! rules turned out to need (`GIT_ESCAPE_DENIES`). Each layer covers another's gap: every path
//! on the sandbox's `denyWrite` has an `Edit(//…)` deny rule and the reverse, since the Write
//! tool ignores the sandbox and an Edit deny alone let `python3 -c open(…)` through (spikes 10,
//! 14). Edit rules are the ones Claude matches for the Write tool too (`FILE_RULE_TOOL`).

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::launch::{sh_quote, LaunchRecord};
use super::paths::{hive_bin_dir, is_component, HivePaths, COORDINATOR_SLUG, LAND_SLUG, RESERVED_SLUGS};
use super::record::Hive;
use super::worker::{list_workers, WorkerFolder, WorkerRecord};
use super::{ROLE_COORDINATOR, ROLE_WORKER};

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
    "Bash(git *--open-files-in-pager*)",
    // git takes an abbreviated long option (`grep --op=<cmd>`); the gate refuses every prefix.
    "Bash(git * grep *--op*)",
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
    "hiveRetireWorker",
    "hiveResolve",
    "hiveFinish",
    "hiveSetCriteria",
    "hiveAnswerWorker",
    "hiveRunPriority",
    "hivePrecedents",
    "hiveRecordPrecedent",
    "hiveProposePlan",
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
    // Writes the whole state, every tab's scrollback and the comms token, to any folder.
    "createBackup",
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

/// Denied to every worker on top of `ESCAPE_MCP_TOOLS`: stopping what others run, the Overlord's
/// channel, a tab restore (it replays a stored launch), reads of other tabs' output and of
/// maiTerm's own logs and diagnostics (the human's terminals, outside the sandbox), and the
/// coordinator's own tools (`coordinator_only_mcp_tools`). Rust refuses these too, by
/// `hive_role` (§7.2). Auto mode has no allow list, so this is a worker's only static bound on
/// maiTerm's tools: a new tool that acts outside the sandbox goes here or on `ESCAPE_MCP_TOOLS`.
const WORKER_EXTRA_DENIED_MCP_TOOLS: &[&str] = &[
    "stopService",
    "stopStack",
    "replyToOverlord",
    "restoreArchivedTab",
    "getTabContext",
    "getServiceOutput",
    "getTabPrompt",
    "readLogs",
    "getDiagnostics",
    "readCommsChannel",
    "readCommsThread",
];

/// The hive and gallery tools only a coordinator holds.
pub fn coordinator_only_mcp_tools() -> impl Iterator<Item = &'static str> {
    COORDINATOR_MCP_TOOLS.iter().copied().filter(|t| t.starts_with("hive") || super::gallery::tools::coordinator_only(t))
}

/// A worker's denies. Its stack verbs (`run::SERVICE_VERBS`) aren't among them: Rust routes a
/// worker's start, restart or stop to the run queue, under its sandbox, before any window sees it.
pub fn worker_denied_mcp_tools() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for t in ESCAPE_MCP_TOOLS.iter().chain(WORKER_EXTRA_DENIED_MCP_TOOLS).copied().chain(coordinator_only_mcp_tools()).chain(overlord_mcp_tools()) {
        if !out.contains(&t) && !super::run::SERVICE_VERBS.contains(&t) {
            out.push(t);
        }
    }
    out
}

/// The Overlord's own tools (close, archive, recover or drive the human's tabs), denied to every
/// Hive agent in its settings as well as in Rust (`refuse_agent_tool`): a Hive agent is never the
/// Overlord. Never one the coordinator is allowed.
pub fn overlord_mcp_tools() -> impl Iterator<Item = &'static str> {
    crate::claude_code::server::OVERLORD_CALLER_TOOLS.iter().copied().filter(|t| !COORDINATOR_MCP_TOOLS.contains(t))
}

/// The coordinator's denies: `ESCAPE_MCP_TOOLS` and the Overlord's tools, once each.
pub fn coordinator_denied_mcp_tools() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = ESCAPE_MCP_TOOLS.to_vec();
    out.extend(overlord_mcp_tools().filter(|t| !ESCAPE_MCP_TOOLS.contains(t)));
    out
}

/// The package registries a worker's sandbox reaches (§14.16), beside the Hive's generator hosts
/// (`settings.gallery.generator_hosts`, `worker_domains`).
pub const PACKAGE_REGISTRIES: &[&str] =
    &["registry.npmjs.org", "crates.io", "index.crates.io", "static.crates.io", "pypi.org", "files.pythonhosted.org"];

/// Git verbs a worker's rules allow without the auto-mode classifier, each in its `-C` form for
/// a secondary repo (§6.3) and with a bare twin: a trailing ` *` is optional only in a pattern
/// with no other wildcard (the 3e probe).
const WORKER_GIT_VERBS: &[&str] = &["status", "diff", "log", "show", "add", "commit"];

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

/// The tool every file rule names. Claude matches only `Edit(path)` rules in its file permission
/// checks, for every tool that edits a file (Write, MultiEdit, NotebookEdit too): read out of
/// 2.1.308–2.1.310, where a `Write(path)` rule is "not matched by file permission checks", and
/// 2.1.310 warns "Deny rule Write(//…) never matches" at every launch. Spike 10's Write deny
/// sat beside its Edit twin, which is the one that blocked the Write tool.
pub const FILE_RULE_TOOL: &str = "Edit";

/// `Edit(//abs/**)`: an absolute path needs the `//` form, `Edit(/abs/**)` matches nothing
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
    add(paths.runs(), true);
    add(paths.precedents(), true);
    // A precedent planted there would steer every Hive as the human's (plan D25).
    add(paths.global_precedents(), true);
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

/// The coordinator's `--settings` file. A worker's is `worker_settings_json`, which needs its
/// `worker.json` and what surrounds it.
pub fn hive_settings_json(hive: &Hive, record: &LaunchRecord, paths: &HivePaths, hooks: &HookEnv) -> Result<Value, String> {
    if record.role != ROLE_COORDINATOR {
        return Err(format!("No coordinator settings for a Hive \"{}\"", record.role));
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
    allow.push(abs_rule(FILE_RULE_TOOL, &home, true));
    // dontAsk denies every tool not listed, maiTerm's own included.
    allow.extend(COORDINATOR_MCP_TOOLS.iter().map(|t| format!("mcp__{mcp}__{t}")));

    let mut deny_rules: Vec<String> = vec!["WebFetch".into(), "WebSearch".into()];
    deny_rules.extend(PUSH_DENIES.iter().chain(GIT_ESCAPE_DENIES).map(|s| s.to_string()));
    deny_rules.extend(coordinator_denied_mcp_tools().iter().map(|t| format!("mcp__{mcp}__{t}")));
    for d in &deny {
        deny_rules.push(abs_rule(FILE_RULE_TOOL, &d.path, d.dir));
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

/// What a worker's settings need from outside its own record, read at each launch.
#[derive(Debug, Clone, Default)]
pub struct WorkerContext {
    /// Every other worker's folder under `wt.noindex/`, as known at this launch.
    pub other_workers: Vec<PathBuf>,
    /// The hosts of the Hive repos' network remotes, which stay unreachable so no agent pushes
    /// (§7.3): a host allowlist can't tell a fetch from a push.
    pub remote_hosts: Vec<String>,
    /// The shared build caches it may write (§6.5).
    pub caches: Vec<PathBuf>,
    /// Write/Edit deny rules for the human's checkouts (`checkout_denies`).
    pub checkout_denies: Vec<DenyPath>,
}

/// The human's checkouts as Write/Edit deny rules. Claude folds an Edit deny rule into the
/// sandbox's write denials (the 4b probe: `Edit(//<checkout>/**)` refused the commit's
/// `.git/worktrees/<N>/index.lock`), so a checkout holding a `.git` the worker commits into is
/// denied entry by entry (`deny_around`), leaving that `.git` to the per-file allowWrite and
/// denyWrite. That covers a checkout it has a worktree of and one a nested repo it works sits
/// in. An entry made after this launch is still refused by the sandbox (outside every writable
/// path) and by the gate, which reads the real path; any other checkout is denied whole. Blocks
/// on the disk.
pub fn checkout_denies(hive: &Hive, w: &WorkerRecord) -> Vec<DenyPath> {
    let keep = git_keep(w);
    let mut out: Vec<DenyPath> = Vec::new();
    for f in hive.git_folders() {
        deny_around(Path::new(&f.path), true, &keep, &mut out);
    }
    out
}

/// What a worker's commits write in the human's repos: each one's `.git` (its config, hooks
/// and info are denied by name) and admin dir, and its worktrees.
fn git_keep(w: &WorkerRecord) -> Vec<PathBuf> {
    w.git_folders().flat_map(|f| [&f.common_dir, &f.admin_dir, &f.worktree]).flatten().map(PathBuf::from).collect()
}

/// `path` as deny entries that leave every `keep` path out: whole when none is inside it, else
/// its entries one by one, descending only toward a kept path, so a Hive folder nested in
/// another (or a local remote that is one) never takes the `.git` a commit writes. Nothing for
/// a path inside a kept one. A symlink's entry names the link; its target is outside every
/// writable path anyway. Blocks on the disk.
pub(crate) fn deny_around(path: &Path, dir: bool, keep: &[PathBuf], out: &mut Vec<DenyPath>) {
    if keep.iter().any(|k| path.starts_with(k)) {
        return;
    }
    if !dir || !keep.iter().any(|k| k.starts_with(path)) {
        if !out.iter().any(|d| d.path == path) {
            out.push(DenyPath { path: path.to_path_buf(), dir });
        }
        return;
    }
    let Ok(rd) = fs::read_dir(path) else {
        log::warn!("hive: {} can't be listed; its Write/Edit rules are left to the sandbox and the gate", path.display());
        return;
    };
    let mut entries: Vec<(PathBuf, bool)> = rd.flatten().map(|e| (e.path(), e.file_type().is_ok_and(|t| t.is_dir()))).collect();
    entries.sort();
    for (p, d) in entries {
        deny_around(&p, d, keep, out);
    }
}

impl WorkerContext {
    /// Blocks on the disk and git.
    pub fn gather(hive: &Hive, paths: &HivePaths, w: &WorkerRecord) -> Self {
        let other_workers = list_workers(paths)
            .into_iter()
            .filter(|o| o.task_id != w.task_id && o.slug != w.slug && is_component(&o.slug) && !RESERVED_SLUGS.contains(&o.slug.as_str()))
            .map(|o| paths.worker_dir(&o.slug))
            .collect();
        Self { other_workers, remote_hosts: hive_remote_hosts(hive), caches: shared_caches(), checkout_denies: checkout_denies(hive, w) }
    }
}

/// The host of every fetch and push url of the Hive's repos, lowercased, once each. Blocks: git.
pub fn hive_remote_hosts(hive: &Hive) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for f in hive.git_folders() {
        for host in remote_urls(Path::new(&f.path)).iter().filter_map(|u| url_host(u)) {
            if !out.contains(&host) {
                out.push(host);
            }
        }
    }
    out
}

/// Every fetch and push url of a repo's remotes. None at all is git's exit 1.
fn remote_urls(repo: &Path) -> Vec<String> {
    let mut cmd = super::git::git_in(repo);
    cmd.args(["config", "--get-regexp", r"^remote\..*\.(push)?url$"]);
    match super::git::run(&mut cmd, super::git::QUERY_TIMEOUT) {
        Ok(out) => out.lines().filter_map(|l| l.split_once(' ').map(|(_, v)| v.trim().to_string())).collect(),
        Err(_) => Vec::new(),
    }
}

/// The host a git remote url names, lowercased; None for a local path or `file://`.
pub fn url_host(url: &str) -> Option<String> {
    let url = url.trim();
    let authority = match url.split_once("://") {
        Some((scheme, _)) if scheme.eq_ignore_ascii_case("file") => return None,
        Some((_, rest)) => rest.split('/').next()?,
        // scp-like `[user@]host:path`; a colon after a slash belongs to a path.
        None => url.split_once(':').map(|(h, _)| h).filter(|h| !h.contains('/'))?,
    };
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = match host.strip_prefix('[') {
        Some(v6) => v6.split(']').next()?,
        None => host.split(':').next()?,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// The shared caches a build writes outside the worker's folders (§6.5): cargo's registry and
/// git checkouts with the lock files cargo takes beside them (never all of `~/.cargo`, whose
/// `bin/` is on the human's PATH), npm's cache and sccache's. A residual channel, like the
/// shared object store: what a worker writes there, other workers and the human's builds read.
pub fn shared_caches() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else { return Vec::new() };
    let env_dir = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let cargo = env_dir("CARGO_HOME").unwrap_or_else(|| home.join(".cargo"));
    let mut out: Vec<PathBuf> = ["registry", "git", ".package-cache", ".package-cache-mutate", ".global-cache"].iter().map(|p| cargo.join(p)).collect();
    out.push(env_dir("npm_config_cache").unwrap_or_else(|| home.join(".npm")));
    let sccache_default = || dirs::cache_dir().map(|c| c.join(if cfg!(target_os = "macos") { "Mozilla.sccache" } else { "sccache" }));
    out.extend(env_dir("SCCACHE_DIR").or_else(sccache_default));
    out
}

fn git_dirs_of(w: &WorkerRecord, f: &WorkerFolder) -> Result<(PathBuf, PathBuf, PathBuf), String> {
    match (&f.worktree, &f.common_dir, &f.admin_dir) {
        (Some(wt), Some(common), Some(admin)) => Ok((PathBuf::from(wt), PathBuf::from(common), PathBuf::from(admin))),
        _ => Err(format!("Worker {}'s record doesn't name the git folders of \"{}\", so its sandbox can't be drawn", w.slug, f.name)),
    }
}

/// A worker's `denyWrite` (§6.7, §14.16), each path mirrored by Write/Edit deny rules: in each
/// of its repos the git files that would have git run a command or look elsewhere, the project
/// settings Claude would load unseen, the other agents' folders and the Hive's Rust-owned ones,
/// the hook script's folder, every local-path remote, and the plain folders (read-only inputs,
/// §6.1). The human's checkouts are not here: whole, they'd take the `.git` entries a commit
/// needs; they get Write/Edit rules only.
pub fn worker_deny_write_paths(hive: &Hive, w: &WorkerRecord, paths: &HivePaths, ctx: &WorkerContext, bin_dir: &Path) -> Result<Vec<DenyPath>, String> {
    let mut out: Vec<DenyPath> = Vec::new();
    let mut add = |path: PathBuf, dir: bool| {
        if !out.iter().any(|d| d.path == path) {
            out.push(DenyPath { path, dir });
        }
    };
    for f in w.git_folders() {
        let (wt, common, admin) = git_dirs_of(w, f)?;
        add(common.join("config"), false);
        add(common.join("hooks"), true);
        add(common.join("info"), true);
        // What redirects an object lookup inside the store it writes (the objstore review): a
        // commit-graph or alternates under `objects/info`, a pack or multi-pack index, a
        // `shallow` list. A commit writes loose objects only; the auto-maintenance it may start
        // fails on `objects/pack` with a warning and the commit stands (measured, git 2.54).
        add(common.join("objects").join("info"), true);
        add(common.join("objects").join("pack"), true);
        add(common.join("shallow"), false);
        for file in ["commondir", "gitdir", "config.worktree"] {
            add(admin.join(file), false);
        }
        // Rewritten, the worktree's `.git` would point Claude's own git, which runs outside the
        // sandbox, at a repo (and a config) the worker made.
        add(wt.join(".git"), false);
        add(wt.join(".claude"), true);
        add(wt.join(".mcp.json"), false);
    }
    // A scratch-only worker's cwd: a repo made here would be the one Claude's own git reads.
    let scratch = paths.worker_scratch(&w.slug);
    add(scratch.join(".git"), true);
    add(scratch.join(".claude"), true);
    add(scratch.join(".mcp.json"), false);
    add(paths.worktrees().join(COORDINATOR_SLUG), true);
    add(paths.worktrees().join(LAND_SLUG), true);
    for o in &ctx.other_workers {
        add(o.clone(), true);
    }
    add(paths.gallery(), true);
    add(paths.tasks(), true);
    add(paths.logs(), true);
    // Its jobs' profiles and pass marks (§6.8): a planted mark would be a forged receipt.
    add(paths.runs(), true);
    add(paths.precedents(), true);
    add(paths.global_precedents(), true);
    add(bin_dir.to_path_buf(), true);
    // Folders that may hold a repo this worker commits into, or its own folders and caches (a
    // plain folder of `~`): split around them rather than denied over them.
    let mut keep = git_keep(w);
    keep.push(paths.worker_dir(&w.slug));
    keep.extend(ctx.caches.iter().cloned());
    for f in &hive.folders {
        for r in &f.local_remotes {
            deny_around(Path::new(r), true, &keep, &mut out);
        }
        if !f.is_git() {
            deny_around(Path::new(&f.path), true, &keep, &mut out);
        }
    }
    Ok(out)
}

/// What a worker may write, as one derivation that both its Claude sandbox and its queued jobs'
/// Seatbelt profile are drawn from (§6.7, §6.8), so a job can never write what its worker can't.
#[derive(Debug, Clone, PartialEq)]
pub struct WriteLists {
    /// Every path it writes, its worktrees included: the job profile has no implicit working
    /// folder, so Claude's settings drop the cwd from this list and the profile keeps it.
    pub allow: Vec<PathBuf>,
    /// `worker_deny_write_paths`, which wins over `allow`.
    pub deny: Vec<DenyPath>,
    /// Its own folders, which get Write/Edit allow rules: scratch, then each worktree.
    pub own: Vec<PathBuf>,
}

/// A worker's write lists. Everything outside its worktrees is named (spike 10 re-run). For a
/// linked worktree that is its own corner of the main repo's `.git`: per-file branch entries,
/// never `refs/heads/` (it would move Base or another worker's branch) or `packed-refs`.
pub fn worker_write_lists(hive: &Hive, w: &WorkerRecord, paths: &HivePaths, ctx: &WorkerContext, bin_dir: &Path) -> Result<WriteLists, String> {
    let deny = worker_deny_write_paths(hive, w, paths, ctx, bin_dir)?;
    let mut allow: Vec<PathBuf> = Vec::new();
    let mut own: Vec<PathBuf> = vec![paths.worker_scratch(&w.slug)];
    let mut aw = |p: PathBuf| {
        if !allow.contains(&p) {
            allow.push(p);
        }
    };
    for f in w.git_folders() {
        let (wt, common, admin) = git_dirs_of(w, f)?;
        aw(wt.clone());
        own.push(wt);
        aw(admin);
        aw(common.join("objects"));
        if let Some(b) = f.branch.as_deref() {
            let r = common.join("refs").join("heads").join(b);
            aw(PathBuf::from(format!("{}.lock", r.to_string_lossy())));
            aw(r);
            aw(common.join("logs").join("refs").join("heads").join(b));
        }
    }
    aw(paths.worker_scratch(&w.slug));
    aw(paths.worker_tmp(&w.slug));
    aw(paths.worker_cargo_target(&w.slug));
    for c in &ctx.caches {
        aw(c.clone());
    }
    Ok(WriteLists { allow, deny, own })
}

/// A worker's `--settings` file (§6.7, §14.16): the sandbox with its write lists derived from
/// `worker.json`, the registries and nothing else on the network, the Hive's memory folder,
/// deny rules mirroring `denyWrite` plus the human's checkouts, the push and git-option denies
/// every Hive agent has, the maiTerm tools no worker may call, and the same command hooks.
pub fn worker_settings_json(hive: &Hive, record: &LaunchRecord, w: &WorkerRecord, paths: &HivePaths, ctx: &WorkerContext, hooks: &HookEnv) -> Result<Value, String> {
    if record.role != ROLE_WORKER {
        return Err(format!("No worker settings for a Hive \"{}\"", record.role));
    }
    let WriteLists { allow: write, deny, own } = worker_write_lists(hive, w, paths, ctx, &hooks.bin_dir)?;
    let cwd = PathBuf::from(&record.cwd);
    let mcp = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
    let path_str = |p: &Path| p.to_string_lossy().to_string();
    // Claude's sandbox writes the cwd already.
    let allow_write: Vec<String> = write.iter().filter(|p| **p != cwd).map(|p| path_str(p)).collect();

    let mut allow: Vec<String> = Vec::new();
    for verb in WORKER_GIT_VERBS {
        allow.push(format!("Bash(git {verb} *)"));
        allow.push(format!("Bash(git -C * {verb})"));
        allow.push(format!("Bash(git -C * {verb} *)"));
    }
    for d in &own {
        allow.push(abs_rule(FILE_RULE_TOOL, d, true));
    }

    let mut deny_rules: Vec<String> = record.disallowed_tools.clone();
    deny_rules.extend(PUSH_DENIES.iter().chain(GIT_ESCAPE_DENIES).map(|s| s.to_string()));
    deny_rules.extend(worker_denied_mcp_tools().iter().map(|t| format!("mcp__{mcp}__{t}")));
    for d in deny.iter().chain(&ctx.checkout_denies) {
        deny_rules.push(abs_rule(FILE_RULE_TOOL, &d.path, d.dir));
    }

    // Generator hosts only for a worker whose role generates (the predicate `worker_mcp_config`
    // uses): a code worker or a reviewer gets the registries alone.
    let generates = !w.is_reviewer() && super::roles::role(&w.role).is_some_and(|r| r.generators);
    let domains = worker_domains(hive, &ctx.remote_hosts, generates);
    Ok(json!({
        "sandbox": {
            "enabled": true,
            "failIfUnavailable": true,
            "allowUnsandboxedCommands": false,
            // `autoAllowBashIfSandboxed` keeps its default: a worker's Bash runs freely, inside.
            // Never `excludedCommands`. Each switch that weakens the sandbox is pinned off rather
            // than left unset, so no other settings file can turn it on: `allowLocalBinding`
            // opens every local port, maiTerm's MCP included (§6.7).
            "allowAppleEvents": false,
            "allowNestedUserNamespaces": false,
            "enableWeakerNestedSandbox": false,
            "enableWeakerNetworkIsolation": false,
            "filesystem": {
                "allowWrite": allow_write,
                "denyWrite": deny.iter().map(|d| path_str(&d.path)).collect::<Vec<_>>(),
                "disabled": false,
            },
            "network": {
                "allowedDomains": domains,
                "strictAllowlist": true,
                "allowLocalBinding": false,
                "allowAllUnixSockets": false,
                // Blocked even if another source allows them: no remote is reachable, so no
                // agent pushes (§7.3).
                "deniedDomains": ctx.remote_hosts,
            },
        },
        // Every Hive hook is the gate or maiTerm's observer; nothing may switch them off.
        "disableAllHooks": false,
        "autoMemoryDirectory": path_str(&paths.memory()),
        "permissions": { "allow": allow, "deny": deny_rules },
        "hooks": hive_hooks(&hooks.script, &hooks.known_newer),
    }))
}

/// A worker's network allowlist (§14.16): the package registries and, for a media worker
/// (`generates`), the Hive's generator hosts,
/// minus anything that reaches a repo's remote, which `deniedDomains` also lists. The hosts were
/// checked when the human saved them; checked again here, since the state file is plain JSON, and
/// a remote added since is taken out all the same.
pub fn worker_domains(hive: &Hive, remote_hosts: &[String], generates: bool) -> Vec<String> {
    use super::gallery::budget::{host_covers, normalize_host};
    let remotes: Vec<String> = remote_hosts.iter().map(|h| h.to_ascii_lowercase()).collect();
    let chosen: &[String] = if generates { &hive.settings.gallery.generator_hosts } else { &[] };
    let generators = chosen.iter().filter_map(|h| {
        normalize_host(h).map_err(|e| log::warn!("hive {}: generator host left out: {e}", hive.id)).ok()
    });
    let mut out: Vec<String> = Vec::new();
    for d in PACKAGE_REGISTRIES.iter().map(|d| d.to_string()).chain(generators) {
        if !remotes.iter().any(|r| host_covers(&d, r)) && !out.contains(&d) {
            out.push(d);
        }
    }
    out
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

/// A worker's `--mcp-config` (plan D1, §6.8 "only the servers it needs"): maiTerm's server, and
/// for a role that generates (`generators: true`; never a reviewer) each MCP server the human
/// chose for the Hive that is a recognised generator (`create::is_generator_server`: its url host
/// or name in `KNOWN_GENERATOR_HOSTS`), its entry copied verbatim from the launching account's
/// `.claude.json`. Any other ticked server (a github, a filesystem) is never copied: it would run
/// outside the sandbox and the gate with that server's whole authority.
/// Copied fresh at every launch, so a rotated key reaches the next one. A chosen server that file
/// no longer has is left out, logged. Generator calls run in Claude's process, outside the
/// sandbox, bounded by the budget (§6.7).
pub fn worker_mcp_config(hive: &Hive, w: &WorkerRecord, claude_json: Option<&Value>) -> Value {
    let mut v = hive_mcp_config();
    let generates = !w.is_reviewer() && super::roles::role(&w.role).is_some_and(|r| r.generators);
    if !generates {
        return v;
    }
    let ours = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
    let theirs = claude_json.and_then(|c| c.get("mcpServers")).and_then(Value::as_object);
    for name in hive.tools.iter().filter_map(|t| t.strip_prefix("mcp:")) {
        if name == ours {
            continue;
        }
        match theirs.and_then(|s| s.get(name)) {
            Some(entry) if super::create::is_generator_server(name, entry) => {
                v["mcpServers"][name] = entry.clone();
            }
            Some(_) => log::warn!("hive {}: worker {}: MCP server \"{name}\" isn't a generator maiTerm recognises; left out (it would run outside the sandbox)", hive.id, w.slug),
            None => log::warn!("hive {}: worker {}: MCP server \"{name}\" isn't in the account's .claude.json; left out", hive.id, w.slug),
        }
    }
    v
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

    // The sample paths are POSIX, and absolute only there; Claude has no sandbox on Windows.
    #[cfg(unix)]
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
        // The Overlord's tools are denied it too, as Rust refuses them to any Hive role.
        for t in crate::claude_code::server::OVERLORD_CALLER_TOOLS {
            let rule = format!("mcp__{mcp}__{t}");
            assert!(deny.contains(&rule) && !allow.contains(&rule), "{rule}");
        }
        let mcp_denies: Vec<&String> = deny.iter().filter(|r| r.starts_with("mcp__")).collect();
        let mut unique = mcp_denies.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), mcp_denies.len(), "each tool denied once");
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
            assert!(deny.contains(&abs_rule("Edit", &d.path, d.dir)), "Edit rule for {}", d.path.display());
        }
        // No `Write(path)` rule: Claude never matches one, and 2.1.310 warns about each at launch.
        let all = strings(&s["permissions"]["allow"]).into_iter().chain(deny.iter().cloned()).collect::<Vec<_>>();
        assert!(!all.iter().any(|r| r.starts_with("Write(")), "{all:?}");
        // The reverse: every Edit deny rule names a denyWrite path.
        for r in deny.iter().filter(|r| r.starts_with("Edit(")) {
            let inner = r.split_once("(/").unwrap().1.trim_end_matches(')').trim_end_matches("/**");
            assert!(deny_write.iter().any(|d| d == inner), "{r} has no denyWrite twin");
        }
        for must in ["/r/web", "/r/brand assets", "/srv/web.git"] {
            assert!(deny_write.contains(&must.to_string()), "{must}");
        }
        for must in [p.coordinator_worktree("web"), p.gallery(), env().bin_dir, p.coordinator_home().join(".claude"), p.precedents(), p.global_precedents()] {
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

    fn worker_ctx() -> WorkerContext {
        WorkerContext {
            other_workers: vec![PathBuf::from("/Users/x/Library/Application Support/com.aiterm.dev/hive/h1/wt.noindex/billing")],
            remote_hosts: vec!["github.com".into(), "crates.io".into()],
            caches: vec![PathBuf::from("/Users/x/.cargo/registry"), PathBuf::from("/Users/x/.npm")],
            checkout_denies: vec![
                DenyPath { path: PathBuf::from("/r/web/README.md"), dir: false },
                DenyPath { path: PathBuf::from("/r/web/src"), dir: true },
                DenyPath { path: PathBuf::from("/r/api/src"), dir: true },
            ],
        }
    }

    fn worker_settings() -> (Value, crate::hive::worker::WorkerRecord) {
        let (h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let r = crate::hive::launch::worker_record(&h, &p, &w);
        (worker_settings_json(&h, &r, &w, &p, &worker_ctx(), &env()).unwrap(), w)
    }

    /// §9: every role's settings name the Hive's memory folder, absolute, and leave writing it to
    /// Claude's own exemption for `autoMemoryDirectory`: no sandbox list names it either way.
    #[cfg(unix)]
    #[test]
    fn every_role_points_its_memory_at_the_hive_folder_and_no_write_list_names_it() {
        let (h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let mut reviewer = w.clone();
        reviewer.task_id = "t-rev".into();
        reviewer.slug = "auth-review".into();
        reviewer.reviews = Some("t-auth".into());
        let coord = hive_settings_json(&h, &coordinator_record(&h, &p), &p, &env()).unwrap();
        let worker = worker_settings_json(&h, &crate::hive::launch::worker_record(&h, &p, &w), &w, &p, &worker_ctx(), &env()).unwrap();
        let rev = worker_settings_json(&h, &crate::hive::launch::worker_record(&h, &p, &reviewer), &reviewer, &p, &worker_ctx(), &env()).unwrap();
        let mem = p.memory();
        for (role, s) in [("coordinator", &coord), ("worker", &worker), ("reviewer", &rev)] {
            assert_eq!(s["autoMemoryDirectory"].as_str(), Some(&*mem.to_string_lossy()), "{role}");
            for list in ["allowWrite", "denyWrite"] {
                let entries = s["sandbox"]["filesystem"][list].as_array().map(|a| strings(&Value::Array(a.clone()))).unwrap_or_default();
                for e in entries {
                    assert!(!Path::new(&e).starts_with(&mem) && !mem.starts_with(&e), "{role}: {list} names {e}");
                }
            }
        }
    }

    #[test]
    fn each_role_has_only_its_own_settings() {
        let (h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let wr = crate::hive::launch::worker_record(&h, &p, &w);
        assert!(hive_settings_json(&h, &wr, &p, &env()).is_err(), "a worker never gets the coordinator's");
        assert!(worker_settings_json(&h, &coordinator_record(&h, &p), &w, &p, &worker_ctx(), &env()).is_err());
        let mut lost = w.clone();
        lost.folders[1].admin_dir = None;
        let err = worker_settings_json(&h, &wr, &lost, &p, &worker_ctx(), &env()).unwrap_err();
        assert!(err.contains("\"api\""), "no git dirs, no sandbox to draw: {err}");
    }

    // POSIX paths, as the coordinator's.
    #[cfg(unix)]
    #[test]
    fn the_worker_sandbox_fails_closed_and_opens_no_localhost() {
        let (s, _) = worker_settings();
        let sb = &s["sandbox"];
        assert_eq!(sb["enabled"], true);
        assert_eq!(sb["failIfUnavailable"], true);
        assert_eq!(sb["allowUnsandboxedCommands"], false);
        assert!(sb.get("autoAllowBashIfSandboxed").is_none(), "left at its default for workers");
        assert!(sb.get("allowLocalBinding").is_none() && sb.get("excludedCommands").is_none());
        // Pinned off, not left unset: unset, a user or project file could switch them on.
        for weakener in [&sb["network"]["allowLocalBinding"], &sb["network"]["allowAllUnixSockets"], &sb["allowAppleEvents"], &sb["allowNestedUserNamespaces"], &sb["enableWeakerNestedSandbox"], &sb["enableWeakerNetworkIsolation"], &sb["filesystem"]["disabled"], &s["disableAllHooks"]] {
            assert_eq!(weakener, &json!(false));
        }
        assert_eq!(strings(&sb["network"]["deniedDomains"]), vec!["github.com", "crates.io"], "a remote's host is denied whatever else allows it");
        assert_eq!(sb["network"]["strictAllowlist"], true);
        let text = s.to_string();
        assert!(!text.contains("127.0.0.1") && !text.contains("localhost"), "no localhost anywhere");
        let domains = strings(&sb["network"]["allowedDomains"]);
        assert!(domains.contains(&"registry.npmjs.org".to_string()) && domains.contains(&"pypi.org".to_string()));
        assert!(!domains.contains(&"crates.io".to_string()), "a git remote's host is never allowed: {domains:?}");
        assert!(!domains.contains(&"github.com".to_string()));
        let mem = s["autoMemoryDirectory"].as_str().unwrap();
        assert!(Path::new(mem).is_absolute() && mem.ends_with("/memory"));
    }

    // POSIX paths, as the coordinator's.
    #[cfg(unix)]
    #[test]
    fn the_worker_writes_its_own_corner_of_each_repo_and_no_more() {
        let (s, _) = worker_settings();
        let p = crate::hive::launch::tests::sample_paths();
        let allow = strings(&s["sandbox"]["filesystem"]["allowWrite"]);
        let deny = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
        let sp = |x: PathBuf| x.to_string_lossy().to_string();
        for must in [
            sp(p.worker_worktree("auth", "api")),
            sp(p.worker_scratch("auth")),
            sp(p.worker_tmp("auth")),
            sp(p.worker_cargo_target("auth")),
            "/r/web/.git/worktrees/web".into(),
            "/r/web/.git/objects".into(),
            "/r/web/.git/refs/heads/hive/ship-it--auth".into(),
            "/r/web/.git/refs/heads/hive/ship-it--auth.lock".into(),
            "/r/web/.git/logs/refs/heads/hive/ship-it--auth".into(),
            "/r/api/.git/objects".into(),
            "/Users/x/.cargo/registry".into(),
        ] {
            assert!(allow.contains(&must), "{must} in allowWrite");
        }
        assert!(!allow.contains(&sp(p.worker_worktree("auth", "web"))), "the cwd is writable already");
        for a in &allow {
            assert!(!a.ends_with("/refs/heads") && !a.ends_with("/refs") && !a.contains("packed-refs"), "per-file branch entries only: {a}");
        }
        for must in [
            "/r/web/.git/config",
            "/r/web/.git/hooks",
            "/r/api/.git/info",
            "/r/api/.git/worktrees/api/commondir",
            "/r/api/.git/worktrees/api/gitdir",
            "/r/web/.git/worktrees/web/config.worktree",
            "/srv/web.git",
            "/r/brand assets",
        ] {
            assert!(deny.contains(&must.to_string()), "{must} in denyWrite");
        }
        let wt = p.worker_worktree("auth", "web");
        for must in [wt.join(".git"), wt.join(".claude"), wt.join(".mcp.json"), p.worker_scratch("auth").join(".claude"), p.worker_scratch("auth").join(".git")] {
            assert!(deny.contains(&sp(must.clone())), "{}", must.display());
        }
        for must in [p.coordinator_worktree("web").parent().unwrap().to_path_buf(), p.worktrees().join("billing"), p.gallery(), p.tasks(), p.logs(), p.precedents(), p.global_precedents(), env().bin_dir] {
            assert!(deny.contains(&sp(must.clone())), "{}", must.display());
        }
        assert!(!deny.iter().any(|d| *d == sp(p.memory())), "memory is written through Claude's own exemption");
    }

    /// The object store stays writable for commits, but nothing in it that redirects a lookup:
    /// workers and reviewers alike, on the sandbox and in the Write/Edit rules (the objstore review).
    #[cfg(unix)]
    #[test]
    fn what_redirects_an_object_lookup_is_denied_to_workers_and_reviewers() {
        let (h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let mut reviewer = w.clone();
        reviewer.task_id = "t-rev".into();
        reviewer.slug = "auth-review".into();
        reviewer.reviews = Some("t-auth".into());
        for f in reviewer.folders.iter_mut() {
            f.branch = None;
        }
        for (role, who) in [("worker", &w), ("reviewer", &reviewer)] {
            let s = worker_settings_json(&h, &crate::hive::launch::worker_record(&h, &p, who), who, &p, &worker_ctx(), &env()).unwrap();
            let allow = strings(&s["sandbox"]["filesystem"]["allowWrite"]);
            let deny = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
            let rules = strings(&s["permissions"]["deny"]);
            for repo in ["/r/web/.git", "/r/api/.git"] {
                assert!(allow.contains(&format!("{repo}/objects")), "{role}: a commit writes loose objects");
                for (sub, dir) in [("objects/info", true), ("objects/pack", true), ("info", true), ("shallow", false), ("config", false), ("hooks", true)] {
                    let path = format!("{repo}/{sub}");
                    assert!(deny.contains(&path), "{role}: {path} in denyWrite");
                    let rule = abs_rule(FILE_RULE_TOOL, Path::new(&path), dir);
                    assert!(rules.contains(&rule), "{role}: {rule}");
                }
            }
        }
    }

    /// The denies cost a commit nothing: with `objects/info`, `objects/pack` and `info` closed to
    /// it, a commit in a linked worktree still lands, its auto-maintenance failing on its own.
    #[cfg(unix)]
    #[test]
    fn a_commit_needs_none_of_the_denied_object_store_entries() {
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;
        let base = crate::share::canon(&{
            let t = std::env::temp_dir().join(format!("hive-objdeny-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&t).unwrap();
            t
        });
        let git = |dir: &Path, args: &[&str]| {
            let out = Command::new("git")
                .current_dir(dir)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
                .args(args)
                .output()
                .unwrap();
            (out.status.success(), String::from_utf8_lossy(&out.stdout).trim().to_string(), String::from_utf8_lossy(&out.stderr).to_string())
        };
        let repo = base.join("web");
        fs::create_dir_all(&repo).unwrap();
        assert!(git(&repo, &["init", "-q", "-b", "main"]).0);
        assert!(git(&repo, &["config", "core.autocrlf", "false"]).0);
        fs::write(repo.join("a"), "a\n").unwrap();
        assert!(git(&repo, &["add", "-A"]).0 && git(&repo, &["commit", "-q", "-m", "init"]).0);
        let wt = base.join("wt");
        assert!(git(&repo, &["worktree", "add", "-q", "-b", "hive/x--w", wt.to_str().unwrap(), "main"]).0);
        // Loose objects over `gc.auto=1`, in the foreground. `gc --auto` samples only `objects/17`
        // and repacks once more than one object is there, so files are added until two blobs
        // (ids computed here, as git hashes them) fall in it.
        let mut in_17 = 0;
        for i in 0.. {
            let content = format!("{i}\n");
            fs::write(wt.join(format!("f{i}")), &content).unwrap();
            use sha1::Digest;
            if sha1::Sha1::digest(format!("blob {}\0{content}", content.len()).as_bytes())[0] == 0x17 {
                in_17 += 1;
            }
            if in_17 == 2 {
                break;
            }
        }
        let closed = [repo.join(".git/objects/info"), repo.join(".git/objects/pack"), repo.join(".git/info")];
        for d in &closed {
            fs::create_dir_all(d).unwrap();
            fs::set_permissions(d, fs::Permissions::from_mode(0o555)).unwrap();
        }
        assert!(git(&wt, &["add", "-A"]).0);
        assert!(fs::read_dir(repo.join(".git/objects/17")).unwrap().count() >= 2, "enough for gc --auto to repack");
        let out = Command::new("git")
            .current_dir(&wt)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_TRACE", "1")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t", "-c", "commit.gpgsign=false"])
            .args(["-c", "gc.auto=1", "-c", "gc.autoDetach=false", "-c", "maintenance.autoDetach=false", "commit", "-q", "-m", "work"])
            .output()
            .unwrap();
        let (ok, err) = (out.status.success(), String::from_utf8_lossy(&out.stderr).to_string());
        let tip = git(&repo, &["rev-parse", "refs/heads/hive/x--w"]).1;
        let main = git(&repo, &["rev-parse", "main"]).1;
        for d in &closed {
            fs::set_permissions(d, fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(ok, "the commit stands: {err}");
        assert_ne!(tip, main, "and moved the branch");
        // The maintenance did run, and failed writing its pack.
        assert!(err.contains("built-in: git pack-objects"), "the auto-maintenance packed: {err}");
        assert!(err.lines().any(|l| !l.contains("trace:") && (l.contains("Permission denied") || l.contains("failed"))), "and failed on objects/pack: {err}");
        assert_eq!(fs::read_dir(repo.join(".git/objects/pack")).unwrap().count(), 0, "nothing was packed");
        let _ = fs::remove_dir_all(&base);
    }

    /// The job profile is drawn from the same lists as the worker's sandbox (§6.8): the lists are
    /// the settings' allowWrite plus the cwd, and its denyWrite exactly, the runs folder included.
    #[test]
    fn the_write_lists_are_the_worker_sandbox_s_own() {
        let (s, _) = worker_settings();
        let (h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let r = crate::hive::launch::worker_record(&h, &p, &w);
        let lists = worker_write_lists(&h, &w, &p, &worker_ctx(), &env().bin_dir).unwrap();
        let sp = |x: &PathBuf| x.to_string_lossy().to_string();
        let mut want: Vec<String> = strings(&s["sandbox"]["filesystem"]["allowWrite"]);
        want.push(r.cwd.clone());
        let mut got: Vec<String> = lists.allow.iter().map(sp).collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
        let deny: Vec<String> = lists.deny.iter().map(|d| sp(&d.path)).collect();
        assert_eq!(deny, strings(&s["sandbox"]["filesystem"]["denyWrite"]));
        assert!(deny.contains(&sp(&p.runs())), "a job's profile and pass mark are out of the worker's reach");
        assert!(strings(&s["permissions"]["deny"]).contains(&abs_rule("Edit", &p.runs(), true)));
        assert_eq!(lists.own[0], p.worker_scratch("auth"));
    }

    #[test]
    fn worker_deny_rules_mirror_deny_write_and_hold_the_human_checkouts() {
        let (s, _) = worker_settings();
        let deny_write = strings(&s["sandbox"]["filesystem"]["denyWrite"]);
        let deny = strings(&s["permissions"]["deny"]);
        let (h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let listed = worker_deny_write_paths(&h, &w, &p, &worker_ctx(), &env().bin_dir).unwrap();
        assert_eq!(listed.len(), deny_write.len());
        for d in &listed {
            assert!(deny.contains(&abs_rule("Edit", &d.path, d.dir)), "Edit rule for {}", d.path.display());
        }
        // The human's checkouts: Edit rules, never on denyWrite, and never over the whole tree,
        // which Claude folds into the sandbox and so takes the `.git` entries a commit needs.
        for r in ["Edit(//r/web/src/**)", "Edit(//r/web/README.md)", "Edit(//r/api/src/**)"] {
            assert!(deny.contains(&r.to_string()), "{r}");
        }
        let allow = strings(&s["permissions"]["allow"]);
        assert!(!deny.iter().chain(&allow).any(|r| r.starts_with("Write(")), "no Write(path) rule: Claude never matches one");
        for checkout in ["/r/web", "/r/api"] {
            assert!(!deny.iter().any(|r| r.ends_with(&format!("(/{checkout}/**)"))), "no whole-tree rule on {checkout}");
            assert!(!deny_write.iter().any(|d| d == checkout || d.starts_with(&format!("{checkout}/src"))), "{checkout} is not on denyWrite");
        }
        for r in deny.iter().chain(&strings(&s["permissions"]["allow"])) {
            let inner = r.split_once('(').map(|(_, i)| i.trim_end_matches(')')).unwrap_or("");
            if r.starts_with("Write(") || r.starts_with("Edit(") {
                assert!(inner.starts_with("//"), "absolute paths need the // form: {r}");
            }
            assert!(!inner.trim_end_matches(":*").contains(":*") && !inner.ends_with(":*"), "{r}");
        }
        for r in ["WebFetch", "WebSearch", "Bash(git * push*)", "Bash(git remote*)", "Bash(gh pr merge*)", "Bash(git * -c *)", "Bash(git *--config-env*)", "Bash(git *--no-index*)"] {
            assert!(deny.contains(&r.to_string()), "{r}");
        }
    }

    #[test]
    fn workers_are_denied_the_tools_that_leave_the_sandbox_or_belong_to_the_coordinator() {
        let (s, _) = worker_settings();
        let deny = strings(&s["permissions"]["deny"]);
        let allow = strings(&s["permissions"]["allow"]);
        let mcp = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
        let routed = crate::hive::run::SERVICE_VERBS;
        for t in ESCAPE_MCP_TOOLS.iter().filter(|t| !routed.contains(t)).chain(&["stopStack", "replyToOverlord", "restoreArchivedTab", "hiveSpawnWorker", "hiveLand", "galleryDeliver"]) {
            assert!(deny.contains(&format!("mcp__{mcp}__{t}")), "{t}");
        }
        // Allowed to call, never to run: Rust routes them to the run queue, under the sandbox.
        for t in routed {
            assert!(!deny.contains(&format!("mcp__{mcp}__{t}")), "{t} reaches Rust's queue routing");
        }
        assert!(!worker_denied_mcp_tools().contains(&"listTasks") && !worker_denied_mcp_tools().contains(&"galleryAdd"));
        // createBackup writes the whole state, comms token and every tab's scrollback included,
        // to a folder the worker names; the reads hand it the human's own terminals.
        for t in ["createBackup", "getTabContext", "getServiceOutput", "getTabPrompt", "readLogs", "getDiagnostics", "readCommsChannel"] {
            assert!(deny.contains(&format!("mcp__{mcp}__{t}")), "{t}");
        }
        for t in crate::claude_code::server::OVERLORD_CALLER_TOOLS {
            assert!(deny.contains(&format!("mcp__{mcp}__{t}")), "the Overlord's: {t}");
        }
        let n = worker_denied_mcp_tools().len();
        let mut once = worker_denied_mcp_tools();
        once.sort();
        once.dedup();
        assert_eq!(once.len(), n, "each tool denied once");
        assert!(ESCAPE_MCP_TOOLS.contains(&"createBackup"), "the coordinator's too");
        assert!(!allow.iter().any(|r| r.starts_with("mcp__")), "auto mode: no MCP allow list");
        // A bare verb has its own rule wherever the pattern holds another wildcard.
        for r in ["Bash(git -C * commit)", "Bash(git -C * commit *)", "Bash(git commit *)", "Bash(git -C * status)"] {
            assert!(allow.contains(&r.to_string()), "{r}");
        }
        let p = crate::hive::launch::tests::sample_paths();
        assert!(allow.contains(&abs_rule("Edit", &p.worker_worktree("auth", "api"), true)));
        let web_rules: Vec<_> = s["hooks"]["PreToolUse"].as_array().unwrap().iter().filter(|e| e["matcher"] == GATE_MATCHER).collect();
        assert_eq!(web_rules.len(), 1, "the same fail-closed gate as the coordinator");
    }

    #[test]
    fn a_checkout_with_a_worktree_is_denied_entry_by_entry_and_never_its_git() {
        let base = std::env::temp_dir().join(format!("hive-checkout-{}", uuid::Uuid::new_v4()));
        for d in ["web/.git/worktrees/web1", "web/src", "api/src"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        fs::write(base.join("web/README.md"), "r").unwrap();
        let (mut h, _, mut w) = crate::hive::launch::tests::sample_worker_hive();
        h.folders.iter_mut().find(|f| f.name == "web").unwrap().path = base.join("web").to_string_lossy().to_string();
        h.folders.iter_mut().find(|f| f.name == "api").unwrap().path = base.join("api").to_string_lossy().to_string();
        w.folders.retain(|f| f.name != "api");
        let web = w.folders.iter_mut().find(|f| f.name == "web").unwrap();
        web.common_dir = Some(base.join("web/.git").to_string_lossy().to_string());
        web.admin_dir = Some(base.join("web/.git/worktrees/web1").to_string_lossy().to_string());
        let got = checkout_denies(&h, &w);
        assert_eq!(
            got,
            vec![
                DenyPath { path: base.join("web/README.md"), dir: false },
                DenyPath { path: base.join("web/src"), dir: true },
                DenyPath { path: base.join("api"), dir: true },
            ],
            "web's entries but .git; api, which it has no worktree of, whole"
        );
        let _ = fs::remove_dir_all(&base);
    }

    /// Nothing stops one Hive folder sitting inside another. A whole-folder denial over a `.git`
    /// the worker commits into fails its commit on `index.lock`, as the whole-checkout Edit rule
    /// did in the 4b probe, so each is split around that `.git`.
    #[test]
    fn a_nested_folder_never_denies_the_git_a_commit_writes() {
        let base = std::env::temp_dir().join(format!("hive-nested-{}", uuid::Uuid::new_v4()));
        for d in ["work/app/.git/worktrees/app1", "work/app/src", "work/docs", "app/.git/worktrees/x", "app/src", "app/sdk/.git/worktrees/sdk1", "app/sdk/lib"] {
            fs::create_dir_all(base.join(d)).unwrap();
        }
        fs::write(base.join("work/README.md"), "r").unwrap();
        let sp = |rel: &str| base.join(rel).to_string_lossy().to_string();
        let git = |name: &str, path: &str, admin: &str| crate::hive::worker::WorkerFolder {
            name: name.into(),
            kind: crate::hive::record::KIND_GIT.into(),
            worktree: Some(format!("/wt/{name}")),
            common_dir: Some(sp(&format!("{path}/.git"))),
            admin_dir: Some(sp(&format!("{path}/.git/worktrees/{admin}"))),
            ..Default::default()
        };
        let folder = |name: &str, rel: &str, kind: &str| crate::hive::record::HiveFolder { name: name.into(), path: sp(rel), kind: kind.into(), ..Default::default() };
        let p = crate::hive::launch::tests::sample_paths();
        // What a commit writes: the admin dir's index.lock and the object store.
        let blocks_commit = |paths: &[PathBuf], repo: &str, admin: &str| {
            let writes = [base.join(repo).join(".git/worktrees").join(admin).join("index.lock"), base.join(repo).join(".git/objects/ab/cd")];
            paths.iter().find(|d| writes.iter().any(|x| x.starts_with(d))).cloned()
        };

        // A plain folder holding a git folder the worker works.
        let h = crate::hive::record::Hive { id: "h".into(), folders: vec![folder("work", "work", "plain"), folder("app", "work/app", "git")], ..Default::default() };
        let w = WorkerRecord { slug: "w".into(), folders: vec![git("app", "work/app", "app1")], ..Default::default() };
        let ctx = WorkerContext { checkout_denies: checkout_denies(&h, &w), ..Default::default() };
        let deny: Vec<PathBuf> = worker_deny_write_paths(&h, &w, &p, &ctx, Path::new("/bin-dir")).unwrap().into_iter().map(|d| d.path).collect();
        for must in ["work/README.md", "work/docs", "work/app/.git/config", "work/app/.git/hooks"] {
            assert!(deny.contains(&base.join(must)), "{must}: {deny:?}");
        }
        assert_eq!(blocks_commit(&deny, "work/app", "app1"), None);
        let rules: Vec<PathBuf> = ctx.checkout_denies.iter().map(|d| d.path.clone()).collect();
        assert_eq!(rules, vec![base.join("work/app/src")], "the checkout's own entries but its .git");

        // A git folder inside another, the worker on the inner one only, then on both.
        let h = crate::hive::record::Hive { id: "h".into(), folders: vec![folder("app", "app", "git"), folder("sdk", "app/sdk", "git")], ..Default::default() };
        let inner = WorkerRecord { slug: "w".into(), folders: vec![git("sdk", "app/sdk", "sdk1")], ..Default::default() };
        let rules: Vec<PathBuf> = checkout_denies(&h, &inner).into_iter().map(|d| d.path).collect();
        assert_eq!(rules, vec![base.join("app/.git"), base.join("app/sdk/lib"), base.join("app/src")], "the outer repo's .git is denied, the inner one's isn't");
        assert_eq!(blocks_commit(&rules, "app/sdk", "sdk1"), None);
        let both = WorkerRecord { slug: "w".into(), folders: vec![git("app", "app", "x"), git("sdk", "app/sdk", "sdk1")], ..Default::default() };
        let rules: Vec<PathBuf> = checkout_denies(&h, &both).into_iter().map(|d| d.path).collect();
        assert_eq!(rules, vec![base.join("app/sdk/lib"), base.join("app/src")], "each once, neither .git");

        // A local remote that is another Hive folder, as `/r/api` is to `/r/web`.
        let mut h = h;
        h.folders[0].local_remotes = vec![sp("app/sdk")];
        let deny: Vec<PathBuf> = worker_deny_write_paths(&h, &inner, &p, &WorkerContext::default(), Path::new("/bin-dir")).unwrap().into_iter().map(|d| d.path).collect();
        assert!(deny.contains(&base.join("app/sdk/lib")), "{deny:?}");
        assert_eq!(blocks_commit(&deny, "app/sdk", "sdk1"), None);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn remote_hosts_are_read_from_every_url_form() {
        for (url, host) in [
            ("https://github.com/o/r.git", Some("github.com")),
            ("ssh://git@GitLab.example.com:2222/o/r.git", Some("gitlab.example.com")),
            ("git@github.com:o/r.git", Some("github.com")),
            ("https://user:tok@host.io:8443/r", Some("host.io")),
            ("ssh://[::1]:22/r", Some("::1")),
            ("/srv/web.git", None),
            ("file:///srv/web.git", None),
            ("../sibling.git", None),
            ("./a:b", None),
        ] {
            assert_eq!(url_host(url).as_deref(), host, "{url}");
        }
    }

    /// §14.16, plan D2, D20: the Hive's generator hosts join a media worker's allowlist, never one
    /// that reaches a git remote (that stays denied), and the coordinator's list stays empty.
    #[cfg(unix)]
    #[test]
    fn generator_hosts_join_a_media_worker_allowlist_but_never_a_remote_or_the_coordinator() {
        let (mut h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        let w = WorkerRecord { role: "image".into(), ..w };
        h.settings.gallery.generator_hosts = vec!["fal.run".into(), "v3.fal.media".into(), "crates.io".into(), "*.github.com".into(), "localhost".into(), "FAL.run".into()];
        let mut ctx = worker_ctx();
        ctx.remote_hosts.push("gist.github.com".into());
        let s = worker_settings_json(&h, &crate::hive::launch::worker_record(&h, &p, &w), &w, &p, &ctx, &env()).unwrap();
        let allowed = strings(&s["sandbox"]["network"]["allowedDomains"]);
        for host in ["registry.npmjs.org", "pypi.org", "fal.run", "v3.fal.media"] {
            assert!(allowed.contains(&host.to_string()), "{host}: {allowed:?}");
        }
        assert_eq!(allowed.iter().filter(|d| *d == "fal.run").count(), 1, "once");
        assert!(!allowed.contains(&"crates.io".to_string()), "a remote's host, though the human listed it: {allowed:?}");
        assert!(!allowed.contains(&"*.github.com".to_string()), "a wildcard over a remote: {allowed:?}");
        assert!(!allowed.contains(&"localhost".to_string()), "never localhost, whatever the state file says");
        let denied = strings(&s["sandbox"]["network"]["deniedDomains"]);
        assert!(denied.contains(&"crates.io".to_string()) && denied.contains(&"gist.github.com".to_string()), "{denied:?}");

        let coord = hive_settings_json(&h, &coordinator_record(&h, &p), &p, &env()).unwrap();
        assert_eq!(coord["sandbox"]["network"]["allowedDomains"], json!([]), "the coordinator reaches generators only as MCP servers");
    }

    /// A code worker and a reviewer (even of a media role) reach the package registries only: the
    /// generator hosts are a media worker's alone.
    #[cfg(unix)]
    #[test]
    fn code_workers_and_reviewers_get_the_registries_only() {
        let (mut h, p, w) = crate::hive::launch::tests::sample_worker_hive();
        h.settings.gallery.generator_hosts = vec!["fal.run".into(), "api.kie.ai".into()];
        let reviewer = WorkerRecord { role: "image".into(), reviews: Some("t-other".into()), ..w.clone() };
        assert_eq!(w.role, "backend");
        for (who, rec) in [("backend", &w), ("reviewer", &reviewer)] {
            let s = worker_settings_json(&h, &crate::hive::launch::worker_record(&h, &p, rec), rec, &p, &worker_ctx(), &env()).unwrap();
            let allowed = strings(&s["sandbox"]["network"]["allowedDomains"]);
            assert!(!allowed.is_empty() && allowed.iter().all(|d| PACKAGE_REGISTRIES.contains(&d.as_str())), "{who}: {allowed:?}");
        }
    }

    /// Plan D1: a media worker gets the Hive's chosen generator servers, copied verbatim from the
    /// account's `.claude.json`; every other worker, a reviewer, and the coordinator get maiTerm's.
    #[test]
    fn only_media_workers_get_the_chosen_generator_servers() {
        let (mut h, _, w) = crate::hive::launch::tests::sample_worker_hive();
        h.tools = vec!["mcp:fal".into(), "mcp:gone".into(), "cli:ffmpeg".into(), "skill:generate".into()];
        let fal = json!({ "type": "http", "url": "https://mcp.fal.ai/mcp", "headers": { "Authorization": "Key fal-secret" }, "x": [1, 2] });
        let claude_json = json!({ "mcpServers": { "fal": fal, "local": { "command": "npx", "args": ["x"] }, "maiterm": { "url": "http://127.0.0.1:1/mcp" } } });
        let ours = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude);
        let names = |v: &Value| {
            let mut n: Vec<String> = v["mcpServers"].as_object().unwrap().keys().cloned().collect();
            n.sort();
            n
        };

        let image = WorkerRecord { role: "image".into(), ..w.clone() };
        let v = worker_mcp_config(&h, &image, Some(&claude_json));
        let mut want = vec!["fal".to_string(), ours.to_string()];
        want.sort();
        assert_eq!(names(&v), want, "a chosen server the file lacks is left out, an unchosen one never added");
        assert_eq!(v["mcpServers"]["fal"], fal, "copied as it is");
        assert_eq!(v["mcpServers"][ours], hive_mcp_config()["mcpServers"][ours], "maiTerm's own entry, not the account's");

        let reviewer = WorkerRecord { role: "image".into(), reviews: Some("t-other".into()), ..w.clone() };
        for (who, rec) in [("backend", &w), ("reviewer", &reviewer)] {
            assert_eq!(worker_mcp_config(&h, rec, Some(&claude_json)), hive_mcp_config(), "{who}");
        }
        assert_eq!(worker_mcp_config(&h, &image, None), hive_mcp_config(), "no account file, maiTerm's only");
    }

    /// A ticked server that isn't a recognised generator runs outside the sandbox and the gate
    /// with its own authority: a media worker never gets it. A generator is recognised by its url
    /// host or its name; a stdio entry only by its name.
    #[test]
    fn a_media_worker_gets_only_recognised_generator_servers() {
        let (mut h, _, w) = crate::hive::launch::tests::sample_worker_hive();
        h.tools = ["github", "filesystem", "fal", "media", "kie-local", "proxied"].iter().map(|n| format!("mcp:{n}")).collect();
        let claude_json = json!({ "mcpServers": {
            "github": { "type": "http", "url": "https://api.githubcopilot.com/mcp/", "headers": { "Authorization": "Bearer ghp" } },
            "filesystem": { "command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/"] },
            "fal": { "type": "http", "url": "https://mcp.fal.ai/mcp" },
            "media": { "type": "http", "url": "https://MCP.Fal.AI:443/mcp" },
            "kie-local": { "command": "npx", "args": ["kie-mcp"] },
            "proxied": { "command": "npx", "args": ["fal-mcp"] },
        }});
        let image = WorkerRecord { role: "image".into(), ..w };
        let v = worker_mcp_config(&h, &image, Some(&claude_json));
        let got: Vec<&String> = v["mcpServers"].as_object().unwrap().keys().collect();
        for name in ["fal", "media", "kie-local"] {
            assert!(got.iter().any(|g| *g == name), "{name}: {got:?}");
        }
        for name in ["github", "filesystem", "proxied"] {
            assert!(!got.iter().any(|g| *g == name), "{name} must not reach a worker: {got:?}");
        }
    }

    /// The MCP config may carry a generator's key: owner-only from its first byte, and a file left
    /// readable by an older build is rewritten even when its text is the same.
    #[cfg(unix)]
    #[test]
    fn the_mcp_config_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("hive-mcp-mode-{}", uuid::Uuid::new_v4().simple()));
        fs::create_dir_all(&dir).unwrap();
        let f = dir.join("mcp.json");
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        crate::hive::launch::write_json(&f, &hive_mcp_config()).unwrap();
        fs::set_permissions(&f, fs::Permissions::from_mode(0o644)).unwrap();
        crate::hive::launch::write_private_json(&f, &hive_mcp_config()).unwrap();
        assert_eq!(mode(&f), 0o600);
        let v = json!({ "mcpServers": { "fal": { "headers": { "Authorization": "Key k" } } } });
        crate::hive::launch::write_private_json(&f, &v).unwrap();
        assert_eq!((mode(&f), serde_json::from_slice::<Value>(&fs::read(&f).unwrap()).unwrap()), (0o600, v));
        let _ = fs::remove_dir_all(&dir);
    }
}
