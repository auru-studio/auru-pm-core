//! The UI-to-core bridge.
//!
//! GPUI owns presentation and interaction; this module owns the blocking and
//! asynchronous work needed to turn those interactions into real project
//! history. Keeping it out of `main.rs` also gives the backup/restore path a
//! testable seam that does not need a window.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use auru_pm::{
    AuthorIdentity, CommitId, CommitSummary, FilesystemProvider, HistoryRange, HttpProvider,
    ProjectFormat, ProjectProvider, ProjectSnapshot, PushOutcome, SampleManifest, Sidecar,
    flstudio, push_with_freshness_check, sidecar_path_for,
};

use crate::catalog::ProviderListing;

#[derive(Debug)]
pub enum BackupResult {
    Committed(Vec<CommitSummary>),
    NeedsResolution(usize),
    NeedsReview(usize),
}

#[derive(Debug)]
pub struct RestoreResult {
    pub project_file: PathBuf,
    pub files_written: usize,
    pub unavailable: usize,
}

/// Back up one project and return its refreshed history.
pub fn back_up(
    listing: ProviderListing,
    project_path: PathBuf,
    display_name: String,
) -> Result<BackupResult, String> {
    runtime()?.block_on(async move {
        let provider = open_provider(&listing, &project_path).await?;
        let snapshot = ProjectSnapshot::load(&project_path)
            .map_err(|error| format!("read {}: {error}", project_path.display()))?;
        let provider_id = listing.entry.id.clone();
        let author = AuthorIdentity {
            display_name: if display_name.trim().is_empty() {
                "Local user".to_owned()
            } else {
                display_name
            },
            provider_user_id: "local-user".to_owned(),
            provider_id: provider_id.clone(),
            email: None,
        };

        let outcome = push_with_freshness_check(
            provider.as_ref(),
            &provider_id,
            &[],
            &sidecar_path_for(&project_path),
            snapshot.as_bytes(),
            author,
            "Backed up changes",
            "",
        )
        .await?;

        Ok(match outcome {
            PushOutcome::Committed { .. } => {
                BackupResult::Committed(load_history(provider.as_ref()).await?)
            }
            PushOutcome::NeedsResolution { conflicts, .. } => {
                BackupResult::NeedsResolution(conflicts.len())
            }
            PushOutcome::NeedsReview { problems, .. } => BackupResult::NeedsReview(problems.len()),
        })
    })
}

/// Read history for a project from the provider recorded in its sidecar.
pub fn history(
    listing: ProviderListing,
    project_path: PathBuf,
) -> Result<Vec<CommitSummary>, String> {
    runtime()?.block_on(async move {
        let provider = open_provider(&listing, &project_path).await?;
        load_history(provider.as_ref()).await
    })
}

/// Restore a commit into a new sibling folder under `destination`.
///
/// Never writes over the working project. A restore is something a person
/// should be able to inspect and open before deciding whether it replaces
/// current work.
pub fn restore(
    listing: ProviderListing,
    working_project: PathBuf,
    commit_id: CommitId,
    destination: PathBuf,
) -> Result<RestoreResult, String> {
    runtime()?.block_on(async move {
        let provider = open_provider(&listing, &working_project).await?;
        let commit = provider
            .get_commit(&commit_id)
            .await
            .map_err(|error| format!("fetch version: {error}"))?;
        let snapshot_bytes = provider
            .get_blob(&commit.tree.snapshot)
            .await
            .map_err(|error| format!("fetch project: {error}"))?;
        let snapshot = ProjectSnapshot::from_canonical_bytes(&snapshot_bytes)
            .map_err(|error| format!("read stored project: {error}"))?;

        let restore_root = unique_restore_root(&destination, &working_project, commit_id);
        let file_name = working_project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Restored Project")
            .to_owned();

        match snapshot.format() {
            ProjectFormat::AbletonLiveSet => {
                let report = auru_pm::ableton::restore_bundle(
                    provider.as_ref(),
                    &commit,
                    &restore_root,
                    &file_name,
                )
                .await
                .map_err(|error| format!("restore Ableton project: {error}"))?;
                Ok(RestoreResult {
                    project_file: report.live_set,
                    files_written: report.files_written,
                    unavailable: report.unavailable.len(),
                })
            }
            ProjectFormat::FlStudio => {
                restore_fl(
                    provider.as_ref(),
                    &commit,
                    &snapshot,
                    &restore_root,
                    &file_name,
                )
                .await
            }
            ProjectFormat::Dawproject | ProjectFormat::Auru => {
                std::fs::create_dir_all(&restore_root)
                    .map_err(|error| format!("create restore folder: {error}"))?;
                let project_file = restore_root.join(file_name);
                snapshot
                    .restore_to_path(&project_file)
                    .map_err(|error| format!("restore project: {error}"))?;
                Ok(RestoreResult {
                    project_file,
                    files_written: 0,
                    unavailable: 0,
                })
            }
        }
    })
}

