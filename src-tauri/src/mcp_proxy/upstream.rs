//! maiTerm as an MCP client of one upstream server, over Streamable HTTP (docs/mcp-proxy.md §2).
//!
//! Only what the proxy needs: `initialize`, `tools/list` (flattened across pages), `tools/call`,
//! the `GET` stream for server notifications, and `DELETE` at the end of a session. A response may
//! come back as plain JSON or as an SSE stream; on a stream, notifications that arrive before the
//! response are collected and handed to the caller.
//!
//! Nothing here logs a token, a tool's arguments or its result.

use serde_json::{json, Value};
use std::time::Duration;

/// The protocol revision maiTerm asks for. A server may answer with an older one; whatever it
/// answers is sent back as `MCP-Protocol-Version` on every later request.
pub const PROTOCOL: &str = "2025-11-25";

/// Listing and handshakes are quick; anything slower is a server in trouble.
const QUICK: Duration = Duration::from_secs(30);
/// A tool call may legitimately run for minutes (a build, a long query).
const CALL: Duration = Duration::from_secs(15 * 60);
/// Pages of `tools/list` followed before giving up on a server that never stops paginating.
const MAX_PAGES: usize = 50;

/// One upstream MCP session.
#[derive(Clone, Debug)]
pub struct Session {
    /// The `Mcp-Session-Id` the server assigned, if it assigns one.
    pub id: Option<String>,
    /// The protocol revision the server answered `initialize` with.
    pub protocol: String,
    /// The server's `initialize` result: `serverInfo`, `capabilities`, `instructions`.
    pub init: Value,
}

/// What can go wrong talking to an upstream, in words that are safe to log and to show.
#[derive(Debug, Clone)]
pub enum UpErr {
    /// 401. `challenge` is the `WWW-Authenticate` header, which names the resource metadata and
    /// the scope the server wants (§5).
    Unauthorized { challenge: Option<String> },
    /// 403. An `insufficient_scope` challenge asks for a wider sign-in.
    Forbidden { challenge: Option<String> },
    /// 404 on a request that carried a session id: the server forgot the session.
    SessionExpired,
    /// Any other HTTP status.
    Http { status: u16, detail: String },
    /// No answer at all: DNS, refused, TLS, timeout.
    Net(String),
    /// The server answered with a JSON-RPC error.
    Rpc { code: i64, message: String, data: Option<Value> },
    /// The server answered with something that isn't MCP.
    Protocol(String),
}

impl std::fmt::Display for UpErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            UpErr::Unauthorized { .. } => f.write_str("the server refused the credentials (401)"),
            UpErr::Forbidden { .. } => f.write_str("the server refused this request (403)"),
            UpErr::SessionExpired => f.write_str("the server ended the session"),
            UpErr::Http { status, detail } => write!(f, "HTTP {status}{}", if detail.is_empty() { String::new() } else { format!(": {detail}") }),
            UpErr::Net(e) => write!(f, "unreachable: {e}"),
            UpErr::Rpc { code, message, .. } => write!(f, "error {code}: {message}"),
            UpErr::Protocol(e) => write!(f, "not an MCP answer: {e}"),
        }
    }
}

/// A server that was answered, with whatever it sent on the way.
struct Answer {
    /// The JSON-RPC response, `None` for an accepted notification (202).
    response: Option<Value>,
    /// Notifications the server sent before the response, on the same stream.
    notifications: Vec<Value>,
    /// `Mcp-Session-Id` from the response headers.
    session_id: Option<String>,
}

pub struct Upstream {
    pub url: String,
    http: reqwest::Client,
}

impl Upstream {
    pub fn new(url: &str) -> Self {
        let http = reqwest::Client::builder()
            .user_agent(format!("maiterm-mcp-proxy/{}", crate::APP_VERSION))
            .connect_timeout(Duration::from_secs(10))
            .build()
            .unwrap_or_default();
        Self { url: url.to_string(), http }
    }

    fn request(&self, method: reqwest::Method, auth: Option<&str>, session: Option<&Session>) -> reqwest::RequestBuilder {
        let mut rb = self
            .http
            .request(method, &self.url)
            .header("accept", "application/json, text/event-stream");
        if let Some(tok) = auth {
            rb = rb.bearer_auth(tok);
        }
        if let Some(s) = session {
            rb = rb.header("mcp-protocol-version", &s.protocol);
            if let Some(id) = &s.id {
                rb = rb.header("mcp-session-id", id);
            }
        }
        rb
    }

