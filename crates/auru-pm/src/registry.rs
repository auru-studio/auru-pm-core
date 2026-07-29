//! Marketplace registry — discovery of available `auru-pm-v1` providers.
//!
//! The Auru app fetches `https://pm.auru.studio/providers.json` at startup
//! (cached 24 h locally). Users can add their own registry URLs, each subject
//! to a one-time trust prompt. All registries share the same JSON schema.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::provider::{AuthMethod, Capabilities};

/// Canonical URL of the Auru-curated provider registry.
pub const AURU_REGISTRY_URL: &str = "https://pm.auru.studio/providers.json";

/// 24-hour cache TTL in seconds.
const CACHE_TTL_SECS: i64 = 86_400;

/// How prominently a registry asks for an entry to be shown.
///
/// Deliberately narrower than the app's own notion of availability: a registry
/// can say a provider is recommended, or that it sits on the local network —
/// both facts the registry's author knows — but it cannot claim the user is
/// *connected* to it. That is per-machine state the app owns, and a document
/// fetched over the network has no business asserting it.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum RegistryAvailability {
    #[default]
    Available,
    Recommended,
    OnYourNetwork,
}

/// A single entry in a provider registry.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct RegistryEntry {
    /// Stable provider identifier — matches what the provider returns from
    /// `GET /v1/health`. E.g. `"auru-hosted"` or `"https://collab.studio.com"`.
    ///
    /// This is the value that goes into `KnownProvider.id` in the `.auru`
    /// project file. Projects store this stable ID rather than the raw
    /// endpoint URL, so URL changes on the provider side are transparent.
    pub id: String,
    /// Human-readable display name.
    pub name: String,
    /// Base URL for the `auru-pm-v1` API. May change when a provider moves;
    /// callers should resolve at runtime via [`resolve_endpoint`] rather
    /// than caching this in project files.
    pub endpoint: String,
    /// What a provider does, as it advertises itself.
    ///
    /// Defaulted because a registry is a hand-maintained document and a
    /// provider that declares nothing claims nothing. The authoritative answer
    /// comes from the provider's own `/v1/health` once connected.
    #[serde(default)]
    pub capabilities: Capabilities,
    /// How this provider authenticates.
    ///
    /// Empty means the entry claims nothing, which consumers read as
    /// [`AuthMethod::None`] — a registry is hand-maintained, and an entry that
    /// omits this should load rather than be rejected.
    #[serde(default)]
    pub auth_methods: Vec<AuthMethod>,
    /// Optional icon (PNG/SVG URL) for the provider picker UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// Short description shown in the marketplace panel.
    #[serde(default)]
    pub description: String,
    /// The line under the name, eg `"Hosted · au-melb · encrypted at rest"`.
    ///
    /// Registry data rather than presentation: where a provider stores things
    /// and how is a fact about the provider, and only the registry knows it.
    #[serde(default)]
    pub detail: String,
    /// Whether this provider is surfaced in the marketplace's default list
    /// (shown when the filter is empty). Non-recommended entries are still
    /// searchable, just hidden behind a filter query.
    #[serde(default)]
    pub recommended: bool,
    /// How the registry asks for this entry to be presented.
    ///
    /// Redundant with `recommended` for the common case, and that is fine:
    /// `recommended` predates it and stays authoritative when this is absent.
    #[serde(default, deserialize_with = "lenient_availability")]
    pub availability: RegistryAvailability,
}

/// Resolve a `KnownProvider.id` to a live endpoint URL.
///
/// Two cases:
/// * **URL-style IDs** (`http://…` / `https://…`) — used by custom-URL
///   providers and pre-stable-ID projects. Returned as-is for backwards compat.
/// * **Stable registry IDs** (everything else, e.g. `"auru-hosted"`) — looked
///   up in `registry`. Returns `None` when the ID is not found in the registry,
///   which the caller should surface as "provider not configured".
pub fn resolve_endpoint(id: &str, registry: &[RegistryEntry]) -> Option<String> {
    if id.starts_with("http://") || id.starts_with("https://") {
        // Legacy URL-style ID — the ID itself is the endpoint URL.
        return Some(id.to_owned());
    }
    registry
        .iter()
        .find(|e| e.id == id)
        .map(|e| e.endpoint.clone())
}

