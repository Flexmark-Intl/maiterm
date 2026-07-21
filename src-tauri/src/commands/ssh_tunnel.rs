use std::sync::Arc;

use tauri::Emitter;

use crate::state::AppState;

/// The maiTerm-owned ControlMaster socket for a bridge host. The tunnel becomes the master;
/// short-lived clients (transcript-mirror fetches, scp) mux over it with `ControlMaster=no`
/// — no re-auth, tens of ms per command. Deliberately NOT the user's `~/.ssh/master-*`
/// namespace: a maiTerm connection owning the user's socket once broke their own
/// `ssh <host>` when it died ("mux_client_request_session: Session open refused by peer").
/// Lives under `~/.maiterm` (dev/prod-suffixed) because macOS caps unix-socket paths at
/// 104 bytes — the app data dir doesn't reliably fit.
#[cfg(unix)]
pub fn cm_socket_path(host_key: &str) -> Option<std::path::PathBuf> {
    let dir = dirs::home_dir()?
        .join(".maiterm")
        .join(if cfg!(debug_assertions) { "cm-dev" } else { "cm" });
    let safe: String = host_key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '@') { c } else { '_' })
        .collect();
    Some(dir.join(format!("{safe}.sock")))
}

/// Create the socket dir (0700) and clear any stale socket file so `ControlMaster=yes`
/// actually becomes the master (with a leftover file ssh prints "ControlSocket already
/// exists, disabling multiplexing" and silently degrades). Only called when no live tunnel
/// is tracked for the host, so the file can't belong to a working master.
#[cfg(unix)]
fn prepare_cm_socket(host_key: &str) -> Option<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let path = cm_socket_path(host_key)?;
    let dir = path.parent()?;
    if !dir.is_dir() {
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).ok()?;
    }
    let _ = std::fs::remove_file(&path);
    Some(path)
}

/// Arg prefix for short-lived maiTerm ssh commands aimed at a bridge host: mux over the
/// tunnel's ControlMaster socket when it's alive (re-auth-free, ~tens of ms), fall back to
/// an independent BatchMode connection when it isn't. Used by the transcript mirror's
/// fetches and remote image staging. Callers append the tunnel's recorded `ssh_args` and
/// the remote command.
pub fn mux_client_args(host_key: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    #[cfg(unix)]
    if let Some(sock) = cm_socket_path(host_key) {
        args.push("-o".into());
        args.push("ControlMaster=no".into());
        args.push("-o".into());
        args.push(format!("ControlPath={}", sock.display()));
    }
    #[cfg(not(unix))]
    let _ = host_key;
    args.push("-o".into());
    args.push("BatchMode=yes".into());
    args.push("-o".into());
    args.push("ConnectTimeout=5".into());
    args.push("-T".into());
    args
}

/// Remove the CM socket for a host. ssh usually unlinks it when the master exits; this is
/// belt-and-braces for kills/crashes so the next tunnel start finds a clean path.
fn cleanup_cm_socket(host_key: &str) {
    #[cfg(unix)]
    if let Some(path) = cm_socket_path(host_key) {
        let _ = std::fs::remove_file(path);
    }
    #[cfg(not(unix))]
    let _ = host_key;
}

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
    // Fail fast + reap wedged tunnels: bound the initial connect and detect a dead
    // peer within ~30s (else a hung remote lingers "alive" for the user's global
    // ServerAliveInterval, often minutes), so the monitor task below removes the stale
    // tunnel and the frontend can re-establish. Explicit -o wins over ~/.ssh/config.
    cmd_args.push("-o".to_string());
    cmd_args.push("ConnectTimeout=15".to_string());
    cmd_args.push("-o".to_string());
    cmd_args.push("ServerAliveInterval=10".to_string());
    cmd_args.push("-o".to_string());
    cmd_args.push("ServerAliveCountMax=3".to_string());
    // Never touch the user's shared ControlMaster socket. With `ControlMaster auto`
    // (common in ~/.ssh/config), this long-lived `-N` tunnel would otherwise CREATE
    // and own `~/.ssh/master-<user>@<host>.socket`, forcing the user's own plain
    // `ssh <host>` to multiplex over OUR tunnel. When our connection then saturates or
    // degrades, their manual ssh breaks with "mux_client_request_session: Session open
    // refused by peer". Instead the tunnel is master of a socket in OUR OWN namespace
    // (~/.maiterm/cm*, see cm_socket_path): the user's ssh never resolves that path, so
    // the poisoning failure mode is impossible, while short-lived maiTerm clients
    // (transcript-mirror fetches, scp) get free mux'd commands over the already-
    // authenticated tunnel. The socket lives and dies with the tunnel process — no
    // ControlPersist, so no daemonized master escapes our pid tracking.
    #[cfg(unix)]
    if let Some(sock) = prepare_cm_socket(&host_key) {
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlMaster=yes".to_string());
        cmd_args.push("-o".to_string());
        cmd_args.push(format!("ControlPath={}", sock.display()));
    } else {
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlMaster=no".to_string());
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlPath=none".to_string());
    }
    // Windows OpenSSH has no ControlMaster support — plain independent connection.
    #[cfg(not(unix))]
    {
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlMaster=no".to_string());
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlPath=none".to_string());
    }
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
            ssh_args: ssh_args.clone(),
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
            cleanup_cm_socket(&hk);
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
        cleanup_cm_socket(&host_key);
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
        cleanup_cm_socket(&host_key);
        log::info!("Killed SSH tunnel for {} on shutdown", host_key);
    }
}

