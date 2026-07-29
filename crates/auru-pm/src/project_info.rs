//! The small, readable summary of what a commit contains.
//!
//! A project snapshot is the whole truth but an awkward thing to ask a
//! question of: a real Live Set is 7 MB of canonical JSON describing about
//! a hundred thousand elements. Showing a library of projects with their
//! tempo and key would mean downloading and parsing all of that per project,
//! per render.
//!
//! So every commit also stores this — a few kilobytes saying what the project
//! *is*. It is derived from the snapshot at commit time and referenced by
//! [`crate::Commit::metadata`], so a client can show a project without ever
//! fetching the project.
//!
//! The envelope is format-agnostic on purpose. Ableton is the only format with
//! anything to say today; adding another later fills in a new field rather
//! than changing the shape of a commit.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::ableton::AbletonMetadata;
use crate::flstudio::FlStudioMetadata;
use crate::hash::ContentHash;
use crate::project_format::ProjectFormat;

/// Schema version of the [`ProjectInfo`] blob.
///
/// Separate from the snapshot schema: this is a derived summary, so a reader
/// that does not understand a newer version can ignore it and fall back to
/// reading the snapshot, rather than refusing the commit.
pub const PROJECT_INFO_SCHEMA: u32 = 1;

/// What a commit's project is, in a form small enough to fetch eagerly.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct ProjectInfo {
    pub schema: u32,
    pub format: ProjectFormat,
    /// Present for Ableton Live Sets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ableton: Option<AbletonMetadata>,
    /// Present for FL Studio projects.
    ///
    /// A separate field rather than a shared shape: the two DAWs describe a
    /// project in genuinely different terms — Ableton has scenes and a scale,
    /// FL has patterns and a channel rack — and flattening them into one
    /// struct would mean inventing values neither format records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flstudio: Option<FlStudioMetadata>,
}

impl ProjectInfo {
    /// Derive the summary for a canonical snapshot.
    ///
    /// `None` when there is nothing worth summarizing — a native Auru project
    /// or a DAWproject, whose detail this crate does not yet read. Returning
    /// `None` rather than an empty summary keeps commits for those formats
    /// byte-identical to what they were before this existed.
    pub fn from_snapshot(snapshot: &Value) -> Option<Self> {
        if let Some(flstudio) = crate::flstudio::metadata_from_value(snapshot) {
            return Some(Self {
                schema: PROJECT_INFO_SCHEMA,
                format: ProjectFormat::FlStudio,
                ableton: None,
                flstudio: Some(flstudio),
            });
        }
        let ableton = crate::ableton::metadata_from_value(snapshot)?;
        Some(Self {
            schema: PROJECT_INFO_SCHEMA,
            format: ProjectFormat::AbletonLiveSet,
            ableton: Some(ableton),
            flstudio: None,
        })
    }

    /// Derive the summary from canonical snapshot bytes.
    ///
    /// The form most callers have: [`ProjectSnapshot::as_bytes`] and blobs
    /// fetched from a provider are both byte slices. `None` for bytes that are
    /// not a snapshot, or a snapshot with nothing to summarize.
    ///
    /// [`ProjectSnapshot::as_bytes`]: crate::ProjectSnapshot::as_bytes
    pub fn from_snapshot_bytes(bytes: &[u8]) -> Option<Self> {
        Self::from_snapshot(&serde_json::from_slice(bytes).ok()?)
    }

    /// Canonical JSON, with map keys in sorted order — the same rule commit
    /// hashing uses, so the same summary always hashes to the same blob.
    pub fn canonical_encoding(&self) -> Result<Vec<u8>, serde_json::Error> {
        let value = serde_json::to_value(self)?;
        serde_json::to_vec(&value)
    }

    pub fn content_hash(&self) -> Result<ContentHash, serde_json::Error> {
        Ok(ContentHash::of(&self.canonical_encoding()?))
    }

    /// Whether this summary came from a schema this build understands.
    ///
    /// A newer writer means the fields present may not be the fields expected;
    /// callers should fall back to reading the snapshot rather than showing
    /// something they have half-understood.
    pub const fn is_readable(&self) -> bool {
        self.schema <= PROJECT_INFO_SCHEMA
    }

    /// One-line description for a project row, eg `"175 BPM · 4/4 · C Phrygian"`.
    ///
    /// Skips anything the set did not declare rather than inventing defaults —
    /// a project with no key should say nothing about its key.
    pub fn headline(&self) -> String {
        let Some(ableton) = &self.ableton else {
            return self.format.to_string();
        };
        let mut parts = Vec::new();
        if let Some(tempo) = ableton.tempo {
            parts.push(format!("{} BPM", format_tempo(tempo)));
        }
        if let Some(signature) = ableton.time_signature {
            parts.push(signature.to_string());
        }
        if let Some(key) = &ableton.key {
            parts.push(key.label());
        }
        if parts.is_empty() {
            self.format.to_string()
        } else {
            parts.join(" · ")
        }
    }
}

