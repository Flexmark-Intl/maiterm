//! The Hive record, `Workspace.hive` (docs/hive.md §3.2).
//!
//! Every kind field is a String and every field defaults: an older build that meets an unknown
//! enum variant fails the whole state file and starts empty. Fields a later build adds survive
//! a load/save here in `extra`, so a downgrade-then-upgrade doesn't lose them.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const LANDING_BASE: &str = "base";
pub const LANDING_INTEGRATION: &str = "integration";
pub const KIND_GIT: &str = "git";
pub const KIND_PLAIN: &str = "plain";
pub const STATUS_ACTIVE: &str = "active";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Hive {
    /// Its own uuid, never the workspace id: `<app data>/hive/<id>/` is named by it, and a
    /// window duplicate or an import re-mints workspace ids.
    pub id: String,
    pub goal: String,
    pub instructions: String,
    pub folders: Vec<HiveFolder>,
    /// Workers at once, enforced in Rust (§3.1).
    pub cap: u32,
    /// `"base"` | `"integration"`. Fixed at creation: it's a branch topology (§3).
    pub landing_mode: String,
    pub models: HiveModels,
    pub tools: Vec<String>,
    /// checkpoint → `"coordinator"` | `"ask"` (§7.4).
    pub checkpoints: BTreeMap<String, String>,
    pub budget_cents: u64,
    pub push_after_land: bool,
    pub settings: HiveSettings,
    /// Completion criteria, current list (§11.4).
    pub criteria: Vec<String>,
    /// `"active"` | `"paused"` | `"done"`.
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub done_at: Option<i64>,
    /// Names the `hive/<slug>` branches (§6.4). Kept on the record so renaming the workspace
    /// never renames a branch out from under its worktrees.
    pub slug: String,
    pub created_at: String,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Hive {
    fn default() -> Self {
        Self {
            id: String::new(),
            goal: String::new(),
            instructions: String::new(),
            folders: Vec::new(),
            cap: 4,
            landing_mode: LANDING_BASE.to_string(),
            models: HiveModels::default(),
            tools: Vec::new(),
            checkpoints: BTreeMap::new(),
            budget_cents: 1000,
            push_after_land: false,
            settings: HiveSettings::default(),
            criteria: Vec::new(),
            status: STATUS_ACTIVE.to_string(),
            done_at: None,
            slug: String::new(),
            created_at: String::new(),
            extra: Map::new(),
        }
    }
}

impl Hive {
    pub fn primary(&self) -> Option<&HiveFolder> {
        self.folders.iter().find(|f| f.primary)
    }

    pub fn git_folders(&self) -> impl Iterator<Item = &HiveFolder> {
        self.folders.iter().filter(|f| f.is_git())
    }

    pub fn is_integration(&self) -> bool {
        self.landing_mode == LANDING_INTEGRATION
    }

    /// The landing target branch in integration mode (§6.4).
    pub fn integration_branch(&self) -> String {
        format!("hive/{}", self.slug)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveFolder {
    /// How briefs, `hiveSpawnWorker` and the worktree paths name it: one path component.
    pub name: String,
    /// Canonical (symlinks resolved) when added.
    pub path: String,
    /// `"git"` | `"plain"`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub land_check: Option<String>,
    pub primary: bool,
    /// Local-path remotes, read when the folder is added: each goes on every agent's
    /// `denyWrite`, since no deny rule reliably stops a push (§7.3).
    pub local_remotes: Vec<String>,
}

impl HiveFolder {
    pub fn is_git(&self) -> bool {
        self.kind == KIND_GIT
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveModels {
    pub coordinator: HiveModel,
    pub worker: HiveModel,
}

impl Default for HiveModels {
    fn default() -> Self {
        Self {
            coordinator: HiveModel::new("claude", "opus", "medium"),
            worker: HiveModel::new("claude", "sonnet", "high"),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveModel {
    pub runtime: String,
    pub model: String,
    pub effort: String,
}

impl HiveModel {
    pub fn new(runtime: &str, model: &str, effort: &str) -> Self {
        Self { runtime: runtime.to_string(), model: model.to_string(), effort: effort.to_string() }
    }
}

/// The other settings sections (§3.2) arrive with the steps that own them; until then whatever
/// is here round-trips untouched.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HiveSettings {
    /// Which commands are queued and what they need (§6.8).
    pub run_queue: RunQueueSettings,
    /// The config rule's extra paths and batching (§8.1, §6.8).
    pub land: LandSettings,
    /// How long the coordinator has to answer a worker's prompt, and the daily digest (§4.7, §7.6).
    pub autonomy: AutonomySettings,
    /// Where `galleryAdd` may also file from, and the generators' hosts (§10.3, §14.16).
    pub gallery: GallerySettings,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

/// The gallery's human-set paths and hosts (§10.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GallerySettings {
    /// Canonical folders a tool writes only to its own place: `galleryAdd` takes a path in one.
    pub output_folders: Vec<String>,
    /// Hosts a worker's sandbox may reach for paid generation (filled in step 8f).
    pub generator_hosts: Vec<String>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

pub const PROMPT_TIMEOUT_DEFAULT_SECS: u64 = 300;
pub const PROMPT_TIMEOUT_MIN_SECS: u64 = 60;
pub const PROMPT_TIMEOUT_MAX_SECS: u64 = 3600;

/// The coordinator's answering window and what it reports (§4.7, §7.6). Not a top-level
/// `settings.prompt_timeout_secs`: that key round-trips in `extra` (plan D22).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AutonomySettings {
    /// A coordinator-owned prompt unanswered this long notifies the human, who may then answer it too.
    pub prompt_timeout_secs: u64,
    /// A daily summary of what the coordinator decided alone, in its chat.
    pub daily_digest: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for AutonomySettings {
    fn default() -> Self {
        Self { prompt_timeout_secs: PROMPT_TIMEOUT_DEFAULT_SECS, daily_digest: true, extra: Map::new() }
    }
}

/// How lands behave beyond the defaults (§8.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LandSettings {
    /// Paths the human adds to the config rule: a land touching one goes to the human.
    pub config_paths: Vec<String>,
    /// Lands already passed to `hiveLand` in one repo share one land check (§6.8).
    pub batch: bool,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for LandSettings {
    fn default() -> Self {
        Self { config_paths: Vec::new(), batch: true, extra: Map::new() }
    }
}

/// The queued commands per folder and the Hive's own resource limits (§6.8).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RunQueueSettings {
    pub commands: Vec<QueuedCommand>,
    /// resource → capacity. On a machine-wide pool (`cargo`, `tauri-dev`, `e2e`, `gpu`) it only
    /// lowers this Hive's share; on the Hive's own resources it is their capacity, except a
    /// `migrate:` namespace, which is always one at a time.
    pub resources: BTreeMap<String, u32>,
}

/// One command a worker runs only through the queue: `hiveRun`'s command must start with
/// `prefix`, word by word, and takes everything else from here, never from the worker.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct QueuedCommand {
    /// The Hive folder's name.
    pub folder: String,
    /// The command's leading words, as the human wrote them (`cargo test`).
    pub prefix: String,
    /// Resources it holds while it runs; `{worker}` expands (`migrate:{worker}`).
    pub needs: Vec<String>,
    /// Stack services it leases, by name, for as long as it runs.
    pub services: Vec<String>,
    /// Set for the job, placeholders filled per worker (`MAITERM_INSTANCE={instance}`).
    pub env: Vec<(String, String)>,
    /// Paths beyond the worker's own that the job's sandbox may also write, placeholders and
    /// `~` expanded.
    pub extra_write: Vec<String>,
}

impl QueuedCommand {
    pub fn words(&self) -> Vec<&str> {
        self.prefix.split_whitespace().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Workspace;

    #[test]
    fn a_workspace_saved_before_hives_loads_without_one() {
        let ws = Workspace::new("Old".into());
        let mut v = serde_json::to_value(&ws).unwrap();
        assert!(v.get("hive").is_none(), "no Hive, no key: an older build sees nothing new");
        v.as_object_mut().unwrap().remove("hive");
        let back: Workspace = serde_json::from_value(v).unwrap();
        assert!(back.hive.is_none());
    }

    #[test]
    fn a_sparse_record_takes_the_defaults() {
        let h: Hive = serde_json::from_str(r#"{"id":"h1","goal":"ship it"}"#).unwrap();
        assert_eq!(h.cap, 4);
        assert_eq!(h.landing_mode, LANDING_BASE);
        assert_eq!(h.status, STATUS_ACTIVE);
        assert_eq!(h.models.coordinator.model, "opus");
        assert_eq!(h.models.worker.effort, "high");
        assert!(h.extra.is_empty());
    }

    #[test]
    fn fields_a_later_build_added_survive_a_load_and_save() {
        let text = r#"{
            "id": "h1", "slug": "s", "landing_mode": "integration", "status": "paused",
            "future_field": {"a": [1, 2]},
            "folders": [{"name": "web", "path": "/r/web", "kind": "git", "primary": true, "folder_future": 7}],
            "settings": {"prompt_timeout_secs": 300},
            "models": {"coordinator": {"runtime": "codex", "model": "m", "effort": "low"}}
        }"#;
        let h: Hive = serde_json::from_str(text).unwrap();
        assert_eq!(h.extra.get("future_field"), Some(&serde_json::json!({"a": [1, 2]})));
        assert_eq!(h.settings.extra.get("prompt_timeout_secs"), Some(&serde_json::json!(300)));
        assert!(h.is_integration());
        assert_eq!(h.integration_branch(), "hive/s");
        assert_eq!(h.primary().map(|f| f.name.as_str()), Some("web"));
        assert_eq!(h.models.worker, HiveModel::new("claude", "sonnet", "high"), "a missing half defaults");

        let again: Hive = serde_json::from_value(serde_json::to_value(&h).unwrap()).unwrap();
        assert_eq!(again, h);
        // A field on a nested struct without an `extra` map is dropped: only the record and its
        // settings promise round-trips.
        assert!(!serde_json::to_string(&again).unwrap().contains("folder_future"));
    }

    #[test]
    fn the_run_queue_settings_load_sparse_and_keep_their_neighbours() {
        let h: Hive = serde_json::from_str(r#"{"id":"h1","settings":{"prompt_timeout_secs":300,
            "run_queue":{"commands":[{"folder":"app","prefix":"cargo  test","needs":["cargo"],"later":1}],"resources":{"cargo":1}}}}"#)
        .unwrap();
        let q = &h.settings.run_queue;
        assert_eq!(q.commands[0].words(), vec!["cargo", "test"]);
        assert!(q.commands[0].env.is_empty() && q.commands[0].services.is_empty());
        assert_eq!(q.resources.get("cargo"), Some(&1));
        assert_eq!(h.settings.extra.get("prompt_timeout_secs"), Some(&serde_json::json!(300)));
        let again: Hive = serde_json::from_value(serde_json::to_value(&h).unwrap()).unwrap();
        assert_eq!(again, h);
        let bare: Hive = serde_json::from_str(r#"{"id":"h1"}"#).unwrap();
        assert!(bare.settings.run_queue.commands.is_empty());
    }

    #[test]
    fn the_land_settings_default_to_batching_and_keep_later_fields() {
        let bare: Hive = serde_json::from_str(r#"{"id":"h1"}"#).unwrap();
        assert!(bare.settings.land.batch && bare.settings.land.config_paths.is_empty(), "batching is on by default (§6.8)");
        let h: Hive = serde_json::from_str(r#"{"id":"h1","settings":{"land":{"config_paths":["scripts/**"],"batch":false,"later":1},"other":2}}"#).unwrap();
        assert_eq!((h.settings.land.config_paths.clone(), h.settings.land.batch), (vec!["scripts/**".to_string()], false));
        assert_eq!(h.settings.land.extra.get("later"), Some(&serde_json::json!(1)));
        assert_eq!(h.settings.extra.get("other"), Some(&serde_json::json!(2)));
        let again: Hive = serde_json::from_value(serde_json::to_value(&h).unwrap()).unwrap();
        assert_eq!(again, h);
    }

    #[test]
    fn the_autonomy_settings_default_and_keep_later_fields() {
        let bare: Hive = serde_json::from_str(r#"{"id":"h1","settings":{"prompt_timeout_secs":120}}"#).unwrap();
        assert_eq!((bare.settings.autonomy.prompt_timeout_secs, bare.settings.autonomy.daily_digest), (PROMPT_TIMEOUT_DEFAULT_SECS, true));
        assert_eq!(bare.settings.extra.get("prompt_timeout_secs"), Some(&serde_json::json!(120)), "the old key stays where it was");
        let h: Hive = serde_json::from_str(r#"{"id":"h1","settings":{"autonomy":{"prompt_timeout_secs":600,"daily_digest":false,"later":1}}}"#).unwrap();
        assert_eq!((h.settings.autonomy.prompt_timeout_secs, h.settings.autonomy.daily_digest), (600, false));
        assert_eq!(h.settings.autonomy.extra.get("later"), Some(&serde_json::json!(1)));
        let again: Hive = serde_json::from_value(serde_json::to_value(&h).unwrap()).unwrap();
        assert_eq!(again, h);
    }

    #[test]
    fn the_gallery_settings_load_sparse_and_keep_their_neighbours() {
        let bare: Hive = serde_json::from_str(r#"{"id":"h1"}"#).unwrap();
        assert_eq!(bare.settings.gallery, GallerySettings::default());
        let h: Hive = serde_json::from_str(
            r#"{"id":"h1","settings":{"gallery":{"output_folders":["/Users/x/ComfyUI/output"],"later":1},
                "autonomy":{"daily_digest":false},"other":2}}"#,
        )
        .unwrap();
        assert_eq!(h.settings.gallery.output_folders, vec!["/Users/x/ComfyUI/output".to_string()]);
        assert!(h.settings.gallery.generator_hosts.is_empty());
        assert_eq!(h.settings.gallery.extra.get("later"), Some(&serde_json::json!(1)));
        assert!(!h.settings.autonomy.daily_digest, "a neighbour section is read as it was");
        assert_eq!(h.settings.extra.get("other"), Some(&serde_json::json!(2)));
        let again: Hive = serde_json::from_value(serde_json::to_value(&h).unwrap()).unwrap();
        assert_eq!(again, h);
    }

    #[test]
    fn an_unknown_kind_string_still_loads() {
        let ws_text = {
            let mut ws = Workspace::new("H".into());
            ws.hive = Some(Hive { id: "h".into(), landing_mode: "someday".into(), status: "archived".into(), ..Hive::default() });
            serde_json::to_string(&ws).unwrap()
        };
        let back: Workspace = serde_json::from_str(&ws_text).unwrap();
        let h = back.hive.unwrap();
        assert_eq!(h.landing_mode, "someday");
        assert_eq!(h.status, "archived");
    }
}
