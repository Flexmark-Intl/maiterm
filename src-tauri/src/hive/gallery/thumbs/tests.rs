use std::path::PathBuf;

use super::*;

const ALL: Have = Have { ffmpeg: true, ffprobe: true, qlmanage: true, sips: true };
const NONE: Have = Have { ffmpeg: false, ffprobe: false, qlmanage: false, sips: false };

fn p(s: &str) -> PathBuf {
    PathBuf::from(s)
}

fn d(duration_ms: Option<u64>) -> Probed {
    Probed { duration_ms, ..Probed::default() }
}

fn dims(width: Option<u32>, height: Option<u32>, duration_ms: Option<u64>) -> Probed {
    Probed { width, height, duration_ms, ..Probed::default() }
}

#[test]
fn plans_follow_the_kind_and_the_tools() {
    assert_eq!(plan(KIND_VIDEO, ALL), vec![Step::Probe, Step::QuickLook, Step::Poster, Step::Strip]);
    let linux = Have { ffmpeg: true, ffprobe: true, ..NONE };
    assert_eq!(plan(KIND_VIDEO, linux), vec![Step::Probe, Step::Poster, Step::Strip]);
    assert_eq!(
        plan(KIND_IMAGE, ALL),
        vec![Step::Sips, Step::QuickLook, Step::Probe, Step::Poster],
        "sips for the size, which ffprobe reads as one tile's for a HEIC; ffprobe and ffmpeg only if QuickLook gives no poster"
    );
    assert_eq!(plan(KIND_IMAGE, Have { ffprobe: false, ffmpeg: false, ..ALL }), vec![Step::Sips, Step::QuickLook]);
    assert_eq!(plan(KIND_IMAGE, linux), vec![Step::Probe, Step::Poster]);
    assert_eq!(plan(KIND_AUDIO, ALL), vec![Step::Probe, Step::Wave], "QuickLook gives audio only an icon");
    assert!(plan(KIND_AUDIO, Have { ffmpeg: false, ffprobe: false, ..ALL }).is_empty(), "audio without ffmpeg: the kind icon");
    assert_eq!(plan(KIND_DOCUMENT, ALL), vec![Step::QuickLook]);
    assert_eq!(plan(KIND_MODEL3D, ALL), vec![Step::QuickLook]);
    assert!(plan(KIND_DOCUMENT, linux).is_empty());
    assert!(plan(KIND_OTHER, ALL).is_empty());
    assert!(plan("hologram", ALL).is_empty(), "a kind a later build adds");
    for kind in KINDS {
        assert!(plan(kind, NONE).is_empty(), "{kind}");
    }
}

#[test]
fn a_long_videos_strip_is_keyframes_and_a_short_ones_is_seeks() {
    let (file, tmp) = (p("/g/files/a-0123456789ab.mp4"), p("/g/.tmp/a-0123456789ab"));
    let long = argv(Step::Strip, KIND_VIDEO, &file, &tmp, &d(Some(600_000)));
    let at = long.iter().position(|a| a == "-skip_frame").expect("keyframes only");
    assert_eq!(long[at + 1], "nokey");
    assert_eq!(long.iter().filter(|a| *a == "-i").count(), 1);
    let vf = &long[long.iter().position(|a| a == "-vf").unwrap() + 1];
    assert!(vf.contains("gte(t-prev_selected_t\\,60.000)") && vf.ends_with("tile=10x1"), "{vf}");
    assert_eq!(long.last().unwrap(), &tmp.join(STRIP_OUT).display().to_string());

    let unknown = argv(Step::Strip, KIND_VIDEO, &file, &tmp, &d(None));
    assert!(unknown.contains(&"nokey".to_string()) && !unknown.iter().any(|a| a.contains("select=")), "{unknown:?}");

    let short = argv(Step::Strip, KIND_VIDEO, &file, &tmp, &d(Some(5_000)));
    assert!(!short.contains(&"-skip_frame".to_string()));
    assert_eq!(short.iter().filter(|a| *a == "-i").count(), STRIP_FRAMES as usize, "one seek a frame");
    let seeks: Vec<&String> = short.iter().enumerate().filter(|(i, _)| *i > 0 && short[i - 1] == "-ss").map(|(_, a)| a).collect();
    assert_eq!(seeks.first().map(|s| s.as_str()), Some("0.250"));
    assert_eq!(seeks.last().map(|s| s.as_str()), Some("4.750"));
    assert!(short.iter().any(|a| a.ends_with("hstack=inputs=10,format=yuvj420p")));
}

