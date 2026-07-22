<script lang="ts">
  import { preferencesStore } from '$lib/stores/preferences.svelte';
  import { updaterStore } from '$lib/stores/updater.svelte';
  import type { CursorStyle, Trigger, TriggerActionType, TriggerActionEntry, VariableMapping, TabStateName } from '$lib/tauri/types';
  import { builtinThemes, getTheme, isBuiltinTheme } from '$lib/themes';
  import ThemeEditor from '$lib/components/ThemeEditor.svelte';
  import ResizableTextarea from '$lib/components/ResizableTextarea.svelte';
  import Tooltip from '$lib/components/Tooltip.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { modLabel, altLabel, isModKey, isMac } from '$lib/utils/platform';
  import { getAllWorkspaces, getAllTabs, listSystemSounds, playSystemSound, detectWindowsShells, exportState, importState, pickBackupDirectory, backupFilename, previewImport, checkFullDiskAccess, openFullDiskAccessSettings, mailinkCreatePairing, mailinkListDevices, mailinkRemoveDevice, commsTestConnection } from '$lib/tauri/commands';
  import type { ImportPreview } from '$lib/tauri/commands';
  import qrcode from 'qrcode-generator';
  import ImportPreviewModal from '$lib/components/ImportPreviewModal.svelte';
  import { open as dialogOpen, save as dialogSave } from '@tauri-apps/plugin-dialog';

  import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
  import type { ShellInfo, MailinkDevice, MailinkPairingPayload } from '$lib/tauri/types';
  import { tick, onMount } from 'svelte';
  import { slide } from 'svelte/transition';
  import { getCurrentWindow } from '@tauri-apps/api/window';
  import { getVersion } from '@tauri-apps/api/app';
  import { DEFAULT_TRIGGERS, seedDefaultTriggers as seedDefaults } from '$lib/triggers/defaults';
  import { resolveMatchMode } from '$lib/stores/triggers.svelte';
  import { parseCondition } from '$lib/triggers/variableCondition';
  import type { MatchMode } from '$lib/tauri/types';

  /** Restore a default trigger to its template values (keeps enabled, workspaces, id). */
  function restoreDefault(trigger: Trigger) {
    if (!trigger.default_id) return;
    const tmpl = DEFAULT_TRIGGERS[trigger.default_id];
    if (!tmpl) return;
    updateTrigger(trigger.id, {
      name: tmpl.name,
      description: tmpl.description ?? null,
      pattern: tmpl.pattern,
      cooldown: tmpl.cooldown,
      plain_text: tmpl.plain_text,
      match_mode: tmpl.match_mode ?? null,
      actions: structuredClone(tmpl.actions),
      variables: structuredClone(tmpl.variables),
      user_modified: false,
    });
  }

  // Workspace list for trigger scope multiselect
  let allWorkspaces = $state<{ id: string; name: string }[]>([]);
  // Tab list for trigger scope multiselect
  let allTabs = $state<{ id: string; name: string; workspaceId: string; workspaceName: string; isActive: boolean }[]>([]);
  // System sounds for notification sound picker
  let systemSounds = $state<string[]>([]);
  // Available Windows shells (empty on non-Windows)
  let windowsShells = $state<ShellInfo[]>([]);
  onMount(async () => {
    try {
      const pairs = await getAllWorkspaces();
      allWorkspaces = pairs.map(([id, name]) => ({ id, name }));
      const tabRows = await getAllTabs();
      allTabs = tabRows.map(([id, name, workspaceId, workspaceName, isActive]) => ({ id, name, workspaceId, workspaceName, isActive }));
    } catch { /* preferences may open before main window */ }

    try {
      systemSounds = await listSystemSounds();
    } catch { /* sound listing may fail on some platforms */ }

    try {
      windowsShells = await detectWindowsShells();
    } catch { /* shell detection may fail */ }

    // Wait for preferences to finish loading before seeding defaults
    await preferencesStore.ready;
    seedDefaultTriggers();
  });

  function seedDefaultTriggers() {
    const result = seedDefaults(
      preferencesStore.triggers,
      preferencesStore.hiddenDefaultTriggers,
    );
    if (result) preferencesStore.setTriggers(result);
  }

  const sectionIds = ['appearance', 'terminal', 'ui', 'tabs', 'workspace', 'notes', 'notifications', 'triggers', 'claude_code', 'integrations', 'backup', 'updates', 'permissions'] as const;
  type SectionId = typeof sectionIds[number];
  const saved = localStorage.getItem('prefs-section');
  let activeSection = $state<SectionId>(
    saved && sectionIds.includes(saved as SectionId) ? saved as SectionId : 'appearance'
  );
  $effect(() => { localStorage.setItem('prefs-section', activeSection); });

  // ─── Comms integration (Mattermost) test-connection state ──────────────────
  let commsTestStatus = $state('');
  let commsTestOk = $state<boolean | null>(null);
  let commsTesting = $state(false);

  async function handleCommsTest() {
    commsTesting = true;
    commsTestStatus = '';
    commsTestOk = null;
    try {
      const result = await commsTestConnection(
        preferencesStore.commsServerUrl,
        preferencesStore.commsBotToken
      );
      commsTestOk = true;
      commsTestStatus = `Connected — bot account @${result.bot_username}`;
    } catch (e) {
      commsTestOk = false;
      commsTestStatus = String(e);
    } finally {
      commsTesting = false;
    }
  }

  // ─── maiLink pairing & paired devices ──────────────────────────────────────
  let mailinkDevices = $state<MailinkDevice[]>([]);
  let mailinkDevicesLoaded = $state(false);
  let pairing = $state<MailinkPairingPayload | null>(null);
  let pairingQrSvg = $state('');
  let pairingError = $state('');
  let pairingRemaining = $state(0); // seconds left on the one-time code (0 = expired)
  let pairingBusy = $state(false);
  let revokeConfirmId = $state<string | null>(null);

  async function loadMailinkDevices() {
    try {
      mailinkDevices = await mailinkListDevices();
    } catch (e) {
      logError(`[maiLink] list devices failed: ${e}`);
    } finally {
      mailinkDevicesLoaded = true;
    }
  }

  // Refresh the device list whenever the maiLink section is open and the bridge is enabled.
  $effect(() => {
    if (activeSection === 'claude_code' && preferencesStore.mailinkEnabled) {
      void loadMailinkDevices();
    }
  });

  async function startPairing() {
    pairingBusy = true;
    pairingError = '';
    try {
      const payload = await mailinkCreatePairing();
      const qr = qrcode(0, 'M');
      qr.addData(JSON.stringify(payload));
      qr.make();
      pairingQrSvg = qr.createSvgTag({ cellSize: 6, margin: 2, scalable: true });
      pairing = payload;
      pairingRemaining = 120; // matches the backend code TTL
    } catch (e) {
      pairingError = `${e}`;
      logError(`[maiLink] create pairing failed: ${e}`);
    } finally {
      pairingBusy = false;
    }
  }

  function closePairing() {
    pairing = null;
    pairingQrSvg = '';
    // A scan may have completed while the modal was open — refresh so the new device shows.
    void loadMailinkDevices();
  }

  function onPairingBackdropClick(e: MouseEvent) {
    if (e.target === e.currentTarget) closePairing();
  }
  function onPairingKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') closePairing();
  }

  // Tick the code countdown. Depends only on `pairing`, so it starts on open and the cleanup
  // clears it on close; reads of pairingRemaining happen inside the timer (untracked).
  $effect(() => {
    if (!pairing) return;
    const t = setInterval(() => {
      pairingRemaining = Math.max(0, pairingRemaining - 1);
      if (pairingRemaining <= 0) clearInterval(t);
    }, 1000);
    return () => clearInterval(t);
  });

  async function revokeDevice(id: string) {
    try {
      await mailinkRemoveDevice(id);
      revokeConfirmId = null;
      await loadMailinkDevices();
    } catch (e) {
      logError(`[maiLink] remove device failed: ${e}`);
    }
  }

  function devicePlatformLabel(d: MailinkDevice): string {
    if (!d.push_platform) return 'no push registered';
    const base = d.push_platform === 'apns' ? 'iOS' : d.push_platform === 'fcm' ? 'Android' : d.push_platform;
    return d.push_env ? `${base} · ${d.push_env}` : base;
  }

  function fmtDeviceTime(ms: number): string {
    if (!ms) return '—';
    try { return new Date(ms).toLocaleString(); } catch { return '—'; }
  }

  const sections = [
    { id: 'appearance' as const, label: 'Appearance' },
    { id: 'terminal' as const, label: 'Terminal' },
    { id: 'ui' as const, label: 'Scrollback' },
    { id: 'tabs' as const, label: 'Tabs' },
    { id: 'workspace' as const, label: 'Workspace' },
    { id: 'notes' as const, label: 'Notes' },
    { id: 'notifications' as const, label: 'Notifications' },
    { id: 'triggers' as const, label: 'Triggers' },
    { id: 'claude_code' as const, label: 'AI Agents' },
    { id: 'integrations' as const, label: 'Integrations' },
    { id: 'backup' as const, label: 'Backup' },
    { id: 'updates' as const, label: 'Updates' },
    ...isMac() ? [{ id: 'permissions' as const, label: 'Permissions' }] : [],
  ];

  let appVersion = $state('');
  getVersion().then(v => { appVersion = v; });

  let fdaGranted = $state<boolean | null>(null);
  if (isMac()) {
    checkFullDiskAccess().then(v => { fdaGranted = v; });
  }

  let backupStatus = $state<string | null>(null);
  let importPreview = $state<ImportPreview | null>(null);
  let importFilePath = $state('');
  let showImportPreview = $state(false);

  async function handleExportState() {
    try {
      const excludeScrollback = preferencesStore.backupExcludeScrollback;
      const path = await dialogSave({
        defaultPath: backupFilename(),
        filters: [{ name: 'Compressed JSON', extensions: ['gz'] }],
      });
      if (path) {
        await exportState(path, excludeScrollback);
        backupStatus = 'State exported successfully.';
        setTimeout(() => { backupStatus = null; }, 3000);
      }
    } catch (e) {
      backupStatus = `Export failed: ${e}`;
      logError(`Export state failed: ${e}`);
    }
  }

  async function handleImportState() {
    try {
      const path = await dialogOpen({
        multiple: false,
        filters: [{ name: 'maiTerm Backup', extensions: ['json', 'gz'] }],
      });
      if (typeof path === 'string') {
        const preview = await previewImport(path);
        importPreview = preview;
        importFilePath = path;
        showImportPreview = true;
      }
    } catch (e) {
      backupStatus = `Import failed: ${e}`;
      logError(`Import state failed: ${e}`);
    }
  }

  async function handlePickDirectory() {
    try {
      const dir = await pickBackupDirectory();
      if (dir) {
        await preferencesStore.setBackupDirectory(dir);
      }
    } catch (e) {
      logError(`Pick backup directory failed: ${e}`);
    }
  }

  let expandedTriggerId = $state<string | null>(null);
  let wsSearchQueries = $state(new Map<string, string>());
  let wsShowSelected = $state(new Map<string, boolean>());
  let tabSearchQueries = $state(new Map<string, string>());
  let tabShowSelected = $state(new Map<string, boolean>());

  function addTrigger() {
    const trigger: Trigger = {
      id: crypto.randomUUID(),
      name: '',
      description: null,
      pattern: '',
      actions: [],
      enabled: false,
      workspaces: [],
      tabs: [],
      cooldown: 5,
      variables: [],
      plain_text: false,
      match_mode: 'regex',
    };
    preferencesStore.setTriggers([...preferencesStore.triggers, trigger]);
    expandedTriggerId = trigger.id;
    tick().then(() => {
      const el = document.querySelector<HTMLInputElement>(`.trigger-card [data-trigger-name="${trigger.id}"]`);
      el?.focus();
    });
  }

  function updateTrigger(id: string, patch: Partial<Trigger>) {
    if (patch.variables) {
      patch.variables = [...patch.variables].sort((a, b) => a.group - b.group);
    }
    const updated = preferencesStore.triggers.map(t => {
      if (t.id !== id) return t;
      const merged = { ...t, ...patch };
      // Mark default triggers as user-modified when content changes (not just enabled toggle)
      if (merged.default_id && !('user_modified' in patch)) {
        const isContentChange = Object.keys(patch).some(k => k !== 'enabled');
        if (isContentChange) {
          merged.user_modified = true;
        }
      }
      return merged;
    });
    preferencesStore.setTriggers(updated);
  }

  let confirmDeleteId = $state<string | null>(null);

  function deleteTrigger(id: string) {
    const trigger = preferencesStore.triggers.find(t => t.id === id);
    if (trigger && (trigger.name || trigger.pattern)) {
      confirmDeleteId = id;
      return;
    }
    doDeleteTrigger(id);
  }

  function restoreAllDefaults() {
    preferencesStore.setHiddenDefaultTriggers([]);
    seedDefaultTriggers();
  }

  function doDeleteTrigger(id: string) {
    confirmDeleteId = null;
    const trigger = preferencesStore.triggers.find(t => t.id === id);
    // Track deleted defaults so they don't get re-seeded
    if (trigger?.default_id) {
      preferencesStore.setHiddenDefaultTriggers([...preferencesStore.hiddenDefaultTriggers, trigger.default_id]);
    }
    preferencesStore.setTriggers(preferencesStore.triggers.filter(t => t.id !== id));
    if (expandedTriggerId === id) expandedTriggerId = null;
  }

  function isValidRegex(pattern: string): boolean {
    if (!pattern) return true;
    try { new RegExp(pattern); return true; } catch { return false; }
  }

  function isValidCondition(pattern: string): boolean {
    if (!pattern) return true;
    try { parseCondition(pattern); return true; } catch { return false; }
  }

  /** Count capture groups in a regex pattern (0 if invalid). */
  function countCaptureGroups(pattern: string): number {
    if (!pattern) return 0;
    try {
      // Match the pattern against empty string — result.length - 1 = number of groups
      const re = new RegExp(pattern + '|');
      const m = ''.match(re);
      return m ? m.length - 1 : 0;
    } catch { return 0; }
  }

  /** Validate variable name. Returns error message or empty string if valid. */
  function varNameError(name: string): string {
    if (!name) return 'Name is required';
    if (/^\d/.test(name)) return 'Cannot start with a digit';
    if (/[^\w]/.test(name)) return 'Only letters, digits, and _';
    return '';
  }

  const fontFamilies = [
    'Menlo',
    'Monaco',
    'SF Mono',
    'JetBrains Mono',
    'Fira Code',
    'Consolas',
  ];

  const autoSaveOptions = [
    { value: 0, label: 'Disabled' },
    { value: 5, label: '5 seconds' },
    { value: 10, label: '10 seconds' },
    { value: 30, label: '30 seconds' },
    { value: 60, label: '60 seconds' },
  ];

  const defaultPromptPatterns = [
    '\\u@\\h:\\d\\p',
    '\\h \\u[\\d]\\p',
    '[\\u@\\h \\d]\\p',
    'PS \\d>',
    '\\d>',
  ];

  const scrollbackOptions = [
    { value: 1000, label: '1,000 lines' },
    { value: 5000, label: '5,000 lines' },
    { value: 10000, label: '10,000 lines' },
    { value: 0, label: 'Unlimited' },
  ];

  const allThemes = $derived([...builtinThemes, ...preferencesStore.customThemes]);

  const selectedTheme = $derived(
    getTheme(preferencesStore.theme, preferencesStore.customThemes)
  );

  function createNewTheme() {
    const source = selectedTheme;
    const newTheme = {
      ...structuredClone(source),
      id: `custom-${crypto.randomUUID()}`,
      name: `Custom ${source.name}`,
    };
    preferencesStore.addCustomTheme(newTheme);
    preferencesStore.setTheme(newTheme.id);
  }

  function handleKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      getCurrentWindow().close();
    }
    // Cmd+W - close preferences window
    if (isModKey(e) && e.key.toLowerCase() === 'w') {
      e.preventDefault();
      getCurrentWindow().close();
    }
  }