    /// POST one JSON-RPC message and read its answer.
    async fn post(&self, auth: Option<&str>, session: Option<&Session>, msg: &Value, timeout: Duration) -> Result<Answer, UpErr> {
        let want = msg.get("id").cloned();
        let fut = async {
            let resp = self
                .request(reqwest::Method::POST, auth, session)
                .header("content-type", "application/json")
                .body(msg.to_string())
                .send()
                .await
                .map_err(|e| UpErr::Net(net_detail(e)))?;
            let status = resp.status().as_u16();
            let header = |name: &str| resp.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
            let session_id = header("mcp-session-id");
            let challenge = header("www-authenticate");
            let content_type = header("content-type").unwrap_or_default();
            match status {
                401 => return Err(UpErr::Unauthorized { challenge }),
                403 => return Err(UpErr::Forbidden { challenge }),
                404 if session.and_then(|s| s.id.as_ref()).is_some() => return Err(UpErr::SessionExpired),
                202 => return Ok(Answer { response: None, notifications: vec![], session_id }),
                200..=299 => {}
                _ => {
                    let body = resp.text().await.unwrap_or_default();
                    return Err(UpErr::Http { status, detail: short(&body) });
                }
            }
            if content_type.starts_with("text/event-stream") {
                let (response, notifications) = read_stream_until(resp, want.as_ref()).await?;
                Ok(Answer { response, notifications, session_id })
            } else {
                let body = resp.text().await.map_err(|e| UpErr::Net(net_detail(e)))?;
                if body.trim().is_empty() {
                    return Ok(Answer { response: None, notifications: vec![], session_id });
                }
                let v: Value = serde_json::from_str(&body).map_err(|e| UpErr::Protocol(e.to_string()))?;
                Ok(Answer { response: Some(v), notifications: vec![], session_id })
            }
        };
        tokio::time::timeout(timeout, fut)
            .await
            .unwrap_or_else(|_| Err(UpErr::Net(format!("no answer within {}s", timeout.as_secs()))))
    }

    /// A request expecting a result: the result, or the JSON-RPC error as `UpErr::Rpc`.
    async fn rpc(&self, auth: Option<&str>, session: &Session, method: &str, params: Value, timeout: Duration) -> Result<(Value, Vec<Value>), UpErr> {
        let msg = json!({ "jsonrpc": "2.0", "id": next_id(), "method": method, "params": params });
        let a = self.post(auth, Some(session), &msg, timeout).await?;
        let resp = a.response.ok_or_else(|| UpErr::Protocol(format!("no response to {method}")))?;
        Ok((result_of(resp)?, a.notifications))
    }

    /// `initialize` + `notifications/initialized`.
    pub async fn initialize(&self, auth: Option<&str>) -> Result<Session, UpErr> {
        let msg = json!({
            "jsonrpc": "2.0",
            "id": next_id(),
            "method": "initialize",
            "params": {
                "protocolVersion": PROTOCOL,
                "capabilities": {},
                "clientInfo": { "name": "maiterm-mcp-proxy", "title": crate::APP_DISPLAY_NAME, "version": crate::APP_VERSION },
            },
        });
        let a = self.post(auth, None, &msg, QUICK).await?;
        let init = result_of(a.response.ok_or_else(|| UpErr::Protocol("no response to initialize".into()))?)?;
        let protocol = init.get("protocolVersion").and_then(Value::as_str).unwrap_or(PROTOCOL).to_string();
        let session = Session { id: a.session_id, protocol, init };
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        self.post(auth, Some(&session), &note, QUICK).await?;
        Ok(session)
    }

    /// Every tool the server lists, pages flattened in order.
    pub async fn list_tools(&self, auth: Option<&str>, session: &Session) -> Result<Vec<Value>, UpErr> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let (result, _) = self.rpc(auth, session, "tools/list", params, QUICK).await?;
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

    /// Call a tool with the caller's `params` as sent (`name`, `arguments`, `_meta`). Returns the
    /// server's result verbatim, and whether it announced a tool-list change on the way.
    pub async fn call_tool(&self, auth: Option<&str>, session: &Session, params: Value) -> Result<(Value, bool), UpErr> {
        let (result, notes) = self.rpc(auth, session, "tools/call", params, CALL).await?;
        Ok((result, notes.iter().any(is_list_changed)))
    }

    /// End a session, best effort.
    pub async fn end(&self, auth: Option<&str>, session: &Session) {
        if session.id.is_none() {
            return;
        }
        let _ = tokio::time::timeout(QUICK, self.request(reqwest::Method::DELETE, auth, Some(session)).send()).await;
    }

    /// Open the `GET` stream for server-initiated messages. `Ok(None)` when the server offers none
    /// (405), which is allowed.
    pub async fn open_stream(&self, auth: Option<&str>, session: &Session) -> Result<Option<reqwest::Response>, UpErr> {
        let resp = tokio::time::timeout(QUICK, self.request(reqwest::Method::GET, auth, Some(session)).header("accept", "text/event-stream").send())
            .await
            .map_err(|_| UpErr::Net("no answer to the stream request".into()))?
            .map_err(|e| UpErr::Net(net_detail(e)))?;
        let challenge = resp.headers().get("www-authenticate").and_then(|v| v.to_str().ok()).map(str::to_string);
        match resp.status().as_u16() {
            200 => Ok(Some(resp)),
            405 => Ok(None),
            401 => Err(UpErr::Unauthorized { challenge }),
            403 => Err(UpErr::Forbidden { challenge }),
            404 => Err(UpErr::SessionExpired),
            s => Err(UpErr::Http { status: s, detail: String::new() }),
        }
    }
}

