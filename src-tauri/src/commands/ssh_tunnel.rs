use std::sync::Arc;

use tauri::Emitter;

use crate::state::AppState;

#[derive(serde::Serialize)]
pub struct SshTunnelInfo {
    pub tunnel_id: String,
    pub remote_port: u16,
    pub host_key: String,
}

/// Start a reverse SSH tunnel to expose the local MCP server on a remote host.
/// Spawns `ssh -N -o ExitOnForwardFailure=yes -R 0:127.0.0.1:{local_port} {ssh_args}`.
/// Parses the allocated remote port from stderr output.
/// Returns the tunnel info including the allocated remote port.
#[tauri::command]
pub async fn start_ssh_tunnel(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    ssh_args: String,
    host_key: String,
    tab_id: String,
    local_port: u16,
) -> Result<SshTunnelInfo, String> {
    // Check if tunnel already exists for this host
    {
        let mut tunnels = state.ssh_tunnels.write();
        if let Some(tunnel) = tunnels.get_mut(&host_key) {
            // Verify the process is still alive
            if is_process_alive(tunnel.pid) {
                tunnel.tab_ids.insert(tab_id);
                return Ok(SshTunnelInfo {
                    tunnel_id: host_key.clone(),
                    remote_port: tunnel.remote_port,
                    host_key,
                });
            }
            // Process died — remove stale entry and create new tunnel
            tunnels.remove(&host_key);
        }
    }

    // Build SSH command args
    // ssh_args is already cleaned (e.g. "user@host" or "-p 2222 user@host")
    let mut cmd_args: Vec<String> = Vec::new();
    cmd_args.push("-N".to_string());
    // -v is required: when SSH multiplexes through an existing ControlMaster,
    // the mux client prints nothing without it. With -v, "Allocated port ..."
    // appears on stderr alongside debug lines (which we filter out).
    cmd_args.push("-v".to_string());
    cmd_args.push("-o".to_string());
    cmd_args.push("ExitOnForwardFailure=yes".to_string());
    // No ControlMaster=no — let SSH multiplex over the user's existing control
    // socket if they have ControlMaster auto. This gives free auth for password/
    // passphrase users whose session is already authenticated.
    cmd_args.push("-R".to_string());
    cmd_args.push(format!("0:127.0.0.1:{}", local_port));

    // Add the user's SSH args
    for arg in ssh_args.split_whitespace() {
        cmd_args.push(arg.to_string());
    }

    log::info!("Starting SSH tunnel: ssh {}", cmd_args.join(" "));

    let mut child = tokio::process::Command::new("ssh")
        .args(&cmd_args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn SSH tunnel: {}", e))?;

    let pid = child.id().ok_or("Failed to get SSH tunnel PID")?;

    // Read both stdout and stderr to find the allocated port.
    // Direct connections print to stderr, but ControlMaster-multiplexed
    // connections print "Allocated port ..." to stdout instead.
    let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
    let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;
    let remote_port = parse_allocated_port(stdout, stderr).await?;

    log::info!("SSH tunnel established: {} → remote port {}", host_key, remote_port);

    // Store the tunnel (don't store the Child — we track by PID)
    {
        let mut tunnels = state.ssh_tunnels.write();
        let mut tab_ids = std::collections::HashSet::new();
        tab_ids.insert(tab_id);
        tunnels.insert(host_key.clone(), crate::state::app_state::SshTunnel {
            pid,
            remote_port,
            host_key: host_key.clone(),
            tab_ids,
        });
    }

    // Spawn background task to monitor the process and clean up on exit.
    // Note: ControlMaster mux clients exit immediately after setting up the
    // forwarding (the master holds it). Don't remove tunnel state if the
    // process exits with code 0 — the forwarding is still alive in the master.
    let state_clone = state.inner().clone();
    let hk = host_key.clone();
    let app_clone = app.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let exit_ok = status.map(|s| s.success()).unwrap_or(false);
        if exit_ok {
            log::info!("SSH tunnel process exited cleanly for {} (likely ControlMaster mux)", hk);
        } else {
            log::info!("SSH tunnel process exited with error for {}", hk);
            let tab_ids: Vec<String> = {
                let mut tunnels = state_clone.ssh_tunnels.write();
                if let Some(tunnel) = tunnels.remove(&hk) {
                    tunnel.tab_ids.into_iter().collect()
                } else {
                    vec![]
                }
            };
            // Notify frontend so bridge indicators update in real-time
            for tid in &tab_ids {
                let _ = app_clone.emit(&format!("ssh-tunnel-down-{}", tid), ());
            }
        }
    });

    Ok(SshTunnelInfo {
        tunnel_id: host_key.clone(),
        remote_port,
        host_key,
    })
}

