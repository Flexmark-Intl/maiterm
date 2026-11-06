//! Stdio upstreams against a fake stdio MCP server: a POSIX sh script written by the tests, so
//! nothing here downloads or runs a package. It prints its key to stderr (which must never reach
//! a log), a banner to stdout (which must be skipped), and answers `initialize`, `tools/list` and a
//! few tools that report what the child sees: its environment, argv, pid and folder. Some echo
//! its key back (which must never get past the proxy), and one never answers. It writes its pid
//! to `pid.txt` in its folder, and takes 2 s to start while a `slowstart` file is there.

use crate::mcp_proxy::{self as proxy, entries, AUTH_KEY, STATE_IDLE};
use super::*;
use serde_json::json;
use std::time::Duration;

/// The fake key. Distinctive, so finding it anywhere is a leak, not a coincidence.
const KEY: &str = "VALUE-fake-mcp-key-9d3e71b2";
const KEY_VAR: &str = "FAKE_MCP_KEY";

const FAKE: &str = r#"
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$1" "$2"; }
printf '%s\n' "$$" > pid.txt
[ -f slowstart ] && sleep 2
printf 'fake server booting\n'
printf 'my key is %s\n' "$FAKE_MCP_KEY" >&2
printf 'a second stderr line\n' >&2
while IFS= read -r line; do
  id=$(printf '%s\n' "$line" | sed -n 's/.*"id":\([0-9][0-9]*\).*/\1/p')
  case "$line" in
    *'"method":"initialize"'*)
      printf '{"jsonrpc":"2.0","id":%s,"result":{"protocolVersion":"2025-06-18","capabilities":{"tools":{"listChanged":true}},"serverInfo":{"name":"fake","version":"1"},"instructions":"a fake"}}\n' "$id" ;;
    *'"method":"tools/list"'*)
      extra=''
      [ -f grown ] && extra=',{"name":"extra","inputSchema":{"type":"object"}}'
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"fast","description":"signed in as %s","inputSchema":{"type":"object"}},{"name":"slow","inputSchema":{"type":"object"}}%s]}}\n' "$id" "$FAKE_MCP_KEY" "$extra" ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *'"name":"slow"'*) ( sleep 1; reply "$id" slow ) & ;;
        *'"name":"fast"'*) reply "$id" fast ;;
        *'"name":"env"'*) reply "$id" "$(env | sed 's/=.*//' | sort | tr '\n' ' ')|$(pwd)" ;;
        *'"name":"settings"'*) reply "$id" "$FAKE_MCP_HOST|$FAKE_MCP_OUT" ;;
        *'"name":"leak"'*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32000,"message":"invalid api key %s","data":{"sent":"%s"}}}\n' "$id" "$FAKE_MCP_KEY" "$FAKE_MCP_KEY" ;;
        *'"name":"leakresult"'*) printf '{"jsonrpc":"2.0","id":%s,"result":{"isError":true,"content":[{"type":"text","text":"key %s was refused"}]}}\n' "$id" "$FAKE_MCP_KEY" ;;
        *'"name":"hang"'*) : ;;
        *'"name":"argv"'*) reply "$id" "$0 $*" ;;
        *'"name":"pid"'*) reply "$id" "$$" ;;
        *'"name":"die"'*) exit 3 ;;
        *'"name":"grow"'*) : > grown; printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'; reply "$id" grown ;;
        *'"name":"record"'*) printf '%s\n' "$line" >> calls.txt; reply "$id" recorded ;;
        *'"name":"bad"'*) printf '{"jsonrpc":"2.0","id":%s,"error":{"code":-32602,"message":"bad arguments"}}\n' "$id" ;;
      esac ;;
  esac
done
"#;

fn script(name: &str, body: &str) -> String {
    let dir = std::env::temp_dir().join(format!("maiterm-test-fake-mcp-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path.to_string_lossy().into_owned()
}

const HOST: &str = "https://api.fake.test";

fn spec_for(path: String) -> StdioSpec {
    let env = [("FAKE_MCP_HOST", HOST), ("FAKE_MCP_OUT", OUTPUT_PLACEHOLDER)].map(|(n, v)| (n.to_string(), v.to_string()));
    StdioSpec { route: "test/fake".into(), command: "/bin/sh".into(), args: vec![path], env_key: Some(KEY_VAR.into()), env: env.into(), arg_rules: vec![] }
}

/// The idle sweep, for this test's server only: another test's sweep never stops it.
fn sweep(id: &str) {
    stop_idle_for_test(id, now_ms(), IDLE);
}

fn fresh_id(prefix: &str) -> String {
    format!("{prefix}-{}", &uuid::Uuid::new_v4().simple().to_string()[..8])
}

// ─── Logs, captured ─────────────────────────────────────────────────────────

struct Capture;
static LINES: parking_lot::Mutex<Vec<String>> = parking_lot::Mutex::new(Vec::new());
static CAPTURE: Capture = Capture;

impl log::Log for Capture {
    fn enabled(&self, _: &log::Metadata) -> bool {
        true
    }
    fn log(&self, r: &log::Record) {
        LINES.lock().push(format!("{} {}", r.target(), r.args()));
    }
    fn flush(&self) {}
}

fn capture_logs() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let _ = log::set_logger(&CAPTURE);
        log::set_max_level(log::LevelFilter::Trace);
    });
}

fn logs() -> String {
    LINES.lock().join("\n")
}

/// A fake server, added through the real verify-before-write path, with its key stored first.
async fn added(prefix: &str) -> String {
    capture_logs();
    let id = fresh_id(prefix);
    proxy::set_key(&id, KEY).unwrap();
    proxy::add_stdio_with(&id, spec_for(script("fake.sh", FAKE)), vec![], "test", Duration::from_secs(20)).await.unwrap();
    id
}

/// One downstream JSON-RPC request, as an agent's runtime sends it, on session `sid`.
async fn post(id: &str, sid: &str, rid: Value, method: &str, params: Value) -> Value {
    let body = json!({ "jsonrpc": "2.0", "id": rid, "method": method, "params": params }).to_string();
    let r = proxy::downstream_post(&entries::entry_name(id), Some(sid), None, &body).await;
    serde_json::from_str(&r.body.expect("a body")).unwrap()
}

async fn call_text(id: &str, tool: &str) -> String {
    let v = post(id, "s1", json!(1), "tools/call", json!({ "name": tool, "arguments": {} })).await;
    v["result"]["content"][0]["text"].as_str().unwrap_or_default().to_string()
}

fn tool_names(v: &Value) -> Vec<String> {
    v["result"]["tools"].as_array().into_iter().flatten().filter_map(|t| t["name"].as_str().map(str::to_string)).collect()
}

async fn eventually(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for {what}");
}

// ─── The record ─────────────────────────────────────────────────────────────

/// An http record written by today's build reads and writes back byte-for-byte the same JSON:
/// the stdio arm adds nothing to it.
#[test]
fn an_http_record_round_trips_unchanged() {
    let stored = json!({
        "id": "linear", "url": "https://mcp.linear.app/mcp", "auth": "oauth", "enabled": true,
        "workspaces": ["w1"], "catalog": { "tools": [{ "name": "a" }], "server_info": {}, "fetched_at": 4 },
        "oauth": { "issuer": "x" }, "added_at": 9, "added_by": "human",
    });
    let read: ProxyServer = serde_json::from_value(stored.clone()).unwrap();
    assert!(!read.is_stdio());
    assert_eq!(read.route(), None);
    assert_eq!(serde_json::to_value(&read).unwrap(), stored);
}

