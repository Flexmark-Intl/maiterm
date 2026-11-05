//! `hive_media_verify` (docs/hive-media.md §6, "Verify"): after a setup route finishes, is the
//! provider really usable by this Hive's agents, and if not, what does the human do about it.
//!
//! For a provider maiTerm serves through its MCP proxy, in order:
//! 1. **The server is on and in scope** for the Hive's workspace (`gate`).
//! 2. **Its entry reaches agents** (`entry_problem`): the proxied entry is written, provably ours,
//!    into `~/.claude.json` and every managed Claude account root. The registry alone is not enough:
//!    with Claude Code IDE integration off no entry is written, and `hive_media_status` (which
//!    reads the registry) still calls the server ready.
//! 3. **Its tools list** through the proxy (`tools/list`, free, the proxy's own `refresh`).
//! 4. **Its balance or pricing tool answers**, where it has one, called with the cheapest harmless
//!    arguments it takes. Only the tools in `CHECK_TOOLS` are ever called, each of which reads a
//!    balance, a price or the account and spends nothing; a generation tool never is, whatever the
//!    catalogue says.
//!
//! Any other provider (Homebrew kit, a plugin, an app) is re-detected with `status::hits`.
//!
//! Nothing here changes the proxy's saved tool list (docs/mcp-proxy.md §4.1): a failed read leaves
//! it as it was, and a check call never touches it. Every kind on the wire is a String. No message
//! carries a credential: the words come from maiTerm, the server's own text only as a short excerpt.

use std::future::Future;

use serde::Serialize;
use serde_json::{json, Value};

use super::status::{self, Hit, ProxyInput, INSTALLED, NEEDS_SIGN_IN, READY, UNREACHABLE};
use super::{provider, providers_for_upstream, Provider};
use crate::mcp_proxy::entries::{entry_name, origin, Origin};
use crate::mcp_proxy::upstream::UpErr;

/// Signed in, but the account has no credit for a job.
pub const NEEDS_CREDIT: &str = "needs_credit";
/// The provider answered with something maiTerm couldn't use.
pub const ERROR: &str = "error";

/// What Verify found, in maiTerm's words. `state` is `ready` | `needs_sign_in` | `needs_credit` |
/// `unreachable` | `error`, and `installed` for something present that maiTerm can't check (a
/// plugin switched off, an app not running, the human's own MCP entry).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct VerifyResult {
    pub state: String,
    pub message: String,
    /// Where to fix it, for a button: the catalogue's `billing_url`, only ever http(s).
    pub link: Option<String>,
    /// How many tools the server listed, for an MCP provider whose list was read.
    pub tool_count: Option<usize>,
}

impl VerifyResult {
    fn new(state: &str, message: impl Into<String>) -> Self {
        Self { state: state.to_string(), message: message.into(), link: None, tool_count: None }
    }
}

// ─── The only tools Verify calls ────────────────────────────────────────────

/// A tool Verify may call: it reads a balance, a price or the account, and spends nothing.
pub struct CheckTool {
    pub provider: &'static str,
    pub name: &'static str,
    /// What the message calls it: "its pricing check".
    pub what: &'static str,
    /// Values for arguments the tool requires (only those its schema requires are sent).
    hints: fn() -> Value,
}

fn no_hints() -> Value {
    json!({})
}

/// fal's `get_pricing` prices model endpoints: asking the price of one model reads a table and
/// runs nothing. Either spelling its schema may require.
fn fal_hints() -> Value {
    json!({ "endpoint_ids": ["fal-ai/flux/schnell"], "endpoint_id": "fal-ai/flux/schnell" })
}

/// The allowlist. A tool not here is never called by Verify, whatever the catalogue's
/// `quote_tool` or `balance_tool` names (WaveSpeed's `get_price` included: it prices a job's
/// arguments, which Verify has none of).
pub const CHECK_TOOLS: &[CheckTool] = &[
    CheckTool { provider: "fal", name: "get_pricing", what: "pricing check", hints: fal_hints },
    CheckTool { provider: "wavespeed", name: "get_balance", what: "balance check", hints: no_hints },
    CheckTool { provider: "bfl", name: "get_credits", what: "credit check", hints: no_hints },
    CheckTool { provider: "recraft", name: "get_user", what: "account check", hints: no_hints },
    CheckTool { provider: "meshy", name: "meshy_check_balance", what: "balance check", hints: no_hints },
];

