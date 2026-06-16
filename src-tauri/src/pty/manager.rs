use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::io::{Read, Write};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::Duration;
use tauri::{AppHandle, Emitter};

use crate::state::{AppState, PtyCommand, PtyHandle, PtyStats};
use crate::state::persistence::app_data_slug;
use crate::terminal::event_proxy::AitermEventProxy;
use crate::terminal::handle::create_terminal;
use crate::terminal::osc::OscEvent;
use crate::terminal::render;

pub fn spawn_pty(
    app_handle: &AppHandle,
    state: &Arc<AppState>,
    pty_id: &str,
    tab_id: &str,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) -> Result<(), String> {
    log::info!("spawn_pty: pty_id={}, tab_id={}, cols={}, rows={}", pty_id, tab_id, cols, rows);

    // Auto-kill any previous PTY for this tab (handles HMR remount, frontend crash, etc.)
    {
        let mut tab_map = state.tab_pty_map.write();
        if let Some(old_pty_id) = tab_map.remove(tab_id) {
            if old_pty_id != pty_id {
                log::info!("spawn_pty: replacing PTY old={}, new={}, tab_id={}", old_pty_id, pty_id, tab_id);
                if let Some(old_handle) = state.pty_registry.write().remove(&old_pty_id) {
                    let _ = old_handle.sender.send(PtyCommand::Kill);
                }
                state.pty_stats.write().remove(&old_pty_id);
            }
        }
        tab_map.insert(tab_id.to_string(), pty_id.to_string());
    }

    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    // --- Platform-specific shell detection and setup ---
    #[cfg(unix)]
    let mut cmd = {
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
        let shell_name = std::path::Path::new(&shell)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("zsh");

        let mut cmd = CommandBuilder::new(&shell);

        // Expose tab ID so processes (e.g. Claude Code) can identify their terminal
        cmd.env("AITERM_TAB_ID", tab_id);

        // Expose MCP server port so hooks can scope to this maiTerm instance
        // (prevents dev/prod cross-talk when both are running)
        if let Some(port) = state.mcp_port.read().as_ref() {
            cmd.env("AITERM_PORT", port.to_string());
        }

        // Most shells use -l for login, fish uses --login
        match shell_name {
            "fish" => { cmd.arg("--login"); }
            "bash" | "zsh" | "sh" | "ksh" | "tcsh" | "csh" => { cmd.arg("-l"); }
            _ => { cmd.arg("-l"); }
        }

        // macOS-specific: disable bash session save/restore and deprecation warning
        #[cfg(target_os = "macos")]
        if shell_name == "bash" {
            cmd.env("SHELL_SESSION_HISTORY", "0");
            cmd.env("SHELL_SESSION_DID_INIT", "1");
            cmd.env("BASH_SILENCE_DEPRECATION_WARNING", "1");
        }

        // Set up per-tab history file
        let safe_tab_id = tab_id.replace(['/', '\\', '.'], "");
        if let Some(data_dir) = dirs::data_dir() {
            let history_dir = data_dir.join(app_data_slug()).join("history");
            let _ = std::fs::create_dir_all(&history_dir);
            let history_file = history_dir.join(format!("{}.history", safe_tab_id));
            let history_path = history_file.to_string_lossy().to_string();

            match shell_name {
                "bash" | "zsh" | "sh" | "ksh" => {
                    cmd.env("HISTFILE", &history_path);
                }
                "fish" => {
                    cmd.env("fish_history", &safe_tab_id);
                }
                _ => {
                    cmd.env("HISTFILE", &history_path);
                }
            }
        }

        // Shell integration: configure shell hooks for title, command completion, and l() function
        let prefs = state.app_data.read().preferences.clone();
        let shell_title_integration = prefs.shell_title_integration;
        let shell_integration = prefs.shell_integration;

        // Guarded one-time source of l() function (ls with OSC 8 file links)
        let l_fn_prefix = match write_ls_function() {
            Ok(path) => format!(
                r#"[[ -z "$__aiterm_l" ]] && __aiterm_l=1 && source "{}"; "#,
                path.display()
            ),
            Err(_) => String::new(),
        };

        match shell_name {
            "bash" => {
                if shell_integration {
                    let title_part = if shell_title_integration {
                        r#" printf "\033]0;%s@%s:%s\007" "${USER}" "${HOSTNAME%%.*}" "${PWD/#$HOME/~}";"#
                    } else { "" };
                    let prompt_cmd = format!(
                        concat!(
                            // Capture the just-finished command's exit code FIRST.
                            // l_fn_prefix's `[[ -z "$__aiterm_l" ]] && …` guard runs
                            // every prompt and, once seeded, fails the test (exit 1),
                            // which would clobber $? before we read it.
                            r#"__aiterm_ec=$?; "#,
                            "{}",
                            r#"[[ -z "$__aiterm_trap" ]] && __aiterm_trap=1 &&"#,
                            r#" trap '[[ "$__aiterm_at_prompt" == 1 ]] && __aiterm_at_prompt= && printf "\033]133;B\007"' DEBUG;"#,
                            r#" printf '\033]133;D;%d\007' "$__aiterm_ec"; printf '\033]133;A\007';"#,
                            r#"{}"#,
                            r#" __aiterm_at_prompt=1"#,
                        ),
                        l_fn_prefix,
                        title_part,
                    );
                    cmd.env("PROMPT_COMMAND", prompt_cmd);
                } else if shell_title_integration {
                    cmd.env(
                        "PROMPT_COMMAND",
                        format!(
                            r#"{}printf "\033]0;%s@%s:%s\007" "${{USER}}" "${{HOSTNAME%%.*}}" "${{PWD/#$HOME/~}}""#,
                            l_fn_prefix
                        ),
                    );
                } else if !l_fn_prefix.is_empty() {
                    cmd.env("PROMPT_COMMAND", l_fn_prefix.trim_end_matches("; "));
                }
            }
            "zsh" => {
                if shell_title_integration || shell_integration {
                    if let Ok(integration_dir) = setup_zsh_integration(shell_title_integration, shell_integration) {
                        let real_zdotdir = std::env::var("ZDOTDIR")
                            .unwrap_or_else(|_| {
                                dirs::home_dir()
                                    .map(|h| h.to_string_lossy().to_string())
                                    .unwrap_or_default()
                            });
                        cmd.env("AITERM_REAL_ZDOTDIR", &real_zdotdir);
                        cmd.env("ZDOTDIR", integration_dir.to_string_lossy().to_string());
                    }
                }
            }
            "fish" => {
                if shell_title_integration || shell_integration {
                    let mut parts: Vec<String> = Vec::new();
                    if shell_integration {
                        parts.push(
                            r#"function __aiterm_osc133 --on-event fish_prompt; printf '\e]133;D;%d\a\e]133;A\a' $status; end"#.to_string()
                        );
                        parts.push(
                            r#"function __aiterm_osc133_preexec --on-event fish_preexec; printf '\e]133;B\a'; end"#.to_string()
                        );
                    }
                    if shell_title_integration {
                        parts.push(
                            r#"function fish_title; printf '%s@%s:%s' $USER (prompt_hostname) (prompt_pwd); end"#.to_string()
                        );
                    }
                    cmd.arg("-C");
                    cmd.arg(parts.join("; "));
                }
            }
            _ => {}
        }

        cmd
    };

    #[cfg(windows)]
    let mut cmd = {
        let prefs = state.app_data.read().preferences.clone();
        let shell_path = resolve_windows_shell(&prefs.windows_shell);
        CommandBuilder::new(&shell_path)
    };

    // --- Cross-platform environment setup ---
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");
    cmd.env("TERM_PROGRAM", "aiterm");
    for m in crate::state::agent_runtime::AGENT_ENV_MARKERS {
        cmd.env_remove(m);
    }

    // Set working directory — use provided cwd (from split) or fall back to home
    if let Some(ref dir) = cwd {
        // Expand ~ to home directory (shells do this, but std::path doesn't)
        let expanded = if dir.starts_with("~/") || dir == "~" {
            dirs::home_dir().map(|h| h.join(&dir[if dir.len() > 1 { 2 } else { 1 }..]).to_string_lossy().to_string())
                .unwrap_or_else(|| dir.clone())
        } else {
            dir.clone()
        };
        let path = std::path::Path::new(&expanded);
        if path.is_dir() {
            cmd.cwd(path);
        } else if let Some(home) = dirs::home_dir() {
            cmd.cwd(home);
        }
    } else if let Some(home) = dirs::home_dir() {
        cmd.cwd(home);
    }

    #[cfg(unix)]
    if let Some(home) = dirs::home_dir() {
        cmd.env("HOME", home.to_string_lossy().to_string());
    }

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| {
        log::error!("Failed to spawn command: {}", e);
        e.to_string()
    })?;

    let child_pid = child.process_id();

    // Drop the slave - this is important! The shell won't start properly if we keep it open
    drop(pair.slave);

    // Get reader and writer from master
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;
    let mut writer = pair.master.take_writer().map_err(|e| e.to_string())?;

    // Create channel for commands
    let (tx, rx) = mpsc::channel::<PtyCommand>();
    let tx_for_proxy = tx.clone();

    // Store PTY handle with child PID
    {
        let mut registry = state.pty_registry.write();
        registry.insert(pty_id.to_string(), PtyHandle { sender: tx, child_pid });
    }

    // Initialize per-PTY stats
    {
        use std::sync::atomic::AtomicU64;
        let mut stats = state.pty_stats.write();
        stats.insert(pty_id.to_string(), PtyStats {
            bytes_written: AtomicU64::new(0),
            bytes_read: AtomicU64::new(0),
            last_read_ms: AtomicU64::new(0),
        });
    }

    // Create alacritty_terminal instance
    {
        let scrollback_limit = {
            let app_data = state.app_data.read();
            let limit = app_data.preferences.scrollback_limit;
            if limit == 0 { 100_000 } else { limit as usize }
        };

        let event_proxy = AitermEventProxy {
            pty_id: pty_id.to_string(),
            app_handle: app_handle.clone(),
            pty_sender: tx_for_proxy,
        };

        let terminal_handle = create_terminal(cols, rows, scrollback_limit, event_proxy);
        state.terminal_registry.write().insert(pty_id.to_string(), terminal_handle);
    }

    // Spawn writer thread (with PTY registry cleanup on exit)
    let master = pair.master;
    let state_clone = Arc::clone(state);
    let pty_id_owned = pty_id.to_string();
    let tab_id_owned = tab_id.to_string();
    thread::spawn(move || {
        loop {
            match rx.recv_timeout(Duration::from_millis(100)) {
                Ok(PtyCommand::Write(data)) => {
                    if writer.write_all(&data).is_err() {
                        break;
                    }
                    let _ = writer.flush();
                }
                Ok(PtyCommand::Resize { cols, rows }) => {
                    let _ = master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    });
                }
                Ok(PtyCommand::Kill) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Check if child is still alive
                    if let Ok(Some(_)) = child.try_wait() {
                        break;
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        // Drop writer and master to unblock the reader thread.
        // On Windows, the reader can block on read() indefinitely if the
        // master handle isn't closed after the child exits.
        drop(writer);
        drop(master);
        // Cleanup: remove PTY handle, terminal handle, and tab mapping on exit
        state_clone.pty_registry.write().remove(&pty_id_owned);
        state_clone.terminal_registry.write().remove(&pty_id_owned);
        // Only remove tab mapping if it still points to this PTY (a new spawn may have already replaced it)
        {
            let mut tab_map = state_clone.tab_pty_map.write();
            if tab_map.get(&tab_id_owned).map(|id| id == &pty_id_owned).unwrap_or(false) {
                tab_map.remove(&tab_id_owned);
            }
        }
    });

    // Spawn reader thread — feeds PTY output through OSC interceptor + alacritty_terminal,
    // then emits rendered frames to the frontend
    let pty_id_clone = pty_id.to_string();
    let tab_id_reader = tab_id.to_string();
    let app_handle_clone = app_handle.clone();
    let state_reader = Arc::clone(state);

    thread::spawn(move || {
        let mut buf = [0u8; 4096];

        loop {
            match reader.read(&mut buf) {
                Ok(0) => {
                    break;
                }
                Ok(n) => {
                    // Track bytes read for diagnostics + resize coalescing
                    {
                        use std::sync::atomic::Ordering;
                        let stats = state_reader.pty_stats.read();
                        if let Some(s) = stats.get(&pty_id_clone) {
                            s.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
                            s.last_read_ms.store(epoch_millis(), Ordering::Relaxed);
                        }
                    }
                    let data = &buf[..n];

                    // Process through OscInterceptor → emit OSC events
                    let osc_events = {
                        let mut registry = state_reader.terminal_registry.write();
                        if let Some(handle) = registry.get_mut(&pty_id_clone) {
                            handle.osc_interceptor.process(data)
                        } else {
                            vec![]
                        }
                    };
                    for osc_event in osc_events {
                        match osc_event {
                            OscEvent::Cwd { cwd, host } => {
                                let _ = app_handle_clone.emit(
                                    &format!("term-osc7-{}", pty_id_clone),
                                    serde_json::json!({ "cwd": cwd, "host": host }),
                                );
                            }
                            OscEvent::ShellIntegration { cmd, exit_code } => {
                                let _ = app_handle_clone.emit(
                                    &format!("term-osc133-{}", pty_id_clone),
                                    serde_json::json!({ "cmd": cmd.to_string(), "exit_code": exit_code }),
                                );
                            }
                            OscEvent::Notification { message } => {
                                let _ = app_handle_clone.emit(
                                    &format!("term-notification-{}", pty_id_clone),
                                    message,
                                );
                            }
                            OscEvent::CurrentDir { cwd } => {
                                let _ = app_handle_clone.emit(
                                    &format!("term-osc7-{}", pty_id_clone),
                                    serde_json::json!({ "cwd": cwd, "host": null }),
                                );
                            }
                        }
                    }

                    // Feed bytes to alacritty_terminal VTE parser.
                    // Temporarily move our external selection onto term.selection so
                    // alacritty's scroll handlers rotate it correctly when new output
                    // pushes content up. Read it back after processing.
                    {
                        let mut registry = state_reader.terminal_registry.write();
                        if let Some(handle) = registry.get_mut(&pty_id_clone) {
                            handle.term.selection = handle.selection.take();
                            handle.processor.advance(&mut handle.term, data);
                            handle.selection = handle.term.selection.take();
                        }
                    }

                    // Render viewport frame and emit to frontend after every read.
                    // No throttling — render_viewport() is fast (grid iteration) and
                    // the read() blocking naturally rate-limits during burst output.
                    // Throttling caused missed final frames when no more data arrives.
                    {
                        let registry = state_reader.terminal_registry.read();
                        if let Some(handle) = registry.get(&pty_id_clone) {
                            let frame = render::render_viewport(&handle.term, handle.selection.as_ref());
                            let _ = app_handle_clone.emit(
                                &format!("term-frame-{}", pty_id_clone),
                                &frame,
                            );
                        }
                    }

                    // Emit raw bytes for trigger engine (frontend, temporary bridge)
                    let _ = app_handle_clone.emit(
                        &format!("pty-raw-{}", pty_id_clone),
                        data.to_vec(),
                    );
                }
                Err(_) => {
                    break;
                }
            }
        }

        // Emit final frame before closing
        {
            let registry = state_reader.terminal_registry.read();
            if let Some(handle) = registry.get(&pty_id_clone) {
                let frame = render::render_viewport(&handle.term, handle.selection.as_ref());
                let _ = app_handle_clone.emit(
                    &format!("term-frame-{}", pty_id_clone),
                    &frame,
                );
            }
        }

        // Only emit close event if this PTY wasn't replaced by a new spawn for the same tab.
        let was_replaced = {
            let tab_map = state_reader.tab_pty_map.read();
            tab_map.get(&tab_id_reader).map(|id| id != &pty_id_clone).unwrap_or(false)
        };
        if !was_replaced {
            let event_name = format!("pty-close-{}", pty_id_clone);
            let _ = app_handle_clone.emit(&event_name, ());
        } else {
            log::info!("spawn_pty: suppressing pty-close for replaced PTY {}, tab_id={}", pty_id_clone, tab_id_reader);
        }
    });

    Ok(())
}

