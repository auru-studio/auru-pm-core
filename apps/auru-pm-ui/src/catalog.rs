use std::path::{Path, PathBuf};

use auru_pm::{AURU_REGISTRY_URL, AuthMethod, Capabilities, RegistryEntry, fetch_registry};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogState {
    Loading,
    Live,
    Fallback,
    /// Supplied by `--providers-file`.
    FromFile,
}

impl CatalogState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Loading => "CHECKING FIRST-PARTY LIST…",
            Self::Live => "FIRST-PARTY LIST",
            Self::Fallback => "OFFLINE FALLBACK",
            Self::FromFile => "PROVIDER LIST FROM FILE",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProviderAvailability {
    Connected,
    OnYourNetwork,
    Recommended,
    #[default]
    Available,
}

impl ProviderAvailability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Connected => "CONNECTED",
            Self::OnYourNetwork => "ON YOUR NETWORK",
            Self::Recommended => "RECOMMENDED",
            Self::Available => "AVAILABLE",
        }
    }
}

/// What signing in to a provider will involve.
///
/// Shown before the person commits to a provider, because "you will be sent to
/// your browser" and "you will need to paste a token you have to go and create"
/// are very different amounts of work, and finding that out afterwards is a
/// poor way to learn it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthHint {
    /// One line for the provider row.
    pub summary: &'static str,
    /// What actually happens next, for the connect screen.
    pub detail: &'static str,
}

impl AuthHint {
    pub const fn for_method(method: &AuthMethod) -> Self {
        match method {
            AuthMethod::OAuthDeviceCode => Self {
                summary: "Sign in with your browser",
                detail: "Auru shows you a short code and opens your browser. \
                         Your password is never typed into this app or stored here.",
            },
            AuthMethod::Pat => Self {
                summary: "Needs an access token",
                detail: "Create an access token in your provider's account settings and \
                         paste it here. It is kept in your operating system's keychain, \
                         not in Auru's own files.",
            },
            AuthMethod::None => Self {
                summary: "No sign-in needed",
                detail: "This provider is on your own machine or network, so there is \
                         nothing to sign in to.",
            },
        }
    }
}

#[derive(Debug)]
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

    /// What signing in to this provider will involve.
    pub fn auth_hint(&self) -> AuthHint {
        AuthHint::for_method(&self.preferred_auth_method())
    }
}

/// A provider list supplied by `--providers-file`.
///
/// Lets the desktop app be driven against a made-up catalogue while the real
/// registry does not exist yet, and lets anyone reproduce a provider-picker
/// state by handing over one file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ProviderFile {
    #[serde(default)]
    pub providers: Vec<ProviderFileEntry>,
}

/// One provider as written in a providers file.
///
/// Deliberately flatter than [`RegistryEntry`]: this is a file a person writes
/// by hand, so it asks for what has to be said and defaults the rest.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderFileEntry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub endpoint: String,
    /// The line under the name, eg `"Hosted · eu-west · encrypted at rest"`.
    #[serde(default)]
    pub detail: String,
    #[serde(default)]
    pub availability: ProviderAvailability,
    /// `"oauth_device_code"`, `"pat"`, or `"none"`. Defaults to `none`, so a
    /// hand-written entry that says nothing about auth claims nothing.
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub recommended: bool,
}

impl ProviderFileEntry {
    fn into_listing(self) -> ProviderListing {
        let auth_methods = if self.auth_methods.is_empty() {
            vec![AuthMethod::None]
        } else {
            self.auth_methods
        };
        let detail = if self.detail.is_empty() {
            if self.description.is_empty() {
                self.endpoint.clone()
            } else {
                self.description.clone()
            }
        } else {
            self.detail
        };

        ProviderListing {
            entry: RegistryEntry {
                id: self.id,
                name: self.name,
                endpoint: self.endpoint,
                capabilities: Capabilities {
                    auth_methods: auth_methods.clone(),
                    ..Capabilities::default()
                },
                auth_methods,
                icon_url: None,
                description: self.description,
                recommended: self.recommended,
            },
            detail,
            availability: self.availability,
        }
    }
}

