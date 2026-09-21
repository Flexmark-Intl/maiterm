//! The credential vault — `docs/login.md` §9.1.
//!
//! maiTerm holds exactly one kind of secret: the `sk-ant-oat01-…` setup-token minted per managed
//! account for §6's remote propagation. It is the highest-value secret in the whole design — one
//! year, **non-rotating**, unlocks the subscription — and unlike every other credential this
//! feature touches, this one is ours to keep rather than the runtime's.
//!
//! So it goes in the OS keychain, under **maiTerm's own service name**, never Claude Code's item
//! (§5 is emphatic about not touching theirs). `aiterm-state.json` keeps only metadata — which
//! hosts are enabled, `token_minted_at` — because that file is plaintext and is the one we tell
//! people to open when something is wrong.
//!
//! **Three rules this module exists to enforce, none of which the type system can:**
//!
//! 1. **A token never appears in an error, a log line, or a `Debug` print.** Every error here is
//!    constructed from the *operation* and the account id, never from the value. `Secret` has a
//!    hand-written `Debug` for the same reason — a `#[derive(Debug)]` on a struct holding one is
//!    the easy way to leak it into a panic message.
//! 2. **No plaintext fallback, ever.** If the keychain is unavailable the feature degrades to
//!    local-only (§5) and says so. Writing the token beside `aiterm-state.json` "just to keep
//!    working" would downgrade precisely the secret that least deserves it.
//! 3. **Dev and prod do not share entries.** The service name carries `app_data_slug()`, the same
//!    split the data directory uses. Without it a `tauri:dev` run and the installed app fight
//!    over one keychain item, and clearing setup in one silently revokes the other.

use crate::state::persistence::app_data_slug;

/// A token in memory. Exists so the value cannot be printed by accident.
///
/// `Debug` is implemented by hand and prints a fixed redaction. Do not add `Display`,
/// `Serialize`, or `#[derive(Debug)]` here: the point of the type is that every route to a
/// string is one someone had to write on purpose. Getting it out is `expose()`, which is
/// deliberately ugly to read at a call site.
#[derive(Clone)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Hand over the raw token. Every call site is a place the secret can escape — the only
    /// legitimate ones are the keychain write and the spawn-env injection.
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Cheap sanity check on a freshly minted token, so a mint that actually captured an error
    /// message or a truncated line fails at the vault rather than six weeks later on a remote
    /// host, as a fall-through to the wrong identity (§6.1).
    pub fn looks_like_setup_token(&self) -> bool {
        let t = self.0.trim();
        t.starts_with("sk-ant-oat01-") && t.len() > 30 && !t.contains(char::is_whitespace)
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Not even the length: it is a fingerprint, and this shows up in panic output.
        f.write_str("Secret(<redacted>)")
    }
}

/// What went wrong, in terms that are safe to log and to show a user.
#[derive(Debug)]
pub enum VaultError {
    /// No usable keychain on this machine — a headless Linux box with no Secret Service
    /// provider, a locked login keychain, a platform with no backend compiled in. **The caller
    /// must degrade to local-only, never to a file.**
    Unavailable(String),
    /// The keychain is there and refused this specific operation.
    Failed { op: &'static str, detail: String },
    /// Asked for a token that was never stored, or has since been removed.
    NotFound,
}

impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VaultError::Unavailable(why) => write!(
                f,
                "no OS keychain is available ({why}) — remote logins need one, so accounts stay \
                 local to this machine"
            ),
            VaultError::Failed { op, detail } => write!(f, "keychain {op} failed: {detail}"),
            VaultError::NotFound => write!(f, "no token stored for that account"),
        }
    }
}

/// Our service name in the keychain. Split by dev/prod exactly as the data directory is.
fn service() -> String {
    format!("{}.accounts", app_data_slug())
}

/// Keychain entry for one account's setup-token.
///
/// The account id is the entry's "user", which keeps one item per account and makes the
/// keychain readable by a human debugging this — the id is also what the pane shows.
fn entry(account_id: &str) -> Result<keyring::Entry, VaultError> {
    if account_id.is_empty() {
        return Err(VaultError::Failed {
            op: "address",
            detail: "empty account id".into(),
        });
    }
    keyring::Entry::new(&service(), account_id).map_err(map_err("open"))
}

/// Translate a keyring error without ever letting a token near the message.
///
/// `keyring::Error` carries only operation context, but this is the one funnel every error goes
/// through, so it is the right place to be explicit about that rather than trusting it.
fn map_err(op: &'static str) -> impl Fn(keyring::Error) -> VaultError {
    move |e| match e {
        keyring::Error::NoEntry => VaultError::NotFound,
        // These mean "there is no working store here", which is a different answer for the
        // caller than "the store said no" — one degrades the feature, the other is a bug.
        keyring::Error::NoStorageAccess(ref inner) => VaultError::Unavailable(inner.to_string()),
        keyring::Error::PlatformFailure(ref inner) => VaultError::Unavailable(inner.to_string()),
        other => VaultError::Failed {
            op,
            detail: other.to_string(),
        },
    }
}

/// Store (or replace) the token for an account.
///
/// Rejects anything that does not look like a setup-token. A mint that captured a prompt, an
/// error line or half a token would otherwise be stored happily and then fail on a remote host
/// as a silent fall-through to whatever login that host already had (§6.1) — the single worst
/// failure mode in this design, and the one that is invisible.
pub fn store(account_id: &str, token: &Secret) -> Result<(), VaultError> {
    if !token.looks_like_setup_token() {
        return Err(VaultError::Failed {
            op: "store",
            detail: "that does not look like a setup-token — refusing to store it".into(),
        });
    }
    entry(account_id)?
        .set_password(token.expose())
        .map_err(map_err("store"))
}

