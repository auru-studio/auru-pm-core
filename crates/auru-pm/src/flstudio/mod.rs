//! Semantic reading of FL Studio projects.
//!
//! The counterpart to [`crate::ableton`], for a format that shares none of its
//! assumptions. Where a Live Set is compressed XML inside a project folder, an
//! FL project is a single `.flp` file holding a binary event stream, and it
//! has no folder at all — the files sampled during design sat loose among
//! unrelated downloads.
//!
//! Two consequences shape everything here:
//!
//! - **The commit unit is the file, not its folder.** Treating the containing
//!   directory as the project would sweep in whatever else happens to live
//!   beside it. Assets are read where they are and only ever materialised
//!   beside the project on *restore*; a backup never reorganises anyone's
//!   files.
//! - **The event stream round-trips byte for byte.** Verified against real
//!   projects from FL 12 and FL 20. That is a stronger guarantee than the
//!   Ableton path offers, and it is worth keeping — see [`events`].
//!
//! Reading is separated from rewriting, as in [`crate::ableton`], so a failure
//! to understand a project can never corrupt it.

pub mod assets;
pub mod diff;
pub mod events;
pub mod meta;
pub mod plugins;
pub mod refs;
pub mod restore;
pub(crate) mod tree;

pub use assets::{AssetPlan, PlannedAsset, UnresolvedAsset};
pub use events::{Event, Header, MAX_FLP_BYTES, Stream};
pub use meta::{AssetSummary, FlStudioMetadata};
pub use refs::{AssetRef, RefClass};
pub use restore::RestoreReport;

/// Work out what a backup of this project would capture.
pub fn plan_bundle_assets(
    source: &[u8],
    aliases: &[crate::ableton::PathAlias],
) -> Result<AssetPlan> {
    Ok(assets::plan(&read_asset_refs(source)?, aliases))
}

/// Read project detail from a `.flp`.
pub fn read_metadata(source: &[u8]) -> Result<FlStudioMetadata> {
    Ok(meta::extract(&Stream::decode(source)?))
}

/// Every file a `.flp` depends on.
pub fn read_asset_refs(source: &[u8]) -> Result<Vec<AssetRef>> {
    Ok(refs::collect(&Stream::decode(source)?))
}

/// Read FL Studio detail out of a canonical snapshot value.
///
/// `None` for any snapshot that is not an FL project, so callers can run it
/// unconditionally over a snapshot of unknown format.
pub(crate) fn metadata_from_value(snapshot: &serde_json::Value) -> Option<FlStudioMetadata> {
    // Deserialised rather than compared against a literal: the wire spelling
    // comes from serde's rename rule, and a hard-coded string that drifted
    // from it would silently stop producing metadata rather than fail.
    let format: crate::project_format::ProjectFormat =
        serde_json::from_value(snapshot.get("format")?.clone()).ok()?;
    if format != crate::project_format::ProjectFormat::FlStudio {
        return None;
    }
    let document: XmlDocument = serde_json::from_value(snapshot.get("project")?.clone()).ok()?;
    Some(meta::extract(&tree::from_document(&document).ok()?))
}

/// Every plugin a `.flp` loads.
pub fn read_plugins(source: &[u8]) -> Result<Vec<crate::ableton::PluginRef>> {
    Ok(plugins::collect(&Stream::decode(source)?))
}

use crate::error::Result;
use crate::project_format::XmlDocument;

/// Decode a `.flp` into the canonical tree a snapshot carries.
pub(crate) fn to_tree(source: &[u8]) -> Result<XmlDocument> {
    Ok(tree::to_document(&Stream::decode(source)?))
}

/// Re-encode the canonical tree back into `.flp` bytes.
pub(crate) fn from_tree(document: &XmlDocument) -> Result<Vec<u8>> {
    Ok(tree::from_document(document)?.encode())
}

/// Whether `source` looks like an FL Studio project.
///
/// A content check rather than an extension check, so a project renamed or
/// handed over without its extension is still recognised.
pub fn is_flp(source: &[u8]) -> bool {
    source.starts_with(b"FLhd")
}

