//! Files an agent sends to a paired phone.
//!
//! An agent calls the `sendFilesToPhone` MCP tool with absolute paths; the bytes are COPIED into
//! a per-install store here, and the phone fetches them over the same authenticated LAN listener
//! as everything else. Two surfaces on the wire: a synthesized `kind: "asset"` turn in the tab's
//! transcript (so a file appears where it was sent, in order), and `GET /assets`, a newest-first
//! list across every tab (the phone's Files view).
//!
//! **Copied, never hardlinked.** A hardlink shares the inode, so an agent that rewrites or
//! truncates the file in place would retroactively change a file someone had already been sent —
//! silently, and unreproducibly. Disk is cheaper than that.
//!
//! **maiTerm decodes nothing.** No image or video crate, no ffprobe shell-out: no dimensions, no
//! duration, no thumbnails, and no thumbnail endpoint. The phone plays a downloaded file from its
//! own container, where the media element is same-origin, so it reads duration/dimensions off
//! `loadedmetadata` and draws its own poster frame — for free, and without maiTerm acquiring a
//! decoder it would then have to keep working on every platform.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Largest single file accepted. Past this the tool REFUSES with the size, rather than staging
/// something the phone would only discover was unreasonable while downloading it.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024 * 1024;

/// Total bytes of stored blobs. Past this the oldest blobs are deleted (see `evict`).
const MAX_STORE_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// How many records the manifest keeps. A record outlives its blob — that is what `available`
/// exists for — but not forever. Dropping the record entirely is the safe direction: transcript
/// turns are BUILT from the manifest, so a dropped record simply stops producing a row, and a
/// dangling reference to a file we know nothing about can never reach the phone.
const MAX_RECORDS: usize = 2000;

/// One stored file, as the wire sees it (`FileAsset` in docs/mailink-protocol.md §4.x).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssetRecord {
    pub asset_id: String,
    /// Groups the files of ONE `sendFilesToPhone` call into a single transcript turn.
    pub batch_id: String,
    pub name: String,
    pub mime: String,
    pub bytes: u64,
    /// Epoch ms, when the agent sent it.
    pub ts: u64,
    #[serde(rename = "tabId")]
    pub tab_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
    /// Whether the bytes are still on disk. Always present, never inferred by the client from a
    /// failed fetch: a 404 cannot distinguish an eviction from a broken server from an expired
    /// token, and a client forced to guess guesses wrong. `false` renders as a tombstone.
    pub available: bool,
}

fn assets_dir() -> Option<PathBuf> {
    super::mailink_dir().map(|d| d.join("assets"))
}

fn index_path() -> Option<PathBuf> {
    assets_dir().map(|d| d.join("index.json"))
}

fn blob_path(asset_id: &str) -> Option<PathBuf> {
    assets_dir().map(|d| d.join(asset_id))
}

/// Serializes every manifest read-modify-write. Sends are rare and human-paced; a single mutex is
/// the right amount of machinery.
static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Bumped on every successful manifest write. The WS streamer polls THIS rather than the file's
/// mtime: mtime has millisecond granularity, so two writes inside one millisecond compared equal
/// and the second batch was never streamed. A counter cannot tie. Also saves a stat per tick.
static REVISION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The manifest's current revision. Changes iff something was stored or evicted.
pub fn revision() -> u64 {
    REVISION.load(std::sync::atomic::Ordering::Acquire)
}

fn load_unlocked() -> Vec<AssetRecord> {
    let Some(path) = index_path() else { return Vec::new() };
    let Ok(raw) = std::fs::read(&path) else { return Vec::new() }; // absent = empty store
    match serde_json::from_slice(&raw) {
        Ok(records) => records,
        Err(e) => {
            // NEVER silently default here. Returning an empty Vec would let the next `store` write
            // a one-record manifest over the wreckage, erasing every asset row — and with them
            // every `available:false` tombstone and every transcript turn, since those are BUILT
            // from this file. Move it aside instead: loud, and recoverable by hand.
            let aside = path.with_extension(format!("corrupt.{}.json", super::now_ms()));
            let moved = std::fs::rename(&path, &aside).is_ok();
            log::error!(
                "[maiLink] asset manifest is unreadable ({e}) — {} asset records are unavailable. \
                 The blobs are still on disk. {}",
                raw.len(),
                if moved { format!("Kept the bad file at {}", aside.display()) } else { "Could not move it aside.".into() }
            );
            Vec::new()
        }
    }
}

