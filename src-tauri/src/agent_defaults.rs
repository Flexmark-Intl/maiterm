//! Agent Defaults (docs/agent-defaults.md): the values every new session of an agent starts
//! with, written into that agent's OWN user settings file. The sister of Deshittify
//! (`commands/deshittify.rs`), with its mechanics:
//!
//! - **Disk is the state** of each value here; `status` reads the files. maiTerm keeps only
//!   WHICH settings the human ticked and the value each was given (`Managed`), because those
//!   are what SSH hosts receive (`remote_script`). An unticked setting is never changed.
//! - **Absent is "the agent's default".** Clearing a value removes its key, never writes the
//!   runtime's current default, so a later upstream change of default reaches the user.
//! - **Only our keys are touched**, everything else is kept (TOML through `toml_edit`).
//! - **An unreadable file is never overwritten**: its runtime's part shows the error instead.
//! - **Writes take maiTerm's config write lock** (`util::json_file::edit_text`, shared by every
//!   maiTerm writer of these files) and are redone when the file changed under them: live
//!   agents rewrite these files too.
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
/// Codex `[tui] fullscreen_transcript`: true (Codex's default) runs the TUI on the alternate
/// screen; false is its "Scrollback" mode, inline in the terminal's own history, whatever
/// `alternate_screen` says (probed against codex-cli 0.172.0, 2026-11-26).
const CODEX_FULLSCREEN: &[&str] = &["tui", "fullscreen_transcript"];

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
    /// Which settings maiTerm manages (`Managed`).
    pub managed: PathBuf,
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
            managed: home.join(".maiterm").join("agent-defaults.json"),
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

/// Read-edit-replace `path` under maiTerm's config write lock, creating it when absent
/// (`util::json_file::edit_text`, shared by every maiTerm writer of these files).
fn edit_text(path: &Path, edit: impl FnMut(Option<&str>) -> Result<Option<String>, String>) -> Result<bool, String> {
    crate::util::json_file::edit_text(path, true, edit)
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
    /// `[tui] fullscreen_transcript`, "true"/"false".
    pub fullscreen: Option<String>,
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
    /// The section is on: maiTerm changes the settings in `managed`, here and on SSH hosts.
    pub enabled: bool,
    /// `claude.advisor`… → the value maiTerm gives every machine, as the UI shows it (None: the
    /// agent's default). A setting not here is never changed by maiTerm.
    pub managed: std::collections::BTreeMap<String, Option<String>>,
    /// The record can't be read: nothing is changed anywhere until it can.
    pub managed_error: Option<String>,
}

fn dir_exists(file: &Path) -> bool {
    file.parent().is_some_and(Path::is_dir)
}

pub fn status(p: &Paths) -> Status {
    let (m, managed_error) = match load_managed(&p.managed) {
        Ok(m) => (m, None),
        Err(e) => (Managed { enabled: false, keys: Default::default() }, Some(e)),
    };
    Status {
        claude: claude_status(&p.claude),
        codex: codex_status(&p.codex),
        agy: agy_status(&p.agy),
        enabled: m.enabled,
        managed: m.keys.iter().map(|(k, v)| (k.clone(), shown(Some(v)))).collect(),
        managed_error,
    }
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
/// `DO_NOT_TRACK`, `CLAUDE_CODE_DISABLE_FEATURE_FLAGS` or `DISABLE_GROWTHBOOK` is on (2.1.330's
/// feature-availability check names each as the reason).
fn flags_off(settings: &Value) -> bool {
    env_text(settings, "DISABLE_TELEMETRY").is_some()
        || env_text(settings, "CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC").is_some()
        || ["DO_NOT_TRACK", "CLAUDE_CODE_DISABLE_FEATURE_FLAGS", "DISABLE_GROWTHBOOK"].iter().any(|k| env_on(settings, k))
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
    let get = |keys: &[&str]| {
        codex_value(&doc, keys).map(|v| match v.as_str() {
            Some(s) => s.to_string(),
            None => v.to_string().trim().to_string(),
        })
    };
    part.model = get(&["model"]);
    part.effort = get(&["model_reasoning_effort"]);
    part.reviewer = get(&["approvals_reviewer"]);
    part.fullscreen = get(CODEX_FULLSCREEN);
    part
}

/// The value at `keys` (a top-level key, or one in a table) in a Codex config.
fn codex_value<'a>(doc: &'a toml_edit::DocumentMut, keys: &[&str]) -> Option<&'a toml_edit::Value> {
    let (leaf, tables) = keys.split_last()?;
    let mut table: &dyn toml_edit::TableLike = doc.as_table();
    for k in tables {
        table = table.get(k)?.as_table_like()?;
    }
    table.get(leaf)?.as_value()
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

impl Runtime {
    /// As the managed record names it (`claude.advisor`) and the wire spells it.
    fn id(self) -> &'static str {
        match self {
            Runtime::Claude => "claude",
            Runtime::Codex => "codex",
            Runtime::Agy => "agy",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Runtime::Claude => "Claude",
            Runtime::Codex => "Codex",
            Runtime::Agy => "Antigravity",
        }
    }
}

fn on_off(v: &str) -> Result<Value, String> {
    match v {
        "true" => Ok(json!(true)),
        "false" => Ok(json!(false)),
        _ => Err(format!("{v:?} is not on or off")),
    }
}

fn one_of(value: &str, allowed: &[&str], what: &str) -> Result<(), String> {
    if allowed.contains(&value) {
        Ok(())
    } else {
        Err(format!("{value:?} is not a {what} maiTerm offers"))
    }
}

