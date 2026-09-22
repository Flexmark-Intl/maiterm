//! Tauri surface for managed agent accounts — see `docs/login.md`.
//!
//! Nothing here returns, accepts or logs credential material. An account is named by id; what
//! comes back is identity *metadata* the Accounts pane displays and the duplicate guard compares.

use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;
use tauri::Emitter;

use crate::accounts::{self, Reconciled, Runtime};

/// One row of the runtime registry, for the Accounts pane.
#[derive(Debug, Clone, Serialize)]
pub struct RuntimeInfo {
    pub slug: String,
    pub label: String,
    pub config_env: String,
    /// False until this runtime has had the §5.4 verification Claude has had. The pane shows
    /// these as not yet available rather than hiding them — "Codex is coming" is useful, a
    /// runtime that silently half-works is not.
    pub supported: bool,
}

/// What a runtime reports about the account in one config root.
///
/// **`logged_in` is never sufficient on its own** (§6.1). Credential precedence is a
/// fall-through, so a root holding none of our credentials still reports `logged_in: true` from
/// some other rung, and every such answer arrives with **no `email` and no `orgId` at all**.
/// Treating that as an identity is the bug this struct exists to prevent: every account would
/// look identical, so the §5.1 duplicate guard fires on every new account (discarding a valid
/// login) and the §6.1 verification can never fail.
///
/// `is_account_login` is therefore the only field a caller may branch on before using
/// `email`/`org_id`. Scrubbing the environment is not enough on its own: `apiKeyHelper` and an
/// `env` block in `settings.json` are *settings-file* rungs, and that file is deliberately
/// shared by every account (§5.4), so they cannot be removed — only reported.
#[derive(Debug, Clone, Serialize)]
pub struct AccountIdentity {
    /// True only when this root's **own** `claude.ai` login answered and carried an email.
    /// When false, `email`/`org_id` are not an identity and must not be used as a duplicate
    /// key or a verification result.
    pub is_account_login: bool,
    /// What answered instead, when `is_account_login` is false — the "resolved source" §2.2
    /// requires the status UI to show rather than a green "logged in".
    pub shadowed_by: Option<String>,
    pub logged_in: bool,
    pub auth_method: Option<String>,
    pub api_key_source: Option<String>,
    /// `firstParty`, `bedrock`, … — set when a cloud provider answered.
    pub api_provider: Option<String>,
    pub email: Option<String>,
    pub org_id: Option<String>,
    pub org_name: Option<String>,
    pub plan: Option<String>,
}

fn runtime_from_slug(slug: &str) -> Result<Runtime, String> {
    accounts::ALL_RUNTIMES
        .iter()
        .copied()
        .find(|r| r.slug() == slug)
        .ok_or_else(|| format!("unknown runtime: {slug}"))
}

fn home_dir() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "no home directory".to_string())
}

/// The runtimes maiTerm knows about, supported or not.
#[tauri::command]
pub async fn list_account_runtimes() -> Result<Vec<RuntimeInfo>, String> {
    Ok(accounts::ALL_RUNTIMES
        .iter()
        .map(|r| {
            let p = r.profile();
            RuntimeInfo {
                slug: r.slug().to_string(),
                label: p.label.to_string(),
                config_env: p.config_env.to_string(),
                supported: p.supported,
            }
        })
        .collect())
}

/// Create or repair an account's config root.
///
/// Safe to call on every spawn, and meant to be: the user installs skills and plugins after an
/// account exists, so the farm is a reconciler rather than a one-shot.
#[tauri::command]
pub async fn reconcile_account(runtime: String, account_id: String) -> Result<Reconciled, String> {
    let rt = runtime_from_slug(&runtime)?;
    let home = home_dir()?;
    // Filesystem work off the async executor — the repo's rule after the mesh pinwheel.
    tauri::async_runtime::spawn_blocking(move || accounts::reconcile(rt, &account_id, &home))
        .await
        .map_err(|e| format!("reconcile task failed: {e}"))?
}

/// Ask the runtime who it thinks it is inside one account root.
///
/// This is the §5.1 duplicate key and the §6.1 positive verification, both of which need the
/// same call. Claude Code only; other runtimes report status differently and are unsupported.
#[tauri::command]
pub async fn read_account_identity(
    runtime: String,
    account_id: String,
) -> Result<AccountIdentity, String> {
    let rt = runtime_from_slug(&runtime)?;
    if rt != Runtime::Claude {
        return Err(format!(
            "{} identity reads are not implemented yet",
            rt.profile().label
        ));
    }
    let profile = rt.profile();
    let root = accounts::account_root(rt, &account_id)
        .ok_or_else(|| "no data directory available".to_string())?;
    let config_env = profile.config_env.to_string();
    // The SAME list `account_spawn_env` removes, so this reports the identity the tab will
    // actually run as rather than one measured in an environment no tab ever has.
    let scrub: Vec<String> = profile.shadowing_env.iter().map(|s| s.to_string()).collect();
    let cli = accounts::resolve_cli(profile).ok_or_else(|| {
        format!(
            "could not find the `{}` command. A Finder-launched app gets launchd's PATH, which \
             excludes ~/.local/bin and Homebrew.",
            profile.cli
        )
    })?;

    let out = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = Command::new(&cli);
        cmd.args(["auth", "status", "--json"]);
        cmd.env(&config_env, &root);
        for var in &scrub {
            cmd.env_remove(var);
        }
        cmd.output()
    })
    .await
    .map_err(|e| format!("identity task failed: {e}"))?
    .map_err(|e| format!("running {} auth status: {e}", rt.profile().cli))?;

    // Exit status is NOT an error channel here: `auth status --json` exits 1 for the ordinary
    // logged-out case and still prints well-formed JSON on stdout. Every account is logged out
    // between creation and sign-in, so treating non-zero as failure would show a blank error
    // where the pane should show "not signed in" — and make a genuinely broken CLI
    // indistinguishable from the expected state. Parse stdout; fail only if that is unusable.
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| {
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stderr = stderr.trim();
        format!(
            "{} auth status exited {} and produced no usable JSON ({e}){}",
            rt.profile().cli,
            out.status.code().unwrap_or(-1),
            if stderr.is_empty() { String::new() } else { format!(": {stderr}") }
        )
    })?;

    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let auth_method = s("authMethod");
    let api_key_source = s("apiKeySource");
    let api_provider = s("apiProvider");
    let email = s("email");
    let logged_in = v.get("loggedIn").and_then(|x| x.as_bool()).unwrap_or(false);

    // An identity is this root's own subscription login, carrying an email. Anything else —
    // `api_key`, `api_key_helper`, `oauth_token`, `third_party`, `none` — is a source that
    // answered *instead*, and reports no email, so it is not something to compare accounts by.
    let is_account_login =
        logged_in && auth_method.as_deref() == Some("claude.ai") && email.is_some();
    let shadowed_by = if is_account_login {
        None
    } else {
        api_key_source
            .clone()
            .or_else(|| auth_method.clone())
            .filter(|m| m != "none")
    };

    Ok(AccountIdentity {
        is_account_login,
        shadowed_by,
        logged_in,
        auth_method,
        api_key_source,
        api_provider,
        email,
        org_id: s("orgId"),
        org_name: s("orgName"),
        plan: s("subscriptionType"),
    })
}

/// Sign-ins currently in flight, so `cancel_account_login` can reach one. Keyed by the account
/// id the caller supplies, which is why that id is a parameter rather than minted in here: the
/// frontend needs a handle on the attempt before the command returns.
static ACTIVE_LOGINS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<String, LoginHandle>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

struct LoginHandle {
    child: std::sync::Arc<parking_lot::Mutex<std::process::Child>>,
    cancelled: bool,
}

fn register_login(id: &str, child: std::sync::Arc<parking_lot::Mutex<std::process::Child>>) {
    ACTIVE_LOGINS.lock().insert(
        id.to_string(),
        LoginHandle { child, cancelled: false },
    );
}

fn unregister_login(id: &str) {
    ACTIVE_LOGINS.lock().remove(id);
}

fn cancelled(id: &str) -> bool {
    ACTIVE_LOGINS.lock().get(id).map(|h| h.cancelled).unwrap_or(false)
}

/// Event carrying the authorization URL to the UI, so "copy the link and open a private window"
/// is a button rather than an instruction the user cannot follow.
///
/// The URL is not credential material: it is the *start* of an OAuth flow, and completing it
/// still requires the user to authenticate. It is safe to put on the clipboard and is exactly
/// what the runtime prints on stdout for the same purpose.
pub const LOGIN_URL_EVENT: &str = "account-login-url";

/// How long to wait for the shim to be called before falling back to scraping the transcript.
/// The runtime opens the browser within milliseconds of printing, so this only elapses if a
/// future version stops shelling out to `open` altogether — in which case a paste-code link is
/// still better than a dialog waiting forever for one that never comes.
const SHIM_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

/// Shown whenever the only link maiTerm could get hold of is the one the runtime PRINTED.
///
/// That link is not the one the runtime opened. It carries
/// `redirect_uri=https://platform.claude.com/oauth/code/callback`, so completing it means
/// pasting a code back into the CLI — and the child's stdin is `Stdio::null()`, so there is
/// nowhere to paste it. maiTerm therefore refuses to open it, however explicitly the user asked
/// for a private window: a dead end that reports success is worse than a shortcut that says it
/// could not run. **Both** announce paths (the no-shim drain and the poll loop's grace-period
/// fallback) share this string so they cannot drift into telling different stories.
const PASTE_CODE_LINK_NOTE: &str =
    "maiTerm could not take over the browser launch, so the agent opened its own window — \
     finish signing in there. The link below is the paste-code variant and will not complete \
     on its own.";

#[derive(Debug, Clone, Serialize)]
struct LoginUrlEvent {
    account_id: String,
    url: String,
    /// Whether maiTerm successfully launched a browser for this URL. False when the caller asked
    /// for none (copy-to-clipboard) **and** when the launch failed — `open_error` distinguishes
    /// them, and the UI must not claim a window opened on either.
    opened: bool,
    open_error: Option<String>,
    /// True when this link is the one the runtime PRINTED rather than the one it opened — the
    /// `platform.claude.com/oauth/code/callback` variant, which ends in "paste this code" and
    /// so cannot complete against a child with null stdin.
    ///
    /// The UI needs this as a fact, not as a string match on `open_error`: it decides whether
    /// to offer "Open in <browser>" at all. Offering it on a paste-code link invites the user
    /// to walk into the dead end that the Rust side just declined to walk them into.
    paste_code: bool,
    /// True for a mint: offer the code field as a FALLBACK.
    ///
    /// Not the inverse of `paste_code`, and not a claim that a code is required. A mint's link
    /// normally completes in the browser through the runtime's own localhost callback; the
    /// runtime also builds a manual URL whose page shows a code, and some browsers land there.
    /// So this means "a code may appear — have somewhere to put it", where `paste_code` means
    /// "this link cannot finish at all".
    needs_code: bool,
}

