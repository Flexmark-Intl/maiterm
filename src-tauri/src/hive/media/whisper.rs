//! whisper.cpp's default model (docs/hive-media.md §6, §9): after `whisper-cpp` installs, maiTerm
//! itself downloads `ggml-base.en` from huggingface.co into its own models folder,
//! `<app data>/models/whisper/`. maiTerm runs outside every sandbox, so no Hive worker needs
//! huggingface.co, and the brief names the path (`model_path`).
//!
//! - **Pinned.** The model's size and SHA-256 are constants here (`MODEL_SIZE`, `MODEL_SHA256`,
//!   read from huggingface.co on 2026-11-05), so the file is checked even if huggingface.co stops
//!   publishing them. Published facts that disagree with the pin fail the download, and nothing
//!   is kept: a different file is a catalogue change, never a silent swap.
//! - **Idempotent.** A model that is there, with the record of its verified download beside it
//!   (`ggml-base.en.bin.json`) naming the pinned size and hash, is ready: no request is made.
//! - **Resumable.** Bytes go to `ggml-base.en.bin.part`, beside a record of what it is a part of
//!   (the pinned size and hash). A later run asks for the rest with a Range; a
//!   server that sends the whole file instead, a part of a different file, or a refused range
//!   starts over. A failed run keeps its part for the next one.
//! - **Verified.** huggingface.co publishes an LFS file's size and SHA-256 on its `resolve` URL
//!   (`x-linked-size`, `x-linked-etag` in double quotes, on its 302 to the CDN,
//!   `us.aws.cdn.hf.co`). The `resolve` request is not followed, so those headers are read, and
//!   the bytes are fetched from the 302's `location` (maiTerm runs outside every sandbox, so the
//!   CDN host needs no grant). The hash is computed over the bytes received and must equal the
//!   pin before the part becomes the model; a mismatch deletes the part.
//! - **One at a time.** A second start while one runs joins it (`hive-media-model-progress`).
//!
//! The HTTP side is the `Fetch` trait, so the tests run on a stub with no network.

use std::fs::{self, File, OpenOptions};
use std::future::Future;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};

/// The default model, as whisper.cpp names it.
pub const MODEL: &str = "ggml-base.en";
pub const MODEL_FILE: &str = "ggml-base.en.bin";
pub const MODEL_URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin";
/// `ggml-base.en.bin`'s size and SHA-256, as huggingface.co published them on 2026-11-05
/// (`x-linked-size`, `x-linked-etag` on the `resolve` URL's 302).
pub const MODEL_SIZE: u64 = 147_964_211;
pub const MODEL_SHA256: &str = "a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002";

/// What a download must be: the size and SHA-256 (lowercase hex) it is checked against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    pub size: u64,
    pub sha256: String,
}

/// The default model's pin.
pub fn base_en() -> Pin {
    Pin { size: MODEL_SIZE, sha256: MODEL_SHA256.to_string() }
}
/// To every window, while it downloads and when it ends: `ModelStatus`.
pub const PROGRESS_EVENT: &str = "hive-media-model-progress";

/// `<app data>/models/whisper`: maiTerm's own, never a hardcoded data folder.
pub fn models_dir() -> Option<PathBuf> {
    crate::state::persistence::app_data_dir().map(|d| d.join("models").join("whisper"))
}

/// What a finished download recorded beside the model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verified {
    pub size: u64,
    /// Computed over the bytes received.
    pub sha256: String,
    /// Checked against the pin (always, since the pin; an older record may say false).
    #[serde(default)]
    pub checked_size: bool,
    #[serde(default)]
    pub checked_sha256: bool,
    #[serde(default)]
    pub url: String,
}

/// What a part is a part of: the pin it was started against.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct PartOf {
    size: Option<u64>,
    sha256: Option<String>,
}

fn record_of(file: &Path) -> PathBuf {
    file.with_file_name(format!("{}.json", file.file_name().and_then(|n| n.to_str()).unwrap_or(MODEL_FILE)))
}

fn read_json<T: for<'de> Deserialize<'de>>(p: &Path) -> Option<T> {
    fs::read(p).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

fn write_json<T: Serialize>(p: &Path, v: &T) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(v).map_err(|e| e.to_string())?;
    fs::write(p, bytes).map_err(|e| format!("writing {}: {e}", p.display()))
}

/// The model in `dir`, when a verified download of exactly `pin` put it there and it is still
/// whole.
pub fn ready_in(dir: &Path, pin: &Pin) -> Option<PathBuf> {
    let file = dir.join(MODEL_FILE);
    let v: Verified = read_json(&record_of(&file))?;
    let len = fs::metadata(&file).ok().filter(|m| m.is_file())?.len();
    (len == pin.size && v.size == pin.size && v.sha256 == pin.sha256).then_some(file)
}

