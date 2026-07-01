use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::selection::Selection;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte;

use super::event_proxy::AitermEventProxy;
use super::osc::OscInterceptor;

/// Dimensions implementation for creating/resizing Term instances.
pub struct TermDimensions {
    pub cols: usize,
    pub rows: usize,
}

impl Dimensions for TermDimensions {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Wraps one alacritty_terminal instance with its associated state.
pub struct TerminalHandle {
    pub term: Term<AitermEventProxy>,
    pub osc_interceptor: OscInterceptor,
    /// VTE processor for feeding bytes to the terminal.
    pub processor: vte::ansi::Processor,
    /// User selection managed externally (not on term.selection which gets
    /// cleared by VTE processing). Stored here so it survives PTY output.
    pub selection: Option<Selection>,
    /// One-shot latch: set once maiTerm has auto-answered Claude Code's blocking
    /// "Resume from summary?" startup menu for this PTY so it never re-injects.
    pub resume_menu_handled: bool,
    /// Small rolling tail of recent output, kept only during session start (until
    /// the menu is handled or the scan budget is spent) so the multi-line menu
    /// signature is still matched when it straddles two PTY reads. See
    /// `detect_resume_menu` in pty/manager.rs.
    pub resume_scan_tail: Vec<u8>,
}

/// Create a new alacritty_terminal instance.
pub fn create_terminal(
    cols: u16,
    rows: u16,
    scrollback_limit: usize,
    event_proxy: AitermEventProxy,
) -> TerminalHandle {
    let config = Config {
        scrolling_history: scrollback_limit,
        ..Config::default()
    };

    let dims = TermDimensions {
        cols: cols as usize,
        rows: rows as usize,
    };

    let term = Term::new(config, &dims, event_proxy);
    let processor = vte::ansi::Processor::default();
    let osc_interceptor = OscInterceptor::new();

    TerminalHandle {
        term,
        osc_interceptor,
        processor,
        selection: None,
        resume_menu_handled: false,
        resume_scan_tail: Vec::new(),
    }
}
