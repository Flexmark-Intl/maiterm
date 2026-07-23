use crate::state::persistence::save_state;
use crate::state::{AppState, EditorFileInfo, FileWatcherHandle, RemoteFileWatch, Tab};
use base64::Engine;
use std::io::Read;
use std::path::Path;
use std::sync::Arc;
use tauri::{command, AppHandle, Emitter, State, Window};

fn expand_tilde(path: &str) -> String {
    if path == "~" {
        return dirs::home_dir()
            .map(|h| h.to_string_lossy().to_string())
            .unwrap_or_else(|| path.to_string());
    }
    if path.starts_with("~/") {
        if let Some(home) = dirs::home_dir() {
            return format!("{}/{}", home.to_string_lossy(), &path[2..]);
        }
    }
    path.to_string()
}

#[derive(serde::Serialize)]
pub struct ReadFileResult {
    pub content: String,
    pub size: u64,
}

#[command]
pub async fn read_file(path: String) -> Result<ReadFileResult, String> {
    let path = expand_tilde(&path);
    let metadata = std::fs::metadata(&path).map_err(|e| format!("Cannot access file: {}", e))?;

    if metadata.is_dir() {
        return Err("IS_DIRECTORY".to_string());
    }

    let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
    if metadata.len() > 2 * 1024 * 1024 {
        return Err(format!("FILE_TOO_LARGE:{:.1}", size_mb));
    }

    let mut file = std::fs::File::open(&path).map_err(|e| format!("Cannot open file: {}", e))?;

    // Check for binary content (null bytes in first 8KB)
    let mut header = vec![0u8; 8192.min(metadata.len() as usize)];
    let n = file
        .read(&mut header)
        .map_err(|e| format!("Cannot read file: {}", e))?;
    if header[..n].contains(&0) {
        return Err("Binary files are not supported".to_string());
    }

    // Read entire file
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("Cannot read file: {}", e))?;

    Ok(ReadFileResult {
        size: metadata.len(),
        content,
    })
}

#[command]
pub async fn write_file(path: String, content: String) -> Result<(), String> {
    let path = expand_tilde(&path);
    // Atomic write: temp file + rename
    let temp_path = format!("{}.aiterm-tmp", path);
    std::fs::write(&temp_path, &content).map_err(|e| format!("Cannot write file: {}", e))?;
    std::fs::rename(&temp_path, &path).map_err(|e| {
        // Clean up temp file on rename failure
        let _ = std::fs::remove_file(&temp_path);
        format!("Cannot save file: {}", e)
    })?;

    Ok(())
}

#[derive(serde::Serialize)]
pub struct ReadFileBase64Result {
    pub data: String,
    pub size: u64,
}

#[command]
pub async fn read_file_base64(path: String) -> Result<ReadFileBase64Result, String> {
    let path = expand_tilde(&path);
    let metadata = std::fs::metadata(&path).map_err(|e| format!("Cannot access file: {}", e))?;

    if metadata.is_dir() {
        return Err("IS_DIRECTORY".to_string());
    }

    if metadata.len() > 20 * 1024 * 1024 {
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        return Err(format!("FILE_TOO_LARGE:{:.1}", size_mb));
    }

    let bytes = std::fs::read(&path).map_err(|e| format!("Cannot read file: {}", e))?;
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(ReadFileBase64Result {
        size: metadata.len(),
        data,
    })
}

#[command]
pub async fn scp_read_file_base64(
    ssh_command: String,
    remote_path: String,
) -> Result<ReadFileBase64Result, String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);

    // Download via SCP
    let temp_dir = std::env::temp_dir();
    let temp_name = format!("aiterm-scp-{}", uuid::Uuid::new_v4());
    let local_path = temp_dir.join(&temp_name);

    let output = std::process::Command::new("scp")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=10")
        .arg(format!("{}:{}", user_host, remote_path))
        .arg(local_path.to_str().unwrap())
        .output()
        .map_err(|e| format!("Failed to run scp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SCP download failed: {}", stderr.trim()));
    }

    let metadata = std::fs::metadata(&local_path).map_err(|e| format!("Cannot stat file: {}", e))?;
    if metadata.len() > 20 * 1024 * 1024 {
        let _ = std::fs::remove_file(&local_path);
        let size_mb = metadata.len() as f64 / (1024.0 * 1024.0);
        return Err(format!("FILE_TOO_LARGE:{:.1}", size_mb));
    }

    let bytes = std::fs::read(&local_path).map_err(|e| format!("Cannot read file: {}", e))?;
    let _ = std::fs::remove_file(&local_path);
    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);

    Ok(ReadFileBase64Result {
        size: metadata.len(),
        data,
    })
}

