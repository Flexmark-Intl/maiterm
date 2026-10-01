<!--
  A composer's attachments as removable chips: a preview for a pasted screenshot, a file icon
  otherwise. Shared by the terminal's composer dock and the Loom's Focus composer
  (lib/composer/attachments.ts holds what a paste or drop becomes).
-->
<script lang="ts">
  import Tooltip from '$lib/components/Tooltip.svelte';
  import type { ComposerAttachment } from '$lib/composer/attachments';

  interface Props {
    attachments: ComposerAttachment[];
    onremove: (index: number) => void;
  }
  let { attachments, onremove }: Props = $props();
</script>

{#if attachments.length > 0}
  <div class="chips">
    {#each attachments as att, i (att.path)}
      <Tooltip text={att.path}>
        <div class="chip">
          {#if att.thumb}
            <img class="thumb" src={att.thumb} alt={att.name} />
          {:else}
            <svg class="icon" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true">
              <path d="M4 1.5h5.5L13 5v9.5H4z"/>
              <path d="M9.5 1.5V5H13"/>
            </svg>
          {/if}
          <span class="name">{att.name}</span>
          <button class="remove" onclick={() => onremove(i)} aria-label="Remove {att.name}">&times;</button>
        </div>
      </Tooltip>
    {/each}
  </div>
{/if}

<style>
  .chips { display: flex; flex-wrap: wrap; gap: 6px; }
  .chip {
    display: flex;
    align-items: center;
    gap: 6px;
    max-width: 240px;
    padding: 3px 4px 3px 8px;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    color: var(--fg);
    font-size: 0.846rem;
  }
  .thumb { height: 24px; max-width: 48px; object-fit: cover; border-radius: 3px; }
  .icon { flex-shrink: 0; color: var(--fg-dim); }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .remove {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    padding: 0;
    color: var(--fg-dim);
    border-radius: 4px;
    font-size: 1rem;
    line-height: 1;
    transition: background 0.1s, color 0.1s;
  }
  .remove:hover { background: var(--bg-light); color: var(--fg); }
</style>
