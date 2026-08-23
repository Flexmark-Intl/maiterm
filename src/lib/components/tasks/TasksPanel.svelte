<script lang="ts">
  /** Per-tab task panel (docs/tasks.md §6).
   *
   *  Deliberately built on the notes-panel pattern — same dock, same drag-resize, same
   *  per-tab persistence — so the muscle memory transfers and there is one mental model
   *  for "side panel" in maiTerm.
   *
   *  Shows this tab's work first, then the rest of the project. That ordering is the
   *  point: the panel answers "what am I doing here" before "what is going on overall". */
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { effectiveStatus, isParked, TASK_STATUSES } from '$lib/tasks/model';
  import type { Task, TaskStatus } from '$lib/tauri/types';
  import Icon from '$lib/components/Icon.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';

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
  let showDone = $state(false);
  let showParked = $state(false);

  const all = $derived(tasksStore.forWorkspace(workspaceId));

  /** Finished and parked work both collapse behind a count — the panel is for what's in
   *  flight. Parked is separate from done because they mean different things: one is
   *  finished, the other is deliberately not started. */
  const visible = (list: Task[]) =>
    list.filter((t) => (showDone || t.status !== 'done') && (showParked || !isParked(t.status)));
  const doneCount = $derived(all.filter((t) => t.status === 'done').length);
  const parkedCount = $derived(all.filter((t) => isParked(t.status)).length);

  /** This tab's work first, grouped by job, then the rest of the project.
   *
   *  The panel answers "what am I doing here" before "what is going on overall", and
   *  within that, one tab is routinely running two unrelated jobs — so the grouping has to
   *  be by workstream or the two blur into one list. */
  interface Group {
    key: string;
    label: string;
    sub: string | null;
    list: Task[];
  }

  const groups = $derived.by<Group[]>(() => {
    const streams = tasksStore.workstreams(workspaceId);
    const nameOf = (id: string | null | undefined) =>
      id ? (streams.find((w) => w.id === id)?.name ?? null) : null;
    const out: Group[] = [];
    for (const [scope, label] of [['mine', 'This tab'], ['others', 'Project']] as const) {
      const scoped = all.filter((t) => (scope === 'mine' ? t.tab_id === tabId : t.tab_id !== tabId));
      const byStream = new Map<string, Task[]>();
      for (const t of scoped) {
        const k = t.workstream_id ?? '';
        if (!byStream.has(k)) byStream.set(k, []);
        byStream.get(k)!.push(t);
      }
      const keys = [...byStream.keys()].sort((a, b) => {
        // Loose tasks last; named jobs alphabetical.
        if (!a) return 1;
        if (!b) return -1;
        return (nameOf(a) ?? '').localeCompare(nameOf(b) ?? '');
      });
      for (const k of keys) {
        const list = visible(byStream.get(k)!);
        if (!list.length) continue;
        out.push({ key: `${scope}:${k}`, label, sub: nameOf(k || null), list });
      }
    }
    return out;
  });

  const STATUS_LABEL: Record<TaskStatus, string> = {
    backlog: 'Parked',
    todo: 'To-do',
    active: 'Active',
    blocked: 'Blocked',
    review: 'Review',
    done: 'Done',
  };

  /** Which job a new task lands in. '' = ungrouped, '+' = name a new one.
   *  Sticky between adds: entering several tasks for one job is the common case. */
  let addStream = $state('');
  let newStreamName = $state('');

  function addTask() {
    const title = draft.trim();
    if (!title) return;
    let workstreamId: string | null = null;
    if (addStream === '+') {
      const name = newStreamName.trim();
      if (!name) return;
      const created = tasksStore.ensureWorkstream(workspaceId, name);
      workstreamId = created?.id ?? null;
      // Switch to the created stream so the next task lands in it too, rather than
      // re-prompting for a name that now exists.
      if (created) addStream = created.id;
      newStreamName = '';
    } else if (addStream) {
      workstreamId = addStream;
    }
    tasksStore.add(workspaceId, { title, tab_id: tabId, origin: 'human', workstream_id: workstreamId });
    draft = '';
  }

  /** Move a task into another job from its row. */
  function setWorkstream(t: Task, id: string) {
    tasksStore.update(workspaceId, t.id, { workstream_id: id || null });
  }

  /** Click the status chip to advance; shift-click to go back. Cycling beats a dropdown
   *  here — status changes are the panel's most frequent action by far.
   *
   *  Steps from the status the chip DISPLAYS, not the stored one. On a task blocked by an
   *  unfinished dependency those differ, and stepping from the stored value made the chip
   *  look frozen: three clicks would silently walk the stored status through the whole
   *  vocabulary while the label stayed "BLOCKED", then jump to "DONE" on the fourth. */
  function cycleStatus(t: Task, back: boolean) {
    const shown = effectiveStatus(t, all);
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

  function assignToMe(t: Task) {
    tasksStore.update(workspaceId, t.id, { tab_id: t.tab_id === tabId ? null : tabId });
  }

  function remove(id: string) {
    tasksStore.remove(workspaceId, id);
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

  <div class="add-row stream-row">
    <select class="stream-select" bind:value={addStream} aria-label="Workstream for new tasks">
      <option value="">Ungrouped</option>
      {#each tasksStore.workstreams(workspaceId) as w (w.id)}
        <option value={w.id}>{w.name}</option>
      {/each}
      <option value="+">New workstream…</option>
    </select>
    {#if addStream === '+'}
      <input
        class="add-input"
        placeholder="Name it, e.g. Auth refactor"
        bind:value={newStreamName}
        onkeydown={(e) => {
          if (e.key === 'Enter') addTask();
        }}
      />
    {/if}
  </div>

  <div class="lists">
    {#if all.length === 0}
      <p class="empty">
        Nothing tracked for this project yet. Add a task above — agents in this workspace can
        read and update the same list.
      </p>
    {/if}

    {#each groups as group (group.key)}
      {#if group.list.length}
        <h4 class="group">
          {group.label}
          {#if group.sub}<span class="group-stream">· {group.sub}</span>
          {:else}<span class="group-loose">· ungrouped</span>{/if}
        </h4>
        <ul class="task-list">
          {#each group.list as t (t.id)}
            {@const eff = effectiveStatus(t, all)}
            <li class="task" class:done={t.status === 'done'} class:parked={isParked(t.status)}>
              <div class="task-main">
                <button
                  class="status s-{eff}"
                  title="{STATUS_LABEL[eff]} — click to advance, shift-click to go back"
                  onclick={(e) => cycleStatus(t, e.shiftKey)}
                >
                  {STATUS_LABEL[eff]}
                </button>

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
                    <span class="origin" title="Recorded by {t.origin === 'imported' ? 'an import from the agent\'s own list' : t.origin}">
                      {t.origin === 'imported' ? '⇥' : '◆'}
                    </span>
                  {/if}
                  <select
                    class="mini-select"
                    title="Move to another workstream"
                    value={t.workstream_id ?? ''}
                    onchange={(e) => setWorkstream(t, e.currentTarget.value)}
                  >
                    <option value="">Ungrouped</option>
                    {#each tasksStore.workstreams(workspaceId) as w (w.id)}
                      <option value={w.id}>{w.name}</option>
                    {/each}
                  </select>
                  <button
                    class="mini"
                    title={t.tab_id === tabId ? 'Unassign (leave for whoever picks it up)' : 'Assign to this tab'}
                    onclick={() => assignToMe(t)}
                  >
                    {t.tab_id === tabId ? '↥' : '↧'}
                  </button>
                  {#if confirmingDelete === t.id}
                    <button class="mini danger" title="Confirm delete" onclick={() => remove(t.id)}>✓</button>
                    <button class="mini" title="Cancel" onclick={() => (confirmingDelete = null)}>✕</button>
                  {:else}
                    <button class="mini" title="Delete" onclick={() => (confirmingDelete = t.id)}>
                      <Icon name="trash" size={11} />
                    </button>
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
      {/if}
    {/each}
  </div>
</div>

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

  .stream-row { padding-top: 0; border-bottom: 1px solid var(--bg-light); }
  .stream-select {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    font-size: 11px;
    max-width: 100%;
    padding: 3px 4px;
  }
  .stream-select:focus { border-color: var(--accent); outline: none; }

  .mini-select {
    background: none;
    border: none;
    color: var(--fg-dim);
    font-size: 10px;
    max-width: 60px;
    padding: 0;
  }
  .mini-select:focus { outline: none; }

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

  .group-stream { color: var(--accent); font-weight: 500; }
  .group-loose { font-style: italic; opacity: 0.7; text-transform: none; letter-spacing: 0; }

  .group {
    color: var(--fg-dim);
    font-size: 10px;
    font-weight: 600;
    letter-spacing: 0.08em;
    margin: 10px 10px 4px;
    text-transform: uppercase;
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