/// Ask the operating system to open a project in its registered DAW.
pub fn open_project(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = std::process::Command::new("open");
    #[cfg(target_os = "linux")]
    let mut command = std::process::Command::new("xdg-open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = std::process::Command::new("cmd");
        command.args(["/C", "start", ""]);
        command
    };
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    return Err("opening projects is not supported on this platform".to_owned());

    command
        .arg(path)
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("open {}: {error}", path.display()))
}

pub fn primary_listing(
    providers: &[ProviderListing],
    project_path: &Path,
) -> Option<ProviderListing> {
    let primary = Sidecar::load(&sidecar_path_for(project_path))
        .ok()
        .and_then(|sidecar| sidecar.primary)?;
    providers
        .iter()
        .find(|provider| provider.entry.id == primary)
        .cloned()
}

pub fn default_listing(providers: &[ProviderListing]) -> Option<ProviderListing> {
    providers
        .iter()
        .find(|provider| provider.is_connected())
        .cloned()
}

async fn load_history(provider: &dyn ProjectProvider) -> Result<Vec<CommitSummary>, String> {
    provider
        .list_history(HistoryRange {
            limit: Some(50),
            before: None,
        })
        .await
        .map_err(|error| format!("load history: {error}"))
}

async fn open_provider(
    listing: &ProviderListing,
    project_path: &Path,
) -> Result<Arc<dyn ProjectProvider>, String> {
    let handle = persisted_project_handle(listing, project_path)?;
    if let Some(root) = listing.local_path() {
        let root = root.join(".auru-pm").join("projects").join(&handle);
        let provider = FilesystemProvider::open(root)
            .map_err(|error| format!("open local backup destination: {error}"))?;
        return Ok(Arc::new(provider));
    }

    let token = auru_pm::token_store::load_token(&listing.entry.id, &handle)
        .ok()
        .flatten()
        .or_else(|| {
            auru_pm::token_store::load_provider_token(&listing.entry.id)
                .ok()
                .flatten()
        });
    let provider = HttpProvider::open(&listing.entry.endpoint, &handle, token)
        .await
        .map_err(|error| format!("connect to {}: {error}", listing.entry.name))?;
    Ok(Arc::new(provider))
}

fn persisted_project_handle(
    listing: &ProviderListing,
    project_path: &Path,
) -> Result<String, String> {
    let sidecar_path = sidecar_path_for(project_path);
    let sidecar = Sidecar::load(&sidecar_path)
        .map_err(|error| format!("read project provider settings: {error}"))?;
    if let Some(handle) = sidecar.provider_handles.get(&listing.entry.id) {
        return Ok(handle.clone());
    }

    let handle = project_handle(project_path);
    Sidecar::modify(&sidecar_path, |sidecar| {
        sidecar
            .provider_handles
            .insert(listing.entry.id.clone(), handle.clone());
    })
    .map_err(|error| format!("save project provider handle: {error}"))?;
    Ok(handle)
}

