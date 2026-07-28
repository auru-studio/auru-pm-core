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

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::hash::ContentHash;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SampleManifest {
    /// Sample entries sorted by `path` ascending. Use [`Self::insert`]
    /// to maintain the invariant.
    pub entries: Vec<SampleEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SampleEntry {
    /// Project-relative logical path of the sample, eg `samples/kick.wav`.
    /// Used for display and for path-collision detection on download.
    pub path: String,
    pub hash: ContentHash,
    pub size: u64,
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

/// Collect the distinct sample file paths referenced by a native Auru project
/// snapshot.
///
/// Walks `channels[].clips[]` and picks out the `file_path` of every audio clip
/// (`data.Audio.file_path`), mirroring how [`crate::diff`] navigates the
/// snapshot. External formats preserve embedded resources inside their
/// normalized snapshot and therefore intentionally produce an empty native
/// sample manifest here. Empty paths are ignored. Returned sorted +
/// de-duplicated so the resulting manifest is deterministic regardless of clip
/// ordering.
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
        }
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
