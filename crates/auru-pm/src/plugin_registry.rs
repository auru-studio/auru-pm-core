//! Telling a person where to get the plugins a project needs.
//!
//! A project's audio is only half of it. Open someone else's Live Set and Live
//! will happily load every sample you brought along and then report five
//! missing devices, because instruments and effects are separate products with
//! their own licences.
//!
//! This module answers one question — *what is this plugin and where does it
//! come from* — and nothing more.
//!
//! # What this deliberately does not do
//!
//! It does not automate licensing, activation, or installation. That is not a
//! gap to be filled later:
//!
//! - Activation APIs are issued to a plugin's own publisher and scoped to that
//!   publisher's products. There is no mechanism by which an unrelated
//!   application could authorize someone's copy of a third-party synth, and
//!   attempting to work around one would be circumventing a licence check.
//! - Subscription catalogues that appear to do this work through per-vendor
//!   commercial agreements plus the vendor's own client, not through anything
//!   a third party can call.
//!
//! What *is* both possible and useful: name the plugin precisely, say whether
//! it is on this computer, and point at where its maker distributes it. For
//! freely redistributable plugins that link can go straight to a release; for
//! everything else it goes to the product page, and the person installs and
//! authorizes it themselves.
//!
//! The reassuring part is true and worth saying plainly: **a project's plugin
//! settings live inside the project file.** Nothing about them is lost by not
//! having the plugin today. Install it, authorize it, reopen the project, and
//! every knob is where it was left.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::str::FromStr as _;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::ableton::{PluginFormat, PluginId, PluginRef};
use crate::error::{Error, Result};

/// Canonical URL of the Auru-curated plugin registry.
pub const AURU_PLUGIN_REGISTRY_URL: &str = "https://pm.auru.studio/plugins.json";

/// 24-hour cache TTL in seconds, matching the provider registry.
const CACHE_TTL_SECS: i64 = 86_400;

/// Registry shipped with the build, so plugin identification works with no
/// network and on the first run.
const BUNDLED_REGISTRY: &str = include_str!("../data/plugins.json");

/// Where a plugin can be obtained.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum PluginSource {
    /// Ships with the DAW. Nothing to obtain; if it is missing, the DAW
    /// installation is incomplete rather than the project.
    ///
    /// The old `bundled-with-live` spelling is still accepted: a hosted
    /// registry written before FL Studio was supported must keep working.
    #[serde(rename = "bundled-with-daw", alias = "bundled-with-live")]
    BundledWithDaw,
    /// Freely redistributable, with an official release to link to.
    ///
    /// Note `formats`: some open-source plugins are licensed such that their
    /// VST2 builds may not be redistributed even though the rest may, so a
    /// link is only offered for the formats the entry actually lists.
    Download {
        url: String,
        /// SPDX-style licence identifier, eg `"GPL-3.0-or-later"`.
        license: String,
        /// Formats this link provides, eg `["VST3", "CLAP"]`.
        formats: Vec<String>,
    },
    /// Commercial, or otherwise not ours to distribute — the maker's page.
    Vendor {
        product_url: String,
        /// Vendor's own installer or account manager, where they have one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        installer_url: Option<String>,
    },
}

impl PluginSource {
    /// A link to show the user, if there is one worth showing.
    pub fn link(&self) -> Option<&str> {
        match self {
            Self::BundledWithDaw => None,
            Self::Download { url, .. } => Some(url),
            Self::Vendor { product_url, .. } => Some(product_url),
        }
    }
}

/// One plugin the registry knows about.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginEntry {
    /// Identity as written by [`PluginId`]'s `Display`, eg `"vst2:1483109208"`.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub vendor: String,
    pub source: PluginSource,
    /// Binary file names this plugin is known by, lowercase.
    ///
    /// FL Studio records no numeric identity for a hosted plugin — only the
    /// file it loaded — so this is the only way one registry entry can serve
    /// both DAWs. Ableton finds the same plugin by `id`; FL finds it here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// Anything a person should know before going to get it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
}

impl PluginEntry {
    /// Parsed identity, or `None` if the entry's key is malformed.
    pub fn identity(&self) -> Option<PluginId> {
        PluginId::from_str(&self.id).ok()
    }
}

