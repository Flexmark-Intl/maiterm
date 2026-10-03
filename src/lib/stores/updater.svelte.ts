import { check, type Update } from '@tauri-apps/plugin-updater';
import { relaunch } from '@tauri-apps/plugin-process';
import { getVersion } from '@tauri-apps/api/app';
import { invoke } from '@tauri-apps/api/core';
import { emit, listen, type UnlistenFn } from '@tauri-apps/api/event';
import { toastStore } from './toasts.svelte';
import { terminalsStore } from './terminals.svelte';
import * as commands from '$lib/tauri/commands';
import { info as logInfo, error as logError } from '@tauri-apps/plugin-log';
import type { ChangelogEntry, ChangelogItem } from '$lib/components/ChangelogModal.svelte';

interface GitHubRelease {
  tag_name: string;
  body: string | null;
}

/** Compare semver strings. Returns true if a > b. */
function isNewerVersion(a: string, b: string): boolean {
  const pa = a.replace(/^v/, '').split('.').map(Number);
  const pb = b.replace(/^v/, '').split('.').map(Number);
  for (let i = 0; i < 3; i++) {
    if ((pa[i] ?? 0) > (pb[i] ?? 0)) return true;
    if ((pa[i] ?? 0) < (pb[i] ?? 0)) return false;
  }
  return false;
}

/** Parse a GitHub release body into changelog items. Release notes are the CHANGELOG
 *  section verbatim, so this has to read the same shapes the bundled parser does —
 *  `###` sections and nested bullets included, or a release fetched from GitHub renders
 *  differently from the same release read out of the app's own CHANGELOG.md. */