/// Render a tempo the way Live displays it — no trailing `.0` on whole numbers.
fn format_tempo(tempo: f64) -> String {
    if (tempo.fract()).abs() < f64::EPSILON {
        format!("{tempo:.0}")
    } else {
        format!("{tempo:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ableton::{KeyInfo, TimeSignature};

    fn live_set_snapshot(body: &str) -> Value {
        let xml = format!(
            r#"<Ableton MajorVersion="5" Creator="Ableton Live 12.0.25"><LiveSet>{body}</LiveSet></Ableton>"#
        );
        let snapshot = crate::ProjectSnapshot::from_source_bytes(
            ProjectFormat::AbletonLiveSet,
            xml.as_bytes(),
        )
        .expect("normalize");
        serde_json::from_slice(snapshot.as_bytes()).expect("parse canonical")
    }

    fn full_set() -> Value {
        live_set_snapshot(
            r#"<MainTrack><DeviceChain><Mixer>
                 <Tempo><Manual Value="175" /></Tempo>
                 <TimeSignature><Manual Value="201" /></TimeSignature>
               </Mixer></DeviceChain></MainTrack>
               <ScaleInformation><RootNote Value="0" /><Name Value="Phrygian" /></ScaleInformation>
               <InKey Value="true" />"#,
        )
    }

    #[test]
    fn a_live_set_should_summarize_its_musical_detail() {
        let info = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        let ableton = info.ableton.as_ref().expect("ableton detail");

        assert_eq!(info.format, ProjectFormat::AbletonLiveSet);
        assert_eq!(ableton.tempo, Some(175.0));
        assert_eq!(
            ableton.time_signature,
            Some(TimeSignature {
                numerator: 4,
                denominator: 4
            })
        );
        assert_eq!(
            ableton.key,
            Some(KeyInfo {
                root_note: 0,
                scale_name: "Phrygian".to_owned(),
                in_key: true,
            })
        );
    }

    #[test]
    fn the_headline_should_read_the_way_a_musician_would_say_it() {
        let info = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        assert_eq!(info.headline(), "175 BPM · 4/4 · C Phrygian");
    }

    #[test]
    fn a_fractional_tempo_should_keep_its_precision() {
        let snapshot = live_set_snapshot(
            r#"<MainTrack><DeviceChain><Mixer>
                 <Tempo><Manual Value="174.5" /></Tempo>
               </Mixer></DeviceChain></MainTrack>"#,
        );
        let info = ProjectInfo::from_snapshot(&snapshot).expect("summary");
        assert_eq!(info.headline(), "174.50 BPM");
    }

    #[test]
    fn a_set_with_nothing_declared_should_fall_back_to_the_format_name() {
        // Better than inventing a tempo the project never stated.
        let info = ProjectInfo::from_snapshot(&live_set_snapshot("")).expect("summary");
        assert_eq!(info.headline(), "Ableton Live Set");
    }

    #[test]
    fn non_ableton_projects_should_produce_no_summary() {
        // Native Auru and DAWproject commits must stay exactly as they were
        // before summaries existed.
        let native = serde_json::json!({ "bpm": 120, "channels": [], "version": 8 });
        assert!(ProjectInfo::from_snapshot(&native).is_none());

        let dawproject = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "dawproject",
            "project": { "root": { "tag": "Project" } }
        });
        assert!(ProjectInfo::from_snapshot(&dawproject).is_none());
    }

    #[test]
    fn the_same_project_should_always_hash_the_same() {
        let first = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        let second = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        assert_eq!(
            first.content_hash().expect("hash"),
            second.content_hash().expect("hash")
        );
    }

    #[test]
    fn a_summary_should_round_trip_through_its_encoding() {
        let info = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        let bytes = info.canonical_encoding().expect("encode");
        let decoded: ProjectInfo = serde_json::from_slice(&bytes).expect("decode");
        assert_eq!(decoded, info);
        assert!(decoded.is_readable());
    }

    #[test]
    fn a_summary_from_a_newer_build_should_be_declared_unreadable() {
        // So a client falls back to the snapshot instead of rendering fields
        // it has only half understood.
        let mut info = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        info.schema = PROJECT_INFO_SCHEMA + 1;
        assert!(!info.is_readable());
    }

    #[test]
    fn a_summary_should_be_small_enough_to_fetch_eagerly() {
        // The entire reason this exists: a real snapshot is ~7 MB.
        let info = ProjectInfo::from_snapshot(&full_set()).expect("summary");
        let bytes = info.canonical_encoding().expect("encode");
        assert!(
            bytes.len() < 8 * 1024,
            "summary grew to {} bytes; it must stay cheap to fetch",
            bytes.len()
        );
    }
}