/// Get the local MCP server port (needed by frontend to construct tunnel).
#[tauri::command]
pub fn get_mcp_port(state: tauri::State<'_, Arc<AppState>>) -> Option<u16> {
    *state.mcp_port.read()
}

/// Get the MCP auth token (needed by frontend to write remote lockfile).
#[tauri::command]
pub fn get_mcp_auth(state: tauri::State<'_, Arc<AppState>>) -> Option<String> {
    state.mcp_auth.read().clone()
}

/// The `/maiterm statusline` helper scripts, served from the same bundled
/// source the local install uses. The frontend embeds these in the remote
/// (SSH) skill setup so `/maiterm statusline` works on remote hosts too.
#[derive(serde::Serialize)]
pub struct MaitermSkillScripts {
    pub skill_md: String,
    pub setup_statusline: String,
    pub statusline_command: String,
}

#[tauri::command]
pub fn get_maiterm_skill_scripts() -> MaitermSkillScripts {
    MaitermSkillScripts {
        skill_md: crate::claude_code::lockfile::MAITERM_SKILL_MD.to_string(),
        setup_statusline: crate::claude_code::lockfile::STATUSLINE_SETUP_SCRIPT.to_string(),
        statusline_command: crate::claude_code::lockfile::STATUSLINE_PAYLOAD_SCRIPT.to_string(),
    }
}

/// python3 merge for `~/.codex/config.toml`: textual block-replace of our
/// `[mcp_servers.<name>]` table (NO tomllib/tomli_w dependency — read-only/non-stdlib).
/// Block on stdin, table name via `$__codex_name`. NO single quotes (shell wraps in `''`).
const CODEX_TOML_MERGE_PY: &str = concat!(
    "import os,sys,re\n",
    "p=os.path.expanduser(\"~/.codex/config.toml\")\n",
    "name=os.environ.get(\"__codex_name\",\"\")\n",
    "block=sys.stdin.read()\n",
    "try:\n src=open(p).read()\nexcept Exception:\n src=\"\"\n",
    "lines=src.splitlines(True)\n",
    "out=[]\ni=0\ntarget=\"[mcp_servers.\"+name+\"]\"\nhdr=re.compile(r\"^\\s*\\[\")\n",
    "while i<len(lines):\n",
    " if lines[i].strip()==target:\n",
    "  i+=1\n",
    "  while i<len(lines) and not hdr.match(lines[i]):\n   i+=1\n",
    "  continue\n",
    " out.append(lines[i])\n i+=1\n",
    "base=\"\".join(out).rstrip()\n",
    "res=(base+\"\\n\\n\"+block.strip()+\"\\n\") if base else (block.strip()+\"\\n\")\n",
    "open(p,\"w\").write(res)\n",
);

