//! Signing in to an MCP server: maiTerm as the one OAuth client (docs/mcp-proxy.md §5).
//!
//! Follows the MCP authorization spec (2025-11-25, with 2025-06-18 servers handled too), against
//! what real servers do, checked live 2026-10-13:
//! - **Discovery doesn't wait for a 401.** langsys.dev lists its tools to anyone and refuses only
//!   calls; svelte.dev needs nothing. So `probe` asks for the protected resource metadata itself:
//!   the 401's `resource_metadata` if there is one, then the path-suffixed well-known URL, then
//!   the root one.
//! - **The authorization server is chosen, not assumed.** It must offer the code flow with PKCE
//!   S256 (Higgsfield lists a device-code-only server first). Its metadata is tried in the spec's
//!   order, RFC 8414 before OIDC: Clerk's OIDC document lacks the registration endpoint its 8414
//!   one has. Server errors are retried; Higgsfield's metadata 502s now and then.
//! - **`resource` is the PRM's, exactly as written.** Servers disagree with themselves about
//!   trailing slashes and paths.
//! - **Registration:** a client id the human supplies (GitHub has no self-registration), else
//!   maiTerm's client ID metadata document where the server takes one (ElevenLabs offers nothing
//!   else), else dynamic registration as a public client. A dynamic registration is redone when
//!   the redirect URI changes, which is every launch: it names maiTerm's port. The metadata
//!   document's redirects are fixed (`CIMD_PORTS`), so that sign-in has its own listener.
//! - **Refresh tokens rotate**, so maiTerm is their only holder: one refresh in flight per
//!   server, and the new token is in the keychain before the new access token is used.

use super::secrets::{self, Cred};
use super::{get, update, ProxyServer};
use base64::Engine as _;
use parking_lot::Mutex;
use rand::Rng;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

/// How long a started sign-in waits for the browser to come back.
const PENDING_TTL_MS: u64 = 10 * 60 * 1000;
/// Refresh this long before the access token says it expires.
const REFRESH_EARLY_MS: u64 = 60 * 1000;

fn http() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent(format!("maiterm-mcp-proxy/{}", crate::APP_VERSION))
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .unwrap_or_default()
}

/// maiTerm's client ID metadata document (draft-ietf-oauth-client-id-metadata-document, the MCP
/// authorization spec 2025-11-25). The URL is the client id. The document is
/// `website/public/oauth/client-metadata.json`, published with the site; a test reads it, so the
/// two can't drift. Dev and release builds share it: nothing in it differs between them.
pub const CLIENT_METADATA_URL: &str = "https://maiterm.dev/oauth/client-metadata.json";

/// The loopback ports a metadata-document sign-in comes back on, tried in order. The document has
/// to list its redirect URIs, and a server may match them exactly (RFC 8252 §7.3 says any port,
/// but not every server does), so these are the only ports such a sign-in uses. Below every OS's
/// ephemeral range (Linux 32768+, macOS and Windows 49152+), so an outgoing connection never holds
/// one by chance, and spread out, so one Windows excluded-port block can't hold all three.
pub const CIMD_PORTS: [u16; 3] = [21847, 26413, 29581];

/// How a sign-in gets a client id (`Probe::registration`, the plan's `registration`).
pub const REG_DYNAMIC: &str = "dynamic";
pub const REG_METADATA_DOCUMENT: &str = "metadata_document";
pub const REG_NONE: &str = "none";

/// What adding a server found: whether it needs a sign-in, and how one would go.
#[derive(Clone, Debug, Serialize, Default)]
pub struct Probe {
    /// `none` | `oauth` | `token`: what the add form should offer.
    pub auth: String,
    /// One sentence for the form: "No sign-in needed", "Sign in with linear.app", …
    pub detail: String,
    /// The authorization server's issuer, when OAuth is offered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    /// Whether maiTerm gets a client id there by itself (`registration` is `dynamic` or
    /// `metadata_document`). False means a client id must be supplied.
    pub can_register: bool,
    /// `dynamic` (RFC 7591) | `metadata_document` (maiTerm's `CLIENT_METADATA_URL`) | `none` (the
    /// human supplies a client id). `none` too when there is no OAuth.
    pub registration: String,
    /// Entries of the human's own reaching the same URL ("Claude: linear"), filled by the command.
    pub duplicates: Vec<String>,
    /// What `begin` needs, stored on the server as `ProxyServer::oauth`. No secret.
    #[serde(skip)]
    pub plan: Option<Value>,
}

/// The parameters of a `WWW-Authenticate: Bearer …` challenge.
fn challenge_params(header: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let rest = header.trim();
    let rest = rest.strip_prefix("Bearer").or_else(|| rest.strip_prefix("bearer")).unwrap_or(rest);
    let mut chars = rest.chars().peekable();
    loop {
        while chars.peek().is_some_and(|c| *c == ',' || c.is_whitespace()) {
            chars.next();
        }
        let key: String = std::iter::from_fn(|| chars.next_if(|c| *c != '=' && *c != ',' && !c.is_whitespace())).collect();
        if key.is_empty() {
            break;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        if chars.next_if_eq(&'=').is_none() {
            continue;
        }
        while chars.peek().is_some_and(|c| c.is_whitespace()) {
            chars.next();
        }
        let value = if chars.next_if_eq(&'"').is_some() {
            let mut v = String::new();
            while let Some(c) = chars.next() {
                match c {
                    '\\' => {
                        if let Some(n) = chars.next() {
                            v.push(n)
                        }
                    }
                    '"' => break,
                    c => v.push(c),
                }
            }
            v
        } else {
            std::iter::from_fn(|| chars.next_if(|c| *c != ',')).collect::<String>().trim().to_string()
        };
        out.insert(key.to_ascii_lowercase(), value);
    }
    out
}

/// GET a JSON document, retrying server errors and timeouts a couple of times.
async fn get_json(client: &reqwest::Client, url: &str) -> Option<Value> {
    for attempt in 0..3 {
        if attempt > 0 {
            tokio::time::sleep(Duration::from_millis(600 * attempt)).await;
        }
        match client.get(url).header("accept", "application/json").send().await {
            Ok(r) if r.status().is_success() => return r.json::<Value>().await.ok().filter(Value::is_object),
            Ok(r) if r.status().is_server_error() => continue,
            Ok(_) => return None,
            Err(e) if e.is_timeout() || e.is_connect() => continue,
            Err(_) => return None,
        }
    }
    None
}

/// `/.well-known/<name>` for a URL, path-suffixed (`/.well-known/<name>/<path>`) when it has one.
fn well_known(url: &reqwest::Url, name: &str, with_path: bool) -> String {
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    if with_path && !path.is_empty() {
        format!("{origin}/.well-known/{name}{path}")
    } else {
        format!("{origin}/.well-known/{name}")
    }
}

/// The authorization server metadata URLs to try, in the 2025-11-25 order.
fn as_metadata_urls(issuer: &str) -> Vec<String> {
    let Ok(u) = reqwest::Url::parse(issuer) else { return vec![] };
    let origin = u.origin().ascii_serialization();
    let p = u.path().trim_end_matches('/');
    if p.is_empty() {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server"),
            format!("{origin}/.well-known/openid-configuration"),
        ]
    } else {
        vec![
            format!("{origin}/.well-known/oauth-authorization-server{p}"),
            format!("{origin}/.well-known/openid-configuration{p}"),
            format!("{origin}{p}/.well-known/openid-configuration"),
        ]
    }
}