/// Pull the authorization URL out of a runtime's sign-in output.
///
/// Not `split_whitespace().find(...)`: a URL is not whitespace-terminated in the presence of
/// control bytes, so a decorated line would hand back a link with an escape sequence glued to
/// its end. Cut at the first whitespace *or* control character, and keep looking past any
/// earlier `https://` that is not the authorization link.
///
/// **`require_terminator` is the difference between the two callers, and it matters.** A
/// complete transcript may legitimately end mid-URL — there is no more output coming, and half a
/// link is still better than nothing in a timeout message. A *streaming* transcript ends at the
/// last `read()`, which can land anywhere, and the caller latches on the first answer: emitting a
/// truncated link there means the UI shows a URL that OAuth rejects and never corrects it when
/// the rest arrives 30ms later. When streaming, an unterminated tail means "not yet", not "done".
fn extract_login_url(transcript: &str, require_terminator: bool) -> Option<String> {
    let mut from = 0usize;
    while let Some(rel) = transcript[from..].find("https://") {
        let start = from + rel;
        let rest = &transcript[start..];
        let terminator = rest.find(|c: char| c.is_whitespace() || c.is_control());
        if require_terminator && terminator.is_none() {
            return None;
        }
        let url = &rest[..terminator.unwrap_or(rest.len())];
        if url.contains("oauth") || url.contains("authorize") {
            return Some(url.to_string());
        }
        from = start + "https://".len();
    }
    None
}

/// The tail of what the runtime said, for an error message.
///
/// Sign-in failures are reported by the runtime itself — `Login failed: <reason>` on stderr —
/// and that reason is the difference between "try again" and knowing the org requires SSO.
/// Without it the user gets an exit code and nothing to act on.
///
/// Bounded and sanitised because it goes into a message box, not a log: last few non-empty
/// lines, control bytes dropped. The transcript carries no credential material — the
/// authorization URL and a paste prompt are all that is on those streams.
fn transcript_tail(transcript: &str) -> Option<String> {
    let tail: Vec<&str> = transcript
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .rev()
        .take(3)
        .collect();
    if tail.is_empty() {
        return None;
    }
    let text: String = tail
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" · ")
        .chars()
        .filter(|c| !c.is_control())
        .take(400)
        .collect();
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Turn a stuck sign-in into something a user can act on. The runtime prints the authorization
/// URL on stdout; when the browser did not open, that line is the whole difference between
/// "try again" and "there is nothing I can do".
fn timeout_message(transcript: &str, flow: &BrowserFlow) -> String {
    // The URL is safe to show even for a secret flow: it is the START of an OAuth exchange, and
    // whoever opens it still has to authenticate. The TOKEN would not be, but it only exists
    // once the flow has succeeded, and this message is only built when it has not.
    match extract_login_url(transcript, false) {
        Some(u) => format!(
            "{} timed out. If the browser did not open, visit: {u}",
            flow.label
        ),
        None => format!("{} timed out before the browser flow completed.", flow.label),
    }
}

/// Blank anything shaped like an Anthropic secret.
///
/// Belt, not braces: the real guard is that a secret transcript never reaches an error or an
/// event in the first place. This exists because `setup-token`'s output IS a credential, error
/// paths are the ones nobody exercises, and a token that escapes is valid for a year.
fn redact_secrets(text: &str) -> String {
    const MARK: &str = "sk-ant-";
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find(MARK) {
        out.push_str(&rest[..i]);
        let after = &rest[i..];
        // To the first whitespace/control char — the same boundary `extract_login_url` uses, and
        // the same reason: a token is not whitespace-terminated when output is decorated.
        let end = after
            .char_indices()
            .find(|(n, c)| *n > 0 && (c.is_whitespace() || c.is_control()))
            .map(|(n, _)| n)
            .unwrap_or(after.len());
        out.push_str("sk-ant-<redacted>");
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{extract_login_url, redact_secrets, transcript_tail};

    #[test]
    fn finds_the_token_wherever_the_runtime_prints_it() {
        use super::extract_setup_token;
        let tok = "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789";

        // Not the last line — the runtime is free to print a hint after it, and betting on
        // position is how the URL scraper got this wrong before (§5.2.2).
        let t = format!("Authorized.\n{tok}\nThis is saved nowhere. Copy it now.\n");
        let got = extract_setup_token(&t, false).expect("found");
        assert_eq!(got.expose(), tok);
        assert!(got.looks_like_setup_token());

        // Trailing newline only — the ordinary case. `Secret::new` trims, so what comes out is
        // what gets stored and injected.
        assert_eq!(
            extract_setup_token(&format!("{tok}\n"), false).unwrap().expose(),
            tok
        );

        // Nothing there: a flow that succeeded but printed no token must be an error, not an
        // empty Secret that the vault would then have to catch.
        assert!(extract_setup_token("Authorized.\nAll done.\n", false).is_none());
        assert!(extract_setup_token("", false).is_none());
    }

    /// The PTY read is incremental, so a token can be half-written when we look. Requiring a
    /// terminator is what stops a partial read being stored as a whole credential — one that
    /// passes every shape check there is and resolves as nobody.
    #[test]
    fn a_half_written_token_is_not_taken_while_the_mint_is_still_streaming() {
        use super::extract_setup_token;
        let partial = "…printing token…\n\u{1b}[32msk-ant-oat01-abcdefghijklmnop";
        assert!(extract_setup_token(partial, true).is_none(), "still streaming");
        // Once the child is gone nothing more is coming, so the tail is all there is.
        assert!(extract_setup_token(partial, false).is_some());

        // An ANSI sequence right after the token terminates it, which is what makes a styled
        // TUI render readable at all.
        let styled = "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789\u{1b}[0m rest";
        let got = extract_setup_token(styled, true).expect("terminated by the escape");
        assert_eq!(got.expose(), "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789");
    }

    #[test]
    fn a_truncated_token_is_found_but_refused_by_the_vault() {
        use super::extract_setup_token;
        // Extraction is deliberately permissive — it finds the marker — and the SHAPE check
        // lives in the vault, so there is one gate rather than two that can disagree. This is
        // the pairing that matters: found here, rejected there, never stored.
        let got = extract_setup_token("sk-ant-oat01-short\n", false).expect("found the marker");
        assert!(!got.looks_like_setup_token(), "must not pass the vault's check");
    }

    #[test]
    fn redaction_blanks_a_token_but_keeps_the_message_around_it() {
        // The shape of a real `setup-token` failure tail: useful prose either side of the one
        // thing that must not survive.
        let t = "Created token sk-ant-oat01-abcdefghijklmnop0123456789 for you · done";
        let r = redact_secrets(t);
        assert!(!r.contains("abcdefghijklmnop"), "the token survived: {r}");
        assert!(r.contains("Created token"), "lost the diagnostic: {r}");
        assert!(r.contains("for you · done"), "lost the diagnostic: {r}");
        assert!(r.contains("sk-ant-<redacted>"));
    }

    #[test]
    fn redaction_handles_the_awkward_positions() {
        // End of input with no trailing whitespace — the `unwrap_or(after.len())` arm.
        assert_eq!(
            redact_secrets("token: sk-ant-oat01-zzzz"),
            "token: sk-ant-<redacted>"
        );
        // Several on one line, which a retry loop produces.
        let two = redact_secrets("sk-ant-oat01-aaaa and sk-ant-api03-bbbb");
        assert_eq!(two, "sk-ant-<redacted> and sk-ant-<redacted>");
        assert!(!two.contains("aaaa") && !two.contains("bbbb"));
        // Nothing to do, and nothing damaged.
        assert_eq!(redact_secrets("all fine here"), "all fine here");
        // The marker itself with nothing after it must terminate rather than spin — the scan
        // advances past the marker because the boundary search skips index 0.
        assert_eq!(redact_secrets("sk-ant-"), "sk-ant-<redacted>");
        assert_eq!(redact_secrets("sk-ant- x"), "sk-ant-<redacted> x");
    }

    #[test]
    fn finds_a_plain_url() {
        let t = "Opening browser…\nIf it didn't open, visit: https://claude.ai/oauth/authorize?code=1&x=2\n";
        assert_eq!(
            extract_login_url(t, true).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=1&x=2")
        );
    }

    #[test]
    fn strips_a_trailing_colour_reset() {
        // The reason this is not `split_whitespace`: a reset sequence glued to the URL means a
        // whitespace split hands the user a link with `\u{1b}[0m` on the end of it.
        let t = "visit: \u{1b}[4mhttps://claude.ai/oauth/authorize?code=1\u{1b}[0m and sign in";
        assert_eq!(
            extract_login_url(t, true).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=1")
        );
    }

    #[test]
    fn skips_a_url_that_is_not_the_authorization_link() {
        let t = "docs: https://docs.claude.com/en/docs\nvisit: https://claude.ai/oauth/authorize?code=1\n";
        assert_eq!(
            extract_login_url(t, true).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=1")
        );
    }

    #[test]
    fn none_before_the_url_is_printed() {
        assert_eq!(extract_login_url("Starting sign-in…\n", true), None);
    }

    #[test]
    fn streaming_waits_for_the_whole_url() {
        // A `read()` can stop anywhere. The streaming caller latches on the first answer, so
        // announcing this prefix would leave the UI showing a link OAuth rejects, permanently.
        let half = "visit: https://claude.ai/oauth/authorize?code=abc&challenge=Q";
        assert_eq!(extract_login_url(half, true), None);

        let whole = format!("{half}xYz&state=42\n");
        assert_eq!(
            extract_login_url(&whole, true).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=abc&challenge=QxYz&state=42")
        );
    }

    #[test]
    fn a_complete_transcript_accepts_an_unterminated_tail() {
        // The timeout path has no more output coming. Half a link beats nothing there.
        let t = "visit: https://claude.ai/oauth/authorize?code=abc";
        assert_eq!(
            extract_login_url(t, false).as_deref(),
            Some("https://claude.ai/oauth/authorize?code=abc")
        );
    }

    #[test]
    fn tail_carries_the_runtimes_own_explanation() {
        let t = "Opening browser…\n\nLogin failed: Your organization requires SSO\n";
        assert_eq!(
            transcript_tail(t).as_deref(),
            Some("Opening browser… · Login failed: Your organization requires SSO")
        );
    }

    #[test]
    fn tail_drops_control_bytes_and_is_bounded() {
        let t = format!("\u{1b}[31mLogin failed: {}\u{1b}[0m\n", "x".repeat(600));
        let tail = transcript_tail(&t).expect("a tail");
        assert!(!tail.contains('\u{1b}'), "control bytes survived: {tail:?}");
        // CHARS, not bytes — `take(400)` runs on a `chars()` iterator. Asserting `len()` here
        // passed only because this fixture is ASCII, and would have said nothing at all about
        // the multi-byte case below.
        assert!(tail.chars().count() <= 400, "unbounded tail: {}", tail.chars().count());
        assert!(tail.starts_with("[31mLogin failed: "));
    }

    #[test]
    fn tail_is_bounded_in_chars_even_when_they_are_wide() {
        // 400 emoji is ~1.6KB. Fine for a message box, but the bound is a character count and
        // the test above must not be read as promising a byte count.
        let t = format!("Login failed: {}", "🙂".repeat(600));
        let tail = transcript_tail(&t).expect("a tail");
        assert_eq!(tail.chars().count(), 400);
        assert!(tail.len() > 400, "expected wide chars to exceed the char bound in bytes");
    }

    #[test]
    fn tail_is_none_when_nothing_was_said() {
        assert_eq!(transcript_tail(""), None);
        assert_eq!(transcript_tail("  \n\n \n"), None);
    }
}

/// Stop a sign-in that is still running. The child is killed, so the browser flow cannot
/// complete later and strand an authenticated root nobody knows about.
#[tauri::command]
pub async fn cancel_account_login(account_id: String) -> Result<(), String> {
    // A mint is cancellable through the same control, because from the dialog it is the same
    // action. Flagged rather than killed here: the mint's own loop owns the child and the PTY,
    // and it is the only thing that can tear both down in the right order.
    if let Some(handle) = ACTIVE_MINTS.lock().get_mut(&account_id) {
        handle.cancelled = true;
    }
    let mut guard = ACTIVE_LOGINS.lock();
    let Some(handle) = guard.get_mut(&account_id) else {
        // Already finished or never started. Nothing to stop, and not an error.
        return Ok(());
    };
    handle.cancelled = true;
    let mut child = handle.child.lock();
    let _ = child.kill();
    Ok(())
}

/// The result of an add-account attempt. The caller decides what to do with it: the duplicate
/// check is §5.1's and belongs where the existing account list lives.
#[derive(Debug, Clone, Serialize)]
pub struct NewAccount {
    /// The root exists on disk under this id. If the caller rejects the account — duplicate,
    /// or the user cancelled — it must call `discard_account_root`, or the directory leaks.
    pub account_id: String,
    pub identity: AccountIdentity,
}

/// Run a runtime's interactive sign-in against a fresh account root.
///
/// The root is created and reconciled first, so the runtime's own login writes into a directory
/// that already has the hook and MCP farm (§5.4) rather than a bare one.
///
/// This is §5.2's **fallback** path: the runtime opens the user's default browser, which may
/// already hold a session and silently return the account they already have. That is expected,
/// not prevented here — the §5.1 duplicate check on the returned identity is what catches it.
/// The in-app incognito webview that avoids it is a later upgrade to this same command.
///
/// **maiTerm decides where the link opens, not the runtime.** `open_with` is `"default"` for the
/// ordinary browser, a browser id for a private window (§5.2.1), or `None` to open nothing —
/// the copy-to-clipboard path. The runtime's own launcher is shadowed either way, for two
/// reasons: it would otherwise put a window signed in to the very account being avoided next to
/// the private one, with Authorize a click away; and shadowing it is the only way to see the URL
/// it actually uses, which is NOT the one it prints. See `accounts::browser`.
#[tauri::command]
pub async fn begin_account_login(
    app: tauri::AppHandle,
    runtime: String,
    account_id: String,
    timeout_secs: Option<u64>,
    open_with: Option<String>,
) -> Result<NewAccount, String> {
    let rt = runtime_from_slug(&runtime)?;
    let profile = rt.profile();
    if !profile.supported {
        return Err(format!("{} accounts are not supported yet", profile.label));
    }
    if rt != Runtime::Claude {
        return Err(format!("{} sign-in is not implemented yet", profile.label));
    }

    // The id comes from the caller so it has a handle on the attempt BEFORE this returns —
    // otherwise there is nothing to pass to `cancel_account_login` while the sign-in is the
    // very thing that has not come back yet.
    if uuid::Uuid::parse_str(&account_id).is_err() {
        return Err("account id must be a UUID".to_string());
    }
    let home = home_dir()?;
    let id_for_reconcile = account_id.clone();
    tauri::async_runtime::spawn_blocking(move || accounts::reconcile(rt, &id_for_reconcile, &home))
        .await
        .map_err(|e| format!("reconcile task failed: {e}"))??;

    // From here the root exists, so EVERY failure has to remove it. Cleaning up only the
    // sign-in's own error left three leaking paths — a missing CLI (one leaked root per click
    // for anyone whose `claude` lives somewhere resolve_cli does not look), a join error, and a
    // failed identity read AFTER a successful login, which strands a root holding a live
    // credential that no UI can reach.
    let outcome = login_into_root(&app, rt, &account_id, timeout_secs, open_with).await;
    match outcome {
        Ok(identity) => Ok(NewAccount { account_id, identity }),
        Err(e) => {
            let id = account_id.clone();
            let _ =
                tauri::async_runtime::spawn_blocking(move || accounts::remove_root(rt, &id)).await;
            Err(e)
        }
    }
}

/// A runtime subcommand that authenticates through a browser.
///
/// Two of them exist — `auth login` (§5) and `setup-token` (§6) — and they need identical
/// plumbing: the same config-dir env, the same scrub of every shadowing variable, the same
/// shadowed browser opener, the same drain-and-poll. **One copy of that plumbing, not two.**
/// It took several rounds to get right (the shim/drain race, the two-URL problem), a review
/// found a defect in it after that, and a second copy would be a second place for the next one
/// to hide.
struct BrowserFlow {
    /// Argv after the CLI name.
    args: &'static [&'static str],
    /// Sentence-initial, for error text: "Sign-in failed…".
    label: &'static str,
}

