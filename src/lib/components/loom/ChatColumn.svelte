<!--
  One open chat, condensed the way the phone shows it (tool runs folded, events as thin rules,
  task events placed where they happened), then whatever the agent is stopped at (a permission,
  a question, a task question, a watch script), answerable in place, and a composer. The header
  carries the chat's own state: model, effort, context.

  One long-lived instance whose `source` changes, never {#key}-ed: drafts, attachments, bubbles
  still on their way, a picked effort, open folds and the notes are kept per chat in here, so
  switching chats keeps each one's. What belongs to a caller (the Loom's ritual, Release and
  Trigger) comes in as snippets: `header` after the gauges, `composerActions` at the composer's
  left (docs/hive.md §4.2).
-->
<script lang="ts" module>
  import type { Task } from '$lib/tauri/types';

  /** Where the column reads its chat from. A retired Hive worker (docs/hive.md §8.4) has no tab:
   *  its kind comes with its Rust reader. */
  export type ChatSource = { kind: 'tab'; tabId: string };

  /** The task question a tab's agent waits on the human for: the list pins the chat on it, the
   *  column answers it. */
  export const asking = (tasks: Task[], tabId: string) =>
    tasks.find((t) => t.tab_id === tabId && t.status === 'blocked' && (t.blocker?.kind === 'decision' || t.blocker?.kind === 'action'));
</script>

