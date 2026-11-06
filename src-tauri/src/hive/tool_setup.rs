//! The coordinator's ask for a media capability its Hive lacks (docs/hive-media.md §8):
//! `hiveRequestCapability {capability, why}` files an agent request of kind `tool_setup`.
//!
//! - **The card is maiTerm's words.** The provider is `hive_media_status`'s recommendation for
//!   the Hive's workspace, and the card says what setting it up does, what it spends and which
//!   hosts it brings, from the catalogue alone (no secret: cards reach the phone). The agent's
//!   `why` is only the quoted `reason`, escaped like every agent string.
//! - **Refused at filing**: an unknown capability, one already Ready for the Hive (Ready on this
//!   Mac but not among the Hive's tools is the human's tools change, not a setup), a capability
//!   maiTerm can't recommend a provider for, and a second open ask for the same capability (a
//!   card waiting, or an approved setup not yet Ready).
//! - **Approval** (`on_approved`, Rust: Hive kinds never reach a window's executor) emits
//!   `hive-tool-setup-approved` to the Hive's window, which opens the setup sheet, and settles the
//!   card at once with what happens next. From the phone, only §7's phone-startable setups run
//!   without the Mac (`install::phone_startable`: a Homebrew install with Homebrew present, a
//!   plugin with a confirmed source), through the setup sheet's own install path; anything else
//!   waits for the Mac, said in the outcome. maiTerm never adds an MCP server here: a remote MCP
//!   is added by the sheet, through the human's add path and `mcp_add`'s checks.
//! - **When Ready**: an approved ask is watched (in memory, like the requests) until the
//!   capability is Ready for the Hive. No polling: the signals that can make it Ready re-check it
//!   (`recheck_soon`: an install's exit, a proxy server connecting, a Verify that says ready, a
//!   status read that already shows it). Then its tools are added to `Hive.tools` through the
//!   tools change's own write (`tool_change::write_tools`), and the coordinator gets a framed
//!   notice. **Hosts**: the hosts the card named (the provider's `sandbox_hosts`, shown before
//!   the human approved) are the human's tick; any other host the added tools would bring is left
//!   for the human to add in Hive settings, and the window's notice names it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, OnceLock};

use serde_json::{json, Value};

use super::create::{generator_hosts, url_host, DetectedTool};
use super::deliver::one_line;
use super::gallery::budget::{normalize_host, reaches_remote};
use super::media::install::{phone_startable, plan_for, Here, InstallStarted};
use super::media::status::{MediaRow, READY};
use super::media::{self, Provider};
use super::record::{Hive, STATUS_DONE};
use super::rpc::{HiveRequest, HiveWindow};
use super::tools::{Caller, Ctx};

/// The request kind (`agent_requests::KINDS`).
pub const KIND: &str = "tool_setup";
pub const TOOL: &str = "hiveRequestCapability";
/// To the Hive's window when its human approves an ask: the window opens the setup sheet.
/// `{request_id, workspace_id, hive_id, capability, provider_id, by, install, waiting_for_mac}`.
pub const APPROVED_EVENT: &str = "hive-tool-setup-approved";
const MAX_WHY: usize = 500;
/// An approved ask nobody finished setting up stops being watched after a day, and may be asked again.
const WATCH_TTL_MS: u64 = 24 * 60 * 60 * 1000;
/// How the coordinator's notices are framed.
const FRAME: &str = "HIVE TOOLS";

fn capability_ids() -> Vec<&'static str> {
    media::catalogue().capabilities.iter().map(|c| c.id.as_str()).collect()
}

pub fn tool_definition(tab_id: &Value) -> Value {
    let ids = capability_ids();
    json!({
        "name": TOOL,
        "description": format!(
            "Ask your human for a media capability this Hive lacks (a worker reported a tool missing, or the plan needs one). \
             maiTerm files a card in its own words naming the provider it recommends for this Mac and what that spends; your why \
             is quoted on it. Approving opens the setup sheet on the Mac. Approved from the phone, a Homebrew or plugin install \
             starts at once only when maiTerm can install that provider there; anything else (a sign-in, a key, an install \
             maiTerm can\u{2019}t do) waits for your human at the Mac, and the card says which. Once the capability works, maiTerm adds it to this \
             Hive\u{2019}s tools and tells you: workers launched from then on have it, running ones don\u{2019}t. Refused for a \
             capability the Hive already has Ready, and while an ask for it is open. For a media provider use this, never \
             addMcpServer. Capabilities: {}.",
            ids.join(", ")
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "capability": { "type": "string", "enum": ids, "description": "What the Hive needs to make" },
                "why": { "type": "string", "description": "One or two sentences: what the work needs it for. Your human reads it on the card" }
            },
            "required": ["capability", "why"]
        }
    })
}

