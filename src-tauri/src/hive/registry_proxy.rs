//! The package registries an install job reaches (docs/hive.md §6.8; first real use,
//! 2026-10-26): an HTTPS CONNECT proxy on loopback that opens a tunnel only to a registry its
//! caller's grant names, on 443. Seatbelt names no host, only `localhost` or every address, so
//! the job's profile reaches this port and nothing else, and the host is checked here, as
//! Claude's sandbox does for a worker's own shell. A worker's hosts minus any git remote's, so
//! a job still reaches no remote and pushes nowhere (§7.3).
//!
//! A grant is a random token in the proxy url's user part (`http://<token>:x@127.0.0.1:<port>`),
//! which every package manager sends as `Proxy-Authorization`, and it ends with its job: another
//! local process that finds the port has no token, and one that reads a finished job's has nothing.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use base64::Engine as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// The only port a grant opens in production: the registries speak HTTPS.
pub const REGISTRY_PORT: u16 = 443;
const HEAD_MAX: usize = 8 * 1024;
const HEAD_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The hosts beside a worker's registries that an install reaches: yarn's own front of npm's.
pub const INSTALL_EXTRA_HOSTS: &[&str] = &["registry.yarnpkg.com", "repo.yarnpkg.com"];

struct Grant {
    job: String,
    hosts: Vec<String>,
    port: u16,
}

static GRANTS: LazyLock<Mutex<HashMap<String, Grant>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
/// The proxy's port, once it listens. Started on the first grant, for the app's life.
static LISTENING: Mutex<Option<u16>> = Mutex::new(None);

/// What a job's line sets: the proxy for every package manager's way of naming one.
pub fn env(url: &str) -> Vec<(String, String)> {
    ["HTTPS_PROXY", "HTTP_PROXY", "https_proxy", "http_proxy", "npm_config_https_proxy", "npm_config_proxy", "YARN_HTTPS_PROXY", "YARN_HTTP_PROXY"]
        .iter()
        .map(|k| (k.to_string(), url.to_string()))
        .collect()
}

/// The hosts an install job of a Hive reaches: the worker's registries (never a generator's) and
/// yarn's, minus every git remote's host, which `settings::worker_domains` takes out.
pub fn install_hosts(hive: &super::record::Hive, remote_hosts: &[String]) -> Vec<String> {
    let mut out = super::settings::worker_domains(hive, remote_hosts, false, false);
    for h in INSTALL_EXTRA_HOSTS {
        let covered = remote_hosts.iter().any(|r| super::gallery::budget::host_covers(h, &r.to_ascii_lowercase()));
        if !covered && !out.iter().any(|o| o == h) {
            out.push(h.to_string());
        }
    }
    out
}

/// A grant for `job` to reach `hosts` on 443, and the proxy url its line sets. Starts the proxy
/// on first use; needs the async runtime (a Tauri app's, or a test's).
pub fn open(job: &str, hosts: Vec<String>) -> Result<(u16, String), String> {
    open_on(job, hosts, REGISTRY_PORT)
}

pub(crate) fn open_on(job: &str, hosts: Vec<String>, port: u16) -> Result<(u16, String), String> {
    let listening = listen()?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let hosts = hosts.into_iter().map(|h| h.to_ascii_lowercase()).collect();
    GRANTS.lock().unwrap_or_else(|e| e.into_inner()).insert(token.clone(), Grant { job: job.to_string(), hosts, port });
    Ok((listening, format!("http://{token}:x@127.0.0.1:{listening}")))
}

/// Ends `job`'s grants: from its end, and from a start that failed after `open`.
pub fn close(job: &str) {
    GRANTS.lock().unwrap_or_else(|e| e.into_inner()).retain(|_, g| g.job != job);
}

fn listen() -> Result<u16, String> {
    let mut at = LISTENING.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = *at {
        return Ok(p);
    }
    let l = std::net::TcpListener::bind("127.0.0.1:0").map_err(|e| format!("The package registry proxy couldn't listen: {e}"))?;
    l.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = l.local_addr().map_err(|e| e.to_string())?.port();
    tauri::async_runtime::spawn(async move {
        let l = match tokio::net::TcpListener::from_std(l) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("hive: registry proxy lost its listener: {e}");
                *LISTENING.lock().unwrap_or_else(|e| e.into_inner()) = None;
                return;
            }
        };
        loop {
            match l.accept().await {
                Ok((s, _)) => {
                    tauri::async_runtime::spawn(serve(s));
                }
                Err(e) => log::warn!("hive: registry proxy accept: {e}"),
            }
        }
    });
    log::info!("hive: registry proxy listening on 127.0.0.1:{port}");
    *at = Some(port);
    Ok(port)
}

