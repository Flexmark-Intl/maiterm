//! The `brew` and `plugin` setup routes (docs/hive-media.md §6): one install command, built only
//! from the catalogue, typed into a visible terminal tab, and its exit read from OSC 133.
//!
//! - **The line.** `plan` builds it from the provider's route and nothing the caller sends: the
//!   setup sheet passes a provider id, never a command. Every word of it is checked against a
//!   narrow alphabet, so a catalogue typo can't become shell syntax. With no Homebrew on this
//!   machine, the line starts with Homebrew's own installer (it asks for the human's password,
//!   which is why it runs in a tab they can see) and then calls the new `brew` by its full path,
//!   since the shell that ran the installer has no `brew` on its PATH yet.
//! - **The tab.** The window makes it (`media.install.start`, `hive/rpc.rs`): a plain strip tab
//!   on this machine, typed into through the stack's guard (a shell at its prompt, settled there).
//!   It is no agent's tab, so the agent-input guards (`trust_dialog_open`, the draft hold) don't
//!   apply. Just before it types, the window arms the install here (`hive_media_install_arm`).
//! - **The end.** The PTY reader hands every OSC 133 mark to `on_osc133`. The first B after the
//!   arm is the line beginning, and the D after that B is its exit: `hive-media-install-finished`
//!   goes to the window, which re-detects. No polling. The exit code only says when to re-detect:
//!   a line's output could print its own D, and the status the sheet reads after it is the truth.
//! - **Plugins are coordinator-only.** A plugin installs into the root the coordinator loads,
//!   pinned with `env` (`plugin_root`). With a managed Claude account active that is `~/.claude`,
//!   which every managed root's `plugins` and `settings.json` link back to (re-linked by
//!   `accounts::resync_roots` after a success), even when the human also exported a
//!   `CLAUDE_CONFIG_DIR`. Without one it is that exported root, else `~/.claude`. Hive workers launch with
//!   `--setting-sources project` and load no user plugin or skill (docs/hive.md §6.7); nothing
//!   here changes that.
//! - **whisper.cpp.** A brew line that installs `whisper-cpp` is followed, on a clean exit, by
//!   maiTerm's own download of the default model (`whisper::ensure_default`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, OnceLock};

use serde::Serialize;
use serde_json::{json, Value};
use tauri::{AppHandle, Emitter, Manager};

use super::{provider, Provider, Route};
use crate::state::AppState;

/// To the window that started an install, when its line exits or its tab's shell goes:
/// `InstallFinished`.
pub const FINISHED_EVENT: &str = "hive-media-install-finished";

/// Homebrew's own installer, exactly as brew.sh gives it.
pub const HOMEBREW_INSTALLER: &str = r#"/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)""#;

/// The formula whose install is followed by the default model's download.
pub const WHISPER_FORMULA: &str = "whisper-cpp";

// ─── The line ───────────────────────────────────────────────────────────────

/// What the line depends on in this machine, read by `Here::detect` or given by a test.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Here {
    /// `brew`, where it is found (`accounts::find_executables`), if it is.
    pub brew: Option<PathBuf>,
    /// Where Homebrew's installer puts `brew` on this platform, if Homebrew runs here at all.
    pub installer_brew: Option<&'static str>,
    /// The Claude config root a plugin installs into (`plugin_root`), or `None` for the default
    /// `~/.claude`.
    pub claude_root: Option<PathBuf>,
    /// A POSIX shell and `env`: the only place these lines run.
    pub unix: bool,
}

impl Here {
    pub fn detect(prefs: &crate::state::Preferences) -> Here {
        Here {
            brew: crate::accounts::find_executables("brew").into_iter().next(),
            installer_brew: installer_brew(),
            claude_root: plugin_root(prefs, crate::accounts::user_claude_root()),
            unix: cfg!(unix),
        }
    }
}

/// The Claude config root the coordinator loads: the active managed Claude account's root, else
/// the human's exported `CLAUDE_CONFIG_DIR` (`users_own`), else `<home>/.claude`. The one rule
/// launch (`launch::config_root_for`, which this extends with the default), status
/// (`status::plugin_roots`) and install (`plugin_root`) all read, so they can't disagree.
pub fn coordinator_config_root(prefs: &crate::state::Preferences, users_own: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    crate::hive::launch::config_root_for(prefs, users_own).or_else(|| home.map(|h| h.join(".claude")))
}

