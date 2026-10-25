//! The Hive's checkpoints (docs/hive.md §7.4): who answers each kind of decision, the
//! coordinator or the human (Ask me). Read here, never off `Hive.checkpoints` directly: a key the
//! record doesn't have reads as its default for the landing mode.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::json;
use tauri::AppHandle;

use super::record::{AutonomySettings, Hive, PROMPT_TIMEOUT_MAX_SECS, PROMPT_TIMEOUT_MIN_SECS};
use crate::state::AppState;

pub const CHECKPOINTS: [&str; 6] = ["plan", "questions", "permissions", "landing", "finals", "interfaces"];
pub const COORDINATOR: &str = "coordinator";
pub const ASK: &str = "ask";

pub const PERMISSIONS: &str = "permissions";
pub const QUESTIONS: &str = "questions";
pub const LANDING: &str = "landing";

/// Whether `key` is set to Ask me.
pub fn asks(hive: &Hive, key: &str) -> bool {
    let set = hive.checkpoints.get(key).cloned().or_else(|| super::create::default_checkpoints(&hive.landing_mode).get(key).cloned());
    set.as_deref() == Some(ASK)
}

/// The checkpoint a prompt of this kind falls under: a doorbell or notification kind, or a
/// routed prompt's. Compaction, completion and a finished turn follow Workers' questions (plan
/// D14). None: not a checkpoint's at all.
pub fn for_prompt_kind(kind: &str) -> Option<&'static str> {
    match kind {
        "permission" => Some(PERMISSIONS),
        "question" | "ask" | "blocker" | "compaction" | "completion" | "idle_done" => Some(QUESTIONS),
        _ => None,
    }
}

/// The checkpoints as the human sets them: the six keys, each `coordinator` or `ask`; Landing
/// stays Ask me in the integration mode. A key left out keeps its setting, and keys a later build
/// added are kept as they were.
pub fn validate(hive: &Hive, next: &BTreeMap<String, String>) -> Result<BTreeMap<String, String>, String> {
    let mut out = super::create::default_checkpoints(&hive.landing_mode);
    out.extend(hive.checkpoints.clone());
    for (k, v) in next {
        if !CHECKPOINTS.contains(&k.as_str()) {
            return Err(format!("There's no checkpoint \"{k}\"."));
        }
        if v != COORDINATOR && v != ASK {
            return Err(format!("{} is set to \"coordinator\" or \"ask\", not \"{v}\".", label(k)));
        }
        if k == "landing" && v == COORDINATOR && hive.is_integration() {
            return Err("In the integration mode you approve each land: Landing stays Ask me.".to_string());
        }
        out.insert(k.clone(), v.clone());
    }
    Ok(out)
}

pub fn label(key: &str) -> &'static str {
    match key {
        "plan" => "the plan",
        "questions" => "workers\u{2019} questions",
        "permissions" => "workers\u{2019} permission prompts",
        "landing" => "landing",
        "finals" => "picking finals in the gallery",
        "interfaces" => "adding a dependency or changing a public interface",
        _ => "this checkpoint",
    }
}

/// The checkpoints that changed, as the coordinator is told: "workers' questions: Ask me".
pub fn describe_changes(before: &BTreeMap<String, String>, after: &BTreeMap<String, String>) -> Vec<String> {
    CHECKPOINTS
        .iter()
        .filter(|k| before.get(**k) != after.get(**k))
        .map(|k| format!("{}: {}", label(k), if after.get(*k).map(String::as_str) == Some(ASK) { "Ask me" } else { "you decide" }))
        .collect()
}

pub fn clamp_timeout(secs: u64) -> u64 {
    secs.clamp(PROMPT_TIMEOUT_MIN_SECS, PROMPT_TIMEOUT_MAX_SECS)
}