/// The allowlisted check `name` for `provider`, or why Verify refuses to call it.
pub fn check_tool(provider: &str, name: &str) -> Result<&'static CheckTool, String> {
    CHECK_TOOLS
        .iter()
        .find(|c| c.provider == provider && c.name == name)
        .ok_or_else(|| format!("Verify never calls {name}: it calls only a balance or pricing check that spends nothing"))
}

/// The arguments to send: each property the schema REQUIRES, from the hints, else its `default`,
/// else its first `enum` value. Optional ones are left out. `None`: a required argument maiTerm
/// has no harmless value for, so the tool isn't called.
pub fn check_args(schema: Option<&Value>, hints: &Value) -> Option<Value> {
    let props = schema.and_then(|s| s.get("properties")).and_then(Value::as_object);
    let required: Vec<&str> = schema.and_then(|s| s.get("required")).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let mut args = serde_json::Map::new();
    for name in required {
        let declared = props.and_then(|p| p.get(name));
        let value = hints
            .get(name)
            .cloned()
            .or_else(|| declared.and_then(|d| d.get("default")).cloned())
            .or_else(|| declared.and_then(|d| d.get("enum")).and_then(Value::as_array).and_then(|e| e.first()).cloned())?;
        args.insert(name.to_string(), value);
    }
    Some(Value::Object(args))
}

/// Which check to run on a server that listed `tools`: the provider's balance tool, else its
/// pricing tool, as long as it is allowlisted, listed, and its arguments can be filled.
pub fn plan_check(p: &Provider, tools: &[Value]) -> Option<(&'static CheckTool, Value)> {
    let named = p.routes.iter().flat_map(|r| [r.balance_tool.as_deref(), r.quote_tool.as_deref()]).flatten();
    for name in named {
        let Ok(check) = check_tool(&p.id, name) else { continue };
        let Some(tool) = tools.iter().find(|t| t.get("name").and_then(Value::as_str) == Some(name)) else { continue };
        if let Some(args) = check_args(tool.get("inputSchema"), &(check.hints)()) {
            return Some((check, args));
        }
    }
    None
}

// ─── Reading through the proxy ──────────────────────────────────────────────

/// A failed read or call, as Verify words it.
#[derive(Debug, Clone, PartialEq)]
pub struct Failure {
    /// `needs_sign_in` | `unreachable` | `error`.
    pub state: String,
    /// The HTTP status the server answered with, when that is what failed.
    pub http: Option<u16>,
    /// The server's JSON-RPC error code, when it answered with one.
    pub rpc: Option<i64>,
    pub detail: String,
}

impl Failure {
    pub fn from_up(e: &UpErr) -> Self {
        let (state, http, rpc) = match e {
            UpErr::Unauthorized { .. } => (NEEDS_SIGN_IN, Some(401), None),
            UpErr::Forbidden { .. } => (NEEDS_SIGN_IN, Some(403), None),
            UpErr::Http { status, .. } => (UNREACHABLE, Some(*status), None),
            UpErr::Net(_) | UpErr::SessionExpired => (UNREACHABLE, None, None),
            UpErr::Rpc { code, .. } => (ERROR, None, Some(*code)),
            UpErr::Protocol(_) => (ERROR, None, None),
        };
        Self { state: state.to_string(), http, rpc, detail: e.to_string() }
    }
}

/// How Verify reaches a proxied server. The real one is the MCP proxy (`ProxyProbe`); tests stub it.
pub trait Probe: Sync {
    /// Read the server's tool list now: the tools, or why not.
    fn list(&self, id: &str) -> impl Future<Output = Result<Vec<Value>, Failure>> + Send;
    /// Call one tool. Only `run_check` calls this, after the allowlist.
    fn call(&self, id: &str, tool: &str, args: Value) -> impl Future<Output = Result<Value, Failure>> + Send;
}

/// The MCP proxy: the list through its own `refresh` (which saves a good list and leaves the saved
/// one alone on failure), a call through `call_once` on a session of its own.
pub struct ProxyProbe;

