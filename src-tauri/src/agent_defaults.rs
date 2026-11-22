//! Agent Defaults (docs/agent-defaults.md): the values every new session of an agent starts
//! with, written into that agent's OWN user settings file. The sister of Deshittify
//! (`commands/deshittify.rs`), with its mechanics:
//!
//! - **Disk is the state.** Nothing here is kept in maiTerm; `status` reads the files.
//! - **Absent is "the agent's default".** Clearing a value removes its key, never writes the
//!   runtime's current default, so a later upstream change of default reaches the user.
//! - **Only our keys are touched**, everything else is kept (TOML through `toml_edit`).
//! - **An unreadable file is never overwritten**: its runtime's part shows the error instead.
//! - **Writes take maiTerm's config write lock** (`util::json_file`) and are redone when the
//!   file changed under them: live agents rewrite these files too. Not every other maiTerm
//!   writer of these files takes that lock yet (the hook installer, Deshittify, the Codex
//!   registrar and the MCP proxy's Codex entries), so one of them can still overwrite a value
//!   written here in the same instant.
//!
//! Every function takes the file's path, so tests run on scratch files and never on the
//! human's own config. Keys and values were checked against Claude Code 2.1.330, Codex 0.166.0
//! and Antigravity 1.5.14 on 2026-11-22: re-check them when those CLIs move, since a wrong key
//! is silently ignored.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{json, Value};

// ─── What can be set ──────────────────────────────────────────────────────

/// Claude advisors offered. Claude refuses an advisor less capable than the main model.
pub const CLAUDE_ADVISORS: &[&str] = &["opus", "sonnet", "fable"];
/// Claude default models offered (`model`): the public aliases.
pub const CLAUDE_MODELS: &[&str] = &["opus", "sonnet", "fable", "haiku"];
/// The model ids Claude Code 2.1.330 resolves each alias to on a first-party login
/// (`provider_alias_targets`). Effort is stored per model id (`modelSettings.<id>.effortLevel`),
/// and an id key wins over an alias key, so effort is written under the id.
pub const CLAUDE_EFFORT_MODELS: &[(&str, &str)] = &[
    ("claude-opus-5-5", "Opus"),
    ("claude-sonnet-5-5", "Sonnet"),
    ("claude-fable-5-2", "Fable"),
    ("claude-haiku-5-5", "Haiku"),
];
/// Effort levels Claude Code keeps from settings (`max` is never persisted).
pub const CLAUDE_EFFORTS: &[&str] = &["low", "medium", "high", "xhigh"];
/// Claude deletes transcripts older than this many days unless `cleanupPeriodDays` says
/// otherwise; maiTerm's own default (seeded once, `seed_retention`) is `RETENTION_SEED_DAYS`.
pub const CLAUDE_RETENTION_DEFAULT_DAYS: u64 = 30;
pub const RETENTION_SEED_DAYS: u64 = 500;
const RETENTION_MAX_DAYS: u64 = 36_500;

/// Bypasses the server feature flag the advisor sits behind, which Claude doesn't fetch while
/// telemetry is off (`DISABLE_TELEMETRY`, set by Deshittify). Written with `advisorModel` and
/// removed with it. The same name as `hive::launch::ADVISOR_TOOL_ENV`.
pub const ADVISOR_TOOL_ENV: &str = crate::hive::launch::ADVISOR_TOOL_ENV;

/// Codex reasoning efforts (its own `ReasoningEffort` list; `none` left out: it turns
/// reasoning off, which no default should do quietly).
pub const CODEX_EFFORTS: &[&str] = &["minimal", "low", "medium", "high", "xhigh", "max"];
/// Codex `approvals_reviewer`: Codex refuses to start on any other value.
pub const CODEX_REVIEWERS: &[&str] = &["user", "auto_review"];

/// Antigravity `agentMode`. `""` is itself a saved choice ("Default"), distinct from absent.
pub const AGY_MODES: &[&str] = &["", "accept-edits", "plan", "auto"];
pub const AGY_QUEUED: &[&str] = &["queue", "send-immediately"];

// ─── Files ────────────────────────────────────────────────────────────────

/// The three files, under one home.
#[derive(Debug, Clone)]
pub struct Paths {
    pub claude: PathBuf,
    pub codex: PathBuf,
    pub agy: PathBuf,
    /// Which one-time defaults were settled (`seed_retention`).
    pub seeded: PathBuf,
}