/// The registry file's shape.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PluginRegistry {
    #[serde(default)]
    pub plugins: Vec<PluginEntry>,
}

impl PluginRegistry {
    /// Look up a plugin by identity.
    ///
    /// Falls back to matching a file name when the identity is one FL Studio
    /// produced, so a project that loaded `Serum_x64.dll` resolves to the same
    /// entry an Ableton project reaches by its VST2 identifier.
    pub fn get(&self, id: &PluginId) -> Option<&PluginEntry> {
        let key = id.to_string();
        if let Some(entry) = self.plugins.iter().find(|entry| entry.id == key) {
            return Some(entry);
        }

        let PluginId::Vst2ByFile { file_name } = id else {
            return None;
        };
        self.plugins.iter().find(|entry| {
            entry
                .files
                .iter()
                .any(|known| known.eq_ignore_ascii_case(file_name))
        })
    }

    /// Index by identity, skipping entries whose key does not parse.
    pub fn by_identity(&self) -> BTreeMap<PluginId, &PluginEntry> {
        self.plugins
            .iter()
            .filter_map(|entry| Some((entry.identity()?, entry)))
            .collect()
    }
}

/// The registry compiled into this build.
///
/// Parsed once. A malformed bundled file yields an empty registry rather than
/// a panic — an identification feature failing closed is a poor experience,
/// but a crash on startup is a worse one.
pub fn bundled() -> &'static PluginRegistry {
    static BUNDLED: OnceLock<PluginRegistry> = OnceLock::new();
    BUNDLED.get_or_init(|| serde_json::from_str(BUNDLED_REGISTRY).unwrap_or_default())
}

/// Whether a plugin is available on this computer.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginAvailability {
    /// Found on this machine.
    Installed,
    /// Ships with the DAW, so it is present wherever the DAW is.
    #[serde(rename = "bundled-with-daw", alias = "bundled-with-live")]
    BundledWithDaw,
    /// Not found. Phrased throughout as "not on this computer" rather than
    /// "missing", because nothing is wrong with the project — this machine
    /// simply does not have the plugin yet.
    NotOnThisComputer,
    /// Could not be determined, and saying so is better than guessing.
    Unknown,
}

impl PluginAvailability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Installed => "installed",
            Self::BundledWithDaw => "comes with your DAW",
            Self::NotOnThisComputer => "not on this computer",
            Self::Unknown => "unknown",
        }
    }

    /// Whether the person needs to do something before the project loads fully.
    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::NotOnThisComputer)
    }
}

/// A plugin the project uses, resolved against the registry and this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedPlugin {
    pub id: PluginId,
    /// Name from the registry when known, else as the project recorded it.
    pub name: String,
    pub vendor: String,
    pub format: PluginFormat,
    pub instances: usize,
    pub availability: PluginAvailability,
    /// `None` when the registry does not recognise this plugin.
    pub source: Option<PluginSource>,
    pub notes: Option<String>,
}

impl ResolvedPlugin {
    /// Where to send someone who wants this plugin.
    pub fn link(&self) -> Option<&str> {
        self.source.as_ref().and_then(PluginSource::link)
    }

    /// Whether the project will load without this being dealt with.
    pub fn blocks_playback(&self) -> bool {
        self.availability.needs_attention()
    }
}

/// Where to look for installed plugins.
#[derive(Clone, Debug, Default)]
pub struct PluginSearchPaths {
    pub directories: Vec<PathBuf>,
}

