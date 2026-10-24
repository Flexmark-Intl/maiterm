//! The port book (docs/hive.md §6.9): which `{port}`/`{port2}` each running copy was given.
//!
//! In memory only: after a restart nothing runs, so nothing holds a port. A port is given only
//! when it is unbooked, bindable on both loopbacks (probed by binding and closing at once; nothing
//! is ever killed to free one), and never one of maiTerm's own: its MCP server (which also serves
//! the MCP proxy and the browser link), maiLink, or the other maiTerms' (`instance::RESERVED_PORTS`,
//! 8765 among them).

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::Mutex;
use std::time::Duration;

/// A service with no port on record scans up from Vite's.
pub const DEFAULT_BASE: u16 = 5173;
const TRIES: u16 = 200;
/// A closed loopback port refuses at once; this only bounds a listener too busy to accept.
const PROBE_TIMEOUT: Duration = Duration::from_millis(200);

#[derive(Debug, Default)]
pub struct PortBook {
    /// Copy key → its ports, while booked.
    booked: HashMap<String, Vec<u16>>,
    /// Copy key → the ports it last had, tried first next time so a restart keeps its address.
    last: HashMap<String, Vec<u16>>,
}

impl PortBook {
    fn held_by_other(&self, key: &str, port: u16) -> bool {
        self.booked.iter().any(|(k, ps)| k != key && ps.contains(&port))
    }

    /// `n` ports for `key`. A copy that holds ports keeps them (its own process may be the one
    /// bound to them); otherwise its last ports if still free, then a scan of `TRIES` ports from
    /// `base` (`own`, the service's own folder) or `base + 1` (any other copy, which must not
    /// take the address the own copy is known by).
    pub fn take(&mut self, key: &str, n: usize, base: u16, own: bool, forbidden: &[u16], free: impl Fn(u16) -> bool) -> Result<Vec<u16>, String> {
        let mut chosen: Vec<u16> = self.booked.get(key).map(|ps| ps.iter().copied().take(n).collect()).unwrap_or_default();
        let usable = |p: u16, chosen: &[u16]| p >= 1024 && !forbidden.contains(&p) && !chosen.contains(&p) && !self.held_by_other(key, p);
        let start = if own { base } else { base.saturating_add(1) }.max(1024);
        while chosen.len() < n {
            let i = chosen.len();
            let remembered = self.last.get(key).and_then(|ps| ps.get(i)).copied().filter(|&p| usable(p, &chosen) && free(p));
            let found = remembered.or_else(|| {
                (0..TRIES).filter_map(|k| start.checked_add(k)).find(|&p| usable(p, &chosen) && free(p))
            });
            match found {
                Some(p) => chosen.push(p),
                None => {
                    return Err(format!(
                        "no free port from {start} to {}: every one is taken or maiTerm's own",
                        start.saturating_add(TRIES - 1)
                    ))
                }
            }
        }
        self.booked.insert(key.to_string(), chosen.clone());
        self.last.insert(key.to_string(), chosen.clone());
        Ok(chosen)
    }

    /// Free a copy's ports; it is still offered them first next time. False if it held none.
    pub fn release(&mut self, key: &str) -> bool {
        self.booked.remove(key).is_some()
    }
}

static BOOK: Mutex<Option<PortBook>> = Mutex::new(None);

fn with_book<R>(f: impl FnOnce(&mut PortBook) -> R) -> R {
    let mut guard = BOOK.lock().unwrap_or_else(|e| e.into_inner());
    f(guard.get_or_insert_with(PortBook::default))
}

/// `PortBook::take` on the process's book, probing for real.
pub fn take(key: &str, n: usize, base: u16, own: bool, forbidden: &[u16]) -> Result<Vec<u16>, String> {
    with_book(|b| b.take(key, n, base, own, forbidden, bindable))
}

pub fn release(key: &str) -> bool {
    with_book(|b| b.release(key))
}

/// Free on 127.0.0.1 and on ::1: nothing answers a connect there, and both bind. The connect is
/// what sees a wildcard listener (`*:P`, Node's default `::`): std binds with SO_REUSEADDR, which
/// on macOS lets a loopback bind succeed beside one. A machine with no IPv6 loopback can't have
/// anything bound there, so "address not available" passes.
pub fn bindable(port: u16) -> bool {
    let answers = |ip: IpAddr| TcpStream::connect_timeout(&SocketAddr::new(ip, port), PROBE_TIMEOUT).is_ok();
    if answers(Ipv4Addr::LOCALHOST.into()) || answers(Ipv6Addr::LOCALHOST.into()) {
        return false;
    }
    if TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_err() {
        return false;
    }
    match TcpListener::bind((Ipv6Addr::LOCALHOST, port)) {
        Ok(_) => true,
        Err(e) => e.kind() == std::io::ErrorKind::AddrNotAvailable,
    }
}