impl Paths {
    /// The human's own files: Claude's is the `settings.json` the human's tabs read
    /// (`claude_settings_in`).
    pub fn real(prefs: &crate::state::Preferences) -> Option<Self> {
        let home = dirs::home_dir()?;
        let managed = crate::accounts::active_accounts(prefs)
            .into_iter()
            .any(|(rt, _)| rt == crate::accounts::Runtime::Claude);
        let mut p = Self::under(&home);
        p.claude = claude_settings_in(&home, managed, crate::accounts::user_claude_root());
        Some(p)
    }

    pub fn under(home: &Path) -> Self {
        Self {
            claude: home.join(".claude").join("settings.json"),
            codex: home.join(".codex").join("config.toml"),
            agy: home.join(".gemini").join("antigravity-cli").join("settings.json"),
            seeded: home.join(".maiterm").join("agent-defaults-seeded"),
        }
    }
}

/// The `settings.json` the human's Claude tabs read, as the Hive launch picks its root
/// (`hive::launch::launch_config_root`): with a managed Claude account, `~/.claude`'s, which
/// every account root symlinks, so one write reaches every account; otherwise the user's own
/// exported `CLAUDE_CONFIG_DIR` when there is one, and `~/.claude` without. Never `~/.claude`'s
/// beside a user's own root: their tabs don't read it.
fn claude_settings_in(home: &Path, managed_account: bool, users_own: Option<&Path>) -> PathBuf {
    match users_own {
        Some(root) if !managed_account => root.join("settings.json"),
        _ => home.join(".claude").join("settings.json"),
    }
}

/// Read-edit-replace `path` under maiTerm's config write lock, creating it (and its folder)
/// when absent. `edit` gets the current text (None: no file) and returns the new text, or None
/// for "nothing to change". Redone when the file changed while it was being edited. Through a
/// symlink to the file it names.
fn edit_text(path: &Path, mut edit: impl FnMut(Option<&str>) -> Result<Option<String>, String>) -> Result<bool, String> {
    let _serial = crate::util::json_file::write_lock();
    let target = match fs::canonicalize(path) {
        Ok(t) => t,
        // A link whose target is gone (a dotfiles checkout not there yet): writing would put a
        // plain file where the user's link was, and Claude would stop reading their real one.
        Err(_) if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) => {
            return Err(format!("{} is a link to a file that isn't there. maiTerm won't replace the link.", path.display()));
        }
        Err(_) => path.to_path_buf(),
    };
    for _ in 0..5 {
        let before = crate::util::json_file::stamp(&target);
        let raw = match fs::read_to_string(&target) {
            Ok(r) => Some(r),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("Cannot read {}: {e}", target.display())),
        };
        let Some(out) = edit(raw.as_deref())? else { return Ok(false) };
        if let Some(dir) = target.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
        }
        if crate::util::json_file::replace_if_unchanged(&target, before, out.as_bytes())
            .map_err(|e| format!("Cannot write {}: {e}", target.display()))?
        {
            return Ok(true);
        }
    }
    Err(format!("{} kept changing while being updated", target.display()))
}

/// A JSON settings object, `{}` for no file or an empty one. An unparseable or non-object file
/// is an error, never `{}`: every write is read-modify-write, and treating it as empty would
/// replace the user's whole file.
fn parse_json(path: &Path, raw: Option<&str>) -> Result<Value, String> {
    let Some(raw) = raw.filter(|r| !r.trim().is_empty()) else { return Ok(json!({})) };
    let v: Value = serde_json::from_str(raw).map_err(|e| {
        format!("{} could not be parsed ({e}). maiTerm won't overwrite settings it can't read.", path.display())
    })?;
    if !v.is_object() {
        return Err(format!("{} is not a JSON object. maiTerm won't overwrite it.", path.display()));
    }
    Ok(v)
}

fn parse_toml(path: &Path, raw: Option<&str>) -> Result<toml_edit::DocumentMut, String> {
    raw.unwrap_or("").parse::<toml_edit::DocumentMut>().map_err(|e| {
        format!("{} could not be parsed ({e}). maiTerm won't overwrite settings it can't read.", path.display())
    })
}

