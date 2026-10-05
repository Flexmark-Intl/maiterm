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
/// Rule id for `disableRemoteControl: true` + `remoteControlAtStartup: false` in
/// `~/.claude/settings.json`.
const RULE_REMOTE_CONTROL: &str = "cc_disable_remote_control";

/// Rule id for the PermissionRequest hook that answers Claude Code's prompts for
/// writes into its own auto-memory directory.
const RULE_MEMORY_WRITES: &str = "cc_allow_memory_writes";

/// Rules maiTerm applies on its own, once per user account (`seed_default_rules`).
/// After that the usual rule holds — disk is the toggle — so switching one off sticks.
const DEFAULT_ON_RULES: &[&str] = &[RULE_REMOTE_CONTROL, RULE_MEMORY_WRITES];

/// Which default-on rules have been settled: seeded by maiTerm, or toggled by the
/// user. One rule id per line. Lives beside the state it protects rather than in
/// `AppData`, and is deliberately not dev/prod-suffixed, for the same reason as the
/// hooks directory: settings.json is one file shared by every maiTerm build, and a
/// per-install marker is lost to a backup import, a downgrade, or the other build —
/// each of which would switch a rule the user turned off back on.
fn seeded_marker_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(".maiterm").join("deshittify-seeded"))
}

fn read_seeded(path: &std::path::Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn mark_seeded(path: &std::path::Path, id: &str) -> Result<(), String> {
    let mut ids = read_seeded(path);
    if ids.iter().any(|s| s == id) {
        return Ok(());
    }
    ids.push(id.to_string());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    fs::write(path, ids.join("\n") + "\n").map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

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

/// Both keys, because they cover different ground: `disableRemoteControl` removes
/// the feature outright (claude.ai/code, `claude remote-control`, `--rc`, the
/// in-session command), `remoteControlAtStartup: false` overrides the auto-start
/// an org or rollout default can switch on even where the feature stays allowed.
/// The user's own explicit opt-in — Remote Control switched on by hand. The seed
/// leaves such a file alone: a default is for people who never chose.
fn remote_control_chosen_on(settings: &serde_json::Value) -> bool {
    settings.get("disableRemoteControl") == Some(&serde_json::Value::Bool(false))
        || settings.get("remoteControlAtStartup") == Some(&serde_json::Value::Bool(true))
}

fn remote_control_applied(settings: &serde_json::Value) -> bool {
    settings.get("disableRemoteControl") == Some(&serde_json::Value::Bool(true))
        && settings.get("remoteControlAtStartup") == Some(&serde_json::Value::Bool(false))
}

fn set_remote_control(enabled: bool) -> Result<(), String> {
    let mut settings = read_claude_settings()?;
    let obj = settings
        .as_object_mut()
        .ok_or("~/.claude/settings.json is not a JSON object")?;
    if enabled {
        obj.insert("disableRemoteControl".into(), serde_json::Value::Bool(true));
        obj.insert("remoteControlAtStartup".into(), serde_json::Value::Bool(false));
    } else {
        // Absent is Claude Code's default for both. Remove only our values: a
        // `remoteControlAtStartup: true` is the user's, and dropping it would
        // silently switch their auto-start off.
        if obj.get("disableRemoteControl") == Some(&serde_json::Value::Bool(true)) {
            obj.remove("disableRemoteControl");
        }
        if obj.get("remoteControlAtStartup") == Some(&serde_json::Value::Bool(false)) {
            obj.remove("remoteControlAtStartup");
        }
    }
    write_claude_settings(&settings)
}

// ─── Auto-memory writes ────────────────────────────────────────────────────

/// Where the hook lives, relative to `$HOME` — the same on every host, so the
/// settings entry can name it through `$HOME` and stay identical everywhere.
const MEMORY_HOOK_REL: &str = ".maiterm/claude-hooks/approve-memory-writes";
/// Identifies our entry in `hooks.PermissionRequest`, however its path is spelled.
const MEMORY_HOOK_TAG: &str = "/.maiterm/claude-hooks/approve-memory-writes";
/// Claude Code runs hook commands through a shell, so `$HOME` expands there.
const MEMORY_HOOK_COMMAND: &str = "\"$HOME\"/.maiterm/claude-hooks/approve-memory-writes";
const MEMORY_HOOK_MATCHER: &str = "Write|Edit|Bash";

fn memory_hook_path() -> Option<PathBuf> {
    dirs::home_dir().map(|h| h.join(MEMORY_HOOK_REL))
}

/// Claude Code allows writes into its memory directory by itself, but compares the
/// symlink-RESOLVED path of the write against a memory directory it did not resolve
/// (2.1.287). Wherever part of the config directory is a link — every maiTerm
/// managed account, whose `projects` points at `~/.claude/projects`, or a dotfiles
/// checkout — the two never match, so each memory write falls through to the
/// sensitive-file check: a prompt, and a flat denial in auto mode. That check sits
/// ahead of permission rules and PreToolUse hooks (both tested; neither gets past
/// it). A PermissionRequest hook does, because it answers the prompt itself.
///
/// Pure sh so it runs on any bridged host. JSON is parsed with a real parser only —
/// `content` is agent-written, and a regex over the raw input could be steered into
/// reading a forged `file_path` out of it. No parser, no answer.
fn memory_hook_script() -> String {
    format!(
        r#"#!/bin/sh
# maiTerm Deshittification: let Claude Code write its own auto-memory without asking.
# {MANAGED_MARKER}, do not edit; maiTerm rewrites this file.
#
# Claude Code approves writes into its memory directory itself, but checks the
# symlink-resolved path of the write against a memory directory it did not
# resolve. Where any part of the config directory is a link, the two never match
# and every memory write turns into a permission prompt (auto mode denies it).
#
# This is a PermissionRequest hook: it runs only when a prompt is about to be
# shown. It answers "allow" for a Markdown file inside a memory directory, judged
# on the path as written AND on where it really leads, and refuses a Bash write
# there (see below). Anything else gets no answer, and the prompt appears exactly
# as before.
input=$(cat)

field() {{
  if [ "$(uname -s)" = Darwin ]; then
    # Not python3: on a Mac without the developer tools it is a stub that opens
    # an install dialog.
    printf '%s' "$input" | plutil -extract "$1" raw -o - - 2>/dev/null
  elif command -v python3 >/dev/null 2>&1; then
    printf '%s' "$input" | python3 -c '
import json,sys
v=json.load(sys.stdin)
for k in sys.argv[1].split("."):
    if isinstance(v,dict):
        v=v.get(k)
    elif isinstance(v,list) and k.isdigit() and int(k)<len(v):
        v=v[int(k)]
    else:
        v=None
sys.stdout.write((v if isinstance(v,str) else "")+"\n")' "$1" 2>/dev/null
  elif command -v jq >/dev/null 2>&1; then
    printf '%s' "$input" | jq -r --arg k "$1" 'getpath($k|split(".")|map(tonumber? // .)) | strings' 2>/dev/null
  fi
}}

# in_memory_dir PATH ROOT...: PATH is ROOT/projects/<key>/memory/<...>.md for one
# of the ROOTs, with no empty, ".", ".." or hidden segment below projects/.
in_memory_dir() {{
  p=$1
  shift
  for r in "$@"; do
    [ -n "$r" ] || continue
    rest=${{p#"${{r%/}}"/projects/}}
    [ "$rest" != "$p" ] || continue
    case "/$rest" in */.*|*//*) continue ;; esac
    key=${{rest%%/*}}
    file=${{rest#"$key"/memory/}}
    [ -n "$key" ] && [ -n "$file" ] && [ "$file" != "$rest" ] || continue
    case "$file" in *.md) return 0 ;; esac
  done
  return 1
}}

realroot() {{
  [ -n "$1" ] && [ -d "$1" ] && (cd -P "$1" 2>/dev/null && pwd -P)
}}

cfg=${{CLAUDE_CONFIG_DIR:-}}

# A Bash write into a memory directory (cat > memory/x.md) raises the same prompt.
# A shell command can't be judged safe from its text, so it is never approved; it
# is REFUSED at once, telling the agent to use Write or Edit, which are approved
# below. Without this it waits for a human, for hours if nobody is looking. The
# directory Claude Code asks to add is how the prompt says what it is about.
# Deleting, moving, copying or linking is left to the human: Write and Edit can't
# do it, so refusing would make it impossible, not redirect it.
if [ "$(field tool_name)" = Bash ]; then
  command -v grep >/dev/null 2>&1 || exit 0
  if field tool_input.command | grep -Eq '(^|[^[:alnum:]_.-])(rm|rmdir|unlink|mv|cp|ln|install|rsync)([[:space:]]|$)'; then
    exit 0
  fi
  for i in 0 1 2 3; do
    for j in 0 1 2 3; do
      d=$(field "permission_suggestions.$i.directories.$j")
      [ -n "$d" ] || break
      if in_memory_dir "${{d%/}}/x.md" "$cfg" "$HOME/.claude"; then
        printf '%s\n' '{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{{"behavior":"deny","message":"Do not write memory files with Bash. Use the Write or Edit tool for files in the memory directory: those are approved automatically, a Bash write there waits for a human."}}}}}}'
        exit 0
      fi
    done
  done
  exit 0
fi

case "$(field tool_name)" in Write|Edit) ;; *) exit 0 ;; esac
# Exactly: every parser ends its value with one newline, and $(...) would strip
# more than that, approving "a.md<newline>" as though it were "a.md".
nl='
'
path=$(field tool_input.file_path; echo .)
path=${{path%.}}
path=${{path%"$nl"}}
case "$path" in *"$nl"*) exit 0 ;; esac
case "$path" in /*) ;; *) exit 0 ;; esac
in_memory_dir "$path" "$cfg" "$HOME/.claude" || exit 0

# Where it really leads. The deepest directory that exists is resolved; what is
# below it does not exist yet, so it cannot be a link.
[ -L "$path" ] && exit 0
dir=${{path%/*}}
probe=$dir
while [ ! -d "$probe" ]; do
  probe=${{probe%/*}}
  [ -n "$probe" ] || exit 0
done
real=$(cd -P "$probe" 2>/dev/null && pwd -P) || exit 0
real=${{real%/}}${{dir#"$probe"}}/${{path##*/}}
in_memory_dir "$real" "$(realroot "$cfg")" "$(realroot "$HOME/.claude")" || exit 0

printf '%s\n' '{{"hookSpecificOutput":{{"hookEventName":"PermissionRequest","decision":{{"behavior":"allow"}}}}}}'
"#
    )
}

fn is_memory_hook(hook: &serde_json::Value) -> bool {
    hook.get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| c.contains(MEMORY_HOOK_TAG))
}

fn memory_hook_registered(settings: &serde_json::Value) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get("PermissionRequest"))
        .and_then(|a| a.as_array())
        .is_some_and(|groups| {
            groups.iter().any(|g| {
                g.get("hooks")
                    .and_then(|h| h.as_array())
                    .is_some_and(|hs| hs.iter().any(is_memory_hook))
            })
        })
}

fn memory_hook_is_current(path: &std::path::Path) -> bool {
    fs::read_to_string(path).is_ok_and(|body| body == memory_hook_script())
}

fn install_memory_hook(path: &PathBuf) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    write_hook_file(path, &memory_hook_script())
}

/// The entry exactly as this build writes it. An older build registered a narrower
/// matcher (`Write|Edit`, before Bash writes were redirected), and a hook that is
/// never called for a tool can't answer for it — so "registered" isn't "current".
fn memory_hook_entry_is_current(settings: &serde_json::Value) -> bool {
    settings
        .get("hooks")
        .and_then(|h| h.get("PermissionRequest"))
        .and_then(|a| a.as_array())
        .is_some_and(|groups| {
            groups.iter().any(|g| {
                g.get("matcher").and_then(|m| m.as_str()) == Some(MEMORY_HOOK_MATCHER)
                    && g.get("hooks")
                        .and_then(|h| h.as_array())
                        .is_some_and(|hs| hs.len() == 1 && hs.iter().any(is_memory_hook))
            })
        })
}

/// Put our entry into `hooks.PermissionRequest` as this build writes it (replacing
/// any older one), or take it out. Removing touches only hooks carrying our tag,
/// and drops a group or the event only when that left it empty — the event is
/// shared with maiTerm's own HTTP hook.
fn edit_memory_hook_entry(
    obj: &mut serde_json::Map<String, serde_json::Value>,
    enabled: bool,
) -> Result<(), String> {
    remove_memory_hook_entry(obj);
    if enabled {
        let hooks = obj.entry("hooks").or_insert_with(|| serde_json::json!({}));
        let hooks = hooks
            .as_object_mut()
            .ok_or("\"hooks\" in ~/.claude/settings.json is not an object")?;
        let groups = hooks
            .entry("PermissionRequest")
            .or_insert_with(|| serde_json::json!([]));
        let groups = groups
            .as_array_mut()
            .ok_or("\"hooks.PermissionRequest\" in ~/.claude/settings.json is not a list")?;
        groups.push(serde_json::json!({
            "matcher": MEMORY_HOOK_MATCHER,
            "hooks": [{ "type": "command", "command": MEMORY_HOOK_COMMAND }],
        }));
    }
    Ok(())
}

fn remove_memory_hook_entry(obj: &mut serde_json::Map<String, serde_json::Value>) {
    let Some(hooks) = obj.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return;
    };
    if let Some(groups) = hooks.get_mut("PermissionRequest").and_then(|a| a.as_array_mut()) {
        groups.retain_mut(|g| {
            let Some(hs) = g.get_mut("hooks").and_then(|h| h.as_array_mut()) else {
                return true;
            };
            let before = hs.len();
            hs.retain(|h| !is_memory_hook(h));
            !(hs.is_empty() && before > 0)
        });
        if groups.is_empty() {
            hooks.remove("PermissionRequest");
        }
    }
    if hooks.is_empty() {
        obj.remove("hooks");
    }
}

fn memory_writes_status(settings: &serde_json::Value, settings_err: &Option<String>) -> DeshittifyRuleStatus {
    let status = |applied: bool, blocked: bool, detail: Option<String>| DeshittifyRuleStatus {
        id: RULE_MEMORY_WRITES.into(),
        applied,
        blocked,
        detail,
    };
    if cfg!(windows) {
        return status(false, true, Some("Not available on Windows yet.".into()));
    }
    if settings_err.is_some() {
        return status(false, true, settings_err.clone());
    }
    let Some(path) = memory_hook_path() else {
        return status(false, true, Some("Could not determine home directory.".into()));
    };
    if !memory_hook_registered(settings) {
        return status(false, false, None);
    }
    if !memory_hook_is_current(&path) || !memory_hook_entry_is_current(settings) {
        match refresh_memory_writes(&path) {
            Ok(true) => log::info!("Deshittify: refreshed the memory-write hook"),
            Ok(false) => {}
            Err(e) => log::warn!("Deshittify: could not refresh the memory-write hook: {e}"),
        }
    }
    let current = memory_hook_is_current(&path);
    status(
        current,
        false,
        (!current).then(|| {
            format!("settings.json names the hook but ~/{MEMORY_HOOK_REL} is missing or out of date — toggle this on to rewrite it.")
        }),
    )
}

/// Bring an installed rule's stale script or entry up to date, as the githooks do,
/// or a fix would never reach the people who have the rule on.
///
/// Status reads call this from any thread, while a switch-off may be running on
/// another, which writes settings and THEN deletes the script. So a MISSING script
/// is never reinstalled here (it is what a switch-off in progress looks like; the
/// row reads as off and the toggle reinstalls it), and settings are re-read right
/// before acting rather than trusted from the caller's earlier read.
fn refresh_memory_writes(path: &PathBuf) -> Result<bool, String> {
    let mut settings = read_claude_settings()?;
    if !memory_hook_registered(&settings) || !path.exists() {
        return Ok(false);
    }
    let mut changed = false;
    if !memory_hook_is_current(path) {
        install_memory_hook(path)?;
        changed = true;
    }
    if !memory_hook_entry_is_current(&settings) {
        let obj = settings
            .as_object_mut()
            .ok_or("~/.claude/settings.json is not a JSON object")?;
        edit_memory_hook_entry(obj, true)?;
        write_claude_settings(&settings)?;
        changed = true;
    }
    Ok(changed)
}

fn set_memory_writes(enabled: bool) -> Result<(), String> {
    let path = memory_hook_path().ok_or("Could not determine home directory")?;
    let mut settings = read_claude_settings()?;
    let current = memory_hook_entry_is_current(&settings);
    let obj = settings
        .as_object_mut()
        .ok_or("~/.claude/settings.json is not a JSON object")?;
    if enabled {
        // Script first: settings must never name a hook that isn't there.
        install_memory_hook(&path)?;
        if !current {
            edit_memory_hook_entry(obj, true)?;
            write_claude_settings(&settings)?;
        }
    } else {
        edit_memory_hook_entry(obj, false)?;
        write_claude_settings(&settings)?;
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Cannot remove {}: {e}", path.display())),
        }
        if let Some(dir) = path.parent() {
            let _ = fs::remove_dir(dir); // only succeeds when empty
        }
    }
    Ok(())
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

/// Bumped whenever a generated script changes, so an already-installed directory
/// can be recognised as stale and rewritten. Without it, every future fix to
/// these scripts would land only for users who DON'T have the rule switched on.
const MANAGED_HOOKS_VERSION: u32 = 3;

/// Marks a file in the managed directory as ours to rewrite or remove.
const MANAGED_MARKER: &str = "maiterm-managed-hook";

fn version_stamp() -> String {
    format!("# {MANAGED_MARKER} v{MANAGED_HOOKS_VERSION} — do not edit; maiTerm rewrites this file.")
}

fn commit_msg_script() -> String {
    let stamp = version_stamp();
    format!(
        r#"#!/bin/sh
# maiTerm Deshittification — strip agent co-authorship trailers from commit messages.
{stamp}
msg="$1"
[ -f "$msg" ] || exit 0

# Two independent matches, both deliberately narrow. This rewrites the message
# before git records it, so a false positive deletes a person's credit with no
# copy left anywhere and no error.
#  1. The address. Every trailer Claude Code actually emits carries one
#     @anthropic.com, whatever the display name says.
#  2. A product name, REQUIRED — not optional. This exists only to backstop a
#     future trailer that stops using an @anthropic.com address, and "Claude" on
#     its own is an ordinary given name: git builds the trailer from user.name,
#     and a first-name-only user.name is the commonest setup there is.
# The third match leads with [^A-Za-z]* rather than naming the robot emoji, so
# this whole script stays ASCII and survives whatever locale a remote shell has.
cleaned="$(grep -v -i -E '^Co-authored-by:.*@anthropic\.com' "$msg" \
  | grep -v -i -E '^Co-authored-by:[[:space:]]*(Claude|Anthropic)[[:space:]]+(Code|Opus|Sonnet|Haiku|Fable)([[:space:]]|<|$)' \
  | grep -v -i -E '^[^A-Za-z]*Generated with \[?Claude Code')"
printf '%s\n' "$cleaned" > "$msg"

# core.hooksPath makes git skip the repository's own hooks — run it ourselves.
{RESOLVE_OWN_HOOK}
[ -x "$own" ] && exec "$own" "$@"
exit 0
"#
    )
}

fn passthrough_script() -> String {
    let stamp = version_stamp();
    format!(
        r#"#!/bin/sh
# maiTerm passthrough. core.hooksPath points git at maiTerm's managed hooks, which
# would otherwise silently disable this repository's own hook of the same name.
{stamp}
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
    // git-p4, which dispatches through `git hook run --ignore-missing` and so
    // honours core.hooksPath too. Absence exits 0 there, so a shim is equivalent —
    // without one, `git p4 submit` reads a skipped p4-pre-submit gate as a pass.
    "p4-changelist",
    "p4-prepare-changelist",
    "p4-post-changelist",
    "p4-pre-submit",
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

/// Write the current hook set into `dir`, clearing out any of OUR files that the
/// current set no longer includes. Files we didn't write (no marker) are left
/// alone; a stale shim of ours is removed, which is how a hook that turns out to
/// be unsafe to shim — `push-to-checkout` was one — gets withdrawn from a machine
/// that already has it.
fn install_managed_hooks(dir: &PathBuf) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;

    let keep: Vec<&str> = std::iter::once("commit-msg")
        .chain(PASSTHROUGH_HOOKS.iter().copied())
        .collect();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if keep.contains(&name.as_str()) {
                continue;
            }
            let ours = fs::read_to_string(entry.path())
                .map(|c| c.contains(MANAGED_MARKER))
                .unwrap_or(false);
            if ours {
                let _ = fs::remove_file(entry.path());
                log::info!("Deshittify: removed stale managed hook {name}");
            }
        }
    }

    write_hook_file(&dir.join("commit-msg"), &commit_msg_script())?;
    let passthrough = passthrough_script();
    for name in PASSTHROUGH_HOOKS {
        write_hook_file(&dir.join(name), &passthrough)?;
    }
    Ok(())
}

