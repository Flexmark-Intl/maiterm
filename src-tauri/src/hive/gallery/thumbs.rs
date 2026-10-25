//! Thumbnails and media metadata (docs/hive.md §10.5): a poster (`thumbs/<id>.png`), a video's
//! hover strip (`<id>.strip.jpg`, ten frames across) and an audio waveform (`<id>.wave.png`,
//! white on transparent, so the view can tint it), plus dimensions and duration. Made off the
//! main thread when an asset is filed, by at most two workers in the process.
//!
//! What runs is planned from the asset's kind and the tools present (`plan`): QuickLook on macOS
//! (`qlmanage -t`) for images, video, documents and 3D, `sips` for an image's dimensions and
//! ffprobe for the rest (and for images without sips), ffmpeg for posters QuickLook didn't give,
//! strips and waveforms. With none of them a tile shows its kind icon; nothing is required.
//!
//! These tools decode files agents made, so ffmpeg, ffprobe and sips run under a Seatbelt profile
//! with the network closed and writes only to the asset's own temp folder (plan D8), with an
//! empty environment and file-only protocols. qlmanage runs as it is: its decoders already run in
//! Apple's sandboxed QuickLook services. Elsewhere (Linux) there is no profile to run them under,
//! so they run with the file-only protocol list alone.

use std::collections::{HashSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, LazyLock, OnceLock};
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::Value;
use tauri::{AppHandle, Manager};

use super::model::*;
use super::{Gallery, GALLERY_CHANGED_EVENT, THUMBS};
use crate::hive::job_sandbox;
use crate::state::AppState;
use crate::util::proc::{command, output_timeout};

/// Each tool call's deadline: QuickLook takes 0.15–0.4 s, a keyframe strip about 0.5 s (spike 7).
pub const STEP_TIMEOUT: Duration = Duration::from_secs(20);
/// Thumbnailers running at once in the process.
pub const WORKERS: usize = 2;
pub const STRIP_FRAMES: u32 = 10;
/// The box a poster fits in, and each strip frame's width.
pub const POSTER_PX: u32 = 512;
pub const STRIP_FRAME_PX: u32 = 256;
pub const WAVE_SIZE: &str = "1024x160";
/// Up to this long, a strip is ten seeks: a short generated clip is often one keyframe, so a
/// keyframe strip would be one frame and nine black ones. Longer, keyframes only, never a full
/// decode (§10.5).
pub const SEEK_STRIP_MAX_MS: u64 = 60_000;

pub const POSTER_OUT: &str = "poster.png";
pub const STRIP_OUT: &str = "strip.jpg";
pub const WAVE_OUT: &str = "wave.png";

/// What `thumbs/` holds for an asset, by part.
pub fn poster_name(id: &str) -> String {
    format!("{id}.png")
}
pub fn strip_name(id: &str) -> String {
    format!("{id}.strip.jpg")
}
pub fn wave_name(id: &str) -> String {
    format!("{id}.wave.png")
}

/// Which tools this machine has.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Have {
    pub ffmpeg: bool,
    pub ffprobe: bool,
    pub qlmanage: bool,
    pub sips: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// ffprobe: dimensions and duration.
    Probe,
    /// sips: an image's dimensions.
    Sips,
    /// qlmanage: the poster.
    QuickLook,
    /// ffmpeg: the poster, run only when QuickLook gave none.
    Poster,
    Strip,
    Wave,
}

impl Step {
    fn makes_picture(self) -> bool {
        matches!(self, Step::QuickLook | Step::Poster | Step::Strip | Step::Wave)
    }
}

