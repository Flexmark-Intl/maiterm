<script lang="ts">
  /** Per-tab task panel (docs/tasks.md §6).
   *
   *  Deliberately built on the notes-panel pattern — same dock, same drag-resize, same
   *  per-tab persistence — so the muscle memory transfers and there is one mental model
   *  for "side panel" in maiTerm.
   *
   *  THIS TAB'S WORK ONLY. It answers one question — "what am I doing here" — and hands
   *  every other question to the board.
   *
   *  It used to also list the rest of the project, offer a workstream picker for new tasks,
   *  a per-row workstream dropdown, and an assign/unassign control. All of that is
   *  ORGANIZING work, and organizing now has a proper home: the board is indexed by
   *  workstream (docs/overlord.md), where a drag moves a task between jobs and the whole
   *  window is visible at once. Duplicating it in a 280px dock made the panel a worse
   *  version of the board and buried the one list the tab actually needs. */
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { overlordStore } from '$lib/stores/overlord.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { effectiveStatus, isInFlight, isParked, TASK_STATUSES } from '$lib/tasks/model';
  import type { Task, TaskStatus } from '$lib/tauri/types';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import TaskAddModal from './TaskAddModal.svelte';

  interface Props {
    tabId: string;
    workspaceId: string;
    onclose: () => void;
  }

  let { tabId, workspaceId, onclose }: Props = $props();

  let draft = $state('');
  let editingId = $state<string | null>(null);
  let editValue = $state('');
  let detailFor = $state<string | null>(null);
  let detailValue = $state('');
  let confirmingDelete = $state<string | null>(null);
  /** The group whose add button was pressed, or null when the modal is closed. Holding the
   *  workstream id here is what lets the modal ask nothing about destination. */
  let addingTo = $state<{ workstreamId: string | null; name: string | null } | null>(null);
  let showDone = $state(false);
  let showParked = $state(false);
  let showUnclaimed = $state(false);

  /** The whole workspace list — needed ONLY as the dependency universe for
   *  `effectiveStatus`, since a prerequisite can live on another tab. Never rendered. */
  const all = $derived(tasksStore.forWorkspace(workspaceId));
  const mine = $derived(all.filter((t) => t.tab_id === tabId));

  /** Finished and parked work both collapse behind a count — the panel is for what's in
   *  flight. Parked is separate from done because they mean different things: one is
   *  finished, the other is deliberately not started. */
  const visible = (list: Task[]) =>
    list.filter((t) => (showDone || t.status !== 'done') && (showParked || !isParked(t.status)));
  const doneCount = $derived(mine.filter((t) => t.status === 'done').length);
  const parkedCount = $derived(mine.filter((t) => isParked(t.status)).length);

  /** Work nobody owns: released when a tab closed (`releaseTab`), or created unassigned.
   *
   *  This IS the per-tab panel's business — it is the pile you can pick up here, not
   *  another tab's work. Surfacing it is also the only way to reach it: nothing else in the
   *  app writes `Task.tab_id`, so without a claim control released rows are unreadable,
   *  uneditable and undeletable from every surface, forever. */
  const unclaimed = $derived(all.filter((t) => !t.tab_id && isInFlight(t)));

  /** Work in flight on OTHER tabs. One line, not a list — enough to say the board has more,
   *  never enough to become a second board. Counts only in-flight and only genuinely
   *  assigned rows: a raw `all - mine` grew monotonically with every task the project ever
   *  finished, so it shouted loudest exactly when nothing was happening. */
  const elsewhere = $derived(all.filter((t) => t.tab_id && t.tab_id !== tabId && isInFlight(t)).length);

  /** This tab's work, grouped by job. One tab routinely runs two unrelated jobs, which is
   *  the whole reason workstreams exist — without grouping they blur into one list. */
  interface Group {
    key: string;
    name: string | null;
    list: Task[];
    unclaimed?: boolean;
  }

  const groups = $derived.by<Group[]>(() => {
    const streams = tasksStore.workstreams(workspaceId);
    const nameOf = (id: string) => streams.find((w) => w.id === id)?.name ?? null;
    const byStream = new Map<string, Task[]>();
    for (const t of mine) {
      const k = t.workstream_id ?? '';
      if (!byStream.has(k)) byStream.set(k, []);
      byStream.get(k)!.push(t);
    }
    const out: Group[] = [...byStream.keys()]
      // Loose tasks last; named jobs alphabetical.
      .sort((a, b) => (!a ? 1 : !b ? -1 : (nameOf(a) ?? '').localeCompare(nameOf(b) ?? '')))
      .map((k) => ({ key: k, name: k ? nameOf(k) : null, list: visible(byStream.get(k)!) }))
      .filter((g) => g.list.length);
    if (showUnclaimed && unclaimed.length) {
      out.push({ key: '__unclaimed', name: null, list: unclaimed, unclaimed: true });
    }
    return out;
  });

  /** Headings only earn their space when there is more than one job in view — but the
   *  unclaimed pile always needs its label, or it reads as this tab's own work. */
  const showGroupHeadings = $derived(groups.length > 1 || groups.some((g) => g.unclaimed));

  const STATUS_LABEL: Record<TaskStatus, string> = {
    backlog: 'Parked',
    todo: 'To-do',
    active: 'Active',
    blocked: 'Blocked',
    review: 'Review',
    done: 'Done',
  };

  /** Where a new task lands, with no picker to answer.
   *
   *  If everything this tab is working on belongs to ONE job, a task typed here obviously
   *  belongs to it too. If the tab is juggling two, guessing would be wrong, so it goes
   *  loose and the board is where it gets filed. That covers both cases without asking a
   *  question the panel used to ask on every single add. */
  function addTask() {
    const title = draft.trim();
    if (!title) return;
    // `isInFlight`, not `status !== 'done'`: backlog is the parking lot and is exempt from
    // every other in-flight question in the codebase. Counting it here meant a tab whose
    // only rows were parked (and therefore hidden) silently filed a new bug into a shelved
    // workstream, with no heading shown to reveal it.
    const streams = new Set(mine.filter(isInFlight).map((t) => t.workstream_id ?? ''));
    const only = streams.size === 1 ? [...streams][0] : '';
    tasksStore.add(workspaceId, {
      title,
      tab_id: tabId,
      origin: 'human',
      workstream_id: only || null,
    });
    draft = '';
  }

  /** Click the status chip to advance; shift-click to go back. Cycling beats a dropdown
   *  here — status changes are the panel's most frequent action by far.
   *
   *  Steps from the status the chip DISPLAYS, not the stored one. On a task blocked by an
   *  unfinished dependency those differ, and stepping from the stored value made the chip
   *  look frozen: three clicks would silently walk the stored status through the whole
   *  vocabulary while the label stayed "BLOCKED", then jump to "DONE" on the fourth. */
  function cycleStatus(t: Task, back: boolean) {
    const shown = effectiveStatus(t, all, workspacesStore.parkedTaskIds);
    const i = TASK_STATUSES.indexOf(shown);
    const next = TASK_STATUSES[(i + (back ? -1 : 1) + TASK_STATUSES.length) % TASK_STATUSES.length];
    tasksStore.setStatus(workspaceId, t.id, next);
  }

  function startEdit(t: Task) {
    editingId = t.id;
    editValue = t.title;
  }

  /** Edits save as you type, not only on blur.
   *
   *  Closing the panel destroys it with the field still focused, and a browser fires no
   *  blur event when the focused element is removed from the document — so a blur-only
   *  commit loses whatever was typed. Cmd+Shift+E is exactly that path: it toggles
   *  visibility without moving focus first. NotesPanel debounces for the same reason. */
  const SAVE_DEBOUNCE_MS = 400;
  let saveTimer: ReturnType<typeof setTimeout> | null = null;

  function queueSave(fn: () => void) {
    if (saveTimer) clearTimeout(saveTimer);
    saveTimer = setTimeout(fn, SAVE_DEBOUNCE_MS);
  }

  function saveTitle(id: string, title: string) {
    if (title.trim()) tasksStore.update(workspaceId, id, { title: title.trim() });
  }

  function saveDetail(id: string, detail: string) {
    const current = all.find((t) => t.id === id);
    const next = detail.trim();
    if (current && (current.detail ?? '') !== next) {
      tasksStore.update(workspaceId, id, { detail: next || null });
    }
  }

  function commitEdit() {
    const id = editingId;
    if (!id) return;
    if (saveTimer) clearTimeout(saveTimer);
    saveTitle(id, editValue);
    editingId = null;
  }

  function toggleDetail(t: Task) {
    if (detailFor === t.id) {
      commitDetail();
      return;
    }
    detailFor = t.id;
    detailValue = t.detail ?? '';
  }

  function commitDetail() {
    const id = detailFor;
    if (!id) return;
    if (saveTimer) clearTimeout(saveTimer);
    saveDetail(id, detailValue);
    detailFor = null;
  }

  /** Take ownership of unclaimed work, or hand this tab's work back to the project. The
   *  only `Task.tab_id` writers in the UI — the board reassigns workstream and status, never
   *  the assignee. */
  /** Add into the workstream whose button was pressed. Unlike the inline field, nothing is
   *  inferred here — the destination came from the press, and the lane came from the modal. */
  function addToGroup(v: { title: string; detail: string | null; status: TaskStatus }) {
    if (!addingTo) return;
    tasksStore.add(workspaceId, {
      title: v.title,
      detail: v.detail,
      status: v.status,
      tab_id: tabId,
      origin: 'human',
      workstream_id: addingTo.workstreamId,
    });
    // A row you just created must not vanish. Parked rows are hidden behind a toggle by
    // default, so adding straight into Parked would otherwise look like the add silently
    // failed — the panel would be unchanged except for a count that is also hidden.
    if (isParked(v.status)) showParked = true;
    addingTo = null;
  }

  function setAssignee(t: Task, mineNow: boolean) {
    tasksStore.update(workspaceId, t.id, { tab_id: mineNow ? tabId : null });
  }

  /** Through the engine, like the board's delete — a task the human removes has to reach
   *  whoever was carrying it, or the agent puts it back on its next list re-send. With
   *  Overlord disabled this is exactly the old silent remove; maiTerm does not type into
   *  a terminal on behalf of a supervisor that has been turned off. */
  function remove(id: string) {
    void overlordStore.deleteTask(id);
    confirmingDelete = null;
  }

  // ── Drag-resize from the left edge (NotesPanel.svelte:198) ──
  let dragging = $state(false);
  let dragStartX = 0;
  let dragStartWidth = 0;
  let panelEl = $state<HTMLElement | null>(null);

  function handleResizePointerDown(e: PointerEvent) {
    e.preventDefault();
    dragging = true;
    dragStartX = e.clientX;
    dragStartWidth = preferencesStore.tasksWidth;
    (e.target as HTMLElement).setPointerCapture(e.pointerId);
  }

  function handleResizePointerMove(e: PointerEvent) {
    if (!dragging) return;
    const delta = dragStartX - e.clientX;
    const paneWidth = panelEl?.parentElement?.clientWidth ?? window.innerWidth;
    const maxWidth = Math.floor(paneWidth * 0.9);
    preferencesStore.setTasksWidth(Math.max(200, Math.min(maxWidth, dragStartWidth + delta)));
  }

  function handleResizePointerUp() {
    dragging = false;
  }
