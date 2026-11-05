use super::*;
use crate::hive::media::status::Cover;
use crate::mcp_proxy::entries::MARKER;
use parking_lot::Mutex;

const PORT: u16 = 9;
const FAL: &str = "https://mcp.fal.ai/mcp-relay";

/// A stub upstream: answers a list and (optionally) a call as told, and records every call.
struct Stub {
    list: Result<Vec<Value>, Failure>,
    call: Option<Result<Value, Failure>>,
    lists: Mutex<usize>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl Stub {
    fn new(list: Result<Vec<Value>, Failure>, call: Option<Result<Value, Failure>>) -> Self {
        Self { list, call, lists: Mutex::new(0), calls: Mutex::new(Vec::new()) }
    }
    fn called(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(n, _)| n.clone()).collect()
    }
}

impl Probe for Stub {
    async fn list(&self, _id: &str) -> Result<Vec<Value>, Failure> {
        *self.lists.lock() += 1;
        self.list.clone()
    }
    async fn call(&self, _id: &str, tool: &str, args: Value) -> Result<Value, Failure> {
        self.calls.lock().push((tool.to_string(), args));
        self.call.clone().expect("Verify called a tool this test didn't expect")
    }
}

fn fal() -> &'static Provider {
    provider("fal").unwrap()
}

fn server(id: &str, url: &str) -> ProxyInput {
    ProxyInput { id: id.to_string(), url: url.to_string(), enabled: true, workspaces: Vec::new(), status: "connected".into() }
}

/// The entry this launch writes for server `id` (`entries::claude_entry`).
fn our_entry(id: &str, port: u16) -> Value {
    json!({
        "type": "http",
        "url": format!("http://127.0.0.1:{port}/mcp/x/{}", entry_name(id)),
        "headers": { MARKER: format!("{}/{id}", crate::state::persistence::app_data_slug()) }
    })
}

fn file(label: &str, servers: Value) -> ConfigFile {
    ConfigFile { label: label.to_string(), json: Some(json!({ "mcpServers": servers })) }
}

/// Written everywhere it should be: `~/.claude.json` and one managed account root.
fn reach_ok(id: &str) -> Reach {
    let servers = json!({ entry_name(id): our_entry(id, PORT) });
    Reach { claude_ide: true, port: Some(PORT), files: vec![file("~/.claude.json", servers.clone()), file("account Work's Claude settings", servers)] }
}

fn fal_tools() -> Vec<Value> {
    vec![
        json!({ "name": "generate_video", "inputSchema": { "type": "object", "properties": { "prompt": { "type": "string" } }, "required": ["prompt"] } }),
        json!({ "name": "get_pricing", "inputSchema": { "type": "object", "properties": { "endpoint_ids": { "type": "array" }, "unit": { "type": "string" } }, "required": ["endpoint_ids"] } }),
        json!({ "name": "estimate_cost" }),
    ]
}

fn ok_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }] })
}

fn error_result(text: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": true })
}

fn http(status: u16, detail: &str) -> Failure {
    Failure::from_up(&UpErr::Http { status, detail: detail.into() })
}

// ─── Ready ──────────────────────────────────────────────────────────────────

#[tokio::test]
async fn ready_when_the_entry_reaches_agents_the_tools_list_and_the_pricing_check_answers() {
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(ok_result("{\"prices\": []}"))));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), Some("hive-ws"), &reach_ok("fal")).await;
    assert_eq!(r.state, READY, "{}", r.message);
    assert_eq!(r.tool_count, Some(3));
    assert_eq!(r.message, "fal is ready: 3 tools, and its pricing check answered.");
    assert_eq!(r.link, None);
    let calls = stub.calls.lock().clone();
    assert_eq!(calls.len(), 1, "one check, nothing else");
    assert_eq!(calls[0].0, "get_pricing");
    // Only what the schema requires: the optional `unit` is left out.
    assert_eq!(calls[0].1, json!({ "endpoint_ids": ["fal-ai/flux/schnell"] }));
}

