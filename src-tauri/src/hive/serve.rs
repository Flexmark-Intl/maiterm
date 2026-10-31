//! A worker runs its app and looks at it (docs/hive.md §6.7, §6.8): `hiveRun` `serve`,
//! `hiveFetch`, `hiveScreenshot`, and the coordinator's `hiveApproveServe`.
//!
//! A worker's own shell can't listen on a port or reach one: localhost stays closed to it, since
//! the only switches that open it open every port, maiTerm's own included (§6.7). So the app runs
//! as a queued job under the job sandbox (`job_sandbox`), which may bind exactly the port booked
//! for it, and maiTerm, outside every sandbox, is what reaches that port: an HTTP request
//! (`hiveFetch`) or headless Chrome (`hiveScreenshot`). Both reach only a port booked for one of
//! the CALLING worker's own running served apps, never another worker's, never an arbitrary one,
//! never maiTerm's.
//!
//! A served command the human didn't queue is asked of the coordinator (the Queen), never the
//! human (Darryl, 2026-10-31): her answer stands for that command in that Hive
//! (`logs/serve-approvals.jsonl`, the latest answer winning) and is a line in the decision log.
//! Whoever approves, the job runs under the worker's sandbox and reaches nothing but its own port.
//!
//! Chrome itself isn't sandboxed by maiTerm, and the page is the worker's code: so every request
//! it makes goes to a proxy that answers nothing, except the app's own port (`chrome_args`), and
//! its profile is a fresh folder maiTerm owns. Page text that reaches an agent is
//! `frame_escape`d.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::AppHandle;

use super::deliver::{frame_escape, one_line};
use super::paths::HivePaths;
use super::queue::{self, Job, JobSpec};
use super::rpc::HiveWindow;
use super::tools::{blocking, worker_label, Caller, Ctx};
use super::worker::WorkerRecord;
use crate::state::{AppState, Service};

pub const FETCH: &str = "hiveFetch";
pub const SCREENSHOT: &str = "hiveScreenshot";
pub const APPROVE: &str = "hiveApproveServe";

/// A worker's served apps at once: an app and its API, and one to spare.
pub const MAX_SERVES: usize = 3;
pub const READY_DEFAULT: Duration = Duration::from_secs(120);
pub const READY_MAX_S: u64 = 600;
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(20);
/// What a fetch reads of a body, and what of it reaches the agent.
pub const MAX_BODY_BYTES: usize = 512 * 1024;
pub const MAX_BODY_CHARS: usize = 20_000;
pub const MAX_REQUEST_BODY: usize = 64 * 1024;
const MAX_HEADERS: usize = 40;
const MAX_PATH: usize = 2_000;
const MAX_REASON: usize = 500;
pub const SHOT_TIMEOUT: Duration = Duration::from_secs(45);
pub const SHOT_DEFAULT: (u32, u32) = (1280, 800);
pub const SHOT_MIN: u32 = 200;
pub const SHOT_MAX: (u32, u32) = (3840, 4000);
/// Chrome's virtual time for scripts and timers to settle before the picture.
pub const VIRTUAL_TIME_MS: u32 = 5_000;
const MAX_SHOT_BYTES: u64 = 40 * 1024 * 1024;
/// Under the worker's scratch folder.
pub const SHOTS_DIR: &str = "screenshots";
/// Loopback as maiTerm reaches it: a server on `localhost` may have bound either.
const HOSTS: [&str; 2] = ["127.0.0.1", "[::1]"];
pub const METHODS: [&str; 7] = ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"];

pub const NO_BROWSER: &str = "maiTerm found no Chrome, Chromium or Edge on this computer, so it can't take a screenshot. \
    hiveFetch still reads your app's pages; ask your coordinator if the human should install Chrome.";

// ── The served app's words, env and URL ──

/// `{port}` in a served app's words becomes its booked port.
pub fn fill_port(argv: &[String], port: u16) -> Vec<String> {
    argv.iter().map(|a| a.replace("{port}", &port.to_string())).collect()
}

/// What a served app is told: its port, on loopback.
pub fn serve_env(port: u16) -> Vec<(String, String)> {
    vec![("PORT".to_string(), port.to_string()), ("HOST".to_string(), "127.0.0.1".to_string())]
}

pub fn url_of(port: u16, path: &str) -> String {
    format!("http://127.0.0.1:{port}{path}")
}

/// The part of a URL after the port: `/` and on, no scheme or host of its own, nothing a parser
/// could read as one (`\`, whitespace, a second leading `/`).
pub fn check_path(path: Option<&str>) -> Result<String, String> {
    let p = path.map(str::trim).filter(|p| !p.is_empty()).unwrap_or("/");
    if p.chars().count() > MAX_PATH {
        return Err(format!("path is over {MAX_PATH} characters."));
    }
    if !p.starts_with('/') || p.starts_with("//") || p.chars().any(|c| c.is_whitespace() || c.is_control() || c == '\\') {
        return Err("path is the part of the URL after the port, starting with one / (`/`, `/about?tab=2`), with no spaces or backslashes.".to_string());
    }
    Ok(p.to_string())
}

/// The URL maiTerm requests, checked after parsing: loopback, the app's port, nothing else.
pub fn target_url(host: &str, port: u16, path: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(&format!("http://{host}:{port}{path}")).map_err(|e| format!("{path} isn't a path maiTerm can request: {e}"))?;
    if url.scheme() != "http" || !HOSTS.contains(&url.host_str().unwrap_or_default()) || url.port() != Some(port) || !url.username().is_empty() {
        return Err(format!("{} isn't your app's address.", one_line(path, 120)));
    }
    Ok(url)
}

// ── Which port a worker may reach ──

