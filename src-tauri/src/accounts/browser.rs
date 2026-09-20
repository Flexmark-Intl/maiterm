//! Opening a sign-in link in a private window — `docs/login.md` §5.2.
//!
//! The problem this solves is §5.1's: the provider reuses the browser's session, so signing in
//! to a *second* account in the default browser silently returns the first one. The remedies are
//! "sign out of the provider first" (destructive to the session the user is in the middle of) or
//! "open the link in a private window" (free). This module makes the second one a button.
//!
//! **It is a best-effort convenience, never the mechanism.** Every caller must still offer the
//! link itself, because there are perfectly ordinary machines where nothing here is found:
//! Safari has no command-line switch for a private window at all (AppleScript can open one, but
//! it cannot be handed a URL to load in it without a second scripted step, and the whole thing
//! breaks on the Automation permission prompt), and Arc, Orion and friends expose no flag either.
//!
//! The private flag is the ONLY thing we pass besides the URL. In particular we never hand the
//! browser a profile directory: the point is a window with no session, and a profile is a
//! session.

use std::path::{Path, PathBuf};

/// A browser we know how to open a private window in.
#[derive(Debug, Clone, Copy)]
struct PrivateBrowser {
    id: &'static str,
    label: &'static str,
    /// The switch that opens a private window with the URL already loading in it.
    flag: &'static str,
    /// Executables to look for, most-preferred first. On macOS these are the binaries *inside*
    /// the app bundle rather than `open -na "<App>"`: `open -n` asks for a second instance of
    /// the app, which is not what we want (Chrome holds a profile lock, and a second instance
    /// negotiating with the first is a different code path on every version). Running the bundle
    /// binary directly relays to the running copy and opens the window, which is exactly the
    /// behaviour a user expects from a browser that is already open.
    candidates: &'static [&'static str],
}

/// Ordered by how likely the flag is to still work: the Chromium family shares one switch and
/// has kept it for a decade, Firefox's is equally old. Anything whose private mode is only
/// reachable through a menu is deliberately absent — see the module docs.
#[cfg(target_os = "macos")]
const BROWSERS: &[PrivateBrowser] = &[
    PrivateBrowser {
        id: "chrome",
        label: "Chrome",
        flag: "--incognito",
        candidates: &["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"],
    },
    PrivateBrowser {
        id: "brave",
        label: "Brave",
        flag: "--incognito",
        candidates: &["/Applications/Brave Browser.app/Contents/MacOS/Brave Browser"],
    },
    PrivateBrowser {
        id: "edge",
        label: "Edge",
        flag: "--inprivate",
        candidates: &["/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge"],
    },
    PrivateBrowser {
        id: "chromium",
        label: "Chromium",
        flag: "--incognito",
        candidates: &["/Applications/Chromium.app/Contents/MacOS/Chromium"],
    },
    PrivateBrowser {
        id: "firefox",
        label: "Firefox",
        flag: "-private-window",
        candidates: &["/Applications/Firefox.app/Contents/MacOS/firefox"],
    },
];

#[cfg(target_os = "linux")]
const BROWSERS: &[PrivateBrowser] = &[
    PrivateBrowser {
        id: "chrome",
        label: "Chrome",
        flag: "--incognito",
        candidates: &["google-chrome", "google-chrome-stable"],
    },
    PrivateBrowser {
        id: "brave",
        label: "Brave",
        flag: "--incognito",
        candidates: &["brave-browser", "brave"],
    },
    PrivateBrowser {
        id: "edge",
        label: "Edge",
        flag: "--inprivate",
        candidates: &["microsoft-edge", "microsoft-edge-stable"],
    },
    PrivateBrowser {
        id: "chromium",
        label: "Chromium",
        flag: "--incognito",
        candidates: &["chromium", "chromium-browser"],
    },
    PrivateBrowser {
        id: "firefox",
        label: "Firefox",
        flag: "-private-window",
        candidates: &["firefox"],
    },
];

#[cfg(target_os = "windows")]
const BROWSERS: &[PrivateBrowser] = &[
    PrivateBrowser {
        id: "chrome",
        label: "Chrome",
        flag: "--incognito",
        candidates: &[
            r"%ProgramFiles%\Google\Chrome\Application\chrome.exe",
            r"%ProgramFiles(x86)%\Google\Chrome\Application\chrome.exe",
            r"%LocalAppData%\Google\Chrome\Application\chrome.exe",
        ],
    },
    PrivateBrowser {
        id: "edge",
        label: "Edge",
        flag: "--inprivate",
        candidates: &[
            r"%ProgramFiles(x86)%\Microsoft\Edge\Application\msedge.exe",
            r"%ProgramFiles%\Microsoft\Edge\Application\msedge.exe",
        ],
    },
    PrivateBrowser {
        id: "brave",
        label: "Brave",
        flag: "--incognito",
        candidates: &[
            r"%ProgramFiles%\BraveSoftware\Brave-Browser\Application\brave.exe",
            r"%LocalAppData%\BraveSoftware\Brave-Browser\Application\brave.exe",
        ],
    },
    PrivateBrowser {
        id: "firefox",
        label: "Firefox",
        flag: "-private-window",
        candidates: &[
            r"%ProgramFiles%\Mozilla Firefox\firefox.exe",
            r"%ProgramFiles(x86)%\Mozilla Firefox\firefox.exe",
        ],
    },
];

