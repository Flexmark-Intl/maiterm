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
//! The file is consumed on read — sourced then removed in the same expression — so in the normal
//! case a standing credential sits on someone else's disk for the few seconds between the push
//! and the shell starting.
//!
//! **That is the normal case, not a bound**, and the difference matters for a credential nothing
//! local can revoke (§9.4). The push runs in parallel with the decision about whether to inject,
//! so a caller that changes its mind has already placed the file; `discard_script` is how it
//! takes it back, and every such path calls it. What remains uncovered is a push whose ssh
//! session never started at all — for that there is the sweep in `stage_script`, which fires on
//! the next push to the same host and so is a bound only for hosts still in use.

/// Where handoff files live on the remote. A directory of our own rather than a file beside
/// `~/.aiterm`, so the sweep below can be a blunt `find … -delete` without ever being pointed
/// at something we did not write.
const REMOTE_DIR: &str = "~/.maiterm/tokens";

/// Minutes after which an unconsumed handoff file is swept **by the next push to that host**.
///
/// A file is normally read within seconds of being written. Anything older is from a push whose
/// ssh never connected. Note what this does and does not promise: it is a bound on hosts that
/// are still being used, and nothing at all on a host nobody opens again. The precise mechanism
/// is `discard_script`; this is the backstop for the case no caller knows about.
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

/// A handoff's name: the tab id plus a nonce minted per prepare. The nonce is what makes a
/// handoff recognisable as ITSELF (maiLink §14 binds a record to the ssh whose argv names it) —
/// with the tab id alone, an ssh re-run from shell history, or the outer ssh around a replay,
/// named the same path as a newer handoff and inherited its account.
pub fn handoff_handle(tab_id: &str) -> Option<String> {
    if !is_safe_handle(tab_id) {
        return None;
    }
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let handle = format!("{tab_id}-{}", &nonce[..12]);
    is_safe_handle(&handle).then_some(handle)
}

/// The handoff file for one handle (see `handoff_handle`).
pub fn token_path(tab_id: &str) -> Option<String> {
    is_safe_handle(tab_id).then(|| format!("{REMOTE_DIR}/tok-{tab_id}"))
}

/// The script the push connection runs. **Carries no credential** — the file's contents arrive
/// on this command's stdin, which is why the whole approach works.
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

/// The only plan values Claude Code understands, and the only ones we will write into a file
/// that a remote shell **sources**.
///
/// A whitelist rather than an escape: the contents are executed, not read, so the safe move is to
/// refuse anything unrecognised outright. These four are exactly what the runtime maps
/// `organization_type` onto (`claude_max` → `max`, and so on).
const KNOWN_PLANS: [&str; 4] = ["max", "pro", "team", "enterprise"];

/// What goes into the handoff file: a shell snippet, not the bare token.
///
/// The file is *sourced* rather than read, and that indirection is what keeps the fragment below
/// parseable by shells that are not POSIX — see `export_fragment`.
///
/// The token is single-quoted and the quote-escape applied anyway. A `setup-token` is
/// `sk-ant-oat01-` plus URL-safe base64 and `Secret::looks_like_setup_token` refuses whitespace,
/// so there is nothing here to escape today; this is for the day the format moves.
///
/// **`CLAUDE_CODE_SUBSCRIPTION_TYPE` rides along, and it is not cosmetic.** With only a token in
/// the environment the runtime has no idea what plan the account is on: the credential it builds
/// hardcodes `subscriptionType: env.CLAUDE_CODE_SUBSCRIPTION_TYPE || null`, and the
/// `/api/oauth/profile` call that would otherwise fill it in is gated on a `user:profile` scope
/// that a `setup-token` does not have (§8 — it is `user:inference` only). That one null decides
/// **which model actually serves the request**, not just how the `/model` list is drawn: a Max
/// account that resolves Opus locally resolves *Sonnet* on a token-authed remote, including for a
/// non-interactive `claude -p`. Observed on the first working §6 session, then confirmed against
/// the 2.1.278 bundle.
///
/// So the plan we already recorded at sign-in is passed through, and the remote behaves like the
/// account it is. This is a **client-side hint only** — the server decides entitlement — so the
/// worst a stale value can do is offer a model the account no longer has, which fails loudly
/// instead of silently downgrading. Claude Code sets this variable on its own child sessions the
/// same way.
pub fn stage_contents(token: &str, plan: Option<&str>) -> String {
    let mut out = format!(
        "export CLAUDE_CODE_OAUTH_TOKEN='{}'\n",
        token.replace('\'', r"'\''")
    );
    if let Some(plan) = plan.map(str::trim).filter(|p| KNOWN_PLANS.contains(p)) {
        out.push_str(&format!("export CLAUDE_CODE_SUBSCRIPTION_TYPE='{plan}'\n"));
    }
    out
}