/// The steps for one asset, in the order they run. A plan with no picture step leaves the tile
/// its kind icon (`thumb: "none"`).
pub fn plan(kind: &str, have: Have) -> Vec<Step> {
    let mut out = Vec::new();
    match kind {
        KIND_IMAGE => {
            // sips first: ImageIO reads every format it handles at its real size, a tiled HEIC
            // included, where ffprobe lists each tile as a stream of its own.
            if have.sips {
                out.push(Step::Sips);
            }
            if have.qlmanage {
                out.push(Step::QuickLook);
            }
            // After QuickLook: `make` skips it once sips and QuickLook gave the size and poster.
            if have.ffprobe {
                out.push(Step::Probe);
            }
            if have.ffmpeg {
                out.push(Step::Poster);
            }
        }
        KIND_VIDEO => {
            if have.ffprobe {
                out.push(Step::Probe);
            }
            if have.qlmanage {
                out.push(Step::QuickLook);
            }
            if have.ffmpeg {
                out.extend([Step::Poster, Step::Strip]);
            }
        }
        KIND_AUDIO => {
            if have.ffprobe {
                out.push(Step::Probe);
            }
            // QuickLook gives audio a generic icon (spike 7): the waveform is its picture.
            if have.ffmpeg {
                out.push(Step::Wave);
            }
        }
        KIND_DOCUMENT | KIND_MODEL3D if have.qlmanage => out.push(Step::QuickLook),
        _ => {}
    }
    out
}

/// Seconds as ffmpeg takes them.
fn secs(ms: u64) -> String {
    format!("{:.3}", ms as f64 / 1000.0)
}

/// Every ffmpeg/ffprobe input reads local files only: a playlist or concat file can't reach a URL.
const PROTOCOLS: [&str; 2] = ["-protocol_whitelist", "file"];

fn ffmpeg_head() -> Vec<String> {
    ["-nostdin", "-hide_banner", "-loglevel", "error"].map(String::from).to_vec()
}

fn input(file: &Path) -> Vec<String> {
    let mut v: Vec<String> = PROTOCOLS.map(String::from).to_vec();
    v.extend(["-i".to_string(), file.display().to_string()]);
    v
}

/// One step's arguments, after the program. `probed` is what the probes read so far.
pub fn argv(step: Step, kind: &str, file: &Path, tmp: &Path, probed: &Probed) -> Vec<String> {
    let duration_ms = probed.duration_ms;
    let out = |name: &str| tmp.join(name).display().to_string();
    let image_out = |name: &str| ["-frames:v".into(), "1".into(), "-update".into(), "1".into(), "-y".into(), out(name)];
    match step {
        Step::Probe => {
            let mut v: Vec<String> = ["-v", "error"].map(String::from).to_vec();
            v.extend(PROTOCOLS.map(String::from));
            v.extend(["-print_format", "json", "-show_streams", "-show_format"].map(String::from));
            // Asked only once a probe found tiles: an ffprobe before 7.0 refuses the option, and
            // the same release is the first to demux tiles at all.
            if probed.tiled {
                v.push("-show_stream_groups".into());
            }
            v.push(file.display().to_string());
            v
        }
        Step::Sips => vec!["-g".into(), "pixelWidth".into(), "-g".into(), "pixelHeight".into(), file.display().to_string()],
        Step::QuickLook => vec!["-t".into(), "-s".into(), POSTER_PX.to_string(), "-o".into(), tmp.display().to_string(), file.display().to_string()],
        Step::Poster => {
            let mut v = ffmpeg_head();
            let fit = format!("scale={POSTER_PX}:{POSTER_PX}:force_original_aspect_ratio=decrease");
            match probed.grid.as_ref().filter(|_| kind == KIND_IMAGE) {
                Some(g) => {
                    // ffmpeg 7.1 decodes a tiled image's first tile alone: stitch the tiles where
                    // the grid lays them, then crop to the picture.
                    v.extend(input(file));
                    let mut graph: String = g.tiles.iter().map(|(i, _, _)| format!("[0:{i}]")).collect();
                    if g.tiles.len() > 1 {
                        let layout: Vec<String> = g.tiles.iter().map(|(_, x, y)| format!("{x}_{y}")).collect();
                        graph.push_str(&format!("xstack=inputs={}:layout={},", g.tiles.len(), layout.join("|")));
                    }
                    graph.push_str(&format!("crop={}:{}:{}:{},{fit}", g.width, g.height, g.x, g.y));
                    v.extend(["-an".into(), "-filter_complex".into(), graph]);
                }
                None => {
                    if let Some(d) = duration_ms.filter(|_| kind == KIND_VIDEO) {
                        v.extend(["-ss".into(), secs(d / 10)]);
                    }
                    v.extend(input(file));
                    v.extend(["-an".into(), "-vf".into(), fit]);
                }
            }
            v.extend(image_out(POSTER_OUT));
            v
        }
        Step::Strip => {
            let mut v = ffmpeg_head();
            let frame = format!("scale={STRIP_FRAME_PX}:-2,setsar=1");
            match duration_ms {
                Some(d) if d > 0 && d <= SEEK_STRIP_MAX_MS => {
                    // Ten seeks, one per frame, each from the frame's middle of its tenth.
                    let mut graph = String::new();
                    for i in 0..STRIP_FRAMES as u64 {
                        v.extend(["-ss".into(), secs(d * (2 * i + 1) / (2 * STRIP_FRAMES as u64))]);
                        v.extend(input(file));
                        graph.push_str(&format!("[{i}:v]{frame}[f{i}];"));
                    }
                    for i in 0..STRIP_FRAMES {
                        graph.push_str(&format!("[f{i}]"));
                    }
                    graph.push_str(&format!("hstack=inputs={STRIP_FRAMES},format=yuvj420p"));
                    v.extend(["-filter_complex".into(), graph]);
                }
                d => {
                    // Keyframes only: never a full decode (spike 7). Spaced a tenth apart when
                    // the duration is known, else the first ten.
                    v.extend(["-skip_frame".into(), "nokey".into()]);
                    v.extend(input(file));
                    let pick = match d.filter(|d| *d > 0) {
                        Some(d) => format!("select='isnan(prev_selected_t)+gte(t-prev_selected_t\\,{})',", secs(d / STRIP_FRAMES as u64)),
                        None => String::new(),
                    };
                    v.extend(["-vf".into(), format!("{pick}{frame},format=yuvj420p,tile={STRIP_FRAMES}x1")]);
                }
            }
            v.extend(["-an".into(), "-q:v".into(), "5".into()]);
            v.extend(image_out(STRIP_OUT));
            v
        }
        Step::Wave => {
            let mut v = ffmpeg_head();
            v.extend(input(file));
            v.extend(["-filter_complex".into(), format!("showwavespic=s={WAVE_SIZE}:colors=white")]);
            v.extend(image_out(WAVE_OUT));
            v
        }
    }
}