/// Remove a tab from a tunnel's ref count. Kills the tunnel if no tabs remain.
#[tauri::command]
pub async fn detach_ssh_tunnel(
    state: tauri::State<'_, Arc<AppState>>,
    host_key: String,
    tab_id: String,
) -> Result<(), String> {
    let should_kill = {
        let mut tunnels = state.ssh_tunnels.write();
        if let Some(tunnel) = tunnels.get_mut(&host_key) {
            tunnel.tab_ids.remove(&tab_id);
            if tunnel.tab_ids.is_empty() {
                let pid = tunnel.pid;
                tunnels.remove(&host_key);
                Some(pid)
            } else {
                None
            }
        } else {
            None
        }
    };

    if let Some(pid) = should_kill {
        kill_process(pid);
        log::info!("Killed SSH tunnel for {} (pid {})", host_key, pid);
    }

    Ok(())
}

/// Get info about an active tunnel for a host.
#[tauri::command]
pub fn get_ssh_tunnel(
    state: tauri::State<'_, Arc<AppState>>,
    host_key: String,
) -> Option<SshTunnelInfo> {
    let tunnels = state.ssh_tunnels.read();
    tunnels.get(&host_key).map(|t| SshTunnelInfo {
        tunnel_id: t.host_key.clone(),
        remote_port: t.remote_port,
        host_key: t.host_key.clone(),
    })
}

/// Kill all SSH tunnels (called on app exit).
pub fn kill_all_tunnels(state: &Arc<AppState>) {
    let tunnels: Vec<(String, u32)> = {
        let mut map = state.ssh_tunnels.write();
        let items: Vec<_> = map.drain().map(|(k, t)| (k, t.pid)).collect();
        items
    };
    for (host_key, pid) in tunnels {
        kill_process(pid);
        log::info!("Killed SSH tunnel for {} on shutdown", host_key);
    }
}

/// Get the local MCP server port (needed by frontend to construct tunnel).
#[tauri::command]
pub fn get_mcp_port(state: tauri::State<'_, Arc<AppState>>) -> Option<u16> {
    *state.claude_code_port.read()
}

/// Get the MCP auth token (needed by frontend to write remote lockfile).
#[tauri::command]
pub fn get_mcp_auth(state: tauri::State<'_, Arc<AppState>>) -> Option<String> {
    state.claude_code_auth.read().clone()
}

/// The `/maiterm statusline` helper scripts, served from the same bundled
/// source the local install uses. The frontend embeds these in the remote
/// (SSH) skill setup so `/maiterm statusline` works on remote hosts too.
#[derive(serde::Serialize)]
pub struct MaitermSkillScripts {
    pub setup_statusline: String,
    pub statusline_command: String,
}

#[tauri::command]
pub fn get_maiterm_skill_scripts() -> MaitermSkillScripts {
    MaitermSkillScripts {
        setup_statusline: crate::claude_code::lockfile::STATUSLINE_SETUP_SCRIPT.to_string(),
        statusline_command: crate::claude_code::lockfile::STATUSLINE_PAYLOAD_SCRIPT.to_string(),
    }
}

