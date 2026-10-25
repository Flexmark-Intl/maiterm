use std::fs;
use std::path::PathBuf;

use super::*;
use crate::hive::gallery::ingest;
use crate::hive::gallery::model::*;

struct Scratch {
    base: PathBuf,
    g: Gallery,
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.base);
    }
}

fn scratch() -> Scratch {
    let base = std::env::temp_dir().join(format!("hive-gallery-serve-{}", uuid::Uuid::new_v4()));
    let g = Gallery::open(&base.join("gallery")).unwrap();
    Scratch { base, g }
}

fn add(s: &Scratch, ext: &str, mime: &str, bytes: &[u8]) -> Asset {
    let id = mint(ASSET_PREFIX);
    let file = format!("{id}.{ext}");
    fs::write(s.g.files_dir().join(&file), bytes).unwrap();
    s.g.insert_asset(NewAsset {
        id,
        kind: ingest::kind_for(ext).into(),
        file,
        mime: mime.into(),
        bytes: bytes.len() as u64,
        sha256: "x".into(),
        title: "t".into(),
        made_by: MadeBy::human(),
        ..NewAsset::default()
    })
    .unwrap()
}

fn uri(s: &str) -> Uri {
    s.parse().unwrap()
}

fn header(r: &Response<Vec<u8>>, h: header::HeaderName) -> Option<&str> {
    r.headers().get(h).and_then(|v| v.to_str().ok())
}

const HIVE: &str = "6f1c2e7a-3b0d-4c55-9a1e-2f7d8c9b0a11";

#[test]
fn both_url_forms_parse_and_anything_else_is_none() {
    let a = "a-0123456789ab";
    assert_eq!(parse_uri(&uri(&format!("gallery://localhost/{HIVE}/{a}"))), Some((HIVE.into(), a.into(), Part::File)));
    assert_eq!(parse_uri(&uri(&format!("http://gallery.localhost/{HIVE}/{a}/thumb"))), Some((HIVE.into(), a.into(), Part::Thumb)));
    assert_eq!(parse_uri(&uri(&format!("https://gallery.localhost/{HIVE}/{a}/strip"))).map(|p| p.2), Some(Part::Strip));
    assert_eq!(parse_uri(&uri(&format!("gallery://localhost/{HIVE}/{a}/wave?v=2"))).map(|p| p.2), Some(Part::Wave));
    for bad in [
        format!("gallery://localhost/../{a}"),
        format!("gallery://localhost/{HIVE}/../{a}"),
        format!("gallery://localhost/{HIVE}/{a}/../../x"),
        format!("gallery://localhost//{a}"),
        format!("gallery://localhost/{HIVE}//{a}"),
        format!("gallery://localhost/{HIVE}/{a}/"),
        format!("gallery://localhost/{HIVE}/{a}/poster"),
        format!("gallery://localhost/{HIVE}/a-..%2f..%2fetc"),
        format!("gallery://localhost/{HIVE}%2f..%2f/{a}"),
        format!("gallery://localhost/.hidden/{a}"),
        format!("gallery://localhost/{HIVE}"),
        format!("gallery://localhost/{HIVE}/{a}/thumb/x"),
        format!("gallery://elsewhere/{HIVE}/{a}"),
        format!("gallery://localhost:8080/{HIVE}/{a}"),
        format!("http://localhost/{HIVE}/{a}"),
        format!("http://gallery.localhost.evil/{HIVE}/{a}"),
        format!("file://localhost/{HIVE}/{a}"),
    ] {
        assert_eq!(parse_uri(&uri(&bad)), None, "{bad}");
    }
}

