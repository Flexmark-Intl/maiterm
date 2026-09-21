/**
 * §6 — handing an SSH tab the active account's identity.
 *
 * One helper shared by every path that starts a remote session, because there are four of them
 * (spawn/restore, reconnect, auto-resume replay, and the bridge's live injection) and a
 * credential rule that holds in three of four places is not a rule.
 *
 * All this does is ask Rust, and then say something when the answer is bad. The token itself
 * never reaches this file — see `RemoteTokenPrep`.
 */
import { error as logError, info as logInfo } from '@tauri-apps/plugin-log';
import { prepareRemoteAccountToken } from '$lib/tauri/commands';
import { dispatch } from '$lib/stores/notificationDispatch';

/**
 * The shell fragment this tab's remote command should carry, or null for "inject nothing".
 *
 * **Null is not a safe default, and that is why the failure states are announced.** Credential
 * resolution on the remote is a fall-through (§6.1): with no token the session does not fail, it
 * comes up as whatever account last logged into that box — `loggedIn: true`, no error, and the
 * work billed and attributed to the wrong identity. So "we could not place the token" has to be
 * visible at the moment it happens, because nothing downstream will ever mention it again.
 *
 * Never throws: a remote session that cannot carry an account is still a remote session the user
 * asked for, and failing the ssh over this would be the wrong trade.
 */
export async function remoteAccountExport(
  tabId: string,
  sshArgs: string,
): Promise<string | null> {
  try {
    const prep = await prepareRemoteAccountToken(tabId, sshArgs);
    switch (prep.status) {
      case 'ready':
        logInfo(`accounts: ${prep.host} will run as ${prep.account_label}`);
        return prep.export;
      case 'not_applicable':
        return null;
      default: {
        // `missing_credential` and `failed` both mean the same thing to the user — this host is
        // about to be somebody else — so they get the same shape of message, differing only in
        // what they can do about it.
        const who = prep.account_label ?? 'the active account';
        const where = prep.host ?? 'this host';
        await dispatch(
          'Agent account not applied',
          `${where} is not running as ${who}: ${prep.detail ?? 'the token could not be placed'}. ` +
            `The session will use whatever login that host already has.`,
          'error',
          { tabId },
        );
        return null;
      }
    }
  } catch (e) {
    // The command itself failed (a webview teardown mid-spawn, a backend that has gone away).
    // Log rather than toast: at this point we cannot even name the account or the host, so
    // there is nothing actionable to say.
    logError(`accounts: could not prepare the remote token for tab ${tabId}: ${e}`);
    return null;
  }
}
