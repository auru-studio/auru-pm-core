//! Per-commit sample manifest.
//!
//! Every commit's [`crate::TreeRef::samples`] points at a blob whose
//! contents are a [`SampleManifest`] JSON. That manifest enumerates the
//! sample files the project depends on, each keyed by its blake3 CAS
//! hash, so the client can probe locally / lazy-download remotely
//! without having to crack open the project snapshot.
//!
//! Samples are lazy by default: on open we fetch the snapshot + this
//! manifest, then download individual sample blobs on demand (track
//! engage, playback, explicit "fetch all"). Manifest entries are
//! sorted by `path` for determinism — same set of samples must hash
//! to the same blob.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ableton::{BundlePolicy, PlannedAsset};
use crate::hash::ContentHash;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SampleManifest {
    /// Sample entries sorted by `path` ascending. Use [`Self::insert`]
    /// to maintain the invariant.
    pub entries: Vec<SampleEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SampleEntry {
    /// Logical path of the asset.
    ///
    /// Native Auru snapshots record the raw `file_path` from the clip, which
    /// is absolute. Ableton project-folder commits record a path relative to
    /// the project folder with `/` separators, eg
    /// `Samples/Processed/loop.wav` — that is what gets written back out on
    /// restore. Used for display and for path-collision detection on download.
    pub path: String,
    pub hash: ContentHash,
    pub size: u64,
    /// What the asset is, for display and for deciding what to write on
    /// restore. Absent — and omitted from the encoding — for plain samples,
    /// which keeps native manifests byte-identical to those written before
    /// project folders existed.
    #[serde(default, skip_serializing_if = "AssetKind::is_default")]
    pub kind: AssetKind,
    /// Where the asset was gathered from, when it lived outside the project
    /// folder. This is the exact path string the Live Set referenced, and it
    /// is the key that [`crate::ableton::rewrite`] matches on to repoint the
    /// `FileRef` at the vendored copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<String>,
}

/// What an entry in the asset manifest is.
///
/// [`Self::Sample`] is the default and is skipped when encoding, so a manifest
/// containing only samples — every manifest written before project-folder
/// support — encodes to exactly the same bytes as before, and therefore hashes
/// the same and yields the same commit id.
#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum AssetKind {
    /// Audio the project plays.
    #[default]
    Sample,
    /// A `.asd` warp/transient analysis sidecar. Live can regenerate these,
    /// but doing so discards hand-edited warp markers.
    Analysis,
    /// A device preset or rack — `.adv`, `.adg`, `.alp`.
    Preset,
    /// One of Live's own `Backup/` autosaves.
    Backup,
    /// `Ableton Project Info/` contents and platform folder metadata.
    ProjectInfo,
    /// Anything else found inside the project folder.
    Other,
}

impl AssetKind {
    /// Whether this is the variant omitted from the canonical encoding.
    pub const fn is_default(&self) -> bool {
        matches!(self, Self::Sample)
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Sample => "sample",
            Self::Analysis => "analysis",
            Self::Preset => "preset",
            Self::Backup => "backup",
            Self::ProjectInfo => "project info",
            Self::Other => "other",
        }
    }
}

impl SampleEntry {
    /// A plain sample entry — the shape every entry had before project
    /// folders introduced [`Self::kind`] and [`Self::origin`].
    pub fn new(path: impl Into<String>, hash: ContentHash, size: u64) -> Self {
        Self {
            path: path.into(),
            hash,
            size,
            kind: AssetKind::Sample,
            origin: None,
        }
    }
}

impl SampleManifest {
    pub fn new() -> Self {
        Self { entries: vec![] }
    }

    /// Add or replace an entry by `path`, keeping `entries` sorted.
    pub fn insert(&mut self, entry: SampleEntry) {
        match self.entries.binary_search_by(|e| e.path.cmp(&entry.path)) {
            Ok(idx) => self.entries[idx] = entry,
            Err(idx) => self.entries.insert(idx, entry),
        }
    }

    /// Canonical JSON encoding — sorted entries already, plus
    /// serde_json's default sorted keys means the byte sequence is
    /// stable for a given logical manifest.
    pub fn canonical_encoding(&self) -> Result<Vec<u8>, serde_json::Error> {
        // Round-trip through Value so map keys serialize alphabetically
        // (BTreeMap-backed by default), matching the rule in
        // canonical.rs for commit hashing.
        let value = serde_json::to_value(self)?;
        serde_json::to_vec(&value)
    }

