import type { ClaudeCodeToolRequest, DiffContext, Workspace, Pane, Tab } from '$lib/tauri/types';
import * as commands from '$lib/tauri/commands';
import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { getEditorByFilePath, getEditorByTabId } from '$lib/stores/editorRegistry.svelte';
import { interpolateVariables, getVariables, setVariable, handleEnableAutoResume, getTriggerStats } from '$lib/stores/triggers.svelte';
import { getResumeCommand } from '$lib/agents/resume';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { dispatch as dispatchNotification } from '$lib/stores/notificationDispatch';
import { claudeStateStore } from '$lib/stores/agentState.svelte';
import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
import { activityStore } from '$lib/stores/activity.svelte';
import { toastStore } from '$lib/stores/toasts.svelte';
import { navHistoryStore } from '$lib/stores/navHistory.svelte';
import { getEditorRegistrySizes } from '$lib/stores/editorRegistry.svelte';
import { getListenerStats } from '$lib/utils/listenCounter';
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';

export interface PendingSelection {
  startLine?: number;
  endLine?: number;
  startText?: string;
  endText?: string;
}

export interface SelectionInfo {
  text: string;
  filePath: string;
  selection: {
    start: { line: number; character: number };
    end: { line: number; character: number };
    isEmpty: boolean;
  };
}