/// What a probe read.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Probed {
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_ms: Option<u64>,
    /// An image stored as tiles (HEIC, HEIF, AVIF): each tile is a `dependent` stream, so no
    /// stream's size is the picture's. Its grid comes from a probe with `-show_stream_groups`.
    pub tiled: bool,
    pub grid: Option<Grid>,
}

/// A tiled image's tile grid group, as ffprobe lists it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Grid {
    /// The picture: a crop of the stitched tiles at (`x`, `y`).
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
    /// (stream index, x, y) per tile.
    pub tiles: Vec<(u32, u32, u32)>,
}

fn rotation(s: &Value) -> f64 {
    s["side_data_list"]
        .as_array()
        .and_then(|l| l.iter().find_map(|d| d["rotation"].as_f64()))
        .or_else(|| s["tags"]["rotate"].as_str().and_then(|r| r.parse::<f64>().ok()))
        .unwrap_or(0.0)
}

/// The grid and the group's rotation.
fn tile_grid(v: &Value) -> Option<(Grid, f64)> {
    let group = v["stream_groups"].as_array()?.iter().find(|g| g["type"] == "Tile Grid")?;
    let c = &group["components"][0];
    let n = |x: &Value| x.as_u64().and_then(|n| u32::try_from(n).ok());
    let tiles = c["subcomponents"]
        .as_array()?
        .iter()
        .map(|t| Some((n(&t["stream_index"])?, n(&t["tile_horizontal_offset"])?, n(&t["tile_vertical_offset"])?)))
        .collect::<Option<Vec<_>>>()
        .filter(|t| !t.is_empty())?;
    let grid = Grid {
        width: n(&c["width"]).filter(|w| *w > 0)?,
        height: n(&c["height"]).filter(|h| *h > 0)?,
        x: n(&c["horizontal_offset"]).unwrap_or(0),
        y: n(&c["vertical_offset"]).unwrap_or(0),
        tiles,
    };
    Some((grid, rotation(group)))
}

