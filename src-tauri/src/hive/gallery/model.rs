//! The gallery's records (docs/hive.md §10.2). Every kind is a String and every struct defaults:
//! the index outlives builds, and an older build must read what a newer one wrote.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const KIND_IMAGE: &str = "image";
pub const KIND_VIDEO: &str = "video";
pub const KIND_AUDIO: &str = "audio";
pub const KIND_DOCUMENT: &str = "document";
pub const KIND_MODEL3D: &str = "model3d";
pub const KIND_OTHER: &str = "other";
pub const KINDS: [&str; 6] = [KIND_IMAGE, KIND_VIDEO, KIND_AUDIO, KIND_DOCUMENT, KIND_MODEL3D, KIND_OTHER];

pub const STATUS_NONE: &str = "";
pub const STATUS_PICKED: &str = "picked";
pub const STATUS_FINAL: &str = "final";
pub const STATUS_REJECTED: &str = "rejected";
pub const STATUSES: [&str; 4] = [STATUS_NONE, STATUS_PICKED, STATUS_FINAL, STATUS_REJECTED];

pub const THUMB_PENDING: &str = "pending";
pub const THUMB_READY: &str = "ready";
pub const THUMB_NONE: &str = "none";
pub const THUMB_FAILED: &str = "failed";

pub const BY_WORKER: &str = "worker";
pub const BY_COORDINATOR: &str = "coordinator";
pub const BY_HUMAN: &str = "human";

/// How many characters of a prompt the light row carries.
pub const PROMPT_HEAD_CHARS: usize = 160;

/// Who filed an asset or a spend: from the caller's identity, never from a tool's arguments (§10.3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MadeBy {
    /// `"worker"` | `"coordinator"` | `"human"`.
    pub kind: String,
    /// Empty for the human.
    pub tab_id: String,
    pub role: String,
    pub worker_title: String,
    /// The worker's board number; 0 for anyone else.
    pub badge: u32,
}

impl MadeBy {
    pub fn human() -> Self {
        Self { kind: BY_HUMAN.to_string(), ..Self::default() }
    }
}

/// What made an asset, so it can be made again (§10.2). Floats only at the tool boundary: the
/// ledger keeps micro-dollars (plan D3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Recipe {
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub negative: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seed: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Asset {
    pub id: String,
    /// One of `KINDS`, but read as any string.
    pub kind: String,
    /// The stored file's name under `gallery/files/`: `<id>.<ext>`.
    pub file: String,
    pub mime: String,
    pub bytes: u64,
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub title: String,
    pub caption: Option<String>,
    pub made_by: MadeBy,
    pub task_id: Option<String>,
    /// The canonical path it was filed from.
    pub source_path: String,
    pub recipe: Option<Recipe>,
    /// What it was derived from, in the order given.
    pub parents: Vec<String>,
    /// Children not in the trash.
    pub children: u32,
    /// `""` | `"picked"` | `"final"` | `"rejected"`.
    pub status: String,
    pub starred: bool,
    pub tags: Vec<String>,
    pub collections: Vec<String>,
    /// `"pending"` | `"ready"` | `"none"` | `"failed"`.
    pub thumb: String,
    /// The thumbnailer's `strip`/`wave` and whatever a later build adds.
    pub extras: Map<String, Value>,
    pub created_at: String,
    pub created_ms: i64,
    /// Set while it is in the trash.
    pub deleted_at: Option<String>,
    /// The trash was emptied: the file and thumbnails are gone, the row stays so lineage,
    /// receipts and the ledger hold (plan D11).
    pub purged: bool,
}

impl Asset {
    pub fn in_trash(&self) -> bool {
        self.deleted_at.is_some() || self.purged
    }
}

/// The light form a list carries: no recipe, the prompt's head.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AssetRow {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub mime: String,
    pub bytes: u64,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub title: String,
    pub made_by: MadeBy,
    pub task_id: Option<String>,
    pub prompt_head: Option<String>,
    pub model: Option<String>,
    pub cost_micros: i64,
    pub parents: Vec<String>,
    pub children: u32,
    pub status: String,
    pub starred: bool,
    pub tags: Vec<String>,
    pub collections: Vec<String>,
    pub thumb: String,
    pub extras: Map<String, Value>,
    pub created_at: String,
    pub created_ms: i64,
    pub deleted_at: Option<String>,
}