/// Is a protected resource metadata `resource` this server? Same origin, and a path that is the
/// server's or a parent of it: servers publish both `https://host` and `https://host/mcp` for
/// `https://host/mcp`, with and without a trailing slash, so an exact string match refuses real
/// servers. Absent: the document is the server's own by where it was found.
fn resource_matches(resource: Option<&str>, server: &reqwest::Url) -> bool {
    let Some(r) = resource else { return true };
    let Ok(ru) = reqwest::Url::parse(r) else { return false };
    if ru.origin() != server.origin() {
        return false;
    }
    let rp = ru.path().trim_end_matches('/');
    let sp = server.path().trim_end_matches('/');
    rp.is_empty() || sp == rp || sp.starts_with(&format!("{rp}/"))
}

fn strs(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_string)).collect()).unwrap_or_default()
}

/// Can this metadata run the code flow with PKCE S256? A document that doesn't say which PKCE
/// methods it takes is refused, as the spec requires.
fn usable(meta: &Value) -> bool {
    let grants = strs(meta.get("grant_types_supported"));
    let code_ok = grants.is_empty() || grants.iter().any(|g| g == "authorization_code");
    code_ok
        && strs(meta.get("code_challenge_methods_supported")).iter().any(|m| m == "S256")
        && meta.get("authorization_endpoint").and_then(Value::as_str).is_some()
        && meta.get("token_endpoint").and_then(Value::as_str).is_some()
}

/// Find out whether and how a server wants a sign-in.
pub async fn probe(server_url: &str) -> Result<Probe, String> {
    let url = reqwest::Url::parse(server_url).map_err(|e| format!("not a URL: {e}"))?;
    let client = http();
    let up = super::upstream::Upstream::new(server_url);
    let mut challenge: HashMap<String, String> = HashMap::new();
    let mut refused = false;
    match up.initialize(None).await {
        Ok(session) => up.end(None, &session).await,
        Err(super::upstream::UpErr::Unauthorized { challenge: c }) | Err(super::upstream::UpErr::Forbidden { challenge: c }) => {
            refused = true;
            challenge = c.as_deref().map(challenge_params).unwrap_or_default();
        }
        Err(e) => return Err(format!("could not reach it: {e}")),
    }

    let mut prm_urls = Vec::new();
    if let Some(u) = challenge.get("resource_metadata") {
        prm_urls.push(u.clone());
    }
    prm_urls.push(well_known(&url, "oauth-protected-resource", true));
    prm_urls.push(well_known(&url, "oauth-protected-resource", false));
    prm_urls.dedup();
    let mut prm = None;
    let mut foreign: Option<String> = None;
    for u in &prm_urls {
        if let Some(doc) = get_json(&client, u).await {
            if strs(doc.get("authorization_servers")).is_empty() {
                continue;
            }
            // The document must be about THIS server (RFC 9728 §3.3). Otherwise a server could
            // name another service's resource and sign-in, and maiTerm would send the human's
            // token for that service here.
            let resource = doc.get("resource").and_then(Value::as_str);
            if !resource_matches(resource, &url) {
                log::warn!("[mcp-proxy] probe {server_url}: metadata at {u} names another resource; ignored");
                foreign = resource.map(str::to_string);
                continue;
            }
            prm = Some(doc);
            break;
        }
    }
    if prm.is_none() {
        if let Some(other) = foreign {
            return Err(format!("its sign-in details are for a different service ({other}), so maiTerm won't sign in to it"));
        }
    }
    let Some(prm) = prm else {
        return Ok(if refused {
            not_oauth(super::AUTH_TOKEN, "It asks for credentials but publishes no sign-in details. Paste a token (an API key or access token) from the service.")
        } else {
            not_oauth(super::AUTH_NONE, "No sign-in needed.")
        });
    };

    // Pick the first authorization server that can run the code flow with PKCE.
    let mut chosen: Option<(String, Value)> = None;
    for issuer in strs(prm.get("authorization_servers")) {
        for mu in as_metadata_urls(&issuer) {
            if let Some(meta) = get_json(&client, &mu).await {
                if usable(&meta) {
                    chosen = Some((issuer.clone(), meta));
                    break;
                }
            }
        }
        if chosen.is_some() {
            break;
        }
    }
    let Some((issuer, meta)) = chosen else {
        return Ok(not_oauth(super::AUTH_TOKEN, "Its sign-in service doesn't offer a flow maiTerm can use. Paste a token from the service instead."));
    };
    Ok(oauth_probe(server_url, &challenge, &prm, issuer, &meta))
}

fn not_oauth(auth: &str, detail: &str) -> Probe {
    Probe { auth: auth.into(), detail: detail.into(), registration: REG_NONE.into(), ..Default::default() }
}

/// How a sign-in at this authorization server gets a client id. The metadata document comes
/// first where both are offered: nothing is written on the server, the client id is the same on
/// every launch (a dynamic registration names maiTerm's port, so it is redone on each), and the
/// human sees maiTerm's own name and site on the consent screen.
fn registration_kind(meta: &Value) -> &'static str {
    if meta.get("client_id_metadata_document_supported").and_then(Value::as_bool) == Some(true) {
        REG_METADATA_DOCUMENT
    } else if meta.get("registration_endpoint").and_then(Value::as_str).is_some() {
        REG_DYNAMIC
    } else {
        REG_NONE
    }
}

/// How a stored plan gets its client id. A plan saved before `registration` existed says so by
/// its registration endpoint alone.
pub fn plan_registration(plan: &Value) -> &str {
    match plan.get("registration").and_then(Value::as_str) {
        Some(k) => k,
        None if plan.get("registration_endpoint").and_then(Value::as_str).is_some() => REG_DYNAMIC,
        None => REG_NONE,
    }
}