/// The fragment the remote shell runs to pick the token up. Safe to put on a command line: it
/// names a path, not a secret.
///
/// **Sources the file instead of reading it, because the remote's login shell may not be a
/// POSIX one and this fragment has to survive that.** The obvious form,
/// `VAR=$(cat file)`, is a *parse-time* error in csh and tcsh ("Illegal variable name"), and a
/// parse error takes the whole statement list with it — including the `exec $SHELL -l` that is
/// the point of the remote command. The session then dies within a second, and on the
/// auto-resume path (where the ssh line and the agent's resume command are typed as one payload)
/// the local shell goes on to run `claude --resume` **on the user's own machine**. Verified
/// against tcsh: `VAR=$(…)` aborts, `[ -r f ] && . f` merely complains and carries on, with the
/// `rm` still reached. csh could never use maiTerm's exports anyway; what it must not do is fail
/// worse than it did before §6 existed.
///
/// **Only exports when there is something to export.** An empty `CLAUDE_CODE_OAUTH_TOKEN` would
/// be the §6.1 trap in its purest form — the variable present and the resolution falling through
/// past it to whatever login the host already has, `loggedIn: true`, no error, wrong identity.
/// `[ -r … ]` and an absent file both leave it unset, which at least fails the way never having
/// tried does.
///
/// No `2>/dev/null` anywhere: csh spells redirection differently and an "Ambiguous output
/// redirect" would put us back where we started.
///
/// Contains no single quote, by construction and by test: `buildSshCommand` splices it into a
/// single-quoted ssh argument.
///
/// **`[ -r ~/.maiterm/tokens/` is a contract, not an implementation detail.** `cleanSshCommand`
/// strips this fragment back out when a stored ssh value is rebuilt from `ps` output, and it
/// recognises the fragment by that leading token. Changing the shape without changing the regex
/// there makes the whole remote command accumulate into the host string on every round trip.
pub fn export_fragment(tab_id: &str) -> Option<String> {
    let path = token_path(tab_id)?;
    Some(format!("[ -r {path} ] && . {path}; rm -f {path}"))
}

/// Remove a handoff file we placed but are not going to use.
///
/// Needed because the push happens in parallel with the decision about whether to inject: by the
/// time a guard says "do not write into this shell", a one-year credential is already on that
/// host's disk. The sweep in `stage_script` only runs on the *next* push to the same host, which
/// may never happen.
pub fn discard_script(tab_id: &str) -> Option<String> {
    let path = token_path(tab_id)?;
    Some(format!("rm -f {path}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_handoff_gets_its_own_name() {
        let tab = "3f2a9c1e-0000-4000-8000-000000000000";
        let (a, b) = (handoff_handle(tab).unwrap(), handoff_handle(tab).unwrap());
        assert_ne!(a, b);
        assert!(a.starts_with(tab) && is_safe_handle(&a));
        assert!(handoff_handle("a'b").is_none());
    }

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
        assert!(discard_script("a'b").is_none());
    }

    /// The one that bit: `VAR=$(…)` is a PARSE error in csh/tcsh, and a parse error takes the
    /// whole statement list with it — `exec $SHELL -l` included. Sourcing degrades to a runtime
    /// complaint instead, so the session still comes up.
    #[test]
    fn the_fragment_avoids_what_csh_cannot_parse() {
        let f = export_fragment("tab-1").unwrap();
        assert!(!f.contains("$("), "command substitution aborts csh at parse time: {f}");
        assert!(!f.contains("2>"), "csh spells redirection differently: {f}");
        assert!(f.contains("] && . "), "must source, not read: {f}");
    }

    #[test]
    fn the_staged_contents_are_a_shell_snippet_with_the_token_quoted() {
        assert_eq!(
            stage_contents("sk-ant-oat01-abc", None),
            "export CLAUDE_CODE_OAUTH_TOKEN='sk-ant-oat01-abc'\n"
        );
        // Nothing can close the quoting from inside the file either.
        let escaped = stage_contents("a'b", None);
        assert_eq!(escaped, "export CLAUDE_CODE_OAUTH_TOKEN='a'\\''b'\n");
    }

    /// Without this the remote cannot tell what the account is entitled to and resolves a
    /// different model than the same account does locally — silently, and for `claude -p` too.
    #[test]
    fn a_known_plan_rides_along_and_anything_else_is_refused() {
        assert!(stage_contents("t", Some("max"))
            .contains("export CLAUDE_CODE_SUBSCRIPTION_TYPE='max'"));
        assert!(stage_contents("t", Some(" pro "))
            .contains("export CLAUDE_CODE_SUBSCRIPTION_TYPE='pro'"));
        // A whitelist, not an escape: this file is SOURCED by the remote shell, so an
        // unrecognised value is dropped rather than quoted around.
        for bad in ["", "max; rm -rf ~", "'", "Max", "ultra"] {
            let out = stage_contents("t", Some(bad));
            assert!(
                !out.contains("SUBSCRIPTION_TYPE"),
                "must not write {bad:?} into a sourced file: {out}"
            );
        }
        assert!(!stage_contents("t", None).contains("SUBSCRIPTION_TYPE"));
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
            f.starts_with("[ -r "),
            "an unreadable or absent file must leave the variable UNSET rather than empty — an \
             empty one is the §6.1 fall-through with a variable in front of it: {f}"
        );
    }

    /// Pinned verbatim because `src/lib/tauri/sshCommand.test.ts` hand-copies this string to
    /// prove `cleanSshCommand` still strips it. Changing the fragment without changing that copy
    /// leaves the frontend test passing against a shape that no longer exists.
    #[test]
    fn the_fragment_is_what_the_frontend_test_expects() {
        assert_eq!(
            export_fragment("TAB").unwrap(),
            "[ -r ~/.maiterm/tokens/tok-TAB ] && . ~/.maiterm/tokens/tok-TAB; \
             rm -f ~/.maiterm/tokens/tok-TAB"
        );
    }

    /// The push and the pickup have to name the same file or the tab silently comes up as the
    /// host's own login.
    #[test]
    fn push_and_pickup_agree_on_the_path() {
        let path = token_path("tab-1").unwrap();
        assert!(stage_script("tab-1").unwrap().contains(&path));
        assert!(export_fragment("tab-1").unwrap().contains(&path));
        assert!(discard_script("tab-1").unwrap().contains(&path));
    }
}
