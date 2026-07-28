use std::path::PathBuf;

use auru_pm::{fetch_registry, AuthMethod, Capabilities, RegistryEntry, AURU_REGISTRY_URL};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogState {
    Loading,
    Live,
    Fallback,
}

impl CatalogState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Loading => "CHECKING FIRST-PARTY LIST…",
            Self::Live => "FIRST-PARTY LIST",
            Self::Fallback => "OFFLINE FALLBACK",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProviderAvailability {
    Connected,
    OnYourNetwork,
    Available,
}

impl ProviderAvailability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "CONNECTED",
            Self::OnYourNetwork => "ON YOUR NETWORK",
            Self::Available => "AVAILABLE",
        }
    }
}

pub struct ProviderListing {
    pub entry: RegistryEntry,
    pub detail: String,
    pub availability: ProviderAvailability,
}

impl ProviderListing {
    pub fn from_registry(entry: RegistryEntry) -> Self {
        let detail = if entry.description.is_empty() {
            entry.endpoint.clone()
        } else {
            entry.description.clone()
        };

        Self {
            entry,
            detail,
            availability: ProviderAvailability::Available,
        }
    }

    pub fn requires_authentication(&self) -> bool {
        self.entry
            .auth_methods
            .iter()
            .any(|method| !matches!(method, AuthMethod::None))
    }

    pub fn preferred_auth_method(&self) -> AuthMethod {
        self.entry
            .auth_methods
            .iter()
            .find(|method| !matches!(method, AuthMethod::None))
            .cloned()
            .unwrap_or(AuthMethod::None)
    }

    pub fn mark_connected(&mut self) {
        self.availability = ProviderAvailability::Connected;
    }
}

fn entry(
    id: &str,
    name: &str,
    endpoint: &str,
    auth_methods: Vec<AuthMethod>,
    description: &str,
    recommended: bool,
) -> RegistryEntry {
    RegistryEntry {
        id: id.to_owned(),
        name: name.to_owned(),
        endpoint: endpoint.to_owned(),
        capabilities: Capabilities {
            auth_methods: auth_methods.clone(),
            ..Capabilities::default()
        },
        auth_methods,
        icon_url: None,
        description: description.to_owned(),
        recommended,
    }
}

pub fn stub_provider_catalog() -> Vec<ProviderListing> {
    vec![
        ProviderListing {
            entry: entry(
                "auru-cloud",
                "Auru Cloud",
                "https://pm.auru.studio",
                vec![AuthMethod::OAuthDeviceCode],
                "Hosted backup with encrypted storage",
                true,
            ),
            detail: "Hosted · eu-west · encrypted at rest".to_owned(),
            availability: ProviderAvailability::Connected,
        },
        ProviderListing {
            entry: entry(
                "studio-nas",
                "Studio NAS",
                "http://studio-nas.local:3000",
                vec![AuthMethod::None],
                "A provider discovered on this network",
                false,
            ),
            detail: "studio-nas.local · private network".to_owned(),
            availability: ProviderAvailability::OnYourNetwork,
        },
        ProviderListing {
            entry: entry(
                "s3-archive",
                "S3 Archive",
                "https://s3-provider.example",
                vec![AuthMethod::Pat],
                "Bring an existing S3-compatible archive",
                false,
            ),
            detail: "S3-compatible · personal access token".to_owned(),
            availability: ProviderAvailability::Available,
        },
    ]
}

pub fn fetch_first_party_catalog() -> Result<Vec<ProviderListing>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("provider catalog runtime: {error}"))?;
    let entries = runtime
        .block_on(fetch_registry(AURU_REGISTRY_URL, &catalog_cache_path()))
        .map_err(|error| error.to_string())?;

    if entries.is_empty() {
        return Err("provider catalog returned no entries".to_owned());
    }

    Ok(entries
        .into_iter()
        .map(ProviderListing::from_registry)
        .collect())
}

fn catalog_cache_path() -> PathBuf {
    std::env::temp_dir()
        .join("auru-pm")
        .join("provider-catalog.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_provider_should_require_authentication() {
        let providers = stub_provider_catalog();

        assert!(providers[0].requires_authentication());
        assert_eq!(
            providers[0].preferred_auth_method(),
            AuthMethod::OAuthDeviceCode
        );
    }

    #[test]
    fn provider_advertising_none_should_connect_directly() {
        let providers = stub_provider_catalog();

        assert!(!providers[1].requires_authentication());
        assert_eq!(providers[1].preferred_auth_method(), AuthMethod::None);
    }

    #[test]
    fn registry_entry_should_become_an_available_listing() {
        let entry = entry(
            "remote",
            "Remote",
            "https://remote.example",
            vec![AuthMethod::None],
            "Remote provider",
            true,
        );

        let listing = ProviderListing::from_registry(entry);

        assert_eq!(listing.availability, ProviderAvailability::Available);
        assert_eq!(listing.detail, "Remote provider");
    }
}
