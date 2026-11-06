//! Stdio upstreams (docs/mcp-proxy.md §2.1, docs/hive-media.md §12.1): a local MCP server that
//! maiTerm starts itself, from a catalogue route `{kind: "stdio_mcp", command, args, env_key}`,
//! and serves at `/mcp/x/<id>` like any proxied server.
//!
//! - **One child per server**, shared by every downstream session. The proxy numbers every
//!   request it sends, so concurrent calls from different agents never collide and each answer
//!   reaches its caller; an agent's own JSON-RPC id never reaches the child.
//! - **Started as `watch.rs` starts a script**: `env_clear`, the login PATH, the key variable,
//!   the route's fixed settings and nothing else; its own process group, killed whole when it
//!   stops (and at the app's quit, `stop_all`); a working directory of its own outside maiTerm's
//!   data folder (`output_folder`).
//! - **The key reaches the child only through its environment.** It is read from the keychain at
//!   spawn; the child's `Running` keeps it only to take it back out of everything the child sends
//!   (`scrub`). The child's stderr is never logged or forwarded (a server may print its own key):
//!   it is drained, and only a line count and the exit status are logged. A child's own error
//!   words never reach a status either (`status_for`).
//! - **Lifecycle.** Restarted on exit, with backoff, while agents are using it; stopped after
//!   `IDLE` with no `tools/call` or `tools/list` from an agent. The proxy's hourly re-list
//!   neither counts as use nor starts it (`running` vs `ensure`). A stopped server is `idle`, not
//!   an outage, and answers `tools/list` from its saved list.

use super::upstream::{UpErr, PROTOCOL};
use super::{ProxyServer, Status};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

/// `ProxyServer::transport` values. Strings, never a serde enum: an older build that meets an
/// unknown variant in `mcp-servers.json` can't read the file at all.
pub const TRANSPORT_HTTP: &str = "http";
pub const TRANSPORT_STDIO: &str = "stdio";

/// Stopped after this long with no `tools/call` or `tools/list` from an agent.
pub const IDLE: Duration = Duration::from_secs(10 * 60);
/// The first start of an `npx`/`uvx` server downloads the package: well over the proxy's 15 s.
pub const VERIFY_WAIT: Duration = Duration::from_secs(180);
/// A later start, from an agent's call: the package is cached by then, but be generous.
pub const START_WAIT: Duration = Duration::from_secs(120);
/// Listing is quick; anything slower is a server in trouble.
const QUICK: Duration = Duration::from_secs(30);
/// A tool call may legitimately run for minutes (a video render).
const CALL: Duration = Duration::from_secs(15 * 60);
const MAX_PAGES: usize = 50;
/// Restart backoff: doubling from the first to the last, and back to the first after a child
/// that stayed up `STABLE`.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(60);
const STABLE: Duration = Duration::from_secs(60);
const SWEEP_EVERY: Duration = Duration::from_secs(30);

/// How a stdio server is started: what its catalogue route said. No secret: `env_key` is the
/// NAME of the variable the key goes in, never the key.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StdioSpec {
    /// The catalogue route this server was set up from: what detection matches it by, since it
    /// has no upstream URL (docs/hive-media.md §5).
    pub route: String,
    /// The program, found on the login PATH (`npx`, `uvx`) or absolute.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// The environment variable the key is given in (`MINIMAX_API_KEY`). `None`: no key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub env_key: Option<String>,
    /// Fixed, non-secret variables the route sets besides the key (`MINIMAX_API_HOST`). From the
    /// catalogue only, never from a webview or an agent. `{output}` in a value is the server's
    /// `output_folder`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// What an agent's `tools/call` may pass (`ArgRule`), from the catalogue route. The proxy
    /// checks every call against these, and the catalogue route's current ones, before the call
    /// reaches the child (`refuse_args`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub arg_rules: Vec<ArgRule>,
}

/// One rule on a stdio server's tool arguments (docs/hive-media.md §12.1). The server runs as the
/// human, outside every sandbox, so an argument that names a path or a URL is a way out for an
/// agent (or a prompt injected into one): these keep its files inside its output folder.
/// `rule` is a String (`RULE_*`), never an enum: an older build that meets an unknown one refuses
/// the call rather than failing to read the file.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ArgRule {
    /// The tool it applies to; "" for every tool.
    #[serde(default)]
    pub tool: String,
    /// The argument it checks; "" for `refuse_tool`.
    #[serde(default)]
    pub arg: String,
    pub rule: String,
    /// `name_part`: how many leading characters become part of a file name (0: all of it).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub head: usize,
    /// `one_of`: the values allowed. `save_inside_output`: top folders (relative to the output
    /// folder) a save may not go under, the server's own bookkeeping (Meshy's `meshy_output`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<String>,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The tool is never called through the proxy.
pub const RULE_REFUSE_TOOL: &str = "refuse_tool";
/// Absent, or a relative folder with no `~` and no `..`: under the server's base folder.
pub const RULE_RELATIVE_DIR: &str = "relative_dir";
/// Absent, or `false`.
pub const RULE_NOT_TRUE: &str = "not_true";
/// Absent, or a path (or a list of paths) inside the output folder once resolved: relative to it,
/// no `~`, no `..`, and every existing part's real path inside it (a symlink out is refused).
pub const RULE_INSIDE_OUTPUT: &str = "inside_output";
/// As `inside_output`, or an `http://`, `https://` or `data:` URL the provider's API fetches.
pub const RULE_URL_OR_INSIDE_OUTPUT: &str = "url_or_inside_output";
/// Where a file is SAVED: as `inside_output`, and no part of the path starts with `.` (a save is
/// never a `.env` the server's dotenv would read at its next start) or sits under one of
/// `values` (the server's own index files).
pub const RULE_SAVE_INSIDE_OUTPUT: &str = "save_inside_output";
/// Text whose first `head` characters (0: all) become part of a file name the server writes
/// (MiniMax's `t2a_<text[:10]>_<date>.mp3`): no `/`, `\` or NUL there, so the name can't walk out
/// of its folder.
pub const RULE_NAME_PART: &str = "name_part";
/// Absent, or exactly one of `values` (MiniMax's `format`, a file's extension).
pub const RULE_ONE_OF: &str = "one_of";
const RULES: [&str; 8] =
    [RULE_REFUSE_TOOL, RULE_RELATIVE_DIR, RULE_NOT_TRUE, RULE_INSIDE_OUTPUT, RULE_URL_OR_INSIDE_OUTPUT, RULE_SAVE_INSIDE_OUTPUT, RULE_NAME_PART, RULE_ONE_OF];