/// The port of `job` if it is one of the caller's own running served apps, booked for it now and
/// none of maiTerm's: the one rule `hiveFetch` and `hiveScreenshot` reach a port by. Anything else
/// (another worker's job, another Hive's, a run that isn't served, an unknown id) gets the same
/// refusal, which says nothing about whose it was.
pub fn serve_port(job: Option<&Job>, task_id: &str, hive_id: &str, booked: &[u16], forbidden: &[u16]) -> Result<u16, String> {
    let j = job.filter(|j| j.serve && j.hive_id == hive_id && j.task_id == task_id && !task_id.is_empty()).ok_or_else(|| {
        format!("{FETCH} and {SCREENSHOT} reach only your own served apps: start one with hiveRun {{serve: true, command: …}} and use the job id it answers.")
    })?;
    if j.state != queue::RUNNING {
        return Err(match j.is_active() {
            true => format!("Your app {} hasn't started yet (it is {}): try again in a moment.", j.id, j.state),
            false => format!("Your app {} isn't running any more (it is {}): hiveRun {{serve: true}} starts it again.", j.id, j.state),
        });
    }
    let port = j.ports.first().copied().ok_or_else(|| format!("Your app {} has no port yet.", j.id))?;
    if !booked.contains(&port) || forbidden.contains(&port) {
        return Err(format!("Port {port} isn't booked for your app {} any more.", j.id));
    }
    Ok(port)
}

/// The ports no job may have, as `queue::prepare` reads them.
fn forbidden_ports(state: &AppState) -> Vec<u16> {
    crate::stack::ports::forbidden(*state.mcp_port.read(), state.mailink_info.read().as_ref().map(|(_, p)| *p))
}

/// `serve_port` for a job id the caller named, against the live queue and port book.
fn own_port(state: &AppState, caller: &Caller, w: &WorkerRecord, id: &str) -> Result<u16, String> {
    if !queue::is_job_id(id) {
        return Err("job is required: your served app's job id (j- and 8 hex digits), from hiveRun's answer.".to_string());
    }
    serve_port(queue::job(id).as_ref(), &w.task_id, &caller.hive.id, &crate::stack::ports::held(id), &forbidden_ports(state))
}

// ── HTTP, from maiTerm ──

fn client(timeout: Duration) -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .no_proxy()
        // A redirect is the app's answer, shown as such: following it could leave the port.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .map_err(|e| format!("maiTerm couldn't make an HTTP client: {e}"))
}

async fn answers(client: &reqwest::Client, port: u16) -> bool {
    for host in HOSTS {
        if let Ok(url) = target_url(host, port, "/") {
            if client.get(url).send().await.is_ok() {
                return true;
            }
        }
    }
    false
}

/// Until something answers HTTP on `port` (any status), `timeout` passes, or `alive` says the job
/// is gone.
pub async fn wait_http(port: u16, timeout: Duration, mut alive: impl FnMut() -> Result<(), String>) -> Result<(), String> {
    let client = client(Duration::from_secs(3))?;
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        alive()?;
        if answers(&client, port).await {
            return Ok(());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!("nothing answered HTTP on port {port} within {} s", timeout.as_secs()));
        }
        tokio::time::sleep(Duration::from_millis(400)).await;
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Fetched {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    /// Text, `frame_escape`d; or a note of what wasn't shown.
    pub body: String,
    pub bytes: usize,
    pub truncated: bool,
}

fn is_text(content_type: &str, body: &[u8]) -> bool {
    let ct = content_type.to_ascii_lowercase();
    if ct.is_empty() {
        return std::str::from_utf8(body).is_ok() || std::str::from_utf8(&body[..body.len().saturating_sub(3)]).is_ok();
    }
    ct.starts_with("text/") || ["json", "xml", "javascript", "html", "x-www-form-urlencoded", "svg"].iter().any(|t| ct.contains(t))
}

async fn read_response(mut resp: reqwest::Response) -> Result<Fetched, String> {
    let status = resp.status().as_u16();
    let headers: Vec<(String, String)> =
        resp.headers().iter().take(MAX_HEADERS).map(|(k, v)| (k.as_str().to_string(), one_line(&String::from_utf8_lossy(v.as_bytes()), 300))).collect();
    let ct = resp.headers().get(reqwest::header::CONTENT_TYPE).map(|v| String::from_utf8_lossy(v.as_bytes()).to_string()).unwrap_or_default();
    let (mut buf, mut truncated) = (Vec::new(), false);
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                let room = MAX_BODY_BYTES - buf.len();
                if c.len() >= room {
                    buf.extend_from_slice(&c[..room]);
                    truncated = c.len() > room || resp.content_length().is_some_and(|n| n as usize > MAX_BODY_BYTES);
                    if truncated {
                        break;
                    }
                } else {
                    buf.extend_from_slice(&c);
                }
            }
            Ok(None) => break,
            // A stream that never ends (server-sent events) or stalls: what came is the answer.
            Err(e) if !buf.is_empty() => {
                log::info!("hive serve: a body cut short after {} bytes: {e}", buf.len());
                truncated = true;
                break;
            }
            Err(e) => return Err(format!("reading your app's answer failed: {}", one_line(&e.to_string(), 200))),
        }
    }
    let body = if buf.is_empty() {
        String::new()
    } else if is_text(&ct, &buf) {
        let text = String::from_utf8_lossy(&buf);
        let cut: String = text.chars().take(MAX_BODY_CHARS).collect();
        if cut.len() < text.len() {
            truncated = true;
        }
        frame_escape(&cut)
    } else {
        format!("[{} bytes of {}, not shown: hiveScreenshot shows a page]", buf.len(), one_line(if ct.is_empty() { "binary data" } else { &ct }, 80))
    };
    Ok(Fetched { status, headers, body, bytes: buf.len(), truncated })
}