/// Write the manifest so a crash mid-write cannot destroy it.
///
/// `std::fs::write` truncates first, so an interruption anywhere in the span leaves a short or
/// empty file — and this app tracks unclean shutdowns as a recurring event, not a hypothetical.
/// Temp-then-rename makes the replacement atomic: a reader sees the old manifest or the new one.
fn save_unlocked(records: &[AssetRecord]) -> Result<(), String> {
    let dir = assets_dir().ok_or("no data dir")?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("create assets dir: {e}"))?;
    let path = index_path().ok_or("no data dir")?;
    let tmp = path.with_extension("json.tmp");
    let json = serde_json::to_vec_pretty(records).map_err(|e| format!("serialize index: {e}"))?;
    std::fs::write(&tmp, json).map_err(|e| format!("write index: {e}"))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("replace index: {e}"))?;
    REVISION.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
    Ok(())
}

/// Delete the oldest blobs until the store fits, then drop the oldest records past `MAX_RECORDS`.
///
/// Oldest-first rather than least-recently-used on purpose: tracking access would mean writing the
/// manifest on every byte-range request a phone makes while scrubbing a video, which is a lot of
/// disk churn to protect a case (an old file that is still being opened) that a timeline-shaped
/// feature barely has. Eviction only clears `available`; the record stays so the row can say what
/// happened instead of 404ing.
fn evict(records: &mut Vec<AssetRecord>) {
    let mut live: u64 = records.iter().filter(|r| r.available).map(|r| r.bytes).sum();
    if live > MAX_STORE_BYTES {
        // Oldest first.
        let mut order: Vec<usize> = (0..records.len()).collect();
        order.sort_by_key(|&i| records[i].ts);
        for i in order {
            if live <= MAX_STORE_BYTES {
                break;
            }
            if !records[i].available {
                continue;
            }
            if let Some(p) = blob_path(&records[i].asset_id) {
                let _ = std::fs::remove_file(p);
            }
            records[i].available = false;
            live = live.saturating_sub(records[i].bytes);
            log::info!(
                "[maiLink] asset store over cap — evicted {} ({} bytes)",
                records[i].name, records[i].bytes
            );
        }
    }
    if records.len() > MAX_RECORDS {
        records.sort_by_key(|r| r.ts);
        let drop_count = records.len() - MAX_RECORDS;
        for r in records.iter().take(drop_count) {
            if let Some(p) = blob_path(&r.asset_id) {
                let _ = std::fs::remove_file(p);
            }
        }
        records.drain(..drop_count);
    }
}