fn name_shaped(n: &str) -> bool {
    !n.is_empty() && n.len() <= 64 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

impl ArgRule {
    fn validate(&self) -> Result<(), String> {
        if !RULES.contains(&self.rule.as_str()) {
            return Err(format!("an argument rule is one of {}", RULES.join(", ")));
        }
        if !self.tool.is_empty() && !name_shaped(&self.tool) {
            return Err("an argument rule's tool is a tool's name".into());
        }
        if self.rule == RULE_REFUSE_TOOL {
            if self.tool.is_empty() || !self.arg.is_empty() {
                return Err("refuse_tool names a tool and no argument".into());
            }
        } else if !name_shaped(&self.arg) {
            return Err(format!("{} names the argument it checks", self.rule));
        }
        if self.rule == RULE_ONE_OF && (self.values.is_empty() || !self.values.iter().all(|v| name_shaped(v))) {
            return Err("one_of lists the values it allows".into());
        }
        if self.rule == RULE_SAVE_INSIDE_OUTPUT && !self.values.iter().all(|v| name_shaped(v)) {
            return Err("save_inside_output's values are top folder names".into());
        }
        Ok(())
    }

    fn applies_to(&self, tool: &str) -> bool {
        self.tool.is_empty() || self.tool == tool
    }
}

/// `true` when `s` names a place inside `output` (which exists): relative to it, with no `~`, no
/// `..` and no NUL, and the real path of its deepest existing part inside `output`'s real path,
/// so neither a lexical trick nor a symlink leads out. What doesn't exist yet can't be a symlink.
pub fn path_inside(s: &str, output: &std::path::Path) -> bool {
    use std::path::Component;
    if s.trim().is_empty() || s.contains('\0') || s.starts_with('~') {
        return false;
    }
    let raw = std::path::Path::new(s);
    if raw.components().any(|c| matches!(c, Component::ParentDir)) {
        return false;
    }
    let Ok(root) = std::fs::canonicalize(output) else { return false };
    let full = if raw.is_absolute() { raw.to_path_buf() } else { output.join(raw) };
    let mut existing = full.as_path();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            let mut real = real;
            for part in rest.iter().rev() {
                real.push(part);
            }
            return real.starts_with(&root);
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else { return false };
        rest.push(name.to_os_string());
        existing = parent;
    }
}

/// `s` as the child would reach it: its deepest existing part's real path (symlinks resolved, and
/// on a case-insensitive volume the on-disk spelling), with the parts that don't exist yet after
/// it. `None` when nothing of it resolves.
fn resolved(s: &str, output: &std::path::Path) -> Option<(PathBuf, PathBuf)> {
    let raw = std::path::Path::new(s);
    let full = if raw.is_absolute() { raw.to_path_buf() } else { output.join(raw) };
    let mut existing = full.as_path();
    let mut rest: Vec<std::ffi::OsString> = Vec::new();
    loop {
        if let Ok(real) = std::fs::canonicalize(existing) {
            let mut whole = real.clone();
            for part in rest.iter().rev() {
                whole.push(part);
            }
            return Some((real, whole));
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else { return None };
        rest.push(name.to_os_string());
        existing = parent;
    }
}

/// Whether `path` (an existing real path) is `dir` or inside it, judged by what is on disk: as
/// path components against `dir`'s real path, and by file identity (device and inode) of `path`
/// and each folder above it, so no spelling the volume folds to the same folder
/// (`MESHY_OUTPUT`, `meſhy_output`) gets past. A `dir` canonicalize can't place refuses.
#[cfg(unix)]
fn same_or_under(path: &std::path::Path, dir: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(real_dir) = std::fs::canonicalize(dir) else { return true };
    if path.starts_with(&real_dir) {
        return true;
    }
    let Ok(id) = std::fs::metadata(&real_dir).map(|m| (m.dev(), m.ino())) else { return true };
    path.ancestors().any(|a| std::fs::metadata(a).is_ok_and(|m| (m.dev(), m.ino()) == id))
}

/// Off unix there is no device and inode to compare (and no stdio server starts there: `spawn`
/// refuses), so the components are compared against `dir`'s real path without regard to case,
/// as the volume (NTFS) folds it.
#[cfg(not(unix))]
fn same_or_under(path: &std::path::Path, dir: &std::path::Path) -> bool {
    let Ok(real_dir) = std::fs::canonicalize(dir) else { return true };
    let lower = |p: &std::path::Path| p.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase()).collect::<Vec<_>>();
    let (path, dir) = (lower(path), lower(&real_dir));
    path.len() >= dir.len() && path[..dir.len()] == dir[..]
}

