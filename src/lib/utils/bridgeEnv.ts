import { getRemoteBridgeEnv } from '$lib/tauri/commands';

/**
 * What each tab's ssh command actually baked, so the bridge can tell whether the guess held.
 *
 * `buildSshCommand` writes MAITERM_PORT into the remote command when the tab spawns — before
 * the tunnel for that host exists — so the value is this maiTerm's *usual* port there, not a
 * fact. It is wrong more often than that framing suggests: the tunnel's port is unique per
 * MACHINE while this map (and the book behind it) is keyed per ACCOUNT, so several accounts on
 * one box contend for the same number and are reassigned in arrival order on every launch.
 * Observed on ews@nova across three launches: 28599, then 28604, then 28607.
 *
 * A wrong port is silent — the tab looks connected and its agent simply has no MCP and no
 * hooks — so the bridge compares against what was baked and corrects the shell when they
 * differ. Deliberately a leaf module with no store imports: the bridge store already imports
 * the trigger store, and the replay path lives there.
 */
const bakedPort = new Map<string, number>();

/** Fetch the bridge values for a tab's ssh command, remembering what we hand out. */
export async function bakeBridgeEnv(
  tabId: string,
  sshCmd: string,
): Promise<{ port: number; auth: string } | null> {
  const env = await getRemoteBridgeEnv(sshCmd);
  if (env) bakedPort.set(tabId, env.port);
  else bakedPort.delete(tabId);
  return env;
}

/** The port this tab's ssh command carries, if one was baked into it. */
export function bakedBridgePort(tabId: string): number | undefined {
  return bakedPort.get(tabId);
}

export function forgetBakedBridgePort(tabId: string): void {
  bakedPort.delete(tabId);
}
