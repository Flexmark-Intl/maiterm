pub mod manager;

pub use manager::{
    get_agent_liveness, get_pty_foreground, get_pty_info, kill_pty, last_output_ms, list_live_ptys,
    resize_pty, spawn_pty, write_pty, AgentLiveness, PtyInfo,
};
