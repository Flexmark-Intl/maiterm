<script module lang="ts">
  /** Per-tab attachment lists, surviving keyed remounts on tab switch. */
  const attachmentsByTab = new Map<string, { path: string; name: string; thumb?: string }[]>();
</script>

<script lang="ts">
  import { tick, onMount, onDestroy } from 'svelte';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import { readText as clipboardReadText, readImage as clipboardReadImage } from '@tauri-apps/plugin-clipboard-manager';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { terminalsStore } from '$lib/stores/terminals.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { writeTerminal, terminalBracketedPaste, readClipboardFilePaths, saveClipboardImage, getPtyInfo } from '$lib/tauri/commands';
  import { uploadWithProgress } from '$lib/utils/scpUpload';
  import { bracketedPasteSubmit } from '$lib/utils/agentPrompt';
  import { isModKey, modLabel } from '$lib/utils/platform';
  import { error as logError } from '@tauri-apps/plugin-log';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';

  interface Props {
    tabId: string;
    draft: string | null;
  }

  let { tabId, draft }: Props = $props();

  interface ComposerAttachment {
    /** Local absolute path (pasted screenshots are materialized to a temp file). */
    path: string;
    name: string;
    /** Small data: URL preview — only for pasted screenshots, where the pixels
        are already in hand. Dropped/copied files get a generic icon. */
    thumb?: string;
  }

  // Initial value only — the component is keyed per tab, so a tab switch remounts
  // it with that tab's persisted draft; live edits flow through `value`.
  // svelte-ignore state_referenced_locally
  let value = $state(draft ?? '');
  let textareaEl = $state<HTMLTextAreaElement | null>(null);
  let open = $derived(workspacesStore.isComposerOpen(tabId));

  // Attachments survive tab switches (keyed remounts) via this module-level map,
  // but are not persisted to disk — pasted screenshots live in temp files anyway.
  // Initial read only, same keyed-remount pattern as `draft` above.
  // svelte-ignore state_referenced_locally
  let attachments = $state<ComposerAttachment[]>(attachmentsByTab.get(tabId) ?? []);
  let isDragOver = $state(false);

  function setAttachments(next: ComposerAttachment[]) {
    attachments = next;
    if (next.length) attachmentsByTab.set(tabId, next);
    else attachmentsByTab.delete(tabId);
  }

  function addAttachments(items: ComposerAttachment[]) {
    // Dedupe by path — re-pasting the same Finder selection shouldn't stack chips.
    const existing = new Set(attachments.map(a => a.path));
    setAttachments([...attachments, ...items.filter(a => !existing.has(a.path))]);
  }

  function removeAttachment(index: number) {
    setAttachments(attachments.filter((_, i) => i !== index));
    textareaEl?.focus();
  }

  function basename(p: string): string {
    return p.split('/').pop() ?? p;
  }

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

  function toggle() {
    workspacesStore.toggleComposer(tabId);
  }

  let shellEl = $state<HTMLDivElement | null>(null);

  // Explicit height animation: WebKit doesn't collapse a 0fr grid row below its
  // content min-content, and a transition:slide outro left orphaned DOM — so the
  // shell stays mounted and we animate its measured height by hand. While open
  // the height is 'auto' so the textarea's autogrow resizes the dock naturally.
  //
  // Deliberately rAF-free: WKWebView pauses requestAnimationFrame in occluded
  // windows, so any rAF-dependent step would leave the dock in a wrong visual
  // state if toggled while the window is in the background. Synchronous reflow
  // (offsetHeight) commits start values instead; only the cosmetic release to
  // 'auto' uses transitionend with a timed fallback.
  let animSeq = 0;
  function setShellHeight(isOpen: boolean, animate: boolean) {
    const el = shellEl;
    if (!el) return;
    const seq = ++animSeq;
    if (!animate) {
      el.style.transitionProperty = 'none';
      el.style.height = isOpen ? 'auto' : '0px';
      void el.offsetHeight; // flush so the height change doesn't animate
      el.style.transitionProperty = '';
      return;
    }
    if (isOpen) {
      // Inline height is 0px (or mid-close px) — animate to content height,
      // then release to auto so autogrow can resize the open dock.
      const target = el.scrollHeight;
      el.style.height = `${target}px`;
      const release = () => {
        el.removeEventListener('transitionend', release);
        clearTimeout(timer);
        if (seq === animSeq) el.style.height = 'auto';
      };
      const timer = setTimeout(release, 250);
      el.addEventListener('transitionend', release);
    } else {
      el.style.height = `${el.scrollHeight}px`;
      void el.offsetHeight; // commit the explicit height before collapsing
      el.style.height = '0px';
    }
  }

  // Height + focus handoff on open/close transitions (button or Cmd+Shift+C
  // alike). The initial run snaps without animation so a tab switch (keyed
  // remount) doesn't replay the slide or steal focus from the terminal.
  let prevOpen: boolean | undefined;
  $effect(() => {
    const isOpen = open;
    if (prevOpen === undefined) {
      setShellHeight(isOpen, false);
    } else if (isOpen !== prevOpen) {
      setShellHeight(isOpen, true);
      if (isOpen) {
        terminalsStore.get(tabId)?.terminal?.blur();
        textareaEl?.focus();
        autogrow();
      } else {
        terminalsStore.get(tabId)?.terminal?.focus();
      }
    }
    prevOpen = isOpen;
  });

  function focusTerminal() {
    terminalsStore.get(tabId)?.terminal?.focus();
  }

  /** Convert clipboard RGBA to PNG base64 (full size) — same approach as the
      terminal's screenshot paste (TerminalPane.rgbaToPngBase64). */
  async function rgbaToPngBase64(rgba: Uint8Array, width: number, height: number): Promise<string> {
    const canvas = new OffscreenCanvas(width, height);
    canvas.getContext('2d')!.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
    const blob = await canvas.convertToBlob({ type: 'image/png' });
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = '';
    for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
    return btoa(binary);
  }

  /** Downscaled data: URL for the chip preview (≤48px tall, 2x for retina). */
  async function makeThumb(rgba: Uint8Array, width: number, height: number): Promise<string> {
    const src = new OffscreenCanvas(width, height);
    src.getContext('2d')!.putImageData(new ImageData(new Uint8ClampedArray(rgba), width, height), 0, 0);
    const scale = Math.min(1, 48 / height);
    const dst = new OffscreenCanvas(Math.max(1, Math.round(width * scale)), Math.max(1, Math.round(height * scale)));
    dst.getContext('2d')!.drawImage(src, 0, 0, dst.width, dst.height);
    const blob = await dst.convertToBlob({ type: 'image/png' });
    const bytes = new Uint8Array(await blob.arrayBuffer());
    let binary = '';
    for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
    return `data:image/png;base64,${btoa(binary)}`;
  }

  function insertAtCursor(text: string) {
    const el = textareaEl;
    if (!el) {
      value += text;
      return;
    }
    el.setRangeText(text, el.selectionStart, el.selectionEnd, 'end');
    value = el.value;
    onInput();
  }

  /** Native-pasteboard paste, mirroring the terminal's paste priorities:
      Finder file copies → attachment chips; screenshot image data → temp PNG +
      chip with preview; otherwise plain text into the textarea. */
  async function pasteIntoComposer() {
    try {
      const paths = await readClipboardFilePaths();
      if (paths.length > 0) {
        addAttachments(paths.map(p => ({ path: p, name: basename(p) })));
        return;
      }
      try {
        const image = await clipboardReadImage();
        const { width, height } = await image.size();
        if (width > 0 && height > 0) {
          const rgba = await image.rgba();
          const base64 = await rgbaToPngBase64(rgba, width, height);
          const localPath = await saveClipboardImage(base64);
          const thumb = await makeThumb(rgba, width, height);
          addAttachments([{ path: localPath, name: basename(localPath), thumb }]);
          return;
        }
      } catch {
        // No image on clipboard — fall through to text
      }
      const text = await clipboardReadText();
      if (text) insertAtCursor(text);
    } catch (e) {
      logError(`Composer paste failed: ${e}`);
    }
  }

  function onPaste(e: ClipboardEvent) {
    // Cmd+V is intercepted in onKeydown; this catches menu/context pastes.
    // Only divert when the clipboard carries files/images — plain text keeps
    // the default textarea paste (preserves undo stack).
    const cd = e.clipboardData;
    const hasFile = !!cd && (cd.files.length > 0 || [...cd.items].some(i => i.kind === 'file'));
    if (hasFile) {
      e.preventDefault();
      void pasteIntoComposer();
    }
  }

  let sending = $state(false);

  /** Resolve attachment paths into the text to send. Local sessions reference
      the files directly; SSH sessions SCP-upload first and reference the
      remote copies — same routing the terminal's drop handler uses. */
  async function resolveAttachmentPaths(ptyId: string): Promise<string | null> {
    if (attachments.length === 0) return '';
    const info = await getPtyInfo(ptyId).catch(() => null);
    const sshCommand = info?.foreground_command;
    if (sshCommand) {
      const localPaths = attachments.map(a => a.path);
      const outcome = await uploadWithProgress(sshCommand, localPaths, '/tmp/aiterm-uploads', { titlePrefix: 'Attachment' });
      if (outcome.status !== 'done') {
        if (outcome.status === 'error') {
          toastStore.addToast('Attachment Upload Failed', outcome.error ?? 'Upload failed', 'error');
        }
        return null; // abort send, keep draft + chips
      }
      return attachments.map(a => `/tmp/aiterm-uploads/${a.name}`).join(' ');
    }
    // Send raw, unescaped paths. Composer attachments are file references for
    // the foreground agent (Claude Code et al.), which wants the literal path —
    // matching the terminal's own local-Claude drop convention. We deliberately
    // don't shell-escape: escaping only ever helped a plain shell consume the
    // path as a command argument, but it broke file detection in a Claude
    // session maiTerm hadn't recognized as Claude (no hooks → backslashes Claude
    // won't un-escape, so a path with spaces never resolves). Raw is correct for
    // the agent case and a no-op for screenshot temp paths (no special chars).
    return attachments.map(a => a.path).join(' ');
  }

  async function send() {
    if (sending) return;
    const text = value.replace(/\s+$/, '');
    if (!text && attachments.length === 0) return;
    const instance = terminalsStore.get(tabId);
    if (!instance) {
      // Suspended tab / PTY not up yet — keep the draft, tell the user instead
      // of silently doing nothing.
      toastStore.addToast('Composer', 'No live terminal in this tab — resume it first', 'info');
      return;
    }
    sending = true;
    try {
      const attachmentCount = attachments.length;
      const pathsPart = await resolveAttachmentPaths(instance.ptyId);
      if (pathsPart === null) return; // upload failed — nothing was sent
      // Attachment paths go on their own line for multi-line prompts, after a
      // space for one-liners.
      const full = [text, pathsPart].filter(Boolean).join(text.includes('\n') ? '\n' : ' ');
      // When the foreground app has bracketed paste on (Claude Code, modern
      // readline), wrap the text so embedded newlines stay literal and the
      // trailing CR is one submit. Otherwise (e.g. macOS bash 3.2) the markers
      // would arrive as garbage input — send raw with CR line breaks instead,
      // which executes line-by-line, the natural semantics for such shells.
      const bracketed = await terminalBracketedPaste(instance.ptyId).catch(() => false);
      if (bracketed) {
        // Submit via bracketed paste with a settle-delayed CR sent as its own
        // keystroke — see bracketedPasteSubmit for why a CR packed into the same
        // write gets swallowed when Claude collapses the paste into a chip.
        await bracketedPasteSubmit(instance.ptyId, full, attachmentCount);
      } else {
        // Old shells (macOS bash 3.2): the markers would arrive as garbage; send
        // raw with CR line breaks, which executes line-by-line — the natural
        // semantics there.
        const enc = (s: string) => Array.from(new TextEncoder().encode(s));
        await writeTerminal(instance.ptyId, enc(`${full.replace(/\n/g, '\r')}\r`));
      }
      value = '';
      setAttachments([]);
      draftDirty = true;
      persistDraft();
      await tick();
      autogrow();
      textareaEl?.focus();
    } catch (e) {
      logError(`Composer send failed: ${e}`);
    } finally {
      sending = false;
    }
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === 'Enter' && isModKey(e)) {
      e.preventDefault();
      send();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      focusTerminal();
    } else if (isModKey(e) && !e.shiftKey && e.key.toLowerCase() === 'v') {
      // Native-pasteboard paste: WKWebView's clipboardData misses Finder file
      // copies and screenshots, so route through the same NSPasteboard checks
      // the terminal paste uses.
      e.preventDefault();
      void pasteIntoComposer();
    }
  }

  // Re-measure height when the component mounts with a restored draft.
  $effect(() => {
    if (open && textareaEl) autogrow();
  });

  // Drop files onto the open dock → attachment chips. Same window-scoped Tauri
  // event the terminal uses (HTML5 drop is disabled under Tauri's dragDrop
  // interception); bounds-checked against the shell so the terminal's own drop
  // zone above keeps its behavior.
  let unlistenDragDrop: UnlistenFn | undefined;
  onMount(() => {
    void (async () => {
      unlistenDragDrop = await getCurrentWebview().onDragDropEvent((event) => {
        const { type } = event.payload;
        if (!open || !shellEl?.isConnected) { isDragOver = false; return; }
        if (type === 'over') {
          const { position } = event.payload;
          const rect = shellEl.getBoundingClientRect();
          isDragOver = position.x >= rect.left && position.x <= rect.right &&
                       position.y >= rect.top && position.y <= rect.bottom;
        } else if (type === 'drop') {
          const wasOver = isDragOver;
          isDragOver = false;
          if (!wasOver) return;
          const { paths } = event.payload;
          addAttachments(paths.map(p => ({ path: p, name: basename(p) })));
          textareaEl?.focus();
        } else {
          isDragOver = false;
        }
      });
    })();
  });

  onDestroy(() => {
    persistDraft();
    unlistenDragDrop?.();
  });
