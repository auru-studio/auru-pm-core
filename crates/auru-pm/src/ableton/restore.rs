//! Writing a committed project back out as a working Ableton project folder.
//!
//! This is the step that makes a backup worth having. It recreates the folder,
//! writes every file the commit captured, and repoints the Live Set at the
//! copies that travelled with it — so a project saved on one machine opens
//! with its media intact on another that has never seen the original paths.
//!
//! Restoring is deliberately conservative about the destination. Writing a
//! project folder means writing many files, and doing that into the wrong
//! directory would be destructive in a way no undo covers, so a destination
//! that is not obviously safe is refused rather than merged into.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ableton::bundle::PROJECT_INFO_DIR;
use crate::ableton::rewrite::{self, RewriteReport, VendorPlan};
use crate::commit::Commit;
use crate::error::{Error, Result};
use crate::project_format::{ProjectFormat, ProjectSnapshot};
use crate::provider::ProjectProvider;
use crate::sample_manifest::SampleManifest;

/// What a restore produced.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RestoreReport {
    pub root: PathBuf,
    /// The `.als` to open.
    pub live_set: PathBuf,
    pub files_written: usize,
    pub bytes_written: u64,
    /// How references were repointed. [`RewriteReport::is_complete`] answers
    /// whether the project will open with everything it needs.
    pub rewrite: RewriteReport,
    /// Files the commit listed that could not be fetched. Empty in the normal
    /// case; non-empty means the project is missing media through no fault of
    /// the person restoring it, and they should be told.
    pub unavailable: Vec<String>,
}

/// Recreate the project folder for `commit` at `destination`.
///
/// `destination` is the project folder itself, and must not already contain a
/// different project — see [`ensure_safe_destination`].
pub async fn restore_bundle(
    provider: &dyn ProjectProvider,
    commit: &Commit,
    destination: &Path,
    live_set_name: &str,
) -> Result<RestoreReport> {
    let snapshot_bytes = provider.get_blob(&commit.tree.snapshot).await?;
    let snapshot = ProjectSnapshot::from_canonical_bytes(&snapshot_bytes)?;
    if snapshot.format() != ProjectFormat::AbletonLiveSet {
        return Err(Error::ProjectFormat(format!(
            "cannot restore a {} project as an Ableton project folder",
            snapshot.format()
        )));
    }

    let manifest_bytes = provider.get_blob(&commit.tree.samples).await?;
    let manifest: SampleManifest = serde_json::from_slice(&manifest_bytes)?;

    ensure_safe_destination(destination, live_set_name)?;
    fs::create_dir_all(destination)?;

    let mut report = RestoreReport {
        root: destination.to_path_buf(),
        live_set: destination.join(live_set_name),
        ..RestoreReport::default()
    };

    // Write the folder's files first, so the Live Set is only produced once
    // everything it will point at is actually on disk.
    for entry in &manifest.entries {
        let Some(relative) = safe_relative_path(&entry.path) else {
            // A manifest path that escapes the folder cannot be honoured. The
            // commit is malformed, but refusing one entry is better than
            // writing outside the directory the user chose.
            report.unavailable.push(entry.path.clone());
            continue;
        };
        let bytes = match provider.get_blob(&entry.hash).await {
            Ok(bytes) => bytes,
            Err(_) => {
                report.unavailable.push(entry.path.clone());
                continue;
            }
        };
        let target = destination.join(&relative);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        write_atomically(&target, &bytes)?;
        report.files_written += 1;
        report.bytes_written += bytes.len() as u64;
    }

    // Then repoint the set at what was just written.
    let mut portable = snapshot
        .portable()?
        .ok_or_else(|| Error::ProjectFormat("Ableton snapshot has no wrapper".to_owned()))?;
    let plan = vendor_plan(&manifest);
    report.rewrite = rewrite::rewrite_file_refs(&mut portable.project.root, &plan, destination);

    let rewritten = ProjectSnapshot::from_portable(portable)?;
    write_atomically(&report.live_set, &rewritten.restore_bytes()?)?;

    // Live recreates this on save, but a folder without it does not read as a
    // project until then.
    fs::create_dir_all(destination.join(PROJECT_INFO_DIR))?;

    Ok(report)
}

/// Map each gathered file's original reference to where it now lives.
fn vendor_plan(manifest: &SampleManifest) -> VendorPlan {
    VendorPlan::new(manifest.entries.iter().filter_map(|entry| {
        entry
            .origin
            .as_ref()
            .map(|origin| (origin.clone(), entry.path.clone()))
    }))
}