/// A setting's value as the agent's file holds it (a string, a number of days, a bool), checked
/// against what maiTerm offers. None: "the agent's default", the key absent. An unknown key is
/// an error either way.
///
/// Keys: Claude `advisor`, `model`, `effort:<model id>`, `retention`; Codex `model`, `effort`,
/// `reviewer`, `fullscreen`; Antigravity `agent_mode`, `show_thinking`, `queued_messages`.
pub fn typed(runtime: Runtime, key: &str, value: Option<&str>) -> Result<Option<Value>, String> {
    match (runtime, key) {
        (Runtime::Claude, "advisor") => value.map(|v| one_of(v, CLAUDE_ADVISORS, "Claude advisor").map(|_| json!(v))).transpose(),
        (Runtime::Claude, "model") => value.map(|v| one_of(v, CLAUDE_MODELS, "Claude model").map(|_| json!(v))).transpose(),
        (Runtime::Claude, "retention") => value
            .map(|v| {
                v.trim()
                    .parse::<u64>()
                    .ok()
                    .filter(|d| (1..=RETENTION_MAX_DAYS).contains(d))
                    .map(|d| json!(d))
                    .ok_or_else(|| format!("Keep transcripts takes a whole number of days from 1 to {RETENTION_MAX_DAYS}"))
            })
            .transpose(),
        (Runtime::Claude, k) if k.starts_with("effort:") => {
            let model_id = &k["effort:".len()..];
            if !CLAUDE_EFFORT_MODELS.iter().any(|(id, _)| *id == model_id) {
                return Err(format!("{model_id:?} is not a model maiTerm sets effort for"));
            }
            value.map(|v| one_of(v, CLAUDE_EFFORTS, "Claude effort level").map(|_| json!(v))).transpose()
        }
        (Runtime::Codex, "model") => value
            .map(|v| if valid_codex_model(v) { Ok(json!(v)) } else { Err(format!("{v:?} is not a Codex model name")) })
            .transpose(),
        (Runtime::Codex, "effort") => value.map(|v| one_of(v, CODEX_EFFORTS, "Codex reasoning effort").map(|_| json!(v))).transpose(),
        (Runtime::Codex, "reviewer") => value.map(|v| one_of(v, CODEX_REVIEWERS, "Codex approvals reviewer").map(|_| json!(v))).transpose(),
        (Runtime::Codex, "fullscreen") => value.map(on_off).transpose(),
        (Runtime::Agy, "agent_mode") => value.map(|v| one_of(v, AGY_MODES, "Antigravity agent mode").map(|_| json!(v))).transpose(),
        (Runtime::Agy, "show_thinking") => value.map(on_off).transpose(),
        (Runtime::Agy, "queued_messages") => {
            value.map(|v| one_of(v, AGY_QUEUED, "Antigravity queued-messages choice").map(|_| json!(v))).transpose()
        }
        (rt, k) => Err(format!("Unknown {} setting {k:?}", rt.name())),
    }
}

/// Set one setting in this machine's file, or clear it with `value` None ("the agent's
/// default"). Every value is checked (`typed`) before anything is written. Whether the human
/// lets maiTerm change it at all is `set_managed`'s question, not this one's.
pub fn set(p: &Paths, runtime: Runtime, key: &str, value: Option<&str>) -> Result<bool, String> {
    let v = typed(runtime, key, value)?;
    match runtime {
        Runtime::Claude => set_claude(&p.claude, key, v),
        Runtime::Codex => set_codex(&p.codex, codex_key(key)?, v),
        Runtime::Agy => edit_json(&p.agy, |s| Ok(put(s, agy_key(key)?, v.clone()))),
    }
}

/// The JSON path a Claude setting is written at (`advisor` also writes its flag bypass).
fn claude_path(key: &str) -> Result<Vec<&str>, String> {
    Ok(match key {
        "advisor" => vec!["advisorModel"],
        "model" => vec!["model"],
        "retention" => vec!["cleanupPeriodDays"],
        k => match k.strip_prefix("effort:") {
            Some(id) => vec!["modelSettings", id, "effortLevel"],
            None => return Err(format!("Unknown Claude setting {key:?}")),
        },
    })
}

/// Where a Codex setting lives in `config.toml`: a top-level key, or a table and its key.
fn codex_key(key: &str) -> Result<&'static [&'static str], String> {
    Ok(match key {
        "model" => &["model"],
        "effort" => &["model_reasoning_effort"],
        "reviewer" => &["approvals_reviewer"],
        "fullscreen" => CODEX_FULLSCREEN,
        _ => return Err(format!("Unknown Codex setting {key:?}")),
    })
}

fn agy_key(key: &str) -> Result<&'static str, String> {
    Ok(match key {
        "agent_mode" => "agentMode",
        "show_thinking" => "showThinking",
        "queued_messages" => "queuedMessages",
        _ => return Err(format!("Unknown Antigravity setting {key:?}")),
    })
}