/// A save's path: inside `output`, no part starting with `.`, and not into a `reserved` folder
/// (relative to `output`), judged by identity on disk rather than by spelling: the output folder
/// sits on a case-insensitive volume, so `MESHY_OUTPUT/history.json` IS `meshy_output/history.json`.
/// Each reserved folder is made first if missing, so there is something on disk to compare with.
fn save_inside(s: &str, output: &std::path::Path, reserved: &[String]) -> bool {
    use std::path::Component;
    if !path_inside(s, output) {
        return false;
    }
    let p = std::path::Path::new(s);
    // Spelling-based on purpose, and enough: ANY part starting with "." is refused, so `.ENV`,
    // `.Env` and every other spelling of a dotfile is caught without asking the disk.
    // Relative to the output folder, as the child (whose working folder it is) reads it; a path
    // inside only through a symlinked prefix is judged on every part.
    let rel: PathBuf = if p.is_absolute() {
        let root = std::fs::canonicalize(output).unwrap_or_else(|_| output.to_path_buf());
        p.strip_prefix(output).or_else(|_| p.strip_prefix(&root)).map_or_else(|_| p.to_path_buf(), |r| r.to_path_buf())
    } else {
        p.to_path_buf()
    };
    let parts: Vec<String> = rel.components().filter_map(|c| match c {
        Component::Normal(n) => Some(n.to_string_lossy().to_string()),
        _ => None,
    }).collect();
    if parts.is_empty() || parts.iter().any(|n| n.starts_with('.')) {
        return false;
    }
    // Made BEFORE the save is resolved: a missing folder would leave the save's deepest existing
    // part above it, and nothing to compare with.
    let dirs: Vec<PathBuf> = reserved.iter().map(|r| output.join(r)).collect();
    if dirs.iter().any(|d| std::fs::create_dir_all(d).is_err()) {
        return false;
    }
    let Some((real, _)) = resolved(s, output) else { return false };
    !dirs.iter().any(|d| same_or_under(&real, d))
}

/// Why `value` breaks `rule`, or `None`.
fn broken(r: &ArgRule, value: &Value, output: &std::path::Path) -> Option<&'static str> {
    let rule = r.rule.as_str();
    let paths = |v: &Value, url_ok: bool| -> Option<&'static str> {
        let one = |s: &str| {
            if url_ok && ["http://", "https://", "data:"].iter().any(|p| s.starts_with(p)) {
                return true;
            }
            path_inside(s, output)
        };
        let ok = match v {
            Value::String(s) => one(s),
            Value::Array(a) => a.iter().all(|x| x.as_str().is_some_and(one)),
            _ => false,
        };
        (!ok).then_some("a path outside the server's output folder")
    };
    match rule {
        RULE_RELATIVE_DIR => {
            let ok = value.as_str().is_some_and(|s| {
                let p = std::path::Path::new(s);
                !s.contains('\0') && !s.starts_with('~') && !s.starts_with('/') && !s.starts_with('\\') && !p.is_absolute()
                    && !p.components().any(|c| matches!(c, std::path::Component::ParentDir))
            });
            (!ok).then_some("a folder outside the server's output folder")
        }
        RULE_NOT_TRUE => (*value != Value::Bool(false)).then_some("a value maiTerm doesn't allow (only false)"),
        RULE_INSIDE_OUTPUT => paths(value, false),
        RULE_URL_OR_INSIDE_OUTPUT => paths(value, true),
        RULE_SAVE_INSIDE_OUTPUT => {
            let ok = value.as_str().is_some_and(|s| save_inside(s, output, &r.values));
            (!ok).then_some("a save outside the server's output folder, a hidden (dot) file, or one of the server's own files")
        }
        RULE_NAME_PART => {
            let ok = value.as_str().is_some_and(|s| {
                let head: String = if r.head == 0 { s.to_string() } else { s.chars().take(r.head).collect() };
                !head.contains(['/', '\\', '\0'])
            });
            (!ok).then_some(if r.head == 0 { "a / or \\ that would become part of a file's path" } else { "a / or \\ near its start, which becomes part of a file's name" })
        }
        RULE_ONE_OF => (!value.as_str().is_some_and(|s| r.values.iter().any(|v| v == s))).then_some("a value maiTerm doesn't allow"),
        _ => Some("an argument maiTerm can't check"),
    }
}

/// Why maiTerm refuses this `tools/call` on stdio server `id` (`params` as the agent sent them),
/// checked against `rules` before anything reaches the child; `None` lets it through. An
/// argument that is absent or null passes every rule but `refuse_tool`.
pub fn refuse_args(id: &str, rules: &[ArgRule], params: &Value) -> Option<String> {
    let tool = params.get("name").and_then(Value::as_str).unwrap_or("");
    let args = params.get("arguments");
    let mut output: Option<PathBuf> = None;
    for r in rules.iter().filter(|r| r.applies_to(tool)) {
        if r.rule == RULE_REFUSE_TOOL {
            return Some(format!("maiTerm doesn't let agents call {tool} on the {id} MCP server: it reaches past the server's own folder on this computer."));
        }
        let Some(v) = args.and_then(|a| a.get(&r.arg)).filter(|v| !v.is_null()) else { continue };
        if output.is_none() {
            let Some(o) = output_folder(id) else { return Some("maiTerm can't find this server's output folder.".into()) };
            let _ = std::fs::create_dir_all(&o);
            output = Some(o);
        }
        if let Some(why) = broken(r, v, output.as_deref().unwrap_or(std::path::Path::new(""))) {
            let folder = output.as_deref().map(|o| o.display().to_string()).unwrap_or_default();
            return Some(format!(
                "maiTerm refused {tool}'s {}: {why}. The {id} server runs outside every sandbox, so its files stay in {folder}; use a path there (or leave {} out).",
                r.arg, r.arg
            ));
        }
    }
    None
}

/// The placeholder a fixed variable's value may hold for the server's output folder.
pub const OUTPUT_PLACEHOLDER: &str = "{output}";

/// Variables neither the key nor a fixed setting may be: the one maiTerm sets itself, and those
/// that change how the child, its shell or its launcher (`npx`, `uvx`) behaves: where it loads
/// code from, where its home, temp and caches are, whom it trusts and which proxy it uses.
const FORBIDDEN_ENV: [&str; 27] = [
    "PATH", "HOME", "TMPDIR", "TMP", "TEMP", "SHELL", "USER", "LOGNAME", "PWD", "IFS", "CDPATH",
    "ENV", "BASH_ENV", "ZDOTDIR", "PERL5OPT", "PERL5LIB", "RUBYOPT", "RUBYLIB", "JAVA_TOOL_OPTIONS",
    "SSL_CERT_FILE", "SSL_CERT_DIR", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
    "REQUESTS_CA_BUNDLE", "CURL_CA_BUNDLE",
];
/// `NODE_*` (NODE_OPTIONS, NODE_PATH, NODE_EXTRA_CA_CERTS), `PYTHON*` (PYTHONPATH, PYTHONHOME,
/// PYTHONSTARTUP), npm's, uv's and pip's own settings, the dynamic loaders', Java's and XDG's.
const FORBIDDEN_ENV_PREFIX: [&str; 11] =
    ["DYLD_", "LD_", "NODE_", "PYTHON", "NPM_CONFIG_", "UV_", "PIP_", "JAVA_", "_JAVA_", "XDG_", "MALLOC"];