/// ffprobe's JSON: an image's or video's first picture stream (never an audio file's cover art,
/// never one tile of a tiled image), or a tiled image's grid, turned a quarter as its rotation
/// says, and a video's or audio's duration.
pub fn parse_probe(kind: &str, json: &str) -> Probed {
    let Ok(v) = serde_json::from_str::<Value>(json) else { return Probed::default() };
    let streams = v["streams"].as_array().cloned().unwrap_or_default();
    let mut out = Probed::default();
    if kind == KIND_IMAGE || kind == KIND_VIDEO {
        let flag = |s: &Value, k: &str| s["disposition"][k].as_i64().unwrap_or(0) == 1;
        let pictures: Vec<&Value> = streams.iter().filter(|s| s["codec_type"] == "video" && !flag(s, "dependent")).collect();
        out.tiled = kind == KIND_IMAGE && streams.iter().any(|s| s["codec_type"] == "video" && flag(s, "dependent"));
        let quarter = |r: f64| (r.round() as i64).rem_euclid(180) == 90;
        if let Some((g, r)) = tile_grid(&v).filter(|_| out.tiled) {
            (out.width, out.height) = if quarter(r) { (Some(g.height), Some(g.width)) } else { (Some(g.width), Some(g.height)) };
            out.grid = Some(g);
        } else if !out.tiled {
            // A tiled image's other streams (its thumbnail) aren't its size either.
            let pic = pictures.iter().find(|s| !flag(s, "attached_pic")).or_else(|| pictures.first());
            if let Some(s) = pic {
                let dim = |k: &str| s[k].as_u64().filter(|n| *n > 0 && *n <= u32::MAX as u64).map(|n| n as u32);
                (out.width, out.height) = if quarter(rotation(s)) { (dim("height"), dim("width")) } else { (dim("width"), dim("height")) };
            }
        }
    }
    if kind == KIND_VIDEO || kind == KIND_AUDIO {
        let seconds = |x: &Value| x.as_str().and_then(|d| d.parse::<f64>().ok()).filter(|d| d.is_finite() && *d > 0.0);
        out.duration_ms = seconds(&v["format"]["duration"])
            .or_else(|| streams.iter().find_map(|s| seconds(&s["duration"])))
            .map(|d| (d * 1000.0).round() as u64);
    }
    out
}

/// `sips -g pixelWidth -g pixelHeight`'s answer.
pub fn parse_sips(out: &str) -> Probed {
    let num = |key: &str| {
        out.lines()
            .find_map(|l| l.trim().strip_prefix(key).and_then(|r| r.trim().parse::<u32>().ok()))
            .filter(|n| *n > 0)
    };
    Probed { width: num("pixelWidth:"), height: num("pixelHeight:"), ..Probed::default() }
}

/// The tools found on this machine.
#[derive(Debug, Clone, Default)]
pub struct Tools {
    pub ffmpeg: Option<PathBuf>,
    pub ffprobe: Option<PathBuf>,
    pub qlmanage: Option<PathBuf>,
    pub sips: Option<PathBuf>,
}

impl Tools {
    /// Looked up fresh: a GUI app's PATH is launchd's, so the usual bins are searched too.
    pub fn detect() -> Self {
        let find = |cli: &str| crate::accounts::find_executables(cli).into_iter().next();
        let mac = cfg!(target_os = "macos");
        // On macOS a decoder runs only inside its profile, which needs sandbox-exec.
        let decoders = !mac || Path::new(job_sandbox::SANDBOX_EXEC).exists();
        let decoder = |cli: &str| if decoders { find(cli) } else { None };
        Self {
            ffmpeg: decoder("ffmpeg"),
            ffprobe: decoder("ffprobe"),
            qlmanage: if mac { find("qlmanage") } else { None },
            sips: if mac { decoder("sips") } else { None },
        }
    }

