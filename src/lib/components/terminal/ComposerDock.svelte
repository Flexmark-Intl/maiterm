<script lang="ts">
  import { tick, onDestroy } from 'svelte';
  import { slide } from 'svelte/transition';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { writeTerminal, terminalBracketedPaste } from '$lib/tauri/commands';
  import { isModKey, modLabel } from '$lib/utils/platform';
  import { error as logError } from '@tauri-apps/plugin-log';

  interface Props {
    tabId: string;
    draft: string | null;
  }

  let { tabId, draft }: Props = $props();

  // Initial value only — the component is keyed per tab, so a tab switch remounts
  // it with that tab's persisted draft; live edits flow through `value`.
  // svelte-ignore state_referenced_locally
  let value = $state(draft ?? '');
  let textareaEl = $state<HTMLTextAreaElement | null>(null);
  let open = $derived(workspacesStore.isComposerOpen(tabId));

  let draftTimer: ReturnType<typeof setTimeout> | undefined;
  let draftDirty = false;

  function persistDraft() {
    clearTimeout(draftTimer);
    draftTimer = undefined;
    if (!draftDirty) return;
    draftDirty = false;
    workspacesStore.setComposerDraft(tabId, value || null);
  }

  function onInput() {
    draftDirty = true;
    clearTimeout(draftTimer);
    draftTimer = setTimeout(persistDraft, 500);
    autogrow();
  }

  function autogrow() {
    if (!textareaEl) return;
    textareaEl.style.height = 'auto';
    textareaEl.style.height = `${textareaEl.scrollHeight}px`;
  }

  async function toggle() {
    workspacesStore.toggleComposer(tabId);
    await tick();
    if (workspacesStore.isComposerOpen(tabId)) {
      terminalsStore.get(tabId)?.terminal?.blur();
      textareaEl?.focus();
      autogrow();
    } else {
      terminalsStore.get(tabId)?.terminal?.focus();
    }
  }

  function focusTerminal() {
    terminalsStore.get(tabId)?.terminal?.focus();
  }

  async function send() {
    const text = value.replace(/\n+$/, '');
    if (!text) return;
    const instance = terminalsStore.get(tabId);
    if (!instance) return;
    try {
      // When the foreground app has bracketed paste on (Claude Code, modern
      // readline), wrap the text so embedded newlines stay literal and the
      // trailing CR is one submit. Otherwise (e.g. macOS bash 3.2) the markers
      // would arrive as garbage input — send raw with CR line breaks instead,
      // which executes line-by-line, the natural semantics for such shells.
      const bracketed = await terminalBracketedPaste(instance.ptyId).catch(() => false);
      const payload = bracketed
        ? `\x1b[200~${text}\x1b[201~\r`
        : `${text.replace(/\n/g, '\r')}\r`;
      await writeTerminal(instance.ptyId, Array.from(new TextEncoder().encode(payload)));
      value = '';
      draftDirty = true;
      persistDraft();
      await tick();
      autogrow();
      textareaEl?.focus();
    } catch (e) {
      logError(`Composer send failed: ${e}`);
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && isModKey(e)) {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      focusTerminal();
    }
  }

  // Re-measure height when the component mounts with a restored draft.
  $effect(() => {
    if (open && textareaEl) autogrow();
  });

  onDestroy(() => {
    persistDraft();
  });
</script>

{#if open}
  <div class="composer-dock" transition:slide={{ duration: 160 }}>
    <textarea
      bind:this={textareaEl}
      bind:value
      class="composer-input"
      style:font-family={preferencesStore.fontFamily}
      style:font-size="{preferencesStore.fontSize}px"
      rows="1"
      placeholder="Compose… ({modLabel}+Enter to send, Esc for terminal)"
      spellcheck="false"
      oninput={onInput}
      onkeydown={onKeydown}
    ></textarea>
    <div class="composer-actions">
      <button class="composer-btn" onclick={toggle} title="Collapse composer" aria-label="Collapse composer">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round">
          <path d="M4 6.5 8 10.5 12 6.5"/>
        </svg>
      </button>
      <button class="composer-btn" onclick={send} title="Send ({modLabel}+Enter)" aria-label="Send">
        <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
          <path d="M1.7 8 1 2.4c-.1-.6.5-1 1-.8l12.6 5.7c.5.2.5 1 0 1.2L2 14.4c-.5.2-1.1-.2-1-.8L1.7 8Zm0 0h6.6"
            fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" stroke-linecap="round"/>
        </svg>
      </button>
    </div>
  </div>
{:else}
  <button
    class="composer-handle"
    onclick={toggle}
    title="Open composer"
    aria-label="Open composer"
  >
    <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round">
      <rect x="1.5" y="3.5" width="13" height="9" rx="1.5"/>
      <path d="M4.5 9.5h7"/>
    </svg>
  </button>
{/if}

<style>
  .composer-dock {
    display: flex;
    align-items: flex-end;
    gap: 8px;
    padding: 10px 12px;
    background: var(--bg-medium);
    border-top: 1px solid var(--bg-light);
  }

  .composer-input {
    flex: 1;
    resize: none;
    overflow-y: auto;
    min-height: 30px;
    max-height: 35vh;
    padding: 6px 8px;
    background: var(--bg-dark);
    color: var(--fg);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    line-height: 1.4;
    outline: none;
  }

  .composer-input:focus {
    border-color: var(--accent);
  }

  .composer-input::placeholder {
    color: var(--fg-dim);
  }

  .composer-actions {
    display: flex;
    align-items: center;
    gap: 4px;
    /* Keep the row vertically centered on the input's single-line height,
       pinned to the bottom as the textarea grows. */
    margin-bottom: 3px;
  }

  .composer-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    color: var(--fg-dim);
    border-radius: 4px;
    transition: background 0.1s, color 0.1s;
  }

  .composer-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .composer-handle {
    position: absolute;
    right: 14px;
    bottom: 8px;
    z-index: 5;
    display: flex;
    align-items: center;
    justify-content: center;
    width: 26px;
    height: 26px;
    padding: 0;
    color: var(--fg-dim);
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    opacity: 0.45;
    transition: opacity 0.15s, color 0.15s, background 0.15s;
  }

  .composer-handle:hover {
    opacity: 1;
    color: var(--fg);
    background: var(--bg-light);
  }
</style>
