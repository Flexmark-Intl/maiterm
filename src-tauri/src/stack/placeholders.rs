//! Placeholders in a service's command and env, filled per copy (docs/hive.md §6.9).
//!
//! One left-to-right pass finds `{port}`, `{port2}`, `{branch}`, `{worker}`, `{checkout}` and
//! `{port:<service>}`; every other brace is left as it is, so the shell's own brace expansion
//! still works. A value is DATA: it must match its placeholder's charset or the copy is refused.
//! Nothing is quoted to make a value safe, and no charset admits a shell metacharacter, so a
//! value is inert in any quoting context. `{checkout}` is a path (spaces under Application
//! Support), so it is allowed in env values only, which `startLine` single-quotes.

use std::ops::Range;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placeholder {
    Port,
    Port2,
    Branch,
    Worker,
    Checkout,
    /// `{port:<service>}`: that service's port, the name as written.
    PortOf(String),
}

/// Each placeholder in `text`, with its byte range.
pub fn scan(text: &str) -> Vec<(Range<usize>, Placeholder)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = text[i..].find('{') {
        let open = i + off;
        let rest = &text[open + 1..];
        // The inner text runs to the next brace of either kind: `{{port}}` is `{` + `{port}` + `}`.
        let Some(j) = rest.find(&['{', '}'][..]) else { break };
        if rest.as_bytes()[j] == b'}' {
            if let Some(p) = placeholder(&rest[..j]) {
                out.push((open..open + j + 2, p));
                i = open + j + 2;
                continue;
            }
        }
        i = open + 1;
    }
    out
}

fn placeholder(inner: &str) -> Option<Placeholder> {
    Some(match inner {
        "port" => Placeholder::Port,
        "port2" => Placeholder::Port2,
        "branch" => Placeholder::Branch,
        "worker" => Placeholder::Worker,
        "checkout" => Placeholder::Checkout,
        _ => {
            let name = inner.strip_prefix("port:")?;
            if !(1..=64).contains(&name.chars().count()) {
                return None;
            }
            Placeholder::PortOf(name.to_string())
        }
    })
}

/// What a command and its env values ask for, so only those values are worked out.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Needs {
    pub port: bool,
    pub port2: bool,
    pub branch: bool,
    pub worker: bool,
    /// In an env value; `checkout_in_command` is refused.
    pub checkout: bool,
    pub checkout_in_command: bool,
    pub port_of: Vec<String>,
}

impl Needs {
    pub fn of(command: &str, env: &[(String, String)]) -> Self {
        let mut n = Needs::default();
        let mut add = |text: &str, in_command: bool| {
            for (_, p) in scan(text) {
                match p {
                    Placeholder::Port => n.port = true,
                    Placeholder::Port2 => n.port2 = true,
                    Placeholder::Branch => n.branch = true,
                    Placeholder::Worker => n.worker = true,
                    Placeholder::Checkout if in_command => n.checkout_in_command = true,
                    Placeholder::Checkout => n.checkout = true,
                    Placeholder::PortOf(s) if !n.port_of.contains(&s) => n.port_of.push(s),
                    Placeholder::PortOf(_) => {}
                }
            }
        };
        add(command, true);
        for (_, v) in env {
            add(v, false);
        }
        n
    }

    pub fn any(&self) -> bool {
        *self != Needs::default()
    }

    /// A copy picks its own port, so several can run at once (§6.9); without `{port}` the
    /// service runs one copy at a time.
    pub fn one_copy(&self) -> bool {
        !self.port
    }
}

/// The values one copy fills in. A value is only read for a placeholder that is used.
#[derive(Debug, Default)]
pub struct Values {
    pub port: Option<u16>,
    pub port2: Option<u16>,
    pub branch: Option<String>,
    pub worker: Option<String>,
    pub checkout: Option<String>,
    /// `{port:<service>}` by the name as written.
    pub port_of: Vec<(String, u16)>,
}

