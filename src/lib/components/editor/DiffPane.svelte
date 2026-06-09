<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { MergeView } from '@codemirror/merge';
  import { EditorView, lineNumbers, highlightSpecialChars, highlightActiveLine } from '@codemirror/view';
  import { EditorState } from '@codemirror/state';
  import type { DiffContext } from '$lib/tauri/types';
  import * as commands from '$lib/tauri/commands';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { buildEditorExtension } from '$lib/utils/editorTheme';
  import { contentSmartQuoteFix } from '$lib/utils/smartQuotes';
  import { getTheme } from '$lib/themes';
  import { dispatch as dispatchToast } from '$lib/stores/notificationDispatch';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Button from '$lib/components/ui/Button.svelte';

  interface Props {
    workspaceId: string;
    paneId: string;
    tabId: string;
    visible: boolean;
    diffContext: DiffContext;
  }

  let { workspaceId, paneId, tabId, visible, diffContext }: Props = $props();

  // Clicking anywhere in this diff focuses its pane, so pane-targeted actions
  // (Cmd+T, Cmd+D split, etc.) operate on the pane the user is looking at.
  function focusPane() {
    if (workspacesStore.activeWorkspace?.active_pane_id !== paneId) {
      workspacesStore.setActivePane(workspaceId, paneId);
    }
  }

  let containerRef: HTMLDivElement;
  let mergeView: MergeView | null = null;
  let accepting = $state(false);
  let rejecting = $state(false);
  const readOnly = !diffContext.request_id;

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

  onMount(() => {
    attachToSlot();
    window.addEventListener('terminal-slot-ready', handleSlotReady);

    const currentTheme = getTheme(preferencesStore.theme, preferencesStore.customThemes);
    const themeExtension = buildEditorExtension(currentTheme);

    const editorTheme = EditorView.theme({
      '&': {
        fontSize: `${preferencesStore.fontSize}px`,
      },
      '.cm-scroller': {
        fontFamily: `"${preferencesStore.fontFamily}", Monaco, "Courier New", monospace`,
      },
    });

    const diffContentEl = containerRef.querySelector('.diff-content') as HTMLElement;
    if (!diffContentEl) return;

    mergeView = new MergeView({
      a: {
        doc: diffContext.old_content,
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
        doc: diffContext.new_content,
        extensions: [
          ...(readOnly ? [EditorState.readOnly.of(true)] : [contentSmartQuoteFix]),
          lineNumbers(),
          highlightSpecialChars(),
          highlightActiveLine(),
          ...themeExtension,
          editorTheme,
        ],
      },
      parent: diffContentEl,
      gutter: true,
      highlightChanges: true,
      collapseUnchanged: { margin: 3, minSize: 4 },
    });

  });

  onDestroy(() => {
    window.removeEventListener('terminal-slot-ready', handleSlotReady);
    mergeView?.destroy();
  });

  async function handleAccept() {
    accepting = true;
    try {
      const content = mergeView ? mergeView.b.state.doc.toString() : diffContext.new_content;
      await commands.writeFile(diffContext.file_path, content);
      await commands.claudeCodeRespond(diffContext.request_id, {
        result: 'FILE_SAVED',
        filePath: diffContext.file_path,
        content,
      });
      await workspacesStore.deleteTab(workspaceId, paneId, tabId);
    } catch (err) {
      accepting = false;
      dispatchToast('Save failed', String(err), 'error');
      logError(`DiffPane save failed: ${err}`);
    }
  }

  async function handleReject() {
    rejecting = true;
    try {
      await commands.claudeCodeRespond(diffContext.request_id, { result: 'DIFF_REJECTED' });
      await workspacesStore.deleteTab(workspaceId, paneId, tabId);
    } catch (err) {
      rejecting = false;
      logError(`DiffPane reject failed: ${err}`);
    }
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="diff-pane"
  class:hidden={!visible}
  bind:this={containerRef}
  onmousedowncapture={focusPane}
>
  <div class="diff-toolbar">
    <span class="diff-file-path">{diffContext.file_path}</span>
    {#if !readOnly}
      <div class="diff-actions">
        <Button variant="secondary" onclick={handleReject} disabled={accepting || rejecting} style="padding:4px 12px;border-radius:4px;font-size: 0.923rem;font-weight:500">
          {rejecting ? 'Rejecting...' : 'Reject'}
        </Button>
        <Button variant="primary" onclick={handleAccept} disabled={accepting || rejecting} style="padding:4px 12px;border-radius:4px;font-size: 0.923rem;font-weight:500">
          {accepting ? 'Saving...' : 'Accept'}
        </Button>
      </div>
    {/if}
  </div>
  <div class="diff-content"></div>
</div>

<style>
  .diff-pane {
    display: flex;
    flex-direction: column;
    flex: 1;
    min-height: 0;
    min-width: 0;
    background: var(--bg-dark);
    overflow: hidden;
  }

  .diff-pane.hidden {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    opacity: 0;
    pointer-events: none;
    z-index: -1;
  }

  .diff-toolbar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    flex-shrink: 0;
  }

  .diff-file-path {
    font-size: 0.923rem;
    color: var(--fg-dim);
    font-family: Menlo, monospace;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 60%;
  }

  .diff-actions {
    display: flex;
    gap: 8px;
  }

  .diff-content {
    flex: 1;
    position: relative;
    min-height: 0;
    overflow: hidden;
  }

  .diff-pane :global(.cm-mergeView) {
    position: absolute;
    inset: 0;
    overflow-y: auto;
    overflow-x: hidden;
  }

  .diff-pane :global(.cm-scroller) {
    overflow: visible !important;
  }

  /* Changed line backgrounds — mix highlight color with editor bg */
  .diff-pane :global(.cm-merge-a .cm-changedLine),
  .diff-pane :global(.cm-deletedLine) {
    background-color: color-mix(in srgb, var(--red) 15%, var(--bg-dark)) !important;
  }

  .diff-pane :global(.cm-merge-b .cm-changedLine) {
    background-color: color-mix(in srgb, var(--green) 15%, var(--bg-dark)) !important;
  }

  /* Inline changed text highlight — makes the actual diff characters pop */
  .diff-pane :global(.cm-deletedText) {
    background-color: color-mix(in srgb, var(--red) 35%, var(--bg-dark)) !important;
  }

  .diff-pane :global(.cm-changedText) {
    background-color: color-mix(in srgb, var(--green) 35%, var(--bg-dark)) !important;
  }

  /* Gutter indicators for changed lines */
  .diff-pane :global(.cm-merge-a .cm-changedLineGutter),
  .diff-pane :global(.cm-deletedLineGutter) {
    background-color: color-mix(in srgb, var(--red) 60%, var(--bg-dark)) !important;
  }

  .diff-pane :global(.cm-merge-b .cm-changedLineGutter) {
    background-color: color-mix(in srgb, var(--green) 60%, var(--bg-dark)) !important;
  }

  /* Deleted chunk (whole-block deletion) */
  .diff-pane :global(.cm-deletedChunk) {
    background-color: color-mix(in srgb, var(--red) 10%, var(--bg-dark)) !important;
  }

  /* Collapsed unchanged lines indicator */
  .diff-pane :global(.cm-collapsedLines) {
    color: var(--fg-dim);
    background: var(--bg-medium);
    border-color: var(--bg-light);
  }
</style>
