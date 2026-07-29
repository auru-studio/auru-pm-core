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
    AuthorIdentity, CommitId, CommitSummary, ContentHash, FilesystemProvider, HistoryRange,
    HttpProvider, ProjectFormat, ProjectProfile, ProjectProvider, ProjectSnapshot, PushOutcome,
    RetentionReport, RetentionRoots, RetentionRule, SampleManifest, Sidecar, fetch_project_info,
    flstudio, push_with_freshness_check, sidecar_path_for,
};
use auru_pm_client::ProviderAccount;

use crate::catalog::ProviderListing;
use crate::model::RemoteProjectSeed;

#[derive(Debug)]
pub enum BackupResult {
    Committed(BackupReceipt),
    NeedsResolution(usize),
    NeedsReview(usize),
}

#[derive(Debug)]
pub struct BackupReceipt {
    pub history: Vec<CommitSummary>,
    pub retention: Option<RetentionReport>,
    pub retention_warning: Option<String>,
}

#[derive(Debug)]
pub struct RestoreResult {
    pub project_file: PathBuf,
    pub files_written: usize,
    pub unavailable: usize,
}

#[derive(Clone, Debug)]
pub struct RemoteCatalogue {
    pub projects: Vec<RemoteProjectSeed>,
    pub unavailable: usize,
}

