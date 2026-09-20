mod accounts;
mod claude_code;
mod commands;
mod comms;
mod mailink;
mod pty;
mod state;
mod terminal;

pub const APP_DISPLAY_NAME: &str = if cfg!(debug_assertions) { "maiTermDev" } else { "maiTerm" };
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

use state::{load_state, save_state, AppState, WindowData, Workspace};
use state::persistence::{arm_running_marker, load_memory_trend, log_previous_run_status, migrate_app_data, migrate_scrollback_to_db, reconcile_tab_liveness};
use std::sync::Arc;
use tauri::{Emitter, Manager};
use tauri::menu::{AboutMetadata, MenuBuilder, MenuItem, SubmenuBuilder};
use tauri::webview::WebviewWindowBuilder;
use tauri_plugin_log::{Target, TargetKind, RotationStrategy, TimezoneStrategy};
use log::LevelFilter;

fn build_log_plugin() -> tauri_plugin_log::Builder {
    let is_dev = cfg!(debug_assertions);
    let file_name = if is_dev { "aiterm-dev" } else { "aiterm" };
    let level = if is_dev { LevelFilter::Debug } else { LevelFilter::Info };

    let mut targets = vec![
        Target::new(TargetKind::Stdout),
        Target::new(TargetKind::LogDir { file_name: Some(file_name.into()) }),
    ];

    if is_dev {
        targets.push(Target::new(TargetKind::Webview));
    }

    tauri_plugin_log::Builder::new()
        .targets(targets)
        .level(level)
        .level_for("tao", LevelFilter::Warn)
        .level_for("hyper", LevelFilter::Warn)
        .rotation_strategy(RotationStrategy::KeepAll)
        .max_file_size(5_000_000)
        .timezone_strategy(TimezoneStrategy::UseLocal)
}

/// Raise the file-descriptor soft limit toward the hard limit. macOS GUI apps
/// start with a soft limit of 256; each open PTY costs 3 fds (master + cloned
/// reader + writer), so ~70 terminal tabs exhausts it and every subsequent
/// spawn fails with EMFILE ("Too many open files"). Returns (old, new) on a
/// successful raise so the caller can log it once the logger is up.
#[cfg(unix)]
fn raise_fd_limit() -> Option<(u64, u64)> {
    unsafe {
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) != 0 {
            return None;
        }
        let mut desired: libc::rlim_t = 65536;
        #[cfg(target_os = "macos")]
        {
            // macOS rejects rlim_cur above kern.maxfilesperproc even when the
            // hard limit reports RLIM_INFINITY.
            let mut maxfiles: libc::c_int = 0;
            let mut size = std::mem::size_of::<libc::c_int>();
            let name = std::ffi::CString::new("kern.maxfilesperproc").unwrap();
            if libc::sysctlbyname(
                name.as_ptr(),
                &mut maxfiles as *mut _ as *mut libc::c_void,
                &mut size,
                std::ptr::null_mut(),
                0,
            ) == 0
                && maxfiles > 0
            {
                desired = desired.min(maxfiles as libc::rlim_t);
            }
        }
        if lim.rlim_max != libc::RLIM_INFINITY {
            desired = desired.min(lim.rlim_max);
        }
        if desired <= lim.rlim_cur {
            return None;
        }
        let old = lim.rlim_cur;
        lim.rlim_cur = desired;
        if libc::setrlimit(libc::RLIMIT_NOFILE, &lim) == 0 {
            Some((old, desired))
        } else {
            None
        }
    }
}

#[cfg(not(unix))]
fn raise_fd_limit() -> Option<(u64, u64)> {
    None
}

/// The terminal window that held focus most recently, which outlives the focus
/// itself — see `emit_to_focused_window`. Preferences and Help never claim it:
/// they are where the user goes to *leave* a terminal window, not a window a
/// menu item like Reload Current Tab could ever have meant.
static LAST_FOCUSED_WINDOW: parking_lot::RwLock<Option<String>> = parking_lot::RwLock::new(None);

