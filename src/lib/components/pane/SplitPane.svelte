<script lang="ts">
  import { tick, onMount } from 'svelte';
  import type { Pane } from '$lib/tauri/types';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import TerminalTabs from '$lib/components/terminal/TerminalTabs.svelte';
  import SearchBar from '$lib/components/terminal/SearchBar.svelte';
  import NotesPanel from '$lib/components/terminal/NotesPanel.svelte';
  import TasksPanel from '$lib/components/tasks/TasksPanel.svelte';
  import ComposerDock from '$lib/components/terminal/ComposerDock.svelte';
  import { pendingResumePanes, resumePane } from '$lib/stores/resumeGate.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { getSavedScrollbackText } from '$lib/tauri/commands';
  import { modLabel } from '$lib/utils/platform';

  interface Props {
    workspaceId: string;
    pane: Pane;
    isActive: boolean;
    showHeader: boolean;
  }

  let { workspaceId, pane, isActive, showHeader }: Props = $props();

  let editingName = $state(false);
  let nameValue = $state('');
  let editInput = $state<HTMLInputElement | null>(null);
  let suspendedPreview = $state<string | null>(null);

  // Load the suspended tab's saved scrollback so the resume overlay can show
  // what was happening in the tab before it was suspended.
  $effect(() => {
    const pending = pendingResumePanes.has(pane.id);
    const tabId = pane.active_tab_id;
    if (!pending || !tabId) {
      suspendedPreview = null;
      return;
    }
    let cancelled = false;
    getSavedScrollbackText(tabId, 200)
      .then((text) => { if (!cancelled) suspendedPreview = text; })
      .catch(() => { if (!cancelled) suspendedPreview = null; });
    return () => { cancelled = true; };
  });

  // Notify portaled TerminalPanes that their slots are ready
  onMount(() => {
    for (const tab of pane.tabs) {
      window.dispatchEvent(new CustomEvent('terminal-slot-ready', { detail: { tabId: tab.id } }));
    }
  });

  async function startEditing() {
    editingName = true;
    nameValue = pane.name;
    await tick();
    editInput?.select();
  }

  async function finishEditing() {
    if (nameValue.trim() && nameValue !== pane.name) {
      await workspacesStore.renamePane(workspaceId, pane.id, nameValue.trim());
    }
    editingName = false;
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      finishEditing();
    } else if (e.key === 'Escape') {
      editingName = false;
      nameValue = pane.name;
    }
  }

  async function handleClick() {
    if (!isActive) {
      await workspacesStore.setActivePane(workspaceId, pane.id);
    }
  }

  async function handleNewTerminal() {
    await workspacesStore.createTab(workspaceId, pane.id, 'Terminal 1');
  }

  async function handleClosePane(e: MouseEvent) {
    e.stopPropagation();
    const ws = workspacesStore.activeWorkspace;
    if (ws && ws.panes.length > 1) {
      await workspacesStore.deletePane(workspaceId, pane.id);
    }
  }
</script>