/// Read an availability hint, falling back rather than failing.
///
/// A presentation hint this build does not recognise — a newer registry, or a
/// typo — must not cost the reader the whole provider list. Everything else in
/// the entry is still usable, and losing every provider over one cosmetic
/// field would be a poor trade.
fn lenient_availability<'de, D>(
    deserializer: D,
) -> std::result::Result<RegistryAvailability, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    Ok(match raw.as_deref() {
        Some("recommended") => RegistryAvailability::Recommended,
        Some("on-your-network") => RegistryAvailability::OnYourNetwork,
        _ => RegistryAvailability::Available,
    })
}

/// A registry document — the list of providers a registry serves.
///
/// One schema for every source. The published Auru list, a studio's own
/// registry URL, and a file passed with `--providers-file` are the same
/// document, so someone can save one, edit it, and hand it back.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct RegistryDocument {
    #[serde(default)]
    pub providers: Vec<RegistryEntry>,
}

impl RegistryDocument {
    /// Parse a registry document.
    ///
    /// Accepts a bare array as well as the `{"providers": […]}` envelope: the
    /// envelope is what is published, and the bare form is what earlier builds
    /// of this crate expected. Refusing one of them would strand whichever
    /// registries had been written against it.
    pub fn parse(text: &str) -> Result<Self> {
        // Report the failure for the shape the document actually looks like.
        // Falling through to the array parser and reporting *its* complaint
        // gave "expected a sequence" for an enveloped document with one bad
        // field, which sends the reader looking in entirely the wrong place.
        if text.trim_start().starts_with('[') {
            let providers: Vec<RegistryEntry> = serde_json::from_str(text)
                .map_err(|error| Error::Other(format!("registry parse: {error}")))?;
            return Ok(Self { providers });
        }
        serde_json::from_str::<Self>(text)
            .map_err(|error| Error::Other(format!("registry parse: {error}")))
    }
}

/// On-disk registry cache format.
#[derive(Debug, Serialize, Deserialize)]
pub struct RegistryCache {
    /// When this cache was populated (Unix epoch seconds).
    pub fetched_at: i64,
    /// URL this cache was fetched from.
    pub source_url: String,
    pub entries: Vec<RegistryEntry>,
}

impl RegistryCache {
    pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        now - self.fetched_at < CACHE_TTL_SECS
    }
}

/// Fetch a registry from `url`. Returns entries on success.
pub async fn fetch(url: &str) -> Result<Vec<RegistryEntry>> {
    let resp = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Error::Network(format!(
            "registry fetch failed ({status}): {body}"
        )));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    Ok(RegistryDocument::parse(&body)?.providers)
}

/// Load the cached registry for `url` from `cache_path`. Returns `None` when
/// the file is absent or stale (older than 24 h).
pub fn load_cache(cache_path: &Path) -> Option<RegistryCache> {
    let text = std::fs::read_to_string(cache_path).ok()?;
    let cache: RegistryCache = serde_json::from_str(&text).ok()?;
    if cache.is_fresh() { Some(cache) } else { None }
}

/// Atomically write a registry cache to `cache_path`.
pub fn save_cache(cache_path: &Path, cache: &RegistryCache) -> Result<()> {
    if let Some(parent) = cache_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(cache)?;
    let tmp = cache_path.with_extension("tmp");
    std::fs::write(&tmp, &body)?;
    std::fs::rename(&tmp, cache_path).map_err(Error::Io)
}

