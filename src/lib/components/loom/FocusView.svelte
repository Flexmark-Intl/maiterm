<!--
  Focus, the maiLink way: on the left the chats that need you, the ones working now, and
  anything unread or active since the start of yesterday; in the middle the chosen chat,
  condensed the way the phone shows it (tool runs folded, events as thin rules, task events
  placed where they happened); on the right that agent's work.
-->
<script lang="ts">
  import { loomStore } from '$lib/stores/loom.svelte';
  import { claudeStateStore } from '$lib/stores/agentState.svelte';
  import { tabDisplayName, navigateToTab } from '$lib/stores/workspaces.svelte';
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { getTabTranscript, type ChatTurn } from '$lib/tauri/commands';
  import { chatRows, focusSections, taskEventsFor, type FocusChat } from '$lib/loom/model';
  import { renderTurnMarkdown } from '$lib/loom/markdown';
  import { BLOCKER_LABEL, isRetired } from '$lib/tasks/model';
  import { fmtAge } from '$lib/overlord/format';
  import BlockerCard from '$lib/components/tasks/BlockerCard.svelte';
  import type { Task, Workspace } from '$lib/tauri/types';

  interface Props {
    workspaces: Workspace[];
    tasks: Task[];
    now: number;
  }
  let { workspaces, tasks, now }: Props = $props();

  interface Chat extends FocusChat { name: string; workspace: string; preview: string }

  const asking = (tabId: string) =>
    tasks.find((t) => t.tab_id === tabId && t.status === 'blocked' && (t.blocker?.kind === 'decision' || t.blocker?.kind === 'action'));

  const chats = $derived.by<Chat[]>(() =>
    workspaces.flatMap((w) =>
      w.panes.flatMap((p) =>
        p.tabs
          .filter((t) => t.tab_type === 'terminal' && !t.service_id)
          .map((t) => {
            const s = claudeStateStore.getState(t.id);
            const ask = asking(t.id);
            return {
              tabId: t.id,
              name: tabDisplayName(t.id),
              workspace: w.name,
              state: s?.state ?? null,
              unread: s?.state === 'idle' && s.read === false,
              lastActivity: s?.updatedAt ?? 0,
              asks: !!ask,
              preview: ask?.blocker?.question ?? (s?.state === 'active' ? (s.toolDetail ?? s.toolName ?? 'Working…') : s?.state === 'permission' ? 'Needs your approval' : ''),
            };
          })
          .filter((c) => c.state !== null || c.asks),
      ),
    ),
  );
  const sections = $derived(focusSections(chats, now));
  const listed = $derived([...sections.needsYou, ...sections.working, ...sections.recent]);

  /** The open chat: the one picked, else the first that needs you. */
  const openId = $derived(
    loomStore.focusChatTabId && chats.some((c) => c.tabId === loomStore.focusChatTabId)
      ? loomStore.focusChatTabId
      : (listed[0]?.tabId ?? null),
  );
  const open = $derived(chats.find((c) => c.tabId === openId) ?? null);

  // ── The transcript: read on open, then every 3 s while shown (the phone polls 2 s). ──
  let turns = $state<ChatTurn[]>([]);
  let loadedFor = $state<string | null>(null);
  $effect(() => {
    const id = openId;
    turns = [];
    loadedFor = null;
    if (!id) return;
    let alive = true;
    const load = () =>
      getTabTranscript(id)
        .then((t) => { if (alive) { turns = t; loadedFor = id; } })
        .catch(() => { if (alive) loadedFor = id; });
    void load();
    const timer = setInterval(load, 3000);
    return () => { alive = false; clearInterval(timer); };
  });

  const rows = $derived(openId ? chatRows(turns, taskEventsFor(openId, tasks)) : []);
  let expanded = $state<string[]>([]);
  const toggle = (key: string) => (expanded = expanded.includes(key) ? expanded.filter((k) => k !== key) : [...expanded, key]);

  const openAsk = $derived(openId ? asking(openId) : undefined);
  const agentTasks = $derived(openId ? tasks.filter((t) => t.tab_id === openId && !isRetired(t.status) && t.status !== 'backlog') : []);
  let outcome = $state('');

  // Keep the newest turn in view as the chat grows, unless the human scrolled up to read.
  let chatEl = $state<HTMLElement | null>(null);
  let pinned = true;
  $effect(() => {
    void rows.length;
    if (chatEl && pinned) requestAnimationFrame(() => chatEl && (chatEl.scrollTop = chatEl.scrollHeight));
  });
  const onChatScroll = () => { if (chatEl) pinned = chatEl.scrollHeight - chatEl.scrollTop - chatEl.clientHeight < 40; };

  const EVENT_LABEL = { added: 'Task added', asked: 'Asked you', answered: 'Answered', note: 'Task note' } as const;
</script>

