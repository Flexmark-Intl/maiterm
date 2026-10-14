<!--
  Agent-written text about a task (a blocker's question, a note), with any task the agent
  named by its id shown by its title instead. The human never sees task ids anywhere else.
  `of` is the task the text belongs to: the ids are looked up in its project.
-->
<script lang="ts">
  import { tasksStore } from '$lib/stores/tasks.svelte';
  import { taskTextParts } from '$lib/tasks/model';
  import Tooltip from '$lib/components/Tooltip.svelte';

  interface Props {
    text: string;
    of: string;
  }

  let { text, of }: Props = $props();

  const parts = $derived.by(() => {
    const ws = tasksStore.findAnywhere(of)?.workspaceId;
    return taskTextParts(text, ws ? tasksStore.forWorkspace(ws) : []);
  });
</script>

{#each parts as p, i (i)}{#if 'task' in p}<Tooltip text="Task · {p.task.status}"><span class="ref">“{p.task.title}”</span></Tooltip>{:else}{p.text}{/if}{/each}

<style>
  .ref {
    color: var(--accent);
  }
</style>