#[test]
fn ranges_parse_cap_and_refuse() {
    assert_eq!(parse_range(None, 1000), Ok(None));
    assert_eq!(parse_range(Some("items=0-1"), 1000), Ok(None), "another unit is ignored");
    assert_eq!(parse_range(Some("bytes=0-"), 1000), Ok(Some((0, 999))));
    assert_eq!(parse_range(Some("bytes=100-199"), 1000), Ok(Some((100, 199))));
    assert_eq!(parse_range(Some("bytes=900-5000"), 1000), Ok(Some((900, 999))), "an end past the file is the last byte");
    assert_eq!(parse_range(Some("bytes=-100"), 1000), Ok(Some((900, 999))));
    assert_eq!(parse_range(Some("bytes=-5000"), 1000), Ok(Some((0, 999))));
    assert_eq!(parse_range(Some("bytes=0-1, 5-6"), 1000), Ok(Some((0, 1))), "the first of several");
    let big = 1 << 30;
    assert_eq!(parse_range(Some("bytes=0-"), big), Ok(Some((0, MAX_CHUNK - 1))), "open-ended is a chunk");
    assert_eq!(parse_range(Some("bytes=10-"), big), Ok(Some((10, 10 + MAX_CHUNK - 1))));
    assert_eq!(parse_range(Some(&format!("bytes=0-{}", big - 1)), big), Ok(Some((0, MAX_CHUNK - 1))));
    for bad in ["bytes=1000-", "bytes=1000-1001", "bytes=5-4", "bytes=-0", "bytes=x-1", "bytes=", "bytes=10"] {
        assert_eq!(parse_range(Some(bad), 1000), Err(()), "{bad}");
    }
    assert_eq!(parse_range(Some("bytes=0-"), 0), Err(()), "an empty file has no range");
}

