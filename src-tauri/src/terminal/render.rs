use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::selection::{Selection, SelectionRange};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

/// A rendered viewport frame sent to the frontend.
#[derive(serde::Serialize, Clone)]
pub struct TerminalFrame {
    /// ANSI escape sequences (raw UTF-8 bytes) that bring the frontend's xterm
    /// from the previous frame to this one. Either a full repaint (`ESC[H ESC[2J`
    /// + every row) or a delta: only the rows that changed, each preceded by an
    /// absolute cursor move. xterm consumes both identically.
    /// Sent as bytes to avoid WebView string encoding issues with non-ASCII characters.
    pub ansi: Vec<u8>,
    pub cursor_x: usize,
    pub cursor_y: usize,
    pub cursor_visible: bool,
    /// 0 = at bottom (live), >0 = scrolled up into history
    pub display_offset: usize,
    /// Total lines including scrollback history
    pub total_lines: usize,
    /// Whether alternate screen buffer is active
    pub alternate_screen: bool,
    /// Whether there is an active selection
    pub has_selection: bool,
}

/// What a frame-producing command reports back to its caller. The pixels travel
/// on the `term-frame-{pty}` event like every other frame; the response carries
/// only the state the caller's UI (scrollbar, selection affordances) needs.
#[derive(serde::Serialize, Clone, Copy)]
pub struct FrameMeta {
    pub display_offset: usize,
    pub total_lines: usize,
    pub alternate_screen: bool,
    pub has_selection: bool,
}

impl From<&TerminalFrame> for FrameMeta {
    fn from(f: &TerminalFrame) -> Self {
        FrameMeta {
            display_offset: f.display_offset,
            total_lines: f.total_lines,
            alternate_screen: f.alternate_screen,
            has_selection: f.has_selection,
        }
    }
}

/// Current metadata without rendering anything.
pub fn frame_meta<T: EventListener>(term: &Term<T>, ext_selection: Option<&Selection>) -> FrameMeta {
    FrameMeta {
        display_offset: term.grid().display_offset(),
        total_lines: term.grid().total_lines(),
        alternate_screen: term.mode().contains(TermMode::ALT_SCREEN),
        has_selection: ext_selection.is_some(),
    }
}

/// The rows of the last frame delivered to a terminal's xterm, so the next
/// frame can be a delta against them. Every row string is self-contained: it
/// starts from default attributes and ends with a reset and any hyperlink
/// closed, so it renders identically whether written in sequence or jumped to
/// with an absolute cursor move.
///
/// The frontend's xterm keeps no scrollback and gets nothing except these
/// frames, so as long as every frame is applied in order the cache is exact.
/// Anything that can disturb xterm's buffer behind our back (a resize reflow, a
/// tab that was hidden and skipped frames) must drop the cache so the next
/// frame is a full repaint.
pub struct FrameCache {
    rows: Vec<String>,
    cols: usize,
    display_offset: usize,
    alt_screen: bool,
}

