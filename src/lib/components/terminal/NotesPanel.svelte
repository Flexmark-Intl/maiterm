<script lang="ts">
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { untrack } from 'svelte';
  import { marked } from 'marked';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import type { WorkspaceNote } from '$lib/tauri/types';

  interface Props {
    tabId: string;
    workspaceId: string;
    paneId: string;
    notes: string | null;
    notesMode: string | null;
    workspaceNotes: WorkspaceNote[];
    onclose: () => void;
  }

  let { tabId, workspaceId, paneId, notes, notesMode, workspaceNotes, onclose }: Props = $props();

  // Scope is persisted in preferences so it survives app restarts
  const scope = $derived(preferencesStore.notesScope);
  // svelte-ignore state_referenced_locally -- props used as initial values; local state is source of truth after mount
  let value = $state(notes ?? '');
  // svelte-ignore state_referenced_locally
  let mode = $state<'source' | 'render'>((notesMode ?? 'source') as 'source' | 'render');
  let textareaEl = $state<HTMLTextAreaElement | null>(null);
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  // Workspace notes state
  let editingNoteId = $state<string | null>(null);
  let wsValue = $state('');
  let wsMode = $state<'source' | 'render'>('source');
  let wsSaveTimer: ReturnType<typeof setTimeout> | null = null;
  let deletingNoteId = $state<string | null>(null);
  let confirmingTabClear = $state(false);

  const textareaStyle = $derived(
    `font-family: '${preferencesStore.fontFamily}', monospace; font-size: ${preferencesStore.fontSize}px; white-space: ${preferencesStore.notesWordWrap ? 'pre-wrap' : 'pre'}; overflow-x: ${preferencesStore.notesWordWrap ? 'hidden' : 'auto'};`
  );
  const renderStyle = $derived(
    `font-family: '${preferencesStore.notesFontFamily}', monospace; font-size: ${preferencesStore.notesFontSize}px; word-wrap: ${preferencesStore.notesWordWrap ? 'break-word' : 'normal'};`
  );

  // Configure marked for safe rendering with interactive checkboxes
  const renderer = new marked.Renderer();
  let checkboxIndex = 0;
  renderer.checkbox = function({ checked }) {
    const i = checkboxIndex++;
    return `<input type="checkbox" data-index="${i}"${checked ? ' checked=""' : ''}>`;
  };
  marked.setOptions({ breaks: true, gfm: true, renderer });

  const renderedHtml = $derived.by(() => {
    checkboxIndex = 0;
    const src = scope === 'tab' ? value : wsValue;
    return marked.parse(src) as string;
  });

  // Sync external changes to notes mode (e.g. from MCP tools)
  $effect(() => {
    const propMode = (notesMode ?? 'source') as 'source' | 'render';
    if (propMode !== untrack(() => mode)) {
      mode = propMode;
    }
  });

  // Sync external changes to tab notes (e.g. from MCP tools) to local state.
  // Read `value` inside untrack() so this effect only re-runs when the `notes`
  // prop changes, not when the user types (which would reset their input).
  $effect(() => {
    const propValue = notes ?? '';
    if (propValue !== untrack(() => value)) {
      value = propValue;
    }
  });

  // Focus at end of content when entering source mode
  $effect(() => {
    if (scope === 'tab' && mode === 'source' && textareaEl) {
      textareaEl.focus();
      textareaEl.selectionStart = textareaEl.selectionEnd = textareaEl.value.length;
    }
  });

  function save() {
    const content = untrack(() => value);
    const n = content.trim() ? content : null;
    workspacesStore.setTabNotes(workspaceId, paneId, tabId, n);
  }

  // Debounced auto-save for tab notes: 1s after last keystroke
  $effect(() => {
    if (scope !== 'tab') return;
    void value;
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(() => save(), 1000);
    return () => {
      if (saveTimer) clearTimeout(saveTimer);
    };
  });

  // Debounced auto-save for workspace notes
  $effect(() => {
    if (scope !== 'workspace' || !editingNoteId) return;
    void wsValue;
    if (wsSaveTimer) clearTimeout(wsSaveTimer);
    wsSaveTimer = setTimeout(() => saveWorkspaceNote(), 1000);
    return () => {
      if (wsSaveTimer) clearTimeout(wsSaveTimer);
    };
  });

  function saveWorkspaceNote() {
    const noteId = untrack(() => editingNoteId);
    const content = untrack(() => wsValue);
    const m = untrack(() => wsMode);
    if (!noteId) return;
    workspacesStore.updateWorkspaceNote(workspaceId, noteId, content, m);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      e.stopPropagation();
      if (scope === 'tab') {
        if (saveTimer) clearTimeout(saveTimer);
        save();
      } else if (editingNoteId) {
        if (wsSaveTimer) clearTimeout(wsSaveTimer);
        saveWorkspaceNote();
        wsView = 'list';
        return;
      }
      onclose();
    }
    if (e.key === 'Tab') {
      e.preventDefault();
      const target = e.target as HTMLTextAreaElement;
      const start = target.selectionStart;
      const end = target.selectionEnd;
      if (scope === 'tab') {
        value = value.substring(0, start) + '  ' + value.substring(end);
      } else {
        wsValue = wsValue.substring(0, start) + '  ' + wsValue.substring(end);
      }
      requestAnimationFrame(() => {
        target.selectionStart = target.selectionEnd = start + 2;
      });
    }
  }

  function toggleMode() {
    if (scope === 'tab') {
      mode = mode === 'source' ? 'render' : 'source';
      workspacesStore.setTabNotesMode(workspaceId, paneId, tabId, mode);
    } else {
      wsMode = wsMode === 'source' ? 'render' : 'source';
      if (editingNoteId) {
        workspacesStore.updateWorkspaceNote(workspaceId, editingNoteId, wsValue, wsMode);
      }
    }
  }

  function currentMode(): 'source' | 'render' {
    return scope === 'tab' ? mode : wsMode;
  }

  // Drag-resize from left edge
  let dragging = $state(false);
  let dragStartX = 0;
  let dragStartWidth = 0;
  let panelEl = $state<HTMLElement | null>(null);

  function handleResizePointerDown(e: PointerEvent) {
    e.preventDefault();
    dragging = true;
    dragStartX = e.clientX;
    dragStartWidth = preferencesStore.notesWidth;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handleResizePointerMove(e: PointerEvent) {
    if (!dragging) return;
    const delta = dragStartX - e.clientX;
    const paneWidth = panelEl?.parentElement?.clientWidth ?? window.innerWidth;
    const maxWidth = Math.floor(paneWidth * 0.9);
    const newWidth = Math.max(200, Math.min(maxWidth, dragStartWidth + delta));
    preferencesStore.setNotesWidth(newWidth);
  }

  function handleResizePointerUp() {
    dragging = false;
  }

  function handleCheckboxToggle(src: string, idx: number): string {
    let count = 0;
    return src.replace(/- \[([ xX])\]/g, (match, ch) => {
      if (count++ === idx) {
        return ch === ' ' ? '- [x]' : '- [ ]';
      }
      return match;
    });
  }

  function handleRenderClick(e: MouseEvent) {
    const target = e.target as HTMLElement;

    const checkbox = target instanceof HTMLInputElement && target.type === 'checkbox'
      ? target
      : target.closest('li')?.querySelector('input[type="checkbox"]') as HTMLInputElement | null;
    if (checkbox?.dataset.index != null) {
      e.preventDefault();
      const idx = parseInt(checkbox.dataset.index, 10);
      if (scope === 'tab') {
        value = handleCheckboxToggle(value, idx);
        save();
      } else {
        wsValue = handleCheckboxToggle(wsValue, idx);
        saveWorkspaceNote();
      }
      return;
    }

    const anchor = target.closest('a');
    if (anchor?.href) {
      e.preventDefault();
      shellOpen(anchor.href);
    }
  }

  function clearTabNotes() {
    if (saveTimer) clearTimeout(saveTimer);
    value = '';
    workspacesStore.setTabNotes(workspaceId, paneId, tabId, null);
    confirmingTabClear = false;
    if (mode === 'render') {
      mode = 'source';
      workspacesStore.setTabNotesMode(workspaceId, paneId, tabId, 'source');
    }
  }

  async function moveTabNoteToWorkspace() {
    if (!value.trim()) return;
    if (saveTimer) clearTimeout(saveTimer);
    save();
    await workspacesStore.addWorkspaceNote(workspaceId, value, mode !== 'source' ? mode : null);
    value = '';
    workspacesStore.setTabNotes(workspaceId, paneId, tabId, null);
    if (mode === 'render') {
      mode = 'source';
      workspacesStore.setTabNotesMode(workspaceId, paneId, tabId, 'source');
    }
  }

  // Workspace note helpers
  async function openNote(note: WorkspaceNote) {
    editingNoteId = note.id;
    wsValue = note.content;
    wsMode = (note.mode ?? 'source') as 'source' | 'render';
    wsView = 'editor';
  }

  async function createNewNote() {
    const note = await workspacesStore.addWorkspaceNote(workspaceId, '', null);
    if (note) {
      editingNoteId = note.id;
      wsValue = '';
      wsMode = 'source';
      wsView = 'editor';
    }
  }

  async function confirmDeleteNote(noteId: string) {
    await workspacesStore.deleteWorkspaceNote(workspaceId, noteId);
    deletingNoteId = null;
    if (editingNoteId === noteId) {
      editingNoteId = null;
    }
  }

  function noteTitle(content: string): string {
    const firstLine = content.split('\n')[0]?.trim();
    if (!firstLine) return 'Untitled';
    return firstLine.length > 60 ? firstLine.slice(0, 60) + '...' : firstLine;
  }

  function relativeTime(isoDate: string): string {
    const diff = Date.now() - new Date(isoDate).getTime();
    const mins = Math.floor(diff / 60000);
    if (mins < 1) return 'just now';
    if (mins < 60) return `${mins}m ago`;
    const hours = Math.floor(mins / 60);
    if (hours < 24) return `${hours}h ago`;
    const days = Math.floor(hours / 24);
    if (days === 1) return 'yesterday';
    if (days < 30) return `${days}d ago`;
    return new Date(isoDate).toLocaleDateString();
  }

  // Workspace sub-view: 'list' or 'editor'
  let wsView = $state<'list' | 'editor'>('list');

  // When switching to workspace scope, default to list view
  $effect(() => {
    if (scope === 'workspace') {
      // If editing a note that was deleted, go back to list
      if (editingNoteId && !workspaceNotes.find(n => n.id === editingNoteId)) {
        editingNoteId = null;
        wsView = 'list';
      }
    }
  });

  // Sort workspace notes by most recent first
  const sortedWorkspaceNotes = $derived.by(() => {
    return [...workspaceNotes].sort((a, b) =>
      new Date(b.updated_at).getTime() - new Date(a.updated_at).getTime()
    );
  });

  const showWsList = $derived(scope === 'workspace' && wsView === 'list');
  const showWsEditor = $derived(scope === 'workspace' && wsView === 'editor');

</script>

<div class="notes-panel" bind:this={panelEl} style:width="{preferencesStore.notesWidth}px" style:min-width="{preferencesStore.notesWidth}px">
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="resize-handle"
    onpointerdown={handleResizePointerDown}
    onpointermove={handleResizePointerMove}
    onpointerup={handleResizePointerUp}
  ></div>
  <div class="notes-header">
    <div class="scope-toggle">
      <button
        class="scope-btn"
        class:active={scope === 'tab'}
        onclick={() => preferencesStore.setNotesScope('tab')}
        title="Tab notes"
      >Tab</button>
      <button
        class="scope-btn"
        class:active={scope === 'workspace'}
        onclick={() => preferencesStore.setNotesScope('workspace')}
        title="Workspace notes"
      >Workspace</button>
    </div>
    <div class="header-actions">
      {#if showWsEditor}
        <IconButton
          tooltip="All workspace notes"
          onclick={() => {
            if (wsSaveTimer) clearTimeout(wsSaveTimer);
            if (editingNoteId) saveWorkspaceNote();
            wsView = 'list';
          }}
        >
          <Icon name="list" />
        </IconButton>
      {/if}
      {#if showWsList}
        <!-- No mode toggle in list view -->
      {:else}
        <IconButton
          tooltip={currentMode() === 'source' ? 'Preview' : 'Edit'}
          active={currentMode() === 'render'}
          onclick={toggleMode}
        >
          {#if currentMode() === 'source'}
            <Icon name="eye" />
          {:else}
            <Icon name="pencil" />
          {/if}
        </IconButton>
      {/if}
      {#if scope === 'tab' && value.trim()}
        <IconButton
          tooltip="Move to workspace notes"
          onclick={moveTabNoteToWorkspace}
        >
          <Icon name="arrow-right" size={14} />
        </IconButton>
        {#if confirmingTabClear}
          <span class="delete-confirm">
            Clear?
            <button class="confirm-yes" onclick={clearTabNotes}>Yes</button>
            <button class="confirm-no" onclick={() => confirmingTabClear = false}>No</button>
          </span>
        {:else}
          <IconButton
            tooltip="Clear notes"
            danger
            onclick={() => confirmingTabClear = true}
          >
            <Icon name="trash" />
          </IconButton>
        {/if}
      {/if}
      <IconButton
        tooltip="Close notes"
        style="font-size: 1.077rem"
        onclick={() => {
          if (saveTimer) clearTimeout(saveTimer);
          if (wsSaveTimer) clearTimeout(wsSaveTimer);
          if (scope === 'tab') save();
          else if (editingNoteId) saveWorkspaceNote();
          onclose();
        }}
      >&times;</IconButton>
    </div>
  </div>

  {#if scope === 'tab'}
    {#if mode === 'source'}
      <textarea
        class="notes-textarea"
        bind:value={value}
        bind:this={textareaEl}
        onkeydown={handleKeydown}
        placeholder="Jot down commands, notes, connection details..."
        spellcheck="false"
        style={textareaStyle}
      ></textarea>
    {:else}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="notes-render"
        onclick={handleRenderClick}
        style={renderStyle}
      >{@html renderedHtml}</div>
    {/if}
  {:else if showWsList}
    <div class="ws-notes-list">
      <button class="new-note-btn" onclick={createNewNote}>+ New Note</button>
      {#each sortedWorkspaceNotes as note (note.id)}
        <div class="ws-note-card">
          <button class="ws-note-content" onclick={() => openNote(note)}>
            <span class="ws-note-title">{noteTitle(note.content)}</span>
            <span class="ws-note-date">{relativeTime(note.updated_at)}</span>
          </button>
          <div class="ws-note-actions">
            {#if deletingNoteId === note.id}
              <span class="delete-confirm">
                Delete?
                <button class="confirm-yes" onclick={() => confirmDeleteNote(note.id)}>Yes</button>
                <button class="confirm-no" onclick={() => deletingNoteId = null}>No</button>
              </span>
            {:else}
              <IconButton tooltip="Delete note" danger onclick={() => deletingNoteId = note.id} style="font-size: 1.077rem">&times;</IconButton>
            {/if}
          </div>
        </div>
      {/each}
    </div>
  {:else if showWsEditor}
    {#if wsMode === 'source'}
      <textarea
        class="notes-textarea"
        bind:value={wsValue}
        onkeydown={handleKeydown}
        placeholder="Write workspace notes..."
        spellcheck="false"
        style={textareaStyle}
      ></textarea>
    {:else}
      <!-- svelte-ignore a11y_click_events_have_key_events -->
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div
        class="notes-render"
        onclick={handleRenderClick}
        style={renderStyle}
      >{@html renderedHtml}</div>
    {/if}
  {/if}
</div>

<style>
  .notes-panel {
    display: flex;
    flex-direction: column;
    background: var(--bg-medium);
    border-left: 1px solid var(--bg-light);
    position: relative;
  }

  .resize-handle {
    position: absolute;
    top: 0;
    left: -3px;
    width: 6px;
    height: 100%;
    cursor: col-resize;
    z-index: 10;
  }

  .resize-handle:hover,
  .resize-handle:active {
    background: var(--accent);
    opacity: 0.3;
  }

  .notes-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 6px 12px;
    border-bottom: 1px solid var(--bg-light);
    flex-shrink: 0;
  }

  .scope-toggle {
    display: flex;
    gap: 2px;
    background: var(--bg-dark);
    border-radius: 4px;
    padding: 2px;
  }

  .scope-btn {
    font-size: 0.846rem;
    padding: 2px 8px;
    border-radius: 3px;
    color: var(--fg-dim);
    background: transparent;
    transition: background 0.1s, color 0.1s;
    cursor: pointer;
    border: none;
  }

  .scope-btn:hover {
    color: var(--fg);
  }

  .scope-btn.active {
    background: var(--bg-light);
    color: var(--fg);
  }

  .header-actions {
    display: flex;
    align-items: center;
    gap: 4px;
  }


  .notes-textarea {
    flex: 1;
    resize: none;
    background: var(--bg-dark);
    color: var(--fg);
    border: none;
    padding: 12px;
    line-height: 1.5;
    outline: none;
  }

  .notes-textarea::placeholder {
    color: var(--fg-dim);
    opacity: 0.5;
  }

  .notes-render {
    flex: 1;
    overflow-y: auto;
    overflow-x: auto;
    padding: 12px;
    background: var(--bg-dark);
    color: var(--fg);
    line-height: 1.6;
    /* Override the global `user-select: none` so rendered notes can be selected/copied */
    -webkit-user-select: text;
    user-select: text;
    cursor: text;
  }

  .notes-render :global(h1),
  .notes-render :global(h2),
  .notes-render :global(h3),
  .notes-render :global(h4),
  .notes-render :global(h5),
  .notes-render :global(h6) {
    margin: 0.8em 0 0.4em;
    color: var(--fg);
    line-height: 1.3;
  }

  .notes-render :global(h1) { font-size: 1.3em; }
  .notes-render :global(h2) { font-size: 1.15em; }
  .notes-render :global(h3) { font-size: 1.05em; }
  .notes-render :global(h4),
  .notes-render :global(h5),
  .notes-render :global(h6) { font-size: 1em; }

  .notes-render :global(p) {
    margin: 0 0 0.6em;
  }

  .notes-render :global(code) {
    background: var(--bg-light);
    padding: 1px 5px;
    border-radius: 3px;
    font-family: var(--font-family, 'Menlo'), monospace;
    font-size: 0.9em;
  }

  .notes-render :global(pre) {
    background: var(--bg-medium);
    padding: 8px 10px;
    border-radius: 4px;
    overflow-x: auto;
    margin: 0 0 0.6em;
  }

  .notes-render :global(pre code) {
    background: none;
    padding: 0;
  }

  .notes-render :global(ul),
  .notes-render :global(ol) {
    margin: 0 0 0.6em;
    padding-left: 1.5em;
  }

  .notes-render :global(li) {
    margin-bottom: 0.2em;
  }

  .notes-render :global(li:has(> input[type="checkbox"])) {
    list-style: none;
    margin-left: -1.5em;
    cursor: pointer;
  }

  .notes-render :global(input[type="checkbox"]) {
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
    cursor: pointer;
  }

  .notes-render :global(input[type="checkbox"]:checked) {
    background: var(--accent);
    border-color: var(--accent);
  }

  .notes-render :global(input[type="checkbox"]:checked::after) {
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

  .notes-render :global(blockquote) {
    border-left: 3px solid var(--bg-light);
    margin: 0 0 0.6em;
    padding: 4px 12px;
    color: var(--fg-dim);
  }

  .notes-render :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .notes-render :global(a:hover) {
    text-decoration: underline;
  }

  .notes-render :global(hr) {
    border: none;
    border-top: 1px solid var(--bg-light);
    margin: 0.8em 0;
  }

  .notes-render :global(table) {
    border-collapse: collapse;
    margin: 0 0 0.6em;
    font-size: 0.9em;
    white-space: nowrap;
  }

  .notes-render :global(th),
  .notes-render :global(td) {
    border: 1px solid var(--bg-light);
    padding: 4px 8px;
    text-align: left;
  }

  .notes-render :global(th) {
    background: var(--bg-medium);
    font-weight: 600;
  }

  /* Workspace notes list */
  .ws-notes-list {
    flex: 1;
    overflow-y: auto;
    padding: 8px;
    background: var(--bg-dark);
  }

  .new-note-btn {
    width: 100%;
    padding: 8px;
    margin-bottom: 8px;
    background: transparent;
    color: var(--accent);
    border: 1px dashed var(--bg-light);
    border-radius: 6px;
    font-size: 0.923rem;
    cursor: pointer;
    transition: background 0.1s;
  }

  .new-note-btn:hover {
    background: var(--bg-medium);
  }

  .ws-note-card {
    display: flex;
    align-items: center;
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    margin-bottom: 4px;
    overflow: hidden;
  }

  .ws-note-content {
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 2px;
    padding: 8px 10px;
    background: transparent;
    border: none;
    text-align: left;
    cursor: pointer;
    color: var(--fg);
    min-width: 0;
    transition: background 0.1s;
  }

  .ws-note-content:hover {
    background: var(--bg-medium);
  }

  .ws-note-title {
    font-size: 0.923rem;
    color: var(--fg);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .ws-note-date {
    font-size: 0.769rem;
    color: var(--fg-dim);
  }

  .ws-note-actions {
    padding: 0 8px;
    flex-shrink: 0;
  }


  .delete-confirm {
    font-size: 0.846rem;
    color: var(--fg-dim);
    display: flex;
    align-items: center;
    gap: 4px;
  }

  .confirm-yes, .confirm-no {
    font-size: 0.846rem;
    padding: 1px 6px;
    border-radius: 3px;
    border: none;
    cursor: pointer;
  }

  .confirm-yes {
    background: #f7768e;
    color: var(--bg-dark);
  }

  .confirm-no {
    background: var(--bg-light);
    color: var(--fg);
  }

</style>