fn set_claude(path: &Path, key: &str, value: Option<Value>) -> Result<bool, String> {
    if key == "advisor" {
        // The model and its flag bypass go together, in one write: an advisor without the
        // bypass is silently ignored while telemetry is off.
        return edit_json(path, |s| {
            let mut changed = put(s, "advisorModel", value.clone());
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
    if let Some(model_id) = key.strip_prefix("effort:") {
        return edit_json(path, |s| {
            if let Some(v) = &value {
                if !s.get("modelSettings").is_some_and(Value::is_object) {
                    s["modelSettings"] = json!({});
                }
                let table = &mut s["modelSettings"];
                if !table.get(model_id).is_some_and(Value::is_object) {
                    table[model_id] = json!({});
                }
                return Ok(put(&mut table[model_id], "effortLevel", Some(v.clone())));
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
    let field = claude_path(key)?[0];
    edit_json(path, |s| Ok(put(s, field, value.clone())))
}

/// A Codex model name: free text (its names change too often for a list), but one TOML string
/// of plain model-name characters.
fn valid_codex_model(v: &str) -> bool {
    !v.is_empty() && v.len() <= 100 && v.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:/".contains(c))
}

/// A Codex value as JSON (a string or a bool, the only kinds maiTerm sets there).
fn codex_json(v: &toml_edit::Value) -> Option<Value> {
    v.as_str().map(|s| json!(s)).or_else(|| v.as_bool().map(|b| json!(b)))
}

fn set_codex(path: &Path, keys: &[&str], value: Option<Value>) -> Result<bool, String> {
    let new: Option<toml_edit::Value> = match &value {
        None => None,
        Some(Value::String(s)) => Some(s.as_str().into()),
        Some(Value::Bool(b)) => Some((*b).into()),
        Some(v) => return Err(format!("{v} can't be written to Codex's config")),
    };
    let Some((leaf, tables)) = keys.split_last() else { return Err("No Codex key".into()) };
    edit_text(path, |raw| {
        let mut doc = parse_toml(path, raw)?;
        let mut table: &mut dyn toml_edit::TableLike = doc.as_table_mut();
        for k in tables {
            if table.get(k).is_none() {
                if new.is_none() {
                    return Ok(None);
                }
                // A new table goes in as a `[k]` section at the end of the file.
                table.insert(k, toml_edit::Item::Table(toml_edit::Table::new()));
            }
            table = match table.get_mut(k).and_then(|i| i.as_table_like_mut()) {
                Some(t) => t,
                None => return Err(format!("{k} in {} is not a table", path.display())),
            };
        }
        let current = table.get(leaf).and_then(|i| i.as_value()).and_then(codex_json);
        match &new {
            Some(v) if current.is_some() && current == codex_json(v) => return Ok(None),
            // An existing key keeps its place and its comments; a new top-level one goes in
            // before the first [table] (`insert` on the root table), where Codex reads it.
            Some(v) => match table.get_mut(leaf).and_then(|i| i.as_value_mut()) {
                Some(old) => {
                    let decor = old.decor().clone();
                    *old = v.clone();
                    *old.decor_mut() = decor;
                }
                None => {
                    table.insert(leaf, toml_edit::Item::Value(v.clone()));
                }
            },
            None => {
                if table.remove(leaf).is_none() {
                    return Ok(None);
                }
            }
        }
        Ok(Some(doc.to_string()))
    })
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

/// Keep Claude transcripts `RETENTION_SEED_DAYS` days instead of Claude's 30, once per settings
/// file (a user who moves their tabs to another Claude root gets it there too), only where
/// nothing sets `cleanupPeriodDays`: a value already there is the user's
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

// ─── Which settings maiTerm manages (and carries to SSH hosts) ──────────

/// The human's choice of which settings maiTerm sets, here and on every bridged SSH host
/// (Darryl, 2026-11-22). A setting not in `keys` is never changed by maiTerm, here or there;
/// one in it is set to the RECORDED value everywhere, its removal ("the agent's default")
/// included. `enabled` off changes nothing anywhere and keeps the ticks.
///
/// Hosts get the recorded value, never whatever this machine's file holds at connect time:
/// anything that dropped a key here (a hand edit, another writer's bug) would otherwise become a
/// removal on every host, and a removed `cleanupPeriodDays` lets Claude sweep a host's
/// transcripts at its 30-day default. Kept in `~/.maiterm/` beside the seeded marker: like the
/// files it describes, it is shared by every maiTerm build.
#[derive(Serialize, serde::Deserialize, Clone, Debug, PartialEq)]
pub struct Managed {
    #[serde(default = "on")]
    pub enabled: bool,
    /// `claude.advisor`, `codex.effort`… → the value (JSON null: the agent's default).
    #[serde(default)]
    pub keys: std::collections::BTreeMap<String, Value>,
}

fn on() -> bool {
    true
}

impl Default for Managed {
    fn default() -> Self {
        Self { enabled: true, keys: Default::default() }
    }
}

fn managed_id(runtime: Runtime, key: &str) -> String {
    format!("{}.{key}", runtime.id())
}

fn parse_managed(path: &Path, raw: Option<&str>) -> Result<Managed, String> {
    match raw.filter(|r| !r.trim().is_empty()) {
        None => Ok(Managed::default()),
        Some(r) => serde_json::from_str(r).map_err(|e| format!("{} could not be parsed ({e})", path.display())),
    }
}

pub fn load_managed(path: &Path) -> Result<Managed, String> {
    match fs::read_to_string(path) {
        Ok(r) => parse_managed(path, Some(&r)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Managed::default()),
        Err(e) => Err(format!("Cannot read {}: {e}", path.display())),
    }
}

fn edit_managed(path: &Path, f: impl Fn(&mut Managed)) -> Result<(), String> {
    edit_text(path, |raw| {
        let mut m = parse_managed(path, raw)?;
        let before = m.clone();
        f(&mut m);
        if m == before {
            return Ok(None);
        }
        serde_json::to_string_pretty(&m).map(|s| Some(s + "\n")).map_err(|e| e.to_string())
    })
    .map(|_| ())
}

/// What this machine's file holds for a setting, as the value a host would be given.
fn local_value(p: &Paths, runtime: Runtime, key: &str) -> Result<Option<Value>, String> {
    match runtime {
        Runtime::Claude => {
            let s = read_json(&p.claude)?;
            let mut at = Some(&s);
            for k in claude_path(key)? {
                at = at.and_then(|v| v.get(k));
            }
            // An empty advisor is no advisor (as `claude_status` reads it).
            Ok(at.filter(|v| !v.is_null() && !(key == "advisor" && v.as_str() == Some(""))).cloned())
        }
        Runtime::Codex => {
            let raw = match fs::read_to_string(&p.codex) {
                Ok(r) => Some(r),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(format!("Cannot read {}: {e}", p.codex.display())),
            };
            let doc = parse_toml(&p.codex, raw.as_deref())?;
            let keys = codex_key(key)?;
            match codex_value(&doc, keys) {
                None => Ok(None),
                Some(v) => codex_json(v).map(Some).ok_or_else(|| format!("{} in {} is not a string or true/false", keys.join("."), p.codex.display())),
            }
        }
        Runtime::Agy => Ok(read_json(&p.agy)?.get(agy_key(key)?).filter(|v| !v.is_null()).cloned()),
    }
}

/// Whether a value found on this machine can be given to hosts as it is. Looser than `typed`,
/// which is what maiTerm OFFERS: a model id typed by hand works here and works there. Refused
/// only where a host would break or misread it: the wrong JSON type, a Codex reviewer Codex
/// refuses to start on, days out of range.
fn carriable(runtime: Runtime, key: &str, v: &Value) -> bool {
    match (runtime, key) {
        (Runtime::Claude, "retention") => v.as_u64().is_some_and(|d| (1..=RETENTION_MAX_DAYS).contains(&d)),
        (Runtime::Agy, "show_thinking") | (Runtime::Codex, "fullscreen") => v.is_boolean(),
        (Runtime::Codex, "reviewer") => v.as_str().is_some_and(|s| CODEX_REVIEWERS.contains(&s)),
        (Runtime::Codex, "model") => v.as_str().is_some_and(valid_codex_model),
        _ => v.is_string(),
    }
}

/// Tick (`on`) or untick a setting. Ticking records what this machine's file holds now, so
/// nothing changes here and hosts are given this machine's value; it writes no agent file.
pub fn manage(p: &Paths, runtime: Runtime, key: &str, on: bool) -> Result<(), String> {
    let id = managed_id(runtime, key);
    // Unticking needs no check: a setting recorded under a model this build no longer offers
    // must still be removable, or it would be carried to hosts for good.
    if on {
        // A known setting (an effort row only for the models offered; others are read-only).
        typed(runtime, key, None)?;
    }
    if !on {
        return edit_managed(&p.managed, |m| {
            m.keys.remove(&id);
        });
    }
    let v = local_value(p, runtime, key)?;
    if let Some(v) = &v {
        if !carriable(runtime, key, v) {
            return Err(format!(
                "This machine has {v} for it, which maiTerm can't give other hosts as it is. Change it in {} first.",
                runtime.name()
            ));
        }
    }
    let v = v.unwrap_or(Value::Null);
    edit_managed(&p.managed, |m| {
        m.keys.insert(id.clone(), v.clone());
    })
}

/// Switch the whole section on or off. Off: maiTerm changes no agent setting anywhere; the
/// ticks are kept for when it's on again.
pub fn set_enabled(p: &Paths, on: bool) -> Result<(), String> {
    edit_managed(&p.managed, |m| m.enabled = on)
}

/// The human picked a value: written here, and recorded as what hosts are given. Refused for a
/// setting maiTerm doesn't manage, and while the section is off.
pub fn set_managed(p: &Paths, runtime: Runtime, key: &str, value: Option<&str>) -> Result<(), String> {
    let m = load_managed(&p.managed)?;
    if !m.enabled {
        return Err("Agent Defaults is off, so maiTerm changes no agent settings. Switch it on first.".into());
    }
    let id = managed_id(runtime, key);
    if !m.keys.contains_key(&id) {
        return Err("maiTerm doesn't manage this setting. Tick it first.".into());
    }
    let v = typed(runtime, key, value)?;
    set(p, runtime, key, value)?;
    edit_managed(&p.managed, |m| {
        if m.keys.contains_key(&id) {
            m.keys.insert(id.clone(), v.clone().unwrap_or(Value::Null));
        }
    })
}

// ─── SSH hosts ────────────────────────────────────────────────────────────

/// Merges managed settings into a remote JSON settings file (`sys.argv[1]`: `claude` or `agy`).
/// The payload on stdin holds `[path, value]` pairs per file; null removes the key, and an
/// object that removal empties goes too; a set replaces a non-object on its way, as `set_claude`
/// does (the advisor never lands without its switch). Does nothing where the agent's folder doesn't exist,
/// to a link whose file is gone, or to a file it can't parse; writes only when something
/// changed, through a temp file beside the link's target that keeps the file's mode. No single quotes (the shell wraps it
/// in them), pure ASCII.
const PY_REMOTE_JSON: &str = r#"import json,os,sys
which=sys.argv[1]
d=json.load(sys.stdin)
ops=d.get(which) or []
paths={"claude":"~/.claude/settings.json","agy":"~/.gemini/antigravity-cli/settings.json"}
p=os.path.expanduser(paths[which])
if not ops or not os.path.isdir(os.path.dirname(p)):
    sys.exit(0)
if os.path.islink(p) and not os.path.exists(p):
    sys.exit(0)
s={}
if os.path.exists(p):
    try:
        raw=open(p).read()
        s=json.loads(raw) if raw.strip() else {}
    except Exception:
        sys.stderr.write("maiterm: Agent Defaults left "+p+" alone: it does not parse\n")
        sys.exit(0)
if not isinstance(s,dict):
    sys.exit(0)
before=json.dumps(s,sort_keys=True)
for path,v in ops:
    if v is None:
        chain=[s]
        for k in path[:-1]:
            n=chain[-1].get(k)
            if not isinstance(n,dict):
                chain=None
                break
            chain.append(n)
        if chain is None or path[-1] not in chain[-1]:
            continue
        chain[-1].pop(path[-1])
        for i in range(len(chain)-1,0,-1):
            if chain[i]:
                break
            chain[i-1].pop(path[i-1])
    else:
        cur=s
        for k in path[:-1]:
            if not isinstance(cur.get(k),dict):
                cur[k]={}
            cur=cur[k]
        cur[path[-1]]=v
if json.dumps(s,sort_keys=True)!=before:
    t=os.path.realpath(p)
    tmp=t+".maiterm-tmp-"+str(os.getpid())
    f=os.fdopen(os.open(tmp,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),"w")
    f.write(json.dumps(s,indent=2)+"\n")
    f.close()
    if os.path.exists(t):
        os.chmod(tmp,os.stat(t).st_mode&0o7777)
    os.replace(tmp,t)"#;

/// Merges managed settings into a remote `~/.codex/config.toml` (a one-line string or
/// true/false, at the top level or in one `[table]`; a key arrives as its path, `["model"]` or
/// `["tui", "fullscreen_transcript"]`),
/// textually, as `CODEX_TOML_MERGE_PY` does: python3 has no TOML writer. A line counts as a
/// key or a table header only where a statement can start: not inside a multi-line string, an
/// array or an inline table. A key whose value isn't a one-line string, or that appears twice,
/// leaves the file alone, and so does a table set as dotted keys or an inline one. A changed
/// value keeps its line and trailing comment; a new top-level key goes on the first line, which
/// is always top level, a new table key just under its `[table]` line, and a table that isn't
/// there yet at the end of the file; a removed key loses its line (its table header stays). With tomllib
/// (Python 3.11+) a file that doesn't parse is left alone, and so is an edit after which the
/// file reads back as anything but the old file with exactly our keys changed. Keeps the file's
/// mode (the bridge's own token is in it).
const PY_REMOTE_TOML: &str = r##"import copy,json,os,re,sys
Q=chr(39)
d=json.load(sys.stdin)
ops=d.get("codex") or []
p=os.path.expanduser("~/.codex/config.toml")
def leave(why):
    sys.stderr.write("maiterm: Agent Defaults left "+p+" alone: "+why+"\n")
    sys.exit(0)
if not ops or not os.path.isdir(os.path.dirname(p)):
    sys.exit(0)
if os.path.islink(p) and not os.path.exists(p):
    sys.exit(0)
try:
    src=open(p).read() if os.path.exists(p) else ""
except Exception:
    sys.exit(0)
try:
    import tomllib
except ImportError:
    tomllib=None
old=None
if tomllib:
    try:
        old=tomllib.loads(src)
    except Exception:
        leave("it does not parse")
lines=src.split("\n")
starts=[]
ml=None
depth=0
for l in lines:
    starts.append(ml is None and depth==0)
    i=0
    n=len(l)
    while i<n:
        if ml:
            if l.startswith(ml,i):
                j=i+3
                while j<n and j-i<5 and l[j]==ml[0]:
                    j+=1
                i=j
                ml=None
            elif ml[0]=="\"" and l[i]=="\\":
                i+=2
            else:
                i+=1
            continue
        c=l[i]
        if c=="#":
            break
        if l.startswith("\"\"\"",i) or l.startswith(Q*3,i):
            ml=l[i:i+3]
            i+=3
        elif c=="\"":
            i+=1
            while i<n and l[i]!="\"":
                i+=2 if l[i]=="\\" else 1
            i+=1
        elif c==Q:
            j=l.find(Q,i+1)
            i=n if j<0 else j+1
        else:
            if c in "[{":
                depth+=1
            elif c in "]}":
                depth=max(0,depth-1)
            i+=1
one=re.compile(r"^(\s*[^\s=#]+\s*=\s*)(\"(?:[^\"\\]|\\.)*\"|"+Q+"[^"+Q+"]*"+Q+r"|true|false)(\s*(?:#.*)?)$")
def named(n):
    e=re.escape(n)
    return "("+e+"|\""+e+"\"|"+Q+e+Q+")"
for k,v in ops:
    path=k if isinstance(k,list) else [k]
    hs=[i for i in range(len(lines)) if starts[i] and re.match(r"^\s*\[",lines[i])]
    end=hs[0] if hs else len(lines)
    lo,hi=0,end
    if len(path)==2:
        t=path[0]
        if any(starts[i] and re.match(r"^\s*"+named(t)+r"\s*[.=]",lines[i]) for i in range(end)):
            leave(t+" is set as a dotted key or an inline table")
        own=[i for i in hs if re.match(r"^\s*\[\s*"+named(t)+r"\s*\]\s*(#.*)?$",lines[i])]
        if len(own)>1:
            leave("["+t+"] appears more than once")
        if not own:
            if v is None:
                continue
            at=len(lines)-1 if lines[-1]=="" else len(lines)
            add=["["+t+"]",path[1]+" = "+json.dumps(v)]
            if at>0 and lines[at-1].strip()!="":
                add=[""]+add
            lines[at:at]=add
            starts[at:at]=[True]*len(add)
            continue
        lo=own[0]+1
        hi=([i for i in hs if i>own[0]]+[len(lines)])[0]
    elif len(path)!=1:
        continue
    name=".".join(path)
    key=re.compile(r"^\s*"+named(path[-1])+r"\s*=")
    idx=[i for i in range(lo,hi) if starts[i] and key.match(lines[i])]
    if len(idx)>1:
        leave(name+" is set more than once")
    m=one.match(lines[idx[0]]) if idx else None
    if idx and not m:
        leave(name+" is not a one-line string or true/false")
    if v is None:
        if idx:
            del lines[idx[0]]
            del starts[idx[0]]
        continue
    if not idx:
        lines.insert(lo,path[-1]+" = "+json.dumps(v))
        starts.insert(lo,True)
        continue
    cur=m.group(2)
    try:
        same=(cur[1:-1] if cur[0]==Q else json.loads(cur))
        same=same==v and type(same)==type(v)
    except Exception:
        same=False
    if not same:
        lines[idx[0]]=m.group(1)+json.dumps(v)+m.group(3)
res="\n".join(lines)
if res==src:
    sys.exit(0)
if tomllib:
    try:
        new=tomllib.loads(res)
    except Exception:
        leave("the edit would not parse")
    want=copy.deepcopy(old)
    for k,v in ops:
        path=k if isinstance(k,list) else [k]
        t=want
        for n in path[:-1]:
            if n not in t:
                t[n]={}
            t=t[n]
        if v is None:
            t.pop(path[-1],None)
        else:
            t[path[-1]]=v
    if new!=want:
        leave("the edit would change more than its own keys")
t=os.path.realpath(p)
tmp=t+".maiterm-tmp-"+str(os.getpid())
f=os.fdopen(os.open(tmp,os.O_WRONLY|os.O_CREAT|os.O_TRUNC,0o600),"w")
f.write(res)
f.close()
if os.path.exists(t):
    os.chmod(tmp,os.stat(t).st_mode&0o7777)
os.replace(tmp,t)"##;

/// The script that gives a bridged SSH host every managed setting: set where a value is
/// recorded, removed where "the agent's default" is. Empty when the section is off, nothing is
/// managed, or the record can't be read. Runs after the bridge's own setup, never beside it:
/// that rewrites the same files.
pub fn remote_script(p: &Paths) -> String {
    let m = match load_managed(&p.managed) {
        Ok(m) => m,
        Err(e) => {
            log::warn!("Agent Defaults: nothing carried to SSH hosts: {e}");
            return String::new();
        }
    };
    if !m.enabled || m.keys.is_empty() {
        return String::new();
    }
    let (mut claude, mut codex, mut agy) = (Vec::new(), Vec::new(), Vec::new());
    for (id, v) in &m.keys {
        let Some((rt, key)) = id.split_once('.') else { continue };
        match rt {
            "claude" => {
                let Ok(path) = claude_path(key) else { continue };
                if key == "advisor" {
                    let switch = if v.is_null() { Value::Null } else { json!("1") };
                    claude.push(json!([["env", ADVISOR_TOOL_ENV], switch]));
                }
                claude.push(json!([path, v]));
            }
            "codex" => {
                if let Ok(k) = codex_key(key) {
                    codex.push(json!([k, v]));
                }
            }
            "agy" => {
                if let Ok(k) = agy_key(key) {
                    agy.push(json!([[k], v]));
                }
            }
            _ => {}
        }
    }
    let payload = json!({ "claude": claude, "codex": codex, "agy": agy });
    // ASCII-only JSON: the remote python3 decodes it under whatever locale ssh gives it.
    let ascii: String = payload
        .to_string()
        .chars()
        .map(|c| {
            if c.is_ascii() {
                c.to_string()
            } else {
                c.encode_utf16(&mut [0; 2]).iter().map(|u| format!("\\u{u:04x}")).collect()
            }
        })
        .collect();
    let escaped = ascii.replace('\'', r"'\''");
    let mut lines = vec![format!("__agd='{escaped}'"), "if command -v python3 >/dev/null 2>&1; then".to_string()];
    for (which, ops) in [("claude", &claude), ("agy", &agy)] {
        if !ops.is_empty() {
            lines.push(format!("printf '%s' \"$__agd\" | python3 -c '{PY_REMOTE_JSON}' {which}"));
        }
    }
    if !codex.is_empty() {
        lines.push(format!("printf '%s' \"$__agd\" | python3 -c '{PY_REMOTE_TOML}'"));
    }
    lines.push("fi".into());
    // `:`, not `exit`: this is also pasted into a user's interactive shell (deshittify.rs).
    lines.push(":".into());
    lines.join("\n")
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
    fn the_codex_screen_goes_in_its_tui_table() {
        let (_h, p) = scratch();
        // No [tui] yet: one is added at the end, the top-level keys stay above every table.
        write(&p.codex, "model = \"a\"\n\n[tui.model_availability_nux]\nx = 1\n");
        assert!(set(&p, Runtime::Codex, "fullscreen", Some("false")).unwrap());
        let text = fs::read_to_string(&p.codex).unwrap();
        let doc: toml_edit::DocumentMut = text.parse().unwrap();
        assert_eq!(doc["tui"]["fullscreen_transcript"].as_bool(), Some(false), "{text}");
        assert_eq!(doc["tui"]["model_availability_nux"]["x"].as_integer(), Some(1));
        assert_eq!(status(&p).codex.fullscreen.as_deref(), Some("false"));
        assert!(!set(&p, Runtime::Codex, "fullscreen", Some("false")).unwrap(), "nothing to change");

        // An existing [tui] keeps its other keys and the value its comment.
        let before = "[tui]\nstatus_line_use_colors = true\nfullscreen_transcript = true # mine\n";
        write(&p.codex, before);
        assert!(set(&p, Runtime::Codex, "fullscreen", Some("false")).unwrap());
        assert_eq!(fs::read_to_string(&p.codex).unwrap(), "[tui]\nstatus_line_use_colors = true\nfullscreen_transcript = false # mine\n");
        assert_eq!(local_value(&p, Runtime::Codex, "fullscreen").unwrap(), Some(json!(false)));
        assert!(set(&p, Runtime::Codex, "fullscreen", None).unwrap());
        assert_eq!(fs::read_to_string(&p.codex).unwrap(), "[tui]\nstatus_line_use_colors = true\n");
        assert!(!set(&p, Runtime::Codex, "fullscreen", None).unwrap());
        assert!(set(&p, Runtime::Codex, "fullscreen", Some("scrollback")).is_err());

        // Removing it where there is no [tui] creates none.
        write(&p.codex, "model = \"a\"\n");
        assert!(!set(&p, Runtime::Codex, "fullscreen", None).unwrap());
        assert_eq!(fs::read_to_string(&p.codex).unwrap(), "model = \"a\"\n");
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
        assert!(flags_off(&s(json!({"DISABLE_GROWTHBOOK": "1"}))));
        assert!(flags_off(&s(json!({"CLAUDE_CODE_DISABLE_FEATURE_FLAGS": "on"}))));
        assert!(!flags_off(&s(json!({"CLAUDE_CODE_DISABLE_FEATURE_FLAGS": "0"}))));
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

    // ─── Managed settings and SSH hosts ───

    #[test]
    fn ticking_records_this_machines_value_and_writes_no_agent_file() {
        let (_h, p) = scratch();
        let before = r#"{"cleanupPeriodDays":548,"model":"claude-opus-5-5[1m]"}"#;
        write(&p.claude, before);
        assert!(set_managed(&p, Runtime::Claude, "model", Some("opus")).is_err(), "not ticked: refused");
        manage(&p, Runtime::Claude, "retention", true).unwrap();
        manage(&p, Runtime::Claude, "model", true).unwrap();
        manage(&p, Runtime::Claude, "advisor", true).unwrap();
        assert_eq!(fs::read_to_string(&p.claude).unwrap(), before, "ticking changes nothing here");
        let m = load_managed(&p.managed).unwrap();
        assert_eq!(m.keys["claude.retention"], json!(548));
        assert_eq!(m.keys["claude.model"], json!("claude-opus-5-5[1m]"), "a hand-typed id is carried as it is");
        assert_eq!(m.keys["claude.advisor"], Value::Null, "absent: the agent's default");

        set_managed(&p, Runtime::Claude, "model", Some("fable")).unwrap();
        assert_eq!(json_at(&p.claude)["model"], "fable");
        assert_eq!(load_managed(&p.managed).unwrap().keys["claude.model"], json!("fable"));
        set_managed(&p, Runtime::Claude, "retention", None).unwrap();
        assert_eq!(load_managed(&p.managed).unwrap().keys["claude.retention"], Value::Null);

        set_enabled(&p, false).unwrap();
        assert!(set_managed(&p, Runtime::Claude, "model", Some("opus")).is_err(), "off: refused");
        assert_eq!(remote_script(&p), "", "off: nothing reaches hosts");
        let st = status(&p);
        assert!(!st.enabled && st.managed.len() == 3, "the ticks are kept");
        set_enabled(&p, true).unwrap();
        manage(&p, Runtime::Claude, "model", false).unwrap();
        assert!(!load_managed(&p.managed).unwrap().keys.contains_key("claude.model"));
        assert_eq!(json_at(&p.claude)["model"], "fable", "unticking changes nothing here");
    }

    #[test]
    fn a_value_hosts_would_break_on_is_not_ticked() {
        let (_h, p) = scratch();
        write(&p.codex, "approvals_reviewer = \"bogus\"\n");
        write(&p.claude, r#"{"cleanupPeriodDays":"long"}"#);
        assert!(manage(&p, Runtime::Codex, "reviewer", true).is_err());
        assert!(manage(&p, Runtime::Claude, "retention", true).is_err());
        assert!(manage(&p, Runtime::Claude, "effort:claude-opus-4", true).is_err(), "only the models offered");
        assert!(load_managed(&p.managed).unwrap().keys.is_empty());
        assert_eq!(remote_script(&p), "", "nothing managed: no script");
        // Recorded by an older build for a model no longer offered: still removable.
        fs::create_dir_all(p.managed.parent().unwrap()).unwrap();
        fs::write(&p.managed, json!({"keys": {"claude.effort:claude-opus-4": "high"}}).to_string()).unwrap();
        manage(&p, Runtime::Claude, "effort:claude-opus-4", false).unwrap();
        assert!(load_managed(&p.managed).unwrap().keys.is_empty());
    }

    #[cfg(unix)]
    /// Run the remote script with `home` as HOME. None when this machine has no python3.
    fn run_remote(p: &Paths, home: &Path) -> Option<()> {
        std::process::Command::new("python3").arg("-c").arg("pass").output().ok()?.status.success().then_some(())?;
        let out = std::process::Command::new("bash")
            .arg("-c")
            .arg(remote_script(p))
            .env("HOME", home)
            .output()
            .unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        Some(())
    }

    // The host scripts run on a Unix host; on Windows python3 reads USERPROFILE, not HOME.
    #[cfg(unix)]
    #[test]
    fn hosts_get_the_recorded_values_and_nothing_else() {
        let (_h, p) = scratch();
        let (remote, r) = scratch();
        // Recorded here: advisor and Opus effort back to the agent's default, 500 days kept,
        // Codex and Antigravity values. The model is not managed.
        fs::create_dir_all(p.managed.parent().unwrap()).unwrap();
        fs::write(
            &p.managed,
            json!({"enabled": true, "keys": {
                "claude.advisor": null, "claude.effort:claude-opus-5-5": null, "claude.retention": 500,
                "codex.model": "gpt-7", "codex.reviewer": null, "codex.effort": "high",
                "agy.agent_mode": "", "agy.show_thinking": true,
            }})
            .to_string(),
        )
        .unwrap();
        write(
            &r.claude,
            &json!({"advisorModel": "opus", "model": "sonnet", "x": 1,
                "env": {"DISABLE_TELEMETRY": "1", ADVISOR_TOOL_ENV: "1"},
                "modelSettings": {"claude-opus-5-5": {"effortLevel": "high"}}})
            .to_string(),
        );
        let toml = "# mine\nmodel = \"gpt-6\"  # pinned\napprovals_reviewer = \"user\"\n\n[profiles.x]\nmodel = \"o\"\n";
        write(&r.codex, toml);
        write(&r.agy, r#"{"agentMode":"plan","permissions":{"allow":["mcp(maiterm/*)"]}}"#);
        if run_remote(&p, &remote).is_none() {
            return;
        }
        let c = json_at(&r.claude);
        assert!(c.get("advisorModel").is_none());
        assert_eq!(c["env"], json!({"DISABLE_TELEMETRY": "1"}), "only the advisor's switch goes");
        assert!(c.get("modelSettings").is_none(), "an emptied entry goes, and the emptied table");
        assert_eq!(c["cleanupPeriodDays"], 500);
        assert_eq!((c["model"].as_str(), c["x"].as_i64()), (Some("sonnet"), Some(1)), "unmanaged: kept");

        let t = fs::read_to_string(&r.codex).unwrap();
        assert!(t.contains("model = \"gpt-7\"  # pinned\n"), "{t}");
        assert!(!t.contains("approvals_reviewer"), "{t}");
        assert!(t.starts_with("model_reasoning_effort = \"high\"\n# mine\n"), "{t}");
        assert!(t.ends_with("[profiles.x]\nmodel = \"o\"\n"), "a table's own key is not ours: {t}");

        let g = json_at(&r.agy);
        assert_eq!((g["agentMode"].as_str(), g["showThinking"].as_bool()), (Some(""), Some(true)), "\"\" is a value");
        assert_eq!(g["permissions"]["allow"][0], "mcp(maiterm/*)");

        // A second connect changes nothing: the files are not even rewritten.
        let compact = fs::read_to_string(&r.agy).unwrap().replace(['\n', ' '], "");
        write(&r.agy, &compact);
        let codex_before = fs::read_to_string(&r.codex).unwrap();
        run_remote(&p, &remote);
        assert_eq!(fs::read_to_string(&r.agy).unwrap(), compact);
        assert_eq!(fs::read_to_string(&r.codex).unwrap(), codex_before);
    }

    // The host scripts run on a Unix host; on Windows python3 reads USERPROFILE, not HOME.
    #[cfg(unix)]
    #[test]
    fn a_host_file_that_does_not_parse_or_an_absent_agent_is_left_alone() {
        let (_h, p) = scratch();
        let (remote, r) = scratch();
        fs::create_dir_all(p.managed.parent().unwrap()).unwrap();
        fs::write(&p.managed, json!({"keys": {"claude.model": "opus", "codex.effort": "low", "agy.agent_mode": "plan"}}).to_string()).unwrap();
        write(&r.claude, "{ not json");
        write(&r.codex, "model = [unclosed\n");
        if run_remote(&p, &remote).is_none() {
            return;
        }
        assert_eq!(fs::read_to_string(&r.claude).unwrap(), "{ not json");
        assert!(!r.agy.parent().unwrap().exists(), "no Antigravity there: nothing created");
        // Without tomllib (Python < 3.11) the text edit goes ahead; with it, the file is kept.
        let probe = std::process::Command::new("python3").args(["-c", "import tomllib"]).output().unwrap();
        if probe.status.success() {
            assert_eq!(fs::read_to_string(&r.codex).unwrap(), "model = [unclosed\n");
        }
    }

    #[cfg(unix)]
    /// Run the Codex merge on `toml` with `ops`, with and without tomllib (a shadow module that
    /// fails to import stands in for Python < 3.11). Each run's result: None when python3 is missing.
    fn codex_merge(toml: &str, ops: Value) -> Option<[String; 2]> {
        std::process::Command::new("python3").arg("-c").arg("pass").output().ok()?.status.success().then_some(())?;
        let run = |hide: bool| {
            let (home, p) = scratch();
            write(&p.codex, toml);
            let shadow = home.join("shadow");
            fs::create_dir_all(&shadow).unwrap();
            fs::write(shadow.join("tomllib.py"), "raise ImportError\n").unwrap();
            let mut cmd = std::process::Command::new("python3");
            cmd.arg("-c").arg(PY_REMOTE_TOML).env("HOME", &home).stdin(std::process::Stdio::piped());
            if hide {
                cmd.env("PYTHONPATH", &shadow);
            }
            let mut child = cmd.spawn().unwrap();
            use std::io::Write;
            child.stdin.take().unwrap().write_all(json!({ "codex": ops }).to_string().as_bytes()).unwrap();
            assert!(child.wait().unwrap().success());
            fs::read_to_string(&p.codex).unwrap()
        };
        Some([run(false), run(true)])
    }

    // The host scripts run on a Unix host; on Windows python3 reads USERPROFILE, not HOME.
    #[cfg(unix)]
    #[test]
    fn the_codex_merge_reads_only_real_top_level_keys() {
        let ops = json!([["model", "b"], ["approvals_reviewer", null]]);
        // Lines inside a multi-line string, an array of arrays and a quoted table name are
        // not keys or headers.
        let toml = "developer_instructions = \"\"\"\nBe terse.\nmodel = \"small\"\napprovals_reviewer = \"x\"\n[notes]\n\"\"\"\nnotify = [\n  [\"a\"],\n]\nmodel = \"a\" # mine\napprovals_reviewer = \"user\"\n\n[projects.\"/home/u/x\"]\nmodel = \"p\"\n";
        let want = "developer_instructions = \"\"\"\nBe terse.\nmodel = \"small\"\napprovals_reviewer = \"x\"\n[notes]\n\"\"\"\nnotify = [\n  [\"a\"],\n]\nmodel = \"b\" # mine\n\n[projects.\"/home/u/x\"]\nmodel = \"p\"\n";
        let Some(out) = codex_merge(toml, ops.clone()) else { return };
        assert_eq!(out, [want.to_string(), want.to_string()]);

        // A literal-quoted key is the key.
        let Some(out) = codex_merge("'model' = 'a'\n", ops.clone()) else { return };
        assert_eq!(out, ["'model' = \"b\"\n".to_string(), "'model' = \"b\"\n".to_string()]);
    }

    // The host scripts run on a Unix host; on Windows python3 reads USERPROFILE, not HOME.
    #[cfg(unix)]
    #[test]
    fn the_codex_merge_leaves_what_it_cannot_edit_safely() {
        for toml in [
            "model = [\n \"a\",\n]\n",            // not a one-line string
            "model = \"a\"\nmodel = \"c\"\n",      // twice
            "model = \"\"\"\na\n\"\"\"\n",         // multi-line
        ] {
            let Some(out) = codex_merge(toml, json!([["model", "b"]])) else { return };
            assert_eq!(out, [toml.to_string(), toml.to_string()], "{toml}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_codex_merge_sets_a_key_in_its_table() {
        let ops = json!([[["tui", "fullscreen_transcript"], false]]);
        let cases = [
            // No [tui]: one is added at the end, after the tables already there.
            ("model = \"a\"\n\n[tui.nux]\nx = 1\n", "model = \"a\"\n\n[tui.nux]\nx = 1\n\n[tui]\nfullscreen_transcript = false\n"),
            ("", "[tui]\nfullscreen_transcript = false\n"),
            // In [tui], never in the table after it; the comment stays.
            ("[tui]\nfullscreen_transcript = true # mine\n[other]\nfullscreen_transcript = true\n", "[tui]\nfullscreen_transcript = false # mine\n[other]\nfullscreen_transcript = true\n"),
            ("[tui]\nstatus_line_use_colors = true\n", "[tui]\nfullscreen_transcript = false\nstatus_line_use_colors = true\n"),
            // Already so: untouched.
            ("[ tui ] # x\nfullscreen_transcript = false\n", "[ tui ] # x\nfullscreen_transcript = false\n"),
        ];
        for (toml, want) in cases {
            let Some(out) = codex_merge(toml, ops.clone()) else { return };
            assert_eq!(out, [want.to_string(), want.to_string()], "{toml:?}");
        }
        // Removal takes the line and leaves the header.
        let Some(out) = codex_merge("[tui]\nfullscreen_transcript = false\nx = 1\n", json!([[["tui", "fullscreen_transcript"], null]])) else { return };
        assert_eq!(out, ["[tui]\nx = 1\n".to_string(), "[tui]\nx = 1\n".to_string()]);
        // A [tui] it can't edit safely is left alone.
        for toml in [
            "tui.fullscreen_transcript = true\n",            // dotted
            "tui = { fullscreen_transcript = true }\n",      // inline
            "[tui]\nx = 1\n[tui]\ny = 2\n",                  // twice (also invalid)
            "[tui]\nfullscreen_transcript = \"\"\"\nx\n\"\"\"\n", // not one line
        ] {
            let Some(out) = codex_merge(toml, ops.clone()) else { return };
            assert_eq!(out, [toml.to_string(), toml.to_string()], "{toml:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn host_files_keep_their_mode() {
        use std::os::unix::fs::PermissionsExt;
        let (_h, p) = scratch();
        let (remote, r) = scratch();
        fs::create_dir_all(p.managed.parent().unwrap()).unwrap();
        fs::write(&p.managed, json!({"keys": {"codex.effort": "low", "codex.fullscreen": false, "agy.agent_mode": "plan", "claude.advisor": "opus"}}).to_string()).unwrap();
        write(&r.codex, "model = \"a\"\n");
        write(&r.agy, "{}");
        write(&r.claude, r#"{"env":null}"#);
        for f in [&r.codex, &r.agy, &r.claude] {
            fs::set_permissions(f, fs::Permissions::from_mode(0o600)).unwrap();
        }
        if run_remote(&p, &remote).is_none() {
            return;
        }
        for f in [&r.codex, &r.agy, &r.claude] {
            assert_eq!(fs::metadata(f).unwrap().permissions().mode() & 0o777, 0o600, "{}", f.display());
        }
        let codex = fs::read_to_string(&r.codex).unwrap();
        assert!(codex.contains("model_reasoning_effort = \"low\""), "{codex}");
        assert!(codex.ends_with("\n[tui]\nfullscreen_transcript = false\n"), "{codex}");
        let c = json_at(&r.claude);
        assert_eq!((c["advisorModel"].as_str(), c["env"][ADVISOR_TOOL_ENV].as_str()), (Some("opus"), Some("1")), "the switch goes with the advisor");
    }

    #[test]
    fn the_remote_snippets_survive_the_shell() {
        for (name, src) in [("PY_REMOTE_JSON", PY_REMOTE_JSON), ("PY_REMOTE_TOML", PY_REMOTE_TOML)] {
            assert!(!src.contains('\''), "{name} contains a single quote");
            assert!(src.is_ascii(), "{name} is not ASCII");
        }
    }

}