impl PluginSearchPaths {
    /// Conventional plugin locations for the running platform, plus anything
    /// in `AURU_VST_PATHS` (separated the way `PATH` is).
    pub fn detect() -> Self {
        let mut directories: Vec<PathBuf> = Vec::new();

        if let Ok(extra) = std::env::var("AURU_VST_PATHS") {
            directories
                .extend(std::env::split_paths(&extra).filter(|path| !path.as_os_str().is_empty()));
        }

        let home = std::env::var_os("HOME").map(PathBuf::from);
        if cfg!(target_os = "macos") {
            directories.push(PathBuf::from("/Library/Audio/Plug-Ins/VST3"));
            directories.push(PathBuf::from("/Library/Audio/Plug-Ins/VST"));
            directories.push(PathBuf::from("/Library/Audio/Plug-Ins/Components"));
            if let Some(home) = &home {
                directories.push(home.join("Library/Audio/Plug-Ins/VST3"));
                directories.push(home.join("Library/Audio/Plug-Ins/VST"));
            }
        } else if cfg!(target_os = "windows") {
            directories.push(PathBuf::from("C:/Program Files/Common Files/VST3"));
            directories.push(PathBuf::from("C:/Program Files/VSTPlugins"));
            directories.push(PathBuf::from("C:/Program Files/Steinberg/VSTPlugins"));
        } else {
            directories.push(PathBuf::from("/usr/lib/vst3"));
            directories.push(PathBuf::from("/usr/lib/lxvst"));
            if let Some(home) = &home {
                directories.push(home.join(".vst3"));
                directories.push(home.join(".vst"));
            }
        }

        Self { directories }
    }

    /// Whether a plugin named `name` appears to be installed.
    ///
    /// Matching is by file stem, case-insensitively, because the name Live
    /// records is the plugin's own and rarely matches the bundle exactly —
    /// `"Serum_x64"` on disk against `"Serum"` in a newer set. Only the top
    /// level of each directory is scanned; recursing into plugin bundles would
    /// cost far more than the answer is worth.
    fn contains(&self, name: &str) -> bool {
        let needle = normalize(name);
        if needle.is_empty() {
            return false;
        }
        self.directories.iter().any(|directory| {
            let Ok(entries) = std::fs::read_dir(directory) else {
                return false;
            };
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .is_some_and(|stem| {
                        let stem = normalize(stem);
                        stem == needle || stem.starts_with(&needle) || needle.starts_with(&stem)
                    })
            })
        })
    }
}

/// Reduce a plugin name to something comparable across how it is written.
fn normalize(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Resolve the plugins a project uses against a registry and this machine.
///
/// Registry lookup and availability are independent: an unrecognised plugin
/// that is installed is fine, and a well-known one that is absent still gets a
/// link. Neither failure hides the other.
pub fn resolve(
    plugins: &[PluginRef],
    registry: &PluginRegistry,
    search_paths: &PluginSearchPaths,
) -> Vec<ResolvedPlugin> {
    plugins
        .iter()
        .map(|plugin| {
            let entry = registry.get(&plugin.id);
            let bundled_with_live = plugin.format.is_native()
                || matches!(
                    entry.map(|entry| &entry.source),
                    Some(PluginSource::BundledWithDaw)
                );

            let availability = if bundled_with_live {
                PluginAvailability::BundledWithDaw
            } else {
                detect_availability(plugin, search_paths)
            };

            ResolvedPlugin {
                id: plugin.id.clone(),
                name: entry.map_or_else(|| plugin.name.clone(), |entry| entry.name.clone()),
                vendor: entry.map(|entry| entry.vendor.clone()).unwrap_or_default(),
                format: plugin.format,
                instances: plugin.instances,
                availability,
                source: entry.map(|entry| entry.source.clone()),
                notes: entry.and_then(|entry| entry.notes.clone()),
            }
        })
        .collect()
}

/// Best-effort check for whether a third-party plugin is on this machine.
fn detect_availability(plugin: &PluginRef, search_paths: &PluginSearchPaths) -> PluginAvailability {
    // The path the project recorded is the strongest signal, when it happens
    // to be a path that exists here.
    if let Some(path) = &plugin.path {
        if Path::new(path).is_file() {
            return PluginAvailability::Installed;
        }
    }
    if search_paths.directories.is_empty() {
        return PluginAvailability::Unknown;
    }
    if search_paths.contains(&plugin.name) {
        PluginAvailability::Installed
    } else {
        PluginAvailability::NotOnThisComputer
    }
}

// ── Fetching ─────────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedRegistry {
    fetched_at: i64,
    source_url: String,
    registry: PluginRegistry,
}