#[test]
fn a_stdio_record_round_trips_and_holds_no_key() {
    let spec = spec_for("/x/fake.sh".into());
    let s = ProxyServer {
        id: "fake".into(),
        url: spec.url(),
        auth: AUTH_KEY.into(),
        enabled: true,
        workspaces: vec![],
        catalog: None,
        oauth: None,
        added_at: 1,
        added_by: "human".into(),
        transport: TRANSPORT_STDIO.into(),
        stdio: Some(spec.clone()),
    };
    let v = serde_json::to_value(&s).unwrap();
    assert_eq!(v["transport"], "stdio");
    assert_eq!(v["url"], "stdio:test/fake", "an older build reads a url it never dials");
    assert_eq!(v["stdio"]["env_key"], KEY_VAR);
    assert_eq!(v["stdio"]["env"]["FAKE_MCP_OUT"], "{output}", "the placeholder is saved, not the folder");
    let bare = StdioSpec { env: Default::default(), ..spec.clone() };
    assert!(serde_json::to_value(&bare).unwrap().get("env").is_none(), "no settings, no field");
    let back: ProxyServer = serde_json::from_value(v.clone()).unwrap();
    assert!(back.is_stdio());
    assert_eq!(back.stdio, Some(spec));
    assert_eq!(back.route(), Some("test/fake"));
    assert!(!format!("{s:?}").contains(KEY));
    assert!(!v.to_string().contains(KEY));
}

/// Recognition reads a stdio server by its route (`upstream_of`), never by the url it stores; an
/// http record whose url only looks like a route is nothing.
#[test]
fn recognition_reads_a_stdio_server_by_its_route() {
    let spec = StdioSpec { route: "meshy".into(), ..spec_for("/x/fake.sh".into()) };
    let mut s = ProxyServer {
        id: "3d".into(),
        url: "stdio:something-else".into(),
        auth: AUTH_KEY.into(),
        enabled: true,
        workspaces: vec![],
        catalog: None,
        oauth: None,
        added_at: 1,
        added_by: "human".into(),
        transport: TRANSPORT_STDIO.into(),
        stdio: Some(spec),
    };
    assert_eq!(s.upstream_of(), "stdio:meshy", "from route(), not the stored url");
    s.stdio = None;
    assert_eq!(s.upstream_of(), "", "a stdio record with no spec is nothing");
    s.transport = TRANSPORT_HTTP.into();
    assert_eq!(s.upstream_of(), "", "an http record that looks like a route is nothing");
    s.url = "https://mcp.fal.ai/mcp-relay".into();
    assert_eq!(s.upstream_of(), "https://mcp.fal.ai/mcp-relay");
}

/// The setup sheet's add names a catalogue route; the command line is made here, from the
/// catalogue, and validated.
#[test]
fn a_route_id_resolves_to_the_catalogues_command_and_nothing_else() {
    let meshy = proxy::spec_for_route("meshy").unwrap();
    assert_eq!((meshy.route.as_str(), meshy.command.as_str()), ("meshy", "npx"));
    assert_eq!(meshy.args, ["-y", "@meshy-ai/meshy-mcp-server@0.6.7"]);
    assert_eq!(meshy.env_key.as_deref(), Some("MESHY_API_KEY"));
    let mm = proxy::spec_for_route("minimax-mainland").unwrap();
    assert_eq!(mm.env.get("MINIMAX_API_HOST").map(String::as_str), Some("https://api.minimaxi.com"));
    meshy.validate().unwrap();
    mm.validate().unwrap();
    for bad in ["", "fal", "elevenlabs", "npx -y evil", "../meshy", "meshy;rm"] {
        assert!(proxy::spec_for_route(bad).is_err(), "{bad:?}");
    }
}

/// The command refuses an unknown route before anything is stored or started.
#[tokio::test]
async fn the_add_command_refuses_a_route_the_catalogue_doesnt_have() {
    let id = fresh_id("noroute");
    let err = proxy::mcp_stdio_server_add(id.clone(), "npx".into(), vec![]).await.err().unwrap();
    assert!(err.contains("No local MCP server route named npx"), "{err}");
    assert!(proxy::get(&id).is_none());
}

#[test]
fn a_spec_is_checked_before_anything_runs() {
    let ok = spec_for("/x".into());
    assert!(ok.validate().is_ok());
    let with = |f: &dyn Fn(&mut StdioSpec)| {
        let mut s = ok.clone();
        f(&mut s);
        s.validate()
    };
    assert!(with(&|s| s.command = " ".into()).is_err());
    assert!(with(&|s| s.route = "has space".into()).is_err());
    assert!(with(&|s| s.args.push("a\nb".into())).is_err());
    // A key variable is named like one, and is none of the variables that steer the child.
    for good in ["MINIMAX_API_KEY", "MESHY_API_KEY", "FAKE_MCP_KEY", "A_TOKEN", "X1_SECRET"] {
        assert!(with(&|s| s.env_key = Some(good.into())).is_ok(), "{good}");
    }
    let steering = [
        "PATH", "HOME", "TMPDIR", "NODE_OPTIONS", "NODE_PATH", "NODE_EXTRA_CA_CERTS", "PYTHONPATH",
        "PYTHONHOME", "PYTHONSTARTUP", "BASH_ENV", "ENV", "PERL5OPT", "RUBYOPT", "JAVA_TOOL_OPTIONS",
        "SSL_CERT_FILE", "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NPM_CONFIG_REGISTRY", "UV_INDEX_URL",
        "PIP_INDEX_URL", "DYLD_INSERT_LIBRARIES", "LD_PRELOAD",
    ];
    // Shaped like keys, but read by npm, uv or node themselves: refused by the denylist.
    let keyish_steering = ["NODE_AUTH_TOKEN", "NPM_CONFIG__AUTH_TOKEN", "UV_PUBLISH_TOKEN", "PIP_KEY", "DYLD_KEY"];
    for bad in steering.iter().chain(&keyish_steering).chain(&["MINIMAX_API_HOST", "_KEY", "KEY", "lower_key", "1X_KEY", ""]) {
        assert!(with(&|s| s.env_key = Some(bad.to_string())).is_err(), "key {bad}");
    }
    assert!(with(&|s| s.env_key = None).is_ok());
    // A fixed setting is none of those either, nor the key's own variable.
    for bad in steering.iter().chain(&["npm_config_registry", "Http_Proxy", KEY_VAR, "has space", ""]) {
        assert!(with(&|s| { s.env.insert(bad.to_string(), "x".into()); }).is_err(), "setting {bad}");
    }
    assert!(with(&|s| { s.env.insert("MINIMAX_API_HOST".into(), "a\nb".into()); }).is_err(), "a line break in a value");
    assert!(with(&|s| { s.env.insert("MINIMAX_MCP_BASE_PATH".into(), "{output}".into()); }).is_ok());
}