/// python3 merge for `~/.codex/hooks.json`: replace the shim placeholder with the
/// remote's absolute path, then replace-or-append OUR entries per event (matched by
/// `agent-hook.sh`), preserving user hooks and other top-level keys. Ours on stdin,
/// absolute shim path via `$MAITERM_SHIM`. NO single quotes.
const CODEX_HOOKS_MERGE_PY: &str = concat!(
    "import os,sys,json\n",
    "p=os.path.expanduser(\"~/.codex/hooks.json\")\n",
    "shim=os.environ.get(\"MAITERM_SHIM\",\"\")\n",
    "ours=json.loads(sys.stdin.read().replace(\"__MAITERM_SHIM__\",shim))\n",
    "try:\n cur=json.load(open(p))\nexcept Exception:\n cur={}\n",
    "if not isinstance(cur,dict):\n cur={}\n",
    "ch=cur.get(\"hooks\")\n",
    "if not isinstance(ch,dict):\n ch={}\n cur[\"hooks\"]=ch\n",
    "def isours(e):\n",
    " for h in e.get(\"hooks\",[]):\n",
    "  if \"agent-hook.sh\" in (h.get(\"command\") or \"\"):\n   return True\n",
    " return False\n",
    "for ev,entries in ours.get(\"hooks\",{}).items():\n",
    " keep=[e for e in ch.get(ev,[]) if not isours(e)]\n",
    " keep.extend(entries)\n",
    " ch[ev]=keep\n",
    "open(p,\"w\").write(json.dumps(cur,indent=2))\n",
);

