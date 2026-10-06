<!--
  Focus: where the human works with the agents without going into their terminals.

  On the left the chats that need you, the ones working now, and anything unread or active since
  the start of yesterday (the phone's inbox rules). In the middle the chosen chat, condensed the
  way the phone shows it (tool runs folded, events as thin rules, task events placed where they
  happened), then whatever the agent is stopped at (a permission, a question, a task question),
  answerable in place, and a composer. On the right that agent's work.

  The header carries what the Fleet cards used to: state, context, a running ritual, an
  unanswered directive with Release, and the Trigger menu.
-->
<script lang="ts">
  import { loomStore } from '$lib/stores/loom.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { tabDisplayName, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { getTabMeta, getTabsLastActivity, getTabTranscript, listTabModels, sendTabMessage, type ChatTurn, type TabMeta } from '$lib/tauri/commands';
  import { chatRows, FOCUS_WINDOWS, focusSections, injectedTurn, taskEventsFor, type FocusChat } from '$lib/loom/model';
  import { renderTurnMarkdown } from '$lib/loom/markdown';
  import { chatImages } from '$lib/loom/chatImages';
  import { BLOCKER_LABEL, isRetired } from '$lib/tasks/model';
  import { fireRefusal, fmtAge } from '$lib/overlord/format';
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import ScriptApprovalCard from '$lib/components/followUps/ScriptApprovalCard.svelte';
  import { followUpsStore, type PendingApproval } from '$lib/stores/followUps.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Resizer from '$lib/components/Resizer.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import PromptCard from './PromptCard.svelte';
  import AttachmentChips from '$lib/components/composer/AttachmentChips.svelte';
  import { fromPaths, mergeAttachments, pasteCarriesFiles, readPaste, type ComposerAttachment } from '$lib/composer/attachments';
  import { isModKey } from '$lib/utils/platform';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import type { Task, Workspace } from '$lib/tauri/types';

  interface Props {
    workspaces: Workspace[];
    tasks: Task[];
    now: number;
    /** The deck is on screen: poll and keep the chat live only then. */
    active: boolean;
  }
  let { workspaces, tasks, now, active }: Props = $props();

  interface Chat extends FocusChat { name: string; workspace: string; preview: string }

  const asking = (tabId: string) =>
    tasks.find((t) => t.tab_id === tabId && t.status === 'blocked' && (t.blocker?.kind === 'decision' || t.blocker?.kind === 'action'));

  /** Watch scripts waiting to be allowed, by tab (docs/follow-ups.md §5.1). They wait on the human
   *  exactly as a task question does, so they pin a chat in Needs you too — the phone does
   *  (`scriptsWaiting`) and the Loom's header counts them. */
  const scriptsByTab = $derived.by(() => {
    const m = new Map<string, PendingApproval[]>();
    for (const a of followUpsStore.pendingApprovals) {
      if (a.archived) continue;
      m.set(a.tabId, [...(m.get(a.tabId) ?? []), a]);
    }
    return m;
  });

  // ── When each agent tab last did something, by the phone's rule (mailink `last_activity_ts`:
  // last real transcript turn, else scrollback time, else `suspended_at`, else now). Hook state
  // alone can't say: it is in memory, and auto-resume restarts every agent at launch. Read here
  // rather than from Overlord's facts, which are polled only while Overlord is on and never for
  // exempt tabs. The key is the id list, so a state change doesn't restart the poll.
  let lastActivity = $state<Record<string, number>>({});
  const agentTabIds = $derived(
    workspaces
      .flatMap((w) => w.panes.flatMap((p) => p.tabs))
      .filter((t) => t.tab_type === 'terminal' && !t.service_id && claudeStateStore.getState(t.id))
      .map((t) => t.id)
      .join(','),
  );
  $effect(() => {
    const ids = agentTabIds ? agentTabIds.split(',') : [];
    if (!active || !ids.length) return;
    let alive = true;
    const poll = () => getTabsLastActivity(ids).then((m) => { if (alive) lastActivity = m; }).catch(() => {});
    void poll();
    const timer = setInterval(poll, 10_000);
    return () => { alive = false; clearInterval(timer); };
  });

  const chats = $derived.by<Chat[]>(() =>
    workspaces.flatMap((w) =>
      w.panes.flatMap((p) =>
        p.tabs
          .filter((t) => t.tab_type === 'terminal' && !t.service_id)
          .map((t) => {
            const s = claudeStateStore.getState(t.id);
            const ask = asking(t.id);
            const scripts = scriptsByTab.get(t.id)?.length ?? 0;
            const scriptAsk = !scripts ? null : scripts === 1 ? 'Allow a watch script?' : `Allow ${scripts} watch scripts?`;
            return {
              tabId: t.id,
              name: tabDisplayName(t.id),
              workspace: w.name,
              state: s?.state ?? null,
              unread: s?.state === 'idle' && s.read === false,
              // The phone's rule (a resume does not move it), or a hook event seen since,
              // whichever is newer.
              lastActivity: Math.max(lastActivity[t.id] ?? 0, s?.updatedAt ?? 0),
              asks: !!ask || scripts > 0,
              preview: s?.state === 'permission' ? 'Needs your approval' : ask?.blocker?.question ?? scriptAsk ?? (s?.state === 'active' ? (s.toolDetail ?? s.toolName ?? 'Working…') : ''),
            };
          })
          // The open chat stays listed even when it stops qualifying. Answering the question on
          // a chat whose agent has exited clears its only reason to be here, and dropping it
          // would jump Focus to another chat and hide the "could not be told" receipt, which
          // is exactly the case that receipt is for.
          .filter((c) => c.state !== null || c.asks || c.tabId === loomStore.focusChatTabId),
      ),
    ),
  );
  /** How far back idle chats are listed: a view filter, changed where it is used and kept per
   *  viewer like the column widths. Needs-you and working chats show whatever it is. */
  const DAYS_KEY = 'maiterm.loom.focus.days';
  let windowDays = $state<number>(1);
  try {
    const saved = Number(localStorage.getItem(DAYS_KEY));
    if (FOCUS_WINDOWS.some((w) => w.days === saved) && localStorage.getItem(DAYS_KEY) !== null) windowDays = saved;
  } catch { /* the default */ }
  const windowLabel = $derived(FOCUS_WINDOWS.find((w) => w.days === windowDays)?.label ?? 'Since yesterday');
  let windowMenu = $state<{ x: number; y: number; anchor: HTMLElement } | null>(null);
  function openWindowMenu(e: MouseEvent) {
    if (windowMenu) { windowMenu = null; return; }
    const anchor = e.currentTarget as HTMLElement;
    const r = anchor.getBoundingClientRect();
    windowMenu = { x: r.left, y: r.bottom + 4, anchor };
  }
  function setWindow(days: number) {
    windowDays = days;
    try { localStorage.setItem(DAYS_KEY, String(days)); } catch { /* not kept */ }
  }
  const sections = $derived(focusSections(chats, now, loomStore.focusChatTabId, windowDays));
  const listed = $derived([...sections.needsYou, ...sections.working, ...sections.recent]);

  /** The open chat: the one picked, else the first that needs you. The fallback is written back
   *  as the pick, so the chat stays put when answering it moves it out of "Needs you". */
  const openId = $derived(
    loomStore.focusChatTabId && chats.some((c) => c.tabId === loomStore.focusChatTabId)
      ? loomStore.focusChatTabId
      : (listed[0]?.tabId ?? null),
  );
  $effect(() => {
    if (openId && openId !== loomStore.focusChatTabId) loomStore.openChat(openId);
  });
  const open = $derived(chats.find((c) => c.tabId === openId) ?? null);
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

  const openAsk = $derived(openId ? asking(openId) : undefined);
  /** The open chat's waiting scripts, answered in place like its task question. */
  const openScripts = $derived(openId ? scriptsByTab.get(openId) ?? [] : []);
  /** When those cards last changed, for their click guard: one inserted above or answered moves
   *  the rest (ScriptApprovalCard). */
  let scriptsChangedAt = $state(0);
  const openScriptIds = $derived(openScripts.map((a) => a.followUp.id).join('|'));
  $effect(() => {
    void openScriptIds;
    scriptsChangedAt = Date.now();
  });
  const agentTasks = $derived(openId ? tasks.filter((t) => t.tab_id === openId && !isRetired(t.status) && t.status !== 'backlog') : []);
  /** The last answer's outcome, for the chat it was given in. Keyed to the CHAT, not the task:
   *  the answer moves the task out of Blocked before the note arrives, so the question is gone
   *  by then, and the note ("the agent could not be told") is exactly what must still show. */
  let outcome = $state<{ chat: string; text: string } | null>(null);

  // ── What the Fleet card showed ────────────────────────────────────────────────────────────
  // The chat's own meta first: the engine's facts are only polled while Overlord is on.
  const ctx = $derived(openId ? (meta?.contextPct ?? overlordStore.facts.get(openId)?.context_pct ?? null) : null);

  // ── Model and effort, set the way the phone sets them: `/model X` and `/effort X` typed
  //    through the composer's path, so an open prompt refuses them the same way. ──────────────
  const claudeChat = $derived(meta?.runtime === 'claude');
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
  const ritual = $derived(openId ? overlordStore.ritualProgress.find((r) => r.tabId === openId) ?? null : null);
  const awaiting = $derived(openId ? !!overlordStore.outstandingFor(openId) : false);
  /** Rules that can be run on this agent by hand, as the terminal composer offers them: only
   *  with Overlord on. */
  const rules = $derived(openId && preferencesStore.overlordEnabled ? overlordStore.rulesForTab(openId) : []);
  let triggerMenu = $state<{ x: number; y: number; anchor: HTMLElement } | null>(null);
  // The menu lists THIS chat's rules; switching chats closes it rather than retarget it.
  $effect(() => {
    void openId;
    triggerMenu = null;
  });
  let headNote = $state<{ chat: string; text: string } | null>(null);
  function openTrigger(e: MouseEvent) {
    if (triggerMenu) { triggerMenu = null; return; }
    const anchor = e.currentTarget as HTMLElement;
    const r = anchor.getBoundingClientRect();
    // Anchored to the button's top: at the bottom of the chat the menu opens upward.
    triggerMenu = { x: r.left, y: r.top - 4, anchor };
  }
  async function fire(ruleId: string, name: string) {
    const chat = openId;
    if (!chat) return;
    const r = await overlordStore.fireRule(chat, ruleId);
    // Success is a toast that goes away, as in the terminal composer; a refusal stays by the
    // composer until the next try.
    if (r.started) {
      headNote = null;
      toastStore.addToast(name, 'Running on this agent: it types once the agent is idle.', 'success');
    } else {
      headNote = { chat, text: fireRefusal(r.reason, 'that rule') };
    }
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
  $effect(() => {
    if (!active) return;
    let unlisten: (() => void) | undefined;
    let gone = false;
    void getCurrentWebview().onDragDropEvent((ev) => {
      const p = ev.payload;
      const inside = (pos: { x: number; y: number }) => {
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

  // ── Column widths: dragged, and remembered per viewer (a convenience, not a document). ────
  const WIDTH_KEY = 'maiterm.loom.focus.widths';
  const clampW = (w: number, min: number, max: number) => Math.round(Math.min(max, Math.max(min, w)));
  let listW = $state(250);
  let railW = $state(260);
  try {
    const saved = JSON.parse(localStorage.getItem(WIDTH_KEY) ?? 'null');
    if (saved && typeof saved.list === 'number') listW = clampW(saved.list, 170, 480);
    if (saved && typeof saved.rail === 'number') railW = clampW(saved.rail, 180, 520);
  } catch { /* no saved widths */ }
  const saveWidths = () => {
    try { localStorage.setItem(WIDTH_KEY, JSON.stringify({ list: listW, rail: railW })); } catch { /* not persisted */ }
  };

  const EVENT_LABEL ={ added: 'Added a task', asked: 'Asked you', answered: 'Answered', note: 'Note' } as const;

  /** A tool marker's argument: `Bash(git status)` reads as `git status` beside its verb. */
  const toolArg = (text: string) => text.match(/^[^(]+\(([\s\S]*?)\)?$/)?.[1]?.trim() || text;
  const sentence = (s: string) => s.charAt(0).toUpperCase() + s.slice(1);
</script>

<div class="focus" style="--list-w: {listW}px; --rail-w: {railW}px">
  <aside class="list" aria-label="Chats">
    {#snippet section(title: string, list: Chat[], needs = false)}
      {#if list.length}
        <section>
          <h4 class:needs><span>{title}</span><span>{list.length}</span></h4>
          {@render chatRowsFor(list)}
        </section>
      {/if}
    {/snippet}
    {#snippet chatRowsFor(list: Chat[])}
          {#each list as c (c.tabId)}
            {@const pct = overlordStore.facts.get(c.tabId)?.context_pct ?? null}
            <button class="row" aria-pressed={openId === c.tabId} onclick={() => loomStore.openChat(c.tabId)}>
              <i class="dot" data-state={c.state === 'permission' ? 'permission' : c.asks ? 'ask' : (c.state ?? 'none')} class:pulse={c.state === 'active'}></i>
              <span class="nm">{c.name}</span>
              <span class="age">{pct !== null ? `${pct}% · ` : ''}{c.lastActivity ? fmtAge(c.lastActivity) : ''}</span>
              {#if workspaces.length > 1}<span class="ws">{c.workspace}</span>{/if}
              {#if c.preview}<span class="pv" class:ask={c.asks || c.state === 'permission'}>{c.preview}</span>{/if}
            </button>
          {/each}
    {/snippet}
    {@render section('Needs you', sections.needsYou, true)}
    {@render section('Working now', sections.working)}
    <!-- Always drawn, empty or not: its heading is where the window is widened. -->
    <section>
      <h4>
        <button class="window" aria-haspopup="menu" aria-label="How far back to list chats" onclick={openWindowMenu}>{windowLabel} <span aria-hidden="true">▾</span></button>
        <span>{sections.recent.length || ''}</span>
      </h4>
      {@render chatRowsFor(sections.recent)}
    </section>
    {#if !listed.length}<p class="hint">No agent chats in scope right now.</p>{/if}
  </aside>
  <Resizer direction="horizontal" onresize={(d) => (listW = clampW(listW + d, 170, 480))} onresizeend={saveWidths} />

  <div class="convo">
    {#if open}
      <header class="chat-h">
        <div class="who">
          <b>{open.name}</b>
          <span>{open.workspace} · {open.state === 'permission' ? 'waiting on a permission' : open.asks ? 'waiting on you' : (open.state ?? 'no session')}</span>
        </div>
        <div class="gauges">
          {#if meta?.model}
            {#if claudeChat}
              <button class="chip model" aria-label="Switch model" aria-haspopup="menu" onclick={openModelPicker}>{meta.model} <span aria-hidden="true">▾</span></button>
            {:else}
              <span class="chip model">{meta.model}</span>
            {/if}
          {/if}
          {#if meta?.contextLimit && meta.contextLimit >= 1_000_000 && !/\d\s*m\b/i.test(meta.model ?? '')}
            <span class="win">{fmtTok(meta.contextLimit)}</span>
          {/if}
          {#if claudeChat}
            <!-- Unknown is common (a resumed session stops recording effort), so it reads as an
                 offer to set one, never as a value. -->
            <button class="chip effort" class:unset={!effortShown} aria-label={effortShown ? 'Change reasoning effort' : 'Set reasoning effort'} aria-haspopup="menu" onclick={openEffortPicker}>{effortShown ?? 'set effort'}</button>
          {:else if effortShown}
            <span class="chip effort">{effortShown}</span>
          {/if}
          <span class="ctx" style="--t: {ctxTone(ctx)}"><i><s style="width: {Math.min(100, ctx ?? 0)}%"></s></i><em>{ctx ?? '—'}% context</em>{#if meta?.contextUsed != null && meta.contextLimit}<span class="tok">{fmtTok(meta.contextUsed)}/{fmtTok(meta.contextLimit)}</span>{/if}</span>
          {#if ritual}<span class="ritual">{ritual.ruleName} · {ritual.step}/{ritual.steps}</span>{/if}
          {#if awaiting && !ritual}
            <span class="await">awaiting reply
              <button onclick={() => overlordStore.releaseDirective(open.tabId)}>Release</button>
            </span>
          {/if}
        </div>
        <div class="acts">
          <button onclick={() => void navigateToTab(open.tabId)}>Open the tab</button>
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
            <div class="agent" use:chatImages={open.tabId}>{@html renderTurnMarkdown(r.turn.text)}</div>
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

      <div class="dock" bind:this={dockEl}>
        <PromptCard tabId={open.tabId} {active} pulse={liveState?.state} />
        {#if openAsk}
          {@const chat = open.tabId}
          {#key openAsk.id}
            <BlockerCard task={openAsk} variant="card" onnote={(text) => (outcome = { chat, text })} />
          {/key}
        {/if}
        {#each openScripts as a (a.followUp.id)}
          {@const chat = open.tabId}
          <ScriptApprovalCard tabId={chat} followUp={a.followUp} listChangedAt={scriptsChangedAt} onnote={(text) => (outcome = { chat, text })} />
        {/each}
        {#if outcome && outcome.chat === open.tabId}<p class="hint">{outcome.text}</p>{/if}
        <AttachmentChips attachments={attachedHere} onremove={(i) => { unattach(open.tabId, i); composerEl?.focus(); }} />
        <div class="composer" class:drag-over={dragOver}>
          {#if rules.length}
            <!-- The terminal composer's Overlord action: run a rule on this agent by hand. -->
            <IconButton tooltip="Run an Overlord rule on this agent" size={30} onclick={openTrigger} active={!!triggerMenu} aria-label="Run an Overlord rule" aria-haspopup="menu">
              <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
                <path d="M9.2 1.2 3.4 9h4l-1.2 5.8L12.6 7h-4z"/>
              </svg>
            </IconButton>
          {/if}
          <textarea
            bind:this={composerEl}
            rows="2"
            placeholder={`Message ${open.name}… (paste or drop files to attach)`}
            value={draft}
            oninput={(e) => (drafts = { ...drafts, [open.tabId]: (e.currentTarget as HTMLTextAreaElement).value })}
            onkeydown={onComposerKey}
            onpaste={onComposerPaste}
            disabled={sending}
          ></textarea>
          <button class="send" onclick={() => void send()} disabled={sending || (!draft.trim() && !attachedHere.length) || loadedFor !== open.tabId}>{sending ? 'Sending…' : 'Send'}</button>
        </div>
        {#if sendNote && sendNote.chat === open.tabId}<p class="hint">{sendNote.text}</p>{/if}
        {#if headNote && headNote.chat === open.tabId}<p class="hint">{headNote.text}</p>{/if}
      </div>
    {:else}
      <p class="hint pad">Pick a chat on the left.</p>
    {/if}
  </div>

  <div class="rail-resizer">
    <Resizer direction="horizontal" onresize={(d) => (railW = clampW(railW - d, 180, 520))} onresizeend={saveWidths} />
  </div>
  <aside class="rail">
    {#if open}
      <h4>{open.name}'s work</h4>
      {#each agentTasks as t (t.id)}
        <button class="task" onclick={() => loomStore.show('weave', t.id)}>
          <span class="t">{t.title}</span>
          <span class="m">{t.status === 'blocked' && t.blocker ? BLOCKER_LABEL[t.blocker.kind] : t.status} · {fmtAge(t.updated_at)}</span>
        </button>
      {:else}
        <p class="hint">No open tasks on this tab.</p>
      {/each}
      {#each workspaces as w (w.id)}
        {#if w.panes.some((p) => p.tabs.some((x) => x.id === open.tabId))}
          {@const loose = tasksStore.forWorkspace(w.id).filter((t) => !t.tab_id && !isRetired(t.status) && t.status !== 'backlog')}
          {#if loose.length}
            <h4>Unclaimed in {w.name}</h4>
            {#each loose as t (t.id)}
              <button class="task" onclick={() => loomStore.show('weave', t.id)}>
                <span class="t">{t.title}</span><span class="m">{t.status}</span>
              </button>
            {/each}
          {/if}
        {/if}
      {/each}
    {/if}
  </aside>
</div>

{#if windowMenu}
  <ContextMenu
    items={FOCUS_WINDOWS.map((w) => ({ label: w.label, shortcut: w.days === windowDays ? 'current' : undefined, action: () => setWindow(w.days) }))}
    x={windowMenu.x}
    y={windowMenu.y}
    anchor={windowMenu.anchor}
    onclose={() => (windowMenu = null)}
  />
{/if}

{#if pickMenu && openId}
  <ContextMenu items={pickMenu.items} x={pickMenu.x} y={pickMenu.y} anchor={pickMenu.anchor} onclose={() => (pickMenu = null)} />
{/if}

{#if triggerMenu && openId}
  <ContextMenu
    items={rules.map((rule) => ({ label: rule.name, shortcut: rule.enabled ? undefined : 'off', action: () => void fire(rule.id, rule.name) }))}
    x={triggerMenu.x}
    y={triggerMenu.y}
    anchor={triggerMenu.anchor}
    onclose={() => (triggerMenu = null)}
  />
{/if}

<style>
  /* list | drag | chat | drag | rail. The drag handles are the column borders. */
  /* Saved widths are capped by the space there is now, so widths dragged out on a big screen
     can't squeeze the chat in a small window. */
  .focus { position: absolute; inset: 0; display: grid; grid-template-columns: min(var(--list-w), 28%) auto minmax(0, 1fr) auto min(var(--rail-w), 28%); }
  .rail-resizer { display: flex; }
  .list, .rail { overflow-y: auto; min-width: 0; }
  .list { padding: 12px 10px; display: flex; flex-direction: column; gap: 14px; }
  section { display: flex; flex-direction: column; gap: 2px; }
  h4 { margin: 0 4px 4px; display: flex; justify-content: space-between; font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  .window { background: none; border: 0; padding: 0; color: inherit; font: inherit; letter-spacing: inherit; text-transform: inherit; cursor: pointer; }
  .window span { font-size: 8px; }
  .window:hover, .window:focus-visible { color: var(--fg); }
  h4.needs { color: var(--orange, #ff9e64); }
  .row {
    display: grid;
    grid-template-columns: 10px 1fr auto;
    gap: 2px 8px;
    align-items: baseline;
    text-align: left;
    background: none;
    border: 1px solid transparent;
    border-radius: 7px;
    padding: 7px 8px;
    color: var(--fg);
    font: inherit;
    cursor: pointer;
  }
  .row[aria-pressed='true'] { background: var(--bg-medium); border-color: var(--bg-light); }
  .nm { font-weight: 600; font-size: 12.5px; overflow-wrap: anywhere; }
  .age { font-size: 10.5px; color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .ws { grid-column: 2 / -1; font-size: 10.5px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .pv { grid-column: 2 / -1; font-size: 11.5px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .pv.ask { color: var(--orange, #ff9e64); }
  .dot { width: 7px; height: 7px; border-radius: 50%; align-self: center; background: var(--fg-dim); }
  .dot[data-state='active'] { background: var(--green); }
  .dot[data-state='permission'] { background: var(--red); }
  .dot[data-state='ask'] { background: var(--orange, #ff9e64); }
  .pulse { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.35; } }
  @media (prefers-reduced-motion: reduce) { .pulse, .working i { animation: none; } }

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
  .ritual { color: var(--green); }
  .await { display: inline-flex; align-items: center; gap: 6px; color: var(--orange, #ff9e64); }
  .acts { display: flex; gap: 6px; margin-left: auto; }
  .acts button, .await button {
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
  /* The app is unselectable by default (app.css); the chat and the rail are read and quoted. */
  .chat, .rail { -webkit-user-select: text; user-select: text; }
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

  .rail { padding: 14px 16px; display: flex; flex-direction: column; gap: 6px; }
  .rail h4 { margin: 8px 0 2px; }
  .task { display: flex; flex-direction: column; gap: 1px; text-align: left; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 6px; padding: 6px 9px; color: var(--fg); font: inherit; cursor: pointer; }
  .task .t { font-size: 12px; overflow-wrap: anywhere; }
  .task .m { font-size: 10.5px; color: var(--fg-dim); }

  @media (max-width: 1100px) {
    .focus { grid-template-columns: min(var(--list-w), 35%) auto minmax(0, 1fr); }
    .rail, .rail-resizer { display: none; }
  }
</style>