#[test]
fn the_output_folder_is_outside_maiterms_data_folder() {
    let out = output_folder("minimax").unwrap();
    assert!(out.ends_with("mcp-out/minimax"));
    if let Some(data) = crate::state::persistence::app_data_dir() {
        assert!(!out.starts_with(&data));
    }
}

// ─── Verify before write ────────────────────────────────────────────────────

#[tokio::test]
async fn verify_reads_the_first_list_before_anything_is_written() {
    let id = added("v").await;
    let s = proxy::get(&id).expect("saved after its first list");
    let names: Vec<_> = s.catalog.as_ref().unwrap().tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["fast", "slow"]);
    assert_eq!(s.catalog.as_ref().unwrap().instructions.as_deref(), Some("a fake"));
    assert_eq!(proxy::status(&id).state, "connected");
    // Saved, and the entry is in the set the agents' configs get.
    let file = std::fs::read_to_string(proxy::store_path().unwrap()).unwrap();
    assert!(file.contains(&format!("\"{id}\"")));
    let remote = entries::remote();
    assert!(remote["set"].get(entries::entry_name(&id)).is_some(), "{remote}");
    // The downstream list is the verified one.
    let v = post(&id, "s1", json!(7), "tools/list", json!({})).await;
    assert_eq!(v["id"], 7);
    assert_eq!(tool_names(&v), ["fast", "slow"]);
    proxy::remove(&id).unwrap();
}

#[tokio::test]
async fn a_failed_verify_writes_nothing_and_leaves_the_key_for_a_retry() {
    capture_logs();
    for (name, body) in [("exits.sh", "exit 2\n"), ("mute.sh", "while IFS= read -r l; do :; done\n")] {
        let id = fresh_id("f");
        proxy::set_key(&id, KEY).unwrap();
        let err = proxy::add_stdio_with(&id, spec_for(script(name, body)), vec![], "test", Duration::from_secs(2)).await.unwrap_err();
        assert!(err.contains("could not set up"), "{err}");
        assert!(!err.contains(KEY));
        assert!(proxy::get(&id).is_none(), "{name}: not added");
        assert!(running(&id).is_none(), "{name}: its child is gone");
        let file = std::fs::read_to_string(proxy::store_path().unwrap()).unwrap_or_default();
        assert!(!file.contains(&id), "{name}: not saved");
        assert!(entries::remote()["set"].get(entries::entry_name(&id)).is_none(), "{name}: no entry");
        assert!(crate::mcp_proxy::secrets::read_env_key(&id).unwrap().is_some(), "{name}: the key stays for a retry");
        proxy::delete_key(&id).unwrap();
        assert!(crate::mcp_proxy::secrets::read_env_key(&id).unwrap().is_none());
    }
}

#[tokio::test]
async fn a_server_that_needs_a_key_is_not_started_without_one() {
    let id = fresh_id("nokey");
    let err = proxy::add_stdio_with(&id, spec_for(script("fake.sh", FAKE)), vec![], "test", Duration::from_secs(5)).await.unwrap_err();
    assert!(err.contains("key first"), "{err}");
    assert!(running(&id).is_none());
}

/// A key too short to be a whole one is refused before it is stored, so nothing starts with it.
/// Were one there, taking it out of what the child sends would mangle every answer.
#[tokio::test]
async fn a_short_key_is_refused_and_nothing_starts() {
    let id = fresh_id("short");
    let err = proxy::set_key(&id, "ul").unwrap_err();
    assert!(err.contains("whole key") && !err.contains("\"ul\""), "{err}");
    assert!(proxy::set_key(&id, "VALUE-15-chars!").is_err(), "15 characters");
    assert!(crate::mcp_proxy::secrets::read_env_key(&id).unwrap().is_none(), "nothing stored");
    let err = proxy::add_stdio_with(&id, spec_for(script("fake.sh", FAKE)), vec![], "test", Duration::from_secs(5)).await.unwrap_err();
    assert!(err.contains("key first"), "{err}");
    assert!(running(&id).is_none() && proxy::get(&id).is_none(), "nothing started");
    let mut v = json!({ "result": { "tools": [{ "name": "ul" }] } });
    let before = v.clone();
    scrub(&mut v, "ul");
    assert_eq!(v, before, "a short key changes nothing");
}

#[tokio::test]
async fn a_key_on_the_command_line_is_refused() {
    let id = fresh_id("argv");
    proxy::set_key(&id, KEY).unwrap();
    let mut spec = spec_for(script("fake.sh", FAKE));
    spec.args.push(format!("--key={KEY}"));
    let err = proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(5)).await.unwrap_err();
    assert!(!err.contains(KEY), "{err}");
    assert!(running(&id).is_none());
    let mut spec = spec_for(script("fake.sh", FAKE));
    spec.env.insert("FAKE_MCP_HOST".into(), format!("https://{KEY}@api.fake.test"));
    let err = proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(5)).await.unwrap_err();
    assert!(err.contains("settings") && !err.contains(KEY), "{err}");
    assert!(running(&id).is_none());
    proxy::delete_key(&id).unwrap();
}

// ─── The child ──────────────────────────────────────────────────────────────

/// Two agents' calls in flight at once, both with JSON-RPC id 1: the slow one is answered last,
/// and each answer reaches its own caller under its own id.
#[tokio::test]
async fn concurrent_calls_from_two_agents_never_cross() {
    let id = added("ids").await;
    let slow = post(&id, "agent-a", json!(1), "tools/call", json!({ "name": "slow", "arguments": {} }));
    let fast = async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        post(&id, "agent-b", json!(1), "tools/call", json!({ "name": "fast", "arguments": {} })).await
    };
    let (slow, fast) = tokio::join!(slow, fast);
    assert_eq!(slow["id"], 1);
    assert_eq!(fast["id"], 1);
    assert_eq!(slow["result"]["content"][0]["text"], "slow");
    assert_eq!(fast["result"]["content"][0]["text"], "fast");
    let s = post(&id, "agent-a", json!("str-id"), "tools/call", json!({ "name": "fast", "arguments": {} })).await;
    assert_eq!(s["id"], "str-id", "the agent's own id comes back, whatever its type");
    proxy::remove(&id).unwrap();
}

/// The login PATH, the key, the route's fixed settings (`{output}` expanded), and nothing else.
#[tokio::test]
async fn the_child_gets_the_login_path_its_key_and_its_settings_and_nothing_else() {
    let id = added("env").await;
    let text = call_text(&id, "env").await;
    let (names, cwd) = text.split_once('|').unwrap();
    let names: Vec<&str> = names.split_whitespace().collect();
    let given = ["PATH", KEY_VAR, "FAKE_MCP_HOST", "FAKE_MCP_OUT"];
    for g in given {
        assert!(names.contains(&g), "{g} missing: {names:?}");
    }
    // What sh itself sets is all there is beyond those.
    for n in &names {
        assert!(given.contains(n) || ["PWD", "SHLVL", "_", "OLDPWD"].contains(n), "maiTerm's environment leaked: {n}");
    }
    let want = output_folder(&id).unwrap();
    assert_eq!(std::fs::canonicalize(cwd).unwrap(), std::fs::canonicalize(&want).unwrap());
    let settings = call_text(&id, "settings").await;
    assert_eq!(settings, format!("{HOST}|{}", want.display()), "the placeholder is the output folder");
    proxy::remove(&id).unwrap();
}

