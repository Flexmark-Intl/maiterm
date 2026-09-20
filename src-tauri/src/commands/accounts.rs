//! Tauri surface for managed agent accounts — see `docs/login.md`.
//!
//! Nothing here returns, accepts or logs credential material. An account is named by id; what
//! comes back is identity *metadata* the Accounts pane displays and the duplicate guard compares.

use serde::Serialize;
use std::path::PathBuf;
use std::process::Command;

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