/// True when the installed set matches what this build would write. Compares the
/// version stamp rather than existence alone: a directory installed by an older
/// build is present but wrong, and nothing else in the app would ever notice.
fn managed_hooks_are_current(dir: &PathBuf) -> bool {
    let stamp = version_stamp();
    match fs::read_to_string(dir.join("commit-msg")) {
        Ok(body) if body.contains(&stamp) => {}
        _ => return false,
    }
    PASSTHROUGH_HOOKS.iter().all(|n| dir.join(n).exists())
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
            // core.hooksPath names this directory, so it is unambiguously ours to
            // maintain — self-heal it rather than reporting a stale install as
            // applied. Nothing else re-applies an enabled rule, so without this a
            // fix to the hook scripts never reaches the users who have them on.
            if hook_file.exists() && !managed_hooks_are_current(&ours) {
                match install_managed_hooks(&ours) {
                    Ok(()) => log::info!(
                        "Deshittify: refreshed managed hooks to v{MANAGED_HOOKS_VERSION}"
                    ),
                    Err(e) => log::warn!("Deshittify: could not refresh managed hooks: {e}"),
                }
            }
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

        install_managed_hooks(&ours)?;

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
    rules.push(DeshittifyRuleStatus {
        id: RULE_REMOTE_CONTROL.into(),
        applied: settings_err.is_none() && remote_control_applied(&settings),
        blocked: settings_err.is_some(),
        detail: settings_err.clone(),
    });
    rules.push(memory_writes_status(&settings, &settings_err));
    rules.push(commit_hook_status());

    DeshittifyStatus { rules }
}