/// The tool's arguments: a catalogue capability id, and the coordinator's reason (escaped, one line).
pub fn parse_args(args: &Value) -> Result<(String, String), String> {
    let raw = args.get("capability").and_then(Value::as_str).map(str::trim).unwrap_or_default();
    let cap = raw.to_ascii_lowercase();
    if media::capability(&cap).is_none() {
        let shown = if raw.is_empty() { "nothing".to_string() } else { format!("\u{201c}{}\u{201d}", one_line(raw, 40)) };
        return Err(format!("capability is one of {}, not {shown}.", capability_ids().join(", ")));
    }
    let why = args.get("why").and_then(Value::as_str).map(str::trim).filter(|w| !w.is_empty());
    let Some(why) = why else {
        return Err("Say why the Hive needs it: your human reads it on the card.".to_string());
    };
    Ok((cap, one_line(why, MAX_WHY)))
}

/// A tool as a sentence names it: its id without the kind prefix, one line.
fn shown_tool(id: &str) -> String {
    one_line(id.split_once(':').map(|(_, r)| r).filter(|r| !r.is_empty()).unwrap_or(id), 80)
}

/// Why an ask for `cap` isn't filed because something ready already covers it, or None.
pub fn ready_refusal(cap: &str, row: &MediaRow, hive: &Hive) -> Option<String> {
    let ready: Vec<&str> = row.covered_by.iter().filter(|c| c.state == READY).map(|c| c.tool_id.as_str()).collect();
    if ready.is_empty() {
        return None;
    }
    if let Some(t) = ready.iter().find(|t| hive.tools.iter().any(|h| h == *t)) {
        return Some(format!("{cap} is already Ready for this Hive: its workers have it through {}. Nothing to set up.", shown_tool(t)));
    }
    let names: Vec<String> = ready.iter().map(|t| shown_tool(t)).collect();
    Some(format!(
        "{cap} is already Ready on this Mac ({}), but it isn\u{2019}t one of this Hive\u{2019}s tools: your human adds it in Hive \
         settings \u{2192} Tools (ask with a blocker). Nothing to set up.",
        names.join(", ")
    ))
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or_default()
}

/// Why an ask is a duplicate of one still open (a card waiting, an approved setup not Ready yet).
pub fn duplicate_refusal(open: &[(String, Value)], hive_id: &str, cap: &str) -> Option<String> {
    let (_, args) = open.iter().find(|(_, a)| arg(a, "hive_id") == hive_id && arg(a, "capability") == cap)?;
    let name = media::provider(arg(args, "provider_id")).map_or("a provider", |p| p.name.as_str());
    Some(format!("You already asked for {cap} ({name}): its card waits for your human. maiTerm tells you the answer."))
}