#[command]
pub async fn scp_read_file(
    ssh_command: String,
    remote_path: String,
) -> Result<ReadFileResult, String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);

    // Pre-check via SSH: file type, size, and binary detection in one command
    // stat -c on Linux, stat -f on macOS — use a portable approach
    let check_cmd = format!(
        "f={}; t=$(stat -c %F \"$f\" 2>/dev/null || stat -f %HT \"$f\" 2>/dev/null); s=$(stat -c %s \"$f\" 2>/dev/null || stat -f %z \"$f\" 2>/dev/null); b=$(head -c 8192 \"$f\" | tr -d '\\0' | wc -c); h=$(head -c 8192 \"$f\" | wc -c); echo \"$t|$s|$b|$h\"",
        shell_quote(&remote_path)
    );

    let check_output = std::process::Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=10")
        .arg(&user_host)
        .arg(&check_cmd)
        .output()
        .map_err(|e| format!("Failed to run ssh: {}", e))?;

    if !check_output.status.success() {
        let stderr = String::from_utf8_lossy(&check_output.stderr);
        return Err(format!("Cannot access remote file: {}", stderr.trim()));
    }

    let info = String::from_utf8_lossy(&check_output.stdout).trim().to_string();
    let parts: Vec<&str> = info.split('|').collect();
    if parts.len() >= 4 {
        let file_type = parts[0].to_lowercase();
        // Check for directory
        if file_type.contains("directory") || file_type.contains("dir") {
            return Err("IS_DIRECTORY".to_string());
        }
        // Check file size
        if let Ok(size) = parts[1].trim().parse::<u64>() {
            if size > 2 * 1024 * 1024 {
                let size_mb = size as f64 / (1024.0 * 1024.0);
                return Err(format!("FILE_TOO_LARGE:{:.1}", size_mb));
            }
        }
        // Check for binary: compare byte count with and without null bytes stripped
        let stripped: u64 = parts[2].trim().parse().unwrap_or(0);
        let original: u64 = parts[3].trim().parse().unwrap_or(0);
        if original > 0 && stripped < original {
            return Err("Binary files are not supported".to_string());
        }
    }

    // Pre-checks passed — download via SCP
    let temp_dir = std::env::temp_dir();
    let temp_name = format!("aiterm-scp-{}", uuid::Uuid::new_v4());
    let local_path = temp_dir.join(&temp_name);

    let output = std::process::Command::new("scp")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=10")
        .arg(format!("{}:{}", user_host, remote_path))
        .arg(local_path.to_str().unwrap())
        .output()
        .map_err(|e| format!("Failed to run scp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SCP download failed: {}", stderr.trim()));
    }

    let content = std::fs::read_to_string(&local_path)
        .map_err(|e| format!("Cannot read downloaded file: {}", e))?;
    let size = std::fs::metadata(&local_path).map(|m| m.len()).unwrap_or(0);

    let _ = std::fs::remove_file(&local_path);

    Ok(ReadFileResult { content, size })
}

#[command]
pub async fn scp_write_file(
    ssh_command: String,
    remote_path: String,
    content: String,
) -> Result<(), String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);

    // Write content to temp file
    let temp_dir = std::env::temp_dir();
    let temp_name = format!("aiterm-scp-{}", uuid::Uuid::new_v4());
    let local_path = temp_dir.join(&temp_name);

    std::fs::write(&local_path, &content).map_err(|e| format!("Cannot write temp file: {}", e))?;

    // Run scp to upload
    let output = std::process::Command::new("scp")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=10")
        .arg(local_path.to_str().unwrap())
        .arg(format!("{}:{}", user_host, remote_path))
        .output()
        .map_err(|e| format!("Failed to run scp: {}", e))?;

    // Clean up temp file
    let _ = std::fs::remove_file(&local_path);

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SCP upload failed: {}", stderr.trim()));
    }

    Ok(())
}

#[command]
pub async fn save_clipboard_image(
    data_base64: String,
    ext: Option<String>,
) -> Result<String, String> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&data_base64)
        .map_err(|e| format!("Invalid base64: {}", e))?;

    // Opaque clipboard images are encoded JPEG; transparent ones stay PNG.
    let ext = ext.unwrap_or_else(|| "png".to_string());
    let temp_dir = std::env::temp_dir();
    let filename = format!("aiterm-clipboard-{}.{}", uuid::Uuid::new_v4(), ext);
    let path = temp_dir.join(&filename);

    std::fs::write(&path, &bytes).map_err(|e| format!("Cannot write temp file: {}", e))?;
    log::info!("save_clipboard_image: wrote {} bytes to {:?}", bytes.len(), path);

    Ok(path.to_string_lossy().to_string())
}

/// Return a non-colliding path inside `dir` for `name`, appending " (1)", " (2)", …
/// before the extension if a file of that name already exists.
fn unique_path(dir: &Path, name: &str) -> std::path::PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| name.to_string());
    let ext = p
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let candidate = dir.join(format!("{} ({}){}", stem, i, ext));
        if !candidate.exists() {
            return candidate;
        }
    }
    first
}