/// Apply or revert one rule. A default-on rule the user has moved either way is
/// settled for good, so the next launch's seed leaves it where they put it.
fn apply_rule(id: &str, enabled: bool) -> Result<(), String> {
    apply_rule_inner(id, enabled)?;
    if DEFAULT_ON_RULES.contains(&id) {
        if let Some(path) = seeded_marker_path() {
            if let Err(e) = mark_seeded(&path, id) {
                log::warn!("Deshittify: could not record {id} as settled: {e}");
            }
        }
    }
    Ok(())
}

fn apply_rule_inner(id: &str, enabled: bool) -> Result<(), String> {
    if let Some((_, key)) = ENV_RULES.iter().find(|(rid, _)| *rid == id) {
        return set_env_var(enabled, key);
    }
    match id {
        RULE_CO_AUTHORED => set_co_authored(enabled),
        RULE_COMMIT_HOOK => set_commit_hook(enabled),
        RULE_REMOTE_CONTROL => set_remote_control(enabled),
        RULE_MEMORY_WRITES => set_memory_writes(enabled),
        other => Err(format!("Unknown deshittification rule: {other}")),
    }
}

// ─── Remote (SSH) parity ───────────────────────────────────────────────────

/// Merges this machine's env/`includeCoAuthoredBy` decisions into a remote
/// `~/.claude/settings.json`. No single quotes (the shell wraps it in them) and
/// pure ASCII (the remote python3 decodes it under whatever locale ssh gives it).
///
/// Rewrites the file only when something actually changed, so a user who has
/// never touched these rules gets reads and nothing else on every bridge connect.
/// Bails out rather than overwriting a file it can't parse — the same rule the
/// local writer follows, and for the same reason.
const PY_REMOTE_SETTINGS: &str = r#"import json,os,sys
d=json.load(sys.stdin)
p=os.path.expanduser("~/.claude/settings.json")
try:
    s=json.load(open(p)) if os.path.exists(p) else {}