/// Settings a route may pin to "" (and to nothing else), though the child may not be given them.
const CLEARABLE_ENV: [&str; 8] =
    ["HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "http_proxy", "https_proxy", "all_proxy", "NODE_TLS_REJECT_UNAUTHORIZED", "NODE_OPTIONS"];
/// What a key variable's name ends in: `^[A-Z][A-Z0-9_]*_(API_KEY|KEY|TOKEN|SECRET)$`.
const KEY_SUFFIX: [&str; 3] = ["_KEY", "_TOKEN", "_SECRET"];

/// A variable a key or a fixed setting may not be, compared without regard to case (npm reads
/// `npm_config_*` either way).
fn forbidden_env(name: &str) -> bool {
    let up = name.to_ascii_uppercase();
    FORBIDDEN_ENV.contains(&up.as_str()) || FORBIDDEN_ENV_PREFIX.iter().any(|p| up.starts_with(p))
}

fn env_name_shaped(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_uppercase() || c == '_')
        && name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

impl StdioSpec {
    pub fn validate(&self) -> Result<(), String> {
        let route_ok = !self.route.is_empty()
            && self.route.len() <= 80
            && self.route.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:".contains(c));
        if !route_ok {
            return Err("a route id is 1–80 letters, digits or - _ . / :".into());
        }
        if self.command.trim().is_empty() {
            return Err("the server has no command".into());
        }
        let bad = |s: &str| s.chars().any(|c| c == '\0' || c == '\n' || c == '\r');
        if bad(&self.command) || self.args.iter().any(|a| bad(a)) {
            return Err("the server's command line has a line break or NUL in it".into());
        }
        if let Some(name) = &self.env_key {
            let keyish = name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && env_name_shaped(name)
                && KEY_SUFFIX.iter().any(|s| name.ends_with(s));
            if !keyish {
                return Err("a key variable is capital letters, digits or _ ending in _KEY, _TOKEN or _SECRET".into());
            }
            if forbidden_env(name) {
                return Err(format!("{name} can't carry a key"));
            }
        }
        for r in &self.arg_rules {
            r.validate()?;
        }
        for (name, value) in &self.env {
            // Cleared to "" so a `.env` the child's dotenv reads can't set it (dotenv never
            // overrides a variable that is set): the proxy variables a request library honours,
            // and Node's TLS and option switches. Only ever cleared, never given a value.
            if CLEARABLE_ENV.contains(&name.as_str()) {
                if !value.is_empty() {
                    return Err(format!("{name} can only be cleared"));
                }
                continue;
            }
            if !env_name_shaped(name) {
                return Err("a setting's variable is 1–64 capital letters, digits or _".into());
            }
            if forbidden_env(name) {
                return Err(format!("the server can't set {name}"));
            }
            if self.env_key.as_deref() == Some(name.as_str()) {
                return Err(format!("{name} is the key's variable"));
            }
            if bad(value) {
                return Err(format!("{name}'s value has a line break or NUL in it"));
            }
        }
        Ok(())
    }

    /// The key is somewhere it must never be: the command line or a fixed setting.
    pub fn carries(&self, key: &str) -> bool {
        !key.is_empty()
            && (self.command.contains(key)
                || self.args.iter().any(|a| a.contains(key))
                || self.env.iter().any(|(n, v)| n.contains(key) || v.contains(key)))
    }

    /// What `ProxyServer::url` holds for a stdio server: no URL to dial, but a self-describing
    /// value an older build reads as a URL it can't reach (it never dials anything for it).
    pub fn url(&self) -> String {
        format!("{STDIO_URL_PREFIX}{}", self.route)
    }
}

/// What a stdio server's `url` starts with, before its route id.
pub const STDIO_URL_PREFIX: &str = "stdio:";

/// The folder a server's child works in, where a server that writes files writes them:
/// `<OS cache dir>/<app_data_slug>/mcp-out/<id>` (`~/Library/Caches/com.aiterm.app/mcp-out/<id>`).
/// Never inside maiTerm's data folder, which `check_output_folders` refuses (docs/hive.md §10.3):
/// this is the folder a Hive adds to its output folders when it gets the tool.
pub fn output_folder(id: &str) -> Option<PathBuf> {
    #[cfg(test)]
    let base = Some(std::env::temp_dir().join(format!("maiterm-test-mcp-out-{}", std::process::id())));
    #[cfg(not(test))]
    let base = dirs::cache_dir().map(|c| c.join(crate::state::persistence::app_data_slug()));
    base.map(|b| b.join("mcp-out").join(id))
}

// ─── The child ──────────────────────────────────────────────────────────────

/// A running child, shared by every downstream session of its server.
pub(super) struct Running {
    stdin: tokio::sync::Mutex<tokio::process::ChildStdin>,
    /// Our request id → the caller waiting for its answer.
    pending: Mutex<HashMap<u64, oneshot::Sender<Value>>>,
    /// The child's `initialize` result: `serverInfo`, `capabilities`, `instructions`.
    pub(super) init: Mutex<Value>,
    /// The process group it leads (`process_group(0)`). Unix only: off unix no child is started
    /// (`spawn` refuses), so there is no group to kill.
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
    pub(super) pid: Option<u32>,
    alive: AtomicBool,
    started: std::time::Instant,
    /// The key this child was given, kept only to take it back out of everything the child sends
    /// (`scrub`): a server may echo its key in an error, a result or a tool's description, and
    /// what it sends reaches statuses, the saved list, Verify's words and the webview.
    key: Option<crate::accounts::vault::Secret>,
}

/// What stands where the child wrote its own key.
pub const KEY_REMOVED: &str = "[key removed]";

/// Replace every occurrence of `key` in `v`'s strings (and object keys) with `KEY_REMOVED`. A key
/// shorter than `secrets::MIN_KEY_LEN` is never stored or started with; were one here, taking it
/// out would mangle every answer, so nothing is done.
fn scrub(v: &mut Value, key: &str) {
    if key.chars().count() < super::secrets::MIN_KEY_LEN {
        return;
    }
    match v {
        Value::String(s) if s.contains(key) => *s = s.replace(key, KEY_REMOVED),
        Value::Array(a) => a.iter_mut().for_each(|x| scrub(x, key)),
        Value::Object(o) => {
            if o.keys().any(|k| k.contains(key)) {
                let old = std::mem::take(o);
                for (k, x) in old {
                    o.insert(k.replace(key, KEY_REMOVED), x);
                }
            }
            o.values_mut().for_each(|x| scrub(x, key));
        }
        _ => {}
    }
}

/// A request's place in `pending`, given up however its caller leaves: answered, timed out, or
/// dropped (an agent that disconnects drops its handler). A call nobody waits for any more must
/// not keep the child from its idle stop.
struct Waiting<'a> {
    r: &'a Running,
    n: u64,
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.r.pending.lock().remove(&self.n);
    }
}

