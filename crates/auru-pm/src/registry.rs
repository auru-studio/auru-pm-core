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
    pub capabilities: Capabilities,
    pub auth_methods: Vec<AuthMethod>,
    /// Optional icon (PNG/SVG URL) for the provider picker UI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
    /// Short description shown in the marketplace panel.
    #[serde(default)]
    pub description: String,
    /// Whether this provider is surfaced in the marketplace's default list
    /// (shown when the filter is empty). Non-recommended entries are still
    /// searchable, just hidden behind a filter query.
    #[serde(default)]
    pub recommended: bool,
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

    resp.json::<Vec<RegistryEntry>>()
        .await
        .map_err(|e| Error::Other(format!("registry parse: {e}")))
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