/// One file on its way into the store: already read, not yet recorded.
pub struct Incoming {
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Copy a whole SEND into the store as ONE manifest write.
///
/// Batch-at-a-time is not an optimization, it is the correctness requirement. A batch renders as
/// a single transcript turn under one `msg_id` (`asset_{batch_id}`), and the WS streamer's
/// seen-set treats that id as final once emitted. Storing file-by-file published the turn after
/// the first file, then mutated it under the same id — so on any send where two files straddle a
/// tick (every multi-file send on an SSH tab, where each fetch spawns ssh) the phone showed file
/// one and never heard about the rest, while the tool reported them all sent.
///
/// Per-file failures are returned alongside the successes rather than aborting: "send the files"
/// is usually plural and one unreadable path must not discard the others.
pub fn store_batch(
    tab_id: &str,
    batch_id: &str,
    files: Vec<Incoming>,
    caption: Option<&str>,
) -> (Vec<AssetRecord>, Vec<(String, String)>) {
    let mut stored = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let ts = super::now_ms();
    let caption = caption.map(str::to_string).filter(|c| !c.trim().is_empty());

    let dir = match assets_dir() {
        Some(d) => d,
        None => {
            let e = "no data directory".to_string();
            return (stored, files.into_iter().map(|f| (f.name, e.clone())).collect());
        }
    };
    if let Err(e) = std::fs::create_dir_all(&dir) {
        let e = format!("create assets dir: {e}");
        return (stored, files.into_iter().map(|f| (f.name, e.clone())).collect());
    }

    for file in files {
        let len = file.bytes.len() as u64;
        if len > MAX_FILE_BYTES {
            failed.push((
                file.name.clone(),
                format!(
                    "{} — over the {} limit for a file sent to a phone",
                    human_bytes(len),
                    human_bytes(MAX_FILE_BYTES)
                ),
            ));
            continue;
        }
        let asset_id = uuid::Uuid::new_v4().to_string();
        let Some(path) = blob_path(&asset_id) else { continue };
        if let Err(e) = std::fs::write(&path, &file.bytes) {
            failed.push((file.name.clone(), format!("could not store it: {e}")));
            continue;
        }
        stored.push(AssetRecord {
            asset_id,
            batch_id: batch_id.to_string(),
            name: file.name.clone(),
            mime: mime_for(&file.name).to_string(),
            bytes: len,
            ts,
            tab_id: tab_id.to_string(),
            caption: caption.clone(),
            available: true,
        });
    }

    if stored.is_empty() {
        return (stored, failed);
    }
    match LOCK.lock() {
        Ok(_guard) => {
            let mut records = load_unlocked();
            records.extend(stored.iter().cloned());
            evict(&mut records);
            if let Err(e) = save_unlocked(&records) {
                // The blobs are written but unrecorded — say so per file rather than reporting a
                // success the phone will never see.
                log::error!("[maiLink] asset manifest write failed: {e}");
                for r in &stored {
                    let _ = blob_path(&r.asset_id).map(std::fs::remove_file);
                    failed.push((r.name.clone(), format!("could not record it: {e}")));
                }
                stored.clear();
            }
        }
        Err(_) => {
            for r in &stored {
                let _ = blob_path(&r.asset_id).map(std::fs::remove_file);
                failed.push((r.name.clone(), "asset index lock poisoned".to_string()));
            }
            stored.clear();
        }
    }
    (stored, failed)
}

/// Newest-first across every tab, bounded. The phone's Files view.
pub fn list(limit: usize) -> Vec<AssetRecord> {
    let _guard = match LOCK.lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let mut records = load_unlocked();
    records.sort_by(|a, b| b.ts.cmp(&a.ts));
    records.truncate(limit);
    records
}

/// Every asset sent to one tab, oldest first — the transcript renders these in place.
///
/// One tab, one parse. For anything that wants MANY tabs, use `by_tab`: calling this in a loop
/// re-reads and re-parses the whole manifest per tab, which is the O(tabs × filesystem) shape that
/// produced the chat-list storm.
pub fn for_tab(tab_id: &str) -> Vec<AssetRecord> {
    let _guard = match LOCK.lock() {
        Ok(g) => g,
        Err(_) => return Vec::new(),
    };
    let mut records: Vec<AssetRecord> =
        load_unlocked().into_iter().filter(|r| r.tab_id == tab_id).collect();
    records.sort_by_key(|r| r.ts);
    records
}

/// Every asset, grouped by tab, oldest first within each — ONE read and ONE parse for the whole
/// roster. The connect-time seed and the WS streamer both walk every designated tab, and at the
/// 2000-record cap that is a ~500KB parse each if they ask per tab.
pub fn by_tab() -> std::collections::HashMap<String, Vec<AssetRecord>> {
    let mut out: std::collections::HashMap<String, Vec<AssetRecord>> =
        std::collections::HashMap::new();
    let Ok(_guard) = LOCK.lock() else { return out };
    let mut records = load_unlocked();
    records.sort_by_key(|r| r.ts);
    for r in records {
        out.entry(r.tab_id.clone()).or_default().push(r);
    }
    out
}

/// The record and the bytes' path, if the asset exists AND still has its blob.
pub fn resolve(asset_id: &str) -> Option<(AssetRecord, PathBuf)> {
    let _guard = LOCK.lock().ok()?;
    let record = load_unlocked().into_iter().find(|r| r.asset_id == asset_id)?;
    if !record.available {
        return None;
    }
    let path = blob_path(asset_id)?;
    path.is_file().then_some((record, path))
}

/// Extension → MIME, because maiTerm has no `mime_guess` and this does not justify one.
///
/// The type matters for two things only: the phone deciding whether it can preview inline, and the
/// `Content-Type` on the bytes response. Anything unrecognized is `application/octet-stream`,
/// which routes the file to the share sheet — the correct outcome for "we don't know what this
/// is", and never a wrong claim about what it is.
pub fn mime_for(name: &str) -> &'static str {
    let ext = name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "heic" => "image/heic",
        "svg" => "image/svg+xml",
        "bmp" => "image/bmp",
        "mp4" | "m4v" => "video/mp4",
        "mov" => "video/quicktime",
        "webm" => "video/webm",
        "mkv" => "video/x-matroska",
        "mp3" => "audio/mpeg",
        "m4a" => "audio/mp4",
        "wav" => "audio/wav",
        "aac" => "audio/aac",
        "pdf" => "application/pdf",
        "txt" | "log" | "text" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "csv" => "text/csv",
        "json" => "application/json",
        "xml" => "application/xml",
        "yaml" | "yml" => "application/yaml",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "ts" | "tsx" | "jsx" => "text/plain",
        "rs" | "py" | "go" | "rb" | "sh" | "toml" | "swift" | "kt" | "java" | "c" | "h"
        | "cpp" | "hpp" | "sql" => "text/plain",
        "zip" => "application/zip",
        "gz" | "tgz" => "application/gzip",
        "tar" => "application/x-tar",
        _ => "application/octet-stream",
    }
}

