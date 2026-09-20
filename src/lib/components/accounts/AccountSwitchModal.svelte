<script lang="ts">
  /** Shown after switching the active account (docs/login.md §10).
   *
   *  **Why a modal and not a notice.** Switching is the one control in Preferences that does not
   *  take effect immediately — the account is an environment variable handed to the shell at
   *  exec, so a running tab cannot be moved, only respawned. A line of text under the account
   *  list was too quiet for something that surprising, and it left the obvious next question
   *  ("so how do I move them?") unanswered.
   *
   *  So this states the constraint and then offers the remedy at the granularity people actually
   *  think in: everything, a window, or one workspace. Closing without reloading is a first-class
   *  outcome — most of the time the right move is to leave running agents alone and let the next
   *  tab pick the new account up.
   *
   *  Only tabs with a live PTY are counted or touched. A suspended or never-opened tab already
   *  starts under the active account, so reloading it would spawn shells nobody asked for. */
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { listen, type UnlistenFn } from '@tauri-apps/api/event';
  import Button from '$lib/components/ui/Button.svelte';
  import * as commands from '$lib/tauri/commands';
  import type { AccountReloadWindow, AccountReloadDone } from '$lib/tauri/commands';

  interface Props {
    /** Headline — what just changed. */
    title: string;
    /** One sentence on what it means for tabs opened from now on. */
    subtitle: string;
    /** What new tabs will use, named so the "nothing is running" case can be specific. */
    destination: string;
    onclose: () => void;
  }

  let { title, subtitle, destination, onclose }: Props = $props();

  let windows = $state<AccountReloadWindow[]>([]);
  let loading = $state(true);
  let error = $state<string | null>(null);
  let panelEl = $state<HTMLDivElement | null>(null);

  /** Per-target progress, keyed by window id or workspace id. `pending` until the window that
   *  did the work reports back — there is no other completion signal, because the live tab
   *  counts look identical afterwards (the replacements are live too). */
  type Progress = { state: 'pending'; requestId: string } | { state: 'done'; reloaded: number };
  let progress = $state<Record<string, Progress>>({});

  $effect(() => {
    let unlisten: UnlistenFn | null = null;
    let dead = false;
    void (async () => {
      const fn = await listen<AccountReloadDone>(commands.ACCOUNT_RELOAD_DONE_EVENT, e => {
        const { request_id, reloaded } = e.payload;
        for (const [key, p] of Object.entries(progress)) {
          if (p.state === 'pending' && p.requestId === request_id) {
            progress = { ...progress, [key]: { state: 'done', reloaded } };
          }
        }
      });
      if (dead) void fn();
      else unlisten = fn;
    })();
    return () => {
      dead = true;
      void unlisten?.();
    };
  });

  $effect(() => {
    const id = requestAnimationFrame(() => panelEl?.focus());
    return () => cancelAnimationFrame(id);
  });

  $effect(() => {
    void (async () => {
      try {
        windows = await commands.accountReloadTargets();
      } catch (e) {
        error = e instanceof Error ? e.message : String(e);
      } finally {
        loading = false;
      }
    })();
  });

  /** Windows that actually have something to reload. A window whose tabs are all suspended is
   *  not a choice worth rendering — it would offer an action that does nothing. */
  const live = $derived(
    windows
      .map(w => ({ ...w, workspaces: w.workspaces.filter(ws => ws.live_tabs > 0) }))
      .filter(w => w.workspaces.length > 0),
  );

  const totalTabs = $derived(
    live.reduce((n, w) => n + w.workspaces.reduce((m, ws) => m + ws.live_tabs, 0), 0),
  );

  function windowTitle(w: AccountReloadWindow): string {
    return w.name ?? (w.label === 'main' ? 'Main window' : w.label);
  }

  async function reload(w: AccountReloadWindow, workspaceIds: string[] | undefined, key: string) {
    error = null;
    const requestId = crypto.randomUUID();
    progress = { ...progress, [key]: { state: 'pending', requestId } };
    try {
      await commands.requestAccountReload(
        w.label,
        workspaceIds,
        getCurrentWindow().label,
        requestId,
      );
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      // Drop the pending marker, or the row claims work that was never started.
      const { [key]: _dropped, ...rest } = progress;
      progress = rest;
    }
  }

  async function reloadEverything() {
    for (const w of live) {
      await reload(w, undefined, `w:${w.window_id}`);
    }
  }

  /** A workspace row follows its own request, or its window's if that is what was asked. */
  function rowProgress(wKey: string, wsKey: string): Progress | null {
    return progress[wsKey] ?? progress[wKey] ?? null;
  }

  function handleKeydown(e: KeyboardEvent) {
    const closeKey = e.key === 'Escape' || (e.key.toLowerCase() === 'w' && (e.metaKey || e.ctrlKey));
    if (!closeKey) return;
    // Swallowed so the preferences window does not close underneath this dialog.
    e.stopPropagation();
    e.preventDefault();
    onclose();
  }

  function handleBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) onclose();
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_tabindex -->
<div
  class="backdrop"
  onclick={handleBackdropClick}
  onkeydown={handleKeydown}
  role="dialog"
  aria-modal="true"
  aria-label="Account switched"
  tabindex="-1"