fn read_json(path: &Path) -> Result<Value, String> {
    match fs::read_to_string(path) {
        Ok(r) => parse_json(path, Some(&r)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(json!({})),
        Err(e) => Err(format!("Cannot read {}: {e}", path.display())),
    }
}

fn edit_json(path: &Path, mut f: impl FnMut(&mut Value) -> Result<bool, String>) -> Result<bool, String> {
    edit_text(path, |raw| {
        let mut v = parse_json(path, raw)?;
        if !f(&mut v)? {
            return Ok(None);
        }
        serde_json::to_string_pretty(&v).map(|s| Some(s + "\n")).map_err(|e| e.to_string())
    })
}

/// `obj[key]`, removing it when `value` is None. True when that changed anything.
fn put(obj: &mut Value, key: &str, value: Option<Value>) -> bool {
    let Some(map) = obj.as_object_mut() else { return false };
    match value {
        Some(v) if map.get(key) == Some(&v) => false,
        Some(v) => {
            map.insert(key.to_string(), v);
            true
        }
        None => map.remove(key).is_some(),
    }
}

/// A setting as the UI shows it: the on-disk value as text (None: absent, the agent's
/// default). A value of a type we don't write (a number where a name belongs) reads as its
/// JSON text, so the UI shows it rather than a blank.
fn shown(v: Option<&Value>) -> Option<String> {
    match v? {
        Value::String(s) => Some(s.clone()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

// ─── Status ───────────────────────────────────────────────────────────────

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct EffortRow {
    pub model_id: String,
    /// "Opus" etc. for the models offered; None for another entry already in the file, which
    /// is shown read-only.
    pub label: Option<String>,
    pub level: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct ClaudePart {
    pub installed: bool,
    pub error: Option<String>,
    pub advisor: Option<String>,
    /// The advisor's flag bypass is set: without it, while telemetry is off, the advisor is
    /// silently ignored.
    pub advisor_switch: bool,
    /// Something in `env` stops Claude fetching feature flags (`DISABLE_TELEMETRY`…).
    pub flags_off: bool,
    pub model: Option<String>,
    pub effort: Vec<EffortRow>,
    pub retention_days: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct CodexPart {
    pub installed: bool,
    pub error: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub reviewer: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct AgyPart {
    pub installed: bool,
    pub error: Option<String>,
    pub agent_mode: Option<String>,
    pub show_thinking: Option<String>,
    pub queued_messages: Option<String>,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Status {
    pub claude: ClaudePart,
    pub codex: CodexPart,
    pub agy: AgyPart,
}

fn dir_exists(file: &Path) -> bool {
    file.parent().is_some_and(Path::is_dir)
}

pub fn status(p: &Paths) -> Status {
    Status { claude: claude_status(&p.claude), codex: codex_status(&p.codex), agy: agy_status(&p.agy) }
}

/// `env[key]` as the string Claude puts in the process environment (None: absent or empty).
fn env_text(settings: &Value, key: &str) -> Option<String> {
    let t = match settings.get("env")?.get(key)? {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => return None,
    };
    (!t.is_empty()).then_some(t)
}

/// On the way Claude 2.1.330 reads a switch like the advisor's or `DO_NOT_TRACK`: "1", "true",
/// "yes" or "on", any case.
fn env_on(settings: &Value, key: &str) -> bool {
    env_text(settings, key).is_some_and(|t| ["1", "true", "yes", "on"].contains(&t.trim().to_ascii_lowercase().as_str()))
}

/// Claude stops fetching feature flags when `DISABLE_TELEMETRY` or
/// `CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC` is set to ANYTHING ("0" included), or
/// `DO_NOT_TRACK` is on.
fn flags_off(settings: &Value) -> bool {
    env_text(settings, "DISABLE_TELEMETRY").is_some()
        || env_text(settings, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").is_some()
        || env_on(settings, "DO_NOT_TRACK")
}

fn claude_status(path: &Path) -> ClaudePart {
    let mut part = ClaudePart { installed: dir_exists(path), ..Default::default() };
    let s = match read_json(path) {
        Ok(s) => s,
        Err(e) => {
            part.error = Some(e);
            return part;
        }
    };
    part.advisor = shown(s.get("advisorModel")).filter(|a| !a.is_empty());
    part.advisor_switch = env_on(&s, ADVISOR_TOOL_ENV);
    part.flags_off = flags_off(&s);
    part.model = shown(s.get("model"));
    part.retention_days = shown(s.get("cleanupPeriodDays"));
    let table = s.get("modelSettings").and_then(Value::as_object);
    let level = |id: &str| table.and_then(|t| t.get(id)).and_then(|e| shown(e.get("effortLevel")));
    for (id, label) in CLAUDE_EFFORT_MODELS {
        part.effort.push(EffortRow { model_id: id.to_string(), label: Some(label.to_string()), level: level(id) });
    }
    if let Some(t) = table {
        for id in t.keys() {
            if CLAUDE_EFFORT_MODELS.iter().any(|(k, _)| k == id) {
                continue;
            }
            if let Some(l) = level(id) {
                part.effort.push(EffortRow { model_id: id.clone(), label: None, level: Some(l) });
            }
        }
    }
    part
}

fn codex_status(path: &Path) -> CodexPart {
    let mut part = CodexPart { installed: dir_exists(path), ..Default::default() };
    let raw = match fs::read_to_string(path) {
        Ok(r) => Some(r),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            part.error = Some(format!("Cannot read {}: {e}", path.display()));
            return part;
        }
    };
    let doc = match parse_toml(path, raw.as_deref()) {
        Ok(d) => d,
        Err(e) => {
            part.error = Some(e);
            return part;
        }
    };
    let get = |k: &str| {
        doc.get(k).and_then(|i| i.as_value()).map(|v| match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string().trim().to_string(),
        })
    };
    part.model = get("model");
    part.effort = get("model_reasoning_effort");
    part.reviewer = get("approvals_reviewer");
    part
}

fn agy_status(path: &Path) -> AgyPart {
    let mut part = AgyPart { installed: dir_exists(path), ..Default::default() };
    match read_json(path) {
        Ok(s) => {
            part.agent_mode = shown(s.get("agentMode"));
            part.show_thinking = shown(s.get("showThinking"));
            part.queued_messages = shown(s.get("queuedMessages"));
        }
        Err(e) => part.error = Some(e),
    }
    part
}

// ─── Writes ───────────────────────────────────────────────────────────────

/// Which agent a setting belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    Claude,
    Codex,
    Agy,
}

fn one_of(value: &str, allowed: &[&str], what: &str) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("{value:?} is not a {what} maiTerm offers"))
    }
}

/// Set one setting, or clear it with `value` None ("the agent's default"). Every value is
/// checked against what the agent accepts before anything is written.
///
/// Keys: Claude `advisor`, `model`, `effort:<model id>`, `retention`; Codex `model`, `effort`,
/// `reviewer`; Antigravity `agent_mode`, `show_thinking`, `queued_messages`.
pub fn set(p: &Paths, runtime: Runtime, key: &str, value: Option<&str>) -> Result<bool, String> {
    match runtime {
        Runtime::Claude => set_claude(&p.claude, key, value),
        Runtime::Codex => set_codex(&p.codex, key, value),
        Runtime::Agy => set_agy(&p.agy, key, value),
    }
}

fn set_claude(path: &Path, key: &str, value: Option<&str>) -> Result<bool, String> {
    if key == "advisor" {
        if let Some(v) = value {
            one_of(v, CLAUDE_ADVISORS, "Claude advisor")?;
        }
        // The model and its flag bypass go together, in one write: an advisor without the
        // bypass is silently ignored while telemetry is off.
        return edit_json(path, |s| {
            let mut changed = put(s, "advisorModel", value.map(|v| json!(v)));
            if value.is_some() {
                if !s.get("env").is_some_and(Value::is_object) {
                    s["env"] = json!({});
                }
                changed |= put(&mut s["env"], ADVISOR_TOOL_ENV, Some(json!("1")));
            } else if let Some(env) = s.get_mut("env") {
                changed |= put(env, ADVISOR_TOOL_ENV, None);
            }
            Ok(changed)
        });
    }
    if key == "model" {
        if let Some(v) = value {
            one_of(v, CLAUDE_MODELS, "Claude model")?;
        }
        return edit_json(path, |s| Ok(put(s, "model", value.map(|v| json!(v)))));
    }
    if key == "retention" {
        let days = value
            .map(|v| {
                v.trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|d| (1..=RETENTION_MAX_DAYS).contains(d))
                    .ok_or_else(|| format!("Keep transcripts takes a whole number of days from 1 to {RETENTION_MAX_DAYS}"))
            })
            .transpose()?;
        return edit_json(path, |s| Ok(put(s, "cleanupPeriodDays", days.map(|d| json!(d)))));
    }
    if let Some(model_id) = key.strip_prefix("effort:") {
        if !CLAUDE_EFFORT_MODELS.iter().any(|(id, _)| *id == model_id) {
            return Err(format!("{model_id:?} is not a model maiTerm sets effort for"));
        }
        if let Some(v) = value {
            one_of(v, CLAUDE_EFFORTS, "Claude effort level")?;
        }
        return edit_json(path, |s| {
            if let Some(v) = value {
                if !s.get("modelSettings").is_some_and(Value::is_object) {
                    s["modelSettings"] = json!({});
                }
                let table = &mut s["modelSettings"];
                if !table.get(model_id).is_some_and(Value::is_object) {
                    table[model_id] = json!({});
                }
                return Ok(put(&mut table[model_id], "effortLevel", Some(json!(v))));
            }
            let Some(table) = s.get_mut("modelSettings").and_then(Value::as_object_mut) else { return Ok(false) };
            let Some(entry) = table.get_mut(model_id) else { return Ok(false) };
            let changed = put(entry, "effortLevel", None);
            // The entry goes too once nothing else is in it; any other per-model setting stays.
            if changed && entry.as_object().is_some_and(|m| m.is_empty()) {
                table.remove(model_id);
            }
            Ok(changed)
        });
    }
    Err(format!("Unknown Claude setting {key:?}"))
}

/// A Codex model name: free text (its names change too often for a list), but one TOML string
/// of plain model-name characters.
fn valid_codex_model(v: &str) -> bool {
    !v.is_empty() && v.len() <= 100 && v.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c))
}