async fn restore_fl(
    provider: &dyn ProjectProvider,
    commit: &auru_pm::Commit,
    snapshot: &ProjectSnapshot,
    restore_root: &Path,
    file_name: &str,
) -> Result<RestoreResult, String> {
    let manifest_bytes = provider
        .get_blob(&commit.tree.samples)
        .await
        .map_err(|error| format!("fetch sample list: {error}"))?;
    let manifest: SampleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("read sample list: {error}"))?;
    let mut stream = flstudio::Stream::decode(
        &snapshot
            .restore_bytes()
            .map_err(|error| format!("rebuild FL project: {error}"))?,
    )
    .map_err(|error| format!("read rebuilt FL project: {error}"))?;

    std::fs::create_dir_all(restore_root)
        .map_err(|error| format!("create restore folder: {error}"))?;
    let mut captured = BTreeMap::new();
    let mut files_written = 0;
    for entry in &manifest.entries {
        let bytes = match provider.get_blob(&entry.hash).await {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        flstudio::restore::write_asset(restore_root, &entry.path, &bytes)
            .map_err(|error| format!("restore {}: {error}", entry.path))?;
        if let Some(origin) = &entry.origin {
            captured.insert(origin.clone(), entry.path.clone());
        }
        files_written += 1;
    }
    let repoint = flstudio::restore::repoint(&mut stream, &captured);
    // `still_missing` is sorted and de-duplicated by recorded sample path. It
    // already includes captured entries whose blob could not be fetched, so
    // counting fetch errors separately would report the same file twice.
    let unavailable = repoint.still_missing.len();

    let project_file = restore_root.join(file_name);
    std::fs::write(&project_file, stream.encode())
        .map_err(|error| format!("write restored FL project: {error}"))?;
    Ok(RestoreResult {
        project_file,
        files_written,
        unavailable,
    })
}

fn unique_restore_root(destination: &Path, project_path: &Path, commit_id: CommitId) -> PathBuf {
    let stem = project_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Project");
    let commit = commit_id.0.to_string();
    let name = format!("{stem} Restored {}", &commit[..8]);
    let first = destination.join(&name);
    if !first.exists() {
        return first;
    }

    (2..)
        .map(|copy| destination.join(format!("{name} ({copy})")))
        .find(|candidate| !candidate.exists())
        .expect("the restore copy number cannot be exhausted")
}

