use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    pub id: Option<Value>,
    pub method: String,
    pub params: Option<Value>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: &'static str,
    pub id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
}

impl JsonRpcResponse {
    pub fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn error(id: Value, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

/// `tasks_enabled` gates the three task tools (docs/tasks.md §4). An agent that is never
/// primed to use them shouldn't be carrying their schemas in context either, so the
/// preference removes the surface rather than just the instruction.
pub fn tool_list_response(tasks_enabled: bool) -> Value {
    // Tools are built in batches to stay under the serde_json::json! macro recursion limit (128).
    // Each batch is a small Vec<Value> that gets extended into the final tools array.

    let mut tools: Vec<Value> = Vec::with_capacity(51);

    // Batch 1: Session, info, notification, logs, document tools
    tools.extend(serde_json::json!([
        {
            "name": "initSession",
            "description": "Call this tool once at the start of every session (new, resume, fork, compact). Registers your terminal tab ID and session ID so all subsequent tool calls automatically target your tab. Read your tab ID from the SessionStart hook context ('Your maiTerm tab ID is ...') or from the $MAITERM_TAB_ID environment variable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Your maiTerm tab ID (from SessionStart hook context or $MAITERM_TAB_ID env var)" },
                    "sessionId": { "type": "string", "description": "Your Claude session ID (optional, for session tracking)" }
                },
                "required": ["tabId"]
            }
        },
        {
            "name": "getOpenEditors",
            "description": "Get a list of all currently open editor tabs in the maiTerm IDE. Returns file paths, active state, language, and dirty (unsaved changes) status.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getWorkspaceFolders",
            "description": "Get the workspace folder paths currently open in maiTerm. Returns root paths for each workspace.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getDiagnostics",
            "description": "Get app diagnostics: version, tab/PTY counts, suspended tabs (inactive workspaces with stale pty_ids — normal, not a bug), uninitialized tabs (never had a PTY), orphaned PTYs (actual leaks), WebGL status, buffer sizes, state file size, PTY throughput, state save timing, trigger engine stats, render FPS, process memory/CPU, memory trend. Use this to investigate performance issues or health of the running maiTerm instance. Note: FPS probe takes ~1 second to measure.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "sendNotification",
            "description": "Send an in-app notification (toast) to the user. Use this to alert the user about important events, task completion, or questions that need attention. Respects the user's notification preferences (auto/in_app/native/disabled).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "title": { "type": "string", "description": "Notification title (short, e.g. 'Task Complete')" },
                    "body": { "type": "string", "description": "Notification body text with details" },
                    "type": { "type": "string", "enum": ["info", "success", "error"], "description": "Notification type (default: info). Affects the visual style." }
                },
                "required": ["title"]
            }
        },
        {
            "name": "readLogs",
            "description": "Read recent log entries from the maiTerm log file. Returns the last N lines, optionally filtered by log level or search string. Use this to investigate errors, warnings, or trace application behavior.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "lines": { "type": "number", "description": "Number of lines to return (default: 100, max: 1000). Returns the most recent lines." },
                    "level": { "type": "string", "description": "Filter by log level: DEBUG, INFO, WARN, ERROR. Only lines containing this level are returned." },
                    "search": { "type": "string", "description": "Filter lines containing this substring (case-sensitive)." }
                },
                "required": []
            }
        },
        {
            "name": "checkDocumentDirty",
            "description": "Check whether a document open in the maiTerm editor has unsaved changes.",
            "inputSchema": { "type": "object", "properties": { "filePath": { "type": "string" } }, "required": ["filePath"] }
        },
        {
            "name": "saveDocument",
            "description": "Save a document that is open in the maiTerm editor to disk.",
            "inputSchema": { "type": "object", "properties": { "filePath": { "type": "string" } }, "required": ["filePath"] }
        },
        {
            "name": "getCurrentSelection",
            "description": "Get the currently selected text and cursor position in the active maiTerm editor tab.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getLatestSelection",
            "description": "Get the most recent text selection made in any maiTerm editor tab.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }
    ]).as_array().unwrap().clone());

    // Batch 2: File/diff tools, windows, workspaces, tabs, notes
    tools.extend(serde_json::json!([
        {
            "name": "openFile",
            "description": "Open a file in the maiTerm IDE editor tab. Use this tool whenever you need to show the user a file — do NOT use shell 'open' or other OS commands. Supports optional line range or text range selection to highlight a specific section. Returns the tabId of the opened tab. To update an existing tab with a new file (e.g. iteratively showing screenshots, test results, or build output in the same tab), pass the returned tabId back as targetTabId — this replaces the tab content in-place without opening a new tab.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "Absolute path to the file to open" },
                    "targetTabId": { "type": "string", "description": "Replace the file in this existing editor tab (returned from a previous openFile call) instead of opening a new tab. Use this when iterating on the same visual output — screenshots, test results, generated images — so the user sees updates in-place without tab clutter." },
                    "startLine": { "type": "number", "description": "Line number to start selection (1-based)" },
                    "endLine": { "type": "number", "description": "Line number to end selection (1-based)" },
                    "startText": { "type": "string", "description": "Text string to find and start selection at" },
                    "endText": { "type": "string", "description": "Text string to find and end selection at" }
                },
                "required": ["filePath"]
            }
        },
        {
            "name": "openDiff",
            "description": "Show a diff of proposed file changes in the maiTerm IDE for the user to review, accept, or reject. Use this tool instead of directly writing files when you want the user to review changes. This is a blocking call — it waits for the user to accept or reject before returning.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "old_file_path": { "type": "string", "description": "Path to the original file (used to read current content)" },
                    "new_file_path": { "type": "string", "description": "Path where the modified file should be saved" },
                    "new_file_contents": { "type": "string", "description": "The complete new file contents to show in the diff" },
                    "tab_name": { "type": "string", "description": "Display name for the diff tab" }
                },
                "required": ["new_file_path", "new_file_contents"]
            }
        },
        {
            "name": "showDiff",
            "description": "Open a read-only diff tab showing a file's changes compared to a git ref. Non-blocking — returns immediately. Use this when the user asks to see what changed in a file (e.g. 'show me the diff', 'what changed in X'). Do NOT use openDiff for this — openDiff is for proposing edits.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "filePath": { "type": "string", "description": "Absolute path to the file to diff" },
                    "ref": { "type": "string", "description": "Git ref to compare against (default: HEAD). Can be a commit SHA, branch, tag, HEAD~N, etc." }
                },
                "required": ["filePath"]
            }
        },
        {
            "name": "closeAllDiffTabs",
            "description": "Close all open diff review tabs in the maiTerm IDE, rejecting any pending changes.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listWindows",
            "description": "List all maiTerm windows with their IDs, labels, and workspace summaries. Use this to discover windows before querying a specific window's workspaces via listWorkspaces with a windowId.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listWorkspaces",
            "description": "List all workspaces with their panes and tabs — the full picture of a window. Returns windowId, windowLabel, workspace IDs and names, pane structure, tab IDs, interpolated display names, tab types, active states and notes indicators. Each maiTerm window has its own set of workspaces; pass windowId to query a specific window.\n\nFOUR THINGS ARE OFTEN CONFUSED — they are separate, reported separately, and have different remedies:\n- `tab.pty: 'suspended'` — a SUSPENDED TAB: it had a terminal, that terminal is gone, and it sits in the pane tree with a Resume prompt. Routine, and it happens INSIDE perfectly active workspaces (suspending all but the active tab is normal housekeeping, and maiTerm auto-suspends idle workspaces). `suspendedAt` says since when. Wake it with resumeTab. A window full of these is normal, not a fleet of dead sessions.\n- `workspace.suspended` — the whole workspace is parked, so ALL of its tabs report pty 'suspended'. Resuming respawns exactly the ones that were live. Use resumeWorkspace; resumeTab refuses these and tells you so.\n- `workspace.archivedTabs[]` — tabs lifted OUT of the pane tree entirely. Not in any pane, no PTY, scrollback and cwd/ssh context preserved. Bring one back with restoreArchivedTab.\n- `tab.loaded: false` — the tab is in a pane and may well have a LIVE PTY, but its TerminalPane is not mounted right now (a background, suspended or never-opened workspace). Nothing can be typed into it or probed until it is, whatever its `state` says.\n\nEach agent tab carries three independent facts. `pty`: 'live' | 'suspended' | 'none' (never started one). `state` — meaningful only over a live PTY: 'active' (mid-turn), 'idle' (bound and waiting), 'permission' (stopped at a prompt — getTabPrompt/answerTabPrompt), 'unbound' (agent process alive but has not run /maiterm init, so nothing routes to it — recoverTab), 'stopped' (no agent; the tab is a shell — recoverTab restarts it), 'unknown' (not classified, usually because it is not loaded — do not guess). `loaded`: whether anything can reach it at all. Read all three before deciding: an 'idle' agent with loaded:false is healthy and undrivable, and a suspended tab is not a dead one.\n\n`overlordExempt: true` on a tab or a workspace means the human exempted it from Overlord: the engine runs nothing on it and every Overlord tool refuses it with reason 'exempt'. Leave it alone and do not raise it to the human.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "windowId": { "type": "string", "description": "Window ID (UUID) to list workspaces from. If omitted, uses the window this terminal belongs to." }
                },
                "required": []
            }
        },
        {
            "name": "switchTab",
            "description": "Navigate to a specific tab by its ID. Automatically switches to the correct workspace and pane. Use listWorkspaces first to discover tab IDs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "The tab ID to navigate to" }
                },
                "required": ["tabId"]
            }
        },
        {
            "name": "getTabNotes",
            "description": "Read the notes content for a terminal or editor tab. Returns the notes text and display mode.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID to read notes from. If omitted, uses the currently active tab." }
                },
                "required": []
            }
        },
        {
            "name": "setTabNotes",
            "description": "Write or clear notes for a terminal or editor tab. Set notes to null or empty string to clear.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID to write notes to. If omitted, uses the currently active tab." },
                    "notes": { "type": ["string", "null"], "description": "The notes content (markdown supported). Set to null or empty to clear." },
                    "mode": { "type": "string", "description": "Display mode: 'source' (edit) or 'render' (preview). Optional." }
                },
                "required": ["notes"]
            }
        }
    ]).as_array().unwrap().clone());

    // editTabNotes: deeply nested schema built from string to avoid macro recursion
    tools.push(serde_json::from_str(r#"{
        "name": "editTabNotes",
        "description": "Make precise edits to existing tab notes using string replacement. Supports a single edit (old_string + new_string) or multiple edits via an array of {old_string, new_string} objects. Edits are applied sequentially — later edits see the result of earlier ones. Each old_string must match uniquely. More efficient than setTabNotes when updating sections of longer notes. Use setTabNotes for full rewrites or clearing.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "tabId": { "type": "string", "description": "Tab ID to edit notes in. If omitted, uses the currently active tab." },
                "old_string": { "type": "string", "description": "The exact text to find in the notes. Must match uniquely. Use this for a single edit." },
                "new_string": { "type": "string", "description": "The replacement text. Use empty string to delete the matched section." },
                "edits": {
                    "type": "array",
                    "description": "Array of edits to apply sequentially. Use this instead of old_string/new_string for multiple edits in one call.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "old_string": { "type": "string", "description": "The exact text to find." },
                            "new_string": { "type": "string", "description": "The replacement text." }
                        },
                        "required": ["old_string", "new_string"]
                    }
                }
            }
        }
    }"#).unwrap());

    // Batch 3: Workspace notes, moveNote, tab context, notes panel
    tools.extend(serde_json::json!([
        {
            "name": "listWorkspaceNotes",
            "description": "List all notes attached to a workspace (not tab-level notes). Returns note IDs, content previews, modes, and timestamps.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspaceId": { "type": "string", "description": "Workspace ID. If omitted, uses the active workspace." }
                },
                "required": []
            }
        },
        {
            "name": "readWorkspaceNote",
            "description": "Read the full content of a workspace-level note by its ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspaceId": { "type": "string", "description": "Workspace ID. If omitted, uses the active workspace." },
                    "noteId": { "type": "string", "description": "The note ID to read" }
                },
                "required": ["noteId"]
            }
        },
        {
            "name": "writeWorkspaceNote",
            "description": "Create a new workspace-level note or update an existing one. Omit noteId to create, include noteId to update.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspaceId": { "type": "string", "description": "Workspace ID. If omitted, uses the active workspace." },
                    "noteId": { "type": "string", "description": "Note ID to update. Omit to create a new note." },
                    "content": { "type": "string", "description": "The note content (markdown supported)" },
                    "mode": { "type": ["string", "null"], "description": "Display mode: 'source' or 'render'. Optional." }
                },
                "required": ["content"]
            }
        },
        {
            "name": "deleteWorkspaceNote",
            "description": "Delete a workspace-level note by its ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspaceId": { "type": "string", "description": "Workspace ID. If omitted, uses the active workspace." },
                    "noteId": { "type": "string", "description": "The note ID to delete" }
                },
                "required": ["noteId"]
            }
        },
        {
            "name": "moveNote",
            "description": "Move a note between tab and workspace levels. 'tab_to_workspace' copies tab notes into a new workspace note and clears the tab. 'workspace_to_tab' moves a workspace note into a tab's notes and deletes the workspace note. Fails if the destination already has content — use force: true to overwrite, or read both notes first to merge manually.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "direction": { "type": "string", "description": "'tab_to_workspace' or 'workspace_to_tab'" },
                    "tabId": { "type": "string", "description": "Tab ID. If omitted, uses the active tab." },
                    "workspaceId": { "type": "string", "description": "Workspace ID. If omitted, uses the active workspace." },
                    "noteId": { "type": "string", "description": "Workspace note ID. Required for 'workspace_to_tab' direction." },
                    "force": { "type": "boolean", "description": "If true, overwrite destination content instead of failing on conflict. Default: false." }
                },
                "required": ["direction"]
            }
        },
        {
            "name": "getTabContext",
            "description": "Get recent terminal output or editor content from tabs to understand what the user was working on. If fewer than 10 total tabs exist, returns context for all tabs automatically. Otherwise, pass specific tab IDs. Each result includes the interpolated tab display name (highest-weight match signal), workspace name, tab type, and the last N lines of content. Use this to find the right tab when the user says things like 'switch to the tab where I was working on X'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabIds": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Specific tab IDs to get context from. If omitted and total tabs < 10, returns all tabs."
                    },
                    "lines": {
                        "type": "number",
                        "description": "Number of recent lines to return per tab. Default: 50."
                    }
                },
                "required": []
            }
        },
        {
            "name": "openNotesPanel",
            "description": "Open or close the notes panel for the current active tab. The panel shows either tab-level or workspace-level notes depending on the current scope. Always call this tool to perform the action — do not rely on previously returned status, as the user may have toggled the panel manually.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "open": { "type": "boolean", "description": "True to open, false to close. If omitted, toggles the current state." }
                },
                "required": []
            }
        },
        {
            "name": "setNotesScope",
            "description": "Switch the notes panel view between tab-level notes and workspace-level notes. The scope persists across tabs.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "scope": { "type": "string", "description": "Either 'tab' for per-tab notes or 'workspace' for workspace-level notes" }
                },
                "required": ["scope"]
            }
        },
        {
            "name": "getActiveTab",
            "description": "Get the currently active workspace, pane, and tab in the current window. Returns windowLabel, IDs, names, tab type, display name, and notes status. Use this as a lightweight alternative to listWorkspaces when you just need to know the current context. Prefer reading $MAITERM_TAB_ID for your own tab ID instead of calling this.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }
    ]).as_array().unwrap().clone());

    // Batch 4: Triggers, auto-resume, preferences, backup, sessions, archive
    tools.extend(serde_json::json!([
        {
            "name": "setTriggerVariable",
            "description": "Set or clear a trigger variable for a terminal tab. Trigger variables like %claudeSessionId are used in auto-resume commands and tab title interpolation. Setting 'claudeSessionId' will automatically enable auto-resume if the default Claude triggers are active — this is the recommended way to set up auto-resume for a Claude Code session. Set value to null to clear a variable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID. If omitted, uses the currently active tab." },
                    "name": { "type": "string", "description": "Variable name (e.g. 'claudeSessionId'). Referenced as %name in commands and titles." },
                    "value": { "type": ["string", "null"], "description": "Value to set. Pass null to clear the variable." }
                },
                "required": ["name", "value"]
            }
        },
        {
            "name": "getTriggerVariables",
            "description": "Get all trigger variables for a terminal tab. Returns variable names and values used in auto-resume commands, tab titles, and trigger conditions.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID. If omitted, uses the currently active tab." }
                },
                "required": []
            }
        },
        {
            "name": "setAutoResume",
            "description": "Enable or disable auto-resume for a terminal tab. When enabled, the tab will automatically replay the configured command on session restore. Disabling preserves all stored settings (SSH, CWD, command) — it only stops the auto-resume from firing. For Claude Code sessions, prefer using setTriggerVariable to set 'claudeSessionId' instead — this triggers auto-resume setup automatically with correct PTY context detection.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID. If omitted, uses the currently active tab." },
                    "enabled": { "type": "boolean", "description": "True to enable, false to disable. Disabling preserves all stored settings." },
                    "command": { "type": "string", "description": "Command to execute on resume. If omitted when enabling, uses the default Claude resume command template." },
                    "cwd": { "type": "string", "description": "Local working directory. If omitted, auto-detected from PTY." },
                    "sshCommand": { "type": "string", "description": "SSH connection target (e.g. 'user@host' or '-p 2222 user@host'). If omitted, auto-detected from PTY." },
                    "remoteCwd": { "type": "string", "description": "Remote working directory for SSH sessions. If omitted, auto-detected." }
                },
                "required": ["enabled"]
            }
        },
        {
            "name": "getAutoResume",
            "description": "Get the current auto-resume configuration for a terminal tab. Returns enabled state, pinned state, configured flag, and the stored command, CWD, SSH command, and remote CWD.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID. If omitted, uses the currently active tab." }
                },
                "required": []
            }
        },
        {
            "name": "findNotes",
            "description": "Search across all workspaces and tabs for notes content. Returns every tab and workspace note that exists, with content previews and tab display names. Use this to quickly find notes without having to list workspaces and check each tab individually.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getPreferences",
            "description": "Get maiTerm preferences (settings). Returns current values with metadata (description, type, valid values). Optionally filter by query string to find relevant settings.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Optional search query. Filters preferences whose key or description contains this string (case-insensitive). Omit to return all preferences." }
                },
                "required": []
            }
        },
        {
            "name": "setPreference",
            "description": "Update a single maiTerm preference by key. Use getPreferences first to discover available keys, their types, and valid values.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": { "type": "string", "description": "The preference key in snake_case (e.g. 'font_size', 'theme', 'cursor_style')" },
                    "value": { "description": "The new value. Type must match the preference (number, string, boolean)." }
                },
                "required": ["key", "value"]
            }
        },
        {
            "name": "createBackup",
            "description": "Create a gzip-compressed backup of the entire maiTerm state (workspaces, tabs, notes, preferences). Returns the path to the created backup file.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "directory": { "type": "string", "description": "Directory to save the backup. Defaults to the configured backup_directory preference." },
                    "excludeScrollback": { "type": "boolean", "description": "Exclude terminal scrollback buffers. Defaults to the backup_exclude_scrollback preference." }
                },
                "required": []
            }
        },
        {
            "name": "getClaudeSessions",
            "description": "Get all active Claude Code sessions across all tabs. Returns session IDs, states (active/waiting_input/waiting_permission/stopped), current tool being executed, model, working directory, and tab/workspace names. Use this for multi-agent coordination — check if Claude is running in other tabs before starting work, avoid conflicting edits, or wait for another session to finish.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listArchivedTabs",
            "description": "List a workspace's ARCHIVED tabs — sessions lifted out of the pane tree and put away, holding no PTY, restorable with restoreArchivedTab. Returns tab IDs, display names, archived dates and restore context (CWD, SSH command, auto-resume info). This is NOT the same as a suspended workspace, whose tabs are still listed in its panes with their PTYs killed and come back via resumeWorkspace. Use listWorkspaces to find workspaces with archivedTabCount > 0.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspaceId": { "type": "string", "description": "Workspace ID. If omitted, uses the currently active workspace." }
                }
            }
        },
        {
            "name": "restoreArchivedTab",
            "description": "Restore an archived tab back into the active pane of its workspace. The tab is inserted after the currently active tab. Use listArchivedTabs to find the tab ID first.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "workspaceId": { "type": "string", "description": "Workspace ID containing the archived tab. If omitted, uses the currently active workspace." },
                    "tabId": { "type": "string", "description": "The archived tab ID to restore." }
                },
                "required": ["tabId"]
            }
        },
        {
            "name": "sendToBridgedAgent",
            "description": "Send a message to a peer AI agent in another maiTerm pane. Two contexts: (1) a 1:1 Agent Bridge — omit `recipient`/`topic`, the message goes to your single bridged partner; (2) a Mesh Workspace — every agent here is reachable, so `recipient` (a peer's role name or tabId handle from listBridgedPeers) and `topic` (an existing topic id from listTopics, or a short new label to start a thread) are REQUIRED, and each message must be crafted for that one recipient (no broadcast). The recipient's reply arrives later as a new turn in your own prompt — this is asynchronous, so finish your current turn after sending. maiTerm stamps your identity (role, cwd) and the topic on the message so the recipient knows it's from you, a peer agent, NOT from a human. If your exchange is complete, just stop — do not reply only to acknowledge.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "The message. Be explicit: state who you are and why you're asking on first contact, then your question or information." },
                    "recipient": { "type": "string", "description": "Mesh only: the peer to send to — its role name (exact, case-insensitive) or its tabId handle from listBridgedPeers. Omit in a 1:1 bridge." },
                    "topic": { "type": "string", "description": "Mesh only: the conversation thread — an existing topic id from listTopics, or a short new label (e.g. 'auth-refactor') to start one (you become its owner). Omit in a 1:1 bridge." }
                },
                "required": ["message"]
            }
        },
        {
            "name": "getBridgedAgent",
            "description": "Check whether you are currently bridged to a peer AI agent and, if so, who. Returns the bridged agent's tab name, workspace, and working directory, or indicates that no bridge is active. Use this to discover the context of the agent you can reach via sendToBridgedAgent. In a Mesh Workspace use listBridgedPeers instead (there are many peers).",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listBridgedPeers",
            "description": "Mesh Workspace only: list every other agent you can reach on the mesh. Returns each peer's `handle` (the stable tabId to address it by), `role` (its display name), working directory, one-line purpose, and whether it's currently live. Use this to discover who to talk to before calling sendToBridgedAgent. Routing keys off the handle, so a role rename never misroutes.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listTopics",
            "description": "Mesh Workspace only: list the conversation topics (threads) in this mesh — id, label, state (open/complete), owner, participants, and turn count. Reuse an existing OPEN topic's id when replying instead of coining a new label for the same thread. Each message you send must be tagged with exactly one topic.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "startTopic",
            "description": "Mesh Workspace only: explicitly start (or reuse) a conversation topic and become its owner. Optional — sending with a new `topic` label also creates one. Returns the topic id to tag messages with. If an open topic with the same normalized label already exists, it is reused (no duplicate).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "label": { "type": "string", "description": "A short human-readable thread label, e.g. 'auth-refactor' or 'deploy plan'." }
                },
                "required": ["label"]
            }
        },
        {
            "name": "completeTopic",
            "description": "Mesh Workspace only: mark a topic complete. Only the topic's OWNER (the agent that started it) can complete it. maiTerm signals every participant that the thread is done; further sends on it are rejected. Call this when the thread's work is finished so peers stop replying.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "topicId": { "type": "string", "description": "The id of the topic to complete (from listTopics)." }
                },
                "required": ["topicId"]
            }
        }
    ]).as_array().unwrap().clone());

    // Batch: comms integration (/maiterm resolve — Mattermost thread binding)
    tools.extend(serde_json::json!([
        {
            "name": "bindCommsThread",
            "description": "Bind this tab to a Mattermost thread (/maiterm resolve). Fetches the whole thread via the configured bot account and returns it as a transcript ([REPORT] marks the root post — the work item), plus `bot_username` (how humans reach you) and, if the operator configured any, `operator_instructions` (their guidance for how to communicate — follow it). Attachments of any type (screenshots, PDFs, markdown, Office documents, logs) are downloaded and staged to temp files whose paths appear in the transcript, each with a note on how to open it — always open them before diagnosing. While bound, new human replies that @mention you are injected into this session. Rebinding replaces any prior binding on this tab. After binding, your FIRST action is a short ack posted to the thread (postCommsReply) — before investigating — so the humans know it was picked up.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "url": { "type": "string", "description": "Mattermost permalink: https://<server>/<team>/pl/<post-id>" }
                },
                "required": ["url"]
            }
        },
        {
            "name": "readCommsThread",
            "description": "Re-fetch the FULL current Mattermost thread this tab is bound to, as a transcript. Two uses, both routine — this call is cheap, prefer it over guessing: (1) catch up on ambient discussion, since only messages that @mention the bot are auto-injected into your session and the rest of the thread is read-on-demand; (2) RECOVER a thread you can no longer see. A summon for a thread you already worked delivers only the NEW messages, on the basis that the earlier ones were already given to this session — but a compaction or a /clear can have taken them from your context since. If you cannot see the history a pickup refers to, call this instead of asking the humans to repeat themselves. Attachments of any type are staged to temp files whose paths appear in the transcript, each with a note on how to open it.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "root_id": { "type": "string", "description": "Which bound thread — REQUIRED when this tab is bound to more than one thread; omit with a single binding" }
                },
                "required": []
            }
        },
        {
            "name": "sendFilesToPhone",
            "description": format!("Send files from this machine to the human's paired maiLink phone. They appear in this tab's chat on the phone AND in its cross-chat Files list, where they can be previewed or saved. Any file type. Use YOUR paths: local files normally; on an SSH tab, paths on the remote host (fetched back over the bridge tunnel). Max {} per file. Each path succeeds or fails on its own, so one bad path does not sink the batch. Only send what the human asked for or would want on their phone — this leaves the machine.", crate::mailink::assets::human_bytes(crate::mailink::assets::MAX_FILE_BYTES)),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "paths": { "type": "array", "items": { "type": "string" }, "description": "Absolute paths of the files to send" },
                    "caption": { "type": "string", "description": "One line saying what these are, shown with them in the chat" }
                },
                "required": ["paths"]
            }
        },
        {
            "name": "postCommsReply",
            "description": format!("Post a reply (Mattermost markdown) to a thread this tab is bound to. Set resolve: true when YOUR work on the thread is done — it posts the message and releases that thread's binding, freeing one of this tab's {} slots. Post-and-release is the normal ending: do NOT hold a finished thread waiting for a human to confirm, since an @mention on the thread summons you straight back — with whatever you have not already been shown, plus readCommsThread to pull the rest (unless the bind result said can_be_resummoned: false — then stay bound). The bot must be a member of the channel.", crate::comms::MAX_TAB_BINDINGS),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "message": { "type": "string", "description": "The message to post (Mattermost markdown)" },
                    "root_id": { "type": "string", "description": "Which bound thread to post to — REQUIRED when this tab is bound to more than one thread; omit with a single binding" },
                    "resolve": { "type": "boolean", "description": "true = this is the confirmed-close post; clears that thread's binding after posting" },
                    "attachments": { "type": "array", "items": { "type": "string" }, "description": "Absolute file paths of files to upload and attach to the post — any type (screenshots, PDFs, logs, documents), max 5, 20 MB each. Use YOUR paths: local files normally; on an SSH tab, paths on the remote host (fetched back over the bridge)" }
                },
                "required": ["message"]
            }
        },
        {
            "name": "startCommsThread",
            "description": format!("Open a NEW Mattermost thread in a channel this tab monitors — for raising something yourself (an incident you found, a heads-up, a question for the channel) rather than replying to an existing thread. Posts a root message and binds this tab to the new thread, so replies that @mention the bot come back to you like any other bound thread (pass bind: false to post without binding — then you won't see replies). Only channels the operator put on this tab's monitor list are allowed. Counts against the same {}-thread cap as summons.", crate::comms::MAX_TAB_BINDINGS),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "message": { "type": "string", "description": "The opening post (Mattermost markdown). @mention the people who should see it — a new thread notifies nobody otherwise" },
                    "channel": { "type": "string", "description": "Which monitored channel — REQUIRED when this tab monitors more than one; omit with a single channel" },
                    "bind": { "type": "boolean", "description": "Default true: bind this tab to the new thread so replies are delivered. false = fire-and-forget post" },
                    "attachments": { "type": "array", "items": { "type": "string" }, "description": "Absolute file paths of files to attach — any type (screenshots, PDFs, logs, documents), max 5, 20 MB each. Use YOUR paths: local files normally; on an SSH tab, paths on the remote host" }
                },
                "required": ["message"]
            }
        },
        {
            "name": "unbindCommsThread",
            "description": "Clear one of this tab's Mattermost thread bindings without posting (e.g. when abandoning the issue). Post a brief note via postCommsReply first so the thread isn't left hanging. Idempotent.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "root_id": { "type": "string", "description": "Which bound thread — REQUIRED when this tab is bound to more than one thread; omit with a single binding" }
                },
                "required": []
            }
        }
    ]).as_array().unwrap().clone());

    if tasks_enabled {
    // ── maiTerm tasks (docs/tasks.md §5) ──
    // Three batched tools, deliberately no delete: an agent may mark a task done, only a
    // human removes one. All scoped to the CALLING TAB'S WORKSPACE via connection→tab
    // affinity, so a tab cannot see or touch another project's list.
    tools.extend(serde_json::json!([
        {
            "name": "listTasks",
            "description": "List the tasks maiTerm is tracking for this project (the workspace this tab belongs to), grouped by workstream. Returns each task's id, title, detail, status, workstream and assignee tab. Dependencies come back resolved, not as bare ids: `blocked_by` gives each prerequisite's title and a `state` — 'met' finished, 'waiting' still live, 'parked' off the list with an archived tab (still blocks), 'gone' deleted (does not block). `blocking` is the reverse edge, the tasks waiting on this one — check it before you go idle, so you know what you just released. Use scope 'tab' for just your own, 'workspace' (default) for the whole project including other agents' work and unassigned tasks.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "scope": { "type": "string", "enum": ["tab", "workspace"], "description": "Default 'workspace'" }
                },
                "required": []
            }
        },
        {
            "name": "createTasks",
            "description": "Add tasks to this project. Batch related items into ONE call. If you are working on more than one distinct thing, pass a `workstream` name per call so each job stays separate — that name is what your human sees as the group heading. Creation is idempotent: an item whose title matches one already on your tab in the same workstream returns the existing task instead of duplicating it, so re-sending your list is safe. Tasks default to assigned to you; pass assign_to_me false to leave one unassigned for whoever picks it up. Status defaults to 'todo' — use 'backlog' ONLY to park something you are deliberately deferring (next month, a future idea), since parked tasks are excluded from progress tracking.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "workstream": { "type": "string", "description": "Name of the job these tasks belong to, e.g. 'Auth refactor'. Reused if it already exists; created if not. Omit for loose tasks." },
                    "tasks": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string", "description": "One line, imperative — 'Add the auth guard'" },
                                "detail": { "type": "string", "description": "Optional body: acceptance criteria, links, notes. Markdown." },
                                "status": { "type": "string", "enum": ["backlog", "todo", "active", "blocked", "review", "done"], "description": "Defaults to 'todo'. 'backlog' means deliberately parked, not 'not started yet'." },
                                "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "Task ids that must finish first" },
                                "assign_to_me": { "type": "boolean", "description": "Default true" }
                            },
                            "required": ["title"]
                        }
                    }
                },
                "required": ["tasks"]
            }
        },
        {
            "name": "updateTasks",
            "description": "Update tasks on this project — keep statuses current as you work, so your human and this window's board see real progress. Batch related updates into ONE call. Pass `workstream` to move a task into a different job, and `assign_to` to change who owns one — claim an unassigned task with \"me\", hand one to another tab by its id, or release yours with null. There is no delete, and the three ways a task leaves the board are NOT interchangeable: 'done' means you finished it, 'backlog' means you are deliberately deferring it, and 'dropped' means it should not have been on the list at all — you misread the work, it was superseded, or it was decided against. Use 'dropped' for those rather than closing them as done: a dropped task does not satisfy anything waiting on it, so nothing you were blocking gets falsely released. Only a human deletes a row outright.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "updates": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": { "type": "string", "description": "Task id from listTasks/createTasks" },
                                "status": { "type": "string", "enum": ["backlog", "todo", "active", "blocked", "review", "done", "dropped"], "description": "'dropped' retracts a task that should not have been on the list — it is not a finish, and nothing blocked on it comes free" },
                                "title": { "type": "string" },
                                "detail": { "type": "string" },
                                "workstream": { "type": "string", "description": "Move this task into the named job (created if new)" },
                                "blocked_by": { "type": "array", "items": { "type": "string" }, "description": "Replace the whole dependency set. Prefer block_on/unblock_from unless you know the complete list — a replace clobbers any edge added concurrently." },
                                "block_on": { "type": "array", "items": { "type": "string" }, "description": "Add these task ids as prerequisites, leaving existing ones alone" },
                                "unblock_from": { "type": "array", "items": { "type": "string" }, "description": "Remove these task ids from this task's prerequisites" },
                                "assign_to": { "description": "Who owns this task: a tab id from listWorkspaces, the literal \"me\" to claim it for yourself, or null to release it so any tab can pick it up. The tab must be in this project. Omit to leave the assignee alone — omitted and null mean different things.", "type": ["string", "null"] }
                            },
                            "required": ["id"]
                        }
                    }
                },
                "required": ["updates"]
            }
        }
    ]).as_array().unwrap().clone());
    }

    // ── Overlord tools (docs/overlord.md §8, §10, §12) ──
    // replyToOverlord is for every supervised agent; the other three are for the
    // Overlord agent tab only (the frontend refuses callers outside the Overlord
    // workspace). All frontend-handled — they round-trip into the window's engine.
    tools.extend(serde_json::json!([
        {
            "name": "replyToOverlord",
            "description": "Report to this window's Overlord (the supervisor coordinating work across tabs). One shape, four uses: kind 'ready' when you come up, 'ack' when you finish something you were asked to do, 'status' for a state change worth recording (e.g. blocked), 'escalate' when you need attention. Set needs_human ONLY for things a human must decide — it raises a real escalation. Keep summary under 280 chars.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "kind": { "type": "string", "enum": ["ready", "ack", "status", "escalate"] },
                    "state": { "type": "string", "enum": ["working", "blocked", "done", "idle"] },
                    "summary": { "type": "string", "description": "Plain prose, ≤280 chars" },
                    "task": { "type": "string", "description": "What you believe you're working on" },
                    "blockers": { "type": "array", "items": { "type": "string" } },
                    "next": { "type": "string" },
                    "needs_human": { "type": "boolean", "description": "true = a human decision is required" }
                },
                "required": ["kind", "state", "summary"]
            }
        },
        {
            "name": "listEscalations",
            "description": "Overlord agent only: pull the queued escalations for this window (step timeouts, blocked agents, unacked directives, agents asking for a human). Returns each with its tab, workspace and detail, and marks them read. Call this when a wake nudge says escalations are pending.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" }
                },
                "required": []
            }
        },
        {
            "name": "driveTab",
            "description": "Overlord agent only: inject a directive into another tab in this window, typed with the human's full authority (the target cannot tell it from the human). kind 'process' = free-text directive; 'slash' = a slash command like /compact. The same mechanical guards as automated rules apply, and a structured refusal comes back with `reason` and usually `detail`. Read `detail` — the reasons are NOT interchangeable: `not_registered` means an agent IS running there but hasn't run /maiterm init, so it is not dead and /maiterm init has already been sent for you — just retry in a few seconds. `no_live_repl` means nothing is running and the tab needs restarting. `not_classified` means the tab's pane isn't mounted (suspended or never-opened workspace). `awaiting_permission` means it is stopped at a prompt, which retrying will NOT clear — use getTabPrompt + answerTabPrompt. `already_running` means a RULE is mid-way through a sequence on that tab and the text you sent is one of its steps: the engine sends it itself, so stand down — do not retry and do not hand-drive the rest of that sequence, or the human gets every directive twice. `outstanding_directive` means the tab owes an answer to an earlier directive (or a rule owns it) — that one you may retry once it clears. Also: agent_busy, runtime_mismatch. Every call lands verbatim in the ledger.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id (from listWorkspaces)" },
                    "kind": { "type": "string", "enum": ["process", "slash"] },
                    "text": { "type": "string", "description": "The exact text to type into the target tab" }
                },
                "required": ["tab_id", "kind", "text"]
            }
        },
        {
            "name": "getTabPrompt",
            "description": "Overlord agent only: what is currently BLOCKING a tab, if anything. Returns null when nothing is open. kind 'permission' = a tool gate (fields: tool, detail — the command or argument being approved). kind 'question' = an AskUserQuestion the agent raised (field: questions[] with each question's options). Always carries prompt_id — pass it back to answerTabPrompt so a slow decision can never answer a prompt that opened since.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id (from listWorkspaces)" }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "answerTabPrompt",
            "description": "Overlord agent only: answer a tab's open prompt, with the human's authority. For kind 'permission' pass `choice` ('1' approve / '2' approve and don't ask again / '3' deny, or the option label). For kind 'question' pass `answers` — one entry per question in order, each with `selected` (option labels verbatim) and/or `other` (free text). ALWAYS read getTabPrompt first and pass its prompt_id. ESCALATE INSTEAD OF ANSWERING when the decision is consequential — anything destructive or irreversible (deleting data, force-push, dropping a database, rm -rf), anything touching money, credentials, production, or an external party, or any question about what the human actually WANTS rather than how to carry out what they already asked for. Those go to the human via AskUserQuestion. Routine approvals in service of work already underway are yours to make. Every answer is recorded in the ledger.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id" },
                    "prompt_id": { "type": "string", "description": "From getTabPrompt — the stale-guard" },
                    "choice": { "type": "string", "description": "kind 'permission': '1' | '2' | '3' or the option label" },
                    "answers": {
                        "type": "array",
                        "description": "kind 'question': one entry per question, in order",
                        "items": {
                            "type": "object",
                            "properties": {
                                "selected": { "type": "array", "items": { "type": "string" }, "description": "Chosen option labels, verbatim" },
                                "other": { "type": "string", "description": "Free-text answer via the Other row" }
                            }
                        }
                    }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "proposeRuleChanges",
            "description": "Overlord agent only: propose changes to the Overlord ruleset. Nothing applies without explicit human approval — the human approves or rejects each change individually in a native prompt. Batch related changes into ONE call. Guards (require_live_repl, only_if_no_outstanding, max_per_hour) are not proposable at all. Do not re-propose rejected changes.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "rationale": { "type": "string", "description": "Why, in one paragraph — shown to the human" },
                    "changes": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "op": { "type": "string", "enum": ["create", "update", "rescope", "enable", "disable", "delete"] },
                                "rule": overlord_rule_schema(),
                                "rule_id": { "type": "string", "description": "ops other than create: target rule id or default_id" },
                                "patch": { "type": "object", "description": "op update: partial rule fields (guards are ignored)" },
                                "workspaces": { "type": "array", "items": { "type": "string" }, "description": "op rescope: new workspace scope ([] = global)" }
                            },
                            "required": ["op"]
                        }
                    }
                },
                "required": ["rationale", "changes"]
            }
        },
        {
            "name": "archiveTab",
            "description": "Overlord agent only: archive a finished session — RECOVERABLE. The tab leaves the pane tree keeping its scrollback, cwd and ssh context, and comes back with restoreArchivedTab. Use this when there is any chance of returning to that session — a bug in what it built, or follow-up work on it. This is the DEFAULT choice for a finished session; prefer it to closeTab whenever you are unsure. A suspended tab can be archived directly — there is no process to end, so it is the safest thing to put away. Refuses, with `reason` and `detail`, anything still working: agent_busy (mid-turn), awaiting_permission (stopped at a prompt — that work is not over, it is waiting), agent_running_unbound (an agent IS alive there and archiving kills its PTY — recoverTab first), tab_in_use (output or a keystroke in the last 60s — a tab with no AGENT is not necessarily idle; a shell part-way through a build looks exactly like one), not_classified (the liveness probe has not reached it yet — retry), outstanding_directive, not_boardable. Ledgered.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id — the session to archive" }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "closeTab",
            "description": "Overlord agent only: close a session — IRREVERSIBLE. The PTY is killed, bridges are torn down, and NO archive entry is kept: the scrollback and context are gone. Use this only when the session is definitively over, or when starting a fresh session would serve just as well. If you would ever want to read that session again, use archiveTab instead — archiving costs nothing and is undoable, this is not. Same refusals as archiveTab (agent_busy, awaiting_permission, agent_running_unbound, tab_in_use, not_classified, outstanding_directive, not_boardable), and the same rule behind them: never take away a tab that is still working — with a longer quiet window, 5 minutes rather than 60s, because this cannot be undone. Ledgered.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id — the session to close for good" }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "recoverTab",
            "description": "Overlord agent only: get an agent tab responding again. The remedy is chosen from the tab's process state, not guessed — 'unbound' (agent alive, never ran /maiterm init) gets /maiterm init typed into it, which restores routing; 'stopped' (nothing running) gets the runtime's resume command, relaunching the agent. Returns `kind` telling you which it did. IMPORTANT: `sent: true` means the line was TYPED, not that the tab recovered — it always comes back with `verified: false`, because an agent still coming up (especially a remote one over SSH replaying its transcript) swallows the line silently and nothing at the moment of typing can tell. Do not report a tab recovered on this result. maiTerm watches for 45s and raises a `rebind_failed` escalation if it did not take, telling you to call recoverTab again — by then the tab reads 'stopped' rather than 'unbound', so the second call relaunches the agent instead of re-typing an init already shown not to work. Use it on any tab listWorkspaces reports as state 'unbound' or 'stopped'. Refuses `no_terminal` when the tab has no live terminal to type into — a suspended tab needs resumeTab, and a tab in a suspended workspace needs resumeWorkspace — and `not_unready` when the tab's agent is running and bound, i.e. there is nothing to recover.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id" }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "deleteArchivedTab",
            "description": "Overlord agent only: permanently delete an ARCHIVED tab — the archive's own disposition verb. closeTab only reaches tabs still in a pane, so without this an archive could be added to and restored from but never pruned. Irreversible, but with none of closeTab's danger: an archived tab holds no PTY and no process, so only the record is destroyed. Read its notes with getTabNotes first if you want to know what it was. Refuses `not_archived` when the id is not in any workspace's archivedTabs[].",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "The archived tab id to delete" }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "resumeTab",
            "description": "Overlord agent only: wake ONE suspended tab — a tab reported with `pty: 'suspended'`, whose terminal was killed but which is still in its pane with a Resume prompt. This happens inside active workspaces (suspending every tab but the active one is routine), so it is NOT the same as an archived tab or a suspended workspace. Brings the tab into view and respawns its terminal, restoring cwd and ssh context; an agent with auto-resume set comes back with it. Refuses `already_live` (nothing to resume) and `workspace_suspended` — if the whole workspace is parked, use resumeWorkspace, which brings back every tab that was live in it rather than just this one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "tab_id": { "type": "string", "description": "TARGET tab id — the suspended tab to wake" }
                },
                "required": ["tab_id"]
            }
        },
        {
            "name": "resumeWorkspace",
            "description": "Overlord agent only: bring a SUSPENDED workspace back, respawning exactly the tabs that were live when it was suspended. This is the answer to tabs reported with `loaded: false` in a workspace whose `suspended` is true — until it is resumed, nothing can be typed into or probed in any of them, whatever their `state` says. Not for archived tabs: those are individual sessions lifted out of the pane tree, and come back with restoreArchivedTab.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Tab ID (auto-injected after initSession)" },
                    "workspace_id": { "type": "string", "description": "The suspended workspace to resume" }
                },
                "required": ["workspace_id"]
            }
        }
    ]).as_array().unwrap().clone());

    serde_json::json!({ "tools": tools })
}

