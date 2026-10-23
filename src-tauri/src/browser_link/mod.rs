//! Browser link (docs/browser-link.md): an agent tab drives the Chrome tabs its human linked to
//! it, through the maiTerm Chrome extension. Each linked Chrome tab belongs to one agent; an agent
//! may hold several, which it names by handle (`b1`, `b2`…).
//!
//! The extension connects to `/browser-link` on the local MCP server, having learnt the port and
//! this launch's token over Chrome native messaging (`native_host`). Everything here is in memory:
//! Chrome tab ids die with the browser, so a link is never persisted on a `Tab` — the extension
//! re-announces its links whenever it (re)connects.
//!
//! The extension does every DevTools Protocol call itself; Rust only relays an action to the
//! linked tab and waits for the answer. Links are made only by the human's click in the
//! extension's popup — no MCP tool creates one, and every tool acts on the CALLER's link.

pub mod install;
pub mod native_host;
mod tools;
mod ws;

pub use tools::{handle_tool, is_browser_tool};
pub use ws::upgrade;

use parking_lot::Mutex;
use serde::Serialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, OnceLock};
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};

use crate::state::AppState;

/// Major version of the extension ↔ maiTerm protocol (docs/browser-link.md §3). Only an
/// incompatible change bumps it; the extension sends its own in `hello`.
pub const PROTOCOL: u64 = 1;

/// Extensions allowed to connect: the unpacked build (its id is fixed by the public `key` in
/// `chrome-extension/manifest.json`). The Chrome Web Store id joins this list once it exists.
pub const ALLOWED_EXTENSION_IDS: [&str; 1] = ["mdaccealpnngffgibmnijfgakddfgjpa"];

/// The tools a browser action may take before the agent is told it timed out. Navigation waits
/// up to 15 s for the load inside the extension, so this sits above that.
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Linked Chrome tabs one agent tab may hold. Each is a debugger attachment and a log buffer in
/// the human's browser, and a handle the agent has to keep straight.
pub const MAX_LINKS_PER_TAB: usize = 8;

/// This launch's secret, handed to the extension by the native host and checked on `hello`.
fn token() -> &'static str {
    static TOKEN: OnceLock<String> = OnceLock::new();
    TOKEN.get_or_init(|| {
        use rand::Rng;
        rand::thread_rng()
            .sample_iter(&rand::distributions::Alphanumeric)
            .take(40)
            .map(char::from)
            .collect()
    })
}

/// How this instance names itself to the extension, which may be connected to a dev and an
/// installed maiTerm at once.
pub fn instance_label() -> &'static str {
    if cfg!(debug_assertions) {
        "maiTerm (dev)"
    } else {
        "maiTerm"
    }
}

/// `browser-link.json` in the app data dir: what the native host hands the extension.
pub fn discovery_path() -> Option<PathBuf> {
    dirs::data_dir().map(|p| p.join(crate::state::persistence::app_data_slug()).join("browser-link.json"))
}

/// Write the discovery file once the MCP server has its port. Owner-only: the token in it is
/// what lets a socket drive the human's browser.
pub fn write_discovery(port: u16) {
    let Some(path) = discovery_path() else { return };
    let body = serde_json::json!({
        "port": port,
        "token": token(),
        "instance": instance_label(),
        "protocol": PROTOCOL,
        "pid": std::process::id(),
    });
    if let Err(e) = write_private(&path, body.to_string().as_bytes()) {
        log::warn!("[browser-link] could not write {}: {e}", path.display());
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
    }
    std::fs::rename(&tmp, path)
}

/// One connected extension (one browser profile).
struct Conn {
    tx: mpsc::UnboundedSender<String>,
    browser: String,
    version: String,
}

/// Does this extension version hold several tabs per agent (0.2.0 and later)? An older one is
/// still running in a Chrome that hasn't reloaded it since maiTerm updated its folder.
fn holds_several(version: &str) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let (major, minor) = (parts.next().unwrap_or(0), parts.next().unwrap_or(0));
    (major, minor) >= (0, 2)
}

#[derive(Clone, Serialize)]
pub struct Link {
    pub maiterm_tab_id: String,
    pub chrome_tab_id: i64,
    /// What the agent calls this tab: `b1`, `b2`…, never reused within its maiTerm tab, so a stale
    /// handle fails instead of reaching another page.
    pub handle: String,
    /// The handle of the linked tab that opened this one (a sign-in popup), when it joined that way.
    pub opened_by: Option<String>,
    #[serde(skip)]
    conn_id: String,
    pub browser: String,
    pub title: String,
    pub url: String,
}