except Exception:
    sys.exit(0)
if not isinstance(s,dict):
    sys.exit(0)
before=json.dumps(s,sort_keys=True)
e=s.get("env")
if not isinstance(e,dict):
    e={}
for k,v in d["envSet"].items():
    e[k]=v
for k in d["envUnset"]:
    e.pop(k,None)
if e:
    s["env"]=e
else:
    s.pop("env",None)
if d["coAuthored"]=="set":
    s["includeCoAuthoredBy"]=False
else:
    s.pop("includeCoAuthoredBy",None)
if d["remoteControl"]=="set":
    s["disableRemoteControl"]=True
    s["remoteControlAtStartup"]=False
else:
    if s.get("disableRemoteControl") is True:
        s.pop("disableRemoteControl")
    if s.get("remoteControlAtStartup") is False:
        s.pop("remoteControlAtStartup")
def ours(x):
    return isinstance(x,dict) and d["memoryHookTag"] in str(x.get("command",""))
hk=s.get("hooks")
pr=hk.get("PermissionRequest") if isinstance(hk,dict) else None
if d["memoryHook"]=="set":
    if hk is None:
        hk=s["hooks"]={}
    if isinstance(hk,dict) and pr is None:
        pr=hk["PermissionRequest"]=[]
    if isinstance(hk,dict) and isinstance(pr,list):
        want={"matcher":d["memoryHookMatcher"],"hooks":[{"type":"command","command":d["memoryHookCommand"]}]}
        if want not in pr:
            keep=[]
            for g in pr:
                if isinstance(g,dict) and isinstance(g.get("hooks"),list):
                    n=len(g["hooks"])
                    g["hooks"]=[x for x in g["hooks"] if not ours(x)]
                    if n and not g["hooks"]:
                        continue
                keep.append(g)
            keep.append(want)
            hk["PermissionRequest"]=keep
elif isinstance(pr,list):
    keep=[]
    for g in pr:
        if isinstance(g,dict) and isinstance(g.get("hooks"),list):
            n=len(g["hooks"])
            g["hooks"]=[x for x in g["hooks"] if not ours(x)]
            if n and not g["hooks"]:
                continue
        keep.append(g)
    if keep:
        hk["PermissionRequest"]=keep
    else:
        hk.pop("PermissionRequest")
    if not hk:
        s.pop("hooks")
if json.dumps(s,sort_keys=True)!=before:
    open(p,"w").write(json.dumps(s,indent=2))"#;

/// Render a shell script that makes a remote account match this machine's rules.
///
/// This machine is the source of truth in both directions: a rule that is off
/// here is REMOVED there, which is what makes switching one off actually reach
/// the hosts you use. The cost is the contention the rest of the remote-config
/// code works hard to avoid — a second maiTerm with different rules will flip
/// the same keys back on its own next connect. That is a deliberate trade, not
/// an oversight: an undo that doesn't travel is worse than one that can be
/// argued with, and both instances belong to the same person.
///
/// With every rule off the script still runs, because that IS the undo — but it
/// then performs reads only and writes nothing, so a user who has never opened
/// the section never has a remote file altered on their behalf.
fn render_remote_setup_script() -> String {
    // A settings file we can't read locally tells us nothing about what the
    // remote should look like — leave the remote's alone rather than reading our
    // own failure as "the user wants all of this removed".
    render_remote_setup_script_from(&build_status(), read_claude_settings().is_ok())
}

