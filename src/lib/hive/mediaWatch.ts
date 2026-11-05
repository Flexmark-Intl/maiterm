/** The listeners of a `SetupWatch` (lib/hive/media.ts): the install end and the server status
 *  events, filtered to what this parent's sheets started. Both are app-wide events (a JS `listen`
 *  hears every window's `emit_to`), which is why the filter, not the event, decides. Returns the
 *  release, which also releases a `listen` that resolves after it. */
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { warn as logWarn } from '@tauri-apps/plugin-log';
import type { SetupWatch } from './media';

export function watchSetupEnds(watch: SetupWatch, onEnd: () => void): () => void {
  let live = true;
  const offs: UnlistenFn[] = [];
  const keep = <T>(event: string, own: (payload: T) => boolean) => {
    listen<T>(event, (e) => {
      if (live && own(e.payload)) onEnd();
    })
      .then((off) => (live ? offs.push(off) : off()))
      .catch((e) => void logWarn(`[mediaWatch] ${event}: not listening: ${String(e)}`));
  };
  keep<{ tab_id: string }>('hive-media-install-finished', (p) => watch.installEnded(p));
  keep<{ id: string; status?: { state?: string } | null }>('mcp-proxy-status', (p) => watch.serverStatus(p));
  return () => {
    live = false;
    for (const off of offs.splice(0)) off();
  };
}