    pub fn have(&self) -> Have {
        Have { ffmpeg: self.ffmpeg.is_some(), ffprobe: self.ffprobe.is_some(), qlmanage: self.qlmanage.is_some(), sips: self.sips.is_some() }
    }

    fn program(&self, step: Step) -> Option<&Path> {
        match step {
            Step::Probe => self.ffprobe.as_deref(),
            Step::Sips => self.sips.as_deref(),
            Step::QuickLook => self.qlmanage.as_deref(),
            Step::Poster | Step::Strip | Step::Wave => self.ffmpeg.as_deref(),
        }
    }
}

/// Runs a decoder on an agent's file: under a profile with the network closed and writes only to
/// `tmp` on macOS, with an empty environment everywhere.
pub fn run_sandboxed(program: &Path, args: &[String], tmp: &Path) -> Result<Output, String> {
    let mut cmd = if cfg!(target_os = "macos") {
        let real = job_sandbox::real_path(tmp);
        let profile = job_sandbox::profile(&job_sandbox::ProfileSpec { allow: vec![real], ..Default::default() })?;
        let mut c = command(job_sandbox::SANDBOX_EXEC);
        c.arg("-p").arg(profile).arg(program);
        c
    } else {
        command(program)
    };
    cmd.args(args).current_dir(tmp).env_clear().env("PATH", "/usr/bin:/bin").env("TMPDIR", tmp);
    output_timeout(&mut cmd, STEP_TIMEOUT).map_err(|e| format!("{}: {e}", program.display()))
}

fn run(tools: &Tools, step: Step, args: &[String], tmp: &Path) -> Result<Output, String> {
    let program = tools.program(step).ok_or("not installed")?;
    if step == Step::QuickLook {
        let mut cmd = command(program);
        cmd.args(args).current_dir(tmp);
        return output_timeout(&mut cmd, STEP_TIMEOUT).map_err(|e| format!("{}: {e}", program.display()));
    }
    run_sandboxed(program, args, tmp)
}