/// Build the shell script that installs maiTerm's Codex integration on a REMOTE host
/// over the SSH reverse tunnel, mirroring the local `CodexRegistrar` by reusing the SAME
/// Rust renderers (`render_codex_remote_artifacts`) so remote and local artifacts can't
/// drift. Writes `~/.codex/config.toml` (`[mcp_servers.<name>]` → the tunnel port via the
/// streamable-HTTP `/mcp` endpoint + `http_headers` auth), the executable hook shim, a
/// merged `~/.codex/hooks.json` (user hooks preserved), and the prompt. The whole body
/// no-ops on hosts without the `codex` CLI. Run it via `ssh_run_setup` (background SSH,
/// NOT the interactive PTY). `tab_id` reaches the shim through the env / `~/.aiterm` file
/// the Claude setup block already writes — identical to how remote Claude resolves it.
#[tauri::command]
pub fn build_codex_setup_script(remote_port: u16, auth: String, tab_id: String) -> String {
    let _ = tab_id; // resolved on the remote via env / ~/.aiterm, like Claude's hooks

    let (config_block, hooks_json, prompt) =
        crate::claude_code::codex::render_codex_remote_artifacts(remote_port, &auth);
    let name = crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Codex);
    let shim = crate::claude_code::lockfile::AGENT_HOOK_SHIM;

    // Single-quote shell-var payloads (escape embedded single quotes the POSIX way).
    let q = |s: &str| s.replace('\'', "'\\''");

    let toml_py = CODEX_TOML_MERGE_PY;
    let hooks_py = CODEX_HOOKS_MERGE_PY;

    let mut lines: Vec<String> = Vec::new();
    // No-op cleanly on hosts without the Codex CLI. Use if/then/fi (NOT `|| exit`):
    // this script is also written into the INTERACTIVE PTY by the "Install MCP for
    // Current User" path, where `exit` would close the user's shell.
    lines.push("if command -v codex >/dev/null 2>&1; then".to_string());
    lines.push("mkdir -p ~/.codex/hooks ~/.codex/prompts".to_string());
    lines.push("shim_abs=\"$HOME/.codex/hooks/agent-hook.sh\"".to_string());
    // Hook shim — literal bytes via a quoted heredoc (no expansion of $1/$HOME/etc).
    lines.push("cat > \"$shim_abs\" <<'MAITERM_CODEX_SHIM_EOF'".to_string());
    lines.push(shim.trim_end().to_string());
    lines.push("MAITERM_CODEX_SHIM_EOF".to_string());
    lines.push("chmod 755 \"$shim_abs\"".to_string());
    // Prompt.
    lines.push("cat > ~/.codex/prompts/maiterm.md <<'MAITERM_CODEX_PROMPT_EOF'".to_string());
    lines.push(prompt.trim_end().to_string());
    lines.push("MAITERM_CODEX_PROMPT_EOF".to_string());
    // config.toml merge (block on stdin, name via env).
    lines.push(format!("__codex_toml='{}'", q(&config_block)));
    lines.push(
        format!("printf '%s' \"$__codex_toml\" | __codex_name='{}' python3 -c '", q(name))
            + toml_py
            + "'",
    );
    // hooks.json merge (ours on stdin, abs shim path via env).
    lines.push(format!("__codex_hooks='{}'", q(&hooks_json)));
    lines.push(
        "printf '%s' \"$__codex_hooks\" | MAITERM_SHIM=\"$shim_abs\" python3 -c '".to_string()
            + hooks_py
            + "'",
    );
    lines.push("fi".to_string());

    lines.join("\n")
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
    // Fully independent connection — never share the user's ControlMaster socket (see
    // start_ssh_tunnel: sharing it lets the bridge poison the user's own `ssh <host>`).
    cmd_args.push("-o".to_string());
    cmd_args.push("ControlMaster=no".to_string());
    cmd_args.push("-o".to_string());
    cmd_args.push("ControlPath=none".to_string());
    // Batch mode — fail fast if no auth method works (no interactive prompt possible)
    cmd_args.push("-o".to_string());
    cmd_args.push("BatchMode=yes".to_string());
    // Bound the connect so a dead/hung remote doesn't burn the full 30s timeout below.
    cmd_args.push("-o".to_string());
    cmd_args.push("ConnectTimeout=15".to_string());
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

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::{Command, Stdio};

    /// Pipe `input` to `program -c <stdin-reader>` and return whether it exited 0.
    fn pipe_ok(program: &str, args: &[&str], input: &str) -> (bool, String) {
        let mut child = match Command::new(program)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            // If the validator binary isn't installed on this machine, don't fail CI.
            Err(_) => return (true, format!("{} not available — skipped", program)),
        };
        child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
        let out = child.wait_with_output().unwrap();
        (out.status.success(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    #[test]
    fn codex_setup_script_is_valid_bash() {
        let script = build_codex_setup_script(40123, "TESTTOKEN123".to_string(), "tab-abc".to_string());
        // bash -n parses (heredocs, pipes, if/fi, single-quoted python -c, multiline vars)
        // without executing — catches the quoting/heredoc hazards before the live test.
        let (ok, stderr) = pipe_ok("bash", &["-n"], &script);
        assert!(ok, "bash -n rejected the generated script:\n{}\n--- stderr ---\n{}", script, stderr);

        // Spot-check the load-bearing pieces are present and pointed at the tunnel port.
        assert!(script.contains("if command -v codex >/dev/null 2>&1; then"));
        assert!(script.contains("fi"));
        assert!(script.contains("http://127.0.0.1:40123/mcp"), "config url uses tunnel port");
        assert!(script.contains("agent-hook.sh"), "shim written");
        assert!(script.contains("chmod 755"), "shim made executable");
        assert!(script.contains("python3 -c '"), "merges via python3 -c");
    }

    #[test]
    fn cm_socket_path_is_sanitized_and_short() {
        let p = cm_socket_path("ews@nova").expect("home dir");
        let s = p.to_string_lossy();
        assert!(s.ends_with("ews@nova.sock"));
        assert!(s.contains("/.maiterm/cm"), "lives in the maiTerm-owned namespace: {s}");
        // Hostile chars can't traverse or break the ssh option value.
        let odd = cm_socket_path("user@host with/slash:port").unwrap();
        let name = odd.file_name().unwrap().to_string_lossy().into_owned();
        assert_eq!(name, "user@host_with_slash_port.sock");
        // macOS caps sun_path at 104 bytes — a realistic host key must fit.
        assert!(s.len() < 104, "socket path too long for macOS: {} bytes", s.len());
    }

    #[test]
    fn codex_merge_python_snippets_compile() {
        // Compile-check (no execution / side effects) both embedded python merges.
        let check = "import sys; compile(sys.stdin.read(), \"<embedded>\", \"exec\")";
        let (ok1, e1) = pipe_ok("python3", &["-c", check], CODEX_TOML_MERGE_PY);
        assert!(ok1, "config.toml merge python does not compile:\n{}", e1);
        let (ok2, e2) = pipe_ok("python3", &["-c", check], CODEX_HOOKS_MERGE_PY);
        assert!(ok2, "hooks.json merge python does not compile:\n{}", e2);
    }
}