/// The model's path, when it is ready. For the brief (docs/hive-media.md §10).
pub fn model_path() -> Option<PathBuf> {
    models_dir().and_then(|d| ready_in(&d, &base_en()))
}

// ─── The HTTP side ──────────────────────────────────────────────────────────

/// What huggingface.co says about the file before any byte of it is sent.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Published {
    pub size: Option<u64>,
    /// Lowercase hex, only when it is a SHA-256 (64 hex digits).
    pub sha256: Option<String>,
    /// Where the bytes are (the redirect's target, or the url itself).
    pub location: String,
}

pub trait Body: Send {
    /// Where the bytes start: the offset asked for when the server honoured the Range, 0 when
    /// it sends the whole file.
    fn starts_at(&self) -> u64;
    /// The next bytes, or `None` at the end.
    fn chunk(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send;
}

pub trait Fetch: Send + Sync {
    type Body: Body;
    fn published(&self, url: &str) -> impl Future<Output = Result<Published, String>> + Send;
    /// The bytes at `location` from `from` on (a Range when `from > 0`).
    fn open(&self, location: &str, from: u64) -> impl Future<Output = Result<Self::Body, String>> + Send;
}

/// The model in `dir`, downloading what is missing of it and checking it against `pin`.
/// `progress(done, total)` is called as bytes arrive.
pub async fn ensure<F: Fetch>(dir: &Path, url: &str, pin: &Pin, fetch: &F, progress: &mut (dyn FnMut(u64, Option<u64>) + Send)) -> Result<PathBuf, String> {
    if let Some(file) = ready_in(dir, pin) {
        return Ok(file);
    }
    fs::create_dir_all(dir).map_err(|e| format!("making {}: {e}", dir.display()))?;
    let file = dir.join(MODEL_FILE);
    let part = dir.join(format!("{MODEL_FILE}.part"));
    let part_of = dir.join(format!("{MODEL_FILE}.part.json"));
    let published = fetch.published(url).await?;
    if published.size.is_some_and(|s| s != pin.size) || published.sha256.as_ref().is_some_and(|h| h != &pin.sha256) {
        let _ = fs::remove_file(&part);
        let _ = fs::remove_file(&part_of);
        return Err(format!(
            "huggingface.co now publishes a different {MODEL} ({} bytes, SHA-256 {}) from the one maiTerm checks against ({} bytes, SHA-256 {}). Nothing was downloaded or kept.",
            published.size.map_or("?".to_string(), |s| s.to_string()),
            published.sha256.as_deref().unwrap_or("?"),
            pin.size,
            pin.sha256,
        ));
    }
    if published.size.is_none() || published.sha256.is_none() {
        log::info!("hive media: huggingface.co didn't publish {MODEL}'s size or hash; checking against the pin alone");
    }
    // From here on the pin is what the bytes must be.
    let published = Published { size: Some(pin.size), sha256: Some(pin.sha256.clone()), location: published.location };
    let want = PartOf { size: published.size, sha256: published.sha256.clone() };

    let same_file = read_json::<PartOf>(&part_of).is_some_and(|p| p == want);
    let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
    let from = if same_file && published.size.is_none_or(|s| have <= s) { have } else { 0 };
    let result = match download(&part, &part_of, &want, &published, from, fetch, progress).await {
        // A refused range (a CDN that lost the object, a part longer than it should be): once
        // more from the start.
        Err(e) if from > 0 => {
            log::info!("hive media: resuming {MODEL} at {from} failed ({e}); starting over");
            download(&part, &part_of, &want, &published, 0, fetch, progress).await
        }
        other => other,
    };
    let (size, sha256) = result?;
    let mismatch = published.size.is_some_and(|s| s != size) || published.sha256.as_ref().is_some_and(|h| h != &sha256);
    if mismatch {
        let _ = fs::remove_file(&part);
        let _ = fs::remove_file(&part_of);
        return Err(format!(
            "The downloaded {MODEL} ({size} bytes, SHA-256 {sha256}) doesn't match the one maiTerm checks against ({} bytes, SHA-256 {}). It was deleted; try again.",
            pin.size, pin.sha256,
        ));
    }
    fs::rename(&part, &file).map_err(|e| format!("moving the model into place: {e}"))?;
    let verified = Verified { size, sha256, checked_size: true, checked_sha256: true, url: url.to_string() };
    write_json(&record_of(&file), &verified)?;
    let _ = fs::remove_file(&part_of);
    Ok(file)
}

/// Bytes from `from` on into `part`, hashed as they go (with the part's own bytes first, when it
/// resumes). Answers the part's size and SHA-256.
async fn download<F: Fetch>(part: &Path, part_of: &Path, want: &PartOf, published: &Published, from: u64, fetch: &F, progress: &mut (dyn FnMut(u64, Option<u64>) + Send)) -> Result<(u64, String), String> {
    write_json(part_of, want)?;
    let mut hasher = Sha256::new();
    let mut done = 0u64;
    let mut out = if from > 0 {
        let mut f = File::open(part).map_err(|e| format!("reading {}: {e}", part.display()))?;
        let mut buf = vec![0u8; 1 << 16];
        loop {
            let n = f.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            done += n as u64;
        }
        OpenOptions::new().append(true).open(part).map_err(|e| e.to_string())?
    } else {
        File::create(part).map_err(|e| format!("making {}: {e}", part.display()))?
    };
    if published.size == Some(done) {
        return Ok((done, hex(&hasher.finalize())));
    }
    let mut body = fetch.open(&published.location, from).await?;
    if from > 0 && body.starts_at() == 0 {
        // The whole file came instead of the rest of it.
        out = File::create(part).map_err(|e| e.to_string())?;
        hasher = Sha256::new();
        done = 0;
    } else if body.starts_at() != from {
        return Err(format!("huggingface.co sent bytes from {} when {from} was asked for", body.starts_at()));
    }
    progress(done, published.size);
    while let Some(bytes) = body.chunk().await? {
        out.write_all(&bytes).map_err(|e| format!("writing the model: {e}"))?;
        hasher.update(&bytes);
        done += bytes.len() as u64;
        if published.size.is_some_and(|s| done > s) {
            return Err(format!("huggingface.co sent more than the {} bytes it published", published.size.unwrap_or_default()));
        }
        progress(done, published.size);
    }
    out.flush().map_err(|e| e.to_string())?;
    Ok((done, hex(&hasher.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// An `X-Linked-Etag` (or `ETag`) value as a SHA-256: quotes and a weak `W/` dropped, and only 64
/// hex digits count.
pub fn etag_sha256(etag: &str) -> Option<String> {
    let t = etag.trim().trim_start_matches("W/").trim_matches('"').to_ascii_lowercase();
    (t.len() == 64 && t.bytes().all(|b| b.is_ascii_hexdigit())).then_some(t)
}

/// What a `resolve` HEAD's answer publishes. Header names are case-insensitive
/// (`HeaderMap`); huggingface.co sends them lowercase over HTTP/2. A 302 (an LFS file) carries
/// `x-linked-size` and `x-linked-etag` and points at the CDN; a 200 is the file itself.
pub fn published_from(url: &str, status: reqwest::StatusCode, headers: &reqwest::header::HeaderMap) -> Result<Published, String> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string);
    let location = if status.is_redirection() {
        let to = header("location").ok_or("huggingface.co redirected without saying where")?;
        reqwest::Url::parse(url).and_then(|base| base.join(&to)).map_err(|e| e.to_string())?.to_string()
    } else if status.is_success() {
        url.to_string()
    } else {
        return Err(format!("huggingface.co answered {status} for {MODEL}"));
    };
    let size = header("x-linked-size").or_else(|| status.is_success().then(|| header("content-length")).flatten()).and_then(|s| s.trim().parse().ok());
    let sha256 = header("x-linked-etag").or_else(|| header("etag")).and_then(|e| etag_sha256(&e));
    Ok(Published { size, sha256, location })
}

/// huggingface.co over reqwest.
pub struct Hf {
    client: reqwest::Client,
}

impl Hf {
    pub fn new() -> Result<Hf, String> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .connect_timeout(Duration::from_secs(20))
            .read_timeout(Duration::from_secs(60))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Hf { client })
    }
}

pub struct HfBody {
    resp: reqwest::Response,
    starts_at: u64,
}

impl Body for HfBody {
    fn starts_at(&self) -> u64 {
        self.starts_at
    }
    fn chunk(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send {
        async move { self.resp.chunk().await.map(|c| c.map(|b| b.to_vec())).map_err(|e| format!("downloading from huggingface.co: {e}")) }
    }
}

impl Fetch for Hf {
    type Body = HfBody;