/// Reveal a local file in the OS file manager (Finder / Explorer / file
/// browser), selecting the file itself where the platform supports it.
#[command]
pub async fn reveal_in_file_manager(path: String) -> Result<(), String> {
    let path = expand_tilde(&path);
    let p = Path::new(&path);
    if !p.exists() {
        return Err(format!("File does not exist: {}", path));
    }

    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("-R")
            .arg(&path)
            .spawn()
            .map_err(|e| format!("Failed to reveal in Finder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        // `explorer /select,<path>` selects the file. Explorer returns a
        // non-zero exit code even on success, so we spawn and don't check status.
        std::process::Command::new("explorer")
            .arg(format!("/select,{}", path))
            .spawn()
            .map_err(|e| format!("Failed to reveal in Explorer: {}", e))?;
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    {
        // Prefer the freedesktop FileManager1 interface (selects the file in
        // most Linux file managers); fall back to opening the parent directory.
        let uri = format!("file://{}", path);
        let dbus_ok = std::process::Command::new("dbus-send")
            .args([
                "--session",
                "--dest=org.freedesktop.FileManager1",
                "--type=method_call",
                "/org/freedesktop/FileManager1",
                "org.freedesktop.FileManager1.ShowItems",
                &format!("array:string:{}", uri),
                "string:",
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !dbus_ok {
            let parent = p
                .parent()
                .map(|x| x.to_path_buf())
                .unwrap_or_else(|| p.to_path_buf());
            std::process::Command::new("xdg-open")
                .arg(parent)
                .spawn()
                .map_err(|e| format!("Failed to open file manager: {}", e))?;
        }
    }

    Ok(())
}

/// Stage a remote file to a stable local temp path and return that path.
///
/// Backs the "Copy Local Copy Path" tab action: opening an SSH file reads its
/// bytes into the editor and deletes the transfer temp immediately (see
/// `scp_read_file`), so there is no lingering local file to point at. This
/// re-materializes the current remote-saved file on demand under
/// `<temp>/maiterm-ssh/<host+path hash>/<basename>` — a subdir keyed by
/// (host, path) so same-named files don't collide and repeat clicks return the
/// same path.
#[command]
pub async fn stage_remote_file_temp(
    ssh_command: String,
    remote_path: String,
) -> Result<String, String> {
    use std::hash::{Hash, Hasher};

    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    user_host.hash(&mut hasher);
    remote_path.hash(&mut hasher);
    let key = format!("{:016x}", hasher.finish());

    let dir = std::env::temp_dir().join("maiterm-ssh").join(&key);
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("Cannot create temp directory: {}", e))?;

    let basename = Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "file".to_string());
    let dest = dir.join(&basename);

    let output = std::process::Command::new("scp")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=10")
        .arg(format!("{}:{}", user_host, remote_path))
        .arg(dest.to_str().ok_or("Invalid destination path")?)
        .output()
        .map_err(|e| format!("Failed to run scp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Staging failed: {}", stderr.trim()));
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Download a remote file over SCP into the local Downloads directory.
/// Returns the absolute local path of the saved copy.
#[command]
pub async fn download_remote_file(
    ssh_command: String,
    remote_path: String,
) -> Result<String, String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);

    let downloads = dirs::download_dir()
        .ok_or_else(|| "Could not locate the Downloads directory".to_string())?;
    std::fs::create_dir_all(&downloads)
        .map_err(|e| format!("Cannot create Downloads directory: {}", e))?;

    let basename = Path::new(&remote_path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "download".to_string());
    let dest = unique_path(&downloads, &basename);

    let output = std::process::Command::new("scp")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=10")
        .arg(format!("{}:{}", user_host, remote_path))
        .arg(dest.to_str().ok_or("Invalid destination path")?)
        .output()
        .map_err(|e| format!("Failed to run scp: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Download failed: {}", stderr.trim()));
    }

    Ok(dest.to_string_lossy().to_string())
}

/// Progress frame emitted as `scp-progress-{upload_id}` during an upload.
#[derive(Clone, serde::Serialize)]
struct ScpProgress {
    upload_id: String,
    bytes_sent: u64,
    total_bytes: u64,
    percent: f64,
    rate_bps: f64,
    files_total: usize,
    done: bool,
    /// True when we can't poll the remote size (e.g. non-GNU `du`) — the UI
    /// should show an indeterminate spinner instead of a percentage.
    indeterminate: bool,
}