/// One request to the app on `port`, from maiTerm. Never through a proxy, never following a
/// redirect.
pub async fn fetch(port: u16, path: &str, method: &str, body: Option<String>) -> Result<Fetched, String> {
    let method = method.trim().to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(format!("method is one of {}.", METHODS.join(", ")));
    }
    let m = reqwest::Method::from_bytes(method.as_bytes()).map_err(|e| e.to_string())?;
    let client = client(FETCH_TIMEOUT)?;
    let mut last = String::new();
    for host in HOSTS {
        let mut req = client.request(m.clone(), target_url(host, port, path)?);
        if let Some(b) = &body {
            let ct = if serde_json::from_str::<Value>(b).is_ok() { "application/json" } else { "text/plain; charset=utf-8" };
            req = req.header(reqwest::header::CONTENT_TYPE, ct).body(b.clone());
        }
        match req.send().await {
            Ok(resp) => return read_response(resp).await,
            Err(e) if e.is_connect() => last = e.to_string(),
            Err(e) => return Err(format!("the request to your app failed: {}", one_line(&e.to_string(), 200))),
        }
    }
    Err(format!("nothing answered on port {port}: {}", one_line(&last, 200)))
}

// ── Headless Chrome ──

/// Where Chrome, Chromium or Edge may be on this OS, best first.
pub fn browser_candidates() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let apps = [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Chromium.app/Contents/MacOS/Chromium",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Brave Browser.app/Contents/MacOS/Brave Browser",
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        let mut roots = vec![PathBuf::from("/Applications")];
        roots.extend(dirs::home_dir().map(|h| h.join("Applications")));
        for a in apps {
            for r in &roots {
                out.push(r.join(a));
            }
        }
    }
    #[cfg(windows)]
    {
        let rel = [r"Google\Chrome\Application\chrome.exe", r"Chromium\Application\chrome.exe", r"Microsoft\Edge\Application\msedge.exe"];
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"].iter().filter_map(|v| std::env::var_os(v).map(PathBuf::from)).collect();
        for r in rel {
            for root in &roots {
                out.push(root.join(r));
            }
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "microsoft-edge-stable", "brave-browser"];
        out.extend(path_candidates(&names, std::env::var_os("PATH").as_deref()));
        for n in names {
            out.push(PathBuf::from("/usr/bin").join(n));
            out.push(PathBuf::from("/snap/bin").join(n));
        }
    }
    out
}

/// Each name in each `PATH` folder, names first: a Chrome anywhere beats a Chromium first on PATH.
pub fn path_candidates(names: &[&str], path: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let dirs: Vec<PathBuf> = path.map(|p| std::env::split_paths(p).collect()).unwrap_or_default();
    names.iter().flat_map(|n| dirs.iter().map(move |d| d.join(n))).collect()
}

/// The first candidate that is a file.
pub fn find_browser_in(candidates: &[PathBuf], is_file: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    candidates.iter().find(|p| is_file(p)).cloned()
}

pub fn find_browser() -> Option<PathBuf> {
    find_browser_in(&browser_candidates(), |p| p.is_file())
}

/// One screenshot's command line.
#[derive(Debug, Clone)]
pub struct Shot<'a> {
    pub out: &'a Path,
    /// A fresh folder maiTerm owns: never the human's profile.
    pub profile: &'a Path,
    pub width: u32,
    pub height: u32,
    pub port: u16,
    pub path: &'a str,
    /// A port where nothing proxies: every request but the app's own goes there and fails.
    pub dead_proxy: u16,
}

/// Chrome's arguments for one picture of `http://localhost:<port><path>`. `<-loopback>` takes
/// away Chrome's own exemption of loopback from the proxy, so only the app's port goes direct:
/// the page can't reach maiTerm's ports, another worker's app or the internet.
pub fn chrome_args(s: &Shot) -> Vec<String> {
    let p = s.port;
    vec![
        "--headless=new".to_string(),
        "--disable-gpu".to_string(),
        "--hide-scrollbars".to_string(),
        "--mute-audio".to_string(),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
        "--disable-extensions".to_string(),
        "--disable-sync".to_string(),
        "--disable-background-networking".to_string(),
        "--disable-component-update".to_string(),
        "--disable-default-apps".to_string(),
        "--use-mock-keychain".to_string(),
        "--password-store=basic".to_string(),
        format!("--user-data-dir={}", s.profile.display()),
        format!("--window-size={},{}", s.width, s.height),
        format!("--screenshot={}", s.out.display()),
        format!("--proxy-server=http://127.0.0.1:{}", s.dead_proxy),
        format!("--proxy-bypass-list=<-loopback>;127.0.0.1:{p};localhost:{p};[::1]:{p}"),
        format!("--virtual-time-budget={VIRTUAL_TIME_MS}"),
        format!("http://localhost:{p}{}", s.path),
    ]
}

/// A proxy that takes every connection and closes it: Chrome's requests for anything but the
/// app fail at once rather than waiting out a timeout.
struct DeadProxy {
    port: u16,
    stop: Arc<std::sync::atomic::AtomicBool>,
}

impl DeadProxy {
    fn start() -> Result<Self, String> {
        let l = std::net::TcpListener::bind(("127.0.0.1", 0)).map_err(|e| format!("maiTerm couldn't open its screenshot proxy: {e}"))?;
        let port = l.local_addr().map_err(|e| e.to_string())?.port();
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let s = stop.clone();
        std::thread::spawn(move || {
            for c in l.incoming() {
                if s.load(std::sync::atomic::Ordering::SeqCst) {
                    break;
                }
                drop(c);
            }
        });
        Ok(Self { port, stop })
    }
}

impl Drop for DeadProxy {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        // Wakes the accept loop so it sees the stop.
        let _ = std::net::TcpStream::connect_timeout(&std::net::SocketAddr::from(([127, 0, 0, 1], self.port)), Duration::from_millis(200));
    }
}

/// Runs the browser to its exit or `timeout`, then kills everything it started.
async fn run_browser(bin: &Path, args: &[String], timeout: Duration) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(bin);
    cmd.args(args).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    let mut child = cmd.spawn().map_err(|e| format!("maiTerm couldn't start {}: {e}", bin.display()))?;
    let pid = child.id();
    let status = tokio::time::timeout(timeout, child.wait()).await;
    kill_tree(pid);
    match status {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("waiting for the browser failed: {e}")),
        Err(_) => {
            let _ = child.start_kill();
            let _ = child.wait().await;
            Err(format!("the browser took over {} s", timeout.as_secs()))
        }
    }
}

