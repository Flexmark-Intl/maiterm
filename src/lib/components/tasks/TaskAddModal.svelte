<script lang="ts">
  /** Add a task into a KNOWN workstream (docs/tasks.md §6).
   *
   *  The panel's inline field stays what it is — a one-line capture that infers where the
   *  task belongs and can only be right when the tab is working on a single job. This is the
   *  other case: the tab is juggling two or more, and the inline field's inference gives up
   *  and files the task loose, silently. The add button that opens this sits IN a workstream
   *  heading, so the destination is not a question anyone has to answer — it came from which
   *  button was pressed, and is shown here as a fact rather than a picker.
   *
   *  Which is also why this is a modal and not a second inline field per group: it exists to
   *  carry the two things the inline field cannot take — a description, and the lane the work
   *  starts in — and those need room the 320px dock does not have.
   */
  import type { TaskStatus } from '$lib/tauri/types';
  import ResizableTextarea from '$lib/components/ResizableTextarea.svelte';
  import { modLabel } from '$lib/utils/platform';

  interface Props {
    /** Display name of the destination job; null for the ungrouped bucket. */
    workstreamName: string | null;
    onsubmit: (v: { title: string; detail: string | null; status: TaskStatus }) => void;
    oncancel: () => void;
  }

  let { workstreamName, onsubmit, oncancel }: Props = $props();

  /** Only the three lanes work can START in. `blocked` needs something to be blocked on,
   *  `review` needs something to review, and `done` is not a thing you create — offering
   *  them here would be offering states a brand-new task cannot truthfully be in. */
  const LANES: { id: TaskStatus; label: string; hint: string }[] = [
    { id: 'todo', label: 'To-do', hint: 'Ready to start' },
    { id: 'active', label: 'Active', hint: 'Starting now' },
    { id: 'backlog', label: 'Parked', hint: 'Not now — shelved, and exempt from stale checks' },
  ];

  let title = $state('');
  let detail = $state('');
  let status = $state<TaskStatus>('todo');
  let titleEl = $state<HTMLInputElement | null>(null);

  /* Explicit focus, not the `autofocus` attribute. Svelte compiles that to a microtask that
     focuses ONLY if `document.activeElement === body`, which holds when the modal was opened
     by mouse (WebKit doesn't mouse-focus buttons) and fails when it was opened from the
     keyboard — the `+` button keeps focus, and since it lives outside this backdrop, typing
     went nowhere AND Escape never reached the handler below, because that handler is on the
     backdrop and only sees events originating inside it. Same rAF pattern QuickOpen and
     AgentBridgePicker already use. */
  $effect(() => {
    const id = requestAnimationFrame(() => titleEl?.focus());
    return () => cancelAnimationFrame(id);
  });

  const canSave = $derived(title.trim().length > 0);

  function save() {
    if (!canSave) return;
    onsubmit({ title: title.trim(), detail: detail.trim() || null, status });
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      oncancel();
      return;
    }
    // Enter saves from the title field; the description is multi-line, so there it needs the
    // modifier or you could never type a second paragraph.
    if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      save();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) oncancel();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="backdrop"
  onclick={handleBackdropClick}
  onkeydown={handleKeydown}
  role="dialog"
  aria-modal="true"
  aria-label="Add a task"
  tabindex="-1"
>
  <div class="panel">
    <div class="header">
      <div class="title">New task</div>
      <div class="subtitle">
        Filed under
        {#if workstreamName}<strong>{workstreamName}</strong>{:else}<em>no workstream</em>{/if},
        assigned to this tab.
      </div>
    </div>

    <div class="body">
      <label class="field">
        <span class="label">Title</span>
        <input
          class="text-input"
          bind:this={titleEl}
          bind:value={title}
          placeholder="What needs doing"
          onkeydown={(e) => { if (e.key === 'Enter') { e.preventDefault(); save(); } }}
        />
      </label>

      <label class="field">
        <span class="label">Description <span class="opt">optional</span></span>
        <ResizableTextarea
          value={detail}
          rows={4}
          maxHeight={220}
          placeholder="Acceptance criteria, links, anything the agent should know"
          onchange={(v) => (detail = v)}
        />
      </label>

      <div class="field">
        <span class="label">Starts in</span>
        <div class="lanes">
          {#each LANES as lane (lane.id)}
            <button
              type="button"
              class="lane"
              class:on={status === lane.id}
              aria-pressed={status === lane.id}
              onclick={() => (status = lane.id)}
            >
              <span class="lane-name">{lane.label}</span>
              <span class="lane-hint">{lane.hint}</span>
            </button>
          {/each}
        </div>
      </div>
    </div>

    <div class="footer">
      <span class="hint">{modLabel}+Enter to add</span>
      <div class="actions">
        <button type="button" class="btn" onclick={oncancel}>Cancel</button>
        <button type="button" class="btn primary" disabled={!canSave} onclick={save}>Add task</button>
      </div>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.4);
    display: flex;
    justify-content: center;
    padding-top: 15vh;
    z-index: 1000;
  }

  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    max-height: 70vh;
    width: 460px;
  }

  .header {
    border-bottom: 1px solid var(--bg-light);
    padding: 12px 14px 10px;
  }

  .title {
    color: var(--fg);
    font-size: 1rem;
    font-weight: 600;
  }

  .subtitle {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.4;
    margin-top: 3px;
  }
  .subtitle strong { color: var(--fg); font-weight: 600; }

  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    overflow-y: auto;
    padding: 14px;
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }

  .label {
    color: var(--fg-dim);
    font-size: 0.72rem;
    font-weight: 600;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .opt {
    font-weight: 400;
    letter-spacing: 0;
    opacity: 0.7;
    text-transform: none;
  }

  .text-input {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font-family: inherit;
    font-size: 0.85rem;
    padding: 7px 9px;
    width: 100%;
  }
  .text-input:focus {
    border-color: var(--accent);
    outline: none;
  }
  .text-input::placeholder { color: var(--fg-dim); }

  .lanes {
    display: flex;
    gap: 6px;
  }

  .lane {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg-dim);
    cursor: pointer;
    display: flex;
    flex: 1;
    flex-direction: column;
    gap: 2px;
    padding: 7px 8px;
    text-align: left;
  }
  .lane:hover { border-color: var(--fg-dim); }
  .lane:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .lane.on {
    background: color-mix(in srgb, var(--accent) 14%, var(--bg-dark));
    border-color: var(--accent);
    color: var(--fg);
  }

  .lane-name { font-size: 0.82rem; font-weight: 600; }
  .lane-hint { font-size: 0.68rem; line-height: 1.3; opacity: 0.8; }

  .footer {
    align-items: center;
    border-top: 1px solid var(--bg-light);
    display: flex;
    gap: 10px;
    justify-content: space-between;
    padding: 10px 14px;
  }

  .hint {
    color: var(--fg-dim);
    font-size: 0.72rem;
  }

  .actions { display: flex; gap: 7px; }

  .btn {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    cursor: pointer;
    font-family: inherit;
    font-size: 0.8rem;
    padding: 6px 12px;
  }
  .btn:hover:not(:disabled) { border-color: var(--fg-dim); }
  .btn:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .btn.primary {
    background: var(--accent);
    border-color: var(--accent);
    color: var(--bg-dark);
    font-weight: 600;
  }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
