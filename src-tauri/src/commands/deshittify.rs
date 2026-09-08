//! Deshittification rules — opt-in tweaks that strip anti-features out of the AI
//! coding agents maiTerm hosts.
//!
//! Every rule owns a piece of state that lives OUTSIDE maiTerm's own preferences
//! (`~/.claude/settings.json`, the user's global git config). So there is no
//! `enabled` flag to keep in sync: `status()` reads what is actually on disk and
//! that IS the toggle position. A rule the user (or an agent) undoes by hand
//! simply reads back as off — no drift, no re-assert loop.

use serde::Serialize;
use std::fs;
use std::path::PathBuf;
use std::process::Command;

/// Claude Code env vars we set to "1" in `~/.claude/settings.json`.
/// (rule id, env var name)
const ENV_RULES: &[(&str, &str)] = &[
    ("cc_disable_telemetry", "DISABLE_TELEMETRY"),
    ("cc_disable_error_reporting", "DISABLE_ERROR_REPORTING"),
    ("cc_disable_bug_command", "DISABLE_BUG_COMMAND"),
    ("cc_disable_feedback_command", "DISABLE_FEEDBACK_COMMAND"),
    ("cc_disable_feedback_survey", "CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY"),
];

const ENV_VALUE: &str = "1";

/// Rule id for `includeCoAuthoredBy: false` in `~/.claude/settings.json`.
const RULE_CO_AUTHORED: &str = "cc_include_co_authored_by";
/// Rule id for the global `commit-msg` hook that strips agent co-authorship trailers.
const RULE_COMMIT_HOOK: &str = "cc_commit_msg_hook";

#[derive(Serialize, Clone, Debug)]
pub struct DeshittifyRuleStatus {
    pub id: String,
    /// True when the rule's change is present on disk right now.
    pub applied: bool,
    /// Why the rule can't be applied (or a caveat about how it is applied).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct DeshittifyStatus {
    pub rules: Vec<DeshittifyRuleStatus>,
}

// ─── ~/.claude/settings.json ───────────────────────────────────────────────

fn claude_user_settings_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".claude").join("settings.json"))
}

fn read_claude_settings() -> serde_json::Value {
    let Some(path) = claude_user_settings_path() else {
        return serde_json::json!({});
    };
    if !path.exists() {
        return serde_json::json!({});
    }
    fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_else(|| serde_json::json!({}))
}

/// Write settings back atomically, preserving everything we didn't touch.
fn write_claude_settings(settings: &serde_json::Value) -> Result<(), String> {
    let path = claude_user_settings_path().ok_or("Could not determine home directory")?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {}", dir.display(), e))?;
    }
    let json = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let tmp = path.with_extension("json.aiterm-tmp");
    fs::write(&tmp, &json).map_err(|e| format!("Cannot write settings tmp: {}", e))?;
    fs::rename(&tmp, &path).map_err(|e| {
        let _ = fs::remove_file(&tmp);
        format!("Cannot update settings: {}", e)
    })
}

/// True when `env[key]` is set to our value. Tolerates the number `1` as well as
/// the string `"1"` — both appear in the wild, Claude Code accepts either.
fn env_var_applied(settings: &serde_json::Value, key: &str) -> bool {
    match settings.get("env").and_then(|e| e.get(key)) {
        Some(serde_json::Value::String(s)) => s == ENV_VALUE,
        Some(serde_json::Value::Number(n)) => n.as_i64() == Some(1),
        Some(serde_json::Value::Bool(b)) => *b,
        _ => false,
    }
}

fn set_env_var(enabled: bool, key: &str) -> Result<(), String> {
    let mut settings = read_claude_settings();
    let obj = settings
        .as_object_mut()
        .ok_or("~/.claude/settings.json is not a JSON object")?;

    if enabled {
        let env = obj.entry("env").or_insert_with(|| serde_json::json!({}));
        let env = env
            .as_object_mut()
            .ok_or("\"env\" in ~/.claude/settings.json is not an object")?;
        env.insert(key.to_string(), serde_json::Value::String(ENV_VALUE.into()));
    } else {
        // Only ever runs on a rule that reads as applied, so removing our key
        // can't discard a value the user deliberately set to something else.
        if let Some(env) = obj.get_mut("env").and_then(|v| v.as_object_mut()) {
            env.remove(key);
            if env.is_empty() {
                obj.remove("env");
            }
        }
    }
    write_claude_settings(&settings)
}

fn co_authored_applied(settings: &serde_json::Value) -> bool {
    settings.get("includeCoAuthoredBy") == Some(&serde_json::Value::Bool(false))
}

