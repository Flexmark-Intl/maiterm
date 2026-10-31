<script lang="ts">
  /** New goal (docs/hive.md §11.5): a Done or paused Hive takes its next goal in place. Workers are
   *  retired, a fresh Queen starts in the same tab, the gallery, settings, folders and (unless
   *  unticked) the memory stay, and the old goal is kept as a chapter. Refused while commits
   *  aren't landed: the sheet lists them with the one button that does something about it. The
   *  click is the human's authority; Rust re-checks everything.
   *  Same backdrop/panel vocabulary as HiveDeleteSheet, and the same explicit rAF focus. */
  import { open as dialogOpen } from '@tauri-apps/plugin-dialog';
  import { error as logError } from '@tauri-apps/plugin-log';
  import { hiveNewGoal, hiveNewGoalPreview } from '$lib/tauri/commands';
  import type { HiveNewGoalPreview } from '$lib/tauri/types';
  import { workspacesStore } from '$lib/stores/workspaces.svelte';
  import { toastStore } from '$lib/stores/toasts.svelte';
  import IconButton from '$lib/components/ui/IconButton.svelte';
  import { charCount, MAX_BRIEF, MAX_BRIEF_FILES, MAX_INSTRUCTIONS } from '$lib/hive/model';
  import { mergeBlocking, newGoalProblem } from '$lib/hive/newGoal';
  import HivePendingCommits from './HivePendingCommits.svelte';

  interface Props {
    workspaceId: string;
    name: string;
    onclose: () => void;
    /** Changes whenever the Hive's board is refetched (a land, a worker stopping, Done): the
     *  sheet re-reads what holds New goal then. */
    refreshKey?: unknown;
  }
  let { workspaceId, name, onclose, refreshKey }: Props = $props();

  let preview = $state<HiveNewGoalPreview | null>(null);
  let readError = $state<string | null>(null);
  let goal = $state('');
  let brief = $state('');
  let briefFiles = $state<string[]>([]);
  let instructions = $state('');
  let rulesOpen = $state(false);
  let keepMemory = $state(true);
  let submitting = $state(false);
  let refusal = $state<string | null>(null);
  let goalEl = $state<HTMLInputElement | null>(null);

  let readSeq = 0;
  $effect(() => {
    const id = workspaceId;
    const mine = ++readSeq;
    hiveNewGoalPreview(id)
      .then((p) => {
        if (id !== workspaceId || mine !== readSeq) return;
        preview = p;
        goal = p.goal;
        brief = p.brief;
        briefFiles = [...p.brief_files];
        instructions = p.instructions;
        // The old goal, selected: typing replaces it, an arrow key keeps it to edit.
        requestAnimationFrame(() => {
          goalEl?.focus();
          goalEl?.select();
        });
      })
      .catch((e) => { readError = String(e).replace(/^Error: /, ''); });
  });

  // Re-read on every board change, once the prefill is in: only what holds New goal is replaced,
  // never the goal, brief, files or rules being edited (the prefill happens once). A board change
  // that comes while the prefill is in flight or a submit runs isn't dropped: `lastKey` moves only
  // when a re-read starts, and `ready`/`submitting` are tracked, so it runs once they allow it (the
  // first time the prefill lands, that is one extra read, which catches a land the prefill missed).
  let lastKey: unknown = undefined;
  const ready = $derived(preview !== null);
  $effect(() => {
    const key = refreshKey;
    if (!ready || submitting) return;
    if (key === lastKey) return;
    lastKey = key;
    const id = workspaceId;
    const mine = ++readSeq;
    hiveNewGoalPreview(id)
      .then((p) => {
        if (id !== workspaceId || mine !== readSeq || !preview) return;
        preview = mergeBlocking(preview, p);
      })
      .catch((e) => logError(`hive: new-goal preview of ${id.slice(0, 8)} not re-read: ${e}`));
  });

  // Before the preview the goal field is disabled: the keyboard waits on the dialog, so Escape works.
  let backdropEl = $state<HTMLDivElement | null>(null);
  $effect(() => {
    const id = requestAnimationFrame(() => {
      if (goalEl && !goalEl.disabled) goalEl.focus();
      else backdropEl?.focus();
    });
    return () => cancelAnimationFrame(id);
  });

  const fileName = (p: string) => p.split(/[\\/]/).filter(Boolean).pop() ?? p;
  async function attachBriefFiles() {
    const picked = await dialogOpen({ directory: false, multiple: true, title: 'Attach files to the project brief' }).catch(() => null);
    const paths = (Array.isArray(picked) ? picked : picked ? [picked] : []).map(String);
    briefFiles = [...briefFiles, ...paths.filter((p) => !briefFiles.includes(p))];
  }

  const briefCount = $derived(charCount(brief));
  const instructionCount = $derived(charCount(instructions));
  const problem = $derived(newGoalProblem(preview, { goal, brief, instructions, briefFiles }));
  const canSubmit = $derived(!problem && !submitting);
  const blockedByWorkers = $derived(!!preview && (!preview.allowed || preview.reasons.length > 0));

  async function submit() {
    if (!canSubmit) return;
    submitting = true;
    refusal = null;
    try {
      const hive = await hiveNewGoal(workspaceId, {
        goal: goal.trim(),
        brief,
        brief_files: briefFiles,
        instructions,
        keep_memory: keepMemory,
      });
      workspacesStore.applyHiveRecord(workspaceId, hive);
      toastStore.addToast(`${name || 'Hive'}: new goal`, 'The Queen is starting fresh. It will propose completion criteria for you to confirm.', 'info');
      onclose();
    } catch (e) {
      submitting = false;
      refusal = String(e).replace(/^Error: /, '');
      logError(`hive: new goal for ${workspaceId.slice(0, 8)} refused: ${e}`);
    }
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && !submitting) {
      e.stopPropagation();
      onclose();
    } else if (e.key === 'Enter' && (e.metaKey || e.ctrlKey)) {
      e.preventDefault();
      void submit();
    }
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget && !submitting) onclose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div class="backdrop" bind:this={backdropEl} onclick={handleBackdropClick} onkeydown={handleKeydown} role="dialog" aria-modal="true" aria-label="New goal" tabindex="-1">
  <div class="panel">
    <div class="header">
      <div class="title">A new goal for {name || 'this Hive'}</div>
      <div class="subtitle">
        Its workers stop and are cleared from the board, and a fresh Queen starts in the same place. The gallery,
        settings and folders stay{preview ? `, and the current goal is kept as chapter ${preview.next_chapter}` : ''}.
        The Queen proposes completion criteria for the new goal, and you confirm them again before any worker starts.
      </div>
    </div>

    <div class="body">
      {#if readError}
        <p class="error" role="alert">Couldn't read the Hive: {readError}</p>
      {/if}

      {#if preview && blockedByWorkers}
        <section class="blocked" role="alert">
          <h4>Not yet</h4>
          <ul>{#each preview.reasons as r (r)}<li>{r}</li>{/each}</ul>
        </section>
      {/if}

      {#if preview?.unlanded.length}
        <section class="pending">
          <HivePendingCommits {workspaceId} unlanded={preview.unlanded} />
        </section>
      {/if}

      <label class="field">
        <span class="label">Goal</span>
        <input bind:this={goalEl} bind:value={goal} disabled={!preview || submitting} placeholder="One line: what done looks like" />
        <span class="hint">The current goal is filled in: type to replace it.</span>
      </label>
      <label class="field">
        <span class="label">Project brief <span class="opt">optional</span></span>
        <textarea rows="6" bind:value={brief} disabled={!preview || submitting} placeholder="What the Queen needs for this goal: the plan, constraints, links."></textarea>
        <span class="hint">Given to the Queen only; workers get what she tells them.</span>
        <span class="hint count" class:over={briefCount > MAX_BRIEF}>{briefCount.toLocaleString()} / 32,000 characters</span>
      </label>
      <div class="brief-files">
        {#each briefFiles as f (f)}
          <div class="brief-file">
            <code>{fileName(f)}</code>
            <IconButton tooltip="Remove {fileName(f)}" onclick={() => (briefFiles = briefFiles.filter((x) => x !== f))} aria-label="Remove {fileName(f)}">&times;</IconButton>
          </div>
        {/each}
        {#if briefFiles.length < MAX_BRIEF_FILES}
          <button type="button" class="btn" disabled={!preview || submitting} onclick={() => void attachBriefFiles()}>Attach files…</button>
        {/if}
      </div>
      {#if rulesOpen || instructions.trim()}
        <label class="field">
          <span class="label">Rules for every agent <span class="opt">optional</span></span>
          <textarea rows="4" bind:value={instructions} disabled={!preview || submitting} placeholder="Rules for all work, whatever the task: “use pnpm”, “don't touch legacy/”"></textarea>
          <span class="hint">Every worker gets these too, so keep them short.</span>
          <span class="hint count" class:over={instructionCount > MAX_INSTRUCTIONS}>{instructionCount.toLocaleString()} / 16,000 characters</span>
        </label>
      {:else}
        <button type="button" class="reveal" onclick={() => (rulesOpen = true)}>Add rules for every agent</button>
      {/if}

      {#if preview?.memory}
        <label class="check">
          <input type="checkbox" bind:checked={keepMemory} disabled={submitting} />
          <span>
            Keep the shared memory ({preview.memory.files} file{preview.memory.files === 1 ? '' : 's'})
            <span class="quiet">Unticked, it's archived with the old goal and the new one starts with none.</span>
          </span>
        </label>
      {/if}

      {#if preview}
        <section>
          <h4>Workers that stop</h4>
          {#if preview.live_workers.length}
            <ul>{#each preview.live_workers as w (w)}<li>{w}</li>{/each}</ul>
          {:else}
            <p class="quiet">None are running.</p>
          {/if}
        </section>
      {/if}

      {#if refusal}<p class="error" role="alert">{refusal}</p>{/if}
    </div>

    <div class="footer">
      {#if problem && preview && !blockedByWorkers && !preview.unlanded.length}<span class="problem">{problem}</span>{/if}
      <button type="button" class="btn" disabled={submitting} onclick={onclose}>Cancel</button>
      <button type="button" class="btn primary" disabled={!canSubmit} onclick={() => void submit()}>{submitting ? 'Starting…' : 'Start the new goal'}</button>
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
    padding-top: 8vh;
    z-index: 1000;
  }
  .backdrop:focus { outline: none; }

  .panel {
    align-self: flex-start;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.5);
    display: flex;
    flex-direction: column;
    max-height: 84vh;
    max-width: calc(100vw - 32px);
    width: 600px;
  }

  .header { border-bottom: 1px solid var(--bg-light); padding: 12px 14px 10px; }
  .title { color: var(--fg); font-size: 1rem; font-weight: 600; overflow-wrap: anywhere; }
  .subtitle { color: var(--fg-dim); font-size: 0.8rem; line-height: 1.45; margin-top: 3px; }

  .body { display: flex; flex-direction: column; gap: 12px; overflow-y: auto; padding: 14px; }
  section { display: flex; flex-direction: column; gap: 4px; }
  section.pending { padding: 10px; border: 1px solid color-mix(in srgb, var(--yellow) 55%, transparent); border-radius: 6px; background: color-mix(in srgb, var(--yellow) 8%, var(--bg-medium)); }
  section.blocked { padding: 8px 10px; border: 1px solid color-mix(in srgb, var(--red) 50%, transparent); border-radius: 6px; }
  h4 { margin: 0; color: var(--fg-dim); font-size: 0.72rem; font-weight: 600; letter-spacing: 0.06em; text-transform: uppercase; }
  ul { margin: 0; padding-left: 1.2em; display: flex; flex-direction: column; gap: 2px; }
  li { color: var(--fg); font-size: 0.8rem; line-height: 1.4; overflow-wrap: anywhere; }
  .quiet { display: block; color: var(--fg-dim); font-size: 0.78rem; margin: 0; }

  .field { display: flex; flex-direction: column; gap: 4px; }
  .label { font-size: 0.8rem; font-weight: 600; color: var(--fg); }
  .opt { font-weight: 400; color: var(--fg-dim); margin-left: 6px; font-size: 0.75rem; }
  input:not([type='checkbox']), textarea {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 5px;
    color: var(--fg);
    font: inherit;
    font-size: 0.85rem;
    padding: 6px 8px;
    width: 100%;
    box-sizing: border-box;
  }
  input:focus, textarea:focus { outline: none; border-color: var(--accent); }
  input:disabled, textarea:disabled { opacity: 0.6; }
  textarea { resize: vertical; line-height: 1.45; }
  .hint { font-size: 0.75rem; color: var(--fg-dim); overflow-wrap: anywhere; line-height: 1.4; }
  .count { align-self: flex-end; font-variant-numeric: tabular-nums; }
  .count.over { color: var(--red); }

  .brief-files { display: flex; flex-wrap: wrap; align-items: center; gap: 6px 8px; }
  .brief-file { display: inline-flex; align-items: center; gap: 2px; min-width: 0; max-width: 100%; border: 1px solid var(--bg-light); border-radius: 6px; padding-left: 8px; }
  .brief-file code { font-family: var(--font-mono, ui-monospace, monospace); font-size: 0.78rem; overflow-wrap: anywhere; }
  .reveal { align-self: flex-start; background: none; border: 0; padding: 2px 0; color: var(--accent); font: inherit; font-size: 0.8rem; cursor: pointer; }
  .reveal:hover { text-decoration: underline; }
  .reveal:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; border-radius: 3px; }

  .check { display: flex; align-items: flex-start; gap: 8px; color: var(--fg); font-size: 0.8rem; cursor: pointer; }
  .check input { margin-top: 2px; }

  .error { color: var(--red); font-size: 0.78rem; margin: 0; overflow-wrap: anywhere; }
  .problem { margin-right: auto; color: var(--fg-dim); font-size: 0.78rem; overflow-wrap: anywhere; }

  .footer { align-items: center; border-top: 1px solid var(--bg-light); display: flex; flex-wrap: wrap; gap: 7px; justify-content: flex-end; padding: 10px 14px; }

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
  .btn.primary { background: var(--accent); border-color: var(--accent); color: var(--bg-dark); font-weight: 600; }
  .btn:disabled { cursor: default; opacity: 0.45; }
</style>
