/**
 * §6 — handing an SSH tab the active account's identity.
 *
 * One helper shared by every path that starts a remote session, because there are five of them
 * (spawn/restore, reconnect, auto-resume replay, the bridge's live injection, and the manual
 * inject action) and a credential rule that holds in four places out of five is not a rule.
 *
 * All this does is ask Rust, and then say something when the answer is bad. The token itself
 * never reaches this file — see `RemoteTokenPrep`.
 */
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import { prepareRemoteAccountToken, discardRemoteAccountToken } from '$lib/tauri/commands';
import type { RemoteTokenPrep } from '$lib/tauri/commands';
import { dispatch } from '$lib/stores/notificationDispatch';

/** Say that this host is about to be somebody else. Shared so the wording cannot drift. */
async function announceMiss(
  tabId: string,
  who: string | null,
  where: string | null,
  detail: string,
): Promise<void> {
  await dispatch(
    'Agent account not applied',
    `${where ?? 'This host'} is not running as ${who ?? 'the active account'}: ${detail}. ` +
      `The session will use whatever login that host already has.`,
    'error',
    { tabId },
  );
}

/**
 * A token placed on a host, and the two things a caller may do with it.
 *
 * **Placing and using it are separate events, and that is the whole reason this exists.** The
 * push is an ssh round trip, so it is started in parallel with the work that decides whether to
 * inject — otherwise it would sit in front of a tab opening. The consequence is that by the time
 * a caller changes its mind, a standing one-year credential is already on that host's disk, and
 * nothing local can revoke it (§9.4). So a caller that does not `take()` must `abandon()`.
 */
export interface RemoteAccountHandoff {
  /** The fragment to splice into the remote command, or null for "inject nothing". */
  take(): Promise<string | null>;
  /**
   * Give up on it: remove the file from the host, and tell the user this tab is not running as
   * the account they chose. Safe to call after `take()`, and safe to call twice.
   */
  abandon(why: string): Promise<void>;
}

/**
 * Start placing the active account's token on this tab's ssh host.
 *
 * Returns a handoff even when nothing is placed — a host the account does not cover, the feature
 * switched off, no token minted — in which case both methods are no-ops. Rust answers those
 * without opening a connection, so calling this on every ssh spawn is free.
 *
 * Never throws: a remote session that cannot carry an account is still a remote session the user
 * asked for, and failing the ssh over this would be the wrong trade.
 */
export function beginRemoteAccount(tabId: string, sshArgs: string): RemoteAccountHandoff {
  let taken = false;
  let abandoned = false;

  const prep: Promise<RemoteTokenPrep | null> = prepareRemoteAccountToken(tabId, sshArgs)
    .then(async (p) => {
      if (p.status === 'missing_credential' || p.status === 'failed') {
        // Announced here rather than at the call site, because this is the §6.1 state: nothing
        // downstream fails, the session just comes up as somebody else. If it is not said now it
        // is never said at all.
        await announceMiss(tabId, p.account_label, p.host, p.detail ?? 'the token could not be placed');
      }
      return p;
    })
    .catch((e) => {
      // The command itself failed (a webview teardown mid-spawn, a backend that has gone away).
      // Logged rather than announced: we cannot even name the account or the host here, so there
      // is nothing actionable to say.
      logError(`accounts: could not prepare the remote token for tab ${tabId}: ${e}`);
      return null;
    });

  return {
    async take() {
      const p = await prep;
      if (!p || p.status !== 'ready') return null;
      taken = true;
      logInfo(`accounts: ${p.host} will run as ${p.account_label}`);
      return p.export;
    },

    async abandon(why: string) {
      const p = await prep;
      // Only a `ready` prep put a file on the host, and only an untaken one is unused.
      if (!p || p.status !== 'ready' || taken || abandoned) return;
      abandoned = true;
      await discardRemoteAccountToken(tabId, sshArgs).catch(() => {});
      await announceMiss(tabId, p.account_label, p.host, why);
    },
  };
}

/**
 * The common case: place the token and use it immediately.
 *
 * For callers that build one ssh command and send it — there is no branch between deciding and
 * doing, so there is nothing to abandon.
 */
export function remoteAccountExport(tabId: string, sshArgs: string): Promise<string | null> {
  return beginRemoteAccount(tabId, sshArgs).take();
}