/// Sum the byte size of a local path, recursing into directories.
fn local_path_size(p: &Path) -> u64 {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => {
            let mut total = 0u64;
            if let Ok(entries) = std::fs::read_dir(p) {
                for entry in entries.flatten() {
                    total += local_path_size(&entry.path());
                }
            }
            total
        }
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

/// Total apparent byte size of the specific destination paths via `du -scb`
/// (GNU coreutils). We measure only the files/dirs being uploaded — never the
/// whole destination directory, which could be the user's (potentially huge)
/// CWD. Reuses the upload's ControlMaster connection when available so each poll
/// is a cheap multiplexed round-trip rather than a fresh SSH login. Returns None
/// when the remote lacks `du -scb` (e.g. BSD/macOS) — the caller then falls back
/// to an indeterminate progress display. Missing targets (not yet transferred)
/// count as 0, so this grows from 0 to the upload total.
fn remote_targets_size(user_host: &str, control_path: Option<&str>, targets: &[String]) -> Option<u64> {
    if targets.is_empty() {
        return Some(0);
    }
    let quoted: Vec<String> = targets.iter().map(|t| shell_quote(t)).collect();
    let mut cmd = std::process::Command::new("ssh");
    cmd.arg("-o").arg("BatchMode=yes").arg("-o").arg("ConnectTimeout=10");
    if let Some(sock) = control_path {
        cmd.arg("-o").arg(format!("ControlPath={}", sock));
    }
    cmd.arg(user_host);
    // `-s` summarize, `-c` grand total (last line), `-b` apparent bytes.
    cmd.arg(format!("du -scb -- {} 2>/dev/null | tail -1 | cut -f1", quoted.join(" ")));
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .lines()
        .last()?
        .trim()
        .parse::<u64>()
        .ok()
}

/// Upload local files/dirs to a remote directory over SCP, emitting live
/// progress and honouring a cooperative cancel flag.
///
/// Runs `scp` as a tracked child process (not blocking `.output()`), so we can
/// (a) poll the growing remote file size and emit `scp-progress-{upload_id}`
/// events, and (b) kill the transfer when the user clicks Cancel. A dedicated
/// SSH ControlMaster is opened for the duration so the mkdir, the scp, and every
/// size poll all share one authenticated connection.
#[command]
pub async fn scp_upload_files(
    state: State<'_, Arc<AppState>>,
    app: AppHandle,
    ssh_command: String,
    local_paths: Vec<String>,
    remote_dir: String,
    upload_id: String,
) -> Result<(), String> {
    let cancel_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    state
        .scp_uploads
        .write()
        .insert(upload_id.clone(), cancel_flag.clone());
    let app_state = state.inner().clone();

    // The transfer is long-running and blocking; run it off the async executor.
    tauri::async_runtime::spawn_blocking(move || {
        run_scp_upload(
            &app,
            &app_state,
            &upload_id,
            &ssh_command,
            &local_paths,
            &remote_dir,
            &cancel_flag,
        )
    })
    .await
    .map_err(|e| format!("Upload task failed to run: {}", e))?
}

/// Request cancellation of an in-flight upload. Sets the cooperative cancel flag;
/// the upload loop notices within one poll interval and kills the `scp` child.
#[command]
pub async fn cancel_scp_upload(
    state: State<'_, Arc<AppState>>,
    upload_id: String,
) -> Result<(), String> {
    if let Some(flag) = state.scp_uploads.read().get(&upload_id) {
        flag.store(true, std::sync::atomic::Ordering::SeqCst);
        log::info!("cancel_scp_upload: requested cancel for {}", upload_id);
    } else {
        log::warn!("cancel_scp_upload: no active upload {}", upload_id);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn run_scp_upload(
    app: &AppHandle,
    app_state: &Arc<AppState>,
    upload_id: &str,
    ssh_command: &str,
    local_paths: &[String],
    remote_dir: &str,
    cancel_flag: &Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    let event_name = format!("scp-progress-{}", upload_id);

    // Always drop the cancel flag from the registry when we leave, however we leave.
    struct RegistryGuard<'a> {
        state: &'a Arc<AppState>,
        id: &'a str,
    }
    impl Drop for RegistryGuard<'_> {
        fn drop(&mut self) {
            self.state.scp_uploads.write().remove(self.id);
        }
    }
    let _registry_guard = RegistryGuard { state: app_state, id: upload_id };

    let remote_dir = remote_dir.trim().to_string();
    let user_host = extract_user_host(ssh_command)?;
    let remote_dir = expand_remote_tilde(&user_host, &remote_dir);
    log::info!(
        "scp_upload_files[{}]: user_host={:?}, remote_dir={:?}, paths={:?}",
        upload_id, user_host, remote_dir, local_paths
    );

    let total_bytes: u64 = local_paths.iter().map(|p| local_path_size(Path::new(p))).sum();
    let files_total = local_paths.len();
    let needs_recursive = local_paths
        .iter()
        .any(|p| std::fs::metadata(p).map(|m| m.is_dir()).unwrap_or(false));

    // The remote destination paths scp will create (remote_dir/<basename>), used
    // for bounded size polling and for partial-file cleanup on cancel.
    let remote_base = remote_dir.trim_end_matches('/').to_string();
    let targets: Vec<String> = local_paths
        .iter()
        .filter_map(|p| Path::new(p).file_name().map(|n| n.to_string_lossy().to_string()))
        .map(|n| format!("{}/{}", remote_base, n))
        .collect();

    // --- ControlMaster: one authenticated connection reused by mkdir/scp/polls. ---
    let sanitized: String = upload_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .take(12)
        .collect();
    let control_sock = format!("/tmp/aiterm-scp-{}.sock", sanitized);
    let master_ok = match std::process::Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=15")
        .arg("-o").arg("ControlMaster=yes")
        .arg("-o").arg(format!("ControlPath={}", control_sock))
        .arg("-o").arg("ControlPersist=120")
        .arg("-N").arg("-f")
        .arg(&user_host)
        .status()
    {
        Ok(s) if s.success() => {
            log::info!("scp_upload_files[{}]: control master up at {}", upload_id, control_sock);
            true
        }
        other => {
            log::warn!(
                "scp_upload_files[{}]: control master unavailable ({:?}); using per-command connections",
                upload_id, other
            );
            false
        }
    };
    let control_path: Option<&str> = if master_ok { Some(control_sock.as_str()) } else { None };

    // Tear the master down on the way out.
    struct MasterGuard<'a> {
        user_host: &'a str,
        sock: &'a str,
        active: bool,
    }
    impl Drop for MasterGuard<'_> {
        fn drop(&mut self) {
            if self.active {
                let _ = std::process::Command::new("ssh")
                    .arg("-o").arg(format!("ControlPath={}", self.sock))
                    .arg("-O").arg("exit")
                    .arg(self.user_host)
                    .output();
            }
        }
    }
    let _master_guard = MasterGuard { user_host: &user_host, sock: &control_sock, active: master_ok };

    let push_conn = |cmd: &mut std::process::Command| {
        cmd.arg("-o").arg("BatchMode=yes");
        if let Some(sock) = control_path {
            cmd.arg("-o").arg(format!("ControlPath={}", sock));
        }
    };

    // Ensure remote directory exists.
    {
        let mut cmd = std::process::Command::new("ssh");
        push_conn(&mut cmd);
        cmd.arg("-o").arg("ConnectTimeout=15");
        cmd.arg(&user_host).arg(format!("mkdir -p {}", shell_quote(&remote_dir)));
        if let Ok(out) = cmd.output() {
            if !out.status.success() {
                log::warn!(
                    "scp_upload_files[{}]: mkdir failed (may already exist): {}",
                    upload_id, String::from_utf8_lossy(&out.stderr).trim()
                );
            }
        }
    }

    // Probe whether we can poll remote sizes (GNU `du`). We measure only the
    // destination paths, so this is bounded regardless of how large remote_dir is.
    let size_poll_ok =
        total_bytes > 0 && remote_targets_size(&user_host, control_path, &targets).is_some();

    let emit = |bytes: u64, rate_bps: f64, done: bool| {
        let percent = if total_bytes > 0 {
            (bytes as f64 / total_bytes as f64 * 100.0).min(100.0)
        } else if done {
            100.0
        } else {
            0.0
        };
        let _ = app.emit(
            &event_name,
            ScpProgress {
                upload_id: upload_id.to_string(),
                bytes_sent: bytes,
                total_bytes,
                percent,
                rate_bps,
                files_total,
                done,
                indeterminate: !size_poll_ok && !done,
            },
        );
    };
    emit(0, 0.0, false);

    // --- Spawn scp as a tracked child. ---
    let mut scp = std::process::Command::new("scp");
    if needs_recursive {
        scp.arg("-r");
    }
    push_conn(&mut scp);
    scp.arg("-o").arg("ConnectTimeout=30");
    for path in local_paths {
        scp.arg(path);
    }
    // Don't shell_quote the remote dir — scp parses the user@host:path format itself.
    let dest = format!("{}:{}/", user_host, remote_dir);
    log::info!(
        "scp_upload_files[{}]: dest={:?}, recursive={}, total_bytes={}, size_poll={}",
        upload_id, dest, needs_recursive, total_bytes, size_poll_ok
    );
    scp.arg(&dest);
    scp.stdin(std::process::Stdio::null());
    scp.stdout(std::process::Stdio::null());
    scp.stderr(std::process::Stdio::piped());

    let mut child = scp.spawn().map_err(|e| format!("Failed to run scp: {}", e))?;

    let start = std::time::Instant::now();
    let mut last_bytes = 0u64;
    let mut last_t = start;
    let poll = std::time::Duration::from_millis(400);

    let exit_status = loop {
        // Cancelled?
        if cancel_flag.load(Ordering::SeqCst) {
            let _ = child.kill();
            let _ = child.wait();
            log::info!("scp_upload_files[{}]: cancelled by user", upload_id);
            // Best-effort cleanup of partial destination files (non-recursive case).
            if !needs_recursive && !targets.is_empty() {
                let quoted: Vec<String> = targets.iter().map(|t| shell_quote(t)).collect();
                let mut cmd = std::process::Command::new("ssh");
                push_conn(&mut cmd);
                cmd.arg(&user_host).arg(format!("rm -f -- {}", quoted.join(" ")));
                let _ = cmd.output();
            }
            emit(last_bytes, 0.0, true);
            return Err("SCP upload cancelled".to_string());
        }

        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(format!("scp wait failed: {}", e)),
        }

        if size_poll_ok {
            if let Some(cur) = remote_targets_size(&user_host, control_path, &targets) {
                let sent = cur.min(total_bytes);
                let now = std::time::Instant::now();
                let dt = now.duration_since(last_t).as_secs_f64();
                let rate = if dt > 0.0 {
                    sent.saturating_sub(last_bytes) as f64 / dt
                } else {
                    0.0
                };
                last_bytes = sent;
                last_t = now;
                emit(sent, rate.max(0.0), false);
            }
        }

        std::thread::sleep(poll);
    };

    if exit_status.success() {
        emit(total_bytes, 0.0, true);
        log::info!("scp_upload_files[{}]: success, {} file(s) uploaded", upload_id, files_total);
        Ok(())
    } else {
        let mut stderr = String::new();
        if let Some(mut e) = child.stderr.take() {
            use std::io::Read;
            let _ = e.read_to_string(&mut stderr);
        }
        emit(last_bytes, 0.0, true);
        log::error!("scp_upload_files[{}] failed: {}", upload_id, stderr.trim());
        Err(format!("SCP upload failed: {}", stderr.trim()))
    }
}