impl From<&Asset> for AssetRow {
    fn from(a: &Asset) -> Self {
        let recipe = a.recipe.as_ref();
        Self {
            id: a.id.clone(),
            kind: a.kind.clone(),
            file: a.file.clone(),
            mime: a.mime.clone(),
            bytes: a.bytes,
            width: a.width,
            height: a.height,
            duration_ms: a.duration_ms,
            title: a.title.clone(),
            made_by: a.made_by.clone(),
            task_id: a.task_id.clone(),
            prompt_head: recipe.and_then(|r| r.prompt.as_deref()).map(|p| p.chars().take(PROMPT_HEAD_CHARS).collect()),
            model: recipe.and_then(|r| r.model.clone()),
            cost_micros: recipe.and_then(|r| r.cost_usd).map(usd_to_micros).unwrap_or(0),
            parents: a.parents.clone(),
            children: a.children,
            status: a.status.clone(),
            starred: a.starred,
            tags: a.tags.clone(),
            collections: a.collections.clone(),
            thumb: a.thumb.clone(),
            extras: a.extras.clone(),
            created_at: a.created_at.clone(),
            created_ms: a.created_ms,
            deleted_at: a.deleted_at.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Collection {
    pub id: String,
    pub name: String,
    /// In order.
    pub asset_ids: Vec<String>,
    pub cover: Option<String>,
    pub created_at: String,
}

/// One paid render, kept or not: the ledger is append-only and the only source of the total
/// (plan D3).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Spend {
    pub id: String,
    pub asset_id: Option<String>,
    pub task_id: Option<String>,
    pub made_by: MadeBy,
    pub tab_id: Option<String>,
    pub cost_micros: i64,
    pub tool: String,
    pub model: String,
    pub at: String,
    pub at_ms: i64,
}

/// What `insert_asset` files. The id is minted first: the stored file is named by it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewAsset {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub mime: String,
    pub bytes: u64,
    pub sha256: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub title: String,
    pub caption: Option<String>,
    pub made_by: MadeBy,
    pub task_id: Option<String>,
    pub source_path: String,
    pub recipe: Option<Recipe>,
    pub parents: Vec<String>,
    pub tags: Vec<String>,
    /// A collection id the asset joins, at its end.
    pub collection: Option<String>,
    /// Empty means `"pending"`.
    pub thumb: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct NewSpend {
    pub asset_id: Option<String>,
    pub task_id: Option<String>,
    pub made_by: MadeBy,
    pub cost_micros: i64,
    pub tool: String,
    pub model: String,
}

/// A list query. Every field narrows; None narrows nothing.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ListFilter {
    pub kind: Option<String>,
    /// A made-by kind (`worker`, `coordinator`, `human`) or a tab id.
    pub made_by: Option<String>,
    pub task_id: Option<String>,
    /// A collection id or name.
    pub collection: Option<String>,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub starred: Option<bool>,
    /// Matched as a substring of the title, the recipe's prompt and the tags (plan D17).
    pub query: Option<String>,
    /// Filed at or after this epoch ms (a chat's inline assets, plan D16).
    pub since_ms: Option<i64>,
    pub include_trash: bool,
    pub limit: Option<u32>,
}

/// Tags are added and removed, never replaced, so two agents never clobber each other (plan D12).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TagChange {
    pub tags: Vec<String>,
    pub untag: Vec<String>,
    pub status: Option<String>,
    pub starred: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CollectChange {
    /// A collection id or name.
    pub collection: String,
    /// Make it when no collection has that name.
    pub create: bool,
    pub add: Vec<String>,
    pub remove: Vec<String>,
    /// The members' full new order: a permutation of them, nothing else.
    pub order: Option<Vec<String>>,
    /// A member; `""` clears it.
    pub cover: Option<String>,
}

/// What the thumbnailer learned about one file. `thumb` is the run's outcome; the three flags,
/// kept in `extras`, say which pictures exist: `<id>.png`, `<id>.strip.jpg`, `<id>.wave.png`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaMeta {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    pub thumb: String,
    pub poster: bool,
    pub strip: bool,
    pub wave: bool,
}

pub const ASSET_PREFIX: &str = "a-";
pub const COLLECTION_PREFIX: &str = "c-";
pub const SPEND_PREFIX: &str = "s-";

/// `<prefix>` plus 12 hex characters of a fresh uuid.
pub fn mint(prefix: &str) -> String {
    format!("{prefix}{}", &uuid::Uuid::new_v4().simple().to_string()[..12])
}

fn is_id(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix).is_some_and(|h| h.len() == 12 && h.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
}

pub fn is_asset_id(s: &str) -> bool {
    is_id(s, ASSET_PREFIX)
}

pub fn is_collection_id(s: &str) -> bool {
    is_id(s, COLLECTION_PREFIX)
}

/// Dollars to the ledger's micro-dollars, rounded once here.
pub fn usd_to_micros(usd: f64) -> i64 {
    (usd * 1_000_000.0).round() as i64
}

/// A tag as stored: trimmed, lowercased, control characters out. Empty means none.
pub fn normalize_tag(t: &str) -> String {
    t.chars().filter(|c| !c.is_control()).collect::<String>().trim().to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_a_prefix_and_twelve_hex() {
        let a = mint(ASSET_PREFIX);
        assert!(is_asset_id(&a), "{a}");
        assert!(!is_collection_id(&a));
        assert!(is_collection_id(&mint(COLLECTION_PREFIX)));
        for bad in ["a-", "a-0123456789ab0", "a-0123456789AB", "a-../../etc/pa", "c-0123456789ab"] {
            assert!(!is_asset_id(bad), "{bad}");
        }
    }

    #[test]
    fn micros_round_once_and_tags_normalize() {
        assert_eq!(usd_to_micros(0.003), 3_000);
        assert_eq!(usd_to_micros(10.4), 10_400_000);
        assert_eq!(normalize_tag("  Hero\u{1f}Shot "), "heroshot");
    }

    #[test]
    fn a_sparse_asset_loads_and_a_row_takes_the_prompt_head() {
        let a: Asset = serde_json::from_str(r#"{"id":"a-0123456789ab","later":1}"#).unwrap();
        assert!(a.parents.is_empty() && !a.purged && a.made_by.kind.is_empty());
        let long = "x".repeat(400);
        let a = Asset { recipe: Some(Recipe { prompt: Some(long), cost_usd: Some(0.25), ..Recipe::default() }), ..a };
        let row = AssetRow::from(&a);
        assert_eq!(row.prompt_head.unwrap().chars().count(), PROMPT_HEAD_CHARS);
        assert_eq!(row.cost_micros, 250_000);
    }
}