/// The browser and every helper it left: its process group (unix), its process tree (Windows).
fn kill_tree(pid: Option<u32>) {
    let Some(pid) = pid else { return };
    #[cfg(unix)]
    {
        // SAFETY: signalling the process group this run created (process_group(0) made the
        // browser its leader); a stale id at worst names an empty group.
        unsafe { libc::killpg(pid as libc::pid_t, libc::SIGKILL) };
    }
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"]).output();
    }
}

/// `bytes` as a new file `name` in the worker's `scratch/screenshots/`, which the worker can
/// write and so could have swapped for a link: no link is followed, the file is new, and it is
/// removed again unless it really landed inside `scratch`. Blocks on the disk.
pub(crate) fn place_in_scratch(scratch: &Path, name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    use std::io::Write;
    let real_dir = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink());
    if !scratch.exists() {
        std::fs::create_dir_all(scratch).map_err(|e| format!("{}: {e}", scratch.display()))?;
    }
    if !real_dir(scratch) {
        return Err(format!("{} isn't a plain folder, so maiTerm won't write into it", scratch.display()));
    }
    let dir = scratch.join(SHOTS_DIR);
    if std::fs::symlink_metadata(&dir).is_err() {
        std::fs::create_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    if !real_dir(&dir) {
        return Err(format!("{} isn't a plain folder, so maiTerm won't write into it", dir.display()));
    }
    let path = dir.join(name);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NOFOLLOW);
    }
    let mut f = opts.open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(bytes).map_err(|e| format!("{}: {e}", path.display()))?;
    drop(f);
    let inside = match (std::fs::canonicalize(&path), std::fs::canonicalize(scratch)) {
        (Ok(p), Ok(s)) => p.starts_with(s),
        _ => false,
    };
    if !inside {
        let _ = std::fs::remove_file(&path);
        return Err(format!("{} moved while maiTerm wrote into it", dir.display()));
    }
    Ok(path)
}

fn dimension(args: &Value, key: &str, default: u32, max: u32) -> Result<u32, String> {
    match args.get(key) {
        None | Some(Value::Null) => Ok(default),
        Some(v) => {
            let n = v.as_u64().ok_or_else(|| format!("{key} is a whole number of pixels"))?;
            if n < SHOT_MIN as u64 || n > max as u64 {
                return Err(format!("{key} is {SHOT_MIN} to {max} pixels."));
            }
            Ok(n as u32)
        }
    }
}

// ── The coordinator's answers on serving a command nobody queued ──

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServeAnswer {
    pub v: u32,
    pub at: String,
    pub folder: String,
    /// Plain words: it covers a command that starts with them.
    pub prefix: String,
    pub approved: bool,
    pub reason: Option<String>,
}

impl ServeAnswer {
    fn words(&self) -> Vec<&str> {
        self.prefix.split_whitespace().collect()
    }

    fn covers(&self, folder: &str, argv: &[String]) -> bool {
        let w = self.words();
        self.folder == folder && !w.is_empty() && w.len() <= argv.len() && w.iter().zip(argv).all(|(a, b)| *a == b.as_str())
    }
}

pub fn read_answers(paths: &HivePaths) -> Vec<ServeAnswer> {
    let Ok(text) = std::fs::read_to_string(paths.serve_approvals()) else { return Vec::new() };
    text.lines().filter_map(|l| serde_json::from_str::<ServeAnswer>(l).ok()).filter(|a| !a.prefix.trim().is_empty()).collect()
}

pub(crate) fn append_answer(paths: &HivePaths, a: &ServeAnswer) -> Result<(), String> {
    let v = serde_json::to_value(a).map_err(|e| e.to_string())?;
    queue::append_line(&paths.serve_approvals(), &v)
}

/// The coordinator's standing answer for `argv` in `folder`: the one naming the most words, the
/// latest among equals.
pub fn answer_for<'a>(answers: &'a [ServeAnswer], folder: &str, argv: &[String]) -> Option<&'a ServeAnswer> {
    let mut best: Option<&ServeAnswer> = None;
    for a in answers.iter().filter(|a| a.covers(folder, argv)) {
        if best.is_none_or(|b| a.words().len() >= b.words().len()) {
            best = Some(a);
        }
    }
    best
}

/// A worker waiting on the coordinator's answer, in memory: told when she answers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Asked {
    pub hive_id: String,
    pub task_id: String,
    pub tab_id: String,
    pub slug: String,
    pub folder: String,
    pub argv: Vec<String>,
}

static ASKED: LazyLock<Mutex<Vec<Asked>>> = LazyLock::new(|| Mutex::new(Vec::new()));

fn asked() -> std::sync::MutexGuard<'static, Vec<Asked>> {
    ASKED.lock().unwrap_or_else(|e| e.into_inner())
}

/// Files the ask; false when this worker already waits on this very command.
pub(crate) fn file_ask(a: Asked) -> bool {
    let mut list = asked();
    if list.iter().any(|o| o.hive_id == a.hive_id && o.task_id == a.task_id && o.folder == a.folder && o.argv == a.argv) {
        return false;
    }
    list.push(a);
    true
}

/// The asks an answer for `prefix` in `folder` covers, taken.
pub(crate) fn take_asks(hive_id: &str, folder: &str, prefix: &str) -> Vec<Asked> {
    let probe = ServeAnswer { folder: folder.to_string(), prefix: prefix.to_string(), ..ServeAnswer::default() };
    let mut list = asked();
    let (out, keep): (Vec<Asked>, Vec<Asked>) = std::mem::take(&mut *list).into_iter().partition(|a| a.hive_id == hive_id && probe.covers(&a.folder, &a.argv));
    *list = keep;
    out
}

// ── What a served app's job is ──

/// What `hiveRun` `serve` does with a command.
#[derive(Debug)]
pub(crate) enum Plan {
    Run(Box<JobSpec>),
    /// Nobody queued it and the coordinator hasn't answered: ask her.
    Ask { folder: String },
    Refused(String),
}