impl std::fmt::Debug for Running {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Running").field("pid", &self.pid).field("alive", &self.alive.load(Ordering::Relaxed)).finish()
    }
}

impl Running {
    pub(super) fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }

    /// Stopped on purpose: marked dead (so its exit is not restarted), its group killed, and
    /// every call waiting on it told.
    fn halt(&self) {
        self.alive.store(false, Ordering::SeqCst);
        self.kill_group();
        self.pending.lock().clear();
    }

    fn kill_group(&self) {
        #[cfg(unix)]
        if let Some(pg) = self.pgid {
            // SAFETY: signalling the process group this child leads (process_group(0)); a stale
            // id at worst names an empty group.
            unsafe { libc::killpg(pg, libc::SIGKILL) };
        }
    }

    async fn send(&self, msg: &Value) -> Result<(), UpErr> {
        let mut line = msg.to_string();
        line.push('\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(line.as_bytes()).await.map_err(|_| UpErr::Net("its process stopped reading".into()))?;
        stdin.flush().await.map_err(|_| UpErr::Net("its process stopped reading".into()))
    }

    /// One request, under maiTerm's own id: the answer is matched back by it, whoever asked.
    async fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, UpErr> {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().insert(n, tx);
        let _waiting = Waiting { r: self, n };
        // Checked AFTER the insert: an exit marks the child dead before it clears what waits, so
        // a request that lands in between is either cleared or sees it dead here, never stranded
        // until its timeout.
        if !self.is_alive() {
            return Err(UpErr::Net("its process exited".into()));
        }
        self.send(&json!({ "jsonrpc": "2.0", "id": n, "method": method, "params": params })).await?;
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(mut resp)) => {
                if let Some(key) = &self.key {
                    scrub(&mut resp, key.expose());
                }
                super::upstream::result_of(resp)
            }
            Ok(Err(_)) => Err(UpErr::Net("its process exited".into())),
            Err(_) => Err(UpErr::Net(format!("no answer within {}s", timeout.as_secs()))),
        }
    }

    pub(super) async fn list_tools(&self) -> Result<Vec<Value>, UpErr> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params, QUICK).await?;
            if let Some(page) = result.get("tools").and_then(Value::as_array) {
                tools.extend(page.iter().cloned());
            }
            cursor = result.get("nextCursor").and_then(Value::as_str).filter(|c| !c.is_empty()).map(str::to_string);
            if cursor.is_none() {
                return Ok(tools);
            }
        }
        Err(UpErr::Protocol(format!("tools/list kept paginating past {MAX_PAGES} pages")))
    }

    /// The caller's `params` as sent (`name`, `arguments`, `_meta`); the result verbatim.
    pub(super) async fn call_tool(&self, params: Value) -> Result<Value, UpErr> {
        self.request("tools/call", params, CALL).await
    }
}

/// One server's child, across restarts.
#[derive(Default)]
struct Slot {
    /// One start or stop at a time.
    gate: tokio::sync::Mutex<()>,
    running: Mutex<Option<Arc<Running>>>,
    /// A child still in its handshake, so a stop (a removal, a new key, a disable, the app's quit)
    /// kills it too rather than finding nothing and letting it come up afterwards.
    starting: Mutex<Option<Arc<Running>>>,
    /// The last `tools/call` or `tools/list` from an AGENT, or a start someone asked for (Verify,
    /// a Refresh, a balance check), in ms. Never a restart's. What the idle stop
    /// reads; the proxy's own reads never touch it.
    last_use: AtomicU64,
    /// Bumped by every deliberate stop: the exit of a child from before it is not restarted, and a
    /// child that was starting meanwhile is killed, not kept.
    generation: AtomicU64,
    backoff_ms: AtomicU64,
    /// A restart loop is running: one per server, however many children exit meanwhile.
    restarting: AtomicBool,
}

static SLOTS: LazyLock<Mutex<HashMap<String, Arc<Slot>>>> = LazyLock::new(Default::default);

fn slot(id: &str) -> Arc<Slot> {
    SLOTS.lock().entry(id.to_string()).or_default().clone()
}

fn now_ms() -> u64 {
    super::now_ms()
}

/// The child, if it is running now. Never starts one: what the hourly re-list asks.
pub(super) fn running(id: &str) -> Option<Arc<Running>> {
    let s = SLOTS.lock().get(id).cloned()?;
    let r = s.running.lock().clone()?;
    r.is_alive().then_some(r)
}

/// An agent listed or called a tool: the child stays up another `IDLE`.
pub(super) fn note_agent_use(id: &str) {
    slot(id).last_use.store(now_ms(), Ordering::SeqCst);
}

