//! Stdio-to-HTTP MCP bridge (`maiterm --mcp-stdio`).
//!
//! Antigravity CLI (`agy`) does not expand `${ENV_VAR}` in `mcp_config.json` `url` or `headers`,
//! but stdio MCP child processes inherit the tab's live PTY environment (`MAITERM_PORT`,
//! `MAITERM_AUTH`, `MAITERM_TAB_ID`). Running maiTerm itself as a lightweight stdio bridge
//! (dispatched in `main()` before Tauri starts, just like the Chrome native messaging host)
//! lets `agy` connect to the owning maiTerm instance with per-tab `x-maiterm-tab` identity and
//! zero ports or tokens written to disk (docs/agy-integration-review.md).

use std::io::{self, BufRead, Write};
use std::time::Duration;

use serde_json::{json, Value};

/// True when the process was launched as the stdio MCP bridge (`--mcp-stdio`).
pub fn requested() -> bool {
    std::env::args_os().nth(1).is_some_and(|a| a == "--mcp-stdio")
}

/// Bridge configuration resolved from CLI args and environment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BridgeConfig {
    port: u16,
    auth: String,
    tab_id: Option<String>,
    /// HTTP endpoint path on loopback maiTerm (default `/mcp`, or `/mcp/x/<name>` for proxied servers).
    path: String,
}

fn resolve_config(args: &[String]) -> Option<BridgeConfig> {
    let mut baked_port: Option<String> = None;
    let mut path: Option<String> = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--port" {
            baked_port = iter.next().cloned();
        } else if arg == "--path" {
            path = iter.next().cloned();
        }
    }
    let port_str = baked_port
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("MAITERM_PORT").ok())?;
    let port: u16 = port_str.trim().parse().ok()?;
    if port == 0 {
        return None;
    }
    let auth = std::env::var("MAITERM_AUTH").ok().filter(|s| !s.is_empty())?;
    let tab_id = std::env::var("MAITERM_TAB_ID").ok().filter(|s| !s.is_empty());
    let path = path
        .filter(|p| p.starts_with('/'))
        .unwrap_or_else(|| "/mcp".to_string());
    Some(BridgeConfig { port, auth, tab_id, path })
}

/// Extract a JSON-RPC payload from either a Streamable HTTP SSE frame (`data: {...}`)
/// or a plain JSON response body.
fn extract_rpc_payload(content_type: &str, body: &str) -> Option<String> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return None;
    }
    if content_type.contains("text/event-stream") || trimmed.starts_with("event:") || trimmed.starts_with("data:") {
        let mut data_lines: Vec<&str> = Vec::new();
        for line in body.lines() {
            if let Some(rest) = line.strip_prefix("data:") {
                data_lines.push(rest.trim_start());
            }
        }
        if data_lines.is_empty() {
            return None;
        }
        let joined = data_lines.join("\n");
        // Compact to single-line JSON for NDJSON stdio transport.
        match serde_json::from_str::<Value>(&joined) {
            Ok(v) => serde_json::to_string(&v).ok(),
            Err(_) => Some(joined.replace('\n', " ")),
        }
    } else {
        match serde_json::from_str::<Value>(trimmed) {
            Ok(v) => serde_json::to_string(&v).ok(),
            Err(_) => Some(trimmed.replace('\n', " ")),
        }
    }
}

/// If `raw` is a JSON-RPC request carrying an `"id"`, build a `-32000` error response so the
/// client never hangs waiting on a failed HTTP call. Notifications (no `"id"`) return `None`.
fn rpc_error_for(raw: &str, message: &str) -> Option<String> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let id = v.get("id")?.clone();
    if id.is_null() {
        return None;
    }
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": message,
        }
    }))
    .ok()
}

