//! Which folder a tab's tree shows (docs/file-tree.md §4.1).
//!
//! One resolver, here, for every caller: the precedence is Move project's proven chain, and a
//! candidate counts only if it exists. Two of the signals (OSC 7, the prompt's folder) live only
//! in the frontend's terminalsStore and come in as hints.
//!
//! Local tab: live shell cwd → OSC 7 → the agent's cwd → `last_cwd` → `auto_resume_cwd` →
//! `restore_cwd`. ssh tab: the agent's cwd → OSC 7 (unless it is just the local shell's
//! folder) → the prompt's folder (absolute or `~/` only) → `auto_resume_remote_cwd` →
//! `restore_remote_cwd`; `last_cwd` is never used there (it can be either side). An ssh tab's
//! candidates are checked on the host by its Files helper (§15): the first that is a folder
//! there wins, and its root is the repository top level the helper finds. When the host can't
//! be asked in time, the first candidate other than the prompt's wins and is its own root.
//!
//! A saved folder that no longer exists comes back as `missing`, never as home: a tree that
//! quietly showed `$HOME` would invite the human to work in the wrong place (docs/relocate.md
//! §4). The root is the git top level of the folder, else the folder itself.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::Mutex;

use super::types::{PlaceHints, TreePlace};
use crate::state::AppState;

/// Everything the resolver reads about one tab, gathered first so the decision is a pure
/// function (and testable without a PTY).
#[derive(Debug, Default, Clone)]
pub struct PlaceInputs {
    /// The tab runs a stack service: never the tree's source (docs/stack.md §7).
    pub is_service: bool,
    /// The live shell's cwd, when the tab has a live PTY.
    pub live_cwd: Option<String>,
    /// What holds the terminal when it is ssh (the command line); None at a local prompt.
    pub ssh_foreground: Option<String>,
    /// Saved ssh command: a suspended or unloaded ssh tab is still an ssh tab.
    pub saved_ssh: Option<String>,
    /// The cwd every agent session registered for the tab reported, deduplicated.
    pub agent_cwds: Vec<String>,
    pub last_cwd: Option<String>,
    pub auto_resume_cwd: Option<String>,
    pub restore_cwd: Option<String>,
    pub auto_resume_remote_cwd: Option<String>,
    pub restore_remote_cwd: Option<String>,
    /// The bridge tunnel's destination, when the tab rides one.
    pub tunnel_host: Option<String>,
    /// The workspace's Files pin.
    pub pin: Option<String>,
}

/// The folder the chain landed on.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    pub cwd: String,
    pub source: &'static str,
    pub remote_host: Option<String>,
    /// The chain found only folders that don't exist; `cwd` is the first of them.
    pub missing: bool,
}

fn non_empty(v: &Option<String>) -> Option<&str> {
    v.as_deref().map(str::trim).filter(|s| !s.is_empty())
}

fn is_remote_abs(p: &str) -> bool {
    p.starts_with('/') || p == "~" || p.starts_with("~/")
}

/// The one agent folder, when the tab's sessions agree on one.
fn single_agent_cwd(i: &PlaceInputs) -> Option<&str> {
    let mut it = i.agent_cwds.iter().map(|s| s.trim()).filter(|s| !s.is_empty());
    let first = it.next()?;
    it.all(|c| c == first).then_some(first)
}

/// Does OSC 7 name this machine? An absent host is a local shell that didn't say.
fn osc_is_local(host: Option<&str>, local_host: &str) -> bool {
    match host.map(str::trim).filter(|h| !h.is_empty()) {
        None => true,
        Some(h) => {
            let short = |s: &str| s.split('.').next().unwrap_or(s).to_ascii_lowercase();
            h.eq_ignore_ascii_case("localhost") || short(h) == short(local_host)
        }
    }
}