/// Read a provider list from `path`.
///
/// Errors name the file and say what was wrong with it — this is a file
/// someone is editing, so the message needs to help them fix it.
pub fn load_provider_file(path: &Path) -> Result<Vec<ProviderListing>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("couldn't read {}: {error}", path.display()))?;
    let file: ProviderFile = serde_json::from_str(&text)
        .map_err(|error| format!("{} isn't a valid provider list: {error}", path.display()))?;

    if file.providers.is_empty() {
        return Err(format!("{} lists no providers", path.display()));
    }

    Ok(file
        .providers
        .into_iter()
        .map(ProviderFileEntry::into_listing)
        .collect())
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

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).expect("write provider file");
    }

    #[test]
    fn a_providers_file_should_drive_the_picker() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("providers.json");
        write(
            &file,
            r#"{"providers":[
                {"id":"auru-cloud","name":"Auru Cloud","endpoint":"https://pm.auru.studio",
                 "detail":"Hosted · eu-west","availability":"recommended",
                 "auth_methods":["oauth_device_code"],"recommended":true},
                {"id":"nas","name":"Studio NAS","endpoint":"http://nas.local:4242",
                 "detail":"smb://studio-nas","availability":"on-your-network",
                 "auth_methods":["none"]}
            ]}"#,
        );

        let providers = load_provider_file(&file).expect("load");
        assert_eq!(providers.len(), 2);
        assert_eq!(providers[0].entry.name, "Auru Cloud");
        assert_eq!(providers[0].detail, "Hosted · eu-west");
        assert_eq!(providers[0].availability, ProviderAvailability::Recommended);
        assert!(providers[0].requires_authentication());
        assert_eq!(
            providers[1].availability,
            ProviderAvailability::OnYourNetwork
        );
        assert!(!providers[1].requires_authentication());
    }

    #[test]
    fn the_shipped_example_file_should_load() {
        // It is documentation people will copy; a broken example is worse
        // than none.
        let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("providers.example.json");
        let providers = load_provider_file(&example).expect("the shipped example should load");

        assert!(providers.len() >= 3);
        assert!(
            providers.iter().any(|p| p.requires_authentication()),
            "the example should show a provider that needs signing in"
        );
        assert!(
            providers.iter().any(|p| !p.requires_authentication()),
            "and one that does not, so both hints are visible"
        );
    }

    #[test]
    fn an_entry_that_says_nothing_about_auth_should_claim_nothing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("providers.json");
        write(
            &file,
            r#"{"providers":[{"id":"x","name":"Bare","endpoint":"https://x.example"}]}"#,
        );

        let providers = load_provider_file(&file).expect("load");
        assert!(!providers[0].requires_authentication());
        assert_eq!(providers[0].auth_hint().summary, "No sign-in needed");
        assert_eq!(
            providers[0].detail, "https://x.example",
            "with no detail or description, the endpoint is the most useful line"
        );
    }

    #[test]
    fn a_broken_providers_file_should_explain_itself() {
        let temp = tempfile::tempdir().expect("tempdir");

        let missing = temp.path().join("nope.json");
        let error = load_provider_file(&missing).expect_err("no such file");
        assert!(error.contains("nope.json"), "{error}");

        let malformed = temp.path().join("bad.json");
        write(&malformed, "{ not json");
        let error = load_provider_file(&malformed).expect_err("malformed");
        assert!(error.contains("isn't a valid provider list"), "{error}");

        let empty = temp.path().join("empty.json");
        write(&empty, r#"{"providers":[]}"#);
        let error = load_provider_file(&empty).expect_err("empty");
        assert!(error.contains("lists no providers"), "{error}");
    }

    #[test]
    fn every_auth_method_should_say_what_will_happen() {
        // The hint is shown before someone commits to a provider, so each
        // method needs a summary and a fuller explanation.
        for method in [
            AuthMethod::OAuthDeviceCode,
            AuthMethod::Pat,
            AuthMethod::None,
        ] {
            let hint = AuthHint::for_method(&method);
            assert!(!hint.summary.is_empty());
            assert!(hint.detail.len() > hint.summary.len());
        }

        assert_eq!(
            AuthHint::for_method(&AuthMethod::OAuthDeviceCode).summary,
            "Sign in with your browser"
        );
        assert_eq!(
            AuthHint::for_method(&AuthMethod::Pat).summary,
            "Needs an access token"
        );
    }

    #[test]
    fn the_token_hint_should_promise_the_keychain_not_a_file() {
        // Where a credential ends up is the thing a person most wants to know
        // before pasting one.
        let hint = AuthHint::for_method(&AuthMethod::Pat);
        assert!(hint.detail.contains("keychain"), "{}", hint.detail);
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