/// Render the next frame for `term`.
///
/// If `cache` holds the previous frame and the viewport geometry is unchanged,
/// only the rows whose rendered form differs are emitted, each addressed with
/// `ESC[row;1H`. A scroll changes every row, so when more than half the rows
/// differ the classic full repaint is emitted instead (it is smaller — no
/// per-row cursor moves). Under a streaming agent the common frame is a few
/// rows out of fifty; xterm's DOM renderer then rebuilds only those rows
/// instead of all of them.
///
/// If `ext_selection` is provided, it's used for highlight rendering instead of
/// `term.selection` (which gets cleared by VTE processing).
pub fn render_frame<T: EventListener>(
    term: &Term<T>,
    ext_selection: Option<&Selection>,
    cache: &mut Option<FrameCache>,
) -> TerminalFrame {
    let content = term.renderable_content();
    let num_cols = term.columns();
    let num_lines = term.screen_lines();

    let cursor = content.cursor;
    let cursor_visible = content.mode.contains(TermMode::SHOW_CURSOR);
    let display_offset = content.display_offset;
    let alternate_screen = content.mode.contains(TermMode::ALT_SCREEN);
    let total_lines = term.grid().total_lines();
    // Prefer the externally-managed selection over term.selection
    let selection_range: Option<SelectionRange> = ext_selection
        .and_then(|s| s.to_range(term))
        .or(content.selection);

    // --- Render every viewport row to its own self-contained string ---
    let mut rows: Vec<String> = Vec::with_capacity(num_lines);
    let mut row = String::with_capacity(num_cols * 4);
    let mut prev_fg = Color::Named(NamedColor::Foreground);
    let mut prev_bg = Color::Named(NamedColor::Background);
    let mut prev_flags = Flags::empty();
    let mut current_line: i32 = i32::MIN;
    // Track active OSC 8 hyperlink URI so we emit open/close at boundaries
    let mut active_hyperlink_uri: Option<String> = None;

    for indexed in content.display_iter {
        let point = indexed.point;
        let cell = indexed.cell;

        // Line change: close the row (reset attributes, close hyperlink) and start a new one.
        if point.line.0 != current_line {
            if current_line != i32::MIN {
                finish_row(&mut row, &mut active_hyperlink_uri);
                rows.push(std::mem::replace(&mut row, String::with_capacity(num_cols * 4)));
                prev_fg = Color::Named(NamedColor::Foreground);
                prev_bg = Color::Named(NamedColor::Background);
                prev_flags = Flags::empty();
            }
            current_line = point.line.0;
        }

        // Skip the trailing cell of a double-width char — the glyph covers it.
        // A LEADING spacer is different: it is a real blank in the last column
        // that alacritty writes when a wide glyph didn't fit and wrapped. It
        // must be emitted as a space, or the row comes out one column short
        // and a delta leaves the previous frame's last column standing.
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }

        // Handle OSC 8 hyperlink transitions
        let cell_uri = cell.hyperlink().map(|h| h.uri().to_string());
        match (&active_hyperlink_uri, &cell_uri) {
            (None, Some(uri)) => {
                row.push_str(&format!("\x1b]8;;{}\x1b\\", uri));
                active_hyperlink_uri = Some(uri.clone());
            }
            (Some(prev), Some(uri)) if prev != uri => {
                row.push_str("\x1b]8;;\x1b\\");
                row.push_str(&format!("\x1b]8;;{}\x1b\\", uri));
                active_hyperlink_uri = Some(uri.clone());
            }
            (Some(_), None) => {
                row.push_str("\x1b]8;;\x1b\\");
                active_hyperlink_uri = None;
            }
            _ => {} // Same link or both None — no change
        }

        // Toggle INVERSE for selected cells so they appear highlighted
        let mut flags = cell.flags;
        if let Some(ref sel) = selection_range {
            if sel.contains(point) {
                flags.toggle(Flags::INVERSE);
            }
        }

        // Emit SGR changes if attributes differ
        let needs_sgr = cell.fg != prev_fg || cell.bg != prev_bg || flags != prev_flags;
        if needs_sgr {
            emit_sgr(&mut row, cell.fg, cell.bg, flags);
            prev_fg = cell.fg;
            prev_bg = cell.bg;
            prev_flags = flags;
        }

        // Output the character
        // Control characters (tab, etc.) must be emitted as spaces — the grid
        // already reflects their visual effect (cursor movement / tab stops).
        // Emitting them raw would cause xterm.js to re-interpret them, e.g. a
        // tab in an 86-col grid produces 8+85 = 93 visible columns → line wrap.
        let c = cell.c;
        if c == '\0' || c == ' ' || c.is_ascii_control() {
            row.push(' ');
        } else {
            row.push(c);
        }

        // Append zero-width characters
        if let Some(zerowidth) = cell.zerowidth() {
            for &zw in zerowidth {
                row.push(zw);
            }
        }
    }
    if current_line != i32::MIN {
        finish_row(&mut row, &mut active_hyperlink_uri);
        rows.push(row);
    }

    // --- Decide: delta against the cache, or full repaint ---
    let reusable = cache.as_ref().map_or(false, |c| {
        c.cols == num_cols
            && c.rows.len() == rows.len()
            && c.display_offset == display_offset
            && c.alt_screen == alternate_screen
    });
    let changed: Vec<usize> = if reusable {
        let old = &cache.as_ref().unwrap().rows;
        (0..rows.len()).filter(|&i| rows[i] != old[i]).collect()
    } else {
        Vec::new()
    };
    let full = !reusable || changed.len() * 2 > rows.len();

    let mut out = String::with_capacity(if full {
        num_cols * num_lines * 10
    } else {
        changed.len() * (num_cols * 4 + 16) + 32
    });
    if full {
        // Clear screen and home cursor, then every row
        out.push_str("\x1b[H\x1b[2J");
        for (i, r) in rows.iter().enumerate() {
            if i > 0 {
                out.push_str("\r\n");
            }
            out.push_str(r);
        }
    } else {
        for &i in &changed {
            out.push_str(&format!("\x1b[{};1H", i + 1));
            out.push_str(&rows[i]);
        }
    }

    // Position cursor (hidden when scrolled into history)
    if cursor_visible && display_offset == 0 {
        out.push_str("\x1b[?25h"); // Re-show cursor (may have been hidden by scrollback)
        let cursor_viewport_line = cursor.point.line.0;
        if cursor_viewport_line >= 0 {
            let cy = cursor_viewport_line as usize + 1; // 1-based
            let cx = cursor.point.column.0 + 1; // 1-based
            out.push_str(&format!("\x1b[{};{}H", cy, cx));
        }
    } else if display_offset > 0 {
        out.push_str("\x1b[?25l"); // Hide cursor when browsing scrollback
    }

    *cache = Some(FrameCache {
        rows,
        cols: num_cols,
        display_offset,
        alt_screen: alternate_screen,
    });

    TerminalFrame {
        ansi: out.into_bytes(),
        cursor_x: cursor.point.column.0,
        cursor_y: {
            let line = cursor.point.line.0 + display_offset as i32;
            if line >= 0 { line as usize } else { 0 }
        },
        cursor_visible,
        display_offset,
        total_lines,
        alternate_screen,
        has_selection: selection_range.is_some(),
    }
}