/// Why a child could not be had.
#[derive(Debug)]
pub(super) enum StartErr {
    /// The server needs a key and none is stored, or the keychain refused to give it.
    NoKey(String),
    /// It could not be spawned, or didn't complete its handshake.
    Up(UpErr),
}

/// Whether a start counts as use, keeping the child up `IDLE` from it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Start {
    /// An agent's call, Verify, a human's Refresh, a balance check.
    Asked,
    /// The restart loop's own: never use, or a child that keeps failing would be restarted for
    /// as long as the app runs.
    Restart,
}

fn stopped_while_starting() -> StartErr {
    StartErr::Up(UpErr::Net("it was stopped while it was starting".into()))
}

/// The running child, started if it isn't (one start at a time per server).
pub(super) async fn ensure(server: &ProxyServer, wait: Duration, why: Start) -> Result<Arc<Running>, StartErr> {
    let s = slot(&server.id);
    // Read BEFORE waiting for the gate: a stop (a disable, a removal, a new key) that lands while
    // this caller waits behind another's start fails this caller too, rather than letting it
    // start a child afterwards for a server that was stopped meanwhile.
    let generation = s.generation.load(Ordering::SeqCst);
    let _gate = s.gate.lock().await;
    if s.generation.load(Ordering::SeqCst) != generation {
        return Err(stopped_while_starting());
    }
    if let Some(r) = s.running.lock().clone().filter(|r| r.is_alive()) {
        return Ok(r);
    }
    let spawned = spawn(server, wait, generation, &s).await;
    *s.starting.lock() = None;
    let r = match spawned {
        Ok(r) => r,
        Err(_) if s.generation.load(Ordering::SeqCst) != generation => return Err(stopped_while_starting()),
        Err(e) => return Err(e),
    };
    {
        // A stop that ran while it started (the server removed, its key replaced, disabled)
        // bumped the generation, or a removal dropped the slot: the child is not kept. Checked
        // under the lock `stop` takes after its bump, so one of the two always sees the other.
        let current = SLOTS.lock().get(&server.id).is_some_and(|cur| Arc::ptr_eq(cur, &s));
        let mut running = s.running.lock();
        if !current || s.generation.load(Ordering::SeqCst) != generation {
            drop(running);
            r.halt();
            return Err(stopped_while_starting());
        }
        *running = Some(r.clone());
    }
    if why == Start::Asked {
        s.last_use.fetch_max(now_ms(), Ordering::SeqCst);
    }
    start_sweeper();
    Ok(r)
}

/// Stop a server's child, whole process group, on purpose: it is not restarted. A child still
/// starting is killed too. Whether there was one.
pub(super) fn stop(id: &str) -> bool {
    let Some(s) = SLOTS.lock().get(id).cloned() else { return false };
    s.generation.fetch_add(1, Ordering::SeqCst);
    let starting = s.starting.lock().take();
    let running = s.running.lock().take();
    let mut any = false;
    for r in starting.iter().chain(running.iter()) {
        r.halt();
        any = true;
    }
    any
}

/// Stop every server's child: the app is quitting. A child runs in its own process group, outside
/// maiTerm's, so nothing else ends it, and it holds its key in its environment.
pub fn stop_all() -> usize {
    stop_where(|_| true)
}

fn stop_where(which: impl Fn(&str) -> bool) -> usize {
    let ids: Vec<String> = SLOTS.lock().keys().filter(|id| which(id)).cloned().collect();
    let n = ids.iter().filter(|id| stop(id)).count();
    if n > 0 {
        log::info!("[mcp-proxy] stopped {n} stdio servers");
    }
    n
}

/// Forget a removed server's child and its slot.
pub(super) fn forget(id: &str) {
    stop(id);
    SLOTS.lock().remove(id);
}

/// Stop every child no agent has used for `idle`. Returns the ids stopped.
pub(super) fn stop_idle(now: u64, idle: Duration) -> Vec<String> {
    stop_idle_where(now, idle, |_| true)
}

fn stop_idle_where(now: u64, idle: Duration, which: impl Fn(&str) -> bool) -> Vec<String> {
    let slots: Vec<(String, Arc<Slot>)> =
        SLOTS.lock().iter().filter(|(k, _)| which(k)).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut out = Vec::new();
    for (id, s) in slots {
        // A call still running (a long render) is use, however long ago it was made.
        let up = s.running.lock().as_ref().is_some_and(|r| r.is_alive() && r.pending.lock().is_empty());
        if up && now.saturating_sub(s.last_use.load(Ordering::SeqCst)) >= idle.as_millis() as u64 && stop(&id) {
            log::info!("[mcp-proxy] {id}: stopped its stdio server, unused for {} min", idle.as_secs() / 60);
            super::set_status(&id, Status::new(super::STATE_IDLE, ""));
            out.push(id);
        }
    }
    out
}

fn start_sweeper() {
    // Tests drive `stop_idle` themselves, with their own clock.
    if cfg!(test) {
        return;
    }
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    tokio::spawn(async {
        loop {
            tokio::time::sleep(SWEEP_EVERY).await;
            stop_idle(now_ms(), IDLE);
        }
    });
}

/// The child's environment: the login PATH and the key, and nothing else. Built apart from the
/// spawn so a test can see exactly what goes where.
pub(super) struct Plan {
    pub(super) program: String,
    pub(super) args: Vec<String>,
    pub(super) cwd: PathBuf,
    pub(super) path: String,
    /// The key's variable and value. `Secret`'s `Debug` is redacted.
    pub(super) key: Option<(String, crate::accounts::vault::Secret)>,
    /// The route's fixed settings, `{output}` expanded. Set after the key.
    pub(super) env: Vec<(String, String)>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("program", &self.program)
            .field("args", &self.args)
            .field("cwd", &self.cwd)
            .field("key", &self.key.as_ref().map(|(name, s)| (name, s)))
            .field("env", &self.env)
            .finish()
    }
}

