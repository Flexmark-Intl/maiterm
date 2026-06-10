use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};

use crate::pty;
use crate::state::AppState;
use crate::terminal::handle::TermDimensions;
use crate::terminal::render::{self, TerminalFrame};
use crate::terminal::search;
use crate::terminal::serialize;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::Handler;

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShellInfo {
    pub id: String,
    pub name: String,
    pub path: String,
}

/// Read file paths from the system clipboard (macOS NSPasteboard).
/// Returns an empty vec if the clipboard doesn't contain file URLs.
#[tauri::command]
pub fn read_clipboard_file_paths() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        read_file_paths_macos()
    }
    #[cfg(not(target_os = "macos"))]
    {
        vec![]
    }
}

#[cfg(target_os = "macos")]
fn read_file_paths_macos() -> Vec<String> {
    use std::process::Command;

    // Use JXA (JavaScript for Automation) to read file URLs from NSPasteboard.
    // Iterates pasteboardItems and reads the public.file-url type from each.
    let script = concat!(
        "ObjC.import('AppKit');",
        "var pb=$.NSPasteboard.generalPasteboard;",
        "var items=pb.pasteboardItems;",
        "var p=[];",
        "for(var i=0;i<items.count;i++){",
        "var u=items.objectAtIndex(i).stringForType('public.file-url');",
        "if(u){p.push($.NSURL.URLWithString(u).path.js)}}",
        "p.join('\\n')"
    );

    let Ok(output) = Command::new("osascript")
        .args(["-l", "JavaScript", "-e", script])
        .output()
    else {
        return vec![];
    };

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        vec![]
    } else {
        stdout.lines().map(String::from).collect()
    }
}

#[tauri::command]
pub fn spawn_terminal(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    tab_id: String,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
) -> Result<(), String> {
    pty::spawn_pty(&app_handle, &*state, &pty_id, &tab_id, cols, rows, cwd)
}

#[tauri::command]
pub fn get_pty_info(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<pty::PtyInfo, String> {
    pty::get_pty_info(&*state, &pty_id)
}

#[tauri::command]
pub fn write_terminal(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    pty::write_pty(&*state, &pty_id, &data)
}

#[tauri::command]
pub fn resize_terminal(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    pty::resize_pty(&*state, &pty_id, cols, rows)
}

#[tauri::command]
pub fn kill_terminal(state: State<'_, Arc<AppState>>, pty_id: String) -> Result<(), String> {
    pty::kill_pty(&*state, &pty_id)
}

#[tauri::command]
pub fn detect_windows_shells() -> Vec<ShellInfo> {
    #[cfg(windows)]
    {
        detect_windows_shells_impl()
    }
    #[cfg(not(windows))]
    {
        vec![]
    }
}

#[cfg(windows)]
fn detect_windows_shells_impl() -> Vec<ShellInfo> {
    use std::process::Command;

    let mut shells = Vec::new();

    // cmd.exe — always available
    let cmd_path = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
    shells.push(ShellInfo {
        id: "cmd".to_string(),
        name: "Command Prompt".to_string(),
        path: cmd_path,
    });

    // powershell.exe — always available on modern Windows
    shells.push(ShellInfo {
        id: "powershell".to_string(),
        name: "Windows PowerShell".to_string(),
        path: "powershell.exe".to_string(),
    });

    // pwsh.exe — PowerShell 7+ (optional install)
    if let Ok(output) = Command::new("where").arg("pwsh").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("pwsh.exe")
                .trim()
                .to_string();
            shells.push(ShellInfo {
                id: "pwsh".to_string(),
                name: "PowerShell 7".to_string(),
                path,
            });
        }
    }

    // Git Bash — check common install paths
    let git_bash_paths = [
        r"C:\Program Files\Git\bin\bash.exe",
        r"C:\Program Files (x86)\Git\bin\bash.exe",
    ];
    for p in &git_bash_paths {
        if std::path::Path::new(p).exists() {
            shells.push(ShellInfo {
                id: "gitbash".to_string(),
                name: "Git Bash".to_string(),
                path: p.to_string(),
            });
            break;
        }
    }

    // WSL
    if let Ok(output) = Command::new("where").arg("wsl").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("wsl.exe")
                .trim()
                .to_string();
            shells.push(ShellInfo {
                id: "wsl".to_string(),
                name: "WSL".to_string(),
                path,
            });
        }
    }

    shells
}

// --- alacritty_terminal commands ---

/// Scroll terminal display by delta lines (positive = up, negative = down).
#[tauri::command]
pub fn scroll_terminal(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    delta: i32,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    handle.term.scroll_display(Scroll::Delta(delta));
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}