/// `auth login` — §5. Output is a URL and progress chatter, never a credential.
/// The only flow that runs here. The mint used to as well, until it turned out to need a PTY
/// and a way to type into it — see `run_mint_on_pty`.
static LOGIN_FLOW: BrowserFlow = BrowserFlow {
    args: &["auth", "login"],
    label: "Sign-in",
};

/// Run a browser flow against an account root and hand back the child's combined output.
///
/// The returned transcript is progress chatter and a URL — never a credential. The one flow
/// whose output *was* the credential is the mint, and it no longer runs here.
///
/// Split out of `begin_account_login` so that caller can clean the root up on any error without
/// every early return having to remember.
async fn run_browser_flow(
    app: &tauri::AppHandle,
    rt: Runtime,
    account_id: &str,
    timeout_secs: Option<u64>,
    open_with: Option<String>,
    flow: &'static BrowserFlow,
) -> Result<String, String> {
    let profile = rt.profile();
    let account_id = account_id.to_string();
    let root = accounts::account_root(rt, &account_id)
        .ok_or_else(|| "no data directory available".to_string())?;
    let config_env = profile.config_env.to_string();
    let scrub: Vec<String> = profile.shadowing_env.iter().map(|s| s.to_string()).collect();
    let cli = accounts::resolve_cli(profile)
        .ok_or_else(|| format!("could not find the `{}` command", profile.cli))?;
    // Long enough for a real person to find the browser window and authenticate; short enough
    // that a flow which never completes releases the process instead of leaking it forever.
    let deadline = std::time::Duration::from_secs(timeout_secs.unwrap_or(300).clamp(30, 900));

    let id_for_task = account_id.clone();
    // Created OUT here, not inside the closure, because the caller needs the output after the
    // child exits — a mint's whole result is in it. The closure gets a clone.
    let transcript_out = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
    let transcript = transcript_out.clone();
    let app = app.clone();
    let login = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let mut cmd = Command::new(&cli);
        cmd.args(flow.args);
        cmd.env(&config_env, &root);
        for var in &scrub {
            cmd.env_remove(var);
        }

        // Always, not just for the private-window path: this is also how we learn the URL the
        // runtime really uses. Kept alive for the child's whole life — dropping it deletes the
        // shim, and a child reaching a PATH entry that no longer exists falls through to the
        // real `open`.
        let suppressor = accounts::browser::suppress_default_browser();
        if let Some(s) = &suppressor {
            let mut entries = vec![s.path_entry().to_path_buf()];
            entries.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            match std::env::join_paths(entries) {
                Ok(p) => {
                    cmd.env("PATH", p);
                }
                // A PATH we cannot rebuild is not worth failing a sign-in over; the user just
                // gets the extra browser window back.
                Err(e) => log::warn!("accounts: could not shadow the browser opener: {e}"),
            }
        }
        // No tty to prompt on. The sign-in completes through the runtime's own local callback
        // server, not the "paste code" fallback, so stdin is genuinely unused.
        cmd.stdin(std::process::Stdio::null());
        // Capture both streams rather than inheriting them. Inherited output goes to the app's
        // stdout, which is /dev/null for a Finder-launched bundle — and the one line a stuck user
        // needs is printed there: "If the browser didn't open, visit: <url>". Which of the two
        // streams carries it is a runtime detail that can change between versions, so both are
        // drained into one transcript instead of betting on stdout.
        cmd.stdout(std::process::Stdio::piped());
        cmd.stderr(std::process::Stdio::piped());
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("starting {}: {e}", flow.label.to_lowercase()))?;

        // Drain on their own threads: a full pipe would otherwise block the child forever while
        // we poll it, which is a deadlock rather than a slow sign-in.
        //
        // The drain also watches for the authorization URL and emits it the moment it appears,
        // rather than only reporting it in the timeout message. That is what makes "copy the
        // link and finish in a private window" possible WHILE the sign-in is still waiting —
        // after it has timed out is too late, because the link has expired with it.
        //
        // **The stdout URL is the FALLBACK, used only when the shim could not be installed.**
        // The printed line carries `redirect_uri=https://platform.claude.com/oauth/code/callback`
        // — the branch that shows a code to paste, which is a dead end here because stdin is
        // null. The browser gets `redirect_uri=http://localhost:<port>/callback`, which completes
        // by itself. The poll loop below prefers what the shim captured for exactly that reason.
        // Moved in from the caller (see above); the drains fill it and the caller reads it.
        // Shared, not per-thread: two drains each holding their own "have I announced?" would
        // both fire if the URL landed on both streams.
        let announced = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        // **The drains must not announce while a shim is installed.** The runtime prints its
        // URL and calls `open` at essentially the same moment, and the drain thread reliably
        // won that race — latching `announced`, emitting the paste-code URL, and skipping the
        // poll loop's shim branch, which is the only thing that opens a browser. The symptom was
        // "Sign in privately does nothing at all". stdout is the fallback for when there is no
        // shim to prefer, and the poll loop re-enables it if the shim never fires.
        let scrape_streams = suppressor.is_none();
        let drain = |reader: Option<Box<dyn std::io::Read + Send>>| {
            let Some(mut reader) = reader else { return };
            let sink = transcript.clone();
            let app = app.clone();
            let id = id_for_task.clone();
            let announced = announced.clone();
            // What the caller asked for, so the no-shim path can explain why it did not happen.
            let ow = open_with.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    let mut sink = sink.lock();
                    sink.push_str(&String::from_utf8_lossy(&buf[..n]));
                    // Always keep filling the transcript — it is what explains a failure — but
                    // only announce from it when nothing better is coming.
                    if !scrape_streams || announced.load(std::sync::atomic::Ordering::Relaxed) {
                        continue;
                    }
                    // Streaming: require the URL to be terminated before announcing it. This
                    // read may have stopped in the middle of the link, and the latch below
                    // means a truncated one would be the only one the UI ever gets.
                    if let Some(url) = extract_login_url(&sink, true) {
                        // swap, not store: whichever drain sees it first is the one that emits.
                        if !announced.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            let _ = app.emit(
                                LOGIN_URL_EVENT,
                                // The no-shim fallback: the runtime opened its own browser, so
                                // maiTerm opened nothing and must not claim otherwise.
                                //
                                // **And it must not open this URL either, however much the user
                                // asked for a private window.** Without the shim the only link
                                // maiTerm can see is the one on stdout, and that is the
                                // paste-code variant — `redirect_uri` pointing at
                                // platform.claude.com rather than the localhost callback the
                                // runtime actually opened. It cannot complete: finishing it
                                // means typing a code back into a child whose stdin is null.
                                // Opening it privately would therefore look like it worked and
                                // strand the user on a page that goes nowhere, which is worse
                                // than opening nothing. Say so instead: a shortcut that
                                // silently does nothing is the thing to avoid here, not the
                                // missing window.
                                LoginUrlEvent {
                                    account_id: id.clone(),
                                    url,
                                    opened: false,
                                    open_error: ow
                                        .as_deref()
                                        .filter(|w| *w != "default")
                                        .map(|_| PASTE_CODE_LINK_NOTE.to_string()),
                                    // Always: this branch only ever runs with no shim, so the
                                    // only link it can see is the printed one.
                                    paste_code: true,
                                    needs_code: false,
                                },
                            );
                        }
                    }
                }
            });
        };
        drain(child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>));
        drain(child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>));

        let child = std::sync::Arc::new(parking_lot::Mutex::new(child));
        register_login(&id_for_task, child.clone());
        let start = std::time::Instant::now();
        let result = loop {
            // Did the runtime try to open a browser? If so that argv holds the REAL URL, which
            // takes precedence over anything the drains scraped, and it is maiTerm's job to open
            // it now that the runtime's own launcher is shadowed.
            if let Some(s) = &suppressor {
                if !announced.load(std::sync::atomic::Ordering::Relaxed) {
                    // The shim's capture, or — if it has not fired after a grace period — the
                    // transcript. A runtime that stopped shelling out to `open` would otherwise
                    // leave the dialog waiting for a link that never arrives, with the URL
                    // sitting unread in the transcript the whole time. The paste-code branch is
                    // a poor link; no link is worse.
                    let captured =
                        s.captured().as_deref().and_then(|c| extract_login_url(c, false));
                    // **Where the URL came from decides whether maiTerm may OPEN it**, not just
                    // whether it can show it. The shim's capture is the argv the runtime handed
                    // to `open`: the real `redirect_uri=http://localhost:<port>/callback` link,
                    // which completes on its own. The transcript fallback is the link the
                    // runtime PRINTED, and that is the paste-code variant — finishing it means
                    // typing a code back into a child whose stdin is null, so it cannot
                    // complete at all. Opening that in a private window would look like it
                    // worked and strand the user on a dead end; the no-shim drain path already
                    // refuses to do exactly that, and this branch must follow the same rule.
                    // (Reachable whenever the shim is on PATH but never fires — a `join_paths`
                    // failure, or a runtime that stops shelling out to `open`.)
                    let from_shim = captured.is_some();
                    let url = captured.or_else(|| {
                        (start.elapsed() > SHIM_GRACE)
                            .then(|| extract_login_url(&transcript.lock(), true))
                            .flatten()
                    });
                    if let Some(url) = url {
                        if !announced.swap(true, std::sync::atomic::Ordering::Relaxed) {
                            // Open FIRST, then emit — so the event carries what actually
                            // happened rather than what was intended. The UI announces "Opened
                            // in Chrome"; it should not say that when the launch failed, which
                            // it cannot know unless we tell it. Best effort either way: the
                            // link and its Copy button stay on screen, so none of this aborts
                            // the sign-in.
                            let open_error = if !from_shim {
                                // Nothing to open. `default` gets no complaint: the runtime's
                                // own launcher was never shadowed in this case, so it has
                                // already opened the window that user asked for.
                                open_with
                                    .as_deref()
                                    .filter(|w| *w != "default")
                                    .map(|_| PASTE_CODE_LINK_NOTE.to_string())
                            } else {
                                match open_with.as_deref() {
                                    Some("default") => accounts::browser::open_default(&url).err(),
                                    Some(id) => {
                                        accounts::browser::open_private_window(id, &url).err()
                                    }
                                    // Copy-to-clipboard: deliberately opens nothing.
                                    None => None,
                                }
                            };
                            if let Some(e) = &open_error {
                                log::warn!("accounts: opening the sign-in link: {e}");
                            }
                            let _ = app.emit(
                                LOGIN_URL_EVENT,
                                LoginUrlEvent {
                                    account_id: id_for_task.clone(),
                                    url: url.clone(),
                                    // `from_shim` has to be in here too. Without it a
                                    // transcript URL with `open_with: Some("default")` reports
                                    // `opened: true` having opened nothing, and the dialog
                                    // tells the user to go and finish in a window that is not
                                    // the one the runtime actually opened.
                                    opened: from_shim
                                        && open_with.is_some()
                                        && open_error.is_none(),
                                    open_error,
                                    paste_code: !from_shim,
                                    needs_code: false,
                                },
                            );
                        }
                    }
                }
            }
            let status = child.lock().try_wait();
            match status {
                Ok(Some(status)) if status.success() => break Ok(()),
                Ok(Some(status)) => {
                    // Cancellation is checked HERE, before the exit is classified, not only in
                    // the still-running arm below. Cancelling SIGKILLs the child, so by the time
                    // this thread wakes from its 250ms sleep the process is usually already
                    // reaped — the `Ok(Some)` arm wins the race almost every time, and a signal
                    // death has no exit code. A user who deliberately cancelled was being told
                    // "sign-in exited -1".
                    if cancelled(&id_for_task) {
                        break Err(format!("{} cancelled", flow.label));
                    }
                    // Not the logged-out case this time: a failed *login* really is an error —
                    // and the runtime has already explained it on stderr ("Login failed: …"),
                    // which is now in the transcript. Without that the user gets an exit code
                    // and nothing to act on, which is the same as nothing.
                    let code = status.code().unwrap_or(-1);
                    break Err(match transcript_tail(&transcript.lock()) {
                        Some(tail) => format!("{} failed: {tail}", flow.label),
                        None => format!("{} failed (exited {code})", flow.label),
                    });
                }
                Ok(None) => {
                    if cancelled(&id_for_task) {
                        let mut c = child.lock();
                        let _ = c.kill();
                        let _ = c.wait();
                        break Err(format!("{} cancelled", flow.label));
                    }
                    if start.elapsed() > deadline {
                        let mut c = child.lock();
                        let _ = c.kill();
                        let _ = c.wait();
                        break Err(timeout_message(&transcript.lock(), flow));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                Err(e) => break Err(format!("waiting on {}: {e}", flow.label.to_lowercase())),
            }
        };
        unregister_login(&id_for_task);
        result
    })
    .await
    .map_err(|e| format!("{} task failed: {e}", flow.label.to_lowercase()))?;
    login?;

    // The child is gone and every drain thread has closed its pipe, so this is the complete
    // output. Ownership moves to the caller, which is what makes the secret case its problem
    // and not this function's.
    Ok(std::sync::Arc::try_unwrap(transcript_out)
        .map(|m| m.into_inner())
        .unwrap_or_else(|arc| arc.lock().clone()))
}