/// One browser the UI may offer. `id` is what comes back to `open_private_window`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct PrivateBrowserInfo {
    pub id: String,
    pub label: String,
}

/// Expand a candidate into a path that exists, or nothing.
///
/// Three shapes, because the three platforms disagree: an absolute path (macOS bundles), a bare
/// command to look up on `PATH` (Linux), and a path with `%VAR%` in it (Windows).
fn resolve_candidate(candidate: &str) -> Option<PathBuf> {
    if candidate.contains('%') {
        let mut out = String::new();
        let mut rest = candidate;
        while let Some(start) = rest.find('%') {
            out.push_str(&rest[..start]);
            let after = &rest[start + 1..];
            // An unpaired `%` is a malformed entry, not a variable. Bail rather than guessing.
            let end = after.find('%')?;
            let name = &after[..end];
            out.push_str(&std::env::var(name).ok()?);
            rest = &after[end + 1..];
        }
        out.push_str(rest);
        let p = PathBuf::from(out);
        return p.is_file().then_some(p);
    }

    if candidate.contains(std::path::MAIN_SEPARATOR) || candidate.starts_with('/') {
        let p = Path::new(candidate);
        return p.is_file().then(|| p.to_path_buf());
    }

    // A bare name: walk PATH ourselves rather than shelling out to `which`, which a bundled app
    // may not be able to find either.
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(candidate))
        .find(|p| p.is_file())
}

fn resolve(browser: &PrivateBrowser) -> Option<PathBuf> {
    browser.candidates.iter().copied().find_map(resolve_candidate)
}

/// The browsers actually installed, in preference order. Empty is a normal answer — the caller
/// falls back to showing the link.
pub fn available() -> Vec<PrivateBrowserInfo> {
    BROWSERS
        .iter()
        .filter(|b| resolve(b).is_some())
        .map(|b| PrivateBrowserInfo { id: b.id.to_string(), label: b.label.to_string() })
        .collect()
}

/// Open `url` in a private window of `browser_id`.
///
/// The URL is validated rather than trusted: this hands a string to a process, and the one
/// argument that must never be attacker-controlled is the first one. `https://` only, and no
/// leading `-` so a crafted value cannot arrive as another switch.
pub fn open_private_window(browser_id: &str, url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("refusing to open a non-https link".to_string());
    }
    let browser = BROWSERS
        .iter()
        .find(|b| b.id == browser_id)
        .ok_or_else(|| format!("unknown browser: {browser_id}"))?;
    let bin = resolve(browser)
        .ok_or_else(|| format!("{} is no longer installed where it was found", browser.label))?;

    std::process::Command::new(&bin)
        .arg(browser.flag)
        .arg(url)
        // Detached from our streams: a browser inheriting the app's pipes can block on them, and
        // nothing here ever reads its output.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("opening {}: {e}", browser.label))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_entry_has_a_flag_and_a_candidate() {
        for b in BROWSERS {
            assert!(!b.flag.is_empty(), "{} has no private flag", b.id);
            assert!(!b.candidates.is_empty(), "{} has nothing to look for", b.id);
            // The flag is passed before the URL, so an empty or non-switch flag would be read as
            // a file to open — silently defeating the entire point of this module.
            assert!(b.flag.starts_with('-'), "{} flag is not a switch", b.id);
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<_> = BROWSERS.iter().map(|b| b.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate browser id");
    }

    #[test]
    fn refuses_a_url_that_is_not_https() {
        // Not pedantry: the URL becomes an argv entry. `file:///…` would open a local file and
        // `--something` would arrive as another switch.
        for bad in ["http://claude.ai/oauth", "file:///etc/passwd", "--user-data-dir=/tmp"] {
            assert!(open_private_window("chrome", bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn refuses_an_unknown_browser() {
        assert!(open_private_window("nope", "https://claude.ai/oauth/authorize").is_err());
    }

    #[test]
    fn resolves_an_absolute_path_only_when_it_exists() {
        assert!(resolve_candidate("/definitely/not/here/browser").is_none());
        // Every platform this builds on has a shell at this path.
        #[cfg(unix)]
        assert!(resolve_candidate("/bin/sh").is_some());
    }

    #[cfg(unix)]
    #[test]
    fn resolves_a_bare_name_from_path() {
        assert!(resolve_candidate("sh").is_some());
        assert!(resolve_candidate("definitely-not-a-real-binary-xyz").is_none());
    }
}