/// Run setup commands on a remote host via a separate background SSH connection.
/// This avoids injecting commands into the user's interactive PTY.
/// Spawns `ssh {ssh_args} 'setup_script'` and waits for completion.
#[tauri::command]
pub async fn ssh_run_setup(
    ssh_args: String,
    setup_script: String,
) -> Result<(), String> {
    let mut cmd_args: Vec<String> = Vec::new();
    // No ControlMaster=no — let SSH multiplex over the user's control socket
    // if available, giving free auth for password/passphrase sessions.
    // Batch mode — fail fast if no auth method works (no interactive prompt possible)
    cmd_args.push("-o".to_string());
    cmd_args.push("BatchMode=yes".to_string());
    // Don't allocate a PTY
    cmd_args.push("-T".to_string());

    for arg in ssh_args.split_whitespace() {
        cmd_args.push(arg.to_string());
    }

    // The setup script is passed as a single command argument
    cmd_args.push(setup_script);

    log::info!("SSH setup: ssh {} <script>", ssh_args);

    let output = tokio::time::timeout(
        tokio::time::Duration::from_secs(30),
        tokio::process::Command::new("ssh")
            .args(&cmd_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
    ).await
        .map_err(|_| "SSH setup timed out (30s)".to_string())?
        .map_err(|e| format!("Failed to spawn SSH setup: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "Connection to ... closed" messages (normal for batch SSH)
        if !stderr.trim().is_empty() && !stderr.contains("Connection to") {
            log::warn!("SSH setup stderr: {}", stderr);
        }
        // Still consider it a success if exit code is 0 or if the commands ran
        // Some SSH servers return non-zero even when commands succeed
        if output.status.code() != Some(0) && output.status.code() != Some(255) {
            return Err(format!("SSH setup failed (exit {}): {}",
                output.status.code().unwrap_or(-1), stderr.trim()));
        }
    }

    log::info!("SSH setup completed for {}", ssh_args);
    Ok(())
}

/// Parse "Allocated port NNNNN for remote forward" from SSH output.
/// Reads both stdout and stderr concurrently — direct connections print to
/// stderr, but ControlMaster-multiplexed connections print to stdout.
/// Times out after 15 seconds.
async fn parse_allocated_port(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
) -> Result<u16, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    fn try_parse_port(line: &str) -> Option<u16> {
        line.strip_prefix("Allocated port ")
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|s| s.parse::<u16>().ok())
    }

    let timeout = tokio::time::Duration::from_secs(15);
    match tokio::time::timeout(timeout, async {
        let mut stdout_done = false;
        let mut stderr_done = false;
        loop {
            if stdout_done && stderr_done {
                return Err("SSH process exited without allocating a port".to_string());
            }
            tokio::select! {
                result = stdout_lines.next_line(), if !stdout_done => {
                    match result {
                        Ok(Some(line)) => {
                            log::debug!("SSH tunnel stdout: {}", line);
                            if let Some(port) = try_parse_port(&line) {
                                return Ok(port);
                            }
                        }
                        Ok(None) => { stdout_done = true; }
                        Err(e) => return Err(format!("Reading stdout: {}", e)),
                    }
                }
                result = stderr_lines.next_line(), if !stderr_done => {
                    match result {
                        Ok(Some(line)) => {
                            log::debug!("SSH tunnel stderr: {}", line);
                            if let Some(port) = try_parse_port(&line) {
                                return Ok(port);
                            }
                        }
                        Ok(None) => { stderr_done = true; }
                        Err(e) => return Err(format!("Reading stderr: {}", e)),
                    }
                }
            }
        }
    }).await {
        Ok(result) => result,
        Err(_) => Err("Timeout waiting for SSH tunnel port allocation (15s)".to_string()),
    }
}

/// Check if a tunnel process is alive (used by diagnostics).
pub fn is_tunnel_alive(pid: u32) -> bool {
    is_process_alive(pid)
}

#[cfg(unix)]
fn is_process_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn is_process_alive(pid: u32) -> bool {
    use std::process::Command;
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {}", pid), "/NH"])
        .output()
        .map(|o| !String::from_utf8_lossy(&o.stdout).contains("No tasks"))
        .unwrap_or(false)
}

#[cfg(unix)]
fn kill_process(pid: u32) {
    unsafe { libc::kill(pid as i32, libc::SIGTERM); }
}

#[cfg(windows)]
fn kill_process(pid: u32) {
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/F"])
        .output();
}