/// Scroll terminal to an absolute position (0 = bottom/live).
#[tauri::command]
pub fn scroll_terminal_to(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    offset: usize,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    // First scroll to bottom, then scroll up by the desired offset
    handle.term.scroll_display(Scroll::Bottom);
    if offset > 0 {
        handle.term.scroll_display(Scroll::Delta(offset as i32));
    }
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}

/// Scrollback metadata.
#[derive(serde::Serialize)]
pub struct ScrollInfo {
    pub display_offset: usize,
    pub total_lines: usize,
    pub viewport_rows: usize,
}

/// Get scrollback metadata.
#[tauri::command]
pub fn get_terminal_scrollback_info(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<ScrollInfo, String> {
    let registry = state.terminal_registry.read();
    let handle = registry.get(&pty_id).ok_or("Terminal not found")?;
    Ok(ScrollInfo {
        display_offset: handle.term.grid().display_offset(),
        total_lines: handle.term.grid().total_lines(),
        viewport_rows: handle.term.screen_lines(),
    })
}

/// Whether the foreground app has enabled bracketed paste mode (DECSET 2004).
/// Used by the composer dock to decide between paste-wrapping multi-line text
/// (Claude Code, modern readline) and sending raw lines (e.g. macOS bash 3.2).
#[tauri::command]
pub fn terminal_bracketed_paste(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<bool, String> {
    let registry = state.terminal_registry.read();
    let handle = registry.get(&pty_id).ok_or("Terminal not found")?;
    Ok(handle
        .term
        .mode()
        .contains(alacritty_terminal::term::TermMode::BRACKETED_PASTE))
}

/// Search the terminal buffer.
#[tauri::command]
pub fn search_terminal(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    query: String,
    case_sensitive: bool,
) -> Result<search::SearchResult, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    search::search_buffer(&mut handle.term, &query, case_sensitive)
}

/// Serialize the terminal buffer for persistence.
/// NOTE: Returns Vec<u8> to avoid WebView bridge double-encoding non-ASCII.
/// Prefer `save_terminal_scrollback` which keeps data entirely in Rust.
#[tauri::command]
pub fn serialize_terminal(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<Vec<u8>, String> {
    let registry = state.terminal_registry.read();
    let handle = registry.get(&pty_id).ok_or("Terminal not found")?;
    // Skip serialization when alternate screen is active
    if handle.term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN) {
        return Err("Alternate screen active".to_string());
    }
    Ok(serialize::serialize_buffer(&handle.term).into_bytes())
}

/// Restore scrollback into the terminal buffer.
#[tauri::command]
pub fn restore_terminal_scrollback(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    scrollback: Vec<u8>,
) -> Result<(), String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    let scrollback_str = String::from_utf8_lossy(&scrollback);
    serialize::restore_scrollback(&mut handle.term, &scrollback_str);
    Ok(())
}

/// Serialize + save scrollback to SQLite in one shot.
/// This is the preferred path for auto-save and shutdown saves.
#[tauri::command]
pub fn save_terminal_scrollback(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    tab_id: String,
) -> Result<(), String> {
    let scrollback = {
        let registry = state.terminal_registry.read();
        let handle = registry.get(&pty_id).ok_or("Terminal not found")?;
        if handle.term.mode().contains(alacritty_terminal::term::TermMode::ALT_SCREEN) {
            return Err("Alternate screen active".to_string());
        }
        serialize::serialize_buffer(&handle.term)
    };

    state.scrollback_db.save(&tab_id, &scrollback)
}

/// Restore scrollback from SQLite into the terminal buffer.
#[tauri::command]
pub fn restore_terminal_from_saved(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    tab_id: String,
) -> Result<(), String> {
    let scrollback = state.scrollback_db.load(&tab_id)?;

    if let Some(scrollback) = scrollback {
        let mut registry = state.terminal_registry.write();
        let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
        serialize::restore_scrollback(&mut handle.term, &scrollback);
    }
    Ok(())
}

/// Check if a tab has saved scrollback in SQLite.
#[tauri::command]
pub fn has_saved_scrollback(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
) -> Result<bool, String> {
    state.scrollback_db.has(&tab_id)
}