/// Does a sign-in under this plan use maiTerm's metadata document? Not when the human gave a
/// client id: theirs wins, on the usual redirect.
fn uses_metadata_document(plan: &Value) -> bool {
    plan.get("manual_client_id").and_then(Value::as_str).is_none() && plan_registration(plan) == REG_METADATA_DOCUMENT
}

/// The OAuth probe once an authorization server is chosen: what the form says, and the plan
/// `begin` uses.
fn oauth_probe(server_url: &str, challenge: &HashMap<String, String>, prm: &Value, issuer: String, meta: &Value) -> Probe {
    // Scope, in the spec's order: the challenge, else everything the resource lists, else none.
    let mut scope: Vec<String> = match challenge.get("scope") {
        Some(s) => s.split_whitespace().map(str::to_string).collect(),
        None => strs(prm.get("scopes_supported")),
    };
    if strs(meta.get("scopes_supported")).iter().any(|s| s == "offline_access") && !scope.iter().any(|s| s == "offline_access") {
        scope.push("offline_access".into());
    }
    let registration_endpoint = meta.get("registration_endpoint").and_then(Value::as_str).map(str::to_string);
    let registration = registration_kind(meta);
    let host = reqwest::Url::parse(&issuer).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| issuer.clone());
    let plan = json!({
        "resource": prm.get("resource").and_then(Value::as_str).unwrap_or(server_url),
        "issuer": issuer,
        "authorization_endpoint": meta["authorization_endpoint"],
        "token_endpoint": meta["token_endpoint"],
        "registration_endpoint": registration_endpoint,
        "registration": registration,
        "scope": if scope.is_empty() { Value::Null } else { Value::String(scope.join(" ")) },
    });
    Probe {
        auth: super::AUTH_OAUTH.into(),
        detail: if registration != REG_NONE {
            format!("Sign in with {host}.")
        } else {
            format!("Sign in with {host}. It doesn't let apps register themselves, so it needs a client id from an app you registered there, or paste a token instead.")
        },
        issuer: Some(issuer),
        can_register: registration != REG_NONE,
        registration: registration.into(),
        plan: Some(plan),
        ..Default::default()
    }
}

// ─── The browser round trip ────────────────────────────────────────────────

struct Pending {
    server_id: String,
    /// Which server of that name the sign-in is for (`ProxyServer::added_at`).
    server_added_at: u64,
    verifier: String,
    redirect_uri: String,
    token_endpoint: String,
    client_id: String,
    client_secret: Option<String>,
    /// A registration made for this sign-in. Written to the server's config only when the sign-in
    /// completes: written at once, an abandoned sign-in left the config naming a client the
    /// stored refresh token wasn't issued to.
    fresh_registration: bool,
    resource: String,
    created: u64,
}

/// Forget a server's sign-ins in progress (it was removed).
pub fn forget(server_id: &str) {
    PENDING.lock().retain(|_, p| p.server_id != server_id);
}

static PENDING: LazyLock<Mutex<HashMap<String, Pending>>> = LazyLock::new(Default::default);

fn random(n: usize) -> String {
    rand::thread_rng().sample_iter(&rand::distributions::Alphanumeric).take(n).map(char::from).collect()
}

fn s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sha2::Sha256::digest(verifier.as_bytes()))
}

fn plan_str(server: &ProxyServer, key: &str) -> Option<String> {
    server.oauth.as_ref()?.get(key)?.as_str().map(str::to_string)
}

/// The client a sign-in uses: the human's, maiTerm's metadata document where the server takes
/// one, the stored registration when it is for exactly this redirect URI, else a new registration
/// (RFC 7591). Writes nothing: a new registration rides in `Pending` until the sign-in completes.
/// Returns `(client_id, client_secret, fresh)`.
async fn client_for(server: &ProxyServer, redirect_uri: &str) -> Result<(String, Option<String>, bool), String> {
    let plan = server.oauth.clone().unwrap_or(Value::Null);
    // A client id the human supplied is used as is, on any redirect.
    if let Some(id) = plan.get("manual_client_id").and_then(Value::as_str) {
        return Ok((id.to_string(), manual_client_secret(&server.id).await, false));
    }
    // The document's URL is the client id: a public client, nothing registered, nothing stored.
    if uses_metadata_document(&plan) {
        return Ok((CLIENT_METADATA_URL.to_string(), None, false));
    }
    if plan.get("redirect_uri").and_then(Value::as_str) == Some(redirect_uri) {
        if let Some(id) = plan.get("client_id").and_then(Value::as_str) {
            // Its secret, if any, travels with the tokens issued to it.
            let secret = super::cached_or_stored_cred(&server.id).await.ok().flatten().filter(|c| c.client_id.as_deref() == Some(id)).and_then(|c| c.client_secret);
            return Ok((id.to_string(), secret, false));
        }
    }
    let endpoint = plan
        .get("registration_endpoint")
        .and_then(Value::as_str)
        .ok_or("this server doesn't let apps register themselves: add a client id, or use a token")?;
    let mut body = json!({
        "client_name": crate::APP_DISPLAY_NAME,
        "redirect_uris": [redirect_uri],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "token_endpoint_auth_method": "none",
    });
    if let Some(s) = plan.get("scope").and_then(Value::as_str) {
        body["scope"] = s.into();
    }
    let resp = http().post(endpoint).json(&body).send().await.map_err(|e| format!("registration failed: {}", e.without_url()))?;
    let status = resp.status();
    let doc: Value = resp.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        let why = doc.get("error_description").or_else(|| doc.get("error")).and_then(Value::as_str).unwrap_or("");
        return Err(format!("registration refused ({status}) {why}").trim().to_string());
    }
    let client_id = doc.get("client_id").and_then(Value::as_str).ok_or("registration returned no client id")?.to_string();
    // A secret the server issued anyway is still a secret: it is stored with the tokens, in the
    // keychain, once the sign-in completes.
    let client_secret = doc.get("client_secret").and_then(Value::as_str).map(str::to_string);
    log::info!("[mcp-proxy] {}: registered with {}", server.id, plan.get("issuer").and_then(Value::as_str).unwrap_or("?"));
    Ok((client_id, client_secret, true))
}

/// The keychain item a human-supplied client secret is kept under, beside the server's tokens.
fn client_secret_key(server_id: &str) -> String {
    format!("{server_id}#client")
}