#[test]
fn every_decoder_input_reads_files_only() {
    let (file, tmp) = (p("/g/files/a.mp4"), p("/g/.tmp/a"));
    for step in [Step::Probe, Step::Poster, Step::Strip, Step::Wave] {
        for ms in [None, Some(5_000), Some(600_000)] {
            let v = argv(step, KIND_VIDEO, &file, &tmp, &d(ms));
            let inputs = v.iter().filter(|a| *a == "-i").count().max(1);
            let guarded = v.windows(2).filter(|w| w[0] == "-protocol_whitelist" && w[1] == "file").count();
            assert_eq!(guarded, inputs, "{step:?} {ms:?}: {v:?}");
        }
    }
    assert!(argv(Step::Poster, KIND_VIDEO, &file, &tmp, &d(Some(10_000))).windows(2).any(|w| w[0] == "-ss" && w[1] == "1.000"), "the poster is a tenth in");
    assert!(!argv(Step::Poster, KIND_IMAGE, &file, &tmp, &d(Some(10_000))).contains(&"-ss".to_string()));
    let wave = argv(Step::Wave, KIND_AUDIO, &file, &tmp, &d(None));
    assert!(wave.contains(&"showwavespic=s=1024x160:colors=white".to_string()) && wave.last().unwrap().ends_with(WAVE_OUT));
    assert_eq!(argv(Step::QuickLook, KIND_IMAGE, &file, &tmp, &d(None)), ["-t", "-s", "512", "-o", "/g/.tmp/a", "/g/files/a.mp4"].map(String::from));
}

#[test]
fn probes_read_dimensions_rotation_and_duration() {
    let video = r#"{"streams":[{"codec_type":"audio","duration":"9.0"},{"codec_type":"video","width":1920,"height":1080,"side_data_list":[{"rotation":-90}]}],"format":{"duration":"12.345"}}"#;
    assert_eq!(parse_probe(KIND_VIDEO, video), dims(Some(1080), Some(1920), Some(12_345)));
    let tagged = r#"{"streams":[{"codec_type":"video","width":640,"height":360,"tags":{"rotate":"180"}}],"format":{}}"#;
    assert_eq!(parse_probe(KIND_VIDEO, tagged), dims(Some(640), Some(360), None));
    let image = r#"{"streams":[{"codec_type":"video","width":200,"height":100}],"format":{"duration":"0.04"}}"#;
    assert_eq!(parse_probe(KIND_IMAGE, image), dims(Some(200), Some(100), None), "an image has no duration");
    let song = r#"{"streams":[{"codec_type":"audio","duration":"2.000000"},{"codec_type":"video","width":600,"height":600,"disposition":{"attached_pic":1}}],"format":{"duration":"N/A"}}"#;
    assert_eq!(parse_probe(KIND_AUDIO, song), dims(None, None, Some(2_000)), "cover art isn't the audio's size");
    let covered = r#"{"streams":[{"codec_type":"video","width":600,"height":600,"disposition":{"attached_pic":1}},{"codec_type":"video","width":320,"height":240}]}"#;
    assert_eq!(parse_probe(KIND_VIDEO, covered).width, Some(320));
    assert_eq!(parse_probe(KIND_VIDEO, "not json"), Probed::default());
    assert_eq!(parse_probe(KIND_VIDEO, r#"{"streams":[{"codec_type":"video","width":0,"height":-1}],"format":{"duration":"-3"}}"#), Probed::default());
}

/// A 1000x700 HEIC as ffprobe 7.1 lists it: four 512x512 tiles, each a `dependent` stream, and
/// a 320x224 thumbnail item. `GROUPS` is what `-show_stream_groups` adds.
const TILES: &str = r#"{"streams":[
    {"index":0,"codec_type":"video","width":512,"height":512,"disposition":{"attached_pic":0,"dependent":1}},
    {"index":1,"codec_type":"video","width":512,"height":512,"disposition":{"attached_pic":0,"dependent":1}},
    {"index":2,"codec_type":"video","width":512,"height":512,"disposition":{"attached_pic":0,"dependent":1}},
    {"index":3,"codec_type":"video","width":512,"height":512,"disposition":{"attached_pic":0,"dependent":1}},
    {"index":4,"codec_type":"video","width":320,"height":224,"disposition":{"attached_pic":0,"dependent":0}}],"format":{}}"#;
