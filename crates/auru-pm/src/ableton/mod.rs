//! Ableton Live Sets on a real filesystem.
//!
//! Reading and understanding a Live Set lives in
//! [`auru_pm_kernel::ableton`] and is re-exported here unchanged, so
//! `auru_pm::ableton::read_metadata` still resolves. What this module adds is
//! everything that needs a disk: detecting a project folder, enumerating it,
//! planning which files a commit must gather, and restoring one.

pub use auru_pm_kernel::ableton::*;

pub mod assets;
pub mod bundle;
pub mod restore;

pub use assets::{AssetPlan, PlannedAsset, UnresolvedAsset};
pub use bundle::{AbletonBundle, BundleFile, BundlePolicy, ScanOptions, scan_for_projects};
pub use restore::{RestoreReport, restore_bundle};

use std::path::Path;

use auru_pm_kernel::error::Result;
use auru_pm_kernel::project_format::{ProjectFormat, ProjectSnapshot};

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
        &auru_pm_kernel::ableton::live_set_root(snapshot)?,
        policy,
    )))
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
    if !auru_pm_kernel::ableton::is_ableton_snapshot(snapshot) {
        return AssetPlan::default();
    }
    let Ok(Some(bundle)) = AbletonBundle::detect(project_root) else {
        return AssetPlan::default();
    };
    let Some(root) = auru_pm_kernel::ableton::root_from_value(snapshot) else {
        return AssetPlan::default();
    };
    assets::plan(&bundle, &root, policy)
}