fn set_co_authored(enabled: bool) -> Result<(), String> {
    let mut settings = read_claude_settings();
    let obj = settings
        .as_object_mut()
        .ok_or("~/.claude/settings.json is not a JSON object")?;
    if enabled {
        obj.insert("includeCoAuthoredBy".into(), serde_json::Value::Bool(false));
    } else {
        // Absent means Claude Code's default (true) — no need to write it back.
        obj.remove("includeCoAuthoredBy");
    }
    write_claude_settings(&settings)
}

// ─── Global commit-msg hook ────────────────────────────────────────────────

/// maiTerm's managed hooks directory. Deliberately NOT dev/prod-suffixed: this is
/// one global git setting, and a dev build and a prod build pointing at different
/// directories would fight over `core.hooksPath`.
fn managed_hooks_dir() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".maiterm").join("githooks"))
}

/// Resolve the repo's real hooks directory without consulting `core.hooksPath`
/// (which points back at us). `--git-common-dir` keeps linked worktrees working.
const RESOLVE_OWN_HOOK: &str = r#"common="$(git rev-parse --git-common-dir 2>/dev/null)" || exit 0
case "$common" in /*) ;; *) common="$(pwd)/$common" ;; esac
own="$common/hooks/$(basename "$0")""#;

fn commit_msg_script() -> String {
    format!(
        r#"#!/bin/sh
# maiTerm Deshittification — strip agent co-authorship trailers from commit messages.
# Managed by maiTerm (Preferences → Deshittification). Rewritten when the rule is re-applied.
msg="$1"
[ -f "$msg" ] || exit 0

cleaned="$(grep -v -i -E '^Co-authored-by:[[:space:]]*(Claude|Anthropic)' "$msg" \
  | grep -v -i -E '^[[:space:]]*(🤖[[:space:]]*)?Generated with \[?Claude Code')"
printf '%s\n' "$cleaned" > "$msg"

# core.hooksPath makes git skip the repository's own hooks — run it ourselves.
{RESOLVE_OWN_HOOK}
[ -x "$own" ] && exec "$own" "$@"
exit 0
"#
    )
}

fn passthrough_script() -> String {
    format!(
        r#"#!/bin/sh
# maiTerm passthrough. core.hooksPath points git at maiTerm's managed hooks, which
# would otherwise silently disable this repository's own hook of the same name.
# Managed by maiTerm (Preferences → Deshittification).
{RESOLVE_OWN_HOOK}
[ -x "$own" ] && exec "$own" "$@"
exit 0
"#
    )
}

/// Every client-side hook that gets a passthrough shim. `fsmonitor-watchman` and
/// `reference-transaction` are left out on purpose: the first has semantics a shim
/// can't fake, the second fires per ref update and the shell spawn shows up on
/// large fetches.
const PASSTHROUGH_HOOKS: &[&str] = &[
    "applypatch-msg",
    "pre-applypatch",
    "post-applypatch",
    "pre-commit",
    "pre-merge-commit",
    "prepare-commit-msg",
    "post-commit",
    "pre-rebase",
    "post-checkout",
    "post-merge",
    "pre-push",
    "post-rewrite",
    "pre-auto-gc",
    "push-to-checkout",
    "sendemail-validate",
    "post-index-change",
];

fn write_hook_file(path: &PathBuf, body: &str) -> Result<(), String> {
    fs::write(path, body).map_err(|e| format!("Cannot write {}: {}", path.display(), e))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Cannot chmod {}: {}", path.display(), e))?;
    }
    Ok(())
}

/// `git config --global --get core.hooksPath`, or None when unset.
fn global_hooks_path() -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--global", "--get", "core.hooksPath"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let val = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if val.is_empty() {
        None
    } else {
        Some(val)
    }
}

/// Compare a configured hooksPath against ours, tolerating `~` and trailing slashes.
fn hooks_path_is_ours(configured: &str, ours: &PathBuf) -> bool {
    let expanded = if let Some(rest) = configured.strip_prefix("~/") {
        dirs::home_dir()
            .map(|h| h.join(rest))
            .unwrap_or_else(|| PathBuf::from(configured))
    } else {
        PathBuf::from(configured)
    };
    expanded.components().eq(ours.components())
}

fn commit_hook_status() -> DeshittifyRuleStatus {
    let Some(ours) = managed_hooks_dir() else {
        return DeshittifyRuleStatus {
            id: RULE_COMMIT_HOOK.into(),
            applied: false,
            detail: Some("Could not determine home directory.".into()),
        };
    };

    let hook_file = ours.join("commit-msg");
    match global_hooks_path() {
        Some(configured) if hooks_path_is_ours(&configured, &ours) => {
            let present = hook_file.exists();
            DeshittifyRuleStatus {
                id: RULE_COMMIT_HOOK.into(),
                applied: present,
                detail: (!present).then(|| {
                    "core.hooksPath points at maiTerm but the hook file is missing — toggle this on to rewrite it.".to_string()
                }),
            }
        }
        Some(other) => DeshittifyRuleStatus {
            id: RULE_COMMIT_HOOK.into(),
            applied: false,
            detail: Some(format!(
                "Your global core.hooksPath is already set to {other} — maiTerm won't overwrite it. Clear it first, or install the hook into that directory yourself."
            )),
        },
        None => DeshittifyRuleStatus {
            id: RULE_COMMIT_HOOK.into(),
            applied: false,
            detail: None,
        },
    }
}

fn set_commit_hook(enabled: bool) -> Result<(), String> {
    let ours = managed_hooks_dir().ok_or("Could not determine home directory")?;
    let ours_str = ours.to_string_lossy().to_string();

    if enabled {
        if let Some(configured) = global_hooks_path() {
            if !hooks_path_is_ours(&configured, &ours) {
                return Err(format!(
                    "Your global core.hooksPath is already set to {configured}. maiTerm won't overwrite it — clear it first with `git config --global --unset core.hooksPath`."
                ));
            }
        }

        fs::create_dir_all(&ours).map_err(|e| format!("Cannot create {ours_str}: {e}"))?;
        write_hook_file(&ours.join("commit-msg"), &commit_msg_script())?;
        let passthrough = passthrough_script();
        for name in PASSTHROUGH_HOOKS {
            write_hook_file(&ours.join(name), &passthrough)?;
        }

        let out = Command::new("git")
            .args(["config", "--global", "core.hooksPath", &ours_str])
            .output()
            .map_err(|e| format!("Could not run git: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "git config failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            ));
        }
        log::info!("Deshittify: installed global commit-msg hook at {ours_str}");
    } else {
        // Only unset the config if it is still ours — the user may have repointed it.
        if let Some(configured) = global_hooks_path() {
            if hooks_path_is_ours(&configured, &ours) {
                let out = Command::new("git")
                    .args(["config", "--global", "--unset", "core.hooksPath"])
                    .output()
                    .map_err(|e| format!("Could not run git: {e}"))?;
                if !out.status.success() {
                    return Err(format!(
                        "git config failed: {}",
                        String::from_utf8_lossy(&out.stderr).trim()
                    ));
                }
            }
        }
        if ours.exists() {
            fs::remove_dir_all(&ours)
                .map_err(|e| format!("Cannot remove {ours_str}: {e}"))?;
        }
        log::info!("Deshittify: removed global commit-msg hook");
    }
    Ok(())
}

// ─── Status / apply ────────────────────────────────────────────────────────

fn build_status() -> DeshittifyStatus {
    let settings = read_claude_settings();
    let mut rules: Vec<DeshittifyRuleStatus> = ENV_RULES
        .iter()
        .map(|(id, key)| DeshittifyRuleStatus {
            id: (*id).into(),
            applied: env_var_applied(&settings, key),
            detail: None,
        })
        .collect();

    rules.push(DeshittifyRuleStatus {
        id: RULE_CO_AUTHORED.into(),
        applied: co_authored_applied(&settings),
        detail: None,
    });
    rules.push(commit_hook_status());

    DeshittifyStatus { rules }
}

fn apply_rule(id: &str, enabled: bool) -> Result<(), String> {
    if let Some((_, key)) = ENV_RULES.iter().find(|(rid, _)| *rid == id) {
        return set_env_var(enabled, key);
    }
    match id {
        RULE_CO_AUTHORED => set_co_authored(enabled),
        RULE_COMMIT_HOOK => set_commit_hook(enabled),
        other => Err(format!("Unknown deshittification rule: {other}")),
    }
}

#[tauri::command]
pub async fn deshittify_status() -> Result<DeshittifyStatus, String> {
    tauri::async_runtime::spawn_blocking(build_status)
        .await
        .map_err(|e| format!("Status task failed: {e}"))
}

/// Apply or revert one rule, then report the whole set back so the UI always
/// reflects disk rather than what it hoped happened.
#[tauri::command]
pub async fn deshittify_set_rule(id: String, enabled: bool) -> Result<DeshittifyStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let result = apply_rule(&id, enabled);
        result.map(|_| build_status())
    })
    .await
    .map_err(|e| format!("Apply task failed: {e}"))?
}

/// The section-level switch. Applies (or reverts) a whole group of rules,
/// collecting failures instead of stopping at the first — one rule that can't
/// apply shouldn't block the rest — and returns the resulting on-disk status
/// alongside the errors.
#[tauri::command]
pub async fn deshittify_set_rules(
    ids: Vec<String>,
    enabled: bool,
) -> Result<(DeshittifyStatus, Vec<String>), String> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut errors = Vec::new();
        let before = build_status();
        for id in &ids {
            // Skip rules already in the requested state so "turn everything on"
            // can't trip over a rule that is on but would now refuse to re-apply.
            let already = before
                .rules
                .iter()
                .find(|r| &r.id == id)
                .map(|r| r.applied)
                .unwrap_or(false);
            if already == enabled {
                continue;
            }
            if let Err(e) = apply_rule(id, enabled) {
                errors.push(e);
            }
        }
        (build_status(), errors)
    })
    .await
    .map_err(|e| format!("Apply task failed: {e}"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("maiterm-deshittify-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_exec(path: &PathBuf, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The hook strips the trailers it is there for and leaves the real message alone.
    #[test]
    fn commit_msg_hook_strips_agent_credit() {
        let dir = scratch("strip");
        let hook = dir.join("commit-msg");
        write_exec(&hook, &commit_msg_script());

        let msg = dir.join("COMMIT_EDITMSG");
        fs::write(
            &msg,
            "Fix the thing\n\nCo-Authored-By: Ada <ada@example.com>\nCo-Authored-By: Claude <noreply@anthropic.com>\n\n🤖 Generated with [Claude Code](https://claude.com/claude-code)\n",
        )
        .unwrap();

        let status = Command::new("sh")
            .arg(&hook)
            .arg(&msg)
            .current_dir(&dir)
            .status()
            .unwrap();
        assert!(status.success());

        let out = fs::read_to_string(&msg).unwrap();
        assert!(!out.contains("Claude"), "agent credit survived: {out:?}");
        assert!(out.contains("Fix the thing"));
        assert!(out.contains("Co-Authored-By: Ada"), "human co-author was eaten: {out:?}");
        // Trailing blank lines left by the removal are cleaned up.
        assert!(out.ends_with("Ada <ada@example.com>\n"), "unexpected tail: {out:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// core.hooksPath makes git skip `.git/hooks/*`; our shims must run them anyway.
    /// This is the whole reason the managed directory is more than one file.
    #[test]
    fn managed_hooks_chain_to_the_repository_own_hooks() {
        let dir = scratch("chain");
        let repo = dir.join("repo");
        let hooks = dir.join("githooks");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&hooks).unwrap();

        let git = |args: &[&str]| {
            let out = Command::new("git").args(args).current_dir(&repo).output().unwrap();
            assert!(out.status.success(), "git {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        git(&["init", "-q"]);
        git(&["config", "user.email", "t@example.com"]);
        git(&["config", "user.name", "T"]);
        // Repo-local hooks: one commit-msg (chained from ours) and one pre-commit (a shim).
        let local = repo.join(".git").join("hooks");
        fs::create_dir_all(&local).unwrap();
        write_exec(&local.join("commit-msg"), "#!/bin/sh\necho 'local-commit-msg-ran' >> \"$1\"\n");
        write_exec(&local.join("pre-commit"), &format!("#!/bin/sh\ntouch '{}'\n", dir.join("pre-commit-ran").display()));

        // Point this repo at the managed directory, exactly as the global config would.
        write_exec(&hooks.join("commit-msg"), &commit_msg_script());
        write_exec(&hooks.join("pre-commit"), &passthrough_script());
        git(&["config", "core.hooksPath", &hooks.to_string_lossy()]);

        fs::write(repo.join("a.txt"), "hi\n").unwrap();
        git(&["add", "a.txt"]);
        git(&["commit", "-q", "-m", "Add a\n\nCo-Authored-By: Claude <noreply@anthropic.com>"]);

        let body = git(&["log", "-1", "--pretty=%B"]);
        assert!(!body.contains("Claude"), "agent credit survived the hook: {body:?}");
        assert!(body.contains("local-commit-msg-ran"), "repo commit-msg was skipped: {body:?}");
        assert!(dir.join("pre-commit-ran").exists(), "repo pre-commit was skipped");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn hooks_path_comparison_expands_tilde() {
        let ours = dirs::home_dir().unwrap().join(".maiterm").join("githooks");
        assert!(hooks_path_is_ours("~/.maiterm/githooks", &ours));
        assert!(hooks_path_is_ours(&ours.to_string_lossy(), &ours));
        assert!(!hooks_path_is_ours("~/.husky", &ours));
    }
}