</script>

<svelte:window onkeydown={handleKeydown} />

<div class="window">
  <div class="titlebar">
    <span class="title">Preferences</span>
  </div>

  <div class="body">
    <nav class="sidebar">
      {#each sections as section (section.id)}
        <button
          class="sidebar-item"
          class:active={activeSection === section.id}
          onclick={() => activeSection = section.id}
        >
          {section.label}
        </button>
      {/each}
    </nav>

    <div class="section-content">
      {#if activeSection === 'appearance'}
        <h3 class="section-heading">UI Font Size</h3>

        <div class="setting">
          <label for="ui-font-size">Size</label>
          <div class="number-input-wrapper">
            <button class="number-btn" onclick={() => preferencesStore.setUiFontSize(preferencesStore.uiFontSize - 1)}>−</button>
            <input
              type="number"
              id="ui-font-size"
              class="number-input"
              min="10"
              max="20"
              value={preferencesStore.uiFontSize}
              onchange={(e) => preferencesStore.setUiFontSize(parseInt(e.currentTarget.value) || 13)}
            />
            <button class="number-btn" onclick={() => preferencesStore.setUiFontSize(preferencesStore.uiFontSize + 1)}>+</button>
          </div>
        </div>

        <h3 class="section-heading">Theme</h3>
        <div class="theme-grid">
          {#each allThemes as t (t.id)}
            <div
              class="theme-swatch"
              class:active={preferencesStore.theme === t.id}
              onclick={() => preferencesStore.setTheme(t.id)}
              onkeydown={(e) => { if (e.key === 'Enter' || e.key === ' ') preferencesStore.setTheme(t.id); }}
              role="button"
              tabindex="0"
              title={t.name}
            >
              <div class="swatch-colors">
                <span class="swatch-bar" style:background={t.terminal.background}></span>
                <span class="swatch-bar" style:background={t.terminal.red}></span>
                <span class="swatch-bar" style:background={t.terminal.green}></span>
                <span class="swatch-bar" style:background={t.terminal.yellow}></span>
                <span class="swatch-bar" style:background={t.terminal.blue}></span>
                <span class="swatch-bar" style:background={t.terminal.magenta}></span>
                <span class="swatch-bar" style:background={t.terminal.cyan}></span>
              </div>
              <span class="swatch-label">{t.name}</span>
              {#if !isBuiltinTheme(t.id)}
                <button
                  class="swatch-delete"
                  onclick={(e) => { e.stopPropagation(); preferencesStore.deleteCustomTheme(t.id); }}
                  title="Delete custom theme"
                >&times;</button>
              {/if}
            </div>
          {/each}
          <button class="theme-swatch new-theme" onclick={createNewTheme} title="Create new theme from current">
            <div class="new-theme-icon">+</div>
            <span class="swatch-label">New Theme</span>
          </button>
        </div>

        <ThemeEditor theme={selectedTheme} />
      {:else if activeSection === 'terminal'}
        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="restore-session">Restore on Relaunch</label>
            <p class="setting-hint">
              Restore working directory and SSH sessions when the app restarts.
            </p>
          </div>
          <button
            id="restore-session"
            class="toggle"
            class:active={preferencesStore.restoreSession}
            onclick={() => preferencesStore.setRestoreSession(!preferencesStore.restoreSession)}
            aria-pressed={preferencesStore.restoreSession}
            aria-label="Toggle session restore on relaunch"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        {#if preferencesStore.restoreSession}
          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="session-restore-mode">Restore Scope</label>
              <p class="setting-hint">
                <strong>All workspaces</strong> respawns and auto-resumes every workspace's active tab on launch, so a crash, update, or quit/relaunch comes back exactly as it was. <strong>Last active only</strong> restores just the last-active workspace and leaves the rest suspended until you open them. (A window reload always reattaches to still-running terminals.)
              </p>
            </div>
            <select
              id="session-restore-mode"
              value={preferencesStore.sessionRestoreMode}
              onchange={(e) => preferencesStore.setSessionRestoreMode(e.currentTarget.value)}
            >
              <option value="all">All workspaces</option>
              <option value="last_active">Last active only</option>
            </select>
          </div>
        {/if}

        <div class="setting">
          <label for="font-size">Font Size</label>
          <div class="number-input-wrapper">
            <button class="number-btn" onclick={() => preferencesStore.setFontSize(preferencesStore.fontSize - 1)}>−</button>
            <input
              type="number"
              id="font-size"
              class="number-input"
              min="10"
              max="24"
              value={preferencesStore.fontSize}
              onchange={(e) => preferencesStore.setFontSize(parseInt(e.currentTarget.value) || 13)}
            />
            <button class="number-btn" onclick={() => preferencesStore.setFontSize(preferencesStore.fontSize + 1)}>+</button>
          </div>
        </div>

        <div class="setting">
          <label for="font-family">Font Family</label>
          <select
            id="font-family"
            value={preferencesStore.fontFamily}
            onchange={(e) => preferencesStore.setFontFamily(e.currentTarget.value)}
          >
            {#each fontFamilies as font}
              <option value={font}>{font}</option>
            {/each}
          </select>
        </div>

        <div class="setting">
          <span class="label-text">Cursor Style</span>
          <div class="radio-group">
            {#each ['block', 'underline', 'bar'] as style}
              <label class="radio-label">
                <input
                  type="radio"
                  name="cursor-style"
                  value={style}
                  checked={preferencesStore.cursorStyle === style}
                  onchange={() => preferencesStore.setCursorStyle(style as CursorStyle)}
                />
                {style.charAt(0).toUpperCase() + style.slice(1)}
              </label>
            {/each}
          </div>
        </div>

        <div class="setting">
          <label for="cursor-blink">Cursor Blink</label>
          <button
            id="cursor-blink"
            class="toggle"
            class:active={preferencesStore.cursorBlink}
            onclick={() => preferencesStore.setCursorBlink(!preferencesStore.cursorBlink)}
            aria-pressed={preferencesStore.cursorBlink}
            aria-label="Toggle cursor blink"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="composer-default-open">Open Composer by Default</label>
            <p class="setting-hint">
              Shows the multi-line input dock at the bottom of terminal tabs.
              Tabs where you've toggled the composer keep their own state.
            </p>
          </div>
          <button
            id="composer-default-open"
            class="toggle"
            class:active={preferencesStore.composerDefaultOpen}
            onclick={() => preferencesStore.setComposerDefaultOpen(!preferencesStore.composerDefaultOpen)}
            aria-pressed={preferencesStore.composerDefaultOpen}
            aria-label="Toggle composer open by default"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <h3 class="section-heading" style="margin-top: 20px;">Rendering</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="terminal-renderer">Renderer</label>
            <p class="setting-hint">
              <strong>DOM</strong> is the default and avoids the rendering artifacts
              (red diff-stripes, smeared input while typing during heavy output) that
              the GPU-accelerated <strong>Canvas</strong> renderer leaves on this
              terminal. Canvas is kept for side-by-side comparison. Applies immediately
              to visible terminals.
            </p>
          </div>
          <select
            id="terminal-renderer"
            value={preferencesStore.terminalRenderer}
            onchange={(e) => preferencesStore.setTerminalRenderer(e.currentTarget.value)}
          >
            <option value="dom">DOM (default)</option>
            <option value="canvas">Canvas (GPU)</option>
          </select>
        </div>

        {#if windowsShells.length > 0}
          <h3 class="section-heading" style="margin-top: 20px;">Default Shell</h3>
          <p class="section-desc">
            Shell used when opening new terminal tabs. Changes apply to new terminals only.
          </p>
          <div class="setting">
            <label for="windows-shell">Shell</label>
            <select
              id="windows-shell"
              value={preferencesStore.windowsShell}
              onchange={(e) => preferencesStore.setWindowsShell(e.currentTarget.value)}
            >
              {#each windowsShells as shell}
                <option value={shell.id}>{shell.name}</option>
              {/each}
            </select>
          </div>
          {#if windowsShells.find(s => s.id === preferencesStore.windowsShell)}
            <p class="setting-hint" style="margin-top: -8px; margin-bottom: 8px;">
              {windowsShells.find(s => s.id === preferencesStore.windowsShell)?.path}
            </p>
          {/if}
        {/if}

        <h3 class="section-heading" style="margin-top: 20px;">Shell Integration</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="shell-title">Auto-set Tab Title</label>
            <p class="setting-hint">
              Updates tab title with user@host:path on each prompt.
              Applies to new terminals only.
            </p>
          </div>
          <button
            id="shell-title"
            class="toggle"
            class:active={preferencesStore.shellTitleIntegration}
            onclick={() => preferencesStore.setShellTitleIntegration(!preferencesStore.shellTitleIntegration)}
            aria-pressed={preferencesStore.shellTitleIntegration}
            aria-label="Toggle shell title integration"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="shell-integration">Command Tracking</label>
            <p class="setting-hint">
              Detect when commands start, finish, and their exit status. Powers completion
              indicators on inactive tabs and detection of dropped SSH sessions.
              Applies to new terminals only.
            </p>
          </div>
          <button
            id="shell-integration"
            class="toggle"
            class:active={preferencesStore.shellIntegration}
            onclick={() => preferencesStore.setShellIntegration(!preferencesStore.shellIntegration)}
            aria-pressed={preferencesStore.shellIntegration}
            aria-label="Toggle shell integration"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <h3 class="section-heading" style="margin-top: 20px;">File Links</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="file-link-action">Click Behavior</label>
            <p class="setting-hint">
              How clicking detected file paths and <code>l</code> command links behaves.
              {preferencesStore.fileLinkAction === 'modifier_click' ? `Hold ${modLabel}+Click to open.` : ''}
              {preferencesStore.fileLinkAction === 'alt_click' ? `Hold ${altLabel}+Click to open.` : ''}
            </p>
          </div>
          <select
            id="file-link-action"
            value={preferencesStore.fileLinkAction}
            onchange={(e) => preferencesStore.setFileLinkAction(e.currentTarget.value)}
          >
            <option value="click">Click opens file</option>
            <option value="modifier_click">{modLabel}+Click opens file</option>
            <option value="alt_click">{altLabel}+Click opens file</option>
            <option value="disabled">Disabled</option>
          </select>
        </div>

        <h3 class="section-heading" style="margin-top: 20px;">Prompt Patterns</h3>
        <p class="section-desc">
          Patterns for detecting the remote directory when splitting SSH panes.
          This lets cloned terminals automatically <code>cd</code> to the source directory on the remote host.
          Use <code>\h</code> hostname, <code>\u</code> username, <code>\d</code> directory, <code>\p</code> prompt char (<code>$ # % &gt;</code>).
        </p>

        {#each preferencesStore.promptPatterns as pattern, idx}
          <div class="pattern-row">
            <input
              type="text"
              class="pattern-input"
              value={pattern}
              placeholder={'e.g. \\h \\u[\\d]\\p'}
              onchange={(e) => {
                const updated = [...preferencesStore.promptPatterns];
                updated[idx] = e.currentTarget.value;
                preferencesStore.setPromptPatterns(updated);
              }}
            />
            <button
              class="pattern-delete"
              onclick={() => {
                const updated = preferencesStore.promptPatterns.filter((_, i) => i !== idx);
                preferencesStore.setPromptPatterns(updated);
              }}
              title="Remove pattern"
            >&times;</button>
          </div>
        {/each}

        <div class="pattern-actions">
          <button
            class="add-pattern-btn"
            onclick={() => preferencesStore.setPromptPatterns([...preferencesStore.promptPatterns, ''])}
          >+ Add Pattern</button>
          <button
            class="add-pattern-btn"
            onclick={() => preferencesStore.setPromptPatterns([...defaultPromptPatterns])}
          >Reset to Defaults</button>
        </div>
      {:else if activeSection === 'ui'}
        <div class="setting">
          <label for="auto-save">Auto-save Interval</label>
          <select
            id="auto-save"
            value={preferencesStore.autoSaveInterval}
            onchange={(e) => preferencesStore.setAutoSaveInterval(parseInt(e.currentTarget.value))}
          >
            {#each autoSaveOptions as opt}
              <option value={opt.value}>{opt.label}</option>
            {/each}
          </select>
        </div>

        <div class="setting">
          <label for="scrollback">Scrollback Limit</label>
          <select
            id="scrollback"
            value={preferencesStore.scrollbackLimit}
            onchange={(e) => preferencesStore.setScrollbackLimit(parseInt(e.currentTarget.value))}
          >
            {#each scrollbackOptions as opt}
              <option value={opt.value}>{opt.label}</option>
            {/each}
          </select>
        </div>
      {:else if activeSection === 'tabs'}
        <h3 class="section-heading">Duplication</h3>
        <p class="section-desc">
          What to clone when splitting a pane (<kbd>{modLabel}+D</kbd>).
        </p>

        <div class="setting">
          <label for="clone-cwd">Working Directory</label>
          <button
            id="clone-cwd"
            class="toggle"
            class:active={preferencesStore.cloneCwd}
            onclick={() => preferencesStore.setCloneCwd(!preferencesStore.cloneCwd)}
            aria-pressed={preferencesStore.cloneCwd}
            aria-label="Toggle clone working directory"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="clone-scrollback">Scrollback Buffer</label>
          <button
            id="clone-scrollback"
            class="toggle"
            class:active={preferencesStore.cloneScrollback}
            onclick={() => preferencesStore.setCloneScrollback(!preferencesStore.cloneScrollback)}
            aria-pressed={preferencesStore.cloneScrollback}
            aria-label="Toggle clone scrollback buffer"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="clone-ssh">SSH Session</label>
          <button
            id="clone-ssh"
            class="toggle"
            class:active={preferencesStore.cloneSsh}
            onclick={() => preferencesStore.setCloneSsh(!preferencesStore.cloneSsh)}
            aria-pressed={preferencesStore.cloneSsh}
            aria-label="Toggle clone SSH session"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="clone-history">Shell History</label>
          <button
            id="clone-history"
            class="toggle"
            class:active={preferencesStore.cloneHistory}
            onclick={() => preferencesStore.setCloneHistory(!preferencesStore.cloneHistory)}
            aria-pressed={preferencesStore.cloneHistory}
            aria-label="Toggle clone shell history"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="clone-notes">Notes</label>
          <button
            id="clone-notes"
            class="toggle"
            class:active={preferencesStore.cloneNotes}
            onclick={() => preferencesStore.setCloneNotes(!preferencesStore.cloneNotes)}
            aria-pressed={preferencesStore.cloneNotes}
            aria-label="Toggle clone notes"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="clone-auto-resume">Auto-Resume</label>
          <button
            id="clone-auto-resume"
            class="toggle"
            class:active={preferencesStore.cloneAutoResume}
            onclick={() => preferencesStore.setCloneAutoResume(!preferencesStore.cloneAutoResume)}
            aria-pressed={preferencesStore.cloneAutoResume}
            aria-label="Toggle clone auto-resume settings"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="clone-variables">Trigger Variables</label>
          <button
            id="clone-variables"
            class="toggle"
            class:active={preferencesStore.cloneVariables}
            onclick={() => preferencesStore.setCloneVariables(!preferencesStore.cloneVariables)}
            aria-pressed={preferencesStore.cloneVariables}
            aria-label="Toggle clone trigger variables"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="number-duplicated-tabs">Autonumber Duplicated Tabs</label>
            <p class="setting-hint">Prepend a numeric index to duplicated tab names (e.g. "2 My Tab").</p>
          </div>
          <button
            id="number-duplicated-tabs"
            class="toggle"
            class:active={preferencesStore.numberDuplicatedTabs}
            onclick={() => preferencesStore.setNumberDuplicatedTabs(!preferencesStore.numberDuplicatedTabs)}
            aria-pressed={preferencesStore.numberDuplicatedTabs}
            aria-label="Toggle number duplicated tabs"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

      {:else if activeSection === 'workspace'}
        <h3 class="section-heading">Sidebar</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="show-recent">Show Recent Workspaces</label>
            <p class="setting-hint">Shows recently visited workspaces at the top of the sidebar for quick access. Workspaces appear here for 30 minutes after switching away from them.</p>
          </div>
          <button
            id="show-recent"
            class="toggle"
            class:active={preferencesStore.showRecentWorkspaces}
            onclick={() => preferencesStore.setShowRecentWorkspaces(!preferencesStore.showRecentWorkspaces)}
            aria-pressed={preferencesStore.showRecentWorkspaces}
            aria-label="Toggle show recent workspaces"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <div>
            <label for="show-tab-count">Show Tab Count</label>
            <p class="setting-hint">Displays the number of tabs after each workspace name.</p>
          </div>
          <button
            id="show-tab-count"
            class="toggle"
            class:active={preferencesStore.showWorkspaceTabCount}
            onclick={() => preferencesStore.setShowWorkspaceTabCount(!preferencesStore.showWorkspaceTabCount)}
            aria-pressed={preferencesStore.showWorkspaceTabCount}
            aria-label="Toggle show tab count"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <div>
            <label for="tab-button-style">Tab Buttons</label>
            <p class="setting-hint">When to show close, archive, and duplicate buttons on tabs.</p>
          </div>
          <select
            id="tab-button-style"
            value={preferencesStore.tabButtonStyle}
            onchange={(e) => preferencesStore.setTabButtonStyle(e.currentTarget.value)}
          >
            <option value="hover">On Hover</option>
            <option value="always">Always</option>
            <option value="modifier">While Holding {modLabel}</option>
            <option value="never">Never</option>
          </select>
        </div>

        <h3 class="section-heading">Sort Order</h3>
        <p class="section-desc">How workspaces are ordered in the sidebar.</p>

        <div class="setting">
          <label for="workspace-sort">Order</label>
          <select
            id="workspace-sort"
            value={preferencesStore.workspaceSortOrder}
            onchange={(e) => preferencesStore.setWorkspaceSortOrder(e.currentTarget.value)}
          >
            <option value="default">Default (drag & drop)</option>
            <option value="alphabetical">Alphabetical</option>
            <option value="recent_activity">Most Recent Activity</option>
          </select>
        </div>

        <h3 class="section-heading">Suspend</h3>
        <p class="section-desc">Suspended workspaces save terminal state and kill PTYs, freeing resources. Click a suspended workspace in the sidebar to resume it.</p>

        <div class="setting">
          <div>
            <label for="group-active-tabs">Group active tabs first</label>
            <p class="setting-hint">Visually move non-suspended tabs to the front of the tab bar. Your manual tab order is preserved — this only affects the display.</p>
          </div>
          <button
            id="group-active-tabs"
            class="toggle"
            class:active={preferencesStore.groupActiveTabs}
            onclick={() => preferencesStore.setGroupActiveTabs(!preferencesStore.groupActiveTabs)}
            aria-pressed={preferencesStore.groupActiveTabs}
            aria-label="Toggle group active tabs first"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <div>
            <label for="auto-suspend">Auto-suspend inactive workspaces</label>
            <p class="setting-hint">Automatically suspend workspaces that haven't been visited within the selected time. The active workspace is never auto-suspended.</p>
          </div>
          <select
            id="auto-suspend"
            value={preferencesStore.autoSuspendMinutes}
            onchange={(e) => preferencesStore.setAutoSuspendMinutes(Number(e.currentTarget.value))}
          >
            <option value={0}>Disabled</option>
            <option value={15}>15 minutes</option>
            <option value={30}>30 minutes</option>
            <option value={60}>1 hour</option>
          </select>
        </div>

      {:else if activeSection === 'notes'}
        <h3 class="section-heading">Preview</h3>

        <div class="setting">
          <label for="notes-font-size">Font Size</label>
          <div class="number-input-wrapper">
            <button class="number-btn" onclick={() => preferencesStore.setNotesFontSize(preferencesStore.notesFontSize - 1)}>−</button>
            <input
              type="number"
              id="notes-font-size"
              class="number-input"
              min="10"
              max="24"
              value={preferencesStore.notesFontSize}
              onchange={(e) => preferencesStore.setNotesFontSize(parseInt(e.currentTarget.value) || 13)}
            />
            <button class="number-btn" onclick={() => preferencesStore.setNotesFontSize(preferencesStore.notesFontSize + 1)}>+</button>
          </div>
        </div>

        <div class="setting">
          <label for="notes-font-family">Font Family</label>
          <select
            id="notes-font-family"
            value={preferencesStore.notesFontFamily}
            onchange={(e) => preferencesStore.setNotesFontFamily(e.currentTarget.value)}
          >
            {#each fontFamilies as font}
              <option value={font}>{font}</option>
            {/each}
          </select>
        </div>

        <h3 class="section-heading">General</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="migrate-tab-notes">Migrate Tab Notes</label>
            <p class="setting-hint">When closing a tab, move its notes to the workspace.</p>
          </div>
          <button
            id="migrate-tab-notes"
            class="toggle"
            class:active={preferencesStore.migrateTabNotes}
            onclick={() => preferencesStore.setMigrateTabNotes(!preferencesStore.migrateTabNotes)}
            aria-pressed={preferencesStore.migrateTabNotes}
            aria-label="Toggle migrate tab notes"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <div class="setting">
          <label for="notes-word-wrap">Word Wrap</label>
          <button
            id="notes-word-wrap"
            class="toggle"
            class:active={preferencesStore.notesWordWrap}
            onclick={() => preferencesStore.setNotesWordWrap(!preferencesStore.notesWordWrap)}
            aria-pressed={preferencesStore.notesWordWrap}
            aria-label="Toggle word wrap in notes"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

      {:else if activeSection === 'notifications'}
        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="notification-mode">Notification Mode</label>
            <p class="setting-hint">
              {#if preferencesStore.notificationMode === 'auto'}
                In-app toasts when focused, OS notifications when unfocused.
              {:else if preferencesStore.notificationMode === 'in_app'}
                Always show in-app toasts inside the window.
              {:else if preferencesStore.notificationMode === 'native'}
                Always use OS notifications.
              {:else}
                Notifications are disabled.
              {/if}
            </p>
          </div>
          <select
            id="notification-mode"
            value={preferencesStore.notificationMode}
            onchange={(e) => preferencesStore.setNotificationMode(e.currentTarget.value)}
          >
            <option value="auto">Auto</option>
            <option value="in_app">In-App Only</option>
            <option value="native">Native Only</option>
            <option value="disabled">Disabled</option>
          </select>
        </div>

        {#if preferencesStore.notificationMode !== 'disabled'}
          <div class="setting">
            <label for="notification-sound">Sound</label>
            <div class="sound-picker">
              <select
                id="notification-sound"
                value={preferencesStore.notificationSound}
                onchange={(e) => {
                  const val = e.currentTarget.value;
                  preferencesStore.setNotificationSound(val);
                  if (val !== 'none') {
                    import('$lib/stores/notificationDispatch').then(m => m.playNotificationSoundPreview());
                  }
                }}
              >
                <option value="none">None</option>
                <option value="default">Default (Built-in)</option>
                {#each systemSounds as sound}
                  <option value={sound}>{sound}</option>
                {/each}
              </select>
              {#if preferencesStore.notificationSound !== 'none'}
                <button
                  class="preview-sound-btn"
                  onclick={() => {
                    import('$lib/stores/notificationDispatch').then(m => m.playNotificationSoundPreview());
                  }}
                  title="Preview sound"
                >&#9654;</button>
              {/if}
            </div>
          </div>

          {#if preferencesStore.notificationSound !== 'none'}
            <div class="setting">
              <label for="notification-volume">Volume</label>
              <div class="volume-wrapper">
                <input
                  type="range"
                  id="notification-volume"
                  class="volume-slider"
                  min="0"
                  max="100"
                  value={preferencesStore.notificationVolume}
                  oninput={(e) => preferencesStore.setNotificationVolume(parseInt(e.currentTarget.value))}
                />
                <span class="volume-label">{preferencesStore.notificationVolume}%</span>
              </div>
            </div>
          {/if}

          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="notify-duration">Command Threshold</label>
              <p class="setting-hint">
                Only notify on command completion if it ran longer than this.
              </p>
            </div>
            <select
              id="notify-duration"
              value={preferencesStore.notifyMinDuration}
              onchange={(e) => preferencesStore.setNotifyMinDuration(parseInt(e.currentTarget.value))}
            >
              <option value={0}>Always</option>
              <option value={3}>3 seconds</option>
              <option value={5}>5 seconds</option>
              <option value={10}>10 seconds</option>
              <option value={15}>15 seconds</option>
              <option value={30}>30 seconds</option>
              <option value={60}>60 seconds</option>
            </select>
          </div>
        {/if}

        {#if preferencesStore.notificationMode !== 'disabled' && preferencesStore.notificationMode !== 'native'}
          <h3 class="section-heading" style="margin-top: 20px;">In-App Toast</h3>

          <div class="setting">
            <label for="toast-duration">Display Duration</label>
            <select
              id="toast-duration"
              value={preferencesStore.toastDuration}
              onchange={(e) => preferencesStore.setToastDuration(parseInt(e.currentTarget.value))}
            >
              <option value={3}>3 seconds</option>
              <option value={5}>5 seconds</option>
              <option value={8}>8 seconds</option>
              <option value={10}>10 seconds</option>
              <option value={15}>15 seconds</option>
              <option value={30}>30 seconds</option>
            </select>
          </div>

          <div class="setting">
            <label for="toast-font-size">Font Size</label>
            <div class="number-input-wrapper">
              <button class="number-btn" onclick={() => preferencesStore.setToastFontSize(preferencesStore.toastFontSize - 1)}>−</button>
              <input
                type="number"
                id="toast-font-size"
                class="number-input"
                min="10"
                max="24"
                value={preferencesStore.toastFontSize}
                onchange={(e) => preferencesStore.setToastFontSize(parseInt(e.currentTarget.value) || 14)}
              />
              <button class="number-btn" onclick={() => preferencesStore.setToastFontSize(preferencesStore.toastFontSize + 1)}>+</button>
            </div>
          </div>

          <div class="setting">
            <label for="toast-width">Max Width</label>
            <div class="number-input-wrapper">
              <button class="number-btn" onclick={() => preferencesStore.setToastWidth(preferencesStore.toastWidth - 20)}>−</button>
              <input
                type="number"
                id="toast-width"
                class="number-input"
                min="280"
                max="600"
                value={preferencesStore.toastWidth}
                onchange={(e) => preferencesStore.setToastWidth(parseInt(e.currentTarget.value) || 400)}
              />
              <button class="number-btn" onclick={() => preferencesStore.setToastWidth(preferencesStore.toastWidth + 20)}>+</button>
            </div>
          </div>
        {/if}

      {:else if activeSection === 'triggers'}
        <p class="section-desc">
          Triggers watch terminal output for regex patterns and react with actions.
          Each trigger has a cooldown to prevent firing in rapid loops.
        </p>

        <div style="display: flex; gap: 8px; margin-bottom: 12px;">
          <button class="add-pattern-btn" onclick={addTrigger}>+ Add Trigger</button>
          {#if preferencesStore.hiddenDefaultTriggers.length > 0}
            <button class="add-pattern-btn" onclick={restoreAllDefaults}>Restore Defaults</button>
          {/if}
        </div>

        {#each preferencesStore.triggers as trigger (trigger.id)}
          <div class="trigger-card">
            <div class="trigger-header" class:trigger-header-expanded={expandedTriggerId === trigger.id}>
              <button
                class="toggle small"
                class:active={trigger.enabled}
                onclick={() => updateTrigger(trigger.id, { enabled: !trigger.enabled })}
                aria-pressed={trigger.enabled}
                aria-label="Toggle trigger"
              >
                <span class="toggle-knob"></span>
              </button>
              <button
                class="trigger-name-btn"
                onclick={() => expandedTriggerId = expandedTriggerId === trigger.id ? null : trigger.id}
              >
                <svg class="trigger-chevron" class:expanded={expandedTriggerId === trigger.id} width="12" height="12" viewBox="0 0 16 16" fill="currentColor"><path d="M6 3l5 5-5 5z"/></svg>
                {trigger.name || 'Unnamed'}
              </button>
              {#if trigger.default_id}
                <button
                  class="restore-default-btn"
                  disabled={!trigger.user_modified}
                  onclick={() => restoreDefault(trigger)}
                  title="Restore to default"
                >Reset</button>
              {/if}
              {#if confirmDeleteId === trigger.id}
                <span class="confirm-delete">
                  <span class="confirm-delete-label">Delete?</span>
                  <button class="confirm-delete-btn confirm-yes" onclick={() => doDeleteTrigger(trigger.id)}>Yes</button>
                  <button class="confirm-delete-btn confirm-no" onclick={() => { confirmDeleteId = null; }}>No</button>
                </span>
              {:else}
                <button
                  class="pattern-delete trigger-delete"
                  onclick={() => deleteTrigger(trigger.id)}
                  title="Delete trigger"
                ><Icon name="trash" /></button>
              {/if}
            </div>

            {#if expandedTriggerId === trigger.id}
              {@const mode = resolveMatchMode(trigger)}
              <div class="trigger-body" transition:slide={{ duration: 150 }}>
                <div class="trigger-field">
                  <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context; input is dynamically rendered per-trigger -->
                  <label>Name</label>
                  <input
                    type="text"
                    class="pattern-input"
                    data-trigger-name={trigger.id}
                    value={trigger.name}
                    placeholder="e.g. Capture session ID"
                    onchange={(e) => updateTrigger(trigger.id, { name: e.currentTarget.value })}
                  />
                </div>

                <div class="trigger-field">
                  <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context for custom ResizableTextarea component -->
                  <label>Description</label>
                  <ResizableTextarea
                    value={trigger.description ?? ''}
                    placeholder="What does this trigger do?"
                    rows={1}
                    maxHeight={120}
                    onchange={(v) => updateTrigger(trigger.id, { description: v || null })}
                  />
                </div>

                <div class="trigger-section">
                  <h4 class="trigger-section-heading">When</h4>

                  <div class="trigger-field">
                    <div class="pattern-label-row">
                      <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context for custom ResizableTextarea below -->
                      <label>
                        Pattern
                        {#if mode === 'plain_text'}
                          <span class="field-hint">(spaces match TUI gaps)</span>
                        {:else if mode === 'variable'}
                          <span class="field-hint">(fires on false&rarr;true transition)</span>
                        {:else}
                          <span class="field-hint">(regex, supports multiline)</span>
                        {/if}
                      </label>
                      <select
                        class="match-mode-select"
                        value={mode}
                        onchange={(e) => {
                          const newMode = e.currentTarget.value as MatchMode;
                          updateTrigger(trigger.id, {
                            match_mode: newMode,
                            plain_text: newMode === 'plain_text',
                          });
                        }}
                      >
                        <option value="regex">Regex</option>
                        <option value="plain_text">Plain Text</option>
                        <option value="variable">Variable Match</option>
                      </select>
                    </div>
                    <ResizableTextarea
                      value={trigger.pattern}
                      placeholder={mode === 'plain_text'
                        ? "e.g. Would you like to proceed?"
                        : mode === 'variable'
                          ? "e.g. claudeSessionId || claudeResumeCommand"
                          : "e.g. error|fail\nor multiline: Resume.*?--resume ([a-z0-9\\-]*)"}
                      rows={2}
                      maxHeight={200}
                      mono
                      invalid={mode === 'regex' ? !isValidRegex(trigger.pattern) : mode === 'variable' ? !isValidCondition(trigger.pattern) : false}
                      onchange={(v) => updateTrigger(trigger.id, { pattern: v })}
                    />
                  </div>

                  <div class="trigger-inline-fields">
                    <div class="trigger-field" style="flex: none;">
                      <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context; input is dynamically rendered per-trigger -->
                      <label>Cooldown <span class="field-hint">(seconds)</span></label>
                      <input
                        type="text"
                        inputmode="decimal"
                        class="pattern-input no-spinner"
                        style="width: 80px;"
                        value={trigger.cooldown}
                        onchange={(e) => updateTrigger(trigger.id, { cooldown: Math.max(0, parseFloat(e.currentTarget.value) || 0) })}
                      />
                    </div>
                    <div class="trigger-field" style="flex: 1; min-width: 0;">
                      <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context for checkbox list below -->
                      <label>Workspaces <span class="field-hint">({trigger.workspaces.length || 'all'})</span></label>
                      {#if true}
                      {@const wsQuery = (wsSearchQueries.get(trigger.id) ?? '').toLowerCase()}
                      {@const wsShowSel = wsShowSelected.get(trigger.id) ?? false}
                      {@const visibleWs = allWorkspaces.filter(ws => {
                        if (wsShowSel && !trigger.workspaces.includes(ws.id)) return false;
                        if (!wsQuery) return true;
                        return ws.name.toLowerCase().includes(wsQuery);
                      })}
                      <div class="tab-selector">
                        <div class="tab-selector-bar">
                          <input
                            type="text"
                            class="tab-search-input"
                            placeholder="Search workspaces…"
                            value={wsSearchQueries.get(trigger.id) ?? ''}
                            oninput={(e) => {
                              wsSearchQueries.set(trigger.id, e.currentTarget.value);
                              wsSearchQueries = new Map(wsSearchQueries);
                            }}
                          />
                          <Tooltip text="Show selected only">
                            <!-- svelte-ignore a11y_consider_explicit_label -->
                            <input
                              type="checkbox"
                              class="tab-show-selected-cb"
                              checked={wsShowSelected.get(trigger.id) ?? false}
                              onchange={() => {
                                wsShowSelected.set(trigger.id, !(wsShowSelected.get(trigger.id) ?? false));
                                wsShowSelected = new Map(wsShowSelected);
                              }}
                            />
                          </Tooltip>
                        </div>
                        <div class="tab-selector-list">
                          {#each visibleWs as ws (ws.id)}
                            <label class="tab-selector-item" class:selected={trigger.workspaces.includes(ws.id)}>
                              <input
                                type="checkbox"
                                checked={trigger.workspaces.includes(ws.id)}
                                onchange={() => {
                                  const cur = trigger.workspaces;
                                  const next = cur.includes(ws.id)
                                    ? cur.filter(id => id !== ws.id)
                                    : [...cur, ws.id];
                                  updateTrigger(trigger.id, { workspaces: next });
                                }}
                              />
                              <span class="tab-selector-label">{ws.name}</span>
                            </label>
                          {:else}
                            <span class="field-hint" style="padding: 4px 8px;">
                              {wsShowSel ? 'No workspaces selected' : wsQuery ? 'No matching workspaces' : 'No workspaces found'}
                            </span>
                          {/each}
                        </div>
                        {#if trigger.workspaces.length > 0}
                          <button class="deselect-all-btn" onclick={() => updateTrigger(trigger.id, { workspaces: [] })}>Deselect all</button>
                        {/if}
                      </div>
                      {/if}
                    </div>
                    <div class="trigger-field" style="flex: 1; min-width: 0;">
                      <!-- svelte-ignore a11y_label_has_associated_control -- label is visual context for checkbox list below -->
                      <label>Tabs <span class="field-hint">({(trigger.tabs ?? []).length || 'all'})</span></label>
                      {#if true}
                      {@const scopedTabs = trigger.workspaces.length > 0
                        ? allTabs.filter(t => trigger.workspaces.includes(t.workspaceId))
                        : allTabs}
                      {@const tQuery = (tabSearchQueries.get(trigger.id) ?? '').toLowerCase()}
                      {@const tShowSelected = tabShowSelected.get(trigger.id) ?? false}
                      {@const visibleTabs = scopedTabs.filter(t => {
                        if (tShowSelected && !(trigger.tabs ?? []).includes(t.id)) return false;
                        if (!tQuery) return true;
                        const label = allWorkspaces.length > 1 ? `${t.workspaceName} / ${t.name}` : t.name;
                        return label.toLowerCase().includes(tQuery);
                      })}
                      <div class="tab-selector">
                        <div class="tab-selector-bar">
                          {#if scopedTabs.some(t => t.isActive) && !(trigger.tabs ?? []).includes(scopedTabs.find(t => t.isActive)!.id)}
                            <button
                              class="tab-active-btn"
                              title="Add the currently active tab"
                              onclick={() => {
                                const active = scopedTabs.find(t => t.isActive);
                                if (active) {
                                  const cur = trigger.tabs ?? [];
                                  if (!cur.includes(active.id)) {
                                    updateTrigger(trigger.id, { tabs: [...cur, active.id] });
                                  }
                                }
                              }}
                            >+ Active</button>
                          {/if}
                          <input
                            type="text"
                            class="tab-search-input"
                            placeholder="Search tabs…"
                            value={tabSearchQueries.get(trigger.id) ?? ''}
                            oninput={(e) => {
                              tabSearchQueries.set(trigger.id, e.currentTarget.value);
                              tabSearchQueries = new Map(tabSearchQueries);
                            }}
                          />
                          <Tooltip text="Show selected only">
                            <!-- svelte-ignore a11y_consider_explicit_label -->
                            <input
                              type="checkbox"
                              class="tab-show-selected-cb"
                              checked={tabShowSelected.get(trigger.id) ?? false}
                              onchange={() => {
                                tabShowSelected.set(trigger.id, !(tabShowSelected.get(trigger.id) ?? false));
                                tabShowSelected = new Map(tabShowSelected);
                              }}
                            />
                          </Tooltip>
                        </div>
                        <div class="tab-selector-list">
                          {#each visibleTabs as tab (tab.id)}
                            <label class="tab-selector-item" class:selected={(trigger.tabs ?? []).includes(tab.id)}>
                              <input
                                type="checkbox"
                                checked={(trigger.tabs ?? []).includes(tab.id)}
                                onchange={() => {
                                  const cur = trigger.tabs ?? [];
                                  const next = cur.includes(tab.id)
                                    ? cur.filter(id => id !== tab.id)
                                    : [...cur, tab.id];
                                  updateTrigger(trigger.id, { tabs: next });
                                }}
                              />
                              <span class="tab-selector-label">{allWorkspaces.length > 1 ? `${tab.workspaceName} / ${tab.name}` : tab.name}</span>
                              {#if tab.isActive}<span class="tab-active-badge">active</span>{/if}
                            </label>
                          {:else}
                            <span class="field-hint" style="padding: 4px 8px;">
                              {tShowSelected ? 'No tabs selected' : tQuery ? 'No matching tabs' : 'No tabs found'}
                            </span>
                          {/each}
                        </div>
                        {#if (trigger.tabs ?? []).length > 0}
                          <button class="deselect-all-btn" onclick={() => updateTrigger(trigger.id, { tabs: [] })}>Deselect all</button>
                        {/if}
                      </div>
                      {/if}
                    </div>
                  </div>
                </div>

                {#if resolveMatchMode(trigger) !== 'variable'}
                <div class="trigger-section">
                  <h4 class="trigger-section-heading">Capture <span class="field-hint">use %varName in tab titles and auto-resume commands</span></h4>

                  {#each trigger.variables as vm, vi}
                    {@const groupCount = countCaptureGroups(trigger.pattern)}
                    {@const nameErr = varNameError(vm.name)}
                    {@const groupErr = groupCount > 0 && vm.group > groupCount ? `Pattern has ${groupCount} group${groupCount === 1 ? '' : 's'}` : ''}
                    <div class="var-row-wrap">
                    <div class="var-row">
                      <input
                        type="text"
                        class="pattern-input var-name-input"
                        class:var-field-invalid={!!nameErr}
                        value={vm.name}
                        placeholder="varName"
                        onchange={(e) => {
                          const newName = e.currentTarget.value.trim();
                          const vars = trigger.variables.map((v, i) =>
                            i === vi ? { ...v, name: newName } : v
                          );
                          updateTrigger(trigger.id, { variables: vars });
                        }}
                      />
                      <span class="var-arrow">&larr; group</span>
                      <input
                        type="text"
                        inputmode="numeric"
                        class="pattern-input var-idx-input"
                        class:var-field-invalid={!!groupErr}
                        value={vm.group}
                        onchange={(e) => {
                          const num = parseInt(e.currentTarget.value) || 1;
                          const clamped = Math.max(1, num);
                          const vars = trigger.variables.map((v, i) =>
                            i === vi ? { ...v, group: clamped } : v
                          );
                          updateTrigger(trigger.id, { variables: vars });
                        }}
                      />
                      <span class="var-arrow">template</span>
                      <input
                        type="text"
                        class="pattern-input var-template-input"
                        value={vm.template ?? ''}
                        placeholder="% (raw value)"
                        onchange={(e) => {
                          const val = e.currentTarget.value;
                          const vars = trigger.variables.map((v, i) =>
                            i === vi ? { ...v, template: val || undefined } : v
                          );
                          updateTrigger(trigger.id, { variables: vars });
                        }}
                      />
                      <button
                        class="pattern-delete"
                        onclick={() => {
                          const vars = trigger.variables.filter((_, i) => i !== vi);
                          updateTrigger(trigger.id, { variables: vars });
                        }}
                        title="Remove variable"
                      >&times;</button>
                    </div>
                    {#if nameErr || groupErr}
                      <div class="var-error">{nameErr || groupErr}</div>
                    {/if}
                    </div>
                  {/each}
                  <button
                    class="add-pattern-btn"
                    onclick={() => {
                      const vars: VariableMapping[] = [...trigger.variables, { name: `var${trigger.variables.length + 1}`, group: 1 }];
                      updateTrigger(trigger.id, { variables: vars });
                    }}
                  >+ Add Variable</button>
                </div>
                {/if}

                <div class="trigger-section">
                  <h4 class="trigger-section-heading">Then</h4>

                  {#each trigger.actions as entry, ai}
                    <div class="action-row">
                      <select
                        class="pattern-input action-type-select"
                        value={entry.action_type}
                        onchange={(e) => {
                          const newType = e.currentTarget.value as TriggerActionType;
                          const actions = trigger.actions.map((a, i) =>
                            i === ai ? {
                              ...a,
                              action_type: newType,
                              // Default tab_state when switching to set_tab_state
                              tab_state: newType === 'set_tab_state' ? (a.tab_state ?? 'alert') : a.tab_state,
                            } : a
                          );
                          updateTrigger(trigger.id, { actions });
                        }}
                      >
                        <option value="notify">Notify</option>
                        <option value="send_command">Send Command</option>
                        <option value="set_tab_state">Change Tab State</option>
                        <option value="enable_auto_resume">Enable Auto-Resume</option>
                        <option value="replay_auto_resume">Replay Auto-Resume</option>
                      </select>
                      {#if entry.action_type === 'send_command'}
                        <input
                          type="text"
                          class="pattern-input mono action-command-input"
                          value={entry.command ?? ''}
                          placeholder="e.g. echo triggered"
                          onchange={(e) => {
                            const actions = trigger.actions.map((a, i) =>
                              i === ai ? { ...a, command: e.currentTarget.value || null } : a
                            );
                            updateTrigger(trigger.id, { actions });
                          }}
                        />
                      {:else if entry.action_type === 'notify'}
                        <div class="notify-fields">
                          <input
                            type="text"
                            class="pattern-input action-command-input"
                            value={entry.title ?? ''}
                            placeholder="title (default: %tabtitle)"
                            onchange={(e) => {
                              const actions = trigger.actions.map((a, i) =>
                                i === ai ? { ...a, title: e.currentTarget.value || null } : a
                              );
                              updateTrigger(trigger.id, { actions });
                            }}
                          />
                          <input
                            type="text"
                            class="pattern-input action-command-input"
                            value={entry.message ?? ''}
                            placeholder="body"
                            onchange={(e) => {
                              const actions = trigger.actions.map((a, i) =>
                                i === ai ? { ...a, message: e.currentTarget.value || null } : a
                              );
                              updateTrigger(trigger.id, { actions });
                            }}
                          />
                        </div>
                        <Tooltip text={"%title — OSC title (set by program)\n%tab — tab name from workspace\n%tabtitle — full tab display name\n%varName — trigger capture variables"}>
                          <span class="notify-help">?</span>
                        </Tooltip>
                      {:else if entry.action_type === 'enable_auto_resume'}
                        <input
                          type="text"
                          class="pattern-input mono action-command-input"
                          value={entry.command ?? ''}
                          placeholder="auto-resume command (%vars supported)"
                          onchange={(e) => {
                            const actions = trigger.actions.map((a, i) =>
                              i === ai ? { ...a, command: e.currentTarget.value || null } : a
                            );
                            updateTrigger(trigger.id, { actions });
                          }}
                        />
                      {:else if entry.action_type === 'set_tab_state'}
                        <select
                          class="pattern-input action-type-select"
                          value={entry.tab_state ?? 'alert'}
                          onchange={(e) => {
                            const actions = trigger.actions.map((a, i) =>
                              i === ai ? { ...a, tab_state: e.currentTarget.value as TabStateName } : a
                            );
                            updateTrigger(trigger.id, { actions });
                          }}
                        >
                          <option value="alert">Alert</option>
                          <option value="question">Question</option>
                        </select>
                      {/if}
                      <button
                        class="pattern-delete"
                        onclick={() => {
                          const actions = trigger.actions.filter((_, i) => i !== ai);
                          updateTrigger(trigger.id, { actions });
                        }}
                        title="Remove action"
                      >&times;</button>
                    </div>
                  {/each}
                  <button
                    class="add-pattern-btn"
                    onclick={() => {
                      const actions = [...trigger.actions, { action_type: 'notify' as TriggerActionType, command: null, title: null, message: null, tab_state: null }];
                      updateTrigger(trigger.id, { actions });
                    }}
                  >+ Add Action</button>
                </div>
              </div>
            {/if}
          </div>
        {/each}

        {#if !preferencesStore.triggers.length}
          <p class="section-desc" style="margin-top: 8px;">No triggers configured.</p>
        {/if}
      {:else if activeSection === 'claude_code'}
        <h3 class="section-heading">AI Agent Integration</h3>
        <p class="section-desc">
          Connect AI coding agents to maiTerm. When enabled, maiTerm starts a local server
          so an agent's CLI can open files, show diffs, and access editor state. Each runtime
          is configured independently below. Requires restart to take effect.
        </p>

        <h3 class="section-heading">Claude Code</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="claude-code-ide">Enable IDE Integration</label>
            <p class="setting-hint">
              Starts a local WebSocket server for Claude Code to communicate with maiTerm.
            </p>
          </div>
          <button
            id="claude-code-ide"
            class="toggle"
            class:active={preferencesStore.claudeCodeIde}
            onclick={() => preferencesStore.setClaudeCodeIde(!preferencesStore.claudeCodeIde)}
            aria-pressed={preferencesStore.claudeCodeIde}
            aria-label="Toggle Claude Code IDE integration"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        {#if preferencesStore.claudeCodeIde}
          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="claude-code-hooks">Enable Hooks Integration</label>
              <p class="setting-hint">
                Registers lifecycle hooks so Claude Code reports session state to maiTerm
                (active/idle/permission tab indicators). Requires restart.
              </p>
            </div>
            <button
              id="claude-code-hooks"
              class="toggle"
              class:active={preferencesStore.claudeCodeHooks}
              onclick={() => preferencesStore.setClaudeCodeHooks(!preferencesStore.claudeCodeHooks)}
              aria-pressed={preferencesStore.claudeCodeHooks}
              aria-label="Toggle hooks integration"
            >
              <span class="toggle-knob"></span>
            </button>
          </div>

          {#if preferencesStore.claudeCodeHooks}
            <div class="setting" style="align-items: flex-start;">
              <div>
                <label for="claude-code-auto-resume">Enable Auto-Resume via Hooks</label>
                <p class="setting-hint">
                  Automatically captures session IDs and configures auto-resume when Claude initializes its maiTerm session.
                  No screen-scraping triggers needed.
                </p>
              </div>
              <button
                id="claude-code-auto-resume"
                class="toggle"
                class:active={preferencesStore.claudeCodeAutoResume}
                onclick={() => preferencesStore.setClaudeCodeAutoResume(!preferencesStore.claudeCodeAutoResume)}
                aria-pressed={preferencesStore.claudeCodeAutoResume}
                aria-label="Toggle hooks-based auto-resume"
              >
                <span class="toggle-knob"></span>
              </button>
            </div>
          {/if}

          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="claude-code-ide-ssh">Enable IDE Integration over SSH</label>
              <p class="setting-hint">
                Automatically creates a secure reverse SSH tunnel when you connect to a remote server,
                so Claude Code running remotely can access your local maiTerm MCP tools (workspace navigation,
                notes, tab context, auto-resume, etc.).
              </p>
              <p class="setting-hint" style="margin-top: 6px; opacity: 0.7;">
                This writes a small discovery file (<code>~/.claude/ide/*.lock</code>) and registers in
                <code>~/.claude.json</code> on the remote server. No other software is installed.
                All traffic is encrypted through your existing SSH connection. The discovery file is
                automatically cleaned up when the session ends.
              </p>
            </div>
            <button
              id="claude-code-ide-ssh"
              class="toggle"
              class:active={preferencesStore.claudeCodeIdeSsh}
              onclick={() => preferencesStore.setClaudeCodeIdeSsh(!preferencesStore.claudeCodeIdeSsh)}
              aria-pressed={preferencesStore.claudeCodeIdeSsh}
              aria-label="Toggle SSH IDE integration"
            >
              <span class="toggle-knob"></span>
            </button>
          </div>
        {/if}

        <h3 class="section-heading">Codex</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="codex-ide">Enable Codex IDE integration</label>
            <p class="setting-hint">
              Starts the local server for OpenAI Codex to communicate with maiTerm. Codex is
              opt-in and disabled by default. This writes Codex's MCP config to
              <code>~/.codex/config.toml</code> (Claude uses <code>~/.claude.json</code>).
            </p>
          </div>
          <button
            id="codex-ide"
            class="toggle"
            class:active={preferencesStore.codexIde}
            onclick={() => preferencesStore.setCodexIde(!preferencesStore.codexIde)}
            aria-pressed={preferencesStore.codexIde}
            aria-label="Toggle Codex IDE integration"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        {#if preferencesStore.codexIde}
          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="codex-hooks">Codex lifecycle hooks</label>
              <p class="setting-hint">
                Registers lifecycle hooks so Codex reports session state to maiTerm
                (active/idle/permission tab indicators). Requires restart.
              </p>
            </div>
            <button
              id="codex-hooks"
              class="toggle"
              class:active={preferencesStore.codexHooks}
              onclick={() => preferencesStore.setCodexHooks(!preferencesStore.codexHooks)}
              aria-pressed={preferencesStore.codexHooks}
              aria-label="Toggle Codex lifecycle hooks"
            >
              <span class="toggle-knob"></span>
            </button>
          </div>

          {#if preferencesStore.codexHooks}
            <div class="setting" style="align-items: flex-start;">
              <div>
                <label for="codex-auto-resume">Codex auto-resume</label>
                <p class="setting-hint">
                  Automatically captures session IDs and configures auto-resume when Codex
                  initializes its maiTerm session. No screen-scraping triggers needed.
                </p>
              </div>
              <button
                id="codex-auto-resume"
                class="toggle"
                class:active={preferencesStore.codexAutoResume}
                onclick={() => preferencesStore.setCodexAutoResume(!preferencesStore.codexAutoResume)}
                aria-pressed={preferencesStore.codexAutoResume}
                aria-label="Toggle Codex auto-resume"
              >
                <span class="toggle-knob"></span>
              </button>
            </div>

            <div class="setting" style="align-items: flex-start;">
              <div>
                <label for="codex-hooks-bypass-trust">Skip the one-time Codex hook-trust prompt (advanced)</label>
                <p class="setting-hint">
                  Codex normally requires a one-time <code>/hooks</code> trust confirmation in
                  its CLI before lifecycle hooks run. Enabling this suppresses that friction.
                </p>
              </div>
              <button
                id="codex-hooks-bypass-trust"
                class="toggle"
                class:active={preferencesStore.codexHooksBypassTrust}
                onclick={() => preferencesStore.setCodexHooksBypassTrust(!preferencesStore.codexHooksBypassTrust)}
                aria-pressed={preferencesStore.codexHooksBypassTrust}
                aria-label="Toggle skipping the Codex hook-trust prompt"
              >
                <span class="toggle-knob"></span>
              </button>
            </div>
          {/if}

          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="codex-ide-ssh">Codex MCP bridge over SSH</label>
              <p class="setting-hint">
                Automatically creates a secure reverse SSH tunnel when you connect to a remote
                server, so Codex running remotely can access your local maiTerm MCP tools
                (workspace navigation, notes, tab context, auto-resume, etc.).
              </p>
              <p class="setting-hint" style="margin-top: 6px; opacity: 0.7;">
                This registers maiTerm's MCP server in <code>~/.codex/config.toml</code> on the
                remote server. No other software is installed. All traffic is encrypted through
                your existing SSH connection.
              </p>
            </div>
            <button
              id="codex-ide-ssh"
              class="toggle"
              class:active={preferencesStore.codexIdeSsh}
              onclick={() => preferencesStore.setCodexIdeSsh(!preferencesStore.codexIdeSsh)}
              aria-pressed={preferencesStore.codexIdeSsh}
              aria-label="Toggle Codex SSH MCP bridge"
            >
              <span class="toggle-knob"></span>
            </button>
          </div>
        {/if}

        <h3 class="section-heading" style="margin-top: 20px;">Mesh Workspace</h3>
        <p class="section-desc">
          Loop control for Mesh Workspaces, where every agent talks to every other over
          topic threads. All three limits default to <strong>0 = off</strong>, so a mesh
          flows freely. Turn one on only if you want maiTerm to pause an unwatched
          ping-pong: a topic pauses at the soft cap (resume or complete it from the cockpit,
          ⌘⇧M), while the hard ceiling and time limit are absolute backstops.
        </p>
        <div class="setting">
          <div>
            <label for="mesh-soft-cap">Soft turn cap (per topic)</label>
            <p class="setting-hint">{preferencesStore.meshSoftCap === 0 ? 'Off — topics never pause on turn count.' : `Pause a topic after ${preferencesStore.meshSoftCap} turns; resume adds another ${preferencesStore.meshSoftCap}.`}</p>
          </div>
          <div class="number-input-wrapper">
            <button class="number-btn" onclick={() => preferencesStore.setMeshSoftCap(preferencesStore.meshSoftCap - 1)}>−</button>
            <input
              type="number"
              id="mesh-soft-cap"
              class="number-input"
              min="0"
              max="1000"
              value={preferencesStore.meshSoftCap}
              onchange={(e) => preferencesStore.setMeshSoftCap(parseInt(e.currentTarget.value) || 0)}
            />
            <button class="number-btn" onclick={() => preferencesStore.setMeshSoftCap(preferencesStore.meshSoftCap + 1)}>+</button>
          </div>
        </div>
        <div class="setting">
          <div>
            <label for="mesh-hard-cap">Hard turn ceiling (per topic)</label>
            <p class="setting-hint">{preferencesStore.meshHardCap === 0 ? 'Off — no absolute turn ceiling.' : `Hard stop at ${preferencesStore.meshHardCap} turns; a resume can't lift it (complete the topic).`}</p>
          </div>
          <div class="number-input-wrapper">
            <button class="number-btn" onclick={() => preferencesStore.setMeshHardCap(preferencesStore.meshHardCap - 5)}>−</button>
            <input
              type="number"
              id="mesh-hard-cap"
              class="number-input"
              min="0"
              max="10000"
              value={preferencesStore.meshHardCap}
              onchange={(e) => preferencesStore.setMeshHardCap(parseInt(e.currentTarget.value) || 0)}
            />
            <button class="number-btn" onclick={() => preferencesStore.setMeshHardCap(preferencesStore.meshHardCap + 5)}>+</button>
          </div>
        </div>
        <div class="setting">
          <div>
            <label for="mesh-ttl">Topic time limit (minutes)</label>
            <p class="setting-hint">{preferencesStore.meshTopicTtlMinutes === 0 ? 'Off — topics never pause on age.' : `Pause a topic ${preferencesStore.meshTopicTtlMinutes} min after it starts (or its last resume).`}</p>
          </div>
          <div class="number-input-wrapper">
            <button class="number-btn" onclick={() => preferencesStore.setMeshTopicTtlMinutes(preferencesStore.meshTopicTtlMinutes - 5)}>−</button>
            <input
              type="number"
              id="mesh-ttl"
              class="number-input"
              min="0"
              max="1440"
              value={preferencesStore.meshTopicTtlMinutes}
              onchange={(e) => preferencesStore.setMeshTopicTtlMinutes(parseInt(e.currentTarget.value) || 0)}
            />
            <button class="number-btn" onclick={() => preferencesStore.setMeshTopicTtlMinutes(preferencesStore.meshTopicTtlMinutes + 5)}>+</button>
          </div>
        </div>

        <h3 class="section-heading" style="margin-top: 20px;">maiLink Mobile Companion</h3>
        <p class="section-desc">
          maiLink is a phone app that lets you answer your agents (approve a permission, reply to
          a question, nudge one forward) and drive them — over your local network or a
          WireGuard tunnel, with no cloud in the data path. By default every agent tab is available;
          use “Tab availability” below to switch to opt-in, and each tab’s right-click menu to make
          an individual tab available or unavailable. See <code>docs/mailink-protocol.md</code>.
        </p>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="mailink-enabled">Enable maiLink bridge</label>
            <p class="setting-hint">
              Starts the on-device LAN bridge that paired phones connect to. Off by default; no
              device can connect until you enable it and pair one (below).
            </p>
          </div>
          <button
            id="mailink-enabled"
            class="toggle"
            class:active={preferencesStore.mailinkEnabled}
            onclick={() => preferencesStore.setMailinkEnabled(!preferencesStore.mailinkEnabled)}
            aria-pressed={preferencesStore.mailinkEnabled}
            aria-label="Toggle maiLink bridge"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        {#if preferencesStore.mailinkEnabled}
          <h3 class="section-heading" style="margin-top: 20px;">Tab availability</h3>
          <p class="section-desc">
            maiLink only ever surfaces agent tabs (Claude, Codex, …) — never plain shells. A tab
            whose agent has stopped (network drop, quit) stays available so you can auto-resume it
            from your phone.
          </p>

          <div class="setting" style="align-items: flex-start;">
            <div>
              <label for="mailink-expose-all">Make all tabs available in maiLink</label>
              <p class="setting-hint">
                On by default: every agent tab is available, except ones you mark “Make unavailable
                in maiLink” from the tab’s right-click menu. Turn this off to make maiLink opt-in —
                then only tabs (or whole workspaces) you explicitly mark “Make available in maiLink”
                appear on your phone.
              </p>
            </div>
            <button
              id="mailink-expose-all"
              class="toggle"
              class:active={preferencesStore.mailinkExposeAll}
              onclick={() => preferencesStore.setMailinkExposeAll(!preferencesStore.mailinkExposeAll)}
              aria-pressed={preferencesStore.mailinkExposeAll}
              aria-label="Toggle make all tabs available in maiLink"
            >
              <span class="toggle-knob"></span>
            </button>
          </div>

          <h3 class="section-heading" style="margin-top: 20px;">Doorbell (push wake)</h3>
          <p class="section-desc">
            When a designated chat needs you and no phone is actively connected, maiTerm sends a
            <em>content-free</em> wake to your paired phones — only the tab name and kind travel,
            never terminal content. The phone wakes and pulls the real content over your
            LAN/WireGuard link. This works automatically once you pair a phone and allow its
            notifications; there’s nothing to configure.
          </p>

          <div class="setting" style="flex-direction: column; align-items: stretch; gap: 6px;">
            <label for="mailink-relay-url">Custom relay URL <span style="opacity:0.6;">(optional, advanced)</span></label>
            <input
              id="mailink-relay-url"
              type="text"
              class="pattern-input"
              value={preferencesStore.mailinkRelayUrl}
              placeholder="https://updates.maiterm.dev/push  (default)"
              onchange={(e) => preferencesStore.setMailinkRelayUrl(e.currentTarget.value)}
            />
            <p class="setting-hint">
              Leave empty to use the built-in shared relay. Only set this if you self-host your own
              push relay. No secret is needed here — each phone mints its own device capability when
              it pairs.
            </p>
          </div>

          <h3 class="section-heading" style="margin-top: 20px;">Paired devices</h3>
          <p class="section-desc">
            Phones paired to this Mac. Pairing shows a one-time QR the maiLink app scans; the
            code expires in two minutes and works once. Revoke a device to stop it connecting and
            ringing.
          </p>

          <div class="mailink-devices">
            {#if mailinkDevicesLoaded && mailinkDevices.length === 0}
              <p class="setting-hint" style="margin: 0 0 4px;">No devices paired yet.</p>
            {/if}
            {#each mailinkDevices as d (d.id)}
              <div class="mailink-device">
                <div class="mailink-device-main">
                  <span class="mailink-device-name">{d.name}</span>
                  <span class="mailink-device-meta">
                    {devicePlatformLabel(d)}
                    {#if d.has_push}<span class="mailink-badge">doorbell ready</span>{/if}
                  </span>
                  <span class="mailink-device-sub">
                    paired {fmtDeviceTime(d.created_at)}{#if d.last_seen_at} · last seen {fmtDeviceTime(d.last_seen_at)}{/if}
                  </span>
                </div>
                {#if revokeConfirmId === d.id}
                  <div class="mailink-confirm">
                    <span class="confirm-delete-label">Unpair?</span>
                    <button class="confirm-delete-btn confirm-yes" onclick={() => revokeDevice(d.id)}>Unpair</button>
                    <button class="confirm-delete-btn confirm-no" onclick={() => (revokeConfirmId = null)}>Cancel</button>
                  </div>
                {:else}
                  <button class="mailink-revoke-btn" onclick={() => (revokeConfirmId = d.id)}>Revoke</button>
                {/if}
              </div>
            {/each}
          </div>

          <button class="mailink-pair-btn" onclick={startPairing} disabled={pairingBusy}>
            {pairingBusy ? 'Generating…' : 'Pair a phone'}
          </button>
          {#if pairingError}
            <p class="setting-hint" style="color: var(--red, #f7768e);">{pairingError}</p>
          {/if}
        {/if}
      {:else if activeSection === 'integrations'}
        <h3 class="section-heading">Chat Integration</h3>
        <p class="section-desc">
          Connect maiTerm to your team's chat server so agents can work threads with
          <code>/maiterm resolve &lt;permalink&gt;</code>: the thread is pulled in as a work item,
          replies are forwarded to the agent while it works, and the resolution is posted back.
          The bot account must be a member of any channel it should read or post in.
        </p>

        <div class="setting">
          <div>
            <label for="comms-provider">Provider</label>
            <p class="setting-hint">Chat platform to integrate with</p>
          </div>
          <select
            id="comms-provider"
            value={preferencesStore.commsProvider}
            onchange={(e) => preferencesStore.setCommsProvider(e.currentTarget.value)}
          >
            <option value="mattermost">Mattermost</option>
          </select>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="comms-server-url">Server URL</label>
            <p class="setting-hint">Base URL of your Mattermost server</p>
          </div>
          <input
            id="comms-server-url"
            type="text"
            class="pattern-input"
            style="max-width: 280px;"
            value={preferencesStore.commsServerUrl}
            placeholder="https://chat.example.com"
            onchange={(e) => preferencesStore.setCommsServerUrl(e.currentTarget.value)}
          />
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="comms-bot-token">Bot Token</label>
            <p class="setting-hint">
              Token of a Mattermost bot account (System Console → Integrations → Bot Accounts).
              Stored locally; never exposed to agents.
            </p>
          </div>
          <input
            id="comms-bot-token"
            type="password"
            class="pattern-input"
            style="max-width: 280px;"
            value={preferencesStore.commsBotToken}
            placeholder="bot access token"
            autocomplete="off"
            onchange={(e) => preferencesStore.setCommsBotToken(e.currentTarget.value)}
          />
        </div>

        <div class="setting" style="flex-direction: column; align-items: flex-start; gap: 10px;">
          <button
            class="backup-btn"
            onclick={handleCommsTest}
            disabled={commsTesting || !preferencesStore.commsServerUrl.trim() || !preferencesStore.commsBotToken.trim()}
          >
            {commsTesting ? 'Testing…' : 'Test Connection'}
          </button>
          {#if commsTestStatus}
            <p class="backup-status" style={commsTestOk === false ? 'color: var(--error, #f7768e);' : ''}>
              {commsTestStatus}
            </p>
          {/if}
        </div>

        <h3 class="section-heading">Message Authority</h3>

        <div class="setting comms-field">
          <div class="comms-field-desc">
            <label for="comms-authorized">Authorized usernames</label>
            <p class="setting-hint">
              When an agent is working a thread, it only acts on messages that
              <strong>@mention the bot</strong>. Those messages are scoped by default — the agent
              may investigate and reply, but won't take destructive or scope-expanding actions on a
              support request without confirming with you first. Usernames listed here are trusted:
              their @mentions carry your full authority. Matching is by Mattermost username, so this
              is only as trustworthy as your server's identities. One per line (with or without a
              leading <code>@</code>).
            </p>
          </div>
          <textarea
            id="comms-authorized"
            class="pattern-input comms-field-input"
            placeholder={'darryl\nlead-dev'}
            value={preferencesStore.commsAuthorizedUsers.join('\n')}
            onchange={(e) => preferencesStore.setCommsAuthorizedUsers(
              e.currentTarget.value
                .split('\n')
                .map((u) => u.trim().replace(/^@/, ''))
                .filter((u) => u.length > 0)
            )}
          ></textarea>
        </div>

        <div class="setting comms-field">
          <div class="comms-field-desc">
            <label for="comms-pickup">Pickup users</label>
            <p class="setting-hint">
              Usernames who can <strong>summon</strong> the bot — @mentioning it in a chat-monitored
              channel assigns that thread to the monitoring tab. Their messages still carry
              support-tier authority while the work runs. Authorized users can always summon.
              One per line (with or without a leading <code>@</code>).
            </p>
          </div>
          <textarea
            id="comms-pickup"
            class="pattern-input comms-field-input"
            placeholder={'support-jane\nsupport-bob'}
            value={preferencesStore.commsPickupUsers.join('\n')}
            onchange={(e) => preferencesStore.setCommsPickupUsers(
              e.currentTarget.value
                .split('\n')
                .map((u) => u.trim().replace(/^@/, ''))
                .filter((u) => u.length > 0)
            )}
          ></textarea>
        </div>

        <h3 class="section-heading">Response Instructions</h3>
        <p class="section-desc">
          Optional guidance for how the agent should communicate on threads — tone, formatting,
          what to include or leave out, when to post. Delivered to the agent when it picks up a
          thread, on top of the built-in defaults. (Safety rules — what the agent may act on, and
          who it takes orders from — are fixed and can't be changed here.)
        </p>

        <div class="setting" style="flex-direction: column; align-items: flex-start; gap: 8px;">
          <label for="comms-instructions">Agent chat instructions</label>
          <textarea
            id="comms-instructions"
            class="pattern-input"
            style="width: 100%; max-width: 480px; min-height: 110px;"
            placeholder={'e.g. Address the customer by name if the report includes it. Keep the support-facing summary under 4 sentences and free of jargon. Sign off as "— maiTerm bot".'}
            value={preferencesStore.commsInstructions}
            onchange={(e) => preferencesStore.setCommsInstructions(e.currentTarget.value)}
          ></textarea>
        </div>
      {:else if activeSection === 'backup'}
        <h3 class="section-heading">Backup Options</h3>
        <p class="section-desc">
          These settings apply to both manual exports and scheduled backups.
        </p>

        <div class="setting">
          <div>
            <label for="backup-exclude-scrollback">Exclude Scrollback</label>
            <p class="setting-hint">Omit terminal scrollback buffers from backup files</p>
          </div>
          <button
            class="toggle"
            class:active={preferencesStore.backupExcludeScrollback}
            onclick={() => preferencesStore.setBackupExcludeScrollback(!preferencesStore.backupExcludeScrollback)}
            aria-pressed={preferencesStore.backupExcludeScrollback}
            aria-label="Toggle exclude scrollback"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>

        <h3 class="section-heading">Manual Export / Import</h3>
        <p class="section-desc">
          Export your entire maiTerm configuration — all workspaces, tabs, preferences, triggers,
          and notes — to a JSON file. Import to restore from a backup.
        </p>

        <div class="setting" style="flex-direction: column; align-items: flex-start; gap: 12px;">
          <div style="display: flex; gap: 10px; align-items: center;">
            <button class="backup-btn" onclick={handleExportState}>Export State</button>
            <button class="backup-btn backup-btn-warn" onclick={handleImportState} title="Opens a preview where you can select workspaces and choose overwrite or merge">Import State</button>
          </div>
          {#if backupStatus}
            <p class="backup-status">{backupStatus}</p>
          {/if}
        </div>

        <h3 class="section-heading">Scheduled Backups</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="backup-dir">Backup Directory</label>
            <p class="setting-hint">
              {#if preferencesStore.backupDirectory}
                {preferencesStore.backupDirectory}
              {:else}
                No directory selected
              {/if}
            </p>
          </div>
          <div style="display: flex; gap: 6px;">
            <button id="backup-dir" class="backup-btn" onclick={handlePickDirectory}>Choose…</button>
            {#if preferencesStore.backupDirectory}
              <button class="backup-btn" onclick={() => preferencesStore.setBackupDirectory(null)}>Clear</button>
            {/if}
          </div>
        </div>

        <div class="setting">
          <label for="backup-interval">Interval</label>
          <select
            id="backup-interval"
            value={preferencesStore.backupInterval}
            onchange={(e) => preferencesStore.setBackupInterval(e.currentTarget.value)}
            disabled={!preferencesStore.backupDirectory}
          >
            <option value="off">Off</option>
            <option value="hourly">Hourly</option>
            <option value="daily">Daily</option>
            <option value="weekly">Weekly</option>
            <option value="monthly">Monthly</option>
          </select>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="backup-trim">Auto-Trim Old Backups</label>
            <p class="setting-hint">Automatically delete backups older than the selected age</p>
          </div>
          <div style="display: flex; align-items: center; gap: 8px;">
            <button
              class="toggle"
              class:active={preferencesStore.backupTrimEnabled}
              onclick={() => preferencesStore.setBackupTrimEnabled(!preferencesStore.backupTrimEnabled)}
              disabled={!preferencesStore.backupDirectory}
              aria-pressed={preferencesStore.backupTrimEnabled}
              aria-label="Toggle auto-trim old backups"
            >
              <span class="toggle-knob"></span>
            </button>
            {#if preferencesStore.backupTrimEnabled}
              <select
                value={preferencesStore.backupTrimAge}
                onchange={(e) => preferencesStore.setBackupTrimAge(e.currentTarget.value)}
                disabled={!preferencesStore.backupDirectory}
                style="min-width: auto;"
              >
                <option value="1h">1 hour</option>
                <option value="1d">1 day</option>
                <option value="1w">1 week</option>
                <option value="1m">1 month</option>
                <option value="1y">1 year</option>
              </select>
            {/if}
          </div>
        </div>
      {:else if activeSection === 'updates'}
        <h3 class="section-heading">Auto-Update</h3>

        <div class="setting">
          <div>
            <label>Current Version</label>
            <p class="setting-hint">v{appVersion}</p>
          </div>
          <button
            class="backup-btn"
            onclick={() => updaterStore.checkForUpdates(false)}
            disabled={updaterStore.checking || updaterStore.downloading}
          >
            {updaterStore.checking ? 'Checking…' : updaterStore.downloading ? 'Installing…' : 'Check Now'}
          </button>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label for="auto-check-updates">Check on Startup</label>
            <p class="setting-hint">Automatically check for updates when maiTerm launches. You will be notified via toast if an update is available.</p>
          </div>
          <button
            id="auto-check-updates"
            class="toggle"
            class:active={preferencesStore.autoCheckUpdates}
            onclick={() => preferencesStore.setAutoCheckUpdates(!preferencesStore.autoCheckUpdates)}
            aria-pressed={preferencesStore.autoCheckUpdates}
            aria-label="Toggle auto-check for updates"
          >
            <span class="toggle-knob"></span>
          </button>
        </div>
      {:else if activeSection === 'permissions'}
        <h3 class="section-heading">Full Disk Access</h3>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <label>Status</label>
            <p class="setting-hint">
              {#if fdaGranted === null}
                Checking…
              {:else if fdaGranted}
                <span class="fda-status fda-granted">Granted</span>
              {:else}
                <span class="fda-status fda-not-granted">Not Granted</span>
              {/if}
            </p>
          </div>
          <button
            class="backup-btn"
            onclick={async () => {
              await openFullDiskAccessSettings();
              setTimeout(async () => { fdaGranted = await checkFullDiskAccess(); }, 2000);
            }}
          >
            Open System Settings
          </button>
        </div>

        <div class="setting" style="align-items: flex-start;">
          <div>
            <p class="setting-hint">
              As a terminal emulator, maiTerm and its child processes (shells, CLI tools, Claude Code) need to read and write files across your system.
              Without Full Disk Access, macOS will repeatedly prompt you to allow access to individual folders.
            </p>
          </div>
        </div>

        {#if !fdaGranted}
          <div class="fda-instructions">
            <p><strong>To enable:</strong></p>
            <ol>
              <li>Click "Open System Settings" above</li>
              <li>Click the <strong>+</strong> button or toggle next to <strong>maiTerm</strong></li>
              <li>If maiTerm isn't listed, click <strong>+</strong> and select it from Applications</li>
              <li>Restart maiTerm for the change to take full effect</li>
            </ol>
          </div>
        {/if}
      {/if}
    </div>
  </div>
</div>

<ImportPreviewModal
  open={showImportPreview}
  preview={importPreview}
  filePath={importFilePath}
  onclose={() => { showImportPreview = false; }}
  onimported={() => { showImportPreview = false; window.location.reload(); }}
/>

{#if pairing}
  <div
    class="pairing-backdrop"
    role="dialog"
    aria-modal="true"
    aria-label="Pair a phone"
    tabindex="-1"
    onclick={onPairingBackdropClick}
    onkeydown={onPairingKeydown}
  >
    <div class="pairing-modal">
      <h2 class="pairing-title">Scan with maiLink</h2>
      <p class="pairing-sub">Open maiLink on your phone and scan this code to pair it with this Mac.</p>

      <div class="pairing-qr">
        <!-- eslint-disable-next-line svelte/no-at-html-tags -->
        {@html pairingQrSvg}
        {#if pairingRemaining <= 0}
          <div class="pairing-expired">
            <span>Code expired</span>
          </div>
        {/if}
      </div>

      <div class="pairing-meta">
        <div class="pairing-code">{pairing.code}</div>
        <div class="pairing-host">{pairing.host}:{pairing.port}</div>
        {#if pairingRemaining > 0}
          <div class="pairing-timer">Expires in {pairingRemaining}s</div>
        {:else}
          <div class="pairing-timer expired">Expired — generate a new code</div>
        {/if}
      </div>

      <div class="pairing-actions">
        {#if pairingRemaining <= 0}
          <button class="mailink-pair-btn" onclick={startPairing} disabled={pairingBusy}>
            {pairingBusy ? 'Generating…' : 'New code'}
          </button>
        {/if}
        <button class="pairing-done-btn" onclick={closePairing}>Done</button>
      </div>
    </div>
  </div>
{/if}

<style>
  .window {
    display: flex;
    flex-direction: column;
    height: 100vh;
    background: var(--bg-medium);
  }

  .titlebar {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 38px;
    flex-shrink: 0;
    border-bottom: 1px solid var(--bg-light);
    -webkit-app-region: drag;
    padding-left: 78px; /* space for macOS traffic lights */
    padding-right: 78px;
    user-select: none;
  }

  .title {
    font-size: 1rem;
    font-weight: 600;
    color: var(--fg);
  }

  .body {
    display: flex;
    flex: 1;
    min-height: 0;
  }

  .sidebar {
    width: 140px;
    flex-shrink: 0;
    border-right: 1px solid var(--bg-light);
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }

  .sidebar-item {
    padding: 8px 12px;
    border-radius: 4px;
    font-size: 1rem;
    color: var(--fg-dim);
    text-align: left;
    cursor: pointer;
    transition: background 0.1s, color 0.1s;
    -webkit-app-region: no-drag;
  }

  .sidebar-item:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .sidebar-item.active {
    background: var(--bg-dark);
    color: var(--fg);
  }

  .section-content {
    flex: 1;
    padding: 20px;
    overflow-y: auto;
  }

  .setting {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: 16px;
  }

  .setting:last-child {
    margin-bottom: 0;
  }

  .setting-hint {
    font-size: 0.846rem;
    color: var(--fg-dim);
    margin: 2px 0 0 0;
    line-height: 1.4;
    max-width: 260px;
  }

  .setting > label,
  .setting > .label-text {
    font-size: 1rem;
    color: var(--fg);
  }

  /* Two-column comms field: description on the left, textarea on the right. */
  .comms-field {
    align-items: flex-start;
    gap: 20px;
  }

  .comms-field-desc {
    flex: 1;
    min-width: 0;
  }

  .comms-field-desc > label {
    font-size: 1rem;
    color: var(--fg);
  }

  .comms-field-desc .setting-hint {
    max-width: none;
  }

  .comms-field-input {
    flex: 0 0 220px;
    align-self: stretch;
    min-height: 90px;
    font-family: var(--font-mono, monospace);
  }

  .number-input-wrapper {
    display: flex;
    align-items: center;
    gap: 0;
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    overflow: hidden;
  }

  .number-input {
    width: 48px;
    text-align: center;
    background: var(--bg-dark);
    border: none;
    border-left: 1px solid var(--bg-light);
    border-right: 1px solid var(--bg-light);
    padding: 6px 4px;
    font-size: 1rem;
    color: var(--fg);
    appearance: textfield;
    -moz-appearance: textfield;
  }

  .number-input::-webkit-inner-spin-button,
  .number-input::-webkit-outer-spin-button {
    -webkit-appearance: none;
    margin: 0;
  }

  .number-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 32px;
    padding: 0;
    background: var(--bg-dark);
    color: var(--fg-dim);
    font-size: 1.077rem;
    cursor: pointer;
    border: none;
    border-radius: 0;
  }

  .number-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  select {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    padding: 6px 10px;
    font-size: 1rem;
    color: var(--fg);
    cursor: pointer;
    min-width: 140px;
  }

  select:hover {
    border-color: var(--accent);
  }

  .radio-group {
    display: flex;
    gap: 16px;
  }

  .radio-label {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 1rem;
    color: var(--fg);
    cursor: pointer;
  }

  input[type="radio"] {
    cursor: pointer;
  }

  .toggle {
    position: relative;
    width: 40px;
    height: 22px;
    background: var(--bg-light);
    border-radius: 11px;
    border: none;
    cursor: pointer;
    transition: background-color 0.2s;
  }

  .toggle.active {
    background: var(--accent);
  }

  .toggle-knob {
    position: absolute;
    top: 2px;
    left: 2px;
    width: 18px;
    height: 18px;
    background: white;
    border-radius: 50%;
    transition: transform 0.2s;
  }

  .toggle.active .toggle-knob {
    transform: translateX(18px);
  }

  .section-heading {
    font-size: 0.846rem;
    font-weight: 600;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.08em;
    margin: 0 0 8px 0;
    padding: 6px 0 6px 10px;
    border-left: 2px solid var(--accent);
  }

  .section-heading:not(:first-child) {
    margin-top: 28px;
  }

  .section-heading ~ .setting,
  .section-heading ~ .section-desc {
    margin-left: 12px;
  }

  .section-desc {
    font-size: 0.923rem;
    color: var(--fg-dim);
    margin: 0 0 16px 0;
    line-height: 1.5;
  }

  .section-desc kbd {
    background: var(--bg-dark);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.846rem;
    font-family: inherit;
  }

  .section-desc code {
    background: var(--bg-dark);
    padding: 1px 4px;
    border-radius: 3px;
    font-size: 0.846rem;
  }

  .pattern-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 6px;
  }

  .pattern-input {
    flex: 1;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    padding: 6px 10px;
    font-size: 1rem;
    font-family: 'Menlo', Monaco, monospace;
    color: var(--fg);
  }

  .pattern-input:focus {
    border-color: var(--accent);
    outline: none;
  }

  .pattern-delete {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 24px;
    height: 24px;
    padding: 0;
    color: var(--fg-dim);
    border-radius: 4px;
    font-size: 1.077rem;
  }

  .pattern-delete:hover {
    background: var(--bg-light);
    color: var(--fg);
  }
  .trigger-delete:hover {
    color: var(--red, #f7768e);
  }
  .restore-default-btn {
    font-size: 0.846rem;
    color: var(--fg);
    padding: 2px 8px;
    border-radius: 4px;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    white-space: nowrap;
    flex-shrink: 0;
  }
  .restore-default-btn:hover:not(:disabled) {
    color: var(--fg);
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 15%, var(--bg-dark));
  }
  .restore-default-btn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .confirm-delete {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 0.923rem;
  }
  .confirm-delete-label {
    color: var(--red, #f7768e);
  }
  .confirm-delete-btn {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 0.846rem;
    cursor: pointer;
  }
  .confirm-yes {
    background: var(--red, #f7768e);
    color: var(--bg-dark);
  }
  .confirm-yes:hover {
    opacity: 0.85;
  }
  .confirm-no {
    background: var(--bg-light);
    color: var(--fg);
  }
  .confirm-no:hover {
    background: var(--fg-dim);
  }

  /* maiLink paired-devices list + pairing modal */
  .mailink-devices {
    display: flex;
    flex-direction: column;
    gap: 6px;
    margin-top: 8px;
  }
  .mailink-device {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 8px 10px;
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    background: var(--bg-dark);
  }
  .mailink-device-main {
    display: flex;
    flex-direction: column;
    gap: 2px;
    min-width: 0;
  }
  .mailink-device-name {
    font-size: 0.923rem;
    font-weight: 600;
    color: var(--fg);
  }
  .mailink-device-meta {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 0.846rem;
    color: var(--fg-dim);
  }
  .mailink-device-sub {
    font-size: 0.77rem;
    color: var(--fg-dim);
    opacity: 0.8;
  }
  .mailink-badge {
    font-size: 0.72rem;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 999px;
    background: color-mix(in srgb, var(--green, #9ece6a) 22%, transparent);
    color: var(--green, #9ece6a);
  }
  .mailink-revoke-btn {
    flex-shrink: 0;
    padding: 4px 10px;
    border-radius: 4px;
    font-size: 0.846rem;
    cursor: pointer;
    background: var(--bg-light);
    color: var(--fg);
    border: none;
  }
  .mailink-revoke-btn:hover {
    background: var(--red, #f7768e);
    color: var(--bg-dark);
  }
  .mailink-confirm {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-shrink: 0;
  }
  .mailink-pair-btn {
    margin-top: 12px;
    padding: 7px 16px;
    border-radius: 6px;
    font-size: 0.923rem;
    font-weight: 600;
    cursor: pointer;
    background: var(--accent);
    color: var(--bg-dark);
    border: none;
  }
  .mailink-pair-btn:hover { opacity: 0.9; }
  .mailink-pair-btn:disabled { opacity: 0.55; cursor: default; }

  .pairing-backdrop {
    position: fixed;
    inset: 0;
    background: rgba(0, 0, 0, 0.55);
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 1000;
  }
  .pairing-modal {
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 12px;
    padding: 24px;
    width: 340px;
    max-width: calc(100vw - 40px);
    display: flex;
    flex-direction: column;
    align-items: center;
    box-shadow: 0 16px 48px rgba(0, 0, 0, 0.45);
  }
  .pairing-title {
    margin: 0;
    font-size: 1.15rem;
    color: var(--fg);
  }
  .pairing-sub {
    margin: 6px 0 16px;
    font-size: 0.846rem;
    color: var(--fg-dim);
    text-align: center;
  }
  .pairing-qr {
    position: relative;
    width: 220px;
    height: 220px;
    padding: 12px;
    background: #ffffff;
    border-radius: 8px;
    box-sizing: border-box;
  }
  .pairing-qr :global(svg) {
    width: 100%;
    height: 100%;
    display: block;
  }
  .pairing-expired {
    position: absolute;
    inset: 12px;
    display: flex;
    align-items: center;
    justify-content: center;
    background: rgba(255, 255, 255, 0.86);
    color: #1a1b26;
    font-weight: 700;
    border-radius: 4px;
  }
  .pairing-meta {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    margin-top: 14px;
  }
  .pairing-code {
    font-family: var(--font-mono, monospace);
    font-size: 1.25rem;
    font-weight: 700;
    letter-spacing: 2px;
    color: var(--fg);
  }
  .pairing-host {
    font-family: var(--font-mono, monospace);
    font-size: 0.8rem;
    color: var(--fg-dim);
  }
  .pairing-timer {
    margin-top: 4px;
    font-size: 0.8rem;
    color: var(--fg-dim);
  }
  .pairing-timer.expired { color: var(--red, #f7768e); }
  .pairing-actions {
    display: flex;
    gap: 8px;
    margin-top: 18px;
  }
  .pairing-done-btn {
    padding: 7px 18px;
    border-radius: 6px;
    font-size: 0.923rem;
    font-weight: 600;
    cursor: pointer;
    background: var(--bg-light);
    color: var(--fg);
    border: none;
  }
  .pairing-done-btn:hover { background: var(--fg-dim); }

  .pattern-actions {
    display: flex;
    justify-content: space-between;
    margin-top: 4px;
  }

  .add-pattern-btn {
    font-size: 0.923rem;
    color: var(--fg-dim);
    padding: 4px 8px;
    border-radius: 4px;
  }

  .add-pattern-btn:hover {
    background: var(--bg-light);
    color: var(--fg);
  }

  .theme-grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 8px;
  }

  .theme-swatch {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px;
    border-radius: 6px;
    border: 2px solid transparent;
    background: var(--bg-dark);
    cursor: pointer;
    transition: border-color 0.15s;
  }

  .theme-swatch:hover {
    border-color: var(--bg-light);
  }

  .theme-swatch.active {
    border-color: var(--accent);
  }

  .swatch-colors {
    display: flex;
    gap: 2px;
    height: 20px;
    border-radius: 3px;
    overflow: hidden;
  }

  .swatch-bar {
    flex: 1;
  }

  .swatch-label {
    font-size: 0.846rem;
    color: var(--fg-dim);
    text-align: center;
  }

  .theme-swatch.active .swatch-label {
    color: var(--fg);
  }

  .swatch-delete {
    position: absolute;
    top: 4px;
    right: 4px;
    width: 18px;
    height: 18px;
    padding: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 0.923rem;
    color: var(--fg-dim);
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 50%;
    cursor: pointer;
    opacity: 0;
    transition: opacity 0.15s;
  }

  .theme-swatch:hover .swatch-delete {
    opacity: 1;
  }

  .swatch-delete:hover {
    color: var(--red, #f7768e);
    border-color: var(--red, #f7768e);
  }

  .new-theme {
    border: 2px dashed var(--bg-light);
    background: transparent;
    align-items: center;
    justify-content: center;
  }

  .new-theme:hover {
    border-color: var(--accent);
  }

  .new-theme-icon {
    font-size: 1.846rem;
    color: var(--fg-dim);
    height: 20px;
    display: flex;
    align-items: center;
    justify-content: center;
  }

  .new-theme:hover .new-theme-icon {
    color: var(--accent);
  }

  /* Triggers */
  .trigger-card {
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    margin-bottom: 8px;
  }

  .trigger-header {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 10px;
    border-radius: 6px;
  }
  .trigger-header-expanded {
    background: var(--bg-light);
    border-radius: 6px 6px 0 0;
  }
  .trigger-header-expanded .trigger-chevron,
  .trigger-header-expanded .trigger-delete {
    color: var(--fg);
  }

  .toggle.small {
    width: 32px;
    height: 18px;
    border-radius: 9px;
    flex-shrink: 0;
  }

  .toggle.small .toggle-knob {
    width: 14px;
    height: 14px;
  }

  .toggle.small.active .toggle-knob {
    transform: translateX(14px);
  }

  .trigger-name-btn {
    flex: 1;
    text-align: left;
    font-size: 1rem;
    color: var(--fg);
    padding: 4px 0;
    cursor: pointer;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .trigger-name-btn:hover {
    color: var(--accent);
  }

  .trigger-chevron {
    color: var(--fg-dim);
    flex-shrink: 0;
    transition: transform 0.15s;
  }

  .trigger-chevron.expanded {
    transform: rotate(90deg);
  }

  .trigger-body {
    padding: 8px 10px 12px;
    border-top: 1px solid var(--bg-light);
    display: flex;
    flex-direction: column;
    gap: 12px;
  }

  .trigger-section {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding-top: 8px;
    border-top: 1px solid color-mix(in srgb, var(--bg-light) 50%, transparent);
  }

  .trigger-section-heading {
    font-size: 0.846rem;
    font-weight: 600;
    color: var(--fg-dim);
    text-transform: uppercase;
    letter-spacing: 0.05em;
    margin: 0;
  }

  .trigger-inline-fields {
    display: flex;
    gap: 10px;
    align-items: flex-start;
  }

  .trigger-field {
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .trigger-field > label {
    font-size: 0.923rem;
    color: var(--fg-dim);
  }

  .field-hint {
    font-size: 0.846rem;
    color: var(--fg-dim);
    opacity: 0.7;
  }

  .pattern-label-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }

  .match-mode-select {
    font-size: 0.846rem;
    padding: 2px 6px;
    min-width: auto;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    cursor: pointer;
  }

  .match-mode-select:hover {
    border-color: var(--accent);
    color: var(--fg);
  }

  .pattern-input.mono {
    font-family: 'Menlo', Monaco, monospace;
  }

  .tab-selector {
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    overflow: hidden;
  }

  .tab-selector-bar {
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 4px;
    background: var(--bg-dark);
    border-bottom: 1px solid var(--bg-light);
  }

  .tab-search-input {
    flex: 1;
    min-width: 0;
    background: var(--bg-medium);
    border: 1px solid var(--bg-light);
    border-radius: 3px;
    color: var(--fg);
    font-size: 0.923rem;
    padding: 3px 6px;
    outline: none;
  }

  .tab-search-input:focus {
    border-color: var(--accent);
  }

  .tab-show-selected-cb {
    flex: none;
    cursor: pointer;
    accent-color: var(--accent);
  }

  .tab-active-btn {
    flex: none;
    padding: 2px 8px;
    border-radius: 3px;
    font-size: 0.846rem;
    color: var(--accent);
    border: 1px solid var(--accent);
    background: transparent;
    cursor: pointer;
    white-space: nowrap;
  }

  .tab-active-btn:hover {
    background: color-mix(in srgb, var(--accent) 15%, var(--bg-dark));
  }

  .tab-selector-list {
    max-height: 140px;
    overflow-y: auto;
    background: var(--bg-dark);
  }

  .tab-selector-item {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 3px 8px;
    font-size: 0.923rem;
    color: var(--fg-dim);
    cursor: pointer;
    user-select: none;
  }

  .tab-selector-item:hover {
    background: var(--bg-medium);
  }

  .tab-selector-item.selected {
    color: var(--fg);
  }

  .tab-selector-item input[type="checkbox"] {
    flex: none;
    accent-color: var(--accent);
  }

  .tab-selector-label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .tab-active-badge {
    flex: none;
    font-size: 0.769rem;
    color: var(--accent);
    padding: 0 4px;
    border: 1px solid var(--accent);
    border-radius: 3px;
    line-height: 1.4;
  }

  .deselect-all-btn {
    display: block;
    width: 100%;
    padding: 3px 8px;
    font-size: 0.846rem;
    color: var(--fg-dim);
    background: none;
    border: none;
    border-top: 1px solid var(--bg-light);
    cursor: pointer;
    text-align: center;
  }

  .deselect-all-btn:hover {
    color: var(--fg);
    background: var(--bg-medium);
  }

  .action-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 4px;
  }

  .action-type-select {
    flex: none;
    width: 140px;
  }

  .action-command-input {
    flex: 1;
    min-width: 0;
  }

  .notify-fields {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }

  .notify-help {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 18px;
    height: 18px;
    border-radius: 50%;
    background: var(--bg-light);
    color: var(--fg-dim);
    font-size: 0.846rem;
    font-weight: 600;
    cursor: help;
    flex-shrink: 0;
  }

  .var-row {
    display: flex;
    align-items: center;
    gap: 6px;
    margin-bottom: 4px;
  }

  .var-name-input {
    flex: 1;
    min-width: 0;
  }

  .var-arrow {
    font-size: 0.846rem;
    color: var(--fg-dim);
    white-space: nowrap;
  }

  .var-idx-input {
    width: 40px;
    flex: none;
    appearance: textfield;
    -moz-appearance: textfield;
  }
  .var-idx-input::-webkit-inner-spin-button,
  .var-idx-input::-webkit-outer-spin-button,
  .no-spinner::-webkit-inner-spin-button,
  .no-spinner::-webkit-outer-spin-button {
    -webkit-appearance: none;
    margin: 0;
  }
  .no-spinner {
    appearance: textfield;
    -moz-appearance: textfield;
  }
  .var-field-invalid {
    border-color: var(--red, #f7768e) !important;
    color: var(--red, #f7768e);
  }
  .var-row-wrap {
    display: contents;
  }
  .var-error {
    font-size: 0.846rem;
    color: var(--red, #f7768e);
    margin: -2px 0 4px 0;
  }

  .var-template-input {
    flex: 1;
    min-width: 0;
  }

  .checkbox-label {
    display: flex;
    align-items: center;
    gap: 4px;
    font-size: 0.923rem;
    color: var(--fg-dim);
    cursor: pointer;
    user-select: none;
  }

  .checkbox-label input[type="checkbox"] {
    accent-color: var(--accent);
    cursor: pointer;
  }

  .backup-btn {
    padding: 8px 16px;
    border-radius: 4px;
    font-size: 1rem;
    color: var(--fg);
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    cursor: pointer;
  }

  .backup-btn:hover {
    border-color: var(--accent);
    background: color-mix(in srgb, var(--accent) 10%, var(--bg-dark));
  }

  .backup-btn-warn:hover {
    border-color: var(--yellow, #e0af68);
    background: color-mix(in srgb, var(--yellow, #e0af68) 10%, var(--bg-dark));
  }

  .fda-status {
    font-weight: 600;
    font-size: 0.923rem;
  }

  .fda-granted {
    color: var(--green, #9ece6a);
  }

  .fda-not-granted {
    color: var(--yellow, #e0af68);
  }

  .fda-instructions {
    margin-top: 8px;
    padding: 12px 16px;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 6px;
    font-size: 0.923rem;
    color: var(--fg-dim);
  }

  .fda-instructions p {
    margin: 0 0 8px 0;
    color: var(--fg);
  }

  .fda-instructions ol {
    margin: 0;
    padding-left: 20px;
    line-height: 1.7;
  }

  .backup-status {
    font-size: 0.923rem;
    color: var(--fg-dim);
    margin: 0;
  }

  .sound-picker {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .preview-sound-btn {
    display: flex;
    align-items: center;
    justify-content: center;
    width: 28px;
    height: 28px;
    padding: 0;
    background: var(--bg-dark);
    border: 1px solid var(--bg-light);
    border-radius: 4px;
    color: var(--fg-dim);
    font-size: 0.923rem;
    cursor: pointer;
  }

  .preview-sound-btn:hover {
    border-color: var(--accent);
    color: var(--fg);
  }

  .volume-wrapper {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .volume-slider {
    width: 120px;
    accent-color: var(--accent);
    cursor: pointer;
  }

  .volume-label {
    font-size: 0.923rem;
    color: var(--fg-dim);
    min-width: 32px;
    text-align: right;
  }
</style>