/// The human-supplied client secret, if any. Read on a blocking thread (keychain).
async fn manual_client_secret(server_id: &str) -> Option<String> {
    let key = client_secret_key(server_id);
    tokio::task::spawn_blocking(move || secrets::read(&key)).await.ok()?.ok()?.map(|c| c.access_token)
}

/// Start a sign-in: returns the URL the browser opens. The answer comes back to `callback`.
pub async fn begin(server_id: &str) -> Result<String, String> {
    let server = get(server_id).ok_or_else(|| format!("no MCP server named {server_id}"))?;
    if server.auth != super::AUTH_OAUTH {
        return Err(format!("{server_id} doesn't use a sign-in"));
    }
    let metadata_document = server.oauth.as_ref().is_some_and(uses_metadata_document);
    // A metadata-document sign-in comes back on its own listener, on a port the document lists,
    // which is bound below, with the sign-in recorded under the same lock (`callback_port`).
    let mcp_port = if metadata_document { None } else { Some(super::APP.get().and_then(|(_, st)| *st.mcp_port.read()).ok_or("maiTerm's server isn't running")?) };
    let redirect_uri = mcp_port.map(callback_uri).unwrap_or_default();
    let (client_id, client_secret, fresh_registration) = client_for(&server, &redirect_uri).await?;
    let server = get(server_id).ok_or("the server was removed")?;
    let authz = plan_str(&server, "authorization_endpoint").ok_or("no sign-in details stored; remove and add the server again")?;
    let token_endpoint = plan_str(&server, "token_endpoint").ok_or("no sign-in details stored")?;
    let resource = plan_str(&server, "resource").unwrap_or_else(|| server.url.clone());
    // A wider scope a server asked for (step-up) joins the planned one.
    let scope = match (plan_str(&server, "scope"), plan_str(&server, "step_up_scope")) {
        (Some(a), Some(b)) => Some(merge_scopes(&a, &b)),
        (a, b) => a.or(b),
    };

    let verifier = random(64);
    let state = random(32);
    let mut url = reqwest::Url::parse(&authz).map_err(|e| format!("bad authorization endpoint: {e}"))?;
    // Held to the end: the listener's sweep reads `PENDING` under it, so it can't stop between
    // here and the sign-in being recorded.
    let mut listener = CALLBACK.lock();
    let redirect_uri = if metadata_document {
        let issuer = plan_str(&server, "issuer").unwrap_or_default();
        callback_uri(callback_port(&mut listener, &issuer)?)
    } else {
        redirect_uri
    };
    {
        let mut q = url.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", &client_id)
            .append_pair("redirect_uri", &redirect_uri)
            .append_pair("code_challenge", &s256(&verifier))
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state)
            .append_pair("resource", &resource);
        if let Some(s) = &scope {
            q.append_pair("scope", s);
        }
    }
    let now = super::now_ms();
    let mut pending = PENDING.lock();
    pending.retain(|_, p| now.saturating_sub(p.created) < PENDING_TTL_MS && p.server_id != server_id);
    pending.insert(state, Pending { server_id: server_id.to_string(), server_added_at: server.added_at, verifier, redirect_uri, token_endpoint, client_id, client_secret, fresh_registration, resource, created: now });
    drop(pending);
    drop(listener);
    log::info!("[mcp-proxy] {server_id}: sign-in started");
    Ok(url.to_string())
}

fn callback_uri(port: u16) -> String {
    format!("http://127.0.0.1:{port}/oauth/callback")
}

// ─── The metadata document's own callback listener ─────────────────────────

/// The port of the listener metadata-document sign-ins come back to, while one runs. It serves
/// only `/oauth/callback`, and stops once no sign-in waits on it.
static CALLBACK: LazyLock<Mutex<Option<u16>>> = LazyLock::new(Default::default);

/// How often the listener looks for sign-ins still waiting on it.
const CALLBACK_SWEEP: Duration = Duration::from_secs(2);

/// The first of `ports` that `bind` takes, with what it bound. All taken is a sentence for the
/// human, naming the ports, since only they can free one.
fn first_free<T>(ports: &[u16], issuer_host: &str, mut bind: impl FnMut(u16) -> std::io::Result<T>) -> Result<(u16, T), String> {
    let mut last = None;
    for &port in ports {
        match bind(port) {
            Ok(l) => return Ok((port, l)),
            Err(e) => {
                log::info!("[mcp-proxy] sign-in callback: port {port} is not free: {e}");
                last = Some(e);
            }
        }
    }
    let list = ports.iter().map(u16::to_string).collect::<Vec<_>>();
    let named = match list.split_last() {
        Some((tail, head)) if !head.is_empty() => format!("{} or {tail}", head.join(", ")),
        _ => list.join(""),
    };
    log::warn!("[mcp-proxy] sign-in callback: no port free ({})", last.map(|e| e.to_string()).unwrap_or_default());
    Err(format!(
        "Signing in to {issuer_host} needs one of the ports {named} on this computer, and other programs are using all of them. Quit one of those programs and sign in again."
    ))
}

/// The port metadata-document sign-ins come back to: the running listener's, else a new
/// listener's on the first free port of `CIMD_PORTS`. Called under `CALLBACK`, which `begin`
/// holds until its sign-in is in `PENDING`.
fn callback_port(running: &mut Option<u16>, issuer: &str) -> Result<u16, String> {
    if let Some(port) = *running {
        return Ok(port);
    }
    let host = reqwest::Url::parse(issuer).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| "this service".into());
    let (port, std_listener) = first_free(&CIMD_PORTS, &host, |p| std::net::TcpListener::bind(("127.0.0.1", p)))?;
    std_listener.set_nonblocking(true).map_err(|e| format!("maiTerm could not listen for the sign-in: {e}"))?;
    let listener = tokio::net::TcpListener::from_std(std_listener).map_err(|e| format!("maiTerm could not listen for the sign-in: {e}"))?;
    let app = axum::Router::new()
        .route("/oauth/callback", axum::routing::get(callback_handler))
        // Only requests addressed to this computer, as on maiTerm's own server.
        .layer(axum::middleware::from_fn(crate::host_guard::require_local_host));
    tokio::spawn(async move {
        let stopped = async move {
            loop {
                tokio::time::sleep(CALLBACK_SWEEP).await;
                let mut running = CALLBACK.lock();
                let now = super::now_ms();
                let waiting = PENDING.lock().values().any(|p| p.redirect_uri == callback_uri(port) && now.saturating_sub(p.created) < PENDING_TTL_MS);
                if !waiting {
                    // Under the lock: a `begin` after this binds a listener of its own.
                    if *running == Some(port) {
                        *running = None;
                    }
                    return;
                }
            }
        };
        if let Err(e) = axum::serve(listener, app).with_graceful_shutdown(stopped).await {
            log::warn!("[mcp-proxy] sign-in callback listener on {port}: {e}");
            let mut running = CALLBACK.lock();
            if *running == Some(port) {
                *running = None;
            }
        }
        log::info!("[mcp-proxy] sign-in callback listener on {port} stopped");
    });
    log::info!("[mcp-proxy] sign-in callback listening on 127.0.0.1:{port}");
    *running = Some(port);
    Ok(port)
}

