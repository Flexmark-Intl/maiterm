pub mod manager;

pub use manager::{
    get_agent_liveness, get_pty_foreground, get_pty_info, kill_pty, last_output_ms, list_live_ptys,
    pty_child_pid_of, resize_pty, shell_holds_tty, spawn_pty, write_pty, AgentLiveness, PtyInfo,
};