const GROUPS: &str = r#","stream_groups":[{"index":0,"type":"Tile Grid","components":[{"nb_tiles":4,"coded_width":1024,"coded_height":1024,
    "horizontal_offset":0,"vertical_offset":0,"width":1000,"height":700,"subcomponents":[
    {"stream_index":0,"tile_horizontal_offset":0,"tile_vertical_offset":0},{"stream_index":1,"tile_horizontal_offset":512,"tile_vertical_offset":0},
    {"stream_index":2,"tile_horizontal_offset":0,"tile_vertical_offset":512},{"stream_index":3,"tile_horizontal_offset":512,"tile_vertical_offset":512}]}]}]}"#;

fn grouped() -> String {
    format!("{}{GROUPS}", TILES.trim_end().strip_suffix('}').unwrap())
}

#[test]
fn a_tiled_image_is_its_grids_size_never_a_tiles() {
    let first = parse_probe(KIND_IMAGE, TILES);
    assert!(first.tiled && first.grid.is_none());
    assert_eq!((first.width, first.height), (None, None), "neither a tile's 512x512 nor the thumbnail's size");

    let (file, tmp) = (p("/g/files/a.heic"), p("/g/.tmp/a"));
    assert!(!argv(Step::Probe, KIND_IMAGE, &file, &tmp, &Probed::default()).contains(&"-show_stream_groups".to_string()), "an older ffprobe refuses it");
    assert!(argv(Step::Probe, KIND_IMAGE, &file, &tmp, &first).contains(&"-show_stream_groups".to_string()), "asked once tiles are seen");

    let second = parse_probe(KIND_IMAGE, &grouped());
    assert_eq!((second.width, second.height), (Some(1000), Some(700)));
    let g = second.grid.clone().expect("the grid");
    assert_eq!(g.tiles, vec![(0, 0, 0), (1, 512, 0), (2, 0, 512), (3, 512, 512)]);

    let poster = argv(Step::Poster, KIND_IMAGE, &file, &tmp, &second);
    let graph = &poster[poster.iter().position(|a| a == "-filter_complex").expect("stitched") + 1];
    assert_eq!(graph, "[0:0][0:1][0:2][0:3]xstack=inputs=4:layout=0_0|512_0|0_512|512_512,crop=1000:700:0:0,scale=512:512:force_original_aspect_ratio=decrease");
    assert_eq!(poster.windows(2).filter(|w| w[0] == "-protocol_whitelist" && w[1] == "file").count(), 1);

    assert!(!parse_probe(KIND_VIDEO, TILES).tiled, "only an image is read as tiles");
    assert_eq!(parse_probe(KIND_VIDEO, TILES).width, Some(320));
}

