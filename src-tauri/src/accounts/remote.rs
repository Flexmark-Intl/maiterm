//! §6 step 2 — getting an account's `setup-token` into a remote shell's environment.
//!
//! `remote_account_for_host` decides *whether* a host may have the active account's token.
//! This module is *how* it gets there, and the how is constrained from four directions at once:
//!
//! 1. **Not on a command line.** The ssh command maiTerm builds is typed into the user's local
//!    shell — so it lands in their scrollback, in their shell history, and in `ps` on both
//!    machines. A standing one-year credential cannot ride there.
//! 2. **Not in the terminal at all.** Scrollback is serialized into `aiterm-state.json`, and
//!    §9.1 says credential material never touches that file. That rules out typing the token
//!    into the PTY the way `MAITERM_TAB_ID` is typed: the tty echoes what we write.
//! 3. **Per tab, never the shared `~/.aiterm`.** That file is one per remote *account* and
//!    outlives the session, so a token written there would not follow an account switch —
//!    which is the one guarantee the active-account model exists to make. See
//!    `docs/login.md` §6 and the `aiterm-file-cross-pollution` incident.
//! 4. **Never through the frontend.** §9.3 keeps credential material away from anything an
//!    agent can reach, and the webview is reachable.
//!
//! So the token travels on **stdin of its own ssh connection**, into a file the remote shell
//! then reads and deletes. Nothing secret appears in any argv, in either terminal, or in any
//! IPC payload: Rust reads the vault, pushes the bytes, and hands the frontend back only a
//! shell fragment naming a *path*.
//!
//! The file is consumed on read — `cat` then `rm` in the same expression — so the window in
//! which a standing credential sits on someone else's disk is the few seconds between the push
//! and the shell starting. A push whose ssh never followed is swept by the next one.

/// Where handoff files live on the remote. A directory of our own rather than a file beside
/// `~/.aiterm`, so the sweep below can be a blunt `find … -delete` without ever being pointed
/// at something we did not write.
const REMOTE_DIR: &str = "~/.maiterm/tokens";

/// Minutes after which an unconsumed handoff file is swept.
///
/// A file is normally read within seconds of being written. Anything older is from a push whose
/// ssh never connected, and leaving a live credential on disk for a session that never happened
/// is the failure this bounds.
const STALE_AFTER_MINUTES: u32 = 10;

