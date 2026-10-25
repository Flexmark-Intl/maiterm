//! The `gallery://` scheme (docs/hive.md §10.5, plan D7): how the webview reads stored files.
//! `gallery://localhost/<hive>/<asset>[/thumb|/strip|/wave]`, or `http://gallery.localhost/...`
//! on Windows, Tauri's form for a custom scheme there.
//!
//! Served by asset id only: the id is looked up in that Hive's index and only its stored file or
//! its own thumbnails open, so no path from the webview reaches the filesystem. The Hive must be
//! one the asking window holds; a trashed or purged asset is a 404. Nothing is served as HTML:
//! the stored type is checked again here, every answer is `nosniff` and `sandbox`ed, and stored
//! files never change, so they cache forever. `Range` is answered for video seeking.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use tauri::http::{header, HeaderValue, Method, Request, Response, StatusCode, Uri};
use tauri::{AppHandle, Manager, UriSchemeResponder};

use super::model::is_asset_id;
use super::thumbs::{poster_name, strip_name, wave_name};
use super::Gallery;
use crate::hive::paths::is_component;
use crate::state::{AppData, AppState};

pub const SCHEME: &str = "gallery";
/// The host on Windows (and Android), where a custom scheme is served as `http://<scheme>.localhost`.
pub const WINDOWS_HOST: &str = "gallery.localhost";
/// The most one range answer carries; a player asks again for the rest.
pub const MAX_CHUNK: u64 = 4 << 20;
/// The most a request without a range gets whole: an image, never a long video in one buffer.
pub const MAX_WHOLE: u64 = 256 << 20;
const OCTET: &str = "application/octet-stream";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    File,
    Thumb,
    Strip,
    Wave,
}

/// A Hive id as a URL carries it: a uuid in practice, and nothing that needs escaping.
fn is_hive_id(s: &str) -> bool {
    is_component(s) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The Hive, the asset and the part a URL names, or None for anything else: another host, a
/// port, an id that isn't one, an empty or extra component.
pub fn parse_uri(uri: &Uri) -> Option<(String, String, Part)> {
    let (scheme, host) = (uri.scheme_str()?, uri.host()?);
    let ours = (scheme == SCHEME && host == "localhost") || (matches!(scheme, "http" | "https") && host == WINDOWS_HOST);
    if !ours || uri.port().is_some() {
        return None;
    }
    let segs: Vec<&str> = uri.path().strip_prefix('/')?.split('/').collect();
    let (hive, asset, part) = match segs.as_slice() {
        [h, a] => (*h, *a, Part::File),
        [h, a, p] => (
            *h,
            *a,
            match *p {
                "thumb" => Part::Thumb,
                "strip" => Part::Strip,
                "wave" => Part::Wave,
                _ => return None,
            },
        ),
        _ => return None,
    };
    (is_hive_id(hive) && is_asset_id(asset)).then(|| (hive.to_string(), asset.to_string(), part))
}

/// A `Range` header against a file of `len` bytes: Ok(None) to answer the whole file (no header,
/// or a unit that isn't bytes), the inclusive span to send, or Err for a 416. Only the first of
/// several ranges is answered, and no span is longer than `MAX_CHUNK`.
#[allow(clippy::result_unit_err)]
pub fn parse_range(h: Option<&str>, len: u64) -> Result<Option<(u64, u64)>, ()> {
    let Some(h) = h else { return Ok(None) };
    let Some(spec) = h.trim().strip_prefix("bytes=") else { return Ok(None) };
    let first = spec.split(',').next().unwrap_or("").trim();
    let (a, b) = first.split_once('-').ok_or(())?;
    let num = |s: &str| s.trim().parse::<u64>().map_err(|_| ());
    if len == 0 {
        return Err(());
    }
    let (start, end) = if a.trim().is_empty() {
        let n = num(b)?;
        if n == 0 {
            return Err(());
        }
        (len.saturating_sub(n), len - 1)
    } else {
        let start = num(a)?;
        let end = if b.trim().is_empty() { len - 1 } else { num(b)?.min(len - 1) };
        (start, end)
    };
    if start >= len || start > end {
        return Err(());
    }
    Ok(Some((start, end.min(start + MAX_CHUNK - 1))))
}

/// The stored type, unless it could render as a page: then opaque bytes. SVG stays an image,
/// which `sandbox` keeps from running script if it's ever opened as a document.
fn safe_mime(m: &str) -> &str {
    let lower = m.to_ascii_lowercase();
    if m.is_empty() || lower.starts_with("text/") || lower.contains("html") || (lower.contains("xml") && lower != "image/svg+xml") {
        OCTET
    } else {
        m
    }
}

/// A stored file's name as the store makes it: `<id>` or `<id>.<ext>`, one component.
fn is_stored_name(id: &str, file: &str) -> bool {
    file.strip_prefix(id).is_some_and(|rest| rest.is_empty() || rest.strip_prefix('.').is_some_and(|e| !e.is_empty() && e.bytes().all(|b| b.is_ascii_alphanumeric())))
}

fn base(status: StatusCode) -> tauri::http::response::Builder {
    Response::builder()
        .status(status)
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::CONTENT_SECURITY_POLICY, "sandbox")
}

fn empty(status: StatusCode) -> Response<Vec<u8>> {
    base(status).header(header::CACHE_CONTROL, "no-store").body(Vec::new()).unwrap_or_default()
}

