//! The native-messaging host (docs/browser-link.md §2). Chrome launches the maiTerm binary itself
//! with the calling extension's origin as an argument; `main()` sends that here before the app
//! starts. One framed request in, the discovery file out, exit — no window, no state loaded.
//!
//! Chrome's framing: a 32-bit length in native byte order, then that many bytes of UTF-8 JSON.

use std::io::{Read, Write};

/// Did Chrome launch us as a native-messaging host? Chrome passes the caller's origin
/// (`chrome-extension://<id>/`) as the first argument; on Windows a `--parent-window` follows.
pub fn requested() -> bool {
    std::env::args().nth(1).is_some_and(|a| a.starts_with("chrome-extension://"))
}

fn read_message(input: &mut impl Read) -> Option<Vec<u8>> {
    let mut len = [0u8; 4];
    input.read_exact(&mut len).ok()?;
    let len = u32::from_ne_bytes(len) as usize;
    // A discovery request is tiny; anything large is not ours.
    if len > 64 * 1024 {
        return None;
    }
    let mut buf = vec![0u8; len];
    input.read_exact(&mut buf).ok()?;
    Some(buf)
}

fn write_message(out: &mut impl Write, v: &serde_json::Value) {
    let body = v.to_string();
    let _ = out.write_all(&(body.len() as u32).to_ne_bytes());
    let _ = out.write_all(body.as_bytes());
    let _ = out.flush();
}

/// Answer one discovery request with this build's discovery file.
pub fn main() {
    let mut stdin = std::io::stdin().lock();
    let mut stdout = std::io::stdout().lock();
    if read_message(&mut stdin).is_none() {
        return;
    }
    let reply = match super::discovery_path().map(std::fs::read_to_string) {
        Some(Ok(s)) => match serde_json::from_str::<serde_json::Value>(&s) {
            Ok(mut v) => {
                v["ok"] = serde_json::Value::Bool(true);
                v
            }
            Err(_) => serde_json::json!({ "ok": false, "error": "maiTerm's discovery file is unreadable" }),
        },
        _ => serde_json::json!({ "ok": false, "error": "maiTerm has not started yet" }),
    };
    write_message(&mut stdout, &reply);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn framing_round_trips() {
        let mut buf = Vec::new();
        write_message(&mut buf, &serde_json::json!({ "type": "discover" }));
        let got = read_message(&mut buf.as_slice()).unwrap();
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&got).unwrap()["type"], "discover");
    }

    #[test]
    fn refuses_oversized_frames() {
        let mut buf = (1_000_000u32).to_ne_bytes().to_vec();
        buf.extend_from_slice(b"{}");
        assert!(read_message(&mut buf.as_slice()).is_none());
    }
}
