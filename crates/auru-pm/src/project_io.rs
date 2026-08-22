//! Reading and writing project files on a real filesystem.
//!
//! The kernel converts project *bytes* to and from canonical snapshots and
//! knows nothing about where they came from. These are the wrappers that add
//! the disk: open a file, detect its format from its name, write a restored
//! project back out without clobbering anything.

use std::path::Path;

use auru_pm_kernel::error::{Error, Result};
use auru_pm_kernel::hash::ContentHash;
use auru_pm_kernel::project_format::{ProjectFormat, ProjectSnapshot};

/// Stream a file through BLAKE3 without loading it all into memory.
///
/// The hashing is [`ContentHash::of_reader`]; only opening the file needs a
/// filesystem, so only that part lives here.
pub fn hash_file(path: &Path) -> std::io::Result<ContentHash> {
    ContentHash::of_reader(std::fs::File::open(path)?)
}

/// Reconstruct the source project file at `path`.
pub fn restore_snapshot_to_path(snapshot: &ProjectSnapshot, path: &Path) -> Result<()> {
    if let Some(destination_format) = ProjectFormat::from_path(path) {
        if destination_format != snapshot.format() {
            return Err(Error::ProjectFormat(format!(
                "cannot restore {} snapshot to '{}'; expected .{}",
                snapshot.format(),
                path.display(),
                snapshot.format().extension()
            )));
        }
    }
    crate::verified_io::write_verified_new(path, &snapshot.restore_bytes()?)?;
    Ok(())
}

/// Read a project file and convert it to canonical PM snapshot JSON.
///
/// The normalization itself is [`ProjectSnapshot::from_source_bytes`] in the
/// kernel; this adds the read and the path-based format detection.
pub fn snapshot_project(path: &Path) -> Result<ProjectSnapshot> {
    let source = std::fs::read(path)?;
    let format = ProjectFormat::detect(path, &source)?;
    ProjectSnapshot::from_source_bytes(format, &source)
}

/// Reconstruct a project file from canonical PM snapshot JSON.
///
/// This byte-only convenience cannot restore a detached DAWproject v2
/// snapshot fetched from a provider. Fetch its manifest resources, hydrate a
/// [`ProjectSnapshot`] with [`crate::dawproject::hydrate_embedded_assets`], and
/// call [`restore_snapshot_to_path`] instead.
pub fn restore_project(snapshot_bytes: &[u8], path: &Path) -> Result<ProjectFormat> {
    let snapshot = ProjectSnapshot::from_canonical_bytes(snapshot_bytes)?;
    restore_snapshot_to_path(&snapshot, path)?;
    Ok(snapshot.format())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_hash_should_match_the_same_bytes_in_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("large-enough-to-stream.bin");
        let bytes = b"auru restore verification".repeat(8_192);
        std::fs::write(&path, &bytes).unwrap();

        assert_eq!(hash_file(&path).unwrap(), ContentHash::of(&bytes));
    }

    #[test]
    fn mismatched_restore_extension_should_be_rejected() {
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Auru, br#"{"version":8}"#)
            .expect("valid Auru JSON");
        let error = restore_snapshot_to_path(&snapshot, Path::new("song.als"))
            .expect_err("mismatched extension must fail");
        assert!(error.to_string().contains("cannot restore Auru snapshot"));
    }

    #[test]
    fn restoring_a_snapshot_should_never_overwrite_an_existing_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("song.auru");
        std::fs::write(&destination, b"existing project").expect("existing project");
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Auru, br#"{"version":8}"#)
            .expect("valid Auru JSON");

        restore_snapshot_to_path(&snapshot, &destination)
            .expect_err("an explicit collision choice is required above the core API");

        assert_eq!(
            std::fs::read(destination).expect("existing project"),
            b"existing project"
        );
    }
}