pub(super) async fn plan(server: &ProxyServer) -> Result<Plan, StartErr> {
    let spec = server.stdio.as_ref().ok_or_else(|| StartErr::Up(UpErr::Protocol("its command is missing".into())))?;
    let key = match &spec.env_key {
        None => None,
        Some(name) => {
            let id = server.id.clone();
            let read = tokio::task::spawn_blocking(move || super::secrets::read_env_key(&id))
                .await
                .map_err(|e| StartErr::NoKey(e.to_string()))?
                .map_err(|e| StartErr::NoKey(e.to_string()))?;
            Some((name.clone(), read.ok_or_else(|| StartErr::NoKey("no key stored".into()))?))
        }
    };
    // Stored before keys had a minimum: refused, never started with.
    if key.as_ref().is_some_and(|(_, k)| k.expose().chars().count() < super::secrets::MIN_KEY_LEN) {
        return Err(StartErr::NoKey("the stored key doesn't look like a whole key".into()));
    }
    // A key replaced since the route was added could be one its command line or settings hold.
    if key.as_ref().is_some_and(|(_, k)| spec.carries(k.expose())) {
        return Err(StartErr::NoKey("the stored key is on its command line or in its settings".into()));
    }
    let cwd = output_folder(&server.id).ok_or_else(|| StartErr::Up(UpErr::Protocol("no cache folder on this computer".into())))?;
    let out = cwd.to_string_lossy();
    let env = spec.env.iter().map(|(n, v)| (n.clone(), v.replace(OUTPUT_PLACEHOLDER, &out))).collect();
    Ok(Plan { program: spec.command.clone(), args: spec.args.clone(), cwd, path: crate::watch::login_path().await, key, env })
}

#[cfg(not(unix))]
async fn spawn(_server: &ProxyServer, _wait: Duration, _generation: u64, _s: &Slot) -> Result<Arc<Running>, StartErr> {
    Err(StartErr::Up(UpErr::Protocol("stdio MCP servers run on macOS and Linux only".into())))
}

#[cfg(unix)]
async fn spawn(server: &ProxyServer, wait: Duration, generation: u64, s: &Slot) -> Result<Arc<Running>, StartErr> {
    use std::process::Stdio;
    let id = server.id.clone();
    let mut plan = plan(server).await?;
    std::fs::create_dir_all(&plan.cwd).map_err(|e| StartErr::Up(UpErr::Protocol(format!("could not create its folder: {}", e.kind()))))?;
    let mut cmd = tokio::process::Command::new(&plan.program);
    cmd.args(&plan.args).current_dir(&plan.cwd).env_clear().env("PATH", &plan.path);
    if let Some((name, key)) = &plan.key {
        cmd.env(name, key.expose());
    }
    cmd.envs(plan.env.iter().map(|(n, v)| (n, v)));
    cmd.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0).kill_on_drop(true);
    // The spawn error is the OS's ("No such file or directory"), never the command line.
    let mut child = cmd.spawn().map_err(|e| StartErr::Up(UpErr::Net(format!("its command could not start ({})", e.kind()))))?;
    drop(cmd);
    let key = plan.key.take().map(|(_, k)| k);
    drop(plan);
    let pid = child.id();
    let (Some(stdin), Some(stdout), Some(stderr)) = (child.stdin.take(), child.stdout.take(), child.stderr.take()) else {
        let _ = child.start_kill();
        return Err(StartErr::Up(UpErr::Protocol("its pipes could not be opened".into())));
    };
    let r = Arc::new(Running {
        stdin: tokio::sync::Mutex::new(stdin),
        pending: Mutex::new(HashMap::new()),
        init: Mutex::new(Value::Null),
        pgid: pid.map(|p| p as libc::pid_t),
        pid,
        alive: AtomicBool::new(true),
        started: std::time::Instant::now(),
        key,
    });
    let stderr_lines = Arc::new(AtomicU64::new(0));
    tokio::spawn(read_stdout(id.clone(), Arc::downgrade(&r), stdout));
    tokio::spawn(drain_stderr(stderr, stderr_lines.clone()));
    tokio::spawn(wait_exit(id.clone(), r.clone(), child, stderr_lines, generation));
    {
        // Seen by a stop from here on; a stop before this bumped the generation.
        let mut starting = s.starting.lock();
        if s.generation.load(Ordering::SeqCst) != generation {
            drop(starting);
            r.halt();
            return Err(stopped_while_starting());
        }
        *starting = Some(r.clone());
    }

    let init = r
        .request(
            "initialize",
            json!({
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "maiterm-mcp-proxy", "title": crate::APP_DISPLAY_NAME, "version": crate::APP_VERSION },
            }),
            wait,
        )
        .await;
    let init = match init {
        Ok(v) => v,
        Err(e) => {
            // A handshake that failed is not restarted by its exit: whoever asked decides.
            r.halt();
            return Err(StartErr::Up(e));
        }
    };
    *r.init.lock() = init;
    if let Err(e) = r.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" })).await {
        r.halt();
        return Err(StartErr::Up(e));
    }
    log::info!("[mcp-proxy] {id}: started its stdio server ({})", server.stdio.as_ref().map_or("", |s| s.route.as_str()));
    Ok(r)
}

/// The child's stdout: one JSON-RPC message per line. Answers go to whoever waits for them, a
/// list change is announced, a request the child makes is answered (the proxy advertises no
/// client capabilities, so only `ping` is served). Anything that isn't JSON is skipped: a
/// package's banner on stdout is common.
async fn read_stdout(id: String, r: std::sync::Weak<Running>, stdout: tokio::process::ChildStdout) {
    let mut reader = tokio::io::BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        line.clear();
        match reader.read_until(b'\n', &mut line).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let Ok(msg) = serde_json::from_slice::<Value>(&line) else { continue };
        let Some(r) = r.upgrade() else { return };
        let method = msg.get("method").and_then(Value::as_str);
        match (method, msg.get("id")) {
            (None, Some(n)) => {
                if let Some(tx) = n.as_u64().and_then(|n| r.pending.lock().remove(&n)) {
                    let _ = tx.send(msg);
                }
            }
            (Some(m), Some(rid)) => {
                let reply = if m == "ping" {
                    json!({ "jsonrpc": "2.0", "id": rid, "result": {} })
                } else {
                    json!({ "jsonrpc": "2.0", "id": rid, "error": { "code": -32601, "message": "maiTerm's proxy answers no requests from a server" } })
                };
                let _ = r.send(&reply).await;
            }
            (Some(m), None) if m == "notifications/tools/list_changed" => {
                let id = id.clone();
                read_list_soon(id.clone());
            }
            _ => {}
        }
    }
}