fn set_codex(path: &Path, key: &str, value: Option<&str>) -> Result<bool, String> {
    let toml_key = match key {
        "model" => {
            if let Some(v) = value {
                if !valid_codex_model(v) {
                    return Err(format!("{v:?} is not a Codex model name"));
                }
            }
            "model"
        }
        "effort" => {
            if let Some(v) = value {
                one_of(v, CODEX_EFFORTS, "Codex reasoning effort")?;
            }
            "model_reasoning_effort"
        }
        "reviewer" => {
            if let Some(v) = value {
                one_of(v, CODEX_REVIEWERS, "Codex approvals reviewer")?;
            }
            "approvals_reviewer"
        }
        _ => return Err(format!("Unknown Codex setting {key:?}")),
    };
    edit_text(path, |raw| {
        let mut doc = parse_toml(path, raw)?;
        let current = doc.get(toml_key).and_then(|i| i.as_str()).map(str::to_string);
        match value {
            Some(v) if current.as_deref() == Some(v) => return Ok(None),
            // An existing key keeps its place and its comments; a new one goes in as a
            // top-level key (`insert` on the root table puts it before the first [table],
            // where Codex reads it).
            Some(v) => match doc.get_mut(toml_key).and_then(|i| i.as_value_mut()) {
                Some(old) => {
                    let decor = old.decor().clone();
                    *old = v.into();
                    *old.decor_mut() = decor;
                }
                None => {
                    doc.insert(toml_key, toml_edit::value(v));
                }
            },
            None => {
                if doc.remove(toml_key).is_none() {
                    return Ok(None);
                }
            }
        }
        Ok(Some(doc.to_string()))
    })
}

