use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::selection::Selection;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte;
use tauri::{AppHandle, Emitter};

use super::event_proxy::AitermEventProxy;
use super::osc::OscInterceptor;
use super::render::{self, FrameCache, FrameMeta};

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
    /// Rows of the last frame delivered to this terminal's xterm, so the next
    /// one can be a delta. `None` forces a full repaint. See `render::FrameCache`.
    pub frame_cache: Option<FrameCache>,
    /// Whether the owning tab is on screen. A hidden tab gets no frames at all —
    /// its xterm deliberately falls behind, and is caught up with one full
    /// repaint when it is shown again (`set_terminal_visible`).
    pub visible: bool,
    /// The (cols, rows) the frontend's xterm was last fitted to, reported by
    /// `refresh_terminal_frame`. While it differs from the grid — the PTY resize
    /// is debounced up to ~400ms behind the fit — every frame is a full repaint,
    /// because a delta's `ESC[row;1H` addresses would land on the wrong rows of
    /// a reflowed buffer and xterm clamps rows past its height onto the last one.
    pub xterm_dims: Option<(usize, usize)>,
}

impl TerminalHandle {
    /// Render the next frame and emit it on `term-frame-{pty_id}`. This is the
    /// only place frames are produced, so every frame xterm receives is a
    /// delta against the one before it, in lock order — commands and the PTY
    /// emitter thread both go through here under the registry write lock.
    ///
    /// `force_full` drops the row cache first, for anything that may have
    /// disturbed xterm's buffer without going through this path (a resize
    /// reflow, a tab that was hidden and skipped frames).
    ///
    /// Hidden tabs render nothing; the cache is dropped so the reveal is a
    /// full repaint. Returns the frame's metadata either way.
    pub fn emit_frame(&mut self, app_handle: &AppHandle, pty_id: &str, force_full: bool) -> FrameMeta {
        // MAITERM_FULL_FRAMES=1 restores the pre-delta behaviour (every frame a
        // full repaint, hidden tabs included) for A/B measurement on one binary.
        let legacy = legacy_full_frames();
        let geometry_mismatch = self
            .xterm_dims
            .map_or(false, |d| d != (self.term.columns(), self.term.screen_lines()));
        if force_full || legacy || geometry_mismatch || !self.visible {
            self.frame_cache = None;
        }
        if !self.visible && !legacy {
            return render::frame_meta(&self.term, self.selection.as_ref());
        }
        let frame = render::render_frame(&self.term, self.selection.as_ref(), &mut self.frame_cache);
        let meta = FrameMeta::from(&frame);
        // Hand the frame to the delivery thread rather than emitting inline.
        // `app.emit` evaluates immediately when called on the main thread and
        // queues on the event loop from any other thread — so a sync command's
        // frame would overtake an emitter-thread frame rendered just before it,
        // and a delta applied out of order stays wrong. Every producer is under
        // the registry write lock here, so channel order is lock order, and the
        // delivery thread is never the main thread, so emit order is channel order.
        let _ = frame_sink(app_handle).send((format!("term-frame-{}", pty_id), frame));
        meta
    }
}

type FrameSink = std::sync::Mutex<std::sync::mpsc::Sender<(String, render::TerminalFrame)>>;

fn frame_sink(app_handle: &AppHandle) -> std::sync::MutexGuard<'static, std::sync::mpsc::Sender<(String, render::TerminalFrame)>> {
    static SINK: std::sync::OnceLock<FrameSink> = std::sync::OnceLock::new();
    SINK.get_or_init(|| {
        let (tx, rx) = std::sync::mpsc::channel::<(String, render::TerminalFrame)>();
        let app = app_handle.clone();
        std::thread::Builder::new()
            .name("term-frame-delivery".into())
            .spawn(move || {
                for (event, frame) in rx {
                    let _ = app.emit(&event, &frame);
                }
            })
            .expect("spawn term-frame-delivery thread");
        std::sync::Mutex::new(tx)
    })
    .lock()
    .unwrap_or_else(|e| e.into_inner())
}

fn legacy_full_frames() -> bool {
    static LEGACY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *LEGACY.get_or_init(|| std::env::var_os("MAITERM_FULL_FRAMES").is_some())
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
        frame_cache: None,
        visible: true,
        xterm_dims: None,
    }
}