/// Hive settings → Autonomy: the checkpoints, the prompt timeout and the digest. The coordinator
/// is told what changed, and a prompt it holds under a checkpoint now Ask me becomes the human's.
#[tauri::command]
pub fn hive_set_autonomy(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    app: AppHandle,
    workspace_id: String,
    checkpoints: BTreeMap<String, String>,
    autonomy: AutonomySettings,
) -> Result<Hive, String> {
    use tauri::Emitter;
    let label = window.label().to_string();
    let (before, hive, snapshot) = {
        let mut data = state.app_data.write();
        let ws = data
            .window_mut(&label)
            .and_then(|w| w.workspaces.iter_mut().find(|w| w.id == workspace_id))
            .ok_or("Workspace not found")?;
        let hive = ws.hive.as_mut().ok_or("That workspace isn't a Hive")?;
        let before = validate(hive, &BTreeMap::new())?;
        hive.checkpoints = validate(hive, &checkpoints)?;
        let mut autonomy = autonomy;
        autonomy.prompt_timeout_secs = clamp_timeout(autonomy.prompt_timeout_secs);
        hive.settings.autonomy = autonomy;
        (before, hive.clone(), data.clone())
    };
    crate::state::save_state(&snapshot)?;
    let changes = describe_changes(&before, &hive.checkpoints);
    log::info!("hive {}: autonomy set: {}", hive.id, if changes.is_empty() { "checkpoints unchanged".to_string() } else { changes.join("; ") });
    let payload = json!({ "workspace_id": workspace_id, "hive": hive });
    if let Err(e) = app.emit_to(label.as_str(), super::fuses::HIVE_CHANGED_EVENT, payload) {
        log::warn!("hive {}: autonomy change not sent to its window: {e}", hive.id);
    }
    if !changes.is_empty() {
        let (state, app, hive_c) = (state.inner().clone(), app.clone(), hive.clone());
        tauri::async_runtime::spawn(async move {
            super::prompts::checkpoints_changed(&state, &app, &label, &workspace_id, &hive_c, changes).await;
        });
    }
    Ok(hive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hive::record::LANDING_INTEGRATION;

    fn hive(mode: &str) -> Hive {
        Hive { id: "h1".into(), landing_mode: mode.into(), checkpoints: super::super::create::default_checkpoints(mode), ..Hive::default() }
    }

    #[test]
    fn a_missing_key_reads_as_its_default() {
        let mut h = hive("base");
        h.checkpoints.clear();
        assert!(!asks(&h, "permissions") && !asks(&h, "landing"));
        h.landing_mode = LANDING_INTEGRATION.into();
        assert!(asks(&h, "landing"), "integration lands are always asked");
        h.checkpoints.insert("questions".into(), ASK.into());
        assert!(asks(&h, "questions"));
        assert!(!asks(&h, "unknown"));
    }

    #[test]
    fn prompt_kinds_map_to_their_checkpoint() {
        assert_eq!(for_prompt_kind("permission"), Some(PERMISSIONS));
        for k in ["question", "ask", "blocker", "compaction", "completion", "idle_done"] {
            assert_eq!(for_prompt_kind(k), Some(QUESTIONS), "{k}");
        }
        for k in ["trust", "escalation", "script", "request", "account", ""] {
            assert_eq!(for_prompt_kind(k), None, "{k}");
        }
    }

    #[test]
    fn the_checkpoints_are_checked() {
        let mut h = hive("base");
        h.checkpoints.insert("future".into(), "x".into());
        let next: BTreeMap<String, String> = [("permissions".to_string(), ASK.to_string())].into();
        let out = validate(&h, &next).unwrap();
        assert_eq!(out.get("permissions").map(String::as_str), Some(ASK));
        assert_eq!(out.get("plan").map(String::as_str), Some(COORDINATOR), "a key left out keeps its setting");
        assert_eq!(out.get("future").map(String::as_str), Some("x"), "a later build's key is kept as it was");
        let bad = |k: &str, v: &str| validate(&h, &[(k.to_string(), v.to_string())].into()).unwrap_err();
        assert!(bad("budget", ASK).contains("no checkpoint"));
        assert!(bad("plan", "maybe").contains("not \"maybe\""));
        assert!(validate(&h, &[("landing".to_string(), ASK.to_string())].into()).is_ok());

        let integ = hive(LANDING_INTEGRATION);
        let e = validate(&integ, &[("landing".to_string(), COORDINATOR.to_string())].into()).unwrap_err();
        assert!(e.contains("Landing stays Ask me"), "{e}");
        assert_eq!(describe_changes(&validate(&h, &BTreeMap::new()).unwrap(), &out), vec!["workers\u{2019} permission prompts: Ask me"]);
        assert_eq!((clamp_timeout(5), clamp_timeout(300), clamp_timeout(99_999)), (60, 300, 3600));
    }
}