impl Link {
    fn describe(&self) -> String {
        let what = if self.title.is_empty() { &self.url } else { &self.title };
        format!("{} ({what})", self.handle)
    }
}

fn handle_number(h: &str) -> Option<u32> {
    h.strip_prefix('b')?.parse().ok()
}

#[derive(Default)]
struct Registry {
    conns: HashMap<String, Conn>,
    links: Vec<Link>,
    /// Commands awaiting the extension, with the connection each went to.
    pending: HashMap<u64, (String, oneshot::Sender<Result<Value, String>>)>,
    next_id: u64,
    /// Every reload's old tab id → new one. A link can miss its remap (the extension was
    /// disconnected, or its `hello` was in flight), and both the re-announce and `prune` follow
    /// this chain rather than treating the old id as a closed tab.
    renamed: HashMap<String, String>,
    /// The last handle number given out per maiTerm tab.
    handles: HashMap<String, u32>,
    /// What changed about each agent's linked tabs that it didn't cause (one joined, closed, was
    /// unlinked), told with its next browser tool call.
    news: HashMap<String, Vec<String>>,
    /// Each agent's current tab (a handle): the one a tool without `tab` acts on. Only the agent
    /// moves it, by naming a tab, or its first link sets it: a tab that joins, or that the human
    /// links beside it, must not silently become where an untargeted click lands. Kept across a
    /// reconnect (the links are re-announced under the same handles).
    current: HashMap<String, String>,
}

impl Registry {
    fn tell(&mut self, maiterm_tab_id: &str, msg: String) {
        let v = self.news.entry(maiterm_tab_id.to_string()).or_default();
        if v.len() < 20 {
            v.push(msg);
        }
    }

    /// A link went: if it was its agent's current tab, the agent has none until it names one.
    fn forget_current(&mut self, gone: &Link) {
        if self.current.get(&gone.maiterm_tab_id) == Some(&gone.handle) {
            self.current.remove(&gone.maiterm_tab_id);
        }
    }

    fn holds(&self, maiterm_tab_id: &str, handle: &str) -> bool {
        self.links.iter().any(|l| l.maiterm_tab_id == maiterm_tab_id && l.handle == handle)
    }

    /// Raise a tab's handle counter, so no handle up to `n` is given out again.
    fn reserve(&mut self, maiterm_tab_id: &str, n: u32) {
        let c = self.handles.entry(maiterm_tab_id.to_string()).or_insert(0);
        *c = (*c).max(n);
    }
}

/// Kept small: an entry only matters until the extension next reconnects.
const RENAMED_KEEP: usize = 500;

static REG: LazyLock<Mutex<Registry>> = LazyLock::new(|| Mutex::new(Registry::default()));

/// Tell the extension a link is gone (its badge, and the debugger bar, follow).
fn notify_unlinked(reg: &Registry, link: &Link, reason: &str) {
    if let Some(c) = reg.conns.get(&link.conn_id) {
        let frame = serde_json::json!({
            "method": "unlinked",
            "params": { "chromeTabId": link.chrome_tab_id, "reason": reason },
        });
        let _ = c.tx.send(frame.to_string());
    }
}

fn emit_changed(app: &AppHandle) {
    let _ = app.emit("browser-link-changed", ());
}

/// A reload replaces the tab under a new id (CLAUDE.md, "Reload replaces a tab"); its link goes
/// with it. Called from `carry_tab_state_on_reload`. The extension is told too (`relinked`): it
/// re-announces its links on every reconnect, and the old id would read as a closed tab. The
/// rename is remembered even when no link holds it now, for a link that is re-announced later.
pub fn remap_tab(app: &AppHandle, from: &str, to: &str) {
    use tauri::Manager;
    let name = app.try_state::<Arc<AppState>>().and_then(|s| tab_name(&s, to));
    let mut reg = REG.lock();
    if reg.renamed.len() >= RENAMED_KEEP {
        reg.renamed.clear();
    }
    reg.renamed.insert(from.to_string(), to.to_string());
    if let Some(n) = reg.handles.remove(from) {
        reg.handles.insert(to.to_string(), n);
    }
    if let Some(n) = reg.news.remove(from) {
        reg.news.insert(to.to_string(), n);
    }
    if let Some(h) = reg.current.remove(from) {
        reg.current.insert(to.to_string(), h);
    }
    let mut moved = false;
    for i in 0..reg.links.len() {
        if reg.links[i].maiterm_tab_id == from {
            reg.links[i].maiterm_tab_id = to.to_string();
            notify_relinked(&reg, &reg.links[i], name.as_deref());
            moved = true;
        }
    }
    drop(reg);
    if moved {
        emit_changed(app);
    }
}

