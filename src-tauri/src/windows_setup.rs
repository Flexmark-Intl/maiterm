//! Getting Claude running on Windows: what the setup wizard detects, and the two things maiTerm
//! fixes for its own tabs without asking.
//!
//! Claude Code runs natively from any Windows shell, but it runs hook commands through Git Bash,
//! falling back to PowerShell when Git for Windows is missing — and maiTerm's own hooks
//! (claude_code/lockfile.rs) are POSIX shell, so without Git for Windows the hook that tells
//! maiTerm which tab a session is in fails silently. That makes Git for Windows the one real
//! requirement, and Git Bash the natural default shell (`"auto"`). The other trap is PATH: the
//! native installer puts `claude.exe` in `%USERPROFILE%\.local\bin` and does NOT add that folder
//! to PATH, and an older Claude Desktop's `Claude.exe` alias in WindowsApps can answer `claude`
//! first. (A tab's PATH is otherwise current: portable-pty builds each spawn's environment from
//! the stored PATH, so something installed while maiTerm runs is already there.)
//!
//! Everything that decides is a pure function over strings, unit-tested on every platform; only
//! the probes and the PATH write touch the machine, and they are Windows-only.

// The pure half is called only from the Windows half and the tests.
#![cfg_attr(not(windows), allow(dead_code))]

use serde::Serialize;

/// What the wizard shows. Off Windows everything is empty and `windows` is false.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ClaudeSetupReport {
    pub windows: bool,
    /// Git for Windows' `bin\bash.exe`. None = Git for Windows is not installed (anywhere we look).
    pub git_bash: Option<String>,
    /// The `claude` maiTerm found, on PATH or in a folder an installer uses.
    pub claude: Option<String>,
    pub claude_version: Option<String>,
    /// `claude` resolves to it in a NEW terminal outside maiTerm (the user's PATH as stored, not
    /// the PATH this process started with). maiTerm's own tabs get its folder either way.
    pub claude_on_path: bool,
    /// An older Claude Desktop's `Claude.exe` in WindowsApps that comes first on PATH: typing
    /// `claude` opens the desktop app instead.
    pub claude_shadowed_by: Option<String>,
    /// Version of the default WSL distro (1 or 2); None when WSL has no distro installed.
    pub wsl_version: Option<u8>,
    /// `winget` is available, so Git and Claude can be installed with it.
    pub winget: bool,
    /// The saved shell preference, and the shell new tabs actually get from it.
    pub shell_pref: String,
    pub shell: String,
}

/// Where to look. Paths are plain strings because they are Windows paths even when the tests run
/// on macOS, where `Path` would not split them.
#[derive(Debug, Clone, Default)]
pub struct Places {
    /// `%ProgramFiles%`, `%ProgramW6432%`, `%ProgramFiles(x86)%`.
    pub program_files: Vec<String>,
    pub local_app_data: Option<String>,
    pub app_data: Option<String>,
    pub user_profile: Option<String>,
    /// The PATH a new terminal gets: machine entries then user entries, expanded.
    pub path_dirs: Vec<String>,
}

pub fn join(dir: &str, rel: &str) -> String {
    format!("{}\\{}", dir.trim_end_matches(['\\', '/']), rel)
}

/// Parent folder of a Windows path, or None at a root.
fn parent(dir: &str) -> Option<&str> {
    let t = dir.trim_end_matches(['\\', '/']);
    t.rfind(['\\', '/']).map(|i| &t[..i]).filter(|p| !p.is_empty() && !p.ends_with(':'))
}

/// Folder paths compare case-insensitively and without a trailing separator, as Windows does.
fn same_dir(a: &str, b: &str) -> bool {
    a.trim_end_matches(['\\', '/']).eq_ignore_ascii_case(b.trim_end_matches(['\\', '/']))
}