/// Get plain text from saved scrollback (SQLite) for unmounted terminals.
/// Strips ANSI sequences and returns the last N lines.
#[tauri::command]
pub fn get_saved_scrollback_text(
    state: State<'_, Arc<AppState>>,
    tab_id: String,
    line_count: usize,
) -> Result<Option<String>, String> {
    let scrollback = state.scrollback_db.load(&tab_id)?;
    match scrollback {
        Some(data) => {
            // Strip ANSI escape sequences
            let mut plain = String::with_capacity(data.len());
            let mut chars = data.chars().peekable();
            while let Some(c) = chars.next() {
                if c == '\x1b' {
                    // Skip ESC sequences
                    if let Some(&next) = chars.peek() {
                        if next == '[' {
                            // CSI sequence — skip until letter
                            chars.next();
                            while let Some(&ch) = chars.peek() {
                                chars.next();
                                if ch.is_ascii_alphabetic() || ch == '@' || ch == '`' {
                                    break;
                                }
                            }
                        } else if next == ']' {
                            // OSC sequence — skip until BEL or ST
                            chars.next();
                            while let Some(ch) = chars.next() {
                                if ch == '\x07' {
                                    break;
                                }
                                if ch == '\x1b' {
                                    if chars.peek() == Some(&'\\') {
                                        chars.next();
                                        break;
                                    }
                                }
                            }
                        } else {
                            chars.next(); // Skip single char after ESC
                        }
                    }
                } else {
                    plain.push(c);
                }
            }
            let lines: Vec<&str> = plain.lines().collect();
            let start = lines.len().saturating_sub(line_count);
            let result = lines[start..].join("\n");
            let trimmed = result.trim_end();
            if trimmed.is_empty() {
                Ok(None)
            } else {
                Ok(Some(trimmed.to_string()))
            }
        }
        None => Ok(None),
    }
}

/// Clear the terminal's scrollback history and visible screen.
/// Mimics iTerm2/Terminal.app Cmd+K: clears everything, shell redraws prompt.
#[tauri::command]
pub fn clear_terminal_scrollback(
    app_handle: AppHandle,
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<(), String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    handle.term.scroll_display(Scroll::Bottom);
    // Clear visible screen first (this pushes content into scrollback),
    // then clear scrollback history (removes everything including what was just pushed).
    handle.term.clear_screen(alacritty_terminal::vte::ansi::ClearMode::All);
    handle.term.clear_screen(alacritty_terminal::vte::ansi::ClearMode::Saved);
    // Move cursor to home so shell prompt redraws at top
    handle.term.goto(0, 0);
    // Emit a frame immediately so the frontend sees the cleared state
    let frame = render::render_viewport(&handle.term, handle.selection.as_ref());
    let _ = app_handle.emit(&format!("term-frame-{}", pty_id), &frame);
    Ok(())
}

/// Get text from the terminal grid at the given coordinates.
/// Used to read text from scrollback when the user selects while scrolled up.
#[tauri::command]
pub fn get_terminal_selection_text(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    start_x: usize,
    start_y: i32,
    end_x: usize,
    end_y: i32,
) -> Result<String, String> {
    let registry = state.terminal_registry.read();
    let handle = registry.get(&pty_id).ok_or("Terminal not found")?;
    let grid = handle.term.grid();
    let num_cols = handle.term.columns();

    let mut text = String::new();
    let mut line = alacritty_terminal::index::Line(start_y);
    let end_line = alacritty_terminal::index::Line(end_y);

    while line <= end_line {
        let start_col = if line.0 == start_y { start_x } else { 0 };
        let end_col = if line.0 == end_y { end_x } else { num_cols.saturating_sub(1) };

        // Bounds check
        if line < grid.topmost_line() || line > grid.bottommost_line() {
            line += 1;
            continue;
        }

        let row = &grid[line];
        for col in start_col..=end_col {
            if col >= num_cols {
                break;
            }
            let cell = &row[alacritty_terminal::index::Column(col)];
            if cell.c != '\0' && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                text.push(cell.c);
            }
        }

        if line < end_line {
            // Trim trailing spaces on this line, then add newline
            let trimmed = text.trim_end_matches(' ');
            text = trimmed.to_string();
            text.push('\n');
        }

        line += 1;
    }

    Ok(text)
}

