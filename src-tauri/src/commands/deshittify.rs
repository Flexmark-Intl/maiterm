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
    /// True when the rule *cannot* currently be applied — something outside
    /// maiTerm owns the state it would write. The UI greys these out and leaves
    /// them out of its "all applied?" arithmetic, so a group holding one can
    /// still reach a fully-on state and be switched back off.
    pub blocked: bool,
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

/// Read `~/.claude/settings.json`, or `{}` when there isn't one yet.
///
/// Errors when the file exists but can't be read or parsed. That case MUST NOT
/// degrade to `{}`: every writer here is read-modify-write, so treating an
/// unparseable file as empty would rename two keys over the top of the user's
/// hooks, permissions and model settings — one click, no error, no backup.
fn read_claude_settings() -> Result<serde_json::Value, String> {
    let path = claude_user_settings_path().ok_or("Could not determine home directory")?;
    read_settings_at(&path)
}

fn read_settings_at(path: &std::path::Path) -> Result<serde_json::Value, String> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let raw = fs::read_to_string(path)
        .map_err(|e| format!("Cannot read ~/.claude/settings.json: {e}"))?;
    if raw.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    serde_json::from_str(&raw).map_err(|e| {
        format!(
            "~/.claude/settings.json could not be parsed ({e}). maiTerm won't overwrite settings it can't read — fix the file (Claude Code is rejecting it too) and come back."
        )
    })
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
    let mut settings = read_claude_settings()?;
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
    let mut settings = read_claude_settings()?;
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

# Matched narrowly on purpose: "Claude" and "Claudette" are ordinary given names,
# and this rewrites the message before git records it, so a false positive deletes
# a human's credit with no copy left anywhere. The address carries the signal —
# the name pattern only backstops a future trailer that stops using @anthropic.com.
cleaned="$(grep -v -i -E '^Co-authored-by:.*@anthropic\.com' "$msg" \
  | grep -v -i -E '^Co-authored-by:[[:space:]]*(Claude|Anthropic)([[:space:]]+(Code|Opus|Sonnet|Haiku|Fable)[^<]*)?[[:space:]]*<' \
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

