/**
 * Tracks SSH sessions that dropped *unexpectedly* (network outage), as opposed
 * to a clean user-initiated logout. Drives the per-tab "disconnected" badge in
 * TerminalTabs, which the user can click to reconnect.
 *
 * Detection lives in TerminalPane.svelte (exit-code 255 via OSC 133, with an
 * ssh stderr-phrase fallback). This store only holds the resulting state +
 * the context needed to reconnect.
 */
export interface DisconnectInfo {
  /** Hostname for display (parsed from the ssh command), if known. */
  host: string | null;
  /** Cleaned ssh command (bare "user@host [flags]") to replay on reconnect. */
  sshCommand: string | null;
  /** Remote cwd to cd into on reconnect, if known. */
  remoteCwd: string | null;
  /** Remote (Claude-set) title at the moment of the drop, preserved on the tab. */
  title: string | null;
  /** Timestamp of the drop (ms). */
  at: number;
}

/**
 * Unattended reconnects in flight at once. A window reload after an outage finds every
 * ssh tab dropped at the same moment — hundreds of them — and each reconnect is an
 * interactive ssh plus a bridge setup ssh to the same host. Four keeps sshd's MaxStartups
 * and the tunnel's port allocation out of it; one would take minutes.
 */
const MAX_CONCURRENT_RECONNECTS = 4;

function createSshDisconnectStore() {
  let disconnected = $state<Map<string, DisconnectInfo>>(new Map());
  let running = 0;
  const waiting: (() => void)[] = [];

  return {
    /** Run `fn` once a reconnect slot is free (see MAX_CONCURRENT_RECONNECTS). */
    async throttle<T>(fn: () => Promise<T>): Promise<T> {
      if (running >= MAX_CONCURRENT_RECONNECTS) {
        await new Promise<void>(resolve => waiting.push(resolve));
      }
      running++;
      try {
        return await fn();
      } finally {
        running--;
        waiting.shift()?.();
      }
    },

    /** Reactive accessor — reading this in a template/`$derived` tracks changes. */
    get map() { return disconnected; },

    isDisconnected(tabId: string): boolean {
      return disconnected.has(tabId);
    },

    getInfo(tabId: string): DisconnectInfo | undefined {
      return disconnected.get(tabId);
    },

    mark(tabId: string, info: DisconnectInfo) {
      disconnected = new Map(disconnected);
      disconnected.set(tabId, info);
    },

    clear(tabId: string) {
      if (!disconnected.has(tabId)) return;
      disconnected = new Map(disconnected);
      disconnected.delete(tabId);
    },
  };
}

export const sshDisconnectStore = createSshDisconnectStore();
