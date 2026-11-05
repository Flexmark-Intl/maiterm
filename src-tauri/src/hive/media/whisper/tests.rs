use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// huggingface.co as a test sees it: a file, what is published about it, and whether a Range is
/// honoured. Counts what it is asked.
struct Stub {
    data: Vec<u8>,
    published: Published,
    /// What the bytes must be (the pin the download is given).
    pin: Pin,
    honour_range: bool,
    /// Bytes sent before a failure, once (a dropped connection).
    fail_after: Mutex<Option<usize>>,
    asked: AtomicUsize,
    opened: Mutex<Vec<u64>>,
}

struct StubBody {
    chunks: VecDeque<Result<Vec<u8>, String>>,
    starts_at: u64,
}

impl Body for StubBody {
    fn starts_at(&self) -> u64 {
        self.starts_at
    }
    fn chunk(&mut self) -> impl Future<Output = Result<Option<Vec<u8>>, String>> + Send {
        let next = self.chunks.pop_front();
        async move { next.transpose() }
    }
}

impl Fetch for Stub {
    type Body = StubBody;
    fn published(&self, _url: &str) -> impl Future<Output = Result<Published, String>> + Send {
        self.asked.fetch_add(1, Ordering::SeqCst);
        let p = self.published.clone();
        async move { Ok(p) }
    }
    fn open(&self, location: &str, from: u64) -> impl Future<Output = Result<StubBody, String>> + Send {
        assert_eq!(location, "https://cdn.example/model");
        self.opened.lock().unwrap().push(from);
        let starts_at = if self.honour_range { from } else { 0 };
        let mut rest = self.data[starts_at as usize..].to_vec();
        let mut chunks = VecDeque::new();
        if let Some(n) = self.fail_after.lock().unwrap().take() {
            rest.truncate(n);
            chunks.extend(rest.chunks(7).map(|c| Ok(c.to_vec())));
            chunks.push_back(Err("connection reset".to_string()));
        } else {
            chunks.extend(rest.chunks(7).map(|c| Ok(c.to_vec())));
        }
        async move { Ok(StubBody { chunks, starts_at }) }
    }
}

fn model_bytes() -> Vec<u8> {
    (0..1000u32).map(|i| (i * 31 % 251) as u8).collect()
}

fn sha(b: &[u8]) -> String {
    hex(&Sha256::digest(b))
}

fn stub(data: Vec<u8>) -> Stub {
    let published = Published { size: Some(data.len() as u64), sha256: Some(sha(&data)), location: "https://cdn.example/model".to_string() };
    let pin = Pin { size: data.len() as u64, sha256: sha(&data) };
    Stub { data, published, pin, honour_range: true, fail_after: Mutex::new(None), asked: AtomicUsize::new(0), opened: Mutex::new(Vec::new()) }
}

fn temp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("hive-whisper-{tag}-{}", uuid::Uuid::new_v4().simple()));
    fs::create_dir_all(&d).unwrap();
    d
}

async fn get(dir: &Path, s: &Stub) -> Result<PathBuf, String> {
    let mut seen = Vec::new();
    let out = ensure(dir, MODEL_URL, &s.pin, s, &mut |done, total| seen.push((done, total))).await;
    if out.is_ok() && !seen.is_empty() {
        assert_eq!(seen.last().map(|p| p.0), Some(s.data.len() as u64), "progress reached the end");
    }
    out
}

#[test]
fn the_models_folder_is_maiterms_own() {
    let dir = models_dir().expect("a data folder");
    assert!(dir.ends_with("models/whisper"), "{}", dir.display());
    assert!(dir.starts_with(crate::state::persistence::app_data_dir().unwrap()));
    assert!(dir.to_string_lossy().contains(crate::state::persistence::app_data_slug()));
}

#[tokio::test]
async fn a_download_is_verified_then_never_made_again() {
    let dir = temp("fresh");
    let s = stub(model_bytes());
    assert_eq!(ready_in(&dir, &s.pin), None);
    let path = get(&dir, &s).await.unwrap();
    assert_eq!(path, dir.join(MODEL_FILE));
    assert_eq!(fs::read(&path).unwrap(), s.data);
    assert_eq!(ready_in(&dir, &s.pin), Some(path.clone()));
    let v: Verified = read_json(&dir.join(format!("{MODEL_FILE}.json"))).unwrap();
    assert_eq!((v.size, v.sha256.as_str(), v.checked_size, v.checked_sha256), (1000, sha(&s.data).as_str(), true, true));
    assert!(!dir.join(format!("{MODEL_FILE}.part")).exists() && !dir.join(format!("{MODEL_FILE}.part.json")).exists());

    // Idempotent: a second run asks huggingface.co nothing.
    assert_eq!(get(&dir, &s).await.unwrap(), path);
    assert_eq!(s.asked.load(Ordering::SeqCst), 1);
    assert_eq!(*s.opened.lock().unwrap(), vec![0]);
}

