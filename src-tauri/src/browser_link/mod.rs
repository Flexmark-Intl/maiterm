//! Browser link (docs/browser-link.md): an agent tab drives the ONE Chrome tab its human linked
//! to it, through the maiTerm Chrome extension.
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

#[derive(Clone, Serialize)]
pub struct Link {
    pub maiterm_tab_id: String,
    pub chrome_tab_id: i64,
    #[serde(skip)]
    conn_id: String,
    pub browser: String,
    pub title: String,
    pub url: String,
}

#[derive(Default)]
struct Registry {
    conns: HashMap<String, Conn>,
    links: Vec<Link>,
    /// Commands awaiting the extension, with the connection each went to.
    pending: HashMap<u64, (String, oneshot::Sender<Result<Value, String>>)>,
    next_id: u64,
}

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
/// with it. Called from `carry_tab_state_on_reload`. The extension is told too: it re-announces
/// its links on every reconnect, and one still naming the old id would be refused as a closed tab.
pub fn remap_tab(app: &AppHandle, from: &str, to: &str) {
    use tauri::Manager;
    let name = app.try_state::<Arc<AppState>>().and_then(|s| tab_name(&s, to));
    let mut reg = REG.lock();
    let mut moved = Vec::new();
    for l in reg.links.iter_mut().filter(|l| l.maiterm_tab_id == from) {
        l.maiterm_tab_id = to.to_string();
        moved.push(l.clone());
    }
    for l in &moved {
        if let Some(c) = reg.conns.get(&l.conn_id) {
            let frame = serde_json::json!({
                "method": "relinked",
                "params": { "chromeTabId": l.chrome_tab_id, "maitermTabId": to, "maitermTabName": name },
            });
            let _ = c.tx.send(frame.to_string());
        }
    }
    drop(reg);
    if !moved.is_empty() {
        emit_changed(app);
    }
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
    let (keep, gone): (Vec<Link>, Vec<Link>) =
        std::mem::take(&mut reg.links).into_iter().partition(|l| tab_exists(state, &l.maiterm_tab_id));
    reg.links = keep;
    for l in &gone {
        notify_unlinked(&reg, l, "The maiTerm tab was closed.");
    }
    drop(reg);
    if !gone.is_empty() {
        emit_changed(app);
    }
}

/// Make a link, replacing whatever either side was linked to (one-to-one, §1.3).
fn set_link(link: Link) {
    let mut reg = REG.lock();
    let (gone, keep): (Vec<Link>, Vec<Link>) = std::mem::take(&mut reg.links).into_iter().partition(|l| {
        l.maiterm_tab_id == link.maiterm_tab_id || (l.conn_id == link.conn_id && l.chrome_tab_id == link.chrome_tab_id)
    });
    reg.links = keep;
    for l in gone.iter().filter(|l| !(l.conn_id == link.conn_id && l.chrome_tab_id == link.chrome_tab_id)) {
        notify_unlinked(&reg, l, "Its maiTerm tab was linked to another Chrome tab.");
    }
    reg.links.push(link);
}

fn remove_link_by_chrome(conn_id: &str, chrome_tab_id: i64) -> bool {
    let mut reg = REG.lock();
    let before = reg.links.len();
    reg.links.retain(|l| !(l.conn_id == conn_id && l.chrome_tab_id == chrome_tab_id));
    before != reg.links.len()
}

/// The human unlinked a tab from maiTerm's side (Preferences).
pub fn unlink_maiterm_tab(app: &AppHandle, maiterm_tab_id: &str) -> bool {
    let mut reg = REG.lock();
    let (gone, keep): (Vec<Link>, Vec<Link>) =
        std::mem::take(&mut reg.links).into_iter().partition(|l| l.maiterm_tab_id == maiterm_tab_id);
    reg.links = keep;
    for l in &gone {
        notify_unlinked(&reg, l, "Unlinked in maiTerm.");
    }
    drop(reg);
    if !gone.is_empty() {
        emit_changed(app);
    }
    !gone.is_empty()
}

/// The link a tab's agent drives, if any.
fn link_for(maiterm_tab_id: &str) -> Option<Link> {
    REG.lock().links.iter().find(|l| l.maiterm_tab_id == maiterm_tab_id).cloned()
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
            conn_id: conn.into(),
            browser: "Chrome".into(),
            title: String::new(),
            url: String::new(),
        }
    }

    /// §1.3: linking either side again replaces the old link, never adds a second.
    #[test]
    fn links_are_one_to_one() {
        REG.lock().links.clear();
        set_link(link("tab-a", "c1", 1));
        set_link(link("tab-a", "c1", 2)); // the same agent tab, another Chrome tab
        assert_eq!(link_for("tab-a").map(|l| l.chrome_tab_id), Some(2));
        set_link(link("tab-b", "c1", 2)); // the same Chrome tab, another agent tab
        assert!(link_for("tab-a").is_none());
        assert_eq!(link_for("tab-b").map(|l| l.chrome_tab_id), Some(2));
        // Chrome tab ids are per browser: the same number in another connection is another tab.
        set_link(link("tab-c", "c2", 2));
        assert_eq!(REG.lock().links.len(), 2);
        REG.lock().links.clear();
    }
}