/// Read an account's token back. `Ok(None)` means "none stored", which is ordinary.
pub fn read(account_id: &str) -> Result<Option<Secret>, VaultError> {
    match entry(account_id)?.get_password() {
        Ok(v) => Ok(Some(Secret::new(v))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err("read")(e)),
    }
}

/// Forget an account's token.
///
/// **This does not revoke anything.** It stops maiTerm handing the token out; a host that
/// already has it keeps working until the token expires or the user revokes it out of band.
/// §9.4 is the reasoning, and callers on the removal path must say so rather than implying the
/// credential is dead.
///
/// Idempotent: removing a token that is not there succeeds, so a half-finished
/// removal can be retried without a spurious error.
pub fn delete(account_id: &str) -> Result<(), VaultError> {
    match entry(account_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err("delete")(e)),
    }
}

/// Is a usable keychain present at all?
///
/// Probes with a real round-trip on a reserved account id rather than trusting that
/// constructing an `Entry` means anything — on most backends it does not touch the store. Used
/// to decide whether to OFFER remote logins, so a wrong answer here is a control that appears
/// and then fails, which §10 calls out as the thing not to ship.
pub fn available() -> bool {
    // Not a real account id (those are uuids), so it can never collide with a stored token.
    const PROBE: &str = "__maiterm_probe__";
    let probe = Secret::new("sk-ant-oat01-probe-not-a-real-token-0000000000");
    match store(PROBE, &probe) {
        Ok(()) => {
            let _ = delete(PROBE);
            true
        }
        Err(_) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_secret_never_prints_itself() {
        let s = Secret::new("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789");
        // Covers the panic path too: `assert_eq!` on a struct holding one of these is how a
        // token would otherwise reach a CI log.
        let shown = format!("{s:?}");
        assert_eq!(shown, "Secret(<redacted>)");
        assert!(!shown.contains("abcdef"), "the value leaked into Debug");
        // Not even the length, which is a fingerprint.
        assert!(!shown.contains(&s.expose().len().to_string()));
    }

    #[test]
    fn only_something_shaped_like_a_setup_token_is_accepted() {
        assert!(Secret::new("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789")
            .looks_like_setup_token());

        // Each of these is a real way a mint goes wrong, and every one of them would otherwise
        // be stored and then fall through to the wrong identity on a remote host (§6.1).
        for bad in [
            "",
            "sk-ant-oat01-",                       // truncated at the prefix
            "sk-ant-api03-abcdefghijklmnopqrstuv", // an API key, not an oat token
            "Error: you must be logged in",        // stdout captured a message
            "sk-ant-oat01-abc def0123456789012345678901234", // a wrapped line
            "sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789\n extra", // trailing output
        ] {
            assert!(
                !Secret::new(bad).looks_like_setup_token(),
                "should have been rejected: {bad:?}"
            );
        }

        // Surrounding whitespace is normal — the token is captured off stdout, which has a
        // trailing newline — and must NOT be a reason to reject.
        assert!(
            Secret::new("  sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789\n")
                .looks_like_setup_token()
        );
    }

    #[test]
    fn store_refuses_a_value_that_is_not_a_token() {
        // The guard has to live in `store`, not only in the mint path: a future caller that
        // forgets to check must not be able to poison the vault.
        let err = store("never-written", &Secret::new("not a token")).unwrap_err();
        assert!(matches!(err, VaultError::Failed { op: "store", .. }));
        // And the rejection must not quote what it was given.
        assert!(!format!("{err}").contains("not a token"));
    }

    #[test]
    fn an_empty_account_id_is_refused_before_it_reaches_the_keychain() {
        assert!(matches!(
            entry(""),
            Err(VaultError::Failed { op: "address", .. })
        ));
    }

    #[test]
    fn the_service_name_splits_dev_from_prod() {
        // Sharing one item between `tauri:dev` and the installed app means clearing setup in
        // one silently pulls the token out from under the other.
        assert!(service().starts_with(app_data_slug()));
        assert!(service().ends_with(".accounts"));
    }

    /// Round-trips against the REAL keychain, so it is opt-in: a headless CI box has no Secret
    /// Service, and on macOS an unsigned test binary can prompt. `VAULT_KEYCHAIN_TEST=1` to run.
    #[test]
    fn round_trips_through_the_real_keychain() {
        if std::env::var("VAULT_KEYCHAIN_TEST").is_err() {
            eprintln!("[vault] skipped — set VAULT_KEYCHAIN_TEST=1 to exercise the real store");
            return;
        }
        let id = format!("test-{}", uuid::Uuid::new_v4());
        let token = Secret::new("sk-ant-oat01-abcdefghijklmnopqrstuvwxyz0123456789");

        assert!(read(&id).unwrap().is_none(), "a fresh id must hold nothing");
        store(&id, &token).unwrap();
        assert_eq!(read(&id).unwrap().unwrap().expose(), token.expose());
        delete(&id).unwrap();
        assert!(read(&id).unwrap().is_none());
        // Idempotent, so a retried removal is not an error.
        delete(&id).unwrap();
    }
}