/// The part of `begin_account_login` that runs once the root exists.
async fn login_into_root(
    app: &tauri::AppHandle,
    rt: Runtime,
    account_id: &str,
    timeout_secs: Option<u64>,
    open_with: Option<String>,
) -> Result<AccountIdentity, String> {
    run_browser_flow(app, rt, account_id, timeout_secs, open_with, &LOGIN_FLOW).await?;
    read_account_identity(rt.slug().to_string(), account_id.to_string()).await
}

/// Mints waiting for their authorization code, so `submit_account_code` can reach one.
///
/// Separate from `ACTIVE_LOGINS` because the two hold different things: a sign-in owns a
/// `std::process::Child` and only ever needs killing, while a mint owns the write end of a PTY
/// and exists precisely so something can be typed into it.
static ACTIVE_MINTS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<String, MintHandle>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

struct MintHandle {
    /// The PTY master's writer — how the code reaches the TUI. `None` until the child is up:
    /// the entry is created *before* that so Cancel can reach a mint during the seconds it
    /// spends reconciling the config root and opening a terminal.
    writer: Option<std::sync::Arc<parking_lot::Mutex<Box<dyn std::io::Write + Send>>>>,
    cancelled: bool,
    /// A code is in flight. Guards against a double-click, not against a retry — a code the CLI
    /// rejects leaves it prompting again, so this clears when the write fails and the dialog can
    /// offer another go.
    code_sent: bool,
}

/// Removes its account's entry from `ACTIVE_MINTS` however the mint ends.
///
/// A `Drop` guard rather than a call at the end, because `mint_account_token` has half a dozen
/// `?` returns after the entry exists and every one of them would otherwise leave a handle
/// behind — after which `submit_account_code` answers a *live* mint with "already submitted",
/// and a second mint is refused for an account that is not minting.
struct MintRegistration(String);

impl Drop for MintRegistration {
    fn drop(&mut self) {
        ACTIVE_MINTS.lock().remove(&self.0);
    }
}

/// Claim the mint slot for an account, before anything slow happens.
///
/// Refuses a second concurrent mint for the same account rather than overwriting: the registry
/// is keyed by account id, so last-writer-wins would have the older mint's cleanup delete the
/// younger one's handle and strand it.
fn register_mint(account_id: &str) -> Result<MintRegistration, String> {
    let mut guard = ACTIVE_MINTS.lock();
    if guard.contains_key(account_id) {
        return Err("a mint is already running for this account".to_string());
    }
    guard.insert(
        account_id.to_string(),
        MintHandle { writer: None, cancelled: false, code_sent: false },
    );
    Ok(MintRegistration(account_id.to_string()))
}

fn mint_cancelled(account_id: &str) -> bool {
    ACTIVE_MINTS.lock().get(account_id).map(|h| h.cancelled).unwrap_or(false)
}

/// How wide the mint's PTY is.
///
/// **Not cosmetic.** The token is printed by a TUI, and a TUI wraps at the terminal width — a
/// token broken across two rows has a newline in the middle of it, which is a control character,
/// which is where `extract_setup_token` stops. It would extract a truncated credential that
/// looks entirely valid and resolves as nobody. Wide enough that nothing it prints can wrap.
const MINT_PTY_COLS: u16 = 400;

