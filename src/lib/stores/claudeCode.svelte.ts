import type { ClaudeCodeToolRequest, DiffContext, Workspace, Pane, Tab, Task, TaskStatus } from '$lib/tauri/types';
import * as commands from '$lib/tauri/commands';
import { workspacesStore, navigateToTab } from '$lib/stores/workspaces.svelte';
import { terminalsStore } from '$lib/stores/terminals.svelte';
import { getEditorByFilePath, getEditorByTabId } from '$lib/stores/editorRegistry.svelte';
import { interpolateVariables, getVariables, setVariable, handleEnableAutoResume, getTriggerStats } from '$lib/stores/triggers.svelte';
import { preferencesStore } from '$lib/stores/preferences.svelte';
import { dispatch as dispatchNotification } from '$lib/stores/notificationDispatch';
import { claudeStateStore, resumeCommandFor } from '$lib/stores/agentState.svelte';
import { agentBridgeStore } from '$lib/stores/agentBridge.svelte';
import { agentMeshStore } from '$lib/stores/agentMesh.svelte';
import { overlordStore } from '$lib/stores/overlord.svelte';
import { tasksStore } from '$lib/stores/tasks.svelte';
import { appendNote, blocking, coerceStatus, effectiveStatus, hasUnmetDeps, isDelegation, isInFlight, normalizeTitle, resolveBlockers, resolveEdges, TASK_NOTE_CAP } from '$lib/tasks/model';
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
          result = handleListWorkspaceNotes(args as { workspaceId?: string; tabId?: string });
          break;
        case 'readWorkspaceNote':
          result = handleReadWorkspaceNote(args as { workspaceId?: string; tabId?: string; noteId: string });
          break;
        case 'writeWorkspaceNote':
          result = await handleWriteWorkspaceNote(args as { workspaceId?: string; tabId?: string; noteId?: string; content: string; mode?: string | null });
          break;
        case 'deleteWorkspaceNote':
          result = await handleDeleteWorkspaceNote(args as { workspaceId?: string; tabId?: string; noteId: string });
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
        case 'listTasks':
          result = handleListTasks(args as { tabId?: string; scope?: 'tab' | 'workspace'; status?: string[]; ready?: boolean; limit?: number });
          break;
        case 'createTasks':
          result = handleCreateTasks(args as { tabId?: string; workstream?: string; tasks?: TaskToolInput[] });
          break;
        case 'updateTasks':
          result = handleUpdateTasks(args as { tabId?: string; updates?: TaskToolUpdate[] });
          break;
        // getPreferences, setPreference, createBackup, listWindows handled directly on backend
        case 'replyToOverlord': {
          const a = args as { tabId?: string; kind: string; state: string; summary: string; task?: string; blockers?: string[]; next?: string; needs_human?: boolean };
          result = a.tabId
            ? overlordStore.handleAgentReply(a.tabId, a)
            : { error: 'No tab identity — call initSession first.' };
          break;
        }
        case 'listEscalations': {
          const a = args as { tabId?: string };
          result = a.tabId
            ? overlordStore.listEscalationsFor(a.tabId)
            : { error: 'No tab identity — call initSession first.' };
          break;
        }
        case 'driveTab': {
          const a = args as { tabId?: string; tab_id: string; kind: 'process' | 'slash'; text: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'driveTab is available only to the Overlord agent tab.' };
          else if (!a.tab_id || !a.text) result = { error: 'tab_id and text are required.' };
          else if (a.tab_id === a.tabId) result = { sent: false, reason: 'cannot drive your own tab' };
          else result = await overlordStore.driveTab(a.tab_id, a.kind === 'slash' ? 'slash' : 'process', a.text);
          break;
        }
        case 'getTabPrompt': {
          const a = args as { tabId?: string; tab_id: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'getTabPrompt is available only to the Overlord agent tab.' };
          else if (!a.tab_id) result = { error: 'tab_id is required.' };
          else if (overlordStore.isExemptTab(a.tab_id)) result = { error: 'That tab is exempt from Overlord — the human marked it, or its workspace, exempt. Leave it alone.' };
          else result = { prompt: await overlordStore.tabPrompt(a.tab_id) };
          break;
        }
        case 'answerTabPrompt': {
          const a = args as {
            tabId?: string;
            tab_id: string;
            prompt_id?: string;
            choice?: string;
            answers?: import('$lib/tauri/commands').PromptAnswer[];
          };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'answerTabPrompt is available only to the Overlord agent tab.' };
          else if (!a.tab_id) result = { error: 'tab_id is required.' };
          // Answering your own prompt is a loop: the agent would resolve the very ask it
          // raised to reach the human, and the human would never see it.
          else if (a.tab_id === a.tabId) result = { ok: false, reason: 'cannot answer your own prompt' };
          else if (overlordStore.isExemptTab(a.tab_id)) result = { ok: false, reason: 'exempt', detail: 'That tab is exempt from Overlord — the human marked it, or its workspace, exempt. Leave it alone.' };
          else if (!a.choice && !a.answers?.length) result = { ok: false, reason: 'bad_request', detail: 'pass choice (permission) or answers (question)' };
          else result = await overlordStore.answerPrompt(a.tab_id, a.prompt_id ?? null, a.choice ?? null, a.answers ?? null);
          break;
        }
        case 'archiveTab':
        case 'closeTab': {
          const a = args as { tabId?: string; tab_id: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: `${tool} is available only to the Overlord agent tab.` };
          else if (!a.tab_id) result = { error: 'tab_id is required.' };
          // Retiring your own tab takes the supervisor out of the window mid-call.
          else if (a.tab_id === a.tabId) result = { ok: false, reason: 'cannot retire your own tab' };
          else result = await overlordStore.retireTab(a.tab_id, tool === 'archiveTab' ? 'archive' : 'close');
          break;
        }
        case 'recoverTab': {
          const a = args as { tabId?: string; tab_id: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'recoverTab is available only to the Overlord agent tab.' };
          else if (!a.tab_id) result = { error: 'tab_id is required.' };
          else if (a.tab_id === a.tabId) result = { sent: false, reason: 'cannot recover your own tab' };
          else result = await overlordStore.recoverTab(a.tab_id);
          break;
        }
        case 'deleteArchivedTab': {
          const a = args as { tabId?: string; tab_id: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'deleteArchivedTab is available only to the Overlord agent tab.' };
          else if (!a.tab_id) result = { error: 'tab_id is required.' };
          else result = await overlordStore.deleteArchivedTabById(a.tab_id);
          break;
        }
        case 'resumeTab': {
          const a = args as { tabId?: string; tab_id: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'resumeTab is available only to the Overlord agent tab.' };
          else if (!a.tab_id) result = { error: 'tab_id is required.' };
          else result = await overlordStore.resumeTabById(a.tab_id);
          break;
        }
        case 'resumeWorkspace': {
          const a = args as { tabId?: string; workspace_id: string };
          if (!a.tabId) result = { error: 'No tab identity — call initSession first.' };
          else if (!overlordStore.isOverlordAgentTab(a.tabId)) result = { error: 'resumeWorkspace is available only to the Overlord agent tab.' };
          else if (!a.workspace_id) result = { error: 'workspace_id is required.' };
          else result = await overlordStore.resumeWorkspaceById(a.workspace_id);
          break;
        }
        case 'proposeRuleChanges': {
          const a = args as { tabId?: string; rationale: string; changes: import('$lib/stores/overlord.svelte').OverlordRuleChange[] };
          result = a.tabId
            ? await overlordStore.proposeRuleChanges(a.tabId, a.rationale, a.changes ?? [])
            : { error: 'No tab identity — call initSession first.' };
          break;
        }
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

  /** Archived tabs live outside the pane tree, so `findTabLocation` cannot see them. Read-only
   *  callers that legitimately want one use this instead. */
  function findArchivedTab(tabId: string): { workspace: Workspace; tab: Tab } | null {
    for (const ws of workspacesStore.workspaces) {
      const tab = (ws.archived_tabs ?? []).find(t => t.id === tabId);
      if (tab) return { workspace: ws, tab };
    }
    return null;
  }

  function resolveWorkspace(workspaceId?: string): Workspace | null {
    if (workspaceId) return workspacesStore.workspaces.find(ws => ws.id === workspaceId) ?? null;
    return workspacesStore.activeWorkspace;
  }

  /** Resolve the workspace for a NOTE operation. Precedence: explicit workspaceId, then the
   *  CALLER'S OWN workspace (from the connection-injected tabId), then the active workspace.
   *  Using the caller's tab — not the focused workspace — stops an agent in mesh B from
   *  writing its note into mesh A just because the human is looking at A (cross-workspace
   *  bleed). Human/UI callers have no tabId and correctly fall back to the active workspace. */
  function resolveWorkspaceForNote(args: { workspaceId?: string; tabId?: string }): Workspace | null {
    if (args.workspaceId) return workspacesStore.workspaces.find(ws => ws.id === args.workspaceId) ?? null;
    if (args.tabId) {
      const loc = findTabLocation(args.tabId);
      if (loc) return loc.workspace;
    }
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
        ...(ws.overlord_exempt ? { overlordExempt: true } : {}),
        noteCount: ws.workspace_notes.length,
        archivedTabCount: ws.archived_tabs?.length ?? 0,
        panes: ws.panes.map(pane => ({
          id: pane.id,
          name: pane.name,
          isActive: pane.id === ws.active_pane_id,
          tabs: pane.tabs.map(tab => {
            const claude = claudeStateStore.getState(tab.id);
            const isAgent = (tab.tab_type ?? 'terminal') === 'terminal' && !!tab.runtime;
            return {
              id: tab.id,
              displayName: tabDisplayName(tab),
              tabType: tab.tab_type ?? 'terminal',
              isActive: tab.id === pane.active_tab_id,
              hasNotes: !!tab.notes,
              // `state` and `loaded` answer two different questions and are both needed to
              // choose an action: what the agent is doing, and whether anything can reach it.
              // A healthy `idle` agent in an unmounted pane is not drivable.
              ...(isAgent
                ? {
                    runtime: tab.runtime,
                    // Off limits to the engine and every Overlord tool — the human said so.
                    ...(overlordStore.isExemptTab(tab.id) ? { overlordExempt: true } : {}),
                    pty: overlordStore.tabPtyState(tab.id),
                    state: overlordStore.tabAgentState(tab.id),
                    loaded: overlordStore.tabLoaded(tab.id),
                    ...(tab.suspended_at && !overlordStore.tabLoaded(tab.id) ? { suspendedAt: tab.suspended_at } : {}),
                    // How old a live tab is. Without it the only way to judge a dormant tab's
                    // age was to grep months of maiTerm logs for its UUID — which bottoms out
                    // at the log floor, so everything older read as the same date.
                    ...(() => {
                      const f = overlordStore.facts.get(tab.id);
                      return {
                        ...(f?.last_turn_ts ? { lastTurnAt: new Date(f.last_turn_ts).toISOString() } : {}),
                        ...(f?.context_pct !== undefined ? { contextPct: Math.round(f.context_pct) } : {}),
                      };
                    })(),
                  }
                : {}),
              ...(claude?.toolName ? { claudeTool: claude.toolName } : {}),
            };
          }),
        })),
        // Listed separately because that is what they ARE: lifted out of the pane tree, not
        // parked inside it. Conflating these with a suspended workspace's tabs is the exact
        // confusion this shape exists to prevent.
        archivedTabs: (ws.archived_tabs ?? []).map(tab => ({
          id: tab.id,
          displayName: tab.archived_name ?? tab.name,
          archivedAt: tab.archived_at ?? null,
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
    // Archived tabs are readable. `hasNotes` was visible on them while the notes themselves
    // were not, so the only way to find out what was in one was to restore it — which is a
    // silly price for reading a note, and exactly backwards when the notes are how you decide
    // whether it is worth restoring. READ only: the write paths keep using findTabLocation,
    // since an archived tab has no pane to render an edit into.
    if (!loc) {
      const archived = findArchivedTab(args.tabId);
      if (archived) {
        return {
          tabId: archived.tab.id,
          displayName: archived.tab.archived_name ?? archived.tab.name,
          notes: archived.tab.notes ?? null,
          notesMode: archived.tab.notes_mode ?? null,
          archived: true,
          archivedAt: archived.tab.archived_at ?? null,
          workspaceId: archived.workspace.id,
          readOnly: 'This tab is archived — restoreArchivedTab first if you need to change its notes.',
        };
      }
      return { error: `Tab not found: ${args.tabId}` };
    }
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

  function handleListWorkspaceNotes(args: { workspaceId?: string; tabId?: string }) {
    const ws = resolveWorkspaceForNote(args);
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

  function handleReadWorkspaceNote(args: { workspaceId?: string; tabId?: string; noteId: string }) {
    const ws = resolveWorkspaceForNote(args);
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

  async function handleWriteWorkspaceNote(args: { workspaceId?: string; tabId?: string; noteId?: string; content: string; mode?: string | null }) {
    const ws = resolveWorkspaceForNote(args);
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

    // NOTE: do not auto-open the notes panel here. Workspace-note writes are
    // frequent (mesh agents emit status notes continuously), and the old
    // auto-open targeted the *active* workspace's tab rather than `ws`, so a
    // note written in one workspace would pop the panel open in whatever
    // workspace the user was currently looking at. Agents that genuinely want
    // to surface a note should call the openNotesPanel tool explicitly.

    return { success: true, noteId: resultNoteId, action };
  }

  async function handleDeleteWorkspaceNote(args: { workspaceId?: string; tabId?: string; noteId: string }) {
    const ws = resolveWorkspaceForNote(args);
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

  // --- maiTerm task tools (docs/tasks.md §5) ---
  //
  // Every call resolves "this project" from the CALLING TAB's workspace, so a tab can
  // neither read nor write another project's list. The tools are also listed in
  // PEER_ADDRESSING_TOOLS server-side, so a deduced (rather than stated) identity is
  // refused outright before reaching here.

  interface TaskToolInput {
    /** Only on updates — creates carry the workstream once, at the call level. */
    title?: string;
    detail?: string;
    /** Declared as an enum in the schema, but nothing enforces it at runtime — typed as a
     *  plain string so the coercion is visibly required rather than assumed. */
    status?: string;
    blocked_by?: string[];
    assign_to_me?: boolean;
  }

  interface TaskToolUpdate {
    id?: string;
    status?: string;
    title?: string;
    detail?: string;
    workstream?: string;
    blocked_by?: string[];
    /** Incremental dependency edits. `blocked_by` replaces the whole array, which is
     *  last-write-wins against any concurrent edit (docs/tasks.md §8.2 — the store persists
     *  a WHOLE workspace list per write); these two add and remove single edges instead. */
    block_on?: string[];
    unblock_from?: string[];
    /** A tab id, the literal "me", or null to release. Absent means leave the assignee
     *  alone — `null` and absent are different answers and must stay so. */
    assign_to?: string | null;
    /** One line appended to the task's log. Never replaces `detail`. */
    note?: string;
  }

  /** The shape agents see. Deliberately not the raw Task: `normalized_title` is an
   *  internal dedup key and would only invite an agent to try to set it. */
  /** Names a prerequisite parked with an archived tab, and says which tab holds it.
   *
   *  `Tab.archived_tasks` is off every list the tools read, so such a blocker arrived as a
   *  bare id that resolved to nothing in the payload — an agent was told it was waiting on
   *  something it could neither see nor act on. It can act: `restoreArchivedTab` takes the
   *  tab id this carries. */
  const parkedLookup = (id: string) => {
    const held = workspacesStore.parkedTasks.get(id);
    return held
      ? { title: held.task.title, status: held.task.status, tab_id: held.tabId, tab_name: held.tabName }
      : undefined;
  };

  function taskForAgent(t: Task, all: Task[], selfTabId: string, workstream?: string, noteTail = 3) {
    const parked = workspacesStore.parkedTaskIds;
    // Blockers RESOLVED, not raw ids. An agent handed `["a3f8…"]` had to scan the whole
    // list to learn what it was waiting on — and on a `scope: 'tab'` list the prerequisite
    // is frequently on another tab and not in the payload at all, so the id resolved to
    // nothing it could see. `state` is the part that decides anything: `met` is finished,
    // `waiting` is live, `parked` is off-list with an archived tab (still blocks), `gone`
    // is deleted (does not). A `waiting` blocker sitting in `dropped` is the case worth
    // reading — nobody intends to do it, and this task is waiting anyway.
    const blockers = resolveBlockers(t, all, parked, parkedLookup);
    // The reverse edge: who is waiting on THIS. Nothing answered it before, and it is what
    // an agent needs before it goes idle — "who did I just unblock". Empty for almost every
    // row, so it costs nothing on the rows that don't block anything.
    const waiters = blocking(t, all);
    return {
      id: t.id,
      title: t.title,
      ...(workstream ? { workstream } : {}),
      ...(t.detail ? { detail: t.detail } : {}),
      status: effectiveStatus(t, all, parked),
      assignee: t.tab_id === selfTabId ? 'you' : (t.tab_id ?? 'unassigned'),
      ...(blockers.length ? { blocked_by: blockers } : {}),
      ...(waiters.length ? { blocking: waiters.map((w) => ({ id: w.id, title: w.title })) } : {}),
      // The tail of the log, not all of it. A workspace-scope list carries every row, and
      // the newest few notes are what says why a task is where it is — the older ones are
      // history the agent can ask for by narrowing to `scope: 'tab'`.
      ...(t.notes?.length
        ? { notes: t.notes.slice(-noteTail).map((n) => ({ at: n.at, by: n.by, text: n.text })) }
        : {}),
      origin: t.origin,
      updated_at: t.updated_at,
    };
  }

  /** Truncation order: what a caller can least afford to lose comes first.
   *
   *  A list that grows without bound has to be cut somewhere, and cutting in stored order
   *  drops whatever happens to be last — routinely the task the agent is working on, while
   *  a year of finished rows survives above it. Ranked instead, so `limit` eats history. */
  const LANE_RANK: Record<TaskStatus, number> = {
    active: 0, blocked: 1, review: 2, todo: 3, backlog: 4, done: 5, dropped: 6,
  };

  function handleListTasks(args: {
    tabId?: string;
    scope?: 'tab' | 'workspace';
    status?: string[];
    ready?: boolean;
    limit?: number;
  }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    const all = tasksStore.forWorkspace(loc.workspace.id);
    const parked = workspacesStore.parkedTaskIds;
    const tabScope = args.scope === 'tab';
    let scoped = tabScope ? all.filter((t) => t.tab_id === loc.tab.id) : all;
    const total = scoped.length;

    // Lane is matched on the EFFECTIVE status, the one the board and this tool both show —
    // filtering on the stored value would omit a task the same call reports as `blocked`.
    const laneOf = (t: Task) => effectiveStatus(t, all, parked);
    if (args.status?.length) {
      const want = new Set(args.status.map((s) => coerceStatus(s)));
      scoped = scoped.filter((t) => want.has(laneOf(t)));
    }
    // "What can I pick up right now", which is the question `listTasks` was always being
    // asked and could only answer by returning everything. Unmet dependencies are the part
    // an agent cannot work out for itself when the prerequisite is on another tab.
    if (args.ready) {
      scoped = scoped.filter(
        (t) =>
          isInFlight(t) &&
          !hasUnmetDeps(t, all, parked) &&
          (t.tab_id === loc.tab.id || !t.tab_id),
      );
    }

    // A workspace list carries every row in the project, so it gets the tail of each log —
    // enough to say why a task sits where it does. Narrowing to your own tab is the way to
    // ask for the whole log, and it costs nothing there.
    const noteTail = tabScope ? TASK_NOTE_CAP : 3;
    const nameOf = (id: string | null | undefined) =>
      tasksStore.workstream(loc.workspace.id, id)?.name;

    const matched = scoped.length;
    const limit = Math.min(Math.max(1, args.limit ?? 100), 500);
    if (matched > limit) {
      scoped = [...scoped].sort((a, b) => LANE_RANK[laneOf(a)] - LANE_RANK[laneOf(b)]).slice(0, limit);
    }

    // Grouped by workstream, because that is the structure the agent is meant to work in:
    // a flat list would invite it to treat two separate jobs as one.
    const groups = new Map<string, { workstream: string | null; tasks: unknown[] }>();
    for (const t of scoped) {
      const key = t.workstream_id ?? '';
      if (!groups.has(key)) groups.set(key, { workstream: nameOf(t.workstream_id) ?? null, tasks: [] });
      groups.get(key)!.tasks.push(taskForAgent(t, all, loc.tab.id, nameOf(t.workstream_id), noteTail));
    }
    return {
      workspace: loc.workspace.name,
      scope: tabScope ? 'tab' : 'workspace',
      workstreams: [...groups.values()],
      // Only when something was left out, and always with the way to get at it. A silently
      // short list is indistinguishable from a project with fewer tasks in it.
      ...(matched > scoped.length
        ? {
            truncated: {
              shown: scoped.length,
              matched,
              detail: `${matched - scoped.length} more match. Rows are ordered active → blocked → review → todo → backlog → done → dropped, so what is missing is the tail of that. Narrow with status, ready, or scope 'tab', or raise limit (max 500).`,
            },
          }
        : {}),
      // Every job in this project, including ones whose tasks are all outside the filter or
      // on another tab — so an agent naming a workstream on createTasks can reuse a spelling
      // that exists rather than minting a near-duplicate it cannot see.
      ...(tabScope || args.status?.length || args.ready
        ? { all_workstreams: tasksStore.workstreams(loc.workspace.id).map((w) => w.name) }
        : {}),
      ...(matched !== total ? { filtered_from: total } : {}),
    };
  }

  function handleCreateTasks(args: { tabId?: string; workstream?: string; tasks?: TaskToolInput[] }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    const inputs = (args.tasks ?? []).filter((t) => t?.title?.trim());
    if (!inputs.length) return { error: 'tasks must be a non-empty array of { title }.' };
    // Agents pass a workstream NAME, not an id — requiring a round trip just to record
    // work would make the common case worse. Reused if it exists, created if not.
    const stream = args.workstream ? tasksStore.ensureWorkstream(loc.workspace.id, args.workstream) : null;
    const before = new Set(tasksStore.forWorkspace(loc.workspace.id).map((t) => t.id));
    const rows = tasksStore.addMany(
      loc.workspace.id,
      inputs.map((t) => ({
        title: t.title!.trim(),
        detail: t.detail ?? null,
        status: coerceStatus(t.status),
        // Assigned to the caller unless it explicitly leaves the task unassigned.
        tab_id: t.assign_to_me === false ? null : loc.tab.id,
        blocked_by: t.blocked_by ?? [],
        origin: 'agent' as const,
        workstream_id: stream?.id ?? null,
      })),
    );
    // Report duplicates honestly rather than silently: an agent re-sending its list after
    // a compact should be able to tell that nothing new was recorded. De-duplicated by id
    // because two entries in ONE batch can collapse onto the same row ("Add the auth
    // guard" and "add auth guard.") — reporting that id twice under `created` would tell
    // the agent it made two tasks, and an agent reconciling counts would retry forever.
    const created = [...new Set(rows.filter((r) => !before.has(r.id)).map((r) => r.id))];
    const existing = [...new Set(rows.filter((r) => before.has(r.id)).map((r) => r.id))];
    return {
      created,
      ...(stream ? { workstream: stream.name } : {}),
      ...(existing.length ? { already_tracked: existing } : {}),
      tasks: [...new Map(rows.map((r) => [r.id, r])).values()].map((r) => ({
        id: r.id,
        title: r.title,
        status: r.status,
      })),
    };
  }

  /** Tab ids in the calling tab's workspace, which is the only place an assignee may point.
   *
   *  Reaching outside it would put a row on a tab whose own panel cannot show it (the panel
   *  reads one workspace's list) and whose close would never release it, since `releaseTab`
   *  walks lists looking for the tab that closed — the row would be attributed to a tab that
   *  is not there and reachable from nowhere. Exactly the state `archive_tab`'s
   *  cross-workspace release exists to prevent. */
  function tabIdsInWorkspace(ws: Workspace): Set<string> {
    return new Set(ws.panes.flatMap((p) => p.tabs.map((t) => t.id)));
  }

  function handleUpdateTasks(args: { tabId?: string; updates?: TaskToolUpdate[] }) {
    const loc = resolveActiveTab(args.tabId);
    if ('error' in loc) return loc;
    const updates = (args.updates ?? []).filter((u) => u?.id);
    if (!updates.length) return { error: 'updates must be a non-empty array of { id, ... }.' };
    const updated: string[] = [];
    const missing: string[] = [];
    /** Updates that named a real row but asked for something we would not do. Reported
     *  rather than silently dropped: an agent that hands work to a tab and is told nothing
     *  believes the hand-off happened and stops tracking the task. */
    const refused: { id: string; reason: string; detail: string }[] = [];
    /** Every row this call touched, task id → the tab that owned it when the call began.
     *
     *  Hand-offs are DERIVED from this against the committed list after the batch, never
     *  accumulated during it. Two rounds of review found bugs in the accumulating version
     *  and they were both the same class — a record written under one condition and read
     *  under another: a refused update left a stale entry, then a later `assign_to: null`
     *  in the same batch left one the write had undone, because the recording branch had
     *  no `else` that cleared it. There is nothing to go stale in a diff of before against
     *  after, so the class is gone rather than the instances. */
    const entryOwner = new Map<string, string | null>();
    const known = tabIdsInWorkspace(loc.workspace);
    tasksStore.mutate(loc.workspace.id, (list) => {
      let changed = false;
      for (const u of updates) {
        const idx = list.findIndex((t) => t.id === u.id);
        // Scoped to this workspace on purpose: an id from another project is "missing"
        // here, not an invitation to reach across.
        if (idx < 0) {
          missing.push(u.id!);
          continue;
        }
        // Who owned this row when the CALL began. Recorded once per id, before anything is
        // written, and diffed against the committed owner after the whole batch — see the
        // hand-off block below the loop.
        if (!entryOwner.has(u.id!)) entryOwner.set(u.id!, list[idx].tab_id ?? null);
        // Resolve the assignee BEFORE touching anything, so a refused hand-off doesn't half
        // apply the rest of the same update — the agent would then be told the row was
        // refused while its title had already changed.
        let assignTo: string | null | undefined;
        if (u.assign_to !== undefined) {
          const want = u.assign_to === 'me' ? loc.tab.id : u.assign_to;
          if (want !== null && !known.has(want)) {
            refused.push({
              id: u.id!,
              reason: 'unknown_tab',
              detail: `No tab ${want} in this project. Assignees must be a tab in this workspace — use listWorkspaces to find one, "me" for your own tab, or null to release the task to whoever picks it up.`,
            });
            continue;
          }
          assignTo = want;
        }
        const patch: Partial<Task> = { updated_at: new Date().toISOString() };
        if (assignTo !== undefined) patch.tab_id = assignTo;
        if (u.status) patch.status = coerceStatus(u.status);
        if (u.title?.trim()) {
          patch.title = u.title.trim();
          patch.normalized_title = normalizeTitle(u.title);
        }
        if (u.detail !== undefined) patch.detail = u.detail;
        // Appended, never replacing `detail`. That separation is the whole point: `detail`
        // is the spec, and recording why a task is blocked by rewriting the spec destroys
        // the spec. Trimmed here and again by Rust before disk.
        if (u.note?.trim()) patch.notes = appendNote(list[idx], u.note, 'agent');
        if (u.workstream !== undefined) {
          patch.workstream_id = u.workstream.trim()
            ? (tasksStore.ensureWorkstream(loc.workspace.id, u.workstream)?.id ?? null)
            : null;
        }
        if (u.blocked_by || u.block_on || u.unblock_from) {
          // `blocked_by` is the base (whole-array replace, kept for the caller that knows
          // the full set), then the incremental edits apply on top. Which ids get validated
          // and why is `resolveEdges`, where it is unit-testable.
          //
          // Parked ids count as existing: off the list with an archived tab, still blocking.
          const edges = resolveEdges(u.id!, list[idx].blocked_by ?? [], u, (b) =>
            list.some((t) => t.id === b) || workspacesStore.parkedTaskIds.has(b),
          );
          if (!edges.ok) {
            refused.push({
              id: u.id!,
              reason: edges.reason,
              detail:
                edges.reason === 'self_dependency'
                  ? 'A task cannot block itself — that edge is never met, so the row would sit in Blocked forever, and no human control can free it. Applies through blocked_by as well as block_on.'
                  : `No task ${edges.bad.join(', ')} in this project. A blocker id that resolves to nothing counts as MET, so recording it would look like a dependency and do nothing. Get ids from listTasks. To drop an edge you already have, use unblock_from.`,
            });
            continue;
          }
          patch.blocked_by = edges.edges;
        }
        list[idx] = { ...list[idx], ...patch };
        updated.push(u.id!);
        changed = true;
      }
      return changed ? list : null;
    });
    // Announced AFTER the commit, and computed from it: for each row the call touched, the
    // owner it had on entry against the owner it has now. A refused update, an assignment
    // undone later in the same batch, and a batch that nets back to the original owner all
    // fall out as "no delegation" without needing a case each — they are simply rows whose
    // owner did not change, or changed to the caller.
    const nameOfTab = (id: string) => {
      const tab = loc.workspace.panes.flatMap((p) => p.tabs).find((t) => t.id === id);
      return tab ? tabDisplayName(tab) : id;
    };
    const handoffs = [...entryOwner]
      .map(([id, from]) => ({ id, from, to: tasksStore.find(loc.workspace.id, id)?.tab_id ?? null }))
      .filter((h): h is { id: string; from: string | null; to: string } =>
        isDelegation(h.from, h.to, loc.tab.id),
      )
      .map(({ id, to }) => {
        const told = overlordStore.announceHandoff(id, loc.tab.id, to);
        return {
          id,
          to,
          told,
          detail:
            told === 'agent'
              ? `${nameOfTab(to)} has NOT been typed into — nothing types into a tab on an agent's say-so. This window's Overlord has been told and will decide whether to drive it.`
              : `The task is assigned to ${nameOfTab(to)} and shows on its task panel and the board, but that tab has NOT been told: nothing types into a tab on an agent's say-so, and there is no supervisor here to relay it. If it needs starting now, say so to your human — they can press "Do it" on the row.`,
        };
      });
    return {
      updated,
      ...(missing.length ? { missing } : {}),
      ...(refused.length ? { refused } : {}),
      ...(handoffs.length ? { handoffs } : {}),
    };
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
      const cmd = args.command ?? resumeCommandFor(workspacesStore.getTabRuntime(tab.id));
      await workspacesStore.setTabAutoResumeContext(
        workspace.id, pane.id, tab.id,
        args.cwd ?? null, args.sshCommand ?? null, args.remoteCwd ?? null, cmd,
      );
      return { success: true, tabId: tab.id, enabled: true, command: cmd };
    }

    // Auto-detect PTY context (same as trigger-based enable)
    const cmd = args.command ?? resumeCommandFor(workspacesStore.getTabRuntime(tab.id));
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
