pub mod app_state;
pub mod persistence;
pub mod scrollback_db;
pub mod workspace;

pub use app_state::{AppState, FileWatcherHandle, PendingResize, PtyCommand, PtyHandle, PtyStats, RemoteFileWatch};
pub use persistence::{load_state, save_state};
pub use scrollback_db::ScrollbackDb;
pub use workspace::{AgentBridge, AppData, DiffContext, EditorFileInfo, Pane, Preferences, Tab, WindowData, WindowGeometry, Workspace};