/// `^[A-Za-z0-9._/+-]{1,200}$`, and no leading `-` (git refuses one, and it would read as a flag).
pub fn valid_branch(s: &str) -> bool {
    (1..=200).contains(&s.len())
        && !s.starts_with('-')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'/' | b'+' | b'-'))
}

/// `^[a-z0-9-]+$`, bounded.
pub fn valid_worker(s: &str) -> bool {
    (1..=100).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Absolute, no control character (a newline typed into a PTY ends the line, quotes or not).
pub fn valid_checkout(s: &str) -> bool {
    !s.is_empty() && std::path::Path::new(s).is_absolute() && !s.chars().any(char::is_control)
}

/// `^[A-Za-z_][A-Za-z0-9_]*$`: `startLine` types `env K='v'`, and only the value is quoted.
pub fn valid_env_key(k: &str) -> bool {
    let mut b = k.bytes();
    matches!(b.next(), Some(c) if c.is_ascii_alphabetic() || c == b'_') && b.all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

/// Every value a copy will use, checked against its charset before anything is substituted or
/// booked, so a refusal never leaves a port taken.
pub fn check(needs: &Needs, v: &Values) -> Result<(), String> {
    if needs.checkout_in_command {
        return Err("{checkout} can only be used in env values: a path with spaces in it isn't safe in a command".to_string());
    }
    let refuse = |name: &str, value: &str, allowed: &str| {
        Err(format!("{{{name}}} would be {value:?}, which isn't safe to put in a command (allowed: {allowed})"))
    };
    if needs.branch {
        match &v.branch {
            Some(b) if valid_branch(b) => {}
            Some(b) => return refuse("branch", b, "letters, digits and . _ / + -"),
            None => return Err("{branch}: this copy isn't on a branch".to_string()),
        }
    }
    if needs.worker {
        match &v.worker {
            Some(w) if valid_worker(w) => {}
            Some(w) => return refuse("worker", w, "lowercase letters, digits and -"),
            None => return Err("{worker}: there is no name for this copy".to_string()),
        }
    }
    if needs.checkout {
        match &v.checkout {
            Some(c) if valid_checkout(c) => {}
            Some(c) => return refuse("checkout", c, "an absolute path without control characters"),
            None => return Err("{checkout}: this copy's folder isn't a checkout".to_string()),
        }
    }
    Ok(())
}

/// `text` with every placeholder filled. `check` has passed and the ports are booked; a value
/// missing here is a caller's bug, refused rather than typed as an empty string.
pub fn fill(text: &str, v: &Values) -> Result<String, String> {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for (range, p) in scan(text) {
        out.push_str(&text[last..range.start]);
        let value = match &p {
            Placeholder::Port => v.port.map(|p| p.to_string()),
            Placeholder::Port2 => v.port2.map(|p| p.to_string()),
            Placeholder::Branch => v.branch.clone().filter(|b| valid_branch(b)),
            Placeholder::Worker => v.worker.clone().filter(|w| valid_worker(w)),
            Placeholder::Checkout => v.checkout.clone().filter(|c| valid_checkout(c)),
            Placeholder::PortOf(name) => v.port_of.iter().find(|(n, _)| n == name).map(|(_, p)| p.to_string()),
        };
        out.push_str(&value.ok_or_else(|| format!("{} has no value", &text[range.clone()]))?);
        last = range.end;
    }
    out.push_str(&text[last..]);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(text: &str) -> Vec<Placeholder> {
        scan(text).into_iter().map(|(_, p)| p).collect()
    }

    #[test]
    fn only_the_six_shapes_are_placeholders() {
        assert_eq!(kinds("vite --port {port} --hmr {port2}"), vec![Placeholder::Port, Placeholder::Port2]);
        assert_eq!(kinds("DATABASE_URL=postgres://localhost/app_{worker}"), vec![Placeholder::Worker]);
        assert_eq!(kinds("{port:db}"), vec![Placeholder::PortOf("db".into())]);
        assert_eq!(kinds("{{port}}"), vec![Placeholder::Port], "the inner pair is one");
        // Brace expansion, ${VAR}, JSON and near misses all stay verbatim.
        for text in ["cp a.{js,map} out", "echo ${PORT}", r#"{"port": 1}"#, "{Port}", "{ port }", "{port:}", "{port:a{b}", "{branch", "}{"] {
            assert!(scan(text).is_empty(), "{text}");
        }
        assert!(scan(&format!("{{port:{}}}", "x".repeat(65))).is_empty(), "a name is at most 64");
        assert_eq!(scan(&format!("{{port:{}}}", "é".repeat(64))).len(), 1, "64 characters, not bytes");
    }

    #[test]
    fn a_legacy_service_needs_nothing() {
        let env = vec![("NODE_ENV".to_string(), "development".to_string())];
        assert!(!Needs::of("npm run dev -- --open {a,b}", &env).any());
        let n = Needs::of("vite --port {port}", &[("DB".into(), "app_{worker}".into()), ("ROOT".into(), "{checkout}".into())]);
        assert!(n.port && n.worker && n.checkout && !n.checkout_in_command && !n.one_copy());
        assert!(Needs::of("ls {checkout}", &[]).checkout_in_command);
    }

    #[test]
    fn values_outside_their_charset_are_refused_never_quoted() {
        let needs = Needs { branch: true, ..Needs::default() };
        for bad in ["x$(touch p)", "a;b", "a`id`b", "it's", "a\"b", "a b", "a\nb", "-x", "a|b", "a&b", "a>b", "a\\b", ""] {
            let v = Values { branch: Some(bad.to_string()), ..Values::default() };
            assert!(check(&needs, &v).is_err(), "{bad:?} passed");
            assert!(fill("{branch}", &v).is_err(), "{bad:?} filled");
        }
        for good in ["main", "feature/session-store", "release-1.2", "a+b_c"] {
            assert!(check(&needs, &Values { branch: Some(good.to_string()), ..Values::default() }).is_ok(), "{good}");
        }
        let needs = Needs { worker: true, ..Needs::default() };
        assert!(check(&needs, &Values { worker: Some("Session".into()), ..Values::default() }).is_err());
        assert!(check(&needs, &Values { worker: Some("session-store".into()), ..Values::default() }).is_ok());
        let needs = Needs { checkout: true, ..Needs::default() };
        assert!(check(&needs, &Values { checkout: Some("relative/x".into()), ..Values::default() }).is_err());
        assert!(check(&needs, &Values { checkout: Some("/a/b\nrm -rf ~".into()), ..Values::default() }).is_err());
        let abs = std::env::temp_dir().join("Application Support").to_string_lossy().to_string();
        assert!(check(&needs, &Values { checkout: Some(abs), ..Values::default() }).is_ok(), "a space is fine in an env value");
        assert!(check(&Needs { checkout_in_command: true, ..Needs::default() }, &Values::default()).is_err());
    }

    #[test]
    fn fill_substitutes_in_one_pass() {
        let v = Values { port: Some(5174), branch: Some("s".into()), port_of: vec![("db".into(), 5432)], ..Values::default() };
        assert_eq!(fill("vite --port {port} {a,b} --db :{port:db} # {branch}", &v).unwrap(), "vite --port 5174 {a,b} --db :5432 # s");
        // A value that itself looks like a placeholder is not filled again.
        let v = Values { branch: Some("x".into()), worker: Some("w".into()), ..Values::default() };
        assert_eq!(fill("{branch}{worker}", &v).unwrap(), "xw");
        assert!(fill("{port2}", &Values::default()).is_err());
    }

    #[test]
    fn env_keys() {
        for good in ["A", "_x", "DATABASE_URL", "a1"] {
            assert!(valid_env_key(good), "{good}");
        }
        for bad in ["", "1A", "A-B", "A B", "A;B", "A=B", "$A"] {
            assert!(!valid_env_key(bad), "{bad}");
        }
    }
}