/// The hosts a provider's results come from, which a media worker's sandbox must reach: its
/// `sandbox_hosts` when its MCP route's host is one it owns (what `generator_hosts` gives a
/// proxied entry of it), less those the gallery has and any repo remote's.
pub fn card_hosts(p: &Provider, hive: &Hive, remote_hosts: &[String]) -> Vec<String> {
    let Some(route) = p.mcp_route() else { return Vec::new() };
    if !url_host(&route.url).is_some_and(|h| p.owns_host(&h)) {
        return Vec::new();
    }
    let have: Vec<String> = hive.settings.gallery.generator_hosts.iter().filter_map(|h| normalize_host(h).ok()).collect();
    let mut out: Vec<String> = Vec::new();
    for h in p.sandbox_hosts.iter().filter_map(|h| normalize_host(h).ok()) {
        if !have.contains(&h) && reaches_remote(&h, remote_hosts).is_none() && !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// What a job spends, in maiTerm's words (the panel's `creditsLine`, src/lib/hive/media.ts).
pub fn credits_line(p: &Provider) -> String {
    let route = p.mcp_route().or_else(|| p.routes.first());
    let quote = if route.and_then(|r| r.quote_tool.as_ref()).is_some() { " \u{b7} prices a job before running it" } else { "" };
    match route.map(|r| r.credits.as_str()).unwrap_or_default() {
        "api" => format!("prepaid API credit{quote}"),
        "plan" => format!("spends your {} plan\u{2019}s credits{quote}", p.name),
        "plan_or_wallet" => format!("spends your {} plan\u{2019}s credits or wallet balance{quote}", p.name),
        "comfy_credits" => format!("spends your {} credits{quote}", p.name),
        "free" | "none" => "free".to_string(),
        _ => "its cost isn\u{2019}t known".to_string(),
    }
}

/// Why maiTerm can't install `p` itself when its setup is a Homebrew or plugin install that
/// `install::plan_for` refuses here (a plugin with no confirmed marketplace, no Homebrew to run):
/// the human installs it by hand from the provider's guide.
fn by_hand(p: &Provider, here: &Here) -> Option<String> {
    let route = p.mcp_route().or_else(|| p.routes.first())?;
    if route.kind != "brew" && route.kind != "plugin" {
        return None;
    }
    plan_for(p, here).err()
}

/// What the setup sheet does for `p`, said before the human approves.
fn what_setup_does(p: &Provider, here: &Here) -> String {
    let route = p.mcp_route().or_else(|| p.routes.first());
    let name = &p.name;
    if let Some(e) = by_hand(p, here) {
        return format!("Approving opens {name}\u{2019}s setup sheet in this Hive\u{2019}s window on the Mac, where you install it by hand: {e}");
    }
    let how = match route.map(|r| (r.kind.as_str(), r.auth.as_str())) {
        Some(("remote_mcp", "token")) => format!(
            "you paste a key from {name} there (never on the phone); maiTerm keeps it in the keychain and serves {name} through its \
             MCP proxy to this Hive\u{2019}s workspace only"
        ),
        Some(("remote_mcp", _)) => {
            format!("you sign in with {name} in your browser, and maiTerm serves it through its MCP proxy to this Hive\u{2019}s workspace only")
        }
        Some(("stdio_mcp", _)) => {
            let shown = route.and_then(|r| r.stdio_spec()).map(|s| std::iter::once(s.command).chain(s.args).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            format!(
                "you paste a key from {name} there (never on the phone); maiTerm keeps it in the keychain and runs {name}\u{2019}s local \
                 server (`{}`) itself, outside every sandbox, serving it through its MCP proxy to this Hive\u{2019}s workspace only. Its \
                 first start downloads the package, which can take a minute or more",
                one_line(&shown, 200)
            )
        }
        Some(("brew", _)) => {
            let line = route.and_then(|r| r.command.as_deref()).unwrap_or("brew install");
            let first = if here.brew.is_none() { "Homebrew\u{2019}s own installer runs first and asks for your password, then " } else { "" };
            format!("{first}maiTerm runs `{}` in a terminal tab you can watch", one_line(line, 200))
        }
        Some(("plugin", _)) => format!(
            "maiTerm installs the Claude Code plugin {} in a terminal tab you can watch, for this Hive\u{2019}s coordinator (workers load no \
             plugins)",
            route.and_then(|r| r.plugin_id.as_deref()).map_or_else(|| name.clone(), |id| one_line(id, 80))
        ),
        Some(("local_app", _)) => format!("you install {name}\u{2019}s app; maiTerm checks again when its window comes back to the front"),
        _ => format!("you follow {name}\u{2019}s guide there"),
    };
    format!("Approving opens {name}\u{2019}s setup sheet in this Hive\u{2019}s window on the Mac: {how}.")
}

/// The card, in maiTerm's words.
#[derive(Debug, Clone, PartialEq)]
pub struct Card {
    pub provider_id: String,
    pub summary: String,
    pub effects: Vec<String>,
    /// The hosts the card names: approving it is the human's tick for them.
    pub hosts: Vec<String>,
}

/// The card for an ask of `cap` in the Hive named `hive_name`, from its status `row`. Refused when
/// nothing can be recommended (only a guide, or no provider at all).
pub fn card(hive_name: &str, hive: &Hive, cap: &str, row: &MediaRow, here: &Here, remote_hosts: &[String]) -> Result<Card, String> {
    let means = media::capability(cap).map(|c| c.means.to_ascii_lowercase()).unwrap_or_default();
    let p = row.recommended.as_deref().and_then(media::provider).ok_or_else(|| {
        format!(
            "maiTerm has no provider it can set up for {cap} on this Mac: only a guide your human follows by hand. Tell your human (a \
             blocker) what the work needs it for."
        )
    })?;
    let summary = format!(
        "The Hive \u{201c}{}\u{201d} needs {cap} ({means}). Recommended: {} ({}). Set up\u{2026}",
        one_line(hive_name, 80),
        p.name,
        credits_line(p)
    );
    let mut effects = vec![what_setup_does(p, here)];
    effects.push(if phone_startable(p, here) {
        "Approved from your phone, the install starts on the Mac at once.".to_string()
    } else {
        "Approved from your phone, it waits for you at the Mac.".to_string()
    });
    let hosts = card_hosts(p, hive, remote_hosts);
    let plugin = p.routes.iter().all(|r| r.kind == "plugin");
    effects.push(if plugin {
        format!("Once it works, this Hive\u{2019}s coordinator has {cap} from its next launch.")
    } else if hosts.is_empty() && p.stdio_routes().next().is_some() {
        format!(
            "Once it works, maiTerm adds it to this Hive\u{2019}s tools, and the folder its server saves files in to this Hive\u{2019}s \
             output folders, so galleryAdd takes them: workers launched after that have {cap}. Approving allows that folder."
        )
    } else if hosts.is_empty() {
        format!("Once it works, maiTerm adds it to this Hive\u{2019}s tools: workers launched after that have {cap}.")
    } else {
        format!(
            "Once it works, maiTerm adds it to this Hive\u{2019}s tools, and media workers launched after that may also reach {} \
             (where {}\u{2019}s results download from). Approving allows those hosts.",
            hosts.join(", "),
            p.name
        )
    });
    let others: Vec<&str> = row.alternatives.iter().filter_map(|a| media::provider(a)).map(|a| a.name.as_str()).collect();
    if !others.is_empty() {
        effects.push(format!("Other ways to get {cap}: {}. Set one up in Hive settings \u{2192} Tools instead, then deny this.", others.join(", ")));
    }
    Ok(Card { provider_id: p.id.clone(), summary, effects, hosts })
}

/// `hiveRequestCapability`, with what it reads of this Mac given: the media status of the Hive's
/// workspace, Homebrew's presence and the Hive's repo remotes.
pub(crate) async fn request_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value, rows: &[MediaRow], here: &Here, remote_hosts: &[String]) -> Result<Value, String> {
    let (cap, why) = parse_args(args)?;
    let hive = super::autonomy::current_hive(ctx, caller).ok_or("This Hive is gone.")?;
    if hive.status == STATUS_DONE {
        return Err("This Hive is done: a message from your human wakes it first.".to_string());
    }
    let row = rows.iter().find(|r| r.capability == cap).ok_or_else(|| format!("maiTerm couldn\u{2019}t read this Mac\u{2019}s setup for {cap}."))?;
    if let Some(e) = ready_refusal(&cap, row, &hive) {
        return Err(e);
    }
    if let Some(e) = duplicate_refusal(&ctx.window.open_requests(KIND), &hive.id, &cap) {
        return Err(e);
    }
    if let Some(w) = watching(&hive.id, &cap) {
        let name = media::provider(&w.provider_id).map_or(w.provider_id.as_str(), |p| p.name.as_str());
        return Err(format!(
            "Your human already approved setting up {cap} ({name}), and it isn\u{2019}t working yet. maiTerm tells you when it is: \
             carry on meanwhile."
        ));
    }
    let card = card(&caller.workstream, &hive, &cap, row, here, remote_hosts)?;
    let req = HiveRequest {
        tab_id: caller.tab_id.clone(),
        kind: KIND,
        args: json!({
            "hive_id": hive.id,
            "workspace_id": caller.workspace_id,
            "capability": cap,
            "provider_id": card.provider_id,
            "hosts": card.hosts,
        }),
        summary: card.summary,
        effects: card.effects,
        reason: Some(why),
        keep: None,
    };
    let id = ctx.window.file_request(req)?;
    log::info!("hive {}: asks for {cap} through {} ({id})", hive.id, card.provider_id);
    Ok(json!({
        "status": "awaiting_approval",
        "request_id": id,
        "capability": cap,
        "provider": card.provider_id,
        "note": "Your human approves or denies it on a card; maiTerm tells you which, and again once it works. Carry on meanwhile.",
    }))
}

/// `hiveRequestCapability` (tools.rs): reads this Mac, then `request_in`.
pub(crate) async fn request_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value) -> Result<Value, String> {
    // Arguments first: a bad call costs no detection.
    parse_args(args)?;
    let prefs = ctx.data.read().preferences.clone();
    let (ws, hive) = (caller.workspace_id.clone(), caller.hive.clone());
    let (rows, here, remotes) = super::tools::blocking(move || {
        Ok((media::status::status(&prefs, Some(&ws), &[]), Here::detect(&prefs), super::settings::hive_remote_hosts(&hive)))
    })
    .await?;
    request_in(ctx, caller, args, &rows, &here, &remotes).await
}

/// A redirect for `addMcpServer` from a Hive agent (§8 "One route, not two"): a URL a catalogue
/// media provider serves is asked for with `hiveRequestCapability`, never `mcp_add`.
pub fn refuse_mcp_add(url: &str) -> Option<String> {
    let p = media::providers_for_upstream(Some(url)).into_iter().next()?;
    Some(format!(
        "{} is a media provider: ask for what it makes with hiveRequestCapability {{capability, why}} ({}), not addMcpServer. \
         Your human then gets its setup sheet, and maiTerm adds it to this Hive once it works.",
        p.name,
        p.capabilities.join(", ")
    ))
}

// ─── Watching an approved ask until it is Ready ─────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Watch {
    pub request_id: String,
    pub hive_id: String,
    pub workspace_id: String,
    pub capability: String,
    pub provider_id: String,
    /// The hosts the card named.
    pub hosts: Vec<String>,
    pub why: String,
    pub since_ms: u64,
}