/// A served app's job: a queued command's needs, leases, env and writes when it is one, else
/// the coordinator's standing approval (or an ask). Always the `serve` pool, a port, the longest
/// timeout, never the registries. Blocks on git.
pub(crate) fn build_serve_spec(caller: &Caller, w: &WorkerRecord, argv: &[String], folder: Option<&str>, stack: &[Service], answers: &[ServeAnswer]) -> Result<Plan, String> {
    let hive = &caller.hive;
    let f = super::run::run_folder(hive, w, folder)?;
    let runnable = super::run::runnable_commands(hive);
    let entry = super::run::match_entry(&runnable, &f.name, argv);
    if entry.is_none() {
        match answer_for(answers, &f.name, argv) {
            Some(a) if a.approved => {}
            Some(a) => {
                let why = a.reason.as_deref().map(|r| format!(": \u{201c}{}\u{201d}", one_line(r, 300))).unwrap_or_default();
                return Ok(Plan::Refused(format!(
                    "Your coordinator refused serving `{}` in {}{why}. Ask her (hiveReport a blocker or your next message) if you think it should run, or serve another command.",
                    one_line(&a.prefix, 120),
                    f.name
                )));
            }
            None => return Ok(Plan::Ask { folder: f.name.clone() }),
        }
    }
    let (commit, dirty) = super::run::worktree_state(f)?;
    let wt = f.worktree.clone().unwrap_or_default();
    let target = format!("{}{wt}", crate::stack::target::CHECKOUT);
    let mut needs = entry.map(|e| e.needs.clone()).unwrap_or_default();
    if !needs.iter().any(|n| n == queue::SERVE) {
        needs.push(queue::SERVE.to_string());
    }
    let leases = match entry {
        Some(e) => super::run::leases_for(e, stack, &target)?,
        None => Vec::new(),
    };
    let label = match entry {
        Some(e) => e.words().join(" "),
        None => one_line(&argv.join(" "), 80),
    };
    Ok(Plan::Run(Box::new(JobSpec {
        hive_id: hive.id.clone(),
        workspace_id: caller.workspace_id.clone(),
        window: caller.label.clone(),
        task_id: w.task_id.clone(),
        worker_slug: w.slug.clone(),
        folder: f.name.clone(),
        target,
        argv: argv.to_vec(),
        entry: label,
        needs,
        env: entry.map(|e| e.env.clone()).unwrap_or_default(),
        extra_write: entry.map(|e| e.extra_write.clone()).unwrap_or_default(),
        registries: false,
        leases,
        shares: runnable.resources.clone(),
        timeout_s: queue::MAX_TIMEOUT_S,
        branch: f.branch.clone(),
        commit: Some(commit),
        dirty,
        serve: true,
        ..JobSpec::default()
    })))
}

/// Whether a `hiveRun` call is serve mode's: `serve: true`, or a `stop`.
pub fn is_serve_call(args: &Value) -> bool {
    args.get("serve").and_then(Value::as_bool) == Some(true) || args.get("stop").is_some_and(|v| !v.is_null())
}

// ── The tools ──

pub fn fetch_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": FETCH,
        "description": format!(
            "Request a page of your own served app (hiveRun {{serve: true}}) from maiTerm, outside your sandbox, and read the \
             answer: status, headers and the body as text (at most {MAX_BODY_CHARS} characters; binary bodies are only \
             described). Redirects are shown, not followed. Only your own running served apps are reachable, by their job id."
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "job": { "type": "string", "description": "Your served app's job id (j-…), from hiveRun's answer" },
                "path": { "type": "string", "description": "The path after the port, starting with / (default /)" },
                "method": { "type": "string", "enum": METHODS, "description": "Default GET" },
                "body": { "type": "string", "description": "A request body (sent as JSON when it parses as JSON, else as text)" }
            },
            "required": ["job"]
        }
    })
}

pub fn screenshot_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": SCREENSHOT,
        "description": format!(
            "Take a screenshot of a page of your own served app (hiveRun {{serve: true}}) with headless Chrome, which maiTerm \
             runs outside your sandbox. Answers with the PNG's path in your scratch folder: Read it to see the page, and \
             galleryAdd it if it is worth keeping. The page reaches only your app's own port (web fonts and CDNs don't load). \
             Default size {}x{}.",
            SHOT_DEFAULT.0, SHOT_DEFAULT.1
        ),
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "job": { "type": "string", "description": "Your served app's job id (j-…), from hiveRun's answer" },
                "path": { "type": "string", "description": "The path after the port, starting with / (default /)" },
                "width": { "type": "integer", "description": format!("Window width in pixels ({SHOT_MIN}-{})", SHOT_MAX.0) },
                "height": { "type": "integer", "description": format!("Window height in pixels ({SHOT_MIN}-{})", SHOT_MAX.1) }
            },
            "required": ["job"]
        }
    })
}

pub fn approve_tool_definition(tab_id: &Value) -> Value {
    json!({
        "name": APPROVE,
        "description": "Answer a worker who asked to serve its app with a command your human didn't queue (a ⟦HIVE SERVE⟧ message). \
            Your answer stands for this Hive: approved, any worker may serve a command starting with these words in that folder \
            without asking again; refused, it is told so with your reason. Either way the job runs under the worker's sandbox and \
            reaches nothing but its own port, so approve what is a plausible way to serve the app (`npm run dev`, `node build`) \
            and refuse what isn't (a test suite, a deploy, anything that doesn't serve). Logged in the Hive's decisions.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": tab_id,
                "command": { "type": "string", "description": "The command, or the words it starts with, as the worker asked (`node build`)" },
                "folder": { "type": "string", "description": "The Hive folder (default: the primary folder)" },
                "approve": { "type": "boolean", "description": "true approves (default), false refuses" },
                "reason": { "type": "string", "description": "Why, in a sentence: told to the worker on a refusal" }
            },
            "required": ["command"]
        }
    })
}

