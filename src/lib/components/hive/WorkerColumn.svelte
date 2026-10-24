<!--
  One worker's column in a Hive (docs/hive.md §4.1, §4.2): its own header (badge, title, folders
  and branches, the diff stat and, apart, what's committed, and a menu), then its transcript
  through ChatColumn. A transcript, never the terminal: opening, closing or squeezing this column
  touches no PTY (principle 3). Show terminal adds the raw terminal under the chat (HiveTerminal),
  at the PTY's own fixed size.

  The header sits outside ChatColumn so a waiting worker, which has no tab and so no chat
  header, can still be pinned and closed. A retired worker (landed or retired, §8.4) has no tab
  either: its chat is the sessions Rust recorded for its task, read-only.
-->
<script lang="ts">
  import ChatColumn, { type ChatSource } from '$lib/components/loom/ChatColumn.svelte';
  import HiveTerminal from './HiveTerminal.svelte';
  import ContextMenu, { type MenuItem } from '$lib/components/ContextMenu.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import { formatDiffStat } from '$lib/hive/activity';
  import { isRetired, retiredLine, stateLine, type CardState } from '$lib/hive/cards';
  import type { HiveDiffStat, HiveWorkerCard, Task } from '$lib/tauri/types';

  interface Props {
    /** The Hive's own id: a retired worker's sessions are read under it. */
    hiveId: string;
    card: HiveWorkerCard;
    cardState: CardState;
    diff: HiveDiffStat | undefined;
    workspace: string;
    tasks: Task[];
    /** On screen: a column given way to a narrow window keeps its drafts but stops polling. */
    active: boolean;
    pinned: boolean;
    focused: boolean;
    /** Show terminal is on for this column (it shows only while the worker has a tab). */
    terminalShown: boolean;
    onpin: () => void;
    onclose: () => void;
    onterminal: (shown: boolean) => void;
  }
  let { hiveId, card, cardState, diff, workspace, tasks, active, pinned, focused, terminalShown, onpin, onclose, onterminal }: Props = $props();

  let chat = $state<ChatColumn | undefined>();
  let rootEl = $state<HTMLElement | undefined>();
  /** Opened from a card or a notification: the composer when there's a chat, else the column. */
  export function focusColumn() {
    if (card.tab_id && chat) chat.focusComposer();
    else requestAnimationFrame(() => rootEl?.focus({ preventScroll: true }));
  }

  const retired = $derived(!card.tab_id && isRetired(card));
  const source = $derived<ChatSource | null>(
    card.tab_id ? { kind: 'tab', tabId: card.tab_id } : retired && hiveId ? { kind: 'retired', hiveId, taskId: card.task_id } : null,
  );
  const kept = $derived(card.retired?.kept ?? []);
  const total = $derived(formatDiffStat(diff));
  const committed = $derived(
    diff && (diff.committed.insertions || diff.committed.deletions)
      ? `committed +${diff.committed.insertions} −${diff.committed.deletions}`
      : '',
  );
  const folders = $derived(
    card.folders.map((f) => (f.branch ? `${f.name} · ${f.branch}${f.target ? ` → ${f.target}` : ''}` : `${f.name} (read-only)`)),
  );
  const waitingText = $derived(
    card.phase === 'waiting'
      ? `Waiting on ${card.wait_reason?.trim() || card.depends_on_labels.join(', ') || 'a slot'}: its chat starts when it launches.`
      : 'This worker has no tab right now.',
  );

  let menuBtn = $state<HTMLElement | null>(null);
  let menu = $state<{ x: number; y: number } | null>(null);
  function openMenu() {
    if (menu) {
      menu = null;
      return;
    }
    const r = menuBtn?.getBoundingClientRect();
    if (r) menu = { x: r.left, y: r.bottom + 4 };
  }
  // Land and Stop wait for landing (build step 6). A worker with no tab has no terminal.
  const items = $derived<MenuItem[]>([
    ...(card.tab_id ? [{ label: terminalShown ? 'Hide terminal' : 'Show terminal', action: () => onterminal(!terminalShown) }] : []),
    { label: pinned ? 'Unpin column' : 'Pin column', action: onpin },
    { label: 'Close column', action: onclose },
  ]);