static WATCHES: LazyLock<parking_lot::Mutex<Vec<Watch>>> = LazyLock::new(Default::default);
static HANDLE: OnceLock<tauri::AppHandle> = OnceLock::new();
static RECHECKING: AtomicBool = AtomicBool::new(false);
static AGAIN: AtomicBool = AtomicBool::new(false);

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn live_watches() -> Vec<Watch> {
    let now = now_ms();
    let mut w = WATCHES.lock();
    w.retain(|x| now.saturating_sub(x.since_ms) < WATCH_TTL_MS);
    w.clone()
}

fn watching(hive_id: &str, cap: &str) -> Option<Watch> {
    live_watches().into_iter().find(|w| w.hive_id == hive_id && w.capability == cap)
}

fn add_watch(w: Watch) {
    let mut all = WATCHES.lock();
    all.retain(|x| !(x.hive_id == w.hive_id && x.capability == w.capability));
    all.push(w);
}

fn take_watch(request_id: &str) -> Option<Watch> {
    let mut all = WATCHES.lock();
    let i = all.iter().position(|w| w.request_id == request_id)?;
    Some(all.remove(i))
}

/// Something that can make a capability Ready happened (an install exited, a proxy server
/// connected, Verify said ready): every approved ask still waiting is looked at again, once,
/// coalesced. Nothing when none waits.
pub fn recheck_soon() {
    if WATCHES.lock().is_empty() {
        return;
    }
    let Some(app) = HANDLE.get().cloned() else { return };
    if RECHECKING.swap(true, Ordering::SeqCst) {
        AGAIN.store(true, Ordering::SeqCst);
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            AGAIN.store(false, Ordering::SeqCst);
            recheck_all(&app).await;
            if !AGAIN.load(Ordering::SeqCst) {
                break;
            }
        }
        RECHECKING.store(false, Ordering::SeqCst);
        if AGAIN.swap(false, Ordering::SeqCst) {
            recheck_soon();
        }
    });
}