impl CachedRegistry {
    fn is_fresh(&self) -> bool {
        now_epoch_secs() - self.fetched_at < CACHE_TTL_SECS
    }
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Fetch a plugin registry from `url`.
pub async fn fetch(url: &str) -> Result<PluginRegistry> {
    let response = reqwest::Client::new()
        .get(url)
        .timeout(Duration::from_secs(8))
        .send()
        .await
        .map_err(|error| Error::Network(error.to_string()))?;
    if !response.status().is_success() {
        return Err(Error::Network(format!(
            "plugin registry returned HTTP {}",
            response.status()
        )));
    }
    response
        .json()
        .await
        .map_err(|error| Error::Other(format!("plugin registry parse error: {error}")))
}

/// Fetch `url`, cache it for 24 hours, and return the registry.
pub async fn get_or_fetch(url: &str, cache_path: &Path) -> Result<PluginRegistry> {
    if let Some(cached) = std::fs::read_to_string(cache_path)
        .ok()
        .and_then(|text| serde_json::from_str::<CachedRegistry>(&text).ok())
        .filter(CachedRegistry::is_fresh)
    {
        return Ok(cached.registry);
    }

    let registry = fetch(url).await?;
    let cache = CachedRegistry {
        fetched_at: now_epoch_secs(),
        source_url: url.to_owned(),
        registry: registry.clone(),
    };
    // Best-effort: a cache we could not write is not a reason to fail.
    if let Ok(body) = serde_json::to_vec_pretty(&cache) {
        if let Some(parent) = cache_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = cache_path.with_extension("tmp");
        if std::fs::write(&tmp, &body).is_ok() {
            let _ = std::fs::rename(&tmp, cache_path);
        }
    }
    Ok(registry)
}

/// Get the best registry available, never failing.
///
/// Network, then the 24-hour cache, then the registry compiled into this
/// build. Returns whether the result came from the bundled fallback, so the
/// UI can say "this list may be out of date" honestly rather than silently
/// showing stale information.
pub async fn resolve_registry(url: &str, cache_path: &Path) -> (PluginRegistry, bool) {
    match get_or_fetch(url, cache_path).await {
        Ok(registry) => (registry, false),
        Err(_) => (bundled().clone(), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> &'static PluginRegistry {
        bundled()
    }

    fn plugin(name: &str, format: PluginFormat, id: PluginId) -> PluginRef {
        PluginRef {
            name: name.to_owned(),
            format,
            id,
            device_type: None,
            path: None,
            instances: 1,
        }
    }

    fn no_search_paths() -> PluginSearchPaths {
        PluginSearchPaths {
            directories: vec![std::path::PathBuf::from("/nonexistent-auru-test")],
        }
    }

    #[test]
    fn the_bundled_registry_should_parse_and_be_populated() {
        // It is compiled in, so a malformed file is a build-time mistake that
        // would otherwise only show as plugins mysteriously unrecognised.
        assert!(
            !registry().plugins.is_empty(),
            "the bundled registry should ship with entries"
        );
    }

    #[test]
    fn every_bundled_entry_should_have_a_parseable_identity() {
        for entry in &registry().plugins {
            assert!(
                entry.identity().is_some(),
                "entry {:?} has an unusable id {:?}",
                entry.name,
                entry.id
            );
        }
    }

    #[test]
    fn every_bundled_entry_should_offer_a_link_or_be_bundled_with_live() {
        // An entry that neither ships with Live nor says where to get it is
        // worse than no entry: it claims knowledge it does not have.
        for entry in &registry().plugins {
            match &entry.source {
                PluginSource::BundledWithDaw => {}
                source => assert!(
                    source
                        .link()
                        .is_some_and(|link| link.starts_with("https://")),
                    "entry {:?} has no usable link",
                    entry.name
                ),
            }
        }
    }

    #[test]
    fn redistributable_entries_should_declare_a_licence_and_formats() {
        for entry in &registry().plugins {
            if let PluginSource::Download {
                license, formats, ..
            } = &entry.source
            {
                assert!(!license.is_empty(), "{:?} has no licence", entry.name);
                assert!(!formats.is_empty(), "{:?} lists no formats", entry.name);
                assert!(
                    !formats
                        .iter()
                        .any(|format| format.eq_ignore_ascii_case("VST2")),
                    "{:?} offers a VST2 download; VST2 builds of open-source plugins \
                     are frequently not redistributable and must not be linked here",
                    entry.name
                );
            }
        }
    }

    #[test]
    fn the_projects_serum_should_be_recognised() {
        // The identities taken from a real Live 12 set. If these stop
        // resolving, the feature has silently stopped working for the exact
        // case it was built for.
        let serum_vst2 = PluginId::Vst2 {
            unique_id: 1_483_109_208,
        };
        let serum_vst3 = PluginId::Vst3 {
            tuid: [1_448_297_816, 1_718_833_267, 1_701_999_981, 540_147_712],
        };
        for id in [serum_vst2, serum_vst3] {
            let entry = registry().get(&id).expect("Serum should be known");
            assert!(entry.name.contains("Serum"), "{entry:?}");
            assert!(entry.source.link().is_some());
        }
    }

    #[test]
    fn a_known_plugin_that_is_absent_should_get_a_link() {
        let plugins = [plugin(
            "Serum_x64",
            PluginFormat::Vst2,
            PluginId::Vst2 {
                unique_id: 1_483_109_208,
            },
        )];
        let resolved = resolve(&plugins, registry(), &no_search_paths());

        assert_eq!(
            resolved[0].availability,
            PluginAvailability::NotOnThisComputer
        );
        assert!(resolved[0].blocks_playback());
        assert!(resolved[0].link().is_some(), "and somewhere to get it");
        assert_eq!(resolved[0].vendor, "Xfer Records");
    }

    #[test]
    fn an_unknown_plugin_should_still_be_named_and_flagged() {
        // No registry entry is not the same as no information: the project
        // still knows what it needs, and the person can search for it.
        let plugins = [plugin(
            "Mystery Synth",
            PluginFormat::Vst3,
            PluginId::Vst3 { tuid: [9, 9, 9, 9] },
        )];
        let resolved = resolve(&plugins, registry(), &no_search_paths());

        assert_eq!(resolved[0].name, "Mystery Synth");
        assert_eq!(
            resolved[0].availability,
            PluginAvailability::NotOnThisComputer
        );
        assert!(resolved[0].source.is_none());
        assert!(resolved[0].link().is_none());
    }

    #[test]
    fn an_fl_plugin_should_resolve_by_its_file_name() {
        // FL records no numeric identity for a hosted plugin, so the file name
        // is the only way one registry entry can serve both DAWs.
        let entry = bundled()
            .get(&PluginId::Vst2ByFile {
                file_name: "serum_x64.dll".to_owned(),
            })
            .expect("Serum by file name");
        assert_eq!(entry.name, "Serum");
        assert_eq!(entry.vendor, "Xfer Records");
    }

    #[test]
    fn a_file_name_should_match_whatever_case_it_was_written_in() {
        // The name comes out of a path recorded on Windows, where case is not
        // meaningful; matching exactly would miss half of them.
        assert!(
            bundled()
                .get(&PluginId::Vst2ByFile {
                    file_name: "Serum_x64.DLL".to_owned(),
                })
                .is_some()
        );
    }

    #[test]
    fn an_unknown_file_should_not_resolve_to_something_else() {
        // An entry under a wrong identity is worse than no entry: it would
        // name one plugin and link to another.
        assert!(
            bundled()
                .get(&PluginId::Vst2ByFile {
                    file_name: "something_nobody_has_heard_of.dll".to_owned(),
                })
                .is_none()
        );
    }

    #[test]
    fn fl_stock_plugins_should_be_known_and_namespaced_apart_from_live() {
        let limiter = bundled()
            .get(&PluginId::FlNative {
                device: "Fruity Limiter".to_owned(),
            })
            .expect("Fruity Limiter");
        assert_eq!(limiter.source, PluginSource::BundledWithDaw);
        assert_eq!(limiter.vendor, "Image-Line");

        // The same name under Ableton's namespace must not find it.
        assert!(
            bundled()
                .get(&PluginId::Native {
                    device: "Fruity Limiter".to_owned(),
                })
                .is_none(),
            "an FL plugin resolved through Ableton's namespace"
        );
    }

    #[test]
    fn the_previous_spelling_of_the_bundled_source_should_still_parse() {
        // A hosted registry written before FL Studio was supported says
        // `bundled-with-live`; dropping it would break every such entry.
        let json = r#"{"plugins":[{"id":"live:Eq8","name":"EQ Eight","vendor":"Ableton","source":{"kind":"bundled-with-live"}}]}"#;
        let registry: PluginRegistry = serde_json::from_str(json).expect("parse");
        assert_eq!(registry.plugins[0].source, PluginSource::BundledWithDaw);
    }

    #[test]
    fn live_devices_should_never_be_reported_as_missing() {
        // EQ Eight is not something to go and download.
        let plugins = [plugin(
            "Eq8",
            PluginFormat::Native,
            PluginId::Native {
                device: "Eq8".to_owned(),
            },
        )];
        let resolved = resolve(&plugins, registry(), &no_search_paths());

        assert_eq!(resolved[0].availability, PluginAvailability::BundledWithDaw);
        assert!(!resolved[0].blocks_playback());
        assert!(resolved[0].link().is_none(), "nothing to link to");
    }

    #[test]
    fn a_plugin_found_on_disk_should_read_as_installed() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("Serum_x64.dll"), b"plugin").expect("write");
        let paths = PluginSearchPaths {
            directories: vec![temp.path().to_path_buf()],
        };

        let plugins = [plugin(
            "Serum",
            PluginFormat::Vst2,
            PluginId::Vst2 {
                unique_id: 1_483_109_208,
            },
        )];
        let resolved = resolve(&plugins, registry(), &paths);

        assert_eq!(
            resolved[0].availability,
            PluginAvailability::Installed,
            "'Serum' should match 'Serum_x64.dll' on disk"
        );
        assert!(!resolved[0].blocks_playback());
    }