/// Schema for `proposeRuleChanges`' `rule` field (op `create`).
///
/// Spelled out because a bare `{"type":"object"}` let a create with no `when` and no
/// `sequence` validate here, render fine in the human's approval prompt, and then apply to
/// nothing — approved, ledgered, and absent from the ruleset. The engine validates these
/// same fields again before the prompt opens; this is what tells the agent the shape in the
/// first place.
///
/// Its own function rather than inline: nesting it in the tools `json!` blew the macro's
/// recursion limit.
fn overlord_rule_schema() -> Value {
    serde_json::json!({
        "type": "object",
        "description": "op create: the full rule (id and guards are set by maiTerm; guards sent here are ignored)",
        "required": ["name", "when", "sequence"],
        "properties": {
            "name": { "type": "string", "description": "Short rule name shown in Preferences" },
            "description": { "type": "string" },
            "enabled": { "type": "boolean", "description": "Default true" },
            "cooldown": { "type": "number", "description": "Seconds, per tab. Default 1800" },
            "workspaces": { "type": "array", "items": { "type": "string" }, "description": "[] or omitted = global" },
            "when": {
                "type": "object",
                "description": "The condition. Each event takes its own extra field: context_pct→at_or_above, tab_idle→minutes, task_stale→days, permission_pending→minutes, directive_unacked→minutes; turn_end, commit, agent_unready and no_todo_list take none.",
                "required": ["event"],
                "properties": {
                    "event": { "type": "string", "enum": ["context_pct", "turn_end", "commit", "tab_idle", "task_stale", "agent_unready", "no_todo_list", "permission_pending", "directive_unacked"] },
                    "at_or_above": { "type": "number" },
                    "minutes": { "type": "number" },
                    "days": { "type": "number" }
                }
            },
            "sequence": {
                "type": "array",
                "description": "One step = a directive; more than one = a gated ritual",
                "minItems": 1,
                "items": {
                    "type": "object",
                    "required": ["kind", "text"],
                    "properties": {
                        "kind": { "type": "string", "enum": ["process", "slash"] },
                        "text": { "type": "string", "description": "The directive, typed verbatim into the tab" },
                        "runtimes": { "type": "array", "items": { "type": "string", "enum": ["claude", "codex", "gemini"] }, "description": "slash steps only; omit = all" },
                        "await": { "type": "object", "description": "Gate to wait for after injection; omit = fire-and-forget" },
                        "timeout_seconds": { "type": "number" },
                        "on_timeout": { "type": "string", "enum": ["abort", "continue", "notify_human", "escalate_to_overlord"] }
                    }
                }
            },
            "supersedes": { "type": "array", "items": { "type": "string" }, "description": "Rule ids / default_ids this replaces in scope" }
        }
    })
}

