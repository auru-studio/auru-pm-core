//! Semantic reading of Ableton Live Sets.
//!
//! [`crate::project_format`] already normalizes a `.als` into a canonical XML
//! tree and back — losslessly enough to commit, merge, and restore. What it
//! deliberately does not do is *understand* the tree: to it, a Live Set is an
//! ordered pile of elements.
//!
//! This module supplies the understanding. It answers three questions the
//! project-management layer needs and the opaque tree cannot:
//!
//! - **What files does this project depend on?** ([`refs`]) — including the
//!   ones outside the project folder, which are exactly the ones that break
//!   when a project moves between machines.
//! - **What instruments and effects does it load?** ([`plugins`]) — with
//!   stable identities, so a plugin the user does not have installed can be
//!   named and pointed at rather than silently failing to load.
//! - **What is this project?** ([`meta`]) — tempo, key, tracks, arrangement
//!   length: the detail a person recognises a project by.
//!
//! Nothing here mutates a snapshot; reading is separated from rewriting so a
//! failure to understand a set can never corrupt it.

pub mod diff;
pub mod meta;
pub mod path_alias;
pub mod plugins;
pub mod refs;
pub mod rewrite;
pub mod validate;

pub use meta::{
    AbletonMetadata, AssetSummary, KeyInfo, TimeSignature, TrackCounts, TrackKind, TrackSummary,
    decode_packed_time_signature,
};
pub use path_alias::PathAlias;
pub use plugins::{PluginFormat, PluginId, PluginRef};
pub use refs::{AssetRef, RefClass, RelativePathType};
pub use rewrite::{RewriteReport, VendorPlan};
pub use validate::IntegrityProblem;

use crate::error::{Error, Result};
use crate::project_format::{ProjectFormat, ProjectSnapshot};

/// Read project detail from an Ableton Live Set snapshot.
///
/// Returns [`Error::ProjectFormat`] if `snapshot` is not a Live Set — callers
/// holding a snapshot of unknown format should check
/// [`ProjectSnapshot::format`] first.
pub fn read_metadata(snapshot: &ProjectSnapshot) -> Result<AbletonMetadata> {
    Ok(meta::extract(&live_set_root(snapshot)?))
}

/// Collect every file the Live Set references.
pub fn read_asset_refs(snapshot: &ProjectSnapshot) -> Result<Vec<AssetRef>> {
    Ok(refs::collect(&live_set_root(snapshot)?))
}

/// Collect the distinct instruments and effects the Live Set loads.
pub fn read_plugins(snapshot: &ProjectSnapshot) -> Result<Vec<PluginRef>> {
    Ok(plugins::collect(&live_set_root(snapshot)?))
}

/// Whether a canonical snapshot value is an Ableton Live Set.
///
/// A cheap key check, so callers can skip deserializing a multi-megabyte tree
/// for projects of other formats.
pub fn is_ableton_snapshot(snapshot: &serde_json::Value) -> bool {
    snapshot.get("auru_pm_snapshot").is_some()
        && snapshot.get("format").and_then(serde_json::Value::as_str) == Some("ableton-live-set")
}

/// Read project detail straight from a canonical snapshot value.
///
/// `None` for any project that is not a Live Set. Used by
/// [`crate::ProjectInfo`], which works from the snapshot JSON the push path
/// already holds rather than from a [`ProjectSnapshot`].
pub fn metadata_from_value(snapshot: &serde_json::Value) -> Option<AbletonMetadata> {
    if !is_ableton_snapshot(snapshot) {
        return None;
    }
    root_from_value(snapshot).map(|root| meta::extract(&root))
}

/// Check a canonical snapshot value for Ableton integrity problems.
///
/// Returns empty for any project that is not a Live Set, so callers can run it
/// unconditionally over merge output.
pub fn validate_snapshot_value(snapshot: &serde_json::Value) -> Vec<validate::IntegrityProblem> {
    if !is_ableton_snapshot(snapshot) {
        return Vec::new();
    }
    root_from_value(snapshot)
        .map(|root| validate::validate(&root))
        .unwrap_or_default()
}

pub fn root_from_value(snapshot: &serde_json::Value) -> Option<crate::project_format::XmlElement> {
    use serde::Deserialize as _;
    let portable = crate::project_format::PortableSnapshot::deserialize(snapshot).ok()?;
    Some(portable.project.root)
}

/// Extract the root `Ableton` element, rejecting non-Ableton snapshots.
pub fn live_set_root(snapshot: &ProjectSnapshot) -> Result<crate::project_format::XmlElement> {
    if snapshot.format() != ProjectFormat::AbletonLiveSet {
        return Err(Error::ProjectFormat(format!(
            "expected an Ableton Live Set snapshot, found {}",
            snapshot.format()
        )));
    }
    let portable = snapshot.portable()?.ok_or_else(|| {
        Error::ProjectFormat("Ableton snapshot is missing its format wrapper".to_owned())
    })?;
    Ok(portable.project.root)
}

/// Helpers for tests that work on Live Set trees.
///
/// Public rather than `#[cfg(test)]` because the crates layered on this one —
/// and the SDK bindings — test against the same representation, and a
/// `cfg(test)` module is invisible to them.
pub mod test_support {
    use crate::project_format::{XmlDocument, XmlElement};

    /// Parse an XML fragment into the normalized tree the readers walk.
    ///
    /// Keeps tests working on the same representation as production rather
    /// than a hand-built parallel one.
    pub fn parse_xml(xml: &str) -> XmlElement {
        XmlDocument::parse(xml.as_bytes(), "test XML")
            .expect("test XML parses")
            .root
    }
}