pub fn split_path(path: &str) -> Vec<String> {
    path.split(';')
        .map(|d| d.trim().trim_matches('"').to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

/// Expand `%NAME%` the way a REG_EXPAND_SZ PATH entry is expanded. An unknown name stays as
/// written, as Windows leaves it.
pub fn expand_env(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after.find('%') {
            Some(end) if end > 0 => {
                let name = &after[..end];
                match lookup(name) {
                    Some(v) => out.push_str(&v),
                    None => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Git for Windows' bash, in the order Claude Code itself looks (Program Files, then `git` on
/// PATH), then the per-user installer's and scoop's folders, which Claude only finds through PATH.
/// Always `bin\bash.exe`: it is the launcher that sets up Git's PATH, where `usr\bin\bash.exe`
/// is a bare shell, and Claude accepts only a file named bash.exe or sh.exe.
pub fn find_git_bash(p: &Places, exists: &dyn Fn(&str) -> bool) -> Option<String> {
    let mut roots: Vec<String> = p.program_files.iter().map(|pf| join(pf, "Git")).collect();
    for dir in &p.path_dirs {
        if exists(&join(dir, "git.exe")) {
            // git.exe lives in Git\cmd, Git\bin or Git\mingw64\bin.
            if let Some(up) = parent(dir) {
                roots.push(up.to_string());
                if let Some(up2) = parent(up) {
                    roots.push(up2.to_string());
                }
            }
        }
    }
    if let Some(lad) = &p.local_app_data {
        roots.push(join(lad, r"Programs\Git"));
    }
    if let Some(up) = &p.user_profile {
        roots.push(join(up, r"scoop\apps\git\current"));
    }
    roots.into_iter().map(|r| join(&r, r"bin\bash.exe")).find(|b| exists(b))
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ClaudeFound {
    pub path: Option<String>,
    pub on_path: bool,
    pub shadowed_by: Option<String>,
}

/// The Store alias folder an older Claude Desktop registers its `Claude.exe` in.
fn is_windows_apps(dir: &str) -> bool {
    dir.to_ascii_lowercase().trim_end_matches(['\\', '/']).ends_with(r"\microsoft\windowsapps")
}

/// Which `claude` a new terminal runs, and where one is installed when none is on PATH.
pub fn find_claude(p: &Places, exists: &dyn Fn(&str) -> bool) -> ClaudeFound {
    let mut found = ClaudeFound::default();
    for dir in &p.path_dirs {
        let hit = ["claude.exe", "claude.cmd"].iter().map(|f| join(dir, f)).find(|f| exists(f));
        let Some(hit) = hit else { continue };
        if is_windows_apps(dir) {
            found.shadowed_by.get_or_insert(hit);
            continue;
        }
        found.path = Some(hit);
        found.on_path = true;
        return found;
    }
    let mut known = Vec::new();
    if let Some(up) = &p.user_profile {
        known.push(join(up, r".local\bin\claude.exe")); // the native installer
    }
    if let Some(lad) = &p.local_app_data {
        known.push(join(lad, r"Microsoft\WinGet\Links\claude.exe"));
    }
    if let Some(ad) = &p.app_data {
        known.push(join(ad, r"npm\claude.cmd"));
    }
    found.path = known.into_iter().find(|k| exists(k));
    found
}

/// The folder a found `claude` is in, written the way it should go into the user's PATH:
/// under the profile as `%USERPROFILE%\...`, so the entry survives a renamed profile folder.
pub fn path_entry_for(claude: &str, user_profile: Option<&str>) -> Option<String> {
    let dir = parent(claude)?;
    if let Some(up) = user_profile {
        let up = up.trim_end_matches(['\\', '/']);
        if dir.len() > up.len() && dir[..up.len()].eq_ignore_ascii_case(up) && dir[up.len()..].starts_with('\\') {
            return Some(format!("%USERPROFILE%{}", &dir[up.len()..]));
        }
    }
    Some(dir.to_string())
}

/// The user PATH value with `entry` added, or None when it (expanded) is already there. It goes
/// just before the WindowsApps folder when the user PATH has one (it does by default), so an
/// older Claude Desktop's alias there can't answer `claude` first; otherwise at the end.
pub fn with_entry_added(current: &str, entry: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let want = expand_env(entry, lookup);
    let mut dirs = split_path(current);
    if dirs.iter().any(|d| same_dir(&expand_env(d, lookup), &want)) {
        return None;
    }
    let at = dirs.iter().position(|d| is_windows_apps(&expand_env(d, lookup))).unwrap_or(dirs.len());
    dirs.insert(at, entry.to_string());
    Some(dirs.join(";"))
}

/// PATH for a new tab, from the one the tab already gets: `claude`'s folder added when it isn't
/// there, ahead of the WindowsApps folder; and when Claude Desktop's alias there `shadows` it,
/// an existing entry behind WindowsApps moves in front. Nothing else moves or goes. None = no
/// change.
pub fn tab_path(current: &str, claude_dir: &str, shadows: bool) -> Option<String> {
    let mut dirs = split_path(current);
    let apps = dirs.iter().position(|d| is_windows_apps(d));
    match (dirs.iter().position(|d| same_dir(d, claude_dir)), apps) {
        (Some(have), Some(apps)) if shadows && have > apps => {
            let d = dirs.remove(have);
            dirs.insert(apps, d);
        }
        (Some(_), _) => return None,
        (None, apps) => dirs.insert(apps.unwrap_or(dirs.len()), claude_dir.to_string()),
    }
    Some(dirs.join(";"))
}

/// The shell id a preference resolves to. `auto` (the default) is Git Bash when Git for Windows
/// is installed, else PowerShell; an explicit Git Bash with no Git installed falls back the same.
pub fn resolve_shell(pref: &str, has_git_bash: bool) -> &'static str {
    match pref {
        "auto" | "gitbash" if has_git_bash => "gitbash",
        "auto" | "gitbash" | "powershell" => "powershell",
        "pwsh" => "pwsh",
        "cmd" => "cmd",
        "wsl" => "wsl",
        _ => "powershell",
    }
}

/// `wsl.exe -l -v` writes UTF-16LE to a pipe; anything else is read as UTF-8.
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    let looks_utf16 = bytes.len() >= 2 && bytes.len() % 2 == 0 && bytes.iter().skip(1).step_by(2).filter(|b| **b == 0).count() * 2 >= bytes.len() / 2;
    if looks_utf16 {
        let units: Vec<u16> = bytes.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        String::from_utf16_lossy(&units).trim_start_matches('\u{feff}').to_string()
    } else {
        String::from_utf8_lossy(bytes).into_owned()
    }
}

/// The default distro's WSL version from `wsl -l -v` (`* Ubuntu  Running  2`); the first
/// distro's when none is marked. None when no distro is listed.
pub fn parse_wsl_list(out: &str) -> Option<u8> {
    let mut first = None;
    for line in out.lines().skip(1) {
        let line = line.replace('\0', "");
        let default = line.trim_start().starts_with('*');
        let version = line.split_whitespace().last().and_then(|v| v.parse::<u8>().ok());
        let Some(v) = version.filter(|v| *v == 1 || *v == 2) else { continue };
        if default {
            return Some(v);
        }
        first.get_or_insert(v);
    }
    first
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::os::windows::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};
    use winreg::enums::{RegType, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE};
    use winreg::{RegKey, RegValue};

    /// A GUI app's console child opens a console window unless told not to.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn env(name: &str) -> Option<String> {
        std::env::var(name).ok().filter(|v| !v.is_empty())
    }

    fn lookup(name: &str) -> Option<String> {
        env(name)
    }

    fn registry_path(hive: winreg::HKEY, subkey: &str) -> Option<String> {
        RegKey::predef(hive).open_subkey(subkey).ok()?.get_value::<String, _>("Path").ok()
    }

    const MACHINE_ENV: &str = r"SYSTEM\CurrentControlSet\Control\Session Manager\Environment";

    /// The PATH a terminal opened now would get: machine then user, as stored. Falls back to this
    /// process's PATH when the registry can't be read.
    fn fresh_path_dirs() -> Vec<String> {
        let machine = registry_path(HKEY_LOCAL_MACHINE, MACHINE_ENV);
        let user = registry_path(HKEY_CURRENT_USER, "Environment");
        if machine.is_none() && user.is_none() {
            return split_path(&env("PATH").unwrap_or_default());
        }
        [machine, user]
            .into_iter()
            .flatten()
            .flat_map(|p| split_path(&p))
            .map(|d| expand_env(&d, &lookup))
            .collect()
    }

    pub fn places() -> Places {
        Places { path_dirs: fresh_path_dirs(), ..known_places() }
    }

    /// Without the PATH walk: what every spawn uses, where a file check per PATH folder could
    /// stall on an unreachable network folder. A Git found only through PATH is one Claude
    /// finds the same way, so a spawn needs no variable for it.
    fn known_places() -> Places {
        let mut program_files: Vec<String> = Vec::new();
        for v in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(pf) = env(v) {
                if !program_files.iter().any(|x| same_dir(x, &pf)) {
                    program_files.push(pf);
                }
            }
        }
        if program_files.is_empty() {
            program_files.push(r"C:\Program Files".into());
            program_files.push(r"C:\Program Files (x86)".into());
        }
        Places {
            program_files,
            local_app_data: env("LOCALAPPDATA"),
            app_data: env("APPDATA"),
            user_profile: env("USERPROFILE"),
            path_dirs: Vec::new(),
        }
    }

    /// Not `is_file`: WindowsApps' `Claude.exe` is an app-execution alias, a reparse point that
    /// following can fail on, and the alias existing is exactly what counts.
    fn exists(p: &str) -> bool {
        std::fs::symlink_metadata(p).is_ok_and(|m| !m.is_dir())
    }

    /// The last Git Bash a full search (with the PATH walk) found, so a spawn can use one that
    /// only PATH leads to without walking PATH itself.
    static SEARCHED: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

    fn remember(found: &Option<String>) {
        if let Ok(mut s) = SEARCHED.lock() {
            s.clone_from(found);
        }
    }

    /// The full search, PATH walk included: for the wizard's probe and Preferences' shell list,
    /// never a spawn.
    pub fn git_bash() -> Option<String> {
        let found = find_git_bash(&places(), &exists);
        remember(&found);
        found
    }

    /// For a spawn, which runs on the main thread: the known folders, then what the last full
    /// search found. No PATH walk — a file check per PATH folder can stall on an unreachable
    /// network folder, and this runs for every new tab.
    pub fn git_bash_for_spawn() -> Option<String> {
        find_git_bash(&known_places(), &exists)
            .or_else(|| SEARCHED.lock().ok().and_then(|s| s.clone()).filter(|p| exists(p)))
    }

    /// Run a console program without a window, giving up (and killing it) after `limit`.
    fn run_quiet(program: &str, args: &[&str], limit: Duration) -> Option<std::process::Output> {
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
            .ok()?;
        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => return child.wait_with_output().ok(),
                Ok(None) if started.elapsed() < limit => std::thread::sleep(Duration::from_millis(50)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }

    fn claude_version(path: &str) -> Option<String> {
        // A .cmd shim (npm) has to go through cmd; claude.exe runs directly.
        let out = if path.to_ascii_lowercase().ends_with(".cmd") {
            run_quiet("cmd.exe", &["/d", "/c", path, "--version"], Duration::from_secs(10))?
        } else {
            run_quiet(path, &["--version"], Duration::from_secs(10))?
        };
        // "2.1.301 (Claude Code)" — the wizard already says what it is.
        let text = String::from_utf8_lossy(&out.stdout);
        text.split_whitespace().next().map(String::from)
    }

    fn wsl_version() -> Option<u8> {
        let out = run_quiet("wsl.exe", &["-l", "-v"], Duration::from_secs(5))?;
        if !out.status.success() {
            return None;
        }
        parse_wsl_list(&decode_wsl_output(&out.stdout))
    }

    pub fn probe(shell_pref: &str) -> ClaudeSetupReport {
        let places = places();
        let git_bash = find_git_bash(&places, &exists);
        remember(&git_bash);
        let claude = find_claude(&places, &exists);
        let claude_version = claude.path.as_deref().and_then(claude_version);
        let winget = places
            .local_app_data
            .as_deref()
            .is_some_and(|lad| exists(&join(lad, r"Microsoft\WindowsApps\winget.exe")));
        ClaudeSetupReport {
            windows: true,
            shell: resolve_shell(shell_pref, git_bash.is_some()).to_string(),
            shell_pref: shell_pref.to_string(),
            git_bash,
            claude: claude.path,
            claude_version,
            claude_on_path: claude.on_path,
            claude_shadowed_by: claude.shadowed_by,
            wsl_version: wsl_version(),
            winget,
        }
    }

    /// What a new tab is handed on top of the environment it already gets (pty/manager.rs passes
    /// that tab's own PATH and whether it already has the Git Bash variable): `claude`'s folder
    /// on PATH ahead of any Claude Desktop alias, and Git Bash's location for Claude unless the
    /// user set one. No subprocesses and no PATH walk — this runs on every spawn.
    pub fn tab_env(tab_path_now: Option<&str>, has_git_bash_var: bool) -> Vec<(String, String)> {
        let places = known_places();
        let mut set = Vec::new();
        if let Some(dir) = find_claude(&places, &exists).path.as_deref().and_then(super::parent) {
            let shadows = places
                .local_app_data
                .as_deref()
                .is_some_and(|lad| exists(&join(lad, r"Microsoft\WindowsApps\claude.exe")));
            if let Some(path) = tab_path(tab_path_now.unwrap_or(""), dir, shadows) {
                set.push(("PATH".to_string(), path));
            }
        }
        if !has_git_bash_var {
            if let Some(bash) = find_git_bash(&places, &exists) {
                set.push(("CLAUDE_CODE_GIT_BASH_PATH".to_string(), bash));
            }
        }
        set
    }

    /// Append `claude`'s folder to the user's stored PATH (as REG_EXPAND_SZ, keeping every
    /// existing `%VAR%` entry as written) and tell running programs the environment changed, so
    /// a terminal opened from the Start menu sees it without signing out.
    pub fn add_claude_to_user_path() -> Result<(), String> {
        let places = places();
        let claude = find_claude(&places, &exists);
        let path = claude.path.ok_or("Claude Code isn't installed yet")?;
        let entry = path_entry_for(&path, places.user_profile.as_deref()).ok_or("Unexpected Claude location")?;
        let key = RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags("Environment", KEY_READ | KEY_WRITE)
            .map_err(|e| format!("Couldn't open your environment settings: {e}"))?;
        // Empty only when there is no user Path at all: any other read failure must not turn
        // into a PATH holding nothing but claude's folder.
        let current: String = match key.get_value("Path") {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("Couldn't read your PATH: {e}")),
        };
        let Some(new) = with_entry_added(&current, &entry, &lookup) else { return Ok(()) };
        let bytes: Vec<u8> = new.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
        key.set_raw_value("Path", &RegValue { bytes, vtype: RegType::REG_EXPAND_SZ })
            .map_err(|e| format!("Couldn't update your PATH: {e}"))?;
        broadcast_environment_change();
        Ok(())
    }

    fn broadcast_environment_change() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
        };
        let what: Vec<u16> = "Environment\0".encode_utf16().collect();
        let mut result = 0usize;
        unsafe {
            SendMessageTimeoutW(HWND_BROADCAST, WM_SETTINGCHANGE, 0, what.as_ptr() as isize, SMTO_ABORTIFHUNG, 5000, &mut result);
        }
    }
}