function parseReleaseBody(body: string): ChangelogItem[] {
  const items: ChangelogItem[] = [];
  const strip = (s: string) => s.replace(/`([^`]+)`/g, '$1');
  // Consecutive prose lines form one paragraph — release bodies are often hard-wrapped.
  let para: string[] = [];
  const flushPara = () => {
    if (para.length) items.push({ kind: 'para', text: strip(para.join(' ')) });
    para = [];
  };
  for (const line of body.split('\n')) {
    const heading = line.match(/^#{3,} (.+)/);
    if (heading) {
      flushPara();
      items.push({ kind: 'heading', text: strip(heading[1]) });
      continue;
    }
    const bullet = line.match(/^(\s*)[-*] (.+)/);
    if (bullet) {
      flushPara();
      items.push({
        kind: 'bullet',
        text: strip(bullet[2]),
        depth: Math.min(1, Math.floor(bullet[1].length / 2)),
      });
      continue;
    }
    // A `## vX.Y.Z` line can lead a body pasted straight from CHANGELOG.md; it names the
    // version the entry already carries, so it is dropped rather than shown as prose.
    if (/^#{1,2} /.test(line)) {
      flushPara();
      continue;
    }
    if (line.trim() === '') flushPara();
    else para.push(line.trim());
  }
  flushPara();
  if (items.some(i => i.kind === 'bullet' || i.kind === 'para')) return items;
  // Fallback: a release body written as bare paragraph(s) with no bullet markers
  // (e.g. a single-fix release) would otherwise parse to zero items and be dropped
  // from the What's New modal entirely. Treat each non-empty, non-heading line as an item.
  return body.split(/\n\s*\n/)
    .map(p => p.trim().replace(/\s*\n\s*/g, ' '))
    .filter(p => p.length > 0 && !p.startsWith('#'))
    .map(p => ({ kind: 'bullet' as const, text: p.replace(/`([^`]+)`/g, '$1'), depth: 0 }));
}

/** How long restart() waits for its state flush before relaunching regardless. */
const RESTART_FLUSH_TIMEOUT_MS = 15_000;

/**
 * The updater is one per PROCESS (a single install, a single relaunch) but this store is one
 * per webview, so each window broadcasts its phase changes and the others mirror them —
 * otherwise a second window still offers Install after the first has installed, and
 * clicking it downloads the whole update again.
 */
const SYNC_EVENT = 'updater-sync';
/** A window that just opened asks; any window mid-update answers with its state. */
const SYNC_REQUEST_EVENT = 'updater-sync-request';
/** Progress is re-broadcast at most this often — a chunk event per window per chunk is noise. */
const SYNC_PROGRESS_INTERVAL_MS = 250;

type SyncPhase = 'idle' | 'downloading' | 'installing' | 'installed' | 'restarting';

interface UpdaterSync {
  origin: string;
  phase: SyncPhase;
  version: string | null;
  downloadedBytes: number;
  totalBytes: number | null;
}

/** Tells this webview's own broadcasts apart from other windows' (emit reaches the sender too). */
const SYNC_ORIGIN = crypto.randomUUID();

function createUpdaterStore() {
  let checking = $state(false);
  let downloading = $state(false);
  let installed = $state(false);
  /** Download done, the bundle swap under way — the phase a hang would sit in. */
  let installing = $state(false);
  /** restart() is flushing state / relaunching — set once, cleared only if relaunch fails. */
  let restarting = $state(false);
  /** The version being downloaded/installed/installed — set here or by another window's
   *  broadcast, so it is known even in a window whose own check never ran. */
  let activeVersion = $state<string | null>(null);
  let lastProgressBroadcast = 0;
  /** This window ran the download or the restart itself, rather than mirroring one. */
  let ownsUpdate = false;
  let downloadedBytes = $state(0);
  /** Null when the server sent no Content-Length. */
  let totalBytes = $state<number | null>(null);
  let currentUpdate = $state<Update | null>(null);
  let dismissed = $state(false);
  let releaseNotes = $state<ChangelogEntry[]>([]);
  let loadingNotes = $state(false);
  let showWhatsNewRequested = $state(false);

  async function checkForUpdates(silent = false): Promise<Update | null> {
    if (downloading) {
      // A manual check mid-install used to return with no word at all — and if the
      // banner had been dismissed, nothing on screen said an install was running.
      if (!silent) {
        dismissed = false;
        toastStore.addToast('Update In Progress', `v${activeVersion ?? currentUpdate?.version} is still ${installing ? 'installing' : 'downloading'}.`, 'info');
      }
      return null;
    }
    if (installed) {
      // Installed (here or in another window): a check would only find the same update
      // and offer to install it again.
      if (!silent) {
        dismissed = false;
        toastStore.addToast('Update Installed', `v${activeVersion ?? currentUpdate?.version} is installed — restart to apply it.`, 'info');
      }
      return null;
    }
    if (checking) return null;
    checking = true;
    try {
      const update = await check();
      if (update) {
        currentUpdate = update;
        dismissed = false;
        logInfo(`Update available: v${update.version}`);
        if (!silent) {
          toastStore.addToast(
            'Update Available',
            `v${update.version} is ready — click to review`,
            'info',
            undefined,
            undefined,
            () => { showWhatsNewRequested = true; },
          );
        }
      } else if (!silent) {
        toastStore.addToast('Up to Date', 'You are running the latest version.', 'success');
      }
      return update;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Update check failed: ${msg}`);
      if (!silent) {
        toastStore.addToast('Update Check Failed', msg, 'error');
      }
      return null;
    } finally {
      checking = false;
    }
  }

  async function fetchReleaseNotes(): Promise<ChangelogEntry[]> {
    loadingNotes = true;
    try {
      const currentVersion = await getVersion();
      const res = await fetch('https://api.github.com/repos/Flexmark-Intl/maiterm/releases');
      if (!res.ok) throw new Error(`GitHub API: ${res.status}`);
      const releases: GitHubRelease[] = await res.json();

      const entries: ChangelogEntry[] = releases
        .filter(r => isNewerVersion(r.tag_name, currentVersion) && r.body)
        .map(r => ({
          version: r.tag_name.replace(/^v/, ''),
          items: parseReleaseBody(r.body!),
        }))
        .filter(e => e.items.length > 0);

      releaseNotes = entries;
      return entries;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Failed to fetch release notes: ${msg}`);
      return [];
    } finally {
      loadingNotes = false;
    }
  }

  /** Re-check for updates and return the newer update if one exists beyond currentUpdate */
  async function recheckForNewer(): Promise<Update | null> {
    if (!currentUpdate) return null;
    try {
      const freshUpdate = await check();
      if (freshUpdate && isNewerVersion(freshUpdate.version, currentUpdate.version)) {
        return freshUpdate;
      }
    } catch (e) {
      logError(`Re-check failed: ${e instanceof Error ? e.message : String(e)}`);
    }
    return null;
  }

  /** Switch currentUpdate to a different update object (e.g. a newer one found during re-check) */
  function switchToUpdate(update: Update) {
    currentUpdate = update;
  }

  /** This window's phase as the other windows should mirror it. */
  function syncPhase(): SyncPhase {
    if (restarting) return 'restarting';
    if (installing) return 'installing';
    if (downloading) return 'downloading';
    if (installed) return 'installed';
    return 'idle';
  }

  function broadcast() {
    lastProgressBroadcast = Date.now();
    const payload: UpdaterSync = {
      origin: SYNC_ORIGIN,
      phase: syncPhase(),
      version: activeVersion,
      downloadedBytes,
      totalBytes,
    };
    emit(SYNC_EVENT, payload).catch((e) => logError(`updater sync broadcast failed: ${e}`));
  }

  /** Clears a mirrored download whose window went away mid-download (closed, reloaded) —
   *  without it every other window would read "Downloading…" until the app restarts. */
  let mirrorWatchdog: ReturnType<typeof setTimeout> | undefined;
  /** Progress arrives every 250ms while downloading; the bundle swap after it is the long
   *  silent stretch, and it takes seconds, not minutes. */
  const MIRROR_STALE_MS = 120_000;

  function applySync(s: UpdaterSync) {
    if (s.origin === SYNC_ORIGIN) return;
    clearTimeout(mirrorWatchdog);
    if (s.version) activeVersion = s.version;
    downloading = s.phase === 'downloading' || s.phase === 'installing';
    installing = s.phase === 'installing';
    downloadedBytes = s.downloadedBytes;
    totalBytes = s.totalBytes;
    // An install is never undone (a later failed download leaves the earlier one on disk),
    // so a mirror only ever sets this.
    if (s.phase === 'installed' || s.phase === 'restarting') installed = true;
    restarting = s.phase === 'restarting';
    if (s.phase !== 'idle') dismissed = false;
    if (downloading) {
      mirrorWatchdog = setTimeout(() => {
        logInfo('Update: no word from the installing window, clearing its mirrored download');
        downloading = false;
        installing = false;
      }, MIRROR_STALE_MS);
    }
  }

  let syncUnlisteners: UnlistenFn[] = [];

  /** Start mirroring the other windows' updater state. Idempotent; returns a cleanup. */
  async function initSync(): Promise<() => void> {
    if (syncUnlisteners.length === 0) {
      syncUnlisteners = await Promise.all([
        listen<UpdaterSync>(SYNC_EVENT, (e) => applySync(e.payload)),
        listen<string>(SYNC_REQUEST_EVENT, (e) => {
          // A download in flight is answered only first-hand (a mirror's copy may be stale);
          // an install is never undone, so any window holding one may answer for it.
          const phase = syncPhase();
          if (e.payload !== SYNC_ORIGIN && phase !== 'idle' && (ownsUpdate || phase === 'installed')) broadcast();
        }),
      ]);
      emit(SYNC_REQUEST_EVENT, SYNC_ORIGIN).catch(() => {});
    }
    return () => {
      syncUnlisteners.forEach((u) => u());
      syncUnlisteners = [];
      clearTimeout(mirrorWatchdog);
    };
  }

  /** Returns true only when THIS call downloaded and installed the update — a caller that
   *  restarts on success must not restart on an install some earlier call made. */
  async function downloadAndInstall(): Promise<boolean> {
    if (!currentUpdate || downloading || restarting) return false;
    downloading = true;
    installing = false;
    downloadedBytes = 0;
    totalBytes = null;
    ownsUpdate = true;
    const version = currentUpdate.version;
    activeVersion = version;
    broadcast();
    logInfo(`Update v${version}: download started`);
    try {
      await currentUpdate.downloadAndInstall((event) => {
        if (event.event === 'Started') {
          totalBytes = event.data.contentLength ?? null;
          broadcast();
        } else if (event.event === 'Progress') {
          downloadedBytes += event.data.chunkLength;
          if (Date.now() - lastProgressBroadcast >= SYNC_PROGRESS_INTERVAL_MS) broadcast();
        } else if (event.event === 'Finished') {
          installing = true;
          broadcast();
          logInfo(`Update v${version}: downloaded ${downloadedBytes} bytes, installing`);
        }
      });
      installed = true;
      // The finished state carries the Restart button; a banner dismissed mid-download
      // would otherwise hide it, leaving an installed update that only a quit reveals.
      dismissed = false;
      logInfo(`Update v${version}: installed`);
      return true;
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Update install failed: ${msg}`);
      toastStore.addToast('Update Failed', msg, 'error');
      return false;
    } finally {
      downloading = false;
      installing = false;
      broadcast();
    }
  }

  function dismiss() {
    dismissed = true;
  }

  /**
   * Flush state to disk, then relaunch. relaunch() hard-kills the process
   * without firing onCloseRequested/quit-requested, so the normal shutdown
   * save path never runs — we must mirror it here or recently-changed state
   * (tab names, scrollback, geometry) is lost across the update.
   */
  async function restart() {
    // Re-entry guard: the flush below takes seconds with many tabs, and every extra click
    // used to start another one racing the first.
    if (restarting) return;
    restarting = true;
    ownsUpdate = true;
    dismissed = false;
    broadcast();
    const flush = (async () => {
      try {
        // 0 is "no answer" (displays asleep, list unreadable) and saveWindowGeometry
        // refuses it — never fabricate a count here, or the update plants a layout the
        // user never arranged under a real monitor-count key.
        const monitorCount = await commands.getMonitorCount().catch(() => 0);
        await commands.saveWindowGeometry(monitorCount).catch(() => {});
        // This window's own terminals (also sets the shutting-down flag that
        // suppresses per-tab autosave races).
        await terminalsStore.saveAllScrollback();
        // Every OTHER window's terminals too: terminalsStore is per-webview, so the
        // call above only covered this window. Rust owns all buffers and flushes
        // them globally — without this a secondary window returns with blank tabs.
        await commands.saveAllScrollback().catch((e) => logError(`save_all_scrollback failed: ${e}`));
        await invoke('sync_state');
      } catch (e) {
        logError(`Pre-relaunch state flush failed: ${e instanceof Error ? e.message : String(e)}`);
      }
    })();
    // A hung save must not strand the app at "Restarting…" — the update is already on
    // disk, and a relaunch with slightly stale state beats one that never happens.
    const timedOut = await Promise.race([
      flush.then(() => false),
      new Promise<boolean>(resolve => setTimeout(() => resolve(true), RESTART_FLUSH_TIMEOUT_MS)),
    ]);
    if (timedOut) logError(`Pre-relaunch state flush still running after ${RESTART_FLUSH_TIMEOUT_MS}ms, relaunching anyway`);
    logInfo('Update: relaunching');
    try {
      await relaunch();
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      logError(`Relaunch failed: ${msg}`);
      // The flush set the shutting-down flag, so autosave is off from here: quitting is
      // the right advice, not another Restart click.
      toastStore.addToast('Restart Failed', `${msg}. Quit and reopen maiTerm to finish the update.`, 'error');
      restarting = false;
      broadcast();
    }
  }

  return {
    get checking() { return checking; },
    get downloading() { return downloading; },
    get installed() { return installed; },
    get installing() { return installing; },
    get restarting() { return restarting; },
    /** The version in flight or installed, else the one on offer. */
    get version() { return activeVersion ?? currentUpdate?.version ?? null; },
    get downloadedBytes() { return downloadedBytes; },
    get totalBytes() { return totalBytes; },
    get currentUpdate() { return currentUpdate; },
    get dismissed() { return dismissed; },
    get releaseNotes() { return releaseNotes; },
    get loadingNotes() { return loadingNotes; },
    /** True when the banner should be visible */
    get showBanner() { return (currentUpdate !== null || installed || downloading || restarting) && !dismissed; },
    /** True when a toast click requested showing the What's New modal */
    get showWhatsNewRequested() { return showWhatsNewRequested; },
    checkForUpdates,
    recheckForNewer,
    switchToUpdate,
    downloadAndInstall,
    fetchReleaseNotes,
    dismiss,
    restart,
    initSync,
    requestShowWhatsNew() { showWhatsNewRequested = true; },
    clearShowWhatsNewRequest() { showWhatsNewRequested = false; },
  };
}

export const updaterStore = createUpdaterStore();