pub fn write_pty(state: &Arc<AppState>, pty_id: &str, data: &[u8]) -> Result<(), String> {
    // Track bytes written for diagnostics
    {
        use std::sync::atomic::Ordering;
        let stats = state.pty_stats.read();
        if let Some(s) = stats.get(pty_id) {
            s.bytes_written.fetch_add(data.len() as u64, Ordering::Relaxed);
        }
    }
    let registry = state.pty_registry.read();
    let handle = registry.get(pty_id).ok_or("PTY not found")?;
    handle
        .sender
        .send(PtyCommand::Write(data.to_vec()))
        .map_err(|e| e.to_string())
}

/// Output within this window means a TUI is actively drawing — resizes are
/// then coalesced instead of applied per-event. Streaming output arrives in
/// bursts with gaps of several hundred ms, so this must comfortably exceed a
/// burst gap; an active TUI's spinner keeps output well inside one second.
const RESIZE_OUTPUT_HOT_MS: u64 = 1000;
/// Trailing debounce for deferred resizes: apply once requests stop arriving
/// for this long. One gesture (window drag, panel toggle storm) = one SIGWINCH.
const RESIZE_DEBOUNCE_MS: u64 = 250;

pub fn epoch_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Resize the PTY + alacritty grid together.
///
/// TUIs (Claude Code/Ink) re-render their retained transcript on every width
/// change; mid-stream, the previous rendering has already scrolled into
/// history where it can't be erased, so each SIGWINCH leaves a permanent
/// duplicate block in scrollback. To minimize that, resize requests that
/// arrive while the PTY is actively streaming are coalesced with a trailing
/// debounce and only the final size is applied — and not at all if it matches
/// the current grid (e.g. an 80×24→fitted flap during tab reattach).
pub fn resize_pty(state: &Arc<AppState>, pty_id: &str, cols: u16, rows: u16) -> Result<(), String> {
    if !state.pty_registry.read().contains_key(pty_id) {
        return Err("PTY not found".to_string());
    }

    let pending_exists = {
        let mut pending = state.pending_resizes.write();
        match pending.get_mut(pty_id) {
            Some(entry) => {
                // An applier is already waiting — just update the target size.
                entry.cols = cols;
                entry.rows = rows;
                entry.last_request = std::time::Instant::now();
                true
            }
            None => false,
        }
    };
    if pending_exists {
        return Ok(());
    }

    // No-op resizes never reach the PTY.
    if state.live_grid_size(pty_id) == Some((cols, rows)) {
        return Ok(());
    }

    let output_hot = {
        use std::sync::atomic::Ordering;
        let stats = state.pty_stats.read();
        stats
            .get(pty_id)
            .map(|s| epoch_millis().saturating_sub(s.last_read_ms.load(Ordering::Relaxed)) < RESIZE_OUTPUT_HOT_MS)
            .unwrap_or(false)
    };

    if !output_hot {
        return apply_resize(state, pty_id, cols, rows);
    }

    state.pending_resizes.write().insert(
        pty_id.to_string(),
        crate::state::PendingResize { cols, rows, last_request: std::time::Instant::now() },
    );
    spawn_resize_applier(Arc::clone(state), pty_id.to_string());
    Ok(())
}