<div class="split-pane" class:active={isActive} data-pane-id={pane.id}>
  {#if showHeader}
    <div
      class="pane-header"
      onclick={handleClick}
      ondblclick={startEditing}
      role="button"
      tabindex="0"
      onkeydown={(e) => e.key === 'Enter' && handleClick()}
    >
      {#if editingName}
        <!-- svelte-ignore a11y_autofocus -->
        <input
          type="text"
          bind:value={nameValue}
          bind:this={editInput}
          onblur={finishEditing}
          onkeydown={handleKeydown}
          class="name-input"
          autofocus
        />
      {:else}
        <span class="pane-name">{pane.name}</span>
        <div class="pane-actions">
          <button
            class="close-btn"
            onclick={handleClosePane}
            title="Close pane"
          >
            &times;
          </button>
        </div>
      {/if}
    </div>
  {/if}

  <TerminalTabs {workspaceId} {pane} />

  {#if pane.tabs.length > 0}
    <div class="terminal-with-notes">
      <div class="terminal-column">
        <div class="terminal-area">
          {#if pane.active_tab_id}
            <SearchBar tabId={pane.active_tab_id} />
          {/if}
          {#each pane.tabs as tab (tab.id)}
            <div
              class="terminal-slot"
              class:hidden-tab={tab.id !== pane.active_tab_id}
              data-terminal-slot={tab.id}
            ></div>
          {/each}
          {#if pendingResumePanes.has(pane.id)}
            {@const activeTab = pane.tabs.find(t => t.id === pane.active_tab_id)}
            <div class="resume-overlay">
              {#if suspendedPreview}
                <pre
                  class="suspended-preview"
                  style="font-family: {preferencesStore.fontFamily}; font-size: {preferencesStore.fontSize}px;"
                >{suspendedPreview}</pre>
                <div class="preview-glass"></div>
              {/if}
              <div class="resume-controls" class:on-glass={suspendedPreview !== null}>
                <p>This tab is suspended</p>
                <button class="resume-btn" onclick={() => resumePane(pane.id)}>
                  Resume{activeTab ? ` "${activeTab.custom_name ? activeTab.name : 'terminal'}"` : ''}
                </button>
                <p class="resume-hint">or click any tab to resume it</p>
              </div>
            </div>
          {/if}
        </div>
        {#if pane.active_tab_id}
          {@const composerTab = pane.tabs.find(t => t.id === pane.active_tab_id)}
          {#if composerTab && composerTab.tab_type === 'terminal'}
            {#key composerTab.id}
              <ComposerDock
                tabId={composerTab.id}
                draft={composerTab.composer_draft ?? null}
              />
            {/key}
          {/if}
        {/if}
      </div>

      {#if pane.active_tab_id && workspacesStore.isTasksVisible(pane.active_tab_id)}
        {@const tasksTab = pane.tabs.find(t => t.id === pane.active_tab_id)}
        {#if tasksTab}
          {#key tasksTab.id}
            <TasksPanel tabId={tasksTab.id} {workspaceId} onclose={() => workspacesStore.toggleTasks(tasksTab.id)} />
          {/key}
        {/if}
      {/if}

      {#if pane.active_tab_id && workspacesStore.isNotesVisible(pane.active_tab_id)}
        {@const activeTab = pane.tabs.find(t => t.id === pane.active_tab_id)}
        {@const ws = workspacesStore.workspaces.find(w => w.id === workspaceId)}
        {#if activeTab}
          {#key activeTab.id}
            <NotesPanel
              tabId={activeTab.id}
              {workspaceId}
              paneId={pane.id}
              notes={activeTab.notes}
              notesMode={activeTab.notes_mode}
              workspaceNotes={ws?.workspace_notes ?? []}
              onclose={() => workspacesStore.toggleNotes(activeTab.id)}
            />
          {/key}
        {/if}
      {/if}
    </div>
  {:else}
    <div class="empty-pane">
      <div class="empty-logo" role="img" aria-label="maiTerm"></div>
      <button class="new-terminal-btn" onclick={handleNewTerminal}>
        New Terminal
      </button>
      <span class="empty-hint"><kbd>{modLabel}</kbd> + <kbd>T</kbd></span>
    </div>
  {/if}
</div>

<style>
  .split-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
  }

  .split-pane:last-child {
    border-right: none;
  }

  .pane-header {
    height: var(--header-height);
    display: flex;
    align-items: center;
    padding: 0 16px;
    background: var(--bg-medium);
    border-bottom: 1px solid var(--bg-light);
    cursor: pointer;
  }

  .pane-name {
    flex: 1;
    font-weight: 500;
    color: var(--fg);
  }

  .pane-actions {
    display: flex;
    align-items: center;
    margin-left: auto;
    opacity: 0;
    transition: opacity 0.15s ease;
  }

  .pane-header:hover .pane-actions {
    opacity: 1;
  }

  .close-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 22px;
    height: 22px;
    padding: 0;
    color: var(--fg-dim);
    border-radius: 4px;
    font-size: 1.077rem;
    transition: background 0.1s, color 0.1s;
  }

  .close-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .name-input {
    font-weight: 500;
    padding: 4px 8px;
  }

  .terminal-with-notes {
    flex: 1;
    display: flex;
    min-height: 0;
    overflow: hidden;
  }

  .terminal-column {
    flex: 1;
    display: flex;
    flex-direction: column;
    min-height: 0;
    min-width: 0;
    position: relative;
  }

  .terminal-area {
    flex: 1;
    display: flex;
    min-height: 0;
    min-width: 0;
    position: relative;
    overflow: hidden;
  }

  .terminal-slot {
    flex: 1;
    display: flex;
    min-height: 0;
    min-width: 0;
  }

  .terminal-slot.hidden-tab {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    bottom: 0;
    opacity: 0;
    pointer-events: none;
    z-index: -1;
  }

  .empty-pane {
    flex: 1;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    gap: 16px;
    background: var(--bg-dark);
  }

  .empty-logo {
    height: 48px;
    aspect-ratio: 1725 / 320;
    opacity: 0.3;
    background: var(--logo-url, url(/logo-light.png)) center / contain no-repeat;
  }

  .new-terminal-btn {
    padding: 8px 20px;
    border-radius: 6px;
    background: var(--accent);
    color: var(--bg-dark);
    font-size: 1rem;
    font-weight: 600;
    cursor: pointer;
    transition: opacity 0.15s;
  }

  .new-terminal-btn:hover {
    opacity: 0.85;
  }

  .empty-hint {
    font-size: 0.923rem;
    color: var(--fg-dim);
  }

  .empty-hint kbd {
    padding: 1px 5px;
    background: var(--bg-medium);
    border-radius: 3px;
    font-family: inherit;
    font-size: 0.846rem;
  }

  .resume-overlay {
    position: absolute;
    inset: 0;
    display: flex;
    flex-direction: column;
    align-items: center;
    justify-content: center;
    background: var(--bg-dark);
    z-index: 5;
    color: var(--fg-dim);
    overflow: hidden;
  }

  .suspended-preview {
    position: absolute;
    left: 0;
    right: 0;
    bottom: 0;
    max-height: 100%;
    margin: 0;
    padding: 8px;
    overflow: hidden;
    white-space: pre;
    line-height: 1.3;
    color: var(--fg);
    opacity: 0.75;
    pointer-events: none;
    user-select: none;
  }

  .preview-glass {
    position: absolute;
    inset: 0;
    backdrop-filter: blur(1.5px) saturate(0.7);
    -webkit-backdrop-filter: blur(1.5px) saturate(0.7);
    background: color-mix(in srgb, var(--bg-dark) 45%, transparent);
    pointer-events: none;
  }

  .resume-controls {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 12px;
  }

  .resume-controls.on-glass {
    padding: 20px 32px;
    border-radius: 10px;
    border: 1px solid color-mix(in srgb, var(--bg-light) 60%, transparent);
    background: color-mix(in srgb, var(--bg-dark) 80%, transparent);
    backdrop-filter: blur(10px);
    -webkit-backdrop-filter: blur(10px);
  }

  .resume-overlay .resume-btn {
    padding: 8px 24px;
    background: var(--bg-medium);
    color: var(--fg);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    cursor: pointer;
    font-family: inherit;
    font-size: 0.9em;
    transition: background 0.15s, border-color 0.15s;
  }

  .resume-overlay .resume-btn:hover {
    background: var(--bg-light);
    border-color: var(--accent);
  }

  .resume-hint {
    font-size: 0.8em;
    color: var(--fg-dim);
    opacity: 0.7;
  }
</style>
