//! Which Host a request to one of maiTerm's own servers may name: this computer (`localhost`, a
//! loopback address) or its network (a private, link-local or VPN address, a `.local` name).
//! Never a public name or address (Darryl, 2026-10-26).
//!
//! The servers bind loopback (the MCP server) or every interface (maiLink, for the phone over the
//! LAN or a WireGuard tunnel). A public DNS name pointed at 127.0.0.1 or a LAN address, by its
//! owner and whenever they like (DNS rebinding), would otherwise reach them from a browser page,
//! which can't choose the Host it sends. A client that can (a Hive worker's shell, through a host
//! its sandbox allows) is not stopped here: for workers the hosts are looked up when added and at
//! every launch instead (docs/hive.md §3.4), and the routes' own auth still applies. Every
//! legitimate client already names an address: agents and hooks
//! `127.0.0.1`/`localhost`, the OAuth redirect `127.0.0.1`, the phone the IP its pairing QR
//! carried (docs/mailink-protocol.md §3). So the name is the check, before any route runs.

use axum::extract::Request;
use axum::http::{header, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::net::IpAddr;

/// The host part of a Host header or URI authority: `[v6]:port`, `name:port` or `name`.
fn host_part(raw: &str) -> &str {
    let raw = raw.trim();
    if let Some(rest) = raw.strip_prefix('[') {
        return rest.split(']').next().unwrap_or("");
    }
    // One colon is a port; more is a bare IPv6 address, which a Host header never carries
    // unbracketed, but take it whole rather than misread it.
    match raw.matches(':').count() {
        1 => raw.split(':').next().unwrap_or(""),
        _ => raw,
    }
}

/// An address on this computer or its network.
pub fn ip_is_local(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                // Carrier-grade NAT, 100.64.0.0/10: Tailscale's addresses, a VPN to the LAN.
                || (o[0] == 100 && (o[1] & 0xc0) == 64)
        }
        IpAddr::V6(v) => {
            let s = v.segments()[0];
            v.is_loopback()
                || (s & 0xfe00) == 0xfc00 // unique local
                || (s & 0xffc0) == 0xfe80 // link-local
                || v.to_ipv4_mapped().is_some_and(|m| ip_is_local(&IpAddr::V4(m)))
        }
    }
}

/// Whether a Host header (or an authority) names this computer or its network.
pub fn host_allowed(raw: &str) -> bool {
    let host = host_part(raw).trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    if let Ok(ip) = host.parse::<IpAddr>() {
        return ip_is_local(&ip);
    }
    host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local")
}

/// This computer's own primary address, as maiLink's pairing QR names it (`mailink::local_ip`):
/// on a network that hands out public addresses (a campus, some offices) it is a public one, and
/// still this computer. A name pointed at it is still refused: only the address itself counts.
/// Read through the routing table (no packet sent), at most once every 30 s.
fn own_address(ip: &IpAddr) -> bool {
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    static CACHE: Mutex<Option<(Instant, Vec<IpAddr>)>> = Mutex::new(None);
    let mut c = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if c.as_ref().is_none_or(|(at, _)| at.elapsed() > Duration::from_secs(30)) {
        let route = |bind: &str, to: &str| {
            let s = std::net::UdpSocket::bind(bind).ok()?;
            s.connect(to).ok()?;
            s.local_addr().ok().map(|a| a.ip())
        };
        let mine: Vec<IpAddr> = [route("0.0.0.0:0", "8.8.8.8:80"), route("[::]:0", "[2001:4860:4860::8888]:80")].into_iter().flatten().collect();
        *c = Some((Instant::now(), mine));
    }
    c.as_ref().is_some_and(|(_, mine)| mine.contains(ip))
}

/// An address that is this computer or its network: what the servers answer by IP, plus the
/// unspecified and broadcast addresses, which reach this computer when dialled. A Hive worker
/// may reach none of them (`hive::change`).
pub fn is_home(ip: &IpAddr) -> bool {
    let odd = match ip {
        IpAddr::V4(v) => v.is_unspecified() || v.is_broadcast(),
        IpAddr::V6(v) => v.is_unspecified(),
    };
    odd || ip_is_local(ip) || own_address(ip)
}

/// The axum layer every maiTerm server puts in front of its routes. A request naming no host at
/// all is refused too: every HTTP/1.1 client sends one, and HTTP/2 carries it as the authority.
pub async fn require_local_host(req: Request, next: Next) -> Response {
    let named = req.headers().get(header::HOST).and_then(|h| h.to_str().ok()).map(str::to_string).or_else(|| req.uri().authority().map(|a| a.to_string()));
    match named {
        Some(h) if host_allowed(&h) || host_part(&h).parse::<IpAddr>().is_ok_and(|ip| own_address(&ip)) => next.run(req).await,
        other => {
            log::warn!("refused a request to {} naming host {:?}: not this computer or its network", req.uri().path(), other.as_deref().map(|h| crate::hive::deliver::one_line(h, 80)));
            (StatusCode::MISDIRECTED_REQUEST, "maiTerm answers only requests addressed to this computer or its network").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_computer_and_its_network_are_allowed() {
        for h in [
            "127.0.0.1:8765",
            "127.0.0.1",
            "localhost:51234",
            "LOCALHOST",
            "app.localhost:3000",
            "[::1]:8765",
            "192.168.1.20:8765",
            "10.0.0.5",
            "172.16.4.1:8765",
            "169.254.10.1",
            "100.101.102.103:8765",
            "[fd7a:115c:a1e0::1]:8765",
            "[fe80::1]",
            "darryls-mac.local:8765",
            "darryls-mac.local.:8765",
        ] {
            assert!(host_allowed(h), "{h}");
        }
    }

    #[test]
    fn a_public_name_or_address_is_refused() {
        for h in [
            "lvh.me:51234",
            "127.0.0.1.nip.io:8765",
            "evil.example.com",
            "localhost.evil.com",
            "8.8.8.8:8765",
            "100.128.0.1",
            "[2606:4700::1111]:443",
            "",
            ":8765",
        ] {
            assert!(!host_allowed(h), "{h}");
        }
    }

    /// Through the layer as the servers mount it: the route runs only for a local Host.
    #[tokio::test]
    async fn the_layer_refuses_before_any_route_runs() {
        use axum::{body::Body, routing::get, Router};
        use tower::ServiceExt;
        let app = Router::new().route("/mcp", get(|| async { "ran" })).layer(axum::middleware::from_fn(require_local_host));
        let send = |host: Option<&str>| {
            let mut b = Request::builder().uri("/mcp");
            if let Some(h) = host {
                b = b.header(header::HOST, h);
            }
            app.clone().oneshot(b.body(Body::empty()).unwrap())
        };
        assert_eq!(send(Some("127.0.0.1:51234")).await.unwrap().status(), StatusCode::OK);
        assert_eq!(send(Some("192.168.1.20:8765")).await.unwrap().status(), StatusCode::OK);
        assert_eq!(send(Some("lvh.me:51234")).await.unwrap().status(), StatusCode::MISDIRECTED_REQUEST);
        assert_eq!(send(None).await.unwrap().status(), StatusCode::MISDIRECTED_REQUEST);
    }
}
