//! OS keychain storage for HTTP provider bearer tokens.
//!
//! Tokens are keyed by `(provider_id, project_id)` so each project on each
//! provider has an independent credential. The service name is `"auru-pm"`.
//! Tokens never appear in `.auru` or `.auru-pm.json` — only in the keychain.

use crate::error::{Error, Result};

const SERVICE: &str = "auru-pm";

fn account_key(provider_id: &str, project_id: &str) -> String {
    // Use a slash separator; provider IDs (URLs) never contain raw slashes
    // after the scheme, so this key is unambiguous.
    format!("{provider_id}\x00{project_id}")
}

fn kring(e: keyring_core::Error) -> Error {
    Error::Other(format!("keychain: {e}"))
}

/// Store a PAT for `(provider_id, project_id)` in the OS keychain.
pub fn store_token(provider_id: &str, project_id: &str, token: &str) -> Result<()> {
    keyring_core::Entry::new(SERVICE, &account_key(provider_id, project_id))
        .map_err(kring)?
        .set_password(token)
        .map_err(kring)
}

/// Load the PAT for `(provider_id, project_id)`. Returns `Ok(None)` when no
/// token has been stored yet.
pub fn load_token(provider_id: &str, project_id: &str) -> Result<Option<String>> {
    let entry =
        keyring_core::Entry::new(SERVICE, &account_key(provider_id, project_id)).map_err(kring)?;
    match entry.get_password() {
        Ok(t) => Ok(Some(t)),
        Err(keyring_core::Error::NoEntry) => Ok(None),
        Err(e) => Err(kring(e)),
    }
}

/// Delete the stored token. No-ops silently if nothing was stored.
pub fn delete_token(provider_id: &str, project_id: &str) -> Result<()> {
    let entry =
        keyring_core::Entry::new(SERVICE, &account_key(provider_id, project_id)).map_err(kring)?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring_core::Error::NoEntry) => Ok(()),
        Err(e) => Err(kring(e)),
    }
}