    /// blake3 of `canonical_encoding`. The matching value goes into
    /// [`crate::TreeRef::samples`] when building a commit.
    pub fn content_hash(&self) -> Result<ContentHash, serde_json::Error> {
        Ok(ContentHash::of(&self.canonical_encoding()?))
    }
}

/// Work out every file a commit of `snapshot` should store.
///
/// Dispatches on what the project is:
///
/// - **Native Auru** — the audio its clips reference. Absolute clip paths keep
///   their historical behaviour; relative paths resolve from `project_root`
///   while retaining the raw path as the manifest key.
/// - **Ableton Live Set with a project folder** — the folder's contents plus
///   everything referenced from outside it, gathered in.
/// - **FL Studio** — every sample referenced by the event stream, gathered
///   into a `Samples/` folder for a self-contained restore.
/// - **Anything else** — nothing. A loose `.als` has no folder to walk.
///   DAWproject's inline media has no source path, so the push coordinator
///   decodes it directly rather than routing it through this filesystem plan.
pub fn plan_assets(
    snapshot: &Value,
    project_root: Option<&Path>,
    policy: &BundlePolicy,
) -> Vec<PlannedAsset> {
    if crate::ableton::is_ableton_snapshot(snapshot) {
        return match project_root {
            Some(root) => crate::ableton::plan_assets_from_value(snapshot, root, policy).assets,
            None => Vec::new(),
        };
    }
    if snapshot_format(snapshot) == Some(crate::ProjectFormat::FlStudio) {
        return flstudio_assets_from_value(snapshot, policy);
    }
    // Native: the manifest path stays the raw clip path, which keeps existing
    // commits and their ids byte-identical.
    sample_paths_in_snapshot(snapshot)
        .into_iter()
        .map(|path| {
            let recorded = PathBuf::from(&path);
            let source = match (recorded.is_relative(), project_root) {
                (true, Some(root)) => root.join(&recorded),
                _ => recorded,
            };
            PlannedAsset {
                source,
                bundle_path: path,
                kind: AssetKind::Sample,
                origin: None,
            }
        })
        .collect()
}

fn snapshot_format(snapshot: &Value) -> Option<crate::ProjectFormat> {
    serde_json::from_value(snapshot.get("format")?.clone()).ok()
}