<div class="focus">
  <aside class="list" aria-label="Chats">
    {#snippet section(title: string, list: Chat[], needs = false)}
      {#if list.length}
        <section>
          <h4 class:needs><span>{title}</span><span>{list.length}</span></h4>
          {#each list as c (c.tabId)}
            <button class="row" aria-pressed={openId === c.tabId} onclick={() => loomStore.openChat(c.tabId)}>
              <i class="dot" data-state={c.asks ? 'ask' : (c.state ?? 'none')} class:pulse={c.state === 'active'}></i>
              <span class="nm">{c.name}</span>
              <span class="age">{c.lastActivity ? fmtAge(c.lastActivity) : ''}</span>
              {#if c.preview}<span class="pv" class:ask={c.asks}>{c.preview}</span>{/if}
            </button>
          {/each}
        </section>
      {/if}
    {/snippet}
    {@render section('Needs you', sections.needsYou, true)}
    {@render section('Working now', sections.working)}
    {@render section('Since yesterday', sections.recent)}
    {#if !listed.length}<p class="hint">No agent chats in scope right now.</p>{/if}
  </aside>

  <div class="chat" bind:this={chatEl} onscroll={onChatScroll}>
    {#if open}
      <div class="chat-h">
        <b>{open.name}</b>
        <span>{open.workspace} · {open.asks ? 'waiting on you' : (open.state ?? 'no session')}</span>
        <button class="link" onclick={() => { loomStore.close(); void navigateToTab(open.tabId); }}>Open the tab</button>
      </div>
      {#if loadedFor !== openId}
        <p class="hint">Reading the chat…</p>
      {:else if !rows.length}
        <p class="hint">No messages captured for this tab yet.</p>
      {/if}
      {#each rows as r (r.kind === 'tools' ? r.key : r.kind === 'task' ? r.key : r.turn.msg_id)}
        {#if r.kind === 'tools'}
          {#if r.turns.length === 1}
            <span class="tool">{r.turns[0].text}</span>
          {:else}
            <div class="run">
              <button aria-expanded={expanded.includes(r.key)} onclick={() => toggle(r.key)}>
                <span class="verbs">{r.verbs.join(' · ')}</span>
                <span class="n">{r.turns.length} {expanded.includes(r.key) ? '▴' : '▾'}</span>
              </button>
              {#if expanded.includes(r.key)}
                <div class="calls">{#each r.turns as t (t.msg_id)}<span class="tool">{t.text}</span>{/each}</div>
              {/if}
            </div>
          {/if}
        {:else if r.kind === 'task'}
          <button class="rule" data-kind={r.event.kind} onclick={() => { loomStore.select(r.event.taskId); loomStore.setView('loom'); }}>
            <span><b>{EVENT_LABEL[r.event.kind]}</b><em>{r.event.kind === 'added' ? r.event.text : `${r.event.title}: ${r.event.text}`}</em></span>
          </button>
        {:else if r.kind === 'rule'}
          <div class="rule" data-kind="peer">
            <span><b>{r.turn.kind === 'goal_status' ? `Goal ${r.turn.goal?.event ?? ''}` : r.turn.peer?.direction === 'in' ? `From ${r.turn.peer?.name ?? 'a peer'}` : `To ${r.turn.peer?.name ?? 'a peer'}`}</b><em>{r.turn.text.split('\n')[0]}</em></span>
          </div>
        {:else if r.turn.kind === 'terminal_snapshot'}
          <pre class="snap">{r.turn.text}</pre>
        {:else if r.turn.role === 'user'}
          <div class="you">{r.turn.text}</div>
        {:else if r.turn.role === 'agent'}
          <div class="agent">{@html renderTurnMarkdown(r.turn.text)}</div>
        {:else}
          <div class="sys">{r.turn.text}</div>
        {/if}
      {/each}
      {#if openAsk}
        {@const forId = openAsk.id}
        <div class="ask">
          {#key openAsk.id}
            <BlockerCard task={openAsk} variant="card" onnote={(text) => { if (openAsk?.id === forId) outcome = text; }} />
          {/key}
        </div>
      {/if}
      {#if outcome}<p class="hint">{outcome}</p>{/if}
    {:else}
      <p class="hint">Pick a chat on the left.</p>
    {/if}
  </div>

  <aside class="rail">
    {#if open}
      <h4>{open.name}'s work</h4>
      {#each agentTasks as t (t.id)}
        <button class="task" onclick={() => { loomStore.select(t.id); loomStore.setView('loom'); }}>
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
              <button class="task" onclick={() => { loomStore.select(t.id); loomStore.setView('loom'); }}>
                <span class="t">{t.title}</span><span class="m">{t.status}</span>
              </button>
            {/each}
          {/if}
        {/if}
      {/each}
    {/if}
  </aside>
</div>

<style>
  .focus { position: absolute; inset: 0; display: grid; grid-template-columns: 240px minmax(0, 560px) minmax(0, 1fr); }
  .list, .chat, .rail { overflow-y: auto; min-width: 0; }
  .list { border-right: 1px solid var(--bg-light); padding: 12px 10px; display: flex; flex-direction: column; gap: 14px; }
  section { display: flex; flex-direction: column; gap: 2px; }
  h4 { margin: 0 4px 4px; display: flex; justify-content: space-between; font-size: 10.5px; font-weight: 500; letter-spacing: 0.08em; text-transform: uppercase; color: var(--fg-dim); }
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
  .age { font-size: 10.5px; color: var(--fg-dim); }
  .pv { grid-column: 2 / -1; font-size: 11.5px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .pv.ask { color: var(--orange, #ff9e64); }
  .dot { width: 7px; height: 7px; border-radius: 50%; align-self: center; background: var(--fg-dim); }
  .dot[data-state='active'] { background: var(--green); }
  .dot[data-state='permission'] { background: var(--red); }
  .dot[data-state='ask'] { background: var(--orange, #ff9e64); }
  .pulse { animation: pulse 1.6s ease-in-out infinite; }
  @keyframes pulse { 50% { opacity: 0.35; } }

  .chat { border-right: 1px solid var(--bg-light); padding: 12px 18px 18px; display: flex; flex-direction: column; gap: 9px; }
  .chat-h { display: flex; flex-wrap: wrap; align-items: baseline; gap: 4px 10px; padding-bottom: 8px; border-bottom: 1px solid var(--bg-light); }
  .chat-h b { font-size: 14px; }
  .chat-h span { font-size: 11px; color: var(--fg-dim); }
  .link { margin-left: auto; background: none; border: 0; color: var(--accent); font: inherit; font-size: 11.5px; cursor: pointer; }
  .you { align-self: flex-end; max-width: 85%; background: var(--bg-light); border-radius: 14px 14px 4px 14px; padding: 8px 12px; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 12.5px; }
  .agent { font-size: 12.5px; line-height: 1.5; overflow-wrap: anywhere; }
  .agent :global(p) { margin: 0 0 6px; }
  .agent :global(pre) { background: var(--bg-medium); padding: 8px; border-radius: 6px; overflow-x: auto; }
  .agent :global(code) { font-family: var(--font-mono, ui-monospace, monospace); font-size: 11.5px; }
  .agent :global(a) { color: var(--accent); }
  .sys { font-size: 11.5px; color: var(--fg-dim); overflow-wrap: anywhere; }
  .snap { margin: 0; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; white-space: pre-wrap; overflow-wrap: anywhere; background: var(--bg-medium); padding: 8px; border-radius: 6px; }
  .tool { align-self: flex-start; font-family: var(--font-mono, ui-monospace, monospace); font-size: 11px; color: var(--fg-dim); border: 1px solid var(--bg-light); border-radius: 5px; padding: 2px 7px; background: var(--bg-medium); overflow-wrap: anywhere; }
  .run > button { display: flex; gap: 8px; width: 100%; background: none; border: 0; padding: 2px 0; color: var(--fg-dim); font: inherit; cursor: pointer; text-align: left; }
  .run > button:hover { color: var(--fg); }
  .verbs { flex: 1; min-width: 0; font-family: var(--font-mono, ui-monospace, monospace); font-size: 12px; overflow-wrap: anywhere; }
  .n { font-size: 10.5px; opacity: 0.7; }
  .calls { display: flex; flex-direction: column; gap: 3px; border-left: 1px solid var(--bg-light); padding-left: 10px; margin: 3px 0 0 9px; }
  .rule {
    --c: var(--fg-dim);
    display: flex;
    align-items: center;
    gap: 10px;
    width: 100%;
    background: none;
    border: 0;
    padding: 3px 0;
    color: var(--fg-dim);
    font: inherit;
    cursor: pointer;
  }
  .rule[data-kind='asked'] { --c: var(--orange, #ff9e64); }
  .rule[data-kind='answered'] { --c: var(--green); }
  .rule[data-kind='added'] { --c: var(--cyan); }
  .rule[data-kind='peer'] { --c: var(--accent); cursor: default; }
  .rule::before, .rule::after { content: ''; flex: 1; min-width: 16px; height: 1px; background: var(--bg-light); }
  .rule span { display: inline-flex; gap: 6px; max-width: 80%; font-size: 10.5px; letter-spacing: 0.06em; text-transform: uppercase; }
  .rule b { color: var(--c); font-weight: 500; flex: none; }
  .rule em { font-style: normal; text-transform: none; letter-spacing: 0; color: var(--fg); opacity: 0.8; overflow-wrap: anywhere; }
  .ask { margin-top: 4px; }
  .hint { margin: 0; font-size: 11.5px; color: var(--fg-dim); }

  .rail { padding: 14px 16px; display: flex; flex-direction: column; gap: 6px; }
  .rail h4 { margin: 8px 0 2px; }
  .task { display: flex; flex-direction: column; gap: 1px; text-align: left; background: var(--bg-medium); border: 1px solid var(--bg-light); border-radius: 6px; padding: 6px 9px; color: var(--fg); font: inherit; cursor: pointer; }
  .task .t { font-size: 12px; overflow-wrap: anywhere; }
  .task .m { font-size: 10.5px; color: var(--fg-dim); }

  @media (max-width: 1000px) {
    .focus { grid-template-columns: 200px minmax(0, 1fr); }
    .rail { display: none; }
  }
</style>