>
  <div class="panel" bind:this={panelEl} tabindex="-1">
    <div class="header">
      <div class="title">{title}</div>
      <div class="subtitle">{subtitle}</div>
    </div>

    <div class="body">
      <section>
        <p>
          <strong>Tabs already running keep the account they started with.</strong> An account is
          an environment variable handed to the shell when a tab starts, and that cannot be
          changed from outside once it is running — so the only way to move a tab across is to
          respawn its shell. This is true of turning the feature on and off as well as of
          switching between accounts.
        </p>
        <p class="hint">
          Reloading keeps the tab, its name, directory and scrollback, and restarts the shell
          inside it. An agent mid-turn in that tab is interrupted, so leaving them be and letting
          your next tab pick the account up is often the better answer.
        </p>
      </section>

      {#if loading}
        <p class="hint">Looking for running tabs…</p>
      {:else if totalTabs === 0}
        <section>
          <h4>Nothing is running under the old setting</h4>
          <p class="hint">
            No tab currently has a live shell, so there is nothing to move — every tab will start
            under {destination}.
          </p>
        </section>
      {:else}
        <section>
          <div class="section-head">
            <h4>{totalTabs} running tab{totalTabs === 1 ? '' : 's'}</h4>
            <Button variant="secondary" onclick={reloadEverything}>Reload all</Button>
          </div>
          {#each live as w (w.window_id)}
            {@const wKey = `w:${w.window_id}`}
            <div class="win">
              <div class="win-head">
                <span class="win-name">{windowTitle(w)}</span>
                {#if progress[wKey]?.state === 'pending'}
                  <span class="done">Reloading…</span>
                {:else if progress[wKey]?.state === 'done'}
                  <span class="done">Reloaded</span>
                {:else}
                  <Button variant="ghost" onclick={() => reload(w, undefined, wKey)}>
                    Reload window
                  </Button>
                {/if}
              </div>
              {#each w.workspaces as ws (ws.id)}
                {@const wsKey = `ws:${ws.id}`}
                {@const p = rowProgress(wKey, wsKey)}
                <div class="ws">
                  <span class="ws-name">
                    {ws.name}
                    <span class="count">
                      {ws.live_tabs} tab{ws.live_tabs === 1 ? '' : 's'}
                    </span>
                  </span>
                  {#if p?.state === 'pending'}
                    <span class="done">Reloading…</span>
                  {:else if p?.state === 'done'}
                    <!-- The count is what the window actually reloaded, not what we asked for. -->
                    <span class="done">Reloaded {p.reloaded}</span>
                  {:else}
                    <Button variant="ghost" onclick={() => reload(w, [ws.id], wsKey)}>
                      Reload
                    </Button>
                  {/if}
                </div>
              {/each}
            </div>
          {/each}
        </section>
      {/if}

      {#if error}<p class="error">{error}</p>{/if}
    </div>

    <div class="footer">
      <Button variant="primary" onclick={onclose}>Close</Button>
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
    padding-top: 10vh;
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
    max-height: 78vh;
    width: 520px;
  }

  .header {
    border-bottom: 1px solid var(--bg-light);
    padding: 12px 14px 10px;
  }

  .title {
    color: var(--fg);
    font-size: 1rem;
    font-weight: 600;
    overflow-wrap: anywhere;
  }

  .subtitle {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.4;
    margin-top: 3px;
  }

  .body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    overflow-y: auto;
    padding: 14px;
  }

  h4 {
    color: var(--fg);
    font-size: 0.85rem;
    font-weight: 600;
    margin: 0;
  }

  .section-head {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: space-between;
    margin-bottom: 8px;
  }

  p {
    color: var(--fg-dim);
    font-size: 0.8rem;
    line-height: 1.5;
    margin: 0 0 6px;
    overflow-wrap: anywhere;
  }

  p:last-child {
    margin-bottom: 0;
  }

  .hint {
    font-size: 0.75rem;
  }

  .win {
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    margin-bottom: 8px;
    overflow: hidden;
  }

  .win:last-child {
    margin-bottom: 0;
  }

  .win-head {
    align-items: center;
    background: var(--bg-dark);
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: space-between;
    padding: 6px 10px;
  }

  .win-name {
    color: var(--fg);
    font-size: 0.8rem;
    font-weight: 600;
    /* Window and workspace names are user-chosen and routinely longer than this panel. */
    overflow-wrap: anywhere;
  }

  .ws {
    align-items: center;
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: space-between;
    padding: 5px 10px 5px 18px;
  }

  .ws-name {
    color: var(--fg-dim);
    font-size: 0.8rem;
    overflow-wrap: anywhere;
  }

  .count {
    color: var(--fg-dim);
    font-size: 0.7rem;
    opacity: 0.8;
    padding-left: 6px;
  }

  .done {
    color: var(--accent);
    font-size: 0.75rem;
    padding-right: 6px;
  }

  .error {
    background: rgba(220, 80, 80, 0.12);
    border-radius: 4px;
    color: #e06c75;
    padding: 8px;
  }

  .footer {
    border-top: 1px solid var(--bg-light);
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 10px 14px;
  }
</style>