fn served_view(job: &Job, port: Option<u16>) -> Value {
    let mut v = super::run::job_view(job);
    if let Some(p) = port {
        v["port"] = json!(p);
        v["url"] = json!(url_of(p, "/"));
    }
    v
}

/// `hiveRun` with `serve: true` (or `stop`).
pub(crate) async fn serve_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, state: &Arc<AppState>, caller: &Caller, args: &Value) -> Result<Value, String> {
    if let Some(id) = args.get("stop").filter(|v| !v.is_null()) {
        return stop_tool(caller, id.as_str().unwrap_or_default()).await;
    }
    let argv = super::run::parse_command(args.get("command").and_then(Value::as_str).unwrap_or_default())?;
    let folder = args.get("folder").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let ready = match args.get("readyTimeout") {
        None | Some(Value::Null) => READY_DEFAULT,
        Some(v) => Duration::from_secs(v.as_u64().filter(|t| *t > 0).ok_or("readyTimeout is a whole number of seconds")?.min(READY_MAX_S)),
    };
    let w = super::run::calling_worker(caller).await?;
    let mine = queue::active_serves(&w.task_id);
    if mine.len() >= MAX_SERVES {
        let list: Vec<String> = mine.iter().map(|j| format!("{} (`{}`)", j.id, one_line(&j.argv.join(" "), 60))).collect();
        return Err(format!("You already serve {MAX_SERVES} apps: {}. Stop one with hiveRun {{stop: <job>}} first.", list.join(", ")));
    }
    let stack = super::run::workspace_stack(ctx.data, caller);
    let (c, rec, words, f) = (caller.clone(), w.clone(), argv.clone(), folder.clone());
    let plan = blocking(move || {
        let answers = read_answers(&c.paths);
        build_serve_spec(&c, &rec, &words, f.as_deref(), &stack, &answers)
    })
    .await?;
    let spec = match plan {
        Plan::Run(s) => *s,
        Plan::Refused(why) => return Err(why),
        Plan::Ask { folder } => return ask_coordinator(ctx, caller, &w, &folder, &argv).await,
    };
    let job = queue::enqueue(spec)?;
    let dir = super::worker::task_dir(&caller.paths, &w.task_id)?;
    let (d, id, cmd) = (dir.clone(), job.id.clone(), format!("serve {}", job.argv.join(" ")));
    if let Err(e) = blocking(move || super::run::record_open(&d, &id, &cmd)).await {
        log::warn!("hive {}: {} not recorded as open for {}: {e}", caller.hive.id, job.id, worker_label(&w));
    }
    log::info!("hive {}: {} serves {} ({})", caller.hive.id, worker_label(&w), job.id, job.argv.join(" "));

    // Until it has started and been given its port, or ended.
    let deadline = tokio::time::Instant::now() + ready;
    let port = loop {
        tokio::time::sleep(Duration::from_millis(300)).await;
        let Some(now) = queue::job(&job.id) else { break None };
        if !now.is_active() {
            break None;
        }
        if now.state == queue::RUNNING {
            if let Some(p) = now.ports.first() {
                break Some(*p);
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let mut v = served_view(&now, None);
            v["note"] = json!(format!(
                "Queued behind the machine's other runs (it needs a free `serve` slot{}). It starts on its own: call hiveFetch with this job \
                 once it runs, or wait for its ⟦HIVE RUN⟧ if it ends.",
                if now.needs.len() > 1 { " and the queued command's resources" } else { "" }
            ));
            return Ok(v);
        }
    };
    let Some(port) = port else { return ended_answer(&dir, &job).await };
    let left = deadline.saturating_duration_since(tokio::time::Instant::now()).max(Duration::from_secs(5));
    let id = job.id.clone();
    let waited = wait_http(port, left, || match queue::job(&id) {
        Some(j) if j.state == queue::RUNNING => Ok(()),
        _ => Err("it stopped".to_string()),
    })
    .await;
    match waited {
        Ok(()) => {
            queue::touch(&job.id);
            let now = queue::job(&job.id).unwrap_or(job.clone());
            let mut v = served_view(&now, Some(port));
            v["ready"] = json!(true);
            v["note"] = json!(format!(
                "Serving on port {port}. Read a page with hiveFetch {{job: \"{0}\", path: \"/\"}}, see one with hiveScreenshot {{job: \"{0}\"}}; \
                 your own shell can't reach the port. Stop it with hiveRun {{stop: \"{0}\"}}; it also stops after {1} minutes with \
                 neither.",
                job.id,
                queue::SERVE_IDLE_MS / 60_000
            ));
            Ok(v)
        }
        Err(_) if queue::job(&job.id).is_none_or(|j| !j.is_active()) => ended_answer(&dir, &job).await,
        Err(e) => {
            let now = queue::job(&job.id).unwrap_or(job.clone());
            let mut v = served_view(&now, Some(port));
            v["ready"] = json!(false);
            let tail = super::run::screen_tail(state, now.pty_id.as_deref());
            v["note"] = json!(format!(
                "It runs, but {e}. Did it listen on PORT (or {{port}} in your command) and on 127.0.0.1? hiveFetch tries again; \
                 hiveRun {{stop: \"{}\"}} stops it.",
                job.id
            ));
            if !tail.trim().is_empty() {
                v["output"] = json!(frame_escape(tail.trim_end()));
            }
            Ok(v)
        }
    }
}

/// The served app ended before it was ready: its own result, taken so it isn't typed as well.
async fn ended_answer(dir: &Path, job: &Job) -> Result<Value, String> {
    // Its result is recorded just after it leaves the queue.
    for _ in 0..10 {
        let (d, id) = (dir.to_path_buf(), job.id.clone());
        if let Ok(mut taken) = blocking(move || super::run::take_results(&d, Some(&id))).await {
            if let Some(r) = taken.pop() {
                let mut v = super::run::job_view(job);
                v["result"] = json!(r.text);
                return Ok(v);
            }
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    let mut v = super::run::job_view(job);
    v["note"] = json!("It ended before it answered HTTP; its ⟦HIVE RUN⟧ result, with its last lines, arrives as a message.");
    Ok(v)
}

/// The coordinator is asked once per worker and command; the worker hears back by message.
async fn ask_coordinator<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, w: &WorkerRecord, folder: &str, argv: &[String]) -> Result<Value, String> {
    let cmd = argv.join(" ");
    let Some(coord) = super::tools::coordinator_tab(ctx.data, caller) else {
        return Err(format!("`{}` isn't one of your human's queued commands, and this Hive has no coordinator to approve it.", one_line(&cmd, 120)));
    };
    let fresh = file_ask(Asked {
        hive_id: caller.hive.id.clone(),
        task_id: w.task_id.clone(),
        tab_id: caller.tab_id.clone(),
        slug: w.slug.clone(),
        folder: folder.to_string(),
        argv: argv.to_vec(),
    });
    if fresh {
        let text = format!(
            "⟦HIVE SERVE · #{} {}⟧ Worker #{} \u{201c}{}\u{201d} asks to serve its app with `{}` in {folder}, which none of your human's \
             queued commands matches. It would run as a job under the worker's own sandbox, reaching nothing but the one port \
             maiTerm books for it. Approve with {APPROVE} {{command: \"{}\", folder: \"{folder}\"}} (it then stands for every worker of \
             this Hive, for a command starting with those words), or refuse with approve: false and a reason.",
            w.badge,
            one_line(&w.slug, 40),
            w.badge,
            one_line(&w.title, 80),
            one_line(&cmd, 200),
            one_line(&cmd, 200),
        );
        let sent = ctx.window.deliver(&coord, text, "Hive serve").await;
        log::info!("hive {}: {} asked the coordinator to serve `{cmd}`: {}", caller.hive.id, worker_label(w), sent.describe());
    }
    Ok(json!({
        "state": "waiting_for_coordinator",
        "command": cmd,
        "folder": folder,
        "note": if fresh {
            "None of your human's queued commands matches this, so your coordinator has been asked to approve serving it. Her answer \
             arrives as a message; once approved, call hiveRun {serve: true} with this command again. Carry on meanwhile."
        } else {
            "Your coordinator has already been asked about this command; her answer arrives as a message."
        },
    }))
}

/// `hiveRun` `stop`: one of the caller's own runs, never a land check.
async fn stop_tool(caller: &Caller, id: &str) -> Result<Value, String> {
    let id = id.trim();
    if !queue::is_job_id(id) {
        return Err("stop is a job id of yours (j- and 8 hex digits).".to_string());
    }
    let w = super::run::calling_worker(caller).await?;
    let job = queue::job(id).filter(|j| j.hive_id == caller.hive.id && j.task_id == w.task_id && j.land.is_none());
    let Some(job) = job else { return Err(format!("You have no run {id}.")) };
    if !job.is_active() {
        return Ok(json!({ "job": id, "state": job.state, "note": "It had already ended." }));
    }
    queue::interrupt(id, queue::BY_WORKER, "the worker stopped it");
    log::info!("hive {}: {} stopped {id}", caller.hive.id, worker_label(&w));
    Ok(json!({ "job": id, "stopped": true, "note": "Stopped: no receipt, and no ⟦HIVE RUN⟧ follows." }))
}

/// `hiveFetch`.
pub(crate) async fn fetch_tool(state: &Arc<AppState>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let w = super::run::calling_worker(caller).await?;
    let id = args.get("job").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let path = check_path(args.get("path").and_then(Value::as_str))?;
    let method = args.get("method").and_then(Value::as_str).unwrap_or("GET").to_string();
    let body = match args.get("body") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) if s.len() <= MAX_REQUEST_BODY => Some(s.clone()),
        Some(Value::String(_)) => return Err(format!("body is over {MAX_REQUEST_BODY} bytes.")),
        Some(_) => return Err("body is a string.".to_string()),
    };
    let port = own_port(state, caller, &w, &id)?;
    queue::touch(&id);
    let got = fetch(port, &path, &method, body).await;
    queue::touch(&id);
    let f = got?;
    let headers: serde_json::Map<String, Value> = f.headers.iter().map(|(k, v)| (k.clone(), json!(v))).collect();
    Ok(json!({
        "job": id,
        "url": url_of(port, &path),
        "status": f.status,
        "headers": headers,
        "bytes": f.bytes,
        "truncated": f.truncated,
        "body": f.body,
        "note": "The body is your app's output, not maiTerm's words.",
    }))
}