async fn run_bridge<R: BufRead, W: Write>(cfg: BridgeConfig, mut reader: R, mut writer: W) {
    let client = match reqwest::Client::builder()
        .no_proxy()
        // Tool calls like openDiff or waitForService can block up to ~120s.
        .timeout(Duration::from_secs(180))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    let url = format!("http://127.0.0.1:{}{}", cfg.port, cfg.path);
    let mut session_id: Option<String> = None;
    let mut line = String::new();

    loop {
        line.clear();
        match reader.read_line(&mut line) {
            Ok(0) => break, // EOF
            Ok(_) => {}
            Err(_) => break,
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut req = client
            .post(&url)
            .header("x-maiterm-authorization", &cfg.auth)
            .header("content-type", "application/json")
            .header("accept", "application/json, text/event-stream");
        if let Some(ref tab) = cfg.tab_id {
            req = req.header("x-maiterm-tab", tab);
        }
        if let Some(ref sid) = session_id {
            req = req.header("mcp-session-id", sid);
        }

        let out_line = match req.body(trimmed.to_string()).send().await {
            Ok(resp) => {
                if let Some(sid) = resp
                    .headers()
                    .get("mcp-session-id")
                    .and_then(|v| v.to_str().ok())
                    .filter(|s| !s.is_empty())
                {
                    session_id = Some(sid.to_string());
                }
                let status = resp.status();
                let content_type = resp
                    .headers()
                    .get(reqwest::header::CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_string();
                match resp.text().await {
                    Ok(body) => {
                        if status == reqwest::StatusCode::ACCEPTED {
                            None
                        } else if status.is_success() {
                            extract_rpc_payload(&content_type, &body)
                        } else {
                            rpc_error_for(trimmed, &format!("maiTerm MCP HTTP {}", status.as_u16()))
                        }
                    }
                    Err(e) => rpc_error_for(trimmed, &format!("maiTerm MCP read failed: {e}")),
                }
            }
            Err(e) => rpc_error_for(trimmed, &format!("maiTerm MCP unreachable: {e}")),
        };

        if let Some(reply) = out_line {
            if writeln!(writer, "{reply}").is_err() || writer.flush().is_err() {
                break;
            }
        }
    }
}

/// Entry point called from `main()` when `--mcp-stdio` is passed.
pub fn run() {
    let args: Vec<String> = std::env::args().skip(2).collect();
    let Some(cfg) = resolve_config(&args) else {
        // Launched outside a maiTerm tab (or in an isolated instance with port 0): exit cleanly.
        return;
    };
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()
    {
        Ok(r) => r,
        Err(_) => return,
    };
    let stdin = io::stdin();
    let stdout = io::stdout();
    rt.block_on(run_bridge(cfg, stdin.lock(), stdout.lock()));
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{header, HeaderMap, StatusCode},
        response::{IntoResponse, Response},
        routing::post,
        Router,
    };
    use std::sync::{Arc, Mutex};

    #[test]
    fn extract_rpc_payload_handles_sse_and_plain_json() {
        let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"ok\":true}}\n\n";
        assert_eq!(
            extract_rpc_payload("text/event-stream", sse).as_deref(),
            Some("{\"id\":1,\"jsonrpc\":\"2.0\",\"result\":{\"ok\":true}}")
        );
        let plain = "  {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}  \n";
        assert_eq!(
            extract_rpc_payload("application/json", plain).as_deref(),
            Some("{\"id\":2,\"jsonrpc\":\"2.0\",\"result\":{}}")
        );
        assert_eq!(extract_rpc_payload("text/event-stream", ""), None);
    }

    #[test]
    fn rpc_error_only_for_requests_with_id() {
        let req = r#"{"jsonrpc":"2.0","id":7,"method":"tools/list"}"#;
        let err = rpc_error_for(req, "boom").expect("request gets error");
        let parsed: Value = serde_json::from_str(&err).unwrap();
        assert_eq!(parsed["id"], 7);
        assert_eq!(parsed["error"]["code"], -32000);
        assert_eq!(parsed["error"]["message"], "boom");

        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert_eq!(rpc_error_for(notif, "boom"), None);
    }

    #[tokio::test]
    async fn bridge_forwards_headers_propagates_session_id_and_unpacks_sse() {
        let seen: Arc<Mutex<Vec<(Option<String>, Option<String>, Option<String>, String)>>> =
            Arc::new(Mutex::new(Vec::new()));
        let seen_c = seen.clone();

        let app = Router::new().route(
            "/mcp",
            post(move |headers: HeaderMap, body: String| {
                let seen = seen_c.clone();
                async move {
                    let auth = headers.get("x-maiterm-authorization").and_then(|v| v.to_str().ok()).map(String::from);
                    let tab = headers.get("x-maiterm-tab").and_then(|v| v.to_str().ok()).map(String::from);
                    let sid = headers.get("mcp-session-id").and_then(|v| v.to_str().ok()).map(String::from);
                    seen.lock().unwrap().push((auth, tab, sid, body.clone()));
                    let req: Value = serde_json::from_str(&body).unwrap();
                    match req["method"].as_str() {
                        Some("initialize") => {
                            let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"protocolVersion\":\"2025-03-26\"}}\n\n";
                            Response::builder()
                                .status(StatusCode::OK)
                                .header(header::CONTENT_TYPE, "text/event-stream")
                                .header("mcp-session-id", "sid-abc")
                                .body(Body::from(sse))
                                .unwrap()
                        }
                        Some("notifications/initialized") => StatusCode::ACCEPTED.into_response(),
                        _ => {
                            let sse = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"tools\":[]}}\n\n";
                            Response::builder()
                                .status(StatusCode::OK)
                                .header(header::CONTENT_TYPE, "text/event-stream")
                                .body(Body::from(sse))
                                .unwrap()
                        }
                    }
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });

        let input = concat!(
            "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{\"clientInfo\":{\"name\":\"antigravity-client\"}}}\n",
            "{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}\n",
            "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/list\"}\n",
        );
        let cfg = BridgeConfig {
            port,
            auth: "tok-123".into(),
            tab_id: Some("tab-xyz".into()),
            path: "/mcp".into(),
        };
        let mut out: Vec<u8> = Vec::new();
        run_bridge(cfg, io::Cursor::new(input.as_bytes()), &mut out).await;

        let out_str = String::from_utf8(out).unwrap();
        let lines: Vec<&str> = out_str.lines().collect();
        assert_eq!(lines.len(), 2, "notification emits no line; got: {out_str}");
        let r1: Value = serde_json::from_str(lines[0]).unwrap();
        let r2: Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(r1["id"], 1);
        assert_eq!(r1["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(r2["id"], 2);
        assert_eq!(r2["result"]["tools"], json!([]));

        let calls = seen.lock().unwrap();
        assert_eq!(calls.len(), 3);
        assert_eq!(calls[0].0.as_deref(), Some("tok-123"));
        assert_eq!(calls[0].1.as_deref(), Some("tab-xyz"));
        assert_eq!(calls[0].2, None);
        // Subsequent calls carry the minted mcp-session-id.
        assert_eq!(calls[1].2.as_deref(), Some("sid-abc"));
        assert_eq!(calls[2].2.as_deref(), Some("sid-abc"));
    }
}