/// The root a plugin installs into for the coordinator to load it (`None`: `~/.claude`, by
/// `env -u CLAUDE_CONFIG_DIR`): `coordinator_config_root`, except that a managed account's root
/// is written through to `~/.claude`, where its `plugins` and `settings.json` link (accounts
/// `cfg("plugins")`). Writing into the account root itself would make a real `plugins` folder
/// there when the link is missing, which the next reconcile renames aside.
pub fn plugin_root(prefs: &crate::state::Preferences, users_own: Option<&Path>) -> Option<PathBuf> {
    let root = coordinator_config_root(prefs, users_own, None)?;
    let managed = crate::accounts::active_accounts(prefs)
        .into_iter()
        .filter(|(rt, _)| *rt == crate::accounts::Runtime::Claude)
        .filter_map(|(rt, id)| crate::accounts::account_root(rt, &id))
        .any(|r| r == root);
    (!managed).then_some(root)
}

/// Homebrew's default prefix: `/opt/homebrew` on Apple silicon, `/usr/local` on an Intel Mac,
/// `/home/linuxbrew/.linuxbrew` on Linux. None on Windows.
fn installer_brew() -> Option<&'static str> {
    if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Some("/opt/homebrew/bin/brew")
    } else if cfg!(target_os = "macos") {
        Some("/usr/local/bin/brew")
    } else if cfg!(target_os = "linux") {
        Some("/home/linuxbrew/.linuxbrew/bin/brew")
    } else {
        None
    }
}

/// One install, as `plan` builds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallPlan {
    pub provider_id: String,
    /// `brew` | `plugin`.
    pub kind: String,
    /// What is typed (and shown): one line, no secret.
    pub line: String,
    /// The line starts with Homebrew's installer.
    pub installs_homebrew: bool,
    /// A clean exit is followed by the whisper model's download.
    pub whisper_model: bool,
}

/// The install line for `provider_id`, from the catalogue route alone. Refuses an unknown
/// provider, one with no `brew` or `plugin` route, and a route whose words aren't plain.
pub fn plan(provider_id: &str, here: &Here) -> Result<InstallPlan, String> {
    let p = provider(provider_id).ok_or_else(|| format!("There is no media provider called {provider_id:?}."))?;
    plan_for(p, here)
}

pub fn plan_for(p: &Provider, here: &Here) -> Result<InstallPlan, String> {
    let Some(route) = p.routes.iter().find(|r| r.kind == "brew" || r.kind == "plugin") else {
        let kinds: Vec<&str> = p.routes.iter().map(|r| r.kind.as_str()).collect();
        let how = if kinds.is_empty() { "nothing maiTerm can set up".to_string() } else { kinds.join(", ") };
        return Err(format!("{} isn't installed in a terminal tab: its setup is {how}.", p.name));
    };
    if !here.unix {
        return Err(format!("{} installs from a macOS or Linux terminal only.", p.name));
    }
    match route.kind.as_str() {
        "brew" => brew_plan(p, route, here),
        _ => plugin_plan(p, route, here),
    }
}

fn brew_plan(p: &Provider, route: &Route, here: &Here) -> Result<InstallPlan, String> {
    let command = route.command.as_deref().unwrap_or_default();
    let formulae = brew_formulae(command).ok_or_else(|| format!("{}'s install command in maiTerm's catalogue isn't a plain `brew install`: {command:?}.", p.name))?;
    let (brew, installs_homebrew) = match &here.brew {
        Some(path) => (sh_word(&path.to_string_lossy()), false),
        None => {
            let b = here.installer_brew.ok_or_else(|| format!("Homebrew doesn't run on this system, so {} can't be installed from here.", p.name))?;
            (b.to_string(), true)
        }
    };
    let install = format!("{brew} install {}", formulae.join(" "));
    let line = if installs_homebrew { format!("{HOMEBREW_INSTALLER} && {install}") } else { install };
    Ok(InstallPlan { provider_id: p.id.clone(), kind: "brew".to_string(), line, installs_homebrew, whisper_model: formulae.contains(&WHISPER_FORMULA) })
}

/// The formulae of `brew install a b c`: nothing else (no flag, no second command), each word a
/// formula name or a `tap/user/formula`.
pub fn brew_formulae(command: &str) -> Option<Vec<&str>> {
    let mut words = command.split_whitespace();
    if words.next() != Some("brew") || words.next() != Some("install") {
        return None;
    }
    let formulae: Vec<&str> = words.collect();
    let plain = |w: &&str| w.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit()) && w.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "@+._-/".contains(c));
    (!formulae.is_empty() && formulae.iter().all(plain)).then_some(formulae)
}

