//! The credentials maiTerm holds for proxied MCP servers (docs/mcp-proxy.md §5): a pasted token,
//! or an OAuth sign-in's access and refresh tokens.
//!
//! The rules are `accounts::vault`'s, and its error mapping is reused: the OS keychain only, under
//! maiTerm's own `app_data_slug()`-scoped service name; never a plaintext fallback; never a value
//! in a log line, an error or a `Debug` print. One keychain item per server, keyed by its id,
//! holding a small JSON document.

use crate::accounts::vault::{map_err, Secret, VaultError};
use crate::state::persistence::app_data_slug;
use serde::{Deserialize, Serialize};

/// A server's credentials. `Debug` is hand-written so a value can't reach a log by accident.
#[derive(Clone, Serialize, Deserialize, Default)]
pub struct Cred {
    /// What goes in `Authorization: Bearer`.
    pub access_token: String,
    /// OAuth only. Rotates on every use (§5), so the new one is stored before the new access
    /// token is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    /// OAuth only: when the access token expires, in ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
    /// OAuth only: the scope the sign-in was granted, as the token response gave it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// OAuth only: when the access token was issued, in ms since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub obtained_at: Option<u64>,
    /// OAuth only: the client the tokens were issued to. A refresh token is bound to it, so it
    /// travels with the tokens rather than being read from the server's config, which a later,
    /// unfinished sign-in may have re-registered under another id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// OAuth only: that client's secret, when the server issued one at registration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_secret: Option<String>,
}

impl std::fmt::Debug for Cred {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cred")
            .field("access_token", &"<redacted>")
            .field("refresh_token", &self.refresh_token.as_ref().map(|_| "<redacted>"))
            .field("expires_at", &self.expires_at)
            .field("client_id", &self.client_id)
            .field("client_secret", &self.client_secret.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

fn service() -> String {
    format!("{}.mcp", app_data_slug())
}

#[cfg(test)]
fn entry_check(server_id: &str) -> Result<(), VaultError> {
    if server_id.is_empty() {
        return Err(VaultError::Failed { op: "address", detail: "empty server id".into() });
    }
    Ok(())
}

#[cfg_attr(test, allow(dead_code))]
fn entry(server_id: &str) -> Result<keyring::Entry, VaultError> {
    if server_id.is_empty() {
        return Err(VaultError::Failed { op: "address", detail: "empty server id".into() });
    }
    keyring::Entry::new(&service(), server_id).map_err(map_err("open"))
}

pub fn store(server_id: &str, cred: &Cred) -> Result<(), VaultError> {
    if cred.access_token.trim().is_empty() {
        return Err(VaultError::Failed { op: "store", detail: "an empty token".into() });
    }
    let doc = serde_json::to_string(cred).map_err(|_| VaultError::Failed { op: "store", detail: "could not encode".into() })?;
    #[cfg(test)]
    {
        entry_check(server_id)?;
        test_keys().lock().insert(server_id.to_string(), Secret::new(doc));
        Ok(())
    }
    #[cfg(not(test))]
    entry(server_id)?.set_password(&doc).map_err(map_err("store"))
}

/// `Ok(None)` when nothing is stored, which is ordinary (a server that needs no sign-in).
pub fn read(server_id: &str) -> Result<Option<Cred>, VaultError> {
    #[cfg(test)]
    let got: Result<String, keyring::Error> = {
        entry_check(server_id)?;
        test_keys().lock().get(server_id).map(|s| s.expose().to_string()).ok_or(keyring::Error::NoEntry)
    };
    #[cfg(not(test))]
    let got = entry(server_id)?.get_password();
    match got {
        Ok(doc) => serde_json::from_str(&doc)
            .map(Some)
            .map_err(|_| VaultError::Failed { op: "read", detail: "the stored credential is unreadable".into() }),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err("read")(e)),
    }
}

/// Idempotent. **This revokes nothing**: it stops maiTerm using the credential.
pub fn delete(server_id: &str) -> Result<(), VaultError> {
    #[cfg(test)]
    let gone: Result<(), keyring::Error> = {
        entry_check(server_id)?;
        test_keys().lock().remove(server_id).map(|_| ()).ok_or(keyring::Error::NoEntry)
    };
    #[cfg(not(test))]
    let gone = entry(server_id)?.delete_credential();
    match gone {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err("delete")(e)),
    }
}

