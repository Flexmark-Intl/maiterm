<script lang="ts">
  /**
   * The Files header (docs/file-tree.md §3.2): the root's name (a click opens the root
   * switcher), a host chip on a remote root, and Refresh / Collapse All / Pin / "⋯".
   * Phase 1 is read-only, so New File and New Folder arrive with file operations.
   */
  import { fileTreeStore } from '$lib/stores/fileTree.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import * as commands from '$lib/tauri/commands';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import RootSwitcher from './RootSwitcher.svelte';
  import { lastSegment } from '$lib/filetree/paths';
  import { modSymbol } from '$lib/utils/platform';

  const place = $derived(fileTreeStore.place);
  const rootName = $derived(place ? lastSegment(place.root) || place.root : 'Files');
  const pinned = $derived(!!fileTreeStore.pinned);
  const rootTip = $derived.by(() => {
    if (!place) return 'Choose a folder';
    const where = fileTreeStore.displayRoot(place);
    const why = place.source === 'pin' ? 'pinned for this workspace' : 'following the active tab';
    return `${where}\n${why} · click to switch`;
  });

  let switcherOpen = $state(false);
  let nameBtn = $state<HTMLButtonElement | null>(null);
  let moreBtn = $state<HTMLElement | null>(null);
  let menu = $state<{ x: number; y: number } | null>(null);

  function openMore() {
    if (menu) { menu = null; return; }
    const r = moreBtn?.getBoundingClientRect();
    if (r) menu = { x: r.left, y: r.bottom + 2 };
  }

  const moreItems = $derived.by((): MenuItem[] => [
    {
      label: `${preferencesStore.fileTreeShowHidden ? '✓ ' : ''}Show Hidden Files`,
      shortcut: `${modSymbol}⇧.`,
      action: () => preferencesStore.setFileTreeShowHidden(!preferencesStore.fileTreeShowHidden),
    },
    {
      label: `${preferencesStore.fileTreeShowIgnored ? '✓ ' : ''}Show Ignored Files`,
      action: () => preferencesStore.setFileTreeShowIgnored(!preferencesStore.fileTreeShowIgnored),
    },
    {
      label: `${preferencesStore.fileTreeFollow ? '✓ ' : ''}Follow the Active Tab`,
      action: () => {
        void preferencesStore.setFileTreeFollow(!preferencesStore.fileTreeFollow).then(() => fileTreeStore.scheduleResolve(false));
      },
    },
    { label: '', separator: true, action: () => {} },
    {
      label: 'Reveal Active File',
      disabled: !fileTreeStore.activeFile,
      action: () => { void fileTreeStore.revealActiveEditor(true); },
    },
    {
      label: 'Reveal This Tab’s Folder',
      disabled: !fileTreeStore.youAreHere,
      action: () => fileTreeStore.revealYouAreHere(),
    },
    { label: '', separator: true, action: () => {} },
    { label: 'Files Settings…', action: () => { void commands.openPreferencesWindow(); } },
  ]);

  function toggleSwitcher() {
    switcherOpen = !switcherOpen;
  }

  function showOutside() {
    const f = fileTreeStore.outsideFile;
    if (!f) return;
    const dir = f.replace(/[\\/][^\\/]*$/, '') || f;
    void fileTreeStore.pin(dir);
  }
</script>

<div class="header">
  <div class="root">
    <Tooltip text={rootTip} block>
      <button
        class="root-name"
        bind:this={nameBtn}
        onclick={toggleSwitcher}
        aria-haspopup="menu"
        aria-expanded={switcherOpen}
      >
        {#if pinned}<span class="pin-mark"><Icon name="pin" size={10} /></span>{/if}
        <span class="name">{rootName}</span>
        {#if place?.host_label}<span class="host">{place.host_label}</span>{/if}
        <span class="caret"><Icon name="chevron" size={10} /></span>
      </button>
    </Tooltip>
  </div>
  <div class="buttons">
    <IconButton tooltip="Refresh" size={22} onclick={() => fileTreeStore.refresh()} disabled={!fileTreeStore.subscribed}>
      <Icon name="refresh" size={13} />
    </IconButton>
    <IconButton tooltip="Collapse All" size={22} onclick={() => fileTreeStore.collapseAll()} disabled={!fileTreeStore.subscribed}>
      <Icon name="collapse-all" size={13} />
    </IconButton>
    <IconButton
      tooltip={pinned ? 'Unpin: follow the active tab again' : 'Pin this folder for the workspace'}
      size={22}
      active={pinned}
      onclick={() => (pinned ? fileTreeStore.unpin() : place && !place.missing ? fileTreeStore.pin(place.place) : undefined)}
      disabled={!place}
    >
      <Icon name="pin" size={12} />
    </IconButton>
    <span bind:this={moreBtn} class="more-anchor">
      <IconButton tooltip="More" size={22} onclick={openMore} aria-haspopup="menu" aria-expanded={!!menu}>
        <Icon name="more" size={13} />
      </IconButton>
    </span>
  </div>
</div>

{#if fileTreeStore.outsideFile}
  <div class="outside">
    <span class="muted">Outside root</span>
    <span class="dot">·</span>
    <Tooltip text={`Pin the folder holding ${fileTreeStore.outsideFile}`}>
      <button class="link" onclick={showOutside}>Show</button>
    </Tooltip>
  </div>
{/if}

{#if switcherOpen}
  <RootSwitcher anchor={nameBtn} onclose={() => { switcherOpen = false; }} />
{/if}

{#if menu}
  <ContextMenu items={moreItems} x={menu.x} y={menu.y} anchor={moreBtn} onclose={() => { menu = null; }} />
{/if}

<style>
  .header {
    flex-shrink: 0;
    height: 28px;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 0 4px 0 6px;
    border-bottom: 1px solid var(--bg-light);
  }

  .root {
    flex: 1;
    min-width: 0;
    display: flex;
  }

  .root-name {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
    max-width: 100%;
    background: none;
    border: none;
    border-radius: 4px;
    padding: 2px 4px;
    color: var(--fg);
    font: inherit;
    font-weight: 700;
    cursor: pointer;
  }

  .root-name:hover {
    background: var(--bg-light);
  }

  /* One line, ellipsized: the tooltip carries the full path. */
  .name {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .host {
    flex-shrink: 0;
    font-weight: 400;
    font-size: 0.85em;
    padding: 0 5px;
    border-radius: 8px;
    background: var(--bg-light);
    color: var(--fg-dim);
  }

  .pin-mark {
    display: inline-flex;
    color: var(--accent);
  }

  .caret {
    display: inline-flex;
    color: var(--fg-dim);
    transform: rotate(90deg);
  }

  .buttons {
    display: flex;
    align-items: center;
    flex-shrink: 0;
  }

  .more-anchor {
    display: inline-flex;
  }

  .outside {
    flex-shrink: 0;
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 2px 10px;
    font-size: 0.85em;
    border-bottom: 1px solid var(--bg-light);
  }

  .muted,
  .dot {
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
</style>