/// Hand a mint the authorization code the browser showed.
///
/// **A fallback, not the main path — and the first version of this comment had that backwards.**
/// `claude setup-token` *does* run its own `127.0.0.1` callback server (verified against
/// 2.1.278: `lsof -p <pid> -a -i` on a freshly started one shows a LISTEN socket, and the URL it
/// hands to `open` carries `redirect_uri=http://localhost:<port>/callback`). Approving in the
/// browser normally finishes the flow with nothing typed at all. The `code=true` parameter is on
/// *both* URLs it builds and is not the paste-code marker — `redirect_uri` is.
///
/// What it also builds is a manual variant, shown on screen, whose callback page displays a code
/// instead. This exists for the browser that lands there.
///
/// The earlier claim — "no localhost callback, so it cannot complete by itself" — came from
/// running `lsof` on a process whose callback had **already fired**, so the server was already
/// closed. An absence read as a claim, in a feature whose whole hazard is exactly that.
///
/// The PTY is still required, for the *other* reason: `setup-token` is an ink TUI and given
/// piped stdio it prints **nothing at all** — no URL, no token, no error. The first version gave
/// it `Stdio::null()` and pipes, so when the flow completed and the token was rendered, maiTerm
/// saw zero bytes and the CLI sat on its final frame. The pane stayed on "Minting…" until the
/// deadline killed a child that had done its job.
///
/// The code is not credential material on its own — it is one half of an exchange that also
/// needs the PKCE verifier held by the child process — but it is treated as write-only anyway:
/// never logged, never echoed back.
#[tauri::command]
pub async fn submit_account_code(account_id: String, code: String) -> Result<(), String> {
    let code = code.trim().to_string();
    if code.is_empty() {
        return Err("no code to submit".to_string());
    }
    // Typed straight at a live TUI's stdin, so anything that is not a single printable line is
    // refused rather than sanitised. A newline in the middle would submit half of it and leave
    // the rest to be read as the answer to whatever comes next.
    if code.len() > 512 || code.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("that does not look like an authorization code".to_string());
    }

    let writer = {
        let mut guard = ACTIVE_MINTS.lock();
        let Some(handle) = guard.get_mut(&account_id) else {
            return Err("that mint is no longer running".to_string());
        };
        if handle.code_sent {
            return Err("a code is already being submitted for this mint".to_string());
        }
        let Some(writer) = handle.writer.clone() else {
            return Err("that mint has not started its terminal yet — try again in a moment".to_string());
        };
        handle.code_sent = true;
        writer
    };

    // `\r`, not `\n`: the child is on a tty in raw mode, where Enter arrives as carriage return.
    let written = {
        let mut w = writer.lock();
        w.write_all(code.as_bytes())
            .and_then(|_| w.write_all(b"\r"))
            .and_then(|_| w.flush())
    };

    // **Released on failure, so a rejected code is not a dead end.** The CLI validates the
    // `<code>#<state>` shape itself and re-prompts on a bad one ("Invalid code. Please make sure
    // the full code was copied" — copying only the half before the `#` is common enough that
    // Anthropic wrote an error for it). Latching here permanently would leave the dialog saying
    // "Finishing…" at a TUI waiting to be told again, until the deadline.
    if written.is_err() {
        if let Some(handle) = ACTIVE_MINTS.lock().get_mut(&account_id) {
            handle.code_sent = false;
        }
    }
    written.map_err(|e| format!("could not hand the code to the mint: {e}"))
}

/// Let the dialog offer the code field again after a code the CLI did not accept.
#[tauri::command]
pub async fn reset_account_code(account_id: String) -> Result<(), String> {
    if let Some(handle) = ACTIVE_MINTS.lock().get_mut(&account_id) {
        handle.code_sent = false;
    }
    Ok(())
}

/// What the frontend learns about a mint. **Never the token.**
#[derive(Debug, Clone, Serialize)]
pub struct TokenMint {
    /// Unix seconds, for `ManagedAccount.token_minted_at`. Expiry is derived from this and
    /// nothing else — §7 forbids parsing the credential, which is an undocumented private
    /// format `auth status --json` does not expose anyway.
    pub minted_at: u64,
    /// The identity the token ACTUALLY resolved to, read back before storing it. Displayed so
    /// the user can see the mint landed on the account they picked.
    pub identity: AccountIdentity,
}

/// Find the `sk-ant-oat01-…` token in a mint's output.
///
/// Scans for the marker rather than taking the last line: the runtime is free to print a
/// trailing blank line or a "saved nowhere, copy this" hint after it, and betting on position
/// is how the URL scraper got this wrong before (§5.2.2).
///
/// `require_terminated` decides what an unterminated match means. Reading a PTY, a token can be
/// split across two reads, and the half already in the buffer is a perfectly plausible token —
/// so while the mint is still streaming, only a match followed by whitespace or a control
/// character (an ANSI sequence counts) is complete. Once the child has exited nothing more is
/// coming, and the last thing in the buffer is the whole of what there is.
fn extract_setup_token(transcript: &str, require_terminated: bool) -> Option<accounts::vault::Secret> {
    let i = transcript.find("sk-ant-oat01-")?;
    let after = &transcript[i..];
    let terminator = after
        .char_indices()
        .find(|(_, c)| c.is_whitespace() || c.is_control())
        .map(|(n, _)| n);
    if require_terminated && terminator.is_none() {
        return None;
    }
    // `Secret::new` trims; the vault validates the shape before it stores anything.
    Some(accounts::vault::Secret::new(&after[..terminator.unwrap_or(after.len())]))
}

/// Drive `claude setup-token` to completion on a PTY, and hand back the token.
///
/// **A PTY, not pipes, and that is forced.** `setup-token` is an ink TUI: with piped stdio it
/// writes nothing whatsoever — not the URL, not the token, not an error — so a completed mint
/// and a hung one are indistinguishable, and there is nothing to read the token out of. It also
/// does not exit once it has printed: it sits on a final frame. See `submit_account_code` for
/// the measurements, including the one the first version of this got wrong.
///
/// **Everything this reads is credential material.** The token is printed into the same stream
/// as the rest of the render, so the buffer is never logged, never emitted, and never returned;
/// only the token comes out, and error text is built from a redacted tail.
///
/// The link still comes from the browser shim rather than from the output, for a reason that is
/// now doubled: the TUI wraps its URL in an OSC 8 hyperlink and splits it across rows, and the
/// buffer holding it is secret. The shim's capture is also the *right* link — the localhost
/// variant the runtime was about to open, not the manual one it prints on screen.
async fn run_mint_on_pty(
    app: &tauri::AppHandle,
    rt: Runtime,
    account_id: &str,
    timeout_secs: Option<u64>,
    open_with: Option<String>,
) -> Result<accounts::vault::Secret, String> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let profile = rt.profile();
    let account_id = account_id.to_string();
    let root = accounts::account_root(rt, &account_id)
        .ok_or_else(|| "no data directory available".to_string())?;
    let config_env = profile.config_env.to_string();
    let scrub: Vec<String> = profile.shadowing_env.iter().map(|s| s.to_string()).collect();
    let cli = accounts::resolve_cli(profile)
        .ok_or_else(|| format!("could not find the `{}` command", profile.cli))?;
    // Longer than a sign-in's: the browser round trip is the same, but this one can also fall
    // back to the user finding a code and bringing it back.
    let deadline = std::time::Duration::from_secs(timeout_secs.unwrap_or(600).clamp(60, 1800));

    let app = app.clone();
    let id = account_id.clone();
    tauri::async_runtime::spawn_blocking(move || -> Result<accounts::vault::Secret, String> {
        let pair = native_pty_system()
            .openpty(PtySize { rows: 50, cols: MINT_PTY_COLS, pixel_width: 0, pixel_height: 0 })
            .map_err(|e| format!("could not open a terminal for the mint: {e}"))?;

        let mut cmd = CommandBuilder::new(&cli);
        cmd.arg("setup-token");
        cmd.env(&config_env, root.to_string_lossy().to_string());
        for var in &scrub {
            cmd.env_remove(var);
        }
        // The TUI renders with colour and OSC 8 links either way; saying so plainly beats
        // letting it guess from an unset TERM.
        cmd.env("TERM", "xterm-256color");

        // Same shim as the sign-in, and for the same reason: it is how maiTerm learns the real
        // URL and gets to open it where the user asked. Held for the child's whole life.
        let suppressor = accounts::browser::suppress_default_browser();
        if let Some(s) = &suppressor {
            let mut entries = vec![s.path_entry().to_path_buf()];
            entries.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            match std::env::join_paths(entries) {
                Ok(p) => cmd.env("PATH", p),
                Err(e) => log::warn!("accounts: could not shadow the browser opener: {e}"),
            }
        }

        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|e| format!("starting the token mint: {e}"))?;
        // Dropped so the master sees EOF when the child exits; holding it open makes the read
        // below block forever on a process that has already gone.
        drop(pair.slave);

        let reader = pair
            .master
            .try_clone_reader()
            .map_err(|e| format!("could not read the mint's terminal: {e}"))?;
        let writer = pair
            .master
            .take_writer()
            .map_err(|e| format!("could not write to the mint's terminal: {e}"))?;

        // The slot was claimed by `mint_account_token` before any of this, so Cancel reaches a
        // mint during the reconcile and the spawn. Filling the writer in is the last step.
        {
            let mut guard = ACTIVE_MINTS.lock();
            match guard.get_mut(&id) {
                Some(handle) => {
                    handle.writer =
                        Some(std::sync::Arc::new(parking_lot::Mutex::new(writer)));
                }
                // Cancelled and cleaned up while we were starting. Nothing to run for.
                None => {
                    drop(guard);
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("Token mint cancelled".to_string());
                }
            }
        }
        // Checked here as well as in the loop: a cancel during the spawn has already happened,
        // and without this the browser window opens *after* the user backed out.
        if mint_cancelled(&id) {
            let _ = child.kill();
            let _ = child.wait();
            return Err("Token mint cancelled".to_string());
        }

        let buffer = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
        {
            let sink = buffer.clone();
            let mut reader = reader;
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().push_str(&String::from_utf8_lossy(&buf[..n]));
                }
            });
        }

        let start = std::time::Instant::now();
        let mut announced = false;
        let mut said_no_shim = false;
        let outcome = loop {
            if mint_cancelled(&id) {
                break Err("Token mint cancelled".to_string());
            }

            // Announce the link the moment the shim has it.
            //
            // **Only a real URL latches.** The first version latched on the grace period too and
            // announced an empty one, which loses the link for good: the shim still swallows the
            // `open` a moment later, so no window ever appears, and the captured URL can never be
            // emitted because `announced` is already set. The user is left with no link, no
            // window, and a message telling them to approve in a window that does not exist.
            // The grace-period note is worth saying once; it must not close the door behind it.
            if !announced {
                let captured = suppressor
                    .as_ref()
                    .and_then(|s| s.captured().as_deref().and_then(|c| extract_login_url(c, false)));
                if let Some(url) = captured {
                    announced = true;
                    let open_error = match open_with.as_deref() {
                        Some("default") => accounts::browser::open_default(&url).err(),
                        Some(id) => accounts::browser::open_private_window(id, &url).err(),
                        // Copy-to-clipboard: deliberately opens nothing.
                        None => None,
                    };
                    if let Some(e) = &open_error {
                        log::warn!("accounts: opening the mint link: {e}");
                    }
                    let _ = app.emit(
                        LOGIN_URL_EVENT,
                        LoginUrlEvent {
                            account_id: id.clone(),
                            url,
                            opened: open_with.is_some() && open_error.is_none(),
                            open_error,
                            // This IS the link the runtime would have opened — the shim captured
                            // the `redirect_uri=http://localhost:<port>/callback` variant, which
                            // completes through the CLI's own listener. Not a dead end.
                            paste_code: false,
                            needs_code: true,
                        },
                    );
                } else if !said_no_shim && start.elapsed() > SHIM_GRACE {
                    // The runtime opened its own window (or `$BROWSER`/`settings.json` sent it
                    // somewhere the shim never sees). Say so, keep looking.
                    said_no_shim = true;
                    let _ = app.emit(
                        LOGIN_URL_EVENT,
                        LoginUrlEvent {
                            account_id: id.clone(),
                            url: String::new(),
                            opened: false,
                            open_error: Some(
                                "maiTerm could not take over the browser launch, so the agent \
                                 opened its own window — approve there."
                                    .to_string(),
                            ),
                            paste_code: false,
                            needs_code: true,
                        },
                    );
                }
            }

            // Finished? Two ways, and the token is the one that matters: the TUI can sit on a
            // final frame after printing it.
            if let Some(token) = extract_setup_token(&buffer.lock(), true) {
                break Ok(token);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    // **Copied out ONCE, deliberately.** A temporary in a `match` scrutinee lives
                    // until the end of the whole `match`, so `match extract(&buffer.lock()) { …
                    // None => … buffer.lock() … }` holds the guard across its own arms — and
                    // `parking_lot::Mutex` is not reentrant, so the second lock parks the thread
                    // forever. That deadlock was on the FAILURE path (a policy that forbids
                    // long-lived tokens, an account on hold, a killed child), which is the one a
                    // user meets first: the dialog would sit on "Minting…" past every deadline,
                    // with the child, the PTY and the reader thread all leaked.
                    let seen = buffer.lock().clone();
                    // Nothing more is coming, so an unterminated tail is the whole of it.
                    break match extract_setup_token(&seen, false) {
                        Some(token) => Ok(token),
                        None => Err(match transcript_tail(&seen) {
                            Some(tail) => {
                                format!("Token mint failed: {}", redact_secrets(&tail))
                            }
                            None => format!(
                                "Token mint failed (exited {})",
                                status.exit_code()
                            ),
                        }),
                    };
                }
                Ok(None) => {}
                Err(e) => break Err(format!("waiting on the token mint: {e}")),
            }

            if start.elapsed() > deadline {
                break Err(
                    "The mint timed out. Approving the link in the browser normally finishes it; \
                     if the browser showed you a code instead, paste it in before the next try."
                        .to_string(),
                );
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        };

        // Always, on every path: the TUI does not exit on its own once it has printed the token,
        // and a leaked one holds a PTY and an open connection for the life of the app. The first
        // version of this feature left two of them running for days.
        let _ = child.kill();
        let _ = child.wait();
        outcome
    })
    .await
    .map_err(|e| format!("token mint task failed: {e}"))?
}