/// `make` on a tiled image with only ffprobe and ffmpeg (Linux): the second probe's grid is the
/// recorded size, and the stand-in ffmpeg is handed the stitching graph.
#[cfg(unix)]
#[test]
fn make_reads_a_tiled_images_size_from_its_grid() {
    if cfg!(target_os = "macos") && !Path::new(job_sandbox::SANDBOX_EXEC).exists() {
        return;
    }
    let s = scratch();
    let a = add(&s, "heic", None);
    let bin = s.base.join("bin");
    fs::create_dir_all(&bin).unwrap();
    let (tiles, groups) = (bin.join("tiles.json"), bin.join("groups.json"));
    fs::write(&tiles, TILES).unwrap();
    fs::write(&groups, grouped()).unwrap();
    let script = |name: &str, body: String| {
        let path = bin.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    };
    let ffprobe = script(
        "ffprobe",
        format!("for x in \"$@\"; do [ \"$x\" = -show_stream_groups ] && exec /bin/cat '{}'; done; exec /bin/cat '{}'", groups.display(), tiles.display()),
    );
    // Writes the poster only when handed the stitching graph.
    let ffmpeg = script("ffmpeg", "for x in \"$@\"; do case \"$x\" in *xstack=inputs=4*) g=1;; esac; last=\"$x\"; done; [ -n \"$g\" ] && echo png > \"$last\"; exit 0".into());
    let m = make(&s.g, &a, &Tools { ffmpeg: Some(ffmpeg), ffprobe: Some(ffprobe), ..Tools::default() });
    assert_eq!((m.width, m.height), (Some(1000), Some(700)), "{m:?}");
    assert_eq!((m.thumb.as_str(), m.poster), (THUMB_READY, true), "the poster came from the stitched tiles");
}

#[test]
fn sips_answers_parse() {
    let out = "/x/a.png\n  pixelWidth: 200\n  pixelHeight: 100\n";
    assert_eq!(parse_sips(out), dims(Some(200), Some(100), None));
    assert_eq!(parse_sips("Error: nope"), Probed::default());
}

#[test]
fn the_queue_takes_each_asset_once_and_runs_two_workers() {
    let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let mut q = Queue::default();
    assert_eq!(q.push("h", &ids(&["a"])), 1);
    assert_eq!(q.push("h", &ids(&["a", "b", "c"])), 1, "a second worker, never a third");
    assert_eq!(q.push("h", &ids(&["d"])), 0);
    assert_eq!(q.push("other", &ids(&["a"])), 0, "another Hive's asset of the same id is its own item");
    let first = q.pop().unwrap();
    assert_eq!(first, ("h".into(), "a".into()));
    q.push("h", &ids(&["a"]));
    assert_eq!(q.items.len(), 4, "in hand, so not queued twice");
    q.finish(&first);
    q.push("h", &ids(&["a"]));
    let mut seen = Vec::new();
    while let Some((h, id)) = q.pop() {
        seen.push(format!("{h}/{id}"));
    }
    assert_eq!(seen, ["h/b", "h/c", "h/d", "other/a", "h/a"], "once finished, it queues again");
    assert_eq!(q.running, 1, "the worker that found it empty stopped");
    assert_eq!(q.pop(), None);
    assert_eq!(q.running, 0);
    assert_eq!(q.push("h", &ids(&["e"])), 1, "an idle queue starts a worker again");
}

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
    let base = std::env::temp_dir().join(format!("hive-gallery-thumbs-{}", uuid::Uuid::new_v4()));
    let g = Gallery::open(&base.join("gallery")).unwrap();
    Scratch { base, g }
}