/// A status read (`hive_media_status`) for `workspace_id` already shows a watched capability
/// Ready: finish it without waiting for another signal.
pub fn on_status(workspace_id: Option<&str>, rows: &[MediaRow]) {
    let Some(ws) = workspace_id else { return };
    let due = live_watches()
        .iter()
        .any(|w| w.workspace_id == ws && rows.iter().any(|r| r.capability == w.capability && r.covered_by.iter().any(|c| c.state == READY)));
    if due {
        recheck_soon();
    }
}

async fn recheck_all(app: &tauri::AppHandle) {
    use tauri::Manager;
    let Some(state) = app.try_state::<std::sync::Arc<crate::state::AppState>>().map(|s| s.inner().clone()) else { return };
    for w in live_watches() {
        let Some(caller) = super::tools::hive_callers(&state.app_data.read()).into_iter().find(|c| c.hive.id == w.hive_id) else {
            take_watch(&w.request_id);
            continue;
        };
        let prefs = state.app_data.read().preferences.clone();
        let (ws, hive) = (caller.workspace_id.clone(), caller.hive.clone());
        let read = super::tools::blocking(move || {
            Ok((media::status::status(&prefs, Some(&ws), &[]), super::create::detect_tools(&prefs), super::settings::hive_remote_hosts(&hive)))
        })
        .await;
        let Ok((rows, detected, remotes)) = read else { continue };
        let Some(row) = rows.iter().find(|r| r.capability == w.capability) else { continue };
        let ctx = super::tools::app_ctx(&state, app, &caller);
        ready_in(&ctx, &caller, &w, row, &detected, &remotes).await;
    }
}

