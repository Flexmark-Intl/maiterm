use super::*;
use crate::hive::media::status::Cover;
use crate::mcp_proxy::entries::MARKER;
use parking_lot::Mutex;

const PORT: u16 = 9;
const FAL: &str = "https://mcp.fal.ai/mcp-relay";

/// A stub upstream: answers a list and (optionally) a call as told, and records every call.
struct Stub {
    list: Result<Vec<Value>, Failure>,
    /// An earlier refused call is on record (`Listed.calls_refused`).
    refused: bool,
    call: Option<Result<Value, Failure>>,
    lists: Mutex<usize>,
    calls: Mutex<Vec<(String, Value)>>,
}

impl Stub {
    fn new(list: Result<Vec<Value>, Failure>, call: Option<Result<Value, Failure>>) -> Self {
        Self { list, refused: false, call, lists: Mutex::new(0), calls: Mutex::new(Vec::new()) }
    }
    fn called(&self) -> Vec<String> {
        self.calls.lock().iter().map(|(n, _)| n.clone()).collect()
    }
}

impl Probe for Stub {
    async fn list(&self, _id: &str) -> Result<Listed, Failure> {
        *self.lists.lock() += 1;
        self.list.clone().map(|tools| Listed { tools, calls_refused: self.refused })
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
fn a_code_counts_only_as_a_status_not_inside_an_id_or_a_name() {
    for yes in ["HTTP 402", "HTTP/1.1 402", "HTTP/2 402", "upstream answered HTTP/1.1 402", "fal API error 402: Payment Required", "402 Payment Required", "{\"status\": 402}", "status code 402"] {
        assert!(says_no_credit(yes), "{yes}");
    }
    for no in [
        "listed 4021 models",
        "HTTP 500: internal error (request id 9c1e402f-77ab-4c1d-b0e2-3a5d1f8e6b21)",
        "error -32603: upstream failed, trace d1e402ff",
        "Endpoint fal-ai/flux-v402 not found",
        "request 17-402-88 failed",
        "$4.02 per image",
        "$402 for 100 videos",
        "{\"prices\": [{\"endpoint\": \"fal-ai/flux/dev\", \"price\": 402}]}",
        "Status codes: 400, 401, 402, 403",
        "job 0x402ab done",
    ] {
        assert!(!says_no_credit(no), "{no}");
    }
    assert!(says_signed_out("HTTP 401"));
    assert!(says_signed_out("HTTP/1.1 401"));
    assert!(!says_no_credit("version 1.1 402"), "digits and dots count only after http/");
    assert!(!says_signed_out("job 14012 done"));
    assert!(!says_signed_out("job a401bc failed"));
    assert!(!says_signed_out("Status codes: 400, 401, 402, 403"));
    assert!(!says_signed_out("$4.01 per second"));
}

#[tokio::test]
async fn an_unrelated_error_keeps_its_own_state_whatever_ids_it_carries() {
    // A 5xx whose body holds a request id with "402" in it is a 5xx.
    let body = "internal error (request id 9c1e402f-77ab-4c1d-b0e2-3a5d1f8e6b21)";
    let stub = Stub::new(Ok(fal_tools()), Some(Err(http(500, body))));
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!((r.state.as_str(), r.link.as_deref()), (UNREACHABLE, None), "{}", r.message);
    let stub = Stub::new(Err(http(503, body)), None);
    assert_eq!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.state, UNREACHABLE);
    // A 5xx that mentions credit is still the 5xx it says it is.
    let stub = Stub::new(Ok(fal_tools()), Some(Err(http(500, "billing service down: insufficient balance check failed"))));
    assert_eq!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.state, UNREACHABLE);
    // A JSON-RPC error with a hex trace id.
    let rpc = Failure::from_up(&UpErr::Rpc { code: -32603, message: "upstream failed, trace d1e402ff".into(), data: None });
    let stub = Stub::new(Ok(fal_tools()), Some(Err(rpc)));
    assert_eq!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.state, ERROR);
    // Tool error texts holding a hex id or a model name with the digits in it.
    for text in ["Job a401bc0e failed", "Endpoint fal-ai/flux-v402 not found", "trace 7e402a: try later"] {
        let stub = Stub::new(Ok(fal_tools()), Some(Ok(error_result(text))));
        let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
        assert_eq!((r.state.as_str(), r.link.as_deref()), (ERROR, None), "{text}: {}", r.message);
    }
}

