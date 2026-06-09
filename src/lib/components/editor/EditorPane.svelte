<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { EditorView, keymap, lineNumbers, highlightActiveLineGutter, highlightSpecialChars, dropCursor, rectangularSelection, crosshairCursor, highlightActiveLine } from '@codemirror/view';
  import { EditorState, Compartment } from '@codemirror/state';
  import { MergeView } from '@codemirror/merge';
  import { defaultKeymap, history, historyKeymap, indentWithTab } from '@codemirror/commands';
  import { foldGutter, indentOnInput, bracketMatching, foldKeymap, foldAll, unfoldAll } from '@codemirror/language';
  import { closeBrackets, closeBracketsKeymap } from '@codemirror/autocomplete';
  import { search, searchKeymap, highlightSelectionMatches, getSearchQuery } from '@codemirror/search';
  import { ViewPlugin } from '@codemirror/view';
  import { contentSmartQuoteFix } from '$lib/utils/smartQuotes';
  import type { EditorFileInfo } from '$lib/tauri/types';
  import { readFile, readFileBase64, writeFile, scpReadFile, scpReadFileBase64, scpWriteFile, watchFile, unwatchFile, getFileMtime, watchRemoteFile, unwatchRemoteFile, getRemoteFileMtime } from '$lib/tauri/commands';
  import { loadLanguageExtension, detectLanguageFromContent, isImageFile, getImageMimeType, isPdfFile, isMarkdownFile } from '$lib/utils/languageDetect';
  import { marked } from 'marked';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import { buildEditorExtension } from '$lib/utils/editorTheme';
  import { getTheme } from '$lib/themes';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { dispatch } from '$lib/stores/notificationDispatch';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { registerEditor, unregisterEditor, setEditorDirty } from '$lib/stores/editorRegistry.svelte';
  import { claudeCodeStore } from '$lib/stores/claudeCode.svelte';
  import { EditorSelection } from '@codemirror/state';
  import { countedListen as listen } from '$lib/utils/listenCounter';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import Button from '$lib/components/ui/Button.svelte';

  interface Props {
    workspaceId: string;
    paneId: string;
    tabId: string;
    visible: boolean;
    editorFile: EditorFileInfo;
  }

  let { workspaceId, paneId, tabId, visible, editorFile }: Props = $props();

  // Clicking anywhere in this editor focuses its pane, so pane-targeted actions
  // (Cmd+T, Cmd+D split, etc.) operate on the pane the user is looking at.
  function focusPane() {
    if (workspacesStore.activeWorkspace?.active_pane_id !== paneId) {
      workspacesStore.setActivePane(workspaceId, paneId);
    }
  }

  let containerRef: HTMLDivElement;
  let editorView: EditorView | null = null;
  let dirty = $state(false);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let originalContent = '';
  let imageDataUrl = $state<string | null>(null);
  let imageFileSize = $state(0);
  let imageNaturalWidth = $state(0);
  let imageNaturalHeight = $state(0);
  /** 0 = fit-to-window mode; positive = explicit zoom percentage (100 = 1:1 pixels) */
  let imageZoom = $state(0);
  let imageEl = $state<HTMLImageElement | null>(null);
  let imageScrollEl = $state<HTMLDivElement | null>(null);
  let altKeyHeld = $state(false);
  let wordWrap = $state(false);
  const wrapCompartment = new Compartment();

  // File watching state
  let lastKnownMtime = 0;
  let fileConflict = $state(false);
  let fileDeleted = $state(false);
  let unlistenFileChanged: UnlistenFn | null = null;
  let unlistenFileDeleted: UnlistenFn | null = null;
  const isLocalFile = !editorFile.is_remote;

  // PDF viewer state
  let pdfDoc = $state<any>(null);
  let pdfPageCount = $state(0);
  let pdfCurrentPage = $state(1);
  let pdfZoom = $state(100);
  let pdfCanvasRefs = $state<HTMLCanvasElement[]>([]);
  let pdfScrollEl = $state<HTMLDivElement | null>(null);
  let pdfFileSize = $state(0);
  let pdfRendering = $state(false);

  // Markdown preview state
  let markdownPreview = $state(false);
  let markdownHtml = $state('');

  // Goto line modal state
  let gotoOpen = $state(false);
  let gotoValue = $state('');
  let gotoError = $state('');
  let gotoInputEl = $state<HTMLInputElement | null>(null);
  let gotoMaxLine = $state(1);

  function openGotoLine() {
    if (!editorView) return false;
    gotoMaxLine = editorView.state.doc.lines;
    const cursor = editorView.state.selection.main.head;
    const currentLine = editorView.state.doc.lineAt(cursor).number;
    gotoValue = String(currentLine);
    gotoError = '';
    gotoOpen = true;
    queueMicrotask(() => {
      gotoInputEl?.focus();
      gotoInputEl?.select();
    });
    return true;
  }

  function closeGotoLine() {
    gotoOpen = false;
    gotoError = '';
    editorView?.focus();
  }

  function submitGotoLine() {
    if (!editorView) return;
    const raw = gotoValue.trim();
    const m = raw.match(/^(\d+)(?::(\d+))?$/);
    if (!m) {
      gotoError = 'Enter a line number (or line:col)';
      return;
    }
    const totalLines = editorView.state.doc.lines;
    const line = Math.max(1, Math.min(totalLines, parseInt(m[1], 10)));
    const col = m[2] ? Math.max(1, parseInt(m[2], 10)) : 1;
    const info = editorView.state.doc.line(line);
    const pos = Math.min(info.from + col - 1, info.to);
    editorView.dispatch({
      selection: EditorSelection.cursor(pos),
      effects: EditorView.scrollIntoView(pos, { y: 'center' }),
    });
    gotoOpen = false;
    gotoError = '';
    editorView.focus();
  }

  function handleGotoKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      submitGotoLine();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      closeGotoLine();
    }
  }
  const isMarkdown = isMarkdownFile(editorFile.remote_path ?? editorFile.file_path);

  /** Resolve a potentially relative path against the directory of the current file. */
  function resolveRelativePath(src: string): string | null {
    // Skip absolute URLs, data URIs, and protocol URLs
    if (/^(https?:|data:|blob:|file:)/i.test(src)) return null;
    const filePath = editorFile.remote_path ?? editorFile.file_path;
    const dir = filePath.substring(0, filePath.lastIndexOf('/'));
    if (src.startsWith('/')) return src;
    // Resolve relative: ./foo, ../foo, foo
    const parts = `${dir}/${src}`.split('/');
    const resolved: string[] = [];
    for (const p of parts) {
      if (p === '.' || p === '') continue;
      if (p === '..') { resolved.pop(); continue; }
      resolved.push(p);
    }
    return '/' + resolved.join('/');
  }

  /** After rendering markdown HTML, replace relative img src with base64 data URLs. */
  async function resolveMarkdownImages(html: string): Promise<string> {
    const imgRe = /<img\s+([^>]*?)src="([^"]+)"([^>]*)>/gi;
    const replacements: { original: string; replacement: string }[] = [];
    let match;
    while ((match = imgRe.exec(html)) !== null) {
      const fullTag = match[0];
      const src = match[2];
      const absPath = resolveRelativePath(src);
      if (!absPath) continue;
      const mime = getImageMimeType(absPath) ?? 'image/png';
      try {
        let data: string;
        if (editorFile.is_remote && editorFile.remote_ssh_command) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command, absPath);
          data = result.data;
        } else {
          const result = await readFileBase64(absPath);
          data = result.data;
        }
        const dataUrl = `data:${mime};base64,${data}`;
        replacements.push({ original: fullTag, replacement: fullTag.replace(src, dataUrl) });
      } catch {
        // Leave broken — file might not exist
      }
    }
    for (const { original, replacement } of replacements) {
      html = html.replace(original, replacement);
    }
    return html;
  }

  function toggleWordWrap() {
    wordWrap = !wordWrap;
    editorView?.dispatch({
      effects: wrapCompartment.reconfigure(wordWrap ? EditorView.lineWrapping : []),
    });
  }

  function toggleMarkdownPreview() {
    markdownPreview = !markdownPreview;
    if (markdownPreview && editorView) {
      const src = editorView.state.doc.toString();
      const html = marked.parse(src, { breaks: true, gfm: true }) as string;
      markdownHtml = html;
      // Async: resolve relative images after initial render
      resolveMarkdownImages(html).then(resolved => { markdownHtml = resolved; });
    }
  }

  function handleMarkdownClick(e: MouseEvent) {
    const anchor = (e.target as HTMLElement).closest('a');
    if (anchor?.href) {
      e.preventDefault();
      shellOpen(anchor.href);
    }
  }

  const PDF_ZOOM_STEPS = [50, 75, 100, 125, 150, 200, 300, 400];

  const ZOOM_STEPS = [10, 25, 50, 75, 100, 150, 200, 300, 400, 500];

  let cursorX = $state(0);
  let cursorY = $state(0);
  let cursorVisible = $state(false);

  function handleImageMouseMove(e: MouseEvent) {
    cursorX = e.clientX;
    cursorY = e.clientY;
    cursorVisible = true;
  }

  function handleImageMouseLeave() {
    cursorVisible = false;
  }

  /** Compute the actual display percentage when in fit-to-window mode. */
  const fitPercent = $derived.by(() => {
    if (!imageScrollEl || !imageNaturalWidth || !imageNaturalHeight) return 100;
    const cw = imageScrollEl.clientWidth - 32; // subtract padding
    const ch = imageScrollEl.clientHeight - 32;
    if (cw <= 0 || ch <= 0) return 100;
    const scale = Math.min(cw / imageNaturalWidth, ch / imageNaturalHeight, 1);
    return Math.round(scale * 100);
  });

  /** The effective zoom percentage shown in the label. */
  const displayZoom = $derived(imageZoom === 0 ? fitPercent : imageZoom);

  /**
   * Zoom and keep the given point (in scroll-container coordinates) anchored.
   * If no point is given, anchors on the current viewport center.
   */
  function zoomTo(newZoom: number, anchorX?: number, anchorY?: number) {
    const sc = imageScrollEl;
    if (!sc || !imageNaturalWidth) {
      imageZoom = newZoom;
      return;
    }

    const oldZoom = displayZoom;
    // Anchor point in scroll-container viewport coords (default = center)
    const ax = anchorX ?? sc.clientWidth / 2;
    const ay = anchorY ?? sc.clientHeight / 2;
    // Point in image-natural-pixel space
    const imgX = (sc.scrollLeft + ax) / (oldZoom / 100);
    const imgY = (sc.scrollTop + ay) / (oldZoom / 100);

    imageZoom = newZoom;

    // After Svelte updates the DOM with the new size, restore scroll
    requestAnimationFrame(() => {
      const effectiveZoom = newZoom === 0 ? fitPercent : newZoom;
      sc.scrollLeft = imgX * (effectiveZoom / 100) - ax;
      sc.scrollTop = imgY * (effectiveZoom / 100) - ay;
    });
  }

  function zoomIn(anchorX?: number, anchorY?: number) {
    const current = displayZoom;
    const next = ZOOM_STEPS.find(z => z > current);
    zoomTo(next ?? ZOOM_STEPS[ZOOM_STEPS.length - 1], anchorX, anchorY);
  }

  function zoomOut(anchorX?: number, anchorY?: number) {
    const current = displayZoom;
    const prev = [...ZOOM_STEPS].reverse().find(z => z < current);
    zoomTo(prev ?? ZOOM_STEPS[0], anchorX, anchorY);
  }

  function zoomFit() {
    imageZoom = 0;
  }

  function handleImageClick(e: MouseEvent) {
    const sc = imageScrollEl;
    if (!sc) return;
    // Anchor at click position relative to scroll container
    const rect = sc.getBoundingClientRect();
    const ax = e.clientX - rect.left;
    const ay = e.clientY - rect.top;

    if (e.altKey || e.button === 2) {
      zoomOut(ax, ay);
    } else {
      zoomIn(ax, ay);
    }
  }

  function handleImageContextMenu(e: MouseEvent) {
    e.preventDefault();
    handleImageClick(e);
  }

  function formatFileSize(bytes: number): string {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(2)} MB`;
  }

  function handleImageLoad(e: Event) {
    const img = e.target as HTMLImageElement;
    imageNaturalWidth = img.naturalWidth;
    imageNaturalHeight = img.naturalHeight;
  }

  let pdfTextLayerRefs = $state<HTMLDivElement[]>([]);

  async function renderPdfPages() {
    if (!pdfDoc || pdfRendering) return;
    pdfRendering = true;
    const scale = pdfZoom / 100;
    const dpr = window.devicePixelRatio || 1;

    for (let i = 0; i < pdfDoc.numPages; i++) {
      const page = await pdfDoc.getPage(i + 1);
      const cssViewport = page.getViewport({ scale });
      const renderViewport = page.getViewport({ scale: scale * dpr });
      const canvas = pdfCanvasRefs[i];
      if (!canvas) continue;

      canvas.width = renderViewport.width;
      canvas.height = renderViewport.height;
      canvas.style.width = `${cssViewport.width}px`;
      canvas.style.height = `${cssViewport.height}px`;

      const ctx = canvas.getContext('2d');
      if (!ctx) continue;
      await page.render({ canvasContext: ctx, viewport: renderViewport }).promise;

      // Render text layer for selection/copy
      const textDiv = pdfTextLayerRefs[i];
      if (textDiv) {
        textDiv.innerHTML = '';
        textDiv.style.width = `${cssViewport.width}px`;
        textDiv.style.height = `${cssViewport.height}px`;

        const textContent = await page.getTextContent();
        const { TextLayer } = await import('pdfjs-dist');
        const textLayer = new TextLayer({
          textContentSource: textContent,
          container: textDiv,
          viewport: cssViewport,
        });
        await textLayer.render();

        // pdfjs uses CSS custom properties for sizing; override with explicit dimensions
        textDiv.style.setProperty('--total-scale-factor', String(scale));
        textDiv.style.setProperty('--scale-round-x', '1px');
        textDiv.style.setProperty('--scale-round-y', '1px');
      }
    }
    pdfRendering = false;
  }

  function pdfZoomIn() {
    const next = PDF_ZOOM_STEPS.find(z => z > pdfZoom);
    if (next) {
      pdfZoom = next;
      renderPdfPages();
    }
  }

  function pdfZoomOut() {
    const prev = [...PDF_ZOOM_STEPS].reverse().find(z => z < pdfZoom);
    if (prev) {
      pdfZoom = prev;
      renderPdfPages();
    }
  }

  function pdfGoToPage(page: number) {
    const clamped = Math.max(1, Math.min(page, pdfPageCount));
    pdfCurrentPage = clamped;
    const canvas = pdfCanvasRefs[clamped - 1];
    canvas?.scrollIntoView({ behavior: 'smooth', block: 'start' });
  }

  function handlePdfScroll() {
    if (!pdfScrollEl || !pdfCanvasRefs.length) return;
    const scrollTop = pdfScrollEl.scrollTop;
    const scrollMid = scrollTop + pdfScrollEl.clientHeight / 2;
    let cumulative = 0;
    for (let i = 0; i < pdfCanvasRefs.length; i++) {
      const h = pdfCanvasRefs[i]?.offsetHeight ?? 0;
      cumulative += h + 12; // 12px gap
      if (cumulative > scrollMid) {
        pdfCurrentPage = i + 1;
        break;
      }
    }
  }

  function attachToSlot() {
    const slot = document.querySelector(`[data-terminal-slot="${tabId}"]`) as HTMLElement;
    if (slot && containerRef && containerRef.parentElement !== slot) {
      slot.appendChild(containerRef);
    }
  }

  function handleSlotReady(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (detail?.tabId === tabId) {
      attachToSlot();
    }
  }

  function handleEditorSave(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (detail?.tabId === tabId) {
      saveFile();
    }
  }

  function handleEditorReload(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (detail?.tabId === tabId) {
      reloadFile();
    }
  }

  function handleEditorReplaceFile(e: Event) {
    const detail = (e as CustomEvent).detail;
    if (detail?.tabId === tabId) {
      replaceFile();
    }
  }

  async function replaceFile() {
    // Load new content in the background before swapping — keeps old content
    // visible so the tab doesn't flash a loading state.
    const filePath = editorFile.remote_path ?? editorFile.file_path;
    const isRemote = editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path;

    try {
      if (isImageFile(filePath)) {
        const mime = getImageMimeType(filePath) ?? 'image/png';
        let data: string;
        let size: number;
        if (isRemote) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command!, editorFile.remote_path!);
          data = result.data; size = result.size;
        } else {
          const result = await readFileBase64(editorFile.file_path);
          data = result.data; size = result.size;
        }
        // Swap in new image atomically — no loading flash
        stopWatching();
        if (editorView) { editorView.destroy(); editorView = null; }
        if (pdfDoc) { pdfDoc.destroy(); pdfDoc = null; }
        pdfPageCount = 0; pdfFileSize = 0;
        errorMsg = null; dirty = false;
        imageDataUrl = `data:${mime};base64,${data}`;
        imageFileSize = size;
        imageNaturalWidth = 0;
        imageNaturalHeight = 0;
        imageZoom = 0;
        loading = false;
      } else if (isPdfFile(filePath)) {
        let data: string;
        let size: number;
        if (isRemote) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command!, editorFile.remote_path!);
          data = result.data; size = result.size;
        } else {
          const result = await readFileBase64(editorFile.file_path);
          data = result.data; size = result.size;
        }
        const pdfjsLib = await import('pdfjs-dist');
        const raw = Uint8Array.from(atob(data), c => c.charCodeAt(0));
        const doc = await pdfjsLib.getDocument({ data: raw }).promise;
        // Swap in new PDF
        stopWatching();
        if (editorView) { editorView.destroy(); editorView = null; }
        if (pdfDoc) pdfDoc.destroy();
        imageDataUrl = null; imageFileSize = 0;
        errorMsg = null; dirty = false;
        pdfDoc = doc;
        pdfPageCount = doc.numPages;
        pdfFileSize = size;
        pdfCanvasRefs = new Array(doc.numPages);
        pdfTextLayerRefs = new Array(doc.numPages);
        loading = false;
        requestAnimationFrame(() => renderPdfPages());
      } else {
        let content: string;
        if (isRemote) {
          const result = await scpReadFile(editorFile.remote_ssh_command!, editorFile.remote_path!);
          content = result.content;
        } else {
          const result = await readFile(editorFile.file_path);
          content = result.content;
        }
        // Swap in new text content
        stopWatching();
        if (pdfDoc) { pdfDoc.destroy(); pdfDoc = null; }
        imageDataUrl = null; imageFileSize = 0;
        pdfPageCount = 0; pdfFileSize = 0;
        errorMsg = null;

        if (editorView) {
          // Reuse existing editor — just replace content
          originalContent = content;
          editorView.dispatch({
            changes: { from: 0, to: editorView.state.doc.length, insert: content },
          });
          dirty = false;
          setEditorDirty(tabId, false);
          registerEditor(tabId, editorView, editorFile.file_path);
        } else {
          // Create new editor (switching from image/PDF to text)
          originalContent = content;
          const langId = editorFile.language ?? detectLanguageFromContent(content);
          const langExt = langId ? await loadLanguageExtension(langId) : null;

          const extensions = [
            lineNumbers(),
            highlightActiveLineGutter(),
            highlightSpecialChars(),
            history(),
            foldGutter(),
            dropCursor(),
            EditorState.allowMultipleSelections.of(true),
            indentOnInput(),
            bracketMatching(),
            closeBrackets(),
            rectangularSelection(),
            crosshairCursor(),
            highlightActiveLine(),
            highlightSelectionMatches(),
            wrapCompartment.of([]),
            search({ top: true }),
            contentSmartQuoteFix,
            keymap.of([
              ...closeBracketsKeymap,
              ...defaultKeymap,
              ...searchKeymap,
              ...historyKeymap,
              ...foldKeymap,
              { key: 'Mod-Shift--', run: foldAll },
              { key: 'Mod-Shift-=', run: unfoldAll },
              indentWithTab,
            ]),
            ...buildEditorExtension(getTheme(preferencesStore.theme, preferencesStore.customThemes)),
            EditorView.updateListener.of((update) => {
              if (update.docChanged) {
                const isDirty = update.state.doc.toString() !== originalContent;
                dirty = isDirty;
                setEditorDirty(tabId, isDirty);
              }
            }),
            EditorView.theme({
              '&': { height: '100%', fontSize: `${preferencesStore.fontSize}px` },
              '.cm-scroller': { fontFamily: `"${preferencesStore.fontFamily}", Monaco, "Courier New", monospace`, overflow: 'auto' },
            }),
            keymap.of([{ key: 'Mod-s', run: () => { saveFile(); return true; } }]),
          ];
          if (langExt) extensions.push(langExt);

          editorView = new EditorView({
            state: EditorState.create({ doc: content, extensions }),
            parent: containerRef,
          });
          registerEditor(tabId, editorView, editorFile.file_path);
        }
        loading = false;
      }
    } catch (e) {
      errorMsg = String(e);
      loading = false;
      logError(`Failed to replace file: ${e}`);
    }
  }

  async function reloadFile() {
    const filePath = editorFile.remote_path ?? editorFile.file_path;
    const fileName = filePath.split('/').pop() ?? 'file';
    const isRemote = editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path;

    try {
      if (imageDataUrl) {
        // Reload image
        const mime = getImageMimeType(filePath) ?? 'image/png';
        let data: string;
        let size: number;
        if (isRemote) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command!, editorFile.remote_path!);
          data = result.data; size = result.size;
        } else {
          const result = await readFileBase64(editorFile.file_path);
          data = result.data; size = result.size;
        }
        imageDataUrl = `data:${mime};base64,${data}`;
        imageFileSize = size;
        dispatch('Reloaded', fileName, 'info');
      } else if (pdfDoc) {
        // Reload PDF
        let data: string;
        let size: number;
        if (isRemote) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command!, editorFile.remote_path!);
          data = result.data; size = result.size;
        } else {
          const result = await readFileBase64(editorFile.file_path);
          data = result.data; size = result.size;
        }
        pdfFileSize = size;
        const pdfjsLib = await import('pdfjs-dist');
        const raw = Uint8Array.from(atob(data), c => c.charCodeAt(0));
        pdfDoc.destroy();
        const doc = await pdfjsLib.getDocument({ data: raw }).promise;
        pdfDoc = doc;
        pdfPageCount = doc.numPages;
        pdfCanvasRefs = new Array(doc.numPages);
        pdfTextLayerRefs = new Array(doc.numPages);
        requestAnimationFrame(() => renderPdfPages());
        dispatch('Reloaded', fileName, 'info');
      } else if (editorView) {
        // Reload text file
        let content: string;
        if (isRemote) {
          const result = await scpReadFile(editorFile.remote_ssh_command!, editorFile.remote_path!);
          content = result.content;
        } else {
          const result = await readFile(editorFile.file_path);
          content = result.content;
        }
        originalContent = content;
        editorView.dispatch({
          changes: { from: 0, to: editorView.state.doc.length, insert: content },
        });
        dirty = false;
        setEditorDirty(tabId, false);
        if (isLocalFile) {
          try { lastKnownMtime = await getFileMtime(editorFile.file_path); } catch { /* ignore */ }
        } else if (editorFile.remote_ssh_command && editorFile.remote_path) {
          try { lastKnownMtime = await getRemoteFileMtime(editorFile.remote_ssh_command, editorFile.remote_path); } catch { /* ignore */ }
        }
        dispatch('Reloaded', fileName, 'info');
      }
    } catch (e) {
      dispatch('Reload failed', String(e), 'error');
      logError(`Failed to reload file: ${e}`);
    }
  }

  async function startWatching() {
    if (isLocalFile) {
      try {
        // Check mtime first — file may have changed while tab was hidden
        const currentMtime = await getFileMtime(editorFile.file_path);
        if (lastKnownMtime > 0 && currentMtime > lastKnownMtime) {
          await handleExternalChange();
        }
        lastKnownMtime = currentMtime;

        // Start fs watcher
        await watchFile(tabId, editorFile.file_path);
        unlistenFileChanged = await listen(`file-changed-${tabId}`, async () => {
          try {
            const newMtime = await getFileMtime(editorFile.file_path);
            if (newMtime <= lastKnownMtime) return;
            lastKnownMtime = newMtime;
            fileDeleted = false;
            await handleExternalChange();
          } catch {
            // File may have been deleted — ignore
          }
        });
      } catch (e) {
        logError(`Failed to start file watcher: ${e}`);
      }
    } else if (editorFile.remote_ssh_command && editorFile.remote_path) {
      try {
        // Check mtime via SSH — file may have changed while tab was hidden
        const currentMtime = await getRemoteFileMtime(editorFile.remote_ssh_command, editorFile.remote_path);
        if (lastKnownMtime > 0 && currentMtime > lastKnownMtime) {
          await handleExternalChange();
        }
        lastKnownMtime = currentMtime;

        // Register with backend polling task
        await watchRemoteFile(tabId, editorFile.remote_ssh_command, editorFile.remote_path);
        unlistenFileChanged = await listen(`file-changed-${tabId}`, async () => {
          try {
            const newMtime = await getRemoteFileMtime(editorFile.remote_ssh_command!, editorFile.remote_path!);
            if (newMtime <= lastKnownMtime) return;
            lastKnownMtime = newMtime;
            fileDeleted = false;
            await handleExternalChange();
          } catch {
            // SSH may have disconnected — ignore
          }
        });
      } catch (e) {
        logError(`Failed to start remote file watcher: ${e}`);
      }
    }

    // Listen for file deletion (both local and remote)
    unlistenFileDeleted = await listen(`file-deleted-${tabId}`, () => {
      fileDeleted = true;
      logInfo(`File deleted: ${editorFile.remote_path ?? editorFile.file_path}`);
    });
  }

  async function stopWatching() {
    if (unlistenFileChanged) {
      unlistenFileChanged();
      unlistenFileChanged = null;
    }
    if (unlistenFileDeleted) {
      unlistenFileDeleted();
      unlistenFileDeleted = null;
    }
    if (isLocalFile) {
      try { await unwatchFile(tabId); } catch { /* ignore */ }
    } else {
      try { await unwatchRemoteFile(tabId); } catch { /* ignore */ }
    }
  }

  async function handleExternalChange() {
    if (!editorView) return;
    if (!dirty) {
      // Auto-reload silently, preserving scroll position and cursor
      try {
        let content: string;
        if (editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path) {
          const result = await scpReadFile(editorFile.remote_ssh_command, editorFile.remote_path);
          content = result.content;
        } else {
          const result = await readFile(editorFile.file_path);
          content = result.content;
        }
        if (content === editorView.state.doc.toString()) {
          // Content identical (mtime changed but content didn't) — skip dispatch
          originalContent = content;
        } else {
          // Save scroll position before replacing content
          const scroller = editorView.scrollDOM;
          const scrollTop = scroller.scrollTop;
          const scrollLeft = scroller.scrollLeft;
          originalContent = content;
          editorView.dispatch({
            changes: { from: 0, to: editorView.state.doc.length, insert: content },
          });
          // Restore scroll position after content swap
          requestAnimationFrame(() => {
            scroller.scrollTop = scrollTop;
            scroller.scrollLeft = scrollLeft;
          });
          logInfo(`Auto-reloaded ${editorFile.remote_path ?? editorFile.file_path}`);
        }
        dirty = false;
        setEditorDirty(tabId, false);
      } catch (e) {
        logError(`Auto-reload failed: ${e}`);
      }
    } else {
      // Show conflict banner
      fileConflict = true;
    }
  }

  async function deletedRecreate() {
    fileDeleted = false;
    await saveFile();
  }

  async function deletedCloseTab() {
    fileDeleted = false;
    await workspacesStore.deleteTab(workspaceId, paneId, tabId);
  }

  function dismissConflict() {
    fileConflict = false;
  }

  async function conflictReload() {
    fileConflict = false;
    await reloadFile();
  }

  async function conflictOverwrite() {
    fileConflict = false;
    await saveFile();
  }

  // Merge conflict resolution state
  let mergeView = $state<MergeView | null>(null);
  let mergeActive = $state(false);
  let mergeContainerEl = $state<HTMLElement | null>(null);

  async function conflictMerge() {
    fileConflict = false;
    if (!editorView) return;

    // Read the current disk content
    let diskContent: string;
    try {
      if (editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path) {
        const result = await scpReadFile(editorFile.remote_ssh_command, editorFile.remote_path);
        diskContent = result.content;
      } else {
        const result = await readFile(editorFile.file_path);
        diskContent = result.content;
      }
    } catch (e) {
      dispatch('Merge failed', `Could not read file: ${e}`, 'error');
      return;
    }

    const editorContent = editorView.state.doc.toString();
    mergeActive = true;

    // Build MergeView after DOM updates
    requestAnimationFrame(() => {
      if (!mergeContainerEl) return;

      const currentTheme = getTheme(preferencesStore.theme, preferencesStore.customThemes);
      const themeExtension = buildEditorExtension(currentTheme);
      const editorTheme = EditorView.theme({
        '&': { fontSize: `${preferencesStore.fontSize}px` },
        '.cm-scroller': {
          fontFamily: `"${preferencesStore.fontFamily}", Monaco, "Courier New", monospace`,
        },
      });

      mergeView = new MergeView({
        a: {
          doc: diskContent,
          extensions: [
            EditorState.readOnly.of(true),
            lineNumbers(),
            highlightSpecialChars(),
            highlightActiveLine(),
            ...themeExtension,
            editorTheme,
          ],
        },
        b: {
          doc: editorContent,
          extensions: [
            contentSmartQuoteFix,
            lineNumbers(),
            highlightSpecialChars(),
            highlightActiveLine(),
            ...themeExtension,
            editorTheme,
          ],
        },
        parent: mergeContainerEl,
        gutter: true,
        highlightChanges: true,
        collapseUnchanged: { margin: 3, minSize: 4 },
      });
    });
  }

  function mergeApply() {
    if (!mergeView || !editorView) return;
    const mergedContent = mergeView.b.state.doc.toString();
    editorView.dispatch({
      changes: { from: 0, to: editorView.state.doc.length, insert: mergedContent },
    });
    originalContent = mergedContent;
    dirty = true;
    setEditorDirty(tabId, true);
    closeMerge();
    dispatch('Merge applied', 'Review and save when ready', 'info');
  }

  function mergeCancel() {
    closeMerge();
    // Re-show the conflict banner so user can still choose
    fileConflict = true;
  }

  function closeMerge() {
    mergeActive = false;
    if (mergeView) {
      mergeView.destroy();
      mergeView = null;
    }
  }

  async function saveFile() {
    if (!editorView || !dirty) return;
    const content = editorView.state.doc.toString();
    try {
      if (editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path) {
        await scpWriteFile(editorFile.remote_ssh_command, editorFile.remote_path, content);
      } else {
        await writeFile(editorFile.file_path, content);
      }
      dirty = false;
      originalContent = content;
      setEditorDirty(tabId, false);
      // Update mtime so the watcher doesn't treat our own save as an external change
      if (isLocalFile) {
        try { lastKnownMtime = await getFileMtime(editorFile.file_path); } catch { /* ignore */ }
      } else if (editorFile.remote_ssh_command && editorFile.remote_path) {
        try { lastKnownMtime = await getRemoteFileMtime(editorFile.remote_ssh_command, editorFile.remote_path); } catch { /* ignore */ }
      }
      dispatch('File saved', editorFile.file_path.split('/').pop() ?? 'file', 'info');
    } catch (e) {
      dispatch('Save failed', String(e), 'error');
      logError(`Failed to save file: ${e}`);
    }
  }

  onMount(async () => {
    // Portal into slot
    attachToSlot();
    window.addEventListener('terminal-slot-ready', handleSlotReady);
    window.addEventListener('editor-save', handleEditorSave);
    window.addEventListener('editor-reload', handleEditorReload);
    window.addEventListener('editor-replace-file', handleEditorReplaceFile);

    const filePath = editorFile.remote_path ?? editorFile.file_path;
    const isImage = isImageFile(filePath);
    const isPdf = isPdfFile(filePath);

    try {
      if (isImage) {
        // Load image as base64 data URL
        const mime = getImageMimeType(filePath) ?? 'image/png';
        let data: string;
        let size: number;
        if (editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command, editorFile.remote_path);
          data = result.data;
          size = result.size;
        } else {
          const result = await readFileBase64(editorFile.file_path);
          data = result.data;
          size = result.size;
        }
        imageDataUrl = `data:${mime};base64,${data}`;
        imageFileSize = size;
        loading = false;
      } else if (isPdf) {
        // Load PDF via pdfjs-dist
        let data: string;
        let size: number;
        if (editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path) {
          const result = await scpReadFileBase64(editorFile.remote_ssh_command, editorFile.remote_path);
          data = result.data;
          size = result.size;
        } else {
          const result = await readFileBase64(editorFile.file_path);
          data = result.data;
          size = result.size;
        }
        pdfFileSize = size;

        const pdfjsLib = await import('pdfjs-dist');
        pdfjsLib.GlobalWorkerOptions.workerSrc = new URL('pdfjs-dist/build/pdf.worker.mjs', import.meta.url).href;

        const raw = Uint8Array.from(atob(data), c => c.charCodeAt(0));
        const doc = await pdfjsLib.getDocument({ data: raw }).promise;
        pdfDoc = doc;
        pdfPageCount = doc.numPages;
        pdfCanvasRefs = new Array(doc.numPages);
        pdfTextLayerRefs = new Array(doc.numPages);
        loading = false;

        // Render after DOM updates with canvas refs
        requestAnimationFrame(() => renderPdfPages());
      } else {
        // Load text file into CodeMirror
        let content: string;
        if (editorFile.is_remote && editorFile.remote_ssh_command && editorFile.remote_path) {
          const result = await scpReadFile(editorFile.remote_ssh_command, editorFile.remote_path);
          content = result.content;
        } else {
          const result = await readFile(editorFile.file_path);
          content = result.content;
        }
        originalContent = content;

        const langId = editorFile.language ?? detectLanguageFromContent(content);
        const langExt = langId ? await loadLanguageExtension(langId) : null;

        const extensions = [
          lineNumbers(),
          highlightActiveLineGutter(),
          highlightSpecialChars(),
          history(),
          foldGutter(),
          dropCursor(),
          EditorState.allowMultipleSelections.of(true),
          indentOnInput(),
          bracketMatching(),
          closeBrackets(),
          rectangularSelection(),
          crosshairCursor(),
          highlightActiveLine(),
          highlightSelectionMatches(),
          wrapCompartment.of([]),
          search({ top: true }),
          contentSmartQuoteFix,
          // No-results indicator: toggle class on editor wrapper when search has no matches
          ViewPlugin.define((view) => {
            let prevNoResults = false;
            function check(v: EditorView) {
              const query = getSearchQuery(v.state);
              const hasQuery = query.search.length > 0;
              let noResults = false;
              if (hasQuery) {
                const cursor = query.getCursor(v.state.doc);
                noResults = !cursor.next().done ? false : true;
              }
              if (noResults !== prevNoResults) {
                prevNoResults = noResults;
                v.dom.closest('.editor-container')?.classList.toggle('search-no-results', noResults);
              }
            }
            check(view);
            return {
              update(update) { check(update.view); },
              destroy() {
                view.dom.closest('.editor-container')?.classList.remove('search-no-results');
              },
            };
          }),
          // Center viewport when search navigates to a match (not on regular clicks)
          ViewPlugin.define(() => {
            return {
              update(update) {
                if (!update.selectionSet) return;
                // Only react to search navigation, not user clicks/edits
                const isSearchNav = update.transactions.some(tr => tr.isUserEvent('select.search'));
                if (!isSearchNav) return;
                const sel = update.state.selection.main.from;
                requestAnimationFrame(() => {
                  update.view.dispatch({ effects: EditorView.scrollIntoView(sel, { y: 'center' }) });
                });
              },
            };
          }),
          // Keep the viewport where the user left it when focus returns to the
          // editor after a scrollbar interaction.
          //
          // Root cause: the browser natively scrolls a contenteditable's caret
          // into view whenever it gains focus. If the user scrolled far from the
          // caret with the scrollbar and then returns focus to cm-content, the
          // browser yanks the view back to the old caret (~a screenful). Two
          // symptoms, one cause:
          //   1. Click-to-edit: on a real click the browser focuses cm-content
          //      and caret-scrolls *before* CodeMirror maps the click, so the
          //      view jumps and the click lands on the wrong line (often with a
          //      stray selection). Fix: pre-focus the content with
          //      {preventScroll:true} in the capture phase — before the browser's
          //      default focus runs — so the viewport stays put and posAtCoords
          //      resolves against the correct (un-jumped) viewport.
          //   2. Releasing the scrollbar without clicking: focus returns from
          //      cm-scroller to cm-content and the caret-scroll fires. focusin
          //      runs while scrollTop is still the user's position (the jump
          //      lands immediately after), so snapshot it there and restore on
          //      the next frame.
          // (The earlier version saved scrollTop on focusout — i.e. the instant
          // the scrollbar was grabbed, before any scrolling — and so restored the
          // pre-scroll position; it also never addressed the click mis-mapping.)
          ViewPlugin.define((view) => {
            const scroller = view.scrollDOM;
            const content = view.contentDOM;
            let cameFromScroller = false;

            function onMouseDownCapture() {
              if (!view.hasFocus) {
                content.focus({ preventScroll: true });
              }
            }
            function onFocusOut(e: FocusEvent) {
              const related = e.relatedTarget as Element | null;
              cameFromScroller = related === scroller || scroller.contains(related as Node);
            }
            function onFocusIn() {
              if (!cameFromScroller) return;
              cameFromScroller = false;
              // scrollTop here is still the user's position; the caret-scroll
              // lands on a later tick, so restore on the next frame.
              const restore = scroller.scrollTop;
              requestAnimationFrame(() => {
                scroller.scrollTop = restore;
              });
            }

            content.addEventListener('mousedown', onMouseDownCapture, true);
            content.addEventListener('focusout', onFocusOut);
            content.addEventListener('focusin', onFocusIn);
            return {
              destroy() {
                content.removeEventListener('mousedown', onMouseDownCapture, true);
                content.removeEventListener('focusout', onFocusOut);
                content.removeEventListener('focusin', onFocusIn);
              },
            };
          }),
          keymap.of([
            { key: 'Ctrl-g', mac: 'Ctrl-g', run: () => openGotoLine(), preventDefault: true },
            ...closeBracketsKeymap,
            ...defaultKeymap,
            ...searchKeymap,
            ...historyKeymap,
            ...foldKeymap,
            { key: 'Mod-Shift--', run: foldAll },
            { key: 'Mod-Shift-=', run: unfoldAll },
            indentWithTab,
          ]),
          ...buildEditorExtension(getTheme(preferencesStore.theme, preferencesStore.customThemes)),
          EditorView.updateListener.of((update) => {
            if (update.docChanged) {
              const isDirty = update.state.doc.toString() !== originalContent;
              dirty = isDirty;
              setEditorDirty(tabId, isDirty);
            }
            if (update.selectionSet) {
              const sel = update.state.selection.main;
              const doc = update.state.doc;
              const fromLine = doc.lineAt(sel.from);
              const toLine = doc.lineAt(sel.to);
              const selectedText = doc.sliceString(sel.from, sel.to);
              claudeCodeStore.updateSelection({
                text: selectedText,
                filePath: editorFile.file_path,
                selection: {
                  start: { line: fromLine.number - 1, character: sel.from - fromLine.from },
                  end: { line: toLine.number - 1, character: sel.to - toLine.from },
                  isEmpty: sel.empty,
                },
              });
            }
          }),
          EditorView.theme({
            '&': {
              height: '100%',
              fontSize: `${preferencesStore.fontSize}px`,
            },
            '.cm-scroller': {
              fontFamily: `"${preferencesStore.fontFamily}", Monaco, "Courier New", monospace`,
              overflow: 'auto',
            },
          }),
          keymap.of([{
            key: 'Mod-s',
            run: () => {
              saveFile();
              return true;
            },
          }]),
        ];

        if (langExt) {
          extensions.push(langExt);
        }

        editorView = new EditorView({
          state: EditorState.create({
            doc: content,
            extensions,
          }),
          parent: containerRef,
        });

        registerEditor(tabId, editorView, editorFile.file_path);

        // Record initial mtime and start watching if visible
        if (isLocalFile) {
          try { lastKnownMtime = await getFileMtime(editorFile.file_path); } catch { /* ignore */ }
        } else if (editorFile.remote_ssh_command && editorFile.remote_path) {
          try { lastKnownMtime = await getRemoteFileMtime(editorFile.remote_ssh_command, editorFile.remote_path); } catch { /* ignore */ }
        }
        if (visible) {
          (async () => { await startWatching(); })();
          requestAnimationFrame(() => { editorView?.focus(); });
        }

        // Apply pending selection from Claude Code openFile
        const pending = claudeCodeStore.getPendingSelection(tabId);
        if (pending) {
          claudeCodeStore.clearPendingSelection(tabId);
          const doc = editorView.state.doc;
          if (pending.startLine !== undefined) {
            const line = doc.line(Math.min(pending.startLine + 1, doc.lines));
            const endLine = pending.endLine !== undefined
              ? doc.line(Math.min(pending.endLine + 1, doc.lines))
              : line;
            editorView.dispatch({
              selection: EditorSelection.range(line.from, endLine.to),
              scrollIntoView: true,
            });
          } else if (pending.startText) {
            const text = doc.toString();
            const idx = text.indexOf(pending.startText);
            if (idx >= 0) {
              const endIdx = pending.endText
                ? text.indexOf(pending.endText, idx) + pending.endText.length
                : idx + pending.startText.length;
              editorView.dispatch({
                selection: EditorSelection.range(idx, endIdx >= 0 ? endIdx : idx + pending.startText.length),
                scrollIntoView: true,
              });
            }
          }
        }

        loading = false;
      }
    } catch (e) {
      const msg = String(e).toLowerCase();
      if (msg.includes('is_directory') || msg.includes('not a regular file') || msg.includes('is a directory')) {
        workspacesStore.deleteTab(workspaceId, paneId, tabId);
        return;
      }
      const raw = String(e);
      if (raw.startsWith('FILE_TOO_LARGE:')) {
        const sizeMb = raw.split(':')[1];
        errorMsg = `File is too large (${sizeMb} MB)`;
      } else if (!isImage && !isPdf && raw.toLowerCase().includes('binary')) {
        errorMsg = 'Binary file — cannot open in editor';
      } else {
        errorMsg = raw;
      }
      loading = false;
      logError(`Failed to load file: ${raw}`);
    }
  });

  onDestroy(() => {
    stopWatching();
    closeMerge();
    window.removeEventListener('terminal-slot-ready', handleSlotReady);
    window.removeEventListener('editor-save', handleEditorSave);
    window.removeEventListener('editor-reload', handleEditorReload);
    window.removeEventListener('editor-replace-file', handleEditorReplaceFile);
    unregisterEditor(tabId);
    if (editorView) {
      editorView.destroy();
      editorView = null;
    }
    if (pdfDoc) {
      pdfDoc.destroy();
      pdfDoc = null;
    }
  });

  // Track Alt key for zoom-out cursor on image viewer
  $effect(() => {
    if (!imageDataUrl && !pdfDoc) return;
    const onKey = (e: KeyboardEvent) => { altKeyHeld = e.altKey; };
    const onBlur = () => { altKeyHeld = false; };
    window.addEventListener('keydown', onKey);
    window.addEventListener('keyup', onKey);
    window.addEventListener('blur', onBlur);
    return () => {
      window.removeEventListener('keydown', onKey);
      window.removeEventListener('keyup', onKey);
      window.removeEventListener('blur', onBlur);
    };
  });

  // Focus editor when becoming visible
  $effect(() => {
    if (visible && editorView) {
      requestAnimationFrame(() => {
        editorView?.focus();
      });
    }
  });

  // Start/stop file watching based on visibility
  $effect(() => {
    if (!editorView) return;
    if (visible) {
      startWatching();
    } else {
      stopWatching();
    }
  });
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="editor-container"
  class:hidden={!visible}
  bind:this={containerRef}
  onmousedowncapture={focusPane}
>
  {#if loading}
    <div class="editor-loading">Loading...</div>
  {:else if errorMsg}
    <div class="editor-error">
      <div class="error-content">
        <span class="error-icon">&#x26A0;</span>
        <span class="error-text">{errorMsg}</span>
      </div>
      <div class="error-actions">
        <Button variant="secondary" onclick={() => { navigator.clipboard.writeText(errorMsg ?? ''); }} style="padding:4px 12px;border-radius:4px;font-size: 0.923rem">Copy error</Button>
        <Button variant="secondary" onclick={() => workspacesStore.deleteTab(workspaceId, paneId, tabId)} style="padding:4px 12px;border-radius:4px;font-size: 0.923rem">Close tab</Button>
      </div>
    </div>
  {:else if imageDataUrl}
    <div class="image-preview">
      <div class="image-info-bar">
        {#if imageNaturalWidth > 0}
          <span class="info-item">{imageNaturalWidth} &times; {imageNaturalHeight}</span>
          <span class="info-sep"></span>
        {/if}
        {#if imageFileSize > 0}
          <span class="info-item">{formatFileSize(imageFileSize)}</span>
          <span class="info-sep"></span>
        {/if}
        <div class="zoom-controls">
          <IconButton tooltip="Zoom out" style="width:22px;height:20px;border-radius:3px;font-size: 1.077rem" onclick={() => zoomOut()} disabled={displayZoom <= ZOOM_STEPS[0]}>&minus;</IconButton>
          <button class="zoom-label" class:zoom-fit={imageZoom === 0} onclick={zoomFit} title="Fit to window">{displayZoom}%</button>
          <IconButton tooltip="Zoom in" style="width:22px;height:20px;border-radius:3px;font-size: 1.077rem" onclick={() => zoomIn()} disabled={displayZoom >= ZOOM_STEPS[ZOOM_STEPS.length - 1]}>+</IconButton>
        </div>
      </div>
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="image-scroll" bind:this={imageScrollEl} onclick={handleImageClick} oncontextmenu={handleImageContextMenu} onmousemove={handleImageMouseMove} onmouseleave={handleImageMouseLeave}>
        <img
          bind:this={imageEl}
          src={imageDataUrl}
          alt={editorFile.file_path.split('/').pop() ?? 'image'}
          onload={handleImageLoad}
          style="{imageZoom === 0 ? 'max-width: 100%; max-height: 100%;' : `width: ${imageNaturalWidth * imageZoom / 100}px; height: ${imageNaturalHeight * imageZoom / 100}px;`} object-fit: contain;"
        />
      </div>
    </div>
    {#if cursorVisible}
      <div class="zoom-cursor" style="left: {cursorX}px; top: {cursorY}px;">
        <svg width="24" height="24" viewBox="0 0 24 24">
          <circle cx="10" cy="10" r="6.5" fill="rgba(0,0,0,0.15)" stroke="#000" stroke-width="2.5" opacity=".3"/>
          <circle cx="10" cy="10" r="6.5" fill="rgba(0,0,0,0.15)" stroke="#fff" stroke-width="1.5"/>
          <line x1="15" y1="15" x2="22" y2="22" stroke="#000" stroke-width="2.5" stroke-linecap="round" opacity=".3"/>
          <line x1="15" y1="15" x2="22" y2="22" stroke="#fff" stroke-width="1.5" stroke-linecap="round"/>
          {#if altKeyHeld}
            <line x1="7.5" y1="10" x2="12.5" y2="10" stroke="#fff" stroke-width="1.5" stroke-linecap="round"/>
          {:else}
            <line x1="7.5" y1="10" x2="12.5" y2="10" stroke="#fff" stroke-width="1.5" stroke-linecap="round"/>
            <line x1="10" y1="7.5" x2="10" y2="12.5" stroke="#fff" stroke-width="1.5" stroke-linecap="round"/>
          {/if}
        </svg>
      </div>
    {/if}
  {:else if pdfDoc}
    <div class="pdf-preview">
      <div class="image-info-bar">
        <div class="pdf-page-nav">
          <IconButton tooltip="Previous page" style="width:22px;height:20px;border-radius:3px;font-size: 1.077rem" onclick={() => pdfGoToPage(pdfCurrentPage - 1)} disabled={pdfCurrentPage <= 1}>&#x25C0;</IconButton>
          <span class="info-item">
            <input
              type="number"
              class="pdf-page-input"
              value={pdfCurrentPage}
              min="1"
              max={pdfPageCount}
              onchange={(e) => pdfGoToPage(parseInt((e.target as HTMLInputElement).value) || 1)}
            /> / {pdfPageCount}
          </span>
          <IconButton tooltip="Next page" style="width:22px;height:20px;border-radius:3px;font-size: 1.077rem" onclick={() => pdfGoToPage(pdfCurrentPage + 1)} disabled={pdfCurrentPage >= pdfPageCount}>&#x25B6;</IconButton>
        </div>
        <span class="info-sep"></span>
        {#if pdfFileSize > 0}
          <span class="info-item">{formatFileSize(pdfFileSize)}</span>
          <span class="info-sep"></span>
        {/if}
        <div class="zoom-controls">
          <IconButton tooltip="Zoom out" style="width:22px;height:20px;border-radius:3px;font-size: 1.077rem" onclick={pdfZoomOut} disabled={pdfZoom <= PDF_ZOOM_STEPS[0]}>&minus;</IconButton>
          <span class="zoom-label">{pdfZoom}%</span>
          <IconButton tooltip="Zoom in" style="width:22px;height:20px;border-radius:3px;font-size: 1.077rem" onclick={pdfZoomIn} disabled={pdfZoom >= PDF_ZOOM_STEPS[PDF_ZOOM_STEPS.length - 1]}>+</IconButton>
        </div>
      </div>
      <div class="pdf-scroll" bind:this={pdfScrollEl} onscroll={handlePdfScroll}>
        {#each Array(pdfPageCount) as _, i}
          <div class="pdf-page-wrapper">
            <canvas
              bind:this={pdfCanvasRefs[i]}
              class="pdf-page"
            ></canvas>
            <div
              bind:this={pdfTextLayerRefs[i]}
              class="pdf-text-layer"
            ></div>
          </div>
        {/each}
      </div>
    </div>
  {/if}
  {#if fileDeleted}
    <div class="deleted-banner">
      <span class="deleted-text">File has been deleted.</span>
      <button class="deleted-btn" onclick={deletedRecreate}>Recreate</button>
      <button class="deleted-btn close" onclick={deletedCloseTab}>Close Tab</button>
    </div>
  {/if}
  {#if fileConflict}
    <div class="conflict-banner">
      <span class="conflict-text">File changed on disk.</span>
      <button class="conflict-btn" onclick={conflictMerge}>Merge</button>
      <button class="conflict-btn" onclick={conflictReload}>Reload</button>
      <button class="conflict-btn" onclick={conflictOverwrite}>Overwrite</button>
      <button class="conflict-btn dismiss" onclick={dismissConflict}>Dismiss</button>
    </div>
  {/if}
  {#if mergeActive}
    <div class="merge-overlay">
      <div class="merge-toolbar">
        <div class="merge-labels">
          <span class="merge-label">Disk (read-only)</span>
          <span class="merge-label">Your edits</span>
        </div>
        <div class="merge-actions">
          <button class="conflict-btn" onclick={mergeApply}>Apply</button>
          <button class="conflict-btn dismiss" onclick={mergeCancel}>Cancel</button>
        </div>
      </div>
      <div class="merge-content" bind:this={mergeContainerEl}></div>
    </div>
  {/if}
  {#if !loading && !errorMsg && !imageDataUrl && !pdfDoc}
    <div class="editor-bar">
      <IconButton
        tooltip={wordWrap ? 'Soft wrap: ON (Alt+Z)' : 'Soft wrap: OFF (Alt+Z)'}
        active={wordWrap}
        onclick={toggleWordWrap}
        size={26}
      >
        <Icon name="word-wrap" size={16} />
      </IconButton>
      {#if isMarkdown}
        <IconButton
          tooltip={markdownPreview ? 'Edit markdown' : 'Preview markdown'}
          active={markdownPreview}
          onclick={toggleMarkdownPreview}
          size={26}
        >
          {#if markdownPreview}
            <Icon name="pencil" size={16} />
          {:else}
            <Icon name="eye" size={16} />
          {/if}
        </IconButton>
      {/if}
    </div>
    {#if markdownPreview}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="md-render" onclick={handleMarkdownClick}>{@html markdownHtml}</div>
    {/if}
  {/if}
  {#if gotoOpen}
    <!-- svelte-ignore a11y_click_events_have_key_events -->
    <!-- svelte-ignore a11y_no_static_element_interactions -->
    <div class="goto-backdrop" onclick={(e) => { if (e.target === e.currentTarget) closeGotoLine(); }}>
      <div class="goto-modal" role="dialog" aria-modal="true" aria-label="Go to line">
        <div class="goto-title">Go to line</div>
        <input
          bind:this={gotoInputEl}
          bind:value={gotoValue}
          onkeydown={handleGotoKeydown}
          type="text"
          inputmode="numeric"
          placeholder={`Line 1–${gotoMaxLine} (or line:col)`}
          class="goto-input"
          class:goto-input-error={!!gotoError}
        />
        {#if gotoError}
          <div class="goto-error">{gotoError}</div>
        {:else}
          <div class="goto-hint">Press Enter to jump, Esc to cancel</div>
        {/if}
        <div class="goto-actions">
          <Button variant="secondary" onclick={closeGotoLine} style="padding:4px 12px;border-radius:4px;font-size:0.923rem">Cancel</Button>
          <Button variant="primary" onclick={submitGotoLine} style="padding:4px 12px;border-radius:4px;font-size:0.923rem">Go</Button>
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .editor-container {
    position: relative;
    flex: 1;
    min-height: 0;
    min-width: 0;
    background: var(--bg-dark);
    overflow: hidden;
  }

  .editor-container :global(.cm-editor) {
    height: 100%;
  }

  /* Editor scrollbar — wider than global default so it's easy to grab,
     with a visible track so it doesn't sit invisibly at the window edge */
  .editor-container :global(.cm-scroller)::-webkit-scrollbar {
    width: 12px;
  }
  .editor-container :global(.cm-scroller)::-webkit-scrollbar-track {
    background: var(--bg-medium);
  }

  /* Search panel styling */
  .editor-container :global(.cm-panel.cm-search) {
    padding: 6px 10px;
    font-size: 1rem;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
  }

  .editor-container :global(.cm-panel.cm-search input),
  .editor-container :global(.cm-panel.cm-search button) {
    font-size: 1rem;
  }

  .editor-container :global(.cm-panel.cm-search input[type="text"]) {
    padding: 3px 6px;
    border-radius: 3px;
    border: 1px solid var(--bg-light);
    background: var(--bg-dark);
    color: var(--fg);
  }

  .editor-container :global(.cm-panel.cm-search input[type="text"]:focus) {
    border-color: var(--accent);
    outline: none;
  }

  /* No-results indicator: red border on search input when query has no matches */
  :global(.search-no-results .cm-panel.cm-search input[name="search"]) {
    border-color: var(--red, #f7768e) !important;
    background: color-mix(in srgb, var(--red, #f7768e) 15%, var(--bg-dark)) !important;
  }

  .editor-container :global(.cm-panel.cm-search button) {
    padding: 2px 8px;
    border-radius: 3px;
    background: var(--bg-light);
    color: var(--fg);
    border: none;
    cursor: pointer;
  }

  .editor-container :global(.cm-panel.cm-search button:hover) {
    background: var(--accent);
    color: var(--bg-dark);
  }

  .editor-container :global(.cm-panel.cm-search label) {
    font-size: 1rem;
    color: var(--fg-dim);
  }

  .editor-container :global(.cm-panel.cm-search .cm-button) {
    background-image: none;
  }

  .editor-container.hidden {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    opacity: 0;
    pointer-events: none;
    z-index: -1;
  }

  .editor-loading {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--fg-dim);
    font-size: 1rem;
  }

  .editor-error {
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 12px;
    height: 100%;
    color: var(--fg-dim);
    font-size: 1rem;
  }

  .error-content {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .error-icon {
    font-size: 1.231rem;
    color: var(--yellow, #e0af68);
  }

  .error-text {
    user-select: text;
    -webkit-user-select: text;
    cursor: text;
  }

  .error-actions {
    display: flex;
    gap: 8px;
  }

  .image-preview {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .image-scroll {
    flex: 1;
    overflow: auto;
    padding: 16px;
    min-height: 0;
    cursor: none;
    /* Center small images via margin auto on the img; large images scroll naturally */
  }

  .image-scroll img {
    display: block;
    margin: auto;
    cursor: none;
  }

  .image-preview img {
    border-radius: 4px;
  }

  .image-info-bar {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 8px;
    padding: 4px 12px;
    border-bottom: 1px solid var(--bg-light);
    background: var(--bg-medium);
    flex-shrink: 0;
    height: 28px;
  }

  .info-item {
    font-size: 0.846rem;
    color: var(--fg-dim);
    white-space: nowrap;
  }

  .info-sep {
    width: 1px;
    height: 12px;
    background: var(--bg-light);
    flex-shrink: 0;
  }

  .zoom-controls {
    display: flex;
    align-items: center;
    gap: 0;
  }

  .zoom-label {
    font-size: 0.846rem;
    color: var(--fg-dim);
    min-width: 36px;
    text-align: center;
    padding: 0 2px;
    background: none;
    border: none;
    border-radius: 3px;
    cursor: pointer;
  }

  .zoom-label:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .zoom-label.zoom-fit {
    color: var(--accent);
  }

  .zoom-cursor {
    position: fixed;
    pointer-events: none;
    z-index: 9999;
    transform: translate(-10px, -10px);
    filter: drop-shadow(0 0 1px rgba(0, 0, 0, 0.5));
  }

  /* PDF viewer */
  .pdf-preview {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
  }

  .pdf-scroll {
    flex: 1;
    overflow: auto;
    padding: 16px;
    min-height: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
    background: var(--bg-dark);
  }

  .pdf-page-wrapper {
    position: relative;
    box-shadow: 0 2px 8px rgba(0, 0, 0, 0.3);
    border-radius: 2px;
    flex-shrink: 0;
  }

  .pdf-page {
    display: block;
  }

  .pdf-text-layer {
    position: absolute;
    top: 0;
    left: 0;
    overflow: hidden;
    line-height: 1;
    pointer-events: auto;
  }

  .pdf-text-layer :global(span) {
    position: absolute;
    white-space: pre;
    color: transparent;
    cursor: text;
    pointer-events: auto;
    user-select: text;
    -webkit-user-select: text;
  }

  .pdf-text-layer :global(span::selection) {
    background: rgba(122, 162, 247, 0.3);
  }

  .pdf-text-layer :global(br) {
    display: none;
  }

  .pdf-page-nav {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .pdf-page-input {
    width: 36px;
    text-align: center;
    padding: 1px 2px;
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    background: var(--bg-dark);
    color: var(--fg);
    font-size: 0.846rem;
    -moz-appearance: textfield;
    appearance: textfield;
  }

  .pdf-page-input::-webkit-inner-spin-button,
  .pdf-page-input::-webkit-outer-spin-button {
    -webkit-appearance: none;
    margin: 0;
  }

  .conflict-banner {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    background: color-mix(in srgb, var(--yellow, #e0af68) 15%, var(--bg-dark));
    border-left: 3px solid var(--yellow, #e0af68);
    font-size: 0.923rem;
    color: var(--yellow, #e0af68);
  }

  .conflict-text {
    flex: 1;
  }

  .conflict-btn {
    padding: 3px 10px;
    border-radius: 4px;
    font-size: 0.923rem;
    border: 1px solid var(--yellow, #e0af68);
    background: transparent;
    color: var(--yellow, #e0af68);
    cursor: pointer;
    white-space: nowrap;
  }

  .conflict-btn:hover {
    background: var(--yellow, #e0af68);
    color: var(--bg-dark);
  }

  .conflict-btn.dismiss {
    border-color: var(--fg-dim);
    color: var(--fg-dim);
  }

  .conflict-btn.dismiss:hover {
    background: var(--fg-dim);
    color: var(--bg-dark);
  }

  .deleted-banner {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    z-index: 10;
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 6px 12px;
    background: color-mix(in srgb, var(--red, #f7768e) 15%, var(--bg-dark));
    border-left: 3px solid var(--red, #f7768e);
    font-size: 0.923rem;
    color: var(--red, #f7768e);
  }

  .deleted-text {
    flex: 1;
  }

  .deleted-btn {
    padding: 3px 10px;
    border-radius: 4px;
    font-size: 0.923rem;
    border: 1px solid var(--red, #f7768e);
    background: transparent;
    color: var(--red, #f7768e);
    cursor: pointer;
    white-space: nowrap;
  }

  .deleted-btn:hover {
    background: var(--red, #f7768e);
    color: var(--bg-dark);
  }

  .deleted-btn.close {
    border-color: var(--fg-dim);
    color: var(--fg-dim);
  }

  .deleted-btn.close:hover {
    background: var(--fg-dim);
    color: var(--bg-dark);
  }

  /* Merge overlay */
  .merge-overlay {
    position: absolute;
    inset: 0;
    z-index: 20;
    display: flex;
    flex-direction: column;
    background: var(--bg-dark);
  }

  .merge-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    background: color-mix(in srgb, var(--accent) 15%, var(--bg-dark));
    border-bottom: 1px solid var(--bg-light);
    font-size: 0.923rem;
  }

  .merge-labels {
    display: flex;
    gap: 4px;
    flex: 1;
  }

  .merge-label {
    flex: 1;
    text-align: center;
    color: var(--fg-dim);
    font-size: 0.846rem;
  }

  .merge-actions {
    display: flex;
    gap: 6px;
  }

  .merge-content {
    flex: 1;
    overflow: hidden;
    position: relative;
  }

  .merge-content :global(.cm-mergeView) {
    position: absolute;
    inset: 0;
  }

  .merge-content :global(.cm-mergeViewEditor) {
    height: 100%;
  }

  .merge-content :global(.cm-mergeViewEditor .cm-editor) {
    height: 100%;
  }

  .merge-content :global(.cm-scroller) {
    overflow: auto !important;
  }

  /* Merge diff highlighting — same as DiffPane */
  .merge-content :global(.cm-changedLine) {
    background: color-mix(in srgb, var(--red, #f7768e) 15%, transparent) !important;
  }

  .merge-content :global(.cm-mergeViewEditor:last-child .cm-changedLine) {
    background: color-mix(in srgb, var(--green, #9ece6a) 15%, transparent) !important;
  }

  .merge-content :global(.cm-changedText) {
    background: color-mix(in srgb, var(--red, #f7768e) 35%, transparent) !important;
  }

  .merge-content :global(.cm-mergeViewEditor:last-child .cm-changedText) {
    background: color-mix(in srgb, var(--green, #9ece6a) 35%, transparent) !important;
  }

  .merge-content :global(.cm-changeGutter .cm-gutterElement) {
    color: color-mix(in srgb, var(--red, #f7768e) 60%, transparent);
    font-weight: bold;
  }

  .merge-content :global(.cm-mergeViewEditor:last-child .cm-changeGutter .cm-gutterElement) {
    color: color-mix(in srgb, var(--green, #9ece6a) 60%, transparent);
  }

  .merge-content :global(.cm-collapsedLines) {
    padding: 2px 8px;
    color: var(--fg-dim);
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    font-style: italic;
    cursor: pointer;
  }

  /* Editor toolbar (word wrap, markdown preview) */
  .editor-bar {
    position: absolute;
    top: 6px;
    right: 20px;
    z-index: 5;
    display: flex;
    align-items: center;
    gap: 4px;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    padding: 2px;
  }

  .md-render {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    padding: 24px 32px;
    background: var(--bg-dark);
    color: var(--fg);
    line-height: 1.6;
    z-index: 4;
  }

  .md-render :global(h1),
  .md-render :global(h2),
  .md-render :global(h3),
  .md-render :global(h4) {
    margin: 0.8em 0 0.4em;
    color: var(--fg);
    line-height: 1.3;
  }

  .md-render :global(h1) { font-size: 1.5em; }
  .md-render :global(h2) { font-size: 1.3em; }
  .md-render :global(h3) { font-size: 1.15em; }
  .md-render :global(h4) { font-size: 1.05em; }

  .md-render :global(p) {
    margin: 0 0 0.6em;
  }

  .md-render :global(code) {
    background: var(--bg-light);
    padding: 1px 5px;
    border-radius: 3px;
    font-size: 0.9em;
  }

  .md-render :global(pre) {
    background: var(--bg-medium);
    padding: 12px 14px;
    border-radius: 6px;
    overflow-x: auto;
    margin: 0 0 0.8em;
  }

  .md-render :global(pre code) {
    background: none;
    padding: 0;
  }

  .md-render :global(ul),
  .md-render :global(ol) {
    margin: 0 0 0.6em;
    padding-left: 1.5em;
  }

  .md-render :global(li) {
    margin-bottom: 0.2em;
  }

  .md-render :global(blockquote) {
    border-left: 3px solid var(--bg-light);
    margin: 0 0 0.6em;
    padding: 4px 12px;
    color: var(--fg-dim);
  }

  .md-render :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .md-render :global(a:hover) {
    text-decoration: underline;
  }

  .md-render :global(hr) {
    border: none;
    border-top: 1px solid var(--bg-light);
    margin: 0.8em 0;
  }

  .md-render :global(table) {
    border-collapse: collapse;
    width: 100%;
    margin: 0 0 0.8em;
    font-size: 0.9em;
  }

  .md-render :global(th),
  .md-render :global(td) {
    border: 1px solid var(--bg-light);
    padding: 6px 10px;
    text-align: left;
  }

  .md-render :global(th) {
    background: var(--bg-medium);
    font-weight: 600;
  }

  .md-render :global(img) {
    max-width: 100%;
    border-radius: 4px;
  }

  .md-render :global(input[type="checkbox"]) {
    appearance: none;
    width: 1em;
    height: 1em;
    border: 2px solid var(--fg-dim);
    border-radius: 3px;
    background: transparent;
    vertical-align: middle;
    margin-right: 6px;
    position: relative;
    top: -1px;
  }

  .md-render :global(input[type="checkbox"]:checked) {
    background: var(--accent);
    border-color: var(--accent);
  }

  .md-render :global(input[type="checkbox"]:checked::after) {
    content: '';
    position: absolute;
    left: 50%;
    top: 45%;
    width: 5px;
    height: 9px;
    border: solid var(--bg-dark);
    border-width: 0 2px 2px 0;
    transform: translate(-50%, -60%) rotate(45deg);
  }

  .md-render :global(li:has(> input[type="checkbox"])) {
    list-style: none;
    margin-left: -1.5em;
  }

  .goto-backdrop {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.35);
    display: flex;
    align-items: flex-start;
    justify-content: center;
    padding-top: 15vh;
    z-index: 20;
  }
  .goto-modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 10px 30px rgba(0, 0, 0, 0.4);
    padding: 14px 16px;
    width: min(360px, 90%);
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .goto-title {
    font-size: 0.85rem;
    font-weight: 600;
    color: var(--fg);
  }
  .goto-input {
    width: 100%;
    box-sizing: border-box;
    padding: 6px 8px;
    font-size: 0.95rem;
    color: var(--fg);
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    outline: none;
  }
  .goto-input:focus {
    border-color: var(--accent);
  }
  .goto-input-error {
    border-color: #f7768e;
  }
  .goto-hint {
    font-size: 0.75rem;
    color: var(--fg-dim);
  }
  .goto-error {
    font-size: 0.75rem;
    color: #f7768e;
  }
  .goto-actions {
    display: flex;
    justify-content: flex-end;
    gap: 8px;
    margin-top: 2px;
  }
</style>