/// Waits out the trailing debounce, then applies the latest pending size.
fn spawn_resize_applier(state: Arc<AppState>, pty_id: String) {
    thread::spawn(move || {
        loop {
            thread::sleep(Duration::from_millis(50));

            if !state.pty_registry.read().contains_key(&pty_id) {
                state.pending_resizes.write().remove(&pty_id);
                return;
            }

            let ready = {
                let pending = state.pending_resizes.read();
                match pending.get(&pty_id) {
                    None => return, // already applied/cleared
                    Some(e) => e.last_request.elapsed() >= Duration::from_millis(RESIZE_DEBOUNCE_MS),
                }
            };
            if !ready {
                continue;
            }

            let (cols, rows, snapshot) = {
                let pending = state.pending_resizes.read();
                match pending.get(&pty_id) {
                    None => return,
                    Some(e) => (e.cols, e.rows, e.last_request),
                }
            };

            if state.live_grid_size(&pty_id) != Some((cols, rows)) {
                let _ = apply_resize(&state, &pty_id, cols, rows);
            }

            // Remove the entry only if no newer request landed while applying;
            // otherwise loop and debounce the newer one too.
            let mut pending = state.pending_resizes.write();
            if pending.get(&pty_id).map(|e| e.last_request == snapshot).unwrap_or(false) {
                pending.remove(&pty_id);
                return;
            }
        }
    });
}