</script>

<!-- Always mounted; see setShellHeight for why the open/close animation is
     hand-rolled. inert blocks focus/input while collapsed. -->
<div class="composer-shell" class:open inert={!open} bind:this={shellEl}>
  <div class="composer-dock" class:drag-over={isDragOver}>
    {#if attachments.length > 0}
      <div class="composer-chips">
        {#each attachments as att, i (att.path)}
          <div class="chip" title={att.path}>
            {#if att.thumb}
              <img class="chip-thumb" src={att.thumb} alt={att.name} />
            {:else}
              <svg class="chip-icon" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round">
                <path d="M4 1.5h5.5L13 5v9.5H4z"/>
                <path d="M9.5 1.5V5H13"/>
              </svg>
            {/if}
            <span class="chip-name">{att.name}</span>
            <button class="chip-remove" onclick={() => removeAttachment(i)} aria-label="Remove {att.name}">&times;</button>
          </div>
        {/each}
      </div>
    {/if}
    <div class="composer-row">
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
        onpaste={onPaste}
      ></textarea>
      <div class="composer-actions">
        <IconButton tooltip="Collapse composer ({modLabel}+Shift+C)" size={26} onclick={toggle} aria-label="Collapse composer">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round">
            <path d="M4 6.5 8 10.5 12 6.5"/>
          </svg>
        </IconButton>
        <IconButton tooltip="Send ({modLabel}+Enter)" size={26} onclick={send} disabled={sending} aria-label="Send">
          <svg width="14" height="14" viewBox="0 0 16 16" fill="currentColor">
            <path d="M1.7 8 1 2.4c-.1-.6.5-1 1-.8l12.6 5.7c.5.2.5 1 0 1.2L2 14.4c-.5.2-1.1-.2-1-.8L1.7 8Zm0 0h6.6"
              fill="none" stroke="currentColor" stroke-width="1.3" stroke-linejoin="round" stroke-linecap="round"/>
          </svg>
        </IconButton>
      </div>
    </div>
  </div>
</div>
{#if !open}
  <div class="composer-handle-pos">
    <Tooltip text="Open composer ({modLabel}+Shift+C)">
      <button class="composer-handle" onclick={toggle} aria-label="Open composer">
        <svg width="15" height="15" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round">
          <rect x="1.5" y="3.5" width="13" height="9" rx="1.5"/>
          <path d="M4.5 9.5h7"/>
        </svg>
      </button>
    </Tooltip>
  </div>
{/if}

<style>
  .composer-shell {
    height: 0;
    overflow: hidden;
    transition: height 160ms ease;
  }

  .composer-dock {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 10px 12px;
    background: var(--bg-medium);
    border-top: 1px solid var(--bg-light);
  }

  .composer-dock.drag-over {
    background: var(--bg-light);
    box-shadow: inset 0 0 0 1px var(--accent);
  }

  .composer-row {
    display: flex;
    align-items: flex-end;
    gap: 8px;
  }

  .composer-chips {
    display: flex;
    flex-wrap: wrap;
    gap: 6px;
  }

  .chip {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 240px;
    padding: 3px 4px 3px 8px;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    color: var(--fg);
    font-size: 0.846rem;
  }

  .chip-thumb {
    height: 24px;
    max-width: 48px;
    object-fit: cover;
    border-radius: 3px;
  }

  .chip-icon {
    flex-shrink: 0;
    color: var(--fg-dim);
  }

  .chip-name {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .chip-remove {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    color: var(--fg-dim);
    border-radius: 4px;
    font-size: 1rem;
    line-height: 1;
    transition: background 0.1s, color 0.1s;
  }

  .chip-remove:hover {
    background: var(--bg-light);
    color: var(--fg);
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

  .composer-handle-pos {
    position: absolute;
    right: 14px;
    bottom: 8px;
    z-index: 5;
  }

  .composer-handle {
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
