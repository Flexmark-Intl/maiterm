<script lang="ts">
  import { open as shellOpen } from '@tauri-apps/plugin-shell';
  import Button from '$lib/components/ui/Button.svelte';
  import { claudeSetupStore, missing, GIT_DOWNLOAD_URL } from '$lib/stores/claudeSetup.svelte';

  const SHELL_NAMES: Record<string, string> = {
    gitbash: 'Git Bash',
    powershell: 'Windows PowerShell',
    pwsh: 'PowerShell 7',
    cmd: 'Command Prompt',
    wsl: 'WSL',
  };

  const report = $derived(claudeSetupStore.report);
  const m = $derived(report ? missing(report) : null);
  const install = $derived(claudeSetupStore.install);
  const allGood = $derived(!!m && !m.git && !m.claude && !m.shell && !m.path && !m.shadow);
  /** Something "Fix everything" can do by itself: Git only installs itself through winget. */
  const canFixAll = $derived(!!report && !!m && ((m.git && report.winget) || m.claude || m.shell || m.path));

  let dialog = $state<HTMLDivElement | null>(null);
  $effect(() => {
    // Focus explicitly: Svelte's autofocus doesn't move focus off a keyboard opener (CLAUDE.md).
    if (dialog) requestAnimationFrame(() => dialog?.focus());
  });

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.preventDefault();
      claudeSetupStore.hide();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="backdrop" onclick={(e) => { if (e.target === e.currentTarget) claudeSetupStore.hide(); }} role="presentation">
  <div class="modal" role="dialog" aria-modal="true" aria-label="Set up Claude on Windows" tabindex="-1" bind:this={dialog}>
    <div class="header">
      <h2>Set up Claude on Windows</h2>
      <button class="close" onclick={() => claudeSetupStore.hide()} aria-label="Close">×</button>
    </div>

    {#if !report}
      <p class="sub">{claudeSetupStore.checking ? 'Checking this computer…' : 'Nothing checked yet.'}</p>
    {:else if !report.windows}
      <p class="sub">This is for Windows. Claude needs nothing extra on this computer.</p>
    {:else}
      <p class="sub">
        Claude runs from any Windows shell, but it uses Git Bash to run commands — and so do maiTerm's
        hooks, which tell maiTerm which tab a Claude session is in. Git Bash is the recommended shell.
      </p>

      <ul class="rows">
        <li class:ok={!m?.git}>
          <span class="mark" aria-hidden="true">{m?.git ? '✗' : '✓'}</span>
          <div class="body">
            <div class="title">Git for Windows</div>
            {#if m?.git}
              <div class="detail">Not installed. Without it Claude falls back to PowerShell and maiTerm's tab tracking, tasks and Overlord don't reach your Claude sessions.</div>
            {:else}
              <div class="detail path">{report.git_bash}</div>
            {/if}
          </div>
          {#if m?.git}
            {#if report.winget}
              <Button onclick={() => claudeSetupStore.runInstall('git')} disabled={claudeSetupStore.busy}>Install</Button>
            {:else}
              <Button onclick={() => shellOpen(GIT_DOWNLOAD_URL)}>Download</Button>
            {/if}
          {/if}
        </li>

        <li class:ok={report.shell === 'gitbash'} class:pending={report.shell !== 'gitbash'}>
          <span class="mark" aria-hidden="true">{report.shell === 'gitbash' ? '✓' : '–'}</span>
          <div class="body">
            <div class="title">Shell for new tabs: {SHELL_NAMES[report.shell] ?? report.shell}</div>
            {#if m?.shell}
              <div class="detail">Git Bash is installed. It runs the same commands as Claude's docs and your Mac or Linux machines.</div>
            {:else if m?.git}
              <div class="detail">New tabs switch to Git Bash once Git for Windows is installed.</div>
            {/if}
          </div>
          {#if m?.shell}
            <Button onclick={() => claudeSetupStore.useGitBash()}>Use Git Bash</Button>
          {/if}
        </li>

        <li class:ok={!m?.claude}>
          <span class="mark" aria-hidden="true">{m?.claude ? '✗' : '✓'}</span>
          <div class="body">
            <div class="title">Claude Code{report.claude_version ? ` ${report.claude_version}` : ''}</div>
            {#if m?.claude}
              <div class="detail">Not installed. Install runs Anthropic's official installer in a new tab.</div>
            {:else}
              <div class="detail path">{report.claude}</div>
            {/if}
          </div>
          {#if m?.claude}
            <Button onclick={() => claudeSetupStore.runInstall('claude')} disabled={claudeSetupStore.busy}>Install</Button>
          {/if}
        </li>

        {#if report.claude}
          <li class:ok={!m?.path}>
            <span class="mark" aria-hidden="true">{m?.path ? '✗' : '✓'}</span>
            <div class="body">
              <div class="title">{m?.path ? '“claude” isn’t on your PATH' : '“claude” is on your PATH'}</div>
              {#if m?.path}
                <div class="detail">Its installer doesn't add it. maiTerm's tabs find it anyway; add it so other terminals do too.</div>
              {/if}
            </div>
            {#if m?.path}
              <Button onclick={() => claudeSetupStore.addToPath()}>Add to PATH</Button>
            {/if}
          </li>
        {/if}

        {#if report.claude_shadowed_by}
          <li class="warn">
            <span class="mark" aria-hidden="true">!</span>
            <div class="body">
              <div class="title">Claude Desktop takes over “claude”</div>
              <div class="detail">
                An older Claude Desktop puts its own Claude.exe first on PATH, so typing <code>claude</code> opens the desktop app.
                Update Claude Desktop, or turn its alias off in Settings › Apps › Advanced app settings › App execution aliases.
              </div>
              <div class="detail path">{report.claude_shadowed_by}</div>
            </div>
          </li>
        {/if}

        {#if report.wsl_version}
          <li class="info">
            <span class="mark" aria-hidden="true">i</span>
            <div class="body">
              <div class="title">WSL {report.wsl_version} is installed</div>
              <div class="detail">
                {#if report.wsl_version === 1}
                  Claude Code doesn't support WSL 1.
                {:else}
                  Claude can run inside WSL too, but it's a separate install there, and maiTerm's Claude integration is set up for Windows itself.
                {/if}
                Use Git Bash here.
              </div>
            </div>
          </li>
        {/if}
      </ul>

      {#if install}
        <div class="running">
          <span class="spinner" aria-hidden="true"></span>
          <span>Installing in the “{install.tabName}” tab. This updates when it's done{install.applyAfter ? ', then sets up the rest' : ''}.</span>
          <button class="link" onclick={() => claudeSetupStore.showInstallTab()}>Show tab</button>
        </div>
      {/if}

      {#if claudeSetupStore.error}
        <p class="error">{claudeSetupStore.error}</p>
      {/if}
    {/if}

    <div class="footer">
      {#if report?.windows}
        <Button variant="ghost" onclick={() => claudeSetupStore.refresh()} disabled={claudeSetupStore.checking}>
          {claudeSetupStore.checking ? 'Checking…' : 'Check again'}
        </Button>
      {/if}
      <span class="spacer"></span>
      {#if allGood}
        <Button variant="primary" onclick={() => claudeSetupStore.startClaude()}>Start Claude in a new tab</Button>
      {:else if canFixAll}
        <Button variant="primary" onclick={() => claudeSetupStore.fixEverything()} disabled={claudeSetupStore.busy}>Fix everything</Button>
      {/if}
      <Button variant="secondary" onclick={() => claudeSetupStore.hide()}>Close</Button>
    </div>
  </div>
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1100;
  }

  .modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 10px;
    width: 560px;
    max-width: calc(100vw - 48px);
    max-height: calc(100vh - 48px);
    overflow-y: auto;
    padding: 20px 22px 16px;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
    outline: none;
  }

  .header {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .header h2 {
    font-size: 1.154rem;
    font-weight: 600;
    color: var(--fg);
    margin: 0;
  }

  .close {
    background: none;
    border: none;
    color: var(--fg-dim);
    font-size: 1.4rem;
    cursor: pointer;
    line-height: 1;
  }

  .close:hover { color: var(--fg); }

  .sub {
    margin: 8px 0 14px;
    font-size: 0.923rem;
    color: var(--fg-dim);
    line-height: 1.45;
  }

  .rows {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .rows li {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 10px 12px;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 8px;
  }

  .mark {
    width: 18px;
    flex-shrink: 0;
    text-align: center;
    font-weight: 700;
    color: var(--error, #f7768e);
  }

  .rows li.ok .mark { color: var(--success, #9ece6a); }
  .rows li.warn .mark { color: var(--warning, #e0af68); }
  .rows li.info .mark { color: var(--accent); }
  .rows li.pending .mark { color: var(--fg-dim); }

  .body {
    flex: 1;
    min-width: 0;
  }

  .title {
    color: var(--fg);
    font-weight: 600;
  }

  .detail {
    margin-top: 3px;
    font-size: 0.846rem;
    color: var(--fg-dim);
    line-height: 1.4;
    overflow-wrap: anywhere;
  }

  .detail.path {
    font-family: monospace;
  }

  code {
    font-family: monospace;
    color: var(--fg);
  }

  .running {
    display: flex;
    align-items: center;
    gap: 10px;
    margin-top: 12px;
    font-size: 0.846rem;
    color: var(--fg);
  }

  .spinner {
    width: 14px;
    height: 14px;
    border-radius: 50%;
    border: 2px solid var(--bg-light);
    border-top-color: var(--accent);
    animation: spin 0.7s linear infinite;
    flex-shrink: 0;
  }

  @keyframes spin {
    to { transform: rotate(360deg); }
  }

  .link {
    background: none;
    border: none;
    color: var(--accent);
    cursor: pointer;
    padding: 0;
    white-space: nowrap;
  }

  .error {
    margin: 12px 0 0;
    font-size: 0.846rem;
    color: var(--error, #f7768e);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }

  .footer {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 16px;
  }

  .spacer { flex: 1; }
</style>