/// Get recent plain text from the terminal buffer (last N lines).
/// Used by MCP getTabContext to read terminal output without going through the WebView.
#[tauri::command]
pub fn get_terminal_recent_text(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    line_count: usize,
) -> Result<String, String> {
    let registry = state.terminal_registry.read();
    let handle = registry.get(&pty_id).ok_or("Terminal not found")?;
    let grid = handle.term.grid();
    let num_cols = handle.term.columns();

    // bottommost_line is the last line in the grid (viewport bottom)
    let bottom = grid.bottommost_line();
    let top = grid.topmost_line();

    // Walk backwards from bottom to find the last non-empty line
    let mut last_used = bottom;
    while last_used > top {
        let row = &grid[last_used];
        let mut empty = true;
        for col in 0..num_cols {
            let cell = &row[alacritty_terminal::index::Column(col)];
            if cell.c != ' ' && cell.c != '\0' {
                empty = false;
                break;
            }
        }
        if !empty {
            break;
        }
        last_used -= 1;
    }

    // Calculate start line
    let start_line = std::cmp::max(top, last_used - (line_count as i32 - 1));

    let mut lines = Vec::new();
    let mut line = start_line;
    while line <= last_used {
        let mut row_text = String::new();
        if line >= top && line <= bottom {
            let row = &grid[line];
            for col in 0..num_cols {
                let cell = &row[alacritty_terminal::index::Column(col)];
                if cell.c != '\0' && !cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    row_text.push(cell.c);
                }
            }
        }
        lines.push(row_text.trim_end().to_string());
        line += 1;
    }

    Ok(lines.join("\n"))
}

/// Resize the alacritty_terminal instance (called alongside PTY resize).
#[tauri::command]
pub fn resize_terminal_grid(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let mut registry = state.terminal_registry.write();
    if let Some(handle) = registry.get_mut(&pty_id) {
        handle.term.resize(TermDimensions {
            cols: cols as usize,
            rows: rows as usize,
        });
    }
    Ok(())
}

// --- Selection commands ---

/// Convert viewport-relative (col, row) to absolute grid Point.
/// viewport_row 0 = top of visible area.
fn viewport_to_point(col: usize, viewport_row: usize, display_offset: usize) -> Point {
    Point::new(Line(viewport_row as i32 - display_offset as i32), Column(col))
}

fn parse_side(side: &str) -> Side {
    if side == "right" { Side::Right } else { Side::Left }
}

fn parse_selection_type(ty: &str) -> SelectionType {
    match ty {
        "block" => SelectionType::Block,
        "semantic" => SelectionType::Semantic,
        "lines" => SelectionType::Lines,
        _ => SelectionType::Simple,
    }
}

/// Start a new selection at the given viewport position.
#[tauri::command]
pub fn start_selection(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    col: usize,
    row: usize,
    side: String,
    selection_type: String,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    let display_offset = handle.term.grid().display_offset();
    let point = viewport_to_point(col, row, display_offset);
    handle.selection = Some(Selection::new(
        parse_selection_type(&selection_type),
        point,
        parse_side(&side),
    ));
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}

/// Update the end of the current selection.
#[tauri::command]
pub fn update_selection(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    col: usize,
    row: usize,
    side: String,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    let display_offset = handle.term.grid().display_offset();
    let point = viewport_to_point(col, row, display_offset);
    if let Some(ref mut sel) = handle.selection {
        sel.update(point, parse_side(&side));
    }
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}

/// Clear the current selection.
#[tauri::command]
pub fn clear_selection(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    handle.selection = None;
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}

/// Copy the current selection text.
#[tauri::command]
pub fn copy_selection(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<Option<String>, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    // Temporarily set term.selection so selection_to_string() can extract text
    handle.term.selection = handle.selection.clone();
    let text = handle.term.selection_to_string();
    handle.term.selection = None;
    Ok(text)
}

/// Select all content in the terminal buffer.
#[tauri::command]
pub fn select_all(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    let top = handle.term.topmost_line();
    let bottom = handle.term.bottommost_line();
    let last_col = handle.term.last_column();
    let mut sel = Selection::new(
        SelectionType::Simple,
        Point::new(top, Column(0)),
        Side::Left,
    );
    sel.update(Point::new(bottom, last_col), Side::Right);
    handle.selection = Some(sel);
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}

/// Scroll the viewport while maintaining an active selection, updating the
/// selection endpoint to the edge of the viewport in the scroll direction.
#[tauri::command]
pub fn scroll_selection(
    state: State<'_, Arc<AppState>>,
    pty_id: String,
    delta: i32,
    col: usize,
) -> Result<TerminalFrame, String> {
    let mut registry = state.terminal_registry.write();
    let handle = registry.get_mut(&pty_id).ok_or("Terminal not found")?;
    handle.term.scroll_display(Scroll::Delta(delta));
    // Update selection endpoint to the edge row in the scroll direction
    let display_offset = handle.term.grid().display_offset();
    let edge_row = if delta > 0 { 0 } else { handle.term.screen_lines() - 1 };
    let point = viewport_to_point(col, edge_row, display_offset);
    if let Some(ref mut sel) = handle.selection {
        sel.update(point, if delta > 0 { Side::Left } else { Side::Right });
    }
    Ok(render::render_viewport(&handle.term, handle.selection.as_ref()))
}