impl Probe for ProxyProbe {
    async fn list(&self, id: &str) -> Result<Vec<Value>, Failure> {
        let read = crate::mcp_proxy::refresh(id).await;
        let st = crate::mcp_proxy::status(id);
        match read {
            // The list reads, but the server refuses calls: that is a sign-in, not readiness.
            Ok(_) if st.state == "needs_sign_in" => Err(Failure { state: NEEDS_SIGN_IN.into(), http: None, rpc: None, detail: st.detail }),
            Ok(_) => Ok(crate::mcp_proxy::get(id).and_then(|s| s.catalog).map(|c| c.tools).unwrap_or_default()),
            Err(why) => {
                let detail = if st.detail.is_empty() { why } else { st.detail };
                let state = match st.state.as_str() {
                    "needs_sign_in" => NEEDS_SIGN_IN,
                    "error" => ERROR,
                    _ => UNREACHABLE,
                };
                Err(Failure { state: state.into(), http: http_in(&detail), rpc: None, detail })
            }
        }
    }

    async fn call(&self, id: &str, tool: &str, args: Value) -> Result<Value, Failure> {
        crate::mcp_proxy::call_once(id, json!({ "name": tool, "arguments": args })).await.map_err(|e| Failure::from_up(&e))
    }
}

/// The status in a proxy failure's words ("HTTP 402: …"), if there is one.
fn http_in(detail: &str) -> Option<u16> {
    let rest = &detail[detail.find("HTTP ")? + 5..];
    let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
    (code.len() == 3).then(|| code.parse().ok()).flatten()
}

/// Call `name` on server `id`, refusing anything not in `CHECK_TOOLS` for this provider before a
/// byte is sent.
pub async fn run_check<P: Probe>(probe: &P, p: &Provider, id: &str, name: &str, args: Value) -> Result<Result<Value, Failure>, String> {
    let check = check_tool(&p.id, name)?;
    Ok(probe.call(id, check.name, args).await)
}

// ─── Does the entry reach agents ────────────────────────────────────────────

/// One Claude config file agents read their MCP servers from.
#[derive(Debug, Clone)]
pub struct ConfigFile {
    /// How the message names it: `~/.claude.json`, `account Work's Claude settings`.
    pub label: String,
    /// Its contents; `None` when it is missing or unreadable.
    pub json: Option<Value>,
}

/// What decides whether a proxied entry reaches agents.
#[derive(Debug, Clone)]
pub struct Reach {
    /// `Preferences.claude_ide`: maiTerm writes the entries only while it is on.
    pub claude_ide: bool,
    /// The port this launch's MCP server is bound to: an entry naming any other is stale.
    pub port: Option<u16>,
    /// `~/.claude.json`, then each managed Claude account root's that exists.
    pub files: Vec<ConfigFile>,
}

/// Read `~/.claude.json` and every managed Claude account's. A root with no file yet is skipped:
/// nothing has launched under it, and its first launch copies the entries in.
pub fn reach(prefs: &crate::state::Preferences) -> Reach {
    let read = |path: &std::path::Path| std::fs::read_to_string(path).ok().and_then(|r| serde_json::from_str::<Value>(&r).ok());
    let mut files = Vec::new();
    if let Some(path) = crate::claude_code::lockfile::claude_settings_path() {
        files.push(ConfigFile { label: "~/.claude.json".into(), json: read(&path) });
    }
    for a in prefs.managed_accounts.iter().filter(|a| a.runtime == crate::accounts::Runtime::Claude.slug()) {
        let Some(path) = crate::accounts::account_root(crate::accounts::Runtime::Claude, &a.id).map(|r| r.join(".claude.json")) else { continue };
        if path.exists() {
            files.push(ConfigFile { label: format!("account {}'s Claude settings", a.label), json: read(&path) });
        }
    }
    Reach { claude_ide: prefs.claude_ide, port: crate::mcp_proxy::live_port(), files }
}

fn join_words(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [init @ .., last] => format!("{} and {last}", init.join(", ")),
    }
}