fn unsatisfiable(len: u64) -> Response<Vec<u8>> {
    let mut r = empty(StatusCode::RANGE_NOT_SATISFIABLE);
    if let Ok(v) = HeaderValue::from_str(&format!("bytes */{len}")) {
        r.headers_mut().insert(header::CONTENT_RANGE, v);
    }
    r
}

fn open(path: &Path) -> std::io::Result<File> {
    let mut o = std::fs::OpenOptions::new();
    o.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    o.open(path)
}

fn read_span(f: &mut File, start: u64, end: u64) -> std::io::Result<Vec<u8>> {
    f.seek(SeekFrom::Start(start))?;
    let mut buf = Vec::with_capacity((end - start + 1) as usize);
    f.take(end - start + 1).read_to_end(&mut buf)?;
    Ok(buf)
}

/// One answer. `hive_known`: the asking window holds this Hive. Blocks on the disk.
pub fn respond(hive_known: bool, gallery: Option<&Gallery>, part: Part, id: &str, range: Option<&str>) -> Response<Vec<u8>> {
    let Some(g) = gallery.filter(|_| hive_known) else { return empty(StatusCode::NOT_FOUND) };
    let a = match g.get(id) {
        Ok(Some(a)) if !a.in_trash() => a,
        Ok(_) => return empty(StatusCode::NOT_FOUND),
        Err(e) => {
            log::warn!("gallery {}: {id} not read to serve: {e}", g.root().display());
            return empty(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let (path, mime) = match part {
        Part::File if is_stored_name(id, &a.file) => (g.files_dir().join(&a.file), safe_mime(&a.mime).to_string()),
        Part::File => return empty(StatusCode::NOT_FOUND),
        Part::Thumb => (g.thumbs_dir().join(poster_name(id)), "image/png".to_string()),
        Part::Strip => (g.thumbs_dir().join(strip_name(id)), "image/jpeg".to_string()),
        Part::Wave => (g.thumbs_dir().join(wave_name(id)), "image/png".to_string()),
    };
    let mut f = match open(&path) {
        Ok(f) => f,
        Err(_) => return empty(StatusCode::NOT_FOUND),
    };
    let len = match f.metadata() {
        Ok(m) if m.is_file() => m.len(),
        _ => return empty(StatusCode::NOT_FOUND),
    };
    let (status, span) = match parse_range(range, len) {
        Err(()) => return unsatisfiable(len),
        Ok(Some(s)) => (StatusCode::PARTIAL_CONTENT, Some(s)),
        Ok(None) if len > MAX_WHOLE => return unsatisfiable(len),
        Ok(None) => (StatusCode::OK, None),
    };
    let body = match span {
        Some((s, e)) => read_span(&mut f, s, e),
        None if len == 0 => Ok(Vec::new()),
        None => read_span(&mut f, 0, len - 1),
    };
    let body = match body {
        Ok(b) => b,
        Err(e) => {
            log::warn!("gallery {}: {} not read to serve: {e}", g.root().display(), path.display());
            return empty(StatusCode::INTERNAL_SERVER_ERROR);
        }
    };
    let mut b = base(status)
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, body.len())
        .header(header::ACCEPT_RANGES, "bytes")
        .header(header::CACHE_CONTROL, "private, max-age=31536000, immutable");
    if let Some((s, _)) = span {
        // What was read, which a file shrinking underneath could make shorter than asked.
        let end = s + (body.len() as u64).max(1) - 1;
        b = b.header(header::CONTENT_RANGE, format!("bytes {s}-{end}/{len}"));
    }
    b.body(body).unwrap_or_else(|_| empty(StatusCode::INTERNAL_SERVER_ERROR))
}

/// Whether window `label` holds a workspace that is Hive `hive_id`.
pub fn window_holds_hive(data: &AppData, label: &str, hive_id: &str) -> bool {
    data.window(label).is_some_and(|w| w.workspaces.iter().any(|ws| ws.hive.as_ref().is_some_and(|h| h.id == hive_id)))
}

fn serve(app: &AppHandle, label: &str, req: &Request<Vec<u8>>) -> Response<Vec<u8>> {
    let head = req.method() == Method::HEAD;
    if !head && req.method() != Method::GET {
        return empty(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Some((hive, asset, part)) = parse_uri(req.uri()) else { return empty(StatusCode::NOT_FOUND) };
    let hive_known = app.try_state::<Arc<AppState>>().is_some_and(|s| window_holds_hive(&s.app_data.read(), label, &hive));
    let gallery = if hive_known {
        super::for_hive(&hive).map_err(|e| log::warn!("gallery for hive {hive} not opened to serve: {e}")).ok()
    } else {
        None
    };
    let range = req.headers().get(header::RANGE).and_then(|v| v.to_str().ok());
    let mut resp = respond(hive_known, gallery.as_deref(), part, &asset, range);
    if head {
        resp.body_mut().clear();
    }
    resp
}

/// The scheme's handler (`lib.rs`): the read runs off the webview's thread.
pub fn handle(app: AppHandle, label: String, req: Request<Vec<u8>>, responder: UriSchemeResponder) {
    tauri::async_runtime::spawn_blocking(move || responder.respond(serve(&app, &label, &req)));
}

#[cfg(test)]
mod tests;