#[tokio::test]
async fn a_provider_without_a_check_tool_is_ready_on_its_list_alone() {
    let eleven = provider("elevenlabs").unwrap();
    let stub = Stub::new(Ok(vec![json!({ "name": "text_to_speech" })]), None);
    let r = verify_mcp(&stub, eleven, &server("elevenlabs", "https://api.elevenlabs.io/v1/mcp"), None, &reach_ok("elevenlabs")).await;
    assert_eq!((r.state.as_str(), r.tool_count), (READY, Some(1)), "{}", r.message);
    assert!(stub.called().is_empty());
}

#[tokio::test]
async fn a_server_listing_no_tools_is_not_ready() {
    let stub = Stub::new(Ok(vec![]), None);
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!((r.state.as_str(), r.tool_count), (ERROR, Some(0)));
}

// ─── Sign-in ────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_401_means_sign_in_again_whether_listing_or_checking() {
    let refused = Failure::from_up(&UpErr::Unauthorized { challenge: Some("Bearer resource_metadata=\"x\"".into()) });
    let stub = Stub::new(Err(refused.clone()), None);
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!(r.state, NEEDS_SIGN_IN, "{}", r.message);
    assert!(r.message.contains("(401)") && r.message.contains("sign in to fal again"), "{}", r.message);
    assert!(stub.called().is_empty(), "nothing is called on a server that refused the list");

    let stub = Stub::new(Ok(fal_tools()), Some(Err(refused)));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!((r.state.as_str(), r.tool_count), (NEEDS_SIGN_IN, Some(3)), "{}", r.message);

    // A tool that answers "unauthorized" in its own words.
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(error_result("Unauthorized: invalid token"))));
    assert_eq!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.state, NEEDS_SIGN_IN);
}

// ─── Credit ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn a_402_means_add_credit_with_the_billing_page_as_the_link() {
    let stub = Stub::new(Ok(fal_tools()), Some(Err(http(402, "Payment Required"))));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!(r.state, NEEDS_CREDIT);
    assert_eq!(r.message, "Signed in, but fal answered 402: add credit at fal.ai/dashboard/billing");
    assert_eq!(r.link.as_deref(), Some("https://fal.ai/dashboard/billing"));
    assert_eq!(r.tool_count, Some(3));

    // The same 402 wrapped in a tool result, and a 402 on the list itself.
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(error_result("fal API error 402: Payment Required"))));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!((r.state.as_str(), r.link.as_deref()), (NEEDS_CREDIT, Some("https://fal.ai/dashboard/billing")));
    let stub = Stub::new(Err(http(402, "")), None);
    assert_eq!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.state, NEEDS_CREDIT);

    // Out of credit in words, with no code.
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(error_result("Insufficient balance for this request"))));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!(r.state, NEEDS_CREDIT);
    assert!(r.message.starts_with("Signed in, but fal says the account is out of credit"), "{}", r.message);
}

#[test]
fn the_billing_link_is_only_ever_http() {
    let mut p = fal().clone();
    p.billing_url = "javascript:alert(1)".into();
    let r = needs_credit(&p, true);
    assert_eq!(r.link, None);
    assert_eq!(r.message, "Signed in, but fal answered 402: add credit in your fal account.");
    p.billing_url = String::new();
    assert_eq!(needs_credit(&p, true).link, None);
}

#[test]
fn a_code_counts_only_as_a_whole_number() {
    assert!(says_no_credit("HTTP 402"));
    assert!(!says_no_credit("listed 4021 models"));
    assert!(!says_signed_out("job 14012 done"));
}

// ─── Reaching agents ────────────────────────────────────────────────────────