/// The key is in the child's environment and nowhere else: not its argv, not a log line (its
/// stderr printed it), not the record's `Debug`, the saved file, the view or the entries.
#[tokio::test]
async fn the_key_appears_nowhere_but_the_childs_environment() {
    let id = added("key").await;
    let argv = call_text(&id, "argv").await;
    assert!(argv.contains("fake.sh") && !argv.contains(KEY), "{argv}");
    let server = proxy::get(&id).unwrap();
    let plan = plan(&server).await.unwrap();
    assert!(!plan.program.contains(KEY) && !plan.args.iter().any(|a| a.contains(KEY)));
    assert_eq!(plan.key.as_ref().map(|(_, k)| k.expose()), Some(KEY), "the plan does carry it, in the env");
    assert!(!format!("{plan:?}").contains(KEY));
    assert!(!format!("{server:?}").contains(KEY));
    assert!(!serde_json::to_string(&proxy::view(&server)).unwrap().contains(KEY));
    assert!(!std::fs::read_to_string(proxy::store_path().unwrap()).unwrap().contains(KEY));
    assert!(!entries::remote().to_string().contains(KEY));
    assert!(!entries::rendered_for_test(9, "tok", &id).contains(KEY));
    // Its exit is logged as a count of stderr lines and a status, never their words.
    let _ = call_text(&id, "die").await;
    eventually("the exit log line", || logs().contains(&format!("{id}: stdio server ended (exit 3); 2 stderr lines"))).await;
    assert!(!logs().contains(KEY), "the key reached a log line");
    assert!(!logs().contains("my key is"), "stderr reached a log line");
    assert!(!proxy::status(&id).detail.contains(KEY));
    proxy::remove(&id).unwrap();
}

#[tokio::test]
async fn a_servers_own_error_goes_back_to_the_caller_as_sent() {
    let id = added("rpc").await;
    let v = post(&id, "s1", json!(3), "tools/call", json!({ "name": "bad", "arguments": {} })).await;
    assert_eq!(v["error"]["code"], -32602);
    assert_eq!(v["error"]["message"], "bad arguments");
    proxy::remove(&id).unwrap();
}

#[tokio::test]
async fn a_list_change_the_child_announces_is_passed_out() {
    let id = added("lc").await;
    let mut stream = proxy::downstream_stream(&entries::entry_name(&id)).unwrap();
    assert_eq!(call_text(&id, "grow").await, "grown");
    let note = tokio::time::timeout(Duration::from_secs(10), stream.recv()).await.expect("a notice").unwrap();
    assert!(note.contains("notifications/tools/list_changed"), "{note}");
    let v = post(&id, "s1", json!(1), "tools/list", json!({})).await;
    assert_eq!(tool_names(&v), ["fast", "slow", "extra"]);
    proxy::remove(&id).unwrap();
}

// ─── Lifecycle ──────────────────────────────────────────────────────────────

/// Unused for `IDLE`, the child stops; the server is idle (no outage), lists its saved tools
/// without starting, and a tool call starts it again.
#[tokio::test]
async fn an_idle_server_stops_answers_from_its_saved_list_and_a_call_starts_it() {
    let id = added("idle").await;
    let pid = call_text(&id, "pid").await;
    age_for_test(&id, IDLE + Duration::from_secs(1));
    sweep(&id);
    assert!(running(&id).is_none(), "stopped");
    assert_eq!(proxy::status(&id).state, STATE_IDLE);
    assert!(!proxy::status(&id).is_outage());
    let v = post(&id, "s1", json!(2), "tools/list", json!({})).await;
    assert_eq!(tool_names(&v), ["fast", "slow"], "the saved list");
    assert!(running(&id).is_none(), "an agent's list never starts it");
    assert!(proxy::refresh_stdio(&id, false).await.is_err(), "the hourly re-list doesn't start it");
    assert!(running(&id).is_none());
    assert_eq!(call_text(&id, "fast").await, "fast", "a call starts it");
    assert!(running(&id).is_some());
    assert_ne!(call_text(&id, "pid").await, pid, "a new child");
    assert_eq!(proxy::status(&id).state, "connected");
    proxy::remove(&id).unwrap();
}

/// An agent's list keeps a child alive; the proxy's own re-list doesn't.
#[tokio::test]
async fn only_an_agents_list_or_call_counts_as_use() {
    let id = added("use").await;
    age_for_test(&id, IDLE + Duration::from_secs(1));
    let aged = last_use_for_test(&id);
    assert!(proxy::refresh_stdio(&id, false).await.is_ok(), "the re-list reads the running child");
    assert_eq!(last_use_for_test(&id), aged, "and is not use");
    let _ = post(&id, "s1", json!(1), "tools/list", json!({})).await;
    assert!(last_use_for_test(&id) > aged, "an agent's list is");
    sweep(&id);
    assert!(running(&id).is_some(), "used just now");
    age_for_test(&id, IDLE + Duration::from_secs(1));
    sweep(&id);
    assert!(running(&id).is_none());
    proxy::remove(&id).unwrap();
}