#[cfg(windows)]
pub use platform::{add_claude_to_user_path, git_bash, git_bash_for_spawn, tab_env};

#[tauri::command]
pub async fn detect_claude_setup(state: tauri::State<'_, std::sync::Arc<crate::state::AppState>>) -> Result<ClaudeSetupReport, String> {
    let shell_pref = state.app_data.read().preferences.windows_shell.clone();
    #[cfg(windows)]
    {
        // `claude --version` and `wsl -l -v` are subprocesses; keep them off the async runtime.
        tauri::async_runtime::spawn_blocking(move || platform::probe(&shell_pref))
            .await
            .map_err(|e| e.to_string())
    }
    #[cfg(not(windows))]
    {
        Ok(ClaudeSetupReport { shell_pref, ..Default::default() })
    }
}

/// Preferences' "Check Claude setup": the wizard opens tabs, so it lives in a terminal window —
/// the one the user was last in, or the main window when none has been focused since launch.
#[tauri::command]
pub fn open_claude_setup(app: tauri::AppHandle) {
    use tauri::Emitter;
    if !crate::emit_to_focused_window(&app, "open-claude-setup") {
        let _ = app.emit_to("main", "open-claude-setup", ());
    }
}

#[tauri::command]
pub async fn claude_setup_add_to_path() -> Result<(), String> {
    #[cfg(windows)]
    {
        tauri::async_runtime::spawn_blocking(platform::add_claude_to_user_path)
            .await
            .map_err(|e| e.to_string())?
    }
    #[cfg(not(windows))]
    {
        Err("Only on Windows".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    fn fs(files: &[&str]) -> impl Fn(&str) -> bool {
        let set: HashSet<String> = files.iter().map(|f| f.to_ascii_lowercase()).collect();
        move |p: &str| set.contains(&p.to_ascii_lowercase())
    }

    fn places(path_dirs: &[&str]) -> Places {
        Places {
            program_files: vec![r"C:\Program Files".into(), r"C:\Program Files (x86)".into()],
            local_app_data: Some(r"C:\Users\ann\AppData\Local".into()),
            app_data: Some(r"C:\Users\ann\AppData\Roaming".into()),
            user_profile: Some(r"C:\Users\ann".into()),
            path_dirs: path_dirs.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn git_bash_in_program_files_first() {
        let exists = fs(&[r"C:\Program Files\Git\bin\bash.exe", r"C:\Users\ann\AppData\Local\Programs\Git\bin\bash.exe"]);
        assert_eq!(find_git_bash(&places(&[]), &exists).as_deref(), Some(r"C:\Program Files\Git\bin\bash.exe"));
    }

    #[test]
    fn git_bash_found_through_git_on_path() {
        let exists = fs(&[r"D:\tools\Git\cmd\git.exe", r"D:\tools\Git\bin\bash.exe"]);
        assert_eq!(find_git_bash(&places(&[r"D:\tools\Git\cmd"]), &exists).as_deref(), Some(r"D:\tools\Git\bin\bash.exe"));
        let exists = fs(&[r"D:\Git\mingw64\bin\git.exe", r"D:\Git\bin\bash.exe"]);
        assert_eq!(find_git_bash(&places(&[r"D:\Git\mingw64\bin"]), &exists).as_deref(), Some(r"D:\Git\bin\bash.exe"));
    }

    #[test]
    fn git_bash_per_user_and_scoop_installs() {
        let exists = fs(&[r"C:\Users\ann\AppData\Local\Programs\Git\bin\bash.exe"]);
        assert_eq!(find_git_bash(&places(&[]), &exists).as_deref(), Some(r"C:\Users\ann\AppData\Local\Programs\Git\bin\bash.exe"));
        let exists = fs(&[r"C:\Users\ann\scoop\apps\git\current\bin\bash.exe"]);
        assert_eq!(find_git_bash(&places(&[]), &exists).as_deref(), Some(r"C:\Users\ann\scoop\apps\git\current\bin\bash.exe"));
        assert_eq!(find_git_bash(&places(&[]), &fs(&[])), None);
    }

    #[test]
    fn claude_off_path_after_native_install() {
        let exists = fs(&[r"C:\Users\ann\.local\bin\claude.exe"]);
        let f = find_claude(&places(&[r"C:\Windows\system32"]), &exists);
        assert_eq!(f.path.as_deref(), Some(r"C:\Users\ann\.local\bin\claude.exe"));
        assert!(!f.on_path);
        assert_eq!(f.shadowed_by, None);
    }

    #[test]
    fn claude_on_path_and_desktop_shadow() {
        let apps = r"C:\Users\ann\AppData\Local\Microsoft\WindowsApps";
        let exists = fs(&[&format!(r"{apps}\Claude.exe"), r"C:\Users\ann\.local\bin\claude.exe"]);
        let f = find_claude(&places(&[apps, r"C:\Users\ann\.local\bin"]), &exists);
        assert_eq!(f.path.as_deref(), Some(r"C:\Users\ann\.local\bin\claude.exe"));
        assert!(f.on_path);
        assert_eq!(f.shadowed_by.as_deref(), Some(&*format!(r"{apps}\claude.exe")));
        // After the real one: no shadow.
        let f = find_claude(&places(&[r"C:\Users\ann\.local\bin", apps]), &exists);
        assert_eq!(f.shadowed_by, None);
        // npm's shim counts as on PATH.
        let exists = fs(&[r"C:\Users\ann\AppData\Roaming\npm\claude.cmd"]);
        let f = find_claude(&places(&[r"C:\Users\ann\AppData\Roaming\npm"]), &exists);
        assert!(f.on_path);
    }

    #[test]
    fn path_entry_uses_userprofile() {
        assert_eq!(path_entry_for(r"C:\Users\ann\.local\bin\claude.exe", Some(r"C:\Users\ann")).as_deref(), Some(r"%USERPROFILE%\.local\bin"));
        assert_eq!(path_entry_for(r"C:\Users\annabel\x\claude.exe", Some(r"C:\Users\ann")).as_deref(), Some(r"C:\Users\annabel\x"));
        assert_eq!(path_entry_for(r"D:\bin\claude.exe", None).as_deref(), Some(r"D:\bin"));
    }

    #[test]
    fn adding_to_user_path_keeps_entries_and_is_idempotent() {
        let lookup = |n: &str| (n.eq_ignore_ascii_case("USERPROFILE")).then(|| r"C:\Users\ann".to_string());
        assert_eq!(
            with_entry_added(r"%USERPROFILE%\go\bin;C:\tools;", r"%USERPROFILE%\.local\bin", &lookup).as_deref(),
            Some(r"%USERPROFILE%\go\bin;C:\tools;%USERPROFILE%\.local\bin")
        );
        assert_eq!(with_entry_added("", r"%USERPROFILE%\.local\bin", &lookup).as_deref(), Some(r"%USERPROFILE%\.local\bin"));
        assert_eq!(with_entry_added(r"c:\users\ann\.local\bin\", r"%USERPROFILE%\.local\bin", &lookup), None);
        // Ahead of WindowsApps, where an older Claude Desktop's alias lives.
        assert_eq!(
            with_entry_added(r"C:\tools;%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;D:\x", r"%USERPROFILE%\.local\bin", &lookup).as_deref(),
            Some(r"C:\tools;%USERPROFILE%\.local\bin;%USERPROFILE%\AppData\Local\Microsoft\WindowsApps;D:\x")
        );
    }

    #[test]
    fn expand_env_leaves_unknown_names() {
        let lookup = |n: &str| (n == "A").then(|| "x".to_string());
        assert_eq!(expand_env("%A%\\%B%\\100%", &lookup), r"x\%B%\100%");
    }

    #[test]
    fn tab_path_adds_claude_ahead_of_windows_apps() {
        let bin = r"C:\Users\ann\.local\bin";
        let apps = r"C:\Users\ann\AppData\Local\Microsoft\WindowsApps";
        assert_eq!(tab_path(r"C:\Windows;C:\Mine", bin, false).as_deref(), Some(r"C:\Windows;C:\Mine;C:\Users\ann\.local\bin"));
        assert_eq!(
            tab_path(&format!(r"C:\Windows;{apps};C:\Mine"), bin, false).as_deref(),
            Some(&*format!(r"C:\Windows;{bin};{apps};C:\Mine"))
        );
        // Already there: left alone, unless Desktop's alias in front of it answers `claude`.
        assert_eq!(tab_path(&format!(r"{apps};c:\users\ann\.local\bin\"), bin, false), None);
        assert_eq!(
            tab_path(&format!(r"C:\Windows;{apps};c:\users\ann\.local\bin\;C:\Mine"), bin, true).as_deref(),
            Some(&*format!(r"C:\Windows;c:\users\ann\.local\bin\;{apps};C:\Mine"))
        );
        assert_eq!(tab_path(&format!(r"{bin};{apps}"), bin, true), None);
    }

    #[test]
    fn shell_resolution() {
        assert_eq!(resolve_shell("auto", true), "gitbash");
        assert_eq!(resolve_shell("auto", false), "powershell");
        assert_eq!(resolve_shell("gitbash", false), "powershell");
        assert_eq!(resolve_shell("powershell", true), "powershell");
        assert_eq!(resolve_shell("pwsh", true), "pwsh");
        assert_eq!(resolve_shell("nonsense", true), "powershell");
    }

    #[test]
    fn wsl_list_parsing() {
        let utf16: Vec<u8> = "  NAME      STATE           VERSION\r\n  Debian    Stopped         1\r\n* Ubuntu    Running         2\r\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let text = decode_wsl_output(&utf16);
        assert_eq!(parse_wsl_list(&text), Some(2));
        assert_eq!(parse_wsl_list("  NAME STATE VERSION\n  Old Stopped 1\n"), Some(1));
        assert_eq!(parse_wsl_list("Windows Subsystem for Linux has no installed distributions.\n"), None);
    }
}
