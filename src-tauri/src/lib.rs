mod claude_code;
mod commands;
mod pty;
mod state;
mod terminal;

pub const APP_DISPLAY_NAME: &str = if cfg!(debug_assertions) { "maiTermDev" } else { "maiTerm" };
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

use state::{load_state, save_state, AppState, WindowData, Workspace};
use state::persistence::{arm_running_marker, load_memory_trend, log_previous_run_status, migrate_app_data, migrate_scrollback_to_db};
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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    // Arm crash marker BEFORE any other init. arm_running_marker() captures
    // whether a marker file from the previous run still exists (= unclean
    // exit) and then re-writes it for this run. The captured PreviousRunInfo
    // is cached in the function's static so get_app_diagnostics can read it
    // back without us having to thread it through AppState.
    let _prev_run = arm_running_marker();

    let app_state = Arc::new(AppState::new());

    // Load persisted state and run migration
    {
        let mut data = app_state.app_data.write();
        *data = load_state();
        migrate_app_data(&mut data);
        migrate_scrollback_to_db(&mut data, &app_state.scrollback_db);
        // Flush the cleaned JSON (scrollback stripped) to disk
        let _ = save_state(&data);

        // Sweep scrollback DB for rows whose tab no longer exists in state —
        // backstop for any path that drops tabs without deleting their row.
        match app_state.scrollback_db.prune_orphans(&data.all_tab_ids()) {
            Ok(n) if n > 0 => log::info!("Pruned {} orphan scrollback rows at startup", n),
            Ok(_) => {}
            Err(e) => log::warn!("Startup scrollback prune failed: {}", e),
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
        .setup(move |app| {
            // tauri-plugin-log is active by now — surface the warning that
            // arm_running_marker() captured before the logger was ready.
            log_previous_run_status();

            // Window title is set dynamically from the frontend (workspace name)

            // Restore additional windows beyond "main"
            // Determine current monitor count for geometry lookup
            let monitor_count = app.primary_monitor()
                .ok()
                .flatten()
                .and_then(|_| app.available_monitors().ok())
                .map(|m| m.len())
                .unwrap_or(1);

            let extra_windows: Vec<String> = {
                let mut data = app_state.app_data.write();
                // Migrate legacy flat fields into geometry map
                for w in &mut data.windows {
                    w.migrate_legacy_geometry(monitor_count);
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

                let geometry = {
                    let data = app_state.app_data.read();
                    data.window(&label).and_then(|w| w.geometry_for(monitor_count)).cloned()
                };

                let (w, h) = geometry.as_ref()
                    .map(|g| (g.width, g.height))
                    .unwrap_or((1200.0, 800.0));

                let mut builder = WebviewWindowBuilder::new(app, &label, url)
                    .title(title)
                    .inner_size(w, h)
                    .min_inner_size(800.0, 600.0)
                    .resizable(true)
                    .fullscreen(false);

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
                                label, geom.x, geom.y, w, h, monitor_count);
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
                        for (_, win) in app_handle.webview_windows() {
                            if win.is_focused().unwrap_or(false) {
                                let _ = win.emit("reload-tab", ());
                                break;
                            }
                        }
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
                        for (_, win) in app_handle.webview_windows() {
                            if win.is_focused().unwrap_or(false) {
                                let _ = win.emit("clear-nav-history", ());
                                break;
                            }
                        }
                    }
                    "export_state" | "import_state" => {
                        // Emit to the focused window so the frontend can show a file dialog
                        let event_name = event.id().as_ref();
                        for (_, win) in app_handle.webview_windows() {
                            if win.is_focused().unwrap_or(false) {
                                let _ = win.emit(event_name, ());
                                break;
                            }
                        }
                    }
                    "new_window" | "duplicate_window" => {
                        // These are handled by frontend keyboard shortcuts.
                        // The menu accelerators trigger the keydown event which
                        // the frontend handles.
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
                        // Emit to all windows so the focused one can handle it
                        let _ = app_handle.emit("check-for-updates", ());
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
            commands::terminal::read_clipboard_file_paths,
            commands::terminal::detect_windows_shells,
            commands::terminal::scroll_terminal,
            commands::terminal::scroll_terminal_to,
            commands::terminal::get_terminal_scrollback_info,
            commands::terminal::search_terminal,
            commands::terminal::terminal_bracketed_paste,
            commands::terminal::serialize_terminal,
            commands::terminal::restore_terminal_scrollback,
            commands::terminal::resize_terminal_grid,
            commands::terminal::clear_terminal_scrollback,
            commands::terminal::get_terminal_selection_text,
            commands::terminal::start_selection,
            commands::terminal::update_selection,
            commands::terminal::clear_selection,
            commands::terminal::copy_selection,
            commands::terminal::select_all,
            commands::terminal::scroll_selection,
            commands::terminal::get_terminal_recent_text,
            commands::terminal::save_terminal_scrollback,
            commands::terminal::restore_terminal_from_saved,
            commands::terminal::has_saved_scrollback,
            commands::terminal::get_saved_scrollback_text,
            commands::terminal::get_saved_terminal_size,
            commands::workspace::get_app_data,
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
            commands::workspace::set_tab_pty_id,
            commands::workspace::suspend_tab,
            commands::workspace::set_sidebar_width,
            commands::workspace::set_sidebar_collapsed,
            commands::workspace::set_split_ratio,
            commands::workspace::set_tab_scrollback,
            commands::workspace::set_tab_notes,
            commands::workspace::set_tab_notes_open,
            commands::workspace::set_tab_notes_mode,
            commands::workspace::set_tab_composer_open,
            commands::workspace::set_tab_composer_draft,
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
            commands::window::get_window_data,
            commands::window::create_window,
            commands::window::duplicate_window,
            commands::window::close_window,
            commands::window::save_window_geometry,
            commands::window::get_monitor_count,
            commands::window::restore_window_geometry,
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
            commands::ssh_tunnel::start_ssh_tunnel,
            commands::ssh_tunnel::detach_ssh_tunnel,
            commands::ssh_tunnel::get_ssh_tunnel,
            commands::ssh_tunnel::get_mcp_port,
            commands::ssh_tunnel::get_mcp_auth,
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
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