/// What `ready_in` did, for its caller and tests.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Readiness {
    /// Nothing ready covers it yet: still watched.
    NotYet,
    /// Ready: these tools and hosts were added (both may be empty), and the coordinator was told.
    Added { tools: Vec<String>, hosts: Vec<String>, through: String },
    /// Ready, but the Hive couldn't take it (gone, done, the write refused): told, and no longer watched.
    Failed(String),
}

/// The tools to add now that `row` is Ready: the ready covers of the provider the card named, else
/// any ready cover, that this Mac's detection has as a tool (a plugin is none) and the Hive lacks.
/// Also the provider they came through.
pub fn tools_to_add(row: &MediaRow, provider_id: &str, detected: &[DetectedTool], hive: &Hive) -> (Vec<String>, Option<String>) {
    let ready: Vec<_> = row.covered_by.iter().filter(|c| c.state == READY).collect();
    let pick: Vec<_> = if ready.iter().any(|c| c.provider == provider_id) { ready.into_iter().filter(|c| c.provider == provider_id).collect() } else { ready };
    let through = pick.first().map(|c| c.provider.clone());
    let mut out: Vec<String> = Vec::new();
    for c in pick {
        if detected.iter().any(|t| t.id == c.tool_id) && !hive.tools.contains(&c.tool_id) && !out.contains(&c.tool_id) {
            out.push(c.tool_id.clone());
        }
    }
    (out, through)
}

fn frame(cap: &str) -> String {
    format!("\u{27e6}{FRAME} \u{b7} {cap}\u{27e7}\n")
}

/// Whether a provider is set up only as a plugin, which a Hive's coordinator loads and no worker does.
fn plugin_only(provider_id: &str) -> bool {
    media::provider(provider_id).is_some_and(|p| !p.routes.is_empty() && p.routes.iter().all(|r| r.kind == "plugin"))
}

/// `w`'s capability, as `row` reads it now: if Ready, its tools join the Hive (with the hosts the
/// card named), the human and the coordinator are told, and the watch ends.
pub(crate) async fn ready_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &Watch, row: &MediaRow, detected: &[DetectedTool], remote_hosts: &[String]) -> Readiness {
    if !row.covered_by.iter().any(|c| c.state == READY) {
        return Readiness::NotYet;
    }
    if take_watch(&w.request_id).is_none() {
        // Another pass finished it meanwhile.
        return Readiness::NotYet;
    }
    let cap = w.capability.as_str();
    let Some(hive) = super::autonomy::current_hive(ctx, caller) else { return Readiness::Failed("This Hive is gone.".to_string()) };
    let (add, through) = tools_to_add(row, &w.provider_id, detected, &hive);
    let through = through.unwrap_or_else(|| w.provider_id.clone());
    let name = media::provider(&through).map_or(through.clone(), |p| p.name.clone());
    let mut hosts_added = Vec::new();
    let mut folders_added: Vec<String> = Vec::new();
    let mut hosts_left = Vec::new();
    if !add.is_empty() {
        let mut tools = hive.tools.clone();
        tools.extend(add.iter().cloned());
        let have: Vec<String> = hive.settings.gallery.generator_hosts.iter().filter_map(|h| normalize_host(h).ok()).collect();
        let offered: Vec<String> = generator_hosts(detected, &add, remote_hosts).into_iter().filter(|h| !have.contains(h)).collect();
        let (ticked, left): (Vec<String>, Vec<String>) = offered.into_iter().partition(|h| w.hosts.contains(h));
        // A local server's output folder: the card said it joins the Hive's output folders, so
        // approving it is the human's tick (docs/hive-media.md §12.1).
        let folders = super::create::tool_output_folders(detected, &add);
        match super::tool_change::write_tools(ctx, caller, detected, remote_hosts, &tools, &ticked, &folders).await {
            Ok((_, c)) => {
                super::change::log_line(caller, &format!("human (approved the queen\u{2019}s ask for {cap})"), &super::tool_change::change_lines(&c), Some(&w.why));
                hosts_added = c.hosts_added;
                folders_added = c.folders_added;
                hosts_left = left;
            }
            Err(e) => {
                ctx.window.settings_changed(&caller.workspace_id, &format!("{name} is set up, but {cap} couldn\u{2019}t join this Hive: {e}"));
                tell_coordinator(
                    ctx,
                    caller,
                    format!("{}{name} is set up on this Mac, but maiTerm couldn\u{2019}t add {cap} to this Hive: {e}", frame(cap)),
                )
                .await;
                return Readiness::Failed(e);
            }
        }
    }
    let mut text = frame(cap);
    if plugin_only(&through) && add.is_empty() {
        text.push_str(&format!(
            "{cap} is now available through {through}, a plugin your own session loads from your next launch. Workers load no \
             plugins: put what they need from it in their briefs."
        ));
    } else {
        text.push_str(&format!("{cap} is now available through {through}; your workers launched from now on have it."));
    }
    if !add.is_empty() {
        let ids: Vec<String> = add.iter().map(|t| one_line(t, 80)).collect();
        text.push_str(&format!("\n- Added to this Hive\u{2019}s tools: {}", ids.join(", ")));
    }
    if !hosts_added.is_empty() {
        text.push_str(&format!("\n- Media workers may also reach: {}", hosts_added.join(", ")));
    }
    if !folders_added.is_empty() {
        text.push_str(&format!("\n- galleryAdd also takes files from: {}", folders_added.iter().map(|f| one_line(f, 200)).collect::<Vec<_>>().join(", ")));
    }
    let p = caller.paths.clone();
    let workers = super::tools::blocking(move || Ok(super::worker::list_workers(&p))).await.unwrap_or_default();
    if let Some(r) = super::change::running_on_old(&workers) {
        text.push('\n');
        text.push_str(&r);
    }
    tell_coordinator(ctx, caller, text).await;
    let mut human = if add.is_empty() {
        format!("{name} is set up: {cap} is available to this Hive, as you approved.")
    } else {
        format!("{name} is set up: {cap} is now one of this Hive\u{2019}s tools, as you approved.")
    };
    if !hosts_left.is_empty() {
        human.push_str(&format!(" It could also use {}, which the card didn\u{2019}t name: add them in Hive settings \u{2192} Budget and generation if workers need them.", hosts_left.join(", ")));
    }
    ctx.window.settings_changed(&caller.workspace_id, &human);
    log::info!("hive {}: {cap} ready through {through}; added {:?}", caller.hive.id, add);
    Readiness::Added { tools: add, hosts: hosts_added, through }
}