/// Every hook that gets a passthrough shim.
///
/// `core.hooksPath` is global and applies to receive-pack too, so the server-side
/// hooks are here as well: without shims, pushing to any local bare/deploy repo
/// silently skips its `pre-receive`/`update` gate and the push is accepted.
///
/// A shim is only safe for hooks whose ABSENCE is a no-op, because ours exits 0
/// when the repo has none. Deliberately excluded, all three for that reason:
/// - `push-to-checkout` — git runs its built-in push-to-deploy checkout only when
///   the hook does NOT exist, so a shim leaves the worktree stale after a push.
/// - `proc-receive` — git speaks a version handshake to it; exiting 0 is not a
///   valid no-op.
/// - `fsmonitor-watchman` — has semantics a shim can't fake.
/// `reference-transaction` is excluded on cost: it fires per ref update and the
/// shell spawn is measurable on large fetches. A repo using one loses it while
/// this rule is on.
const PASSTHROUGH_HOOKS: &[&str] = &[
    // Client-side
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
    "sendemail-validate",
    "post-index-change",
    // Server-side (a local push to a bare or deploy repo runs these)
    "pre-receive",
    "update",
    "post-receive",
    "post-update",
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
            blocked: true,
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
                blocked: false,
                detail: (!present).then(|| {
                    "core.hooksPath points at maiTerm but the hook file is missing — toggle this on to rewrite it.".to_string()
                }),
            }
        }
        Some(other) => DeshittifyRuleStatus {
            id: RULE_COMMIT_HOOK.into(),
            applied: false,
            blocked: true,
            detail: Some(format!(
                "Your global core.hooksPath is already set to {other} — maiTerm won't overwrite it. Clear it with `git config --global --unset core.hooksPath`, or install the hook into that directory yourself."
            )),
        },
        None => DeshittifyRuleStatus {
            id: RULE_COMMIT_HOOK.into(),
            applied: false,
            blocked: false,
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
    // An unreadable settings file blocks every rule backed by it, rather than
    // reading as "nothing applied" and inviting a click that would overwrite it.
    let (settings, settings_err) = match read_claude_settings() {
        Ok(v) => (v, None),
        Err(e) => (serde_json::json!({}), Some(e)),
    };

    let mut rules: Vec<DeshittifyRuleStatus> = ENV_RULES
        .iter()
        .map(|(id, key)| DeshittifyRuleStatus {
            id: (*id).into(),
            applied: settings_err.is_none() && env_var_applied(&settings, key),
            blocked: settings_err.is_some(),
            detail: settings_err.clone(),
        })
        .collect();

    rules.push(DeshittifyRuleStatus {
        id: RULE_CO_AUTHORED.into(),
        applied: settings_err.is_none() && co_authored_applied(&settings),
        blocked: settings_err.is_some(),
        detail: settings_err.clone(),
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
            let Some(rule) = before.rules.iter().find(|r| &r.id == id) else {
                continue;
            };
            // Skip rules already in the requested state so "turn everything on"
            // can't trip over a rule that is on but would now refuse to re-apply,
            // and skip blocked ones — the UI already shows why they can't move,
            // and an error here would be noise the user can't act on differently.
            if rule.applied == enabled || rule.blocked {
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

    /// "Claude" is an ordinary given name and the hook rewrites the message before
    /// git records it — a false positive deletes a person's credit irrecoverably.
    #[test]
    fn commit_msg_hook_spares_humans_named_claude() {
        let dir = scratch("humans");
        let hook = dir.join("commit-msg");
        write_exec(&hook, &commit_msg_script());

        let msg = dir.join("COMMIT_EDITMSG");
        fs::write(
            &msg,
            "Fix it\n\n\
             Co-authored-by: Claude Dubois <claude.dubois@example.fr>\n\
             Co-authored-by: Claudia Rossi <claudia@example.com>\n\
             Co-authored-by: Claude <noreply@anthropic.com>\n\
             Co-authored-by: Claude Opus 5 (1M context) <noreply@anthropic.com>\n\
             Co-authored-by: Claude Code <someone@example.com>\n",
        )
        .unwrap();

        assert!(Command::new("sh").arg(&hook).arg(&msg).current_dir(&dir).status().unwrap().success());
        let out = fs::read_to_string(&msg).unwrap();

        assert!(out.contains("Claude Dubois"), "a human co-author was deleted: {out:?}");
        assert!(out.contains("Claudia Rossi"), "a human co-author was deleted: {out:?}");
        assert!(!out.contains("anthropic.com"), "agent trailer survived: {out:?}");
        // Backstop for a future trailer that stops using an @anthropic.com address.
        assert!(!out.contains("Claude Code <"), "agent trailer survived: {out:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// A shim is only safe where the hook's ABSENCE is a no-op. `push-to-checkout`
    /// and `proc-receive` fail that test; the server-side gates need shims or a
    /// push to a local bare repo silently bypasses them.
    #[test]
    fn passthrough_list_covers_server_hooks_and_omits_the_unsafe_ones() {
        for unsafe_hook in ["push-to-checkout", "proc-receive", "fsmonitor-watchman"] {
            assert!(
                !PASSTHROUGH_HOOKS.contains(&unsafe_hook),
                "{unsafe_hook} cannot be shimmed — its absence is not a no-op"
            );
        }
        for gate in ["pre-receive", "update", "post-receive", "post-update"] {
            assert!(
                PASSTHROUGH_HOOKS.contains(&gate),
                "{gate} needs a shim or core.hooksPath silently disables it"
            );
        }
    }

    /// Every writer here is read-modify-write, so a settings file we can't parse
    /// must stop the write rather than degrade to `{}` and rename over the user's
    /// hooks, permissions and model settings.
    #[test]
    fn unparseable_settings_block_rather_than_overwrite() {
        let dir = scratch("badjson");
        let path = dir.join("settings.json");

        // Trailing comma — what a hand-edit typically leaves behind.
        fs::write(&path, "{\n  \"model\": \"opus\",\n}\n").unwrap();
        let err = read_settings_at(&path).unwrap_err();
        assert!(err.contains("could not be parsed"), "unexpected error: {err}");
        // set_env_var / set_co_authored propagate this with `?` before writing.

        // A missing or empty file is genuinely empty settings, not a failure.
        assert_eq!(read_settings_at(&dir.join("nope.json")).unwrap(), serde_json::json!({}));
        fs::write(&path, "  \n").unwrap();
        assert_eq!(read_settings_at(&path).unwrap(), serde_json::json!({}));

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