#[tokio::test]
async fn a_dropped_download_resumes_where_it_stopped() {
    let dir = temp("resume");
    let s = stub(model_bytes());
    *s.fail_after.lock().unwrap() = Some(400);
    assert!(get(&dir, &s).await.unwrap_err().contains("connection reset"));
    assert_eq!(fs::metadata(dir.join(format!("{MODEL_FILE}.part"))).unwrap().len(), 400, "the part is kept");
    assert_eq!(ready_in(&dir, &s.pin), None);

    let path = get(&dir, &s).await.unwrap();
    assert_eq!(*s.opened.lock().unwrap(), vec![0, 400], "the rest, with a Range");
    assert_eq!(fs::read(path).unwrap(), s.data);
}

#[tokio::test]
async fn a_server_that_ignores_the_range_starts_the_file_over() {
    let dir = temp("norange");
    let mut s = stub(model_bytes());
    s.honour_range = false;
    *s.fail_after.lock().unwrap() = Some(300);
    get(&dir, &s).await.unwrap_err();
    let path = get(&dir, &s).await.unwrap();
    assert_eq!(fs::read(path).unwrap(), s.data, "no doubled bytes");
}

#[tokio::test]
async fn a_part_of_some_other_file_is_not_resumed() {
    let dir = temp("other");
    fs::write(dir.join(format!("{MODEL_FILE}.part")), vec![9u8; 500]).unwrap();
    write_json(&dir.join(format!("{MODEL_FILE}.part.json")), &PartOf { size: Some(1000), sha256: Some("0".repeat(64)) }).unwrap();
    let s = stub(model_bytes());
    let path = get(&dir, &s).await.unwrap();
    assert_eq!(*s.opened.lock().unwrap(), vec![0]);
    assert_eq!(fs::read(path).unwrap(), s.data);
}

#[tokio::test]
async fn a_finished_part_that_was_never_moved_needs_no_request() {
    let dir = temp("whole-part");
    let s = stub(model_bytes());
    fs::write(dir.join(format!("{MODEL_FILE}.part")), &s.data).unwrap();
    write_json(&dir.join(format!("{MODEL_FILE}.part.json")), &PartOf { size: s.published.size, sha256: s.published.sha256.clone() }).unwrap();
    let path = ensure(&dir, MODEL_URL, &s.pin, &s, &mut |_, _| {}).await.unwrap();
    assert!(s.opened.lock().unwrap().is_empty());
    assert_eq!(fs::read(path).unwrap(), s.data);
}

#[tokio::test]
async fn published_facts_that_disagree_with_the_pin_fail_and_keep_nothing() {
    for (tag, bad_size, bad_sha) in [("sha", false, true), ("size", true, false)] {
        let dir = temp(tag);
        let mut s = stub(model_bytes());
        if bad_sha {
            s.published.sha256 = Some("a".repeat(64));
        }
        if bad_size {
            s.published.size = Some(999);
        }
        // A part left by an earlier run goes too.
        fs::write(dir.join(format!("{MODEL_FILE}.part")), [1u8; 10]).unwrap();
        write_json(&dir.join(format!("{MODEL_FILE}.part.json")), &PartOf { size: Some(1000), sha256: Some(s.pin.sha256.clone()) }).unwrap();
        let e = get(&dir, &s).await.unwrap_err();
        assert!(e.contains("different") && e.contains("Nothing was downloaded or kept"), "{e}");
        assert!(s.opened.lock().unwrap().is_empty(), "no byte asked for");
        for f in [MODEL_FILE.to_string(), format!("{MODEL_FILE}.part"), format!("{MODEL_FILE}.part.json"), format!("{MODEL_FILE}.json")] {
            assert!(!dir.join(&f).exists(), "{tag}: {f} kept");
        }
    }
}