async fn callback_handler(axum::extract::Query(params): axum::extract::Query<HashMap<String, String>>) -> axum::response::Response {
    let (status, html) = callback(params).await;
    axum::response::Response::builder()
        .status(status)
        .header(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")
        .header(axum::http::header::CACHE_CONTROL, "no-store")
        .body(axum::body::Body::from(html))
        .unwrap_or_default()
}

fn merge_scopes(a: &str, b: &str) -> String {
    let mut out: Vec<&str> = a.split_whitespace().collect();
    for s in b.split_whitespace() {
        if !out.contains(&s) {
            out.push(s);
        }
    }
    out.join(" ")
}

/// The browser came back to `/oauth/callback`. Returns the page to show there.
pub async fn callback(params: HashMap<String, String>) -> (u16, String) {
    let Some(state) = params.get("state") else { return (400, page("Sign-in failed", "The answer was missing its state.")) };
    let Some(p) = PENDING.lock().remove(state) else {
        return (400, page("Sign-in expired", "This sign-in is unknown or too old. Start it again from maiTerm."));
    };
    if super::now_ms().saturating_sub(p.created) > PENDING_TTL_MS {
        return (400, page("Sign-in expired", "Start it again from maiTerm."));
    }
    if let Some(err) = params.get("error") {
        let why = params.get("error_description").cloned().unwrap_or_else(|| err.clone());
        log::warn!("[mcp-proxy] {}: sign-in refused: {err}", p.server_id);
        super::set_status(&p.server_id, super::Status::new("needs_sign_in", format!("the sign-in was refused: {why}")));
        return (400, page("Sign-in refused", &why));
    }
    let Some(code) = params.get("code") else { return (400, page("Sign-in failed", "No code came back.")) };
    let mut form = vec![
        ("grant_type", "authorization_code".to_string()),
        ("code", code.clone()),
        ("redirect_uri", p.redirect_uri.clone()),
        ("client_id", p.client_id.clone()),
        ("code_verifier", p.verifier.clone()),
        ("resource", p.resource.clone()),
    ];
    if let Some(s) = &p.client_secret {
        form.push(("client_secret", s.clone()));
    }
    // Under the server's refresh turn: a refresh in flight must not store the old sign-in over
    // this one.
    let turn = REFRESHING.lock().entry(p.server_id.clone()).or_default().clone();
    let _turn = turn.lock().await;
    match token_request(&p.token_endpoint, &form).await {
        Ok(mut cred) => {
            cred.client_id = Some(p.client_id.clone());
            // A human-supplied client's secret stays in its own item; a registered one's goes
            // with the tokens it was issued.
            let manual = get(&p.server_id).is_some_and(|s| plan_str(&s, "manual_client_id").is_some());
            cred.client_secret = if manual { None } else { p.client_secret.clone() };
            if let Err(e) = store_cred(&p.server_id, p.server_added_at, cred).await {
                return (500, page("Sign-in failed", &format!("maiTerm could not keep the sign-in: {e}")));
            }
            let (fresh, cid, ru) = (p.fresh_registration, p.client_id.clone(), p.redirect_uri.clone());
            let _ = update(&p.server_id, |s| {
                let o = s.oauth.get_or_insert_with(|| json!({}));
                o["signed_in_at"] = super::now_ms().into();
                if fresh {
                    o["client_id"] = cid.into();
                    o["redirect_uri"] = ru.into();
                    o["registered_at"] = super::now_ms().into();
                }
                if let Some(m) = o.as_object_mut() {
                    m.remove("step_up_scope");
                }
            });
            log::info!("[mcp-proxy] {}: signed in", p.server_id);
            super::signed_in(&p.server_id);
            (200, page("Signed in", &format!("maiTerm is signed in to {}. You can close this tab.", p.server_id)))
        }
        Err(e) => {
            super::set_status(&p.server_id, super::Status::new("needs_sign_in", format!("the sign-in failed: {e}")));
            (502, page("Sign-in failed", &e))
        }
    }
}

fn page(title: &str, body: &str) -> String {
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    format!(
        "<!doctype html><meta charset=utf-8><title>{t}</title><body style=\"font:16px system-ui;margin:4em auto;max-width:32em;color:#c0caf5;background:#1a1b26\"><h1 style=\"font-size:1.4em\">{t}</h1><p>{b}</p></body>",
        t = esc(title),
        b = esc(body)
    )
}

/// POST a token request; any answer without an access token is an error.
async fn token_request(endpoint: &str, form: &[(&str, String)]) -> Result<Cred, String> {
    // reqwest is built without its `form` feature; the URL type's serializer encodes the same.
    let mut enc = reqwest::Url::parse("http://form.invalid/").expect("static URL");
    enc.query_pairs_mut().extend_pairs(form.iter().map(|(k, v)| (*k, v.as_str())));
    let body = enc.query().unwrap_or_default().to_string();
    let resp = http()
        .post(endpoint)
        .header("accept", "application/json")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .map_err(|e| format!("the token service didn't answer: {}", e.without_url()))?;
    let status = resp.status();
    let doc: Value = resp.json().await.unwrap_or(Value::Null);
    let Some(access) = doc.get("access_token").and_then(Value::as_str) else {
        let why = doc.get("error_description").or_else(|| doc.get("error")).and_then(Value::as_str).unwrap_or("no token in the answer");
        return Err(format!("{why} ({status})"));
    };
    let now = super::now_ms();
    // `expires_in` is a number by the RFC; some servers send it as a string.
    let expires_in = doc.get("expires_in").and_then(|v| v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())));
    Ok(Cred {
        access_token: access.to_string(),
        refresh_token: doc.get("refresh_token").and_then(Value::as_str).map(str::to_string),
        expires_at: expires_in.map(|s| now + s * 1000),
        scope: doc.get("scope").and_then(Value::as_str).map(str::to_string),
        obtained_at: Some(now),
        ..Default::default()
    })
}

