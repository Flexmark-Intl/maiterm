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

pub fn tool_list_response() -> Value {
    // Tools are built in batches to stay under the serde_json::json! macro recursion limit (128).
    // Each batch is a small Vec<Value> that gets extended into the final tools array.

    let mut tools: Vec<Value> = Vec::with_capacity(42);

    // Batch 1: Session, info, notification, logs, document tools
    tools.extend(serde_json::json!([
        {
            "name": "initSession",
            "description": "Call this tool once at the start of every session (new, resume, fork, compact). Registers your terminal tab ID and session ID so all subsequent tool calls automatically target your tab. Read your tab ID from the SessionStart hook context ('Your aiTerm tab ID is ...') or from the $AITERM_TAB_ID environment variable.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tabId": { "type": "string", "description": "Your aiTerm tab ID (from SessionStart hook context or $AITERM_TAB_ID env var)" },
                    "sessionId": { "type": "string", "description": "Your Claude session ID (optional, for session tracking)" }
                },
                "required": ["tabId"]
            }
        },
        {
            "name": "getOpenEditors",
            "description": "Get a list of all currently open editor tabs in the aiTerm IDE. Returns file paths, active state, language, and dirty (unsaved changes) status.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getWorkspaceFolders",
            "description": "Get the workspace folder paths currently open in aiTerm. Returns root paths for each workspace.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getDiagnostics",
            "description": "Get app diagnostics: version, tab/PTY counts, suspended tabs (inactive workspaces with stale pty_ids — normal, not a bug), uninitialized tabs (never had a PTY), orphaned PTYs (actual leaks), WebGL status, buffer sizes, state file size, PTY throughput, state save timing, trigger engine stats, render FPS, process memory/CPU, memory trend. Use this to investigate performance issues or health of the running aiTerm instance. Note: FPS probe takes ~1 second to measure.",
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
            "description": "Read recent log entries from the aiTerm log file. Returns the last N lines, optionally filtered by log level or search string. Use this to investigate errors, warnings, or trace application behavior.",
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
            "description": "Check whether a document open in the aiTerm editor has unsaved changes.",
            "inputSchema": { "type": "object", "properties": { "filePath": { "type": "string" } }, "required": ["filePath"] }
        },
        {
            "name": "saveDocument",
            "description": "Save a document that is open in the aiTerm editor to disk.",
            "inputSchema": { "type": "object", "properties": { "filePath": { "type": "string" } }, "required": ["filePath"] }
        },
        {
            "name": "getCurrentSelection",
            "description": "Get the currently selected text and cursor position in the active aiTerm editor tab.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "getLatestSelection",
            "description": "Get the most recent text selection made in any aiTerm editor tab.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }
    ]).as_array().unwrap().clone());

    // Batch 2: File/diff tools, windows, workspaces, tabs, notes
    tools.extend(serde_json::json!([
        {
            "name": "openFile",
            "description": "Open a file in the aiTerm IDE editor tab. Use this tool whenever you need to show the user a file — do NOT use shell 'open' or other OS commands. Supports optional line range or text range selection to highlight a specific section. Returns the tabId of the opened tab. To update an existing tab with a new file (e.g. iteratively showing screenshots, test results, or build output in the same tab), pass the returned tabId back as targetTabId — this replaces the tab content in-place without opening a new tab.",
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
            "description": "Show a diff of proposed file changes in the aiTerm IDE for the user to review, accept, or reject. Use this tool instead of directly writing files when you want the user to review changes. This is a blocking call — it waits for the user to accept or reject before returning.",
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
            "description": "Close all open diff review tabs in the aiTerm IDE, rejecting any pending changes.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listWindows",
            "description": "List all aiTerm windows with their IDs, labels, and workspace summaries. Use this to discover windows before querying a specific window's workspaces via listWorkspaces with a windowId.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        },
        {
            "name": "listWorkspaces",
            "description": "List all workspaces with their panes and tabs. Returns windowId, windowLabel, workspace IDs, names, pane structure, tab IDs, interpolated display names, tab types, active states, and notes indicators. Use this to discover tabs for switchTab or notes operations. Each aiTerm window has its own set of workspaces; pass windowId to query a specific window.",
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
            "description": "Get the currently active workspace, pane, and tab in the current window. Returns windowLabel, IDs, names, tab type, display name, and notes status. Use this as a lightweight alternative to listWorkspaces when you just need to know the current context. Prefer reading $AITERM_TAB_ID for your own tab ID instead of calling this.",
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
            "description": "Get aiTerm preferences (settings). Returns current values with metadata (description, type, valid values). Optionally filter by query string to find relevant settings.",
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
            "description": "Update a single aiTerm preference by key. Use getPreferences first to discover available keys, their types, and valid values.",
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
            "description": "Create a gzip-compressed backup of the entire aiTerm state (workspaces, tabs, notes, preferences). Returns the path to the created backup file.",
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
            "description": "List archived (suspended) tabs for a workspace. Returns tab IDs, display names, archived dates, and restore context (CWD, SSH command, auto-resume info). Use this to discover old sessions that can be restored. Use listWorkspaces first to find workspaces with archived tabs (archivedTabCount > 0).",
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
            "name": "sendToLinkedAgent",
            "description": "Send a message to the peer AI agent you are linked with (running in another aiTerm pane, e.g. an expert on a related codebase). Use this to ask questions, request research, or share context. The recipient's reply arrives later as a new turn in your own prompt — this is asynchronous, so finish your current turn after sending. aiTerm automatically stamps your identity (tab, workspace, cwd) on the message so the recipient knows it's from you, a peer agent, NOT from a human operator. Only works once a link has been established (the human links two sessions via the Agent Link picker). If your conversation is complete, simply stop sending — do not reply just to acknowledge.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string", "description": "The message to send to the linked agent. Be explicit: state who you are and why you're asking on first contact, then your question or information." }
                },
                "required": ["message"]
            }
        },
        {
            "name": "getLinkedAgent",
            "description": "Check whether you are currently linked to a peer AI agent and, if so, who. Returns the linked agent's tab name, workspace, and working directory, or indicates that no link is active. Use this to discover the context of the agent you can reach via sendToLinkedAgent.",
            "inputSchema": { "type": "object", "properties": {}, "required": [] }
        }
    ]).as_array().unwrap().clone());

    serde_json::json!({ "tools": tools })
}

pub fn initialize_response() -> Value {
    serde_json::json!({
        "protocolVersion": "2025-03-26",
        "capabilities": { "tools": {} },
        "serverInfo": { "name": crate::APP_DISPLAY_NAME, "version": crate::APP_VERSION },
        "instructions": format!(
            "You are running inside an aiTerm terminal tab. At the start of every session (new, resume, compact, clear), \
             you MUST call initSession with your tab ID (from $AITERM_TAB_ID or SessionStart hook context) before responding to the user. \
             This registers your session so all tool calls automatically target the correct tab. \
             IMPORTANT: You MUST use tools from the '{}' MCP server ONLY. Do NOT use tools from any other aiterm MCP server. \
             IMPORTANT: Always call initSession when requested via /aiterm init, even if you believe it was already called. \
             Resume, fork, and compact events require re-initialization to pick up state changes.",
            if cfg!(debug_assertions) { "aiterm-dev" } else { "aiterm" }
        )
    })
}