// ─── An earlier refused call ────────────────────────────────────────────────

#[tokio::test]
async fn a_list_that_reads_while_a_call_was_refused_lets_the_check_decide() {
    // An agent's call was refused earlier; the human fixed the account. The list reads, the free
    // check answers: ready (and the real proxy's call_once clears the refusal).
    let mut stub = Stub::new(Ok(fal_tools()), Some(Ok(ok_result("{\"prices\": []}"))));
    stub.refused = true;
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!((r.state.as_str(), r.tool_count), (READY, Some(3)), "{}", r.message);
    assert_eq!(stub.called(), vec!["get_pricing"]);
    // Still refused: the check says so, about the call, not the list.
    let mut stub = Stub::new(Ok(fal_tools()), Some(Err(Failure::from_up(&UpErr::Forbidden { challenge: None }))));
    stub.refused = true;
    let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
    assert_eq!((r.state.as_str(), r.tool_count), (NEEDS_SIGN_IN, Some(3)));
    assert!(!r.message.contains("listing"), "{}", r.message);
    // The check rejects maiTerm's arguments: nothing proved calls work, so the proxy still says
    // sign in, and Verify agrees with the panel.
    for code in [-32602, -32601] {
        let rejected = Failure::from_up(&UpErr::Rpc { code, message: "invalid params".into(), data: None });
        let mut stub = Stub::new(Ok(fal_tools()), Some(Err(rejected)));
        stub.refused = true;
        let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
        assert_eq!((r.state.as_str(), r.tool_count), (NEEDS_SIGN_IN, Some(3)), "{code}: {}", r.message);
        assert_eq!(r.message, "fal lists its 3 tools but refused a tool call: sign in to fal again.");
    }
    // No check tool to try: the refusal stands, in words about tool calls, with the count kept.
    let mut stub = Stub::new(Ok(vec![json!({ "name": "text_to_speech" })]), None);
    stub.refused = true;
    let r = verify_mcp(&stub, provider("elevenlabs").unwrap(), &server("elevenlabs", "https://api.elevenlabs.io/v1/mcp"), None, &reach_ok("elevenlabs")).await;
    assert_eq!((r.state.as_str(), r.tool_count), (NEEDS_SIGN_IN, Some(1)));
    assert_eq!(r.message, "ElevenLabs lists its 1 tool but refused a tool call: sign in to ElevenLabs again.");
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
fn the_allowlist_is_exactly_the_six_checks_and_refuses_anything_else() {
    let names: Vec<(&str, &str)> = CHECK_TOOLS.iter().map(|c| (c.provider, c.name)).collect();
    assert_eq!(
        names,
        vec![("fal", "get_pricing"), ("wavespeed", "get_balance"), ("bfl", "get_credits"), ("recraft", "get_user"), ("meshy", "meshy_check_balance"), ("minimax", "list_voices")]
    );
    for (p, name) in [
        ("fal", "generate_video"),
        ("fal", "submit_job"),
        ("fal", "get_balance"),
        ("wavespeed", "get_price"),
        ("wavespeed", "text_to_video"),
        ("recraft", "generate_image"),
        ("kling", "get_user"),
        ("fal", ""),
        ("minimax", "text_to_audio"),
        ("minimax", "voice_clone"),
        ("minimax", "play_audio"),
        ("meshy", "meshy_text_to_3d"),
    ] {
        assert!(check_tool(p, name).is_err(), "{p}/{name} must be refused");
    }
    // Each allowlisted tool is one its provider's catalogue route names, so the two can't drift.
    for c in CHECK_TOOLS {
        let p = provider(c.provider).unwrap_or_else(|| panic!("{} isn't in the catalogue", c.provider));
        assert!(
            p.routes.iter().any(|r| [r.balance_tool.as_deref(), r.quote_tool.as_deref(), r.check_tool.as_deref()].contains(&Some(c.name))),
            "{}/{}",
            c.provider,
            c.name
        );
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
        transport: crate::mcp_proxy::stdio::TRANSPORT_HTTP.into(),
        stdio: None,
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

#[tokio::test]
async fn one_tool_is_one_tool() {
    let eleven = provider("elevenlabs").unwrap();
    let one = |list: Vec<Value>, call: Option<Result<Value, Failure>>| Stub::new(Ok(list), call);
    let stub = one(vec![json!({ "name": "text_to_speech" })], None);
    let r = verify_mcp(&stub, eleven, &server("elevenlabs", "https://api.elevenlabs.io/v1/mcp"), None, &reach_ok("elevenlabs")).await;
    assert_eq!(r.message, "ElevenLabs is ready: 1 tool.");
    let pricing = || vec![fal_tools()[1].clone()];
    let stub = one(pricing(), Some(Ok(ok_result("ok"))));
    assert_eq!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.message, "fal is ready: 1 tool, and its pricing check answered.");
    let stub = one(pricing(), Some(Ok(error_result("something odd"))));
    let m = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.message;
    assert!(m.contains("Its 1 tool is listed,"), "{m}");
    let rejected = Failure::from_up(&UpErr::Rpc { code: -32602, message: "invalid params".into(), data: None });
    let stub = one(pricing(), Some(Err(rejected)));
    assert!(verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.message.starts_with("fal is ready: 1 tool. "));
    // And the plural.
    let stub = one(fal_tools(), Some(Ok(error_result("something odd"))));
    let m = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await.message;
    assert!(m.contains("Its 3 tools are listed,"), "{m}");
}

/// Verify says ready only where the panel's next `hiveMediaStatus` says ready too.
/// - MCP: the gate passed (on, in scope: `status::proxy_state` judges the same two), and the
///   proxy's status after Verify is `connected`. That holds when the list read with no refusal
///   on record (`refresh` sets `connected`), and when a refusal was on record only if the check
///   call ANSWERED (`call_once`'s Ok arm clears the refusal and sets `connected`; an isError
///   result is still an answer). Every other answer leaves `needs_sign_in`, so it must not be ready.
/// - Not proxied: `verify_local` reads the same hits `status::rows` does.
#[tokio::test]
async fn ready_only_where_the_panels_next_status_is_ready() {
    let rpc = |code| Failure::from_up(&UpErr::Rpc { code, message: "bad".into(), data: None });
    let answers: Vec<(&str, Result<Value, Failure>)> = vec![
        ("answered", Ok(ok_result("ok"))),
        ("tool error", Ok(error_result("something odd"))),
        ("-32602", Err(rpc(-32602))),
        ("-32601", Err(rpc(-32601))),
        ("-32603", Err(rpc(-32603))),
        ("401", Err(Failure::from_up(&UpErr::Unauthorized { challenge: None }))),
        ("403", Err(Failure::from_up(&UpErr::Forbidden { challenge: None }))),
        ("500", Err(http(500, ""))),
        ("net", Err(Failure::from_up(&UpErr::Net("timed out".into())))),
    ];
    for (what, answer) in answers {
        let mut stub = Stub::new(Ok(fal_tools()), Some(answer));
        stub.refused = true;
        let r = verify_mcp(&stub, fal(), &server("fal", FAL), None, &reach_ok("fal")).await;
        // Only an answer clears the refusal in the proxy (call_once's Ok arm).
        assert_eq!(r.state == READY, what == "answered", "refused, then {what}: {} ({})", r.state, r.message);
    }
    // A server the panel calls unreachable (off, or scoped away) is never ready here.
    let mut off = server("fal", FAL);
    off.enabled = false;
    let mut scoped = server("fal", FAL);
    scoped.workspaces = vec!["other".into()];
    for (s, ws) in [(off, None), (scoped, Some("hive-ws"))] {
        assert_eq!(status::proxy_state(&s, ws), UNREACHABLE);
        let stub = Stub::new(Ok(fal_tools()), Some(Ok(ok_result("ok"))));
        assert_ne!(verify_mcp(&stub, fal(), &s, ws, &reach_ok("fal")).await.state, READY);
    }
    // Not proxied: ready exactly when the rows show a ready cover from that provider.
    let kit = provider("homebrew-kit").unwrap();
    let cli = |b: &str| crate::hive::create::DetectedTool { id: format!("cli:{b}"), kind: "cli".into(), label: b.into(), host: None, pack: None, proxy: None };
    let detected: Vec<_> = ["ffmpeg", "ffprobe", "magick", "sox", "whisper-cli"].iter().map(|b| cli(b)).collect();
    for found in [&detected[..], &detected[..2], &[][..]] {
        let hits = &status::cli_hits(found)[..];
        let ready_here = verify_local(kit, hits).state == READY;
        let rows = status::rows(hits, &[]);
        let all_rows_ready = ["finish", "transcribe"].iter().all(|c| {
            rows.iter().find(|r| r.capability == *c).unwrap().covered_by.iter().any(|cv| cv.provider == "homebrew-kit" && cv.state == READY)
        });
        assert_eq!(ready_here, all_rows_ready, "{} programs", found.len());
    }
}

// ─── Phase 5: servers maiTerm runs itself with a key ────────────────────────

fn minimax_tools() -> Vec<Value> {
    vec![
        json!({ "name": "text_to_audio", "inputSchema": { "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] } }),
        json!({ "name": "list_voices", "inputSchema": { "type": "object", "properties": { "voice_type": { "type": "string", "default": "all" } } } }),
        json!({ "name": "play_audio" }),
    ]
}

fn meshy_tools() -> Vec<Value> {
    vec![
        json!({ "name": "meshy_text_to_3d", "inputSchema": { "type": "object", "properties": { "prompt": { "type": "string" } }, "required": ["prompt"] } }),
        json!({ "name": "meshy_check_balance", "inputSchema": { "type": "object", "properties": { "response_format": { "type": "string" } } } }),
    ]
}

#[test]
fn a_stdio_server_is_the_one_verified_for_its_provider_by_its_route() {
    let servers = vec![server("meshy", "stdio:meshy"), server("mm", "stdio:minimax-mainland"), server("fal", FAL)];
    assert_eq!(pick_server(&servers, provider("meshy").unwrap(), None).map(|s| s.id.as_str()), Some("meshy"));
    assert_eq!(pick_server(&servers, provider("minimax").unwrap(), None).map(|s| s.id.as_str()), Some("mm"));
    // Its name is nothing: a server called "meshy" that dials elsewhere isn't Meshy.
    assert!(pick_server(&[server("meshy", FAL)], provider("meshy").unwrap(), None).is_none());
}

#[tokio::test]
async fn meshy_is_ready_when_its_balance_check_answers_and_only_that_tool_is_called() {
    let meshy = provider("meshy").unwrap();
    let stub = Stub::new(Ok(meshy_tools()), Some(Ok(ok_result("{ \"balance\": 120 }"))));
    let r = verify_mcp(&stub, meshy, &server("meshy", "stdio:meshy"), None, &reach_ok("meshy")).await;
    assert_eq!(r.state, READY, "{r:?}");
    assert_eq!(stub.called(), vec!["meshy_check_balance"]);
    assert_eq!(stub.calls.lock()[0].1, json!({}), "only required arguments, and it has none");
}

#[tokio::test]
async fn a_refused_meshy_key_says_paste_a_new_key_not_sign_in() {
    let meshy = provider("meshy").unwrap();
    let text = "Error: Authentication failed: the Meshy API rejected MESHY_API_KEY (401). The key is mistyped, revoked or expired.";
    let stub = Stub::new(Ok(meshy_tools()), Some(Ok(error_result(text))));
    let r = verify_mcp(&stub, meshy, &server("meshy", "stdio:meshy"), None, &reach_ok("meshy")).await;
    assert_eq!(r.state, NEEDS_SIGN_IN, "{r:?}");
    assert!(r.message.contains("paste a new Meshy key"), "{}", r.message);
    assert!(!r.message.contains("sign in"), "a key server has no sign-in: {}", r.message);
    // Out of credit: Meshy's 402, with its billing page.
    let stub = Stub::new(Ok(meshy_tools()), Some(Ok(error_result("Error: Payment required (402). Insufficient credits."))));
    let r = verify_mcp(&stub, meshy, &server("meshy", "stdio:meshy"), None, &reach_ok("meshy")).await;
    assert_eq!(r.state, NEEDS_CREDIT);
    assert_eq!(r.link.as_deref(), Some("https://www.meshy.ai/pricing"));
}

/// MiniMax's tools answer a failure as ordinary text ("Failed to list voices: …"), never isError:
/// read as a pass, a wrong key would show Ready.
#[tokio::test]
async fn minimax_s_failure_text_is_a_failure_and_a_wrong_key_points_at_the_region() {
    let mm = provider("minimax").unwrap();
    let ok = Stub::new(Ok(minimax_tools()), Some(Ok(ok_result("Success. System Voices: [...], Voice Cloning Voices: []"))));
    let r = verify_mcp(&ok, mm, &server("minimax", "stdio:minimax"), None, &reach_ok("minimax")).await;
    assert_eq!(r.state, READY, "{r:?}");
    assert_eq!(ok.called(), vec!["list_voices"], "the free account check, never a generation");

    let text = "Failed to list voices: API Error: login fail: Please carry the API secret key in the 'Authorization' field of the request header, please check your API key and API host.Trace-Id: abc";
    let bad = Stub::new(Ok(minimax_tools()), Some(Ok(ok_result(text))));
    let r = verify_mcp(&bad, mm, &server("minimax", "stdio:minimax"), None, &reach_ok("minimax")).await;
    assert_eq!(r.state, NEEDS_SIGN_IN, "{r:?}");
    assert!(r.message.contains("the account you chose"), "two routes: the key may be for the other region: {}", r.message);

    let broke = Stub::new(Ok(minimax_tools()), Some(Ok(ok_result("Failed to list voices: API Error: 1008-insufficient balance"))));
    assert_eq!(verify_mcp(&broke, mm, &server("minimax", "stdio:minimax"), None, &reach_ok("minimax")).await.state, NEEDS_CREDIT);

    let other = Stub::new(Ok(minimax_tools()), Some(Ok(ok_result("Failed to list voices: API Error: 2038-real-name verification"))));
    let r = verify_mcp(&other, mm, &server("minimax", "stdio:minimax"), None, &reach_ok("minimax")).await;
    assert_eq!(r.state, ERROR, "{r:?}");
}

#[test]
fn a_key_servers_refusal_words_are_about_the_key() {
    let f = Failure { state: NEEDS_SIGN_IN.into(), http: Some(401), rpc: None, detail: "401".into() };
    let r = failed(provider("meshy").unwrap(), &f, "listing its tools");
    assert!(r.message.contains("refused the key (401)") && r.message.contains("paste a new Meshy key"), "{}", r.message);
    assert!(failed(fal(), &f, "listing its tools").message.contains("sign in to fal again"));
}