/// Refuse a destination that holds something other than this project.
///
/// Accepted: a path that does not exist, an empty directory, or a directory
/// already holding this project's `.als`. Anything else — someone's Documents
/// folder, a different project — is refused, because restoring writes many
/// files and there is no undo for overwriting the wrong ones.
fn ensure_safe_destination(destination: &Path, live_set_name: &str) -> Result<()> {
    if !destination.exists() {
        return Ok(());
    }
    if !destination.is_dir() {
        return Err(Error::ProjectFormat(format!(
            "'{}' is a file; restore needs a folder",
            destination.display()
        )));
    }
    let mut entries = fs::read_dir(destination)?.peekable();
    if entries.peek().is_none() {
        return Ok(());
    }
    if destination.join(live_set_name).is_file() {
        return Ok(());
    }
    Err(Error::ProjectFormat(format!(
        "'{}' already contains other files. Restore into a new or empty folder, \
         or one holding this project's '{live_set_name}'.",
        destination.display()
    )))
}

/// Reject manifest paths that would write outside the project folder.
///
/// Manifest paths are folder-relative with `/` separators. Anything absolute,
/// drive-qualified, or containing `..` is refused — a commit should never
/// carry one, and honouring it would let a malformed or hostile commit write
/// anywhere the user can.
fn safe_relative_path(path: &str) -> Option<PathBuf> {
    if path.is_empty() || path.starts_with('/') || path.starts_with('\\') {
        return None;
    }
    let mut chars = path.chars();
    let drive_qualified =
        matches!(chars.next(), Some(c) if c.is_ascii_alphabetic()) && chars.next() == Some(':');
    if drive_qualified {
        return None;
    }

    let mut out = PathBuf::new();
    for segment in path.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return None,
            segment => out.push(segment),
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// Write via a temporary file and rename, so an interrupted restore never
/// leaves a half-written sample that Live would try to play.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".auru-tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, bytes)?;
    match fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(error) => {
            let _ = fs::remove_file(&tmp);
            Err(Error::Io(error))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_paths_that_escape_the_folder_should_be_refused() {
        // A commit should never carry one of these; honouring it would let a
        // malformed commit write anywhere the user can.
        assert_eq!(safe_relative_path("../../etc/passwd"), None);
        assert_eq!(safe_relative_path("/etc/passwd"), None);
        assert_eq!(safe_relative_path(r"\windows\system32"), None);
        assert_eq!(safe_relative_path("C:/Windows/System32"), None);
        assert_eq!(safe_relative_path("Samples/../../outside.wav"), None);
        assert_eq!(safe_relative_path(""), None);
    }

    #[test]
    fn ordinary_manifest_paths_should_be_accepted() {
        assert_eq!(
            safe_relative_path("Samples/Imported/break.wav"),
            Some(PathBuf::from("Samples/Imported/break.wav"))
        );
        assert_eq!(
            safe_relative_path("Ableton Project Info/AProject.ico"),
            Some(PathBuf::from("Ableton Project Info/AProject.ico"))
        );
        // A no-op `.` segment is harmless.
        assert_eq!(
            safe_relative_path("./Samples/a.wav"),
            Some(PathBuf::from("Samples/a.wav"))
        );
    }

    #[test]
    fn a_missing_or_empty_destination_should_be_accepted() {
        let temp = tempfile::tempdir().expect("tempdir");
        assert!(ensure_safe_destination(&temp.path().join("new"), "song.als").is_ok());
        assert!(ensure_safe_destination(temp.path(), "song.als").is_ok());
    }

    #[test]
    fn a_destination_holding_this_project_should_be_accepted() {
        // Restoring over an earlier restore of the same project is the normal
        // way to move back to a previous version.
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("song.als"), b"set").expect("write");
        fs::create_dir_all(temp.path().join("Samples")).expect("mkdir");
        assert!(ensure_safe_destination(temp.path(), "song.als").is_ok());
    }

    #[test]
    fn a_destination_holding_someone_elses_files_should_be_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("taxes.pdf"), b"important").expect("write");

        let error = ensure_safe_destination(temp.path(), "song.als")
            .expect_err("restoring over unrelated files must be refused");
        assert!(
            error.to_string().contains("already contains other files"),
            "the message should say why: {error}"
        );
    }

    #[test]
    fn a_file_destination_should_be_refused() {
        let temp = tempfile::tempdir().expect("tempdir");
        let file = temp.path().join("not-a-folder");
        fs::write(&file, b"x").expect("write");
        assert!(ensure_safe_destination(&file, "song.als").is_err());
    }
}
