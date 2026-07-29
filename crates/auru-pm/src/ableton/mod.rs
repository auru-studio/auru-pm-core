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

pub mod assets;
pub mod bundle;
pub mod diff;
pub mod meta;
pub mod plugins;
pub mod refs;
pub mod restore;
pub mod rewrite;
pub mod validate;

pub use assets::{AssetPlan, PlannedAsset, UnresolvedAsset};
pub use bundle::{
    AbletonBundle, BundleFile, BundlePolicy, PathAlias, ScanOptions, scan_for_projects,
};
pub use meta::{
    AbletonMetadata, AssetSummary, KeyInfo, TimeSignature, TrackCounts, TrackKind, TrackSummary,
    decode_packed_time_signature,
};
pub use plugins::{PluginFormat, PluginId, PluginRef};
pub use refs::{AssetRef, RefClass, RelativePathType};
pub use restore::{RestoreReport, restore_bundle};
pub use rewrite::{RewriteReport, VendorPlan};
pub use validate::IntegrityProblem;

use std::path::Path;

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

/// Work out what a commit of the project folder containing `project_path`
/// should capture.
///
/// `project_path` is the `.als` or its folder. Returns `Ok(None)` when the
/// path is not part of an Ableton project folder — a loose `.als`, or a
/// project of another format — in which case the caller keeps its existing
/// behaviour and commits the snapshot alone.
pub fn plan_bundle_assets(
    snapshot: &ProjectSnapshot,
    project_path: &Path,
    policy: &BundlePolicy,
) -> Result<Option<AssetPlan>> {
    if snapshot.format() != ProjectFormat::AbletonLiveSet {
        return Ok(None);
    }
    let Some(bundle) = AbletonBundle::detect(project_path)? else {
        return Ok(None);
    };
    Ok(Some(assets::plan(
        &bundle,
        &live_set_root(snapshot)?,
        policy,
    )))
}

/// Whether a canonical snapshot value is an Ableton Live Set.
///
/// A cheap key check, so callers can skip deserializing a multi-megabyte tree
/// for projects of other formats.
pub(crate) fn is_ableton_snapshot(snapshot: &serde_json::Value) -> bool {
    snapshot.get("auru_pm_snapshot").is_some()
        && snapshot.get("format").and_then(serde_json::Value::as_str) == Some("ableton-live-set")
}

/// Plan a project-folder commit straight from a canonical snapshot value.
///
/// The push path holds the snapshot as JSON rather than a [`ProjectSnapshot`],
/// so this is the entry point it uses. Returns an empty plan — never an
/// error — when the project is not a folder-backed Live Set, so a commit is
/// never blocked by a project we cannot fully understand.
pub(crate) fn plan_assets_from_value(
    snapshot: &serde_json::Value,
    project_root: &Path,
    policy: &BundlePolicy,
) -> AssetPlan {
    if !is_ableton_snapshot(snapshot) {
        return AssetPlan::default();
    }
    let Ok(Some(bundle)) = AbletonBundle::detect(project_root) else {
        return AssetPlan::default();
    };
    let Some(root) = root_from_value(snapshot) else {
        return AssetPlan::default();
    };
    assets::plan(&bundle, &root, policy)
}

/// Read project detail straight from a canonical snapshot value.
///
/// `None` for any project that is not a Live Set. Used by
/// [`crate::ProjectInfo`], which works from the snapshot JSON the push path
/// already holds rather than from a [`ProjectSnapshot`].
pub(crate) fn metadata_from_value(snapshot: &serde_json::Value) -> Option<AbletonMetadata> {
    if !is_ableton_snapshot(snapshot) {
        return None;
    }
    root_from_value(snapshot).map(|root| meta::extract(&root))
}

/// Check a canonical snapshot value for Ableton integrity problems.
///
/// Returns empty for any project that is not a Live Set, so callers can run it
/// unconditionally over merge output.
pub(crate) fn validate_snapshot_value(
    snapshot: &serde_json::Value,
) -> Vec<validate::IntegrityProblem> {
    if !is_ableton_snapshot(snapshot) {
        return Vec::new();
    }
    root_from_value(snapshot)
        .map(|root| validate::validate(&root))
        .unwrap_or_default()
}

fn root_from_value(snapshot: &serde_json::Value) -> Option<crate::project_format::XmlElement> {
    use serde::Deserialize as _;
    let portable = crate::project_format::PortableSnapshot::deserialize(snapshot).ok()?;
    Some(portable.project.root)
}

/// Extract the root `Ableton` element, rejecting non-Ableton snapshots.
fn live_set_root(snapshot: &ProjectSnapshot) -> Result<crate::project_format::XmlElement> {
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

#[cfg(test)]
pub(crate) mod test_support {
    use crate::project_format::{XmlDocument, XmlElement};

    /// Parse an XML fragment into the normalized tree the readers walk.
    ///
    /// Keeps the unit tests in this module working on the same representation
    /// as production rather than a hand-built parallel one.
    pub(crate) fn parse_xml(xml: &str) -> XmlElement {
        XmlDocument::parse(xml.as_bytes(), "test XML")
            .expect("test XML parses")
            .root
    }
}
