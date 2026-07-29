use std::path::{Path, PathBuf};

use auru_pm::{
    AURU_REGISTRY_URL, AuthMethod, Capabilities, RegistryAvailability, RegistryDocument,
    RegistryEntry, fetch_registry,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogState {
    /// The published list is being fetched; no providers are shown yet.
    Loading,
    Live,
    /// The list could not be reached.
    ///
    /// Distinct from [`Self::Loading`] because there is nothing more to wait
    /// for, and the person is owed the reason rather than a spinner that never
    /// resolves. Nothing is invented to fill the gap — a fabricated provider
    /// list is worse than an empty one, since the entries would be things the
    /// user cannot actually connect to.
    Unreachable,
    /// Supplied by `--providers-file`.
    FromFile,
}

impl CatalogState {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Loading => "CHECKING FOR PROVIDERS…",
            Self::Live => "FIRST-PARTY LIST",
            Self::Unreachable => "COULD NOT REACH THE PROVIDER LIST",
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
    /// Compact standards label shown above the connect screen.
    pub eyebrow: &'static str,
    /// Call-to-action shown on the connect button.
    pub action: &'static str,
    /// Whether the connect screen needs a credential input.
    pub accepts_credential: bool,
}

impl AuthHint {
    pub const fn for_method(method: &AuthMethod) -> Self {
        match method {
            AuthMethod::OAuthAuthorizationCodePkce => Self {
                summary: "Sign in with your browser",
                detail: "Auru opens your provider's sign-in page in your browser and waits \
                         for its secure loopback callback. Your password is never typed into \
                         this app or stored here.",
                eyebrow: "OAUTH + PKCE",
                action: "SIGN IN WITH BROWSER →",
                accepts_credential: false,
            },
            AuthMethod::OAuthDeviceCode => Self {
                summary: "Sign in with your browser",
                detail: "Auru shows you a short code and opens your browser. \
                         Your password is never typed into this app or stored here.",
                eyebrow: "OAUTH DEVICE CODE",
                action: "BEGIN PROVIDER SIGN-IN →",
                accepts_credential: false,
            },
            AuthMethod::Pat => Self {
                summary: "Needs an access token",
                detail: "Create an access token in your provider's account settings and \
                         paste it here. It is kept in your operating system's keychain, \
                         not in Auru's own files.",
                eyebrow: "PERSONAL ACCESS TOKEN",
                action: "CONNECT SECURELY →",
                accepts_credential: true,
            },
            AuthMethod::None => Self {
                summary: "No sign-in needed",
                detail: "This provider is on your own machine or network, so there is \
                         nothing to sign in to.",
                eyebrow: "NO AUTHENTICATION",
                action: "CONNECT →",
                accepts_credential: false,
            },
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderListing {
    pub entry: RegistryEntry,
    pub detail: String,
    pub availability: ProviderAvailability,
}

impl ProviderListing {
    /// Present a registry entry in the picker.
    pub fn from_registry(entry: RegistryEntry) -> Self {
        // The registry's own one-liner, falling back to whatever else it said.
        let detail = [&entry.detail, &entry.description, &entry.endpoint]
            .into_iter()
            .find(|value| !value.is_empty())
            .cloned()
            .unwrap_or_default();

        // What the registry asked for, falling back to the older
        // `recommended` flag. `Connected` is never reachable from here: it is
        // per-machine state the app applies afterwards, and a document fetched
        // over the network must not be able to claim it.
        let availability = match entry.availability {
            RegistryAvailability::Recommended => ProviderAvailability::Recommended,
            RegistryAvailability::OnYourNetwork => ProviderAvailability::OnYourNetwork,
            RegistryAvailability::Available if entry.recommended => {
                ProviderAvailability::Recommended
            }
            RegistryAvailability::Available => ProviderAvailability::Available,
        };

        Self {
            entry,
            detail,
            availability,
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

    pub fn is_connected(&self) -> bool {
        self.availability == ProviderAvailability::Connected
    }

    /// What signing in to this provider will involve.
    pub fn auth_hint(&self) -> AuthHint {
        AuthHint::for_method(&self.preferred_auth_method())
    }
}

/// Read a registry from a file.
///
/// A `--providers-file` is just a registry that happens to live on disk, so it
/// goes through the same parser as the published one.
pub fn load_provider_file(path: &Path) -> Result<Vec<ProviderListing>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("couldn't read {}: {error}", path.display()))?;
    parse_provider_list(&text, &path.display().to_string())
}

/// Read a provider list, from wherever it came.
///
/// One parser for the published list and for `--providers-file`, because they
/// are the same document: someone can save what the server publishes, edit it,
/// and pass it back with the flag, and get exactly what they expect. Two
/// schemas for one concept would drift, and the drift would only show up as a
/// provider list that mysteriously fails to load.
fn parse_provider_list(text: &str, source: &str) -> Result<Vec<ProviderListing>, String> {
    let document = RegistryDocument::parse(text)
        .map_err(|error| format!("{source} isn't a valid provider list: {error}"))?;

    if document.providers.is_empty() {
        return Err(format!("{source} lists no providers"));
    }

    Ok(document
        .providers
        .into_iter()
        .map(ProviderListing::from_registry)
        .collect())
}

/// Prefix marking a provider that is a folder rather than a server.
const LOCAL_ID_PREFIX: &str = "local:";

/// A destination that is just a folder — an external drive, or a NAS share
/// mounted on this machine.
///
/// Not every safe copy needs a server. A NAS with no Auru software on it is a
/// perfectly good second home for a project, and treating it as a provider that
/// happens to need no authentication means the rest of the app — pushing,
/// history, restore — works against it unchanged.
///
/// Stored as a `file://` endpoint so it is distinguishable from an HTTP
/// provider by inspection rather than by a separate flag.
pub fn local_provider(path: &Path) -> ProviderListing {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Local folder")
        .to_owned();

    ProviderListing {
        entry: RegistryEntry {
            // Path-derived, so adding the same folder twice replaces rather
            // than duplicates it.
            id: format!("{LOCAL_ID_PREFIX}{}", path.display()),
            name,
            endpoint: format!("file://{}", path.display()),
            capabilities: Capabilities::default(),
            // A folder authenticates nobody. Saying so explicitly is what stops
            // the connect screen asking for a token that does not exist.
            auth_methods: vec![AuthMethod::None],
            icon_url: None,
            description: path.display().to_string(),
            detail: path.display().to_string(),
            recommended: false,
            availability: RegistryAvailability::OnYourNetwork,
        },
        detail: path.display().to_string(),
        availability: ProviderAvailability::OnYourNetwork,
    }
}

impl ProviderListing {
    /// Whether this destination is a folder on this machine or network.
    pub fn is_local(&self) -> bool {
        self.entry.id.starts_with(LOCAL_ID_PREFIX)
    }

    /// The folder behind a local destination.
    ///
    /// The bridge to `FilesystemProvider`.
    pub fn local_path(&self) -> Option<PathBuf> {
        self.entry
            .id
            .strip_prefix(LOCAL_ID_PREFIX)
            .map(PathBuf::from)
    }
}

/// Fetch the published registry.
///
/// The default source of providers, cached for a day by
/// [`auru_pm::registry`] so a launch is not held up by the network.
/// `--providers-file` overrides it, which is how a studio points the app at
/// its own list.
pub fn fetch_first_party_catalog() -> Result<Vec<ProviderListing>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("provider catalog runtime: {error}"))?;

    let entries = runtime
        .block_on(fetch_registry(AURU_REGISTRY_URL, &catalog_cache_path()))
        .map_err(|error| error.to_string())?;

    if entries.is_empty() {
        return Err(format!("{AURU_REGISTRY_URL} lists no providers"));
    }
    Ok(entries
        .into_iter()
        .map(ProviderListing::from_registry)
        .collect())
}

/// Where the registry is cached between launches.
fn catalog_cache_path() -> PathBuf {
    std::env::temp_dir()
        .join("auru-pm")
        .join("provider-catalog.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly what `https://pm.auru.studio/providers.json` serves today.
    ///
    /// Pinned verbatim because the app is useless if this stops parsing, and
    /// the failure would be silent — an empty picker rather than an error.
    const PUBLISHED: &str = r#"{"providers":[{"id":"auru-cloud","name":"Auru Cloud","endpoint":"https://pm.auru.studio","detail":"Hosted · au-melb · encrypted at rest","availability":"recommended","auth_methods":["oauth_device_code"],"description":"Hosted backup with encrypted storage","recommended":true}]}"#;

    #[test]
    fn the_published_provider_list_should_parse() {
        let providers = parse_provider_list(PUBLISHED, "published").expect("parse");

        assert_eq!(providers.len(), 1);
        assert_eq!(providers[0].entry.name, "Auru Cloud");
        assert_eq!(providers[0].entry.endpoint, "https://pm.auru.studio");
        assert_eq!(providers[0].detail, "Hosted · au-melb · encrypted at rest");
        assert_eq!(providers[0].availability, ProviderAvailability::Recommended);
    }

    #[test]
    fn the_published_list_and_the_flag_should_read_the_same_document() {
        // Someone can save what the server publishes, edit it, and pass it back
        // with --providers-file. Two schemas for one concept would drift, and
        // the drift would surface only as a list that fails to load.
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("providers.json");
        std::fs::write(&path, PUBLISHED).expect("write");

        let from_file = load_provider_file(&path).expect("from file");
        let from_network = parse_provider_list(PUBLISHED, "published").expect("from network");

        assert_eq!(from_file.len(), from_network.len());
        assert_eq!(from_file[0].entry.id, from_network[0].entry.id);
        assert_eq!(from_file[0].detail, from_network[0].detail);
    }

    #[test]
    fn an_empty_or_broken_list_should_be_an_error_not_an_empty_picker() {
        // A list that parses to nothing is a misconfiguration, not a state to
        // render silently.
        assert!(parse_provider_list(r#"{"providers":[]}"#, "test").is_err());
        assert!(parse_provider_list("not json", "test").is_err());
    }

    #[test]
    fn oauth_provider_should_require_authentication() {
        let providers = parse_provider_list(PUBLISHED, "published").expect("parse");

        assert!(providers[0].requires_authentication());
        assert_eq!(
            providers[0].preferred_auth_method(),
            AuthMethod::OAuthDeviceCode
        );
    }

    #[test]
    fn provider_advertising_none_should_connect_directly() {
        let json = r#"{"providers":[{"id":"studio-nas","name":"Studio NAS","endpoint":"http://nas.local:3000","auth_methods":["none"]}]}"#;
        let providers = parse_provider_list(json, "test").expect("parse");

        assert!(!providers[0].requires_authentication());
        assert_eq!(providers[0].preferred_auth_method(), AuthMethod::None);
    }

    #[test]
    fn an_entry_that_says_nothing_about_availability_should_be_available() {
        let json = r#"{"providers":[{"id":"remote","name":"Remote","endpoint":"https://remote.example","description":"Remote provider"}]}"#;
        let providers = parse_provider_list(json, "test").expect("parse");

        assert_eq!(providers[0].availability, ProviderAvailability::Available);
    }

    #[test]
    fn a_registry_should_not_be_able_to_claim_you_are_connected() {
        // Connection is per-machine state. A document fetched over the network
        // asserting it would show a provider as ready to use when it is not,
        // and the type system is what stops that rather than a check.
        let json = r#"{"providers":[{"id":"a","name":"A","endpoint":"https://a.example","availability":"connected"}]}"#;
        let providers = parse_provider_list(json, "test").expect("parse");
        assert_ne!(providers[0].availability, ProviderAvailability::Connected);
    }

    #[test]
    fn a_registry_may_say_a_provider_is_on_your_network() {
        // A studio's own registry knows this about its own NAS.
        let json = r#"{"providers":[{"id":"nas","name":"NAS","endpoint":"http://nas.local:3000","availability":"on-your-network"}]}"#;
        let providers = parse_provider_list(json, "test").expect("parse");
        assert_eq!(
            providers[0].availability,
            ProviderAvailability::OnYourNetwork
        );
    }

    #[test]
    fn a_local_folder_should_need_no_account() {
        // The whole point of a local destination: a NAS share with no Auru
        // software on it is a valid second home, and asking for a token that
        // does not exist would make it unusable.
        let listing = local_provider(Path::new("/mnt/nas/Auru Backups"));

        assert!(listing.is_local());
        assert!(!listing.requires_authentication());
        assert_eq!(listing.preferred_auth_method(), AuthMethod::None);
        assert_eq!(listing.entry.name, "Auru Backups");
        assert_eq!(
            listing.local_path().as_deref(),
            Some(Path::new("/mnt/nas/Auru Backups"))
        );
    }

    #[test]
    fn the_same_folder_should_always_produce_the_same_identity() {
        // Identity is the path, so adding a folder twice replaces rather than
        // duplicating it.
        let path = Path::new("/mnt/nas/Backups");
        assert_eq!(local_provider(path).entry.id, local_provider(path).entry.id);
        assert_ne!(
            local_provider(path).entry.id,
            local_provider(Path::new("/mnt/other/Backups")).entry.id
        );
    }

    #[test]
    fn a_fetched_provider_should_not_be_mistaken_for_a_local_one() {
        let providers = parse_provider_list(PUBLISHED, "published").expect("parse");
        assert!(!providers[0].is_local());
        assert_eq!(providers[0].local_path(), None);
    }

    #[test]
    fn an_unreachable_list_should_be_distinguishable_from_a_loading_one() {
        // Both leave the picker empty; only one is worth waiting on, so the
        // states must not read alike.
        assert_ne!(
            CatalogState::Loading.label(),
            CatalogState::Unreachable.label()
        );
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
            AuthMethod::OAuthAuthorizationCodePkce,
            AuthMethod::OAuthDeviceCode,
            AuthMethod::Pat,
            AuthMethod::None,
        ] {
            let hint = AuthHint::for_method(&method);
            assert!(!hint.summary.is_empty());
            assert!(hint.detail.len() > hint.summary.len());
            assert!(!hint.eyebrow.is_empty());
            assert!(!hint.action.is_empty());
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
}

#[cfg(test)]
mod live_fetch {
    /// Hits the network. Ignored by default; run with
    /// `cargo test --offline -- --ignored live_fetch`.
    #[test]
    #[ignore = "requires network"]
    fn the_published_list_should_be_reachable_and_parse() {
        let providers = super::fetch_first_party_catalog().expect("fetch");
        assert!(!providers.is_empty());
        for provider in &providers {
            println!(
                "  {} · {} · {:?}",
                provider.entry.name, provider.detail, provider.availability
            );
        }
    }
}