/// Why server `id`'s entry doesn't reach agents, in a sentence that says what to fix; `None` when
/// every file holds this launch's entry for it.
pub fn entry_problem(p: &Provider, id: &str, reach: &Reach) -> Option<String> {
    let name = &p.name;
    if !reach.claude_ide {
        return Some(format!(
            "{name} is set up in maiTerm, but no agent can see it: maiTerm writes MCP servers into Claude's settings only while Claude Code IDE integration is on. Turn it on in Preferences → AI Agents."
        ));
    }
    if reach.port.is_none() {
        return Some(format!("maiTerm's MCP server isn't running, so no agent can reach {name} through it. Restart maiTerm."));
    }
    let entry = entry_name(id);
    let (mut held, mut missing) = (Vec::new(), Vec::new());
    for f in &reach.files {
        let found = f.json.as_ref().and_then(|j| j.get("mcpServers")).and_then(|m| m.get(&entry));
        match found.map(|e| origin(e, reach.port)) {
            Some(Origin::Ours(i)) if i == id => {}
            Some(Origin::Native) => held.push(f.label.clone()),
            _ => missing.push(f.label.clone()),
        }
    }
    if !held.is_empty() {
        return Some(format!(
            "Your own MCP server named \"{entry}\" holds that name in {}, so maiTerm can't write {name}'s entry there. Rename or remove yours, or add {name} to maiTerm under another name.",
            join_words(&held)
        ));
    }
    if !missing.is_empty() {
        return Some(format!(
            "{name}'s entry isn't written in {} yet, so agents there don't see it. maiTerm rewrites its entries every 30 seconds: verify again in a minute, and restart maiTerm if it stays.",
            join_words(&missing)
        ));
    }
    None
}

/// Whether a server offers its tools to `workspace` (none: Preferences, where scope isn't judged).
fn in_scope(s: &ProxyInput, workspace: Option<&str>) -> bool {
    workspace.is_none_or(|ws| s.workspaces.is_empty() || s.workspaces.iter().any(|w| w == ws))
}

/// Why the server can't serve this Hive before anything is read: off, scoped away, or its entry
/// not where agents look.
pub fn gate(p: &Provider, s: &ProxyInput, workspace: Option<&str>, reach: &Reach) -> Option<VerifyResult> {
    let entry = entry_name(&s.id);
    if !s.enabled {
        return Some(VerifyResult::new(UNREACHABLE, format!("{} is switched off in maiTerm. Turn \"{entry}\" on in Preferences → MCP servers.", p.name)));
    }
    if !in_scope(s, workspace) {
        return Some(VerifyResult::new(
            UNREACHABLE,
            format!("{} is limited to other workspaces, so this Hive's agents don't see its tools. Add this workspace to \"{entry}\" in Preferences → MCP servers.", p.name),
        ));
    }
    entry_problem(p, &s.id, reach).map(|m| VerifyResult::new(UNREACHABLE, m))
}

/// The proxied server Verify checks for `p`: the first one recognised as `p` by its upstream (never
/// its name) that is on and in scope, else the first that is on, else the first.
pub fn pick_server<'a>(servers: &'a [ProxyInput], p: &Provider, workspace: Option<&str>) -> Option<&'a ProxyInput> {
    let mine: Vec<&ProxyInput> = servers.iter().filter(|s| providers_for_upstream(Some(&s.url)).iter().any(|q| q.id == p.id)).collect();
    mine.iter().find(|s| s.enabled && in_scope(s, workspace)).or_else(|| mine.iter().find(|s| s.enabled)).or_else(|| mine.first()).copied()
}

// ─── Into words ─────────────────────────────────────────────────────────────

/// `text` holds `code` as a whole number, not inside a longer one.
fn has_code(text: &str, code: &str) -> bool {
    text.match_indices(code).any(|(i, _)| {
        let before = text[..i].chars().next_back().is_none_or(|c| !c.is_ascii_digit());
        let after = text[i + code.len()..].chars().next().is_none_or(|c| !c.is_ascii_digit());
        before && after
    })
}

fn says_no_credit(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    has_code(&t, "402")
        || ["payment required", "insufficient credit", "insufficient balance", "insufficient funds", "out of credit", "not enough credit", "no credits", "exhausted balance", "balance is too low"]
            .iter()
            .any(|w| t.contains(w))
}

fn says_signed_out(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    has_code(&t, "401") || ["unauthorized", "unauthenticated", "invalid api key", "invalid token", "not signed in", "not logged in"].iter().any(|w| t.contains(w))
}

/// At most 160 characters of a server's own words.
fn excerpt(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut out: String = s.chars().take(160).collect();
    if s.chars().count() > 160 {
        out.push('…');
    }
    out
}

/// The catalogue's billing page, as shown (no scheme) and as a link: only http(s).
fn billing(p: &Provider) -> Option<(String, String)> {
    let url = p.billing_url.trim();
    let shown = url.strip_prefix("https://").or_else(|| url.strip_prefix("http://"))?;
    (!shown.is_empty()).then(|| (shown.trim_end_matches('/').to_string(), url.to_string()))
}