fn add(s: &Scratch, ext: &str, src: Option<&Path>) -> Asset {
    let id = mint(ASSET_PREFIX);
    let file = format!("{id}.{ext}");
    let dst = s.g.files_dir().join(&file);
    match src {
        Some(src) => {
            fs::copy(src, &dst).unwrap();
        }
        None => fs::write(&dst, b"x").unwrap(),
    }
    s.g.insert_asset(NewAsset {
        id,
        kind: super::super::ingest::kind_for(ext).into(),
        file,
        mime: super::super::ingest::mime_for(ext).into(),
        bytes: 1,
        sha256: "x".into(),
        title: "t".into(),
        made_by: MadeBy::human(),
        ..NewAsset::default()
    })
    .unwrap()
}

#[test]
fn with_no_tools_a_tile_keeps_its_kind_icon() {
    let s = scratch();
    for ext in ["png", "mp4", "wav", "pdf", "zip"] {
        let a = add(&s, ext, None);
        let m = make(&s.g, &a, &Tools::default());
        assert_eq!((m.thumb.as_str(), m.poster, m.strip, m.wave), (THUMB_NONE, false, false, false), "{ext}");
    }
    assert!(!s.g.tmp_dir().exists(), "nothing ran, so no temp folder");
}

#[test]
fn a_tool_that_writes_nothing_is_a_failure_not_a_picture() {
    let s = scratch();
    let a = add(&s, "mp4", None);
    // `true` exits 0 and writes nothing: ffmpeg on a file it can't read does the same.
    let truthy = ["/usr/bin/true", "/bin/true"].into_iter().map(PathBuf::from).find(|p| p.exists());
    let Some(truthy) = truthy else { return };
    if cfg!(target_os = "macos") && !Path::new(job_sandbox::SANDBOX_EXEC).exists() {
        return;
    }
    let tools = Tools { ffmpeg: Some(truthy.clone()), ffprobe: Some(truthy), ..Tools::default() };
    let m = make(&s.g, &a, &tools);
    assert_eq!((m.thumb.as_str(), m.poster, m.strip), (THUMB_FAILED, false, false));
    assert!(fs::read_dir(s.g.thumbs_dir()).unwrap().next().is_none(), "nothing reached thumbs/");
    assert!(!s.g.tmp_dir().join(&a.id).exists(), "the temp folder goes");
}