/// Keep a credential: the keychain first, then the cache. Only for the same server the request
/// was made for: one removed meanwhile, or removed and added again under the same name (perhaps
/// another URL), gets nothing — a late answer would otherwise hand one service's token to the
/// next server called that. A server is told apart by when it was added.
async fn store_cred(server_id: &str, added_at: u64, cred: Cred) -> Result<(), String> {
    if !get(server_id).is_some_and(|s| s.added_at == added_at) {
        return Err("the server was removed".into());
    }
    let id = server_id.to_string();
    let c = cred.clone();
    tokio::task::spawn_blocking(move || secrets::store(&id, &c)).await.map_err(|e| e.to_string())?.map_err(|e| e.to_string())?;
    super::cache_cred(server_id, cred);
    Ok(())
}

// ─── Keeping the token fresh ───────────────────────────────────────────────

static REFRESHING: LazyLock<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(Default::default);

/// A usable access token for an OAuth server: the stored one, refreshed first when it is about to
/// expire, or now when the server just `refused` this token.
pub async fn access_token(server: &ProxyServer, refused: Option<&str>) -> Result<String, String> {
    let turn = REFRESHING.lock().entry(server.id.clone()).or_default().clone();
    let _turn = turn.lock().await;
    // Read under the turn: a refresh that just finished has already stored its token.
    let cred = super::cached_or_stored_cred(&server.id).await?.ok_or("not signed in yet")?;
    // Several calls refused together: the first one's refresh serves the rest.
    let force = refused.is_some_and(|r| r == cred.access_token);
    if refused.is_some() && !force {
        return Ok(cred.access_token);
    }
    if !force && !refresh_due(&cred, super::now_ms()) {
        return Ok(cred.access_token);
    }
    let Some(refresh) = cred.refresh_token.clone() else {
        return if force || cred.expires_at.is_some_and(|t| super::now_ms() >= t) {
            Err("the sign-in has expired; sign in again".into())
        } else {
            Ok(cred.access_token)
        };
    };
    let token_endpoint = plan_str(server, "token_endpoint").ok_or("no sign-in details stored")?;
    // The client the refresh token was issued to, which travels with it.
    let client_id = cred
        .client_id
        .clone()
        .or_else(|| plan_str(server, "manual_client_id"))
        .or_else(|| plan_str(server, "client_id"))
        .ok_or("no client registration stored")?;
    let mut form = vec![
        ("grant_type", "refresh_token".to_string()),
        ("refresh_token", refresh.clone()),
        ("client_id", client_id),
        ("resource", plan_str(server, "resource").unwrap_or_else(|| server.url.clone())),
    ];
    let secret = match cred.client_secret.clone() {
        Some(s) => Some(s),
        None => manual_client_secret(&server.id).await,
    };
    if let Some(secret) = secret {
        form.push(("client_secret", secret));
    }
    match token_request(&token_endpoint, &form).await {
        Ok(mut next) => {
            // A server that doesn't rotate sends no new refresh token: the old one stays good.
            if next.refresh_token.is_none() {
                next.refresh_token = Some(refresh);
            }
            if next.scope.is_none() {
                next.scope = cred.scope.clone();
            }
            next.client_id = cred.client_id.clone();
            next.client_secret = cred.client_secret.clone();
            let tok = next.access_token.clone();
            store_cred(&server.id, server.added_at, next).await?;
            log::info!("[mcp-proxy] {}: token refreshed", server.id);
            Ok(tok)
        }
        Err(e) => Err(format!("the sign-in could not be renewed ({e}); sign in again")),
    }
}

/// Is it time to refresh? `REFRESH_EARLY_MS` before expiry, but never more than half the
/// token's life early: a token that lives under a minute would otherwise be refreshed on every
/// use, queueing every call behind a token request.
fn refresh_due(cred: &Cred, now: u64) -> bool {
    let Some(expires) = cred.expires_at else { return false };
    let early = match cred.obtained_at {
        Some(got) => REFRESH_EARLY_MS.min(expires.saturating_sub(got) / 2),
        None => REFRESH_EARLY_MS,
    };
    now + early >= expires
}