/// Close a row: any open hyperlink, then reset attributes, so the row is
/// self-contained regardless of what is written after it.
fn finish_row(row: &mut String, active_hyperlink_uri: &mut Option<String>) {
    if active_hyperlink_uri.is_some() {
        row.push_str("\x1b]8;;\x1b\\");
        *active_hyperlink_uri = None;
    }
    row.push_str("\x1b[0m");
}

/// Emit SGR escape sequence for the given attributes.
fn emit_sgr(
    out: &mut String,
    fg: Color,
    bg: Color,
    flags: Flags,
) {
    out.push_str("\x1b[0"); // Reset first, then set what's needed

    // Flags
    if flags.contains(Flags::BOLD) {
        out.push_str(";1");
    }
    if flags.contains(Flags::DIM) {
        out.push_str(";2");
    }
    if flags.contains(Flags::ITALIC) {
        out.push_str(";3");
    }
    if flags.contains(Flags::UNDERLINE) {
        out.push_str(";4");
    }
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        out.push_str(";21");
    }
    if flags.contains(Flags::UNDERCURL) {
        out.push_str(";4:3");
    }
    if flags.contains(Flags::DOTTED_UNDERLINE) {
        out.push_str(";4:4");
    }
    if flags.contains(Flags::DASHED_UNDERLINE) {
        out.push_str(";4:5");
    }
    if flags.contains(Flags::INVERSE) {
        out.push_str(";7");
    }
    if flags.contains(Flags::HIDDEN) {
        out.push_str(";8");
    }
    if flags.contains(Flags::STRIKEOUT) {
        out.push_str(";9");
    }

    // Foreground color
    emit_color_sgr(out, fg, true);

    // Background color
    emit_color_sgr(out, bg, false);

    out.push('m');
}

/// Emit the SGR parameters for a single color (fg or bg).
///
/// IMPORTANT: We emit standard ANSI SGR codes for Named and Indexed colors
/// (not resolved RGB values) so that xterm.js resolves them through its theme.
/// If we looked up alacritty_terminal's default palette and emitted RGB, the
/// colors would not match the user's xterm.js theme (e.g. Tokyo Night).
fn emit_color_sgr(
    out: &mut String,
    color: Color,
    is_fg: bool,
) {
    match color {
        Color::Named(name) => {
            let code = match name {
                NamedColor::Black => 30,
                NamedColor::Red => 31,
                NamedColor::Green => 32,
                NamedColor::Yellow => 33,
                NamedColor::Blue => 34,
                NamedColor::Magenta => 35,
                NamedColor::Cyan => 36,
                NamedColor::White => 37,
                NamedColor::BrightBlack => 90,
                NamedColor::BrightRed => 91,
                NamedColor::BrightGreen => 92,
                NamedColor::BrightYellow => 93,
                NamedColor::BrightBlue => 94,
                NamedColor::BrightMagenta => 95,
                NamedColor::BrightCyan => 96,
                NamedColor::BrightWhite => 97,
                // Default foreground/background — skip (already in reset)
                NamedColor::Foreground | NamedColor::BrightForeground if is_fg => return,
                NamedColor::Background if !is_fg => return,
                // Dim colors — map to their base + dim flag (already handled via Flags::DIM)
                NamedColor::DimBlack => 30,
                NamedColor::DimRed => 31,
                NamedColor::DimGreen => 32,
                NamedColor::DimYellow => 33,
                NamedColor::DimBlue => 34,
                NamedColor::DimMagenta => 35,
                NamedColor::DimCyan => 36,
                NamedColor::DimWhite => 37,
                NamedColor::DimForeground => return,
                NamedColor::Cursor => return,
                _ => return,
            };
            let code = if !is_fg { code + 10 } else { code };
            out.push_str(&format!(";{}", code));
        }
        Color::Spec(rgb) => {
            // True color — must emit RGB directly
            if is_fg {
                out.push_str(&format!(";38;2;{};{};{}", rgb.r, rgb.g, rgb.b));
            } else {
                out.push_str(&format!(";48;2;{};{};{}", rgb.r, rgb.g, rgb.b));
            }
        }
        Color::Indexed(idx) => {
            if idx < 8 {
                // Standard ANSI colors — emit as SGR codes for theme resolution
                let base = if is_fg { 30 } else { 40 };
                out.push_str(&format!(";{}", base + idx));
            } else if idx < 16 {
                // Bright ANSI colors
                let base = if is_fg { 90 } else { 100 };
                out.push_str(&format!(";{}", base + idx - 8));
            } else {
                // 256-color palette (16-255) — emit as indexed for xterm.js
                if is_fg {
                    out.push_str(&format!(";38;5;{}", idx));
                } else {
                    out.push_str(&format!(";48;5;{}", idx));
                }
            }
        }
    }
}