/// The real tools under the real profile (macOS, ffmpeg installed): a PNG, a 3 s `testsrc` mp4
/// and a `sine` wav get their pictures, and inside the profile the network and writes outside the
/// asset's temp folder fail. `cargo test --lib hive::gallery::thumbs -- --ignored`
#[cfg(target_os = "macos")]
#[test]
#[ignore]
fn thumbs_probe() {
    let tools = Tools::detect();
    let ffmpeg = tools.ffmpeg.clone().expect("ffmpeg installed");
    let s = scratch();
    let src = s.base.join("src");
    fs::create_dir_all(&src).unwrap();
    let gen = |args: &[&str]| {
        let st = std::process::Command::new(&ffmpeg).args(["-nostdin", "-v", "error", "-y"]).args(args).status().unwrap();
        assert!(st.success(), "{args:?}");
    };
    gen(&["-f", "lavfi", "-i", "testsrc=size=320x240", "-frames:v", "1", src.join("i.png").to_str().unwrap()]);
    gen(&["-f", "lavfi", "-i", "testsrc=duration=3:size=320x240:rate=25", "-pix_fmt", "yuv420p", src.join("v.mp4").to_str().unwrap()]);
    gen(&["-f", "lavfi", "-i", "sine=frequency=440:duration=2", src.join("a.wav").to_str().unwrap()]);

    let image = add(&s, "png", Some(&src.join("i.png")));
    let m = make(&s.g, &image, &tools);
    assert_eq!((m.thumb.as_str(), m.poster, m.width, m.height), (THUMB_READY, true, Some(320), Some(240)));
    assert!(s.g.thumbs_dir().join(poster_name(&image.id)).is_file());

    // A HEIC is stored as 512x512 tiles: sips reads its real size, and without sips and
    // QuickLook ffmpeg's poster is the stitched picture, not its first tile.
    gen(&["-f", "lavfi", "-i", "testsrc=size=1200x900", "-frames:v", "1", src.join("big.png").to_str().unwrap()]);
    let heic = src.join("big.heic");
    let st = std::process::Command::new("/usr/bin/sips").args(["-s", "format", "heic"]).arg(src.join("big.png")).arg("--out").arg(&heic).output().unwrap();
    assert!(st.status.success() && heic.is_file());
    let tiled = add(&s, "heic", Some(&heic));
    let m = make(&s.g, &tiled, &tools);
    assert_eq!((m.thumb.as_str(), m.width, m.height), (THUMB_READY, Some(1200), Some(900)));
    let tiled = add(&s, "heic", Some(&heic));
    let m = make(&s.g, &tiled, &Tools { qlmanage: None, sips: None, ..tools.clone() });
    assert_eq!((m.thumb.as_str(), m.poster, m.width, m.height), (THUMB_READY, true, Some(1200), Some(900)));
    let poster = std::process::Command::new("/usr/bin/sips").args(["-g", "pixelWidth", "-g", "pixelHeight"]).arg(s.g.thumbs_dir().join(poster_name(&tiled.id))).output().unwrap();
    assert_eq!(parse_sips(&String::from_utf8_lossy(&poster.stdout)), dims(Some(512), Some(384), None), "the whole picture, fitted");

    let video = add(&s, "mp4", Some(&src.join("v.mp4")));
    let m = make(&s.g, &video, &tools);
    assert_eq!((m.thumb.as_str(), m.poster, m.strip, m.width, m.height), (THUMB_READY, true, true, Some(320), Some(240)));
    assert!(m.duration_ms.is_some_and(|d| (2_900..=3_100).contains(&d)), "{:?}", m.duration_ms);
    assert!(s.g.thumbs_dir().join(poster_name(&video.id)).is_file() && s.g.thumbs_dir().join(strip_name(&video.id)).is_file());

    let audio = add(&s, "wav", Some(&src.join("a.wav")));
    let m = make(&s.g, &audio, &tools);
    assert_eq!((m.thumb.as_str(), m.poster, m.wave), (THUMB_READY, false, true));
    assert!(m.duration_ms.is_some_and(|d| (1_900..=2_100).contains(&d)), "{:?}", m.duration_ms);
    assert!(s.g.thumbs_dir().join(wave_name(&audio.id)).is_file());
    assert!(fs::read_dir(s.g.tmp_dir()).map(|d| d.count()).unwrap_or(0) == 0, "every temp folder went");

    // Inside the profile: no network, and no write outside the temp folder.
    let tmp = s.base.join("jail");
    fs::create_dir_all(&tmp).unwrap();
    let curl = run_sandboxed(Path::new("/usr/bin/curl"), &["-sS".into(), "-m".into(), "5".into(), "-o".into(), "/dev/null".into(), "https://example.com".into()], &tmp).unwrap();
    assert!(!curl.status.success(), "a network call from inside the profile must fail");
    let outside = s.base.join("outside.png");
    let w = run_sandboxed(&ffmpeg, &["-nostdin".into(), "-v".into(), "error".into(), "-f".into(), "lavfi".into(), "-i".into(), "testsrc".into(), "-frames:v".into(), "1".into(), "-y".into(), outside.display().to_string()], &tmp).unwrap();
    assert!(!w.status.success() && !outside.exists(), "a write outside the temp folder must fail");
    let inside = tmp.join("inside.png");
    let w = run_sandboxed(&ffmpeg, &["-nostdin".into(), "-v".into(), "error".into(), "-f".into(), "lavfi".into(), "-i".into(), "testsrc".into(), "-frames:v".into(), "1".into(), "-y".into(), inside.display().to_string()], &tmp).unwrap();
    assert!(w.status.success() && inside.is_file(), "{}", String::from_utf8_lossy(&w.stderr));
}