fn needs_credit(p: &Provider, saw_402: bool) -> VerifyResult {
    let head = if saw_402 { format!("Signed in, but {} answered 402", p.name) } else { format!("Signed in, but {} says the account is out of credit", p.name) };
    match billing(p) {
        Some((shown, url)) => VerifyResult { link: Some(url), ..VerifyResult::new(NEEDS_CREDIT, format!("{head}: add credit at {shown}")) },
        None => VerifyResult::new(NEEDS_CREDIT, format!("{head}: add credit in your {} account.", p.name)),
    }
}

/// A read or call that failed, `during` saying which ("listing its tools", "its balance check").
pub fn failed(p: &Provider, f: &Failure, during: &str) -> VerifyResult {
    let name = &p.name;
    if f.http == Some(402) || says_no_credit(&f.detail) {
        return needs_credit(p, f.http == Some(402) || has_code(&f.detail, "402"));
    }
    match f.state.as_str() {
        NEEDS_SIGN_IN => {
            let code = f.http.map(|c| format!(" ({c})")).unwrap_or_default();
            VerifyResult::new(NEEDS_SIGN_IN, format!("{name} refused maiTerm's sign-in{code} while {during}: sign in to {name} again."))
        }
        UNREACHABLE => VerifyResult::new(UNREACHABLE, format!("maiTerm couldn't reach {name} while {during} ({}). Check the connection and verify again.", excerpt(&f.detail))),
        _ => VerifyResult::new(ERROR, format!("{name} answered {during} with something maiTerm couldn't use ({}).", excerpt(&f.detail))),
    }
}