/// Tell the extension a link now belongs to its maiTerm tab's new id.
fn notify_relinked(reg: &Registry, link: &Link, name: Option<&str>) {
    if let Some(c) = reg.conns.get(&link.conn_id) {
        let frame = serde_json::json!({
            "method": "relinked",
            "params": { "chromeTabId": link.chrome_tab_id, "maitermTabId": link.maiterm_tab_id, "maitermTabName": name },
        });
        let _ = c.tx.send(frame.to_string());
    }
}

/// The live tab an id now names: itself, or where its reloads led. None for a closed tab.
fn current_id(state: &Arc<AppState>, reg: &Registry, id: &str) -> Option<String> {
    let mut id = id.to_string();
    // Bounded: a chain is one hop per reload, and a cycle must not hang the lock.
    for _ in 0..16 {
        if tab_exists(state, &id) {
            return Some(id);
        }
        id = reg.renamed.get(&id)?.clone();
    }
    None
}

/// The human turned the feature off: every link ends now (the extension detaches and Chrome's
/// debugging bar goes), and every connection is closed. Agents keep a cached tool list, so the
/// tools also refuse while it is off (`tools::handle_tool`), and new connections are refused.
pub fn shut_down(app: &AppHandle) {
    let mut reg = REG.lock();
    let links = std::mem::take(&mut reg.links);
    for l in &links {
        notify_unlinked(&reg, l, "The browser link was turned off in maiTerm.");
    }
    // Dropping a connection's sender ends its socket loop, after the frames already queued.
    reg.conns.clear();
    reg.pending.clear();
    reg.current.clear();
    drop(reg);
    emit_changed(app);
}

pub fn enabled(state: &Arc<AppState>) -> bool {
    state.app_data.read().preferences.browser_link_enabled
}

/// Does this maiTerm tab exist in any window's pane tree?
fn tab_exists(state: &Arc<AppState>, tab_id: &str) -> bool {
    let data = state.app_data.read();
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .flat_map(|ws| ws.panes.iter())
        .any(|p| p.tabs.iter().any(|t| t.id == tab_id))
}

/// Drop links whose maiTerm tab is gone (closed, archived), telling the extension. Run on every
/// read and every few seconds while an extension is connected (`ws::run`), so a closed tab's
/// Chrome tab stops being debugged without waiting for someone to look.
fn prune(state: &Arc<AppState>, app: &AppHandle) {
    let mut reg = REG.lock();
    let mut changed = false;
    let mut kept = Vec::new();
    for mut l in std::mem::take(&mut reg.links) {
        match current_id(state, &reg, &l.maiterm_tab_id) {
            Some(id) if id == l.maiterm_tab_id => kept.push(l),
            // Reloaded while this link missed the remap: follow it.
            Some(id) => {
                l.maiterm_tab_id = id;
                notify_relinked(&reg, &l, tab_name(state, &l.maiterm_tab_id).as_deref());
                kept.push(l);
                changed = true;
            }
            None => {
                notify_unlinked(&reg, &l, "The maiTerm tab was closed.");
                changed = true;
            }
        }
    }
    reg.links = kept;
    // A gone tab's notices and current tab go with it. Its handle counter stays (a few bytes): an
    // ARCHIVED tab leaves the tree and comes back under the same id, and must not reuse a handle.
    let gone: Vec<String> = reg.news.keys().chain(reg.current.keys()).filter(|id| !tab_exists(state, id)).cloned().collect();
    for id in gone {
        if current_id(state, &reg, &id).is_none() {
            reg.news.remove(&id);
            reg.current.remove(&id);
        }
    }
    drop(reg);
    if changed {
        emit_changed(app);
    }
}

/// A link the extension re-announces on `hello`, under the id its maiTerm tab has now; None when
/// that tab is closed. A changed id is sent back as `relinked` once the welcome is out.
fn resolve_announced(state: &Arc<AppState>, link: &mut Link) -> Option<bool> {
    let reg = REG.lock();
    let id = current_id(state, &reg, &link.maiterm_tab_id)?;
    let changed = id != link.maiterm_tab_id;
    link.maiterm_tab_id = id;
    Some(changed)
}