/// Sizes as a human reads them, for refusal messages an agent will relay to a person.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(id: &str, ts: u64, bytes: u64, available: bool) -> AssetRecord {
        AssetRecord {
            asset_id: id.into(),
            batch_id: "b".into(),
            name: format!("{id}.bin"),
            mime: "application/octet-stream".into(),
            bytes,
            ts,
            tab_id: "tab".into(),
            caption: None,
            available,
        }
    }

    #[test]
    fn an_evicted_asset_keeps_its_record_so_the_row_can_say_so() {
        // The whole point of `available`: the transcript turn is permanent, the bytes are not, and
        // a client that had to infer this from a 404 could not tell an eviction from a broken
        // server or an expired token.
        let mut records = vec![
            rec("old", 1_000, MAX_STORE_BYTES, true),
            rec("new", 2_000, MAX_STORE_BYTES, true),
        ];
        evict(&mut records);
        assert_eq!(records.len(), 2, "records survive eviction");
        assert!(!records[0].available, "the oldest blob goes first");
        assert!(records[1].available, "the newest is kept");
    }

    #[test]
    fn the_manifest_itself_is_bounded_and_drops_from_the_old_end() {
        // A record outlives its blob but not forever. Dropping it entirely is safe in a way
        // keeping a dangling id is not: transcript turns are BUILT from these records, so a
        // dropped record produces no row rather than a row pointing at nothing.
        let mut records: Vec<AssetRecord> =
            (0..MAX_RECORDS + 5).map(|i| rec(&format!("a{i}"), i as u64, 1, true)).collect();
        evict(&mut records);
        assert_eq!(records.len(), MAX_RECORDS);
        assert_eq!(records[0].asset_id, "a5", "the five oldest are gone");
    }

    #[test]
    fn an_unknown_extension_never_claims_a_type_it_cannot_know() {
        assert_eq!(mime_for("clip.MOV"), "video/quicktime", "case-insensitive");
        assert_eq!(mime_for("shot.png"), "image/png");
        assert_eq!(mime_for("notes.md"), "text/markdown");
        // No extension, and an extension we don't know, both fall to octet-stream — which routes
        // the file to the phone's share sheet instead of a preview that would fail.
        assert_eq!(mime_for("Makefile"), "application/octet-stream");
        assert_eq!(mime_for("archive.sbjkt"), "application/octet-stream");
    }

    #[test]
    fn a_whole_send_lands_in_one_manifest_write() {
        // The defect this replaced: storing file-by-file bumped the revision per FILE, and the
        // batch renders as ONE transcript turn under one msg_id. The streamer emitted that turn
        // after file one and then skipped it as already-seen when files two and three arrived —
        // so a multi-file send showed one file on the phone while the tool reported them all.
        let before = revision();
        let (stored, failed) = store_batch(
            "tab-batch-test",
            "batch-1",
            vec![
                Incoming { name: "a.txt".into(), bytes: b"one".to_vec() },
                Incoming { name: "b.txt".into(), bytes: b"two".to_vec() },
            ],
            Some("two files"),
        );
        assert_eq!(stored.len(), 2);
        assert!(failed.is_empty());
        assert_eq!(
            revision(),
            before + 1,
            "one send is one revision, however many files it carried"
        );
        // Same batch id and timestamp, so they group into a single turn.
        assert_eq!(stored[0].batch_id, stored[1].batch_id);
        assert_eq!(stored[0].ts, stored[1].ts);
        assert_eq!(stored[0].caption.as_deref(), Some("two files"));

        // Clean up: these are real files under the per-install dir.
        for r in &stored {
            if let Some(p) = blob_path(&r.asset_id) {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    #[test]
    fn a_send_that_stores_nothing_wakes_nobody() {
        // The revision is what the WS streamer polls. A call that stored nothing must not move
        // it, or every connected phone re-walks the roster for a change that isn't there.
        let before = revision();
        let (stored, failed) = store_batch("tab-empty-test", "batch-3", Vec::new(), None);
        assert!(stored.is_empty() && failed.is_empty());
        assert_eq!(revision(), before, "no write, no revision");
    }

    #[test]
    fn refusal_messages_read_like_a_person_wrote_them() {
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
        assert_eq!(human_bytes(MAX_FILE_BYTES), "1.0 GB");
    }
}