/// A non-empty regular file at `p`: a tool can exit 0 having written nothing.
fn made(p: &Path) -> bool {
    fs::symlink_metadata(p).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Makes one asset's pictures and reads its metadata. Outputs go to `.tmp/<id>/`, then are
/// renamed into `thumbs/`, so a half-written picture is never served.
pub fn make(g: &Gallery, a: &Asset, tools: &Tools) -> MediaMeta {
    let steps = plan(&a.kind, tools.have());
    if steps.is_empty() {
        return MediaMeta { thumb: THUMB_NONE.into(), ..MediaMeta::default() };
    }
    let tmp = g.tmp_dir().join(&a.id);
    let _ = fs::remove_dir_all(&tmp);
    if let Err(e) = fs::create_dir_all(&tmp) {
        log::warn!("gallery {}: no temp folder for {}: {e}", g.root().display(), a.id);
        return MediaMeta { thumb: THUMB_FAILED.into(), ..MediaMeta::default() };
    }
    let file = g.file_path(a);
    let mut meta = MediaMeta::default();
    let mut probed = Probed::default();
    let (mut tried, mut failures) = (false, Vec::new());
    for step in &steps {
        let skip = match step {
            Step::Poster if !meta.poster && probed.tiled && probed.grid.is_none() => {
                // One tile would be a corner of the picture: the kind icon is truer.
                failures.push("Poster: tiled, and no tile grid read".to_string());
                true
            }
            Step::Poster => meta.poster,
            // An image's probe gives its size and its tiles: not needed once sips and QuickLook
            // gave the size and the poster.
            Step::Probe => a.kind == KIND_IMAGE && probed.width.is_some() && (meta.poster || tools.ffmpeg.is_none()),
            _ => false,
        };
        if skip {
            continue;
        }
        let args = argv(*step, &a.kind, &file, &tmp, &probed);
        let res = run(tools, *step, &args, &tmp);
        tried |= step.makes_picture();
        let out = match res {
            Ok(o) => o,
            Err(e) => {
                failures.push(format!("{step:?}: {e}"));
                continue;
            }
        };
        match step {
            Step::Probe | Step::Sips => {
                let text = String::from_utf8_lossy(&out.stdout);
                let mut p = if *step == Step::Probe { parse_probe(&a.kind, &text) } else { parse_sips(&text) };
                if p.tiled && p.grid.is_none() {
                    // A second probe, for the tile grid only it lists.
                    match run(tools, Step::Probe, &argv(Step::Probe, &a.kind, &file, &tmp, &p), &tmp) {
                        Ok(o) => p = parse_probe(&a.kind, &String::from_utf8_lossy(&o.stdout)),
                        Err(e) => failures.push(format!("Probe (tile grid): {e}")),
                    }
                }
                probed.width = probed.width.or(p.width);
                probed.height = probed.height.or(p.height);
                probed.duration_ms = probed.duration_ms.or(p.duration_ms);
                probed.tiled |= p.tiled;
                probed.grid = probed.grid.take().or(p.grid);
            }
            Step::QuickLook => {
                // qlmanage names its output after the input: `<file>.png`.
                let ql = tmp.join(format!("{}.png", a.file));
                if made(&ql) {
                    meta.poster = fs::rename(&ql, tmp.join(POSTER_OUT)).is_ok();
                }
            }
            Step::Poster => meta.poster = made(&tmp.join(POSTER_OUT)),
            Step::Strip => meta.strip = made(&tmp.join(STRIP_OUT)),
            Step::Wave => meta.wave = made(&tmp.join(WAVE_OUT)),
        }
        if step.makes_picture() && !out.status.success() {
            failures.push(format!("{step:?}: {}", String::from_utf8_lossy(&out.stderr).lines().next().unwrap_or("failed")));
        }
    }
    (meta.width, meta.height, meta.duration_ms) = (probed.width, probed.height, probed.duration_ms);
    let thumbs = g.root().join(THUMBS);
    for (ok, from, to) in [(&mut meta.poster, POSTER_OUT, poster_name(&a.id)), (&mut meta.strip, STRIP_OUT, strip_name(&a.id)), (&mut meta.wave, WAVE_OUT, wave_name(&a.id))] {
        if *ok {
            if let Err(e) = fs::rename(tmp.join(from), thumbs.join(&to)) {
                log::warn!("gallery {}: {to} not kept: {e}", g.root().display());
                *ok = false;
            }
        }
    }
    let _ = fs::remove_dir_all(&tmp);
    meta.thumb = if meta.poster || meta.strip || meta.wave {
        THUMB_READY
    } else if tried {
        THUMB_FAILED
    } else {
        THUMB_NONE
    }
    .into();
    if !failures.is_empty() {
        log::info!("gallery {}: {} ({}): {}", g.root().display(), a.id, a.kind, failures.join("; "));
    }
    meta
}

// ── The queue ──

/// The process's work: (hive id, asset id) pairs, each queued or in hand once (two workers on one
/// asset would share its temp folder), and how many workers run.
#[derive(Debug, Default)]
pub struct Queue {
    items: VecDeque<(String, String)>,
    held: HashSet<(String, String)>,
    running: usize,
}

impl Queue {
    /// Queues what isn't queued or in hand already; answers how many workers to start.
    pub fn push(&mut self, hive_id: &str, ids: &[String]) -> usize {
        for id in ids {
            let key = (hive_id.to_string(), id.clone());
            if self.held.insert(key.clone()) {
                self.items.push_back(key);
            }
        }
        let start = WORKERS.saturating_sub(self.running).min(self.items.len());
        self.running += start;
        start
    }

    /// The next item, or None, and the worker that asked stops counting as running.
    pub fn pop(&mut self) -> Option<(String, String)> {
        let next = self.items.pop_front();
        if next.is_none() {
            self.running = self.running.saturating_sub(1);
        }
        next
    }

    /// An item's run is over: it may be queued again.
    pub fn finish(&mut self, key: &(String, String)) {
        self.held.remove(key);
    }
}

static QUEUE: LazyLock<Mutex<Queue>> = LazyLock::new(|| Mutex::new(Queue::default()));
/// Set once at startup: without it (tests, before setup) nothing is queued, and assets stay
/// `pending` for the next process's `resume`.
static HANDLE: OnceLock<AppHandle> = OnceLock::new();

/// Starts the thumbnailer: from here on filed assets are queued, and every gallery already open
/// picks up what it has waiting.
pub fn install(handle: AppHandle) {
    if HANDLE.set(handle).is_err() {
        return;
    }
    for (hive_id, g) in super::open_galleries() {
        resume(&hive_id, &g);
    }
}

/// Queues assets for thumbnails. Each is looked up when its turn comes: one trashed, purged or
/// already made by then is skipped.
pub fn enqueue(hive_id: &str, ids: Vec<String>) {
    if ids.is_empty() || HANDLE.get().is_none() {
        return;
    }
    let start = QUEUE.lock().push(hive_id, &ids);
    for _ in 0..start {
        tauri::async_runtime::spawn_blocking(drain);
    }
}

/// Re-queues a gallery's waiting assets: the first time it opens in a process, since a crash or
/// a quit can leave some `pending`.
pub fn resume(hive_id: &str, g: &Gallery) {
    if HANDLE.get().is_none() {
        return;
    }
    match g.pending_thumbs() {
        Ok(ids) if !ids.is_empty() => {
            log::info!("gallery {}: {} asset(s) waiting for thumbnails", g.root().display(), ids.len());
            enqueue(hive_id, ids);
        }
        Ok(_) => {}
        Err(e) => log::warn!("gallery {}: waiting thumbnails not read: {e}", g.root().display()),
    }
}

fn drain() {
    let tools = Tools::detect();
    loop {
        let Some(key) = QUEUE.lock().pop() else { return };
        let (hive_id, id) = (&key.0, &key.1);
        // Only an open gallery: a forgotten one is a Hive being deleted, never reopened here.
        // A panic must still hand the item back, or it could never be queued again.
        let changed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| super::cached(hive_id).is_some_and(|g| thumbnail_one(&g, id, &tools))))
            .unwrap_or_else(|_| {
                log::error!("gallery: thumbnailing {id} in hive {hive_id} panicked");
                false
            });
        QUEUE.lock().finish(&key);
        if changed {
            announce(hive_id, id);
        }
    }
}