fn set_agy(path: &Path, key: &str, value: Option<&str>) -> Result<bool, String> {
    let (json_key, v) = match key {
        "agent_mode" => {
            if let Some(v) = value {
                one_of(v, AGY_MODES, "Antigravity agent mode")?;
            }
            ("agentMode", value.map(|v| json!(v)))
        }
        "show_thinking" => {
            let b = value
                .map(|v| match v {
                    "true" => Ok(true),
                    "false" => Ok(false),
                    _ => Err(format!("{v:?} is not on or off")),
                })
                .transpose()?;
            ("showThinking", b.map(|b| json!(b)))
        }
        "queued_messages" => {
            if let Some(v) = value {
                one_of(v, AGY_QUEUED, "Antigravity queued-messages choice")?;
            }
            ("queuedMessages", value.map(|v| json!(v)))
        }
        _ => return Err(format!("Unknown Antigravity setting {key:?}")),
    };
    edit_json(path, |s| Ok(put(s, json_key, v.clone())))
}

// ─── The one default maiTerm applies itself ───────────────────────────────

/// Settled per FILE: a user who later points their tabs at another Claude root gets it there too.
fn seed_retention_id(claude: &Path) -> String {
    format!("claude_retention {}", claude.display())
}

fn seeded(path: &Path) -> Vec<String> {
    fs::read_to_string(path)
        .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect())
        .unwrap_or_default()
}