fn plugin_plan(p: &Provider, route: &Route, here: &Here) -> Result<InstallPlan, String> {
    let id = route.plugin_id.as_deref().filter(|id| plugin_id_ok(id)).ok_or_else(|| format!("{}'s plugin id in maiTerm's catalogue isn't `name@marketplace`.", p.name))?;
    let Some(source) = route.marketplace.as_deref() else {
        return Err(format!(
            "{name} can't be installed from here yet: maiTerm doesn't have a confirmed source for its plugin marketplace. Follow {name}'s guide to install the plugin `{id}` in Claude Code yourself.",
            name = p.name
        ));
    };
    if !marketplace_ok(source) {
        return Err(format!("{}'s marketplace source in maiTerm's catalogue isn't plain: {source:?}.", p.name));
    }
    // The coordinator's root (`plugin_root`), whatever account this tab spawned under: a managed
    // root links its `plugins` back to `~/.claude` (accounts `cfg("plugins")`), never the other way.
    let env = match &here.claude_root {
        Some(root) => format!("env CLAUDE_CONFIG_DIR={}", sh_word(&root.to_string_lossy())),
        None => "env -u CLAUDE_CONFIG_DIR".to_string(),
    };
    // `;`, not `&&`: adding a marketplace that is already there fails, and the install is what
    // decides the exit either way.
    let line = format!("{env} claude plugin marketplace add {source}; {env} claude plugin install {id}");
    Ok(InstallPlan { provider_id: p.id.clone(), kind: "plugin".to_string(), line, installs_homebrew: false, whisper_model: false })
}

fn plain_word(w: &str, extra: &str) -> bool {
    !w.is_empty() && !w.starts_with('-') && w.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c) || extra.contains(c))
}

fn plugin_id_ok(id: &str) -> bool {
    id.split_once('@').is_some_and(|(name, market)| plain_word(name, "") && plain_word(market, ""))
}

/// `owner/repo`, or an https URL of one.
fn marketplace_ok(source: &str) -> bool {
    plain_word(source, "/:@~+")
}

/// `s` as one shell word: as it is when every character is plain, else single-quoted.
pub fn sh_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+@=:,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

// ─── Installs in flight ─────────────────────────────────────────────────────

/// One install from its start to its exit. Memory only: a restart forgets it, and the sheet's
/// own Re-detect still works.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Install {
    pub id: String,
    pub provider_id: String,
    /// The window that asked, which hears the end.
    pub window: String,
    pub tab_id: Option<String>,
    /// Set by the arm, just before the window types the line.
    pub pty_id: Option<String>,
    /// The shell's B for the line has been seen.
    pub began: bool,
    pub kind: String,
    pub whisper_model: bool,
}

/// What `hive-media-install-finished` carries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallFinished {
    pub provider_id: String,
    pub tab_id: String,
    /// The line exited 0. Only when to re-detect: the status is the truth.
    pub ok: bool,
    /// The shell's exit code for the line; null when the tab's shell went away first.
    pub exit_code: Option<i32>,
    /// `exited` (its D arrived) | `closed` (the tab's shell ended before it).
    pub ended: String,
    /// maiTerm now downloads whisper.cpp's model: `hive-media-model-progress` follows.
    pub model_download: bool,
}

#[derive(Debug, Default)]
pub struct Installs {
    list: Vec<Install>,
}

impl Installs {
    /// A new install of `plan`. Refused while one for the same provider is in flight.
    pub fn begin(&mut self, id: &str, plan: &InstallPlan, window: &str) -> Result<(), String> {
        if self.list.iter().any(|i| i.provider_id == plan.provider_id) {
            return Err("It is already installing in its own terminal tab.".to_string());
        }
        self.list.push(Install {
            id: id.to_string(),
            provider_id: plan.provider_id.clone(),
            window: window.to_string(),
            tab_id: None,
            pty_id: None,
            began: false,
            kind: plan.kind.clone(),
            whisper_model: plan.whisper_model,
        });
        Ok(())
    }

    /// The window is about to type the line into `pty_id`. Refused for an install that was given
    /// up (its start timed out, or it was forgotten), so a window that woke late types nothing.
    pub fn arm(&mut self, id: &str, tab_id: &str, pty_id: &str) -> Result<(), String> {
        let i = self.list.iter_mut().find(|i| i.id == id).ok_or("maiTerm is no longer waiting for that install.")?;
        if i.pty_id.is_some() {
            return Err("That install was already typed.".to_string());
        }
        i.tab_id = Some(tab_id.to_string());
        i.pty_id = Some(pty_id.to_string());
        Ok(())
    }

    pub fn forget(&mut self, id: &str) -> Option<Install> {
        let at = self.list.iter().position(|i| i.id == id)?;
        Some(self.list.remove(at))
    }

