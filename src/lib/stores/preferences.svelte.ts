import type { CursorStyle, Preferences, Trigger } from '$lib/tauri/types';
import type { Theme } from '$lib/themes';
import { builtinThemes } from '$lib/themes';
import * as commands from '$lib/tauri/commands';

function createPreferencesStore() {
  let _resolveReady: () => void;
  const ready = new Promise<void>(r => { _resolveReady = r; });

  let uiFontSize = $state(13);
  let fontSize = $state(13);
  let fontFamily = $state('Menlo');
  let cursorStyle = $state<CursorStyle>('block');
  let cursorBlink = $state(true);
  let autoSaveInterval = $state(10);
  let scrollbackLimit = $state(10000);
  let promptPatterns = $state<string[]>([]);
  let cloneCwd = $state(true);
  let cloneScrollback = $state(true);
  let cloneSsh = $state(true);
  let cloneHistory = $state(true);
  let cloneNotes = $state(true);
  let cloneAutoResume = $state(true);
  let cloneVariables = $state(true);
  let numberDuplicatedTabs = $state(true);
  let theme = $state('tokyo-night');
  let shellTitleIntegration = $state(false);
  let shellIntegration = $state(false);
  let customThemes = $state<Theme[]>([]);
  let restoreSession = $state(true);
  let sessionRestoreMode = $state('all');
  let notificationMode = $state('auto');
  let notifyMinDuration = $state(30);
  let notesFontSize = $state(13);
  let notesFontFamily = $state('Menlo');
  let notesWidth = $state(320);
  let notesWordWrap = $state(true);
  let toastFontSize = $state(14);
  let toastWidth = $state(400);
  let toastDuration = $state(8);
  let notificationSound = $state('default');
  let notificationVolume = $state(50);
  let migrateTabNotes = $state(true);
  let notesScope = $state<'tab' | 'workspace'>('tab');
  let showRecentWorkspaces = $state(true);
  let workspaceSortOrder = $state('default');
  let showWorkspaceTabCount = $state(false);
  let tabButtonStyle = $state('hover');
  let terminalRenderer = $state('dom');
  let triggers = $state<Trigger[]>([]);
  let hiddenDefaultTriggers = $state<string[]>([]);
  let claudeTriggersPrompted = $state(false);
  let claudeCodeIde = $state(false);
  let claudeCodeIdeSsh = $state(true);
  let claudeCodeHooks = $state(true);
  let claudeCodeAutoResume = $state(true);
  let codexIde = $state(true);
  let codexIdeSsh = $state(true);
  let codexHooks = $state(true);
  let codexAutoResume = $state(true);
  let codexHooksBypassTrust = $state(false);
  let mailinkEnabled = $state(false);
  let mailinkExposeAll = $state(true);
  let mailinkRelayUrl = $state('');
  let commsProvider = $state('mattermost');
  let commsServerUrl = $state('');
  let commsBotToken = $state('');
  let composerDefaultOpen = $state(true);
  let windowsShell = $state('powershell');
  let fileLinkAction = $state('modifier_click');
  let backupDirectory = $state<string | null>(null);
  let backupInterval = $state('off');
  let backupExcludeScrollback = $state(true);
  let backupTrimEnabled = $state(false);
  let backupTrimAge = $state('1m');
  let autoSuspendMinutes = $state(0);
  let groupActiveTabs = $state(false);
  let autoCheckUpdates = $state(true);
  let quickOpenShowHidden = $state(false);
  let quickOpenShowIgnored = $state(false);
  let meshSoftCap = $state(0);
  let meshHardCap = $state(0);
  let meshTopicTtlMinutes = $state(0);

  return {
    /** Resolves once the initial load() has completed. */
    get ready() { return ready; },
    get uiFontSize() { return uiFontSize; },
    get fontSize() { return fontSize; },
    get fontFamily() { return fontFamily; },
    get cursorStyle() { return cursorStyle; },
    get cursorBlink() { return cursorBlink; },
    get autoSaveInterval() { return autoSaveInterval; },
    get scrollbackLimit() { return scrollbackLimit; },
    get promptPatterns() { return promptPatterns; },
    get cloneCwd() { return cloneCwd; },
    get cloneScrollback() { return cloneScrollback; },
    get cloneSsh() { return cloneSsh; },
    get cloneHistory() { return cloneHistory; },
    get cloneNotes() { return cloneNotes; },
    get cloneAutoResume() { return cloneAutoResume; },
    get cloneVariables() { return cloneVariables; },
    get numberDuplicatedTabs() { return numberDuplicatedTabs; },
    get theme() { return theme; },
    get shellTitleIntegration() { return shellTitleIntegration; },
    get shellIntegration() { return shellIntegration; },
    get customThemes() { return customThemes; },
    get restoreSession() { return restoreSession; },
    get sessionRestoreMode() { return sessionRestoreMode; },
    get notificationMode() { return notificationMode; },
    get notifyMinDuration() { return notifyMinDuration; },
    get notesFontSize() { return notesFontSize; },
    get notesFontFamily() { return notesFontFamily; },
    get notesWidth() { return notesWidth; },
    get notesWordWrap() { return notesWordWrap; },
    get toastFontSize() { return toastFontSize; },
    get toastWidth() { return toastWidth; },
    get toastDuration() { return toastDuration; },
    get notificationSound() { return notificationSound; },
    get notificationVolume() { return notificationVolume; },
    get migrateTabNotes() { return migrateTabNotes; },
    get notesScope() { return notesScope; },
    get showRecentWorkspaces() { return showRecentWorkspaces; },
    get workspaceSortOrder() { return workspaceSortOrder; },
    get showWorkspaceTabCount() { return showWorkspaceTabCount; },
    get tabButtonStyle() { return tabButtonStyle; },
    get terminalRenderer() { return terminalRenderer; },
    get triggers() { return triggers; },
    get hiddenDefaultTriggers() { return hiddenDefaultTriggers; },
    get claudeTriggersPrompted() { return claudeTriggersPrompted; },
    get claudeCodeIde() { return claudeCodeIde; },
    get claudeCodeIdeSsh() { return claudeCodeIdeSsh; },
    get claudeCodeHooks() { return claudeCodeHooks; },
    get claudeCodeAutoResume() { return claudeCodeAutoResume; },
    get codexIde() { return codexIde; },
    get codexIdeSsh() { return codexIdeSsh; },
    get codexHooks() { return codexHooks; },
    get codexAutoResume() { return codexAutoResume; },
    get codexHooksBypassTrust() { return codexHooksBypassTrust; },
    get mailinkEnabled() { return mailinkEnabled; },
    get mailinkExposeAll() { return mailinkExposeAll; },
    get mailinkRelayUrl() { return mailinkRelayUrl; },
    get commsProvider() { return commsProvider; },
    get commsServerUrl() { return commsServerUrl; },
    get commsBotToken() { return commsBotToken; },
    get composerDefaultOpen() { return composerDefaultOpen; },
    get windowsShell() { return windowsShell; },
    get fileLinkAction() { return fileLinkAction; },
    get backupDirectory() { return backupDirectory; },
    get backupInterval() { return backupInterval; },
    get backupExcludeScrollback() { return backupExcludeScrollback; },
    get backupTrimEnabled() { return backupTrimEnabled; },
    get backupTrimAge() { return backupTrimAge; },
    get autoSuspendMinutes() { return autoSuspendMinutes; },
    get groupActiveTabs() { return groupActiveTabs; },
    get autoCheckUpdates() { return autoCheckUpdates; },
    get quickOpenShowHidden() { return quickOpenShowHidden; },
    get quickOpenShowIgnored() { return quickOpenShowIgnored; },
    get meshSoftCap() { return meshSoftCap; },
    get meshHardCap() { return meshHardCap; },
    get meshTopicTtlMinutes() { return meshTopicTtlMinutes; },

    async load() {
      const prefs = await commands.getPreferences();
      uiFontSize = prefs.ui_font_size ?? 13;
      fontSize = prefs.font_size;
      fontFamily = prefs.font_family;
      cursorStyle = prefs.cursor_style;
      cursorBlink = prefs.cursor_blink;
      autoSaveInterval = prefs.auto_save_interval;
      scrollbackLimit = prefs.scrollback_limit;
      promptPatterns = prefs.prompt_patterns;
      // Migration: add Windows shell patterns if missing
      const windowsPatterns = ['PS \\d>', '\\d>'];
      const missingPatterns = windowsPatterns.filter(p => !promptPatterns.includes(p));
      if (missingPatterns.length > 0) {
        promptPatterns = [...promptPatterns, ...missingPatterns];
      }
      cloneCwd = prefs.clone_cwd;
      cloneScrollback = prefs.clone_scrollback;
      cloneSsh = prefs.clone_ssh;
      cloneHistory = prefs.clone_history;
      cloneNotes = prefs.clone_notes ?? true;
      cloneAutoResume = prefs.clone_auto_resume ?? true;
      cloneVariables = prefs.clone_variables ?? true;
      numberDuplicatedTabs = prefs.number_duplicated_tabs ?? true;
      theme = prefs.theme;
      shellTitleIntegration = prefs.shell_title_integration;
      shellIntegration = prefs.shell_integration ?? false;
      customThemes = prefs.custom_themes ?? [];
      restoreSession = prefs.restore_session ?? true;
      sessionRestoreMode = prefs.session_restore_mode || 'all';
      // Migration: derive notification_mode from old notify_on_completion if absent
      if (prefs.notification_mode) {
        notificationMode = prefs.notification_mode;
      } else {
        notificationMode = prefs.notify_on_completion ? 'auto' : 'disabled';
      }
      notifyMinDuration = prefs.notify_min_duration ?? 30;
      notesFontSize = prefs.notes_font_size ?? 13;
      notesFontFamily = prefs.notes_font_family ?? 'Menlo';
      notesWidth = prefs.notes_width ?? 320;
      notesWordWrap = prefs.notes_word_wrap ?? true;
      toastFontSize = prefs.toast_font_size ?? 14;
      toastWidth = prefs.toast_width ?? 400;
      toastDuration = prefs.toast_duration ?? 8;
      notificationSound = prefs.notification_sound ?? 'default';
      notificationVolume = prefs.notification_volume ?? 50;
      migrateTabNotes = prefs.migrate_tab_notes ?? true;
      notesScope = (prefs.notes_scope === 'workspace' ? 'workspace' : 'tab');
      showRecentWorkspaces = prefs.show_recent_workspaces ?? true;
      workspaceSortOrder = prefs.workspace_sort_order || 'default';
      showWorkspaceTabCount = prefs.show_workspace_tab_count ?? false;
      tabButtonStyle = prefs.tab_button_style || 'hover';
      terminalRenderer = prefs.terminal_renderer || 'dom';
      triggers = prefs.triggers ?? [];
      hiddenDefaultTriggers = prefs.hidden_default_triggers ?? [];
      claudeTriggersPrompted = prefs.claude_triggers_prompted ?? false;
      claudeCodeIde = prefs.claude_ide ?? false;
      claudeCodeIdeSsh = prefs.claude_ide_ssh ?? true;
      claudeCodeHooks = prefs.claude_hooks ?? true;
      claudeCodeAutoResume = prefs.claude_auto_resume ?? true;
      codexIde = prefs.codex_ide ?? true;
      codexIdeSsh = prefs.codex_ide_ssh ?? true;
      codexHooks = prefs.codex_hooks ?? true;
      codexAutoResume = prefs.codex_auto_resume ?? true;
      codexHooksBypassTrust = prefs.codex_hooks_bypass_trust ?? false;
      mailinkEnabled = prefs.mailink_enabled ?? false;
      mailinkExposeAll = prefs.mailink_expose_all ?? true;
      mailinkRelayUrl = prefs.mailink_relay_url ?? '';
      commsProvider = prefs.comms_provider || 'mattermost';
      commsServerUrl = prefs.comms_server_url ?? '';
      commsBotToken = prefs.comms_bot_token ?? '';
      composerDefaultOpen = prefs.composer_default_open ?? true;
      windowsShell = prefs.windows_shell ?? 'powershell';
      fileLinkAction = prefs.file_link_action ?? 'modifier_click';
      backupDirectory = prefs.backup_directory ?? null;
      backupInterval = prefs.backup_interval || 'off';
      backupExcludeScrollback = prefs.backup_exclude_scrollback ?? true;
      backupTrimEnabled = prefs.backup_trim_enabled ?? false;
      backupTrimAge = prefs.backup_trim_age || '1m';
      autoSuspendMinutes = prefs.auto_suspend_minutes ?? 0;
      groupActiveTabs = prefs.group_active_tabs ?? false;
      autoCheckUpdates = prefs.auto_check_updates ?? true;
      quickOpenShowHidden = prefs.quick_open_show_hidden ?? false;
      quickOpenShowIgnored = prefs.quick_open_show_ignored ?? false;
      meshSoftCap = prefs.mesh_soft_cap ?? 0;
      meshHardCap = prefs.mesh_hard_cap ?? 0;
      meshTopicTtlMinutes = prefs.mesh_topic_ttl_minutes ?? 0;
      _resolveReady();
    },

    async setUiFontSize(value: number) {
      uiFontSize = Math.max(10, Math.min(20, value));
      await this.save();
    },

    async setFontSize(value: number) {
      fontSize = Math.max(10, Math.min(24, value));
      await this.save();
    },

    async setFontFamily(value: string) {
      fontFamily = value;
      await this.save();
    },

    async setCursorStyle(value: CursorStyle) {
      cursorStyle = value;
      await this.save();
    },

    async setCursorBlink(value: boolean) {
      cursorBlink = value;
      await this.save();
    },

    async setAutoSaveInterval(value: number) {
      autoSaveInterval = value;
      await this.save();
    },

    async setScrollbackLimit(value: number) {
      scrollbackLimit = value;
      await this.save();
    },

    async setPromptPatterns(value: string[]) {
      promptPatterns = value;
      await this.save();
    },

    async setCloneCwd(value: boolean) {
      cloneCwd = value;
      await this.save();
    },

    async setCloneScrollback(value: boolean) {
      cloneScrollback = value;
      await this.save();
    },

    async setCloneSsh(value: boolean) {
      cloneSsh = value;
      await this.save();
    },

    async setCloneHistory(value: boolean) {
      cloneHistory = value;
      await this.save();
    },

    async setCloneNotes(value: boolean) {
      cloneNotes = value;
      await this.save();
    },

    async setCloneAutoResume(value: boolean) {
      cloneAutoResume = value;
      await this.save();
    },

    async setCloneVariables(value: boolean) {
      cloneVariables = value;
      await this.save();
    },

    async setNumberDuplicatedTabs(value: boolean) {
      numberDuplicatedTabs = value;
      await this.save();
    },

    async setTheme(value: string) {
      theme = value;
      await this.save();
    },

    async setShellTitleIntegration(value: boolean) {
      shellTitleIntegration = value;
      await this.save();
    },

    async setShellIntegration(value: boolean) {
      shellIntegration = value;
      await this.save();
    },

    async setRestoreSession(value: boolean) {
      restoreSession = value;
      await this.save();
    },

    async setSessionRestoreMode(value: string) {
      sessionRestoreMode = value;
      await this.save();
    },

    async setNotificationMode(value: string) {
      notificationMode = value;
      await this.save();
    },

    async setNotifyMinDuration(value: number) {
      notifyMinDuration = value;
      await this.save();
    },

    async setNotesFontSize(value: number) {
      notesFontSize = Math.max(10, Math.min(24, value));
      await this.save();
    },

    async setNotesFontFamily(value: string) {
      notesFontFamily = value;
      await this.save();
    },

    async setNotesWidth(value: number) {
      notesWidth = Math.max(200, value);
      await this.save();
    },

    async setNotesWordWrap(value: boolean) {
      notesWordWrap = value;
      await this.save();
    },

    async setToastFontSize(value: number) {
      toastFontSize = Math.max(10, Math.min(24, value));
      await this.save();
    },

    async setToastWidth(value: number) {
      toastWidth = Math.max(280, Math.min(600, value));
      await this.save();
    },

    async setToastDuration(value: number) {
      toastDuration = Math.max(3, Math.min(30, value));
      await this.save();
    },

    async setNotificationSound(value: string) {
      notificationSound = value;
      await this.save();
    },

    async setNotificationVolume(value: number) {
      notificationVolume = Math.max(0, Math.min(100, value));
      await this.save();
    },

    async setMigrateTabNotes(value: boolean) {
      migrateTabNotes = value;
      await this.save();
    },

    async setNotesScope(value: 'tab' | 'workspace') {
      notesScope = value;
      await this.save();
    },

    async setShowRecentWorkspaces(value: boolean) {
      showRecentWorkspaces = value;
      await this.save();
    },

    async setWorkspaceSortOrder(value: string) {
      workspaceSortOrder = value;
      await this.save();
    },

    async setShowWorkspaceTabCount(value: boolean) {
      showWorkspaceTabCount = value;
      await this.save();
    },

    async setTabButtonStyle(value: string) {
      tabButtonStyle = value;
      await this.save();
    },

    async setTerminalRenderer(value: string) {
      terminalRenderer = value;
      await this.save();
    },

    async setTriggers(value: Trigger[]) {
      triggers = value;
      await this.save();
    },

    async setHiddenDefaultTriggers(value: string[]) {
      hiddenDefaultTriggers = value;
      await this.save();
    },

    async setClaudeTriggersPrompted(value: boolean) {
      claudeTriggersPrompted = value;
      await this.save();
    },

    async setClaudeCodeHooks(value: boolean) {
      claudeCodeHooks = value;
      await this.save();
    },
    async setClaudeCodeAutoResume(value: boolean) {
      claudeCodeAutoResume = value;
      await this.save();
    },
    async setCodexIde(value: boolean) {
      codexIde = value;
      await this.save();
      await commands.refreshAgentIntegrations();
    },
    async setCodexIdeSsh(value: boolean) {
      codexIdeSsh = value;
      await this.save();
      await commands.refreshAgentIntegrations();
    },
    async setCodexHooks(value: boolean) {
      codexHooks = value;
      await this.save();
      await commands.refreshAgentIntegrations();
    },
    async setCodexAutoResume(value: boolean) {
      codexAutoResume = value;
      await this.save();
      await commands.refreshAgentIntegrations();
    },
    async setCodexHooksBypassTrust(value: boolean) {
      codexHooksBypassTrust = value;
      await this.save();
      await commands.refreshAgentIntegrations();
    },
    async setComposerDefaultOpen(value: boolean) {
      composerDefaultOpen = value;
      await this.save();
    },
    async setClaudeCodeIde(value: boolean) {
      claudeCodeIde = value;
      await this.save();
    },

    async setClaudeCodeIdeSsh(value: boolean) {
      claudeCodeIdeSsh = value;
      await this.save();
    },

    async setMailinkEnabled(value: boolean) {
      mailinkEnabled = value;
      try {
        // Dedicated command: persists the flag AND starts/stops the live LAN listener, so the
        // bridge actually comes up (pairing works) without an app restart.
        await commands.mailinkSetEnabled(value);
      } catch (e) {
        mailinkEnabled = !value; // revert the toggle if start/stop failed
        throw e;
      }
    },

    async setMailinkExposeAll(value: boolean) {
      mailinkExposeAll = value;
      await this.save();
    },

    async setMailinkRelayUrl(value: string) {
      mailinkRelayUrl = value;
      await this.save();
    },

    async setCommsProvider(value: string) {
      commsProvider = value;
      await this.save();
    },

    async setCommsServerUrl(value: string) {
      commsServerUrl = value;
      await this.save();
    },

    async setCommsBotToken(value: string) {
      commsBotToken = value;
      await this.save();
    },

    async setWindowsShell(value: string) {
      windowsShell = value;
      await this.save();
    },

    async setFileLinkAction(value: string) {
      fileLinkAction = value;
      await this.save();
    },

    async setBackupDirectory(value: string | null) {
      backupDirectory = value;
      await this.save();
    },

    async setBackupInterval(value: string) {
      backupInterval = value;
      await this.save();
    },

    async setBackupExcludeScrollback(value: boolean) {
      backupExcludeScrollback = value;
      await this.save();
    },

    async setBackupTrimEnabled(value: boolean) {
      backupTrimEnabled = value;
      await this.save();
    },

    async setBackupTrimAge(value: string) {
      backupTrimAge = value;
      await this.save();
    },

    async setAutoSuspendMinutes(value: number) {
      autoSuspendMinutes = value;
      await this.save();
    },

    async setGroupActiveTabs(value: boolean) {
      groupActiveTabs = value;
      await this.save();
    },

    async setAutoCheckUpdates(value: boolean) {
      autoCheckUpdates = value;
      await this.save();
    },

    async setQuickOpenShowHidden(value: boolean) {
      quickOpenShowHidden = value;
      await this.save();
    },

    async setQuickOpenShowIgnored(value: boolean) {
      quickOpenShowIgnored = value;
      await this.save();
    },

    // All three mesh loop-control limits use 0 = OFF and are independent (no inter-field
    // coupling): a user can disable any one without the others forcing it back on.
    async setMeshSoftCap(value: number) {
      meshSoftCap = Math.max(0, Math.round(value));
      await this.save();
    },

    async setMeshHardCap(value: number) {
      meshHardCap = Math.max(0, Math.round(value));
      await this.save();
    },

    async setMeshTopicTtlMinutes(value: number) {
      meshTopicTtlMinutes = Math.max(0, Math.round(value));
      await this.save();
    },

    async addCustomTheme(t: Theme) {
      customThemes = [...customThemes, t];
      await this.save();
    },

    async updateCustomTheme(id: string, updated: Theme) {
      customThemes = customThemes.map((t) => (t.id === id ? updated : t));
      await this.save();
    },

    async deleteCustomTheme(id: string) {
      customThemes = customThemes.filter((t) => t.id !== id);
      if (theme === id) {
        theme = builtinThemes[0].id;
      }
      await this.save();
    },

    applyFromBackend(prefs: Preferences) {
      uiFontSize = prefs.ui_font_size ?? 13;
      fontSize = prefs.font_size;
      fontFamily = prefs.font_family;
      cursorStyle = prefs.cursor_style;
      cursorBlink = prefs.cursor_blink;
      autoSaveInterval = prefs.auto_save_interval;
      scrollbackLimit = prefs.scrollback_limit;
      promptPatterns = prefs.prompt_patterns;
      cloneCwd = prefs.clone_cwd;
      cloneScrollback = prefs.clone_scrollback;
      cloneSsh = prefs.clone_ssh;
      cloneHistory = prefs.clone_history;
      cloneNotes = prefs.clone_notes ?? true;
      cloneAutoResume = prefs.clone_auto_resume ?? true;
      cloneVariables = prefs.clone_variables ?? true;
      numberDuplicatedTabs = prefs.number_duplicated_tabs ?? true;
      theme = prefs.theme;
      shellTitleIntegration = prefs.shell_title_integration;
      shellIntegration = prefs.shell_integration ?? false;
      customThemes = prefs.custom_themes ?? [];
      restoreSession = prefs.restore_session ?? true;
      sessionRestoreMode = prefs.session_restore_mode || 'all';
      if (prefs.notification_mode) {
        notificationMode = prefs.notification_mode;
      } else {
        notificationMode = prefs.notify_on_completion ? 'auto' : 'disabled';
      }
      notifyMinDuration = prefs.notify_min_duration ?? 30;
      notesFontSize = prefs.notes_font_size ?? 13;
      notesFontFamily = prefs.notes_font_family ?? 'Menlo';
      notesWidth = prefs.notes_width ?? 320;
      notesWordWrap = prefs.notes_word_wrap ?? true;
      toastFontSize = prefs.toast_font_size ?? 14;
      toastWidth = prefs.toast_width ?? 400;
      toastDuration = prefs.toast_duration ?? 8;
      notificationSound = prefs.notification_sound ?? 'default';
      notificationVolume = prefs.notification_volume ?? 50;
      migrateTabNotes = prefs.migrate_tab_notes ?? true;
      notesScope = (prefs.notes_scope === 'workspace' ? 'workspace' : 'tab');
      showRecentWorkspaces = prefs.show_recent_workspaces ?? true;
      workspaceSortOrder = prefs.workspace_sort_order || 'default';
      showWorkspaceTabCount = prefs.show_workspace_tab_count ?? false;
      tabButtonStyle = prefs.tab_button_style || 'hover';
      terminalRenderer = prefs.terminal_renderer || 'dom';
      triggers = prefs.triggers ?? [];
      hiddenDefaultTriggers = prefs.hidden_default_triggers ?? [];
      claudeTriggersPrompted = prefs.claude_triggers_prompted ?? false;
      claudeCodeIde = prefs.claude_ide ?? false;
      claudeCodeIdeSsh = prefs.claude_ide_ssh ?? true;
      claudeCodeHooks = prefs.claude_hooks ?? true;
      claudeCodeAutoResume = prefs.claude_auto_resume ?? true;
      codexIde = prefs.codex_ide ?? true;
      codexIdeSsh = prefs.codex_ide_ssh ?? true;
      codexHooks = prefs.codex_hooks ?? true;
      codexAutoResume = prefs.codex_auto_resume ?? true;
      codexHooksBypassTrust = prefs.codex_hooks_bypass_trust ?? false;
      mailinkEnabled = prefs.mailink_enabled ?? false;
      mailinkExposeAll = prefs.mailink_expose_all ?? true;
      mailinkRelayUrl = prefs.mailink_relay_url ?? '';
      commsProvider = prefs.comms_provider || 'mattermost';
      commsServerUrl = prefs.comms_server_url ?? '';
      commsBotToken = prefs.comms_bot_token ?? '';
      composerDefaultOpen = prefs.composer_default_open ?? true;
      windowsShell = prefs.windows_shell ?? 'powershell';
      fileLinkAction = prefs.file_link_action ?? 'modifier_click';
      backupDirectory = prefs.backup_directory ?? null;
      backupInterval = prefs.backup_interval || 'off';
      backupExcludeScrollback = prefs.backup_exclude_scrollback ?? true;
      backupTrimEnabled = prefs.backup_trim_enabled ?? false;
      backupTrimAge = prefs.backup_trim_age || '1m';
      autoSuspendMinutes = prefs.auto_suspend_minutes ?? 0;
      groupActiveTabs = prefs.group_active_tabs ?? false;
      autoCheckUpdates = prefs.auto_check_updates ?? true;
      quickOpenShowHidden = prefs.quick_open_show_hidden ?? false;
      quickOpenShowIgnored = prefs.quick_open_show_ignored ?? false;
      meshSoftCap = prefs.mesh_soft_cap ?? 0;
      meshHardCap = prefs.mesh_hard_cap ?? 0;
      meshTopicTtlMinutes = prefs.mesh_topic_ttl_minutes ?? 0;
    },

    async save() {
      const prefs: Preferences = {
        ui_font_size: uiFontSize,
        font_size: fontSize,
        font_family: fontFamily,
        cursor_style: cursorStyle,
        cursor_blink: cursorBlink,
        auto_save_interval: autoSaveInterval,
        scrollback_limit: scrollbackLimit,
        prompt_patterns: promptPatterns,
        clone_cwd: cloneCwd,
        clone_scrollback: cloneScrollback,
        clone_ssh: cloneSsh,
        clone_history: cloneHistory,
        clone_notes: cloneNotes,
        clone_auto_resume: cloneAutoResume,
        clone_variables: cloneVariables,
        number_duplicated_tabs: numberDuplicatedTabs,
        theme,
        shell_title_integration: shellTitleIntegration,
        shell_integration: shellIntegration,
        custom_themes: customThemes,
        restore_session: restoreSession,
        session_restore_mode: sessionRestoreMode,
        notify_on_completion: notificationMode !== 'disabled',
        notification_mode: notificationMode,
        notify_min_duration: notifyMinDuration,
        notes_font_size: notesFontSize,
        notes_font_family: notesFontFamily,
        notes_width: notesWidth,
        notes_word_wrap: notesWordWrap,
        toast_font_size: toastFontSize,
        toast_width: toastWidth,
        toast_duration: toastDuration,
        notification_sound: notificationSound,
        notification_volume: notificationVolume,
        migrate_tab_notes: migrateTabNotes,
        notes_scope: notesScope === 'workspace' ? 'workspace' : null,
        show_recent_workspaces: showRecentWorkspaces,
        workspace_sort_order: workspaceSortOrder === 'default' ? '' : workspaceSortOrder,
        show_workspace_tab_count: showWorkspaceTabCount,
        tab_button_style: tabButtonStyle === 'hover' ? '' : tabButtonStyle,
        terminal_renderer: terminalRenderer,
        triggers,
        hidden_default_triggers: hiddenDefaultTriggers,
        claude_triggers_prompted: claudeTriggersPrompted,
        claude_ide: claudeCodeIde,
        claude_ide_ssh: claudeCodeIdeSsh,
        claude_hooks: claudeCodeHooks,
        claude_auto_resume: claudeCodeAutoResume,
        codex_ide: codexIde,
        codex_ide_ssh: codexIdeSsh,
        codex_hooks: codexHooks,
        codex_auto_resume: codexAutoResume,
        codex_hooks_bypass_trust: codexHooksBypassTrust,
        mailink_enabled: mailinkEnabled,
        mailink_expose_all: mailinkExposeAll,
        mailink_relay_url: mailinkRelayUrl.trim() || null,
        comms_provider: commsProvider,
        comms_server_url: commsServerUrl.trim() || null,
        comms_bot_token: commsBotToken.trim() || null,
        composer_default_open: composerDefaultOpen,
        windows_shell: windowsShell,
        file_link_action: fileLinkAction,
        backup_directory: backupDirectory,
        backup_interval: backupInterval === 'off' ? '' : backupInterval,
        backup_exclude_scrollback: backupExcludeScrollback,
        auto_suspend_minutes: autoSuspendMinutes,
        group_active_tabs: groupActiveTabs,
        backup_trim_enabled: backupTrimEnabled,
        backup_trim_age: backupTrimAge,
        auto_check_updates: autoCheckUpdates,
        quick_open_show_hidden: quickOpenShowHidden,
        quick_open_show_ignored: quickOpenShowIgnored,
        mesh_soft_cap: meshSoftCap,
        mesh_hard_cap: meshHardCap,
        mesh_topic_ttl_minutes: meshTopicTtlMinutes,
      };
      await commands.setPreferences(prefs);
    }
  };
}

export const preferencesStore = createPreferencesStore();