/// Why a CONNECT is refused, as its status line, or the host and port to tunnel to.
pub(crate) fn decide(head: &str) -> Result<(String, u16), &'static str> {
    let mut lines = head.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    if first.next() != Some("CONNECT") {
        return Err("405 Method Not Allowed");
    }
    let target = first.next().unwrap_or_default();
    let token = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.trim().eq_ignore_ascii_case("proxy-authorization"))
        .and_then(|(_, v)| v.trim().strip_prefix("Basic ").map(str::trim).map(str::to_string))
        .and_then(|b| base64::engine::general_purpose::STANDARD.decode(b).ok())
        .and_then(|raw| String::from_utf8(raw).ok())
        .map(|creds| creds.split(':').next().unwrap_or_default().to_string());
    let Some(token) = token else { return Err("407 Proxy Authentication Required") };
    // A name only: a bracketed or bare address names no registry.
    let (host, port) = target.rsplit_once(':').ok_or("400 Bad Request")?;
    let port: u16 = port.parse().map_err(|_| "400 Bad Request")?;
    let host = host.to_ascii_lowercase();
    let grants = GRANTS.lock().unwrap_or_else(|e| e.into_inner());
    let Some(g) = grants.get(&token) else { return Err("407 Proxy Authentication Required") };
    if port != g.port || !g.hosts.iter().any(|h| *h == host) {
        return Err("403 Forbidden");
    }
    Ok((host, port))
}

