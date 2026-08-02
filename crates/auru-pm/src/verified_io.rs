//! No-clobber filesystem writes whose bytes are verified after they reach disk.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::Path;

use crate::error::{Error, Result};
use crate::hash::ContentHash;

/// Create a new file, flush it, then re-read it through BLAKE3.
///
/// Existing files are deliberately refused. Restore callers must make an
/// explicit collision decision before reaching this boundary.
pub(crate) fn write_verified_new(path: &Path, bytes: &[u8]) -> Result<ContentHash> {
    let expected = ContentHash::of(bytes);
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = std::fs::remove_file(path);
        return Err(Error::Io(error));
    }
    drop(file);

    if let Err(error) = verify_file(path, expected) {
        let _ = std::fs::remove_file(path);
        return Err(error);
    }
    Ok(expected)
}

pub(crate) fn verify_file(path: &Path, expected: ContentHash) -> Result<()> {
    let actual = ContentHash::of_file(path)?;
    if actual == expected {
        return Ok(());
    }
    Err(Error::Other(format!(
        "written file '{}' failed BLAKE3 verification: expected {expected}, read {actual}",
        path.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_write_should_refuse_to_replace_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.bin");
        std::fs::write(&path, b"working copy").unwrap();

        write_verified_new(&path, b"restored copy").expect_err("must not overwrite");

        assert_eq!(std::fs::read(path).unwrap(), b"working copy");
    }

    #[test]
    fn verified_write_should_match_the_bytes_after_flushing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("project.bin");

        let hash = write_verified_new(&path, b"restored copy").unwrap();

        assert_eq!(hash, ContentHash::of_file(&path).unwrap());
    }
}
