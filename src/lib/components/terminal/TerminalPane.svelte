<script lang="ts">
  import { onMount, onDestroy, untrack } from 'svelte';
  import { countedListen as listen } from '$lib/utils/listenCounter';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { Terminal } from '@xterm/xterm';
  import { FitAddon } from '@xterm/addon-fit';
  import { WebLinksAddon } from '@xterm/addon-web-links';
  import { CanvasAddon } from '@xterm/addon-canvas';
  import '@xterm/xterm/css/xterm.css';
  import { spawnTerminal, writeTerminal, resizeTerminal, killTerminal, setTabScrollback, getPtyInfo, setTabRestoreContext, cleanSshCommand, normalizeSshInput, buildSshCommand, shellEscapePath, readClipboardFilePaths, serializeTerminal, restoreTerminalScrollback, resizeTerminalGrid, scrollTerminal, scrollTerminalTo, saveTerminalScrollback, restoreTerminalFromSaved, hasSavedScrollback, scpUploadFiles, playBellSound, saveClipboardImage, startSelection, updateSelection, clearSelection, copySelection, selectAll, scrollSelection } from '$lib/tauri/commands';
  import type { TerminalFrame, OscCwdEvent, OscShellEvent } from '$lib/tauri/types';
  import { readText as clipboardReadText, writeText as clipboardWriteText, readImage as clipboardReadImage } from '@tauri-apps/plugin-clipboard-manager';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { activityStore } from '$lib/stores/activity.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import { getTheme } from '$lib/themes';
  import { getCompiledPatterns } from '$lib/utils/promptPattern';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import { isModKey, modSymbol } from '$lib/utils/platform';
  import { buildShellIntegrationSnippet, buildInstallSnippet } from '$lib/utils/shellIntegration';
  import ResizableTextarea from '$lib/components/ResizableTextarea.svelte';
  import { processOutput, cleanupTab, loadTabVariables, interpolateVariables, getVariables, clearTabVariables, suppressTab, unsuppressTab, replayAutoResume } from '$lib/stores/triggers.svelte';
  import { dispatch } from '$lib/stores/notificationDispatch';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { CLAUDE_RESUME_COMMAND } from '$lib/triggers/defaults';
  import { createFilePathLinkProvider } from '$lib/utils/filePathDetector';
  import { openFileFromTerminal } from '$lib/utils/openFile';
  import { enableBridge, disableBridge, hasBridge, getBridgeInfo, buildUserSetupScript, isInteractiveSshSession } from '$lib/stores/sshMcpBridge.svelte';
  import { claudeStateStore } from '$lib/stores/claudeState.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import Button from '$lib/components/ui/Button.svelte';

  interface Props {
    workspaceId: string;
    paneId: string;
    tabId: string;
    existingPtyId?: string | null;
    visible: boolean;
    restoreCwd?: string | null;
    restoreSshCommand?: string | null;
    restoreRemoteCwd?: string | null;
    autoResumeCwd?: string | null;
    autoResumeSshCommand?: string | null;
    autoResumeRemoteCwd?: string | null;
    autoResumeCommand?: string | null;
    autoResumeRememberedCommand?: string | null;
    autoResumePinned?: boolean;
    autoResumeEnabled?: boolean;
    triggerVariables?: Record<string, string>;
  }

  let { workspaceId, paneId, tabId, existingPtyId, visible, restoreCwd, restoreSshCommand, restoreRemoteCwd, autoResumeCwd, autoResumeSshCommand, autoResumeRemoteCwd, autoResumeCommand, autoResumeRememberedCommand, autoResumePinned, autoResumeEnabled, triggerVariables }: Props = $props();

  let containerRef: HTMLDivElement;
  let terminal: Terminal;
  let fitAddon: FitAddon;
  let ptyId: string;
  let destroyed = false;
  let unlistenOutput: UnlistenFn;
  let unlistenRaw: UnlistenFn;
  let unlistenClose: UnlistenFn;
  let unlistenTitle: UnlistenFn;
  let unlistenCwd: UnlistenFn;
  let unlistenShell: UnlistenFn;
  let unlistenNotification: UnlistenFn;
  let unlistenClipboard: UnlistenFn;
  let unlistenBell: UnlistenFn;
  let unlistenDragDrop: UnlistenFn;
  let resizeObserver: ResizeObserver;
  let filePathLinkDisposable: { dispose: () => void } | null = null;
  let initialized = $state(false);
  let canvasAddon: CanvasAddon | null = null;
  let trackActivity = false;
  let visibilityGraceUntil = 0; // timestamp — suppress activity until this time
  let isAutoResume = $state(false);
  // Sync from props so external changes (e.g. triggers) update the local flag
  $effect(() => {
    isAutoResume = (autoResumeEnabled ?? true) && !!(autoResumeSshCommand || autoResumeCwd || autoResumeCommand);
  });
  let resizePtyTimeout: ReturnType<typeof setTimeout> | undefined;
  let lastFrameAlternateScreen = false;
  // Scrollback scrollbar state
  let scrollDisplayOffset = $state(0);
  let scrollTotalLines = $state(0);
  let scrollViewportRows = $state(0);
  // Tracks user's intentional scroll position — prevents TUI redraws from snapping back to bottom
  let userScrollOffset = 0;
  let scrollbarDragging = false;
  let scrollbarFadeTimeout: ReturnType<typeof setTimeout> | undefined;
  let scrollbarVisible = $state(false);
  // Inline prompt for auto-resume command
  let autoResumePrompt = $state<{ cwd: string | null; sshCmd: string | null; remoteCwd: string | null; pinned: boolean } | null>(null);
  let autoResumePromptValue = $state('');
  let claudeSetupModal = $state(false);
  let autoResumeTextarea = $state<{ focus: () => void } | undefined>();
  let autoResumeHeightBeforeMouse = 0;
  let sessionIdCopied = $state(false);

  // --- Selection state (Rust-managed via alacritty_terminal) ---
  let selectionActive = false; // mouse is down and dragging
  let hasRustSelection = false; // Rust has an active selection
  let selectionClickCount = 0;
  let selectionClickTimer: ReturnType<typeof setTimeout> | undefined;
  let autoScrollInterval: ReturnType<typeof setInterval> | undefined;
  let lastMouseCol = 0;

  function getCellPosition(e: MouseEvent): { col: number; row: number; side: 'left' | 'right' } {
    // Use the xterm-screen element (not containerRef) to avoid padding offset
    const screenEl = containerRef.querySelector('.xterm-screen') as HTMLElement;
    const rect = screenEl ? screenEl.getBoundingClientRect() : containerRef.getBoundingClientRect();
    const x = e.clientX - rect.left;
    const y = e.clientY - rect.top;
    const cellWidth = rect.width / terminal.cols;
    const cellHeight = rect.height / terminal.rows;
    const col = Math.min(Math.max(Math.floor(x / cellWidth), 0), terminal.cols - 1);
    const row = Math.min(Math.max(Math.floor(y / cellHeight), 0), terminal.rows - 1);
    const cellX = x - col * cellWidth;
    const side = cellX < cellWidth / 2 ? 'left' : 'right';
    return { col, row, side };
  }

  function applyFrame(frame: TerminalFrame) {
    terminal.write(new Uint8Array(frame.ansi));
    hasRustSelection = frame.has_selection;
    updateScrollbar(frame.display_offset, frame.total_lines);
  }

  function stopAutoScroll() {
    if (autoScrollInterval) {
      clearInterval(autoScrollInterval);
      autoScrollInterval = undefined;
    }
  }

  function onSelectionMouseMove(e: MouseEvent) {
    if (!selectionActive || lastFrameAlternateScreen) return;

    const screenEl = containerRef.querySelector('.xterm-screen') as HTMLElement;
    const rect = screenEl ? screenEl.getBoundingClientRect() : containerRef.getBoundingClientRect();
    const y = e.clientY - rect.top;
    const { col, row, side } = getCellPosition(e);
    lastMouseCol = col;

    // Auto-scroll when mouse is above or below viewport
    if (y < 0) {
      if (!autoScrollInterval) {
        autoScrollInterval = setInterval(() => {
          scrollSelection(ptyId, 1, lastMouseCol).then(frame => {
            userScrollOffset = frame.display_offset;
            applyFrame(frame);
          }).catch(() => {});
        }, 50);
      }
      return;
    } else if (y > rect.height) {
      if (!autoScrollInterval) {
        autoScrollInterval = setInterval(() => {
          scrollSelection(ptyId, -1, lastMouseCol).then(frame => {
            userScrollOffset = frame.display_offset;
            applyFrame(frame);
          }).catch(() => {});
        }, 50);
      }
      return;
    } else {
      stopAutoScroll();
    }

    updateSelection(ptyId, col, row, side).then(applyFrame).catch(() => {});
  }

  function onSelectionMouseUp() {
    selectionActive = false;
    stopAutoScroll();
  }

  function updateScrollbar(displayOffset: number, totalLines: number) {
    scrollDisplayOffset = displayOffset;
    scrollTotalLines = totalLines;
    scrollViewportRows = terminal?.rows ?? 0;
    if (displayOffset > 0) {
      scrollbarVisible = true;
      clearTimeout(scrollbarFadeTimeout);
      scrollbarFadeTimeout = setTimeout(() => { scrollbarVisible = false; }, 1500);
    } else {
      // At live position — hide after brief delay
      clearTimeout(scrollbarFadeTimeout);
      scrollbarFadeTimeout = setTimeout(() => { scrollbarVisible = false; }, 500);
    }
  }

  // Fit terminal with one fewer row for bottom breathing room.
  // Uses proposeDimensions() + a single resize instead of fit() + resize()
  // to avoid a double reflow.
  function fitWithPadding() {
    // Guard: skip if container is not in the document (detached during split re-render)
    if (!containerRef?.isConnected) return;
    const dims = fitAddon.proposeDimensions();
    if (!dims || isNaN(dims.cols) || isNaN(dims.rows)) return;
    const cols = dims.cols;
    const rows = Math.max(dims.rows - 1, 1);
    // Guard: skip transient layouts during portal moves where the container
    // is connected but hasn't been laid out yet, producing tiny dimensions.
    if (cols < 10 || rows < 2) return;
    if (cols === terminal.cols && rows === terminal.rows) return;
    terminal.resize(cols, rows);
  }
  let contextMenu = $state<{ x: number; y: number } | null>(null);
  let hoveredLinkUri: string | null = null;
  let contextMenuLinkUri: string | null = null;
  let isDragOver = $state(false);
  // Only cache the SSH command at drag-enter; CWD is resolved fresh at drop time
  let dragSshCommand: string | null = $state(null);

  // Escape a file path for pasting into a terminal (backslash-escape shell metacharacters)
  function escapePathForTerminal(p: string): string {
    return p.replace(/([^a-zA-Z0-9_\-.,/:@+])/g, '\\$1');
  }

  // Paste from clipboard using native Tauri APIs (bypasses WKWebView paste popup).
  // Checks for file paths first (Finder copy), then falls back to text.
  /** Convert clipboard RGBA image to PNG via offscreen canvas, return base64. PNG preserves transparency. */
  async function rgbaToPngBase64(rgba: Uint8Array, width: number, height: number): Promise<string> {
    const canvas = new OffscreenCanvas(width, height);
    const ctx = canvas.getContext('2d')!;
    ctx.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
    const blob = await canvas.convertToBlob({ type: 'image/png' });
    const buf = await blob.arrayBuffer();
    // Manual base64 encoding (no btoa needed for binary)
    const bytes = new Uint8Array(buf);
    let binary = '';
    for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
    return btoa(binary);
  }

  async function pasteFromClipboard() {
    // Check for file URLs first (Finder Cmd+C puts filename as text too,
    // but we want the full path from NSPasteboard)
    const paths = await readClipboardFilePaths();
    if (paths.length > 0) {
      const escaped = paths.map(escapePathForTerminal).join(' ');
      const bytes = Array.from(new TextEncoder().encode(escaped));
      await writeTerminal(ptyId, bytes);
      return;
    }

    // Check for image data on clipboard (screenshots) — only useful for Claude sessions
    if (claudeStateStore.getState(tabId)) {
      try {
        const image = await clipboardReadImage();
        const { width, height } = await image.size();
        if (width > 0 && height > 0) {
          const rgba = await image.rgba();
          const base64 = await rgbaToPngBase64(rgba, width, height);
          const localPath = await saveClipboardImage(base64);

          // Check if SSH session — need to SCP upload
          const info = await getPtyInfo(ptyId);
          if (info.foreground_command) {
            toastStore.addToast('Screenshot', 'Uploading screenshot…', 'info');
            await scpUploadFiles(info.foreground_command, [localPath], '/tmp/aiterm-uploads');
            const basename = localPath.split('/').pop() ?? localPath;
            const remotePath = `/tmp/aiterm-uploads/${basename}`;
            const bytes = Array.from(new TextEncoder().encode(remotePath));
            await writeTerminal(ptyId, bytes);
            toastStore.addToast('Screenshot', 'Screenshot uploaded', 'success');
          } else {
            // Local Claude session — paste local temp path
            const bytes = Array.from(new TextEncoder().encode(localPath));
            await writeTerminal(ptyId, bytes);
          }
          return;
        }
      } catch {
        // No image on clipboard or readImage not supported — fall through to text
      }
    }

    const text = await clipboardReadText();
    if (text) {
      const bytes = Array.from(new TextEncoder().encode(text));
      await writeTerminal(ptyId, bytes);
    }
  }

  // Escape a path for use inside single quotes.
  // Handles ~ by leaving it unquoted so the shell expands it.
  // Portal: attach containerRef to its slot in the split tree
  function attachToSlot() {
    const slot = document.querySelector(`[data-terminal-slot="${tabId}"]`) as HTMLElement;
    if (slot && containerRef && containerRef.parentElement !== slot) {
      slot.appendChild(containerRef);
      if (visible && initialized) {
        requestAnimationFrame(() => {
          fitWithPadding();
          const { cols, rows } = terminal;
          resizeTerminal(ptyId, cols, rows).catch(e => logError(String(e)));
        });
      }
    }
  }

  function handleSlotReady(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (detail?.tabId === tabId) {
      attachToSlot();
    }
  }

  onMount(async () => {
    // If the tab already has a running PTY (e.g. moved between workspaces),
    // reattach to it instead of spawning a new one.
    const reattaching = !!existingPtyId;
    ptyId = existingPtyId || crypto.randomUUID();

    terminal = new Terminal({
      theme: getTheme(preferencesStore.theme, preferencesStore.customThemes).terminal,
      fontFamily: `"${preferencesStore.fontFamily}", Monaco, "Courier New", monospace`,
      fontSize: preferencesStore.fontSize,
      lineHeight: 1.2,
      cursorBlink: preferencesStore.cursorBlink,
      cursorStyle: preferencesStore.cursorStyle,
      scrollback: 0, // Rust (alacritty_terminal) manages all scrollback
      allowProposedApi: true,
      linkHandler: {
        allowNonHttpProtocols: true,
        activate: (event, uri) => {
          if (event.button !== 0) return; // left click only
          if (uri.startsWith('file://')) {
            const mode = preferencesStore.fileLinkAction;
            if (mode === 'disabled') return;
            if (mode === 'modifier_click' && !event.metaKey && !event.ctrlKey) return;
            if (mode === 'alt_click' && !event.altKey) return;
            const filePath = decodeURIComponent(new URL(uri).pathname);
            openFileFromTerminal(workspaceId, paneId, tabId, filePath);
          } else {
            shellOpen(uri);
          }
        },
        hover: (_event, uri) => { hoveredLinkUri = uri; },
        leave: () => { hoveredLinkUri = null; },
      },
    });

    fitAddon = new FitAddon();
    terminal.loadAddon(fitAddon);
    terminal.loadAddon(new WebLinksAddon((_event, uri) => {
      shellOpen(uri);
    }));

    terminal.open(containerRef);

    // File path link provider: managed reactively based on preference
    // (initial registration handled by $effect below)

    // OSC events are now handled by Rust (alacritty_terminal + OscInterceptor).
    // Listeners are set up below after PTY spawn/reattach, using Tauri events.

    // Portal into the slot rendered by SplitPane
    attachToSlot();

    // Listen for slot re-creation (after split tree changes)
    window.addEventListener('terminal-slot-ready', handleSlotReady);

    // Scrollback restore is deferred until after PTY spawn — Rust's
    // restoreTerminalScrollback needs the terminal handle to exist first.
    // The initialScrollback value is held and restored below.

    // Wait for container to have dimensions
    await new Promise(resolve => requestAnimationFrame(resolve));
    await new Promise(resolve => setTimeout(resolve, 100)); // Extra delay for layout
    fitWithPadding();

    let { cols, rows } = terminal;

    // Ensure minimum dimensions
    if (cols < 1) cols = 80;
    if (rows < 1) rows = 24;

    // Suppress trigger actions during the restore/auto-resume window.
    // Variables are still extracted so state is correct, but notifications
    // and commands won't fire from old scrollback or Claude redraw output.
    suppressTab(tabId);

    // Listen for rendered frames from Rust (alacritty_terminal renders viewport as ANSI bytes).
    // When user is scrolled back, new PTY data causes alacritty to snap display_offset to 0.
    // We hold the user's scroll position by re-requesting the frame at their offset.
    unlistenOutput = await listen<TerminalFrame>(`term-frame-${ptyId}`, (event) => {
      const frame = event.payload;
      lastFrameAlternateScreen = frame.alternate_screen;
      scrollTotalLines = frame.total_lines;
      scrollViewportRows = terminal.rows;

      // Alternate screen (TUI apps like Claude/vim) has no scrollback — clear hold
      if (frame.alternate_screen) {
        userScrollOffset = 0;
      } else if (userScrollOffset > 0 && frame.display_offset === 0) {
        // User is scrolled back but alacritty snapped to bottom — re-request at their offset.
        // Still update total_lines so scrollbar reflects new content.
        scrollTerminalTo(ptyId, userScrollOffset).then(held => {
          userScrollOffset = held.display_offset;
          scrollDisplayOffset = held.display_offset;
          scrollTotalLines = held.total_lines;
          terminal.write(new Uint8Array(held.ansi));
        }).catch(() => {});
        return;
      }

      scrollDisplayOffset = frame.display_offset;
      if (frame.display_offset === 0) userScrollOffset = 0;
      hasRustSelection = frame.has_selection;
      terminal.write(new Uint8Array(frame.ansi));
    });

    // Raw PTY bytes for trigger engine + activity tracking
    unlistenRaw = await listen<number[]>(`pty-raw-${ptyId}`, (event) => {
      const data = new Uint8Array(event.payload);
      processOutput(tabId, data);
      terminalsStore.markDirty(tabId);
      // Mark tab as active for background tabs, but skip:
      // - tiny writes (spinner frames, cursor blinks)
      // - TUI redraws (cursor-up/reposition sequences that just repaint existing content)
      if (!visible && trackActivity && data.length > 64 && Date.now() > visibilityGraceUntil) {
        const text = new TextDecoder().decode(data);
        const isRedraw = /\x1b\[\d*[AHf]|\x1b\[\d+;\d+[Hf]|\x1b\[\d*J/.test(text);
        if (!isRedraw) {
          activityStore.markActive(tabId);
        }
      }
    });

    // OSC event listeners from Rust
    let lastPersistedTitle = '';
    let commandStartedAt = 0;
    const MIN_COMPLETION_MS = 2000;

    unlistenTitle = await listen<string>(`term-title-${ptyId}`, (event) => {
      const title = event.payload;
      if (!title) return;
      terminalsStore.updateOsc(tabId, { title });
      if (title !== lastPersistedTitle) {
        lastPersistedTitle = title;
        const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
        const tab = ws?.panes.find(p => p.id === paneId)?.tabs.find(t => t.id === tabId);
        if (tab && !tab.custom_name) {
          workspacesStore.renameTab(workspaceId, paneId, tabId, title, false);
        }
      }
      // Title changes when SSH starts or exits — manage bridge accordingly.
      // Filter out non-interactive SSH (git, scp, rsync) which use SSH internally
      // but don't provide a remote shell to bridge into, and one-shot remote
      // commands (`ssh host 'cmd'`) which exit before the tunnel is ready —
      // their env-var injection would land in the local shell.
      if (preferencesStore.claudeCodeIde && preferencesStore.claudeCodeIdeSsh) {
        getPtyInfo(ptyId).then(info => {
          const cmd = info.foreground_command;
          const isInteractiveSsh = cmd
            && !cmd.includes('git@')
            && !cmd.includes('BatchMode=yes')
            && isInteractiveSshSession(cmd);
          if (isInteractiveSsh && !hasBridge(tabId)) {
            enableBridge(tabId, cmd, ptyId).catch(() => {});
          } else if (!cmd && hasBridge(tabId)) {
            disableBridge(tabId).catch(() => {});
          }
        }).catch(() => {});
      }
    });

    unlistenCwd = await listen<OscCwdEvent>(`term-osc7-${ptyId}`, (event) => {
      const { cwd, host } = event.payload;
      if (cwd) terminalsStore.updateOsc(tabId, { cwd, cwdHost: host });
    });

    unlistenShell = await listen<OscShellEvent>(`term-osc133-${ptyId}`, (event) => {
      if (!trackActivity) return;
      const { cmd, exit_code } = event.payload;
      if (cmd === 'A') {
        activityStore.setShellState(tabId, 'prompt');
        // Remote shells also emit OSC 133 A, so verify SSH is actually gone
        // before clearing Claude state or tearing down the bridge.
        if (claudeStateStore.getState(tabId) || hasBridge(tabId)) {
          getPtyInfo(ptyId).then(info => {
            if (!info.foreground_command) {
              // Local shell prompt — Claude/SSH session truly ended
              if (claudeStateStore.getState(tabId)) {
                claudeStateStore.clearSession(tabId);
              }
              if (hasBridge(tabId)) {
                disableBridge(tabId).catch(() => {});
              }
            }
          }).catch(() => {});
        }
      } else if (cmd === 'D') {
        const elapsed = commandStartedAt ? Date.now() - commandStartedAt : 0;
        if (elapsed >= MIN_COMPLETION_MS) {
          activityStore.setShellState(tabId, 'completed', exit_code ?? 0);
        }
      }
      if (cmd === 'B' || cmd === 'C') {
        commandStartedAt = Date.now();
        activityStore.setShellState(tabId, null);
      }
    });

    unlistenNotification = await listen<string>(`term-notification-${ptyId}`, (event) => {
      if (!trackActivity || !event.payload) return;
      const oscState = terminalsStore.getOsc(tabId);
      const title = oscState?.title || 'Terminal';
      dispatch(title, event.payload, 'info');
    });

    unlistenClipboard = await listen<string>(`term-clipboard-${ptyId}`, (event) => {
      if (!trackActivity) return;
      clipboardWriteText(event.payload).catch(e => logError(String(e)));
    });

    unlistenBell = await listen(`term-bell-${ptyId}`, () => {
      if (!trackActivity) return;
      playBellSound().catch(() => {});
    });

    // Listen for PTY close — when the shell exits (exit/logout/Ctrl+D),
    // close the tab using the same logic as Cmd+W.
    unlistenClose = await listen(`pty-close-${ptyId}`, () => {
      if (destroyed || terminalsStore.shuttingDown) return;
      // Don't delete tabs when workspace is being suspended — PTYs are
      // killed intentionally and tabs must survive for resume.
      if (workspacesStore.isWorkspaceSuspending(workspaceId)) return;
      // Don't delete tabs when being intentionally suspended via the tab's
      // suspend button — the tab must remain visible for later resume.
      if (workspacesStore.isTabSuspending(tabId)) return;

      const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
      const pane = ws?.panes.find(p => p.id === paneId);
      if (!ws || !pane) return;

      if (pane.tabs.length > 1) {
        workspacesStore.deleteTab(workspaceId, paneId, tabId).catch(() => {});
      } else if (ws.panes.length > 1) {
        workspacesStore.deletePane(workspaceId, paneId).catch(() => {});
      } else {
        // Last tab in last pane — delete tab, pane shows empty state
        workspacesStore.deleteTab(workspaceId, paneId, tabId).catch(() => {});
      }
    });

    // Check for split context (cwd, SSH command from source pane)
    // Fall back to auto-resume context, then persisted restore context from last session.
    // Auto-resume context always wins over restore context (survives SSH disconnects).
    const splitCtx = terminalsStore.consumeSplitContext(tabId);
    const autoResumeCtx = ((autoResumeEnabled ?? true) && (autoResumeSshCommand || autoResumeCwd))
      ? { cwd: autoResumeCwd ?? restoreCwd ?? null, sshCommand: autoResumeSshCommand ?? null, remoteCwd: autoResumeRemoteCwd ?? null }
      : null;
    const restoreCtx = (restoreCwd || restoreSshCommand)
      ? { cwd: restoreCwd ?? null, sshCommand: restoreSshCommand ? cleanSshCommand(restoreSshCommand) : null, remoteCwd: restoreRemoteCwd ?? null }
      : null;
    const ctx = splitCtx ?? autoResumeCtx ?? restoreCtx;

    // Spawn PTY (or skip if reattaching to an existing one)
    if (!reattaching) {
      try {
        await spawnTerminal(ptyId, tabId, cols, rows, ctx?.cwd);
      } catch (e) {
        logError(`Failed to spawn PTY: ${e}`);
      }
      await workspacesStore.setTabPtyId(workspaceId, paneId, tabId, ptyId);
    } else {
      // Reattaching: force a PTY resize after container is laid out so TUI apps
      // redraw at the correct terminal dimensions.
      // TODO: TUI apps (Claude Code/Ink) may still render at wrong size after
      // a tab move — the resize signal doesn't always trigger a full redraw.
      setTimeout(() => {
        if (destroyed) return;
        fitWithPadding();
        resizeTerminal(ptyId, terminal.cols, terminal.rows).catch(e => logError(String(e)));
      }, 300);
    }

    // If the source pane was running SSH (or last session had SSH), replay the command.
    // SSH command sent immediately; auto-resume deferred until after bridge setup so
    // AITERM_TAB_ID env var is available in the remote shell when Claude starts.
    // Skip all of this when reattaching to an existing PTY (e.g. tab moved between workspaces).
    if (!reattaching) {
      if (ctx?.sshCommand) {
        // Send SSH command first — small delay for local shell to initialize
        setTimeout(async () => {
          try {
            const cmd = buildSshCommand(ctx.sshCommand, ctx.remoteCwd);
            const bytes = Array.from(new TextEncoder().encode(cmd + '\n'));
            await writeTerminal(ptyId, bytes);
          } catch (e) {
            logError(`Failed to replay SSH command: ${e}`);
          }
        }, 500);

        // Poll for SSH connection, then enable bridge + auto-resume.
        // getPtyInfo shows the SSH process as foreground_command once connected.
        if (ctx.sshCommand) {
          const pollForSsh = async () => {
            const maxAttempts = 30; // 15s max
            for (let i = 0; i < maxAttempts; i++) {
              if (destroyed) return;
              await new Promise(r => setTimeout(r, 500));
              try {
                const info = await getPtyInfo(ptyId);
                if (info.foreground_command) break;
              } catch { return; } // tab gone
              if (i === maxAttempts - 1) return; // timed out
            }
            if (destroyed) return;
            await enableBridge(tabId, ctx.sshCommand!, ptyId).catch(() => {});
            if (destroyed) return;
            if ((autoResumeEnabled ?? true) && autoResumeCommand) {
              try {
                const bytes = Array.from(new TextEncoder().encode(interpolateVariables(tabId, autoResumeCommand, true) + '\n'));
                await writeTerminal(ptyId, bytes);
              } catch (e) {
                logError(`Failed to send auto-resume after bridge: ${e}`);
              }
            }
          };
          pollForSsh();
        }
      } else if ((autoResumeEnabled ?? true) && autoResumeCommand && (!splitCtx || splitCtx.fireAutoResume)) {
        // Local auto-resume: send command after shell starts (also fires on reload)
        setTimeout(async () => {
          try {
            const bytes = Array.from(new TextEncoder().encode(interpolateVariables(tabId, autoResumeCommand, true) + '\n'));
            await writeTerminal(ptyId, bytes);
          } catch (e) {
            logError(`Failed to replay auto-resume command: ${e}`);
          }
        }, 500);
      }
    }

    // Load persisted trigger variables into runtime map
    if (triggerVariables) loadTabVariables(tabId, triggerVariables);

    // Register terminal instance
    terminalsStore.register(tabId, terminal, ptyId, workspaceId, paneId);

    // Restore scrollback from SQLite directly in Rust (never passes through WebView).
    // Must happen after spawn so the terminal handle exists in Rust.
    if (!reattaching) {
      try {
        const hasScrollback = await hasSavedScrollback(tabId);
        if (hasScrollback) {
          await restoreTerminalFromSaved(ptyId, tabId);
        }
      } catch (e) {
        logError(`Failed to restore scrollback: ${e}`);
      }
    }

    // Cmd+C: copy if selection, SIGINT if not. Cmd+V: paste into PTY.
    terminal.attachCustomKeyEventHandler((e: KeyboardEvent) => {
      if (e.type !== 'keydown') return true;

      if (isModKey(e) && e.key === 'c') {
        e.preventDefault();
        if (hasRustSelection) {
          copySelection(ptyId).then(text => {
            if (text) clipboardWriteText(text).catch(e => logError(String(e)));
            clearSelection(ptyId).then(applyFrame).catch(() => {});
          }).catch(e => logError(String(e)));
        } else {
          writeTerminal(ptyId, [0x03]).catch(e => logError(String(e)));
        }
        return false;
      }

      if (isModKey(e) && e.key === 'a' && !lastFrameAlternateScreen) {
        e.preventDefault();
        selectAll(ptyId).then(applyFrame).catch(() => {});
        return false;
      }

      if (isModKey(e) && e.key === 'v') {
        e.preventDefault();
        pasteFromClipboard().catch(e => logError(String(e)));
        return false;
      }

      if (isModKey(e) && e.altKey && e.key === 'r') {
        e.preventDefault();
        if (isAutoResume) {
          replayAutoResume(tabId);
        }
        return false;
      }

      if (isModKey(e) && !e.altKey && e.key === 'r') {
        e.preventDefault();
        if (isAutoResume) {
          workspacesStore.disableAutoResume(workspaceId, paneId, tabId);
        } else {
          gatherAutoResumeContext().then(ctx => {
            autoResumePromptValue = autoResumeRememberedCommand ?? '';
            autoResumePrompt = ctx;
          }).catch(e => logError(`Auto-resume failed: ${e}`));
        }
        return false;
      }

      // Keyboard scrollback navigation (non-alternate screen only)
      if (!lastFrameAlternateScreen) {
        if (e.key === 'PageUp') {
          e.preventDefault();
          scrollTerminal(ptyId, terminal.rows).then(frame => {
            userScrollOffset = frame.display_offset;
            terminal.write(new Uint8Array(frame.ansi));
            updateScrollbar(frame.display_offset, frame.total_lines);
          }).catch(() => {});
          return false;
        }
        if (e.key === 'PageDown') {
          e.preventDefault();
          scrollTerminal(ptyId, -terminal.rows).then(frame => {
            userScrollOffset = frame.display_offset;
            terminal.write(new Uint8Array(frame.ansi));
            updateScrollbar(frame.display_offset, frame.total_lines);
          }).catch(() => {});
          return false;
        }
        if (e.shiftKey && e.key === 'ArrowUp') {
          e.preventDefault();
          scrollTerminal(ptyId, 1).then(frame => {
            userScrollOffset = frame.display_offset;
            terminal.write(new Uint8Array(frame.ansi));
            updateScrollbar(frame.display_offset, frame.total_lines);
          }).catch(() => {});
          return false;
        }
        if (e.shiftKey && e.key === 'ArrowDown') {
          e.preventDefault();
          scrollTerminal(ptyId, -1).then(frame => {
            userScrollOffset = frame.display_offset;
            terminal.write(new Uint8Array(frame.ansi));
            updateScrollbar(frame.display_offset, frame.total_lines);
          }).catch(() => {});
          return false;
        }
      }

      return true;
    });

    // Handle keyboard input — clear selection on any input
    terminal.onData(async (data) => {
      if (hasRustSelection) {
        clearSelection(ptyId).then(applyFrame).catch(() => {});
      }
      const bytes = Array.from(new TextEncoder().encode(data));
      try {
        await writeTerminal(ptyId, bytes);
      } catch (e) {
        logError(`Failed to write to PTY: ${e}`);
      }
    });

    // Handle resize — fit immediately for visual update,
    // debounce PTY resize to avoid rapid-fire SIGWINCH during window drag.
    resizeObserver = new ResizeObserver(() => {
      if (!visible || !containerRef?.isConnected) return;
      fitWithPadding();
      clearTimeout(resizePtyTimeout);
      resizePtyTimeout = setTimeout(() => {
        const { cols, rows } = terminal;
        resizeTerminal(ptyId, cols, rows).catch(e => logError(String(e)));
      }, 150);
    });
    resizeObserver.observe(containerRef);

    // Intercept mouse wheel for Rust-managed scrollback navigation.
    // In alternate screen mode (TUI apps), let xterm.js handle scrolling
    // (sends arrow keys to the app, which is the expected behavior).
    // Uses velocity-sensitive scrolling: small movements = 1 line, fast flicks = many lines.
    let scrollAccumulator = 0;
    containerRef.addEventListener('wheel', (e) => {
      if (lastFrameAlternateScreen) return;

      e.preventDefault();
      e.stopPropagation();

      // Normalize delta to lines based on deltaMode
      let delta: number;
      if (e.deltaMode === 1) {
        // Line mode (mouse wheel) — use directly
        delta = -e.deltaY;
      } else {
        // Pixel mode (trackpad) — convert to lines, ~20px per line
        delta = -e.deltaY / 20;
      }

      // Accumulate sub-line amounts for smooth trackpad scrolling
      scrollAccumulator += delta;
      const lines = Math.trunc(scrollAccumulator);
      if (lines === 0) return;
      scrollAccumulator -= lines;

      scrollTerminal(ptyId, lines).then((frame) => {
        userScrollOffset = frame.display_offset;
        terminal.write(new Uint8Array(frame.ansi));
        updateScrollbar(frame.display_offset, frame.total_lines);
      }).catch(() => { /* terminal may have been killed */ });
    }, { passive: false, capture: true });

    // --- Selection mouse handlers (Rust-managed) ---
    // Capture phase + stopPropagation prevents xterm.js from handling selection.
    // Only intercept plain left-clicks for selection — let everything else
    // (right-click, Cmd+click for links, alt-screen) pass through to xterm.js.
    containerRef.addEventListener('mousedown', (e) => {
      // Let xterm.js handle non-selection clicks normally
      if (e.button !== 0 || lastFrameAlternateScreen) return;
      if ((e.target as HTMLElement)?.closest('.scrollbar-track, .auto-resume-prompt, .context-menu')) return;
      if (e.metaKey || e.ctrlKey || e.altKey) return;

      // Block xterm.js from receiving this mousedown (prevents its selection)
      e.stopPropagation();

      const { col, row, side } = getCellPosition(e);
      lastMouseCol = col;

      // Track click count for double/triple click
      selectionClickCount++;
      clearTimeout(selectionClickTimer);
      selectionClickTimer = setTimeout(() => { selectionClickCount = 0; }, 400);

      if (e.shiftKey && hasRustSelection) {
        updateSelection(ptyId, col, row, side).then(applyFrame).catch(() => {});
      } else {
        const selType = selectionClickCount >= 3 ? 'lines'
          : selectionClickCount === 2 ? 'semantic'
          : 'simple';

        startSelection(ptyId, col, row, side, selType).then(applyFrame).catch(() => {});
        selectionActive = selType === 'simple';
      }

      // Restore focus after stopPropagation blocked xterm.js's mousedown handler.
      // Use requestAnimationFrame so it runs after the browser's default behavior.
      requestAnimationFrame(() => terminal.focus());
    }, { capture: true });

    window.addEventListener('mousemove', onSelectionMouseMove);
    window.addEventListener('mouseup', onSelectionMouseUp);

    // Drag & drop file support: window-scoped via getCurrentWebview() to prevent cross-window firing
    unlistenDragDrop = await getCurrentWebview().onDragDropEvent((event) => {
      const { type } = event.payload;

      if (type === 'over') {
        if (!visible || !containerRef?.isConnected) { isDragOver = false; return; }
        const { position } = event.payload;
        const rect = containerRef.getBoundingClientRect();
        const over = (
          position.x >= rect.left && position.x <= rect.right &&
          position.y >= rect.top && position.y <= rect.bottom
        );
        // On first enter, detect SSH session — cache only the SSH command
        if (over && !isDragOver) {
          getPtyInfo(ptyId).then(info => {
            logInfo(`drag-enter: foreground_command=${info.foreground_command}, cwd=${info.cwd}`);
            dragSshCommand = info.foreground_command ?? null;
          }).catch((e) => { logError(`drag-enter getPtyInfo failed: ${e}`); dragSshCommand = null; });
        }
        isDragOver = over;
      } else if (type === 'drop') {
        const sshCommand = dragSshCommand;
        isDragOver = false;
        dragSshCommand = null;
        if (!visible || !containerRef?.isConnected) return;
        const { paths, position } = event.payload;
        const rect = containerRef.getBoundingClientRect();
        if (
          position.x >= rect.left && position.x <= rect.right &&
          position.y >= rect.top && position.y <= rect.bottom
        ) {
          if (sshCommand) {
            // SSH session — resolve remote CWD fresh at drop time
            const isClaudeSession = !!claudeStateStore.getState(tabId);
            let remoteCwd = '~';
            if (!isClaudeSession) {
              const oscState = terminalsStore.getOsc(tabId);
              const osc7Cwd = oscState?.cwd ?? null;
              const promptCwd = oscState?.promptCwd ?? null;
              remoteCwd = (osc7Cwd ?? promptCwd ?? '~').trim();
              logInfo(`drag-drop: remoteCwd=${remoteCwd} (osc7=${osc7Cwd}, prompt=${promptCwd})`);
            }
            const remoteDir = isClaudeSession ? '/tmp/aiterm-uploads' : remoteCwd;
            const count = paths.length;
            logInfo(`drag-drop SSH: uploading ${count} file(s) to ${remoteDir} via ${sshCommand} (claude=${isClaudeSession})`);
            logInfo(`drag-drop SSH: paths=${JSON.stringify(paths)}`);
            toastStore.addToast('SCP Upload', `Uploading ${count} file${count > 1 ? 's' : ''}…`, 'info');
            scpUploadFiles(sshCommand, paths, remoteDir).then(async () => {
              const basenames = paths.map(p => p.split('/').pop() ?? p);
              if (isClaudeSession) {
                // Write each path separately so Claude Code detects each as a file reference
                for (let i = 0; i < basenames.length; i++) {
                  const path = `/tmp/aiterm-uploads/${basenames[i]}`;
                  const bytes = Array.from(new TextEncoder().encode(path + ' '));
                  if (i > 0) await new Promise(r => setTimeout(r, 200));
                  await writeTerminal(ptyId, bytes);
                }
                toastStore.addToast('SCP Upload', `${count} file${count > 1 ? 's' : ''} uploaded`, 'success');
              } else {
                // Non-Claude: clickable toast to list uploaded files (no echo to prompt)
                const lCmd = `l ${basenames.map(escapePathForTerminal).join(' ')}\n`;
                toastStore.addToast(
                  'SCP Upload',
                  `${count} file${count > 1 ? 's' : ''} uploaded — click to list`,
                  'success',
                  undefined,
                  undefined,
                  () => {
                    const bytes = Array.from(new TextEncoder().encode(lCmd));
                    writeTerminal(ptyId, bytes).catch(e => logError(String(e)));
                  },
                );
              }
            }).catch(e => {
              logError(`drag-drop SCP upload failed: ${e}`);
              toastStore.addToast('SCP Upload Failed', String(e), 'error');
            });
          } else if (claudeStateStore.getState(tabId)) {
            // Local Claude session — write absolute paths so Claude can reference files
            const count = paths.length;
            logInfo(`drag-drop local Claude: sending ${count} file path(s)`);
            (async () => {
              for (let i = 0; i < paths.length; i++) {
                const bytes = Array.from(new TextEncoder().encode(paths[i] + ' '));
                if (i > 0) await new Promise(r => setTimeout(r, 200));
                await writeTerminal(ptyId, bytes);
              }
            })().catch(e => logError(`drag-drop local Claude write failed: ${e}`));
          } else {
            // Local session — paste escaped file paths
            const escaped = paths.map(escapePathForTerminal).join(' ');
            const bytes = Array.from(new TextEncoder().encode(escaped));
            writeTerminal(ptyId, bytes).catch(e => logError(String(e)));
          }
          terminal.focus();
        }
      } else if (type === 'leave') {
        isDragOver = false;
        dragSshCommand = null;
      }
    });

    initialized = true;
    terminal.focus();
    // Delay activity tracking and trigger actions so initial shell prompt
    // and restored/auto-resumed output don't fire indicators or triggers.
    // Auto-resume (especially SSH + Claude) can take much longer to produce
    // output, so use a longer suppression window.
    const suppressMs = autoResumeCommand ? 15000 : 2000;
    setTimeout(() => { trackActivity = true; unsuppressTab(tabId); }, suppressMs);
  });

  onDestroy(() => {
    destroyed = true;

    // Detach SSH MCP bridge (fire-and-forget, non-blocking)
    disableBridge(tabId).catch(() => {});

    window.removeEventListener('terminal-slot-ready', handleSlotReady);
    window.removeEventListener('mousemove', onSelectionMouseMove);
    window.removeEventListener('mouseup', onSelectionMouseUp);
    stopAutoScroll();

    if (unlistenOutput) unlistenOutput();
    if (unlistenRaw) unlistenRaw();
    if (unlistenClose) unlistenClose();
    if (unlistenTitle) unlistenTitle();
    if (unlistenCwd) unlistenCwd();
    if (unlistenShell) unlistenShell();
    if (unlistenNotification) unlistenNotification();
    if (unlistenClipboard) unlistenClipboard();
    if (unlistenBell) unlistenBell();
    if (unlistenDragDrop) unlistenDragDrop();
    clearTimeout(resizePtyTimeout);
    if (resizeObserver) resizeObserver.disconnect();
    if (filePathLinkDisposable) filePathLinkDisposable.dispose();
    if (ptyId && terminalsStore.consumePreserve(ptyId)) {
      // PTY is being preserved (e.g. tab moving between workspaces).
      // Don't kill the PTY — the new TerminalPane will reattach.
      if (terminal) terminal.dispose();
    } else {
      // Save scrollback from Rust before killing the PTY.
      // Fire-and-forget: onDestroy is sync, but the save must complete before
      // the kill. Chain them so kill waits for serialize to finish.
      if (ptyId) {
        saveTerminalScrollback(ptyId, tabId)
          .catch(() => {})
          .finally(() => {
            killTerminal(ptyId).catch(e => logError(String(e)));
          });
      }
      if (terminal) terminal.dispose();
      terminalsStore.unregister(tabId);
      cleanupTab(tabId);
    }
  });

  // Suppress false activity when terminal transitions to hidden —
  // residual output (SSH restore, prompt redraws) can arrive briefly after switch.
  $effect(() => {
    if (!visible && initialized) {
      visibilityGraceUntil = Date.now() + 1000;
      // Explicitly blur so hidden terminals don't retain keyboard focus.
      // Without this, keyboard shortcuts (Cmd+R, etc.) can fire on the wrong tab.
      terminal?.blur();
    }
  });

  $effect(() => {
    if (visible && initialized && fitAddon) {
      // Delay fit to ensure container is visible
      requestAnimationFrame(() => {
        fitWithPadding();
        // Always sync PTY dimensions when becoming visible — the PTY may have been
        // writing at a different size while the terminal was in the background
        // (e.g. auto-resume reconnecting to Claude Code at default 80x24).
        const { cols, rows } = terminal;
        resizeTerminal(ptyId, cols, rows).catch(e => logError(String(e)));
        if (!autoResumePrompt) terminal.focus();
      });
      untrack(() => {
        activityStore.clearActive(tabId);
        activityStore.clearShellState(tabId);
        activityStore.clearTabState(tabId);
      });
    }
  });

  // Mark a finished Claude result as "read" once its tab is the visible one —
  // whether the user switched to it, or it finished while already in view.
  $effect(() => {
    const cs = claudeStateStore.getState(tabId);
    if (visible && cs?.state === 'idle' && !cs.read) {
      untrack(() => claudeStateStore.markRead(tabId));
    }
  });

  // Canvas (2D) renderer: load when visible, dispose when hidden. Falls back to
  // xterm's built-in DOM renderer if the addon throws. We previously used the
  // WebGL renderer, but its backbuffer is alpha-blended (alpha:true,
  // premultipliedAlpha:true) even though the terminal is opaque — so redrawn
  // cells composited over the previous frame instead of opaquely replacing it,
  // leaving ghost glyphs on animated/styled text (Claude Code spinners, diffs).
  // The canvas renderer clears each cell opaquely before drawing, so it can't
  // ghost; aiTerm renders only one bounded viewport (scrollback:0), so WebGL's
  // scroll-perf advantage didn't apply here anyway.
  $effect(() => {
    if (!initialized || !terminal) return;
    if (visible) {
      if (!canvasAddon) {
        try {
          canvasAddon = new CanvasAddon();
          terminal.loadAddon(canvasAddon);
          terminalsStore.canvasRendererLoaded(tabId);
        } catch {
          canvasAddon = null;
        }
      }
    } else {
      if (canvasAddon) {
        canvasAddon.dispose();
        canvasAddon = null;
        terminalsStore.canvasRendererUnloaded(tabId);
      }
    }
  });

  // React to preference changes for existing terminals
  $effect(() => {
    if (!initialized || !terminal) return;

    const fontSize = preferencesStore.fontSize;
    const fontFamily = preferencesStore.fontFamily;
    const cursorBlink = preferencesStore.cursorBlink;
    const cursorStyle = preferencesStore.cursorStyle;
    const themeId = preferencesStore.theme;

    terminal.options.fontSize = fontSize;
    terminal.options.fontFamily = `"${fontFamily}", Monaco, "Courier New", monospace`;
    terminal.options.cursorBlink = cursorBlink;
    terminal.options.cursorStyle = cursorStyle;
    terminal.options.theme = getTheme(themeId, preferencesStore.customThemes).terminal;

    // Re-fit after font changes
    requestAnimationFrame(() => {
      if (fitAddon && visible) {
        fitWithPadding();
        const { cols, rows } = terminal;
        resizeTerminal(ptyId, cols, rows).catch(e => logError(String(e)));
      }
    });
  });

  // React to file link preference changes — register/dispose provider
  $effect(() => {
    if (!initialized || !terminal) return;
    const mode = preferencesStore.fileLinkAction;
    filePathLinkDisposable?.dispose();
    filePathLinkDisposable = null;
    if (mode !== 'disabled') {
      filePathLinkDisposable = createFilePathLinkProvider(terminal, (path, event) => {
        if (mode === 'modifier_click' && !event.metaKey && !event.ctrlKey) return;
        if (mode === 'alt_click' && !event.altKey) return;
        openFileFromTerminal(workspaceId, paneId, tabId, path);
      });
    }
    return () => {
      filePathLinkDisposable?.dispose();
      filePathLinkDisposable = null;
    };
  });

  // React to auto-save interval changes
  $effect(() => {
    if (!initialized) return;

    const interval = preferencesStore.autoSaveInterval;

    // Set up new interval if enabled.
    // Stagger start by a random offset (0–interval) so 80+ terminals don't
    // all serialize in the same tick, which creates massive GC pressure.
    let localInterval: ReturnType<typeof setInterval> | undefined;
    let staggerTimeout: ReturnType<typeof setTimeout> | undefined;
    if (interval > 0) {
      // Stagger start by a random offset so 80+ terminals don't all
      // serialize in the same tick, creating massive GC pressure bursts.
      const staggerMs = Math.random() * interval * 1000;
      staggerTimeout = setTimeout(() => {
        localInterval = setInterval(async () => {
        // Skip auto-save during shutdown — saveAllScrollback handles it
        if (terminalsStore.shuttingDown) return;
        // Skip terminals that haven't received output since last save.
        if (!terminalsStore.isDirty(tabId)) return;
        terminalsStore.clearDirty(tabId);
        try {
          await saveTerminalScrollback(ptyId, tabId);
        } catch {
          // Terminal may have been killed or alternate screen active — ignore
        }

        // Also save restore context (cwd/SSH) if enabled
        if (preferencesStore.restoreSession) {
          try {
            const info = await getPtyInfo(ptyId);
            let cwd = info.cwd;
            const sshCommand = info.foreground_command;
            let remoteCwd: string | null = null;

            const oscState = terminalsStore.getOsc(tabId);
            const osc7Cwd = oscState?.cwd ?? null;
            const promptCwd = oscState?.promptCwd ?? null;
            if (sshCommand) {
              const isOsc7Stale = osc7Cwd === cwd;
              const osc7RemoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : null;
              remoteCwd = osc7RemoteCwd ?? promptCwd ?? null;
              if (!remoteCwd) {
                // Last resort: scan buffer for prompt pattern
                const patterns = getCompiledPatterns(preferencesStore.promptPatterns);
                const buffer = terminal.buffer.active;
                const cursorLine = buffer.baseY + buffer.cursorY;
                for (let i = cursorLine; i >= Math.max(0, cursorLine - 5); i--) {
                  const line = buffer.getLine(i);
                  if (!line) continue;
                  const text = line.translateToString(true).trim();
                  if (!text) continue;
                  for (const re of patterns) {
                    const match = text.match(re);
                    if (match?.[1]) { remoteCwd = match[1].trim(); break; }
                  }
                  if (remoteCwd) break;
                }
              }
            } else {
              cwd = cwd ?? osc7Cwd;
            }

            await setTabRestoreContext(workspaceId, paneId, tabId, cwd, sshCommand, remoteCwd);
          } catch {
            // PTY may be gone — ignore
          }
        }
      }, interval * 1000);
      }, staggerMs);
    }

    // Cleanup when effect re-runs or component unmounts
    return () => {
      if (staggerTimeout) clearTimeout(staggerTimeout);
      if (localInterval) clearInterval(localInterval);
    };
  });

  function getCurrentTab(): import('$lib/tauri/types').Tab | undefined {
    const ws = workspacesStore.workspaces.find(w => w.id === workspaceId);
    const pane = ws?.panes.find(p => p.id === paneId);
    return pane?.tabs.find(t => t.id === tabId);
  }

  async function gatherAutoResumeContext(): Promise<{ cwd: string | null; sshCmd: string | null; remoteCwd: string | null; pinned: boolean }> {
    // If pinned, use stored values from the live store (not stale props)
    const tab = getCurrentTab();
    if (tab?.auto_resume_pinned) {
      return {
        cwd: tab.auto_resume_cwd ?? null,
        sshCmd: tab.auto_resume_ssh_command ?? null,
        remoteCwd: tab.auto_resume_remote_cwd ?? null,
        pinned: true,
      };
    }

    const info = await getPtyInfo(ptyId);
    const sshCmd = info.foreground_command ? cleanSshCommand(info.foreground_command) : null;
    const localCwd = info.cwd ?? null;
    let remoteCwd: string | null = null;
    if (sshCmd) {
      const oscState = terminalsStore.getOsc(tabId);
      const osc7Cwd = oscState?.cwd ?? null;
      const promptCwd = oscState?.promptCwd ?? null;
      const isOsc7Stale = osc7Cwd === localCwd;
      remoteCwd = (osc7Cwd && !isOsc7Stale) ? osc7Cwd : promptCwd ?? null;
      if (!remoteCwd) {
        // Last resort: scan buffer for prompt pattern
        const patterns = getCompiledPatterns(preferencesStore.promptPatterns);
        const buffer = terminal.buffer.active;
        const cursorLine = buffer.baseY + buffer.cursorY;
        for (let i = cursorLine; i >= Math.max(0, cursorLine - 5); i--) {
          const line = buffer.getLine(i);
          if (!line) continue;
          const text = line.translateToString(true).trim();
          if (!text) continue;
          for (const re of patterns) {
            const match = text.match(re);
            if (match?.[1]) { remoteCwd = match[1].trim(); break; }
          }
          if (remoteCwd) break;
        }
      }
    }

    // Prevent context downgrade: if live detection found no SSH but the tab
    // already has stored SSH context (e.g. detection failed, SSH not running
    // yet, or re-enabling after disable), fall back to stored values.
    if (!sshCmd && tab?.auto_resume_ssh_command) {
      return {
        cwd: tab.auto_resume_cwd ?? localCwd,
        sshCmd: tab.auto_resume_ssh_command,
        remoteCwd: tab.auto_resume_remote_cwd ?? null,
        pinned: false,
      };
    }

    return { cwd: localCwd, sshCmd, remoteCwd, pinned: false };
  }

  async function submitAutoResumePrompt() {
    if (!autoResumePrompt) return;
    const cmd = autoResumePromptValue.trim() || null;
    // Normalize SSH input: strip "ssh" prefix and standard flags, store just user@host
    const sshCmd = autoResumePrompt.sshCmd?.trim() ? normalizeSshInput(autoResumePrompt.sshCmd.trim()) : null;
    const remoteCwd = sshCmd ? (autoResumePrompt.remoteCwd?.trim() || null) : null;
    const cwd = autoResumePrompt.cwd?.trim() || null;
    const pinned = autoResumePrompt.pinned;
    await workspacesStore.setTabAutoResumeContext(workspaceId, paneId, tabId, cwd, sshCmd, remoteCwd, cmd, pinned);
    isAutoResume = true;
    autoResumePrompt = null;
    autoResumePromptValue = '';
    terminal?.focus();
  }

  function cancelAutoResumePrompt() {
    autoResumePrompt = null;
    autoResumePromptValue = '';
    terminal?.focus();
  }

  // When auto-resume prompt opens: blur xterm so it stops competing, then focus the input
  $effect(() => {
    if (autoResumePrompt) {
      terminal?.blur();
      requestAnimationFrame(() => {
        autoResumeTextarea?.focus();
      });
    }
  });

  function handleContextMenu(e: MouseEvent) {
    e.preventDefault();
    contextMenuLinkUri = hoveredLinkUri;
    contextMenu = { x: e.clientX, y: e.clientY };
  }

  function getContextMenuItems() {
    // Extract full path from file:// link that was hovered when context menu opened
    const hoveredFilePath = contextMenuLinkUri?.startsWith('file://')
      ? decodeURIComponent(new URL(contextMenuLinkUri).pathname)
      : null;
    return [
      ...(hoveredFilePath ? [{
        label: 'Copy Full Path',
        action: async () => { await clipboardWriteText(hoveredFilePath); },
      }] : []),
      {
        label: 'Copy',
        shortcut: `${modSymbol}C`,
        disabled: !hasRustSelection,
        action: async () => {
          const text = await copySelection(ptyId);
          if (text) await clipboardWriteText(text);
          clearSelection(ptyId).then(applyFrame).catch(() => {});
        },
      },
      {
        label: 'Paste',
        shortcut: `${modSymbol}V`,
        action: () => pasteFromClipboard(),
      },
      {
        label: 'Select All',
        shortcut: `${modSymbol}A`,
        action: () => {
          terminal.selectAll();
        },
      },
      { label: '', separator: true, action: () => {} },
      {
        label: 'Clear',
        shortcut: `${modSymbol}K`,
        action: () => {
          terminalsStore.clearTerminal(tabId);
        },
      },
      ...(getVariables(tabId)?.size ? [
        {
          label: 'Clear Trigger Variables',
          action: () => {
            const vars = getVariables(tabId);
            if (vars?.size) {
              const entries = [...vars.entries()].map(([k, v]) => `${k}: ${v}`).join('\n');
              dispatch('Variables Cleared', entries, 'info');
            }
            clearTabVariables(tabId);
          },
        },
      ] : []),
      { label: '', separator: true, action: () => {} },
      ...(isAutoResume ? [
        {
          label: 'Replay Auto-Resume',
          action: () => replayAutoResume(tabId),
        },
        {
          label: 'Edit Auto-resume\u2026',
          action: async () => {
            try {
              const ctx = await gatherAutoResumeContext();
              autoResumePromptValue = autoResumeRememberedCommand ?? '';
              autoResumePrompt = ctx;
            } catch (e) {
              logError(`Edit auto-resume failed: ${e}`);
            }
          },
        },
        {
          label: 'Disable Auto-resume',
          action: async () => {
            await workspacesStore.disableAutoResume(workspaceId, paneId, tabId);
            isAutoResume = false;
          },
        },
      ] : [
        {
          label: 'Auto-resume\u2026',
          action: async () => {
            try {
              const ctx = await gatherAutoResumeContext();
              autoResumePromptValue = autoResumeRememberedCommand ?? '';
              autoResumePrompt = ctx;
            } catch (e) {
              logError(`Auto-resume failed: ${e}`);
            }
          },
        },
        {
          label: 'Auto-resume + Claude\u2026',
          action: () => { claudeSetupModal = true; },
        },
      ]),
      { label: '', separator: true, action: () => {} },
      {
        label: 'Suspend Other Tabs',
        action: async () => {
          const tornDown = await workspacesStore.suspendOtherTabs();
          if (tornDown.length) {
            window.dispatchEvent(new CustomEvent('deactivate-tabs', { detail: tornDown }));
          }
        },
      },
      {
        label: 'Suspend Other Workspaces',
        action: () => workspacesStore.suspendAllOtherWorkspaces(),
      },
      ...(preferencesStore.shellTitleIntegration || preferencesStore.shellIntegration ? [
        { label: '', separator: true, action: () => {} },
        {
          label: 'Setup Shell Integration',
          action: async () => {
            const snippet = buildShellIntegrationSnippet({
              shellTitle: preferencesStore.shellTitleIntegration,
              shellIntegration: preferencesStore.shellIntegration,
            });
            if (snippet) {
              const bytes = Array.from(new TextEncoder().encode(snippet + '\n'));
              await writeTerminal(ptyId, bytes);
            }
          },
        },
        {
          label: 'Install Shell Integration',
          action: async () => {
            const snippet = buildInstallSnippet();
            const bytes = Array.from(new TextEncoder().encode(snippet + '\n'));
            await writeTerminal(ptyId, bytes);
          },
        },
      ] : []),
      ...(preferencesStore.claudeCodeIde && preferencesStore.claudeCodeIdeSsh ? [
        { label: '', separator: true, action: () => {} },
        ...(hasBridge(tabId) ? [
          {
            label: 'Inject aiTerm Env Vars',
            action: async () => {
              const bridge = getBridgeInfo(tabId);
              if (bridge?.remotePort) {
                const envCmd = " export AITERM_TAB_ID=" + tabId + " AITERM_PORT=" + bridge.remotePort + "\n";
                const bytes = Array.from(new TextEncoder().encode(envCmd));
                await writeTerminal(ptyId, bytes);
              }
            },
          },
          {
            label: 'Install MCP for Current User',
            action: async () => {
              const script = await buildUserSetupScript(tabId);
              if (script) {
                const cmd = ' ' + script + '\n';
                const bytes = Array.from(new TextEncoder().encode(cmd));
                await writeTerminal(ptyId, bytes);
              }
            },
          },
          {
            label: 'Disable Remote MCP Bridge',
            action: async () => {
              await disableBridge(tabId);
            },
          },
        ] : [
          {
            label: 'Enable Remote MCP Bridge',
            action: async () => {
              try {
                const info = await getPtyInfo(ptyId);
                if (info.foreground_command) {
                  await enableBridge(tabId, info.foreground_command, ptyId);
                } else {
                  dispatch('MCP Bridge', 'No SSH session detected — connect via SSH first', 'info');
                }
              } catch (e) {
                logError(`MCP bridge failed: ${e}`);
              }
            },
          },
        ]),
      ] : []),
    ];
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="terminal-container"
  class:hidden={!visible}
  bind:this={containerRef}
  oncontextmenu={handleContextMenu}
>
  {#if isDragOver}
    <div class="drop-overlay">
      <span>{claudeStateStore.getState(tabId) ? 'Drop to send to Claude' : dragSshCommand ? 'Drop to upload via SCP' : 'Drop to paste path'}</span>
    </div>
  {/if}
  {#if contextMenu}
    <ContextMenu
      items={getContextMenuItems()}
      x={contextMenu.x}
      y={contextMenu.y}
      onclose={() => { contextMenu = null; terminal?.focus(); }}
    />
  {/if}
  {#if autoResumePrompt}
    {@const claudeSessionIdValue = getVariables(tabId)?.get('claudeSessionId')}
    <div class="auto-resume-prompt-backdrop">
    <div class="auto-resume-prompt">
      <div class="auto-resume-context-info">
        <div class="auto-resume-context-row">
          <span class="auto-resume-context-label">SSH</span>
          <input class="auto-resume-context-input" type="text" bind:value={autoResumePrompt.sshCmd}
            oninput={() => { if (autoResumePrompt) autoResumePrompt.pinned = true; }}
            onkeydown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submitAutoResumePrompt(); if (e.key === 'Escape') cancelAutoResumePrompt(); }}
            placeholder="user@host or ssh user@host" />
        </div>
        <div class="auto-resume-context-row">
          <span class="auto-resume-context-label">Remote CWD</span>
          <input class="auto-resume-context-input" type="text" bind:value={autoResumePrompt.remoteCwd}
            oninput={() => { if (autoResumePrompt) autoResumePrompt.pinned = true; }}
            onkeydown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submitAutoResumePrompt(); if (e.key === 'Escape') cancelAutoResumePrompt(); }}
            placeholder="~/path" />
        </div>
        <div class="auto-resume-context-row">
          <span class="auto-resume-context-label">CWD</span>
          <input class="auto-resume-context-input" type="text" bind:value={autoResumePrompt.cwd}
            oninput={() => { if (autoResumePrompt) autoResumePrompt.pinned = true; }}
            onkeydown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submitAutoResumePrompt(); if (e.key === 'Escape') cancelAutoResumePrompt(); }}
            placeholder="/path/to/dir" />
        </div>
        <!-- svelte-ignore a11y_label_has_associated_control -- checkbox is nested inside label -->
        <label class="auto-resume-pin-label">
          <input type="checkbox" checked={autoResumePrompt.pinned} onchange={() => { if (autoResumePrompt) autoResumePrompt.pinned = !autoResumePrompt.pinned; }} />
          Pin these settings <span class="auto-resume-pin-hint">(skip auto-detection when editing)</span>
        </label>
      </div>
      <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context for custom ResizableTextarea component -->
      <label class="auto-resume-prompt-label">Command to run after {autoResumePrompt.sshCmd ? 'connect' : 'start'}</label>
      <ResizableTextarea
        bind:this={autoResumeTextarea}
        value={autoResumePromptValue}
        placeholder="e.g. claude --continue"
        autofocus
        onchange={(v) => { autoResumePromptValue = v; }}
        onkeydown={(e) => { if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) submitAutoResumePrompt(); if (e.key === 'Escape') cancelAutoResumePrompt(); }}
      />
      <div class="auto-resume-prompt-hint">{autoResumePrompt.sshCmd ? 'Leave empty for SSH + cwd only' : 'Leave empty for cwd only'} &middot; Each line sent as a separate command &middot; {modSymbol}Enter to save</div>
      {#if claudeSessionIdValue}
        <div class="auto-resume-session-id-row">
          <span class="auto-resume-session-id-label">%claudeSessionId</span>
          <code class="auto-resume-session-id" title="Current tab's captured Claude session ID">{claudeSessionIdValue}</code>
          <button type="button" class="auto-resume-session-id-copy" title="Copy session ID" onclick={async () => {
            await clipboardWriteText(claudeSessionIdValue);
            sessionIdCopied = true;
            setTimeout(() => { sessionIdCopied = false; }, 1200);
          }}>{sessionIdCopied ? 'Copied' : 'Copy'}</button>
        </div>
      {/if}
      <div class="auto-resume-prompt-actions">
        <div class="auto-resume-presets">
          <span class="auto-resume-presets-label">Presets</span>
          <Button variant="secondary" onclick={() => { autoResumePromptValue = CLAUDE_RESUME_COMMAND; }} style="padding:6px 14px;border-radius:4px;font-size: 0.923rem;background:var(--bg-dark);border-color:var(--bg-light)" title="Uses trigger variables %claudeSessionId and %claudeResumeCommand">Claude Resume</Button>
        </div>
        <span style="flex: 1;"></span>
        <Button variant="secondary" onclick={cancelAutoResumePrompt} style="padding:6px 14px;border-radius:4px;font-size: 0.923rem">Cancel</Button>
        <Button variant="primary" onclick={submitAutoResumePrompt} style="padding:6px 14px;border-radius:4px;font-size: 0.923rem">Save</Button>
      </div>
    </div>
  </div>
  {/if}
  {#if claudeSetupModal}
    <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -- backdrop dismiss on click; keyboard handled via Escape -->
    <div class="claude-setup-backdrop" onclick={() => { claudeSetupModal = false; }} onkeydown={(e) => { if (e.key === 'Escape') claudeSetupModal = false; }}>
      <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -- modal body stops click propagation to prevent backdrop dismiss; Escape key handled on backdrop -->
      <div class="claude-setup-modal" onclick={(e) => e.stopPropagation()} onkeydown={(e) => e.stopPropagation()}>
        <h3 class="claude-setup-title">Auto-resume + Claude</h3>
        <div class="claude-setup-body">
          <p>Automatically resume your Claude Code session when the terminal restarts.</p>
          <h4>This will:</h4>
          <ul>
            <li>Enable the <strong>Claude Resume</strong> trigger &mdash; captures the <code>claude --resume</code> command when Claude exits</li>
            <li>Enable the <strong>Claude Session ID</strong> trigger &mdash; captures the session UUID from <code>/status</code></li>
            <li>Set an <strong>auto-resume command</strong> on this tab &mdash; resumes by session ID, falls back to the resume command, or starts <code>claude --continue</code></li>
          </ul>
          <h4>How it works:</h4>
          <ol>
            <li>Run Claude Code in this tab as usual</li>
            <li>When Claude exits or you run <code>/status</code>, the triggers capture the session ID</li>
            <li>If the terminal restarts (app relaunch, SSH reconnect), the auto-resume script uses the captured ID to reconnect to the same session</li>
          </ol>
          <p class="claude-setup-note">Triggers are global (configurable in Preferences &gt; Triggers) &mdash; they'll capture Claude session info in any tab. The auto-resume command is specific to this tab.</p>
        </div>
        <div class="claude-setup-actions">
          <Button variant="secondary" onclick={() => { claudeSetupModal = false; }} style="padding:6px 18px;border-radius:4px;font-size: 1rem;font-weight:500">Cancel</Button>
          <Button variant="primary" onclick={async () => {
            try {
              const ctx = await gatherAutoResumeContext();
              const sshCmd = ctx.sshCmd ? normalizeSshInput(ctx.sshCmd) : null;
              await workspacesStore.setTabAutoResumeContext(workspaceId, paneId, tabId, ctx.cwd, sshCmd, ctx.remoteCwd, CLAUDE_RESUME_COMMAND);
              isAutoResume = true;
            } catch (e) {
              logError(`Auto-resume + Claude setup failed: ${e}`);
            }
            claudeSetupModal = false;
          }} style="padding:6px 18px;border-radius:4px;font-size: 1rem;font-weight:500">Activate</Button>
        </div>
      </div>
    </div>
  {/if}
  {#if claudeStateStore.getState(tabId)?.toolName}
    {@const cs = claudeStateStore.getState(tabId)!}
    <div class="claude-action-tag">
      <span class="claude-action-dot"><Icon name="circle" size={6} /></span>
      {cs.toolName}{#if cs.toolDetail}: <span class="claude-action-detail">{cs.toolDetail}</span>{/if}
    </div>
  {/if}
  {#if scrollTotalLines > scrollViewportRows}
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div
      class="scrollbar-track"
      class:scrollbar-visible={scrollbarVisible || scrollbarDragging}
      onmousedown={(e) => {
        // Click on track → jump to position
        const rect = e.currentTarget.getBoundingClientRect();
        const fraction = (e.clientY - rect.top) / rect.height;
        const maxOffset = scrollTotalLines - scrollViewportRows;
        const targetOffset = Math.round((1 - fraction) * maxOffset);
        scrollTerminalTo(ptyId, targetOffset).then(frame => {
          userScrollOffset = frame.display_offset;
          terminal.write(new Uint8Array(frame.ansi));
          updateScrollbar(frame.display_offset, frame.total_lines);
        }).catch(() => {});
      }}
    >
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="scrollbar-thumb"
        style="height: {Math.max(20, (scrollViewportRows / scrollTotalLines) * 100)}%; top: {((scrollTotalLines - scrollViewportRows - scrollDisplayOffset) / (scrollTotalLines - scrollViewportRows)) * (100 - Math.max(20, (scrollViewportRows / scrollTotalLines) * 100))}%;"
        onmousedown={(e) => {
          e.preventDefault();
          e.stopPropagation();
          scrollbarDragging = true;
          const trackEl = e.currentTarget.parentElement!;
          const startY = e.clientY;
          const startOffset = scrollDisplayOffset;
          const maxOffset = scrollTotalLines - scrollViewportRows;

          const onMove = (me: MouseEvent) => {
            const trackRect = trackEl.getBoundingClientRect();
            const deltaFraction = (me.clientY - startY) / trackRect.height;
            const targetOffset = Math.round(startOffset - deltaFraction * maxOffset);
            const clamped = Math.max(0, Math.min(maxOffset, targetOffset));
            scrollTerminalTo(ptyId, clamped).then(frame => {
              userScrollOffset = frame.display_offset;
              terminal.write(new Uint8Array(frame.ansi));
              updateScrollbar(frame.display_offset, frame.total_lines);
            }).catch(() => {});
          };
          const onUp = () => {
            scrollbarDragging = false;
            document.removeEventListener('mousemove', onMove);
            document.removeEventListener('mouseup', onUp);
          };
          document.addEventListener('mousemove', onMove);
          document.addEventListener('mouseup', onUp);
        }}
      ></div>
    </div>
  {/if}
</div>

<style>
  .terminal-container {
    position: relative;
    flex: 1;
    padding: 4px;
    background: var(--bg-dark);
    overflow: hidden;
  }

  .drop-overlay {
    position: absolute;
    inset: 0;
    background: rgba(122, 162, 247, 0.15);
    border: 2px dashed var(--accent);
    border-radius: 4px;
    display: flex;
    align-items: center;
    justify-content: center;
    pointer-events: none;
    z-index: 10;
    backdrop-filter: blur(2px);
  }

  .drop-overlay span {
    background: var(--bg-medium);
    padding: 10px 20px;
    border-radius: 8px;
    color: var(--fg);
    font-size: 1.1rem;
    font-weight: 600;
    border: 1px solid var(--accent);
    box-shadow: 0 4px 12px rgba(0, 0, 0, 0.3);
  }

  .terminal-container.hidden {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    opacity: 0;
    pointer-events: none;
    z-index: -1;
  }

  .terminal-container :global(.xterm) {
    height: 100%;
  }

  /* Hide the default dashed underline on OSC 8 hyperlinks — only show underline on hover.
     No !important: the inline style.textDecoration xterm.js sets on hover must take precedence. */
  .terminal-container :global(.xterm-underline-5) {
    text-decoration: none;
  }

  .terminal-container :global(.xterm-viewport) {
    overflow: hidden !important;
  }

  .claude-action-tag {
    position: absolute;
    bottom: 6px;
    left: 8px;
    display: flex;
    align-items: center;
    gap: 5px;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    color: var(--fg-dim);
    font-size: 0.77rem;
    line-height: 1;
    padding: 3px 8px;
    border-radius: 4px;
    pointer-events: none;
    z-index: 4;
    max-width: 50%;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }

  .claude-action-dot {
    color: var(--accent);
    flex-shrink: 0;
    display: flex;
    align-items: center;
  }

  .claude-action-detail {
    opacity: 0.7;
  }

  .scrollbar-track {
    position: absolute;
    top: 4px;
    bottom: 4px;
    right: 2px;
    width: 8px;
    border-radius: 4px;
    opacity: 0;
    transition: opacity 0.2s ease;
    z-index: 5;
    pointer-events: auto;
  }

  .scrollbar-track.scrollbar-visible {
    opacity: 1;
  }

  .scrollbar-track:hover {
    opacity: 1;
    background: rgba(255, 255, 255, 0.05);
  }

  .scrollbar-thumb {
    position: absolute;
    width: 100%;
    min-height: 20px;
    background: rgba(255, 255, 255, 0.25);
    border-radius: 4px;
    cursor: default;
    transition: background 0.15s ease;
  }

  .scrollbar-thumb:hover,
  .scrollbar-thumb:active {
    background: rgba(255, 255, 255, 0.4);
  }

  .auto-resume-prompt-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
    pointer-events: auto;
  }

  .auto-resume-prompt {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    padding: 16px;
    min-width: 320px;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .auto-resume-prompt-label {
    color: var(--fg);
    font-size: 1rem;
    font-weight: 500;
  }

  .auto-resume-context-info {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .auto-resume-context-row {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.923rem;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    padding: 6px 10px;
  }

  .auto-resume-context-label {
    color: var(--fg-dim);
    font-size: 0.846rem;
    min-width: 85px;
    flex-shrink: 0;
  }

  .auto-resume-pin-label {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.846rem;
    color: var(--fg-dim);
    cursor: pointer;
    margin-top: 2px;
    margin-bottom: 6px;
  }

  .auto-resume-pin-label input[type="checkbox"] {
    margin: 0;
    accent-color: var(--accent);
  }

  .auto-resume-pin-hint {
    color: var(--fg-dim);
    opacity: 0.7;
  }

  .auto-resume-session-id-row {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 6px;
  }

  .auto-resume-session-id-label {
    color: var(--fg-dim);
    font-size: 0.846rem;
  }

  .auto-resume-session-id-copy {
    color: var(--fg-dim);
    font-size: 0.846rem;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    padding: 2px 8px;
    cursor: pointer;
  }

  .auto-resume-session-id-copy:hover {
    color: var(--fg);
    border-color: var(--accent);
  }

  .auto-resume-session-id {
    color: var(--fg);
    font-size: 0.846rem;
    font-family: var(--font-mono, monospace);
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    padding: 2px 6px;
    user-select: all;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .auto-resume-context-input,
  .auto-resume-context-input:focus {
    appearance: none;
    -webkit-appearance: none;
    color: var(--fg);
    color-scheme: dark;
    background: transparent;
    border: none;
    box-shadow: none;
    font-family: inherit;
    font-size: 0.923rem;
    padding: 0;
    flex: 1;
    min-width: 0;
    outline: 0;
    outline-style: none;
  }

  .auto-resume-prompt-hint {
    color: var(--fg-dim);
    font-size: 0.846rem;
  }

  .auto-resume-prompt-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 4px;
  }

  .auto-resume-presets {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .auto-resume-presets-label {
    font-size: 0.846rem;
    color: var(--fg-dim);
  }

  .claude-setup-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.5);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .claude-setup-modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    padding: 20px 24px;
    max-width: 480px;
    width: 90%;
    max-height: 80vh;
    overflow-y: auto;
  }

  .claude-setup-title {
    font-size: 1.154rem;
    font-weight: 600;
    color: var(--fg);
    margin: 0 0 12px 0;
  }

  .claude-setup-body {
    font-size: 1rem;
    color: var(--fg);
    line-height: 1.5;
  }

  .claude-setup-body p {
    margin: 0 0 10px 0;
  }

  .claude-setup-body h4 {
    font-size: 0.923rem;
    font-weight: 600;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 14px 0 6px 0;
  }

  .claude-setup-body ul,
  .claude-setup-body ol {
    margin: 0 0 10px 0;
    padding-left: 20px;
  }

  .claude-setup-body li {
    margin-bottom: 4px;
  }

  .claude-setup-body code {
    background: var(--bg-dark);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.923rem;
    font-family: 'Menlo', Monaco, monospace;
  }

  .claude-setup-note {
    font-size: 0.923rem;
    color: var(--fg-dim);
    font-style: italic;
  }

  .claude-setup-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 16px;
  }

</style>