#[command]
pub async fn create_editor_tab(
    state: State<'_, Arc<AppState>>,
    window: Window,
    workspace_id: String,
    pane_id: String,
    name: String,
    file_info: EditorFileInfo,
    after_tab_id: Option<String>,
) -> Result<Tab, String> {
    let mut app_data = state.app_data.write();
    let win_label = window.label().to_string();

    let win = app_data
        .window_mut(&win_label)
        .ok_or("Window not found")?;
    let ws = win
        .workspaces
        .iter_mut()
        .find(|w| w.id == workspace_id)
        .ok_or("Workspace not found")?;
    let pane = ws
        .panes
        .iter_mut()
        .find(|p| p.id == pane_id)
        .ok_or("Pane not found")?;

    let mut file_info = file_info;
    file_info.file_path = expand_tilde(&file_info.file_path);
    let tab = Tab::new_editor(name, file_info);
    let tab_id = tab.id.clone();

    // Insert after the specified tab, or append to end
    let insert_idx = after_tab_id
        .and_then(|id| pane.tabs.iter().position(|t| t.id == id))
        .map(|idx| idx + 1)
        .unwrap_or(pane.tabs.len());
    pane.tabs.insert(insert_idx, tab.clone());
    pane.active_tab_id = Some(tab_id);

    let _ = save_state(&app_data);

    Ok(tab)
}

/// Shell-quote a string for safe use in remote commands.
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// Expand `~` and `~username` prefixes on a remote host via SSH.
/// SCP in SFTP mode doesn't support `~user` paths, so we resolve them first.
fn expand_remote_tilde(user_host: &str, path: &str) -> String {
    if !path.starts_with('~') {
        return path.to_string();
    }
    // Run `echo ~` or `echo ~username` on the remote to get the real path
    // Extract the tilde prefix (~ or ~username) before any /
    let (tilde_prefix, rest) = match path.find('/') {
        Some(i) => (&path[..i], &path[i..]),
        None => (path, ""),
    };
    let cmd = format!("echo {}", tilde_prefix);
    if let Ok(output) = std::process::Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=10")
        .arg(user_host)
        .arg(&cmd)
        .output()
    {
        if output.status.success() {
            let expanded = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !expanded.is_empty() && expanded.starts_with('/') {
                return format!("{}{}", expanded, rest);
            }
        }
    }
    path.to_string()
}

