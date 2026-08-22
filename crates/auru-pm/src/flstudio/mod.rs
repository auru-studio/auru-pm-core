//! FL Studio projects on a real filesystem.
//!
//! Reading a `.flp` lives in [`auru_pm_kernel::flstudio`] and is re-exported
//! here unchanged. This module adds the parts that need a disk: gathering the
//! samples a backup must capture, and restoring a project folder.

pub use auru_pm_kernel::flstudio::*;

pub mod assets;
pub mod restore;

pub use assets::{AssetPlan, PlannedAsset, UnresolvedAsset};
pub use restore::RestoreReport;

use auru_pm_kernel::error::Result;

/// Work out what a backup of this project would capture.
pub fn plan_bundle_assets(
    source: &[u8],
    aliases: &[crate::ableton::PathAlias],
) -> Result<AssetPlan> {
    plan_bundle_assets_from_directory(source, None, aliases)
}

/// Work out what a backup captures when the directory containing the `.flp`
/// is known.
///
/// The directory is not treated as owned by the project. It is used only to
/// recover a referenced file with the exact same basename, as produced by
/// FL's zipped-project export.
pub fn plan_bundle_assets_from_directory(
    source: &[u8],
    project_directory: Option<&std::path::Path>,
    aliases: &[crate::ableton::PathAlias],
) -> Result<AssetPlan> {
    Ok(assets::plan_from_directory(
        &read_asset_refs(source)?,
        project_directory,
        aliases,
    ))
}