/// Prove a freshly minted token actually works, in a config root that holds nothing else.
///
/// **This cannot check WHO the token is, and the first version wrongly assumed it could.**
/// `auth status --json` answers a `setup-token` with `authMethod: "oauth_token"` and *nothing
/// else* — no `email`, no `orgId`, no `subscriptionType`. Measured 2026-09-21: the same five
/// fields come back for a token made of the word DEFINITELYNOTAREALTOKEN. That is consistent
/// with §8 — the token's scope is `user:inference`, so there is no profile to report — but it
/// meant the mint's own guard (`email.is_none()` ⇒ discard) could never pass, and a perfectly
/// good one-year credential was minted and thrown away. A textbook `absence-read-as-a-claim`,
/// in the code written to prevent exactly that.
///
/// `loggedIn: true` is worth even less here than §6.1 already says: it is true for a garbage
/// token, because it reports only that the variable is set.
///
/// So the check that is possible is **validity, not identity** — spend one round trip and see
/// whether the token authenticates. A bad one fails in about a second with
/// `API Error: 401 OAuth access token is invalid`. That is the failure worth catching: a
/// truncated or corrupt token passes every shape test there is, and on a remote host it does not
/// error, it falls through to whatever login that box already had (§6.1).
///
/// The temp root is still the point: with an empty config dir the only rung that can answer is
/// the token we inject, so nothing the account root happens to hold can answer in its place.
///
/// The returned `AccountIdentity` therefore carries no email by design, and `logged_in` means
/// **"we proved this token authenticates"**, not "the runtime said loggedIn".
fn verify_token_identity(profile: &accounts::RuntimeProfile, token: &accounts::vault::Secret) -> Result<AccountIdentity, String> {
    let cli = accounts::resolve_cli(profile)
        .ok_or_else(|| format!("could not find the `{}` command", profile.cli))?;
    let probe_root = std::env::temp_dir().join(format!("maiterm-tokcheck-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&probe_root).map_err(|e| format!("preparing the check: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&probe_root, std::fs::Permissions::from_mode(0o700));
    }

    // Shared by both probes below. Scrubs every shadowing rung EXCEPT the one being tested —
    // otherwise an inherited ANTHROPIC_API_KEY outranks the token (§2.2 rung 3 beats rung 5) and
    // the answer describes the key rather than the token, which is the precise confusion §6.1
    // exists to prevent.
    let probe = |args: &[&str]| {
        let mut cmd = Command::new(&cli);
        cmd.args(args);
        cmd.env(profile.config_env, &probe_root);
        for var in profile.shadowing_env.iter().filter(|v| **v != TOKEN_ENV) {
            cmd.env_remove(var);
        }
        cmd.env(TOKEN_ENV, token.expose());
        cmd.stdin(std::process::Stdio::null());
        cmd.output()
    };

    let status = probe(&["auth", "status", "--json"]).map_err(|e| format!("checking the token: {e}"));
    // One real request. This is the only thing that separates a live token from a truncated one,
    // and a truncated one is the §6.1 nightmare: it passes every shape check, and on a remote
    // host it does not error, it silently resolves as whoever else is logged in there.
    // `--max-turns 1` on a two-word prompt keeps the cost to about nothing.
    let live = probe(&["-p", "ok", "--max-turns", "1"]);

    // Before anything can return: the probe root is a directory the runtime just wrote into.
    let _ = std::fs::remove_dir_all(&probe_root);

    let live = live.map_err(|e| format!("checking the token: {e}"))?;
    if !live.status.success() {
        // `Failed to authenticate. API Error: 401 OAuth access token is invalid.` is what a bad
        // one says, and it says it in about a second. Redacted, because the runtime is perfectly
        // capable of echoing what it was given back at us.
        let why = String::from_utf8_lossy(&live.stderr);
        let why = if why.trim().is_empty() {
            String::from_utf8_lossy(&live.stdout).trim().to_string()
        } else {
            why.trim().to_string()
        };
        return Err(format!(
            "the minted token did not work: {}",
            redact_secrets(&why.chars().take(200).collect::<String>())
        ));
    }

    let v: serde_json::Value = serde_json::from_slice(&status?.stdout)
        .map_err(|_| "the runtime gave no usable answer when checking the token".to_string())?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    let auth_method = s("authMethod");
    Ok(AccountIdentity {
        // A token answers as `oauth_token`, not `claude.ai` — so `is_account_login` is about
        // whether THIS root's own interactive login answered, and is false here by construction.
        is_account_login: false,
        shadowed_by: auth_method.clone(),
        // **Proved, not reported.** The runtime's own `loggedIn` is true for a token made of
        // nonsense — it only says the variable is set — so it is deliberately not read here.
        logged_in: true,
        auth_method,
        api_key_source: s("apiKeySource"),
        api_provider: s("apiProvider"),
        // Absent for every `oauth_token`, valid or not: the scope is `user:inference` (§8), so
        // there is no profile to report. Do not reintroduce a check on these — that is what
        // discarded a good token.
        email: None,
        org_id: None,
        org_name: None,
        plan: None,
    })
}

/// The variable a minted token is consumed through, here and on every remote (§6 step 2).
const TOKEN_ENV: &str = "CLAUDE_CODE_OAUTH_TOKEN";

/// Said when a mint is cancelled after the browser has already finished it.
///
/// The cancel is honoured — nothing is stored — but a credential exists in the world and maiTerm
/// cannot take it back (§9.4: no revoke, and `auth logout` is not known to reach one). Saying so
/// is the whole obligation: silence here would leave a standing one-year token nobody knows about.
const TOKEN_MINTED_BUT_DISCARDED: &str =
    "Cancelled. The browser had already finished, so a token was created — it has NOT been kept, \
     but it is live for a year and nothing local can revoke it. Revoke it in your Anthropic \
     account settings if you do not want it standing.";

/// Mint a `setup-token` for an existing account and put it in the vault (§6 step 1).
///
/// Returns metadata only. **The token itself never crosses the IPC boundary** — §9.3 is explicit
/// that the surface is status-only, and a token that reached the frontend would be one webview
/// bug away from being a credential in a log.
///
/// Three things happen in an order that matters:
///
/// 1. **The vault is checked first.** Minting is a browser round trip and a standing one-year
///    credential; discovering afterwards that there is nowhere to put it would mean a live token
///    printed to a pipe and then dropped, unusable and unrevokable.
/// 2. **The token is verified before it is stored** (§6.1), against a throwaway config root so
///    only the token can answer. A mint that landed on the wrong account — the §5.1 browser-
///    session trap applies to minting exactly as it does to signing in — is discarded here
///    rather than shipped to a remote host where it would silently resolve as someone else.
/// 3. **Only then is it stored.** A stored token the caller then fails to record is a leak the
///    user cannot see; this order means the worst case is a vault entry with no metadata, which
///    `remote_hosts` being empty already renders inert.
#[tauri::command]
pub async fn mint_account_token(
    app: tauri::AppHandle,
    runtime: String,
    account_id: String,
    timeout_secs: Option<u64>,
    open_with: Option<String>,
) -> Result<TokenMint, String> {
    let rt = runtime_from_slug(&runtime)?;
    let profile = rt.profile();
    if !profile.supported {
        return Err(format!("{} accounts are not supported yet", profile.label));
    }
    if rt != Runtime::Claude {
        return Err(format!("{} token minting is not implemented yet", profile.label));
    }
    if uuid::Uuid::parse_str(&account_id).is_err() {
        return Err("account id must be a UUID".to_string());
    }
    // Step 1: nowhere to put it means do not mint it.
    if !accounts::vault::available() {
        return Err(
            "no OS keychain is available, so there is nowhere to keep a token — remote logins \
             need one, and maiTerm will not write a credential to a plain file."
                .to_string(),
        );
    }
    // Claimed BEFORE the reconcile below, which walks the whole config root and can take
    // seconds. Cancel is keyed by account id and can only reach a mint that is registered, so
    // without this an Escape in that window is a silent no-op: the dialog closes, a browser
    // window the user has already backed out of opens behind it, and — because the flow finishes
    // through the runtime's own localhost callback — approving there stores a real token for an
    // account whose webview is gone. Dropped on every exit path, `?` returns included.
    let _registration = register_mint(&account_id)?;

    // The root must already exist and hold this account's login; unlike a sign-in, a mint never
    // creates one. Reconcile anyway — the user may have installed skills since.
    let home = home_dir()?;
    let id_for_reconcile = account_id.clone();
    tauri::async_runtime::spawn_blocking(move || accounts::reconcile(rt, &id_for_reconcile, &home))
        .await
        .map_err(|e| format!("reconcile task failed: {e}"))??;

    let token = run_mint_on_pty(&app, rt, &account_id, timeout_secs, open_with).await?;

    // **Cancelled between the browser and here?** This is the window that matters most and the
    // one that was open longest: the mint is done, a real credential exists, and the checks below
    // take seconds — a live API round trip among them. The usual reason to cancel is watching the
    // browser authorise the WRONG account, which is noticed exactly here.
    //
    // Cancelling cannot un-mint it. `setup-token` has no revoke and `auth logout` is not known to
    // reach one (§9.4), so the honest thing is to not keep it and to say that a credential now
    // exists which only the user can retire. Storing it instead would record account B's token
    // against account A, and per §6.1 nothing downstream could ever tell — a token carries no
    // identity to compare.
    if mint_cancelled(&account_id) {
        return Err(TOKEN_MINTED_BUT_DISCARDED.to_string());
    }

    // Step 2: does this token actually work?
    //
    // **Not "who is it" — that is not answerable.** `auth status --json` gives a token no email,
    // no org and no plan whatever it is worth, so the previous version's `email.is_none()` guard
    // could never pass: it minted real one-year credentials and threw every one of them away.
    // `verify_token_identity` now returns an error with the runtime's own reason when the token
    // does not authenticate, so there is nothing left to re-test here.
    let identity = {
        let token = token.clone();
        tauri::async_runtime::spawn_blocking(move || verify_token_identity(rt.profile(), &token))
            .await
            .map_err(|e| format!("token check task failed: {e}"))??
    };

    // Asked again, because the check above is seconds old by now and those seconds are a live
    // network request. The last moment a cancel can still mean anything is immediately before
    // the write.
    if mint_cancelled(&account_id) {
        return Err(TOKEN_MINTED_BUT_DISCARDED.to_string());
    }

    // Step 3.
    let id_for_store = account_id.clone();
    let stored = tauri::async_runtime::spawn_blocking(move || {
        accounts::vault::store(&id_for_store, &token)
    })
    .await
    .map_err(|e| format!("vault task failed: {e}"))?;
    stored.map_err(|e| format!("could not store the token: {e}"))?;

    Ok(TokenMint {
        minted_at: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        identity,
    })
}