/// Immediately resize the kernel PTY and the alacritty grid.
fn apply_resize(state: &Arc<AppState>, pty_id: &str, cols: u16, rows: u16) -> Result<(), String> {
    {
        let registry = state.pty_registry.read();
        let handle = registry.get(pty_id).ok_or("PTY not found")?;
        handle
            .sender
            .send(PtyCommand::Resize { cols, rows })
            .map_err(|e| e.to_string())?;
    }

    // Also resize the alacritty_terminal instance
    {
        use crate::terminal::handle::TermDimensions;
        let mut term_registry = state.terminal_registry.write();
        if let Some(term_handle) = term_registry.get_mut(pty_id) {
            term_handle.term.resize(TermDimensions {
                cols: cols as usize,
                rows: rows as usize,
            });
        }
    }

    Ok(())
}

pub fn kill_pty(state: &Arc<AppState>, pty_id: &str) -> Result<(), String> {
    let mut registry = state.pty_registry.write();

    if let Some(handle) = registry.remove(pty_id) {
        let _ = handle.sender.send(PtyCommand::Kill);
    }

    // Clean up terminal registry
    state.terminal_registry.write().remove(pty_id);

    // Drop any resize still waiting on the debounce
    state.pending_resizes.write().remove(pty_id);

    // Clean up tab → pty mapping (reverse lookup)
    {
        let mut tab_map = state.tab_pty_map.write();
        tab_map.retain(|_, v| v != pty_id);
    }

    Ok(())
}

