<script lang="ts">
  /**
   * The Files column (docs/file-tree.md §3): one per window, between the workspace sidebar
   * and the terminal area. Mounted only while open; mounting subscribes, unmounting closes
   * the subscription (the view is kept for the next open).
   *
   * Its width is the parent's business (+page.svelte, ghost resize): nothing in here may
   * change it, because a width change at a streaming agent duplicates its scrollback.
   */
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { relocateStore } from '$lib/stores/relocate.svelte';
  import { ICON_SPRITE } from '$lib/filetree/icons';
  import FilesHeader from './FilesHeader.svelte';
  import TreeBody from './TreeBody.svelte';

  const rowH = $derived(preferencesStore.fileTreeDensity === 'compact' ? 20 : preferencesStore.fileTreeDensity === 'comfortable' ? 24 : 22);

  onMount(() => {
    void fileTreeStore.attach();
  });
  onDestroy(() => fileTreeStore.detach());

  // Follow: the active tab (or workspace) changed. Debounced in the store; an editor tab
  // only reveals its file there, never re-roots.
  $effect(() => {
    void workspacesStore.activeWorkspaceId;
    void workspacesStore.activeTab?.id;
    untrack(() => fileTreeStore.scheduleResolve(false));
  });

  $effect(() => {
    const hidden = preferencesStore.fileTreeShowHidden;
    const ignored = preferencesStore.fileTreeShowIgnored;
    untrack(() => fileTreeStore.setFilters(hidden, ignored));
  });

  // Editor dots: which rows have an editor tab, recomputed when the editors or the root change.
  $effect(() => {
    void fileTreeStore.subscribed;
    const tabs: { id: string; path: string }[] = [];
    for (const ws of workspacesStore.workspaces) {
      for (const p of ws.panes) {
        for (const t of p.tabs) {
          if (t.tab_type === 'editor' && t.editor_file && !t.editor_file.is_remote) tabs.push({ id: t.id, path: t.editor_file.file_path });
        }
      }
    }
    untrack(() => void fileTreeStore.syncEditorMarks(tabs));
  });

  const place = $derived(fileTreeStore.place);
  const status = $derived(fileTreeStore.status);

  // Asked for focus (Cmd+Shift+B) and there is no tree to take it — a missing folder, an ssh
  // tab, an error: the column takes it, so the shortcut's next press closes it as promised.
  // Still resolving: wait, the tree will.
  let columnEl = $state<HTMLElement | null>(null);
  $effect(() => {
    if (!fileTreeStore.focusPending || !columnEl) return;
    const settled = status === 'missing' || status === 'remote' || status === 'gone' || status === 'error'
      || (status === 'none' && !!fileTreeStore.statusText);
    if (!settled) return;
    untrack(() => fileTreeStore.takeFocus());
    void tick().then(() => columnEl?.focus({ preventScroll: true }));
  });

  /** The footer speaks only when there is something to say (§3.2): no permanent "all good". */
  const footer = $derived.by(() => {
    const parts: string[] = [];
    const h = fileTreeStore.health;
    if (h?.note) parts.push(h.note);
    if (place?.fs_class === 'network' && !h?.note) parts.push('network mount: not watched');
    if (place?.shallow && status === 'ready') parts.push('large folder: only open folders are watched');
    const n = fileTreeStore.selection.size;
    if (n > 1) parts.push(`${n} selected`);
    return parts;
  });
</script>

<!-- data-filetree scopes the app's keyboard (+layout isAppShortcut): while focus is anywhere
     in the column only app shortcuts reach the app, so Cmd+D can't split a pane from here. -->
<aside
  class="files-column"
  data-filetree
  aria-label="Files"
  tabindex="-1"
  bind:this={columnEl}
  onfocusin={() => { fileTreeStore.focused = true; }}
  onfocusout={(e) => { if (!columnEl?.contains(e.relatedTarget as Node | null)) fileTreeStore.focused = false; }}
  style="--ft-row-h: {rowH}px"
