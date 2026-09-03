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

/// Ask the host's ControlMaster (if any) whether it is alive: `ssh -O check` against the
/// maiTerm-owned socket. A tunnel entry whose pid is gone can still be fully functional —
/// a CM mux client exits as soon as the master holds its forwarding — so pid liveness
/// alone under-reports. Cheap (local socket round-trip), bounded at 3s.
async fn cm_master_alive(host_key: &str, ssh_args: &str) -> bool {
    #[cfg(unix)]
    {
        let Some(sock) = cm_socket_path(host_key) else {
            return false;
        };
        if !sock.exists() {
            return false;
        }
        let mut args: Vec<String> = vec![
            "-o".into(),
            format!("ControlPath={}", sock.display()),
            "-O".into(),
            "check".into(),
        ];
        for arg in ssh_args.split_whitespace() {
            args.push(arg.to_string());
        }
        matches!(
            tokio::time::timeout(
                std::time::Duration::from_secs(3),
                tokio::process::Command::new("ssh")
                    .args(&args)
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status(),
            )
            .await,
            Ok(Ok(status)) if status.success()
        )
    }
    #[cfg(not(unix))]
    {
        let _ = (host_key, ssh_args);
        false
    }
}

// ── Which remote port this maiTerm listens on ────────────────────────────────────────
//
// The tunnel's remote port is what identifies this maiTerm to a remote agent, and it used
// to be chosen by the remote sshd (`-R 0:`). That is the root of the shared-config problem:
// an sshd-chosen port does not exist yet when a tab's ssh command is built, so it cannot
// ride into the remote shell's environment, so it has to be baked into the per-ACCOUNT
// files instead (`~/.claude.json`, `~/.claude/settings.json`) — where the next maiTerm to
// bridge overwrites it and takes the first one's tabs down with it.
//
// Choosing it ourselves makes the port a property of THIS INSTALL rather than of one
// connection: stable across restarts (the remote config stops being rewritten every launch)
// and knowable before a tab connects (so it can be exported next to MAITERM_TAB_ID).
//
// The preferred port is drawn once per install from a range below the Linux ephemeral floor
// (32768), so the remote kernel's own outbound allocations cannot land on it. Dev and prod
// draw separately — `app_data_slug()` already splits their data dirs — because they are two
// writers of the same remote files today.
//
// A collision is still possible: a peer maiTerm that drew the same number, or our own zombie
// listener held open by a dead ControlMaster (which this codebase has seen). So a taken port
// falls through to the next candidate and, once those are exhausted, back to `-R 0:` — today's
// behaviour, kept as the floor. Port choice must never be the reason a tunnel fails to come up.
const REMOTE_PORT_BASE: u16 = 28000;
const REMOTE_PORT_SPAN: u16 = 1000;
const REMOTE_PORT_ATTEMPTS: usize = 4;

#[derive(serde::Serialize, serde::Deserialize, Default)]
struct RemotePortBook {
    /// The port this install asks for on every host.
    instance_port: u16,
    /// Hosts where a collision pushed us off `instance_port`.
    #[serde(default)]
    hosts: std::collections::HashMap<String, u16>,
}

fn remote_port_book_path() -> Option<std::path::PathBuf> {
    dirs::data_dir().map(|p| {
        p.join(crate::state::persistence::app_data_slug())
            .join("aiterm-remote-ports.json")
    })
}

/// The in-process copy. Loaded once, written through on every change — the file only
/// matters across restarts, which is the whole point of it.
fn remote_port_book() -> &'static parking_lot::Mutex<RemotePortBook> {
    static BOOK: std::sync::OnceLock<parking_lot::Mutex<RemotePortBook>> = std::sync::OnceLock::new();
    BOOK.get_or_init(|| {
        let mut book: RemotePortBook = remote_port_book_path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        if !(REMOTE_PORT_BASE..REMOTE_PORT_BASE + REMOTE_PORT_SPAN).contains(&book.instance_port) {
            // First run on this install (or a file from before this existed). A real PRNG, not
            // the clock: `subsec_nanos()` looks like a fine source of jitter and is not one —
            // macOS's realtime clock is microsecond-granular, so it is always a multiple of
            // 1000 and `% 1000` is always ZERO. Every install would draw the base port, which
            // turns the collision path from a rare fallback into the guaranteed case for every
            // pair of instances — including dev and prod on one Mac.
            use rand::Rng;
            book.instance_port = REMOTE_PORT_BASE + rand::thread_rng().gen_range(0..REMOTE_PORT_SPAN);
            log::info!("SSH tunnel: this install will ask for remote port {}", book.instance_port);
            save_remote_port_book(&book);
        }
        parking_lot::Mutex::new(book)
    })
}