/// Make a link (§1.3): a Chrome tab belongs to one agent, so linking it again moves it, and keeps
/// its handle when it stays with the same agent. A link that carries a handle (re-announced on
/// `hello`) keeps it unless that agent already uses it. With `fresh` (the human linked it now,
/// not a re-announce) an agent's first link becomes its current tab; nothing else does
/// (`Registry.current`): a reconnect must not pick a current tab the agent never named. Returns
/// the link as stored.
fn set_link(mut link: Link, fresh: bool) -> Result<Link, String> {
    let mut reg = REG.lock();
    let same = |l: &Link| l.conn_id == link.conn_id && l.chrome_tab_id == link.chrome_tab_id;
    if let Some(i) = reg.links.iter().position(|l| same(l) && l.maiterm_tab_id == link.maiterm_tab_id) {
        let l = &mut reg.links[i];
        l.title = link.title;
        l.url = link.url;
        return Ok(l.clone());
    }
    let held = reg.links.iter().filter(|l| l.maiterm_tab_id == link.maiterm_tab_id).count();
    if held >= MAX_LINKS_PER_TAB {
        return Err(format!(
            "That agent tab already has {MAX_LINKS_PER_TAB} linked Chrome tabs. Unlink one of them first."
        ));
    }
    if let Some(i) = reg.links.iter().position(|l| same(l)) {
        let old = reg.links.remove(i);
        reg.tell(&old.maiterm_tab_id, format!("{} was linked to another agent and is no longer yours.", old.describe()));
        reg.forget_current(&old);
    }
    let tab = link.maiterm_tab_id.clone();
    match handle_number(&link.handle) {
        Some(n) if !reg.holds(&tab, &link.handle) => reg.reserve(&tab, n),
        _ => {
            let n = reg.handles.get(&tab).copied().unwrap_or(0) + 1;
            reg.reserve(&tab, n);
            link.handle = format!("b{n}");
        }
    }
    if fresh && held == 0 && !reg.current.contains_key(&tab) {
        reg.current.insert(tab, link.handle.clone());
    }
    reg.links.push(link.clone());
    Ok(link)
}

/// The extension's record of the highest handle each agent tab has had (`hello`), so a maiTerm
/// restart, which empties `handles`, doesn't hand out a closed tab's handle again.
fn reserve_handles(maiterm_tab_id: &str, n: u32) {
    REG.lock().reserve(maiterm_tab_id, n);
}

/// An extension from before several tabs per agent (0.1.x) keeps one Chrome tab per agent tab:
/// its `link` drops the agent's other links on its own side without saying so, so maiTerm drops
/// them too or it would hold links the extension has forgotten.
fn drop_others_for_one_tab_extension(link: &Link) {
    let mut reg = REG.lock();
    let (gone, keep): (Vec<Link>, Vec<Link>) = std::mem::take(&mut reg.links).into_iter().partition(|l| {
        l.maiterm_tab_id == link.maiterm_tab_id && l.conn_id == link.conn_id && l.chrome_tab_id != link.chrome_tab_id
    });
    reg.links = keep;
    for l in gone {
        reg.current.remove(&l.maiterm_tab_id);
        reg.tell(&l.maiterm_tab_id, format!("{} was replaced by the tab your human linked next.", l.describe()));
    }
}

/// Tell an agent about a change to its linked tabs it didn't make.
fn tell(maiterm_tab_id: &str, msg: String) {
    REG.lock().tell(maiterm_tab_id, msg);
}

/// What changed since the agent's last browser tool call; reading it clears it.
fn take_news(maiterm_tab_id: &str) -> Vec<String> {
    REG.lock().news.remove(maiterm_tab_id).unwrap_or_default()
}

/// The extension dropped a link (the human unlinked it, closed the tab, cancelled the debugging
/// bar). `why` finishes "b2 (title) …" for the agent.
fn remove_link_by_chrome(conn_id: &str, chrome_tab_id: i64, why: &str) -> bool {
    let mut reg = REG.lock();
    let Some(i) = reg.links.iter().position(|l| l.conn_id == conn_id && l.chrome_tab_id == chrome_tab_id) else {
        return false;
    };
    let gone = reg.links.remove(i);
    reg.tell(&gone.maiterm_tab_id, format!("{} {why}.", gone.describe()));
    reg.forget_current(&gone);
    true
}