async fn serve(mut s: tokio::net::TcpStream) {
    let mut head: Vec<u8> = Vec::new();
    let read = tokio::time::timeout(HEAD_TIMEOUT, async {
        let mut buf = [0u8; 1024];
        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < HEAD_MAX {
            match s.read(&mut buf).await {
                Ok(0) | Err(_) => return false,
                Ok(n) => head.extend_from_slice(&buf[..n]),
            }
        }
        true
    })
    .await;
    if !matches!(read, Ok(true)) || !head.windows(4).any(|w| w == b"\r\n\r\n") {
        return;
    }
    let text = String::from_utf8_lossy(&head).to_string();
    let (host, port) = match decide(&text) {
        Ok(t) => t,
        Err(status) => {
            let target = text.split(' ').nth(1).unwrap_or_default().chars().take(200).collect::<String>();
            log::info!("hive: registry proxy refused {target}: {status}");
            let auth = if status.starts_with("407") { "Proxy-Authenticate: Basic realm=\"maiTerm\"\r\n" } else { "" };
            let _ = s.write_all(format!("HTTP/1.1 {status}\r\n{auth}Content-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
            return;
        }
    };
    let up = match tokio::time::timeout(CONNECT_TIMEOUT, tokio::net::TcpStream::connect((host.as_str(), port))).await {
        Ok(Ok(u)) => u,
        _ => {
            let _ = s.write_all(b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
            return;
        }
    };
    if s.write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n").await.is_err() {
        return;
    }
    let mut up = up;
    let _ = tokio::io::copy_bidirectional(&mut s, &mut up).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    fn connect(proxy: u16, target: &str, auth: Option<&str>) -> (String, TcpStream) {
        let mut c = TcpStream::connect(("127.0.0.1", proxy)).unwrap();
        c.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
        let auth = auth.map(|a| format!("Proxy-Authorization: Basic {}\r\n", base64::engine::general_purpose::STANDARD.encode(a))).unwrap_or_default();
        write!(c, "CONNECT {target} HTTP/1.1\r\nHost: {target}\r\n{auth}\r\n").unwrap();
        // The head byte by byte, so nothing after it is read here.
        let mut head = Vec::new();
        let mut b = [0u8; 1];
        while !head.ends_with(b"\r\n\r\n") && c.read(&mut b).unwrap_or(0) == 1 {
            head.push(b[0]);
        }
        let head = String::from_utf8_lossy(&head).to_string();
        (head.lines().next().unwrap_or_default().to_string(), c)
    }

    fn token(url: &str) -> String {
        url.trim_start_matches("http://").split(':').next().unwrap().to_string()
    }

    /// The tunnel opens only to a granted host and port, for the grant's own token, and only
    /// until its job ends.
    #[test]
    fn a_grant_opens_its_hosts_and_nothing_else() {
        let up = TcpListener::bind("127.0.0.1:0").unwrap();
        let up_port = up.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for s in up.incoming().flatten() {
                let mut s = s;
                let mut b = [0u8; 4];
                if s.read_exact(&mut b).is_ok() {
                    let _ = s.write_all(&b);
                }
            }
        });
        let (proxy, url) = open_on("j-1", vec!["127.0.0.1".into()], up_port).unwrap();
        let t = token(&url);
        assert!(url.ends_with(&format!("@127.0.0.1:{proxy}")), "{url}");
        let (status, mut c) = connect(proxy, &format!("127.0.0.1:{up_port}"), Some(&format!("{t}:x")));
        assert!(status.contains("200"), "{status}");
        c.write_all(b"ping").unwrap();
        let mut back = [0u8; 4];
        c.read_exact(&mut back).unwrap();
        assert_eq!(&back, b"ping", "the tunnel carries bytes both ways");

        assert!(connect(proxy, &format!("127.0.0.1:{up_port}"), None).0.contains("407"), "no token");
        assert!(connect(proxy, &format!("127.0.0.1:{up_port}"), Some("nope:x")).0.contains("407"), "a token no grant holds");
        assert!(connect(proxy, &format!("localhost:{up_port}"), Some(&format!("{t}:x"))).0.contains("403"), "a host it wasn't granted");
        assert!(connect(proxy, "127.0.0.1:22", Some(&format!("{t}:x"))).0.contains("403"), "another port");
        let mut c = TcpStream::connect(("127.0.0.1", proxy)).unwrap();
        write!(c, "GET http://127.0.0.1:{up_port}/ HTTP/1.1\r\nHost: x\r\n\r\n").unwrap();
        let mut status = String::new();
        BufReader::new(c).read_line(&mut status).unwrap();
        assert!(status.contains("405"), "plain http is never proxied: {status}");

        close("j-1");
        assert!(connect(proxy, &format!("127.0.0.1:{up_port}"), Some(&format!("{t}:x"))).0.contains("407"), "the job ended");
    }

    #[test]
    fn decisions_read_the_connect_line_and_the_token() {
        let (_, url) = open_on("j-2", vec!["registry.npmjs.org".into()], REGISTRY_PORT).unwrap();
        let auth = base64::engine::general_purpose::STANDARD.encode(format!("{}:x", token(&url)));
        let head = |target: &str| format!("CONNECT {target} HTTP/1.1\r\nproxy-authorization: Basic {auth}\r\n\r\n");
        assert_eq!(decide(&head("Registry.NPMJS.org:443")), Ok(("registry.npmjs.org".to_string(), 443)));
        assert_eq!(decide(&head("github.com:443")), Err("403 Forbidden"));
        assert_eq!(decide(&head("registry.npmjs.org:22")), Err("403 Forbidden"));
        assert_eq!(decide(&head("[::1]:443")), Err("403 Forbidden"));
        assert_eq!(decide(&head("registry.npmjs.org")), Err("400 Bad Request"));
        close("j-2");
        assert_eq!(decide(&head("registry.npmjs.org:443")), Err("407 Proxy Authentication Required"));
    }

    #[test]
    fn an_install_reaches_the_registries_but_never_a_remote_or_a_generator() {
        let mut h = crate::hive::launch::tests::sample_hive();
        h.settings.gallery.generator_hosts = vec!["fal.run".into()];
        let hosts = install_hosts(&h, &["github.com".into(), "registry.yarnpkg.com".into()]);
        assert!(hosts.contains(&"registry.npmjs.org".to_string()) && hosts.contains(&"pypi.org".to_string()) && hosts.contains(&"repo.yarnpkg.com".to_string()));
        assert!(!hosts.iter().any(|h| h == "fal.run" || h == "github.com" || h == "registry.yarnpkg.com"), "{hosts:?}");
        let proxied = env("http://t:x@127.0.0.1:1");
        assert!(proxied.iter().any(|(k, _)| k == "npm_config_https_proxy") && proxied.iter().all(|(_, v)| v == "http://t:x@127.0.0.1:1"));
    }
}