fn save_remote_port_book(book: &RemotePortBook) {
    let Some(path) = remote_port_book_path() else { return };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_string_pretty(book) {
        let _ = std::fs::write(path, json);
    }
}

/// SSH short flags that take a following argument (`man ssh`), so the host token can be
/// found without mistaking a flag's value for it.
const SSH_FLAGS_WITH_ARG: &[&str] = &[
    "-b", "-c", "-D", "-E", "-e", "-F", "-I", "-i", "-J", "-L", "-l", "-m", "-O", "-o", "-p", "-Q",
    "-R", "-S", "-W", "-w",
];

/// The key this host is remembered under: the `user@host` token alone, not the whole arg
/// string. The two paths that need to agree see different strings for the same host — the
/// bridge keys off the ssh command it OBSERVED in the foreground, while a tab's replay keys
/// off the one it has STORED, and those differ by flags (`-x -C ews@nova` vs `ews@nova`).
/// Keyed on the raw string, a host that had to move off the instance port would be looked up
/// under one spelling and recorded under the other, so the ssh command would predict the
/// wrong port on that host forever.
fn port_book_key(host_key: &str) -> String {
    let mut tokens = host_key.split_whitespace().peekable();
    while let Some(token) = tokens.next() {
        if token.starts_with('-') {
            // `-p 2222` consumes its value; `-p2222` and `-oKey=Val` carry it inline.
            if token.len() == 2 && SSH_FLAGS_WITH_ARG.contains(&token) {
                tokens.next();
            }
            continue;
        }
        return token.to_string();
    }
    host_key.trim().to_string()
}

/// The remote port this maiTerm will ask for on `host_key`. Answerable without a tunnel:
/// that is what lets the ssh command carry it before the tunnel exists.
pub fn preferred_remote_port(host_key: &str) -> u16 {
    let book = remote_port_book().lock();
    *book
        .hosts
        .get(&port_book_key(host_key))
        .unwrap_or(&book.instance_port)
}

/// Remember where we actually landed, INCLUDING a port the remote chose for us. The
/// temptation is to forget that one, on the grounds that an ephemeral number is a one-off
/// not worth chasing — but the book is what `get_remote_bridge_env` predicts from, and
/// forgetting leaves it predicting `instance_port` while the tunnel is somewhere else
/// entirely. On a contended account `instance_port` is not merely wrong, it is another live
/// maiTerm, so every tab on that host would bake a stranger's port and quietly talk to them.
/// Recording it also tends to stabilise: the port was free last time, so asking for it again
/// usually works, and `next_port_candidate` pulls the walk back into our range if it does not.
fn record_remote_port(host_key: &str, port: u16) {
    let key = port_book_key(host_key);
    let mut book = remote_port_book().lock();
    let changed = if port == book.instance_port {
        book.hosts.remove(&key).is_some()
    } else {
        book.hosts.insert(key, port) != Some(port)
    };
    if changed {
        save_remote_port_book(&book);
    }
}

fn next_port_candidate(prev: u16) -> u16 {
    let offset = prev.wrapping_sub(REMOTE_PORT_BASE).wrapping_add(1) % REMOTE_PORT_SPAN;
    REMOTE_PORT_BASE + offset
}