    pub fn is_armed(&self, id: &str) -> bool {
        self.list.iter().any(|i| i.id == id && i.pty_id.is_some())
    }

    /// An OSC 133 mark on `pty_id`. B (or C) after the arm begins the line; a D after that is its
    /// exit, which ends the install. A D before any B (an empty Enter's prompt cycle) says
    /// nothing about it.
    pub fn osc(&mut self, pty_id: &str, cmd: char, exit_code: Option<i32>) -> Option<(Install, InstallFinished)> {
        let at = self.list.iter().position(|i| i.pty_id.as_deref() == Some(pty_id))?;
        match cmd {
            'B' | 'C' => {
                self.list[at].began = true;
                None
            }
            'D' if self.list[at].began => {
                let i = self.list.remove(at);
                let ok = exit_code == Some(0);
                let done = finished(&i, ok, exit_code, "exited");
                Some((i, done))
            }
            _ => None,
        }
    }

    /// `pty_id`'s shell is gone: an install armed on it ends, not ok.
    pub fn pty_closed(&mut self, pty_id: &str) -> Option<(Install, InstallFinished)> {
        let at = self.list.iter().position(|i| i.pty_id.as_deref() == Some(pty_id))?;
        let i = self.list.remove(at);
        let done = finished(&i, false, None, "closed");
        Some((i, done))
    }
}

fn finished(i: &Install, ok: bool, exit_code: Option<i32>, ended: &str) -> InstallFinished {
    InstallFinished {
        provider_id: i.provider_id.clone(),
        tab_id: i.tab_id.clone().unwrap_or_default(),
        ok,
        exit_code,
        ended: ended.to_string(),
        model_download: ok && i.whisper_model,
    }
}

static INSTALLS: LazyLock<Mutex<Installs>> = LazyLock::new(Default::default);
static HANDLE: OnceLock<AppHandle> = OnceLock::new();

fn installs() -> MutexGuard<'static, Installs> {
    INSTALLS.lock().unwrap_or_else(|e| e.into_inner())
}

/// Called by the PTY reader on every OSC 133 mark: one lookup for a PTY that runs no install.
pub fn on_osc133(pty_id: &str, cmd: char, exit_code: Option<i32>) {
    if !matches!(cmd, 'B' | 'C' | 'D') {
        return;
    }
    let hit = installs().osc(pty_id, cmd, exit_code);
    if let Some((i, done)) = hit {
        ended(i, done);
    }
}

/// Called by the PTY's teardown.
pub fn on_pty_closed(pty_id: &str) {
    let hit = installs().pty_closed(pty_id);
    if let Some((i, done)) = hit {
        ended(i, done);
    }
}

/// Whether the account roots are resynced BEFORE `hive-media-install-finished` goes out: only
/// after a plugin install that exited 0. A managed root reconciled before `~/.claude/plugins`
/// existed has no `plugins` link yet (`reconcile_at` skips an absent source), and the sheet
/// re-detects on the event, so announcing first would show the plugin missing right after a good
/// install. A brew install and a failed plugin install are announced at once.
pub fn resync_before_announcing(i: &Install, done: &InstallFinished) -> bool {
    done.ok && i.kind == "plugin"
}

fn ended(i: Install, done: InstallFinished) {
    log::info!("hive media: install of {} {} (exit {:?})", i.provider_id, done.ended, done.exit_code);
    let Some(handle) = HANDLE.get() else { return };
    let resync_first = resync_before_announcing(&i, &done);
    let announce = move || {
        if let Err(e) = handle.emit_to(i.window.as_str(), FINISHED_EVENT, &done) {
            log::warn!("hive media: {FINISHED_EVENT} not sent: {e}");
        }
        if done.model_download {
            super::whisper::start_default(handle.clone());
        }
    };
    if !resync_first {
        announce();
        return;
    }
    // Order matters: relink every managed root to `~/.claude/plugins`, THEN announce, so the
    // sheet's re-detect on the event already sees the plugin through the link.
    tauri::async_runtime::spawn_blocking(move || {
        match dirs::home_dir() {
            Some(home) => {
                if let Err(e) = crate::accounts::resync_roots(crate::accounts::Runtime::Claude, &home) {
                    log::warn!("hive media: account roots not resynced after a plugin install: {e}");
                }
            }
            None => log::warn!("hive media: no home folder; account roots not resynced after a plugin install"),
        }
        announce();
    });
}

// ─── Commands ───────────────────────────────────────────────────────────────