/// The child dies mid-call: the call answers `isError` with a sentence, every tool stays listed,
/// and the child is started again.
#[tokio::test]
async fn a_child_that_exits_is_restarted_and_no_tool_is_removed() {
    let id = added("rs").await;
    let pid = call_text(&id, "pid").await;
    let v = post(&id, "s1", json!(4), "tools/call", json!({ "name": "die", "arguments": {} })).await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    let text = v["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains(&id) && text.contains("Its tools stay listed"), "{text}");
    let listed = post(&id, "s1", json!(5), "tools/list", json!({})).await;
    assert_eq!(tool_names(&listed), ["fast", "slow"]);
    eventually("the restart", || running(&id).is_some()).await;
    let again = call_text(&id, "pid").await;
    assert!(!again.is_empty() && again != pid, "a new child: {again} vs {pid}");
    proxy::remove(&id).unwrap();
}

#[cfg(unix)]
#[tokio::test]
async fn stopping_kills_the_whole_process_group() {
    let id = added("pg").await;
    let pid: i32 = call_text(&id, "pid").await.parse().unwrap();
    // A grandchild of the server, in its group: the slow tool's subshell.
    let slow = tokio::spawn({
        let id = id.clone();
        async move { post(&id, "s1", json!(9), "tools/call", json!({ "name": "slow", "arguments": {} })).await }
    });
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(stop(&id));
    // SAFETY: probing a pid with signal 0 sends nothing.
    eventually("the child to be gone", || unsafe { libc::kill(pid, 0) } != 0).await;
    let v = slow.await.unwrap();
    assert_eq!(v["result"]["isError"], true, "the call in flight is told: {v}");
    // SAFETY: as above, for the group.
    eventually("its group to be gone", || unsafe { libc::killpg(pid, 0) } != 0).await;
    proxy::remove(&id).unwrap();
}

#[tokio::test]
async fn removing_the_server_stops_its_child_deletes_its_key_and_its_entry() {
    let id = added("rm").await;
    assert!(running(&id).is_some());
    proxy::remove(&id).unwrap();
    assert!(running(&id).is_none());
    assert!(proxy::get(&id).is_none());
    assert!(crate::mcp_proxy::secrets::read_env_key(&id).unwrap().is_none(), "its key is deleted");
    assert!(entries::remote()["set"].get(entries::entry_name(&id)).is_none());
    let v = post(&id, "s1", json!(1), "tools/list", json!({})).await;
    assert_eq!(tool_names(&v), Vec::<String>::new(), "an entry left on an ssh host is an empty server");
}

#[tokio::test]
async fn a_key_belongs_only_to_a_stdio_server_that_takes_one() {
    let id = added("k").await;
    assert!(proxy::delete_key(&id).is_err(), "a server using its key keeps it");
    // Replacing it stops the child, so the next start has the new one.
    proxy::set_key(&id, "VALUE-another-key-5e21").unwrap();
    assert!(running(&id).is_none());
    assert!(proxy::set_token(&id, "x").is_err(), "not a token server");
    proxy::remove(&id).unwrap();
    let http = fresh_id("h");
    proxy::insert_for_test(ProxyServer {
        id: http.clone(),
        url: "http://127.0.0.1:1/mcp".into(),
        auth: proxy::AUTH_TOKEN.into(),
        enabled: true,
        workspaces: vec![],
        catalog: None,
        oauth: None,
        added_at: 0,
        added_by: "test".into(),
        transport: TRANSPORT_HTTP.into(),
        stdio: None,
    });
    assert!(proxy::set_key(&http, KEY).is_err(), "an http server takes no key");
    proxy::SERVERS.write().retain(|s| s.id != http);
}

/// A restart is not use: a child that keeps failing is restarted only until `IDLE` after an
/// agent last used it, not for as long as the app runs.
#[tokio::test]
async fn a_restart_is_not_counted_as_use() {
    let id = added("ru").await;
    age_for_test(&id, IDLE - Duration::from_secs(120));
    let aged = last_use_for_test(&id);
    let old = running(&id).unwrap();
    // Straight to the child, as no agent: its exit is what restarts it.
    let _ = old.call_tool(json!({ "name": "die", "arguments": {} })).await;
    eventually("the restart", || running(&id).is_some_and(|r| !Arc::ptr_eq(&r, &old))).await;
    assert_eq!(last_use_for_test(&id), aged, "the restart moved the idle clock");
    proxy::remove(&id).unwrap();
}

/// A caller that gives up (an agent that disconnects drops its call) leaves nothing waiting, so
/// the idle stop still stops the child.
#[tokio::test]
async fn a_dropped_call_leaves_nothing_waiting() {
    let id = added("drop").await;
    let r = running(&id).unwrap();
    let call = r.call_tool(json!({ "name": "hang", "arguments": {} }));
    assert!(tokio::time::timeout(Duration::from_millis(300), call).await.is_err(), "it never answers");
    assert!(r.pending.lock().is_empty(), "a dropped call is not waited for");
    age_for_test(&id, IDLE + Duration::from_secs(1));
    sweep(&id);
    assert!(running(&id).is_none(), "stopped when idle");
    proxy::remove(&id).unwrap();
}

/// The key a child echoes back (in an error, a result, a tool's description) is taken out of
/// everything it sends: what Verify reads, what an agent gets, the saved list, the file.
#[tokio::test]
async fn a_key_the_child_echoes_is_taken_out_of_everything_it_sends() {
    let id = added("echo").await;
    // Verify's door (`call_once`): its words reach the webview, so only the code goes out.
    let e = proxy::call_once(&id, json!({ "name": "leak", "arguments": {} })).await.unwrap_err();
    let words = format!("{e} {e:?}");
    assert!(!words.contains(KEY) && !words.contains("invalid api key"), "{words}");
    assert!(matches!(e, UpErr::Rpc { code: -32000, .. }), "{words}");
    let v = proxy::call_once(&id, json!({ "name": "leakresult", "arguments": {} })).await.unwrap();
    assert!(!v.to_string().contains(KEY) && v.to_string().contains(KEY_REMOVED), "{v}");
    // The agent that called gets the server's own words, without the key.
    let v = post(&id, "s1", json!(2), "tools/call", json!({ "name": "leak", "arguments": {} })).await;
    assert_eq!(v["error"]["code"], -32000);
    assert!(v["error"]["message"].as_str().unwrap().contains("invalid api key"), "{v}");
    assert!(!v.to_string().contains(KEY), "{v}");
    let v = post(&id, "s1", json!(3), "tools/call", json!({ "name": "leakresult", "arguments": {} })).await;
    assert!(!v.to_string().contains(KEY), "{v}");
    // A description that held it is listed and saved without it.
    let listed = post(&id, "s1", json!(4), "tools/list", json!({})).await;
    assert!(!listed.to_string().contains(KEY) && listed.to_string().contains(KEY_REMOVED), "{listed}");
    assert!(!std::fs::read_to_string(proxy::store_path().unwrap()).unwrap().contains(KEY));
    assert!(!proxy::status(&id).detail.contains(KEY));
    assert!(!logs().contains(KEY));
    proxy::remove(&id).unwrap();
}

/// Stop `id`'s child, start it again from an agent's call with a 2 s handshake, queue a second
/// agent's call behind that start, and run `act` half a second into it. Both calls must fail and
/// no other child may start: the first call's answer and the starting child's pid.
async fn mid_start(id: &str, act: impl FnOnce()) -> (Value, i32) {
    let dir = output_folder(id).unwrap();
    stop(id);
    let pidfile = dir.join("pid.txt");
    let _ = std::fs::remove_file(&pidfile);
    std::fs::write(dir.join("slowstart"), "").unwrap();
    let call = tokio::spawn({
        let id = id.to_string();
        async move { post(&id, "s1", json!(1), "tools/call", json!({ "name": "fast", "arguments": {} })).await }
    });
    eventually("the child to start", || std::fs::read_to_string(&pidfile).is_ok_and(|p| p.ends_with('\n'))).await;
    let pid: i32 = std::fs::read_to_string(&pidfile).unwrap().trim().parse().unwrap();
    // Accepted while the server was still up, waiting behind the first call's start.
    let queued = tokio::spawn({
        let id = id.to_string();
        async move { post(&id, "s2", json!(2), "tools/call", json!({ "name": "pid", "arguments": {} })).await }
    });
    tokio::time::sleep(Duration::from_millis(500)).await;
    act();
    let queued = queued.await.unwrap();
    let first = call.await.unwrap();
    assert_eq!(queued["result"]["isError"], true, "the queued call started a child: {queued}");
    assert!(running(id).is_none(), "a child is running after the stop");
    assert_eq!(std::fs::read_to_string(&pidfile).unwrap().trim(), pid.to_string(), "another child was started");
    let _ = std::fs::remove_file(dir.join("slowstart"));
    (first, pid)
}

fn gone(pid: i32) -> bool {
    // SAFETY: probing a pid with signal 0 sends nothing.
    unsafe { libc::kill(pid, 0) != 0 }
}

/// A removal, a new key or a disable while an agent's call is starting the child: the child that
/// was starting is killed, never kept (with the old key, or for a server that is gone).
#[cfg(unix)]
#[tokio::test]
async fn a_stop_during_a_start_leaves_no_child() {
    let id = added("orph").await;
    let (v, pid) = mid_start(&id, || proxy::remove(&id).unwrap()).await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    eventually("the removed server's child to be gone", || gone(pid)).await;
    assert!(running(&id).is_none());
    assert!(!SLOTS.lock().contains_key(&id), "no slot left for it");

    let id = added("rekey").await;
    let (v, pid) = mid_start(&id, || proxy::set_key(&id, "VALUE-a-new-key-77c0a1").unwrap()).await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    eventually("the child with the old key to be gone", || gone(pid)).await;
    assert!(running(&id).is_none());
    let next: i32 = call_text(&id, "pid").await.parse().unwrap();
    assert_ne!(next, pid, "the next call starts a new child");
    proxy::remove(&id).unwrap();

    let id = added("off").await;
    let (v, pid) = mid_start(&id, || {
        proxy::set_enabled(&id, false).unwrap();
    })
    .await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    eventually("the disabled server's child to be gone", || gone(pid)).await;
    assert!(running(&id).is_none());
    proxy::remove(&id).unwrap();
}

/// At the app's quit every child goes, the running and the starting alike: each is in a process
/// group of its own, which nothing else ends. (`stop_all` is this over every server; a test that
/// ran it would stop the other tests' children.)
#[cfg(unix)]
#[tokio::test]
async fn quitting_stops_every_child_running_or_starting() {
    let up = added("quit").await;
    let up_pid: i32 = call_text(&up, "pid").await.parse().unwrap();
    let starting = added("quit").await;
    let (v, starting_pid) = mid_start(&starting, || {
        assert_eq!(stop_for_test(&[up.as_str(), starting.as_str()]), 2);
    })
    .await;
    assert_eq!(v["result"]["isError"], true, "{v}");
    eventually("the running child to be gone", || gone(up_pid)).await;
    eventually("the starting child to be gone", || gone(starting_pid)).await;
    assert!(running(&up).is_none() && running(&starting).is_none());
    proxy::remove(&up).unwrap();
    proxy::remove(&starting).unwrap();
}

// ─── Argument rules: a path stays inside the output folder ──────────────────

fn rule(tool: &str, arg: &str, rule: &str) -> ArgRule {
    ArgRule { tool: tool.into(), arg: arg.into(), rule: rule.into(), head: 0, values: vec![] }
}

fn call(tool: &str, args: Value) -> Value {
    json!({ "name": tool, "arguments": args })
}

/// A refused argument never reaches the child: the fake's `record` tool writes every call it gets
/// to `calls.txt`, and only the allowed ones are there.
#[tokio::test]
async fn a_refused_argument_never_reaches_the_child() {
    capture_logs();
    let id = fresh_id("rules");
    proxy::set_key(&id, KEY).unwrap();
    let rules = vec![
        rule("", "output_directory", RULE_RELATIVE_DIR),
        rule("", "is_url", RULE_NOT_TRUE),
        rule("record", "file", RULE_INSIDE_OUTPUT),
        rule("record", "files", RULE_INSIDE_OUTPUT),
        rule("record", "first_frame_image", RULE_URL_OR_INSIDE_OUTPUT),
        rule("die", "", RULE_REFUSE_TOOL),
    ];
    let spec = StdioSpec { arg_rules: rules, ..spec_for(script("fake.sh", FAKE)) };
    proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(20)).await.unwrap();
    let out = output_folder(&id).unwrap();
    let outside = std::env::temp_dir().join(format!("maiterm-test-outside-{id}"));
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, out.join("escape")).unwrap();
    std::fs::write(out.join("in.wav"), b"x").unwrap();
    let inside_abs = out.join("in.wav").to_string_lossy().to_string();

    let refused = [
        json!({ "output_directory": "/tmp/x" }),
        json!({ "output_directory": "~/Desktop" }),
        json!({ "output_directory": "a/../../x" }),
        json!({ "is_url": true }),
        json!({ "is_url": "true" }),
        json!({ "file": "/etc/passwd" }),
        json!({ "file": "../../x.wav" }),
        json!({ "file": "~/.zshrc" }),
        json!({ "file": "escape/secret.wav" }),
        json!({ "file": 7 }),
        json!({ "files": ["in.wav", "/etc/hosts"] }),
        json!({ "first_frame_image": "/etc/hosts" }),
    ];
    for args in &refused {
        let v = post(&id, "s1", json!(1), "tools/call", call("record", args.clone())).await;
        assert_eq!(v["result"]["isError"], true, "{args}: {v}");
        assert!(v["result"]["content"][0]["text"].as_str().unwrap().starts_with("maiTerm refused record's"), "{v}");
    }
    let pid = call_text(&id, "pid").await;
    let v = post(&id, "s1", json!(2), "tools/call", call("die", json!({}))).await;
    assert!(v["result"]["content"][0]["text"].as_str().unwrap().contains("doesn't let agents call die"), "{v}");
    assert_eq!(call_text(&id, "pid").await, pid, "the refused tool never ran: the child is the same one");

    let allowed = [
        json!({}),
        json!({ "output_directory": "takes/one", "is_url": false, "file": null }),
        json!({ "file": "in.wav" }),
        json!({ "file": inside_abs }),
        json!({ "file": "new/sub/out.glb" }),
        json!({ "files": ["in.wav", "new/b.png"] }),
        json!({ "first_frame_image": "https://cdn.example.com/a.png" }),
    ];
    for args in &allowed {
        let v = post(&id, "s1", json!(3), "tools/call", call("record", args.clone())).await;
        assert_eq!(v["result"]["content"][0]["text"], "recorded", "{args}: {v}");
    }
    let seen = std::fs::read_to_string(out.join("calls.txt")).unwrap();
    assert_eq!(seen.lines().count(), allowed.len(), "only the allowed calls reached the child:\n{seen}");
    for bad in ["/etc/passwd", "/tmp/x", "~/", "escape", "/etc/hosts", "\"is_url\":true"] {
        assert!(!seen.contains(bad), "{bad} reached the child:\n{seen}");
    }
    proxy::remove(&id).unwrap();
    let _ = std::fs::remove_dir_all(&outside);
    let _ = std::fs::remove_dir_all(&out);
}

