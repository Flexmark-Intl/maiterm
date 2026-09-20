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

#[derive(Debug, Clone, Serialize)]
struct LoginUrlEvent {
    account_id: String,
    url: String,
    /// Whether maiTerm successfully launched a browser for this URL. False when the caller asked
    /// for none (copy-to-clipboard) **and** when the launch failed — `open_error` distinguishes
    /// them, and the UI must not claim a window opened on either.
    opened: bool,
    open_error: Option<String>,
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
fn timeout_message(transcript: &str) -> String {
    match extract_login_url(transcript, false) {
        Some(u) => format!("Sign-in timed out. If the browser did not open, visit: {u}"),
        None => "Sign-in timed out before the browser flow completed.".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::{extract_login_url, transcript_tail};

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

/// The part of `begin_account_login` that runs once the root exists. Split out so its caller can
/// clean up on any error without every early return having to remember.
async fn login_into_root(
    app: &tauri::AppHandle,
    rt: Runtime,
    account_id: &str,
    timeout_secs: Option<u64>,
    open_with: Option<String>,
) -> Result<AccountIdentity, String> {
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
    let app = app.clone();
    let login = tauri::async_runtime::spawn_blocking(move || -> Result<(), String> {
        let mut cmd = Command::new(&cli);
        cmd.args(["auth", "login"]);
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
        let mut child = cmd.spawn().map_err(|e| format!("starting sign-in: {e}"))?;

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
        let transcript = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
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
                                LoginUrlEvent {
                                    account_id: id.clone(),
                                    url,
                                    opened: false,
                                    open_error: None,
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
                            let open_error = match open_with.as_deref() {
                                Some("default") => accounts::browser::open_default(&url).err(),
                                Some(id) => accounts::browser::open_private_window(id, &url).err(),
                                // Copy-to-clipboard: deliberately opens nothing.
                                None => None,
                            };
                            if let Some(e) = &open_error {
                                log::warn!("accounts: opening the sign-in link: {e}");
                            }
                            let _ = app.emit(
                                LOGIN_URL_EVENT,
                                LoginUrlEvent {
                                    account_id: id_for_task.clone(),
                                    url: url.clone(),
                                    opened: open_with.is_some() && open_error.is_none(),
                                    open_error,
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
                        break Err("sign-in cancelled".to_string());
                    }
                    // Not the logged-out case this time: a failed *login* really is an error —
                    // and the runtime has already explained it on stderr ("Login failed: …"),
                    // which is now in the transcript. Without that the user gets an exit code
                    // and nothing to act on, which is the same as nothing.
                    let code = status.code().unwrap_or(-1);
                    break Err(match transcript_tail(&transcript.lock()) {
                        Some(tail) => format!("Sign-in failed: {tail}"),
                        None => format!("Sign-in failed (exited {code})"),
                    });
                }
                Ok(None) => {
                    if cancelled(&id_for_task) {
                        let mut c = child.lock();
                        let _ = c.kill();
                        let _ = c.wait();
                        break Err("sign-in cancelled".to_string());
                    }
                    if start.elapsed() > deadline {
                        let mut c = child.lock();
                        let _ = c.kill();
                        let _ = c.wait();
                        break Err(timeout_message(&transcript.lock()));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
                Err(e) => break Err(format!("waiting on sign-in: {e}")),
            }
        };
        unregister_login(&id_for_task);
        result
    })
    .await
    .map_err(|e| format!("sign-in task failed: {e}"))?;
    login?;

    read_account_identity(rt.slug().to_string(), account_id).await
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
