//! The gallery (docs/hive.md §10): everything a Hive makes that isn't a commit, one Rust-owned
//! store per Hive under `<hive>/gallery/`: immutable files (`files/<id>.<ext>`, read-only),
//! thumbnails, a SQLite index (`store`) and the spend ledger in it. `ingest` decides which paths
//! a caller may file from and makes the copy; `model` holds the records; `tools` is what agents
//! call (step 8b); `thumbs` makes the pictures and `serve` answers the webview's `gallery://`
//! requests, by asset id only (8c).

pub mod ingest;
pub mod model;
pub mod serve;
pub mod store;
pub mod thumbs;
pub mod tools;

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, LazyLock};

use parking_lot::Mutex;

pub use store::Gallery;

use super::paths::{self, HivePaths};

/// The stored files, by asset id.
pub const FILES: &str = "files";
/// Thumbnails, posters, strips and waveforms: `<id>.png`, `<id>.strip.jpg`, `<id>.wave.png`.
pub const THUMBS: &str = "thumbs";
/// Where a purge moves files before it deletes them (`Gallery::purge_trash`).
pub const TRASH: &str = "trash";
/// The thumbnailer's per-asset output: a dot name, so no folder name can be it.
pub const TMP: &str = ".tmp";
pub const INDEX: &str = "index.sqlite";

/// To the Hive's window when assets are filed or changed: `{workspace_id, hive_id, ids}`.
pub const GALLERY_CHANGED_EVENT: &str = "hive-gallery-changed";

/// One open gallery per Hive in the process: the index has one writer.
static OPEN: LazyLock<Mutex<HashMap<String, Arc<Gallery>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// The Hive's gallery, opened once per process. Refused while the Hive's folder doesn't exist,
/// so a stale id never re-creates a deleted Hive's tree.
pub fn for_hive(hive_id: &str) -> Result<Arc<Gallery>, String> {
    let root = paths::hive_root(hive_id).ok_or_else(|| format!("\"{hive_id}\" isn't a Hive id"))?;
    if !root.is_dir() {
        return Err(format!("This Hive's folder is gone: {}", root.display()));
    }
    open_cached(hive_id, &HivePaths::new(root).gallery())
}

/// `for_hive` from the paths a caller already holds (a tool's `Caller`), refused the same way
/// once the Hive's folder is gone.
pub(crate) fn for_paths(hive_id: &str, paths: &HivePaths) -> Result<Arc<Gallery>, String> {
    if !paths.root.is_dir() {
        return Err(format!("This Hive's folder is gone: {}", paths.root.display()));
    }
    open_cached(hive_id, &paths.gallery())
}

/// `for_hive` over an explicit folder: a caller that already holds the Hive's paths, and tests'
/// scratch Hives. A cached handle on another folder (a test reusing a Hive id) is replaced.
pub(crate) fn open_cached(hive_id: &str, gallery_dir: &Path) -> Result<Arc<Gallery>, String> {
    let g = {
        let mut open = OPEN.lock();
        if let Some(g) = open.get(hive_id).filter(|g| g.root() == gallery_dir) {
            return Ok(g.clone());
        }
        let g = Arc::new(Gallery::open(gallery_dir)?);
        open.insert(hive_id.to_string(), g.clone());
        g
    };
    // First open in this process: whatever a quit left waiting for thumbnails is queued again.
    thumbs::resume(hive_id, &g);
    Ok(g)
}

/// The Hive's gallery only if it is open already: the thumbnailer never reopens one a Hive
/// delete just forgot.
pub(crate) fn cached(hive_id: &str) -> Option<Arc<Gallery>> {
    OPEN.lock().get(hive_id).cloned()
}

pub(crate) fn open_galleries() -> Vec<(String, Arc<Gallery>)> {
    OPEN.lock().iter().map(|(k, g)| (k.clone(), g.clone())).collect()
}

/// Drops the process's handle, so once in-flight calls finish no connection holds the index or
/// its WAL files (Hive delete, before its disk goes).
pub fn forget(hive_id: &str) {
    OPEN.lock().remove(hive_id);
}

pub(crate) fn is_open(hive_id: &str) -> bool {
    OPEN.lock().contains_key(hive_id)
}
