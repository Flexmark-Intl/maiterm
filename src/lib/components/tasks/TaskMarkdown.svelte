<!--
  Longer agent-written text about a task as formatted text: lists, code, emphasis, with a task
  named by its id shown by its title (`TaskText` is the one-line sibling). It stays readable in a
  side panel 200px wide, so nothing here sets a width and wide blocks scroll on their own.
  `of` is the task the text belongs to: the ids are looked up in its project.
-->
<script lang="ts">
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { renderTaskMarkdown } from '$lib/tasks/markdown';

  interface Props {
    text: string;
    of: string;
  }

  let { text, of }: Props = $props();

  const html = $derived.by(() => {
    const ws = tasksStore.findAnywhere(of)?.workspaceId;
    return renderTaskMarkdown(text, ws ? tasksStore.forWorkspace(ws) : []);
  });
</script>

<div class="task-md">{@html html}</div>

<style>
  .task-md { line-height: 1.45; }
  .task-md :global(p) { margin: 0 0 0.6em; }
  .task-md :global(:last-child) { margin-bottom: 0; }
  .task-md :global(ul), .task-md :global(ol) { margin: 0 0 0.6em; padding-left: 1.7em; }
  .task-md :global(li) { margin: 0.35em 0; }
  .task-md :global(li::marker) { color: var(--fg-dim); font-variant-numeric: tabular-nums; }
  .task-md :global(h1), .task-md :global(h2), .task-md :global(h3), .task-md :global(h4) {
    margin: 0.5em 0 0.25em;
    font-size: inherit;
    font-weight: 600;
  }
  .task-md :global(strong) { font-weight: 600; }
  .task-md :global(a) { color: var(--accent); }
  .task-md :global(code) {
    font-family: var(--font-mono, ui-monospace, monospace);
    font-size: 0.9em;
    background: var(--bg-dark);
    padding: 1px 4px;
    border-radius: 3px;
  }
  .task-md :global(pre) {
    margin: 0 0 0.6em;
    background: var(--bg-dark);
    padding: 6px 8px;
    border-radius: 4px;
    overflow-x: auto;
  }
  .task-md :global(pre code) { background: none; padding: 0; }
  .task-md :global(table) { display: block; overflow-x: auto; border-collapse: collapse; margin: 0 0 0.6em; }
  .task-md :global(th), .task-md :global(td) { padding: 2px 10px 2px 0; text-align: left; }
  .task-md :global(th) { font-weight: 600; border-bottom: 1px solid var(--bg-light); }
  .task-md :global(.md-img) { color: var(--fg-dim); font-style: italic; }
  .task-md :global(.ref) { color: var(--accent); }
</style>