/// Verify's own call (`call_once`) is held to the same rules.
#[tokio::test]
async fn a_call_maiterm_makes_itself_is_held_to_the_rules_too() {
    capture_logs();
    let id = fresh_id("rules-once");
    proxy::set_key(&id, KEY).unwrap();
    let spec = StdioSpec { arg_rules: vec![rule("", "save_to", RULE_INSIDE_OUTPUT)], ..spec_for(script("fake.sh", FAKE)) };
    proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(20)).await.unwrap();
    let err = proxy::call_once(&id, call("record", json!({ "save_to": "/etc/x" }))).await.unwrap_err();
    assert!(matches!(err, crate::mcp_proxy::upstream::UpErr::Rpc { code: -32602, .. }), "{err:?}");
    assert!(!output_folder(&id).unwrap().join("calls.txt").exists(), "nothing reached the child");
    proxy::remove(&id).unwrap();
    let _ = std::fs::remove_dir_all(output_folder(&id).unwrap());
}

/// The catalogue's own rules for MiniMax and Meshy, checked without starting anything.
#[test]
fn the_catalogue_rules_keep_minimax_and_meshy_inside_their_folders() {
    let id = fresh_id("cat-rules");
    let rules_of = |route: &str| proxy::spec_for_route(route).unwrap().arg_rules;
    let (mm, meshy) = (rules_of("minimax"), rules_of("meshy"));
    assert_eq!(rules_of("minimax-mainland"), mm);
    let refused = |rules: &[ArgRule], tool: &str, args: Value| refuse_args(&id, rules, &call(tool, args));
    // MiniMax.
    assert!(refused(&mm, "play_audio", json!({ "input_file_path": "x.mp3" })).is_some(), "play_audio is never called");
    assert!(refused(&mm, "text_to_audio", json!({ "text": "hi", "output_directory": "/Users/x/Desktop" })).is_some());
    assert!(refused(&mm, "text_to_image", json!({ "output_directory": "../.." })).is_some());
    assert!(refused(&mm, "voice_clone", json!({ "voice_id": "v", "file": "https://127.0.0.1:8765/x", "is_url": true })).is_some());
    assert!(refused(&mm, "voice_clone", json!({ "voice_id": "v", "file": "/Users/x/voice.wav" })).is_some());
    assert!(refused(&mm, "generate_video", json!({ "prompt": "p", "first_frame_image": "/Users/x/a.jpg" })).is_some());
    assert!(refused(&mm, "generate_video", json!({ "prompt": "p", "first_frame_image": "https://x.example/a.jpg" })).is_none());
    assert!(refused(&mm, "text_to_audio", json!({ "text": "hi", "output_directory": "scenes/1" })).is_none());
    assert!(refused(&mm, "list_voices", json!({})).is_none());
    // Meshy.
    assert!(refused(&meshy, "meshy_download_model", json!({ "task_id": "t", "save_to": "/Users/x/.zshrc" })).is_some());
    assert!(refused(&meshy, "meshy_image_to_3d", json!({ "file_path": "/Users/x/Pictures/a.png" })).is_some());
    assert!(refused(&meshy, "meshy_multi_image_to_3d", json!({ "file_paths": ["a.png", "/etc/hosts"] })).is_some());
    assert!(refused(&meshy, "meshy_text_to_image", json!({ "reference_file_paths": ["~/a.png"] })).is_some());
    assert!(refused(&meshy, "meshy_download_model", json!({ "task_id": "t", "save_to": "models/a.glb" })).is_none());
    assert!(refused(&meshy, "meshy_check_balance", json!({})).is_none());
    // Meshy's host is pinned, so a `.env` in its folder can't send the key elsewhere.
    assert_eq!(proxy::spec_for_route("meshy").unwrap().env.get("MESHY_API_HOST").map(String::as_str), Some("https://api.meshy.ai"));
    let _ = std::fs::remove_dir_all(output_folder(&id).unwrap());
}