/// Read an open `GET` stream until it ends, calling `on_message` with each JSON message.
pub async fn read_stream(mut resp: reqwest::Response, mut on_message: impl FnMut(Value)) -> Result<(), UpErr> {
    let mut parser = SseParser::default();
    loop {
        match resp.chunk().await {
            Ok(Some(bytes)) => {
                for data in parser.push(&bytes) {
                    if let Ok(v) = serde_json::from_str::<Value>(&data) {
                        on_message(v);
                    }
                }
            }
            Ok(None) => return Ok(()),
            Err(e) => return Err(UpErr::Net(net_detail(e))),
        }
    }
}

pub fn is_list_changed(v: &Value) -> bool {
    v.get("method").and_then(Value::as_str) == Some("notifications/tools/list_changed")
}

/// Read a POST's SSE answer until the response to `want` arrives. Server requests on this stream
/// (sampling, elicitation) are not answered: the proxy advertises no client capabilities.
async fn read_stream_until(mut resp: reqwest::Response, want: Option<&Value>) -> Result<(Option<Value>, Vec<Value>), UpErr> {
    let mut parser = SseParser::default();
    let mut notifications = Vec::new();
    loop {
        let chunk = resp.chunk().await.map_err(|e| UpErr::Net(net_detail(e)))?;
        let Some(bytes) = chunk else {
            return Err(UpErr::Protocol("the stream ended without a response".into()));
        };
        for data in parser.push(&bytes) {
            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
            let is_response = v.get("result").is_some() || v.get("error").is_some();
            if is_response && (want.is_none() || v.get("id") == want) {
                return Ok((Some(v), notifications));
            }
            if v.get("method").is_some() && v.get("id").is_none() {
                notifications.push(v);
            }
        }
    }
}

fn result_of(resp: Value) -> Result<Value, UpErr> {
    if let Some(err) = resp.get("error") {
        return Err(UpErr::Rpc {
            code: err.get("code").and_then(Value::as_i64).unwrap_or(-32603),
            message: err.get("message").and_then(Value::as_str).unwrap_or("").to_string(),
            data: err.get("data").cloned(),
        });
    }
    resp.get("result").cloned().ok_or_else(|| UpErr::Protocol("a response with neither result nor error".into()))
}

fn next_id() -> Value {
    static N: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    json!(N.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
}

/// reqwest's message without the URL. Its `Display` appends the full URL, and a server URL can
/// carry a key (`…/s/<secret>/mcp`, `?api_key=`); this text reaches the log, the frontend and
/// the agent's tool result.
fn net_detail(e: reqwest::Error) -> String {
    if e.is_timeout() {
        "timed out".into()
    } else if e.is_connect() {
        "could not connect".into()
    } else {
        e.without_url().to_string()
    }
}

fn short(s: &str) -> String {
    let s = s.trim();
    let mut out: String = s.chars().take(200).collect();
    if s.chars().count() > 200 {
        out.push('…');
    }
    out
}

/// Server-sent events, fed in chunks that may split anywhere (even inside a UTF-8 character).
/// Yields each event's `data`, multi-line data joined with `\n`.
#[derive(Default)]
pub struct SseParser {
    buf: Vec<u8>,
    data: Vec<String>,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(nl) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=nl).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if !self.data.is_empty() {
                    out.push(self.data.join("\n"));
                    self.data.clear();
                }
            } else if let Some(rest) = line.strip_prefix("data:") {
                self.data.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
            }
            // `event:`, `id:`, `retry:` and `:` comments carry nothing the proxy uses.
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sse_events_survive_any_chunking() {
        let stream = "event: message\r\ndata: {\"a\":1}\r\n\r\n: keepalive\n\ndata: line1\ndata: line2\n\ndata: é\n\n";
        let bytes = stream.as_bytes();
        for split in 1..bytes.len() {
            let mut p = SseParser::default();
            let mut got = p.push(&bytes[..split]);
            got.extend(p.push(&bytes[split..]));
            assert_eq!(got, vec!["{\"a\":1}", "line1\nline2", "é"], "split at {split}");
        }
    }

    #[test]
    fn errors_come_back_as_rpc_errors() {
        let e = result_of(json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32602, "message": "bad" } })).unwrap_err();
        assert!(matches!(e, UpErr::Rpc { code: -32602, .. }));
        assert_eq!(result_of(json!({ "id": 1, "result": { "ok": true } })).unwrap(), json!({ "ok": true }));
    }
}
