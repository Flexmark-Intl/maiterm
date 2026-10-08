//! The `maiterm-tab` Claude Code mod (a plugin of function hooks): maiTerm's link to the Claude
//! session running in a tab.
//!
//! The mod's files are compiled into the binary and written to `<data dir>/claude-mod/` at
//! startup; every tab's shell gets that folder in `CLAUDE_CODE_PLUGIN_DIRS`, so any `claude`
//! started there loads it. It forwards the hook events the settings.json hooks send
//! (`lockfile::build_our_hooks`) to `/hooks` with `via=mod` and the tab's id. The server then
//! drops the settings hooks' anonymous copy of each event for that session, and the
//! SessionStart/SessionEnd command hooks stand down when the mod has set `MAITERM_VIA_MOD`.
//!
//! A Claude Code too old for mods ignores the variable, and its settings hooks carry on
//! exactly as before — that fallback is why both paths exist.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::state::persistence::app_data_slug;

/// Set once, by `install` at startup: the folder tabs are pointed at, or `None` when it could
/// not be written (and then no tab is).
static INSTALLED: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The installed mod's folder, for a tab's `CLAUDE_CODE_PLUGIN_DIRS`.
pub fn installed() -> Option<&'static Path> {
    INSTALLED.get().and_then(|dir| dir.as_deref())
}

/// The plugin's folder name, which is also its `name` in plugin.json.
const PLUGIN: &str = "maiterm-tab";

/// (path inside the plugin folder, contents)
const FILES: &[(&str, &str)] = &[
    (
        ".claude-plugin/plugin.json",
        include_str!("../../resources/claude-mod/maiterm-tab/.claude-plugin/plugin.json"),
    ),
    (
        "hooks/hooks.json",
        include_str!("../../resources/claude-mod/maiterm-tab/hooks/hooks.json"),
    ),
    (
        "hooks/register.ts",
        include_str!("../../resources/claude-mod/maiterm-tab/hooks/register.ts"),
    ),
];

/// Where this build installs the mod. Per build flavor (`app_data_slug`), so a dev maiTerm and
/// the installed one never overwrite each other's copy.
pub fn plugin_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join(app_data_slug()).join("claude-mod").join(PLUGIN))
}

/// Writes the mod's files where `plugin_dir` says, rewriting only those that changed: a running
/// Claude watches the folder and reloads the module on every write. Records the folder for
/// `installed` when every file is in place; on failure no tab is pointed at it.
pub fn install() {
    let dir = plugin_dir().and_then(|dir| match write_files(&dir) {
        Ok(()) => {
            log::info!("claude mod: installed at {}", dir.display());
            Some(dir)
        }
        Err(e) => {
            log::warn!("claude mod: could not install into {}: {}", dir.display(), e);
            None
        }
    });
    let _ = INSTALLED.set(dir);
}

fn write_files(dir: &Path) -> std::io::Result<()> {
    for (rel, contents) in FILES {
        let path = dir.join(rel);
        if std::fs::read_to_string(&path).ok().as_deref() == Some(*contents) {
            continue;
        }
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, contents)?;
    }
    Ok(())
}

/// The `CLAUDE_CODE_PLUGIN_DIRS` value for a tab: `ours` (when this tab gets the mod) ahead of
/// the folders this process inherited, so a user's own setting is kept. Every inherited copy of
/// this mod is dropped, whichever build installed it: a dev maiTerm started from a prod tab
/// inherits prod's folder, and two plugins of one name load the first and warn about the
/// second in every session; with the mod off here, prod's copy would still load and send.
/// `None` when nothing is left, and then the variable is removed.
pub fn plugin_dirs_env(ours: Option<&Path>, inherited: Option<&str>) -> Option<String> {
    let suffix = format!("/claude-mod/{}", PLUGIN);
    let mut dirs: Vec<String> = ours.map(|d| d.to_string_lossy().into_owned()).into_iter().collect();
    dirs.extend(
        inherited
            .unwrap_or("")
            .split(':')
            .filter(|p| !p.is_empty() && !p.trim_end_matches('/').ends_with(&suffix))
            .map(String::from),
    );
    (!dirs.is_empty()).then(|| dirs.join(":"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_writes_every_file_and_leaves_unchanged_ones_alone() {
        let root = std::env::temp_dir().join(format!("maiterm-claude-mod-test-{}", uuid::Uuid::new_v4()));
        let dir = root.join(PLUGIN);
        write_files(&dir).unwrap();
        for (rel, contents) in FILES {
            assert_eq!(std::fs::read_to_string(dir.join(rel)).unwrap(), *contents);
        }
        let module = dir.join("hooks/register.ts");
        let before = std::fs::metadata(&module).unwrap().modified().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        write_files(&dir).unwrap();
        assert_eq!(std::fs::metadata(&module).unwrap().modified().unwrap(), before);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plugin_dirs_env_keeps_the_users_folders_and_no_other_copy_of_the_mod() {
        let dev = Path::new("/data/com.aiterm.dev/claude-mod/maiterm-tab");
        let prod = "/data/com.aiterm.app/claude-mod/maiterm-tab";
        let dev_s = dev.to_str().unwrap();
        assert_eq!(plugin_dirs_env(Some(dev), None).as_deref(), Some(dev_s));
        assert_eq!(plugin_dirs_env(Some(dev), Some("")).as_deref(), Some(dev_s));
        assert_eq!(plugin_dirs_env(Some(dev), Some("/mine")), Some(format!("{dev_s}:/mine")));
        // Another build's copy (or our own, inherited) is dropped, the user's folder kept.
        assert_eq!(
            plugin_dirs_env(Some(dev), Some(&format!("/mine:{prod}:{dev_s}/"))),
            Some(format!("{dev_s}:/mine"))
        );
        // Mod off for this tab: an inherited copy must not load either.
        assert_eq!(plugin_dirs_env(None, Some(prod)), None);
        assert_eq!(plugin_dirs_env(None, Some(&format!("{prod}:/mine"))).as_deref(), Some("/mine"));
        assert_eq!(plugin_dirs_env(None, None), None);
    }
}