/// The text of a tool result's content.
fn result_text(v: &Value) -> String {
    v.get("content").and_then(Value::as_array).map(|c| c.iter().filter_map(|b| b.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join(" ")).unwrap_or_default()
}

/// What a check call's answer means for a server that listed `count` tools.
pub fn after_check(p: &Provider, count: usize, check: &CheckTool, answer: Result<Value, Failure>) -> VerifyResult {
    let (name, what) = (&p.name, check.what);
    match answer {
        Ok(v) if v.get("isError").and_then(Value::as_bool) != Some(true) => VerifyResult::new(READY, format!("{name} is ready: {count} tools, and its {what} answered.")),
        Ok(v) => {
            let text = result_text(&v);
            if says_no_credit(&text) {
                needs_credit(p, has_code(&text, "402"))
            } else if says_signed_out(&text) {
                VerifyResult::new(NEEDS_SIGN_IN, format!("{name} refused its {what}: sign in to {name} again."))
            } else {
                VerifyResult::new(
                    ERROR,
                    format!("{name}'s {what} answered with an error: {}. Its {count} tools are listed, but maiTerm can't confirm the account until that check passes.", excerpt(&text)),
                )
            }
        }
        // The server didn't take maiTerm's arguments (or no longer has the tool): that is
        // maiTerm's guess failing, not the account. The list already answered.
        Err(f) if !says_no_credit(&f.detail) && matches!(f.rpc, Some(-32602 | -32601)) => {
            VerifyResult::new(READY, format!("{name} is ready: {count} tools. Its {what} didn't take maiTerm's arguments, so the account itself wasn't read."))
        }
        Err(f) => failed(p, &f, &format!("running its {what}")),
    }
}

/// Verify a proxied server for `p`.
pub async fn verify_mcp<P: Probe>(probe: &P, p: &Provider, s: &ProxyInput, workspace: Option<&str>, reach: &Reach) -> VerifyResult {
    if let Some(stop) = gate(p, s, workspace, reach) {
        return stop;
    }
    let tools = match probe.list(&s.id).await {
        Ok(t) => t,
        Err(f) => return failed(p, &f, "listing its tools"),
    };
    let count = tools.len();
    let mut out = if count == 0 {
        VerifyResult::new(ERROR, format!("{} answered, but lists no tools. Sign in again, or check \"{}\" in Preferences → MCP servers.", p.name, entry_name(&s.id)))
    } else {
        match plan_check(p, &tools) {
            None => VerifyResult::new(READY, format!("{} is ready: {count} tools.", p.name)),
            Some((check, args)) => match run_check(probe, p, &s.id, check.name, args).await {
                Ok(answer) => after_check(p, count, check, answer),
                // Unreachable through `plan_check`, which only plans allowlisted tools.
                Err(why) => VerifyResult::new(ERROR, why),
            },
        }
    };
    out.tool_count = Some(count);
    out
}

/// The label a hit's `tool_id` shows: `cli:ffmpeg` is `ffmpeg`.
fn shown(tool_id: &str) -> &str {
    tool_id.split_once(':').map_or(tool_id, |(_, rest)| rest)
}

/// Verify a provider maiTerm doesn't proxy, from what detection found (`status::hits`).
pub fn verify_local(p: &Provider, hits: &[Hit]) -> VerifyResult {
    let name = &p.name;
    let mine: Vec<&Hit> = hits.iter().filter(|h| h.cover.provider == p.id).collect();
    if !p.detect.clis.is_empty() {
        let bins: Vec<&str> = p.detect.clis.iter().map(|c| c.bin.as_str()).collect();
        let (found, missing): (Vec<&str>, Vec<&str>) = bins.iter().partition(|b| mine.iter().any(|h| h.cover.tool_id == format!("cli:{b}")));
        let words = |v: &[&str]| join_words(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        if missing.is_empty() {
            return VerifyResult::new(READY, format!("{name} is ready: {} found.", words(&found)));
        }
        if !found.is_empty() {
            return VerifyResult::new(UNREACHABLE, format!("Found {}, but not {}. Run {name}'s install again.", words(&found), words(&missing)));
        }
    }
    if let Some(h) = mine.iter().find(|h| h.cover.state == READY) {
        return VerifyResult::new(READY, format!("{name} is ready ({}).", shown(&h.cover.tool_id)));
    }
    if let Some(h) = mine.iter().find(|h| h.cover.state == INSTALLED) {
        let id = &h.cover.tool_id;
        let message = if id.starts_with("plugin:") {
            format!("{name} is installed but switched off. Turn it on with /plugin in Claude Code, then verify again.")
        } else if id.starts_with("app:") {
            format!("{name} is installed but not running. Open it, then verify again.")
        } else if id.starts_with("mcp:") {
            format!("{name} is in your own MCP settings as \"{}\". maiTerm doesn't hold its sign-in, so it can't check it.", shown(id))
        } else {
            format!("{name} is here ({}), but maiTerm can't confirm it works.", shown(id))
        };
        return VerifyResult::new(INSTALLED, message);
    }
    let next = match p.routes.first().map(|r| r.kind.as_str()) {
        Some("brew") => "Run its install again.",
        Some("plugin") => "Install it again.",
        Some("local_app") => "Install it, then verify again.",
        _ => "Set it up first.",
    };
    VerifyResult::new(UNREACHABLE, format!("maiTerm doesn't find {name} on this Mac yet. {next}"))
}

/// The setup sheet's Verify button (docs/hive-media.md §6): re-detect `provider_id` and, for a
/// server maiTerm proxies, prove it reaches this Hive's agents and answers. `workspace_id` is the
/// Hive's workspace (its proxy scope is judged), or none from Preferences. Never runs a paid job.
#[tauri::command]
pub async fn hive_media_verify(
    window: tauri::Window,
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    workspace_id: Option<String>,
    provider_id: String,
) -> Result<VerifyResult, String> {
    let p = provider(&provider_id).ok_or_else(|| format!("No media provider named {provider_id}"))?;
    let prefs = {
        let data = state.app_data.read();
        if let Some(id) = &workspace_id {
            data.window(window.label()).and_then(|w| w.workspaces.iter().find(|w| &w.id == id)).ok_or("Workspace not found")?;
        }
        data.preferences.clone()
    };
    let ws = workspace_id.clone();
    let (hits, servers, reach) = tauri::async_runtime::spawn_blocking(move || (status::hits(&prefs, ws.as_deref()), status::proxy_inputs(), reach(&prefs)))
        .await
        .map_err(|e| e.to_string())?;
    let out = match pick_server(&servers, p, workspace_id.as_deref()) {
        Some(s) => verify_mcp(&ProxyProbe, p, s, workspace_id.as_deref(), &reach).await,
        None => verify_local(p, &hits),
    };
    log::info!("[hive-media] verify {}: {}", p.id, out.state);
    Ok(out)
}

#[cfg(test)]
mod tests;