/// A 403 asked for more access: remember the scope so the next sign-in asks for it.
pub fn note_step_up(server_id: &str, challenge: Option<&str>) {
    let Some(scope) = challenge.map(challenge_params).and_then(|p| {
        (p.get("error").map(String::as_str) == Some("insufficient_scope")).then(|| p.get("scope").cloned()).flatten()
    }) else {
        return;
    };
    let _ = update(server_id, |s| {
        let o = s.oauth.get_or_insert_with(|| json!({}));
        o["step_up_scope"] = scope.into();
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenges_parse_quoted_and_bare_values() {
        let p = challenge_params(r#"Bearer resource_metadata="https://mcp.x/.well-known/oauth-protected-resource/mcp", scope="read write", error=invalid_token"#);
        assert_eq!(p["resource_metadata"], "https://mcp.x/.well-known/oauth-protected-resource/mcp");
        assert_eq!(p["scope"], "read write");
        assert_eq!(p["error"], "invalid_token");
        let p = challenge_params(r#"Bearer error="insufficient_scope", scope="a \"b\"""#);
        assert_eq!(p["scope"], r#"a "b""#);
        assert!(challenge_params("Bearer").is_empty());
    }

    #[test]
    fn well_known_urls_follow_the_spec_order() {
        let u = reqwest::Url::parse("https://mcp.linear.app/mcp").unwrap();
        assert_eq!(well_known(&u, "oauth-protected-resource", true), "https://mcp.linear.app/.well-known/oauth-protected-resource/mcp");
        assert_eq!(well_known(&u, "oauth-protected-resource", false), "https://mcp.linear.app/.well-known/oauth-protected-resource");
        assert_eq!(
            as_metadata_urls("https://github.com/login/oauth"),
            vec![
                "https://github.com/.well-known/oauth-authorization-server/login/oauth",
                "https://github.com/.well-known/openid-configuration/login/oauth",
                "https://github.com/login/oauth/.well-known/openid-configuration",
            ]
        );
        assert_eq!(as_metadata_urls("https://mcp.linear.app")[0], "https://mcp.linear.app/.well-known/oauth-authorization-server");
    }

    #[test]
    fn a_server_without_pkce_s256_is_not_used() {
        let ok = json!({ "authorization_endpoint": "a", "token_endpoint": "t", "code_challenge_methods_supported": ["S256"] });
        assert!(usable(&ok));
        let no_pkce = json!({ "authorization_endpoint": "a", "token_endpoint": "t" });
        assert!(!usable(&no_pkce));
        let device_only = json!({ "authorization_endpoint": "a", "token_endpoint": "t", "code_challenge_methods_supported": ["S256"], "grant_types_supported": ["urn:ietf:params:oauth:grant-type:device_code"] });
        assert!(!usable(&device_only));
    }

    #[test]
    fn pkce_challenge_is_rfc7636() {
        // BASE64URL(SHA256(verifier)) without padding, computed independently with Python's
        // hashlib + base64.urlsafe_b64encode.
        assert_eq!(s256("dBjftJeZ4CVP-mJ92K9qBZFlC5qfk0Uvk8jFn5jT8wc"), "SL6jy0yV4Zbb4o-F_7dA13V9behyA7J5bLglqVGJRBc");
    }

    #[test]
    fn metadata_must_be_about_this_server() {
        let s = reqwest::Url::parse("https://mcp.linear.app/mcp").unwrap();
        assert!(resource_matches(Some("https://mcp.linear.app/mcp"), &s));
        assert!(resource_matches(Some("https://mcp.linear.app/mcp/"), &s));
        assert!(resource_matches(Some("https://mcp.linear.app"), &s), "a root resource covers its paths (langsys.dev)");
        assert!(resource_matches(None, &s));
        assert!(!resource_matches(Some("https://evil.example/mcp"), &s));
        assert!(!resource_matches(Some("https://mcp.linear.app/other"), &s));
        assert!(!resource_matches(Some("https://mcp.linear.app/mc"), &s), "a prefix must end at a path segment");
        assert!(!resource_matches(Some("http://mcp.linear.app/mcp"), &s), "another scheme is another origin");
    }

    #[test]
    fn short_tokens_refresh_at_half_life_not_on_every_use() {
        let c = |got: u64, exp: u64| Cred { access_token: "a".into(), obtained_at: Some(got), expires_at: Some(exp), ..Default::default() };
        // A 6 s token: due from 3 s on, not at once.
        assert!(!refresh_due(&c(0, 6_000), 1_000));
        assert!(refresh_due(&c(0, 6_000), 3_000));
        // An hour-long token: due a minute before expiry.
        assert!(!refresh_due(&c(0, 3_600_000), 3_500_000));
        assert!(refresh_due(&c(0, 3_600_000), 3_540_000));
        // No expiry: never proactively.
        assert!(!refresh_due(&Cred { access_token: "a".into(), ..Default::default() }, u64::MAX / 2));
    }

    #[test]
    fn step_up_scopes_merge_without_repeats() {
        assert_eq!(merge_scopes("read write", "write admin"), "read write admin");
    }

    // ElevenLabs' documents as fetched by the coordinator on 2026-11-14 (its hosted MCP at
    // https://api.elevenlabs.io/v1/mcp answers 401 with only `resource_metadata` in the challenge).
    const ELEVENLABS_MCP: &str = "https://api.elevenlabs.io/v1/mcp";

    fn elevenlabs_prm() -> Value {
        json!({
            "resource": "https://api.elevenlabs.io/v1/mcp",
            "authorization_servers": ["https://api.us.elevenlabs.io"],
            "scopes_supported": ["convai_read", "convai_write", "text_to_speech", "speech_history_read", "flows", "image_video_generation", "voice_generation", "ads_read", "ads_write", "user_read", "pronunciation_dictionaries_read"],
            "bearer_methods_supported": ["header"]
        })
    }

    fn elevenlabs_as() -> Value {
        json!({
            "issuer": "https://api.us.elevenlabs.io",
            "authorization_endpoint": "https://elevenlabs.io/app/oauth/authorize",
            "token_endpoint": "https://api.us.elevenlabs.io/v1/oauth/token",
            "revocation_endpoint": "https://api.us.elevenlabs.io/v1/oauth/revoke",
            "response_types_supported": ["code"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": ["none", "client_secret_post", "private_key_jwt"],
            "scopes_supported": ["convai_read", "convai_write", "text_to_speech", "speech_history_read", "flows", "image_video_generation", "voice_generation", "ads_read", "ads_write", "user_read", "pronunciation_dictionaries_read"],
            "authorization_response_iss_parameter_supported": true,
            "client_id_metadata_document_supported": true,
            "token_endpoint_auth_signing_alg_values_supported": ["RS256"]
        })
    }

    fn server_with(plan: Value) -> ProxyServer {
        ProxyServer {
            id: "elevenlabs".into(),
            url: ELEVENLABS_MCP.into(),
            auth: crate::mcp_proxy::AUTH_OAUTH.into(),
            enabled: true,
            workspaces: vec![],
            catalog: None,
            oauth: Some(plan),
            added_at: 1,
            added_by: "human".into(),
            transport: crate::mcp_proxy::stdio::TRANSPORT_HTTP.into(),
            stdio: None,
        }
    }

    #[test]
    fn elevenlabs_is_signable_with_the_metadata_document() {
        let meta = elevenlabs_as();
        assert!(usable(&meta));
        let prm = elevenlabs_prm();
        // Its resource is the server itself, so the foreign-metadata rule lets it through; the
        // authorization server on another host is not what that rule judges.
        assert!(resource_matches(prm["resource"].as_str(), &reqwest::Url::parse(ELEVENLABS_MCP).unwrap()));
        let p = oauth_probe(ELEVENLABS_MCP, &HashMap::new(), &prm, "https://api.us.elevenlabs.io".into(), &meta);
        assert_eq!(p.auth, crate::mcp_proxy::AUTH_OAUTH);
        assert_eq!(p.registration, REG_METADATA_DOCUMENT);
        assert!(p.can_register, "old callers read can_register: it must not dead-end");
        assert_eq!(p.detail, "Sign in with api.us.elevenlabs.io.");
        let plan = p.plan.unwrap();
        assert_eq!(plan["registration"], REG_METADATA_DOCUMENT);
        assert_eq!(plan["registration_endpoint"], Value::Null);
        assert_eq!(plan["resource"], ELEVENLABS_MCP);
        assert_eq!(plan["authorization_endpoint"], "https://elevenlabs.io/app/oauth/authorize");
        assert_eq!(plan["token_endpoint"], "https://api.us.elevenlabs.io/v1/oauth/token");
        // No scope in the challenge: everything the resource lists, as for every server.
        assert_eq!(plan["scope"].as_str().unwrap().split(' ').count(), 11);
        assert_eq!(plan_registration(&plan), REG_METADATA_DOCUMENT);
        assert!(uses_metadata_document(&plan));
    }

    #[test]
    fn registration_kinds_prefer_the_metadata_document() {
        let both = json!({ "registration_endpoint": "https://as/register", "client_id_metadata_document_supported": true });
        assert_eq!(registration_kind(&both), REG_METADATA_DOCUMENT, "nothing written on the server, same client id every launch");
        assert_eq!(registration_kind(&json!({ "registration_endpoint": "https://as/register" })), REG_DYNAMIC);
        assert_eq!(registration_kind(&json!({ "registration_endpoint": "https://as/register", "client_id_metadata_document_supported": false })), REG_DYNAMIC);
        assert_eq!(registration_kind(&json!({})), REG_NONE);
        assert_eq!(registration_kind(&json!({ "client_id_metadata_document_supported": "true" })), REG_NONE, "only a JSON true counts");

        // A server with neither still dead-ends to a client id, as before.
        let meta = json!({ "authorization_endpoint": "a", "token_endpoint": "t", "code_challenge_methods_supported": ["S256"] });
        let p = oauth_probe("https://x/mcp", &HashMap::new(), &json!({}), "https://github.com/login/oauth".into(), &meta);
        assert_eq!((p.registration.as_str(), p.can_register), (REG_NONE, false));
        assert!(p.detail.contains("client id"));
        let mut dcr = meta.clone();
        dcr["registration_endpoint"] = "https://x/register".into();
        let p = oauth_probe("https://x/mcp", &HashMap::new(), &json!({}), "https://x".into(), &dcr);
        assert_eq!((p.registration.as_str(), p.can_register), (REG_DYNAMIC, true));
        assert_eq!(p.plan.unwrap()["registration_endpoint"], "https://x/register");
    }

    #[test]
    fn plans_saved_before_registration_existed_read_as_before() {
        assert_eq!(plan_registration(&json!({ "registration_endpoint": "https://x/register" })), REG_DYNAMIC);
        assert_eq!(plan_registration(&json!({ "registration_endpoint": null })), REG_NONE);
        assert_eq!(plan_registration(&json!({})), REG_NONE);
        // The human's client id wins over the document, on the usual redirect.
        assert!(!uses_metadata_document(&json!({ "registration": REG_METADATA_DOCUMENT, "manual_client_id": "abc" })));
        assert!(!uses_metadata_document(&json!({ "registration": REG_DYNAMIC })));
    }

    #[tokio::test]
    async fn a_metadata_document_server_signs_in_as_the_document_url() {
        // The registration endpoint is unreachable: a client_for that registered would fail.
        let plan = json!({ "registration": REG_METADATA_DOCUMENT, "registration_endpoint": "http://127.0.0.1:9/register", "issuer": "https://api.us.elevenlabs.io" });
        let (id, secret, fresh) = client_for(&server_with(plan), &callback_uri(CIMD_PORTS[0])).await.unwrap();
        assert_eq!(id, CLIENT_METADATA_URL);
        assert_eq!(secret, None, "a public client");
        assert!(!fresh, "nothing to record as a registration");
        // The same plan saved before `registration` existed still registers dynamically.
        let legacy = json!({ "registration_endpoint": "http://127.0.0.1:9/register", "issuer": "https://x" });
        let err = client_for(&server_with(legacy), "http://127.0.0.1:1234/oauth/callback").await.unwrap_err();
        assert!(err.starts_with("registration"), "{err}");
    }

    #[test]
    fn the_callback_port_is_the_first_free_of_the_fixed_set() {
        let taken = |busy: &'static [u16]| {
            move |p: u16| if busy.contains(&p) { Err(std::io::Error::from(std::io::ErrorKind::AddrInUse)) } else { Ok(p) }
        };
        assert_eq!(first_free(&CIMD_PORTS, "h", taken(&[])).unwrap().0, CIMD_PORTS[0]);
        assert_eq!(first_free(&CIMD_PORTS, "h", taken(&[21847])).unwrap().0, CIMD_PORTS[1]);
        assert_eq!(first_free(&CIMD_PORTS, "h", taken(&[21847, 26413])).unwrap().0, CIMD_PORTS[2]);
        let mut tried = vec![];
        let err = first_free(&CIMD_PORTS, "api.us.elevenlabs.io", |p| {
            tried.push(p);
            Err::<(), _>(std::io::Error::from(std::io::ErrorKind::AddrInUse))
        })
        .unwrap_err();
        assert_eq!(tried, CIMD_PORTS, "only the fixed ports, in order: never a random one");
        assert_eq!(
            err,
            "Signing in to api.us.elevenlabs.io needs one of the ports 21847, 26413 or 29581 on this computer, and other programs are using all of them. Quit one of those programs and sign in again."
        );
        // A running listener is reused, with nothing bound.
        assert_eq!(callback_port(&mut Some(26413), "https://api.us.elevenlabs.io"), Ok(26413));
    }

    /// The document maiterm.dev publishes is the one this code signs in as: its client id is its
    /// own URL, and it lists every redirect a sign-in may use.
    #[test]
    fn the_published_client_metadata_document_matches_the_code() {
        let site = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../website/public");
        let url = reqwest::Url::parse(CLIENT_METADATA_URL).unwrap();
        assert_eq!(url.scheme(), "https");
        let cname = std::fs::read_to_string(site.join("CNAME")).unwrap();
        assert_eq!(url.host_str(), Some(cname.trim()), "served from the site's own domain");
        let raw = std::fs::read_to_string(site.join(url.path().trim_start_matches('/'))).unwrap();
        let doc: Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(doc["client_id"], CLIENT_METADATA_URL, "the client id must equal the document's URL");
        // One document for dev and release builds: the consent screen names the product.
        assert_eq!(doc["client_name"], "maiTerm");
        assert_eq!(doc["token_endpoint_auth_method"], "none");
        assert!(doc.get("client_secret").is_none());
        assert_eq!(strs(doc.get("response_types")), vec!["code"]);
        let grants = strs(doc.get("grant_types"));
        assert!(grants.contains(&"authorization_code".to_string()) && grants.contains(&"refresh_token".to_string()), "{grants:?}");
        let redirects = strs(doc.get("redirect_uris"));
        assert!(redirects.contains(&"http://127.0.0.1/oauth/callback".to_string()), "the port-less loopback form (RFC 8252 §7.3)");
        for port in CIMD_PORTS {
            assert!(redirects.contains(&callback_uri(port)), "{port} missing from {redirects:?}");
        }
        assert_eq!(redirects.len(), CIMD_PORTS.len() + 1, "nothing the code never uses");
    }
}