</script>

<div
  class="tasks-panel"
  bind:this={panelEl}
  style:width="{preferencesStore.tasksWidth}px"
  style:min-width="{preferencesStore.tasksWidth}px"
>
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="resize-handle"
    onpointerdown={handleResizePointerDown}
    onpointermove={handleResizePointerMove}
    onpointerup={handleResizePointerUp}
  ></div>

  <div class="panel-header">
    <span class="title">Tasks</span>
    <span class="spacer"></span>
    {#if unclaimed.length > 0}
      <button class="done-toggle" class:on={showUnclaimed} onclick={() => (showUnclaimed = !showUnclaimed)}>
        {unclaimed.length} unclaimed
      </button>
    {/if}
    {#if parkedCount > 0}
      <button class="done-toggle" class:on={showParked} onclick={() => (showParked = !showParked)}>
        {parkedCount} parked
      </button>
    {/if}
    {#if doneCount > 0}
      <button class="done-toggle" class:on={showDone} onclick={() => (showDone = !showDone)}>
        {doneCount} done
      </button>
    {/if}
    <IconButton tooltip="Close tasks" onclick={onclose}>
      <Icon name="close" size={13} />
    </IconButton>
  </div>

  <div class="add-row">
    <input
      class="add-input"
      placeholder="Add a task…"
      bind:value={draft}
      onkeydown={(e) => {
        if (e.key === 'Enter') addTask();
      }}
    />
    <button class="add-btn" disabled={!draft.trim()} onclick={addTask} aria-label="Add task">+</button>
  </div>

  <div class="lists">
    {#if groups.length === 0}
      <p class="empty">
        {#if mine.length}
          Nothing in flight on this tab — the counts above hold the rest.
        {:else}
          Nothing tracked on this tab yet. Add a task above — the agent here reads and
          updates the same list.
        {/if}
      </p>
    {/if}

    {#each groups as group (group.key)}
      <!-- A wrapper per job, so the separation can live BETWEEN groups. Heading and list
           used to be loose siblings, which left nothing to hang a rule on. -->
      <section class="ws-group">
      {#if showGroupHeadings}
        <h4 class="group">
          <span class="group-name">
            {#if group.unclaimed}<span class="group-loose">Unclaimed — nobody is on these</span>
            {:else if group.name}{group.name}
            {:else}<span class="group-loose">Ungrouped</span>{/if}
          </span>
          <span class="group-count">{group.list.length}</span>
          <!-- Not offered on the unclaimed pile. That group is an ASSIGNMENT bucket, not a
               workstream — its rows come from every job at once — so there is no destination
               a press of this button could mean. The inline field above still adds there. -->
          {#if !group.unclaimed}
            <Tooltip text="Add a task to {group.name ?? 'no workstream'}">
              <button
                class="group-add"
                aria-label="Add a task to {group.name ?? 'no workstream'}"
                onclick={() => (addingTo = { workstreamId: group.key || null, name: group.name })}
              >
                <Icon name="plus" size={11} />
              </button>
            </Tooltip>
          {/if}
        </h4>
      {/if}
      <ul class="task-list">
          {#each group.list as t (t.id)}
            {@const eff = effectiveStatus(t, all, workspacesStore.parkedTaskIds)}
            <li class="task" class:done={t.status === 'done'} class:parked={isParked(t.status)}>
              <div class="task-main">
                <Tooltip text="{STATUS_LABEL[eff]} — click to advance, shift-click to go back">
                  <button
                    class="status s-{eff}"
                    onclick={(e) => cycleStatus(t, e.shiftKey)}
                  >
                    {STATUS_LABEL[eff]}
                  </button>
                </Tooltip>

                {#if editingId === t.id}
                  <!-- svelte-ignore a11y_autofocus -->
                  <input
                    class="edit-input"
                    autofocus
                    bind:value={editValue}
                    oninput={() => editingId && queueSave(() => saveTitle(editingId!, editValue))}
                    onblur={commitEdit}
                    onkeydown={(e) => {
                      if (e.key === 'Enter') commitEdit();
                      if (e.key === 'Escape') editingId = null;
                    }}
                  />
                {:else}
                  <button class="task-title" ondblclick={() => startEdit(t)} onclick={() => toggleDetail(t)}>
                    {t.title}
                  </button>
                {/if}

                <span class="row-ctl">
                  {#if t.origin !== 'human'}
                    <Tooltip text="Recorded by {t.origin === 'imported' ? "an import from the agent's own list" : t.origin}">
                      <span class="origin">
                        {t.origin === 'imported' ? '⇥' : '◆'}
                      </span>
                    </Tooltip>
                  {/if}
                  <Tooltip text={group.unclaimed ? 'Claim for this tab' : 'Hand back — leave for whoever picks it up'}>
                    <button
                      class="mini"
                      onclick={() => setAssignee(t, !!group.unclaimed)}
                    >
                      {group.unclaimed ? '↧' : '↥'}
                    </button>
                  </Tooltip>
                  {#if confirmingDelete === t.id}
                    <Tooltip text="Confirm delete">
                      <button class="mini danger" onclick={() => remove(t.id)}>✓</button>
                    </Tooltip>
                    <Tooltip text="Cancel">
                      <button class="mini" onclick={() => (confirmingDelete = null)}>✕</button>
                    </Tooltip>
                  {:else}
                    <Tooltip text="Delete">
                      <button class="mini" onclick={() => (confirmingDelete = t.id)}>
                        <Icon name="trash" size={11} />
                      </button>
                    </Tooltip>
                  {/if}
                </span>
              </div>

              {#if t.blocked_by?.length}
                <div class="deps">
                  waiting on {t.blocked_by
                    .map((id) => all.find((x) => x.id === id)?.title)
                    .filter(Boolean)
                    .join(', ') || 'a task that no longer exists'}
                </div>
              {/if}

              {#if detailFor === t.id}
                <textarea
                  class="detail"
                  placeholder="Notes, acceptance criteria, links…"
                  bind:value={detailValue}
                  oninput={() => detailFor && queueSave(() => saveDetail(detailFor!, detailValue))}
                  onblur={commitDetail}
                ></textarea>
              {:else if t.detail}
                <button class="detail-preview" onclick={() => toggleDetail(t)}>{t.detail}</button>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {/each}

    {#if elsewhere > 0}
      <!-- A pointer, never a list. The moment this shows other tabs' work it stops being a
           per-tab panel and starts being a cramped second board. -->
      <p class="elsewhere">
        {elsewhere} task{elsewhere === 1 ? '' : 's'} in flight on other tabs{#if preferencesStore.overlordEnabled} — see the board{/if}.
      </p>
    {/if}
  </div>
</div>

{#if addingTo}
  <TaskAddModal
    workstreamName={addingTo.name}
    onsubmit={addToGroup}
    oncancel={() => (addingTo = null)}
  />
{/if}

<style>
  .tasks-panel {
    display: flex;
    flex-direction: column;
    background: var(--bg-medium);
    border-left: 1px solid var(--bg-light);
    position: relative;
    overflow: hidden;
  }

  .resize-handle {
    position: absolute;
    top: 0;
    left: -3px;
    width: 6px;
    height: 100%;
    cursor: col-resize;
    z-index: 10;
  }
  .resize-handle:hover,
  .resize-handle:active {
    background: var(--accent);
    opacity: 0.3;
  }

  .panel-header {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--bg-light);
    flex-shrink: 0;
  }
  .title {
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--fg-dim);
  }
  .spacer { flex: 1; }

  .done-toggle {
    background: none;
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    color: var(--fg-dim);
    font-size: 10px;
    padding: 1px 7px;
    cursor: pointer;
  }
  .done-toggle.on {
    color: var(--accent);
    border-color: var(--accent);
  }

  .add-row {
    display: flex;
    gap: 4px;
    padding: 6px 8px;
    border-bottom: 1px solid var(--bg-light);
    flex-shrink: 0;
  }
  .add-input {
    flex: 1;
    min-width: 0;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    font-size: 12px;
    padding: 4px 6px;
  }
  .add-input:focus {
    outline: none;
    border-color: var(--accent);
  }
  .add-btn {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
    padding: 0 8px;
  }
  .add-btn:disabled {
    opacity: 0.35;
    cursor: default;
  }

  .lists {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0 12px;
  }

  .empty {
    color: var(--fg-dim);
    font-size: 11px;
    line-height: 1.5;
    margin: 10px 10px 0;
  }

  .group-loose { font-style: italic; opacity: 0.7; text-transform: none; letter-spacing: 0; }

  /* A workstream is a different JOB, and the separation has to outweigh the hairline
     between two rows of the SAME job. It didn't: the heading was 10px dim text with no rule,
     sitting among rows that each carry a border, so it read as one more row and two jobs
     blurred into one list — the exact thing workstreams exist to prevent. */
  .ws-group + .ws-group {
    border-top: 1px solid var(--bg-light);
    margin-top: 16px;
  }

  .group {
    align-items: baseline;
    color: var(--fg);
    display: flex;
    font-size: 10px;
    font-weight: 600;
    gap: 8px;
    letter-spacing: 0.08em;
    margin: 10px 10px 5px;
    text-transform: uppercase;
  }

  /* Wraps, deliberately. Clipping to one line cost more than it bought: the heading is the
     thing that tells two jobs apart, and at the default 320px panel most real workstream
     names here are long enough to truncate — two that share a prefix then clip to the same
     visible string, which is the opposite of what this heading is for. It has no tooltip, so
     the clipped half was unreachable at any width below the drag. `overflow-wrap` only
     catches a pathological unbroken token. Not being an overflow container also keeps the
     flex baseline a real text baseline, so the count sits on the first line properly. */
  .group-name {
    flex: 1;
    min-width: 0;
    overflow-wrap: anywhere;
  }

  /* Gives the heading some mass against the rows below it, and answers "how much is in
     this job" without opening the board. */
  .group-add {
    align-items: center;
    background: none;
    border: none;
    border-radius: 4px;
    color: var(--fg-dim);
    cursor: pointer;
    display: flex;
    flex-shrink: 0;
    justify-content: center;
    padding: 2px;
  }
  .group-add:hover { background: var(--bg-light); color: var(--fg); }
  .group-add:focus-visible { outline: 1px solid var(--accent); outline-offset: 1px; }

  .group-count {
    background: var(--bg-light);
    border-radius: 999px;
    color: var(--fg-dim);
    flex-shrink: 0;
    font-size: 9px;
    font-variant-numeric: tabular-nums;
    letter-spacing: 0;
    padding: 1px 6px;
  }

  /* Without this the last row's hairline sat directly above the next group's heading, so the
     boundary looked like it belonged to the heading rather than closing the group. */
  .task-list .task:last-child { border-bottom: none; }

  .elsewhere {
    border-top: 1px solid var(--bg-light);
    color: var(--fg-dim);
    font-size: 11px;
    line-height: 1.5;
    margin: 12px 10px 0;
    padding-top: 8px;
  }

  .task-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }

  .task {
    border-bottom: 1px solid color-mix(in srgb, var(--bg-light) 45%, transparent);
    padding: 5px 8px;
  }
  .task.done { opacity: 0.5; }
  .task.parked { opacity: 0.62; }

  .task-main {
    align-items: center;
    display: flex;
    gap: 6px;
  }
  /* The status pill is wrapped for its tooltip, and the wrapper is what the row lays
     out — without this the pill gets squeezed by a long title instead of the title
     wrapping. */
  .task-main > :global(.tooltip-wrapper) { flex-shrink: 0; }

  .status {
    background: none;
    border: 1px solid currentColor;
    border-radius: 3px;
    cursor: pointer;
    flex-shrink: 0;
    font-size: 9px;
    letter-spacing: 0.04em;
    padding: 1px 5px;
    text-transform: uppercase;
  }
  .s-backlog { color: var(--fg-dim); opacity: 0.85; }
  .s-todo { color: var(--fg-dim); }
  .s-active { color: var(--accent); }
  .s-blocked { color: var(--red, #f7768e); }
  .s-review { color: var(--yellow, #e0af68); }
  .s-done { color: var(--green, #9ece6a); }

  .task-title {
    background: none;
    border: none;
    color: var(--fg);
    cursor: text;
    flex: 1;
    font-size: 12px;
    line-height: 1.35;
    min-width: 0;
    overflow-wrap: anywhere;
    padding: 0;
    text-align: left;
  }
  .task.done .task-title { text-decoration: line-through; }

  .edit-input {
    background: var(--bg-dark);
    border: 1px solid var(--accent);
    border-radius: 3px;
    color: var(--fg);
    flex: 1;
    font-size: 12px;
    min-width: 0;
    padding: 2px 4px;
  }
  .edit-input:focus { outline: none; }

  .row-ctl {
    align-items: center;
    display: flex;
    flex-shrink: 0;
    gap: 2px;
    opacity: 0;
  }
  .task:hover .row-ctl { opacity: 1; }
  .row-ctl:focus-within { opacity: 1; }

  .origin {
    color: var(--fg-dim);
    font-size: 10px;
    opacity: 0.8;
  }

  .mini {
    background: none;
    border: none;
    border-radius: 3px;
    color: var(--fg-dim);
    cursor: pointer;
    font-size: 11px;
    line-height: 1;
    padding: 2px 3px;
  }
  .mini:hover {
    background: var(--bg-light);
    color: var(--fg);
  }
  .mini.danger { color: var(--red, #f7768e); }

  .deps {
    color: var(--yellow, #e0af68);
    font-size: 10px;
    margin: 2px 0 0 calc(4px + 3.6em);
    opacity: 0.85;
  }

  .detail {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    color: var(--fg);
    font-family: inherit;
    font-size: 11px;
    margin-top: 4px;
    min-height: 54px;
    padding: 4px 6px;
    resize: vertical;
    width: 100%;
  }
  .detail:focus {
    border-color: var(--accent);
    outline: none;
  }

  .detail-preview {
    background: none;
    border: none;
    color: var(--fg-dim);
    cursor: text;
    display: block;
    font-size: 11px;
    line-height: 1.4;
    margin-top: 2px;
    overflow: hidden;
    padding: 0;
    text-align: left;
    text-overflow: ellipsis;
    white-space: nowrap;
    width: 100%;
  }
</style>