/// Info returned when querying a PTY for split cloning
#[derive(serde::Serialize, Clone)]
pub struct PtyInfo {
    pub cwd: Option<String>,
    pub foreground_command: Option<String>,
}

pub fn get_pty_info(state: &Arc<AppState>, pty_id: &str) -> Result<PtyInfo, String> {
    let registry = state.pty_registry.read();
    let handle = registry.get(pty_id).ok_or("PTY not found")?;
    let pid = handle.child_pid.ok_or("No child PID")?;

    let cwd = get_cwd_for_pid(pid);
    let foreground_command = get_foreground_command(pid);

    Ok(PtyInfo { cwd, foreground_command })
}

/// Get the current working directory of a process via /proc (Linux)
#[cfg(target_os = "linux")]
fn get_cwd_for_pid(pid: u32) -> Option<String> {
    std::fs::read_link(format!("/proc/{}/cwd", pid))
        .ok()
        .and_then(|p| p.into_os_string().into_string().ok())
}

/// Get the current working directory of a process via lsof (macOS)
#[cfg(target_os = "macos")]
fn get_cwd_for_pid(pid: u32) -> Option<String> {
    let output = std::process::Command::new("lsof")
        .args(["-a", "-d", "cwd", "-p", &pid.to_string(), "-Fn"])
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    for line in stdout.lines() {
        if let Some(path) = line.strip_prefix('n') {
            if path.starts_with('/') {
                return Some(path.to_string());
            }
        }
    }
    None
}

#[cfg(windows)]
fn get_cwd_for_pid(pid: u32) -> Option<String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    let mut sys = System::new();
    let refresh = ProcessRefreshKind::nothing().with_cwd(UpdateKind::Always);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[Pid::from_u32(pid)]),
        true,
        refresh,
    );
    sys.process(Pid::from_u32(pid))
        .and_then(|p| {
            let cwd = p.cwd()?;
            Some(cwd.to_string_lossy().into_owned())
        })
        .filter(|s| !s.is_empty())
}

/// Check if a command string looks like an SSH/remote connection command
#[cfg(unix)]
fn is_ssh_command(cmd: &str) -> bool {
    let base = cmd.split_whitespace().next().unwrap_or("");
    let basename = std::path::Path::new(base)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(base);
    matches!(basename, "ssh" | "mosh" | "autossh")
}

/// Get the foreground process command via ps (Unix)
///
/// We rely on the tty's foreground process group (tpgid) instead of "any ssh
/// descendant of the shell": a subprocess that the foreground app happens to
/// spawn (e.g. Claude running ssh as a worker for one of its Bash tool calls)
/// must NOT make the shell look remote. Only the actual foreground job counts.
#[cfg(unix)]
fn get_foreground_command(shell_pid: u32) -> Option<String> {
    // macOS BSD ps uses -x (show processes without controlling terminal)
    // Linux procps uses -e (select all processes) for equivalent behavior
    #[cfg(target_os = "macos")]
    let args: &[&str] = &["-o", "pid=,pgid=,tpgid=,command=", "-x"];
    #[cfg(target_os = "linux")]
    let args: &[&str] = &["-e", "-o", "pid=,pgid=,tpgid=,command="];

    let output = std::process::Command::new("ps")
        .args(args)
        .output()
        .ok()?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    struct Row {
        pid: u32,
        pgid: u32,
        tpgid: i32,
        cmd: String,
    }

    let mut rows: Vec<Row> = Vec::new();
    for line in stdout.lines() {
        let trimmed = line.trim();
        // split_whitespace collapses multiple spaces — critical because ps
        // right-justifies the numeric columns with variable-width padding.
        let mut iter = trimmed.split_whitespace();
        let pid: u32 = match iter.next().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let pgid: u32 = match iter.next().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let tpgid: i32 = match iter.next().and_then(|s| s.parse().ok()) {
            Some(p) => p,
            None => continue,
        };
        let cmd: String = iter.collect::<Vec<&str>>().join(" ");
        if cmd.is_empty() {
            continue;
        }
        rows.push(Row { pid, pgid, tpgid, cmd });
    }

    let shell_row = rows.iter().find(|r| r.pid == shell_pid)?;

    // tpgid <= 0 → no controlling-tty foreground pgid (rare for an interactive
    // shell, but treat as "no foreground command").
    // tpgid == shell pgid → the shell itself is the foreground job (sitting at
    // its prompt), so any ssh elsewhere in the tree is a background/worker job.
    if shell_row.tpgid <= 0 || (shell_row.tpgid as u32) == shell_row.pgid {
        return None;
    }

    let tpgid = shell_row.tpgid as u32;
    let leader = rows.iter().find(|r| r.pid == tpgid)?;

    // Only report ssh when the foreground job leader itself is ssh. Subprocesses
    // an app spawns under the hood share its pgid but aren't what the user is
    // interacting with.
    if is_ssh_command(&leader.cmd) {
        Some(leader.cmd.clone())
    } else {
        None
    }
}