async fn tell_coordinator<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, text: String) {
    let Some(c) = super::tools::coordinator_tab(ctx.data, caller) else { return };
    let sent = ctx.window.deliver(&c, text, "Hive tools").await;
    log::info!("hive {}: tool setup notice to the coordinator: {}", caller.hive.id, sent.describe());
}

// ─── Approval ───────────────────────────────────────────────────────────────

/// What an approval did, for tests.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Approved {
    pub ok: bool,
    pub outcome: String,
    pub payload: Option<Value>,
}

/// Why the setup waits for the Mac, from the provider's route.
fn mac_reason(p: &Provider, here: &Here) -> String {
    if by_hand(p, here).is_some() {
        return format!("maiTerm can\u{2019}t install {name} from here, so you install it at the Mac by following {name}\u{2019}s guide", name = p.name);
    }
    let route = p.mcp_route().or_else(|| p.routes.first());
    match route.map(|r| (r.kind.as_str(), r.auth.as_str())) {
        Some(("remote_mcp" | "stdio_mcp", "token")) => format!("a key from {} is pasted at the Mac, never on the phone", p.name),
        Some(("remote_mcp", _)) => format!("signing in to {} happens in a browser at the Mac", p.name),
        Some(("brew", _)) if here.brew.is_none() => "Homebrew\u{2019}s installer asks for your password at the Mac".to_string(),
        _ => format!("{}\u{2019}s setup steps are done at the Mac", p.name),
    }
}

