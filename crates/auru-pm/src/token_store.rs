//! OS keychain storage for HTTP provider bearer tokens.
//!
//! Tokens are keyed by `(provider_id, project_id)` so each project on each
//! provider has an independent credential. The service name is `"auru-pm"`.
//! Tokens never appear in `.auru` or `.auru-pm.json` — only in the keychain.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};

const SERVICE: &str = "auru-pm";
const CREDENTIAL_PREFIX: &str = "auru-pm-credential-v1:";

/// Credential bundle held only in the operating-system keychain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProviderCredential {
    Pat {
        access_token: String,
    },
    OAuth {
        access_token: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        refresh_token: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expires_at: Option<i64>,
        token_endpoint: String,
        client_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<String>,
    },
}

impl ProviderCredential {
    pub fn access_token(&self) -> &str {
        match self {
            Self::Pat { access_token } | Self::OAuth { access_token, .. } => access_token,
        }
    }
}

#[derive(Clone, Copy)]
enum CredentialScope<'a> {
    Provider,
    Project(&'a str),
}

fn account_key(provider_id: &str, scope: CredentialScope<'_>) -> String {
    // Keychain identifier support differs by platform (notably around NULs
    // and maximum lengths), so use one short printable identifier derived
    // from a length-delimited tuple. The namespace keeps an account credential
    // distinct from a project whose handle happens to be "provider".
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"auru-pm-key-v1");
    hash_key_part(&mut hasher, provider_id.as_bytes());
    match scope {
        CredentialScope::Provider => {
            hasher.update(b"\x00");
        }
        CredentialScope::Project(project_id) => {
            hasher.update(b"\x01");
            hash_key_part(&mut hasher, project_id.as_bytes())
        }
    };
    format!("v1-{}", hasher.finalize().to_hex())
}

fn hash_key_part(hasher: &mut blake3::Hasher, value: &[u8]) {
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn kring(e: keyring::Error) -> Error {
    Error::Other(format!("keychain: {e}"))
}

fn entry(provider_id: &str, scope: CredentialScope<'_>) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, &account_key(provider_id, scope)).map_err(kring)
}

fn store_secret(provider_id: &str, scope: CredentialScope<'_>, secret: &str) -> Result<()> {
    entry(provider_id, scope)?
        .set_password(secret)
        .map_err(kring)
}

fn load_secret(provider_id: &str, scope: CredentialScope<'_>) -> Result<Option<String>> {
    match entry(provider_id, scope)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(kring(error)),
    }
}

fn delete_secret(provider_id: &str, scope: CredentialScope<'_>) -> Result<()> {
    match entry(provider_id, scope)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(kring(error)),
    }
}

/// Store a PAT for `(provider_id, project_id)` in the OS keychain.
pub fn store_token(provider_id: &str, project_id: &str, token: &str) -> Result<()> {
    store_secret(provider_id, CredentialScope::Project(project_id), token)
}

/// Load the PAT for `(provider_id, project_id)`. Returns `Ok(None)` when no
/// token has been stored yet.
pub fn load_token(provider_id: &str, project_id: &str) -> Result<Option<String>> {
    load_secret(provider_id, CredentialScope::Project(project_id))
}

/// Delete the stored token. No-ops silently if nothing was stored.
pub fn delete_token(provider_id: &str, project_id: &str) -> Result<()> {
    delete_secret(provider_id, CredentialScope::Project(project_id))
}

/// Store an account-wide token for a provider.
///
/// Device-code OAuth and most PAT providers authenticate the person, not one
/// project. Project-scoped tokens still use [`store_token`]; clients can try a
/// project token first and fall back to this account token.
pub fn store_provider_token(provider_id: &str, token: &str) -> Result<()> {
    store_secret(provider_id, CredentialScope::Provider, token)
}

/// Load an account-wide provider token.
pub fn load_provider_token(provider_id: &str) -> Result<Option<String>> {
    Ok(load_provider_credential(provider_id)?
        .map(|credential| credential.access_token().to_owned()))
}

/// Delete an account-wide provider token.
pub fn delete_provider_token(provider_id: &str) -> Result<()> {
    delete_secret(provider_id, CredentialScope::Provider)
}

/// Store a PAT or refreshable OAuth credential bundle in the keychain.
pub fn store_provider_credential(provider_id: &str, credential: &ProviderCredential) -> Result<()> {
    let encoded = serde_json::to_string(credential)
        .map_err(|error| Error::Other(format!("encode provider credential: {error}")))?;
    store_secret(
        provider_id,
        CredentialScope::Provider,
        &format!("{CREDENTIAL_PREFIX}{encoded}"),
    )
}

/// Load an account credential, treating historical plain strings as PATs.
pub fn load_provider_credential(provider_id: &str) -> Result<Option<ProviderCredential>> {
    let Some(stored) = load_secret(provider_id, CredentialScope::Provider)? else {
        return Ok(None);
    };
    let Some(encoded) = stored.strip_prefix(CREDENTIAL_PREFIX) else {
        return Ok(Some(ProviderCredential::Pat {
            access_token: stored,
        }));
    };
    serde_json::from_str(encoded)
        .map(Some)
        .map_err(|error| Error::Other(format!("decode provider credential: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_keychain_backend_should_be_wired() {
        keyring::Entry::new(SERVICE, "backend-wiring").unwrap();
    }

    #[test]
    fn keychain_identifiers_should_be_printable_fixed_length_and_unambiguous() {
        let account = account_key("https://pm.example.com", CredentialScope::Provider);
        let project = account_key(
            "https://pm.example.com",
            CredentialScope::Project("provider"),
        );
        let first_split = account_key("ab", CredentialScope::Project("c"));
        let second_split = account_key("a", CredentialScope::Project("bc"));

        for key in [&account, &project, &first_split, &second_split] {
            assert_eq!(key.len(), 67);
            assert!(key.is_ascii());
            assert!(!key.contains('\0'));
        }
        assert_ne!(account, project);
        assert_ne!(first_split, second_split);
    }

    #[test]
    fn credential_bundle_should_round_trip_without_exposing_a_legacy_format_change() {
        let credential = ProviderCredential::OAuth {
            access_token: "access".to_owned(),
            refresh_token: Some("refresh".to_owned()),
            expires_at: Some(1_800_000_000),
            token_endpoint: "https://identity.example.com/oauth/token".to_owned(),
            client_id: "desktop".to_owned(),
            scope: Some("openid".to_owned()),
        };
        let encoded = serde_json::to_string(&credential).unwrap();
        assert_eq!(
            serde_json::from_str::<ProviderCredential>(&encoded).unwrap(),
            credential
        );
        assert_eq!(credential.access_token(), "access");
    }
}