/// Whether an agent CLI (one of `proc_names`, matched by argv0/comm basename) is
/// alive anywhere in the descendant process tree rooted at `shell_pid`.
///
/// The dormancy reaper uses this to distinguish "the agent CLI is still running in
/// this tab's shell" from "the agent exited and the dot should clear". Walking the
/// whole descendant tree (not just the tty foreground leader) keeps a backgrounded
/// (Ctrl-Z) or tool-spawning agent from being mistaken for gone. Cross-platform —
/// reuses the same `sysinfo` parent→children idiom as `get_foreground_command`.
pub fn agent_process_alive(shell_pid: u32, proc_names: &[&str]) -> bool {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
    if proc_names.is_empty() {
        return false;
    }

    let mut sys = System::new();
    let refresh = ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);

    let basename_lower = |s: &str| -> String {
        std::path::Path::new(s)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or(s)
            .to_ascii_lowercase()
    };

    // Build parent → children adjacency and mark which pids are the agent binary
    // (argv0 or comm basename matches). Same refresh/idiom as get_foreground_command.
    let mut children: std::collections::HashMap<u32, Vec<u32>> = std::collections::HashMap::new();
    let mut is_agent: std::collections::HashMap<u32, bool> = std::collections::HashMap::new();
    for (pid, process) in sys.processes() {
        let pid = pid.as_u32();
        if let Some(ppid) = process.parent() {
            children.entry(ppid.as_u32()).or_default().push(pid);
        }
        let name = basename_lower(&process.name().to_string_lossy());
        let argv0 = process
            .cmd()
            .first()
            .map(|s| basename_lower(&s.to_string_lossy()))
            .unwrap_or_default();
        let matched = proc_names.iter().any(|c| {
            let c = c.to_ascii_lowercase();
            name == c || argv0 == c
        });
        is_agent.insert(pid, matched);
    }

    // BFS descendants of the shell (the shell row itself is not treated as a match).
    let mut stack = vec![shell_pid];
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    while let Some(pid) = stack.pop() {
        if !seen.insert(pid) {
            continue;
        }
        if let Some(kids) = children.get(&pid) {
            for &kid in kids {
                if is_agent.get(&kid).copied().unwrap_or(false) {
                    return true;
                }
                stack.push(kid);
            }
        }
    }
    false
}

#[cfg(windows)]
fn is_ssh_command_win(cmd: &[std::ffi::OsString]) -> bool {
    let exe = cmd.first().map(|s| s.to_string_lossy()).unwrap_or_default();
    let basename = std::path::Path::new(exe.as_ref())
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(basename.as_str(), "ssh" | "mosh" | "autossh")
}

#[cfg(windows)]
fn is_ssh_wrapper_win(cmd: &[std::ffi::OsString]) -> bool {
    let exe = cmd.first().map(|s| s.to_string_lossy()).unwrap_or_default();
    let basename = std::path::Path::new(exe.as_ref())
        .file_stem()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    matches!(basename.as_str(), "scp" | "rsync" | "git" | "sftp" | "git-remote-ssh")
}