/// What `hive_media_install` answers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InstallStarted {
    /// The visible terminal tab the line was typed into.
    pub tab_id: String,
    /// The line, exactly as typed.
    pub command_shown: String,
    /// `hive-media-install-finished` will come for it. False when the tab's shell has no OSC 133
    /// integration: its exit can't be read, so the sheet re-detects on the human's word.
    pub watches_exit: bool,
}

/// Install `provider_id` (a catalogue `brew` or `plugin` route) in a visible terminal tab of the
/// calling window. The line comes from the catalogue alone. Answers once the line is typed;
/// `hive-media-install-finished` follows when it exits, to the calling window.
///
/// `avoid_workspace_id`: a workspace the tab must not open in. The Create sheet passes the
/// workspace it is making into a Hive: it isn't a Hive yet, so the window's own rule (never a
/// Hive's workspace) would let the install tab land in it.
///
/// Preferences has no terminal tabs, so a call from it opens the tab in the terminal window the
/// human was last in; the finished event still comes back to Preferences, whose sheet listens.
#[tauri::command]
pub async fn hive_media_install(
    window: tauri::Window,
    state: tauri::State<'_, Arc<AppState>>,
    provider_id: String,
    avoid_workspace_id: Option<String>,
) -> Result<InstallStarted, String> {
    let prefs = state.app_data.read().preferences.clone();
    let here = tauri::async_runtime::spawn_blocking(move || Here::detect(&prefs)).await.map_err(|e| e.to_string())?;
    let plan = plan(&provider_id, &here)?;
    let name = provider(&provider_id).map_or(provider_id.clone(), |p| p.name.clone());
    let handle = window.app_handle().clone();
    HANDLE.get_or_init(|| handle.clone());
    let id = uuid::Uuid::new_v4().to_string();
    installs().begin(&id, &plan, window.label()).map_err(|e| format!("{name}: {e}"))?;
    let tab_window = match tab_window_for(window.label(), || crate::focused_app_window(&handle)) {
        Some(w) => w,
        None => {
            installs().forget(&id);
            return Err("Open a maiTerm window first: the install runs in one of its tabs.".to_string());
        }
    };
    let args = json!({
        "install_id": id,
        "provider_id": plan.provider_id,
        "title": format!("Install {name}"),
        "line": plan.line,
        "avoid_workspace_id": avoid_workspace_id,
    });
    let outcome = crate::mailink::rpc::request_on(
        state.inner(),
        Some(&handle),
        crate::hive::rpc::EVENT,
        &tab_window,
        crate::hive::rpc::VERB_MEDIA_INSTALL,
        args,
        crate::hive::rpc::JOB_START_TIMEOUT,
    )
    .await;
    let answer = crate::hive::rpc::accepted(outcome);
    let started = answer.and_then(|v| started_from(&v, &plan.line));
    match started {
        Ok(s) => {
            // No integration: no D will come, so nothing should wait for one. (A watched line
            // that already exited is gone from the list, and its event already went: the sheet
            // listens before it calls.)
            if !s.watches_exit {
                installs().forget(&id);
            }
            log::info!("hive media: installing {} in tab {} ({})", plan.provider_id, s.tab_id, plan.kind);
            Ok(s)
        }
        Err(e) => {
            installs().forget(&id);
            log::warn!("hive media: install of {} not started: {e}", plan.provider_id);
            Err(e)
        }
    }
}

/// The window whose tab runs the install: the caller, unless it is Preferences or Help (no
/// terminal tabs, and no `hive-frontend-request` listener), then the app window `fallback` names.
pub fn tab_window_for(caller: &str, fallback: impl FnOnce() -> Option<String>) -> Option<String> {
    if caller == "preferences" || caller == "help" {
        fallback()
    } else {
        Some(caller.to_string())
    }
}

/// The window's answer to `media.install.start`: `{tab_id, watches_exit}`, or an error.
fn started_from(v: &Value, line: &str) -> Result<InstallStarted, String> {
    let tab_id = v.get("tab_id").and_then(Value::as_str).filter(|t| !t.is_empty()).ok_or("maiTerm's window didn't say which tab it opened.")?;
    let watches_exit = v.get("watches_exit").and_then(Value::as_bool) == Some(true);
    Ok(InstallStarted { tab_id: tab_id.to_string(), command_shown: line.to_string(), watches_exit })
}

/// The window has a shell at its prompt for install `install_id` and is about to type its line.
/// Refused for an install maiTerm gave up on: the window then closes the tab and types nothing.
#[tauri::command]
pub fn hive_media_install_arm(install_id: String, tab_id: String, pty_id: String) -> Result<(), String> {
    installs().arm(&install_id, &tab_id, &pty_id)
}

#[cfg(test)]
mod tests;