</script>

<!-- The column is a focus stop of its own (Cmd+Shift+[ ]), for a worker with no composer. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<section class="wcol" class:focused bind:this={rootEl} tabindex="-1" aria-label="#{card.badge} {card.title}" data-hive-column={card.task_id}>
  <header class="wh">
    <div class="top">
      <span class="badge">{card.badge}</span>
      <b class="t">{card.title}</b>
      {#if pinned}<span class="pin" aria-label="Pinned"><Icon name="pin" size={10} /></span>{/if}
      <span bind:this={menuBtn}>
        <IconButton tooltip="Column menu" aria-haspopup="menu" onclick={openMenu}><Icon name="more" size={13} /></IconButton>
      </span>
    </div>
    <div class="facts">
      <span class="st" data-state={cardState.state}>{stateLine(cardState)}{#if cardState.sub} · {cardState.sub}{/if}</span>
      {#each folders as f (f)}<span class="m">{f}</span>{/each}
      {#if kept.length}
        <Tooltip text={kept.join('\n')}><span class="m">Kept on disk: {kept.length}</span></Tooltip>
      {/if}
      {#if total.text || committed}
        <span class="diffs">
          {#if total.text}<Tooltip text={total.tip}><span class="diff">{total.text}</span></Tooltip>{/if}
          {#if committed}<Tooltip text="Committed on its branch: what a land would take"><span class="diff">{committed}</span></Tooltip>{/if}
        </span>
      {/if}
    </div>
  </header>
  <ChatColumn
    bind:this={chat}
    {source}
    name={card.role || 'worker'}
    {workspace}
    {tasks}
    {active}
    actions={null}
    readOnly={retired}
    statusText={retired ? retiredLine(card) : undefined}
    emptyText={waitingText}
  />
  {#if terminalShown && card.tab_id}
    <HiveTerminal tabId={card.tab_id} onhide={() => onterminal(false)} />
  {/if}
  {#if menu}
    <ContextMenu {items} x={menu.x} y={menu.y} anchor={menuBtn} onclose={() => (menu = null)} />
  {/if}
</section>

<style>
  .wcol { flex: 1; min-width: 0; min-height: 0; display: flex; flex-direction: column; border-left: 1px solid var(--bg-light); outline: none; transition: box-shadow 0.3s; }
  .wcol.focused { box-shadow: inset 0 2px 0 var(--accent); }
  .wcol > :global(.convo) { flex: 1; }
  .wh { display: flex; flex-direction: column; gap: 3px; padding: 8px 12px 8px 18px; border-bottom: 1px solid var(--bg-light); background: var(--bg-medium); }
  .top { display: flex; align-items: flex-start; gap: 7px; min-width: 0; }
  .t { flex: 1; min-width: 0; font-size: 13px; line-height: 1.35; overflow-wrap: anywhere; }
  .badge { flex: none; font-size: 10px; line-height: 15px; margin-top: 2px; min-width: 15px; padding: 0 4px; border-radius: 8px; text-align: center; background: var(--bg-light); color: var(--fg); font-variant-numeric: tabular-nums; }
  .pin { flex: none; color: var(--fg-dim); margin-top: 3px; }
  .facts { display: flex; flex-wrap: wrap; align-items: baseline; gap: 2px 12px; font-size: 11px; color: var(--fg-dim); min-width: 0; }
  .m { overflow-wrap: anywhere; min-width: 0; }
  .st[data-state='needs_you'] { color: var(--orange, #ff9e64); }
  .st[data-state='blocked'] { color: var(--yellow); }
  .st[data-state='failed'] { color: var(--red); }
  .diffs { display: inline-flex; flex-wrap: wrap; gap: 2px 10px; }
  .diff { font-variant-numeric: tabular-nums; white-space: nowrap; }
  @media (prefers-reduced-motion: reduce) {
    .wcol { transition: none; }
  }
</style>