/// Back up one project and return its refreshed history.
pub fn back_up(
    listing: ProviderListing,
    project_path: PathBuf,
    display_name: String,
    retention_rule: Option<RetentionRule>,
) -> Result<BackupResult, String> {
    runtime()?.block_on(async move {
        let provider = open_provider(&listing, &project_path).await?;
        let snapshot = ProjectSnapshot::load(&project_path)
            .map_err(|error| format!("read {}: {error}", project_path.display()))?;
        if provider.capabilities().project_listing {
            provider
                .put_project_profile(&ProjectProfile {
                    display_name: project_display_name(&project_path),
                    format: snapshot.format(),
                })
                .await
                .map_err(|error| format!("register project with provider: {error}"))?;
        }
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

        let sidecar_path = sidecar_path_for(&project_path);
        let outcome = push_with_freshness_check(
            provider.as_ref(),
            &provider_id,
            &[],
            &sidecar_path,
            snapshot.as_bytes(),
            author,
            "Backed up changes",
            "",
        )
        .await?;

        Ok(match outcome {
            PushOutcome::Committed { .. } => {
                let (retention, retention_warning) = match retention_rule {
                    None => (None, None),
                    Some(_) if !provider.capabilities().history_retention => (
                        None,
                        Some(format!(
                            "{} does not support version retention",
                            listing.entry.name
                        )),
                    ),
                    Some(rule) => match Sidecar::load(&sidecar_path) {
                        Err(error) => (
                            None,
                            Some(format!(
                                "{} could not read pending sync work before pruning: {error}",
                                listing.entry.name
                            )),
                        ),
                        Ok(sidecar) => {
                            let protected = RetentionRoots {
                                commits: sidecar.pending_pushes,
                                blobs: sidecar
                                    .stash
                                    .map(|stash| vec![stash.snapshot])
                                    .unwrap_or_default(),
                            };
                            match provider.prune_history(rule, &protected).await {
                                Ok(report) => (Some(report), None),
                                Err(error) => (
                                    None,
                                    Some(format!(
                                        "{} could not prune old versions: {error}",
                                        listing.entry.name
                                    )),
                                ),
                            }
                        }
                    },
                };
                BackupResult::Committed(BackupReceipt {
                    history: load_history(provider.as_ref()).await?,
                    retention,
                    retention_warning,
                })
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

/// List and enrich every project visible to one connected provider account.
pub fn remote_catalogue(listing: ProviderListing) -> Result<RemoteCatalogue, String> {
    runtime()?.block_on(async move {
        let account = provider_account(&listing).await?;
        if !account.supports_project_listing() {
            return Ok(RemoteCatalogue {
                projects: Vec::new(),
                unavailable: 0,
            });
        }
        let records = account
            .list_projects()
            .await
            .map_err(|error| format!("list projects from {}: {error}", listing.entry.name))?;
        let mut projects = Vec::new();
        let mut unavailable = 0;
        for record in records {
            let provider = match account.open_project(&record.handle).await {
                Ok(provider) => provider,
                Err(_) => {
                    unavailable += 1;
                    continue;
                }
            };
            let commit = match provider.get_commit(&record.head).await {
                Ok(commit) => commit,
                Err(_) => {
                    unavailable += 1;
                    continue;
                }
            };
            let info = fetch_project_info(provider.as_ref(), &commit)
                .await
                .unwrap_or(None);
            let profile = match record.profile {
                Some(profile) => profile,
                None => {
                    let snapshot = match provider.get_blob(&commit.tree.snapshot).await {
                        Ok(bytes) => ProjectSnapshot::from_canonical_bytes(&bytes),
                        Err(error) => Err(error),
                    };
                    let Ok(snapshot) = snapshot else {
                        unavailable += 1;
                        continue;
                    };
                    ProjectProfile {
                        display_name: record.handle.clone(),
                        format: snapshot.format(),
                    }
                }
            };
            projects.push(RemoteProjectSeed {
                provider_id: listing.entry.id.clone(),
                provider_name: listing.entry.name.clone(),
                handle: record.handle,
                head: record.head,
                file_name: safe_project_file_name(&profile.display_name, profile.format),
                name: profile.display_name,
                format: profile.format,
                updated_at: record.updated_at,
                info,
            });
        }
        projects.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
        });
        Ok(RemoteCatalogue {
            projects,
            unavailable,
        })
    })
}

pub fn remote_history(
    listing: ProviderListing,
    handle: String,
) -> Result<Vec<CommitSummary>, String> {
    runtime()?.block_on(async move {
        let provider = provider_account(&listing)
            .await?
            .open_project(&handle)
            .await
            .map_err(|error| format!("open remote project: {error}"))?;
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
        let file_name = working_project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Restored Project")
            .to_owned();
        restore_from_provider(provider.as_ref(), commit_id, &destination, &file_name).await
    })
}

/// Recover a provider-only project and enroll the restored copy locally.
pub fn restore_remote(
    listing: ProviderListing,
    handle: String,
    file_name: String,
    commit_id: CommitId,
    destination: PathBuf,
) -> Result<RestoreResult, String> {
    runtime()?.block_on(async move {
        let provider = provider_account(&listing)
            .await?
            .open_project(&handle)
            .await
            .map_err(|error| format!("open remote project: {error}"))?;
        let result =
            restore_from_provider(provider.as_ref(), commit_id, &destination, &file_name).await?;
        let result = require_complete_recovery(result)?;
        let sidecar_path = sidecar_path_for(&result.project_file);
        Sidecar {
            primary: Some(listing.entry.id.clone()),
            provider_handles: BTreeMap::from([(listing.entry.id, handle)]),
            local_head: Some(commit_id),
            ..Sidecar::default()
        }
        .save(&sidecar_path)
        .map_err(|error| format!("enroll restored project: {error}"))?;
        Ok(result)
    })
}

fn require_complete_recovery(result: RestoreResult) -> Result<RestoreResult, String> {
    if result.unavailable == 0 {
        return Ok(result);
    }
    Err(format!(
        "Restored a partial copy to {}, but {} referenced file(s) could not be restored. \
         The copy was not enrolled as synchronized.",
        result.project_file.display(),
        result.unavailable
    ))
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

async fn provider_account(listing: &ProviderListing) -> Result<ProviderAccount, String> {
    if let Some(root) = listing.local_path() {
        return Ok(ProviderAccount::filesystem(
            root.join(".auru-pm").join("projects"),
        ));
    }
    let token = auru_pm::token_store::load_provider_token(&listing.entry.id)
        .ok()
        .flatten();
    ProviderAccount::connect_http(&listing.entry.endpoint, token)
        .await
        .map_err(|error| format!("connect to {}: {error}", listing.entry.name))
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

async fn restore_from_provider(
    provider: &dyn ProjectProvider,
    commit_id: CommitId,
    destination: &Path,
    requested_file_name: &str,
) -> Result<RestoreResult, String> {
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
    let requested_stem = Path::new(requested_file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Restored Project");
    let file_name = safe_project_file_name(requested_stem, snapshot.format());
    let restore_root = unique_restore_root(destination, Path::new(&file_name), commit_id);

    match snapshot.format() {
        ProjectFormat::AbletonLiveSet => {
            let report =
                auru_pm::ableton::restore_bundle(provider, &commit, &restore_root, &file_name)
                    .await
                    .map_err(|error| format!("restore Ableton project: {error}"))?;
            Ok(RestoreResult {
                project_file: report.live_set,
                files_written: report.files_written,
                unavailable: report.unavailable.len(),
            })
        }
        ProjectFormat::FlStudio => {
            restore_fl(provider, &commit, &snapshot, &restore_root, &file_name).await
        }
        ProjectFormat::Auru => {
            restore_auru(provider, &commit, &snapshot, &restore_root, &file_name).await
        }
        ProjectFormat::Dawproject => {
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
}

async fn restore_auru(
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
    let mut project: serde_json::Value = serde_json::from_slice(snapshot.as_bytes())
        .map_err(|error| format!("read native project: {error}"))?;
    let referenced = auru_pm::sample_manifest::sample_paths_in_snapshot(&project);
    let mut restored_paths = BTreeMap::new();

    std::fs::create_dir_all(restore_root)
        .map_err(|error| format!("create restore folder: {error}"))?;
    for entry in &manifest.entries {
        if !referenced.contains(&entry.path) {
            continue;
        }
        let Ok(bytes) = provider.get_blob(&entry.hash).await else {
            continue;
        };
        let relative = format!(
            "Samples/{}",
            safe_native_asset_file_name(&entry.path, entry.hash)
        );
        let target = restore_root.join(&relative);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("create sample folder: {error}"))?;
        }
        std::fs::write(&target, bytes)
            .map_err(|error| format!("restore sample {}: {error}", entry.path))?;
        restored_paths.insert(entry.path.clone(), relative);
    }

    rewrite_native_sample_paths(&mut project, &restored_paths);
    let encoded =
        serde_json::to_vec(&project).map_err(|error| format!("encode native project: {error}"))?;
    let restored_snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Auru, &encoded)
        .map_err(|error| format!("rebuild native project: {error}"))?;
    let project_file = restore_root.join(file_name);
    restored_snapshot
        .restore_to_path(&project_file)
        .map_err(|error| format!("restore project: {error}"))?;

    Ok(RestoreResult {
        project_file,
        files_written: restored_paths.len(),
        unavailable: referenced.len().saturating_sub(restored_paths.len()),
    })
}

fn rewrite_native_sample_paths(
    project: &mut serde_json::Value,
    restored_paths: &BTreeMap<String, String>,
) {
    let Some(channels) = project
        .get_mut("channels")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };
    for clip in channels
        .iter_mut()
        .filter_map(|channel| channel.get_mut("clips"))
        .filter_map(serde_json::Value::as_array_mut)
        .flatten()
    {
        let Some(serde_json::Value::String(path)) = clip.pointer_mut("/data/Audio/file_path")
        else {
            continue;
        };
        if let Some(restored) = restored_paths.get(path) {
            *path = restored.clone();
        }
    }
}

fn safe_native_asset_file_name(recorded_path: &str, hash: ContentHash) -> String {
    let raw_name = recorded_path
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("sample");
    let path = Path::new(raw_name);
    let stem = safe_file_component(
        path.file_stem()
            .and_then(|part| part.to_str())
            .unwrap_or("sample"),
        "sample",
        120,
    );
    let extension = path
        .extension()
        .and_then(|part| part.to_str())
        .map(|extension| safe_file_component(extension, "", 24))
        .filter(|extension| !extension.is_empty());
    let hash = hash.to_string();
    let hash = hash.strip_prefix("blake3:").unwrap_or(&hash);
    match extension {
        Some(extension) => format!("{hash}-{stem}.{extension}"),
        None => format!("{hash}-{stem}"),
    }
}

fn safe_file_component(value: &str, fallback: &str, max_bytes: usize) -> String {
    let mut safe = String::new();
    for character in value.chars() {
        let character = match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        };
        if safe.len() + character.len_utf8() > max_bytes {
            break;
        }
        safe.push(character);
    }
    let safe = safe.trim().trim_matches('.').trim();
    if safe.is_empty() {
        fallback.to_owned()
    } else {
        safe.to_owned()
    }
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
    let commit = commit.strip_prefix("blake3:").unwrap_or(&commit);
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
    let hash = hash.strip_prefix("blake3:").unwrap_or(&hash);
    format!("{}-{}", slug.trim_matches('-'), &hash[..12])
}

fn project_display_name(project_path: &Path) -> String {
    project_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or("Untitled Project")
        .to_owned()
}

fn safe_project_file_name(display_name: &str, format: ProjectFormat) -> String {
    let mut stem = safe_file_component(display_name, "Restored Project", 160);
    if is_windows_reserved_name(&stem) {
        stem.insert(0, '_');
    }
    format!("{stem}.{}", format.extension())
}

fn is_windows_reserved_name(value: &str) -> bool {
    let stem = value
        .split('.')
        .next()
        .unwrap_or(value)
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
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

        let BackupResult::Committed(BackupReceipt { history, .. }) =
            back_up(listing.clone(), project.clone(), "Jake".to_owned(), None).expect("backup")
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
        let catalogue = remote_catalogue(listing).expect("provider catalogue");
        assert_eq!(catalogue.unavailable, 0);
        assert_eq!(catalogue.projects.len(), 1);
        assert_eq!(catalogue.projects[0].name, "Song");
        assert_eq!(catalogue.projects[0].format, ProjectFormat::Auru);
        assert_eq!(catalogue.projects[0].head, history[0].id);
    }

    #[test]
    fn a_successful_backup_should_apply_the_selected_retention_rule() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        let listing = local_listing(&temp.path().join("Backups"));
        let rule = Some(RetentionRule::Latest { count: 2 });

        for version in 1..=3 {
            std::fs::write(
                &project,
                serde_json::to_vec(&serde_json::json!({
                    "version": 8,
                    "channels": [{ "name": format!("version {version}") }]
                }))
                .expect("project json"),
            )
            .expect("project");
            let BackupResult::Committed(receipt) =
                back_up(listing.clone(), project.clone(), "Jake".to_owned(), rule).expect("backup")
            else {
                panic!("each revision should commit");
            };

            if version == 3 {
                assert_eq!(receipt.history.len(), 2);
                assert_eq!(
                    receipt
                        .retention
                        .expect("local providers support retention")
                        .versions_removed,
                    1
                );
                assert!(receipt.retention_warning.is_none());
            }
        }
    }

    #[test]
    fn recovering_a_remote_project_should_enroll_the_restored_copy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        let sample = temp.path().join("source snare.wav");
        std::fs::write(&sample, b"snare audio").expect("sample");
        std::fs::write(
            &project,
            serde_json::to_vec(&serde_json::json!({
                "version": 8,
                "channels": [{ "clips": [{
                    "data": { "Audio": { "file_path": sample.to_str().unwrap() } }
                }]}]
            }))
            .expect("project json"),
        )
        .expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let BackupResult::Committed(BackupReceipt { history, .. }) =
            back_up(listing.clone(), project, "Jake".to_owned(), None).expect("backup")
        else {
            panic!("first backup should commit");
        };
        let remote = remote_catalogue(listing.clone())
            .expect("catalogue")
            .projects
            .remove(0);
        std::fs::remove_file(&sample).expect("recovery must use the provider copy");

        let restored = restore_remote(
            listing.clone(),
            remote.handle.clone(),
            remote.file_name,
            history[0].id,
            temp.path().join("Recovered"),
        )
        .expect("recover");
        assert_eq!(restored.files_written, 1);
        assert_eq!(restored.unavailable, 0);
        let sidecar =
            Sidecar::load(&sidecar_path_for(&restored.project_file)).expect("restored sidecar");
        let restored_project: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&restored.project_file).expect("restored project"),
        )
        .expect("restored project json");
        let restored_sample = restored_project
            .pointer("/channels/0/clips/0/data/Audio/file_path")
            .and_then(serde_json::Value::as_str)
            .expect("rewritten sample path");
        assert!(restored_sample.starts_with("Samples/"));
        assert_eq!(
            std::fs::read(
                restored
                    .project_file
                    .parent()
                    .expect("restore folder")
                    .join(restored_sample)
            )
            .expect("materialized sample"),
            b"snare audio"
        );

        assert_eq!(sidecar.primary.as_deref(), Some(listing.entry.id.as_str()));
        assert_eq!(sidecar.local_head, Some(history[0].id));
        assert_eq!(
            sidecar.provider_handles.get(&listing.entry.id),
            Some(&remote.handle)
        );

        let BackupResult::Committed(BackupReceipt {
            history: recovered_history,
            ..
        }) = back_up(
            listing.clone(),
            restored.project_file.clone(),
            "Jake".to_owned(),
            None,
        )
        .expect("back up recovered project")
        else {
            panic!("rewritten native sample paths should remain backup-able");
        };
        let stored = FilesystemProvider::open(
            temp.path()
                .join("Backups/.auru-pm/projects")
                .join(&remote.handle),
        )
        .expect("stored project");
        let recovered_manifest = runtime()
            .expect("runtime")
            .block_on(async {
                let commit = stored.get_commit(&recovered_history[0].id).await?;
                let bytes = stored.get_blob(&commit.tree.samples).await?;
                serde_json::from_slice::<SampleManifest>(&bytes).map_err(auru_pm::Error::from)
            })
            .expect("recovered sample manifest");
        assert_eq!(recovered_manifest.entries.len(), 1);
        assert!(recovered_manifest.entries[0].path.starts_with("Samples/"));
        assert_eq!(
            runtime()
                .expect("runtime")
                .block_on(stored.get_blob(&recovered_manifest.entries[0].hash))
                .expect("re-backed-up sample"),
            b"snare audio"
        );
    }

    #[test]
    fn a_partial_remote_restore_should_not_be_treated_as_synchronized() {
        let result = RestoreResult {
            project_file: PathBuf::from("/recover/Night Drive.als"),
            files_written: 3,
            unavailable: 2,
        };

        let error = require_complete_recovery(result).expect_err("partial restore");

        assert!(error.contains("was not enrolled as synchronized"));
        assert!(error.contains("2 referenced file(s) could not be restored"));
    }

    #[test]
    fn a_native_recovery_with_an_uncaptured_sample_should_not_enroll() {
        let temp = tempfile::tempdir().expect("tempdir");
        let missing_sample = temp.path().join("already missing.wav");
        let project = temp.path().join("Song.auru");
        std::fs::write(
            &project,
            serde_json::to_vec(&serde_json::json!({
                "channels": [{ "clips": [{
                    "data": { "Audio": { "file_path": missing_sample.to_str().unwrap() } }
                }]}]
            }))
            .expect("project json"),
        )
        .expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let BackupResult::Committed(BackupReceipt { history, .. }) =
            back_up(listing.clone(), project, "Jake".to_owned(), None).expect("backup")
        else {
            panic!("backup should commit even when a referenced sample is already missing");
        };
        let remote = remote_catalogue(listing.clone())
            .expect("catalogue")
            .projects
            .remove(0);

        let error = restore_remote(
            listing,
            remote.handle,
            remote.file_name,
            history[0].id,
            temp.path().join("Recovered"),
        )
        .expect_err("an incomplete native recovery must not enroll");

        assert!(error.contains("1 referenced file(s) could not be restored"));
        assert!(error.contains("was not enrolled as synchronized"));
    }

    #[test]
    fn a_native_version_should_restore_without_overwriting_the_working_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        let original = br#"{"version":8,"channels":[]}"#;
        std::fs::write(&project, original).expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let BackupResult::Committed(BackupReceipt { history, .. }) =
            back_up(listing.clone(), project.clone(), "Jake".to_owned(), None).expect("backup")
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
        let BackupResult::Committed(BackupReceipt { history, .. }) =
            back_up(listing.clone(), project.clone(), "Jake".to_owned(), None).expect("backup")
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
        assert!(!one.contains(':'));
    }

    #[test]
    fn restored_native_asset_names_should_be_safe_path_components() {
        let name = safe_native_asset_file_name(
            r#"C:\Users\Jake\..\CON:?<>|.wav"#,
            ContentHash::of(b"sample"),
        );

        assert!(!name.contains(['/', '\\', ':', '?', '<', '>', '|']));
        assert!(name.ends_with(".wav"));
    }

    #[test]
    fn restored_project_names_should_avoid_windows_devices() {
        assert_eq!(
            safe_project_file_name("CON", ProjectFormat::Auru),
            "_CON.auru"
        );
        assert_eq!(
            safe_project_file_name("lpt9.session", ProjectFormat::AbletonLiveSet),
            "_lpt9.session.als"
        );
        assert_eq!(
            safe_project_file_name("LPT10", ProjectFormat::FlStudio),
            "LPT10.flp"
        );
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