fn mark_seeded(path: &Path, id: &str) -> Result<(), String> {
    let mut ids = seeded(path);
    if ids.iter().any(|s| s == id) {
        return Ok(());
    }
    ids.push(id.to_string());
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
    }
    fs::write(path, ids.join("\n") + "\n").map_err(|e| format!("Cannot write {}: {e}", path.display()))
}

/// Keep Claude transcripts `RETENTION_SEED_DAYS` days instead of Claude's 30, once per user
/// account, only where nothing sets `cleanupPeriodDays`: a value already there is the user's
/// choice. Once settled (seeded, or a value seen), it is never applied again, so a change in
/// either direction sticks. An unreadable file is skipped and tried again next launch.
///
/// Only the human's own settings: a Hive worker reads no user settings and so runs no
/// transcript sweep at all; giving it a period would start one over the shared `projects/`.
pub fn seed_retention(p: &Paths) {
    // No Claude here (yet): nothing to seed, and no `~/.claude` of ours. Tried next launch.
    let id = seed_retention_id(&p.claude);
    if seeded(&p.seeded).contains(&id) || !dir_exists(&p.claude) {
        return;
    }
    let result = edit_json(&p.claude, |s| {
        if s.get("cleanupPeriodDays").is_some() {
            return Ok(false);
        }
        Ok(put(s, "cleanupPeriodDays", Some(json!(RETENTION_SEED_DAYS))))
    });
    match result {
        Ok(wrote) => {
            if wrote {
                log::info!("Agent Defaults: Claude keeps transcripts {RETENTION_SEED_DAYS} days (was its default {CLAUDE_RETENTION_DEFAULT_DAYS})");
            }
            if let Err(e) = mark_seeded(&p.seeded, &id) {
                log::warn!("Agent Defaults: could not record the retention default as settled: {e}");
            }
        }
        Err(e) => log::warn!("Agent Defaults: retention default not applied: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> (PathBuf, Paths) {
        let home = std::env::temp_dir().join(format!("agent-defaults-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&home).unwrap();
        let p = Paths::under(&home);
        (home, p)
    }

    fn write(path: &Path, s: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, s).unwrap();
    }

    fn json_at(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    #[test]
    fn the_advisor_and_its_flag_bypass_are_set_and_cleared_together() {
        let (_h, p) = scratch();
        write(&p.claude, r#"{"env":{"DISABLE_TELEMETRY":"1"},"hooks":{"x":1}}"#);
        assert!(set(&p, Runtime::Claude, "advisor", Some("fable")).unwrap());
        let s = json_at(&p.claude);
        assert_eq!(s["advisorModel"], "fable");
        assert_eq!(s["env"][ADVISOR_TOOL_ENV], "1");
        assert_eq!(s["env"]["DISABLE_TELEMETRY"], "1", "Deshittify's keys are kept");
        assert_eq!(s["hooks"]["x"], 1);
        let st = status(&p).claude;
        assert_eq!(st.advisor.as_deref(), Some("fable"));
        assert!(st.advisor_switch && st.flags_off);

        assert!(set(&p, Runtime::Claude, "advisor", None).unwrap());
        let s = json_at(&p.claude);
        assert!(s.get("advisorModel").is_none());
        assert!(s["env"].get(ADVISOR_TOOL_ENV).is_none());
        assert_eq!(s["env"]["DISABLE_TELEMETRY"], "1");
        assert!(!set(&p, Runtime::Claude, "advisor", None).unwrap(), "clearing twice changes nothing");
    }

    #[test]
    fn values_outside_the_offered_lists_are_refused_before_any_write() {
        let (_h, p) = scratch();
        write(&p.claude, "{}");
        assert!(set(&p, Runtime::Claude, "advisor", Some("haiku")).is_err());
        assert!(set(&p, Runtime::Claude, "model", Some("gpt")).is_err());
        assert!(set(&p, Runtime::Claude, "effort:claude-opus-5-5", Some("max")).is_err());
        assert!(set(&p, Runtime::Claude, "effort:claude-opus-4", Some("high")).is_err());
        for bad in ["0", "5x", "-3", "40000", ""] {
            assert!(set(&p, Runtime::Claude, "retention", Some(bad)).is_err(), "{bad}");
        }
        assert!(set(&p, Runtime::Codex, "reviewer", Some("bogus")).is_err());
        assert!(set(&p, Runtime::Codex, "effort", Some("ultra")).is_err());
        assert!(set(&p, Runtime::Codex, "model", Some("gpt\"\n[x]")).is_err());
        assert!(set(&p, Runtime::Agy, "agent_mode", Some("yolo")).is_err());
        assert!(set(&p, Runtime::Agy, "show_thinking", Some("1")).is_err());
        assert_eq!(fs::read_to_string(&p.claude).unwrap(), "{}");
        assert!(!p.codex.exists() && !p.agy.exists());
    }

    #[test]
    fn effort_is_written_per_model_id_and_an_emptied_entry_goes() {
        let (_h, p) = scratch();
        write(&p.claude, r#"{"modelSettings":{"claude-opus-5":{"effortLevel":"high"},"claude-sonnet-5-5":{"effortLevel":"low","other":true}}}"#);
        assert!(set(&p, Runtime::Claude, "effort:claude-opus-5-5", Some("xhigh")).unwrap());
        assert_eq!(json_at(&p.claude)["modelSettings"]["claude-opus-5-5"]["effortLevel"], "xhigh");

        let rows = status(&p).claude.effort;
        assert_eq!(rows[0].level.as_deref(), Some("xhigh"));
        let other = rows.iter().find(|r| r.model_id == "claude-opus-5").unwrap();
        assert_eq!((other.label.as_deref(), other.level.as_deref()), (None, Some("high")), "shown, read-only");

        assert!(set(&p, Runtime::Claude, "effort:claude-opus-5-5", None).unwrap());
        assert!(json_at(&p.claude)["modelSettings"].get("claude-opus-5-5").is_none());
        assert!(set(&p, Runtime::Claude, "effort:claude-sonnet-5-5", None).unwrap());
        assert_eq!(json_at(&p.claude)["modelSettings"]["claude-sonnet-5-5"], json!({"other": true}));
        assert_eq!(json_at(&p.claude)["modelSettings"]["claude-opus-5"]["effortLevel"], "high");
    }

    #[test]
    fn an_unreadable_file_blocks_its_part_and_is_never_overwritten() {
        let (_h, p) = scratch();
        write(&p.claude, "{ not json");
        write(&p.codex, "model = [unclosed");
        assert!(status(&p).claude.error.is_some());
        assert!(status(&p).codex.error.is_some());
        assert!(set(&p, Runtime::Claude, "model", Some("opus")).is_err());
        assert!(set(&p, Runtime::Codex, "model", Some("gpt-6")).is_err());
        assert_eq!(fs::read_to_string(&p.claude).unwrap(), "{ not json");
        assert_eq!(fs::read_to_string(&p.codex).unwrap(), "model = [unclosed");
        seed_retention(&p);
        assert_eq!(fs::read_to_string(&p.claude).unwrap(), "{ not json");
        assert!(seeded(&p.seeded).is_empty(), "retried once the file reads again");
    }

    #[test]
    fn codex_keys_go_at_the_top_level_and_the_rest_of_the_file_is_kept() {
        let (_h, p) = scratch();
        let before = "# mine\nmodel = \"gpt-6\"  # pinned\n\n[features]\nimage_generation = true\n";
        write(&p.codex, before);
        assert!(set(&p, Runtime::Codex, "model", Some("gpt-7")).unwrap());
        assert!(fs::read_to_string(&p.codex).unwrap().contains("model = \"gpt-7\"  # pinned\n"), "the comment stays");
        write(&p.codex, before);
        assert!(set(&p, Runtime::Codex, "effort", Some("high")).unwrap());
        assert!(set(&p, Runtime::Codex, "reviewer", Some("auto_review")).unwrap());
        let text = fs::read_to_string(&p.codex).unwrap();
        assert!(text.starts_with("# mine\n"), "{text}");
        assert!(text.contains("[features]\nimage_generation = true"));
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["model_reasoning_effort"].as_str(), Some("high"));
        assert_eq!(doc["approvals_reviewer"].as_str(), Some("auto_review"));
        assert!(doc["features"].get("model_reasoning_effort").is_none());
        let st = status(&p).codex;
        assert_eq!((st.model.as_deref(), st.effort.as_deref()), (Some("gpt-6"), Some("high")));

        assert!(set(&p, Runtime::Codex, "model", None).unwrap());
        assert!(!fs::read_to_string(&p.codex).unwrap().contains("gpt-6"));
        assert!(!set(&p, Runtime::Codex, "model", None).unwrap());
    }

    #[test]
    fn a_missing_file_is_created_and_a_symlink_stays_a_link() {
        let (home, p) = scratch();
        assert!(set(&p, Runtime::Agy, "agent_mode", Some("")).unwrap());
        assert_eq!(json_at(&p.agy)["agentMode"], "", "\"\" (Default) is a saved choice, not absent");
        assert_eq!(status(&p).agy.agent_mode.as_deref(), Some(""));

        #[cfg(unix)]
        {
            let real = home.join("dotfiles").join("settings.json");
            write(&real, r#"{"a":1}"#);
            fs::create_dir_all(p.claude.parent().unwrap()).unwrap();
            std::os::unix::fs::symlink(&real, &p.claude).unwrap();
            assert!(set(&p, Runtime::Claude, "model", Some("opus")).unwrap());
            assert!(fs::symlink_metadata(&p.claude).unwrap().file_type().is_symlink());
            assert_eq!(json_at(&real)["model"], "opus");
            assert_eq!(json_at(&real)["a"], 1);
        }
    }

    #[test]
    fn claude_settings_are_the_ones_the_humans_tabs_read() {
        let home = Path::new("/h");
        let own = Path::new("/own");
        assert_eq!(claude_settings_in(home, false, None), Path::new("/h/.claude/settings.json"));
        assert_eq!(claude_settings_in(home, false, Some(own)), Path::new("/own/settings.json"));
        // A managed account's root symlinks ~/.claude's, whatever the user exported.
        assert_eq!(claude_settings_in(home, true, Some(own)), Path::new("/h/.claude/settings.json"));
    }

    #[test]
    fn switches_read_on_and_off_as_claude_reads_them() {
        let s = |env: Value| json!({ "env": env });
        assert!(!env_on(&s(json!({ ADVISOR_TOOL_ENV: "off" })), ADVISOR_TOOL_ENV));
        assert!(!env_on(&s(json!({ ADVISOR_TOOL_ENV: 2 })), ADVISOR_TOOL_ENV));
        assert!(env_on(&s(json!({ ADVISOR_TOOL_ENV: "Yes" })), ADVISOR_TOOL_ENV));
        assert!(env_on(&s(json!({ ADVISOR_TOOL_ENV: 1 })), ADVISOR_TOOL_ENV));
        assert!(flags_off(&s(json!({"DISABLE_TELEMETRY": "0"}))), "set at all is set");
        assert!(!flags_off(&s(json!({"DISABLE_TELEMETRY": ""}))));
        assert!(!flags_off(&s(json!({"DO_NOT_TRACK": "0"}))));
        assert!(flags_off(&s(json!({"DO_NOT_TRACK": "true"}))));
    }

    #[cfg(unix)]
    #[test]
    fn a_link_to_a_missing_file_is_never_replaced() {
        let (home, p) = scratch();
        fs::create_dir_all(p.claude.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(home.join("dotfiles/settings.json"), &p.claude).unwrap();
        assert!(set(&p, Runtime::Claude, "model", Some("opus")).is_err());
        seed_retention(&p);
        assert!(fs::symlink_metadata(&p.claude).unwrap().file_type().is_symlink());
        assert!(seeded(&p.seeded).is_empty());
    }

    #[test]
    fn retention_is_seeded_once_only_where_nothing_sets_it() {
        let (_h, p) = scratch();
        write(&p.claude, r#"{"cleanupPeriodDays":548}"#);
        seed_retention(&p);
        assert_eq!(json_at(&p.claude)["cleanupPeriodDays"], 548, "the user's own value stays");
        assert_eq!(seeded(&p.seeded), vec![seed_retention_id(&p.claude)]);

        let (_h2, q) = scratch();
        write(&q.claude, r#"{"model":"opus"}"#);
        seed_retention(&q);
        assert_eq!(json_at(&q.claude)["cleanupPeriodDays"], RETENTION_SEED_DAYS);
        // The user clears it back to Claude's default: that sticks.
        set(&q, Runtime::Claude, "retention", None).unwrap();
        seed_retention(&q);
        assert!(json_at(&q.claude).get("cleanupPeriodDays").is_none());
    }
}