#[command]
pub async fn watch_file(
    state: State<'_, Arc<AppState>>,
    window: Window,
    tab_id: String,
    path: String,
) -> Result<(), String> {
    let path = expand_tilde(&path);
    let file_path = Path::new(&path).to_path_buf();

    if !file_path.exists() {
        return Err("File does not exist".to_string());
    }

    // Canonicalize so our stored path matches what `notify` reports for directory
    // events — this resolves symlinks (e.g. macOS /tmp -> /private/tmp) so the
    // path-equality filter below holds.
    let file_path = std::fs::canonicalize(&file_path)
        .map_err(|e| format!("Cannot resolve file path: {}", e))?;
    let parent = file_path
        .parent()
        .ok_or("Cannot determine parent directory")?
        .to_path_buf();

    // Remove existing watcher for this tab if any
    state.file_watchers.write().remove(&tab_id);

    let event_tab_id = tab_id.clone();
    let watch_target = file_path.clone();
    // Watch the PARENT directory, not the file itself. Most editors and agents
    // save via write-temp-then-rename (atomic write), which swaps the file's
    // inode — a single-file watch stays bound to the old inode and goes silent
    // after the first replace. Watching the directory and filtering for our
    // file's path catches rename-into-place reliably.
    let debouncer = notify_debouncer_mini::new_debouncer(
        std::time::Duration::from_millis(500),
        move |res: Result<Vec<notify_debouncer_mini::DebouncedEvent>, notify::Error>| {
            if let Ok(events) = res {
                // Ignore activity on other files in the directory.
                if !events.iter().any(|e| e.path == watch_target) {
                    return;
                }
                if watch_target.exists() {
                    let _ = window.emit(&format!("file-changed-{}", event_tab_id), ());
                } else {
                    let _ = window.emit(&format!("file-deleted-{}", event_tab_id), ());
                }
            }
        },
    )
    .map_err(|e| format!("Failed to create file watcher: {}", e))?;

    let mut debouncer = debouncer;
    debouncer
        .watcher()
        .watch(&parent, notify::RecursiveMode::NonRecursive)
        .map_err(|e| format!("Failed to watch directory: {}", e))?;

    state.file_watchers.write().insert(
        tab_id,
        FileWatcherHandle {
            _debouncer: debouncer,
        },
    );

    Ok(())
}

#[command]
pub async fn unwatch_file(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<(), String> {
    state.file_watchers.write().remove(&tab_id);
    Ok(())
}

#[command]
pub async fn get_file_mtime(path: String) -> Result<u64, String> {
    let path = expand_tilde(&path);
    let metadata = std::fs::metadata(&path).map_err(|e| format!("Cannot stat file: {}", e))?;
    let mtime = metadata
        .modified()
        .map_err(|e| format!("Cannot get mtime: {}", e))?;
    let epoch = mtime
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("Time error: {}", e))?;
    Ok(epoch.as_millis() as u64)
}

/// Extract user@host from an SSH command string.
/// Handles formats like "ssh user@host", "ssh -o Foo=bar user@host", etc.
fn extract_user_host(ssh_command: &str) -> Result<String, String> {
    let parts: Vec<&str> = ssh_command.split_whitespace().collect();

    // Find the user@host part (first argument that contains @ and isn't a flag value)
    let mut skip_next = false;
    for part in &parts {
        if skip_next {
            skip_next = false;
            continue;
        }
        if *part == "ssh" {
            continue;
        }
        // Flags that take a value
        if [
            "-o", "-i", "-p", "-l", "-F", "-J", "-L", "-R", "-D", "-W", "-S", "-b", "-c", "-E",
            "-m", "-O", "-Q", "-w", "-B", "-e",
        ]
        .contains(part)
        {
            skip_next = true;
            continue;
        }
        // Single-letter flags (no value)
        if part.starts_with('-') && !part.contains('=') {
            continue;
        }
        // This should be user@host or just host
        return Ok(part.to_string());
    }

    Err("Cannot extract host from SSH command".to_string())
}

// ── Remote file watching (SSH stat polling) ──────────────────────────

/// Get modification time of a remote file via SSH stat.
/// Returns epoch seconds (not ms) since that's what `stat` gives us.
fn ssh_stat_mtime(user_host: &str, remote_path: &str) -> Result<u64, String> {
    let quoted = shell_quote(remote_path);
    // stat -c %Y = Linux (GNU coreutils), stat -f %m = macOS/BSD
    let cmd = format!(
        "stat -c %Y {} 2>/dev/null || stat -f %m {} 2>/dev/null",
        quoted, quoted
    );
    let output = std::process::Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=5")
        .arg(user_host)
        .arg(&cmd)
        .output()
        .map_err(|e| format!("Failed to run ssh: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SSH stat failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    stdout
        .parse::<u64>()
        .map_err(|_| format!("Cannot parse mtime from: {}", stdout))
}

/// One-shot remote file mtime check (used by frontend before/after saves).
#[command]
pub async fn get_remote_file_mtime(ssh_command: String, remote_path: String) -> Result<u64, String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);
    let mtime = ssh_stat_mtime(&user_host, &remote_path)?;
    // Return as seconds (frontend handles comparison consistently)
    Ok(mtime)
}

/// Register a remote file for periodic mtime polling.
#[command]
pub async fn watch_remote_file(
    state: State<'_, Arc<AppState>>,
    app: tauri::AppHandle,
    tab_id: String,
    ssh_command: String,
    remote_path: String,
) -> Result<(), String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);

    {
        let mut watchers = state.remote_file_watchers.write();
        watchers.insert(tab_id, RemoteFileWatch {
            user_host,
            remote_path,
            last_mtime: None,
        });
    }

    // Start polling task if not already running
    if !state.remote_watcher_running.swap(true, std::sync::atomic::Ordering::SeqCst) {
        let state_clone = state.inner().clone();
        let app_clone = app.clone();
        tokio::spawn(remote_file_poll_loop(state_clone, app_clone));
    }

    Ok(())
}

