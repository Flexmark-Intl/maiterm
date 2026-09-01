<script lang="ts">
  import changelogRaw from '../../../CHANGELOG.md?raw';
  import { marked } from 'marked';
  import IconButton from '$lib/components/ui/IconButton.svelte';

  /** Render a single changelog item as inline markdown (bold, italic, code, links). */
  function renderItem(text: string): string {
    return marked.parseInline(text, { gfm: true }) as string;
  }

  interface Props {
    open: boolean;
    onclose: () => void;
    version: string;
    /** If provided, renders these entries instead of the bundled CHANGELOG.md */
    entries?: ChangelogEntry[];
    /** Custom modal title */
    title?: string;
    /** When provided, shows an "Install & Restart" button */
    oninstall?: () => void;
    /** Label for the install button (e.g. "Downloading…") */
    installLabel?: string;
    /** Disable the install button (e.g. while downloading) */
    installDisabled?: boolean;
    /** When set, shows a "newer version available" choice prompt instead of the normal install button */
    newerVersionPrompt?: { version: string; originalVersion: string };
    /** Called when user chooses to install the latest (newer) version */
    oninstallLatest?: () => void;
    /** Called when user chooses to install the originally found version */
    oninstallOriginal?: () => void;
    /** Called when user wants to review the latest version's notes */
    onreviewLatest?: () => void;
  }

  let { open, onclose, version, entries, title, oninstall, installLabel, installDisabled, newerVersionPrompt, oninstallLatest, oninstallOriginal, onreviewLatest }: Props = $props();

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      onclose();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) {
      onclose();
    }
  }

  /** One rendered block of a release's notes. A `heading` is a `###` section within the
   *  version; a `bullet` carries its nesting depth so sub-points read as sub-points; a
   *  `para` is prose between them. Prose MUST be a kind of its own: once headings render,
   *  a section whose body is paragraphs (v2.0.0's file-sending section) would otherwise
   *  draw its title with nothing underneath it. */
  export type ChangelogItem =
    | { kind: 'heading'; text: string }
    | { kind: 'bullet'; text: string; depth: number }
    | { kind: 'para'; text: string };

  export interface ChangelogEntry {
    version: string;
    items: ChangelogItem[];
  }

  /** Indent width of one nesting level in CHANGELOG.md — two spaces, as markdown lists
   *  are written here. Deeper nesting is clamped at depth 1: the modal is 420px wide and
   *  a third level has nowhere left to go. */
  const INDENT = 2;

  function parseChangelog(raw: string): ChangelogEntry[] {
    const entries: ChangelogEntry[] = [];
    let current: ChangelogEntry | null = null;
    // Consecutive prose lines are one paragraph: a body written with hard-wrapped lines
    // would otherwise render as a stack of orphaned fragments.
    let para: string[] = [];
    const flushPara = () => {
      if (para.length && current) current.items.push({ kind: 'para', text: para.join(' ') });
      para = [];
    };
    for (const line of raw.split('\n')) {
      const versionMatch = line.match(/^## v(.+)/);
      if (versionMatch) {
        flushPara();
        current = { version: versionMatch[1], items: [] };
        entries.push(current);
        continue;
      }
      if (!current) continue;
      const headingMatch = line.match(/^#{3,} (.+)/);
      if (headingMatch) {
        flushPara();
        current.items.push({ kind: 'heading', text: headingMatch[1] });
        continue;
      }
      // Sub-bullets are indented, and were dropped entirely before this anchored on
      // column 0 — v1.25.0's three maiLink views never appeared in this modal.
      const itemMatch = line.match(/^(\s*)- (.+)/);
      if (itemMatch) {
        flushPara();
        // Keep raw markdown — rendered inline at display time via renderItem()
        current.items.push({
          kind: 'bullet',
          text: itemMatch[2],
          depth: Math.min(1, Math.floor(itemMatch[1].length / INDENT)),
        });
        continue;
      }
      if (line.trim() === '') flushPara();
      else para.push(line.trim());
    }
    flushPara();
    return entries;
  }

  const changelog = $derived(entries ?? parseChangelog(changelogRaw));
</script>

{#if open}
  <div
    class="backdrop"
    onclick={handleBackdropClick}
    onkeydown={handleKeydown}
    role="dialog"
    aria-modal="true"
    tabindex="-1"
  >
    <div class="modal">
      <div class="header">
        <h2>{title ?? 'Changelog'}</h2>
        <IconButton tooltip="Close" style="font-size: 1.538rem;padding:4px 8px;width:auto;height:auto" onclick={onclose}>&times;</IconButton>
      </div>

      <div class="content">
        {#each changelog as entry}
          <section>
            <h3 class:current={entry.version === version}>v{entry.version}{entry.version === version ? ' (current)' : ''}</h3>
            <div class="items">
              {#each entry.items as item}
                {#if item.kind === 'heading'}
                  <h4>{@html renderItem(item.text)}</h4>
                {:else if item.kind === 'para'}
                  <p class="para">{@html renderItem(item.text)}</p>
                {:else}
                  <div class="item" class:sub={item.depth > 0}>{@html renderItem(item.text)}</div>
                {/if}
              {/each}
            </div>
          </section>
        {/each}
      </div>

      {#if newerVersionPrompt}
        <div class="footer newer-prompt">
          <p class="newer-msg">A newer version <strong>v{newerVersionPrompt.version}</strong> is now available!</p>
          <div class="newer-actions">
            <button class="install-btn" onclick={oninstallLatest}>Update to v{newerVersionPrompt.version}</button>
            <button class="install-btn secondary" onclick={oninstallOriginal}>Update to v{newerVersionPrompt.originalVersion}</button>
            <button class="install-btn secondary" onclick={onreviewLatest}>Review v{newerVersionPrompt.version}</button>
          </div>
          <span class="install-hint">The app will restart to apply the update</span>
        </div>
      {:else if oninstall}
        <div class="footer">
          <button class="install-btn" onclick={oninstall} disabled={installDisabled}>
            {installLabel ?? 'Install & Restart'}
          </button>
          <span class="install-hint">The app will restart to apply the update</span>
        </div>
      {/if}
    </div>
  </div>
{/if}

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }

  .modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    width: 420px;
    max-height: 80vh;
    overflow-y: auto;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 16px 20px;
    border-bottom: 1px solid var(--bg-light);
  }

  h2 {
    margin: 0;
    font-size: 1.231rem;
    font-weight: 600;
    color: var(--fg);
  }

  .content {
    padding: 16px 20px;
  }

  section {
    margin-bottom: 20px;
  }

  section:last-child {
    margin-bottom: 0;
  }

  h3 {
    margin: 0 0 8px 0;
    font-size: 0.923rem;
    font-weight: 600;
    color: var(--fg-dim);
    letter-spacing: 0.5px;
  }

  h3.current {
    color: var(--accent);
  }

  h4 {
    margin: 14px 0 6px 0;
    font-size: 0.923rem;
    font-weight: 600;
    color: var(--fg);
  }

  /* A section heading opening the version needs no gap above it — the version
     heading is already there. */
  .items h4:first-child {
    margin-top: 0;
  }

  .para {
    margin: 0 0 8px 0;
    font-size: 1rem;
    color: var(--fg-dim);
    line-height: 1.5;
  }

  .item {
    position: relative;
    padding-left: 18px;
    font-size: 1rem;
    color: var(--fg-dim);
    line-height: 1.5;
    margin-bottom: 4px;
  }

  /* Bullets are drawn rather than list markers so a heading can break the flow
     without splitting the notes into several lists. */
  .item::before {
    content: '•';
    position: absolute;
    left: 6px;
    color: var(--fg-dim);
  }

  .item.sub {
    padding-left: 34px;
  }

  .item.sub::before {
    content: '◦';
    left: 22px;
  }

  .item:last-child {
    margin-bottom: 0;
  }

  .para :global(strong),
  .item :global(strong) {
    font-weight: 600;
    color: var(--fg);
  }

  .para :global(em),
  .item :global(em) {
    font-style: italic;
  }

  h4 :global(code),
  .para :global(code),
  .item :global(code) {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.85em;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    padding: 0.5px 4px;
  }

  .para :global(a),
  .item :global(a) {
    color: var(--accent);
    text-decoration: none;
  }

  .para :global(a:hover),
  .item :global(a:hover) {
    text-decoration: underline;
  }

  .footer {
    padding: 12px 20px;
    border-top: 1px solid var(--bg-light);
    display: flex;
    align-items: center;
    gap: 12px;
  }

  .install-btn {
    font-size: 0.923rem;
    font-weight: 600;
    padding: 6px 16px;
    border: none;
    border-radius: 4px;
    background: var(--accent);
    color: var(--bg-dark);
    cursor: pointer;
    white-space: nowrap;
    flex-shrink: 0;
  }

  .install-btn:hover:not(:disabled) {
    filter: brightness(1.15);
  }

  .install-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }

  .install-hint {
    font-size: 0.769rem;
    color: var(--fg-dim);
  }

  .newer-prompt {
    flex-direction: column;
    gap: 8px;
  }

  .newer-msg {
    margin: 0;
    font-size: 0.923rem;
    color: var(--fg);
  }

  .newer-actions {
    display: flex;
    gap: 8px;
    flex-wrap: wrap;
  }

  .install-btn.secondary {
    background: var(--bg-light);
    color: var(--fg);
  }

  .install-btn.secondary:hover {
    filter: brightness(1.3);
  }
</style>