/// Does the vault actually hold a token for this account?
///
/// **The pane asks this instead of trusting `token_minted_at`**, and that is not belt-and-braces.
/// The token is stored by Rust before the frontend records the metadata, and every way that
/// second step can fail — the two-instance save guard refusing, the window being closed
/// mid-mint, a mint whose reply lands in a destroyed webview — leaves a live one-year credential
/// with no metadata pointing at it. A UI keyed on the metadata then shows "not set up" and hides
/// the only control that could remove it. Keyed on this, the token is visible because it exists.
///
/// Errors are reported as "no token": a keychain that cannot be read is a reason to show the
/// recovery affordance, not to hide it.
#[tauri::command]
pub async fn has_account_token(account_id: String) -> Result<bool, String> {
    Ok(
        tauri::async_runtime::spawn_blocking(move || accounts::vault::read(&account_id))
            .await
            .map_err(|e| format!("vault task failed: {e}"))?
            .map(|t| t.is_some())
            .unwrap_or(false),
    )
}

/// Forget an account's remote token.
///
/// **This does not revoke it** (§9.4 — we build as if `auth logout` does not reach these). It
/// stops maiTerm handing the token out; a host that already has it keeps working until the token
/// expires. Callers must say so rather than implying the credential is dead.
#[tauri::command]
pub async fn forget_account_token(account_id: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || accounts::vault::delete(&account_id))
        .await
        .map_err(|e| format!("vault task failed: {e}"))?
        .map_err(|e| format!("could not remove the token: {e}"))
}

/// What a caller should do about this tab's remote identity. **No field ever carries the
/// token** — `export` names a file, and Rust is the only thing that ever holds the bytes.
#[derive(Debug, Clone, Serialize)]
pub struct RemoteTokenPrep {
    /// `not_applicable` | `ready` | `missing_credential` | `failed`
    pub status: &'static str,
    /// The shell fragment to splice into the remote command, when `status == "ready"`.
    pub export: Option<String>,
    /// Who this is about, for a message the user can act on.
    pub account_label: Option<String>,
    pub host: Option<String>,
    /// Why nothing is being injected, for the states that need saying out loud.
    pub detail: Option<String>,
}

impl RemoteTokenPrep {
    /// This host is not covered, the feature is off, or there is no active account with a
    /// token. The overwhelmingly common answer, and a silent one: a host that was never opted
    /// in is not a problem to report.
    fn not_applicable() -> Self {
        Self {
            status: "not_applicable",
            export: None,
            account_label: None,
            host: None,
            detail: None,
        }
    }
}

/// Place the active account's remote token on an ssh host, for one tab.
///
/// Answers the question a tab asks as it opens an ssh session: *should this session run as one
/// of my managed accounts, and if so, how does it pick the credential up?* The two halves are
/// deliberately separate — `accounts::remote_account_for_host` is policy, `accounts::remote` is
/// mechanism — and both are in Rust because the answer to the second is a credential.
///
/// **The token never crosses this boundary.** It is read from the vault here, written to the
/// push connection's stdin here, and what the caller gets back is a shell fragment naming a
/// path. §9.3 exists because agents can reach the frontend; this keeps there being nothing
/// there to reach.
///
/// A host that is not covered gets `not_applicable` **without opening any connection** — this
/// runs on the tab-spawn path, so the cost of the feature being off, or of this host not being
/// one of the enabled ones, has to be zero.
///
/// Every other non-ready answer is reported rather than swallowed, and that is §6.1: injecting
/// nothing is not an error condition on the remote, it is the host's own login answering
/// instead, with `loggedIn: true` and the work billed to whoever last signed in there. Silence
/// would be indistinguishable from success.
#[tauri::command]
pub async fn prepare_remote_account_token(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
    tab_id: String,
    ssh_args: String,
) -> Result<RemoteTokenPrep, String> {
    // A tab id is a UUID everywhere it is minted. Anything else is not ours to name a file
    // after, let alone interpolate into a remote shell command.
    if !accounts::remote::is_safe_handle(&tab_id) {
        return Ok(RemoteTokenPrep::not_applicable());
    }

    // The stored ssh value is free-form and routinely carries flags (`-x -C ews@nova`,
    // `ews@nova -p 2222`). Matching the raw string against the user's host list would fail on
    // every one of those, and per §6.1 the failure is invisible — the tab simply comes up as
    // the host's own login. `port_book_key` is the codebase's most careful ssh-target
    // extractor (it understands `-p2222` and `-oKey=Val` inline), and it is already the thing
    // that makes the bridge and a tab's replay agree on what host they are talking about.
    let target = crate::commands::ssh_tunnel::port_book_key(ssh_args.trim_start_matches("ssh "));

    let (account_id, label, plan) = {
        let prefs = &state.app_data.read().preferences;
        match accounts::remote_account_for_host(prefs, &target) {
            // The plan goes with the token: without it the remote cannot tell what the account is
            // entitled to and silently resolves a different model. See `stage_contents`.
            Some(a) => (a.id.clone(), a.label.clone(), a.plan.clone()),
            None => return Ok(RemoteTokenPrep::not_applicable()),
        }
    };

    // Metadata said yes; the vault is the one that actually has to. The two diverge in states
    // the UI renders — a `forgetToken` interrupted between its two awaits, an item removed in
    // Keychain Access — and `remote_account_for_host` keys on metadata, so it can answer
    // "propagate" for an account whose credential is gone.
    let vault_id = account_id.clone();
    let token = match tauri::async_runtime::spawn_blocking(move || accounts::vault::read(&vault_id))
        .await
        .map_err(|e| format!("vault task failed: {e}"))?
    {
        Ok(Some(secret)) => secret,
        Ok(None) => {
            log::warn!(
                "accounts: {target} is enabled for account {account_id} but its token is not in \
                 the vault — injecting nothing, so the host keeps its own login"
            );
            return Ok(RemoteTokenPrep {
                status: "missing_credential",
                export: None,
                account_label: Some(label),
                host: Some(target),
                detail: Some(
                    "its remote token is no longer in the keychain — mint it again in \
                     Preferences → Accounts"
                        .into(),
                ),
            });
        }
        Err(e) => {
            log::warn!("accounts: could not read the token for {account_id}: {e}");
            return Ok(RemoteTokenPrep {
                status: "failed",
                export: None,
                account_label: Some(label),
                host: Some(target),
                detail: Some(format!("the keychain could not be read ({e})")),
            });
        }
    };

    // A last shape check before a credential leaves this machine. Nothing should be able to put
    // something else in the vault, but this is the one place where being wrong writes a file to
    // a computer we do not own.
    if !token.looks_like_setup_token() {
        log::warn!("accounts: the vault entry for {account_id} is not a setup-token — not sending it");
        return Ok(RemoteTokenPrep {
            status: "missing_credential",
            export: None,
            account_label: Some(label),
            host: Some(target),
            detail: Some("its stored remote token is not in the expected form — mint it again".into()),
        });
    }

    let script = accounts::remote::stage_script(&tab_id)
        .ok_or_else(|| "unusable tab id".to_string())?;
    let contents = accounts::remote::stage_contents(token.expose(), plan.as_deref());
    if let Err(e) = run_ssh(&ssh_args, &script, Some(&contents)).await {
        log::warn!("accounts: could not place the token on {target}: {e}");
        return Ok(RemoteTokenPrep {
            status: "failed",
            export: None,
            account_label: Some(label),
            host: Some(target),
            detail: Some(e),
        });
    }

    Ok(RemoteTokenPrep {
        status: "ready",
        export: accounts::remote::export_fragment(&tab_id),
        account_label: Some(label),
        host: Some(target),
        detail: None,
    })
}

/// Take back a handoff file that is not going to be used.
///
/// The push runs in parallel with the decision about whether to inject — that is what keeps it
/// off the critical path — so by the time a caller decides not to write into a shell, the
/// credential is already on that host. Without this, the only thing that would ever remove it is
/// the sweep inside the next push to the same host, which for a host nobody opens again is
/// never. A one-year credential that nothing local can revoke (§9.4) must not be left behind
/// because a tunnel failed to come up.
///
/// Best effort and quiet: this runs on paths that have already gone wrong, and the file will be
/// swept eventually. Only the path is on the command line.
#[tauri::command]
pub async fn discard_remote_account_token(tab_id: String, ssh_args: String) -> Result<(), String> {
    let Some(script) = accounts::remote::discard_script(&tab_id) else {
        return Ok(());
    };
    match run_ssh(&ssh_args, &script, None).await {
        Ok(()) => {
            log::info!("accounts: removed the unused handoff file for tab {tab_id}");
            Ok(())
        }
        Err(e) => {
            log::warn!("accounts: could not remove the unused handoff file for tab {tab_id}: {e}");
            Ok(())
        }
    }
}