// ─── A stdio server's key (docs/mcp-proxy.md §2.1) ────────────────────────

/// Where a stdio server's key is kept: its own item, never the server id's. An older build reads
/// the id's item as a bearer token for any auth it doesn't know, and must find nothing there.
fn key_item(server_id: &str) -> String {
    format!("{server_id}#key")
}

/// The shortest key a stdio server takes. Every API key a catalogue route uses is longer, and a
/// short one would make `stdio::scrub` take that text out of everything the child sends ("ul"
/// out of every "result"), and turn up by chance in a command line.
pub const MIN_KEY_LEN: usize = 16;

/// Store (or replace) the key a stdio server's child gets in its environment. Trimmed by
/// `Secret::new`, so what is checked is what is stored.
pub fn store_env_key(server_id: &str, key: &Secret) -> Result<(), VaultError> {
    if key.expose().chars().count() < MIN_KEY_LEN {
        return Err(VaultError::Failed { op: "store", detail: "that doesn't look like a whole key".into() });
    }
    if key.expose().chars().any(|c| c.is_control()) {
        return Err(VaultError::Failed { op: "store", detail: "a key with a line break or control character in it".into() });
    }
    #[cfg(test)]
    {
        entry_check(server_id)?;
        test_keys().lock().insert(key_item(server_id), key.clone());
        Ok(())
    }
    #[cfg(not(test))]
    entry(&key_item(server_id))?.set_password(key.expose()).map_err(map_err("store"))
}

/// `Ok(None)` when no key is stored.
pub fn read_env_key(server_id: &str) -> Result<Option<Secret>, VaultError> {
    #[cfg(test)]
    {
        Ok(test_keys().lock().get(&key_item(server_id)).cloned())
    }
    #[cfg(not(test))]
    match entry(&key_item(server_id))?.get_password() {
        Ok(v) => Ok(Some(Secret::new(v))),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err("read")(e)),
    }
}

/// Idempotent. Revokes nothing: the provider still honours the key until the human revokes it.
pub fn delete_env_key(server_id: &str) -> Result<(), VaultError> {
    #[cfg(test)]
    {
        test_keys().lock().remove(&key_item(server_id));
        Ok(())
    }
    #[cfg(not(test))]
    delete(&key_item(server_id))
}

/// Tests never touch the real keychain (an unsigned test binary can raise a prompt, and a CI box
/// has none): every item of this module lives here instead.
#[cfg(test)]
fn test_keys() -> &'static parking_lot::Mutex<std::collections::HashMap<String, Secret>> {
    static KEYS: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<String, Secret>>> = std::sync::OnceLock::new();
    KEYS.get_or_init(Default::default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_is_kept_apart_from_the_servers_own_item_and_refuses_line_breaks() {
        assert_eq!(key_item("minimax"), "minimax#key");
        assert!(store_env_key("k-test", &Secret::new("")).is_err());
        let e = store_env_key("k-test", &Secret::new("VALUE-short")).unwrap_err();
        assert!(e.to_string().contains("whole key") && !e.to_string().contains("VALUE-"), "{e}");
        assert!(read_env_key("k-test").unwrap().is_none(), "a short key is not stored");
        let e = store_env_key("k-test", &Secret::new("VALUE-1\nVALUE-2")).unwrap_err();
        assert!(!e.to_string().contains("VALUE-"), "{e}");
        store_env_key("k-test", &Secret::new("  VALUE-K-0123456789 \n")).unwrap();
        assert_eq!(read_env_key("k-test").unwrap().unwrap().expose(), "VALUE-K-0123456789");
        delete_env_key("k-test").unwrap();
        assert!(read_env_key("k-test").unwrap().is_none());
        delete_env_key("k-test").unwrap();
    }

    #[test]
    fn a_cred_never_prints_its_tokens() {
        let c = Cred {
            access_token: "VALUE-A".into(),
            refresh_token: Some("VALUE-R".into()),
            expires_at: Some(1),
            client_secret: Some("VALUE-C".into()),
            ..Default::default()
        };
        let shown = format!("{c:?}");
        assert!(!shown.contains("VALUE-"), "{shown}");
    }
}