#[tokio::test]
async fn unreachable_when_the_entry_isnt_written_and_nothing_is_read() {
    let check = |reach: Reach| async move {
        let stub = Stub::new(Ok(fal_tools()), None);
        let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach).await;
        assert_eq!(*stub.lists.lock(), 0, "the gate stops before the network");
        assert!(stub.called().is_empty());
        assert_eq!(r.state, UNREACHABLE, "{}", r.message);
        r.message
    };
    // Phase 1's gap: IDE integration off, so no entry is written though the registry has fal.
    let m = check(Reach { claude_ide: false, ..reach_ok("fal") }).await;
    assert!(m.contains("Claude Code IDE integration is on") && m.contains("Preferences → AI Agents"), "{m}");
    let m = check(Reach { port: None, ..reach_ok("fal") }).await;
    assert!(m.contains("isn't running"), "{m}");

    // Written in ~/.claude.json but missing from an account root.
    let mut reach = reach_ok("fal");
    reach.files[1] = file("account Work's Claude settings", json!({}));
    let m = check(reach).await;
    assert!(m.contains("isn't written in account Work's Claude settings"), "{m}");
    // No ~/.claude.json at all.
    let mut reach = reach_ok("fal");
    reach.files[0].json = None;
    assert!(check(reach).await.contains("~/.claude.json"));
    // A previous launch's port is a stale entry, not a working one.
    let stale = json!({ entry_name("fal"): our_entry("fal", PORT + 1) });
    let reach = Reach { files: vec![file("~/.claude.json", stale)], ..reach_ok("fal") };
    assert!(check(reach).await.contains("isn't written in ~/.claude.json"));
    // The human's own entry holds the name.
    let native = json!({ entry_name("fal"): { "type": "http", "url": "https://mcp.fal.ai/mcp" } });
    let reach = Reach { files: vec![file("~/.claude.json", native)], ..reach_ok("fal") };
    let m = check(reach).await;
    assert!(m.contains("Your own MCP server named"), "{m}");
}

#[tokio::test]
async fn unreachable_when_switched_off_or_scoped_away_from_the_hive() {
    let mut s = server("fal", FAL);
    s.workspaces = vec!["other-ws".into()];
    let stub = Stub::new(Ok(fal_tools()), None);
    let r = verify_mcp(&stub, fal(), &s, Some("hive-ws"), &reach_ok("fal")).await;
    assert_eq!(r.state, UNREACHABLE);
    assert!(r.message.contains("limited to other workspaces"), "{}", r.message);
    assert_eq!(*stub.lists.lock(), 0);
    // In scope once the Hive's workspace is on it; Preferences (no workspace) doesn't judge scope.
    s.workspaces.push("hive-ws".into());
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(ok_result("ok"))));
    assert_eq!(verify_mcp(&stub, fal(), &s, Some("hive-ws"), &reach_ok("fal")).await.state, READY);
    s.workspaces = vec!["other-ws".into()];
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(ok_result("ok"))));
    assert_eq!(verify_mcp(&stub, fal(), &s, None, &reach_ok("fal")).await.state, READY);

    let mut off = server("fal", FAL);
    off.enabled = false;
    let stub = Stub::new(Ok(fal_tools()), None);
    let r = verify_mcp(&stub, fal(), &off, None, &reach_ok("fal")).await;
    assert_eq!(r.state, UNREACHABLE);
    assert!(r.message.contains("switched off"), "{}", r.message);
}

#[test]
fn the_server_checked_is_recognised_by_its_upstream_and_prefers_one_in_scope() {
    let mut scoped = server("fal-work", FAL);
    scoped.workspaces = vec!["other".into()];
    let named_fal_elsewhere = server("fal", "https://example.com/mcp");
    let open = server("video", FAL);
    let servers = vec![named_fal_elsewhere, scoped, open];
    assert_eq!(pick_server(&servers, fal(), Some("hive-ws")).map(|s| s.id.as_str()), Some("video"));
    assert_eq!(pick_server(&servers[..2], fal(), Some("hive-ws")).map(|s| s.id.as_str()), Some("fal-work"), "out of scope beats nothing, and says why");
    assert!(pick_server(&servers[..1], fal(), None).is_none(), "a server named fal that dials elsewhere is not fal");
}