/// The ssh destination of a foreground command line (`ssh -p 2222 ews@nova` → `-p 2222 ews@nova`).
///
/// The process table shows the raw argv, so an ssh maiTerm started itself (a clone, a split, a
/// restore, Open Terminal Here) still carries the `-o ControlMaster=no` `buildSshCommand`
/// injects. That says how the session was started, not where it goes: kept, it made the
/// place's host a spelling no stored command has (the frontend compares the cleaned one), and
/// a second helper for the same host as a hand-typed `ssh ews@nova` tab.
fn host_of_command(cmd: &str) -> Option<String> {
    let rest = cmd.trim().split_once(char::is_whitespace).map(|(_, r)| r).unwrap_or("");
    let h = crate::watch::connect_args(&without_control_master(rest));
    (!h.is_empty()).then_some(h)
}

/// ssh args without any `ControlMaster` option (`-o ControlMaster=x` or `-oControlMaster=x`,
/// any case), as the frontend's `cleanSshCommand` stores them.
fn without_control_master(args: &str) -> String {
    let is_cm = |o: &str| o.to_ascii_lowercase().starts_with("controlmaster=");
    let words: Vec<&str> = args.split_whitespace().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        if w == "-o" && words.get(i + 1).is_some_and(|o| is_cm(o)) {
            i += 2;
            continue;
        }
        if w.strip_prefix("-o").is_some_and(is_cm) {
            i += 1;
            continue;
        }
        out.push(w);
        i += 1;
    }
    out.join(" ")
}