fn project_handle(project_path: &Path) -> String {
    let stem = project_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("project");
    let slug: String = stem
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let hash = auru_pm::ContentHash::of(project_path.to_string_lossy().as_bytes()).to_string();
    format!("{}-{}", slug.trim_matches('-'), &hash[..12])
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("background runtime: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use auru_pm::{AuthMethod, Capabilities, RegistryAvailability, RegistryEntry};

    fn local_listing(path: &Path) -> ProviderListing {
        crate::catalog::local_provider(path)
    }

    #[test]
    fn a_local_backup_should_create_history_and_record_its_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        std::fs::write(&project, br#"{"version":8,"channels":[]}"#).expect("project");
        let destination = temp.path().join("Backups");
        let listing = local_listing(&destination);

        let BackupResult::Committed(history) =
            back_up(listing.clone(), project.clone(), "Jake".to_owned()).expect("backup")
        else {
            panic!("first backup should commit");
        };

        assert_eq!(history.len(), 1);
        assert_eq!(
            Sidecar::load(&sidecar_path_for(&project))
                .expect("sidecar")
                .primary
                .as_deref(),
            Some(listing.entry.id.as_str())
        );
        assert!(
            destination.join(".auru-pm/projects").is_dir(),
            "real provider storage should exist"
        );
    }

    #[test]
    fn a_native_version_should_restore_without_overwriting_the_working_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        let original = br#"{"version":8,"channels":[]}"#;
        std::fs::write(&project, original).expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let BackupResult::Committed(history) =
            back_up(listing.clone(), project.clone(), "Jake".to_owned()).expect("backup")
        else {
            panic!("first backup should commit");
        };

        std::fs::write(
            &project,
            br#"{"version":8,"channels":[{"name":"changed"}]}"#,
        )
        .expect("change working project");
        let restored = restore(
            listing,
            project.clone(),
            history[0].id,
            temp.path().join("Restores"),
        )
        .expect("restore");

        let restored_json: serde_json::Value =
            serde_json::from_slice(&std::fs::read(restored.project_file).expect("restored bytes"))
                .expect("restored JSON");
        let original_json: serde_json::Value =
            serde_json::from_slice(original).expect("original JSON");
        assert_eq!(restored_json, original_json);
        assert_ne!(
            std::fs::read(project).expect("working bytes"),
            original,
            "restore must leave current work alone"
        );
    }

    #[test]
    fn an_fl_version_should_restore_its_captured_samples() {
        use auru_pm::flstudio::events::{EVENT_VERSION, Event, Header, Stream};
        use auru_pm::flstudio::refs::EVENT_SAMPLE_PATH;

        let temp = tempfile::tempdir().expect("tempdir");
        let sample = temp.path().join("Kick.wav");
        std::fs::write(&sample, b"kick audio").expect("sample");
        let mut sample_path = Vec::new();
        for unit in sample.to_string_lossy().encode_utf16() {
            sample_path.extend_from_slice(&unit.to_le_bytes());
        }
        sample_path.extend_from_slice(&[0, 0]);
        let source = Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events: vec![
                Event::new(EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
                Event::new(EVENT_SAMPLE_PATH, sample_path),
            ],
        }
        .encode();
        let project = temp.path().join("Beat.flp");
        std::fs::write(&project, source).expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let BackupResult::Committed(history) =
            back_up(listing.clone(), project.clone(), "Jake".to_owned()).expect("backup")
        else {
            panic!("first backup should commit");
        };

        let restored = restore(
            listing.clone(),
            project.clone(),
            history[0].id,
            temp.path().join("Restores"),
        )
        .expect("restore");
        let root = restored.project_file.parent().expect("restore root");
        assert_eq!(
            std::fs::read(root.join("Samples/Kick.wav")).expect("restored sample"),
            b"kick audio"
        );
        assert_eq!(restored.files_written, 1);
        assert_eq!(restored.unavailable, 0);

        let sidecar = Sidecar::load(&sidecar_path_for(&project)).expect("sidecar");
        let handle = sidecar
            .provider_handles
            .get(&listing.entry.id)
            .expect("provider handle");
        let stored =
            FilesystemProvider::open(temp.path().join("Backups/.auru-pm/projects").join(handle))
                .expect("stored project");
        let manifest = runtime()
            .expect("runtime")
            .block_on(async {
                let commit = stored.get_commit(&history[0].id).await?;
                let bytes = stored.get_blob(&commit.tree.samples).await?;
                serde_json::from_slice::<SampleManifest>(&bytes).map_err(auru_pm::Error::from)
            })
            .expect("sample manifest");
        std::fs::remove_file(stored.blobs_cas().path_for(&manifest.entries[0].hash))
            .expect("remove captured sample blob");

        let missing = restore(
            listing,
            project,
            history[0].id,
            temp.path().join("Restores"),
        )
        .expect("restore with missing sample");
        assert_eq!(missing.files_written, 0);
        assert_eq!(
            missing.unavailable, 1,
            "one missing blob must not be counted again by repointing"
        );
    }

    #[test]
    fn project_handles_should_be_safe_and_distinguish_equal_names() {
        let one = project_handle(Path::new("/music/one/My Song.flp"));
        let two = project_handle(Path::new("/music/two/My Song.flp"));
        assert_ne!(one, two);
        assert!(one.starts_with("my-song-"));
        assert!(!one.contains('/'));
    }

    #[test]
    fn a_persisted_project_handle_should_survive_a_local_move() {
        let temp = tempfile::tempdir().expect("tempdir");
        let listing = local_listing(&temp.path().join("Backups"));
        let original = temp.path().join("First Name.auru");
        std::fs::write(&original, b"{}").expect("project");
        let handle = persisted_project_handle(&listing, &original).expect("persist project handle");

        let moved = temp.path().join("Renamed.auru");
        std::fs::rename(&original, &moved).expect("move project");
        std::fs::rename(sidecar_path_for(&original), sidecar_path_for(&moved))
            .expect("move sidecar");

        assert_eq!(
            persisted_project_handle(&listing, &moved).expect("read project handle"),
            handle
        );
    }

    #[test]
    fn restoring_the_same_version_again_should_choose_a_fresh_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = Path::new("/music/My Song.flp");
        let commit = CommitId(auru_pm::ContentHash::of(b"same version"));
        let first = unique_restore_root(temp.path(), project, commit);
        std::fs::create_dir(&first).expect("first restore folder");

        let second = unique_restore_root(temp.path(), project, commit);

        assert_ne!(second, first);
        let folder = second
            .file_name()
            .and_then(|name| name.to_str())
            .expect("restore folder name");
        assert!(folder.starts_with("My Song Restored "));
        assert!(folder.ends_with(" (2)"));
    }

    #[test]
    fn a_remote_listing_should_not_be_treated_as_a_local_folder() {
        let listing = ProviderListing::from_registry(RegistryEntry {
            id: "remote".to_owned(),
            name: "Remote".to_owned(),
            endpoint: "https://example.invalid".to_owned(),
            capabilities: Capabilities::default(),
            auth_methods: vec![AuthMethod::None],
            icon_url: None,
            description: String::new(),
            detail: String::new(),
            recommended: false,
            availability: RegistryAvailability::Available,
        });
        assert!(listing.local_path().is_none());
    }
}