/// The `ssh` arguments for a reverse tunnel to `host_key`. `listen` is the remote port to
/// bind; `None` lets the remote sshd choose one (`-R 0:`).
fn build_tunnel_args(host_key: &str, ssh_args: &str, local_port: u16, listen: Option<u16>) -> Vec<String> {
    let mut cmd_args: Vec<String> = Vec::new();
    cmd_args.push("-N".to_string());
    // -v is required: when SSH multiplexes through an existing ControlMaster,
    // the mux client prints nothing without it. With -v, the forwarding result
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
    //
    // ControlPersist MUST be forced off, and it is the whole reason tunnels used to leak.
    // We override ControlMaster and ControlPath but inherited ControlPersist from the user's
    // ~/.ssh/config (600 here), and a master with ControlPersist set FORKS ITSELF INTO THE
    // BACKGROUND once the connection is up. The pid we recorded was the parent, which exits
    // seconds later, so `is_process_alive` was false for every tunnel we owned: the reuse
    // path never hit, `detach_ssh_tunnel` and `kill_all_tunnels` killed a pid that was
    // already gone, and the real process — reparented to init, holding its remote port —
    // was abandoned. Every launch then found its own previous tunnel squatting the port and
    // walked to the next one, which is what marched ews@nova from 28599 to 28616 and left
    // ten orphans alive. Verified both ways against a live host: with ControlPersist
    // inherited the recorded pid is dead within 6s and an unparented ssh holds the forward;
    // with `no` the recorded pid stays alive, killing it removes the tunnel, and muxed
    // clients still work over the socket.
    #[cfg(unix)]
    if let Some(sock) = prepare_cm_socket(host_key) {
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlMaster=yes".to_string());
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlPersist=no".to_string());
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
        let _ = host_key;
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlMaster=no".to_string());
        cmd_args.push("-o".to_string());
        cmd_args.push("ControlPath=none".to_string());
    }
    cmd_args.push("-R".to_string());
    cmd_args.push(format!("{}:127.0.0.1:{}", listen.unwrap_or(0), local_port));

    // Add the user's SSH args
    for arg in ssh_args.split_whitespace() {
        cmd_args.push(arg.to_string());
    }
    cmd_args
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
    // Serialize same-host starts (single-flight): an app restart re-bridges dozens of
    // tabs at once, many to the same server. Without this they all pass the exists-check
    // below before any has stored an entry, and every tab spawns its own redundant
    // tunnel with its own remote port. With it, the first caller establishes the tunnel
    // and the rest fall into the reuse paths.
    let start_lock = {
        let mut locks = state.ssh_tunnel_start_locks.lock();
        locks.entry(host_key.clone()).or_default().clone()
    };
    let _start_guard = start_lock.lock().await;

    // Fast path: a tracked tunnel whose process is still alive.
    {
        let mut tunnels = state.ssh_tunnels.write();
        if let Some(tunnel) = tunnels.get_mut(&host_key) {
            if is_process_alive(tunnel.pid) {
                tunnel.tab_ids.insert(tab_id);
                return Ok(SshTunnelInfo {
                    tunnel_id: host_key.clone(),
                    remote_port: tunnel.remote_port,
                    host_key,
                });
            }
        }
    }

    // Tracked entry with a dead pid is often NOT a dead tunnel: a CM mux client exits as
    // soon as the master holds its forwarding. Ask the master directly before respawning.
    let has_entry = state.ssh_tunnels.read().contains_key(&host_key);
    if has_entry && cm_master_alive(&host_key, &ssh_args).await {
        let mut tunnels = state.ssh_tunnels.write();
        if let Some(tunnel) = tunnels.get_mut(&host_key) {
            tunnel.tab_ids.insert(tab_id);
            return Ok(SshTunnelInfo {
                tunnel_id: host_key.clone(),
                remote_port: tunnel.remote_port,
                host_key,
            });
        }
    }

    // Genuinely stale (or first start for this host): replace the entry, but carry its
    // tab_ids over — dropping them silently strips those tabs of every tunnel-keyed
    // feature (comms attachment staging, maiLink image sends).
    let inherited_tab_ids: std::collections::HashSet<String> = {
        let mut tunnels = state.ssh_tunnels.write();
        tunnels
            .remove(&host_key)
            .map(|t| t.tab_ids)
            .unwrap_or_default()
    };

    // Ask for this install's own remote port, walking to the next candidate if something
    // already holds it, and finally letting the remote choose (`-R 0:`) so a run of
    // collisions degrades to the old behaviour instead of leaving the tab unbridged.
    // ssh_args is already cleaned (e.g. "user@host" or "-p 2222 user@host").
    let mut candidates: Vec<Option<u16>> = Vec::with_capacity(REMOTE_PORT_ATTEMPTS + 1);
    let mut candidate = preferred_remote_port(&host_key);
    for _ in 0..REMOTE_PORT_ATTEMPTS {
        candidates.push(Some(candidate));
        candidate = next_port_candidate(candidate);
    }
    candidates.push(None);

    let mut established: Option<(tokio::process::Child, u16)> = None;
    for listen in candidates {
        let cmd_args = build_tunnel_args(&host_key, &ssh_args, local_port, listen);
        log::info!("Starting SSH tunnel: ssh {}", cmd_args.join(" "));

        let mut child = tokio::process::Command::new("ssh")
            .args(&cmd_args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| format!("Failed to spawn SSH tunnel: {}", e))?;

        // Read both stdout and stderr for the forwarding result. Direct connections
        // report on stderr, but ControlMaster-multiplexed ones use stdout instead.
        let stdout = child.stdout.take().ok_or("Failed to capture stdout")?;
        let stderr = child.stderr.take().ok_or("Failed to capture stderr")?;
        // Kill before propagating. The timeout arm is reached only while BOTH pipes are still
        // open — i.e. the ssh is provably alive and merely slow to authenticate — and tokio
        // does not kill on drop, so returning here used to abandon a running process that
        // goes on to bind its remote port with no entry in `ssh_tunnels` to reach it by. That
        // is the same orphan this file's startup sweep exists to clean up, except created
        // fresh, and the frontend's retry loop can mint another on the next term-title event.
        let outcome = match await_forward_result(stdout, stderr, listen).await {
            Ok(outcome) => outcome,
            Err(e) => {
                let _ = child.kill().await;
                return Err(e);
            }
        };
        match outcome {
            ForwardOutcome::Ready(port) => {
                established = Some((child, port));
                break;
            }
            ForwardOutcome::PortTaken => {
                // ExitOnForwardFailure has already ended it; kill() is the reap.
                let _ = child.kill().await;
                log::info!(
                    "SSH tunnel: remote port {} is taken on {} — trying the next candidate",
                    listen.unwrap_or(0),
                    host_key
                );
            }
        }
    }

    let (mut child, remote_port) =
        established.ok_or_else(|| "SSH tunnel: no remote port could be bound".to_string())?;
    let pid = child.id().ok_or("Failed to get SSH tunnel PID")?;
    record_remote_port(&host_key, remote_port);

    log::info!("SSH tunnel established: {} → remote port {}", host_key, remote_port);

    // Store the tunnel (don't store the Child — we track by PID).
    // Merge, never blind-insert: N tabs bridging the same host at once (typical right
    // after an app restart) all pass the exists-check above before any has stored, and
    // a plain insert() would leave only the last writer's tab_id in the entry.
    {
        let mut tunnels = state.ssh_tunnels.write();
        let entry = tunnels
            .entry(host_key.clone())
            .or_insert_with(|| crate::state::app_state::SshTunnel {
                pid,
                remote_port,
                host_key: host_key.clone(),
                tab_ids: Default::default(),
                ssh_args: ssh_args.clone(),
            });
        entry.pid = pid;
        entry.remote_port = remote_port;
        entry.tab_ids.extend(inherited_tab_ids);
        entry.tab_ids.insert(tab_id);
    }

    // Spawn background task to monitor the process and clean up on exit.
    //
    // This used to keep the tunnel's state on a clean exit, reasoning that the process was a
    // ControlMaster mux client whose master still held the forwarding. That reading was
    // wrong: what actually exited was the master forking itself into the background under an
    // inherited ControlPersist (see build_tunnel_args). The entry it preserved held a pid
    // that was already dead, which is how a leaked tunnel stayed invisible. With
    // ControlPersist forced off we own the master and it does not fork, so an exit — clean
    // or not — means the forwarding is gone and the tabs on it need to hear about it.
    let state_clone = state.inner().clone();
    let hk = host_key.clone();
    let app_clone = app.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let exit_ok = status.map(|s| s.success()).unwrap_or(false);
        {
            log::info!(
                "SSH tunnel process for {} exited {} — dropping the tunnel",
                hk,
                if exit_ok { "cleanly" } else { "with an error" }
            );
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

/// Whether a `ps` row is one of OUR reverse tunnels AND genuinely orphaned.
///
/// `needle` carries the trailing separator so `…/cm/` cannot match `…/cm-dev/`; `-R` excludes
/// the short-lived muxed clients (transcript fetches, scp) that share the same socket and are
/// none of our business to kill.
///
/// `ppid == 1` is the part that makes this safe to run at all, and it only became checkable
/// with `ControlPersist=no`: a tunnel belonging to a LIVE maiTerm is now that process's child,
/// while one whose owner is gone has been reparented to init. Without it the sweep rests on
/// "no other instance of my flavour is running", which nothing enforces — there is no
/// single-instance guard, and on Linux a second launch is simply a second process. It would
/// then SIGTERM the live instance's tunnels and take its remote agents offline.
fn is_orphaned_tunnel(cmd: &str, ppid: u32, needle: &str) -> bool {
    ppid == 1 && cmd.contains(needle) && cmd.contains(" -R ")
}

/// One `ps -eo pid=,ppid=,command=` row → `(pid, ppid, command)`.
///
/// Both numeric columns are RIGHT-ALIGNED in a padded field, so what separates them is a RUN
/// of spaces, not one. The first version of this split with `splitn(3, char::is_whitespace)`,
/// which breaks on each single space: for `"  134     1 /usr/libexec/logd"` the middle field
/// came out EMPTY, the `u32` parse failed, and the row was skipped. That discarded every row
/// whose ppid is short — which is every orphan, the only rows this sweep looks for. Measured
/// against a live process table: 670 rows with ppid 1, none of them seen, so `killed` was
/// always 0. Nothing logs on zero kills, so the sweep looked like it was working while doing
/// nothing at all; the ports kept walking (28607 → 28609 on the first boot after it shipped).
///
/// Consume a whole run of whitespace per column, and keep the command verbatim after it —
/// `is_orphaned_tunnel` matches on `" -R "`, so the interior spacing has to survive.
fn parse_ps_row(line: &str) -> Option<(u32, u32, &str)> {
    let (pid, rest) = line.trim_start().split_once(char::is_whitespace)?;
    let (ppid, cmd) = rest.trim_start().split_once(char::is_whitespace)?;
    Some((pid.parse().ok()?, ppid.parse().ok()?, cmd.trim_start()))
}

/// The pids of our abandoned tunnels in a `ps` dump. Split out from the sweep so the parse and
/// the predicate can be tested together against REAL `ps` output — testing the predicate alone
/// on pre-split fields is what let the row parse ship broken.
fn orphaned_tunnel_pids<'a>(ps_output: &'a str, needle: &str) -> Vec<(u32, &'a str)> {
    ps_output
        .lines()
        .filter_map(parse_ps_row)
        .filter(|(_, ppid, cmd)| is_orphaned_tunnel(cmd, *ppid, needle))
        .map(|(pid, _, cmd)| (pid, cmd))
        .collect()
}

/// Kill reverse tunnels left behind by a previous run (called at startup).
///
/// `kill_all_tunnels` runs on a clean exit, and a crash or an externally-issued quit — which
/// is how the local deploy script restarts maiTerm — bypasses it. Anything still holding one
/// of our ControlPath sockets at startup is therefore an orphan by definition: this process
/// has not opened a tunnel yet. Left alone they squat their remote ports, and the port walk
/// steps over them, which is how one host marched from 28599 to 28616.
///
/// Matched on our OWN socket directory, with the separator included so the prod sweep cannot
/// reach `cm-dev/` — dev and prod run at the same time by design, and each must only ever
/// kill its own.
#[cfg(unix)]
pub fn kill_orphaned_tunnels() {
    let Some(dir) = cm_socket_path("x").and_then(|p| p.parent().map(|d| d.to_path_buf())) else {
        return;
    };
    let needle = format!("ControlPath={}/", dir.display());
    let Ok(out) = std::process::Command::new("ps").args(["-eo", "pid=,ppid=,command="]).output() else {
        return;
    };
    let mut killed = 0;
    let stdout = String::from_utf8_lossy(&out.stdout);
    for (pid, cmd) in orphaned_tunnel_pids(&stdout, &needle) {
        log::info!("Killing orphaned SSH tunnel from a previous run (pid {}): {}", pid, cmd);
        kill_process(pid);
        killed += 1;
    }
    if killed > 0 {
        log::info!("Killed {} orphaned SSH tunnel(s) left by a previous run", killed);
    }
    // Only the sockets of what we just killed. NOT the whole directory: a live sibling
    // instance of this flavour keeps its own masters' sockets in here, and deleting one
    // silently downgrades every muxed client (transcript fetches, remote image staging) to a
    // fresh authenticated connection.
    if killed > 0 {
        if let Ok(entries) = std::fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                // A socket whose master answers is somebody's live tunnel — leave it.
                if !std::os::unix::net::UnixStream::connect(&path).is_ok() {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }
}

#[cfg(not(unix))]
pub fn kill_orphaned_tunnels() {}

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

/// What a tab's ssh command exports so the remote agent can reach THIS maiTerm.
#[derive(serde::Serialize)]
pub struct RemoteBridgeEnv {
    pub port: u16,
    pub auth: String,
}

/// The bridge values for a host, answerable BEFORE its tunnel exists — which is the whole
/// point: they are baked into the tab's ssh command, and that command is built while the
/// remote is still a login prompt.
///
/// A LIVE tunnel to that host outranks the book, because it is not a prediction: it is where
/// this maiTerm is actually listening. That matters most in the case the book is worst at —
/// a host where collisions pushed us onto a port we did not choose. Tunnels are per-host and
/// shared by every tab on it, so one lookup answers for all of them. Matched on the
/// normalised key, since the tunnel was opened under the ssh command the bridge OBSERVED and
/// this is called with the one the tab has STORED.
#[tauri::command]
pub fn get_remote_bridge_env(
    state: tauri::State<'_, Arc<AppState>>,
    host_key: String,
) -> Option<RemoteBridgeEnv> {
    let auth = state.mcp_auth.read().clone()?;
    let key = port_book_key(&host_key);
    let live = state
        .ssh_tunnels
        .read()
        .values()
        .find(|t| port_book_key(&t.host_key) == key)
        .map(|t| t.remote_port);
    Some(RemoteBridgeEnv {
        port: live.unwrap_or_else(|| preferred_remote_port(&host_key)),
        auth,
    })
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

enum ForwardOutcome {
    /// The remote is listening on this port.
    Ready(u16),
    /// Something else already holds the port we asked for.
    PortTaken,
}

/// Classify one line of `ssh -v` output. `requested` is the port we asked to bind, or
/// `None` when we left the choice to the remote.
fn classify_forward_line(line: &str, requested: Option<u16>) -> Option<ForwardOutcome> {
    // The remote chose for us (`-R 0:`): "Allocated port NNNNN for remote forward to ...".
    if let Some(port) = line
        .strip_prefix("Allocated port ")
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|s| s.parse::<u16>().ok())
    {
        return Some(ForwardOutcome::Ready(port));
    }
    // We named the port, so nothing is allocated back to us. Observed against OpenSSH on
    // a real host, a taken port produces BOTH of these, the debug line first:
    //   debug1: remote forward failure for: listen 28123, connect 127.0.0.1:11420
    //   Error: remote port forwarding failed for listen port 28123
    // and a free one produces:
    //   debug1: remote forward success for: listen 28123, connect 127.0.0.1:11420
    // The "Error:"/"Warning:" prefix varies by version, so match on the phrase alone.
    if let Some(port) = requested {
        if line.contains("remote forward success for: listen") {
            return Some(ForwardOutcome::Ready(port));
        }
        if line.contains("remote port forwarding failed")
            || line.contains("remote forward failure for: listen")
        {
            return Some(ForwardOutcome::PortTaken);
        }
    }
    None
}

/// Wait for the reverse forwarding to be reported. Reads both stdout and stderr
/// concurrently — direct connections report on stderr, but ControlMaster-multiplexed
/// connections use stdout. Times out after 15 seconds.
async fn await_forward_result(
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
    requested: Option<u16>,
) -> Result<ForwardOutcome, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let mut stdout_lines = BufReader::new(stdout).lines();
    let mut stderr_lines = BufReader::new(stderr).lines();

    let try_parse_port = |line: &str| classify_forward_line(line, requested);

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
                            if let Some(outcome) = try_parse_port(&line) {
                                return Ok(outcome);
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
                            if let Some(outcome) = try_parse_port(&line) {
                                return Ok(outcome);
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

    /// Naming the port changes what ssh says on success: there is no "Allocated port"
    /// line to read, because nothing was allocated back to us. Reading the wrong line
    /// would hang every tunnel until the 15s timeout.
    #[test]
    fn a_named_port_is_confirmed_by_the_forward_success_line() {
        let line = "debug1: remote forward success for: listen 28123, connect 127.0.0.1:11420";
        assert!(matches!(
            classify_forward_line(line, Some(28123)),
            Some(ForwardOutcome::Ready(28123))
        ));
        // Same line means nothing when we did not name a port — the dynamic path is
        // still waiting for its allocation.
        assert!(classify_forward_line(line, None).is_none());
    }

    /// Both lines a real collision produces (captured against a live host), plus the
    /// older "Warning:" phrasing. Missing the verdict costs 15s of timeout per candidate.
    #[test]
    fn a_taken_port_is_a_retry_not_a_failure() {
        for line in [
            "debug1: remote forward failure for: listen 28123, connect 127.0.0.1:11420",
            "Error: remote port forwarding failed for listen port 28123",
            "Warning: remote port forwarding failed for listen port 28123",
        ] {
            assert!(
                matches!(
                    classify_forward_line(line, Some(28123)),
                    Some(ForwardOutcome::PortTaken)
                ),
                "not read as a collision: {line}"
            );
        }
    }

    #[test]
    fn the_remote_chosen_port_is_still_read_from_the_allocation_line() {
        let line = "Allocated port 45015 for remote forward to 127.0.0.1:11420";
        assert!(matches!(
            classify_forward_line(line, None),
            Some(ForwardOutcome::Ready(45015))
        ));
    }

    /// The startup sweep kills by command line, so its match has to be exact about two
    /// things: dev and prod run at the same time by design and must never kill each other's
    /// tunnels, and the muxed clients sharing the socket (transcript fetches, scp) are not
    /// tunnels at all.
    #[test]
    fn the_orphan_sweep_matches_only_our_own_tunnels() {
        let prod = "ControlPath=/Users/d/.maiterm/cm/";
        let tunnel = "ssh -N -v -o ControlMaster=yes -o ControlPath=/Users/d/.maiterm/cm/ews@nova.sock -R 28616:127.0.0.1:30375 -x -C ews@nova";
        assert!(is_orphaned_tunnel(tunnel, 1, prod));

        // The dev sibling's tunnel, which prod must leave strictly alone.
        let dev_tunnel = tunnel.replace("/cm/", "/cm-dev/");
        assert!(!is_orphaned_tunnel(&dev_tunnel, 1, prod));
        assert!(is_orphaned_tunnel(&dev_tunnel, 1, "ControlPath=/Users/d/.maiterm/cm-dev/"));

        // A muxed client over the same socket — no forwarding, not ours to kill.
        let mux = "ssh -o ControlMaster=no -o ControlPath=/Users/d/.maiterm/cm/ews@nova.sock -o BatchMode=yes -T ews@nova tail -c +1 /home/ews/.claude/x.jsonl";
        assert!(!is_orphaned_tunnel(mux, 1, prod));
    }

    /// The sweep runs at startup and kills by pattern, so its safety cannot rest on "no other
    /// instance of my flavour is running" — nothing enforces that, and on Linux a second
    /// launch is just a second process. A tunnel owned by a LIVE maiTerm is that process's
    /// child; only a reparented one is genuinely abandoned.
    #[test]
    fn a_live_instances_tunnel_is_not_an_orphan() {
        let prod = "ControlPath=/Users/d/.maiterm/cm/";
        let tunnel = "ssh -N -v -o ControlMaster=yes -o ControlPath=/Users/d/.maiterm/cm/ews@nova.sock -R 28616:127.0.0.1:30375 -x -C ews@nova";
        assert!(is_orphaned_tunnel(tunnel, 1, prod), "reparented: ours to clean up");
        assert!(!is_orphaned_tunnel(tunnel, 80701, prod), "still owned by a running maiTerm");
    }

    /// Verbatim `ps -eo pid=,ppid=,command=` output, captured on macOS 25.5. The padding is
    /// the whole point: both numeric columns are right-aligned in a five-wide field, so a
    /// three-digit pid carries two leading spaces and a ppid of 1 carries four. The last two
    /// rows are real maiTerm tunnels (pid/ppid both five digits, ppid = the live app), edited
    /// only to shorten the ControlPath.
    // No `\`-continuation after the opening quote: it strips the leading whitespace of the
    // line that follows, which would eat the launchd row's own padding — in a fixture whose
    // entire purpose is that padding.
    const REAL_PS_OUTPUT: &str = "    1     0 /sbin/launchd
  134     1 /Applications/Copy 'Em Helper.app/Contents/MacOS/Copy 'Em Helper
  612     1 /usr/libexec/logd
  142 39047 /Applications/Google Chrome.app/Contents/Frameworks/Google Chrome Helper
33251 32792 ssh -N -v -o ControlMaster=yes -o ControlPersist=no -o ControlPath=/Users/d/.maiterm/cm/-x_-C_root@nova2.sock -R 28701:127.0.0.1:13075 -x -C root@nova2
35452     1 ssh -N -v -o ControlMaster=yes -o ControlPersist=no -o ControlPath=/Users/d/.maiterm/cm/-x_-C_ews@nova.sock -R 28617:127.0.0.1:13075 -x -C ews@nova";

    /// The regression that shipped in v2.1.0: the row parse, not the predicate.
    ///
    /// `splitn(3, char::is_whitespace)` broke on each SINGLE space, so every row with a padded
    /// (i.e. short) ppid lost its middle field to the empty string and was discarded — which
    /// is exactly and only the orphans. The sweep could never kill anything, and said nothing
    /// about it. The old tests passed because they called `is_orphaned_tunnel` with fields
    /// already split by hand, so they never touched the parse. This one feeds real output.
    #[test]
    fn the_sweep_reads_real_ps_output() {
        let (pid, ppid, cmd) = parse_ps_row("  134     1 /usr/libexec/logd").expect("padded row");
        assert_eq!((pid, ppid), (134, 1), "right-aligned columns are one field each");
        assert_eq!(cmd, "/usr/libexec/logd");

        // The command keeps its interior spacing — ` -R ` is matched inside it.
        let (_, _, cmd) = parse_ps_row(REAL_PS_OUTPUT.lines().last().unwrap()).unwrap();
        assert!(cmd.contains(" -R "), "command survives verbatim: {cmd}");

        // Rows that are not three fields, or not numeric, are skipped rather than panicking.
        assert!(parse_ps_row("").is_none());
        assert!(parse_ps_row("12345").is_none());
        assert!(parse_ps_row("  pid  ppid command").is_none());

        let prod = "ControlPath=/Users/d/.maiterm/cm/";
        let found = orphaned_tunnel_pids(REAL_PS_OUTPUT, prod);
        assert_eq!(
            found.iter().map(|(p, _)| *p).collect::<Vec<_>>(),
            vec![35452],
            "only the reparented tunnel: not launchd, not logd, not the live app's own tunnel"
        );

        // And the isolation still holds through the real-output path.
        assert!(
            orphaned_tunnel_pids(REAL_PS_OUTPUT, "ControlPath=/Users/d/.maiterm/cm-dev/").is_empty(),
            "a prod dump holds nothing for the dev sweep"
        );
    }

    /// The draw must actually vary. `subsec_nanos()` was the first source used here and is a
    /// trap: macOS's realtime clock is microsecond-granular, so it is always a multiple of
    /// 1000 and `% REMOTE_PORT_SPAN` was always ZERO — every install asked for the base port,
    /// making a collision certain between any two instances instead of unlikely.
    #[test]
    fn the_instance_port_is_not_the_same_on_every_install() {
        use rand::Rng;
        let mut rng = rand::thread_rng();
        let drawn: std::collections::HashSet<u16> = (0..500)
            .map(|_| REMOTE_PORT_BASE + rng.gen_range(0..REMOTE_PORT_SPAN))
            .collect();
        assert!(drawn.len() > 100, "draw barely varies: {} distinct in 500", drawn.len());
        assert!(drawn
            .iter()
            .all(|p| (REMOTE_PORT_BASE..REMOTE_PORT_BASE + REMOTE_PORT_SPAN).contains(p)));
    }

    /// Candidates must stay inside the range the preferred port was drawn from — walking
    /// off the end would put us in the remote's ephemeral range, where the kernel's own
    /// outbound connections can take the port out from under us.
    #[test]
    fn candidates_wrap_inside_the_range() {
        let last = REMOTE_PORT_BASE + REMOTE_PORT_SPAN - 1;
        assert_eq!(next_port_candidate(last), REMOTE_PORT_BASE);
        assert_eq!(next_port_candidate(REMOTE_PORT_BASE), REMOTE_PORT_BASE + 1);
        // A port from outside the range (an older file, a hand-edit) still lands inside it.
        let stray = next_port_candidate(45015);
        assert!((REMOTE_PORT_BASE..REMOTE_PORT_BASE + REMOTE_PORT_SPAN).contains(&stray));
    }

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