/// Run one short script on the remote over a connection of its own, optionally feeding it
/// something on **stdin**.
///
/// Not `ssh_run_setup`, which passes its script as an argv entry: that is fine for the config
/// it writes and fatal for a credential, which would then be visible in `ps` on this machine
/// and in the remote's process list. Here argv carries only the script — `mkdir`, `chmod`, a
/// sweep and `cat >` — and the bytes arrive out of band.
///
/// Fully independent of the user's ControlMaster socket, for the reason `start_ssh_tunnel`
/// documents: a maiTerm connection owning that socket once broke the user's own `ssh <host>`.
///
/// Timeouts are deliberately tighter than `ssh_run_setup`'s 30s: this one sits in front of a tab
/// opening, and a host that is slow to authenticate must not be able to hold a spawn for half a
/// minute.
async fn run_ssh(ssh_args: &str, script: &str, stdin_data: Option<&str>) -> Result<(), String> {
    use tokio::io::AsyncWriteExt;

    let mut args: Vec<String> = vec![
        "-o".into(),
        "ControlMaster=no".into(),
        "-o".into(),
        "ControlPath=none".into(),
        // No interactive prompt is possible here, and hanging on one would stall a tab opening.
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=8".into(),
        // No tty: nothing about this is interactive, and a tty would echo stdin back at us.
        "-T".into(),
    ];
    args.extend(
        ssh_args
            .trim_start_matches("ssh ")
            .split_whitespace()
            .map(str::to_string),
    );
    args.push(script.to_string());

    let mut child = tokio::process::Command::new("ssh")
        .args(&args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("could not run ssh: {e}"))?;

    // Closed either way. A script reading stdin (`cat >`) would otherwise wait on a pipe nobody
    // is going to write to, and the timeout below would be the only thing that ended it.
    {
        let mut stdin = child
            .stdin
            .take()
            .ok_or_else(|| "ssh gave us no stdin".to_string())?;
        if let Some(data) = stdin_data {
            stdin
                .write_all(data.as_bytes())
                .await
                .map_err(|e| format!("could not hand the token to ssh: {e}"))?;
        }
        stdin
            .shutdown()
            .await
            .map_err(|e| format!("could not close the handoff: {e}"))?;
    }

    let output = tokio::time::timeout(
        tokio::time::Duration::from_secs(15),
        child.wait_with_output(),
    )
    .await
    .map_err(|_| "the host did not answer within 15s".to_string())?
    .map_err(|e| format!("ssh failed: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stderr = stderr.trim();
        return Err(if stderr.is_empty() {
            format!("ssh exited {}", output.status.code().unwrap_or(-1))
        } else {
            // Bounded: this reaches a toast, and ssh is capable of paragraphs.
            stderr.chars().take(200).collect()
        });
    }
    Ok(())
}

/// Sign an account out and delete its config root. Used for a §5.1 duplicate, a cancelled
/// sign-in, "Remove" and "Clear setup".
///
/// **Signs out first, and that is not optional.** Deleting the directory does not touch the
/// credential: on macOS it lives in a Keychain item keyed by a hash of the config-dir path, so
/// removing the root orphans a credential that stays valid server-side, under a uuid that is
/// never reused. Every caller of this promises the user the account is gone — "Clear setup"
/// says so in its confirm text — so revocation belongs here rather than in each caller.
///
/// Sign-out is best effort: an account that was never signed in, or whose CLI has gone missing,
/// must still be removable. A failure is logged, not returned, because leaving the root behind
/// as well would be strictly worse.
///
/// The directory delete unlinks symlinks without following them — the root points at the user's
/// real transcripts and project state, which are not ours to delete.
#[tauri::command]
pub async fn discard_account_root(runtime: String, account_id: String) -> Result<(), String> {
    let rt = runtime_from_slug(&runtime)?;
    let profile = rt.profile();

    // Drop any remote token with the account, in the same shared path as the sign-out and for
    // the same reason: every caller of this promises the user the account is gone, and a vault
    // entry outliving its account is a credential nothing in the UI can reach to remove.
    //
    // **It does not revoke the token** (§9.4). A host that already holds it keeps working until
    // it expires — which is why the per-host list, not this call, is the revoke story, and why
    // the removal copy has to say so.
    //
    // Best effort, like the sign-out below: a missing keychain must not make an account
    // unremovable.
    if let Err(e) = accounts::vault::delete(&account_id) {
        log::warn!("accounts: could not remove the vault entry for {account_id}: {e}");
    }

    if let (Some(cli), Some(root)) = (
        accounts::resolve_cli(profile),
        accounts::account_root(rt, &account_id),
    ) {
        if root.exists() {
            // Before running anything against this root: a root built by an earlier build has
            // `.claude.json` as a symlink, and the runtime writes THROUGH it — observed live,
            // sign-out stripped the identity block out of the user's own ~/.claude.json.
            if let Err(e) = accounts::detach_write_through_links(rt, &account_id) {
                log::warn!("accounts: detaching write-through links for {account_id}: {e}");
            }
            let config_env = profile.config_env.to_string();
            let scrub: Vec<String> =
                profile.shadowing_env.iter().map(|s| s.to_string()).collect();
            let result = tauri::async_runtime::spawn_blocking(move || {
                let mut cmd = Command::new(&cli);
                cmd.args(["auth", "logout"]);
                cmd.env(&config_env, &root);
                // Same scrub as everywhere else: sign out THIS root's credential, not whatever
                // a leftover variable would have answered with.
                for var in &scrub {
                    cmd.env_remove(var);
                }
                cmd.stdin(std::process::Stdio::null());
                cmd.output()
            })
            .await;
            match result {
                Ok(Ok(out)) if !out.status.success() => log::warn!(
                    "accounts: sign-out for {account_id} exited {}: {}",
                    out.status.code().unwrap_or(-1),
                    String::from_utf8_lossy(&out.stderr).trim()
                ),
                Err(e) => log::warn!("accounts: sign-out task for {account_id} failed: {e}"),
                Ok(Err(e)) => log::warn!("accounts: sign-out for {account_id} failed: {e}"),
                Ok(Ok(_)) => {}
            }
        }
    }

    tauri::async_runtime::spawn_blocking(move || accounts::remove_root(rt, &account_id))
        .await
        .map_err(|e| format!("discard task failed: {e}"))?
}

/// Browsers on this machine that can be told to open a private window, in preference order.
///
/// An empty list is a normal answer, not a failure — Safari and several others have no such
/// switch. The caller keeps the copy-the-link path either way.
#[tauri::command]
pub async fn list_private_browsers() -> Result<Vec<accounts::browser::PrivateBrowserInfo>, String> {
    // Touches the filesystem and reads PATH: off the async executor, per the mesh-pinwheel rule.
    tauri::async_runtime::spawn_blocking(accounts::browser::available)
        .await
        .map_err(|e| format!("browser scan failed: {e}"))
}

/// Open a sign-in link in a private window, so it can be completed as a *different* account
/// without signing the current one out of the browser (§5.1).
#[tauri::command]
pub async fn open_private_window(browser_id: String, url: String) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        accounts::browser::open_private_window(&browser_id, &url)
    })
    .await
    .map_err(|e| format!("browser launch failed: {e}"))?
}

/// One workspace a user could reload after switching accounts.
#[derive(Debug, Clone, Serialize)]
pub struct ReloadWorkspace {
    pub id: String,
    pub name: String,
    /// Tabs with a **live PTY**. Only these are worth reloading: a suspended or never-opened tab
    /// picks the new account up whenever it next starts, so offering to reload it would spawn
    /// shells the user never asked for.
    pub live_tabs: usize,
}

/// One window, for the post-switch reload offer.
#[derive(Debug, Clone, Serialize)]
pub struct ReloadWindow {
    pub window_id: String,
    /// The Tauri label — what `request_account_reload` addresses the event to.
    pub label: String,
    pub name: Option<String>,
    pub workspaces: Vec<ReloadWorkspace>,
}

/// Emitted to ONE window, asking it to reload the tabs running under the old account.
pub const RELOAD_TABS_EVENT: &str = "accounts-reload-tabs";

/// Emitted back to whoever asked, once that window has finished.
pub const RELOAD_DONE_EVENT: &str = "accounts-reload-done";

#[derive(Debug, Clone, Serialize)]
struct ReloadTabsEvent {
    /// `None` means every workspace in the window.
    workspace_ids: Option<Vec<String>>,
    /// Window label to report completion to.
    reply_to: String,
    /// Echoed back, so a caller with several requests in flight knows which one finished.
    request_id: String,
}

#[derive(Debug, Clone, Serialize)]
struct ReloadDoneEvent {
    request_id: String,
    reloaded: usize,
}

/// Which windows and workspaces have tabs still running under the previous account.
///
/// Switching accounts cannot move a running tab — the config dir is in the environment the shell
/// was exec'd with. This is what lets the UI offer the only real remedy (respawn the shell)
/// without the user hunting for affected tabs by hand.
///
/// Counts only live terminal tabs, and never a stack service tab: reloading one of those would
/// restart the user's dev server, which has nothing to do with which account is active.
#[tauri::command]
pub async fn account_reload_targets(
    state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>,
) -> Result<Vec<ReloadWindow>, String> {
    let tab_pty = state.tab_pty_map.read();
    let ptys = state.pty_registry.read();
    let app_data = state.app_data.read();

    Ok(app_data
        .windows
        .iter()
        .filter(|w| w.label != "preferences" && w.label != "help")
        .map(|w| {
            let workspaces = w
                .workspaces
                .iter()
                .map(|ws| {
                    let live_tabs = ws
                        .panes
                        .iter()
                        .flat_map(|p| p.tabs.iter())
                        .filter(|t| {
                            t.service_id.is_none()
                                && tab_pty
                                    .get(&t.id)
                                    .is_some_and(|pty_id| ptys.contains_key(pty_id))
                        })
                        .count();
                    ReloadWorkspace {
                        id: ws.id.clone(),
                        name: ws.name.clone(),
                        live_tabs,
                    }
                })
                .collect();
            ReloadWindow {
                window_id: w.id.clone(),
                label: w.label.clone(),
                name: w.name.clone(),
                workspaces,
            }
        })
        .collect())
}

/// Ask one window to reload its running tabs, so they come back under the active account.
///
/// Addressed with `emit_to` rather than broadcast: a plain `emit` reaches every window, and each
/// one would reload its own tabs — turning "reload this workspace" into "reload everything".
#[tauri::command]
pub async fn request_account_reload(
    app: tauri::AppHandle,
    label: String,
    workspace_ids: Option<Vec<String>>,
    reply_to: String,
    request_id: String,
) -> Result<(), String> {
    app.emit_to(
        &label,
        RELOAD_TABS_EVENT,
        ReloadTabsEvent { workspace_ids, reply_to, request_id },
    )
    .map_err(|e| format!("asking {label} to reload: {e}"))
}

/// Report that a window has finished reloading, so the dialog that asked can stop claiming it is
/// still in progress. Without this there is no completion signal at all: the request is one-way,
/// and the tab counts look identical afterwards because the replacements are live too.
#[tauri::command]
pub async fn report_account_reload_done(
    app: tauri::AppHandle,
    reply_to: String,
    request_id: String,
    reloaded: usize,
) -> Result<(), String> {
    app.emit_to(&reply_to, RELOAD_DONE_EVENT, ReloadDoneEvent { request_id, reloaded })
        .map_err(|e| format!("reporting reload completion to {reply_to}: {e}"))
}

/// The environment a tab spawning under this account needs: variables to set, and variables to
/// REMOVE. The removals are not optional — see `read_account_identity` for what a leftover
/// override does.
#[derive(Debug, Clone, Serialize)]
pub struct AccountSpawnEnv {
    pub set: Vec<(String, String)>,
    pub unset: Vec<String>,
}

#[tauri::command]
pub async fn account_spawn_env(
    runtime: String,
    account_id: String,
) -> Result<AccountSpawnEnv, String> {
    let rt = runtime_from_slug(&runtime)?;
    let (set, unset) = accounts::spawn_env(rt, &account_id)
        .ok_or_else(|| format!("{} accounts are not supported yet", rt.profile().label))?;
    Ok(AccountSpawnEnv { set, unset })
}