/// Unregister a remote file watcher.
#[command]
pub async fn unwatch_remote_file(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<(), String> {
    state.remote_file_watchers.write().remove(&tab_id);
    Ok(())
}

#[command]
pub async fn git_show_file(file_path: String, git_ref: String) -> Result<String, String> {
    let file_path = expand_tilde(&file_path);
    let dir = Path::new(&file_path)
        .parent()
        .ok_or("Invalid file path")?;

    // Get repo root
    let root_output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(dir)
        .output()
        .map_err(|e| format!("Failed to run git: {}", e))?;
    if !root_output.status.success() {
        return Err("Not a git repository".to_string());
    }
    let repo_root = String::from_utf8_lossy(&root_output.stdout)
        .trim()
        .to_string();

    // Compute relative path
    let rel_path = Path::new(&file_path)
        .strip_prefix(&repo_root)
        .map_err(|_| "File is outside the git repository".to_string())?;

    // git show ref:path
    let output = std::process::Command::new("git")
        .arg("show")
        .arg(format!("{}:{}", git_ref, rel_path.to_string_lossy()))
        .current_dir(&repo_root)
        .output()
        .map_err(|e| format!("Failed to run git show: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("git show failed: {}", stderr.trim()));
    }

    String::from_utf8(output.stdout)
        .map_err(|_| "File content is not valid UTF-8".to_string())
}

#[command]
pub async fn is_directory(path: String) -> Result<bool, String> {
    let path = expand_tilde(&path);
    Ok(std::path::Path::new(&path).is_dir())
}

#[command]
pub async fn ssh_is_directory(ssh_command: String, remote_path: String) -> Result<bool, String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);
    let quoted = shell_quote(&remote_path);

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::process::Command::new("ssh")
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ConnectTimeout=5")
            .arg(&user_host)
            .arg(format!("test -d {}", quoted))
            .output()
    )
    .await
    .map_err(|_| "SSH timed out".to_string())?
    .map_err(|e| format!("SSH failed: {}", e))?;

    Ok(output.status.success())
}

#[command]
pub async fn list_files(
    path: String,
    max_files: Option<u32>,
    show_hidden: Option<bool>,
    show_ignored: Option<bool>,
) -> Result<Vec<String>, String> {
    let path = expand_tilde(&path);
    let max = max_files.unwrap_or(10_000) as usize;
    let hidden = show_hidden.unwrap_or(false);
    let no_ignore = show_ignored.unwrap_or(false);
    let base = std::path::PathBuf::from(&path);

    if !base.is_dir() {
        return Err(format!("Not a directory: {}", path));
    }

    let walker = ignore::WalkBuilder::new(&base)
        .hidden(!hidden)
        .git_ignore(!no_ignore)
        .git_global(!no_ignore)
        .git_exclude(!no_ignore)
        .max_depth(Some(20))
        .filter_entry(|entry| {
            // Allow .git/ files at depth 1 (e.g. .git/config) but skip deeper traversal.
            // This prevents the massive .git/objects/ tree from flooding results while
            // still showing useful top-level .git files like HEAD, config, description.
            let path = entry.path();
            for (i, component) in path.components().enumerate() {
                if let std::path::Component::Normal(name) = component {
                    if name == ".git" {
                        // Count how many components follow .git
                        let depth_after_git = path.components().count() - i - 1;
                        // Allow .git itself (dir entry) and direct children (depth 1),
                        // skip anything nested deeper (depth 2+)
                        return depth_after_git <= 1;
                    }
                }
            }
            true
        })
        .build();

    let mut entries: Vec<(String, u64)> = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    for entry in walker {
        if entries.len() >= max {
            break;
        }
        if let Ok(entry) = entry {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                if let Ok(rel) = entry.path().strip_prefix(&base) {
                    let rel_str = rel.to_string_lossy().to_string();
                    let mtime = entry
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    seen.insert(rel_str.clone());
                    entries.push((rel_str, mtime));
                }
            }
        }
    }

    // Always surface .env* files in the base directory, even when dotfiles or
    // gitignored files are filtered out. They're among the most commonly edited
    // config files and would otherwise be unreachable from Quick Open without
    // toggling both the hidden and gitignore filters.
    if let Ok(read_dir) = std::fs::read_dir(&base) {
        for entry in read_dir.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !name.starts_with(".env") || seen.contains(&name) {
                continue;
            }
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let mtime = entry
                    .metadata()
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                seen.insert(name.clone());
                entries.push((name, mtime));
            }
        }
    }

    // Sort by mtime descending (most recently modified first)
    entries.sort_by(|a, b| b.1.cmp(&a.1));

    Ok(entries.into_iter().map(|(path, _)| path).collect())
}

