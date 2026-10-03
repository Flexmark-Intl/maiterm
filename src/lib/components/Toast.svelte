<script lang="ts">
  import { toastStore } from '$lib/stores/toasts.svelte';
  import { navigateToTab } from '$lib/stores/workspaces.svelte';
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { fly, fade } from 'svelte/transition';

  function handleToastClick(toast: typeof toastStore.toasts[0]) {
    if (toast.action) {
      toast.action();
      toastStore.removeToast(toast.id);
    } else if (toast.source?.tabId) {
      navigateToTab(toast.source.tabId);
      toastStore.removeToast(toast.id);
    }
  }
</script>

{#if toastStore.toasts.length > 0}
  <div class="toast-container" style:max-width="{preferencesStore.toastWidth}px" style:font-size="{preferencesStore.toastFontSize}px">
    {#each toastStore.toasts as toast (toast.id)}
      <!-- svelte-ignore a11y_no_static_element_interactions a11y_click_events_have_key_events -- toast dismiss is decorative, close button provides keyboard access -->
      <div
        class="toast toast-{toast.type}"
        class:request={!!toast.request}
        class:clickable={!!toast.action || !!toast.source?.tabId}
        in:fly={{ x: 300, duration: 250 }}
        out:fade={{ duration: 150 }}
        onclick={() => handleToastClick(toast)}
        onmouseenter={() => toastStore.pauseToast(toast.id)}
        onmouseleave={() => toastStore.resumeToast(toast.id)}
      >
        <div class="toast-content">
          <div class="toast-title">{toast.title}</div>
          {#if toast.body}
            <div class="toast-body">{toast.body}</div>
          {/if}
        </div>
        {#if toast.onCancel}
          <button
            class="toast-cancel"
            onclick={(e) => { e.stopPropagation(); toast.onCancel?.(); }}
            aria-label="Cancel upload"
          >Cancel</button>
        {:else}
          <button
            class="toast-close"
            onclick={(e) => { e.stopPropagation(); toastStore.removeToast(toast.id); }}
            aria-label="Dismiss notification"
          >&times;</button>
        {/if}
        {#if toast.request}
          <!-- Waiting on the human: no countdown, nothing filling up. -->
        {:else if toast.sticky}
          {#if toast.indeterminate}
            <div class="toast-progress indeterminate"></div>
          {:else}
            <div class="toast-progress determinate" style:width="{toast.progress ?? 0}%"></div>
          {/if}
        {:else}
          <div class="toast-progress" style:animation-duration="{toast.duration}ms" style:animation-play-state={toastStore.isActive(toast.id) ? 'running' : 'paused'}></div>
        {/if}
      </div>
    {/each}
  </div>
{/if}

<style>
  .toast-container {
    position: fixed;
    bottom: 16px;
    right: 16px;
    z-index: 10000;
    display: flex;
    flex-direction: column;
    gap: 8px;
    pointer-events: none;
    /* max-width set via inline style from preferences */
  }

  .toast {
    pointer-events: auto;
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 10px 14px;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
    box-shadow: 0 4px 16px rgba(0, 0, 0, 0.4);
    position: relative;
    overflow: hidden;
  }

  .toast.clickable {
    cursor: pointer;
  }

  .toast.clickable:hover {
    border-color: var(--accent);
  }

  .toast-success {
    border-left: 3px solid var(--green, #9ece6a);
  }

  .toast-error {
    border-left: 3px solid var(--red, #f7768e);
  }

  .toast-info {
    border-left: 3px solid var(--cyan, #7dcfff);
  }

  /* Waiting on the human: the same yellow as the card it opens. */
  .toast.request {
    border-left: 3px solid var(--yellow, #e0af68);
  }

  .toast-content {
    flex: 1;
    min-width: 0;
  }

  .toast-title {
    font-size: inherit;
    font-weight: 600;
    color: var(--fg);
    margin-bottom: 2px;
  }

  .toast-body {
    font-size: inherit;
    color: #9aa5ce;
    line-height: 1.4;
    word-break: break-word;
  }

  .toast-close {
    flex-shrink: 0;
    width: 22px;
    height: 22px;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    font-size: 1.1em;
    color: var(--fg-dim);
    border-radius: 4px;
    cursor: pointer;
  }

  .toast-close:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .toast-cancel {
    flex-shrink: 0;
    height: 22px;
    padding: 0 10px;
    display: flex;
    align-items: center;
    font-size: 0.85em;
    font-weight: 500;
    color: var(--fg);
    background: var(--bg-light);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    cursor: pointer;
    transition: background 0.12s, border-color 0.12s, color 0.12s;
  }

  .toast-cancel:hover {
    color: var(--red, #f7768e);
    border-color: var(--red, #f7768e);
    background: color-mix(in srgb, var(--red, #f7768e) 12%, var(--bg-light));
  }

  .toast-progress {
    position: absolute;
    bottom: 0;
    left: 0;
    height: 3px;
    background: var(--accent);
    opacity: 0.5;
    animation: toast-timer linear forwards;
    /* duration set via inline style */
  }

  /* Determinate upload progress — width set inline from toast.progress. */
  .toast-progress.determinate {
    animation: none;
    opacity: 0.9;
    transition: width 0.3s linear;
  }

  /* Indeterminate upload progress — sliding marquee. */
  .toast-progress.indeterminate {
    animation: toast-marquee 1.2s ease-in-out infinite;
    opacity: 0.9;
    width: 35%;
  }

  @keyframes toast-timer {
    from { width: 100%; }
    to { width: 0%; }
  }

  @keyframes toast-marquee {
    0% { left: -35%; }
    100% { left: 100%; }
  }
</style>