/// Fetch `url`, save to `cache_path`, and return entries. Uses the existing
/// cache if it is fresh (≤24 h old), otherwise re-fetches.
pub async fn get_or_fetch(url: &str, cache_path: &Path) -> Result<Vec<RegistryEntry>> {
    if let Some(cached) = load_cache(cache_path) {
        return Ok(cached.entries);
    }
    let entries = fetch(url).await?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cache = RegistryCache {
        fetched_at: now,
        source_url: url.to_owned(),
        entries: entries.clone(),
    };
    // Best-effort — don't fail the fetch if the cache write fails.
    let _ = save_cache(cache_path, &cache);
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Exactly what `https://pm.auru.studio/providers.json` serves today.
    ///
    /// Pinned verbatim: if this stops parsing the app has no providers, and it
    /// fails silently — an empty picker rather than an error.
    const PUBLISHED: &str = r#"{"providers":[{"id":"auru-cloud","name":"Auru Cloud","endpoint":"https://pm.auru.studio","detail":"Hosted · au-melb · encrypted at rest","availability":"recommended","auth_methods":["oauth_device_code"],"description":"Hosted backup with encrypted storage","recommended":true}]}"#;

    #[test]
    fn the_published_registry_should_parse() {
        let document = RegistryDocument::parse(PUBLISHED).expect("parse");
        let entry = &document.providers[0];

        assert_eq!(entry.id, "auru-cloud");
        assert_eq!(entry.endpoint, "https://pm.auru.studio");
        assert_eq!(entry.detail, "Hosted · au-melb · encrypted at rest");
        assert_eq!(entry.auth_methods, vec![AuthMethod::OAuthDeviceCode]);
        assert!(entry.recommended);
    }

    #[test]
    fn an_entry_that_declares_no_capabilities_should_claim_none() {
        // A registry is a hand-maintained document; requiring every field
        // would make a reasonable one fail to load. The authoritative answer
        // comes from the provider's own health endpoint once connected.
        let document = RegistryDocument::parse(PUBLISHED).expect("parse");
        assert_eq!(document.providers[0].capabilities, Capabilities::default());
    }

    #[test]
    fn a_bare_array_should_still_parse() {
        // The shape earlier builds of this crate expected. Refusing it would
        // strand any registry already written against it.
        let bare = r#"[{"id":"one","name":"One","endpoint":"https://one.example"}]"#;
        assert_eq!(
            RegistryDocument::parse(bare)
                .expect("parse")
                .providers
                .len(),
            1
        );
    }

    #[test]
    fn something_that_is_not_a_registry_should_be_refused() {
        assert!(RegistryDocument::parse("not json").is_err());
    }

    #[test]
    fn auru_should_be_resolvable_as_a_provider_from_its_own_registry() {
        // Auru is both things: it publishes the registry and appears in it.
        // Resolving its id must yield its endpoint like any other provider's.
        let document = RegistryDocument::parse(PUBLISHED).expect("parse");
        assert_eq!(
            resolve_endpoint("auru-cloud", &document.providers).as_deref(),
            Some("https://pm.auru.studio")
        );
    }

    #[test]
    fn a_custom_url_provider_should_resolve_without_a_registry() {
        // Somebody running their own server is not in anyone's registry, and
        // must not need to be.
        assert_eq!(
            resolve_endpoint("https://nas.studio.local:3000", &[]).as_deref(),
            Some("https://nas.studio.local:3000")
        );
    }
}

#[cfg(test)]
mod lenient_tests {
    use super::*;

    #[test]
    fn an_unknown_availability_should_not_cost_the_whole_list() {
        // One cosmetic field a newer registry uses must not take every provider
        // with it. The entry is still perfectly usable.
        let json = r#"{"providers":[{"id":"a","name":"A","endpoint":"https://a.example","availability":"sparkly"}]}"#;
        let document = RegistryDocument::parse(json).expect("parse");

        assert_eq!(document.providers.len(), 1);
        assert_eq!(
            document.providers[0].availability,
            RegistryAvailability::Available
        );
    }

    #[test]
    fn a_broken_envelope_should_be_reported_as_an_envelope() {
        // Falling through to the array parser reported "expected a sequence"
        // for a document that plainly was not one, sending whoever maintains
        // the registry looking in the wrong place entirely.
        let json = r#"{"providers":[{"name":"missing an id"}]}"#;
        let error = RegistryDocument::parse(json).expect_err("should fail");
        let message = error.to_string();

        assert!(message.contains("id"), "{message}");
        assert!(!message.contains("sequence"), "{message}");
    }
}