/// Rebuild the redacted FL event stream carried by the canonical snapshot,
/// then pass it through the same planner used for a project file on disk.
///
/// Planning remains best-effort like the Ableton path: a snapshot that cannot
/// be reconstructed contributes no assets rather than making the project
/// impossible to commit.
fn flstudio_assets_from_value(snapshot: &Value, policy: &BundlePolicy) -> Vec<PlannedAsset> {
    let source = serde_json::to_vec(snapshot)
        .ok()
        .and_then(|bytes| crate::ProjectSnapshot::from_canonical_bytes(&bytes).ok())
        .and_then(|snapshot| snapshot.restore_bytes().ok());
    let Some(source) = source else {
        return Vec::new();
    };
    crate::flstudio::plan_bundle_assets(&source, &policy.path_aliases)
        .map(|plan| {
            plan.assets
                .into_iter()
                .map(|asset| PlannedAsset {
                    source: asset.source,
                    bundle_path: asset.bundle_path,
                    kind: asset.kind,
                    origin: Some(asset.origin),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Collect the distinct sample file paths referenced by a native Auru project
/// snapshot.
///
/// Walks `channels[].clips[]` and picks out the `file_path` of every audio clip
/// (`data.Audio.file_path`), mirroring how [`crate::diff`] navigates the
/// snapshot. Empty paths are ignored. Returned sorted + de-duplicated so the
/// resulting manifest is deterministic regardless of clip ordering.
///
/// External formats have no `channels` array and yield nothing here — their
/// assets are found by [`plan_assets`] instead, which understands project
/// folders.
pub fn sample_paths_in_snapshot(snapshot: &Value) -> BTreeSet<String> {
    let mut paths = BTreeSet::new();
    let Some(channels) = snapshot.get("channels").and_then(Value::as_array) else {
        return paths;
    };
    for channel in channels {
        let Some(clips) = channel.get("clips").and_then(Value::as_array) else {
            continue;
        };
        for clip in clips {
            let path = clip
                .get("data")
                .and_then(|d| d.get("Audio"))
                .and_then(|a| a.get("file_path"))
                .and_then(Value::as_str);
            if let Some(path) = path {
                if !path.is_empty() {
                    paths.insert(path.to_owned());
                }
            }
        }
    }
    paths
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, bytes: &[u8]) -> SampleEntry {
        SampleEntry {
            path: path.into(),
            hash: ContentHash::of(bytes),
            size: bytes.len() as u64,
            kind: AssetKind::default(),
            origin: None,
        }
    }

    #[test]
    fn a_sample_only_manifest_should_encode_exactly_as_before() {
        // `kind` and `origin` were added for project folders. Both are skipped
        // when they hold their pre-existing meaning, so a manifest of plain
        // samples must encode to the same bytes it always did — otherwise
        // every existing commit id would change and history would fork.
        let mut manifest = SampleManifest::new();
        manifest.insert(entry("samples/kick.wav", b"kick"));

        let encoded =
            String::from_utf8(manifest.canonical_encoding().expect("encode")).expect("valid utf-8");
        assert_eq!(
            encoded,
            format!(
                r#"{{"entries":[{{"hash":"{}","path":"samples/kick.wav","size":4}}]}}"#,
                ContentHash::of(b"kick")
            ),
            "no `kind` or `origin` key may appear for a plain sample"
        );
    }

    #[test]
    fn project_folder_fields_should_appear_only_when_meaningful() {
        let mut manifest = SampleManifest::new();
        manifest.insert(SampleEntry {
            path: "Samples/Imported/break.wav".into(),
            hash: ContentHash::of(b"break"),
            size: 5,
            kind: AssetKind::Preset,
            origin: Some("E:/lib/break.wav".into()),
        });

        let encoded =
            String::from_utf8(manifest.canonical_encoding().expect("encode")).expect("valid utf-8");
        assert!(encoded.contains(r#""kind":"preset""#), "{encoded}");
        assert!(
            encoded.contains(r#""origin":"E:/lib/break.wav""#),
            "{encoded}"
        );
    }

    #[test]
    fn a_manifest_written_before_project_folders_should_still_decode() {
        // Entries in already-committed manifests carry neither new field.
        // Keys are in canonical (alphabetical) order because that is how
        // `canonical_encoding` wrote them.
        let legacy = r#"{"entries":[{"hash":"$HASH","path":"samples/kick.wav","size":4}]}"#
            .replace("$HASH", &ContentHash::of(b"kick").to_string());
        let manifest: SampleManifest = serde_json::from_str(&legacy).expect("decode");

        assert_eq!(manifest.entries[0].kind, AssetKind::Sample);
        assert_eq!(manifest.entries[0].origin, None);
        // And re-encoding it reproduces the original bytes.
        assert_eq!(
            manifest.canonical_encoding().expect("encode"),
            legacy.as_bytes()
        );
    }

    #[test]
    fn plan_assets_should_reproduce_native_sample_paths() {
        // The native path must be unaffected by project-folder support: same
        // paths, same order, and the manifest key stays the raw clip path.
        let snapshot = serde_json::json!({
            "channels": [{ "clips": [
                { "data": { "Audio": { "file_path": "/samples/snare.wav" } } },
                { "data": { "Audio": { "file_path": "/samples/kick.wav" } } },
            ]}]
        });
        let planned = plan_assets(&snapshot, None, &BundlePolicy::default());

        let paths: Vec<&str> = planned
            .iter()
            .map(|asset| asset.bundle_path.as_str())
            .collect();
        assert_eq!(paths, vec!["/samples/kick.wav", "/samples/snare.wav"]);
        assert!(planned.iter().all(|asset| asset.kind == AssetKind::Sample));
        assert!(planned.iter().all(|asset| asset.origin.is_none()));
        assert!(
            planned
                .iter()
                .all(|asset| asset.source.as_os_str() == asset.bundle_path.as_str())
        );
    }

    #[test]
    fn native_relative_sample_paths_should_resolve_from_the_project_folder() {
        let snapshot = serde_json::json!({
            "channels": [{ "clips": [{
                "data": { "Audio": { "file_path": "Samples/kick.wav" } }
            }]}]
        });

        let planned = plan_assets(
            &snapshot,
            Some(Path::new("/projects/Song")),
            &BundlePolicy::default(),
        );

        assert_eq!(
            planned[0].source,
            Path::new("/projects/Song/Samples/kick.wav")
        );
        assert_eq!(planned[0].bundle_path, "Samples/kick.wav");
    }

    #[test]
    fn plan_assets_should_stay_empty_for_dawproject() {
        // Inline bytes have no filesystem source path. The push coordinator
        // adds them to the same manifest through dawproject's archive reader.
        let snapshot = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "dawproject",
            "project": { "root": { "tag": "Project" } }
        });
        assert!(plan_assets(&snapshot, None, &BundlePolicy::default()).is_empty());
    }

    #[test]
    fn plan_assets_should_stay_empty_for_a_live_set_with_no_folder() {
        // A loose `.als` keeps behaving exactly as it did before.
        let snapshot = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "ableton-live-set",
            "project": { "root": { "tag": "Ableton" } }
        });
        assert!(plan_assets(&snapshot, None, &BundlePolicy::default()).is_empty());
    }

    #[test]
    fn plan_assets_should_dispatch_fl_projects_to_their_asset_planner() {
        use crate::flstudio::events::{Event, Header, Stream};
        use crate::flstudio::refs::EVENT_SAMPLE_PATH;
        use crate::{ProjectFormat, ProjectSnapshot};

        let temp = tempfile::tempdir().expect("tempdir");
        let sample = temp.path().join("Kick.wav");
        std::fs::write(&sample, b"audio").expect("write sample");

        let mut encoded_path = Vec::new();
        for unit in sample.to_string_lossy().encode_utf16() {
            encoded_path.extend_from_slice(&unit.to_le_bytes());
        }
        encoded_path.extend_from_slice(&[0, 0]);
        let source = Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events: vec![
                Event::new(
                    crate::flstudio::events::EVENT_VERSION,
                    b"20.5.0.1142\0".to_vec(),
                ),
                Event::new(EVENT_SAMPLE_PATH, encoded_path),
            ],
        }
        .encode();
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source)
            .expect("normalize FL project");
        let value: Value = serde_json::from_slice(snapshot.as_bytes()).expect("snapshot JSON");

        let planned = plan_assets(&value, None, &BundlePolicy::default());

        assert_eq!(planned.len(), 1);
        assert_eq!(planned[0].source, sample);
        assert_eq!(planned[0].bundle_path, "Samples/Kick.wav");
        assert_eq!(
            planned[0].origin.as_deref(),
            Some(sample.to_string_lossy().as_ref())
        );
    }

    #[test]
    fn empty_manifest_hashes() {
        // Empty manifest still gets a stable hash — used by commits
        // with no audio clips.
        let m = SampleManifest::new();
        assert_eq!(m.content_hash().unwrap(), m.content_hash().unwrap());
    }

    #[test]
    fn insert_preserves_sort() {
        let mut m = SampleManifest::new();
        m.insert(entry("z.wav", b"z"));
        m.insert(entry("a.wav", b"a"));
        m.insert(entry("m.wav", b"m"));
        let paths: Vec<&str> = m.entries.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, vec!["a.wav", "m.wav", "z.wav"]);
    }

    #[test]
    fn insert_replaces() {
        let mut m = SampleManifest::new();
        m.insert(entry("a.wav", b"first"));
        m.insert(entry("a.wav", b"second"));
        assert_eq!(m.entries.len(), 1);
        assert_eq!(m.entries[0].hash, ContentHash::of(b"second"));
    }

    #[test]
    fn order_independent_hash() {
        let mut a = SampleManifest::new();
        a.insert(entry("a.wav", b"a"));
        a.insert(entry("b.wav", b"b"));

        let mut b = SampleManifest::new();
        b.insert(entry("b.wav", b"b"));
        b.insert(entry("a.wav", b"a"));

        // Inserted in different orders, but `insert` keeps them sorted
        // so the canonical encoding — and the hash — match.
        assert_eq!(a.content_hash().unwrap(), b.content_hash().unwrap());
    }

    #[test]
    fn extracts_audio_clip_paths() {
        let snapshot = serde_json::json!({
            "channels": [
                {
                    "clips": [
                        { "data": { "Audio": { "file_path": "/samples/kick.wav" } } },
                        { "data": { "Midi": { "notes": [] } } },
                        { "data": { "Audio": { "file_path": "/samples/snare.wav" } } },
                    ]
                },
                {
                    // Duplicate path across channels collapses to one entry.
                    "clips": [
                        { "data": { "Audio": { "file_path": "/samples/kick.wav" } } },
                    ]
                },
            ]
        });
        let paths = sample_paths_in_snapshot(&snapshot);
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        assert_eq!(paths, vec!["/samples/kick.wav", "/samples/snare.wav"]);
    }

    #[test]
    fn tolerates_missing_or_empty_fields() {
        // No channels, no clips, empty path — all yield nothing, no panic.
        assert!(sample_paths_in_snapshot(&serde_json::json!({})).is_empty());
        assert!(sample_paths_in_snapshot(&serde_json::json!({ "channels": [] })).is_empty());
        let snapshot = serde_json::json!({
            "channels": [{ "clips": [
                { "data": { "Audio": { "file_path": "" } } },
                { "data": { "Audio": {} } },
            ]}]
        });
        assert!(sample_paths_in_snapshot(&snapshot).is_empty());
    }
}
