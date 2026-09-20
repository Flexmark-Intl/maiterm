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
/// **`logged_in` is never sufficient on its own** (§6.1). The credential precedence list is a
/// fall-through, so a root with no credential of ours still reports `logged_in: true` via some
/// other rung — an `ANTHROPIC_API_KEY` in the environment, or the host's own login. Callers
/// compare `email`/`org_id` against the account they expected. `auth_method` says which rung
/// answered, which is what makes the mismatch legible.
#[derive(Debug, Clone, Serialize)]
pub struct AccountIdentity {
    pub logged_in: bool,
    pub auth_method: Option<String>,
    pub api_key_source: Option<String>,
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
    let scrub: Vec<String> = profile.scrub_env.iter().map(|s| s.to_string()).collect();

    let out = tauri::async_runtime::spawn_blocking(move || {
        let mut cmd = Command::new("claude");
        cmd.args(["auth", "status", "--json"]);
        cmd.env(&config_env, &root);
        // Scrub what would answer *instead* of this root's credential. Leaving
        // CLAUDE_SECURESTORAGE_CONFIG_DIR set collapses every account onto one login; leaving
        // ANTHROPIC_API_KEY set outranks it from precedence #3. Either makes this read report
        // an identity that has nothing to do with `account_id` — the §6.1 trap, in the one
        // place whose entire job is detecting it.
        for var in &scrub {
            cmd.env_remove(var);
        }
        cmd.env_remove("ANTHROPIC_API_KEY");
        cmd.env_remove("ANTHROPIC_AUTH_TOKEN");
        cmd.output()
    })
    .await
    .map_err(|e| format!("identity task failed: {e}"))?
    .map_err(|e| format!("running claude auth status: {e}"))?;

    if !out.status.success() {
        return Err(format!(
            "claude auth status exited {}: {}",
            out.status.code().unwrap_or(-1),
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }

    let v: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|e| format!("parsing claude auth status: {e}"))?;

    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(|x| x.to_string());
    Ok(AccountIdentity {
        logged_in: v.get("loggedIn").and_then(|x| x.as_bool()).unwrap_or(false),
        auth_method: s("authMethod"),
        api_key_source: s("apiKeySource"),
        email: s("email"),
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