// ─── The allowlist ──────────────────────────────────────────────────────────

#[test]
fn the_allowlist_is_exactly_the_five_checks_and_refuses_anything_else() {
    let names: Vec<(&str, &str)> = CHECK_TOOLS.iter().map(|c| (c.provider, c.name)).collect();
    assert_eq!(names, vec![("fal", "get_pricing"), ("wavespeed", "get_balance"), ("bfl", "get_credits"), ("recraft", "get_user"), ("meshy", "meshy_check_balance")]);
    for (p, name) in [("fal", "generate_video"), ("fal", "submit_job"), ("fal", "get_balance"), ("wavespeed", "get_price"), ("wavespeed", "text_to_video"), ("recraft", "generate_image"), ("kling", "get_user"), ("fal", "")] {
        assert!(check_tool(p, name).is_err(), "{p}/{name} must be refused");
    }
    // Each allowlisted tool is one its provider's catalogue route names, so the two can't drift.
    for c in CHECK_TOOLS {
        let p = provider(c.provider).unwrap_or_else(|| panic!("{} isn't in the catalogue", c.provider));
        assert!(p.routes.iter().any(|r| r.balance_tool.as_deref() == Some(c.name) || r.quote_tool.as_deref() == Some(c.name)), "{}/{}", c.provider, c.name);
    }
}

#[tokio::test]
async fn run_check_refuses_a_tool_outside_the_allowlist_before_calling() {
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(ok_result("spent"))));
    let refused = run_check(&stub, fal(), "fal", "generate_video", json!({ "prompt": "x" })).await;
    assert!(refused.unwrap_err().contains("Verify never calls generate_video"));
    assert!(stub.called().is_empty(), "nothing reached the upstream");
    assert!(run_check(&stub, fal(), "fal", "get_pricing", json!({})).await.is_ok());
    assert_eq!(stub.called(), vec!["get_pricing"]);
}

#[tokio::test]
async fn a_catalogue_tool_outside_the_allowlist_is_never_called() {
    let wavespeed = provider("wavespeed").unwrap();
    let ws = server("wavespeed", "https://api.wavespeed.ai/mcp");
    // WaveSpeed names `get_price` (quote) and `get_balance`: only the balance is called.
    let stub = Stub::new(Ok(vec![json!({ "name": "get_price" }), json!({ "name": "get_balance" }), json!({ "name": "text_to_video" })]), Some(Ok(ok_result("$12.40"))));
    let r = verify_mcp(&stub, wavespeed, &ws, None, &reach_ok("wavespeed")).await;
    assert_eq!(r.state, READY);
    assert_eq!(stub.called(), vec!["get_balance"]);
    // With only `get_price` listed nothing is called at all.
    let stub = Stub::new(Ok(vec![json!({ "name": "get_price" }), json!({ "name": "text_to_video" })]), None);
    let r = verify_mcp(&stub, wavespeed, &ws, None, &reach_ok("wavespeed")).await;
    assert_eq!(r.state, READY);
    assert!(stub.called().is_empty());
}

#[test]
fn check_arguments_fill_only_what_is_required_and_harmless() {
    let hints = json!({ "endpoint_ids": ["a"] });
    assert_eq!(check_args(None, &hints), Some(json!({})));
    let schema = json!({ "properties": { "endpoint_ids": {}, "currency": { "default": "USD" }, "unit": { "enum": ["image", "second"] } }, "required": ["endpoint_ids", "currency", "unit"] });
    assert_eq!(check_args(Some(&schema), &hints), Some(json!({ "endpoint_ids": ["a"], "currency": "USD", "unit": "image" })));
    // A required argument maiTerm has no value for: not called.
    let schema = json!({ "properties": { "model": { "type": "string" } }, "required": ["model"] });
    assert_eq!(check_args(Some(&schema), &hints), None);
    let stub_tools = vec![json!({ "name": "get_pricing", "inputSchema": schema })];
    assert!(plan_check(fal(), &stub_tools).is_none());
}