    fn published(&self, url: &str) -> impl Future<Output = Result<Published, String>> + Send {
        let url = url.to_string();
        async move {
            // Not followed: the facts are on the redirect itself.
            let client = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).connect_timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
            let resp = client.head(&url).send().await.map_err(|e| format!("asking huggingface.co about {MODEL}: {e}"))?;
            published_from(&url, resp.status(), resp.headers())
        }
    }

    fn open(&self, location: &str, from: u64) -> impl Future<Output = Result<HfBody, String>> + Send {
        let mut req = self.client.get(location);
        if from > 0 {
            req = req.header(reqwest::header::RANGE, format!("bytes={from}-"));
        }
        async move {
            let resp = req.send().await.map_err(|e| format!("downloading {MODEL}: {e}"))?;
            let starts_at = match resp.status().as_u16() {
                206 => from,
                200 => 0,
                s => return Err(format!("huggingface.co answered {s} for {MODEL}")),
            };
            Ok(HfBody { resp, starts_at })
        }
    }
}

// ─── The download maiTerm runs ──────────────────────────────────────────────

/// The model's state, as the sheet shows it and `hive-media-model-progress` carries it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ModelStatus {
    pub model: String,
    /// `ready` | `downloading` | `missing` | `failed`.
    pub state: String,
    pub path: Option<String>,
    pub done: u64,
    pub total: Option<u64>,
    /// Why the last download failed, in plain words.
    pub error: Option<String>,
}