>
  <!-- eslint-disable-next-line svelte/no-at-html-tags -->
  {@html ICON_SPRITE}
  <FilesHeader />

  <div class="body" class:mono={preferencesStore.fileTreeIcons === 'mono'}>
    {#if status === 'ready' || status === 'opening'}
      <TreeBody {rowH} />
    {:else if status === 'missing'}
      <div class="state">
        <p>Folder missing: <span class="path">{fileTreeStore.statusText}</span></p>
        <p class="hint">It may have been moved or renamed. Files never falls back to your home folder.</p>
        <button class="link" onclick={() => relocateStore.open({ mode: 'repoint', old: fileTreeStore.statusText ?? '', suggested: null })}>Move project…</button>
      </div>
    {:else if status === 'remote'}
      <div class="state">
        <p>This tab is on <strong>{place?.host_label ?? 'an ssh host'}</strong>.</p>
        <p class="hint">Files shows folders on this computer for now; folders on ssh hosts come in a later version.</p>
      </div>
    {:else if status === 'gone'}
      <div class="state">
        <p>This folder no longer exists.</p>
        {#if fileTreeStore.statusText}<p class="path">{fileTreeStore.statusText}</p>{/if}
        <div class="actions">
          {#if fileTreeStore.pinned}
            <button class="link" onclick={() => fileTreeStore.unpin()}>Follow the tab</button>
          {:else}
            <button class="link" onclick={() => fileTreeStore.scheduleResolve(true)}>Follow the tab</button>
          {/if}
          <span class="sep">·</span>
          <span class="hint">or pick another from the folder name above</span>
        </div>
      </div>
    {:else if status === 'resolving'}
      <div class="state"><p class="hint">Finding this tab’s folder…</p></div>
    {:else}
      <div class="state">
        <p class="hint">{fileTreeStore.statusText ?? 'Open a terminal tab, or pick a folder from the name above.'}</p>
      </div>
    {/if}
  </div>

  {#if footer.length > 0}
    <div class="footer">
      {#each footer as line (line)}<div class="footer-line">{line}</div>{/each}
    </div>
  {/if}

  <div class="sr-only" aria-live="polite">{fileTreeStore.announcement}</div>
</aside>

<style>
  .files-column {
    /* Icon colours: theme tokens pulled toward the foreground so every glyph reads on the
       theme's background (§3.2). */
    --ft-ic-blue: color-mix(in srgb, var(--accent) 80%, var(--fg));
    --ft-ic-cyan: color-mix(in srgb, var(--cyan) 80%, var(--fg));
    --ft-ic-green: color-mix(in srgb, var(--green) 80%, var(--fg));
    --ft-ic-yellow: color-mix(in srgb, var(--yellow) 80%, var(--fg));
    --ft-ic-red: color-mix(in srgb, var(--red) 80%, var(--fg));
    --ft-ic-magenta: color-mix(in srgb, var(--magenta) 80%, var(--fg));
    --ft-ic-orange: color-mix(in srgb, color-mix(in srgb, var(--red) 50%, var(--yellow)) 80%, var(--fg));
    --ft-ic-dim: color-mix(in srgb, var(--fg-dim) 60%, var(--fg));
    --ft-ic-fg: var(--fg);
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    background: var(--bg-medium);
    color: var(--fg);
    font-size: 0.923rem;
    overflow: hidden;
  }

  .body {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
  }

  .body.mono {
    --ft-ic-blue: var(--fg-dim);
    --ft-ic-cyan: var(--fg-dim);
    --ft-ic-green: var(--fg-dim);
    --ft-ic-yellow: var(--fg-dim);
    --ft-ic-red: var(--fg-dim);
    --ft-ic-magenta: var(--fg-dim);
    --ft-ic-orange: var(--fg-dim);
    --ft-ic-dim: var(--fg-dim);
  }

  .state {
    padding: 14px 12px;
    color: var(--fg);
    line-height: 1.5;
    overflow-wrap: anywhere;
  }

  .state p {
    margin: 0 0 8px;
  }

  .hint {
    color: var(--fg-dim);
    font-size: 0.92em;
  }

  .path {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.92em;
  }

  .actions {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
    align-items: baseline;
  }

  .sep {
    color: var(--fg-dim);
  }

  .link {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    font: inherit;
  }

  .link:hover {
    text-decoration: underline;
  }

  .footer {
    flex-shrink: 0;
    border-top: 1px solid var(--bg-light);
    padding: 4px 10px;
    font-size: 0.85em;
    color: var(--fg-dim);
    overflow-wrap: anywhere;
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    overflow: hidden;
    clip: rect(0 0 0 0);
    white-space: nowrap;
  }
</style>