#[test]
fn whole_files_and_slices_come_back_exact_with_their_headers() {
    let s = scratch();
    let data: Vec<u8> = (0..=255u8).cycle().take(5000).collect();
    let a = add(&s, "mp4", "video/mp4", &data);
    let whole = respond(true, Some(&s.g), Part::File, &a.id, None);
    assert_eq!(whole.status(), StatusCode::OK);
    assert_eq!(whole.body(), &data);
    assert_eq!(header(&whole, header::CONTENT_TYPE), Some("video/mp4"));
    assert_eq!(header(&whole, header::CONTENT_LENGTH), Some("5000"));
    assert_eq!(header(&whole, header::ACCEPT_RANGES), Some("bytes"));
    assert_eq!(header(&whole, header::X_CONTENT_TYPE_OPTIONS), Some("nosniff"));
    assert_eq!(header(&whole, header::CONTENT_SECURITY_POLICY), Some("sandbox"));
    assert_eq!(header(&whole, header::CACHE_CONTROL), Some("private, max-age=31536000, immutable"));

    let part = respond(true, Some(&s.g), Part::File, &a.id, Some("bytes=100-199"));
    assert_eq!(part.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(part.body(), &data[100..200]);
    assert_eq!(header(&part, header::CONTENT_RANGE), Some("bytes 100-199/5000"));
    assert_eq!(header(&part, header::CONTENT_LENGTH), Some("100"));

    let tail = respond(true, Some(&s.g), Part::File, &a.id, Some("bytes=-10"));
    assert_eq!((tail.body().as_slice(), header(&tail, header::CONTENT_RANGE)), (&data[4990..], Some("bytes 4990-4999/5000")));

    let bad = respond(true, Some(&s.g), Part::File, &a.id, Some("bytes=6000-"));
    assert_eq!(bad.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(header(&bad, header::CONTENT_RANGE), Some("bytes */5000"));
    assert!(bad.body().is_empty());
}

#[test]
fn thumbnails_are_served_by_part_and_a_missing_one_is_a_404() {
    let s = scratch();
    let a = add(&s, "mp4", "video/mp4", b"v");
    fs::write(s.g.thumbs_dir().join(format!("{}.png", a.id)), b"poster").unwrap();
    fs::write(s.g.thumbs_dir().join(format!("{}.strip.jpg", a.id)), b"strip").unwrap();
    let t = respond(true, Some(&s.g), Part::Thumb, &a.id, None);
    assert_eq!((t.status(), t.body().as_slice(), header(&t, header::CONTENT_TYPE)), (StatusCode::OK, &b"poster"[..], Some("image/png")));
    let st = respond(true, Some(&s.g), Part::Strip, &a.id, None);
    assert_eq!((st.body().as_slice(), header(&st, header::CONTENT_TYPE)), (&b"strip"[..], Some("image/jpeg")));
    let w = respond(true, Some(&s.g), Part::Wave, &a.id, None);
    assert_eq!(w.status(), StatusCode::NOT_FOUND);
    assert_eq!(header(&w, header::CACHE_CONTROL), Some("no-store"), "a 404 isn't cached: the thumbnail may come");
}

#[test]
fn an_unknown_hive_a_trashed_or_unknown_asset_is_a_404() {
    let s = scratch();
    let a = add(&s, "png", "image/png", b"png");
    assert_eq!(respond(false, Some(&s.g), Part::File, &a.id, None).status(), StatusCode::NOT_FOUND, "a Hive the window doesn't hold");
    assert_eq!(respond(true, None, Part::File, &a.id, None).status(), StatusCode::NOT_FOUND);
    assert_eq!(respond(true, Some(&s.g), Part::File, "a-ffffffffffff", None).status(), StatusCode::NOT_FOUND);
    s.g.trash(std::slice::from_ref(&a.id)).unwrap();
    assert_eq!(respond(true, Some(&s.g), Part::File, &a.id, None).status(), StatusCode::NOT_FOUND);
    s.g.restore(std::slice::from_ref(&a.id)).unwrap();
    assert_eq!(respond(true, Some(&s.g), Part::File, &a.id, None).status(), StatusCode::OK);
    s.g.trash(std::slice::from_ref(&a.id)).unwrap();
    s.g.purge_trash().unwrap();
    assert_eq!(respond(true, Some(&s.g), Part::File, &a.id, None).status(), StatusCode::NOT_FOUND, "purged");
}

#[test]
fn nothing_is_served_as_a_page() {
    let s = scratch();
    let html = add(&s, "html", ingest::mime_for("html"), b"<script>alert(1)</script>");
    let r = respond(true, Some(&s.g), Part::File, &html.id, None);
    assert_eq!(header(&r, header::CONTENT_TYPE), Some("application/octet-stream"));
    // A type stored by some other build is checked again here.
    for stored in ["text/html", "TEXT/HTML; charset=utf-8", "application/xhtml+xml", "text/plain", "application/xml", ""] {
        let a = add(&s, "bin", stored, b"x");
        let r = respond(true, Some(&s.g), Part::File, &a.id, None);
        assert_eq!(header(&r, header::CONTENT_TYPE), Some("application/octet-stream"), "{stored}");
    }
    let svg = add(&s, "svg", ingest::mime_for("svg"), b"<svg/>");
    let r = respond(true, Some(&s.g), Part::File, &svg.id, None);
    assert_eq!((header(&r, header::CONTENT_TYPE), header(&r, header::CONTENT_SECURITY_POLICY)), (Some("image/svg+xml"), Some("sandbox")));
}

#[test]
fn a_stored_name_must_be_the_assets_own() {
    let id = "a-0123456789ab";
    assert!(is_stored_name(id, &format!("{id}.png")) && is_stored_name(id, id));
    for bad in ["a-0123456789ab.", "a-0123456789ab./x", "a-0123456789ab.p/g", "a-ffffffffffff.png", "../a-0123456789ab.png", "a-0123456789abx"] {
        assert!(!is_stored_name(id, bad), "{bad}");
    }
}

#[test]
fn only_the_asking_window_holds_its_hive() {
    use crate::state::{AppData, WindowData, Workspace};
    let mut ws = Workspace::new("Hive".into());
    ws.hive = Some(crate::hive::Hive { id: HIVE.into(), ..Default::default() });
    let mut main = WindowData::new("main".into());
    main.workspaces = vec![ws];
    let data = AppData { windows: vec![main, WindowData::new("other".into())], ..Default::default() };
    assert!(window_holds_hive(&data, "main", HIVE));
    assert!(!window_holds_hive(&data, "other", HIVE));
    assert!(!window_holds_hive(&data, "main", "6f1c2e7a-0000-0000-0000-000000000000"));
    assert!(!window_holds_hive(&data, "gone", HIVE));
}

#[test]
fn both_csps_let_the_webview_load_the_scheme() {
    for (name, conf) in [("tauri.conf.json", include_str!("../../../../tauri.conf.json")), ("tauri.dev.conf.json", include_str!("../../../../tauri.dev.conf.json"))] {
        let v: serde_json::Value = serde_json::from_str(conf).unwrap();
        let csp = v["app"]["security"]["csp"].as_str().unwrap_or_else(|| panic!("{name} has a CSP"));
        for directive in ["img-src", "media-src"] {
            let sources = csp.split(';').map(str::trim).find(|d| d.starts_with(&format!("{directive} "))).unwrap_or_else(|| panic!("{name}: {directive}"));
            assert!(sources.split_whitespace().any(|s| s == "gallery:"), "{name}: {sources}");
            assert!(sources.split_whitespace().any(|s| s == "http://gallery.localhost"), "{name}: {sources}");
        }
    }
}