/// Deliver a menu-item event to the ONE window the click was meant for.
///
/// A menu click carries no window, so every one of these arms has to pick one,
/// and there are two silent traps in doing it. First, `WebviewWindow::emit`
/// reads as window-scoped and isn't: `impl Emitter for WebviewWindow` is empty,
/// so it takes the trait default, which hands off to the app manager and
/// reaches every webview — Reload Current Tab was reloading a tab in every
/// open window. Second, `emit_to` only closes half of that: a JS `listen()`
/// with no target registers as `EventTarget::Any`, and Any short-circuits every
/// `emit_to` filter, so each listener in +layout.svelte has to name its own
/// label as well. Both halves are required; either one alone still broadcasts.
///
/// "Focused" is not always available to answer with. The menu bar stays live
/// while Preferences or Help is key and while every window is minimized, and in
/// both cases no terminal window reports focus — so fall back to the one they
/// were last in, then to a lone window, and only then admit we don't know.
fn emit_to_focused_window(app_handle: &tauri::AppHandle, event: &str) {
    let windows: Vec<_> = app_handle
        .webview_windows()
        .into_iter()
        .filter(|(label, _)| label != "preferences" && label != "help")
        .collect();
    let last_focused = LAST_FOCUSED_WINDOW.read().clone();
    let target = windows
        .iter()
        .find(|(_, win)| win.is_focused().unwrap_or(false))
        .or_else(|| {
            last_focused
                .as_ref()
                .and_then(|label| windows.iter().find(|(l, _)| l == label))
        })
        .or_else(|| windows.first().filter(|_| windows.len() == 1));
    match target {
        Some((label, _)) => {
            let _ = app_handle.emit_to(label.as_str(), event, ());
        }
        None => log::warn!(
            "Menu event '{event}': no focused window among {} candidates",
            windows.len()
        ),
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Arm crash marker BEFORE any other init. arm_running_marker() captures
    // whether a marker file from the previous run still exists (= unclean
    // exit) and then re-writes it for this run. The captured PreviousRunInfo
    // is cached in the function's static so get_app_diagnostics can read it
    // back without us having to thread it through AppState.
    let _prev_run = arm_running_marker();

    // Raise the fd soft limit before anything can spawn PTYs or sockets.
    // Logged in setup() once the log plugin is active.
    let fd_limit_raise = raise_fd_limit();

    let app_state = Arc::new(AppState::new());

    // Load persisted state and run migration
    {
        let mut data = app_state.app_data.write();
        *data = load_state();
        migrate_app_data(&mut data);
        migrate_scrollback_to_db(&mut data, &app_state.scrollback_db);
        // One-time: clear the stale pty_id high-watermark so restore can trust
        // pty_id as "live at last shutdown" instead of inferring from timestamps.
        reconcile_tab_liveness(&mut data, &app_state.scrollback_db);
        // Flush the cleaned JSON (scrollback stripped + liveness reconciled) to disk
        let _ = save_state(&data);

        // Sweep scrollback DB for rows whose tab no longer exists in state —
        // backstop for any path that drops tabs without deleting their row.
        match app_state.scrollback_db.prune_orphans(&data.all_tab_ids()) {
            Ok(n) if n > 0 => log::info!("Pruned {} orphan scrollback rows at startup", n),
            Ok(_) => {}
            Err(e) => log::warn!("Startup scrollback prune failed: {}", e),
        }

        // The same backstop for account config roots (docs/login.md §10). Removing an account
        // deletes its root, but a runtime process launched under it holds CLAUDE_CONFIG_DIR for
        // its whole life and RECREATES the directory on its next write — by which point maiTerm
        // has forgotten the account, so no other path would ever clean it up. Startup is the
        // right moment: preferences are already loaded here, so an empty list means "no
        // accounts", never "not loaded yet", and the process that resurrected the root is gone.
        for rt in accounts::ALL_RUNTIMES.iter().copied() {
            let keep: Vec<String> = data
                .preferences
                .managed_accounts
                .iter()
                .filter(|a| a.runtime == rt.slug())
                .map(|a| a.id.clone())
                .collect();
            match accounts::prune_orphan_roots(rt, &keep) {
                Ok(ids) if !ids.is_empty() => log::info!(
                    "Pruned {} orphan {} account root(s) at startup",
                    ids.len(),
                    rt.slug()
                ),
                Ok(_) => {}
                Err(e) => {
                    log::warn!("Startup account root prune failed for {}: {}", rt.slug(), e)
                }
            }
        }

        // Seed memory trend ring buffer from disk so post-mortem analysis
        // after a crash/restart still has the RSS history leading up to it.
        let persisted_trend = load_memory_trend();
        if !persisted_trend.is_empty() {
            log::info!("Loaded {} memory trend samples from disk", persisted_trend.len());
            *app_state.memory_samples.write() = persisted_trend;
        }

        // Ensure at least one window exists (fresh install)
        if data.windows.is_empty() {
            let mut win = WindowData::new("main".to_string());
            let ws = Workspace::new("Default".to_string());
            win.active_workspace_id = Some(ws.id.clone());
            win.workspaces.push(ws);
            data.windows.push(win);
        }

        // Ensure the first window has label "main" (Tauri creates this from tauri.conf.json)
        if let Some(first) = data.windows.first_mut() {
            if first.label != "main" {
                first.label = "main".to_string();
            }
        }
    }

    let builder = tauri::Builder::default()
        .plugin(build_log_plugin().build())
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .plugin(tauri_plugin_process::init())
        .plugin({
            let mut ws = tauri_plugin_window_state::Builder::new()
                .with_state_flags(tauri_plugin_window_state::StateFlags::all())
                // Only track the "main" window — dynamically created windows
                // (UUID labels) are managed by our own state system. The plugin
                // can cause WebView2 init issues on Windows for unknown labels.
                .with_filter(|label| label == "main");
            if cfg!(debug_assertions) {
                ws = ws.with_filename("window-state-dev.json");
            }
            ws.build()
        });

    #[cfg(all(feature = "mcp-bridge", debug_assertions))]
    let builder = builder.plugin(tauri_plugin_mcp_bridge::init());

    builder
        .manage(app_state.clone())
        .on_window_event(|window, event| {
            // Remember which terminal window was last key, so a menu click made
            // while Preferences is focused (or while everything is minimized)
            // still has a window to act on.
            if let tauri::WindowEvent::Focused(true) = event {
                let label = window.label();
                if label != "preferences" && label != "help" {
                    *LAST_FOCUSED_WINDOW.write() = Some(label.to_string());
                }
            }
        })
        .setup(move |app| {
            // tauri-plugin-log is active by now — surface the warning that
            // arm_running_marker() captured before the logger was ready.
            log_previous_run_status();

            match fd_limit_raise {
                Some((old, new)) => log::info!("Raised RLIMIT_NOFILE soft limit {} -> {}", old, new),
                None => log::warn!("RLIMIT_NOFILE soft limit not raised (already at max or setrlimit failed)"),
            }

            // Before anything opens a tunnel of its own: whatever still holds one of our
            // ControlPath sockets belongs to a run that did not exit cleanly, and would
            // otherwise squat its remote port for as long as the machine stays up.
            commands::ssh_tunnel::kill_orphaned_tunnels();

            // Window title is set dynamically from the frontend (workspace name)

            // Restore additional windows beyond "main".
            //
            // The monitor count can be unknowable at launch — a deploy or a relaunch while
            // the displays are asleep or the lock screen is up reports no screens at all.
            // That is an absence, not a display configuration (see
            // commands::window::monitor_count): restoring geometry under a "0" key put
            // every window at a phantom rect, and because the frontend poller then adopted
            // the real count without re-placing anything, the next move saved that rect
            // back under the real key. With no count we restore nothing and let the
            // windows come up at their default size; the frontend places them the moment
            // the displays return.
            let monitor_count = commands::window::monitor_count(app.handle());
            if monitor_count.is_none() {
                log::info!("No monitors at startup (displays asleep or locked) — deferring window geometry until they return");
            }

            let extra_windows: Vec<String> = {
                let mut data = app_state.app_data.write();
                // Migrate legacy flat fields into geometry map — only under a real count,
                // so an old arrangement is never filed under a phantom layout.
                if let Some(count) = monitor_count {
                    for w in &mut data.windows {
                        w.migrate_legacy_geometry(count);
                    }
                }
                data.windows.iter()
                    .skip(1) // skip "main" — already created by Tauri
                    .map(|w| w.label.clone())
                    .collect()
            };

            for label in extra_windows {
                let url = if cfg!(debug_assertions) {
                    tauri::WebviewUrl::External("http://localhost:1420".parse().unwrap())
                } else {
                    tauri::WebviewUrl::App("index.html".into())
                };
                // Title is set dynamically from the frontend (workspace name)
                let title = if cfg!(debug_assertions) { "maiTerm (Dev)" } else { "maiTerm" };

                let geometry = monitor_count.and_then(|count| {
                    let data = app_state.app_data.read();
                    data.window(&label).and_then(|w| w.geometry_for(count)).cloned()
                });

                // No count means no answer about WHERE the window goes — but its size is
                // not in question, and defaulting it would hand the placement that follows
                // the displays returning a width change to deliver to a live agent.
                let last_size = if monitor_count.is_none() {
                    let data = app_state.app_data.read();
                    data.window(&label).and_then(|w| w.last_geometry()).map(|g| (g.width, g.height))
                } else {
                    None
                };

                let (w, h) = geometry.as_ref()
                    .map(|g| (g.width, g.height))
                    .or(last_size)
                    .unwrap_or((1200.0, 800.0));

                let mut builder = WebviewWindowBuilder::new(app, &label, url)
                    .title(title)
                    .inner_size(w, h)
                    .min_inner_size(800.0, 600.0)
                    .resizable(true)
                    .fullscreen(false)
                    .background_color(commands::window::DEFAULT_WINDOW_BG);

                #[cfg(target_os = "macos")]
                {
                    builder = builder
                        .hidden_title(true)
                        .title_bar_style(tauri::TitleBarStyle::Overlay);
                }

                match builder.build() {
                    Ok(win) => {
                        if let Some(ref geom) = geometry {
                            log::info!("Restoring window '{}' at ({}, {}) size {}x{} (monitors={})",
                                label, geom.x, geom.y, w, h, monitor_count.unwrap_or(0));
                            let scale = win.scale_factor().unwrap_or(1.0);
                            let phys_x = (geom.x * scale) as i32;
                            let phys_y = (geom.y * scale) as i32;
                            if let Err(e) = win.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(phys_x, phys_y))) {
                                log::warn!("Failed to set position for '{}': {}", label, e);
                            }
                        }
                    }
                    Err(e) => {
                        log::error!("Failed to restore window '{}': {}", label, e);
                    }
                }
            }

            // Custom app menu
            let quit_item = MenuItem::with_id(app, "quit", "Quit maiTerm", true, Some("CmdOrCtrl+Q"))?;
            let preferences_item = MenuItem::with_id(app, "preferences", "Preferences…", true, Some("CmdOrCtrl+,"))?;
            let reload_all_item = MenuItem::with_id(app, "reload_all", "Reload All Windows", true, None::<&str>)?;
            let new_window_item = MenuItem::with_id(app, "new_window", "New Window", true, Some("CmdOrCtrl+N"))?;
            let duplicate_window_item = MenuItem::with_id(app, "duplicate_window", "Duplicate Window", true, Some("CmdOrCtrl+Shift+N"))?;
            let reload_tab_item = MenuItem::with_id(app, "reload_tab", "Reload Current Tab", true, None::<&str>)?;
            let reload_window_item = MenuItem::with_id(app, "reload_window", "Reload Current Window", true, None::<&str>)?;
            let clear_nav_history_item = MenuItem::with_id(app, "clear_nav_history", "Clear Back/Forward History", true, None::<&str>)?;
            let help_item = MenuItem::with_id(app, "help", "Help", true, Some("CmdOrCtrl+/"))?;
            let check_updates_item = MenuItem::with_id(app, "check_updates", "Check for Updates…", true, None::<&str>)?;
            let report_bug_item = MenuItem::with_id(app, "report_bug", "Report Bug…", true, None::<&str>)?;
            let feature_request_item = MenuItem::with_id(app, "feature_request", "Submit Feature Request…", true, None::<&str>)?;

            let about = AboutMetadata {
                name: Some("maiTerm".into()),
                version: Some(APP_VERSION.into()),
                copyright: Some("© 2025 Flexmark International".into()),
                credits: Some("A modern terminal emulator with workspace organization, split panes, and Claude Code integration.\n\nhttps://maiterm.dev/".into()),
                ..Default::default()
            };

            let app_menu = SubmenuBuilder::new(app, "maiTerm")
                .about(Some(about))
                .separator()
                .item(&check_updates_item)
                .item(&preferences_item)
                .separator()
                .services()
                .separator()
                .hide()
                .hide_others()
                .show_all()
                .separator()
                .item(&quit_item)
                .build()?;

            let export_state_item = MenuItem::with_id(app, "export_state", "Export State…", true, None::<&str>)?;
            let import_state_item = MenuItem::with_id(app, "import_state", "Import State…", true, None::<&str>)?;

            let file_menu = SubmenuBuilder::new(app, "File")
                .item(&new_window_item)
                .item(&duplicate_window_item)
                .separator()
                .item(&reload_tab_item)
                .item(&reload_all_item)
                .separator()
                .item(&export_state_item)
                .item(&import_state_item)
                .build()?;

            let edit_menu = SubmenuBuilder::new(app, "Edit")
                .undo()
                .redo()
                .separator()
                .cut()
                .copy()
                .paste()
                .select_all()
                .build()?;

            let window_menu = SubmenuBuilder::new(app, "Window")
                .minimize()
                .close_window()
                .separator()
                .item(&reload_window_item)
                .item(&clear_nav_history_item)
                .build()?;

            let help_menu = SubmenuBuilder::new(app, "Help")
                .item(&help_item)
                .item(&check_updates_item)
                .separator()
                .item(&report_bug_item)
                .item(&feature_request_item)
                .build()?;

            let menu = MenuBuilder::new(app)
                .items(&[&app_menu, &file_menu, &edit_menu, &window_menu, &help_menu])
                .build()?;

            app.set_menu(menu)?;

            // Prepare the Claude Code IDE MCP server synchronously: bind the
            // port, generate the auth token, and write ~/.claude.json + hooks
            // + skill BEFORE setup() returns. The frontend doesn't load (and
            // therefore no PTY can spawn or fire auto-resume) until we're
            // done here, which eliminates the race where `claude --resume`
            // read a stale MCP port from a prior maiTerm instance.
            if let Some(setup) = claude_code::server::prepare_server(&app_state) {
                let server_state = app_state.clone();
                let server_handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    claude_code::server::serve_server(server_handle, server_state, setup).await;
                });
            }

            // Shadowed SSH transcripts outlive whichever feature wrote them, so pruning is
            // the app's job, not maiLink's. It used to run inside `mailink::start`, which
            // meant an Overlord-only user — who now populates the same directory — kept
            // every shadow file forever.
            mailink::mirror::prune_stale_shadows();

            // maiLink mobile-companion LAN bridge (docs/mailink-protocol.md): a separate,
            // opt-in TLS listener — only started when the user has enabled it. Kept distinct
            // from the localhost-only Claude-Code server above.
            if app_state.app_data.read().preferences.mailink_enabled {
                if let Err(e) = mailink::start(&app_state, app.handle().clone()) {
                    log::error!("[maiLink] {e}");
                }
            }

            // Comms integration (/maiterm resolve): global thread-reply watcher.
            // Always spawned — it idles cheaply when no tab is bound, and bound
            // tabs persist on disk so this doubles as restart rehydration.
            tauri::async_runtime::spawn(comms::watcher_loop(app_state.clone(), app.handle().clone()));

            // Background tasks owned by Rust (independent of any webview's
            // event loop). See commands/scheduler.rs for the rationale.
            commands::scheduler::spawn_backup_scheduler(app_state.clone());
            commands::scheduler::spawn_memory_sampler(app_state.clone());

            app.on_menu_event(|app_handle, event| {
                match event.id().as_ref() {
                    "quit" => {
                        // Emit event so each window can save scrollback before exit.
                        // Don't close windows directly — that triggers closeWindow()
                        // which removes window data from state.
                        let _ = app_handle.emit("quit-requested", ());
                    }
                    "preferences" => {
                        if let Some(win) = app_handle.get_webview_window("main") {
                            let _ = commands::window::open_preferences_window(win, app_handle.clone());
                        }
                    }
                    "reload_tab" => {
                        // Emit event so the focused window can reload the active tab's PTY
                        emit_to_focused_window(app_handle, "reload-tab");
                    }
                    "reload_all" => {
                        for (_, win) in app_handle.webview_windows() {
                            let _ = tauri::WebviewWindow::eval(&win, "window.location.reload()");
                        }
                    }
                    "reload_window" => {
                        // Reload the focused window (find it by checking is_focused)
                        for (_, win) in app_handle.webview_windows() {
                            if win.is_focused().unwrap_or(false) {
                                let _ = tauri::WebviewWindow::eval(&win, "window.location.reload()");
                                break;
                            }
                        }
                    }
                    "clear_nav_history" => {
                        // Each window has its own navHistoryStore; emit only to
                        // the focused window so we don't wipe history elsewhere.
                        emit_to_focused_window(app_handle, "clear-nav-history");
                    }
                    "export_state" | "import_state" => {
                        // Emit to the focused window so the frontend can show a file
                        // dialog — one dialog, not one per open window.
                        emit_to_focused_window(app_handle, event.id().as_ref());
                    }
                    // The accelerator and the menu item are two different events, and
                    // only one of them reaches the webview. macOS offers a Cmd-key to
                    // the key window's view hierarchy BEFORE the main menu, so the
                    // frontend keydown handler in +layout.svelte swallows Cmd+N /
                    // Cmd+Shift+N and this arm never runs for the shortcut. A CLICK on
                    // File ▸ New Window has no keydown at all, so if this arm does
                    // nothing the menu item is dead — which is what it was.
                    "new_window" => {
                        let state = app_handle.state::<Arc<AppState>>();
                        if let Err(e) = commands::window::create_window(app_handle.clone(), state) {
                            log::error!("Menu 'New Window' failed: {e}");
                        }
                    }
                    "duplicate_window" => {
                        // Duplication needs every tab's live scrollback and cwd, which
                        // only the webview holds — so one window has to do it for us.
                        emit_to_focused_window(app_handle, "duplicate-window");
                    }
                    "help" => {
                        if let Some(win) = app_handle.get_webview_window("main") {
                            let _ = commands::window::open_help_window(win, app_handle.clone(), None);
                        }
                    }
                    "report_bug" => {
                        #[allow(deprecated)]
                        let _ = tauri_plugin_shell::ShellExt::shell(app_handle)
                            .open("https://github.com/Flexmark-Intl/maiterm/issues/new?labels=bug&type=bug", None);
                    }
                    "check_updates" => {
                        // The old comment here said "emit to all windows so the focused
                        // one can handle it" — but all of them handled it, so one click
                        // ran N update checks and popped N toasts.
                        emit_to_focused_window(app_handle, "check-for-updates");
                    }
                    "feature_request" => {
                        #[allow(deprecated)]
                        let _ = tauri_plugin_shell::ShellExt::shell(app_handle)
                            .open("https://github.com/Flexmark-Intl/maiterm/issues/new?type=feature", None);
                    }
                    _ => {}
                }
            });

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::terminal::spawn_terminal,
            commands::terminal::write_terminal,
            commands::terminal::resize_terminal,
            commands::terminal::kill_terminal,
            commands::terminal::get_pty_info,
            commands::terminal::get_pty_foreground,
            commands::terminal::get_pty_foreground_job,
            commands::terminal::kill_pty_foreground_job,
            commands::terminal::list_live_ptys,
            commands::terminal::read_clipboard_file_paths,
            commands::terminal::detect_windows_shells,
            commands::terminal::scroll_terminal,
            commands::terminal::scroll_terminal_to,
            commands::terminal::get_terminal_scrollback_info,
            commands::terminal::search_terminal,
            commands::terminal::terminal_bracketed_paste,
            commands::terminal::get_agent_liveness,
            commands::terminal::get_agent_liveness_batch,
            commands::terminal::serialize_terminal,
            commands::terminal::restore_terminal_scrollback,
            commands::terminal::clear_terminal_scrollback,
            commands::terminal::get_terminal_selection_text,
            commands::terminal::start_selection,
            commands::terminal::update_selection,
            commands::terminal::clear_selection,
            commands::terminal::copy_selection,
            commands::terminal::select_all,
            commands::terminal::scroll_selection,
            commands::terminal::set_terminal_visible,
            commands::terminal::refresh_terminal_frame,
            commands::terminal::get_terminal_recent_text,
            commands::terminal::save_terminal_scrollback,
            commands::terminal::save_all_scrollback,
            commands::terminal::restore_terminal_from_saved,
            commands::terminal::has_saved_scrollback,
            commands::terminal::get_saved_scrollback_text,
            commands::terminal::get_saved_terminal_size,
            commands::workspace::get_app_data,
            commands::workspace::count_session_id_claimants,
            commands::workspace::create_workspace,
            commands::workspace::delete_workspace,
            commands::workspace::rename_workspace,
            commands::workspace::split_pane,
            commands::workspace::delete_pane,
            commands::workspace::rename_pane,
            commands::workspace::create_tab,
            commands::workspace::delete_tab,
            commands::workspace::move_tab_to_workspace,
            commands::workspace::move_tab_to_pane,
            commands::workspace::move_tab_to_split,
            commands::workspace::rename_tab,
            commands::workspace::update_editor_tab_file,
            commands::workspace::set_active_workspace,
            commands::workspace::suspend_workspace,
            commands::workspace::resume_workspace,
            commands::workspace::set_active_pane,
            commands::workspace::set_active_tab,
            commands::workspace::heal_pane_active_tab,
            commands::workspace::set_tab_pty_id,
            commands::workspace::suspend_tab,
            commands::workspace::mark_tabs_suspended,
            commands::workspace::set_tab_pinned,
            commands::workspace::set_sidebar_width,
            commands::workspace::set_sidebar_collapsed,
            commands::workspace::set_split_ratio,
            commands::workspace::set_tab_scrollback,
            commands::workspace::set_tab_notes,
            commands::workspace::set_tab_notes_open,
            commands::workspace::set_tab_notes_mode,
            commands::workspace::set_tab_composer_open,
            commands::workspace::set_tab_composer_draft,
            commands::workspace::set_tab_mesh_purpose,
            commands::workspace::set_tab_runtime,
            commands::workspace::reorder_tabs,
            commands::workspace::reorder_workspaces,
            commands::workspace::duplicate_workspace,
            commands::workspace::exit_app,
            commands::workspace::sync_state,
            commands::workspace::get_preferences,
            commands::workspace::set_preferences,
            commands::workspace::copy_tab_history,
            commands::workspace::set_tab_restore_context,
            commands::workspace::set_tab_last_cwd,
            commands::workspace::set_tab_auto_resume_context,
            commands::workspace::set_tab_auto_resume_enabled,
            commands::workspace::set_tab_agent_bridge,
            commands::workspace::set_tab_trigger_variables,
            commands::workspace::get_all_workspaces,
            commands::workspace::get_all_tabs,
            commands::workspace::list_system_sounds,
            commands::workspace::play_system_sound,
            commands::workspace::play_bell_sound,
            commands::workspace::add_workspace_note,
            commands::workspace::update_workspace_note,
            commands::workspace::delete_workspace_note,
            commands::workspace::set_workspace_bridge_all,
            commands::workspace::set_workspace_mesh_topics,
            commands::workspace::set_workspace_stack,
            commands::workspace::set_tab_service_id,
            commands::workspace::publish_stack_runtime,
            commands::stack::suggest_stack,
            commands::accounts::list_account_runtimes,
            commands::accounts::reconcile_account,
            commands::accounts::read_account_identity,
            commands::accounts::account_spawn_env,
            commands::accounts::begin_account_login,
            commands::accounts::cancel_account_login,
            commands::accounts::discard_account_root,
            commands::accounts::list_private_browsers,
            commands::accounts::open_private_window,
            commands::workspace::set_tab_mailink_native,
            commands::workspace::set_tab_mailink_excluded,
            commands::workspace::clear_tab_comms_binding,
            commands::workspace::set_workspace_mailink_native,
            commands::mailink::mailink_create_pairing,
            commands::mailink::mailink_set_enabled,
            commands::mailink::mailink_list_devices,
            commands::mailink::mailink_remove_device,
            commands::overlord::get_overlord_tab_facts,
            commands::overlord::get_agent_reply_since,
            commands::overlord::get_tab_prompt,
            commands::overlord::answer_tab_prompt,
            commands::overlord::append_overlord_ledger,
            commands::overlord::publish_overlord_snapshot,
            commands::overlord::get_overlord_ledger,
            commands::overlord::set_workspace_overlord,
            commands::overlord::set_workspace_overlord_exempt,
            commands::overlord::create_overlord_workspace,
            commands::workspace::set_tab_tasks_open,
            commands::workspace::set_tab_overlord_exempt,
            commands::tasks::set_workspace_tasks,
            commands::tasks::get_window_tasks,
            commands::comms::comms_test_connection,
            commands::comms::comms_list_bot_channels,
            commands::workspace::set_tab_comms_monitor,
            commands::workspace::carry_tab_state_on_reload,
            commands::window::get_window_data,
            commands::window::create_window,
            commands::window::duplicate_window,
            commands::window::close_window,
            commands::window::set_window_name,
            commands::window::save_window_geometry,
            commands::window::get_monitor_count,
            commands::window::restore_window_geometry,
            commands::window::set_window_background,
            commands::window::reset_window,
            commands::window::get_window_count,
            commands::window::open_preferences_window,
            commands::window::open_help_window,
            commands::editor::read_file,
            commands::editor::read_file_base64,
            commands::editor::write_file,
            commands::editor::scp_read_file,
            commands::editor::scp_read_file_base64,
            commands::editor::scp_write_file,
            commands::editor::save_clipboard_image,
            commands::editor::reveal_in_file_manager,
            commands::editor::download_remote_file,
            commands::editor::stage_remote_file_temp,
            commands::editor::scp_upload_files,
            commands::editor::cancel_scp_upload,
            commands::editor::create_editor_tab,
            commands::editor::watch_file,
            commands::editor::unwatch_file,
            commands::editor::get_file_mtime,
            commands::editor::watch_remote_file,
            commands::editor::unwatch_remote_file,
            commands::editor::get_remote_file_mtime,
            commands::editor::git_show_file,
            commands::editor::is_directory,
            commands::editor::ssh_is_directory,
            commands::editor::list_files,
            commands::editor::ssh_list_files,
            commands::claude_code::claude_code_respond,
            commands::claude_code::claude_code_notify_selection,
            commands::claude_code::refresh_agent_integrations,
            commands::deshittify::deshittify_status,
            commands::deshittify::deshittify_set_rule,
            commands::deshittify::deshittify_set_rules,
            commands::deshittify::build_deshittify_setup_script,
            commands::ssh_tunnel::start_ssh_tunnel,
            commands::ssh_tunnel::detach_ssh_tunnel,
            commands::ssh_tunnel::get_ssh_tunnel,
            commands::ssh_tunnel::get_mcp_port,
            commands::ssh_tunnel::get_mcp_auth,
            commands::ssh_tunnel::get_remote_bridge_env,
            commands::ssh_tunnel::get_maiterm_skill_scripts,
            commands::ssh_tunnel::build_codex_setup_script,
            commands::ssh_tunnel::ssh_run_setup,
            commands::workspace::create_diff_tab,
            commands::workspace::archive_tab,
            commands::workspace::restore_archived_tab,
            commands::workspace::delete_archived_tab,
            commands::workspace::export_state,
            commands::workspace::import_state,
            commands::workspace::preview_import,
            commands::workspace::import_state_selective,
            commands::workspace::run_scheduled_backup,
            commands::workspace::trim_old_backups,
            commands::workspace::pick_backup_directory,
            commands::workspace::get_app_diagnostics,
            commands::workspace::read_app_logs,
            commands::system::check_full_disk_access,
            commands::system::open_full_disk_access_settings,
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application")
        .run(|app_handle, event| {
            // Native quit paths — Dock → Quit, `osascript … to quit`, logout/
            // restart — bypass the menu "Quit" → quit-requested → exit_app flow,
            // so historically the running marker was never cleared (making every
            // clean native quit look like a crash next launch) and MCP/PTY/tunnel
            // cleanup never ran. Converge them on the shared shutdown path here.
            // Idempotent with exit_app via the guard in run_shutdown_cleanup.
            // Both events: an AppleEvent quit (`osascript … to quit`, the deploy
            // script's path) tears the app down without ever raising ExitRequested,
            // so listening for that alone left the running marker behind — every
            // deploy looked like a crash next launch — and skipped the final state
            // flush, losing everything since the last command that happened to save.
            if matches!(event, tauri::RunEvent::ExitRequested { .. } | tauri::RunEvent::Exit) {
                if let Some(state) = app_handle.try_state::<Arc<AppState>>() {
                    commands::workspace::run_shutdown_cleanup(&state);
                } else {
                    state::persistence::clear_running_marker();
                }
            }
        });
}