/// The human unlinked from maiTerm's side (Preferences): one of a tab's links, or all of them.
pub fn unlink_maiterm_tab(app: &AppHandle, maiterm_tab_id: &str, handle: Option<&str>) -> bool {
    let mut reg = REG.lock();
    let (gone, keep): (Vec<Link>, Vec<Link>) = std::mem::take(&mut reg.links)
        .into_iter()
        .partition(|l| l.maiterm_tab_id == maiterm_tab_id && handle.is_none_or(|h| l.handle == h));
    reg.links = keep;
    for l in &gone {
        notify_unlinked(&reg, l, "Unlinked in maiTerm.");
        let msg = format!("{} was unlinked by your human.", l.describe());
        reg.tell(maiterm_tab_id, msg);
        reg.forget_current(l);
    }
    drop(reg);
    if !gone.is_empty() {
        emit_changed(app);
    }
    !gone.is_empty()
}

/// A tab's linked Chrome tabs, b1 first.
fn links_of(maiterm_tab_id: &str) -> Vec<Link> {
    let mut v: Vec<Link> = REG.lock().links.iter().filter(|l| l.maiterm_tab_id == maiterm_tab_id).cloned().collect();
    v.sort_by_key(|l| handle_number(&l.handle).unwrap_or(u32::MAX));
    v
}

/// The linked tab a tool acts on: the one named (which becomes current), else the current one,
/// else the only one. None when the agent has no linked tab; Err for a handle it doesn't hold, or
/// several tabs and no current one (after a restart, or the current one closed): the agent must
/// say which rather than land on a page it didn't mean.
fn pick(maiterm_tab_id: &str, handle: Option<&str>) -> Option<Result<Link, String>> {
    let mut reg = REG.lock();
    let mine: Vec<Link> = reg.links.iter().filter(|l| l.maiterm_tab_id == maiterm_tab_id).cloned().collect();
    if mine.is_empty() {
        return None;
    }
    let list = || {
        let mut v = mine.clone();
        v.sort_by_key(|l| handle_number(&l.handle).unwrap_or(u32::MAX));
        v.iter().map(Link::describe).collect::<Vec<_>>().join(", ")
    };
    let chosen = match handle {
        Some(h) => match mine.iter().find(|l| l.handle == h) {
            Some(l) => l.clone(),
            None => return Some(Err(format!("You have no linked tab {h}. Yours: {}.", list()))),
        },
        None => {
            let cur = reg.current.get(maiterm_tab_id);
            match mine.iter().find(|l| Some(&l.handle) == cur) {
                Some(l) => l.clone(),
                None if mine.len() == 1 => mine[0].clone(),
                None => {
                    return Some(Err(format!(
                        "You have several linked tabs and none is current. Say which with tab: {}.",
                        list()
                    )))
                }
            }
        }
    };
    reg.current.insert(maiterm_tab_id.to_string(), chosen.handle.clone());
    Some(Ok(chosen))
}

/// The handle a tool without `tab` would use now, if any (browserStatus).
fn current_handle(maiterm_tab_id: &str) -> Option<String> {
    let reg = REG.lock();
    let mine: Vec<&Link> = reg.links.iter().filter(|l| l.maiterm_tab_id == maiterm_tab_id).collect();
    match reg.current.get(maiterm_tab_id) {
        Some(h) if mine.iter().any(|l| &l.handle == h) => Some(h.clone()),
        _ if mine.len() == 1 => Some(mine[0].handle.clone()),
        _ => None,
    }
}

fn connected() -> bool {
    !REG.lock().conns.is_empty()
}