/// Read stderr to its end and keep nothing but the count of lines.
async fn drain_stderr(stderr: tokio::process::ChildStderr, lines: Arc<AtomicU64>) {
    let mut reader = tokio::io::BufReader::new(stderr);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => return,
            Ok(_) => {
                lines.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Wait for the child to end; then kill what is left of its group, fail the calls still waiting,
/// and restart it if agents are still using it and nobody stopped it on purpose.
async fn wait_exit(id: String, r: Arc<Running>, mut child: tokio::process::Child, stderr_lines: Arc<AtomicU64>, generation: u64) {
    let status = child.wait().await;
    let deliberate = !r.alive.swap(false, Ordering::SeqCst);
    r.kill_group();
    r.pending.lock().clear();
    // Let the stderr reader count the last lines.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let how = match &status {
        Ok(s) => match s.code() {
            Some(c) => format!("exit {c}"),
            None => "killed by a signal".to_string(),
        },
        Err(_) => "its status could not be read".to_string(),
    };
    log::info!("[mcp-proxy] {id}: stdio server ended ({how}); {} stderr lines", stderr_lines.load(Ordering::Relaxed));
    let Some(s) = SLOTS.lock().get(&id).cloned() else { return };
    {
        let mut slot_running = s.running.lock();
        if slot_running.as_ref().is_some_and(|cur| Arc::ptr_eq(cur, &r)) {
            *slot_running = None;
        }
    }
    if deliberate || s.generation.load(Ordering::SeqCst) != generation {
        return;
    }
    if r.started.elapsed() >= STABLE {
        s.backoff_ms.store(0, Ordering::SeqCst);
    }
    if s.restarting.swap(true, Ordering::SeqCst) {
        return;
    }
    restart_soon(id, s, generation, how);
}

// The two below are plain functions on purpose: a child's tasks start reads and restarts, which
// start children, and an `async fn` calling back into `spawn` makes its future's type recursive.

/// Re-read the list the child just said changed, on a task of its own.
fn read_list_soon(id: String) {
    tokio::spawn(async move {
        let _ = super::refresh_stdio(&id, false).await;
    });
}

fn restart_soon(id: String, s: Arc<Slot>, generation: u64, how: String) {
    tokio::spawn(async move {
        restart(id, s.clone(), generation, how).await;
        s.restarting.store(false, Ordering::SeqCst);
    });
}

fn recently_used(s: &Slot) -> bool {
    now_ms().saturating_sub(s.last_use.load(Ordering::SeqCst)) < IDLE.as_millis() as u64
}

async fn restart(id: String, s: Arc<Slot>, generation: u64, how: String) {
    loop {
        let wanted = super::get(&id).filter(|srv| srv.enabled && srv.is_stdio());
        let Some(server) = wanted else { return };
        if !recently_used(&s) {
            super::set_status(&id, Status::new(super::STATE_IDLE, ""));
            return;
        }
        let prev = s.backoff_ms.load(Ordering::SeqCst);
        let wait = if prev == 0 { BACKOFF_FIRST } else { (Duration::from_millis(prev) * 2).min(BACKOFF_MAX) };
        s.backoff_ms.store(wait.as_millis() as u64, Ordering::SeqCst);
        super::set_status(&id, Status::new("unreachable", format!("its process ended ({how}); restarting in {}s", wait.as_secs())));
        tokio::time::sleep(wait).await;
        if s.generation.load(Ordering::SeqCst) != generation {
            return;
        }
        match ensure(&server, START_WAIT, Start::Restart).await {
            Ok(_) => {
                super::set_status(&id, Status::new("connected", "restarted"));
                return;
            }
            Err(e) => {
                super::set_status(&id, super::stdio_status(&server, &e));
            }
        }
    }
}

/// What a failed start or exchange means, in words of maiTerm's: a child's own error message
/// never reaches a status (it is logged and sent to the frontend), since a server may put its key
/// in one. A tool call's JSON-RPC error still goes back to the agent that made it, as sent.
pub(super) fn status_for(e: &UpErr) -> Status {
    match e {
        UpErr::Net(detail) => Status::new("unreachable", detail.clone()),
        UpErr::Rpc { code, .. } => Status::new("error", format!("it answered with error {code}")),
        UpErr::Protocol(_) => Status::new("error", "it answered with something that isn't MCP"),
        other => Status::new("error", other.to_string()),
    }
}

// Used only by the tests that start the fake, which are unix only.
#[cfg(all(test, unix))]
pub(super) fn age_for_test(id: &str, by: Duration) {
    let s = slot(id);
    let back = now_ms().saturating_sub(by.as_millis() as u64);
    s.last_use.store(back, Ordering::SeqCst);
}

/// The idle stop for these servers only: a test's sweep never stops another test's child.
// Used only by the tests that start the fake, which are unix only.
#[cfg(all(test, unix))]
pub(super) fn stop_idle_for_test(id: &str, now: u64, idle: Duration) -> Vec<String> {
    stop_idle_where(now, idle, |i| i == id)
}

// Used only by the tests that start the fake, which are unix only.
#[cfg(all(test, unix))]
pub(super) fn stop_for_test(ids: &[&str]) -> usize {
    stop_where(|i| ids.contains(&i))
}

// Used only by the tests that start the fake, which are unix only.
#[cfg(all(test, unix))]
pub(super) fn last_use_for_test(id: &str) -> u64 {
    slot(id).last_use.load(Ordering::SeqCst)
}

#[cfg(test)]
#[path = "stdio_tests.rs"]
mod tests;