#[test]
fn an_argument_rule_is_checked_when_the_spec_is() {
    let ok = spec_for("/x".into());
    let with = |r: ArgRule| StdioSpec { arg_rules: vec![r], ..ok.clone() }.validate();
    assert!(with(rule("", "save_to", RULE_INSIDE_OUTPUT)).is_ok());
    assert!(with(rule("play_audio", "", RULE_REFUSE_TOOL)).is_ok());
    assert!(with(rule("", "save_to", "anywhere")).is_err(), "an unknown rule");
    assert!(with(rule("", "", RULE_REFUSE_TOOL)).is_err(), "refuse_tool names its tool");
    assert!(with(rule("t", "x", RULE_REFUSE_TOOL)).is_err(), "and no argument");
    assert!(with(rule("", "", RULE_INSIDE_OUTPUT)).is_err(), "a path rule names its argument");
}

/// A wrong-region MiniMax key is fixed by moving the same server to the other account's route:
/// one server per provider, its command made from the catalogue, its child stopped (and nothing
/// started here: the new route's command is a real package).
#[tokio::test]
async fn a_server_moves_to_another_route_of_its_own_provider_only() {
    capture_logs();
    let id = fresh_id("route");
    proxy::set_key(&id, KEY).unwrap();
    let spec = StdioSpec { route: "minimax".into(), ..spec_for(script("fake.sh", FAKE)) };
    proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(20)).await.unwrap();
    assert!(proxy::set_route(&id, "meshy").unwrap_err().contains("its own provider"));
    assert!(proxy::set_route(&id, "nope").unwrap_err().contains("No local MCP server route named nope"));
    assert!(proxy::set_route(&fresh_id("absent"), "minimax-mainland").is_err());
    let moved = proxy::set_route(&id, "minimax-mainland").unwrap();
    assert_eq!(moved.route(), Some("minimax-mainland"));
    assert_eq!(moved.url, "stdio:minimax-mainland");
    assert_eq!(moved.upstream_of(), "stdio:minimax-mainland");
    let spec = moved.stdio.clone().unwrap();
    assert_eq!(spec, proxy::spec_for_route("minimax-mainland").unwrap(), "the catalogue's command and settings, nothing else");
    assert_eq!(proxy::get(&id).unwrap().route(), Some("minimax-mainland"), "saved");
    assert_eq!(proxy::status(&id).state, STATE_IDLE, "stopped, not started: the next call starts it on the new route");
    assert!(running(&id).is_none(), "no child");
    proxy::remove(&id).unwrap();
}

// ─── File names built from text, a format, and saves that aren't a .env ─────

fn rule_with(tool: &str, arg: &str, r: &str, head: usize, values: &[&str]) -> ArgRule {
    ArgRule { tool: tool.into(), arg: arg.into(), rule: r.into(), head, values: values.iter().map(|v| v.to_string()).collect() }
}

/// MiniMax names a file `t2a_<text[:10]>_<date>.<format>` inside `output_directory`: a `/` in the
/// first ten characters, or a format that isn't one of its four, would walk out of the folder.
/// The reviewer's probes, each refused, through the catalogue's own rules.
#[test]
fn minimaxs_file_names_cant_walk_out_of_its_folder() {
    let id = fresh_id("names");
    let mm = proxy::spec_for_route("minimax").unwrap().arg_rules;
    assert_eq!(proxy::spec_for_route("minimax-mainland").unwrap().arg_rules, mm);
    let refused = |tool: &str, args: Value| refuse_args(&id, &mm, &call(tool, args));
    assert!(refused("text_to_audio", json!({ "text": "/../../../", "output_directory": "t2a_" })).is_some());
    assert!(refused("text_to_audio", json!({ "text": "ab\\..\\..\\x" })).is_some());
    assert!(refused("voice_design", json!({ "prompt": "p", "preview_text": "/../../../" })).is_some());
    assert!(refused("voice_clone", json!({ "voice_id": "v", "file": "in.wav", "text": "x/../../.." })).is_some());
    assert!(refused("text_to_image", json!({ "prompt": "/../../." })).is_some());
    assert!(refused("query_video_generation", json!({ "task_id": "123/../../../x" })).is_some());
    for bad in ["/../../x", "mp3/../..", "MP3", "exe", ""] {
        assert!(refused("text_to_audio", json!({ "text": "hello", "format": bad })).is_some(), "format {bad:?}");
    }
    // What a real job sends goes through, a `/` past the part that becomes the name included.
    for f in ["mp3", "pcm", "flac", "wav"] {
        assert!(refused("text_to_audio", json!({ "text": "Hello there, 1/2 of it", "format": f })).is_none(), "{f}");
    }
    assert!(refused("text_to_audio", json!({ "text": "Hello .. world" })).is_none(), "dots without a slash stay one name");
    assert!(refused("text_to_image", json!({ "prompt": "a red fox in snow, 35mm/f2" })).is_none());
    assert!(refused("query_video_generation", json!({ "task_id": "106916112212032" })).is_none());
    assert!(refused("text_to_audio", json!({ "text": 5 })).is_some(), "not text: refused");
    let _ = std::fs::remove_dir_all(output_folder(&id).unwrap());
}

