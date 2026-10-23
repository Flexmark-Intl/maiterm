<!--
  Focus: where the human works with the agents without going into their terminals.

  On the left the chats that need you, the ones working now, and anything unread or active since
  the start of yesterday (the phone's inbox rules). In the middle the chosen chat (ChatColumn),
  condensed the way the phone shows it, then whatever the agent is stopped at, answerable in
  place, and a composer. On the right that agent's work.

  The header carries what the Fleet cards used to: state, context, a running ritual, an
  unanswered directive with Release, and the Trigger menu. The last three are the Loom's own,
  handed to ChatColumn as snippets.
-->
<script lang="ts">
  import { loomStore } from '$lib/stores/loom.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { tabDisplayName } from '$lib/stores/workspaces.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { getTabsLastActivity } from '$lib/tauri/commands';
  import { FOCUS_WINDOWS, focusSections, type FocusChat } from '$lib/loom/model';
  import { BLOCKER_LABEL, isRetired } from '$lib/tasks/model';
  import { fireRefusal, fmtAge } from '$lib/overlord/format';
  import { followUpsStore, type PendingApproval } from '$lib/stores/followUps.svelte';
  import ContextMenu from '$lib/components/ContextMenu.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Resizer from '$lib/components/Resizer.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import ChatColumn, { asking as askingIn, type ChatSource } from './ChatColumn.svelte';
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

  const asking = (tabId: string) => askingIn(tasks, tabId);

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
  /** One ChatColumn for every chat, its source switched: it keeps each chat's draft. */
  const source = $derived<ChatSource | null>(openId ? { kind: 'tab', tabId: openId } : null);
  let column: ChatColumn | undefined;

  const agentTasks = $derived(openId ? tasks.filter((t) => t.tab_id === openId && !isRetired(t.status) && t.status !== 'backlog') : []);

  // ── What the Fleet card showed, the Overlord's part ───────────────────────────────────────
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
      column?.note(chat, null);
      toastStore.addToast(name, 'Running on this agent: it types once the agent is idle.', 'success');
    } else {
      column?.note(chat, fireRefusal(r.reason, 'that rule'));
    }
  }

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
        <span class="recent">Recent
          <button class="window" class:open={!!windowMenu} aria-haspopup="menu" aria-label="How far back to list chats" onclick={openWindowMenu}>{windowLabel} <span aria-hidden="true">▾</span></button>
        </span>
        <span>{sections.recent.length || ''}</span>
      </h4>
      {@render chatRowsFor(sections.recent)}
    </section>
    {#if !listed.length}<p class="hint">No agent chats in scope right now.</p>{/if}
  </aside>
  <Resizer direction="horizontal" onresize={(d) => (listW = clampW(listW + d, 170, 480))} onresizeend={saveWidths} />

  <ChatColumn bind:this={column} {source} name={open?.name ?? ''} workspace={open?.workspace ?? ''} {tasks} {active}>
    {#snippet header(tabId: string)}
      {#if ritual}<span class="ritual">{ritual.ruleName} · {ritual.step}/{ritual.steps}</span>{/if}
      {#if awaiting && !ritual}
        <span class="await">awaiting reply
          <button onclick={() => overlordStore.releaseDirective(tabId)}>Release</button>
        </span>
      {/if}
    {/snippet}
    {#snippet composerActions()}
      {#if rules.length}
        <!-- The terminal composer's Overlord action: run a rule on this agent by hand. -->
        <IconButton tooltip="Run an Overlord rule on this agent" size={30} onclick={openTrigger} active={!!triggerMenu} aria-label="Run an Overlord rule" aria-haspopup="menu">
          <svg width="15" height="15" viewBox="0 0 16 16" fill="currentColor" aria-hidden="true">
            <path d="M9.2 1.2 3.4 9h4l-1.2 5.8L12.6 7h-4z"/>
          </svg>
        </IconButton>
      {/if}
    {/snippet}
  </ChatColumn>

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
     can't squeeze the chat in a small window. The chat cell is ChatColumn's root. */
  .focus { position: absolute; inset: 0; display: grid; grid-template-columns: min(var(--list-w), 28%) auto minmax(0, 1fr) auto min(var(--rail-w), 28%); }
  .rail-resizer { display: flex; }
  .list, .rail { overflow-y: auto; min-width: 0; }
  .list { padding: 12px 10px; display: flex; flex-direction: column; gap: 14px; }
  section { display: flex; flex-direction: column; gap: 2px; }
  h4 { margin: 0 4px 4px; display: flex; justify-content: space-between; font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
  h4 .recent { display: flex; align-items: center; gap: 6px; }
  /* A visible pill, not heading text: as plain heading text nobody found the window picker. */
  .window { background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 10px; padding: 1px 7px; color: var(--accent); font: inherit; font-size: 11px; letter-spacing: normal; text-transform: none; cursor: pointer; }
  .window span { font-size: 8px; }
  .window:hover, .window:focus-visible, .window.open { color: var(--fg); border-color: var(--accent); }
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
  @media (prefers-reduced-motion: reduce) { .pulse { animation: none; } }

  /* The header snippet's pieces, drawn in ChatColumn's gauges (which set their size and dim). */
  .ritual { color: var(--green); }
  .await { display: inline-flex; align-items: center; gap: 6px; color: var(--orange, #ff9e64); }
  /* The same button as ChatColumn's "Open the tab" (its `.acts button`). */
  .await button {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    color: var(--fg);
    border-radius: 6px;
    padding: 4px 10px;
    font: inherit;
    font-size: 11.5px;
    cursor: pointer;
  }

  /* The app is unselectable by default (app.css); the rail is read and quoted, as the chat is. */
  .rail { -webkit-user-select: text; user-select: text; }
  .hint { margin: 0; font-size: 11.5px; color: var(--fg-dim); }

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
