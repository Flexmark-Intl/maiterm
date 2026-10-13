//! The credentials maiTerm holds for proxied MCP servers (docs/mcp-proxy.md §5): a pasted token,
//! or an OAuth sign-in's access and refresh tokens.
//!
//! The rules are `accounts::vault`'s, and its error mapping is reused: the OS keychain only, under
//! maiTerm's own `app_data_slug()`-scoped service name; never a plaintext fallback; never a value
//! in a log line, an error or a `Debug` print. One keychain item per server, keyed by its id,
//! holding a small JSON document.

use crate::accounts::vault::{map_err, VaultError};
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
    entry(server_id)?.set_password(&doc).map_err(map_err("store"))
}

/// `Ok(None)` when nothing is stored, which is ordinary (a server that needs no sign-in).
pub fn read(server_id: &str) -> Result<Option<Cred>, VaultError> {
    match entry(server_id)?.get_password() {
        Ok(doc) => serde_json::from_str(&doc)
            .map(Some)
            .map_err(|_| VaultError::Failed { op: "read", detail: "the stored credential is unreadable".into() }),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(map_err("read")(e)),
    }
}

/// Idempotent. **This revokes nothing**: it stops maiTerm using the credential.
pub fn delete(server_id: &str) -> Result<(), VaultError> {
    match entry(server_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(map_err("delete")(e)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