#[cfg(windows)]
fn get_foreground_command(shell_pid: u32) -> Option<String> {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    let mut sys = System::new();
    let refresh = ProcessRefreshKind::nothing().with_cmd(UpdateKind::Always);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, refresh);

    // Build parent → children map
    let mut children: std::collections::HashMap<u32, Vec<(u32, Vec<std::ffi::OsString>)>> =
        std::collections::HashMap::new();
    for (pid, process) in sys.processes() {
        if let Some(ppid) = process.parent() {
            children
                .entry(ppid.as_u32())
                .or_default()
                .push((pid.as_u32(), process.cmd().to_vec()));
        }
    }

    // Walk tree from shell_pid, preferring SSH children
    let mut current_pid = shell_pid;
    let mut ssh_cmd: Option<String> = None;
    let mut parent_cmd: Option<Vec<std::ffi::OsString>> = None;

    loop {
        if let Some(kids) = children.get(&current_pid) {
            if kids.is_empty() {
                break;
            }
            let chosen = kids
                .iter()
                .find(|(_, cmd)| is_ssh_command_win(cmd))
                .or_else(|| kids.first());
            if let Some((kid_pid, kid_cmd)) = chosen {
                if is_ssh_command_win(kid_cmd) {
                    // Don't count ssh children of non-interactive wrappers (scp, rsync, git, sftp)
                    let is_wrapper_child = parent_cmd.as_ref().map_or(false, |p| is_ssh_wrapper_win(p));
                    if !is_wrapper_child {
                        let cmd_str: Vec<String> = kid_cmd
                            .iter()
                            .map(|s| s.to_string_lossy().into_owned())
                            .collect();
                        ssh_cmd = Some(cmd_str.join(" "));
                    }
                }
                parent_cmd = Some(kid_cmd.clone());
                current_pid = *kid_pid;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    ssh_cmd
}

/// Resolve a Windows shell preference ID to an executable path.
/// Falls back to powershell.exe if the configured shell can't be found.
#[cfg(windows)]
fn resolve_windows_shell(id: &str) -> String {
    match id {
        "cmd" => {
            std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
        }
        "pwsh" => {
            // Check if pwsh is available, fall back to powershell
            if let Ok(output) = std::process::Command::new("where").arg("pwsh").output() {
                if output.status.success() {
                    return String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("pwsh.exe")
                        .trim()
                        .to_string();
                }
            }
            "powershell.exe".to_string()
        }
        "gitbash" => {
            let paths = [
                r"C:\Program Files\Git\bin\bash.exe",
                r"C:\Program Files (x86)\Git\bin\bash.exe",
            ];
            for p in &paths {
                if std::path::Path::new(p).exists() {
                    return p.to_string();
                }
            }
            // Git Bash not found, fall back to powershell
            "powershell.exe".to_string()
        }
        "wsl" => {
            if let Ok(output) = std::process::Command::new("where").arg("wsl").output() {
                if output.status.success() {
                    return String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or("wsl.exe")
                        .trim()
                        .to_string();
                }
            }
            "powershell.exe".to_string()
        }
        // "powershell" or any unknown value
        _ => "powershell.exe".to_string(),
    }
}

/// Create zsh integration directory with shim files that source the user's
/// real config and add precmd hooks for title and/or command completion.
#[cfg(unix)]
fn setup_zsh_integration(title: bool, shell_integration: bool) -> Result<std::path::PathBuf, String> {
    let data_dir = dirs::data_dir().ok_or("No data directory")?;
    let zsh_dir = data_dir.join(app_data_slug()).join("shell-integration").join("zsh");
    std::fs::create_dir_all(&zsh_dir).map_err(|e| e.to_string())?;

    let zshenv_content = r#"# maiTerm shell integration - do not edit
if [[ -n "$AITERM_REAL_ZDOTDIR" ]]; then
  [[ -f "$AITERM_REAL_ZDOTDIR/.zshenv" ]] && source "$AITERM_REAL_ZDOTDIR/.zshenv"
else
  [[ -f "$HOME/.zshenv" ]] && source "$HOME/.zshenv"
fi
"#;

    let mut hooks = String::new();
    hooks.push_str("# maiTerm shell integration - do not edit\n");
    hooks.push_str("if [[ -n \"$AITERM_REAL_ZDOTDIR\" ]]; then\n");
    hooks.push_str("  ZDOTDIR=\"$AITERM_REAL_ZDOTDIR\"\n");
    hooks.push_str("  [[ -f \"$ZDOTDIR/.zshrc\" ]] && source \"$ZDOTDIR/.zshrc\"\n");
    hooks.push_str("else\n");
    hooks.push_str("  ZDOTDIR=\"$HOME\"\n");
    hooks.push_str("  [[ -f \"$HOME/.zshrc\" ]] && source \"$HOME/.zshrc\"\n");
    hooks.push_str("fi\n\n");
    hooks.push_str("autoload -Uz add-zsh-hook\n");

    if shell_integration {
        hooks.push_str("_aiterm_osc133_precmd() {\n");
        hooks.push_str("  print -Pn '\\e]133;D;%?\\a\\e]133;A\\a'\n");
        hooks.push_str("}\n");
        hooks.push_str("add-zsh-hook precmd _aiterm_osc133_precmd\n");
        hooks.push_str("_aiterm_osc133_preexec() {\n");
        hooks.push_str("  print -Pn '\\e]133;B\\a'\n");
        hooks.push_str("}\n");
        hooks.push_str("add-zsh-hook preexec _aiterm_osc133_preexec\n");
    }

    if title {
        hooks.push_str("_aiterm_title_precmd() {\n");
        hooks.push_str("  printf '\\033]0;%s@%s:%s\\007' \"${USER}\" \"${HOST%%.*}\" \"${PWD/#$HOME/~}\"\n");
        hooks.push_str("}\n");
        hooks.push_str("add-zsh-hook precmd _aiterm_title_precmd\n");
    }

    // Source l() function (ls with OSC 8 file links) if available
    if let Ok(l_fn_path) = write_ls_function() {
        hooks.push_str(&format!("\nsource \"{}\"\n", l_fn_path.display()));
    }

    std::fs::write(zsh_dir.join(".zshenv"), zshenv_content).map_err(|e| e.to_string())?;
    std::fs::write(zsh_dir.join(".zshrc"), &hooks).map_err(|e| e.to_string())?;

    Ok(zsh_dir)
}

/// Write the `l()` shell function (ls with OSC 8 hyperlinks) to a file.
/// When sourced, defines `l` as an ls wrapper that emits clickable file:// links.
/// Returns the path to the written file.
#[cfg(unix)]
fn write_ls_function() -> Result<std::path::PathBuf, String> {
    let data_dir = dirs::data_dir().ok_or("No data directory")?;
    let integration_dir = data_dir.join(app_data_slug()).join("shell-integration");
    std::fs::create_dir_all(&integration_dir).map_err(|e| e.to_string())?;

    let path = integration_dir.join("l_function.sh");
    let content = r#"# maiTerm: ls with clickable file links (OSC 8 hyperlinks)
unalias l 2>/dev/null
if ls --hyperlink=auto / >/dev/null 2>&1; then
  # GNU ls supports hyperlinks natively
  alias l='ls --hyperlink=auto -la'
else
  # macOS/BSD fallback: post-process ls output with awk to inject OSC 8 links
  l() {
    local _args="" _dirs=""
    local _nondash=0
    for _a in "$@"; do
      case "$_a" in -*) _args="$_args $_a";; *) _nondash=$((_nondash+1)); _dirs="$_dirs
$_a";; esac
    done
    # No non-flag args: list cwd
    if [ "$_nondash" -eq 0 ]; then
      _dirs="."
    fi
    # For a single directory arg, use it as the base dir for absolute paths
    # For multiple args or files, use per-file resolution
    if [ "$_nondash" -le 1 ]; then
      local _d
      _d=$(echo "$_dirs" | tail -1)
      local _abs
      _abs=$(cd "$_d" 2>/dev/null && pwd -P) || { ls -la "$@"; return; }
      ls -la $_args "$_d" | awk -v dir="$_abs" '
        /^total / { print; next }
        /^d/ { print; next }
        /^[lcbps-]/ {
          if (match($0, /^[^ ]+ +[0-9]+ +[^ ]+ +[^ ]+ +[0-9,]+ +[A-Za-z]+ +[0-9]+ +[0-9:]+/)) {
            pre = substr($0, 1, RLENGTH)
            rest = substr($0, RLENGTH + 1)
            sub(/^ +/, " ", rest)
            fname = substr(rest, 2)
            if (fname == "." || fname == "..") { print; next }
            link_target = ""
            if (index(fname, " -> ") > 0) {
              idx = index(fname, " -> ")
              link_target = substr(fname, idx)
              fname = substr(fname, 1, idx - 1)
            }
            fpath = dir "/" fname
            gsub(/ /, "%20", fpath)
            gsub(/\(/, "%28", fpath)
            gsub(/\)/, "%29", fpath)
            printf "%s \033]8;;file://%s\033\\%s\033]8;;\033\\%s\n", pre, fpath, fname, link_target
            next
          }
          print; next
        }
        { print }
      '
    else
      # Multiple file/dir args (e.g. globs): resolve each file individually
      local _pwd
      _pwd=$(pwd -P)
      ls -la $_args "$@" | awk -v base="$_pwd" '
        /^$/ { print; next }
        /^total / { print; next }
        /^d/ { print; next }
        /^[lcbps-]/ {
          if (match($0, /^[^ ]+ +[0-9]+ +[^ ]+ +[^ ]+ +[0-9,]+ +[A-Za-z]+ +[0-9]+ +[0-9:]+/)) {
            pre = substr($0, 1, RLENGTH)
            rest = substr($0, RLENGTH + 1)
            sub(/^ +/, " ", rest)
            fname = substr(rest, 2)
            if (fname == "." || fname == "..") { print; next }
            link_target = ""
            if (index(fname, " -> ") > 0) {
              idx = index(fname, " -> ")
              link_target = substr(fname, idx)
              fname = substr(fname, 1, idx - 1)
            }
            # fname may include dir prefix from ls (e.g. "Downloads/foo.jpg")
            if (substr(fname, 1, 1) == "/") {
              fpath = fname
            } else {
              fpath = base "/" fname
            }
            gsub(/ /, "%20", fpath)
            gsub(/\(/, "%28", fpath)
            gsub(/\)/, "%29", fpath)
            printf "%s \033]8;;file://%s\033\\%s\033]8;;\033\\%s\n", pre, fpath, fname, link_target
            next
          }
          print; next
        }
        { print }
      '
    fi
  }
fi
"#;
    std::fs::write(&path, content).map_err(|e| e.to_string())?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_process_alive_empty_names_is_false() {
        // No candidate names → nothing can match, even for a live pid.
        assert!(!agent_process_alive(std::process::id(), &[]));
    }

    #[test]
    fn agent_process_alive_dead_shell_is_false() {
        // PID 1 (init/launchd) is not the parent of any `codex`/`claude`, and a
        // wildly out-of-range pid has no descendants — both must read as gone.
        assert!(!agent_process_alive(u32::MAX, &["codex"]));
        assert!(!agent_process_alive(1, &["definitely-not-a-real-binary-xyz"]));
    }
}