<script lang="ts">
  import type { Snippet } from 'svelte';
  import { loomStore } from '$lib/stores/loom.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { navigateToTab } from '$lib/stores/workspaces.svelte';
  import { getTabMeta, getTabTranscript, listTabModels, sendTabMessage, type ChatTurn, type TabMeta } from '$lib/tauri/commands';
  import { chatRows, injectedTurn, taskEventsFor } from '$lib/loom/model';
  import { renderTurnMarkdown } from '$lib/loom/markdown';
  import { chatImages } from '$lib/loom/chatImages';
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import ScriptApprovalCard from '$lib/components/followUps/ScriptApprovalCard.svelte';
  import { followUpsStore } from '$lib/stores/followUps.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import PromptCard from './PromptCard.svelte';
  import AttachmentChips from '$lib/components/composer/AttachmentChips.svelte';
  import { fromPaths, mergeAttachments, pasteCarriesFiles, readPaste, type ComposerAttachment } from '$lib/composer/attachments';
  import { isModKey } from '$lib/utils/platform';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { dropPointToCss } from '$lib/utils/dropPoint';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';

  interface Props {
    /** The chat shown; none draws the hint. Changing it switches chats in place. */
    source: ChatSource | null;
    /** The chat's name and workspace as the caller lists it. */
    name: string;
    workspace: string;
    tasks: Task[];
    /** The column is on screen: poll and keep the chat live only then. */
    active: boolean;
    /** The chat only: no composer, and the prompt, task question and scripts aren't answered. */
    readOnly?: boolean;
    /** The caller's own header pieces, after the gauges. */
    header?: Snippet<[tabId: string]>;
    /** The caller's own actions, at the composer's left. */
    composerActions?: Snippet<[tabId: string]>;
  }
  let { source, name, workspace, tasks, active, readOnly = false, header, composerActions }: Props = $props();

  /** A string, so a caller handing a new source object for the same tab switches nothing. */
  const openId = $derived(source?.tabId ?? null);
  const liveState = $derived(openId ? claudeStateStore.getState(openId) : null);

  // ── The transcript: read on open, then every 3 s while shown (the phone polls 2 s). ──
  let turns = $state<ChatTurn[]>([]);
  let loadedFor = $state<string | null>(null);
  /** Model, effort and context, read with the transcript: the phone's thread header. */
  let meta = $state<TabMeta | null>(null);
  /** When the reads now shown were ASKED for: a read asked before a send knows nothing of it. */
  let metaAskedAt = $state(0);
  let turnsAskedAt = $state(0);
  let reload: () => void = () => {};
  $effect(() => {
    const id = openId;
    turns = [];
    loadedFor = null;
    meta = null;
    metaAskedAt = 0;
    turnsAskedAt = 0;
    if (!id || !active) return;
    let alive = true;
    const load = () => {
      const asked = Date.now();
      getTabMeta(id).then((m) => { if (alive) { meta = m; metaAskedAt = asked; } }).catch(() => {});
      return getTabTranscript(id)
        .then((t) => { if (alive) { turns = t; loadedFor = id; turnsAskedAt = asked; } })
        .catch(() => { if (alive) loadedFor = id; });
    };
    reload = () => void load();
    void load();
    const timer = setInterval(load, 3000);
    return () => { alive = false; clearInterval(timer); reload = () => {}; };
  });

  const rows = $derived(openId ? chatRows(turns, taskEventsFor(openId, tasks)) : []);
  let expanded = $state<string[]>([]);
  // A drag that selects text and ends on a fold row is a copy, not a click on the row.
  const toggle = (key: string) => {
    if (window.getSelection()?.toString()) return;
    expanded = expanded.includes(key) ? expanded.filter((k) => k !== key) : [...expanded, key];
  };

  const openAsk = $derived(openId ? asking(tasks, openId) : undefined);
  /** The open chat's watch scripts waiting to be allowed (docs/follow-ups.md §5.1), answered in
   *  place like its task question. */
  const openScripts = $derived(openId ? followUpsStore.pendingApprovals.filter((a) => !a.archived && a.tabId === openId) : []);
  /** Waiting on the human, as the chat list counts it: a task question or a script. */
  const asks = $derived(!!openAsk || openScripts.length > 0);
  /** When those cards last changed, for their click guard: one inserted above or answered moves
   *  the rest (ScriptApprovalCard). */
  let scriptsChangedAt = $state(0);
  const openScriptIds = $derived(openScripts.map((a) => a.followUp.id).join('|'));
  $effect(() => {
    void openScriptIds;
    scriptsChangedAt = Date.now();
  });
  /** The last answer's outcome, for the chat it was given in. Keyed to the CHAT, not the task:
   *  the answer moves the task out of Blocked before the note arrives, so the question is gone
   *  by then, and the note ("the agent could not be told") is exactly what must still show. */
  let outcome = $state<{ chat: string; text: string } | null>(null);

  // ── What the Fleet card showed ────────────────────────────────────────────────────────────
  // The chat's own meta first: the engine's facts are only polled while Overlord is on. (A read
  // any column may make, Loom or not, as is the checkpoint threshold in ctxTone.)
  const ctx = $derived(openId ? (meta?.contextPct ?? overlordStore.facts.get(openId)?.context_pct ?? null) : null);

  // ── Model and effort, set the way the phone sets them: `/model X` and `/effort X` typed
  //    through the composer's path, so an open prompt refuses them the same way. ──────────────
  const claudeChat = $derived(meta?.runtime === 'claude');
  /** The pickers type into the tab, so a read-only column shows the values only. */
  const pickable = $derived(claudeChat && !readOnly);
  /** An effort picked here, shown until the transcript reports one. Claude Code stops writing
   *  effort after a resume, so for a resumed session this is the only record. Per chat. */
  let effortPicked = $state<Record<string, string>>({});
  const effortShown = $derived(openId ? (meta?.effort ?? effortPicked[openId] ?? null) : null);
  const EFFORTS = [
    { value: 'low', name: 'Low' },
    { value: 'medium', name: 'Medium' },
    { value: 'high', name: 'High' },
    { value: 'xhigh', name: 'Extra high' },
    { value: 'max', name: 'Max' },
  ];
  let pickSeq = 0;
  let pickMenu = $state<{ x: number; y: number; anchor: HTMLElement; seq?: number; items: { label: string; shortcut?: string; disabled?: boolean; action: () => void }[] } | null>(null);
  $effect(() => {
    void openId;
    pickMenu = null;
  });
  const menuAt = (e: MouseEvent) => {
    const anchor = e.currentTarget as HTMLElement;
    const r = anchor.getBoundingClientRect();
    return { x: r.left, y: r.bottom + 4, anchor };
  };
  async function typeCommand(chat: string, text: string): Promise<boolean> {
    try {
      const r = await sendTabMessage(chat, text);
      if (r.status === 'delivered') return true;
      headNote = { chat, text: r.detail ?? `Not sent (${r.reason ?? r.status}).` };
    } catch (e) {
      headNote = { chat, text: `Not sent: ${e}` };
    }
    return false;
  }
  async function openModelPicker(e: MouseEvent) {
    if (pickMenu) { pickMenu = null; return; }
    const chat = openId;
    if (!chat) return;
    // Open at once and fill in when the list lands, into THIS menu only: a menu closed, or
    // replaced by another picker or the rule menu, while the list was read stays that way.
    // Compared by a number, not by object: `$state` stores a proxy, never the object assigned.
    const seq = ++pickSeq;
    const at = menuAt(e);
    pickMenu = { ...at, seq, items: [{ label: 'Reading models…', disabled: true, action: () => {} }] };
    const models = await listTabModels(chat).catch(() => []);
    if (pickMenu?.seq !== seq || openId !== chat) return;
    pickMenu = {
      ...at,
      seq,
      items: models.map((m) => ({
        label: m.name + (m.note ? ` · ${m.note}` : ''),
        // An ambiguous row matching says "on this model", never "on this row" (§ ModelOption).
        shortcut: meta?.model && m.display === meta.model ? 'current' : undefined,
        action: async () => {
          if (await typeCommand(chat, `/model ${m.value}`)) {
            // Not stamped on the header: a sent /model can be refused or put behind a dialog.
            headNote = { chat, text: `Sent /model ${m.value}. The header shows the model the agent next replies on.` };
          }
        },
      })),
    };
    if (!models.length) pickMenu = { ...at, seq, items: [{ label: 'Could not read the model list', disabled: true, action: () => {} }] };
  }
  function openEffortPicker(e: MouseEvent) {
    if (pickMenu) { pickMenu = null; return; }
    const chat = openId;
    if (!chat) return;
    pickMenu = {
      ...menuAt(e),
      items: EFFORTS.map((o) => ({
        label: o.name,
        shortcut: effortShown === o.value ? 'current' : undefined,
        action: async () => {
          if (await typeCommand(chat, `/effort ${o.value}`)) {
            effortPicked = { ...effortPicked, [chat]: o.value };
            headNote = null;
          }
        },
      })),
    };
  }
  const fmtTok = (n: number) => (n >= 1_000_000 ? `${+(n / 1_000_000).toFixed(1)}M` : n >= 1000 ? `${Math.round(n / 1000)}k` : `${n}`);
  let headNote = $state<{ chat: string; text: string } | null>(null);
  /** A caller's own action reports under the composer, where the pickers do, sharing their one
   *  note (the Loom's rule Trigger: a refusal stays until the next try). `null` clears it. */
  export function note(chat: string, text: string | null) {
    headNote = text === null ? null : { chat, text };
  }
  const ctxTone = (p: number | null) =>
    p === null ? 'var(--fg-dim)' : p >= 75 ? 'var(--red)' : p >= (overlordStore.checkpointThreshold ?? 60) ? 'var(--orange, #ff9e64)' : 'var(--green)';

  // ── Composer ──────────────────────────────────────────────────────────────────────────────
  /** Drafts by chat, so switching chats never sends one agent's words to another. */
  let drafts = $state<Record<string, string>>({});
  const draft = $derived(openId ? (drafts[openId] ?? '') : '');
  let sending = $state(false);
  let sendNote = $state<{ chat: string; text: string } | null>(null);

  /** Messages sent from here that the transcript hasn't echoed yet, shown as bubbles at the end
   *  of their chat (the phone's `pending`). Without them a message typed while the agent was busy
   *  vanished from the composer and showed nowhere until Claude took it, which reads as lost. */
  /** `seen`, `lastUser` and `lastTurn` are the transcript as it stood at send time, so an older
   *  turn that happens to say the same thing ("ok") can't retire a new message. `sentAt` is when
   *  it was typed into the tab; 0 while sending. */
  interface Outgoing {
    id: number; chat: string; text: string; sentAt: number;
    seen: number; lastUser: string | null; lastTurn: string | null;
  }
  let outgoing = $state<Outgoing[]>([]);
  let outSeq = 0;
  const sameText = (trs: ChatTurn[], t: string) => trs.filter((m) => m.role === 'user' && m.text.trim() === t).length;
  const lastUserTurn = (trs: ChatTurn[]) => trs.findLast((m) => m.role === 'user') ?? null;
  /** Text the chat never shows as a user turn (the transcript reader's `is_system_noise`, plus
   *  slash and `!` commands, which Claude records as tags): a bubble for it could never retire. */
  const neverEchoed = (text: string) =>
    /^\s*(\/[\w:.-]+(\s|$)|[!<⟦]|\[Request interrupted|Caveat:)/.test(text);

  /** Has the transcript caught up on this message? One more exact match than at send time, or a
   *  NEW last user turn containing it: Claude merges a queued message handed back on interrupt
   *  with the next one into ONE user turn (the phone's `echoedBy`). */
  function echoed(trs: ChatTurn[], o: Outgoing): boolean {
    const t = o.text.trim();
    if (sameText(trs, t) > o.seen) return true;
    const last = lastUserTurn(trs);
    return !!last && last.msg_id !== o.lastUser && last.text.includes(t);
  }
  /** Or the agent has moved past it: a queue read asked for after the send no longer holds it,
   *  and the chat has a different last user turn, or has scrolled the send-time chat out of its
   *  40-turn window entirely. Catches an echo that scrolled out while another chat was open,
   *  where no match can ever be found. Agent turns alone don't count: the queue read can lag. */
  function taken(trs: ChatTurn[], o: Outgoing): boolean {
    if (!o.sentAt || metaAskedAt <= o.sentAt || turnsAskedAt <= o.sentAt) return false;
    if (queuedTexts.has(o.text.trim())) return false;
    if ((lastUserTurn(trs)?.msg_id ?? null) !== o.lastUser) return true;
    return o.lastTurn !== null && !trs.some((m) => m.msg_id === o.lastTurn);
  }
  // Retire what the open chat's transcript now shows. Only from a transcript read for that chat.
  $effect(() => {
    const chat = loadedFor;
    if (!chat) return;
    const trs = turns;
    const keep = outgoing.filter((o) => o.chat !== chat || !o.sentAt || !(echoed(trs, o) || taken(trs, o)));
    if (keep.length !== outgoing.length) outgoing = keep;
  });
  const queuedTexts = $derived(new Set((meta?.queued ?? []).map((q) => q.text.trim())));
  const outgoingHere = $derived(outgoing.filter((o) => o.chat === openId));

  // ── Attachments: the terminal composer's (lib/composer/attachments.ts) — Cmd+V of a screenshot
  //    or Finder files, a menu paste, a drop onto the composer — kept per chat like drafts. ──
  let attached = $state<Record<string, ComposerAttachment[]>>({});
  const attachedHere = $derived(openId ? (attached[openId] ?? []) : []);
  const attach = (chat: string, items: ComposerAttachment[]) =>
    (attached = { ...attached, [chat]: mergeAttachments(attached[chat] ?? [], items) });
  const unattach = (chat: string, index: number) =>
    (attached = { ...attached, [chat]: (attached[chat] ?? []).filter((_, i) => i !== index) });
  let composerEl = $state<HTMLTextAreaElement | null>(null);
  let dockEl = $state<HTMLElement | null>(null);
  let dragOver = $state(false);

  async function pasteInto(chat: string) {
    try {
      const r = await readPaste();
      if (r?.kind === 'attachments') attach(chat, r.items);
      else if (r?.kind === 'text' && composerEl && openId === chat) {
        composerEl.setRangeText(r.text, composerEl.selectionStart, composerEl.selectionEnd, 'end');
        drafts = { ...drafts, [chat]: composerEl.value };
      }
    } catch (e) {
      sendNote = { chat, text: `Couldn't paste: ${e}` };
    }
  }
  function onComposerPaste(e: ClipboardEvent) {
    // Cmd+V is handled in onComposerKey; this catches a menu paste carrying files.
    if (openId && pasteCarriesFiles(e)) {
      e.preventDefault();
      void pasteInto(openId);
    }
  }
  // A drop lands on the chat it was dropped on, while Focus is on screen. The webview's drop
  // event is window-wide (HTML5 drop is off under Tauri), so it is bounds-checked to the dock.
  // A read-only column has no composer to attach to.
  $effect(() => {
    if (!active || readOnly) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    void getCurrentWebview().onDragDropEvent((ev) => {
      const p = ev.payload;
      const inside = (raw: { x: number; y: number }) => {
        const pos = dropPointToCss(raw);
        const r = dockEl?.getBoundingClientRect();
        return !!r && pos.x >= r.left && pos.x <= r.right && pos.y >= r.top && pos.y <= r.bottom;
      };
      if (p.type === 'over') dragOver = inside(p.position);
      else if (p.type === 'drop') {
        const was = dragOver;
        dragOver = false;
        if (was && openId) { attach(openId, fromPaths(p.paths)); composerEl?.focus(); }
      } else dragOver = false;
    }).then((u) => { if (gone) u(); else unlisten = u; });
    return () => { gone = true; unlisten?.(); dragOver = false; };
  });

  async function send() {
    const chat = openId;
    const text = chat ? (drafts[chat] ?? '') : '';
    const files = chat ? (attached[chat] ?? []) : [];
    // Not before the chat's first read: the bubble's baseline is that read.
    if (!chat || (!text.trim() && !files.length) || sending || loadedFor !== chat) return;
    sending = true;
    sendNote = null;
    // The bubble goes up and the composer empties at once; a refusal brings the text back.
    const id = ++outSeq;
    // Attachments alone get no bubble: there is no text to recognise the echo by.
    const bubble = !!text.trim() && !neverEchoed(text);
    attached = { ...attached, [chat]: [] };
    // The bubble goes up on the last poll's baseline, then takes a FRESH one before anything is
    // typed: the poll can be 3 s old, and a user turn written since (a message sent just before,
    // one from the phone) would otherwise read as this one being taken.
    const baseline = (trs: ChatTurn[]) => ({
      seen: sameText(trs, text.trim()), lastUser: lastUserTurn(trs)?.msg_id ?? null, lastTurn: trs[trs.length - 1]?.msg_id ?? null,
    });
    if (bubble) outgoing = [...outgoing, { id, chat, text, sentAt: 0, ...baseline(turns) }];
    drafts = { ...drafts, [chat]: '' };
    pinned = true;
    if (bubble) {
      const fresh = await getTabTranscript(chat).catch(() => null);
      if (fresh) outgoing = outgoing.map((o) => (o.id === id ? { ...o, ...baseline(fresh) } : o));
    }
    const fail = (note: string) => {
      outgoing = outgoing.filter((o) => o.id !== id);
      if (!(drafts[chat] ?? '').trim()) drafts = { ...drafts, [chat]: text };
      if (files.length) attach(chat, files);
      sendNote = { chat, text: note };
    };
    try {
      const r = await sendTabMessage(chat, text, files.map((f) => f.path));
      if (r.status === 'delivered') {
        const at = Date.now();
        outgoing = outgoing.map((o) => (o.id === id ? { ...o, sentAt: at } : o));
        const sentFiles = files.length ? `Sent ${files.length === 1 ? files[0].name : `${files.length} files`}.` : '';
        sendNote = r.woke
          ? { chat, text: `Woke the agent (${r.woke === 'init' ? 're-registered it' : 'resumed it'}) and sent.` }
          : sentFiles && !bubble ? { chat, text: sentFiles } : null;
        reload();
      } else {
        fail(r.detail ?? `Not sent (${r.reason ?? r.status}).`);
      }
    } catch (e) {
      fail(`Not sent: ${e}`);
    } finally {
      sending = false;
    }
  }
  function onComposerKey(e: KeyboardEvent) {
    if (e.key === 'Enter' && !e.shiftKey && !e.isComposing) {
      e.preventDefault();
      void send();
    } else if (isModKey(e) && !e.shiftKey && e.key.toLowerCase() === 'v' && openId) {
      // The native pasteboard, as the terminal composer reads it: WKWebView's clipboardData
      // misses Finder file copies and screenshots.
      e.preventDefault();
      void pasteInto(openId);
    } else if (e.key === 'Escape') {
      (e.currentTarget as HTMLElement).blur();
    }
  }

  /** Links in a transcript open in the browser. WKWebView drops `target=_blank` without a
   *  new-window handler, so a plain link would do nothing (NotesPanel does the same). */
  function onChatClick(e: MouseEvent) {
    const anchor = (e.target as Element | null)?.closest?.('a');
    if (!anchor?.href) return;
    e.preventDefault();
    void shellOpen(anchor.href);
  }

  // Keep the newest turn in view as the chat grows, unless the human scrolled up to read.
  let chatEl = $state<HTMLElement | null>(null);
  let pinned = true;
  const toBottom = () => { if (chatEl && pinned) chatEl.scrollTop = chatEl.scrollHeight; };
  // What's at the bottom, not `rows.length` (at the 40-turn window a new turn pushes an old one out
  // and the count doesn't move) and not `rows` itself (a new array every poll, which yanked a fold
  // the human had just opened at the bottom to its end within 3 s): the last row, how big a run it
  // is, and what's drawn under it.
  const tailKey = $derived.by(() => {
    const r = rows.at(-1);
    const key = !r ? '' : r.kind === 'tools' || r.kind === 'task' || r.kind === 'added' ? r.key : r.turn.msg_id;
    const n = r?.kind === 'tools' ? r.turns.length : r?.kind === 'added' ? r.events.length : 0;
    return `${rows.length}|${key}|${n}|${outgoingHere.length}|${liveState?.state === 'active'}`;
  });
  // Another chat opens at its newest turn, whatever was scrolled up in the last one (before the
  // tail effect, which runs in the same flush).
  $effect(() => { void openId; pinned = true; });
  $effect(() => {
    void tailKey;
    if (chatEl && pinned) requestAnimationFrame(toBottom);
  });
  // The chat's OWN height changes too: a prompt card opening (or resizing) in the dock below
  // shrinks it, which leaves the newest lines under the edge — and the next scroll event (scroll
  // anchoring as content changes) then read that as the human having scrolled up, unpinning it.
  // And what's drawn grows without the tail changing: a bubble's "Sending…" becoming the longer
  // "Queued · …" line, an image arriving, the working line's detail — so the content is watched too.
  // Except growth the human just caused: opening a fold at the bottom must not scroll its top away.
  $effect(() => {
    const el = chatEl;
    if (!el) return;
    const box = new ResizeObserver(toBottom);
    box.observe(el);
    // Only a CLICK holds it off (that's what opens a fold): a wheel's momentum at the bottom must
    // still follow growth.
    const content = new ResizeObserver(() => { if (!dragging && performance.now() - clickAt > 800) toBottom(); });
    for (const c of el.children) content.observe(c);
    return () => { box.disconnect(); content.disconnect(); };
  });
  // Only the HUMAN unpins it. Position alone can't tell them apart: content that grows between our
  // scroll and its scroll event reads as "scrolled up" (the event fires before the ResizeObserver
  // in a frame), which left a just-sent bubble under the edge for good. So unpinning needs a wheel,
  // touch, key or scrollbar drag in the chat just before; reaching the bottom by any means re-pins.
  let intentAt = 0;
  let clickAt = 0;
  let dragging = false;
  /** The last press was in the chat: WebKit keyboard-scrolls the last-clicked scroller, but the
   *  key goes to body (the chat has no tabindex), so a scroll key is read window-wide. */
  let pressedInChat = false;
  const onChatIntent = () => { intentAt = performance.now(); };
  const SCROLL_KEYS = new Set(['ArrowUp', 'ArrowDown', 'PageUp', 'PageDown', 'Home', 'End', ' ']);
  $effect(() => {
    const press = (e: PointerEvent) => { pressedInChat = !!chatEl && chatEl.contains(e.target as Node); };
    const key = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (pressedInChat && SCROLL_KEYS.has(e.key) && !t?.closest?.('input, textarea, [contenteditable]')) onChatIntent();
    };
    window.addEventListener('pointerdown', press, true);
    window.addEventListener('keydown', key, true);
    return () => { window.removeEventListener('pointerdown', press, true); window.removeEventListener('keydown', key, true); };
  });
  const onChatPointerDown = () => {
    dragging = true;
    // A native drag (a link, an image, a selection) ends without a pointerup.
    const end = () => {
      dragging = false;
      clickAt = performance.now();
      onChatIntent();
      for (const t of ['pointerup', 'pointercancel', 'dragend'] as const) window.removeEventListener(t, end, true);
    };
    for (const t of ['pointerup', 'pointercancel', 'dragend'] as const) window.addEventListener(t, end, true);
  };
  const onChatScroll = () => {
    if (!chatEl) return;
    if (chatEl.scrollHeight - chatEl.scrollTop - chatEl.clientHeight < 40) pinned = true;
    else if (dragging || performance.now() - intentAt < 800) pinned = false;
  };

  const EVENT_LABEL ={ added: 'Added a task', asked: 'Asked you', answered: 'Answered', note: 'Note' } as const;

  /** A tool marker's argument: `Bash(git status)` reads as `git status` beside its verb. */
  const toolArg = (text: string) => text.match(/^[^(]+\(([\s\S]*?)\)?$/)?.[1]?.trim() || text;
  const sentence = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
</script>

<div class="convo">
  {#if openId}
    <header class="chat-h">
      <div class="who">
        <b>{name}</b>
        <span>{workspace} · {liveState?.state === 'permission' ? 'waiting on a permission' : asks ? 'waiting on you' : (liveState?.state ?? 'no session')}</span>
      </div>
      <div class="gauges">
        {#if meta?.model}
          {#if pickable}
            <button class="chip model" aria-label="Switch model" aria-haspopup="menu" onclick={openModelPicker}>{meta.model} <span aria-hidden="true">▾</span></button>
          {:else}
            <span class="chip model">{meta.model}</span>
          {/if}
        {/if}
        {#if meta?.contextLimit && meta.contextLimit >= 1_000_000 && !/\d\s*m\b/i.test(meta.model ?? '')}
          <span class="win">{fmtTok(meta.contextLimit)}</span>
        {/if}
        {#if pickable}
          <!-- Unknown is common (a resumed session stops recording effort), so it reads as an
               offer to set one, never as a value. -->
          <button class="chip effort" class:unset={!effortShown} aria-label={effortShown ? 'Change reasoning effort' : 'Set reasoning effort'} aria-haspopup="menu" onclick={openEffortPicker}>{effortShown ?? 'set effort'}</button>
        {:else if effortShown}
          <span class="chip effort">{effortShown}</span>
        {/if}
        <span class="ctx" style="--t: {ctxTone(ctx)}"><i><s style="width: {Math.min(100, ctx ?? 0)}%"></s></i><em>{ctx ?? '—'}% context</em>{#if meta?.contextUsed != null && meta.contextLimit}<span class="tok">{fmtTok(meta.contextUsed)}/{fmtTok(meta.contextLimit)}</span>{/if}</span>
        {@render header?.(openId)}
      </div>
      <div class="acts">
        <button onclick={() => void navigateToTab(openId)}>Open the tab</button>
      </div>
    </header>

    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="chat" bind:this={chatEl} onscroll={onChatScroll} onclick={onChatClick}
      onwheel={onChatIntent} ontouchmove={onChatIntent} onpointerdown={onChatPointerDown}>
      {#if loadedFor !== openId}
        <p class="hint">Reading the chat…</p>
      {:else if !rows.length}
        <p class="hint">No messages captured for this tab yet.</p>
      {/if}
      <div class="column">
      {#each rows as r (r.kind === 'tools' || r.kind === 'task' || r.kind === 'added' ? r.key : r.turn.msg_id)}
        {#if r.kind === 'tools'}
          {@const shown = expanded.includes(r.key)}
          <div class="act">
            <button class="act-line" aria-expanded={shown} onclick={() => toggle(r.key)}>
              <span class="act-verb">{sentence(r.verbs.join(', '))}</span>
              <span class="act-what">{r.turns.length === 1 ? toolArg(r.turns[0].text) : `${r.turns.length} steps`}</span>
              <span class="chev" aria-hidden="true">{shown ? '▾' : '▸'}</span>
            </button>
            {#if shown}
              <div class="calls">{#each r.turns as t (t.msg_id)}<code>{t.text}</code>{/each}</div>
            {/if}
          </div>
        {:else if r.kind === 'added'}
          <div class="event" data-kind="added">
            <span class="ev-head">{r.events.length === 1 ? 'Added a task' : `Added ${r.events.length} tasks`}</span>
            <ul>
              {#each r.events as e (e.key)}
                <li><button onclick={() => loomStore.show('weave', e.taskId)}>{e.title}</button></li>
              {/each}
            </ul>
          </div>
        {:else if r.kind === 'task'}
          <button class="event" data-kind={r.event.kind} onclick={() => loomStore.show('weave', r.event.taskId)}>
            <span class="ev-head">{EVENT_LABEL[r.event.kind]} · {r.event.title}</span>
            <span class="ev-body">{r.event.text}</span>
          </button>
        {:else if r.kind === 'rule' && r.turn.kind === 'goal_status'}
          <div class="event" data-kind="goal">
            <span class="ev-head">Goal {r.turn.goal?.event ?? ''}</span>
            {#if r.turn.goal?.condition}<span class="ev-body">{r.turn.goal.condition}</span>{/if}
          </div>
        {:else if r.kind === 'rule'}
          {@const inbound = r.turn.peer?.direction === 'in'}
          {@const long = r.turn.text.length > 320 || r.turn.text.split('\n').length > 4}
          {@const shown = expanded.includes(r.turn.msg_id)}
          <div class="peer" data-dir={inbound ? 'in' : 'out'}>
            <span class="peer-h">
              {inbound ? 'From' : 'To'} <b>{r.turn.peer?.name ?? 'a peer'}</b>{#if r.turn.peer?.topic}<span class="topic">on {r.turn.peer.topic}</span>{/if}
            </span>
            <p class="peer-body" class:clamped={long && !shown}>{r.turn.text}</p>
            {#if long}
              <button class="more" onclick={() => toggle(r.turn.msg_id)}>{shown ? 'Show less' : 'Show all'}</button>
            {/if}
          </div>
        {:else if r.turn.kind === 'terminal_snapshot'}
          <pre class="snap">{r.turn.text}</pre>
        {:else if r.turn.role === 'user' && r.turn.typedBy}
          <!-- Typed by maiTerm, not the human: an Overlord directive, or a message sent for
               them. Folded and named, so the human's own bubbles are only their words. -->
          {@const tb = r.turn.typedBy}
          {@const shown = expanded.includes(r.turn.msg_id)}
          <div class="act">
            <button class="act-line" aria-expanded={shown} onclick={() => toggle(r.turn.msg_id)}>
              <span class="act-verb">{tb.by === 'overlord' ? 'Overlord' : 'Sent for you'}{tb.rule ? ` · ${tb.rule}` : ''}</span>
              {#if !shown}<span class="act-what plain">{r.turn.text}</span>{/if}
              <span class="chev" aria-hidden="true">{shown ? '▾' : '▸'}</span>
            </button>
            {#if shown}
              <p class="typed">{r.turn.text}</p>
            {/if}
          </div>
        {:else if r.turn.role === 'user' && injectedTurn(r.turn.text)}
          {@const inj = injectedTurn(r.turn.text)!}
          {@const shown = expanded.includes(r.turn.msg_id)}
          <div class="act">
            <button class="act-line" aria-expanded={shown} onclick={() => toggle(r.turn.msg_id)}>
              <span class="act-verb">{inj.label}</span>
              <span class="chev" aria-hidden="true">{shown ? '▾' : '▸'}</span>
            </button>
            {#if shown}
              <div class="agent report">{@html renderTurnMarkdown(inj.body)}</div>
            {/if}
          </div>
        {:else if r.turn.role === 'user'}
          <div class="you">{r.turn.text}</div>
        {:else if r.turn.role === 'agent'}
          <div class="agent" use:chatImages={openId}>{@html renderTurnMarkdown(r.turn.text)}</div>
        {:else}
          <div class="sys">{r.turn.text}</div>
        {/if}
      {/each}
      {#each outgoingHere as o (o.id)}
        {@const queued = !!o.sentAt && queuedTexts.has(o.text.trim())}
        <div class="you pending" class:sending={!o.sentAt}>
          {o.text}
          <span class="state">{!o.sentAt ? 'Sending…' : queued ? 'Queued · the agent takes it when this turn ends' : 'Delivered'}</span>
        </div>
      {/each}
      {#if liveState?.state === 'active'}
        <p class="working"><i></i>{liveState.toolDetail ?? liveState.toolName ?? 'Working…'}</p>
      {/if}
      </div>
    </div>

    {#if !readOnly}
      <div class="dock" bind:this={dockEl}>
        <PromptCard tabId={openId} {active} pulse={liveState?.state} />
        {#if openAsk}
          {@const chat = openId}
          {#key openAsk.id}
            <BlockerCard task={openAsk} variant="card" onnote={(text) => (outcome = { chat, text })} />
          {/key}
        {/if}
        {#each openScripts as a (a.followUp.id)}
          {@const chat = openId}
          <ScriptApprovalCard tabId={chat} followUp={a.followUp} listChangedAt={scriptsChangedAt} onnote={(text) => (outcome = { chat, text })} />
        {/each}
        {#if outcome && outcome.chat === openId}<p class="hint">{outcome.text}</p>{/if}
        <AttachmentChips attachments={attachedHere} onremove={(i) => { unattach(openId, i); composerEl?.focus(); }} />
        <div class="composer" class:drag-over={dragOver}>
          {@render composerActions?.(openId)}
          <textarea
            bind:this={composerEl}
            rows="2"
            placeholder={`Message ${name}… (paste or drop files to attach)`}
            value={draft}
            oninput={(e) => (drafts = { ...drafts, [openId]: (e.currentTarget as HTMLTextAreaElement).value })}
            onkeydown={onComposerKey}
            onpaste={onComposerPaste}
            disabled={sending}
          ></textarea>
          <button class="send" onclick={() => void send()} disabled={sending || (!draft.trim() && !attachedHere.length) || loadedFor !== openId}>{sending ? 'Sending…' : 'Send'}</button>
        </div>
        {#if sendNote && sendNote.chat === openId}<p class="hint">{sendNote.text}</p>{/if}
        {#if headNote && headNote.chat === openId}<p class="hint">{headNote.text}</p>{/if}
      </div>
    {/if}
  {:else}
    <p class="hint pad">Pick a chat on the left.</p>
  {/if}

  <!-- Fixed-position, so where it sits in the DOM doesn't place it; inside the root so a caller
       lays out one element. -->
  {#if pickMenu && openId}
    <ContextMenu items={pickMenu.items} x={pickMenu.x} y={pickMenu.y} anchor={pickMenu.anchor} onclose={() => (pickMenu = null)} />
  {/if}
</div>

<style>
  .convo { display: flex; flex-direction: column; min-width: 0; min-height: 0; }
  .chat-h { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 14px; padding: 10px 18px; border-bottom: 1px solid var(--bg-light); }
  .who { display: flex; flex-direction: column; min-width: 0; }
  .who b { font-size: 14px; overflow-wrap: anywhere; }
  .who span { font-size: 11px; color: var(--fg-dim); }
  .gauges { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 12px; font-size: 11px; color: var(--fg-dim); }
  .ctx { display: inline-flex; align-items: center; gap: 6px; font-variant-numeric: tabular-nums; }
  .ctx i { display: block; width: 60px; height: 3px; border-radius: 2px; background: var(--bg-light); overflow: hidden; }
  .ctx s { display: block; height: 100%; background: var(--t); }
  .ctx em { font-style: normal; color: var(--t); }
  .tok { margin-left: 4px; opacity: 0.7; }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: none;
    border: 1px solid transparent;
    border-radius: 5px;
    padding: 2px 6px;
    color: var(--fg);
    font: inherit;
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 11px;
  }
  button.chip { cursor: pointer; }
  button.chip:hover, button.chip[aria-haspopup]:focus-visible { border-color: var(--bg-light); background: var(--bg-medium); }
  .chip span { font-size: 9px; color: var(--fg-dim); }
  .chip.effort { background: var(--bg-medium); text-transform: uppercase; letter-spacing: 0.05em; font-size: 10px; }
  .chip.effort.unset { color: var(--fg-dim); text-transform: none; letter-spacing: 0; }
  .win { font-family: var(--font-mono, ui-monospace, monospace); font-size: 9.5px; font-weight: 600; color: var(--accent); border: 1px solid color-mix(in srgb, var(--accent) 45%, transparent); border-radius: 3px; padding: 0 4px; }
  .acts { display: flex; gap: 6px; margin-left: auto; }
  /* The same button as the Loom's Release (FocusView's `.await button`). */
  .acts button {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    color: var(--fg);
    border-radius: 6px;
    padding: 4px 10px;
    font: inherit;
    font-size: 11.5px;
    cursor: pointer;
  }

  /* The chat reads as one column at a reading measure: prose at the left edge, the human's own
     messages at the right, and everything that isn't conversation (tool steps, task events,
     peer traffic) indented off a thin left rule so the eye can skip it. */
  /* The app is unselectable by default (app.css); the chat is read and quoted. */
  .chat { -webkit-user-select: text; user-select: text; }
  .chat { flex: 1; min-height: 0; overflow-y: auto; padding: 16px 24px 20px; }
  .column { max-width: 76ch; margin: 0 auto; display: flex; flex-direction: column; gap: 14px; font-size: 13px; line-height: 1.55; }

  .you {
    align-self: flex-end;
    max-width: 80%;
    background: var(--bg-light);
    border-radius: 14px 14px 4px 14px;
    padding: 8px 13px;
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .you.pending { display: flex; flex-direction: column; gap: 3px; }
  .you.sending { opacity: 0.6; }
  .you .state { align-self: flex-end; font-size: 10.5px; color: var(--fg-dim); white-space: normal; }
  .agent { overflow-wrap: anywhere; }
  .agent :global(p) { margin: 0 0 8px; }
  .agent :global(p:last-child) { margin-bottom: 0; }
  .agent :global(ul), .agent :global(ol) { margin: 0 0 8px; padding-left: 1.4em; }
  .agent :global(li) { margin: 2px 0; }
  .agent :global(li::marker) { color: var(--fg-dim); }
  .agent :global(pre) { background: var(--bg-medium); padding: 9px 11px; border-radius: 6px; overflow-x: auto; font-size: 11.5px; }
  .agent :global(code) { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.9em; background: var(--bg-medium); padding: 1px 4px; border-radius: 3px; }
  .agent :global(pre code) { background: none; padding: 0; }
  .agent :global(a) { color: var(--accent); }
  /* An image the agent showed by its path (loom/chatImages.ts): the placeholder until it loads,
     its alt text plus the reason when it can't be shown. */
  .agent :global(.md-img) { color: var(--fg-dim); font-style: italic; }
  .agent :global(img.md-img-loaded) { display: block; max-width: 100%; max-height: 480px; margin: 6px 0; border-radius: 6px; border: 1px solid var(--bg-light); }
  .agent :global(strong) { color: var(--fg); font-weight: 600; }
  .agent :global(table) { display: block; overflow-x: auto; border-collapse: collapse; margin: 4px 0 8px; font-size: 12px; font-variant-numeric: tabular-nums; }
  .agent :global(th) { text-align: left; font-weight: 600; color: var(--fg-dim); border-bottom: 1px solid var(--bg-light); }
  .agent :global(th), .agent :global(td) { padding: 4px 14px 4px 0; white-space: nowrap; }
  .agent :global(tr + tr td) { border-top: 1px solid color-mix(in srgb, var(--bg-light) 45%, transparent); }
  .sys { font-size: 12px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .snap { margin: 0; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; white-space: pre-wrap; overflow-wrap: anywhere; background: var(--bg-medium); padding: 8px; border-radius: 6px; }

  /* Tool steps: one quiet line each, the command cut to one line; the full calls on demand. */
  .act { margin: -6px 0; }
  .act-line {
    display: flex;
    align-items: baseline;
    gap: 8px;
    width: 100%;
    min-width: 0;
    background: none;
    border: 0;
    border-left: 2px solid var(--bg-light);
    padding: 1px 0 1px 10px;
    color: var(--fg-dim);
    font: inherit;
    font-size: 12px;
    text-align: left;
    cursor: pointer;
  }
  .act-line:hover { color: var(--fg); border-left-color: var(--fg-dim); }
  .act-verb { flex: none; }
  .act-what { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; opacity: 0.8; }
  .chev { flex: none; font-size: 10px; }
  .act-what.plain { font-family: inherit; font-size: 11.5px; }
  .typed { margin: 6px 0 4px 12px; padding-left: 10px; border-left: 2px solid var(--bg-light); font-size: 12.5px; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--fg-dim); }
  .report { margin: 6px 0 4px 12px; padding-left: 10px; border-left: 2px solid var(--bg-light); font-size: 12.5px; }
  .calls { display: flex; flex-direction: column; gap: 4px; margin: 4px 0 2px 12px; }
  .calls code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--fg-dim); white-space: pre-wrap; overflow-wrap: anywhere; }

  /* Task events: a short colored rule on the left, what happened, then to which task. */
  .event {
    --c: var(--fg-dim);
    display: flex;
    flex-direction: column;
    gap: 2px;
    width: 100%;
    background: none;
    border: 0;
    border-left: 2px solid var(--c);
    padding: 1px 0 1px 10px;
    color: var(--fg);
    font: inherit;
    font-size: 12px;
    text-align: left;
  }
  button.event { cursor: pointer; }
  .event[data-kind='asked'] { --c: var(--orange, #ff9e64); }
  .event[data-kind='answered'] { --c: var(--green); }
  .event[data-kind='added'] { --c: var(--cyan); }
  .event[data-kind='goal'] { --c: var(--magenta); }
  .ev-head { color: var(--c); font-weight: 500; overflow-wrap: anywhere; }
  .ev-body { color: var(--fg-dim); overflow-wrap: anywhere; }
  .event ul { margin: 2px 0 0; padding: 0; list-style: none; display: flex; flex-direction: column; gap: 1px; }
  .event li button { background: none; border: 0; padding: 0; color: var(--fg-dim); font: inherit; text-align: left; cursor: pointer; overflow-wrap: anywhere; }
  .event li button:hover { color: var(--fg); }

  /* Peer traffic: who it is from or to, in plain words, then the message as written. Long
     ones fold to four lines. */
  .peer {
    --c: var(--accent);
    display: flex;
    flex-direction: column;
    gap: 3px;
    border-left: 2px solid var(--c);
    padding: 2px 0 2px 12px;
    font-size: 12.5px;
  }
  .peer[data-dir='out'] { --c: color-mix(in srgb, var(--accent) 55%, var(--fg-dim)); }
  .peer-h { color: var(--fg-dim); font-size: 11.5px; }
  .peer-h b { color: var(--c); font-weight: 600; }
  .topic { margin-left: 6px; }
  .peer-body { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; color: var(--fg); opacity: 0.88; }
  .peer-body.clamped { display: -webkit-box; -webkit-box-orient: vertical; -webkit-line-clamp: 4; line-clamp: 4; overflow: hidden; }
  .more { align-self: flex-start; background: none; border: 0; padding: 0; color: var(--accent); font: inherit; font-size: 11.5px; cursor: pointer; }

  .act-line:focus-visible, .event:focus-visible, .more:focus-visible, .event li button:focus-visible { outline: 1px solid var(--accent); outline-offset: 2px; }
  .working { margin: 0; display: flex; align-items: center; gap: 8px; font-size: 11.5px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .working i { width: 6px; height: 6px; border-radius: 50%; background: var(--green); flex: none; animation: pulse 1.2s ease-in-out infinite; }
  /* Keyframes are scoped per component: FocusView's list dots have their own copy. */
  @keyframes pulse { 50% { opacity: 0.35; } }
  @media (prefers-reduced-motion: reduce) { .working i { animation: none; } }

  .dock { display: flex; flex-direction: column; gap: 8px; padding: 10px 18px 14px; border-top: 1px solid var(--bg-light); background: var(--bg-dark); }
  .composer { display: flex; gap: 8px; align-items: flex-end; border-radius: 8px; }
  .composer.drag-over textarea { border-color: var(--accent); background: var(--bg-light); }
  .composer textarea {
    flex: 1;
    min-width: 0;
    resize: vertical;
    min-height: 40px;
    max-height: 40vh;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    color: var(--fg);
    font: inherit;
    font-size: 12.5px;
    line-height: 1.45;
    padding: 8px 10px;
  }
  .composer textarea:focus { outline: none; border-color: var(--accent); }
  .send { background: var(--accent); color: var(--bg-dark); border: 0; border-radius: 8px; padding: 8px 14px; font: inherit; font-size: 12px; font-weight: 600; cursor: pointer; }
  .send:disabled { opacity: 0.45; cursor: default; }
  .hint { margin: 0; font-size: 11.5px; color: var(--fg-dim); }
  .pad { padding: 18px; }
</style>