/// Send an action to the extension holding this link and wait for its answer.
async fn command(link: &Link, action: &str, args: Value) -> Result<Value, String> {
    let (id, orx) = {
        let mut reg = REG.lock();
        let Some(conn) = reg.conns.get(&link.conn_id) else {
            return Err("The browser holding your linked tab disconnected from maiTerm.".into());
        };
        let tx = conn.tx.clone();
        reg.next_id += 1;
        let id = reg.next_id;
        let (otx, orx) = oneshot::channel();
        reg.pending.insert(id, (link.conn_id.clone(), otx));
        let frame = serde_json::json!({
            "id": id,
            "method": "command",
            "params": { "chromeTabId": link.chrome_tab_id, "action": action, "args": args },
        });
        if tx.send(frame.to_string()).is_err() {
            reg.pending.remove(&id);
            return Err("The browser holding your linked tab disconnected from maiTerm.".into());
        }
        (id, orx)
    };
    match tokio::time::timeout(COMMAND_TIMEOUT, orx).await {
        Ok(Ok(r)) => r,
        Ok(Err(_)) => Err("The browser disconnected before answering.".into()),
        Err(_) => {
            REG.lock().pending.remove(&id);
            Err(format!("The browser did not answer within {} s.", COMMAND_TIMEOUT.as_secs()))
        }
    }
}

#[derive(Serialize)]
pub struct ConnView {
    pub browser: String,
    pub version: String,
}

#[derive(Serialize)]
pub struct LinkView {
    #[serde(flatten)]
    pub link: Link,
    pub tab_name: Option<String>,
}

#[derive(Serialize)]
pub struct StatusView {
    pub enabled: bool,
    pub connections: Vec<ConnView>,
    pub links: Vec<LinkView>,
    /// Browsers whose native-messaging folder holds our host manifest.
    pub installed_in: Vec<String>,
    /// The folder "Load unpacked" points Chrome at.
    pub extension_dir: Option<String>,
    pub extension_id: &'static str,
}

/// When each maiTerm tab was last brought to the front (activated, or its window focused): the
/// popup lists agent tabs most recently used first. In memory; after a restart the tree order
/// stands in until tabs are used again.
static TAB_USED: LazyLock<Mutex<HashMap<String, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static CLOCK: AtomicU64 = AtomicU64::new(0);

pub fn touch_tab(tab_id: &str) {
    let t = CLOCK.fetch_add(1, Ordering::Relaxed) + 1;
    let mut used = TAB_USED.lock();
    if used.len() > 2000 {
        used.clear();
    }
    used.insert(tab_id.to_string(), t);
}

fn tab_used(tab_id: &str) -> u64 {
    TAB_USED.lock().get(tab_id).copied().unwrap_or(0)
}

/// The tab a window shows in front: its active workspace's active pane's active tab.
fn front_tab(w: &crate::state::WindowData) -> Option<&str> {
    let ws = w.workspaces.iter().find(|ws| Some(&ws.id) == w.active_workspace_id.as_ref())?;
    let pane = ws.panes.iter().find(|p| Some(&p.id) == ws.active_pane_id.as_ref())?;
    pane.active_tab_id.as_deref()
}

/// A window came to the front: so did its front tab.
pub fn touch_window(state: &Arc<AppState>, label: &str) {
    let data = state.app_data.read();
    if let Some(id) = data.window(label).and_then(front_tab) {
        touch_tab(id);
    }
}

fn tab_name(state: &Arc<AppState>, tab_id: &str) -> Option<String> {
    let data = state.app_data.read();
    data.windows
        .iter()
        .flat_map(|w| w.workspaces.iter())
        .flat_map(|ws| ws.panes.iter())
        .flat_map(|p| p.tabs.iter())
        .find(|t| t.id == tab_id)
        .map(|t| t.name.clone())
}

/// The human, from maiTerm's tab strip: bring that linked Chrome tab to the front. Not an agent
/// tool (`tools::action_for` has no `reveal`), and the extension does it without the debugger.
pub async fn reveal(maiterm_tab_id: &str, handle: &str) -> Result<(), String> {
    let link = REG
        .lock()
        .links
        .iter()
        .find(|l| l.maiterm_tab_id == maiterm_tab_id && l.handle == handle)
        .cloned()
        .ok_or_else(|| "That Chrome tab is no longer linked.".to_string())?;
    command(&link, "reveal", Value::Null).await.map(|_| ())
}

/// Every live link with its maiTerm tab's name: what the tab strip's browser indicator draws. The
/// cheap half of `status` (no manifest scan), read on every `browser-link-changed`.
pub fn links(state: &Arc<AppState>, app: &AppHandle) -> Vec<LinkView> {
    prune(state, app);
    let links = REG.lock().links.clone();
    links.into_iter().map(|l| LinkView { tab_name: tab_name(state, &l.maiterm_tab_id), link: l }).collect()
}