/// maiTerm's own ports, never given to a copy.
pub fn forbidden(mcp_port: Option<u16>, mailink_port: Option<u16>) -> Vec<u16> {
    let mut out: Vec<u16> = crate::instance::RESERVED_PORTS.to_vec();
    out.extend(mcp_port);
    out.extend(mailink_port);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const FORBIDDEN: [u16; 3] = [1420, 8765, 27365];

    #[test]
    fn copies_never_share_a_port_and_skip_taken_and_forbidden_ones() {
        let mut book = PortBook::default();
        let taken = [5174u16, 5176];
        let free = |p: u16| !taken.contains(&p);
        assert_eq!(book.take("web", 1, 5173, true, &FORBIDDEN, free).unwrap(), vec![5173], "the own copy starts at its port");
        assert_eq!(book.take("web@branch:a", 1, 5173, false, &FORBIDDEN, free).unwrap(), vec![5175], "+1, and 5174 is bound");
        assert_eq!(book.take("web@branch:b", 2, 5173, false, &FORBIDDEN, free).unwrap(), vec![5177, 5178]);
        assert_eq!(book.take("api", 1, 8764, false, &FORBIDDEN, |_| true).unwrap(), vec![8766], "never 8765");
        assert_eq!(book.take("x", 1, 27364, true, &[27364, 27365], |_| true).unwrap(), vec![27366], "nor maiTerm's own");
    }

    #[test]
    fn a_booked_copy_keeps_its_ports_and_a_released_one_gets_them_back() {
        let mut book = PortBook::default();
        assert_eq!(book.take("web@branch:a", 1, 5173, false, &FORBIDDEN, |_| true).unwrap(), vec![5174]);
        // Its own server is bound there now: still its port.
        assert_eq!(book.take("web@branch:a", 1, 5173, false, &FORBIDDEN, |_| false).unwrap(), vec![5174]);
        assert!(book.release("web@branch:a"));
        assert!(!book.release("web@branch:a"));
        assert_eq!(book.take("web@branch:b", 1, 5173, false, &FORBIDDEN, |p| p != 5174).unwrap(), vec![5175]);
        assert_eq!(book.take("web@branch:a", 1, 5173, false, &FORBIDDEN, |_| true).unwrap(), vec![5174], "its last port, still free");
        book.release("web@branch:a");
        assert_eq!(book.take("web@branch:c", 1, 5173, false, &FORBIDDEN, |_| true).unwrap(), vec![5174], "released means free");
        assert_eq!(book.take("web@branch:a", 1, 5173, false, &FORBIDDEN, |_| true).unwrap(), vec![5176], "its last is someone else's now");
    }

    #[test]
    fn a_full_range_is_refused_and_the_top_never_wraps() {
        let mut book = PortBook::default();
        assert!(book.take("web", 1, 6000, false, &FORBIDDEN, |_| false).unwrap_err().contains("no free port"));
        assert_eq!(book.take("web", 1, 65535, true, &FORBIDDEN, |_| true).unwrap(), vec![65535]);
        assert!(book.take("api", 1, 65535, false, &FORBIDDEN, |_| true).is_err(), "65536 doesn't exist");
        assert_eq!(book.take("low", 1, 80, true, &FORBIDDEN, |_| true).unwrap(), vec![1024], "never a privileged port");
    }

    #[test]
    fn a_real_listener_is_never_offered() {
        let held = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = held.local_addr().unwrap().port();
        assert!(!bindable(port), "bound on 127.0.0.1");
        let mut book = PortBook::default();
        let got = book.take("web", 1, port, true, &FORBIDDEN, bindable).unwrap();
        assert_ne!(got[0], port);
        drop(held);
    }

    #[test]
    fn a_wildcard_listener_is_never_offered() {
        let v4 = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
        let p4 = v4.local_addr().unwrap().port();
        assert!(!bindable(p4), "held on 0.0.0.0");
        if let Ok(v6) = TcpListener::bind((Ipv6Addr::UNSPECIFIED, 0)) {
            let p6 = v6.local_addr().unwrap().port();
            assert!(!bindable(p6), "held on [::], as Node's listen(port) does");
            let mut book = PortBook::default();
            assert_ne!(book.take("web", 1, p6, true, &FORBIDDEN, bindable).unwrap()[0], p6);
        }
        let mut book = PortBook::default();
        assert_ne!(book.take("web", 1, p4, true, &FORBIDDEN, bindable).unwrap()[0], p4);
    }

    #[test]
    fn forbidden_always_holds_the_other_maiterms() {
        let f = forbidden(Some(40000), None);
        assert!(f.contains(&8765) && f.contains(&27365) && f.contains(&1420) && f.contains(&40000));
    }
}