/// The human approved the card (`decide`): the Hive's window opens the setup sheet, a
/// phone-startable install starts when the phone approved it, the card settles with what happens
/// next, and the capability is watched until it is Ready.
pub(crate) async fn approved_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, r: &crate::agent_requests::AgentRequest, here: &Here) -> Approved {
    let (cap, provider_id) = (arg(&r.args, "capability").to_string(), arg(&r.args, "provider_id").to_string());
    let fail = |e: String| {
        ctx.window.settle_request(&r.id, false, e.clone());
        Approved { ok: false, outcome: e, payload: None }
    };
    let Some(p) = media::provider(&provider_id) else { return fail(format!("maiTerm no longer knows the provider {provider_id:?}")) };
    if media::capability(&cap).is_none() {
        return fail(format!("maiTerm no longer knows the capability {cap:?}"));
    }
    match super::autonomy::current_hive(ctx, caller) {
        None => return fail("its Hive is gone".to_string()),
        Some(h) if h.status == STATUS_DONE => return fail("its Hive is done".to_string()),
        Some(_) => {}
    }
    let by = r.decided_by.clone().unwrap_or_else(|| "desktop".to_string());
    let by_phone = by == "phone";
    let install: Option<Result<InstallStarted, String>> = if by_phone && phone_startable(p, here) { Some(ctx.window.start_install(&p.id).await) } else { None };
    let started = install.as_ref().and_then(|i| i.as_ref().ok()).cloned();
    let waiting = by_phone && started.is_none();
    let payload = json!({
        "request_id": r.id,
        "workspace_id": caller.workspace_id,
        "hive_id": caller.hive.id,
        "capability": cap,
        "provider_id": p.id,
        "by": by,
        "install": started,
        "waiting_for_mac": waiting,
    });
    ctx.window.tool_setup_approved(payload.clone());
    add_watch(Watch {
        request_id: r.id.clone(),
        hive_id: caller.hive.id.clone(),
        workspace_id: caller.workspace_id.clone(),
        capability: cap.clone(),
        provider_id: p.id.clone(),
        hosts: r.args.get("hosts").and_then(Value::as_array).map(|a| a.iter().filter_map(|h| h.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        why: r.reason.clone().unwrap_or_default(),
        since_ms: now_ms(),
    });
    let joins = format!("{cap} joins the Hive once it works.");
    let outcome = match (by_phone, &install, &started) {
        (false, _, _) => format!("Approved: {}\u{2019}s setup sheet is open in the Hive\u{2019}s window. {joins}", p.name),
        (true, _, Some(s)) => format!("Approved: {} is installing in a tab on the Mac (`{}`). {joins}", p.name, one_line(&s.command_shown, 200)),
        (true, Some(Err(e)), None) => {
            format!("Approved, waiting for the Mac: the install couldn\u{2019}t start from the phone ({}). {}\u{2019}s setup sheet is open there.", one_line(e, 200), p.name)
        }
        (true, _, None) => format!("Approved, waiting for the Mac: {}. Its setup sheet is open there.", mac_reason(p, here)),
    };
    ctx.window.settle_request(&r.id, true, outcome.clone());
    let next = if waiting { "It waits for your human at the Mac. " } else { "" };
    tell_coordinator(
        ctx,
        caller,
        format!(
            "{}Your human approved setting up {cap} through {}. {next}maiTerm tells you when it works; carry on meanwhile.",
            frame(&cap),
            p.name
        ),
    )
    .await;
    recheck_soon();
    Approved { ok: true, outcome, payload: Some(payload) }
}

/// Denied or lapsed: the coordinator is told.
pub(crate) async fn closed_in<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, r: &crate::agent_requests::AgentRequest) {
    let cap = arg(&r.args, "capability");
    let how = if r.status == "denied" { "denied" } else { "didn\u{2019}t answer in time; it lapsed" };
    tell_coordinator(ctx, caller, format!("{}Your human {how} your ask for {cap}: nothing was set up. Plan without it, or ask again if the work can\u{2019}t do without it.", frame(cap))).await;
}

fn app_and_caller(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) -> Option<(std::sync::Arc<crate::state::AppState>, Caller)> {
    use tauri::Manager;
    let state = app.try_state::<std::sync::Arc<crate::state::AppState>>()?.inner().clone();
    let hive_id = arg(&r.args, "hive_id").to_string();
    let caller = super::tools::hive_callers(&state.app_data.read()).into_iter().find(|c| c.hive.id == hive_id)?;
    Some((state, caller))
}

/// `agent_requests::decide`'s approval of a `tool_setup` card.
pub fn on_approved(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) {
    let _ = HANDLE.set(app.clone());
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let Some((state, caller)) = app_and_caller(&app, &r) else {
            crate::agent_requests::settle(&app, &r.id, false, "its Hive is gone".to_string());
            return;
        };
        let prefs = state.app_data.read().preferences.clone();
        let here = match tauri::async_runtime::spawn_blocking(move || Here::detect(&prefs)).await {
            Ok(h) => h,
            Err(e) => {
                crate::agent_requests::settle(&app, &r.id, false, format!("maiTerm couldn\u{2019}t read this Mac: {e}"));
                return;
            }
        };
        let ctx = super::tools::app_ctx(&state, &app, &caller);
        approved_in(&ctx, &caller, &r, &here).await;
    });
}

/// A `tool_setup` card denied or lapsed.
pub fn on_closed(app: &tauri::AppHandle, r: &crate::agent_requests::AgentRequest) {
    let (app, r) = (app.clone(), r.clone());
    tauri::async_runtime::spawn(async move {
        let Some((state, caller)) = app_and_caller(&app, &r) else { return };
        let ctx = super::tools::app_ctx(&state, &app, &caller);
        closed_in(&ctx, &caller, &r).await;
    });
}

#[cfg(test)]
mod tests;