fn render_remote_setup_script_from(status: &DeshittifyStatus, settings_readable: bool) -> String {
    let applied = |id: &str| {
        status
            .rules
            .iter()
            .any(|r| r.id == id && r.applied)
    };

    let mut lines: Vec<String> = Vec::new();

    if settings_readable {
        let mut env_set = serde_json::Map::new();
        let mut env_unset: Vec<&str> = Vec::new();
        for (id, key) in ENV_RULES {
            if applied(id) {
                env_set.insert((*key).to_string(), serde_json::Value::String(ENV_VALUE.into()));
            } else {
                env_unset.push(key);
            }
        }
        let payload = serde_json::json!({
            "envSet": env_set,
            "envUnset": env_unset,
            "coAuthored": if applied(RULE_CO_AUTHORED) { "set" } else { "unset" },
            "remoteControl": if applied(RULE_REMOTE_CONTROL) { "set" } else { "unset" },
            "memoryHook": if applied(RULE_MEMORY_WRITES) { "set" } else { "unset" },
            "memoryHookCommand": MEMORY_HOOK_COMMAND,
            "memoryHookTag": MEMORY_HOOK_TAG,
            "memoryHookMatcher": MEMORY_HOOK_MATCHER,
        });
        let escaped = payload.to_string().replace('\'', r"'\''");
        lines.push(format!("__desh='{escaped}'"));
        lines.push(format!("__mh=\"$HOME/{MEMORY_HOOK_REL}\""));
        if applied(RULE_MEMORY_WRITES) {
            // Script before the settings entry that names it — and only where the
            // entry can be written at all, so a host without python3 isn't left
            // holding a script nothing calls.
            lines.push("if command -v python3 >/dev/null 2>&1; then".into());
            lines.push("mkdir -p \"${__mh%/*}\"".into());
            lines.push(format!(
                "cat > \"$__mh\" << 'MAITERMMEMEOF'\n{}MAITERMMEMEOF",
                memory_hook_script()
            ));
            lines.push("chmod +x \"$__mh\"".into());
            lines.push("fi".into());
        }
        lines.push("if command -v python3 >/dev/null 2>&1; then".into());
        lines.push(format!("printf '%s' \"$__desh\" | python3 -c '{PY_REMOTE_SETTINGS}'"));
        lines.push("fi".into());
        if !applied(RULE_MEMORY_WRITES) {
            lines.push("if [ -f \"$__mh\" ]; then".into());
            lines.push("rm -f \"$__mh\"".into());
            lines.push("rmdir \"${__mh%/*}\" 2>/dev/null".into());
            lines.push("fi".into());
        }
    }

    lines.push("__gh=\"$HOME/.maiterm/githooks\"".into());
    lines.push("__cur=$(git config --global --get core.hooksPath 2>/dev/null)".into());

    if applied(RULE_COMMIT_HOOK) {
        let keep = std::iter::once("commit-msg")
            .chain(PASSTHROUGH_HOOKS.iter().copied())
            .collect::<Vec<_>>()
            .join(" ");
        // Same refusal as local: a hooksPath somebody else set is theirs.
        lines.push("if [ -z \"$__cur\" ] || [ \"$__cur\" = \"$__gh\" ]; then".into());
        lines.push("mkdir -p \"$__gh\"".into());
        // Withdraw shims from an older install that this build no longer ships —
        // ours carry the marker, anything else in there is the user's.
        lines.push(format!("__keep=\" {keep} \""));
        lines.push("for __f in \"$__gh\"/*; do".into());
        lines.push("[ -f \"$__f\" ] || continue".into());
        lines.push(format!("grep -q {MANAGED_MARKER} \"$__f\" 2>/dev/null || continue"));
        lines.push("case \"$__keep\" in *\" ${__f##*/} \"*) ;; *) rm -f \"$__f\";; esac".into());
        lines.push("done".into());
        lines.push(format!(
            "cat > \"$__gh/commit-msg\" << 'MAITERMHOOKEOF'\n{}MAITERMHOOKEOF",
            commit_msg_script()
        ));
        lines.push(format!(
            "cat > \"$__gh/.passthrough\" << 'MAITERMPASSEOF'\n{}MAITERMPASSEOF",
            passthrough_script()
        ));
        lines.push(format!("for __h in {}; do", PASSTHROUGH_HOOKS.join(" ")));
        lines.push("cp \"$__gh/.passthrough\" \"$__gh/$__h\"".into());
        lines.push("done".into());
        lines.push("rm -f \"$__gh/.passthrough\"".into());
        lines.push("chmod +x \"$__gh\"/*".into());
        lines.push("git config --global core.hooksPath \"$__gh\"".into());
        lines.push("fi".into());
    } else {
        // Only ever unset a hooksPath that is ours, and skip the whole thing on a
        // host we never touched.
        lines.push("if [ \"$__cur\" = \"$__gh\" ]; then".into());
        lines.push("git config --global --unset core.hooksPath".into());
        lines.push("fi".into());
        lines.push("if [ -d \"$__gh\" ]; then".into());
        lines.push("rm -rf \"$__gh\"".into());
        lines.push("fi".into());
    }

    // `:` and not `exit 0` — buildUserSetupScript concatenates this into a script
    // the user pastes into their own interactive shell, and an `exit` there closes
    // it. It also keeps the script's status 0 after a guard that tested false.
    lines.push(":".into());
    lines.join("\n")
}

/// Apply each default-on rule not yet settled (see `seeded_marker_path`). A rule
/// that is blocked or fails to apply stays unsettled so the next launch tries again
/// — an unreadable settings file is not the user saying no. A rule already on disk,
/// or one the user has explicitly chosen the other way, is settled without a write.
///
/// Also brings an installed rule's maiTerm-written files up to date: reading the
/// status self-heals them (`memory_writes_status`, `commit_hook_status`), and
/// without a read at launch a fix would wait until someone opened the section.
pub fn seed_default_rules() {
    let _ = build_status();
    let Some(marker) = seeded_marker_path() else {
        return;
    };
    let seeded = read_seeded(&marker);
    let pending: Vec<&str> = DEFAULT_ON_RULES
        .iter()
        .copied()
        .filter(|id| !seeded.iter().any(|s| s == id))
        .collect();
    if pending.is_empty() {
        return;
    }
    let status = build_status();
    let settings = read_claude_settings().unwrap_or_else(|_| serde_json::json!({}));
    for id in pending {
        let Some(rule) = status.rules.iter().find(|r| r.id == id) else {
            continue;
        };
        if rule.blocked {
            log::warn!("Deshittify: default rule {id} is blocked, will retry next launch");
            continue;
        }
        if rule.applied || (id == RULE_REMOTE_CONTROL && remote_control_chosen_on(&settings)) {
            if let Err(e) = mark_seeded(&marker, id) {
                log::warn!("Deshittify: could not record {id} as settled: {e}");
            }
            continue;
        }
        match apply_rule(id, true) {
            Ok(()) => log::info!("Deshittify: applied default rule {id}"),
            Err(e) => log::warn!("Deshittify: could not apply default rule {id}: {e}"),
        }
    }
}