/// Answers whether the asset changed.
fn thumbnail_one(g: &Arc<Gallery>, id: &str, tools: &Tools) -> bool {
    let a = match g.get(id) {
        Ok(Some(a)) if !a.in_trash() && a.thumb == THUMB_PENDING => a,
        Ok(_) => return false,
        Err(e) => {
            log::warn!("gallery {}: {id} not read for thumbnails: {e}", g.root().display());
            return false;
        }
    };
    let meta = make(g, &a, tools);
    if let Err(e) = g.set_meta(id, &meta) {
        log::warn!("gallery {}: {id}'s thumbnails not recorded: {e}", g.root().display());
        return false;
    }
    // Emptied from the trash while it ran: the purge already swept thumbs/, so sweep again.
    if g.get(id).ok().flatten().is_some_and(|a| a.purged) {
        for name in [poster_name(id), strip_name(id), wave_name(id)] {
            let _ = fs::remove_file(g.root().join(THUMBS).join(name));
        }
        return false;
    }
    true
}

/// `hive-gallery-changed` to the window holding the Hive, as a tool's change goes.
fn announce(hive_id: &str, id: &str) {
    use tauri::Emitter;
    let Some(handle) = HANDLE.get() else { return };
    let Some(state) = handle.try_state::<Arc<AppState>>() else { return };
    let target = {
        let data = state.app_data.read();
        data.windows.iter().find_map(|w| {
            w.workspaces.iter().find(|ws| ws.hive.as_ref().is_some_and(|h| h.id == hive_id)).map(|ws| (w.label.clone(), ws.id.clone()))
        })
    };
    let Some((label, workspace_id)) = target else { return };
    let payload = serde_json::json!({ "workspace_id": workspace_id, "hive_id": hive_id, "ids": [id] });
    if let Err(e) = handle.emit_to(label.as_str(), GALLERY_CHANGED_EVENT, payload) {
        log::warn!("hive {hive_id}: thumbnail news not sent to window {label}: {e}");
    }
}

#[cfg(test)]
mod tests;