/// An ssh tab's candidate folders in order, deduplicated: the agent's, OSC 7 (unless it is
/// the local shell's), the prompt's (absolute or `~/` only), then the saved ones.
pub fn ssh_chain(i: &PlaceInputs, hints: &PlaceHints) -> Vec<(String, &'static str)> {
    let osc = non_empty(&hints.osc_cwd).filter(|o| Some(*o) != i.live_cwd.as_deref() && is_remote_abs(o));
    let prompt = non_empty(&hints.prompt_cwd).filter(|p| p.starts_with('/') || *p == "~" || p.starts_with("~/"));
    let chain: [(Option<&str>, &'static str); 5] = [
        (single_agent_cwd(i).filter(|c| is_remote_abs(c)), "agent"),
        (osc, "osc7"),
        (prompt, "prompt"),
        (non_empty(&i.auto_resume_remote_cwd), "auto_resume"),
        (non_empty(&i.restore_remote_cwd), "restore"),
    ];
    let mut out: Vec<(String, &'static str)> = Vec::new();
    for (c, src) in chain {
        if let Some(c) = c {
            if !out.iter().any(|(o, _)| o == c) {
                out.push((c.to_string(), src));
            }
        }
    }
    out
}

/// Decide. `exists` answers "is this a folder on this computer" (injected for tests).
pub fn pick(i: &PlaceInputs, hints: &PlaceHints, local_host: &str, exists: &dyn Fn(&Path) -> bool) -> Result<Pick, String> {
    if i.is_service {
        return Err("A stack service tab is never the Files source".into());
    }
    let ssh = match (&i.ssh_foreground, &i.live_cwd) {
        (Some(_), _) => true,
        // A live shell at a local prompt is local, whatever was saved.
        (None, Some(_)) => false,
        (None, None) => non_empty(&i.saved_ssh).is_some(),
    };
    if ssh {
        let host = i
            .tunnel_host
            .clone()
            .or_else(|| i.ssh_foreground.as_deref().and_then(host_of_command))
            .or_else(|| non_empty(&i.saved_ssh).and_then(|s| {
                let h = crate::watch::connect_args(s);
                (!h.is_empty()).then_some(h)
            }));
        let Some(host) = host else { return Err("This ssh tab's host is unknown".into()) };
        // Unchecked, the first candidate wins; the prompt's folder never does, since only the
        // host can tell a fish `~/D/I/m/x` abbreviation from a real path (`resolve` asks it).
        return ssh_chain(i, hints)
            .into_iter()
            .find(|(_, src)| *src != "prompt")
            .map(|(c, src)| Pick { cwd: c, source: src, remote_host: Some(host.clone()), missing: false })
            .ok_or_else(|| "No folder is known for this ssh tab yet".into());
    }
    let expand = |s: &str| crate::relocate::expand_home(s).to_string_lossy().into_owned();
    let osc = non_empty(&hints.osc_cwd).filter(|_| osc_is_local(hints.osc_host.as_deref(), local_host));
    let chain: [(Option<String>, &'static str); 6] = [
        (non_empty(&i.live_cwd).map(String::from), "pty"),
        (osc.map(String::from), "osc7"),
        (single_agent_cwd(i).map(expand), "agent"),
        (non_empty(&i.last_cwd).map(expand), "last_cwd"),
        (non_empty(&i.auto_resume_cwd).map(expand), "auto_resume"),
        (non_empty(&i.restore_cwd).map(expand), "restore"),
    ];
    let candidates: Vec<(String, &'static str)> = chain.into_iter().filter_map(|(c, s)| c.filter(|c| Path::new(c).is_absolute()).map(|c| (c, s))).collect();
    if let Some((c, s)) = candidates.iter().find(|(c, _)| exists(Path::new(c))) {
        return Ok(Pick { cwd: c.clone(), source: s, remote_host: None, missing: false });
    }
    match candidates.into_iter().next() {
        Some((c, s)) => Ok(Pick { cwd: c, source: s, remote_host: None, missing: true }),
        None => Err("No folder is known for this tab yet".into()),
    }
}

/// A root equal to home, `/`, a drive root, `/Volumes/*` or `/mnt/*` (§6.5): watched per opened
/// folder only, or a home directory's caches would stream events forever.
pub fn is_shallow(root: &Path) -> bool {
    if root.parent().is_none() {
        return true;
    }
    if let Some(h) = dirs::home_dir() {
        if crate::share::canon(&h) == root || h == root {
            return true;
        }
    }
    let comps: Vec<String> = root.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
    // ["/", "Volumes", "Data"] and ["/", "mnt", "x"]
    comps.len() == 3 && (comps[1] == "Volumes" || comps[1] == "mnt")
}

/// The same rule for a folder on an ssh host, whose home is known once its helper has said
/// (`home`); before that, the usual homes.
pub fn is_shallow_remote(root: &str, home: Option<&str>) -> bool {
    let r = root.trim_end_matches('/');
    if r.is_empty() || r == "~" || r == "/root" || home.is_some_and(|h| !h.is_empty() && h.trim_end_matches('/') == r) {
        return true;
    }
    let parts: Vec<&str> = r.split('/').collect();
    parts.len() == 3 && parts[0].is_empty() && (parts[1] == "home" || parts[1] == "Users" || parts[1] == "mnt" || parts[1] == "Volumes")
}

// ── git toplevel, cached ─────────────────────────────────────────────────────────────────

const TOPLEVEL_CACHE: usize = 256;

#[derive(Default)]
struct TopCache {
    map: HashMap<PathBuf, Option<PathBuf>>,
    order: VecDeque<PathBuf>,
}

fn top_cache() -> &'static Mutex<TopCache> {
    static C: std::sync::OnceLock<Mutex<TopCache>> = std::sync::OnceLock::new();
    C.get_or_init(|| Mutex::new(TopCache::default()))
}

/// The repository top level containing `dir`, cached per folder (an LRU of 256): a prompt in
/// each of 70 tabs must not run 70 git processes.
pub fn toplevel_cached(dir: &Path) -> Option<PathBuf> {
    if let Some(v) = top_cache().lock().map.get(dir) {
        return v.clone();
    }
    let v = crate::share::git::toplevel(dir);
    let mut c = top_cache().lock();
    if c.map.insert(dir.to_path_buf(), v.clone()).is_none() {
        c.order.push_back(dir.to_path_buf());
        while c.order.len() > TOPLEVEL_CACHE {
            if let Some(old) = c.order.pop_front() {
                c.map.remove(&old);
            }
        }
    }
    v
}

/// A repository appeared or went away under `root` (a hub saw a `.git` change).
pub fn invalidate_toplevel_under(root: &Path) {
    let mut c = top_cache().lock();
    c.map.retain(|k, v| !(k.starts_with(root) || v.as_ref().is_some_and(|t| t.starts_with(root))));
    let TopCache { map, order } = &mut *c;
    order.retain(|k| map.contains_key(k));
}

/// The cwd relative to the root, POSIX; None when it IS the root or isn't under it.
fn cwd_rel(root: &Path, cwd: &Path) -> Option<String> {
    let fold = crate::relocate::case_insensitive(root);
    let comps = super::paths::strip_root(cwd, root, fold).or_else(|| super::paths::strip_root(&super::paths::canon_existing(cwd), root, fold))?;
    (!comps.is_empty()).then(|| comps.join("/"))
}

fn remote_place(host: &str, root: &str, cwd: Option<&str>, source: &str) -> TreePlace {
    let rel = cwd.and_then(|c| c.strip_prefix(root.trim_end_matches('/'))).and_then(|r| r.strip_prefix('/')).filter(|r| !r.is_empty());
    TreePlace {
        place: crate::watch::place(Some(host), root),
        kind: "remote".into(),
        host_key: Some(host.to_string()),
        host_label: Some(crate::watch::host_label(host)),
        root: root.to_string(),
        cwd_rel: rel.map(String::from),
        source: source.into(),
        fs_class: super::fsclass::UNKNOWN.into(),
        shallow: is_shallow_remote(root, None),
        missing: None,
    }
}

/// An ssh tab whose every candidate is missing on the host: shown as missing, never as home.
fn missing_remote_place(host: &str, path: &str, source: &str) -> TreePlace {
    TreePlace { missing: Some(path.to_string()), cwd_rel: None, shallow: false, ..remote_place(host, path, None, source) }
}

/// `user@host:/abs` (or `user@host:~/x`) as (host, root); None for anything else. The host is
/// everything before the first `:` that starts a path, so connect args with options
/// (`-p 2222 ews@nova`) stay whole.
pub fn split_remote(place: &str) -> Option<(String, String)> {
    let (host, root) = place.split_once(":/").map(|(h, r)| (h, format!("/{r}"))).or_else(|| place.split_once(":~").map(|(h, r)| (h, format!("~{r}"))))?;
    (!host.trim().is_empty()).then(|| (host.to_string(), root))
}

fn local_place(root: &Path, cwd: Option<&Path>, source: &str) -> TreePlace {
    TreePlace {
        place: root.to_string_lossy().into_owned(),
        kind: "local".into(),
        host_key: None,
        host_label: None,
        root: root.to_string_lossy().into_owned(),
        cwd_rel: cwd.and_then(|c| cwd_rel(root, c)),
        source: source.into(),
        fs_class: super::fsclass::classify(root).into(),
        shallow: is_shallow(root),
        missing: None,
    }
}

fn missing_place(path: &str, source: &str) -> TreePlace {
    TreePlace {
        place: path.to_string(),
        kind: "local".into(),
        host_key: None,
        host_label: None,
        root: path.to_string(),
        cwd_rel: None,
        source: source.into(),
        fs_class: super::fsclass::UNKNOWN.into(),
        shallow: false,
        missing: Some(path.to_string()),
    }
}

/// The place for a pick (and the workspace pin, which wins over the chain).
pub fn place_for(pick: Option<&Pick>, pin: Option<&str>) -> Result<TreePlace, String> {
    if let Some(pin) = pin.map(str::trim).filter(|p| !p.is_empty()) {
        let local = crate::relocate::expand_home(pin);
        if local.is_absolute() {
            if !local.is_dir() {
                return Ok(missing_place(pin, "pin"));
            }
            let root = crate::share::canon(&local);
            let cwd = pick.filter(|p| p.remote_host.is_none() && !p.missing).map(|p| PathBuf::from(&p.cwd));
            return Ok(local_place(&root, cwd.as_deref(), "pin"));
        }
        // "user@host:/abs": the host is everything before the first ':' that precedes a path.
        if let Some((host, root)) = split_remote(pin) {
            let cwd = pick.filter(|p| p.remote_host.as_deref() == Some(host.as_str())).map(|p| p.cwd.as_str());
            return Ok(remote_place(&host, &root, cwd, "pin"));
        }
        return Err(format!("Unreadable Files pin: {pin}"));
    }
    let pick = pick.ok_or("No folder is known for this tab yet")?;
    if let Some(host) = &pick.remote_host {
        // Unchecked on the host (`resolve` couldn't ask it): the folder is its own root.
        return Ok(remote_place(host, &pick.cwd, Some(&pick.cwd), pick.source));
    }
    if pick.missing {
        return Ok(missing_place(&pick.cwd, pick.source));
    }
    let cwd = crate::share::canon(Path::new(&pick.cwd));
    let root = toplevel_cached(&cwd).unwrap_or_else(|| cwd.clone());
    Ok(local_place(&root, Some(&cwd), pick.source))
}

/// Gather a tab's inputs from app state.
fn gather(state: &Arc<AppState>, tab_id: &str, fresh: bool) -> Result<PlaceInputs, String> {
    let mut i = PlaceInputs::default();
    {
        let data = state.app_data.read();
        let mut found = false;
        'outer: for w in &data.windows {
            for ws in &w.workspaces {
                for t in ws.panes.iter().flat_map(|p| p.tabs.iter()).chain(ws.archived_tabs.iter()) {
                    if t.id == tab_id {
                        i.is_service = t.service_id.is_some();
                        i.last_cwd = t.last_cwd.clone();
                        i.auto_resume_cwd = t.auto_resume_cwd.clone();
                        i.restore_cwd = t.restore_cwd.clone();
                        i.auto_resume_remote_cwd = t.auto_resume_remote_cwd.clone();
                        i.restore_remote_cwd = t.restore_remote_cwd.clone();
                        i.saved_ssh = t.auto_resume_ssh_command.clone().filter(|s| !s.trim().is_empty()).or_else(|| t.restore_ssh_command.clone());
                        i.pin = ws.file_tree_pin.clone();
                        found = true;
                        break 'outer;
                    }
                }
            }
        }
        if !found {
            return Err("Tab not found".into());
        }
    }
    let pty = state.tab_pty_map.read().get(tab_id).cloned();
    if let Some(pty) = pty {
        if fresh {
            // An ssh start/exit edge: answer from a fresh process table, not one from before it.
            let _ = crate::pty::manager::get_pty_foreground(state, &pty, true);
        }
        if let Ok(info) = crate::pty::manager::get_pty_info(state, &pty) {
            i.live_cwd = info.cwd;
            i.ssh_foreground = info.foreground_command;
        }
    }
    {
        let sessions = state.agent_sessions.read();
        let mut cwds: Vec<String> = sessions.values().filter(|s| s.tab_id == tab_id).filter_map(|s| s.cwd.clone()).collect();
        cwds.sort();
        cwds.dedup();
        i.agent_cwds = cwds;
    }
    i.tunnel_host = state
        .ssh_tunnels
        .read()
        .values()
        .find(|t| t.tab_ids.contains(tab_id))
        .map(|t| crate::watch::connect_args(&t.host_key))
        .filter(|h| !h.is_empty());
    Ok(i)
}

fn local_hostname() -> String {
    sysinfo::System::host_name().unwrap_or_default()
}

/// An ssh tab's place from its candidates as the host's helper found them (`probed`, in the
/// same order): the first that is a folder there, rooted at its repository's top level. None
/// of them there: the first is reported missing. None when the host couldn't be asked.
pub fn remote_from_probe(host: &str, chain: &[(String, &'static str)], probed: &[super::remote::proto::Probed]) -> Option<TreePlace> {
    for ((_, src), p) in chain.iter().zip(probed) {
        if let Some(real) = &p.real {
            let root = p.top.as_deref().unwrap_or(real);
            return Some(remote_place(host, root, Some(real), src));
        }
    }
    chain.first().map(|(c, src)| missing_remote_place(host, c, src))
}

/// `ft_resolve_place`. Blocking: runs `git rev-parse` (cached), reads the process table, and
/// for an ssh tab asks the host (a few seconds at most).
pub fn resolve(state: &Arc<AppState>, ft: &super::FileTreeState, tab_id: &str, hints: &PlaceHints) -> Result<TreePlace, String> {
    let inputs = gather(state, tab_id, hints.fresh)?;
    if inputs.is_service {
        return Err("A stack service tab is never the Files source".into());
    }
    let picked = pick(&inputs, hints, &local_hostname(), &|p: &Path| p.is_dir());
    match (&picked, inputs.pin.as_deref()) {
        (_, Some(pin)) => place_for(picked.as_ref().ok(), Some(pin)),
        (Ok(p), None) => {
            if let Some(host) = &p.remote_host {
                let chain = ssh_chain(&inputs, hints);
                let paths: Vec<String> = chain.iter().map(|(c, _)| c.clone()).collect();
                let lookup = super::remote::tunnel_lookup(state);
                if let Some(place) = ft.probe_remote(host, Some(lookup), &paths).and_then(|probed| remote_from_probe(host, &chain, &probed)) {
                    return Ok(place);
                }
            }
            place_for(Some(p), None)
        }
        (Err(e), None) => Err(e.clone()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An absolute local path on this platform. A POSIX literal like `/live` is not absolute
    /// on Windows (it has no drive), and the resolver rightly drops it there.
    fn abs(name: &str) -> String {
        std::env::temp_dir().join("ft-place").join(name).to_string_lossy().into_owned()
    }

    fn exists_in(names: &[&str]) -> impl Fn(&Path) -> bool {
        let set: Vec<String> = names.iter().map(|n| abs(n)).collect();
        move |p: &Path| set.iter().any(|s| Path::new(s) == p)
    }

    fn local(i: &PlaceInputs, hints: &PlaceHints, names: &[&str]) -> Result<Pick, String> {
        pick(i, hints, "mybox.local", &exists_in(names))
    }

    #[test]
    fn local_precedence_and_existence() {
        let mut i = PlaceInputs {
            live_cwd: Some(abs("live")),
            agent_cwds: vec![abs("agent")],
            last_cwd: Some(abs("last")),
            auto_resume_cwd: Some(abs("auto")),
            restore_cwd: Some(abs("restore")),
            ..Default::default()
        };
        let osc = PlaceHints { osc_cwd: Some(abs("osc")), ..Default::default() };
        let all: &[&str] = &["live", "osc", "agent", "last", "auto", "restore"];
        assert_eq!(local(&i, &osc, all).unwrap().source, "pty");
        i.live_cwd = None;
        assert_eq!(local(&i, &osc, all).unwrap().source, "osc7");
        // OSC 7 from another machine (an ssh the process table didn't show) is not this disk.
        let remote_osc = PlaceHints { osc_cwd: Some(abs("osc")), osc_host: Some("nova".into()), ..Default::default() };
        assert_eq!(local(&i, &remote_osc, all).unwrap().source, "agent");
        let same_host = PlaceHints { osc_cwd: Some(abs("osc")), osc_host: Some("MyBox".into()), ..Default::default() };
        assert_eq!(local(&i, &same_host, all).unwrap().source, "osc7");
        // Candidates count only if they exist.
        assert_eq!(local(&i, &osc, &["last", "restore"]).unwrap().source, "last_cwd");
        assert_eq!(local(&i, &osc, &["restore"]).unwrap().source, "restore");
        // Two agent sessions disagreeing: neither is "the" folder.
        i.agent_cwds = vec![abs("a1"), abs("a2")];
        assert_eq!(local(&i, &PlaceHints::default(), &["a1", "last"]).unwrap().source, "last_cwd");
    }

    #[test]
    fn a_relative_saved_folder_is_never_a_candidate() {
        // Not a folder on this computer from any starting point: `proj`, and on Windows a
        // rootless `\proj`, which names a different folder on every drive.
        let rel = if cfg!(windows) { r"\proj" } else { "proj" };
        let i = PlaceInputs { last_cwd: Some(rel.into()), restore_cwd: Some(abs("restore")), ..Default::default() };
        assert_eq!(local(&i, &PlaceHints::default(), &[]).unwrap().cwd, abs("restore"));
    }

    #[test]
    fn a_missing_folder_is_reported_never_replaced_by_home() {
        let (gone, old) = (abs("gone-proj"), abs("gone-old"));
        let i = PlaceInputs { last_cwd: Some(gone.clone()), restore_cwd: Some(old), ..Default::default() };
        let p = local(&i, &PlaceHints::default(), &[]).unwrap();
        assert_eq!(p, Pick { cwd: gone.clone(), source: "last_cwd", remote_host: None, missing: true });
        let place = place_for(Some(&p), None).unwrap();
        assert_eq!(place.missing.as_deref(), Some(gone.as_str()));
        assert_eq!(place.root, gone);
        let home = dirs::home_dir().unwrap().to_string_lossy().to_string();
        assert_ne!(place.root, home);
        assert!(local(&PlaceInputs::default(), &PlaceHints::default(), &[]).is_err(), "nothing known is an error, not home");
    }

    #[test]
    fn a_service_tab_is_never_the_source() {
        let i = PlaceInputs { is_service: true, live_cwd: Some(abs("live")), ..Default::default() };
        assert!(local(&i, &PlaceHints::default(), &["live"]).is_err());
    }

    #[test]
    fn ssh_precedence_skips_last_cwd_and_a_stale_osc7() {
        let mut i = PlaceInputs {
            live_cwd: Some("/Users/me/proj".into()),
            ssh_foreground: Some("ssh -p 2222 ews@nova".into()),
            last_cwd: Some("/Users/me/proj".into()),
            auto_resume_remote_cwd: Some("/srv/app".into()),
            restore_remote_cwd: Some("/srv/old".into()),
            ..Default::default()
        };
        // OSC 7 still naming the local shell's folder is stale: the remote shell hasn't spoken.
        let stale = PlaceHints { osc_cwd: Some("/Users/me/proj".into()), ..Default::default() };
        let p = local(&i, &stale, &[]).unwrap();
        assert_eq!((p.cwd.as_str(), p.source, p.remote_host.as_deref()), ("/srv/app", "auto_resume", Some("-p 2222 ews@nova")));
        let fresh = PlaceHints { osc_cwd: Some("/home/ews/x".into()), ..Default::default() };
        assert_eq!(local(&i, &fresh, &[]).unwrap().source, "osc7");
        i.agent_cwds = vec!["/home/ews/agent".into()];
        assert_eq!(local(&i, &fresh, &[]).unwrap().source, "agent");
        // An ssh maiTerm started (a split, a restore) is the same host as a hand-typed one:
        // the `-o ControlMaster=no` it injects is not part of where it goes.
        let mut split = i.clone();
        split.ssh_foreground = Some("ssh -t -o ControlMaster=no -p 2222 ews@nova export MAITERM_TAB_ID=x; cd /srv/app && exec $SHELL -l".into());
        assert_eq!(local(&split, &fresh, &[]).unwrap().remote_host.as_deref(), Some("-p 2222 ews@nova"));
        split.ssh_foreground = Some("ssh -oControlMaster=NO -x ews@nova".into());
        assert_eq!(local(&split, &fresh, &[]).unwrap().remote_host.as_deref(), Some("-x ews@nova"));
        // The tunnel's host wins over parsing the command line.
        i.tunnel_host = Some("ews@nova".into());
        assert_eq!(local(&i, &fresh, &[]).unwrap().remote_host.as_deref(), Some("ews@nova"));
        // A suspended ssh tab is still an ssh tab: its saved remote folder, never last_cwd.
        let suspended = PlaceInputs { saved_ssh: Some("ews@nova".into()), last_cwd: Some(abs("me")), restore_remote_cwd: Some("/srv/r".into()), ..Default::default() };
        let p = local(&suspended, &PlaceHints::default(), &["me"]).unwrap();
        assert_eq!((p.cwd.as_str(), p.remote_host.as_deref()), ("/srv/r", Some("ews@nova")));
        let place = place_for(Some(&p), None).unwrap();
        assert_eq!(place.place, "ews@nova:/srv/r");
        assert_eq!(place.kind, "remote");
    }

    #[test]
    fn a_pin_wins_and_keeps_you_are_here() {
        let tmp = std::env::temp_dir().join(format!("ft-pin-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(tmp.join("sub")).unwrap();
        let root = crate::share::canon(&tmp);
        let pick = Pick { cwd: root.join("sub").to_string_lossy().into(), source: "pty", remote_host: None, missing: false };
        let place = place_for(Some(&pick), Some(&tmp.to_string_lossy())).unwrap();
        assert_eq!((place.source.as_str(), place.cwd_rel.as_deref()), ("pin", Some("sub")));
        assert_eq!(place.root, root.to_string_lossy());
        let no_such = tmp.join("no").join("such").to_string_lossy().into_owned();
        let gone = place_for(Some(&pick), Some(&no_such)).unwrap();
        assert_eq!(gone.missing.as_deref(), Some(no_such.as_str()));
        let remote = place_for(None, Some("ews@nova:/srv/app")).unwrap();
        assert_eq!((remote.kind.as_str(), remote.host_key.as_deref(), remote.root.as_str()), ("remote", Some("ews@nova"), "/srv/app"));
        std::fs::remove_dir_all(&tmp).unwrap();
    }

    #[test]
    fn shallow_roots() {
        assert!(is_shallow(Path::new("/")));
        assert!(is_shallow(&dirs::home_dir().unwrap()));
        assert!(is_shallow(Path::new("/Volumes/Data")));
        assert!(is_shallow(Path::new("/mnt/x")));
        assert!(!is_shallow(Path::new("/Volumes/Data/proj")));
        assert!(!is_shallow(&std::env::temp_dir().join("proj")));
        assert!(is_shallow_remote("/home/ews", None));
        assert!(is_shallow_remote("~", None));
        assert!(!is_shallow_remote("/home/ews/app", None));
        assert!(is_shallow_remote("/srv/users/ews", Some("/srv/users/ews/")), "the home the helper reported");
    }

    #[test]
    fn an_ssh_tab_is_rooted_by_what_the_host_found() {
        use crate::filetree::remote::proto::Probed;
        let i = PlaceInputs {
            ssh_foreground: Some("ssh ews@nova".into()),
            live_cwd: Some("/Users/me".into()),
            agent_cwds: vec!["/home/ews/gone".into()],
            restore_remote_cwd: Some("/home/ews/app/src".into()),
            ..Default::default()
        };
        // A fish abbreviation in the title looks like a path; only the host can say it isn't.
        let hints = PlaceHints { prompt_cwd: Some("~/D/I/m/x".into()), ..Default::default() };
        let chain = ssh_chain(&i, &hints);
        assert_eq!(chain.iter().map(|(_, s)| *s).collect::<Vec<_>>(), vec!["agent", "prompt", "restore"]);
        // Unchecked, the prompt's folder never wins.
        assert_eq!(local(&i, &hints, &[]).unwrap().source, "agent");
        let probed = vec![
            Probed::default(),
            Probed::default(),
            Probed { real: Some("/home/ews/app/src".into()), top: Some("/home/ews/app".into()) },
        ];
        let place = remote_from_probe("ews@nova", &chain, &probed).unwrap();
        assert_eq!(
            (place.place.as_str(), place.root.as_str(), place.cwd_rel.as_deref(), place.source.as_str()),
            ("ews@nova:/home/ews/app", "/home/ews/app", Some("src"), "restore")
        );
        // Nothing there: the first candidate is missing, never home.
        let gone = remote_from_probe("ews@nova", &chain, &[Probed::default(), Probed::default(), Probed::default()]).unwrap();
        assert_eq!((gone.missing.as_deref(), gone.kind.as_str()), (Some("/home/ews/gone"), "remote"));
        // A prompt folder that is real on the host does win in its turn.
        let hints = PlaceHints { prompt_cwd: Some("~/proj".into()), ..Default::default() };
        let chain = ssh_chain(&i, &hints);
        let probed = vec![Probed::default(), Probed { real: Some("/home/ews/proj".into()), top: None }, Probed::default()];
        let place = remote_from_probe("ews@nova", &chain, &probed).unwrap();
        assert_eq!((place.root.as_str(), place.source.as_str(), place.cwd_rel), ("/home/ews/proj", "prompt", None));
        assert_eq!(split_remote("-p 2222 ews@nova:/srv/a b"), Some(("-p 2222 ews@nova".into(), "/srv/a b".into())));
        assert_eq!(split_remote("/local/path"), None);
    }
}