/// Is this safe to interpolate into a shell command unquoted?
///
/// Every value this module puts in a script is a tab id, and tab ids are UUIDs. Validating
/// rather than quoting is deliberate: the fragment below is spliced into a *single-quoted*
/// argument by `buildSshCommand`, so a value containing a quote would not merely be mis-parsed,
/// it would break out of the quoting and run as a command on the remote host.
pub fn is_safe_handle(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

/// The handoff file for one tab.
pub fn token_path(tab_id: &str) -> Option<String> {
    is_safe_handle(tab_id).then(|| format!("{REMOTE_DIR}/tok-{tab_id}"))
}

/// The script the push connection runs. **Carries no credential** — the token arrives on this
/// command's stdin, which is why the whole approach works.
///
/// `umask 077` covers the create; the explicit `chmod` covers a directory that already existed
/// with looser permissions, which `mkdir -p` would leave alone.
pub fn stage_script(tab_id: &str) -> Option<String> {
    let path = token_path(tab_id)?;
    Some(format!(
        "umask 077; mkdir -p {REMOTE_DIR} || exit 1; chmod 700 {REMOTE_DIR} 2>/dev/null; \
         find {REMOTE_DIR} -type f -name 'tok-*' -mmin +{STALE_AFTER_MINUTES} -delete 2>/dev/null; \
         cat > {path} && chmod 600 {path}"
    ))
}

/// The fragment the remote shell runs to pick the token up. Safe to put on a command line: it
/// names a path, not a secret.
///
/// **Only exports when there is something to export.** An empty `CLAUDE_CODE_OAUTH_TOKEN` would
/// be the §6.1 trap in its purest form — the variable present and the resolution falling through
/// past it to whatever login the host already has, `loggedIn: true`, no error, wrong identity.
/// Leaving it unset at least fails the same way as never having tried.
///
/// Contains no single quote, by construction and by test: `buildSshCommand` splices it into a
/// single-quoted ssh argument.
///
/// **`__mt_oat=` is a contract, not an implementation detail.** `cleanSshCommand` strips this
/// fragment back out when a stored ssh value is rebuilt from `ps` output, and it recognises the
/// fragment by that leading token. Renaming the variable without changing the regex there makes
/// the whole remote command accumulate into the host string on every round trip.
pub fn export_fragment(tab_id: &str) -> Option<String> {
    let path = token_path(tab_id)?;
    Some(format!(
        "__mt_oat=$(cat {path} 2>/dev/null); rm -f {path} 2>/dev/null; \
         [ -n \"$__mt_oat\" ] && export CLAUDE_CODE_OAUTH_TOKEN=\"$__mt_oat\"; unset __mt_oat"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_anything_that_is_not_a_uuid_shape() {
        assert!(is_safe_handle("fc8ecc9c-69f5-46cd-ab8f-6b16e0d2ca0c"));
        assert!(!is_safe_handle(""));
        assert!(!is_safe_handle("a b"));
        assert!(!is_safe_handle("a/b"));
        assert!(!is_safe_handle("a;rm -rf ~"));
        assert!(!is_safe_handle("a'b"));
        assert!(!is_safe_handle("a$(id)"));
        assert!(!is_safe_handle(&"a".repeat(65)));
        assert!(token_path("a'b").is_none());
        assert!(stage_script("a'b").is_none());
        assert!(export_fragment("a'b").is_none());
    }

    /// The fragment is spliced into `ssh host '<fragment>…'`. One apostrophe in it and the rest
    /// of the remote command is no longer inside the quotes.
    #[test]
    fn no_single_quotes_reach_a_single_quoted_argument() {
        let f = export_fragment("tab-1").unwrap();
        assert!(!f.contains('\''), "{f}");
        // The stage script is its own argv entry, but it is built the same way and has the
        // same property except for the deliberate quoting around the find pattern.
        let s = stage_script("tab-1").unwrap();
        assert_eq!(s.matches('\'').count(), 2, "only the find -name pattern is quoted: {s}");
    }

    #[test]
    fn the_stage_script_never_carries_the_token() {
        let s = stage_script("tab-1").unwrap();
        // It ends by reading stdin — that, and nothing on the command line, is the credential.
        assert!(s.contains("cat > "), "{s}");
        assert!(!s.contains("sk-ant"), "{s}");
    }

    #[test]
    fn the_fragment_consumes_the_file_and_refuses_to_export_nothing() {
        let f = export_fragment("tab-1").unwrap();
        assert!(f.contains("rm -f "), "read-once: {f}");
        assert!(
            f.contains("[ -n \"$__mt_oat\" ] && export"),
            "an empty token must not be exported — that is the §6.1 fall-through: {f}"
        );
    }

    /// Pinned verbatim because `src/lib/tauri/sshCommand.test.ts` hand-copies this string to
    /// prove `cleanSshCommand` still strips it. Changing the fragment without changing that copy
    /// leaves the frontend test passing against a shape that no longer exists.
    #[test]
    fn the_fragment_is_what_the_frontend_test_expects() {
        assert_eq!(
            export_fragment("TAB").unwrap(),
            "__mt_oat=$(cat ~/.maiterm/tokens/tok-TAB 2>/dev/null); \
             rm -f ~/.maiterm/tokens/tok-TAB 2>/dev/null; \
             [ -n \"$__mt_oat\" ] && export CLAUDE_CODE_OAUTH_TOKEN=\"$__mt_oat\"; unset __mt_oat"
        );
    }

    /// The push and the pickup have to name the same file or the tab silently comes up as the
    /// host's own login.
    #[test]
    fn push_and_pickup_agree_on_the_path() {
        let path = token_path("tab-1").unwrap();
        assert!(stage_script("tab-1").unwrap().contains(&path));
        assert!(export_fragment("tab-1").unwrap().contains(&path));
    }
}