    #[test]
    fn the_path_recorded_in_the_project_should_count_when_it_exists() {
        let temp = tempfile::tempdir().expect("tempdir");
        let installed = temp.path().join("Ozone 8 Elements.dll");
        std::fs::write(&installed, b"plugin").expect("write");

        let mut plugin = plugin(
            "Ozone 8 Elements",
            PluginFormat::Vst2,
            PluginId::Vst2 {
                unique_id: 1_517_176_172,
            },
        );
        plugin.path = Some(installed.to_string_lossy().into_owned());

        let resolved = resolve(&[plugin], registry(), &no_search_paths());
        assert_eq!(resolved[0].availability, PluginAvailability::Installed);
    }

    #[test]
    fn availability_should_be_unknown_when_there_is_nowhere_to_look() {
        // Saying "not on this computer" without having looked would be a
        // guess dressed as a fact.
        let plugins = [plugin(
            "Serum",
            PluginFormat::Vst2,
            PluginId::Vst2 {
                unique_id: 1_483_109_208,
            },
        )];
        let resolved = resolve(&plugins, registry(), &PluginSearchPaths::default());

        assert_eq!(resolved[0].availability, PluginAvailability::Unknown);
        assert!(!resolved[0].blocks_playback(), "unknown is not a blocker");
    }

    #[test]
    fn name_matching_should_survive_the_ways_a_plugin_is_written() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("Serum_x64.dll"), b"x").expect("write");
        let paths = PluginSearchPaths {
            directories: vec![temp.path().to_path_buf()],
        };

        assert!(paths.contains("Serum"));
        assert!(paths.contains("serum_x64"));
        assert!(paths.contains("SERUM X64"));
        assert!(!paths.contains("Massive"));
        assert!(!paths.contains(""), "an empty name matches nothing");
    }

    #[test]
    fn a_malformed_registry_should_degrade_rather_than_crash() {
        let broken: PluginRegistry = serde_json::from_str("{ nonsense").unwrap_or_default();
        assert!(broken.plugins.is_empty());

        let plugins = [plugin(
            "Serum",
            PluginFormat::Vst2,
            PluginId::Vst2 { unique_id: 1 },
        )];
        // Still names the plugin and reports availability.
        let resolved = resolve(&plugins, &broken, &no_search_paths());
        assert_eq!(resolved[0].name, "Serum");
    }

    #[test]
    fn identities_should_index_uniquely() {
        let index = registry().by_identity();
        assert_eq!(
            index.len(),
            registry().plugins.len(),
            "two entries share an identity, so one is unreachable"
        );
    }
}
