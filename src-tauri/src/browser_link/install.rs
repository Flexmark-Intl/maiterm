//! Installing the native-messaging host manifests (docs/browser-link.md §2, §5), and the folder
//! "Load unpacked" points Chrome at.

use std::path::PathBuf;
use tauri::{AppHandle, Manager};

use super::ALLOWED_EXTENSION_IDS;

/// Our host's name in each browser. Dev and release are separate hosts, so both can be linked.
pub fn host_name() -> &'static str {
    if cfg!(debug_assertions) {
        "com.maiterm.browser_dev"
    } else {
        "com.maiterm.browser"
    }
}

/// Each Chromium browser's user-data root, by name. A manifest is written only into a root that
/// exists, i.e. a browser that is installed and has been run.
#[cfg(target_os = "macos")]
fn browser_roots() -> Vec<(&'static str, PathBuf)> {
    let Some(base) = dirs::data_dir() else { return Vec::new() };
    [
        ("Chrome", "Google/Chrome"),
        ("Chrome Beta", "Google/Chrome Beta"),
        ("Chrome Dev", "Google/Chrome Dev"),
        ("Chrome Canary", "Google/Chrome Canary"),
        ("Chromium", "Chromium"),
        ("Brave", "BraveSoftware/Brave-Browser"),
        ("Edge", "Microsoft Edge"),
        ("Vivaldi", "Vivaldi"),
        ("Arc", "Arc/User Data"),
    ]
    .into_iter()
    .map(|(n, p)| (n, base.join(p)))
    .collect()
}

#[cfg(target_os = "linux")]
fn browser_roots() -> Vec<(&'static str, PathBuf)> {
    let Some(base) = dirs::config_dir() else { return Vec::new() };
    [
        ("Chrome", "google-chrome"),
        ("Chrome Beta", "google-chrome-beta"),
        ("Chrome Dev", "google-chrome-unstable"),
        ("Chromium", "chromium"),
        ("Brave", "BraveSoftware/Brave-Browser"),
        ("Edge", "microsoft-edge"),
        ("Vivaldi", "vivaldi"),
    ]
    .into_iter()
    .map(|(n, p)| (n, base.join(p)))
    .collect()
}

/// On Windows a manifest can live anywhere; the browser finds it through a registry key.
#[cfg(windows)]
const WINDOWS_REG_ROOTS: [(&str, &str); 4] = [
    ("Chrome", r"Software\Google\Chrome"),
    ("Chromium", r"Software\Chromium"),
    ("Brave", r"Software\BraveSoftware\Brave-Browser"),
    ("Edge", r"Software\Microsoft\Edge"),
];

fn manifest_json() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let origins: Vec<String> = ALLOWED_EXTENSION_IDS.iter().map(|id| format!("chrome-extension://{id}/")).collect();
    Some(
        serde_json::to_string_pretty(&serde_json::json!({
            "name": host_name(),
            "description": "maiTerm browser link: tells the maiTerm extension where maiTerm is listening",
            "path": exe,
            "type": "stdio",
            "allowed_origins": origins,
        }))
        .ok()?,
    )
}

/// Has this browser actually run here? Its root alone proves nothing: other apps' native hosts
/// (1Password, Dropbox) create `<root>/NativeMessagingHosts` for every browser they support.
/// `Local State` is written by the browser itself on first launch.
#[cfg(not(windows))]
fn browser_has_run(root: &std::path::Path) -> bool {
    root.join("Local State").is_file()
}

/// Write (or, with `enabled` false, remove) our host manifest for every installed browser.
/// Rewritten at each launch while enabled, so a moved or updated app heals itself.
pub fn sync(enabled: bool) {
    #[cfg(not(windows))]
    {
        let manifest = if enabled { manifest_json() } else { None };
        for (name, root) in browser_roots() {
            if !root.is_dir() {
                continue;
            }
            let path = root.join("NativeMessagingHosts").join(format!("{}.json", host_name()));
            let manifest = manifest.as_ref().filter(|_| browser_has_run(&root));
            let r = match manifest {
                Some(m) => std::fs::create_dir_all(path.parent().unwrap()).and_then(|_| std::fs::write(&path, m)),
                None if path.exists() => std::fs::remove_file(&path),
                None => Ok(()),
            };
            if let Err(e) = r {
                log::warn!("[browser-link] {name}: could not update {}: {e}", path.display());
            }
        }
    }
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let Some(dir) = dirs::data_dir().map(|p| p.join(crate::state::persistence::app_data_slug())) else { return };
        let path = dir.join(format!("{}.json", host_name()));
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        if enabled {
            let Some(m) = manifest_json() else { return };
            if let Err(e) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(&path, m)) {
                log::warn!("[browser-link] could not write {}: {e}", path.display());
                return;
            }
        } else {
            let _ = std::fs::remove_file(&path);
        }
        for (name, key) in WINDOWS_REG_ROOTS {
            let sub = format!(r"{key}\NativeMessagingHosts\{}", host_name());
            let r = if enabled {
                hkcu.create_subkey(&sub).and_then(|(k, _)| k.set_value("", &path.to_string_lossy().to_string()))
            } else {
                hkcu.delete_subkey_all(&sub).or(Ok(()))
            };
            if let Err(e) = r {
                log::warn!("[browser-link] {name}: registry update failed: {e}");
            }
        }
    }
}

/// The browsers our manifest is installed in, for the Preferences pane.
pub fn installed_in() -> Vec<String> {
    #[cfg(not(windows))]
    {
        browser_roots()
            .into_iter()
            .filter(|(_, root)| browser_has_run(root))
            .filter(|(_, root)| root.join("NativeMessagingHosts").join(format!("{}.json", host_name())).is_file())
            .map(|(n, _)| n.to_string())
            .collect()
    }
    #[cfg(windows)]
    {
        use winreg::enums::HKEY_CURRENT_USER;
        use winreg::RegKey;
        let hkcu = RegKey::predef(HKEY_CURRENT_USER);
        WINDOWS_REG_ROOTS
            .iter()
            .filter(|(_, key)| hkcu.open_subkey(format!(r"{key}\NativeMessagingHosts\{}", host_name())).is_ok())
            .map(|(n, _)| n.to_string())
            .collect()
    }
}

/// The extension folder "Load unpacked" points Chrome at. A dev build uses the repo's own, so an
/// edit is one reload away; a release copies the bundled one into its data dir, a path that
/// survives app updates (Chrome remembers the folder, not the files).
pub fn extension_dir(app: &AppHandle) -> Option<PathBuf> {
    if cfg!(debug_assertions) {
        return PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../chrome-extension").canonicalize().ok();
    }
    let _ = app;
    dirs::data_dir().map(|p| p.join(crate::state::persistence::app_data_slug()).join("chrome-extension"))
}

/// Refresh the release build's copy of the extension from the app bundle.
pub fn refresh_unpacked_copy(app: &AppHandle) -> Result<PathBuf, String> {
    let dest = extension_dir(app).ok_or("No app data folder")?;
    if cfg!(debug_assertions) {
        return Ok(dest);
    }
    let src = app
        .path()
        .resource_dir()
        .map_err(|e| e.to_string())?
        .join("chrome-extension");
    copy_dir(&src, &dest).map_err(|e| format!("Could not copy the extension: {e}"))?;
    Ok(dest)
}

fn copy_dir(src: &std::path::Path, dest: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}