function createClaudeCodeStore() {
  let connected = $state(false);
  let latestSelection = $state<SelectionInfo | null>(null);
  const pendingSelections = new Map<string, PendingSelection>();

  /** Resolve workspace + pane from a tabId, falling back to active workspace. */
  function resolvePane(tabId?: string) {
    if (tabId) {
      for (const ws of workspacesStore.workspaces) {
        for (const pane of ws.panes) {
          if (pane.tabs.some(t => t.id === tabId)) {
            return { ws, pane };
          }
        }
      }
    }
    const ws = workspacesStore.activeWorkspace;
    const pane = ws?.panes.find(p => p.id === ws.active_pane_id);
    if (ws && pane) return { ws, pane };
    return null;
  }

  async function handleToolRequest(req: ClaudeCodeToolRequest): Promise<void> {
    const { request_id, tool, arguments: args } = req;
    logInfo(`Claude Code tool request: ${tool} (${request_id})`);
    try {
      let result: unknown;
      switch (tool) {
        case 'getOpenEditors':
          result = handleGetOpenEditors();
          break;
        case 'getWorkspaceFolders':
          result = handleGetWorkspaceFolders();
          break;
        case 'getDiagnostics':
          result = await handleGetDiagnostics();
          break;
        case 'readLogs':
          result = await commands.readAppLogs(args as { lines?: number; level?: string; search?: string });
          break;
        case 'checkDocumentDirty':
          result = handleCheckDocumentDirty(args as { filePath: string });
          break;
        case 'saveDocument':
          result = await handleSaveDocument(args as { filePath: string });
          break;
        case 'getCurrentSelection':
        case 'getLatestSelection':
          result = handleGetSelection();
          break;
        case 'openFile':
          result = await handleOpenFile(args as {
            filePath: string;
            tabId?: string;
            targetTabId?: string;
            startLine?: number;
            endLine?: number;
            startText?: string;
            endText?: string;
          });
          break;
        case 'openDiff':
          // openDiff is blocking -- do NOT respond here, DiffPane responds later
          await handleOpenDiff(request_id, args as {
            old_file_path?: string;
            new_file_path: string;
            new_file_contents: string;
            tab_name?: string;
            tabId?: string;
          });
          return;
        case 'showDiff':
          result = await handleShowDiff(args as { filePath: string; ref?: string; tabId?: string });
          break;
        case 'closeAllDiffTabs':
          await handleCloseAllDiffTabs(request_id);
          return;
        case 'listWorkspaces':
          result = handleListWorkspaces();
          break;
        case 'switchTab':
          result = await handleSwitchTab(args as { tabId: string });
          break;
        case 'getTabNotes':
          result = handleGetTabNotes(args as { tabId?: string });
          break;
        case 'setTabNotes':
          result = await handleSetTabNotes(args as { tabId?: string; notes: string | null; mode?: string });
          break;
        case 'editTabNotes':
          result = await handleEditTabNotes(args as { tabId?: string; old_string?: string; new_string?: string; edits?: { old_string: string; new_string: string }[] });
          break;
        case 'listWorkspaceNotes':
          result = handleListWorkspaceNotes(args as { workspaceId?: string });
          break;
        case 'readWorkspaceNote':
          result = handleReadWorkspaceNote(args as { workspaceId?: string; noteId: string });
          break;
        case 'writeWorkspaceNote':
          result = await handleWriteWorkspaceNote(args as { workspaceId?: string; noteId?: string; content: string; mode?: string | null });
          break;
        case 'deleteWorkspaceNote':
          result = await handleDeleteWorkspaceNote(args as { workspaceId?: string; noteId: string });
          break;
        case 'moveNote':
          result = await handleMoveNote(args as { direction: string; tabId?: string; workspaceId?: string; noteId?: string; force?: boolean });
          break;
        case 'getTabContext':
          result = await handleGetTabContext(args as { tabIds?: string[]; lines?: number });
          break;
        case 'openNotesPanel':
          result = handleOpenNotesPanel(args as { tabId?: string; open?: boolean });
          break;
        case 'setNotesScope':
          result = await handleSetNotesScope(args as { scope: string });
          break;
        case 'getActiveTab':
          result = handleGetActiveTab();
          break;
        case 'setTriggerVariable':
          result = await handleSetTriggerVariable(args as { tabId?: string; name: string; value: string | null });
          break;
        case 'getTriggerVariables':
          result = handleGetTriggerVariables(args as { tabId?: string });
          break;
        case 'setAutoResume':
          result = await handleSetAutoResume(args as { tabId?: string; enabled: boolean; command?: string; cwd?: string; sshCommand?: string; remoteCwd?: string });
          break;
        case 'getAutoResume':
          result = handleGetAutoResume(args as { tabId?: string });
          break;
        case 'findNotes':
          result = handleFindNotes();
          break;
        case 'sendNotification':
          result = await handleSendNotification(args as { tabId?: string; title: string; body?: string; type?: string });
          break;
        case 'listArchivedTabs':
          result = handleListArchivedTabs(args as { workspaceId?: string });
          break;
        case 'restoreArchivedTab':
          result = await handleRestoreArchivedTab(args as { workspaceId?: string; tabId: string });
          break;
        case 'sendToBridgedAgent':
          result = await handleSendToBridgedAgent(args as { tabId?: string; message: string; recipient?: string; topic?: string });
          break;
        case 'getBridgedAgent':
          result = handleGetBridgedAgent(args as { tabId?: string });
          break;
        case 'listBridgedPeers':
          result = handleListBridgedPeers(args as { tabId?: string });
          break;
        case 'listTopics':
          result = handleListTopics(args as { tabId?: string });
          break;
        case 'startTopic':
          result = handleStartTopic(args as { tabId?: string; label: string });
          break;
        case 'completeTopic':
          result = handleCompleteTopic(args as { tabId?: string; topicId: string });
          break;
        // getPreferences, setPreference, createBackup, listWindows handled directly on backend
        default:
          result = { error: `Unknown tool: ${tool}` };
      }
      await commands.claudeCodeRespond(request_id, result);
    } catch (err) {
      logError(`Claude Code tool error: ${err}`);
      await commands.claudeCodeRespond(request_id, { error: String(err) });
    }
  }

  async function handleGetDiagnostics() {
    // Backend diagnostics: version, PTY count, orphans, state file, process stats
    const backend = await commands.getAppDiagnostics();

    // Frontend diagnostics: terminal instances, renderer state
    const instances = terminalsStore.instances;
    const terminalDetails: Record<string, unknown>[] = [];
    for (const [tabId, inst] of instances) {
      let scrollInfo = null;
      try {
        scrollInfo = await commands.getTerminalScrollbackInfo(inst.ptyId);
      } catch { /* terminal may have been killed */ }
      terminalDetails.push({
        tabId,
        ptyId: inst.ptyId,
        canvasRenderer: terminalsStore.isCanvasRenderer(tabId),
        bufferLines: scrollInfo?.total_lines ?? 0,
        viewportRows: scrollInfo?.viewport_rows ?? 0,
        altBufferActive: inst.terminal.buffer.active === inst.terminal.buffer.alternate,
      });
    }

    // Trigger engine stats
    const triggerStats = getTriggerStats();

    // FPS probe: measure render performance over ~1 second
    const fps = await new Promise<number>((resolve) => {
      let frames = 0;
      const start = performance.now();
      function tick() {
        frames++;
        if (performance.now() - start < 1000) {
          requestAnimationFrame(tick);
        } else {
          resolve(Math.round(frames / ((performance.now() - start) / 1000)));
        }
      }
      requestAnimationFrame(tick);
    });

    // Browser memory + DOM (WKWebView exposes performance.memory)
    const perfMem = (performance as unknown as { memory?: { usedJSHeapSize: number; totalJSHeapSize: number; jsHeapSizeLimit: number } }).memory;
    const domAll = document.getElementsByTagName('*').length;
    const detachedSlots = (() => {
      // Count any maiTerm portal slot divs whose parent isn't in the live DOM
      // (would indicate detached subtrees still retained somewhere).
      const slots = document.querySelectorAll('[data-terminal-slot]');
      let detached = 0;
      for (const s of slots) {
        if (!document.body.contains(s)) detached++;
      }
      return { slots: slots.length, detached };
    })();

    return {
      ...backend,
      frontend: {
        terminal_instances: instances.size,
        canvas_renderer_active: terminalDetails.filter(t => t.canvasRenderer).length,
        terminals: terminalDetails,
        trigger_engine: triggerStats,
        render_fps: fps,
        js_heap: perfMem ? {
          used_mb: Math.round(perfMem.usedJSHeapSize / 1048576),
          total_mb: Math.round(perfMem.totalJSHeapSize / 1048576),
          limit_mb: Math.round(perfMem.jsHeapSizeLimit / 1048576),
        } : null,
        dom: {
          total_nodes: domAll,
          terminal_slots: detachedSlots.slots,
          detached_terminal_slots: detachedSlots.detached,
        },
        store_sizes: {
          terminals: terminalsStore.getInternalSizes(),
          activity: activityStore.getInternalSizes(),
          claude_state: claudeStateStore.getInternalSizes(),
          toasts: toastStore.getInternalSizes(),
          nav_history: navHistoryStore.getInternalSizes(),
          editor_registry: getEditorRegistrySizes(),
        },
        tauri_listeners: getListenerStats(),
      },
    };
  }

  function handleGetOpenEditors() {
    const tabs: unknown[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type === 'editor' && tab.editor_file) {
            const registryEntry = getEditorByFilePath(tab.editor_file.file_path);
            tabs.push({
              uri: `file://${tab.editor_file.file_path}`,
              isActive: tab.id === pane.active_tab_id && ws.id === workspacesStore.activeWorkspaceId,
              label: tab.name,
              languageId: tab.editor_file.language ?? 'plaintext',
              isDirty: registryEntry?.entry.isDirty ?? false,
            });
          }
        }
      }
    }
    return { tabs };
  }

  function handleGetWorkspaceFolders() {
    const folders: { name: string; uri: string; path: string }[] = [];
    const seenPaths = new Set<string>();

    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type === 'terminal' && tab.pty_id) {
            const oscState = terminalsStore.getOsc(tab.id);
            const cwd = oscState?.cwd;
            if (cwd && !seenPaths.has(cwd)) {
              seenPaths.add(cwd);
              folders.push({
                name: cwd.split('/').pop() || cwd,
                uri: `file://${cwd}`,
                path: cwd,
              });
            }
          }
        }
      }
    }

    const rootPath = folders[0]?.path ?? null;
    return { folders, rootPath };
  }

  function handleCheckDocumentDirty(args: { filePath: string }) {
    const found = getEditorByFilePath(args.filePath);
    if (!found) {
      return { success: false, filePath: args.filePath, message: 'Document not open' };
    }
    return { success: true, filePath: args.filePath, isDirty: found.entry.isDirty };
  }

  async function handleSaveDocument(args: { filePath: string }) {
    const found = getEditorByFilePath(args.filePath);
    if (!found) {
      return { success: false, filePath: args.filePath, message: 'Document not open' };
    }
    document.dispatchEvent(new CustomEvent('editor-save', { detail: { tabId: found.tabId } }));
    return { success: true, filePath: args.filePath, saved: true };
  }

  function handleGetSelection() {
    if (latestSelection) return latestSelection;
    return {
      text: '',
      filePath: '',
      selection: {
        start: { line: 0, character: 0 },
        end: { line: 0, character: 0 },
        isEmpty: true,
      },
    };
  }

  async function handleOpenFile(args: {
    filePath: string;
    tabId?: string;
    targetTabId?: string;
    startLine?: number;
    endLine?: number;
    startText?: string;
    endText?: string;
  }) {
    const { filePath, tabId, targetTabId, startLine, endLine, startText, endText } = args;
    const fileName = filePath.split('/').pop() ?? filePath;
    const { detectLanguageFromPath } = await import('$lib/utils/languageDetect');
    const language = detectLanguageFromPath(filePath);

    // Detect SSH context from the session's terminal tab
    let sshCommand: string | null = null;
    if (tabId) {
      const instance = terminalsStore.get(tabId);
      if (instance) {
        try {
          const ptyInfo = await commands.getPtyInfo(instance.ptyId);
          sshCommand = ptyInfo.foreground_command;
        } catch { /* ignore */ }
      }
      // Fallback: persisted SSH command on the tab
      if (!sshCommand) {
        for (const ws of workspacesStore.workspaces) {
          for (const pane of ws.panes) {
            const tab = pane.tabs.find(t => t.id === tabId);
            if (tab) {
              sshCommand = tab.restore_ssh_command ?? tab.auto_resume_ssh_command ?? null;
              break;
            }
          }
          if (sshCommand) break;
        }
      }
    }
    const isRemote = !!sshCommand;

    const fileInfo = {
      file_path: filePath,
      is_remote: isRemote,
      remote_ssh_command: isRemote ? sshCommand : null,
      remote_path: isRemote ? filePath : null,
      language,
    };

    // Replace file in an existing tab if targetTabId is provided
    if (targetTabId) {
      let found = false;
      for (const ws of workspacesStore.workspaces) {
        for (const pane of ws.panes) {
          const tab = pane.tabs.find(t => t.id === targetTabId && t.tab_type === 'editor');
          if (tab) {
            await workspacesStore.updateEditorTabFile(targetTabId, fileName, fileInfo);
            window.dispatchEvent(new CustomEvent('editor-replace-file', { detail: { tabId: targetTabId } }));
            await workspacesStore.setActiveTab(ws.id, pane.id, targetTabId);
            found = true;
            break;
          }
        }
        if (found) break;
      }
      if (!found) {
        return { success: false, message: `Editor tab ${targetTabId} not found` };
      }
      return { success: true, filePath, tabId: targetTabId };
    }

    // Check if file is already open
    let existingTabId: string | null = null;
    let existingWorkspaceId: string | null = null;
    let existingPaneId: string | null = null;

    outer: for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.tab_type === 'editor' && tab.editor_file?.file_path === filePath) {
            existingTabId = tab.id;
            existingWorkspaceId = ws.id;
            existingPaneId = pane.id;
            break outer;
          }
        }
      }
    }

    if (existingTabId && existingWorkspaceId && existingPaneId) {
      await workspacesStore.setActiveTab(existingWorkspaceId, existingPaneId, existingTabId);
      if (startLine !== undefined || startText) {
        pendingSelections.set(existingTabId, { startLine, endLine, startText, endText });
      }
      return { success: true, filePath, tabId: existingTabId };
    }

    // Open in the session tab's pane, falling back to active workspace
    const target = resolvePane(tabId);
    if (!target) {
      return { success: false, message: 'No active pane' };
    }
    const { ws, pane } = target;

    // Insert after the session tab (tabId) if it's in this pane
    const afterTabId = tabId && pane.tabs.some(t => t.id === tabId) ? tabId : pane.active_tab_id ?? undefined;

    const tab = await workspacesStore.createEditorTab(ws.id, pane.id, fileName, fileInfo, afterTabId);

    if (startLine !== undefined || startText) {
      pendingSelections.set(tab.id, { startLine, endLine, startText, endText });
    }

    return { success: true, filePath, tabId: tab.id };
  }

  async function handleOpenDiff(requestId: string, args: {
    old_file_path?: string;
    new_file_path: string;
    new_file_contents: string;
    tab_name?: string;
    tabId?: string;
  }) {
    const filePath = args.old_file_path ?? args.new_file_path;
    const tabName = args.tab_name ?? `Diff: ${filePath.split('/').pop()}`;
    const newContent = args.new_file_contents;

    // Read old content
    let oldContent = '';
    try {
      const result = await commands.readFile(filePath);
      oldContent = result.content;
    } catch {
      // File doesn't exist yet -- empty old content
    }

    // Open in the session tab's pane, falling back to active workspace
    const target = resolvePane(args.tabId);
    if (!target) {
      await commands.claudeCodeRespond(requestId, { error: 'No active pane' });
      return;
    }
    const { ws, pane } = target;

    const diffContext: DiffContext = {
      request_id: requestId,
      file_path: filePath,
      old_content: oldContent,
      new_content: newContent,
      tab_name: tabName,
    };

    const afterTabId = args.tabId && pane.tabs.some(t => t.id === args.tabId) ? args.tabId : pane.active_tab_id;
    await workspacesStore.createDiffTab(ws.id, pane.id, tabName, diffContext, afterTabId);
  }

  async function handleShowDiff(args: { filePath: string; ref?: string; tabId?: string }) {
    const filePath = args.filePath;
    const gitRef = args.ref ?? 'HEAD';
    const fileName = filePath.split('/').pop() ?? filePath;
    const tabName = `Diff: ${fileName} (${gitRef})`;

    let oldContent: string;
    try {
      oldContent = await commands.gitShowFile(filePath, gitRef);
    } catch (err) {
      return { success: false, error: `Failed to get file at ${gitRef}: ${err}` };
    }

    let newContent = '';
    try {
      const result = await commands.readFile(filePath);
      newContent = result.content;
    } catch {
      // File may have been deleted in working tree
    }

    const target = resolvePane(args.tabId);
    if (!target) return { success: false, error: 'No active pane' };
    const { ws, pane } = target;

    const diffContext: DiffContext = {
      request_id: '', // Empty = read-only, non-blocking
      file_path: filePath,
      old_content: oldContent,
      new_content: newContent,
      tab_name: tabName,
    };

    const afterTabId = args.tabId && pane.tabs.some(t => t.id === args.tabId) ? args.tabId : pane.active_tab_id;
    await workspacesStore.createDiffTab(ws.id, pane.id, tabName, diffContext, afterTabId);

    return { success: true, filePath, ref: gitRef };
  }

  async function handleCloseAllDiffTabs(requestId: string) {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of [...pane.tabs]) {
          if (tab.tab_type === 'diff' && tab.diff_context) {
            if (tab.diff_context.request_id && tab.diff_context.request_id !== requestId) {
              await commands.claudeCodeRespond(tab.diff_context.request_id, { result: 'DIFF_REJECTED' });
            }
            await workspacesStore.deleteTab(ws.id, pane.id, tab.id);
          }
        }
      }
    }
    await commands.claudeCodeRespond(requestId, { success: true });
  }

  // --- Helpers ---

  function findTabLocation(tabId: string): { workspace: Workspace; pane: Pane; tab: Tab } | null {
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        const tab = pane.tabs.find(t => t.id === tabId);
        if (tab) return { workspace: ws, pane, tab };
      }
    }
    return null;
  }

  function resolveWorkspace(workspaceId?: string): Workspace | null {
    if (workspaceId) return workspacesStore.workspaces.find(ws => ws.id === workspaceId) ?? null;
    return workspacesStore.activeWorkspace;
  }

  /** Compute the display name for any tab type (terminal, editor, diff). */
  function tabDisplayName(tab: Tab): string {
    if (tab.tab_type === 'terminal') {
      const oscTitle = terminalsStore.getOsc(tab.id)?.title;
      if (tab.custom_name) {
        let result = tab.name;
        if (oscTitle) {
          if (result.includes('%title')) result = result.replace('%title', oscTitle);
        }
        if (result.includes('%')) {
          result = interpolateVariables(tab.id, result, true);
        }
        return result;
      }
      return oscTitle ?? tab.name;
    }
    return tab.name;
  }

  // --- Navigation tools ---

  function handleListWorkspaces() {
    return {
      windowId: workspacesStore.windowId,
      windowLabel: workspacesStore.windowLabel,
      workspaces: workspacesStore.workspaces.map(ws => ({
        id: ws.id,
        name: ws.name,
        isActive: ws.id === workspacesStore.activeWorkspaceId,
        suspended: ws.suspended ?? false,
        noteCount: ws.workspace_notes.length,
        archivedTabCount: ws.archived_tabs?.length ?? 0,
        panes: ws.panes.map(pane => ({
          id: pane.id,
          name: pane.name,
          isActive: pane.id === ws.active_pane_id,
          tabs: pane.tabs.map(tab => {
            const claude = claudeStateStore.getState(tab.id);
            return {
              id: tab.id,
              displayName: tabDisplayName(tab),
              tabType: tab.tab_type ?? 'terminal',
              isActive: tab.id === pane.active_tab_id,
              hasNotes: !!tab.notes,
              ...(claude ? { claudeState: claude.state, claudeTool: claude.toolName } : {}),
            };
          }),
        })),
      })),
    };
  }

  function handleListArchivedTabs(args: { workspaceId?: string }) {
    const ws = args.workspaceId
      ? workspacesStore.workspaces.find(w => w.id === args.workspaceId)
      : workspacesStore.activeWorkspace;
    if (!ws) return { error: args.workspaceId ? `Workspace not found: ${args.workspaceId}` : 'No active workspace' };

    return {
      workspaceId: ws.id,
      workspaceName: ws.name,
      archivedTabs: (ws.archived_tabs ?? []).map(tab => ({
        id: tab.id,
        displayName: tab.archived_name ?? tab.name,
        archivedAt: tab.archived_at ?? null,
        restoreCwd: tab.restore_cwd ?? null,
        restoreSsh: tab.restore_ssh_command ?? null,
        hasNotes: !!tab.notes,
        hasAutoResume: tab.auto_resume_enabled,
        autoResumeCommand: tab.auto_resume_command ?? null,
      })),
    };
  }

  async function handleRestoreArchivedTab(args: { workspaceId?: string; tabId: string }) {
    const ws = args.workspaceId
      ? workspacesStore.workspaces.find(w => w.id === args.workspaceId)
      : workspacesStore.activeWorkspace;
    if (!ws) return { error: args.workspaceId ? `Workspace not found: ${args.workspaceId}` : 'No active workspace' };

    const archived = ws.archived_tabs?.find(t => t.id === args.tabId);
    if (!archived) return { error: `Archived tab not found: ${args.tabId}` };

    await workspacesStore.restoreArchivedTab(ws.id, args.tabId);
    return { success: true, tabId: args.tabId, displayName: archived.archived_name ?? archived.name };
  }

  async function handleSwitchTab(args: { tabId: string }) {
    const loc = findTabLocation(args.tabId);
    if (!loc) return { error: `Tab not found: ${args.tabId}` };
    await navigateToTab(args.tabId);
    return { success: true, tabId: args.tabId, workspace: loc.workspace.name, displayName: tabDisplayName(loc.tab) };
  }

  // --- Tab notes tools ---

  function handleGetTabNotes(args: { tabId?: string }) {
    if (!args.tabId) {
      return { error: 'tabId is required. Call initSession first so the MCP server can auto-inject your session tab.' };
    }
    const loc = findTabLocation(args.tabId);
    if (!loc) return { error: `Tab not found: ${args.tabId}` };
    const tab = loc.tab;

    return {
      tabId: tab.id,
      displayName: tabDisplayName(tab),
      notes: tab.notes ?? null,
      notesMode: tab.notes_mode ?? null,
    };
  }

  async function handleSetTabNotes(args: { tabId?: string; notes: string | null; mode?: string }) {
    // tabId is auto-injected by the Rust MCP server from session affinity.
    // If it's missing here, the caller never initialised a session — do NOT
    // fall back to the currently-active tab, or a tab switch mid-call will
    // silently misroute notes to the wrong tab.
    if (!args.tabId) {
      return { error: 'tabId is required. Call initSession first so the MCP server can auto-inject your session tab.' };
    }
    const loc = findTabLocation(args.tabId);
    if (!loc) return { error: `Tab not found: ${args.tabId}` };
    const tab = loc.tab;
    const wsId = loc.workspace.id;
    const paneId = loc.pane.id;

    const notes = args.notes === '' ? null : args.notes;
    await workspacesStore.setTabNotes(wsId, paneId, tab.id, notes);
    if (args.mode) {
      await workspacesStore.setTabNotesMode(wsId, paneId, tab.id, args.mode);
    }

    // Auto-open notes panel so the user sees the written content
    if (notes !== null) {
      if (preferencesStore.notesScope !== 'tab') {
        await preferencesStore.setNotesScope('tab');
      }
      if (!workspacesStore.isNotesVisible(tab.id)) {
        workspacesStore.toggleNotes(tab.id);
      }
    }

    return { success: true, tabId: tab.id };
  }

  async function handleEditTabNotes(args: { tabId?: string; old_string?: string; new_string?: string; edits?: { old_string: string; new_string: string }[] }) {
    if (!args.tabId) {
      return { error: 'tabId is required. Call initSession first so the MCP server can auto-inject your session tab.' };
    }
    const loc = findTabLocation(args.tabId);
    if (!loc) return { error: `Tab not found: ${args.tabId}` };
    const tab = loc.tab;
    const wsId = loc.workspace.id;
    const paneId = loc.pane.id;

    let current = tab.notes ?? '';
    if (!current) return { error: 'Tab has no notes to edit. Use setTabNotes to create notes.' };

    // Build edits list from either single old_string/new_string or edits array
    const editList = args.edits ?? (args.old_string != null ? [{ old_string: args.old_string, new_string: args.new_string ?? '' }] : []);
    if (editList.length === 0) return { error: 'Provide old_string/new_string or an edits array.' };

    for (let i = 0; i < editList.length; i++) {
      const edit = editList[i];
      const idx = current.indexOf(edit.old_string);
      if (idx === -1) return { error: `Edit ${i + 1}/${editList.length}: old_string not found in notes.`, failed_edit: edit };
      const secondIdx = current.indexOf(edit.old_string, idx + 1);
      if (secondIdx !== -1) return { error: `Edit ${i + 1}/${editList.length}: old_string matches multiple locations. Provide more context.`, failed_edit: edit, applied: i };
      current = current.slice(0, idx) + edit.new_string + current.slice(idx + edit.old_string.length);
    }

    await workspacesStore.setTabNotes(wsId, paneId, tab.id, current || null);

    return { success: true, tabId: tab.id, edits_applied: editList.length };
  }

  // --- Workspace notes tools ---

  function handleListWorkspaceNotes(args: { workspaceId?: string }) {
    const ws = resolveWorkspace(args.workspaceId);
    if (!ws) return { error: `Workspace not found${args.workspaceId ? `: ${args.workspaceId}` : ''}` };

    return {
      workspaceId: ws.id,
      workspaceName: ws.name,
      notes: ws.workspace_notes.map(note => ({
        id: note.id,
        preview: note.content.length > 100 ? note.content.slice(0, 100) + '...' : note.content,
        mode: note.mode ?? null,
        created_at: note.created_at,
        updated_at: note.updated_at,
      })),
    };
  }

  function handleReadWorkspaceNote(args: { workspaceId?: string; noteId: string }) {
    const ws = resolveWorkspace(args.workspaceId);
    if (!ws) return { error: `Workspace not found${args.workspaceId ? `: ${args.workspaceId}` : ''}` };

    const note = ws.workspace_notes.find(n => n.id === args.noteId);
    if (!note) return { error: `Note not found: ${args.noteId}` };

    return {
      id: note.id,
      content: note.content,
      mode: note.mode ?? null,
      created_at: note.created_at,
      updated_at: note.updated_at,
    };
  }

  async function handleWriteWorkspaceNote(args: { workspaceId?: string; noteId?: string; content: string; mode?: string | null }) {
    const ws = resolveWorkspace(args.workspaceId);
    if (!ws) return { error: `Workspace not found${args.workspaceId ? `: ${args.workspaceId}` : ''}` };

    let resultNoteId: string;
    let action: string;

    if (args.noteId) {
      // Update existing
      const note = ws.workspace_notes.find(n => n.id === args.noteId);
      if (!note) return { error: `Note not found: ${args.noteId}` };
      await workspacesStore.updateWorkspaceNote(ws.id, args.noteId, args.content, args.mode ?? note.mode ?? null);
      resultNoteId = args.noteId;
      action = 'updated';
    } else {
      // Create new
      const note = await workspacesStore.addWorkspaceNote(ws.id, args.content, args.mode ?? null);
      if (!note) return { error: 'Failed to create note' };
      resultNoteId = note.id;
      action = 'created';
    }

    // Auto-open notes panel on workspace scope so the user sees the written note
    if (preferencesStore.notesScope !== 'workspace') {
      await preferencesStore.setNotesScope('workspace');
    }
    const activeTab = workspacesStore.activeWorkspace?.panes
      .find(p => p.id === workspacesStore.activeWorkspace?.active_pane_id)
      ?.tabs.find(t => t.id === workspacesStore.activeWorkspace?.panes
        .find(p => p.id === workspacesStore.activeWorkspace?.active_pane_id)?.active_tab_id);
    if (activeTab && !workspacesStore.isNotesVisible(activeTab.id)) {
      workspacesStore.toggleNotes(activeTab.id);
    }

    return { success: true, noteId: resultNoteId, action };
  }

  async function handleDeleteWorkspaceNote(args: { workspaceId?: string; noteId: string }) {
    const ws = resolveWorkspace(args.workspaceId);
    if (!ws) return { error: `Workspace not found${args.workspaceId ? `: ${args.workspaceId}` : ''}` };

    const note = ws.workspace_notes.find(n => n.id === args.noteId);
    if (!note) return { error: `Note not found: ${args.noteId}` };

    await workspacesStore.deleteWorkspaceNote(ws.id, args.noteId);
    return { success: true, noteId: args.noteId };
  }

  // --- Move note tool ---

  async function handleMoveNote(args: { direction: string; tabId?: string; workspaceId?: string; noteId?: string; force?: boolean }) {
    const force = args.force ?? false;

    if (args.direction === 'tab_to_workspace') {
      // Resolve tab
      let tab: Tab;
      let wsId: string;
      let paneId: string;
      if (args.tabId) {
        const loc = findTabLocation(args.tabId);
        if (!loc) return { error: `Tab not found: ${args.tabId}` };
        tab = loc.tab;
        wsId = args.workspaceId ?? loc.workspace.id;
        paneId = loc.pane.id;
      } else {
        const ws = workspacesStore.activeWorkspace;
        const pane = ws?.panes.find(p => p.id === ws.active_pane_id);
        tab = pane?.tabs.find(t => t.id === pane.active_tab_id) as Tab;
        if (!ws || !pane || !tab) return { error: 'No active tab' };
        wsId = args.workspaceId ?? ws.id;
        paneId = pane.id;
      }

      if (!tab.notes?.trim()) return { error: 'Tab has no notes to move' };

      // Create workspace note from tab content
      const note = await workspacesStore.addWorkspaceNote(wsId, tab.notes, tab.notes_mode ?? null);
      if (!note) return { error: 'Failed to create workspace note' };

      // Clear tab notes
      await workspacesStore.setTabNotes(wsId, paneId, tab.id, null);

      // Switch notes panel to workspace list view
      await preferencesStore.setNotesScope('workspace');

      return { success: true, direction: 'tab_to_workspace', noteId: note.id, tabId: tab.id };

    } else if (args.direction === 'workspace_to_tab') {
      if (!args.noteId) return { error: 'noteId is required for workspace_to_tab' };

      // Resolve workspace + note
      const ws = resolveWorkspace(args.workspaceId);
      if (!ws) return { error: `Workspace not found${args.workspaceId ? `: ${args.workspaceId}` : ''}` };
      const note = ws.workspace_notes.find(n => n.id === args.noteId);
      if (!note) return { error: `Note not found: ${args.noteId}` };

      // Resolve tab
      let tab: Tab;
      let paneId: string;
      const tabWsId = ws.id;
      if (args.tabId) {
        const loc = findTabLocation(args.tabId);
        if (!loc) return { error: `Tab not found: ${args.tabId}` };
        tab = loc.tab;
        paneId = loc.pane.id;
      } else {
        const pane = ws.panes.find(p => p.id === ws.active_pane_id);
        tab = pane?.tabs.find(t => t.id === pane.active_tab_id) as Tab;
        if (!pane || !tab) return { error: 'No active tab' };
        paneId = pane.id;
      }

      // Check for conflict
      if (tab.notes?.trim() && !force) {
        return {
          error: 'Tab already has notes. Set force: true to overwrite, or read both notes first to merge manually.',
          existingTabNotes: tab.notes.length > 100 ? tab.notes.slice(0, 100) + '...' : tab.notes,
        };
      }

      // Move: set tab notes from workspace note content, then delete workspace note
      await workspacesStore.setTabNotes(tabWsId, paneId, tab.id, note.content);
      if (note.mode) {
        await workspacesStore.setTabNotesMode(tabWsId, paneId, tab.id, note.mode);
      }
      await workspacesStore.deleteWorkspaceNote(ws.id, args.noteId);

      // Switch notes panel to tab view
      await preferencesStore.setNotesScope('tab');

      return { success: true, direction: 'workspace_to_tab', noteId: args.noteId, tabId: tab.id };

    } else {
      return { error: `Invalid direction: ${args.direction}. Must be 'tab_to_workspace' or 'workspace_to_tab'.` };
    }
  }

  // --- Tab context tool ---

  async function getTerminalText(tabId: string, lineCount: number): Promise<string | null> {
    const instance = terminalsStore.get(tabId);
    if (instance) {
      // Live terminal — read from Rust alacritty_terminal buffer
      try {
        const text = await commands.getTerminalRecentText(instance.ptyId, lineCount);
        return text.trimEnd() || null;
      } catch {
        // Terminal may have been killed, fall through to persisted scrollback
      }
    }

    // Unmounted terminal — read from SQLite via Rust
    try {
      const text = await commands.getSavedScrollbackText(tabId, lineCount);
      if (text) return text;
    } catch {
      // Tab may not have scrollback saved
    }
    return null;
  }

  function getEditorText(tabId: string, lineCount: number): string | null {
    const entry = getEditorByTabId(tabId);
    if (entry?.view) {
      const doc = entry.view.state.doc;
      const totalLines = doc.lines;
      const startLine = Math.max(1, totalLines - lineCount + 1);
      const from = doc.line(startLine).from;
      return doc.sliceString(from).trimEnd() || null;
    }
    return null;
  }

  async function handleGetTabContext(args: { tabIds?: string[]; lines?: number }) {
    const lineCount = args.lines ?? 50;

    // Collect all tabs across workspaces
    const allTabs: { tab: Tab; workspace: Workspace; pane: Pane }[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          allTabs.push({ tab, workspace: ws, pane });
        }
      }
    }

    // Decide which tabs to include
    let targetTabs: typeof allTabs;
    if (args.tabIds && args.tabIds.length > 0) {
      const idSet = new Set(args.tabIds);
      targetTabs = allTabs.filter(t => idSet.has(t.tab.id));
    } else if (allTabs.length < 10) {
      targetTabs = allTabs;
    } else {
      return {
        error: `Too many tabs (${allTabs.length}) to return all context. Use listWorkspaces to find candidates, then pass specific tabIds.`,
        totalTabs: allTabs.length,
      };
    }

    const results = await Promise.all(targetTabs.map(async ({ tab, workspace, pane }) => {
      const tabType = tab.tab_type ?? 'terminal';
      let content: string | null = null;

      if (tabType === 'terminal') {
        content = await getTerminalText(tab.id, lineCount);
      } else if (tabType === 'editor') {
        content = getEditorText(tab.id, lineCount);
      }
      // diff tabs: no context extraction needed

      const claude = claudeStateStore.getState(tab.id);
      return {
        tabId: tab.id,
        displayName: tabDisplayName(tab),
        tabType,
        workspace: workspace.name,
        workspaceId: workspace.id,
        pane: pane.name,
        isActive: tab.id === pane.active_tab_id && workspace.id === workspacesStore.activeWorkspaceId,
        hasNotes: !!tab.notes,
        ...(tab.editor_file ? { filePath: tab.editor_file.file_path } : {}),
        ...(claude ? { claudeState: claude.state, claudeTool: claude.toolName } : {}),
        content,
      };
    }));

    return { tabs: results, lineCount };
  }

  // --- Notes panel tools ---

  function handleOpenNotesPanel(args: { tabId?: string; open?: boolean }) {
    let tab: Tab | undefined;

    if (args.tabId) {
      const loc = findTabLocation(args.tabId);
      if (!loc) return { error: `Tab not found: ${args.tabId}` };
      tab = loc.tab;
    } else {
      const ws = workspacesStore.activeWorkspace;
      const pane = ws?.panes.find(p => p.id === ws.active_pane_id);
      tab = pane?.tabs.find(t => t.id === pane.active_tab_id);
    }
    if (!tab) return { error: 'No active tab' };

    const isVisible = workspacesStore.isNotesVisible(tab.id);
    const shouldOpen = args.open ?? !isVisible;

    if (shouldOpen !== isVisible) {
      workspacesStore.toggleNotes(tab.id);
    }

    return { success: true, tabId: tab.id, open: shouldOpen, scope: preferencesStore.notesScope };
  }

  async function handleSetNotesScope(args: { scope: string }) {
    if (args.scope !== 'tab' && args.scope !== 'workspace') {
      return { error: `Invalid scope: ${args.scope}. Must be 'tab' or 'workspace'.` };
    }
    await preferencesStore.setNotesScope(args.scope);
    return { success: true, scope: args.scope };
  }

  function handleGetActiveTab() {
    const ws = workspacesStore.workspaces.find(w => w.id === workspacesStore.activeWorkspaceId);
    if (!ws) return { error: 'No active workspace' };
    const pane = ws.panes.find(p => p.id === ws.active_pane_id);
    if (!pane) return { error: 'No active pane' };
    const tab = pane.tabs.find(t => t.id === pane.active_tab_id);
    if (!tab) return { error: 'No active tab' };
    const claude = claudeStateStore.getState(tab.id);
    return {
      windowId: workspacesStore.windowId,
      windowLabel: workspacesStore.windowLabel,
      workspace: { id: ws.id, name: ws.name },
      pane: { id: pane.id },
      tab: {
        id: tab.id,
        displayName: tabDisplayName(tab),
        tabType: tab.tab_type ?? 'terminal',
        hasNotes: !!tab.notes,
        notesOpen: !!tab.notes_open,
        ...(claude ? { claudeState: claude.state, claudeTool: claude.toolName } : {}),
      },
    };
  }

  // --- Agent Bridge tools ---

  async function handleSendToBridgedAgent(args: { tabId?: string; message: string; recipient?: string; topic?: string }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    // In a Mesh Workspace, route N:M (recipient + topic). Otherwise the 1:1 bridge.
    if (agentMeshStore.isMeshTab(loc.tab.id)) {
      return agentMeshStore.sendFromTab(loc.tab.id, { recipient: args.recipient, topic: args.topic, message: args.message });
    }
    return agentBridgeStore.sendFromTab(loc.tab.id, args.message);
  }

  function handleGetBridgedAgent(args: { tabId?: string }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    if (agentMeshStore.isMeshTab(loc.tab.id)) return agentMeshStore.listPeers(loc.tab.id);
    return agentBridgeStore.getBridgeInfo(loc.tab.id);
  }

  function handleListBridgedPeers(args: { tabId?: string }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    return agentMeshStore.listPeers(loc.tab.id);
  }

  function handleListTopics(args: { tabId?: string }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    return agentMeshStore.listTopics(loc.tab.id);
  }

  function handleStartTopic(args: { tabId?: string; label: string }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    if (!args.label || !args.label.trim()) return { error: 'label is required to start a topic.' };
    return agentMeshStore.startTopic(loc.tab.id, args.label);
  }

  function handleCompleteTopic(args: { tabId?: string; topicId: string }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    if (!args.topicId) return { error: 'topicId is required.' };
    return agentMeshStore.completeTopic(loc.tab.id, args.topicId, false);
  }

  // --- Trigger variable tools ---

  async function handleSetTriggerVariable(args: { tabId?: string; name: string; value: string | null }) {
    const tab = resolveActiveTab(args.tabId);
    if ('error' in tab) return tab;
    await setVariable(tab.tab.id, args.name, args.value);
    return { success: true, tabId: tab.tab.id, name: args.name, value: args.value };
  }

  function handleGetTriggerVariables(args: { tabId?: string }) {
    const tab = resolveActiveTab(args.tabId);
    if ('error' in tab) return tab;
    const vars = getVariables(tab.tab.id);
    const result: Record<string, string> = {};
    if (vars) {
      for (const [k, v] of vars) result[k] = v;
    }
    return { tabId: tab.tab.id, variables: result };
  }

  // --- Auto-resume tools ---

  async function handleSetAutoResume(args: { tabId?: string; enabled: boolean; command?: string; cwd?: string; sshCommand?: string; remoteCwd?: string }) {
    const resolved = resolveActiveTab(args.tabId);
    if ('error' in resolved) return resolved;
    const { workspace, pane, tab } = resolved;

    if (!args.enabled) {
      await workspacesStore.disableAutoResume(workspace.id, pane.id, tab.id);
      return { success: true, tabId: tab.id, enabled: false };
    }

    // If all context fields are provided, set directly
    if (args.cwd !== undefined || args.sshCommand !== undefined || args.remoteCwd !== undefined) {
      const cmd = args.command ?? getResumeCommand(workspacesStore.getTabRuntime(tab.id));
      await workspacesStore.setTabAutoResumeContext(
        workspace.id, pane.id, tab.id,
        args.cwd ?? null, args.sshCommand ?? null, args.remoteCwd ?? null, cmd,
      );
      return { success: true, tabId: tab.id, enabled: true, command: cmd };
    }

    // Auto-detect PTY context (same as trigger-based enable)
    const cmd = args.command ?? getResumeCommand(workspacesStore.getTabRuntime(tab.id));
    await handleEnableAutoResume(tab.id, cmd);
    return { success: true, tabId: tab.id, enabled: true, command: cmd };
  }

  function handleGetAutoResume(args: { tabId?: string }) {
    const resolved = resolveActiveTab(args.tabId);
    if ('error' in resolved) return resolved;
    const { tab } = resolved;
    const hasConfig = !!(tab.auto_resume_command || tab.auto_resume_cwd || tab.auto_resume_ssh_command);
    return {
      tabId: tab.id,
      enabled: tab.auto_resume_enabled && hasConfig,
      configured: hasConfig,
      pinned: tab.auto_resume_pinned,
      command: tab.auto_resume_command ?? null,
      cwd: tab.auto_resume_cwd ?? null,
      sshCommand: tab.auto_resume_ssh_command ?? null,
      remoteCwd: tab.auto_resume_remote_cwd ?? null,
    };
  }

  function handleFindNotes() {
    const tabNotes: { tabId: string; displayName: string; workspace: string; notes: string; notesMode: string }[] = [];
    const workspaceNotes: { workspaceId: string; workspace: string; noteId: string; preview: string; mode: string | null }[] = [];

    for (const ws of workspacesStore.workspaces) {
      // Collect workspace-level notes
      for (const note of ws.workspace_notes) {
        workspaceNotes.push({
          workspaceId: ws.id,
          workspace: ws.name,
          noteId: note.id,
          preview: note.content.slice(0, 200),
          mode: note.mode ?? null,
        });
      }
      // Collect tab-level notes
      for (const pane of ws.panes) {
        for (const tab of pane.tabs) {
          if (tab.notes) {
            tabNotes.push({
              tabId: tab.id,
              displayName: tabDisplayName(tab),
              workspace: ws.name,
              notes: tab.notes.slice(0, 200),
              notesMode: tab.notes_mode ?? 'source',
            });
          }
        }
      }
    }

    return { tabNotes, workspaceNotes };
  }

  async function handleSendNotification(args: { tabId?: string; title: string; body?: string; type?: string }) {
    const type = (['success', 'error', 'info'].includes(args.type ?? '') ? args.type : 'info') as 'success' | 'error' | 'info';
    const source = args.tabId ? { tabId: args.tabId } : undefined;
    await dispatchNotification(args.title, args.body ?? '', type, source);
    return { sent: true };
  }

  /** Resolve a tab by ID or fall back to the active tab. Returns { workspace, pane, tab } or { error }. */
  function resolveActiveTab(tabId?: string): { workspace: Workspace; pane: Pane; tab: Tab } | { error: string } {
    if (tabId) {
      const loc = findTabLocation(tabId);
      if (!loc) return { error: `Tab not found: ${tabId}` };
      return loc;
    }
    const ws = workspacesStore.workspaces.find(w => w.id === workspacesStore.activeWorkspaceId);
    if (!ws) return { error: 'No active workspace' };
    const pane = ws.panes.find(p => p.id === ws.active_pane_id);
    if (!pane) return { error: 'No active pane' };
    const tab = pane.tabs.find(t => t.id === pane.active_tab_id);
    if (!tab) return { error: 'No active tab' };
    return { workspace: ws, pane, tab };
  }

  function updateSelection(info: SelectionInfo) {
    latestSelection = info;
    commands.claudeCodeNotifySelection({
      jsonrpc: '2.0',
      method: 'notifications/selection_changed',
      params: info,
    }).catch(() => {});
  }

  function setConnected(value: boolean) {
    connected = value;
  }

  function getPendingSelection(tabId: string): PendingSelection | undefined {
    return pendingSelections.get(tabId);
  }

  function clearPendingSelection(tabId: string): void {
    pendingSelections.delete(tabId);
  }

  return {
    get connected() { return connected; },
    get latestSelection() { return latestSelection; },
    handleToolRequest,
    updateSelection,
    setConnected,
    getPendingSelection,
    clearPendingSelection,
  };
}

export const claudeCodeStore = createClaudeCodeStore();