/// Put a project through Auru's canonical representation and back.
///
/// Comparing the output against the input is the honest test of whether the
/// format is understood: "it parsed without an error" means very little for a
/// stream that has no length prefixes and cannot be resynchronised after a
/// misread. Deliberately *without* [`redact`], so that this stays a check of
/// the codec rather than of the commit policy layered on top of it.
pub fn normalize_bytes(source: &[u8]) -> Result<Vec<u8>> {
    from_tree(&to_tree(source)?)
}

/// The event carrying the FL Studio registration name.
///
/// Its value identifies the licence holder — real projects carry strings like
/// `ez:57h2vAv0@>=B>C;8`. It is not needed to open a project, and FL writes a
/// fresh one on the next save.
pub const EVENT_REG_NAME: u8 = 200;

/// Remove identifying data a backup has no business carrying.
///
/// A committed project is a thing people share, and anything stored travels
/// with every copy of it. The registration name is stripped rather than
/// preserved: it says who owns the FL licence, contributes nothing to opening
/// the project, and would otherwise be handed to everyone the project is ever
/// shared with.
///
/// This is the single, deliberate exception to byte-exact round-tripping —
/// see `flp_round_trip_should_differ_only_in_the_redacted_reg_name`. The event
/// is emptied rather than deleted so the stream keeps its shape, and FL
/// repopulates it the next time the project is saved.
///
/// Returns how many events were emptied.
pub fn redact(stream: &mut Stream) -> usize {
    let terminator: &[u8] = if events::uses_utf16(stream.major_version()) {
        &[0, 0]
    } else {
        &[0]
    };

    let mut redacted = 0;
    for event in &mut stream.events {
        // Already empty is not a redaction, and counting it as one would make
        // the report claim to have removed something that was not there.
        if event.id == EVENT_REG_NAME && event.payload.len() > terminator.len() {
            event.payload = terminator.to_vec();
            redacted += 1;
        }
    }
    redacted
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_real_project_should_be_recognised() {
        assert!(is_flp(b"FLhd\x06\x00\x00\x00"));
        assert!(!is_flp(b"PK\x03\x04"), "a zip is not a project");
        assert!(!is_flp(b""), "nor is nothing");
    }

    #[test]
    fn metadata_should_be_readable_back_out_of_a_committed_snapshot() {
        // The path the library list actually uses: a commit stores the tree,
        // and the detail is read from that rather than from the `.flp`.
        let source = Stream {
            header: Header {
                format: 0,
                channels: 4,
                ppq: 96,
            },
            events: vec![
                Event::new(events::EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
                Event::new(156, 174_000u32.to_le_bytes()),
            ],
        }
        .encode();

        let snapshot =
            crate::ProjectSnapshot::from_source_bytes(crate::ProjectFormat::FlStudio, &source)
                .expect("snapshot");
        let value: serde_json::Value = serde_json::from_slice(snapshot.as_bytes()).expect("parse");

        let meta = metadata_from_value(&value).expect("FL metadata");
        assert_eq!(meta.tempo, Some(174.0));
        assert_eq!(meta.channels, 4);
    }

    #[test]
    fn an_ableton_snapshot_should_not_yield_fl_metadata() {
        // Callers run this over snapshots of unknown format, so a wrong answer
        // here would attach FL detail to a Live Set.
        let value = serde_json::json!({ "format": "ableton-live-set", "project": {} });
        assert!(metadata_from_value(&value).is_none());
    }

    #[test]
    fn normalising_an_unedited_project_should_change_nothing() {
        let source = Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events: vec![
                Event::new(events::EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
                Event::new(156, 92_000u32.to_le_bytes()),
                Event::new(213, vec![0x00, 0xff, 0x7f, 0x80]),
            ],
        }
        .encode();

        assert_eq!(normalize_bytes(&source).expect("normalize"), source);
    }
}