#[tokio::test]
async fn with_no_published_facts_the_pin_still_checks_the_bytes() {
    let dir = temp("unpublished");
    let mut s = stub(model_bytes());
    s.published.size = None;
    s.published.sha256 = None;
    let path = get(&dir, &s).await.unwrap();
    let v: Verified = read_json(&dir.join(format!("{MODEL_FILE}.json"))).unwrap();
    assert_eq!((v.sha256.as_str(), v.checked_size, v.checked_sha256), (s.pin.sha256.as_str(), true, true));
    assert_eq!(fs::read(path).unwrap(), s.data);

    // Bytes that aren't the pinned file are deleted, published facts or not.
    let dir = temp("bad");
    let mut s = stub(model_bytes());
    (s.published.size, s.published.sha256) = (None, None);
    s.pin.sha256 = "b".repeat(64);
    let e = get(&dir, &s).await.unwrap_err();
    assert!(e.contains("doesn't match") && e.contains("deleted"), "{e}");
    assert!(!dir.join(format!("{MODEL_FILE}.part")).exists() && !dir.join(MODEL_FILE).exists());
    assert_eq!(ready_in(&dir, &s.pin), None);

    let dir = temp("long");
    let mut s = stub(model_bytes());
    (s.published.size, s.published.sha256) = (None, None);
    s.pin.size = 10;
    assert!(get(&dir, &s).await.unwrap_err().contains("more than"));
    assert_eq!(ready_in(&dir, &s.pin), None);
}

#[tokio::test]
async fn a_model_verified_against_another_pin_is_not_ready() {
    let dir = temp("repin");
    let s = stub(model_bytes());
    get(&dir, &s).await.unwrap();
    let other = Pin { size: 1000, sha256: "c".repeat(64) };
    assert_eq!(ready_in(&dir, &other), None);
}

#[test]
fn huggingface_s_real_302_is_read_as_the_pin() {
    // HEAD https://huggingface.co/ggerganov/whisper.cpp/resolve/main/ggml-base.en.bin, 2026-11-05.
    use reqwest::header::{HeaderMap, HeaderValue};
    let mut h = HeaderMap::new();
    h.insert("location", HeaderValue::from_static("https://us.aws.cdn.hf.co/repos/ab/cd/a03779c8?response-content-disposition=attachment&Expires=1&Signature=x"));
    h.insert("x-linked-size", HeaderValue::from_static("147964211"));
    h.insert("x-linked-etag", HeaderValue::from_static("\"a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002\""));
    h.insert("etag", HeaderValue::from_static("\"137c40403d78fd54d454da0f9bd998f78703390c\""));
    let p = published_from(MODEL_URL, reqwest::StatusCode::FOUND, &h).unwrap();
    assert_eq!(p.size, Some(MODEL_SIZE));
    assert_eq!(p.sha256.as_deref(), Some(MODEL_SHA256));
    assert_eq!(base_en(), Pin { size: p.size.unwrap(), sha256: p.sha256.unwrap() });
    // The bytes come from the CDN the 302 names, not from huggingface.co's resolve URL.
    assert!(p.location.starts_with("https://us.aws.cdn.hf.co/"), "{}", p.location);

    // Names are case-insensitive, and a relative location resolves against the url.
    let mut h = HeaderMap::new();
    h.insert("Location", HeaderValue::from_static("/cdn/model.bin"));
    h.insert("X-Linked-Size", HeaderValue::from_static("147964211"));
    let p = published_from(MODEL_URL, reqwest::StatusCode::FOUND, &h).unwrap();
    assert_eq!((p.size, p.sha256, p.location.as_str()), (Some(MODEL_SIZE), None, "https://huggingface.co/cdn/model.bin"));

    assert!(published_from(MODEL_URL, reqwest::StatusCode::NOT_FOUND, &HeaderMap::new()).is_err());
    assert!(published_from(MODEL_URL, reqwest::StatusCode::FOUND, &HeaderMap::new()).is_err(), "a 302 with no location");
}

#[test]
fn a_model_whose_file_changed_since_is_not_ready() {
    let dir = temp("changed");
    fs::write(dir.join(MODEL_FILE), b"short").unwrap();
    let pin = Pin { size: 1000, sha256: "x".into() };
    write_json(&dir.join(format!("{MODEL_FILE}.json")), &Verified { size: 1000, sha256: "x".into(), checked_size: true, checked_sha256: true, url: MODEL_URL.into() }).unwrap();
    assert_eq!(ready_in(&dir, &pin), None);
    fs::remove_file(dir.join(format!("{MODEL_FILE}.json"))).unwrap();
    fs::write(dir.join(MODEL_FILE), vec![0u8; 1000]).unwrap();
    assert_eq!(ready_in(&dir, &pin), None, "no record of a verified download: not ready");
}

#[test]
fn only_a_sha256_etag_counts() {
    let h = "7d2a1e4f".repeat(8);
    assert_eq!(etag_sha256(&format!("\"{h}\"")), Some(h.clone()));
    assert_eq!(etag_sha256(&format!("W/\"{}\"", h.to_uppercase())), Some(h));
    assert_eq!(etag_sha256("\"137c40403d78fd54d454da0f9bd998f78703390c\""), None, "a SHA-1 git etag is not the file's SHA-256");
    assert_eq!(etag_sha256("\"abc\""), None);
}