#[command]
pub async fn ssh_list_files(
    ssh_command: String,
    remote_path: String,
    max_files: Option<u32>,
    show_hidden: Option<bool>,
    show_ignored: Option<bool>,
) -> Result<Vec<String>, String> {
    let user_host = extract_user_host(&ssh_command)?;
    let remote_path = expand_remote_tilde(&user_host, &remote_path);
    let max = max_files.unwrap_or(5000);
    let hidden = show_hidden.unwrap_or(false);
    let no_ignore = show_ignored.unwrap_or(false);
    let quoted = shell_quote(&remote_path);

    // Build find command: exclude hidden dirs/files unless show_hidden is set.
    // Always limit .git/ to depth 1 (show HEAD, config, etc. but skip objects/).
    let find_cmd = if hidden {
        // Show dotfiles but only top-level .git/ files (HEAD, config, etc.)
        // Skip anything nested inside .git subdirs (objects/, refs/, hooks/, logs/)
        "find . -maxdepth 10 -path '*/.git/*/*' -prune -o -type f -print".to_string()
    } else {
        "find . -maxdepth 10 -path '*/.*' -prune -o -type f -print".to_string()
    };

    // Always surface top-level .env* files regardless of the hidden/gitignore
    // filters — they're commonly edited and would otherwise be unreachable.
    // Duplicates (when the filters already include them) are removed below.
    //
    // The trailing `|| true` is load-bearing: this is the LAST command in the
    // remote group, so its exit status becomes the whole command's status.
    // `grep` exits 1 when there are no .env* files (the common case), which
    // would otherwise make a perfectly good listing report "SSH list files
    // failed". Force exit 0 here so an empty .env match is never fatal — real
    // failures (bad host, missing dir) still surface via the `cd` short-circuit.
    let env_lister = "ls -1p .env* 2>/dev/null | grep -v '/$' || true";

    // git ls-files --cached --others --exclude-standard lists tracked + untracked
    // files while respecting .gitignore. Plain `git ls-files` only shows tracked
    // files, missing anything not yet committed.
    // show_ignored: use find instead of git ls-files to bypass .gitignore entirely.
    let cmd = if no_ignore {
        format!(
            "cd {} && {{ {} | head -{}; {}; }}",
            quoted, find_cmd, max, env_lister
        )
    } else {
        // Use git ls-files with fallback to find for non-git directories.
        // The `| head -1` test ensures we fall back if git ls-files returns empty
        // (e.g. empty repo with no tracked files at all).
        format!(
            "cd {} && {{ {{ out=$(git ls-files --cached --others --exclude-standard 2>/dev/null) && [ -n \"$out\" ] && echo \"$out\" || {}; }} | head -{}; {}; }}",
            quoted, find_cmd, max, env_lister
        )
    };

    let output = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        tokio::process::Command::new("ssh")
            .arg("-o").arg("BatchMode=yes")
            .arg("-o").arg("ConnectTimeout=10")
            .arg(&user_host)
            .arg(&cmd)
            .output()
    )
    .await
    .map_err(|_| "SSH connection timed out (15s)".to_string())?
    .map_err(|e| format!("SSH failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SSH list files failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    let files: Vec<String> = stdout
        .lines()
        .filter(|line| !line.is_empty())
        .map(|line| line.strip_prefix("./").unwrap_or(line).to_string())
        .filter(|f| seen.insert(f.clone()))
        .collect();

    Ok(files)
}

/// Background polling loop for remote file watchers.
/// Groups files by user@host, runs one batched stat per host every 3 seconds.
async fn remote_file_poll_loop(state: Arc<AppState>, app: tauri::AppHandle) {
    use std::collections::HashMap;

    // Track consecutive failures per host
    let mut host_failures: HashMap<String, u32> = HashMap::new();
    const MAX_FAILURES: u32 = 5;

    loop {
        tokio::time::sleep(tokio::time::Duration::from_secs(3)).await;

        // Take a snapshot of current watchers
        let snapshot: Vec<(String, String, String, Option<u64>)> = {
            let watchers = state.remote_file_watchers.read();
            if watchers.is_empty() {
                // No watchers — stop the polling task
                state.remote_watcher_running.store(false, std::sync::atomic::Ordering::SeqCst);
                log::info!("Remote file watcher: no watchers remaining, stopping poll loop");
                return;
            }
            watchers.iter().map(|(tab_id, w)| {
                (tab_id.clone(), w.user_host.clone(), w.remote_path.clone(), w.last_mtime)
            }).collect()
        };

        // Group by user_host
        let mut by_host: HashMap<String, Vec<(String, String, Option<u64>)>> = HashMap::new();
        for (tab_id, user_host, remote_path, last_mtime) in snapshot {
            by_host.entry(user_host).or_default().push((tab_id, remote_path, last_mtime));
        }

        // Poll each host
        for (user_host, files) in &by_host {
            // Skip hosts that have failed too many times
            if host_failures.get(user_host).copied().unwrap_or(0) >= MAX_FAILURES {
                continue;
            }

            let result = poll_host_files(user_host, files).await;

            match result {
                Ok(mtimes) => {
                    host_failures.remove(user_host);

                    // Compare and emit events for changed files
                    let mut watchers = state.remote_file_watchers.write();
                    for (i, (tab_id, _path, _old_mtime)) in files.iter().enumerate() {
                        if let Some(&new_mtime) = mtimes.get(i) {
                            if let Some(watcher) = watchers.get_mut(tab_id) {
                                if new_mtime == 0 {
                                    // stat failed — file may have been deleted
                                    if watcher.last_mtime.is_some() {
                                        log::info!("Remote file deleted: {} (tab {})", watcher.remote_path, tab_id);
                                        let _ = app.emit(&format!("file-deleted-{}", tab_id), ());
                                    }
                                    continue;
                                }
                                let changed = watcher.last_mtime
                                    .map(|old| new_mtime != old)
                                    .unwrap_or(false);
                                watcher.last_mtime = Some(new_mtime);
                                if changed {
                                    log::info!("Remote file changed: {} (tab {})", watcher.remote_path, tab_id);
                                    let _ = app.emit(&format!("file-changed-{}", tab_id), ());
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    let count = host_failures.entry(user_host.clone()).or_insert(0);
                    *count += 1;
                    if *count >= MAX_FAILURES {
                        log::warn!("Remote file watcher: giving up on {} after {} failures", user_host, MAX_FAILURES);
                    } else {
                        log::debug!("Remote file watcher: poll failed for {}: {}", user_host, e);
                    }
                }
            }
        }
    }
}

/// Poll mtime for multiple files on a single host in one SSH call.
/// Returns a vec of mtime values (one per file, 0 if stat failed for that file).
async fn poll_host_files(
    user_host: &str,
    files: &[(String, String, Option<u64>)],
) -> Result<Vec<u64>, String> {
    // Build a script that stats each file and prints one mtime per line
    let mut file_list = String::new();
    for (_, path, _) in files {
        if !file_list.is_empty() {
            file_list.push(' ');
        }
        file_list.push_str(&shell_quote(path));
    }

    let script = format!(
        "for f in {}; do stat -c %Y \"$f\" 2>/dev/null || stat -f %m \"$f\" 2>/dev/null || echo 0; done",
        file_list
    );

    let output = tokio::process::Command::new("ssh")
        .arg("-o").arg("BatchMode=yes")
        .arg("-o").arg("ConnectTimeout=5")
        .arg(user_host)
        .arg(&script)
        .output()
        .await
        .map_err(|e| format!("SSH failed: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("SSH stat failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mtimes: Vec<u64> = stdout
        .lines()
        .map(|line| line.trim().parse::<u64>().unwrap_or(0))
        .collect();

    Ok(mtimes)
}