pub fn status(state: &Arc<AppState>, app: &AppHandle) -> StatusView {
    prune(state, app);
    let enabled = state.app_data.read().preferences.browser_link_enabled;
    let (connections, links) = {
        let reg = REG.lock();
        let c = reg
            .conns
            .values()
            .map(|c| ConnView { browser: c.browser.clone(), version: c.version.clone() })
            .collect();
        (c, reg.links.clone())
    };
    let links = links
        .into_iter()
        .map(|l| LinkView { tab_name: tab_name(state, &l.maiterm_tab_id), link: l })
        .collect();
    StatusView {
        enabled,
        connections,
        links,
        installed_in: install::installed_in(),
        extension_dir: install::extension_dir(app).map(|p| p.to_string_lossy().into_owned()),
        extension_id: ALLOWED_EXTENSION_IDS[0],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(tab: &str, conn: &str, chrome: i64) -> Link {
        Link {
            maiterm_tab_id: tab.into(),
            chrome_tab_id: chrome,
            handle: String::new(),
            opened_by: None,
            conn_id: conn.into(),
            browser: "Chrome".into(),
            title: String::new(),
            url: String::new(),
        }
    }

    /// The registry is one global and tests run in parallel: each test holds this while it uses it.
    static SERIAL: Mutex<()> = Mutex::new(());

    fn reset() {
        let mut reg = REG.lock();
        reg.links.clear();
        reg.handles.clear();
        reg.news.clear();
        reg.current.clear();
    }

    /// §1.3: an agent tab holds several Chrome tabs; a Chrome tab belongs to one agent, and
    /// linking it again moves it (telling the agent that lost it). Handles are never reused.
    #[test]
    fn links_share_an_agent_not_a_chrome_tab() {
        let _serial = SERIAL.lock();
        reset();
        assert_eq!(set_link(link("tab-a", "c1", 1), true).unwrap().handle, "b1");
        assert_eq!(set_link(link("tab-a", "c1", 2), true).unwrap().handle, "b2");
        // The same Chrome tab again, same agent: the same handle, no new link.
        assert_eq!(set_link(link("tab-a", "c1", 1), true).unwrap().handle, "b1");
        assert_eq!(links_of("tab-a").len(), 2);
        pick("tab-a", Some("b2")).unwrap().unwrap(); // b2 is now tab-a's current tab
        // Moved to another agent: gone from tab-a, which hears about it.
        assert_eq!(set_link(link("tab-b", "c1", 2), true).unwrap().handle, "b1");
        assert_eq!(links_of("tab-a").iter().map(|l| l.handle.as_str()).collect::<Vec<_>>(), ["b1"]);
        assert_eq!(take_news("tab-a").len(), 1);
        // tab-a's current tab moved away: no stale current is left, and its one remaining tab serves.
        assert!(REG.lock().current.get("tab-a").is_none());
        assert_eq!(current_handle("tab-a").as_deref(), Some("b1"));
        assert!(take_news("tab-a").is_empty());
        // tab-a's next link is b3, not a reused b2.
        assert_eq!(set_link(link("tab-a", "c1", 3), true).unwrap().handle, "b3");
        // Chrome tab ids are per browser: the same number in another connection is another tab.
        set_link(link("tab-c", "c2", 2), true).unwrap();
        assert_eq!(links_of("tab-b").len(), 1);
        reset();
    }

    /// A re-announced link keeps its handle; a clash is renumbered. The cap holds.
    #[test]
    fn handles_survive_reannounce_and_the_cap_holds() {
        let _serial = SERIAL.lock();
        reset();
        let mut l = link("tab-r", "c9", 5);
        l.handle = "b4".into();
        assert_eq!(set_link(l, true).unwrap().handle, "b4");
        let mut clash = link("tab-r", "c9", 6);
        clash.handle = "b4".into();
        assert_eq!(set_link(clash, true).unwrap().handle, "b5");
        for chrome in 7..13 {
            set_link(link("tab-r", "c9", chrome), true).unwrap();
        }
        assert_eq!(links_of("tab-r").len(), MAX_LINKS_PER_TAB);
        assert!(set_link(link("tab-r", "c9", 99), true).is_err());
        reset();
    }

    /// No handle: the current tab, which only the agent's first link or naming a tab sets. A tab
    /// linked or joined beside it doesn't take over; with several and none current, it must name one.
    #[test]
    fn untargeted_calls_go_to_the_current_tab_only() {
        let _serial = SERIAL.lock();
        reset();
        set_link(link("tab-p", "c3", 1), true).unwrap();
        set_link(link("tab-p", "c3", 2), true).unwrap();
        assert_eq!(pick("tab-p", None).unwrap().unwrap().handle, "b1"); // the first link, not the newest
        assert_eq!(pick("tab-p", Some("b2")).unwrap().unwrap().handle, "b2");
        assert_eq!(pick("tab-p", None).unwrap().unwrap().handle, "b2");
        assert!(pick("tab-p", Some("b9")).unwrap().is_err());
        assert!(pick("tab-none", None).is_none());
        // The current tab closes: with one left that one; with several, the agent must say.
        set_link(link("tab-p", "c3", 3), true).unwrap();
        remove_link_by_chrome("c3", 2, "was closed");
        assert!(pick("tab-p", None).unwrap().is_err());
        assert_eq!(current_handle("tab-p"), None);
        remove_link_by_chrome("c3", 3, "was closed");
        assert_eq!(pick("tab-p", None).unwrap().unwrap().handle, "b1");
        reset();
    }

    /// An extension still running 0.1.x code keeps one tab per agent on its side, so maiTerm drops
    /// the agent's other links when it links one, rather than holding links the extension forgot.
    #[test]
    fn a_one_tab_extension_keeps_one_tab_per_agent() {
        let _serial = SERIAL.lock();
        reset();
        assert!(!holds_several("0.1.0") && holds_several("0.2.0") && holds_several("1.0"));
        set_link(link("tab-o", "c5", 1), true).unwrap();
        let second = set_link(link("tab-o", "c5", 2), true).unwrap();
        drop_others_for_one_tab_extension(&second);
        assert_eq!(links_of("tab-o").iter().map(|l| l.chrome_tab_id).collect::<Vec<_>>(), [2]);
        assert_eq!(pick("tab-o", None).unwrap().unwrap().chrome_tab_id, 2);
        reset();
    }

    /// A re-announced link (a reconnect, a maiTerm restart) never becomes current on its own: an
    /// agent with several tabs must name one rather than land on whichever was announced first.
    #[test]
    fn a_reconnect_picks_no_current_tab() {
        let _serial = SERIAL.lock();
        reset();
        let mut a = link("tab-s", "c6", 1);
        a.handle = "b1".into();
        let mut b = link("tab-s", "c6", 2);
        b.handle = "b2".into();
        set_link(a, false).unwrap();
        set_link(b, false).unwrap();
        assert!(pick("tab-s", None).unwrap().is_err());
        reset();
    }

    /// A maiTerm restart forgets the counter; the extension's record keeps closed handles retired.
    #[test]
    fn reserved_handles_are_not_given_out_again() {
        let _serial = SERIAL.lock();
        reset();
        reserve_handles("tab-q", 3);
        let mut l = link("tab-q", "c4", 1);
        l.handle = "b1".into();
        set_link(l, true).unwrap();
        assert_eq!(set_link(link("tab-q", "c4", 2), true).unwrap().handle, "b4");
        reset();
    }

    /// A link that missed a reload's remap (the extension was away) follows the rename chain to
    /// the live tab; one whose chain ends at a closed tab resolves to nothing.
    #[test]
    fn announced_links_follow_reloads() {
        use crate::state::{WindowData, Workspace};
        let state = Arc::new(AppState::new());
        let live = {
            let mut data = state.app_data.write();
            let mut win = WindowData::new("main".into());
            let ws = Workspace::new("Proj".into());
            let id = ws.panes[0].tabs[0].id.clone();
            win.workspaces.push(ws);
            data.windows.push(win);
            id
        };
        {
            let mut reg = REG.lock();
            reg.renamed.insert("orig".into(), "mid".into());
            reg.renamed.insert("mid".into(), live.clone());
            reg.renamed.insert("loop-a".into(), "loop-b".into());
            reg.renamed.insert("loop-b".into(), "loop-a".into());
        }
        let mut l = link("orig", "c1", 7);
        assert_eq!(resolve_announced(&state, &mut l), Some(true));
        assert_eq!(l.maiterm_tab_id, live);
        let mut same = link(&live, "c1", 8);
        assert_eq!(resolve_announced(&state, &mut same), Some(false));
        assert_eq!(resolve_announced(&state, &mut link("closed", "c1", 9)), None);
        assert_eq!(resolve_announced(&state, &mut link("loop-a", "c1", 10)), None);
        REG.lock().renamed.clear();
    }
}
