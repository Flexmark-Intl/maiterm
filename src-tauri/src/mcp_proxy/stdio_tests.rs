//! Stdio upstreams against a fake stdio MCP server: a POSIX sh script written by the tests, so
//! nothing here downloads or runs a package. It prints its key to stderr (which must never reach
//! a log), a banner to stdout (which must be skipped), and answers `initialize`, `tools/list` and a
//! few tools that report what the child sees: its environment, argv, pid and folder.

use crate::mcp_proxy::{self as proxy, entries, AUTH_KEY, STATE_IDLE};
use super::*;
use serde_json::json;
use std::time::Duration;

/// The fake key. Distinctive, so finding it anywhere is a leak, not a coincidence.
const KEY: &str = "VALUE-fake-mcp-key-9d3e71b2";
const KEY_VAR: &str = "FAKE_MCP_KEY";

const FAKE: &str = r#"
reply() { printf '{"jsonrpc":"2.0","id":%s,"result":{"content":[{"type":"text","text":"%s"}]}}\n' "$1" "$2"; }
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
      printf '{"jsonrpc":"2.0","id":%s,"result":{"tools":[{"name":"fast","inputSchema":{"type":"object"}},{"name":"slow","inputSchema":{"type":"object"}}%s]}}\n' "$id" "$extra" ;;
    *'"method":"tools/call"'*)
      case "$line" in
        *'"name":"slow"'*) ( sleep 1; reply "$id" slow ) & ;;
        *'"name":"fast"'*) reply "$id" fast ;;
        *'"name":"env"'*) reply "$id" "$(env | sed 's/=.*//' | sort | tr '\n' ' ')|$(pwd)" ;;
        *'"name":"argv"'*) reply "$id" "$0 $*" ;;
        *'"name":"pid"'*) reply "$id" "$$" ;;
        *'"name":"die"'*) exit 3 ;;
        *'"name":"grow"'*) : > grown; printf '{"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n'; reply "$id" grown ;;
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

fn spec_for(path: String) -> StdioSpec {
    StdioSpec { route: "test/fake".into(), command: "/bin/sh".into(), args: vec![path], env_key: Some(KEY_VAR.into()) }
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
    let back: ProxyServer = serde_json::from_value(v.clone()).unwrap();
    assert!(back.is_stdio());
    assert_eq!(back.stdio, Some(spec));
    assert_eq!(back.route(), Some("test/fake"));
    assert!(!format!("{s:?}").contains(KEY));
    assert!(!v.to_string().contains(KEY));
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
    for bad in ["PATH", "NODE_OPTIONS", "DYLD_INSERT_LIBRARIES", "LD_PRELOAD", "lower", "1X", ""] {
        assert!(with(&|s| s.env_key = Some(bad.into())).is_err(), "{bad}");
    }
    assert!(with(&|s| s.env_key = None).is_ok());
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

#[tokio::test]
async fn a_key_on_the_command_line_is_refused() {
    let id = fresh_id("argv");
    proxy::set_key(&id, KEY).unwrap();
    let mut spec = spec_for(script("fake.sh", FAKE));
    spec.args.push(format!("--key={KEY}"));
    let err = proxy::add_stdio_with(&id, spec, vec![], "test", Duration::from_secs(5)).await.unwrap_err();
    assert!(!err.contains(KEY), "{err}");
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

#[tokio::test]
async fn the_child_gets_the_login_path_and_its_key_and_nothing_else() {
    let id = added("env").await;
    let text = call_text(&id, "env").await;
    let (names, cwd) = text.split_once('|').unwrap();
    let names: Vec<&str> = names.split_whitespace().collect();
    assert!(names.contains(&"PATH") && names.contains(&KEY_VAR), "{names:?}");
    // What sh itself sets is all there is beyond the two.
    for n in &names {
        assert!(["PATH", KEY_VAR, "PWD", "SHLVL", "_", "OLDPWD"].contains(n), "maiTerm's environment leaked: {n}");
    }
    let want = output_folder(&id).unwrap();
    assert_eq!(std::fs::canonicalize(cwd).unwrap(), std::fs::canonicalize(&want).unwrap());
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
    stop_idle(now_ms(), IDLE);
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
    stop_idle(now_ms(), IDLE);
    assert!(running(&id).is_some(), "used just now");
    age_for_test(&id, IDLE + Duration::from_secs(1));
    stop_idle(now_ms(), IDLE);
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
    proxy::set_key(&id, "VALUE-another").unwrap();
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