/// The script that carries these rules to a bridged SSH host. Empty string when
/// there is nothing to do.
#[tauri::command]
pub async fn build_deshittify_setup_script() -> Result<String, String> {
    tauri::async_runtime::spawn_blocking(render_remote_setup_script)
        .await
        .map_err(|e| format!("Render task failed: {e}"))
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
             Co-authored-by: Claudette Dupont <claudette@example.fr>\n\
             Co-authored-by: Claude <claude.martin@example.fr>\n\
             Co-authored-by: Claude <noreply@anthropic.com>\n\
             Co-authored-by: Claude Opus 5 (1M context) <noreply@anthropic.com>\n\
             Co-authored-by: Claude Sonnet 4.5 <noreply@anthropic.com>\n\
             Co-authored-by: Claude Code <someone@example.com>\n",
        )
        .unwrap();

        assert!(Command::new("sh").arg(&hook).arg(&msg).current_dir(&dir).status().unwrap().success());
        let out = fs::read_to_string(&msg).unwrap();

        assert!(out.contains("Claude Dubois"), "a human co-author was deleted: {out:?}");
        assert!(out.contains("Claudia Rossi"), "a human co-author was deleted: {out:?}");
        assert!(out.contains("Claudette Dupont"), "a human co-author was deleted: {out:?}");
        // git builds the trailer from user.name, and a first-name-only user.name is
        // the commonest setup there is — so this is the likeliest form a real human
        // named Claude produces, and the one the first fix still ate.
        assert!(
            out.contains("Claude <claude.martin@example.fr>"),
            "a human whose git name is just \"Claude\" was deleted: {out:?}"
        );
        assert!(!out.contains("anthropic.com"), "agent trailer survived: {out:?}");
        // Backstop for a future trailer that stops using an @anthropic.com address.
        assert!(!out.contains("Claude Code <"), "agent trailer survived: {out:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    /// A directory installed by an older build must be recognised as stale and
    /// rewritten — otherwise every fix to these scripts lands only for the users
    /// who DON'T have the rule switched on.
    #[test]
    fn a_stale_managed_directory_is_detected_and_rewritten() {
        let dir = scratch("stale");
        fs::create_dir_all(&dir).unwrap();

        // What the previous version left behind: an unstamped commit-msg and a
        // push-to-checkout shim that is no longer safe to ship.
        write_exec(&dir.join("commit-msg"), "#!/bin/sh\n# maiterm-managed-hook v1\nexit 0\n");
        write_exec(&dir.join("push-to-checkout"), "#!/bin/sh\n# maiterm-managed-hook v1\nexit 0\n");
        // Something the user put there themselves — not ours, must survive.
        write_exec(&dir.join("their-own-script"), "#!/bin/sh\necho mine\n");

        assert!(!managed_hooks_are_current(&dir), "a v1 install should read as stale");

        install_managed_hooks(&dir).unwrap();

        assert!(managed_hooks_are_current(&dir));
        assert!(
            !dir.join("push-to-checkout").exists(),
            "the withdrawn shim must be removed, not just left in place"
        );
        assert!(dir.join("their-own-script").exists(), "a file we didn't write was deleted");
        assert!(dir.join("pre-receive").exists(), "the newly added shims were not written");
        assert!(fs::read_to_string(dir.join("commit-msg")).unwrap().contains("@anthropic"));

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
        for gate in [
            "pre-receive",
            "update",
            "post-receive",
            "post-update",
            // git-p4 runs these through `git hook run --ignore-missing`, which
            // honours core.hooksPath — a missing shim reads as "gate passed".
            "p4-changelist",
            "p4-prepare-changelist",
            "p4-post-changelist",
            "p4-pre-submit",
        ] {
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

    fn status_with(all_on: bool) -> DeshittifyStatus {
        let mut rules: Vec<DeshittifyRuleStatus> = ENV_RULES
            .iter()
            .map(|(id, _)| DeshittifyRuleStatus {
                id: (*id).into(),
                applied: all_on,
                blocked: false,
                detail: None,
            })
            .collect();
        for id in [RULE_CO_AUTHORED, RULE_COMMIT_HOOK, RULE_REMOTE_CONTROL, RULE_MEMORY_WRITES] {
            rules.push(DeshittifyRuleStatus {
                id: id.into(),
                applied: all_on,
                blocked: false,
                detail: None,
            });
        }
        DeshittifyStatus { rules }
    }

    /// Run a rendered remote script against a throwaway HOME, exactly as the
    /// bridge would over ssh, and report what the "remote" account looks like.
    fn run_remote_script(script: &str, home: &PathBuf) -> std::process::Output {
        Command::new("sh")
            .arg("-c")
            .arg(script)
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .current_dir(home)
            .output()
            .unwrap()
    }

    fn git_global(home: &PathBuf, key: &str) -> String {
        let out = Command::new("git")
            .args(["config", "--global", "--get", key])
            .env("HOME", home)
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// The remote script is assembled as one ssh argument out of heredocs, a
    /// python program and a case statement — the kind of thing that is either
    /// exactly right or silently truncated. Run it for real.
    #[test]
    fn remote_script_applies_every_rule_to_a_fresh_account() {
        let home = scratch("remote-on");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(home.join(".claude/settings.json"), r#"{"model":"opus"}"#).unwrap();

        let script = render_remote_setup_script_from(&status_with(true), true);
        assert!(
            Command::new("sh").args(["-n", "-c", &script]).status().unwrap().success(),
            "rendered script is not valid sh"
        );

        let out = run_remote_script(&script, &home);
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

        // Settings merged, the user's own key untouched.
        let s: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(s["model"], "opus", "the remote's own settings were clobbered");
        assert_eq!(s["env"]["DISABLE_TELEMETRY"], "1");
        assert_eq!(s["env"]["CLAUDE_CODE_DISABLE_FEEDBACK_SURVEY"], "1");
        assert_eq!(s["includeCoAuthoredBy"], serde_json::Value::Bool(false));
        assert_eq!(s["disableRemoteControl"], serde_json::Value::Bool(true));
        assert_eq!(s["remoteControlAtStartup"], serde_json::Value::Bool(false));
        assert_eq!(s["hooks"]["PermissionRequest"][0]["hooks"][0]["command"], MEMORY_HOOK_COMMAND);
        assert_eq!(
            fs::read_to_string(home.join(MEMORY_HOOK_REL)).unwrap(),
            memory_hook_script(),
            "the memory hook arrived altered"
        );

        // Hooks installed and pointed at.
        let gh = home.join(".maiterm/githooks");
        assert!(gh.join("commit-msg").exists());
        assert!(gh.join("pre-receive").exists(), "server-side shim missing");
        assert!(gh.join("p4-pre-submit").exists(), "p4 shim missing");
        assert!(!gh.join(".passthrough").exists(), "the shim template was left behind");
        assert_eq!(git_global(&home, "core.hooksPath"), gh.to_string_lossy());

        // And the installed hook actually works on that "remote".
        let msg = home.join("MSG");
        fs::write(&msg, "Fix\n\nCo-Authored-By: Claude <noreply@anthropic.com>\n").unwrap();
        assert!(Command::new("sh").arg(gh.join("commit-msg")).arg(&msg).current_dir(&home).status().unwrap().success());
        assert!(!fs::read_to_string(&msg).unwrap().contains("Claude"));

        let _ = fs::remove_dir_all(&home);
    }

    /// Switching the rules off has to reach the hosts already carrying them —
    /// that is the whole point of re-running this on every connect.
    #[test]
    fn remote_script_undoes_every_rule_on_next_connect() {
        let home = scratch("remote-off");
        fs::create_dir_all(home.join(".claude")).unwrap();
        fs::write(home.join(".claude/settings.json"), r#"{"model":"opus"}"#).unwrap();

        // First bring the account fully up...
        run_remote_script(&render_remote_setup_script_from(&status_with(true), true), &home);
        assert!(home.join(".maiterm/githooks/commit-msg").exists());

        // ...then run the script this machine emits once the rules are switched off.
        let out = run_remote_script(&render_remote_setup_script_from(&status_with(false), true), &home);
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));

        let s: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(home.join(".claude/settings.json")).unwrap()).unwrap();
        assert_eq!(s["model"], "opus");
        assert!(s.get("env").is_none(), "env keys survived the undo: {s}");
        assert!(s.get("includeCoAuthoredBy").is_none(), "includeCoAuthoredBy survived the undo");
        assert!(s.get("disableRemoteControl").is_none(), "disableRemoteControl survived the undo");
        assert!(s.get("remoteControlAtStartup").is_none(), "remoteControlAtStartup survived the undo");
        assert!(s.get("hooks").is_none(), "the memory hook entry survived the undo: {s}");
        assert!(!home.join(MEMORY_HOOK_REL).exists(), "the memory hook script survived the undo");
        assert!(!home.join(".maiterm/githooks").exists(), "managed hooks survived the undo");
        assert_eq!(git_global(&home, "core.hooksPath"), "", "core.hooksPath survived the undo");

        let _ = fs::remove_dir_all(&home);
    }

    /// Switching Remote Control's rule off removes only OUR values — a remote whose
    /// owner set `remoteControlAtStartup: true` keeps it on every connect.
    #[test]
    fn remote_script_keeps_a_users_own_remote_control_choice() {
        let home = scratch("remote-rc");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude/settings.json");
        fs::write(&settings, "{\"remoteControlAtStartup\":true}").unwrap();
        let out = run_remote_script(&render_remote_setup_script_from(&status_with(false), true), &home);
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(fs::read_to_string(&settings).unwrap(), "{\"remoteControlAtStartup\":true}");
        let _ = fs::remove_dir_all(&home);
    }

    /// Feed the memory hook one PermissionRequest, as Claude Code would, and say
    /// whether it answered "allow".
    fn memory_hook_allows(home: &PathBuf, config_dir: Option<&PathBuf>, input: &serde_json::Value) -> bool {
        memory_hook_decision(home, config_dir, input).as_deref() == Some("allow")
    }

    /// The hook's answer to one PermissionRequest: "allow", "deny", or None for no
    /// answer at all (the prompt is shown as usual).
    fn memory_hook_decision(home: &PathBuf, config_dir: Option<&PathBuf>, input: &serde_json::Value) -> Option<String> {
        use std::io::Write;
        let hook = home.join("hook.sh");
        if !hook.exists() {
            write_exec(&hook, &memory_hook_script());
        }
        let mut cmd = Command::new("sh");
        cmd.arg(&hook)
            .env("HOME", home)
            .env_remove("CLAUDE_CONFIG_DIR")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped());
        if let Some(dir) = config_dir {
            cmd.env("CLAUDE_CONFIG_DIR", dir);
        }
        let mut child = cmd.spawn().unwrap();
        child.stdin.take().unwrap().write_all(input.to_string().as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        let stdout = String::from_utf8_lossy(&out.stdout);
        if stdout.trim().is_empty() {
            return None;
        }
        let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
        v["hookSpecificOutput"]["decision"]["behavior"].as_str().map(String::from)
    }

    fn bash_req(suggested: &[&std::path::Path]) -> serde_json::Value {
        let dirs: Vec<String> = suggested.iter().map(|p| p.to_string_lossy().into_owned()).collect();
        serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Bash",
            "tool_input": { "command": "cat > x <<'EOF'\nhi\nEOF" },
            "permission_suggestions": [
                { "type": "addDirectories", "directories": dirs, "destination": "session" },
            ],
        })
    }

    /// A Bash write into memory is refused at once, pointing the agent at Write or
    /// Edit — never approved, and nothing else about Bash is answered at all.
    #[test]
    fn memory_hook_turns_a_bash_memory_write_back_to_write() {
        let home = scratch("memhook-bash");
        let account = home.join("accounts/a1");
        let memory = account.join("projects/-repo/memory");
        fs::create_dir_all(&memory).unwrap();

        assert_eq!(memory_hook_decision(&home, Some(&account), &bash_req(&[&memory])).as_deref(), Some("deny"));
        // The memory directory need not be the first one suggested.
        let other = home.join("work");
        assert_eq!(
            memory_hook_decision(&home, Some(&account), &bash_req(&[&other, &memory])).as_deref(),
            Some("deny")
        );
        assert_eq!(memory_hook_decision(&home, Some(&account), &bash_req(&[&other])), None);
        // Removing, renaming or copying a memory is something Write/Edit can't do:
        // the human is asked, as before, rather than the agent refused outright.
        for cmd in [
            "rm \"$M/stale.md\"",
            "cd \"$M\" && mv old.md new.md",
            "rmdir \"$M/sub\"",
            "cp a.md \"$M/b.md\"",
            "/bin/rm -f x.md",
        ] {
            let mut req = bash_req(&[&memory]);
            req["tool_input"]["command"] = cmd.into();
            assert_eq!(memory_hook_decision(&home, Some(&account), &req), None, "refused {cmd}");
        }
        // ...but a write that merely mentions such a word in a filename is still refused.
        let mut req = bash_req(&[&memory]);
        req["tool_input"]["command"] = "cat > \"$M/firm-rules.md\" <<'EOF'\nx\nEOF".into();
        assert_eq!(memory_hook_decision(&home, Some(&account), &req).as_deref(), Some("deny"));
        assert_eq!(memory_hook_decision(&home, Some(&account), &bash_req(&[])), None);
        let mut plain = bash_req(&[]);
        plain.as_object_mut().unwrap().remove("permission_suggestions");
        assert_eq!(memory_hook_decision(&home, Some(&account), &plain), None);

        let _ = fs::remove_dir_all(&home);
    }

    /// A settings file holding the entry an older build wrote (narrower matcher)
    /// is brought up to date in place, not left beside a second copy.
    #[test]
    fn an_older_memory_hook_entry_is_replaced_not_duplicated() {
        let mut s = serde_json::json!({ "hooks": { "PermissionRequest": [
            { "matcher": "Write|Edit", "hooks": [{ "type": "command", "command": MEMORY_HOOK_COMMAND }] },
            { "matcher": "", "hooks": [{ "type": "http", "url": "http://127.0.0.1:1/hooks" }] },
        ] } });
        assert!(memory_hook_registered(&s));
        assert!(!memory_hook_entry_is_current(&s));
        edit_memory_hook_entry(s.as_object_mut().unwrap(), true).unwrap();
        assert!(memory_hook_entry_is_current(&s));
        let groups = s["hooks"]["PermissionRequest"].as_array().unwrap();
        assert_eq!(groups.len(), 2, "{s}");
        assert_eq!(groups.iter().filter(|g| g["hooks"].as_array().unwrap().iter().any(is_memory_hook)).count(), 1);
    }

    fn write_req(path: &std::path::Path) -> serde_json::Value {
        serde_json::json!({
            "hook_event_name": "PermissionRequest",
            "tool_name": "Write",
            "tool_input": { "file_path": path.to_string_lossy(), "content": "x" },
        })
    }

    /// The managed-account layout is the case this rule exists for: the account's
    /// `projects` is a link to `~/.claude/projects`. Everything that is not a
    /// Markdown file inside a memory directory, on both readings of the path, must
    /// get no answer at all.
    #[test]
    fn memory_hook_approves_memory_files_and_nothing_else() {
        let home = scratch("memhook");
        let home = PathBuf::from(Command::new("sh").arg("-c").arg("cd -P \"$0\" && pwd -P").arg(&home)
            .output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap());
        let real_projects = home.join(".claude/projects");
        fs::create_dir_all(real_projects.join("-repo/memory")).unwrap();
        let account = home.join("accounts/a1");
        fs::create_dir_all(&account).unwrap();
        std::os::unix::fs::symlink(&real_projects, account.join("projects")).unwrap();

        let via_account = account.join("projects/-repo/memory/note.md");
        assert!(memory_hook_allows(&home, Some(&account), &write_req(&via_account)), "the managed-account memory write was not approved");
        // The memory directory doesn't exist yet for a project's first memory.
        let fresh = account.join("projects/-new-repo/memory/MEMORY.md");
        assert!(memory_hook_allows(&home, Some(&account), &write_req(&fresh)), "a first memory was not approved");
        // A plain ~/.claude, and Edit as well as Write.
        let plain = real_projects.join("-repo/memory/sub/note.md");
        let mut edit = write_req(&plain);
        edit["tool_name"] = "Edit".into();
        assert!(memory_hook_allows(&home, None, &edit));

        let refused = [
            account.join("projects/-repo/memory/note.txt"),
            account.join("projects/-repo/note.md"),
            account.join("projects/-repo/memory/../../../settings.json"),
            account.join("projects/-repo/memory/.hidden/x.md"),
            account.join("settings.md"),
            home.join("elsewhere/projects/-repo/memory/note.md"),
        ];
        for p in &refused {
            assert!(!memory_hook_allows(&home, Some(&account), &write_req(p)), "approved {}", p.display());
        }

        // A memory file that is itself a link out of the memory directory.
        fs::write(home.join(".zshrc"), "").unwrap();
        let link = real_projects.join("-repo/memory/evil.md");
        std::os::unix::fs::symlink(home.join(".zshrc"), &link).unwrap();
        assert!(!memory_hook_allows(&home, None, &write_req(&link)), "followed a symlinked memory file");
        // A memory directory that is a link elsewhere.
        fs::create_dir_all(home.join("outside")).unwrap();
        std::os::unix::fs::symlink(home.join("outside"), real_projects.join("-linked")).unwrap();
        let linked = real_projects.join("-linked/memory/x.md");
        fs::create_dir_all(home.join("outside/memory")).unwrap();
        assert!(!memory_hook_allows(&home, None, &write_req(&linked)), "followed a linked project dir out");

        // Another tool, or a forged file_path inside agent-written content.
        let mut bash = write_req(&via_account);
        bash["tool_name"] = "Bash".into();
        assert!(!memory_hook_allows(&home, Some(&account), &bash));
        let forged = serde_json::json!({
            "tool_name": "Write",
            "tool_input": {
                "file_path": home.join(".zshrc").to_string_lossy(),
                "content": format!("\"file_path\": \"{}\"", via_account.display()),
            },
        });
        assert!(!memory_hook_allows(&home, Some(&account), &forged), "read file_path out of content");
        // A trailing newline is a different file from the one the checks would see.
        let newline = write_req(std::path::Path::new(&format!("{}\n", via_account.display())));
        assert!(!memory_hook_allows(&home, Some(&account), &newline), "approved a path ending in a newline");

        let _ = fs::remove_dir_all(&home);
    }

    /// The entry shares `hooks.PermissionRequest` with maiTerm's own HTTP hook;
    /// taking ours out must leave that one, and anything else, exactly where it was.
    #[test]
    fn memory_hook_entry_comes_and_goes_without_touching_other_hooks() {
        let theirs = serde_json::json!({ "matcher": "", "hooks": [{ "type": "http", "url": "http://127.0.0.1:1/hooks" }] });
        let mut s = serde_json::json!({ "hooks": { "PermissionRequest": [theirs.clone()] }, "model": "opus" });
        let obj = s.as_object_mut().unwrap();
        edit_memory_hook_entry(obj, true).unwrap();
        assert!(memory_hook_registered(&s));
        let obj = s.as_object_mut().unwrap();
        edit_memory_hook_entry(obj, false).unwrap();
        assert!(!memory_hook_registered(&s));
        assert_eq!(s, serde_json::json!({ "hooks": { "PermissionRequest": [theirs] }, "model": "opus" }));

        let mut empty = serde_json::json!({});
        edit_memory_hook_entry(empty.as_object_mut().unwrap(), true).unwrap();
        edit_memory_hook_entry(empty.as_object_mut().unwrap(), false).unwrap();
        assert_eq!(empty, serde_json::json!({}), "left an empty hooks scaffold behind");
    }

    #[test]
    fn seeded_marker_round_trips_and_dedupes() {
        let dir = scratch("seeded");
        let path = dir.join("nested").join("deshittify-seeded");
        assert!(read_seeded(&path).is_empty());
        mark_seeded(&path, RULE_REMOTE_CONTROL).unwrap();
        mark_seeded(&path, RULE_REMOTE_CONTROL).unwrap();
        assert_eq!(read_seeded(&path), vec![RULE_REMOTE_CONTROL.to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_explicit_remote_control_opt_in_is_recognised() {
        assert!(remote_control_chosen_on(&serde_json::json!({"remoteControlAtStartup": true})));
        assert!(remote_control_chosen_on(&serde_json::json!({"disableRemoteControl": false})));
        assert!(!remote_control_chosen_on(&serde_json::json!({})));
        assert!(!remote_control_chosen_on(&serde_json::json!({"remoteControlAtStartup": false})));
    }

    /// A host whose owner set their own core.hooksPath, and a user who never
    /// opened the section, must both come through untouched.
    #[test]
    fn remote_script_refuses_a_foreign_hookspath_and_writes_nothing_when_idle() {
        let home = scratch("remote-foreign");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude/settings.json");
        // Deliberately compact, so any rewrite at all shows up as a diff.
        fs::write(&settings, "{\"model\":\"opus\"}").unwrap();
        let theirs = home.join(".their-hooks");
        fs::create_dir_all(&theirs).unwrap();
        Command::new("git")
            .args(["config", "--global", "core.hooksPath", &theirs.to_string_lossy()])
            .env("HOME", &home)
            .env("GIT_CONFIG_GLOBAL", home.join(".gitconfig"))
            .status()
            .unwrap();

        let out = run_remote_script(&render_remote_setup_script_from(&status_with(true), true), &home);
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            git_global(&home, "core.hooksPath"),
            theirs.to_string_lossy(),
            "maiTerm overwrote a core.hooksPath it did not set"
        );
        assert!(!home.join(".maiterm/githooks/commit-msg").exists());

        // Every rule off, nothing ever installed: reads only, file untouched.
        let home2 = scratch("remote-idle");
        fs::create_dir_all(home2.join(".claude")).unwrap();
        let settings2 = home2.join(".claude/settings.json");
        fs::write(&settings2, "{\"model\":\"opus\"}").unwrap();
        let out2 = run_remote_script(&render_remote_setup_script_from(&status_with(false), true), &home2);
        assert!(out2.status.success(), "stderr: {}", String::from_utf8_lossy(&out2.stderr));
        assert_eq!(
            fs::read_to_string(&settings2).unwrap(),
            "{\"model\":\"opus\"}",
            "an idle run rewrote a remote settings file it had no business touching"
        );
        assert!(!home2.join(".maiterm").exists());

        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&home2);
    }

    /// An unparseable remote settings.json must stop the merge, not be replaced —
    /// the same rule the local writer follows.
    #[test]
    fn remote_script_leaves_an_unparseable_settings_file_alone() {
        let home = scratch("remote-badjson");
        fs::create_dir_all(home.join(".claude")).unwrap();
        let settings = home.join(".claude/settings.json");
        let broken = "{\n  \"model\": \"opus\",\n}\n";
        fs::write(&settings, broken).unwrap();

        let out = run_remote_script(&render_remote_setup_script_from(&status_with(true), true), &home);
        assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(fs::read_to_string(&settings).unwrap(), broken, "the broken file was overwritten");
        // The git half is independent and should still have applied.
        assert!(home.join(".maiterm/githooks/commit-msg").exists());

        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn hooks_path_comparison_expands_tilde() {
        let ours = dirs::home_dir().unwrap().join(".maiterm").join("githooks");
        assert!(hooks_path_is_ours("~/.maiterm/githooks", &ours));
        assert!(hooks_path_is_ours(&ours.to_string_lossy(), &ours));
        assert!(!hooks_path_is_ours("~/.husky", &ours));
    }
}