pub fn initialize_response(client_protocol_version: Option<&str>) -> Value {
    serde_json::json!({
        "protocolVersion": client_protocol_version.unwrap_or("2025-03-26"),
        "capabilities": { "tools": {} },
        "serverInfo": { "name": crate::APP_DISPLAY_NAME, "version": crate::APP_VERSION },
        "instructions": format!(
            "You are running inside a maiTerm terminal tab. Your tab is identified automatically — every request \
             you make carries it, and your SessionStart hook registers your session — so you do NOT need to call \
             initSession to be correctly targeted, and should not spend an opening turn on it. \
             Call initSession only to REPAIR identity: when a maiterm tool answers that it does not know your tab, \
             that your tab was inferred rather than stated, or when the human asks (/maiterm init). \
             Pass your tab ID from $MAITERM_TAB_ID or the SessionStart hook context, and do NOT batch that call with \
             other maiterm tool calls, which would race the registration and can target the wrong tab. \
             IMPORTANT: You MUST use tools from the '{}' MCP server ONLY. Do NOT use tools from any other maiterm MCP server. \
             IMPORTANT: Always call initSession when requested via /maiterm init, even if you believe it was already called. \
             Resume, fork, and compact events require re-initialization to pick up state changes.",
            crate::state::agent_runtime::mcp_server_name(crate::state::AgentRuntime::Claude)
        )
    })
}