#[tokio::test]
async fn arguments_the_server_rejects_are_maitermss_guess_not_the_account() {
    let rejected = Failure::from_up(&UpErr::Rpc { code: -32602, message: "invalid params".into(), data: None });
    let stub = Stub::new(Ok(fal_tools()), Some(Err(rejected)));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!(r.state, READY);
    assert!(r.message.contains("didn't take maiTerm's arguments"), "{}", r.message);
    // Any other tool error is shown, in a short excerpt.
    let stub = Stub::new(Ok(fal_tools()), Some(Ok(error_result(&"x".repeat(500)))));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!(r.state, ERROR);
    assert!(r.message.len() < 400, "{}", r.message);
}

// ─── A failure keeps the saved list ─────────────────────────────────────────

/// Verify through the real proxy against an upstream that doesn't answer (a closed loopback port,
/// so nothing leaves this machine): not ready, and the saved tool list is exactly as it was.
#[tokio::test]
async fn a_failed_verify_leaves_the_proxys_saved_list_unchanged() {
    use crate::mcp_proxy::{Catalog, ProxyServer};
    let id = format!("verify-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]);
    let saved = Catalog { tools: vec![json!({ "name": "get_pricing" }), json!({ "name": "generate_video" })], server_info: json!({}), instructions: None, fetched_at: 3 };
    crate::mcp_proxy::insert_for_test(ProxyServer {
        id: id.clone(),
        url: "http://127.0.0.1:1/mcp".into(),
        auth: crate::mcp_proxy::AUTH_NONE.into(),
        enabled: true,
        workspaces: vec![],
        catalog: Some(saved.clone()),
        oauth: None,
        added_at: 0,
        added_by: "test".into(),
    });
    let r = verify_mcp(&ProxyProbe, fal(), &server(&id, "http://127.0.0.1:1/mcp"), None, &reach_ok(&id)).await;
    assert_ne!(r.state, READY, "{}", r.message);
    assert_eq!(r.tool_count, None, "no list was read");
    assert_eq!(crate::mcp_proxy::get(&id).unwrap().catalog, Some(saved), "a failure never removes a tool or rewrites the list");
}

// ─── Not proxied ────────────────────────────────────────────────────────────

fn hit(tool_id: &str, provider: &str, state: &str) -> Hit {
    Hit { cover: Cover { tool_id: tool_id.into(), provider: provider.into(), state: state.into() }, capabilities: vec![] }
}

#[test]
fn a_provider_maiterm_doesnt_proxy_is_verified_by_detection() {
    let kit = provider("homebrew-kit").unwrap();
    let all: Vec<Hit> = ["ffmpeg", "ffprobe", "magick", "sox", "whisper-cli"].iter().map(|b| hit(&format!("cli:{b}"), "homebrew-kit", READY)).collect();
    assert_eq!(verify_local(kit, &all).state, READY);
    let r = verify_local(kit, &all[..2]);
    assert_eq!(r.state, UNREACHABLE);
    assert_eq!(r.message, "Found ffmpeg and ffprobe, but not magick, sox and whisper-cli. Run Homebrew kit's install again.");
    let r = verify_local(kit, &[]);
    assert_eq!((r.state.as_str(), r.message.as_str()), (UNREACHABLE, "maiTerm doesn't find Homebrew kit on this Mac yet. Run its install again."));

    let remotion = provider("remotion").unwrap();
    assert_eq!(verify_local(remotion, &[hit("plugin:remotion@remotion", "remotion", READY)]).state, READY);
    let r = verify_local(remotion, &[hit("plugin:remotion@remotion", "remotion", INSTALLED)]);
    assert_eq!(r.state, INSTALLED);
    assert!(r.message.contains("switched off"), "{}", r.message);
    // Another provider's hit is not this one's.
    assert_eq!(verify_local(remotion, &[hit("cli:ffmpeg", "homebrew-kit", READY)]).state, UNREACHABLE);
}