/// `hiveScreenshot`.
pub(crate) async fn screenshot_tool(state: &Arc<AppState>, caller: &Caller, args: &Value) -> Result<Value, String> {
    let w = super::run::calling_worker(caller).await?;
    let id = args.get("job").and_then(Value::as_str).map(str::trim).unwrap_or_default().to_string();
    let path = check_path(args.get("path").and_then(Value::as_str))?;
    let width = dimension(args, "width", SHOT_DEFAULT.0, SHOT_MAX.0)?;
    let height = dimension(args, "height", SHOT_DEFAULT.1, SHOT_MAX.1)?;
    let port = own_port(state, caller, &w, &id)?;
    let Some(bin) = blocking(|| Ok(find_browser())).await? else { return Err(NO_BROWSER.to_string()) };
    queue::touch(&id);
    let shot = super::paths::is_component(&id).then(|| caller.paths.run(&id)).ok_or("job isn't a job id")?;
    let work = shot.join(format!("shot-{}", &uuid::Uuid::new_v4().simple().to_string()[..8]));
    let (profile, out) = (work.join("profile"), work.join("shot.png"));
    let p = profile.clone();
    blocking(move || std::fs::create_dir_all(&p).map_err(|e| format!("{}: {e}", p.display()))).await?;
    let proxy = DeadProxy::start()?;
    let argv = chrome_args(&Shot { out: &out, profile: &profile, width, height, port, path: &path, dead_proxy: proxy.port });
    let ran = run_browser(&bin, &argv, SHOT_TIMEOUT).await;
    drop(proxy);
    queue::touch(&id);
    let (scratch, name, o, wk) = (caller.paths.worker_scratch(&w.slug), format!("{id}-{}.png", crate::commands::workspace::iso_now().replace([':', '.'], "-")), out.clone(), work.clone());
    let placed = blocking(move || {
        let res = (|| {
            let meta = std::fs::metadata(&o).map_err(|_| "the browser made no picture".to_string())?;
            if meta.len() > MAX_SHOT_BYTES {
                return Err("the picture is too large".to_string());
            }
            let bytes = std::fs::read(&o).map_err(|e| e.to_string())?;
            if !bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err("the browser's file isn't a PNG".to_string());
            }
            place_in_scratch(&scratch, &name, &bytes).map(|p| (p, bytes.len()))
        })();
        let _ = std::fs::remove_dir_all(&wk);
        res
    })
    .await;
    let (file, bytes) = match (placed, ran) {
        (Ok(p), _) => p,
        (Err(e), Err(r)) => return Err(format!("No screenshot: {r} ({e}).")),
        (Err(e), Ok(())) => return Err(format!("No screenshot: {e}.")),
    };
    log::info!("hive {}: {} screenshotted {id}{path}", caller.hive.id, worker_label(&w));
    Ok(json!({
        "job": id,
        "url": url_of(port, &path),
        "width": width,
        "height": height,
        "path": file.to_string_lossy(),
        "bytes": bytes,
        "note": "Read this path to see the page. galleryAdd {path} files it to the gallery if it's worth keeping. Only your app's own \
                 port loads in it: other hosts (web fonts, CDNs) don't.",
    }))
}