impl ModelStatus {
    fn new(state: &str) -> ModelStatus {
        ModelStatus { model: MODEL.to_string(), state: state.to_string(), path: None, done: 0, total: None, error: None }
    }
}

/// The download in flight, or the last one's failure.
static RUN: LazyLock<Mutex<Option<ModelStatus>>> = LazyLock::new(Default::default);

fn run() -> MutexGuard<'static, Option<ModelStatus>> {
    RUN.lock().unwrap_or_else(|e| e.into_inner())
}

/// The model's state now: ready on disk, downloading, failed last time, or missing.
pub fn status() -> ModelStatus {
    if let Some(p) = model_path() {
        let mut s = ModelStatus::new("ready");
        s.path = Some(p.to_string_lossy().to_string());
        return s;
    }
    run().clone().unwrap_or_else(|| ModelStatus::new("missing"))
}

/// Starts the download unless the model is ready or one is already running; answers the state.
pub fn start_default(handle: AppHandle) -> ModelStatus {
    let now = status();
    if now.state == "ready" {
        return now;
    }
    let Some(dir) = models_dir() else {
        let mut s = ModelStatus::new("failed");
        s.error = Some("maiTerm has no data folder to keep the model in.".to_string());
        return s;
    };
    let started = ModelStatus::new("downloading");
    {
        // Checked and claimed under one lock: two starts never write one part.
        let mut r = run();
        if let Some(s) = r.as_ref().filter(|s| s.state == "downloading") {
            return s.clone();
        }
        *r = Some(started.clone());
    }
    tauri::async_runtime::spawn(async move {
        let emit = |s: &ModelStatus| {
            if let Err(e) = handle.emit(PROGRESS_EVENT, s) {
                log::warn!("hive media: {PROGRESS_EVENT} not sent: {e}");
            }
        };
        let mut last = (Instant::now(), 0u64);
        let mut progress = |done: u64, total: Option<u64>| {
            let step = total.map_or(1 << 20, |t| (t / 100).max(1));
            let mut s = ModelStatus::new("downloading");
            (s.done, s.total) = (done, total);
            *run() = Some(s.clone());
            if done.saturating_sub(last.1) >= step || last.0.elapsed() >= Duration::from_millis(500) || Some(done) == total {
                last = (Instant::now(), done);
                emit(&s);
            }
        };
        let result = match Hf::new() {
            Ok(hf) => ensure(&dir, MODEL_URL, &base_en(), &hf, &mut progress).await,
            Err(e) => Err(e),
        };
        let end = match result {
            Ok(path) => {
                log::info!("hive media: {MODEL} ready at {}", path.display());
                *run() = None;
                status()
            }
            Err(e) => {
                log::warn!("hive media: {MODEL} not downloaded: {e}");
                let mut s = run().clone().unwrap_or_else(|| ModelStatus::new("failed"));
                s.state = "failed".to_string();
                s.error = Some(e);
                *run() = Some(s.clone());
                s
            }
        };
        emit(&end);
    });
    started
}

/// Where whisper.cpp's default model is, once maiTerm has downloaded and verified it.
#[tauri::command]
pub fn hive_media_whisper_model_path() -> Option<String> {
    model_path().map(|p| p.to_string_lossy().to_string())
}

/// The model's state, starting its download when it is neither there nor on its way (a retry
/// resumes what a failed run left). Progress and the end come as `hive-media-model-progress`.
#[tauri::command]
pub async fn hive_media_whisper_model_download(app: AppHandle) -> Result<ModelStatus, String> {
    Ok(start_default(app))
}

#[cfg(test)]
mod tests;