/// Meshy's working folder is its output folder, where its dotenv reads a `.env` at every start
/// and it keeps its own index under `meshy_output/`: a save is neither.
#[test]
fn meshys_save_is_never_a_dotfile_or_its_own_index() {
    let id = fresh_id("saves");
    let meshy = proxy::spec_for_route("meshy").unwrap().arg_rules;
    let out = output_folder(&id).unwrap();
    std::fs::create_dir_all(out.join("meshy_output")).unwrap();
    let save = |to: &str| refuse_args(&id, &meshy, &call("meshy_download_model", json!({ "task_id": "t", "save_to": to })));
    let abs = |rel: &str| out.join(rel).to_string_lossy().to_string();
    for bad in [
        ".env".to_string(),
        "./.env".to_string(),
        abs(".env"),
        "models/.env".to_string(),
        ".hidden/a.glb".to_string(),
        "meshy_output/history.json".to_string(),
        abs("meshy_output/history.json"),
        "meshy_output/x/model.glb".to_string(),
        ".".to_string(),
        "/Users/x/.zshrc".to_string(),
    ] {
        assert!(save(&bad).is_some(), "{bad}");
    }
    for ok in ["model.glb".to_string(), "models/cat.v2.glb".to_string(), abs("models/a.glb")] {
        assert!(save(&ok).is_none(), "{ok}");
    }
    // Reading is another rule: an input image under meshy_output/ may be read.
    assert!(refuse_args(&id, &meshy, &call("meshy_image_to_3d", json!({ "file_path": "meshy_output/a.png" }))).is_none());
    let _ = std::fs::remove_dir_all(&out);
}

#[test]
fn the_new_rules_and_cleared_settings_are_checked_with_the_spec() {
    let ok = spec_for("/x".into());
    let rule_ok = |r: ArgRule| StdioSpec { arg_rules: vec![r], ..ok.clone() }.validate();
    assert!(rule_ok(rule_with("t", "format", RULE_ONE_OF, 0, &["mp3"])).is_ok());
    assert!(rule_ok(rule_with("t", "format", RULE_ONE_OF, 0, &[])).is_err(), "one_of lists its values");
    assert!(rule_ok(rule_with("t", "format", RULE_ONE_OF, 0, &["../x"])).is_err());
    assert!(rule_ok(rule_with("t", "text", RULE_NAME_PART, 10, &[])).is_ok());
    assert!(rule_ok(rule_with("", "save_to", RULE_SAVE_INSIDE_OUTPUT, 0, &["meshy_output"])).is_ok());
    assert!(rule_ok(rule_with("", "save_to", RULE_SAVE_INSIDE_OUTPUT, 0, &["a/b"])).is_err());
    let env = |n: &str, v: &str| {
        let mut s = ok.clone();
        s.env.insert(n.into(), v.into());
        s.validate()
    };
    for n in ["HTTPS_PROXY", "https_proxy", "NODE_TLS_REJECT_UNAUTHORIZED", "NODE_OPTIONS"] {
        assert!(env(n, "").is_ok(), "{n} cleared");
        assert!(env(n, "http://evil.example:8080").is_err(), "{n} given a value");
    }
}

/// A cleared setting reaches the child as set-and-empty, which is what keeps a `.env` from it.
#[tokio::test]
async fn a_cleared_setting_reaches_the_child_set_and_empty() {
    capture_logs();
    let id = fresh_id("cleared");
    proxy::set_key(&id, KEY).unwrap();
    let mut spec = spec_for(script("fake.sh", FAKE));
    spec.env.insert("HTTPS_PROXY".into(), String::new());
    spec.env.insert("NODE_OPTIONS".into(), String::new());
    proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(20)).await.unwrap();
    let names = call_text(&id, "env").await;
    assert!(names.split(' ').any(|n| n == "HTTPS_PROXY") && names.split(' ').any(|n| n == "NODE_OPTIONS"), "{names}");
    proxy::remove(&id).unwrap();
}

/// The exact spelling, the absolute path and the first save are refused everywhere. The output
/// folder usually sits on a case-insensitive volume (APFS also folds `ſ` to `s`), where every
/// spelling that reaches `meshy_output/` must be refused too: those are asserted only where the
/// volume is seen to fold them, through the catalogue's real Meshy rules.
#[test]
fn a_save_into_meshys_index_is_refused_whatever_its_spelling() {
    let id = fresh_id("folded");
    let meshy = proxy::spec_for_route("meshy").unwrap().arg_rules;
    let out = output_folder(&id).unwrap();
    std::fs::create_dir_all(&out).unwrap();
    let save = |to: &str| refuse_args(&id, &meshy, &call("meshy_download_model", json!({ "task_id": "t", "save_to": to })));
    // The index folder need not exist yet: the check makes it, then compares.
    assert!(save("meshy_output/history.json").is_some());
    assert!(out.join("meshy_output").is_dir(), "made so it can be compared with");
    std::fs::write(out.join("meshy_output/history.json"), b"[]").unwrap();
    let abs = out.join("meshy_output").join("history.json").to_string_lossy().to_string();
    for bad in ["meshy_output/history.json", "meshy_output/x/metadata.json", "meshy_output", abs.as_str()] {
        assert!(save(bad).is_some(), "{bad}");
    }
    // Each folded spelling is asserted only where the volume really folds it to the same folder
    // (APFS and NTFS do case; APFS also folds the long s): on a case-sensitive volume those names
    // are new, separate folders, and a save there is allowed.
    let same_folder = |name: &str| std::fs::metadata(out.join(name)).is_ok();
    if same_folder("MESHY_OUTPUT") && same_folder("Meshy_Output") && same_folder("meshy_OUTPUT") && same_folder("MESHY_Output") {
        let abs_folded = out.join("MESHY_Output").join("history.json").to_string_lossy().to_string();
        for bad in ["MESHY_OUTPUT/history.json", "Meshy_Output/history.json", "meshy_OUTPUT/x/metadata.json", abs_folded.as_str(), "MESHY_OUTPUT", "Meshy_Output/new/model.glb"] {
            assert!(save(bad).is_some(), "{bad}");
        }
    }
    if same_folder("me\u{17f}hy_output") {
        assert!(save("me\u{17f}hy_output/history.json").is_some());
    }
    for ok in ["models/robot.glb", "robot.glb", "meshy_outputs/robot.glb", "my_meshy_output/robot.glb"] {
        assert!(save(ok).is_none(), "{ok}");
    }
    assert_eq!(std::fs::read(out.join("meshy_output/history.json")).unwrap(), b"[]", "a check never writes the index");
    let _ = std::fs::remove_dir_all(&out);
}