/// `hiveApproveServe`: the coordinator's answer, standing for this Hive, logged as her decision,
/// told to every worker it covers that was waiting.
pub(crate) async fn approve_tool<W: HiveWindow>(ctx: &Ctx<'_, W>, caller: &Caller, args: &Value, handle: Option<&AppHandle>) -> Result<Value, String> {
    use super::decisions::{self, DecisionLine, DecisionOption};
    let words = super::run::parse_command(args.get("command").and_then(Value::as_str).unwrap_or_default())?;
    let prefix = words.join(" ");
    let folder = match args.get("folder").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()) {
        Some(f) => f.to_string(),
        None => caller.hive.primary().map(|f| f.name.clone()).ok_or("This Hive has no primary folder: name one with folder.")?,
    };
    if !caller.hive.git_folders().any(|f| f.name == folder) {
        return Err(format!("This Hive has no repo folder \"{}\".", one_line(&folder, 60)));
    }
    let approved = args.get("approve").and_then(Value::as_bool).unwrap_or(true);
    let reason = args.get("reason").and_then(Value::as_str).map(|r| one_line(r, MAX_REASON)).filter(|r| !r.is_empty());
    if !approved && reason.is_none() {
        return Err("A refusal needs a reason: the worker is told it.".to_string());
    }
    let now = crate::commands::workspace::iso_now();
    let answer = ServeAnswer { v: 1, at: now.clone(), folder: folder.clone(), prefix: prefix.clone(), approved, reason: reason.clone() };
    let asks = take_asks(&caller.hive.id, &folder, &prefix);
    let (paths, a, hive_id) = (caller.paths.clone(), answer.clone(), caller.hive.id.clone());
    let first = asks.first().cloned();
    blocking(move || {
        append_answer(&paths, &a)?;
        let (yes, no) = ("Approve".to_string(), "Refuse".to_string());
        decisions::record(
            &paths,
            &DecisionLine {
                id: decisions::new_id(),
                event: decisions::EVENT_ANSWERED.to_string(),
                hive_id,
                category: decisions::CATEGORY_OTHER.to_string(),
                source: "serve".to_string(),
                task_id: first.as_ref().map(|f| f.task_id.clone()),
                worker: first.as_ref().map(|f| f.slug.clone()),
                question: format!("May workers serve their app with `{}` in {}?", one_line(&a.prefix, 200), a.folder),
                context: Some("It runs as a job under the worker's sandbox, reaching only the port maiTerm books for it.".to_string()),
                options: vec![DecisionOption { label: yes.clone(), recommended: false }, DecisionOption { label: no.clone(), recommended: false }],
                answer: Some(if a.approved { yes } else { no }),
                reason: a.reason.clone(),
                by: decisions::BY_COORDINATOR.to_string(),
                at: a.at.clone(),
                ..DecisionLine::default()
            },
        );
        Ok(())
    })
    .await?;
    if let Some(h) = handle {
        decisions::emit_changed(h, &caller.label, &caller.workspace_id);
    }
    log::info!("hive {}: the coordinator {} serving `{prefix}` in {folder}", caller.hive.id, if approved { "approved" } else { "refused" });
    let mut told = 0;
    for a in &asks {
        let cmd = one_line(&a.argv.join(" "), 200);
        let text = if approved {
            format!(
                "⟦HIVE RUN · serve approved⟧ Your coordinator approved serving `{cmd}` in {}: run hiveRun {{serve: true, command: \"{cmd}\"}} \
                 again to start it.",
                a.folder
            )
        } else {
            format!(
                "⟦HIVE RUN · serve refused⟧ Your coordinator refused serving `{cmd}` in {}: \u{201c}{}\u{201d}",
                a.folder,
                reason.as_deref().unwrap_or_default()
            )
        };
        let sent = ctx.window.deliver(&a.tab_id, text, "Hive serve").await;
        if matches!(sent, super::deliver::Sent::Delivered | super::deliver::Sent::Held(_)) {
            told += 1;
        } else {
            log::info!("hive {}: worker {} not told the serve answer: {}", caller.hive.id, a.slug, sent.describe());
        }
    }
    Ok(json!({ "command": prefix, "folder": folder, "approved": approved, "told": told }))
}

#[cfg(test)]
mod tests;
