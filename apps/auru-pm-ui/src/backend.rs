//! The UI-to-core bridge.
//!
//! GPUI owns presentation and interaction; this module owns the blocking and
//! asynchronous work needed to turn those interactions into real project
//! history. Keeping it out of `main.rs` also gives the backup/restore path a
//! testable seam that does not need a window.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use auru_pm::{
    AuthorIdentity, BundlePolicy, CommitId, CommitSummary, ConflictChoice, ConflictResolution,
    ConflictedField, ContentHash, FilesystemProvider, HistoryRange, HttpAccount, ProjectFormat,
    ProjectLocation, ProjectMetadata, ProjectProfile, ProjectProvider, ProjectSnapshot,
    PushOptions, PushOutcome, RetentionReport, RetentionRoots, RetentionRule, SampleManifest,
    Sidecar, fetch_project_info, flstudio, push_with_options, sidecar_path_for, verify_commit_copy,
};
use auru_pm_client::ProviderAccount;

use crate::catalog::ProviderListing;
use crate::model::RemoteProjectSeed;

#[derive(Debug)]
pub enum BackupResult {
    Committed(BackupReceipt),
    NeedsResolution(Box<ConflictBackup>),
    NeedsReview(usize),
}

#[derive(Debug)]
pub struct BackupReceipt {
    pub history: Vec<CommitSummary>,
    pub retention: Option<RetentionReport>,
    pub retention_warning: Option<String>,
    pub verification: BackupVerification,
}

/// An immutable project snapshot ready to hand to the backup coordinator.
///
/// Preparation is deliberately separate from publication so the UI can
/// durably record the exact file revision before the coordinator commits it.
#[derive(Clone, Debug)]
pub struct PreparedBackup {
    project_path: PathBuf,
    snapshot: ProjectSnapshot,
    metadata: ProjectMetadata,
    location: Option<ProjectLocation>,
    source_revision: Option<SystemTime>,
}

#[derive(Clone, Debug)]
pub struct ConflictBackup {
    conflicts: Vec<ConflictedField>,
    listing: ProviderListing,
    prepared: PreparedBackup,
    display_name: String,
    retention_rule: Option<RetentionRule>,
    verify_uploads: bool,
    bundle_policy: BundlePolicy,
}

impl ConflictBackup {
    pub fn conflicts(&self) -> &[ConflictedField] {
        &self.conflicts
    }
}

impl PreparedBackup {
    pub const fn source_revision(&self) -> Option<SystemTime> {
        self.source_revision
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BackupVerification {
    Skipped,
    Verified,
    Incomplete(Vec<String>),
    Failed(String),
}

#[derive(Debug)]
pub struct RestoreResult {
    pub project_file: PathBuf,
    pub files_written: usize,
    pub unavailable: usize,
    pub verified_files: usize,
    /// A previous destination retained because cleanup could not remove it.
    /// The restore itself is valid; this path remains as an extra safety copy.
    pub previous_copy: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RestoreCollisionChoice {
    /// Fail if another process creates the destination after preflight.
    AbortIfExists,
    /// Keep the existing project and choose a numbered restore folder.
    Duplicate,
    /// Replace matching restored files while preserving unrelated files.
    Overwrite,
    /// Replace the whole existing restore folder after verification.
    DeleteAndReplace,
}

#[derive(Clone, Debug)]
pub struct RemoteCatalogue {
    pub projects: Vec<RemoteProjectSeed>,
    pub unavailable: usize,
}

/// Read one stable project revision without publishing it.
pub fn prepare_backup(
    project_path: PathBuf,
    location: Option<ProjectLocation>,
) -> Result<PreparedBackup, String> {
    let (snapshot, source_revision) = load_stable_snapshot(&project_path)?;
    let sidecar = Sidecar::load(&sidecar_path_for(&project_path))
        .map_err(|error| format!("read project metadata: {error}"))?;
    Ok(PreparedBackup {
        project_path,
        snapshot,
        metadata: sidecar.metadata,
        location: location.or(sidecar.location),
        source_revision,
    })
}

/// Back up one prepared project revision and return its refreshed history.
pub fn back_up_prepared(
    listing: ProviderListing,
    prepared: PreparedBackup,
    display_name: String,
    retention_rule: Option<RetentionRule>,
    verify_uploads: bool,
    bundle_policy: BundlePolicy,
) -> Result<BackupResult, String> {
    back_up_prepared_with_resolutions(
        listing,
        prepared,
        display_name,
        retention_rule,
        verify_uploads,
        bundle_policy,
        None,
    )
}

fn back_up_prepared_with_resolutions(
    listing: ProviderListing,
    prepared: PreparedBackup,
    display_name: String,
    retention_rule: Option<RetentionRule>,
    verify_uploads: bool,
    bundle_policy: BundlePolicy,
    conflict_resolutions: Option<Vec<ConflictResolution>>,
) -> Result<BackupResult, String> {
    let PreparedBackup {
        project_path,
        snapshot,
        metadata,
        location,
        source_revision,
    } = prepared;
    runtime()?.block_on(async move {
        let (provider, author) =
            open_provider_for_backup(&listing, &project_path, &display_name).await?;
        let previous_history = if verify_uploads {
            load_history(provider.as_ref()).await.unwrap_or_default()
        } else {
            Vec::new()
        };
        let sidecar_path = sidecar_path_for(&project_path);
        if let Some(location) = &location {
            Sidecar::modify(&sidecar_path, |sidecar| {
                sidecar.location = Some(location.clone());
            })
            .map_err(|error| format!("save project library location: {error}"))?;
        }
        if provider.capabilities().project_listing {
            provider
                .put_project_profile(&ProjectProfile {
                    display_name: project_display_name(&project_path),
                    format: snapshot.format(),
                    metadata: metadata.clone(),
                    location: location.clone(),
                })
                .await
                .map_err(|error| format!("register project with provider: {error}"))?;
        }
        let remote_id = listing.entry.id.clone();

        let mut push_options = PushOptions::for_snapshot(&snapshot);
        push_options.bundle_policy = bundle_policy.clone();
        push_options.conflict_resolutions = conflict_resolutions;
        let outcome = push_with_options(
            provider.as_ref(),
            &remote_id,
            &[],
            &sidecar_path,
            snapshot.as_bytes(),
            author,
            "Backed up changes",
            "",
            &push_options,
        )
        .await?;

        Ok(match outcome {
            PushOutcome::Committed {
                commit_id,
                unavailable_assets,
                ..
            } => {
                let mut verification = if !unavailable_assets.is_empty() {
                    BackupVerification::Incomplete(unavailable_assets)
                } else if verify_uploads {
                    match verify_commit_copy(provider.as_ref(), commit_id).await {
                        Ok(()) => BackupVerification::Verified,
                        Err(error) => BackupVerification::Failed(error.to_string()),
                    }
                } else {
                    BackupVerification::Skipped
                };
                let (retention, retention_warning) = match retention_rule {
                    None => (None, None),
                    Some(_) if verification != BackupVerification::Verified => (
                        None,
                        Some(
                            "old versions were kept because this backup was not completely verified"
                                .to_owned(),
                        ),
                    ),
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
                            let mut protected_blobs = Vec::new();
                            if let Some(stash) = sidecar.stash {
                                protected_blobs.push(stash.snapshot);
                                protected_blobs.extend(stash.resources.into_values());
                            }
                            let protected = RetentionRoots {
                                commits: sidecar.pending_pushes,
                                blobs: protected_blobs,
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
                let history = load_history_after_backup(
                    provider.as_ref(),
                    previous_history,
                    &mut verification,
                )
                .await?;
                if verification == BackupVerification::Verified {
                    Sidecar::modify(&sidecar_path, |sidecar| {
                        sidecar.verified_head = Some(commit_id);
                    })
                    .map_err(|error| format!("record verified backup: {error}"))?;
                }
                BackupResult::Committed(BackupReceipt {
                    history,
                    retention,
                    retention_warning,
                    verification,
                })
            }
            PushOutcome::NeedsResolution { conflicts, .. } => {
                BackupResult::NeedsResolution(Box::new(ConflictBackup {
                    conflicts,
                    listing,
                    prepared: PreparedBackup {
                        project_path,
                        snapshot,
                        metadata,
                        location,
                        source_revision,
                    },
                    display_name,
                    retention_rule,
                    verify_uploads,
                    bundle_policy,
                }))
            }
            PushOutcome::NeedsReview { problems, .. } => BackupResult::NeedsReview(problems.len()),
        })
    })
}

async fn open_provider_for_backup(
    listing: &ProviderListing,
    project_path: &Path,
    local_display_name: &str,
) -> Result<(Arc<dyn ProjectProvider>, AuthorIdentity), String> {
    if listing.local_path().is_some() {
        return Ok((
            open_provider(listing, project_path).await?,
            AuthorIdentity {
                display_name: if local_display_name.trim().is_empty() {
                    "Local user".to_owned()
                } else {
                    local_display_name.to_owned()
                },
                provider_user_id: "local-user".to_owned(),
                provider_id: listing.entry.id.clone(),
                email: None,
            },
        ));
    }

    let handle = persisted_project_handle(listing, project_path)?;
    let account = http_account(listing, &handle).await?;
    let identity = match account.identity().await {
        Ok(identity) => identity,
        Err(auru_pm::Error::NotFound(_)) if account.authentication().is_none() => {
            // Compatibility with pre-authentication `auru-pm-v1` providers,
            // which had no `/v1/me` identity endpoint.
            return Ok((
                Arc::new(account.open_project(handle)),
                AuthorIdentity {
                    display_name: if local_display_name.trim().is_empty() {
                        "Local user".to_owned()
                    } else {
                        local_display_name.to_owned()
                    },
                    provider_user_id: "local-user".to_owned(),
                    provider_id: listing.entry.id.clone(),
                    email: None,
                },
            ));
        }
        Err(error) => return Err(format!("read authenticated identity: {error}")),
    };
    Ok((
        Arc::new(account.open_project(handle)),
        AuthorIdentity {
            display_name: identity.display_name,
            provider_user_id: identity.user_id,
            provider_id: identity.provider_id,
            email: identity.email,
        },
    ))
}

pub fn resolve_backup(
    conflict: &ConflictBackup,
    choices: Vec<ConflictChoice>,
) -> Result<BackupResult, String> {
    if choices.len() != conflict.conflicts.len() {
        return Err(format!(
            "expected {} conflict choice(s), received {}",
            conflict.conflicts.len(),
            choices.len()
        ));
    }
    let resolutions = conflict
        .conflicts
        .iter()
        .cloned()
        .zip(choices)
        .map(|(conflict, choice)| ConflictResolution { conflict, choice })
        .collect();
    back_up_prepared_with_resolutions(
        conflict.listing.clone(),
        conflict.prepared.clone(),
        conflict.display_name.clone(),
        conflict.retention_rule,
        conflict.verify_uploads,
        conflict.bundle_policy.clone(),
        Some(resolutions),
    )
}

#[cfg(test)]
fn back_up(
    listing: ProviderListing,
    project_path: PathBuf,
    display_name: String,
    retention_rule: Option<RetentionRule>,
    verify_uploads: bool,
) -> Result<BackupResult, String> {
    let prepared = prepare_backup(project_path, None)?;
    back_up_prepared(
        listing,
        prepared,
        display_name,
        retention_rule,
        verify_uploads,
        BundlePolicy::default(),
    )
}

fn load_stable_snapshot(path: &Path) -> Result<(ProjectSnapshot, Option<SystemTime>), String> {
    const MAX_ATTEMPTS: usize = 3;
    for _ in 0..MAX_ATTEMPTS {
        let before = project_modified_at(path);
        let snapshot = ProjectSnapshot::load(path)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        let after = project_modified_at(path);
        if before == after {
            return Ok((snapshot, after));
        }
    }
    Err(format!(
        "{} kept changing while it was being read; wait for the save to finish and try again",
        path.display()
    ))
}

fn project_modified_at(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
}

async fn load_history_after_backup(
    provider: &dyn ProjectProvider,
    previous_history: Vec<CommitSummary>,
    verification: &mut BackupVerification,
) -> Result<Vec<CommitSummary>, String> {
    match (load_history(provider).await, verification) {
        (Ok(history), _) => Ok(history),
        (Err(error), BackupVerification::Failed(detail)) => {
            detail.push_str(&format!("; re-read backup history: {error}"));
            Ok(previous_history)
        }
        (Err(_), BackupVerification::Incomplete(_)) => Ok(previous_history),
        (Err(error), verification @ BackupVerification::Verified) => {
            *verification = BackupVerification::Failed(format!("re-read backup history: {error}"));
            Ok(previous_history)
        }
        (Err(error), BackupVerification::Skipped) => Err(error),
    }
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
                        metadata: ProjectMetadata::default(),
                        location: None,
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
                metadata: profile.metadata,
                location: profile.location,
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

/// Restore a commit into a new safety folder under `destination`.
///
/// When the profile knows its watched-root-relative location, the organizing
/// parent folders are recreated first. A restore never writes over the working
/// project; it is something a person can inspect before deciding whether it
/// replaces current work.
#[cfg(test)]
pub fn restore(
    listing: ProviderListing,
    working_project: PathBuf,
    commit_id: CommitId,
    destination: PathBuf,
) -> Result<RestoreResult, String> {
    restore_with_collision(
        listing,
        working_project,
        commit_id,
        destination,
        RestoreCollisionChoice::Duplicate,
    )
}

pub fn restore_with_collision(
    listing: ProviderListing,
    working_project: PathBuf,
    commit_id: CommitId,
    destination: PathBuf,
    collision: RestoreCollisionChoice,
) -> Result<RestoreResult, String> {
    runtime()?.block_on(async move {
        let provider = open_provider(&listing, &working_project).await?;
        let location = Sidecar::load(&sidecar_path_for(&working_project))
            .map_err(|error| format!("read project library location: {error}"))?
            .location;
        let file_name = working_project
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("Restored Project")
            .to_owned();
        restore_from_provider(
            provider.as_ref(),
            commit_id,
            &destination,
            &file_name,
            location.as_ref(),
            collision,
        )
        .await
    })
}

/// Recover a provider-only project and enroll the restored copy locally.
#[cfg(test)]
pub fn restore_remote(
    listing: ProviderListing,
    handle: String,
    file_name: String,
    metadata: ProjectMetadata,
    location: Option<ProjectLocation>,
    commit_id: CommitId,
    destination: PathBuf,
) -> Result<RestoreResult, String> {
    restore_remote_with_collision(
        listing,
        handle,
        file_name,
        metadata,
        location,
        commit_id,
        destination,
        RestoreCollisionChoice::Duplicate,
    )
}

#[expect(clippy::too_many_arguments)]
pub fn restore_remote_with_collision(
    listing: ProviderListing,
    handle: String,
    file_name: String,
    metadata: ProjectMetadata,
    location: Option<ProjectLocation>,
    commit_id: CommitId,
    destination: PathBuf,
    collision: RestoreCollisionChoice,
) -> Result<RestoreResult, String> {
    runtime()?.block_on(async move {
        let provider = provider_account(&listing)
            .await?
            .open_project(&handle)
            .await
            .map_err(|error| format!("open remote project: {error}"))?;
        let result = restore_from_provider(
            provider.as_ref(),
            commit_id,
            &destination,
            &file_name,
            location.as_ref(),
            collision,
        )
        .await?;
        let result = require_complete_recovery(result)?;
        let sidecar_path = sidecar_path_for(&result.project_file);
        Sidecar {
            location,
            metadata,
            primary: Some(listing.entry.id.clone()),
            provider_handles: BTreeMap::from([(listing.entry.id, handle)]),
            local_head: Some(commit_id),
            verified_head: Some(commit_id),
            ..Sidecar::default()
        }
        .save(&sidecar_path)
        .map_err(|error| format!("enroll restored project: {error}"))?;
        Ok(result)
    })
}

/// Persist user-authored project metadata locally and, when enrolled, update
/// the provider catalogue profile as well.
pub fn save_project_metadata(
    provider_target: Option<(ProviderListing, String)>,
    project_path: Option<PathBuf>,
    profile: ProjectProfile,
) -> Result<(), String> {
    if let Some(project_path) = project_path {
        let sidecar_path = sidecar_path_for(&project_path);
        let backup_marker = std::fs::metadata(&sidecar_path)
            .and_then(|metadata| metadata.modified())
            .ok();
        let metadata = profile.metadata.clone();
        let location = profile.location.clone();
        Sidecar::modify(&sidecar_path, |sidecar| {
            sidecar.metadata = metadata;
            sidecar.location = location;
        })
        .map_err(|error| format!("save local project metadata: {error}"))?;
        if let Some(backup_marker) = backup_marker {
            // Project status compares this timestamp with the DAW file's mtime.
            // Metadata is not a backup and must not move that marker forward.
            let sidecar_file = std::fs::OpenOptions::new()
                .write(true)
                .open(&sidecar_path)
                .map_err(|error| format!("restore project backup marker: {error}"))?;
            sidecar_file
                .set_times(std::fs::FileTimes::new().set_modified(backup_marker))
                .map_err(|error| format!("restore project backup marker: {error}"))?;
        }
    }

    let Some((listing, handle)) = provider_target else {
        return Ok(());
    };
    runtime()?.block_on(async move {
        let provider = provider_account(&listing)
            .await?
            .open_project(&handle)
            .await
            .map_err(|error| format!("open project metadata on {}: {error}", listing.entry.name))?;
        provider
            .put_project_profile(&profile)
            .await
            .map_err(|error| format!("update project metadata on {}: {error}", listing.entry.name))
    })
}

fn require_complete_recovery(result: RestoreResult) -> Result<RestoreResult, String> {
    if result.unavailable == 0 && result.verified_files > 0 {
        return Ok(result);
    }
    if result.verified_files == 0 {
        return Err(format!(
            "Restored a copy to {}, but no output files could be hash-verified. The copy was not enrolled as synchronized.",
            result.project_file.display()
        ));
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
    ProviderAccount::connect_http_stored(&listing.entry.endpoint, &listing.entry.id)
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

    let account = http_account(listing, &handle).await?;
    Ok(Arc::new(account.open_project(handle)))
}

async fn http_account(listing: &ProviderListing, handle: &str) -> Result<HttpAccount, String> {
    let project_token = auru_pm::token_store::load_token(&listing.entry.id, handle)
        .ok()
        .flatten();
    let account = if project_token.is_some() {
        HttpAccount::connect(&listing.entry.endpoint, project_token).await
    } else {
        HttpAccount::connect_stored(&listing.entry.endpoint, &listing.entry.id).await
    }
    .map_err(|error| format!("connect to {}: {error}", listing.entry.name))?;
    Ok(account)
}

async fn restore_from_provider(
    provider: &dyn ProjectProvider,
    commit_id: CommitId,
    destination: &Path,
    requested_file_name: &str,
    location: Option<&ProjectLocation>,
    collision: RestoreCollisionChoice,
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
    let requested_root = restore_root_for(destination, location, Path::new(&file_name), commit_id);
    let final_root = resolve_restore_collision(&requested_root, collision)?;
    let staging_root = create_staging_root(&final_root)?;

    let restored =
        restore_into_staging(provider, &commit, &snapshot, &staging_root, &file_name).await;
    let mut restored = match restored {
        Ok(restored) => restored,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    let project_relative = match restored.project_file.strip_prefix(&staging_root) {
        Ok(relative) => relative.to_path_buf(),
        Err(_) => {
            let _ = fs::remove_dir_all(&staging_root);
            return Err("restored project escaped its staging folder".to_owned());
        }
    };
    let expected_files = match hash_restore_tree(&staging_root) {
        Ok(expected) => expected,
        Err(error) => {
            let _ = fs::remove_dir_all(&staging_root);
            return Err(error);
        }
    };
    if restored.unavailable > 0
        && matches!(
            collision,
            RestoreCollisionChoice::Overwrite | RestoreCollisionChoice::DeleteAndReplace
        )
    {
        let _ = fs::remove_dir_all(&staging_root);
        return Err(format!(
            "The staged restore is missing {} referenced file(s), so the existing copy was left untouched.",
            restored.unavailable
        ));
    }

    let previous_copy =
        match publish_verified_restore(&staging_root, &final_root, &expected_files, collision) {
            Ok(previous) => previous,
            Err(error) => {
                let _ = fs::remove_dir_all(&staging_root);
                return Err(error);
            }
        };
    restored.project_file = final_root.join(project_relative);
    restored.verified_files = expected_files.len();
    restored.previous_copy = previous_copy;
    Ok(restored)
}

async fn restore_into_staging(
    provider: &dyn ProjectProvider,
    commit: &auru_pm::Commit,
    snapshot: &ProjectSnapshot,
    restore_root: &Path,
    file_name: &str,
) -> Result<RestoreResult, String> {
    match snapshot.format() {
        ProjectFormat::AbletonLiveSet => {
            let report =
                auru_pm::ableton::restore_bundle(provider, commit, restore_root, file_name)
                    .await
                    .map_err(|error| format!("restore Ableton project: {error}"))?;
            Ok(RestoreResult {
                project_file: report.live_set,
                files_written: report.files_written,
                unavailable: report
                    .unavailable
                    .into_iter()
                    .chain(report.rewrite.unresolved)
                    .collect::<std::collections::BTreeSet<_>>()
                    .len(),
                verified_files: 0,
                previous_copy: None,
            })
        }
        ProjectFormat::FlStudio => {
            restore_fl(provider, commit, snapshot, restore_root, file_name).await
        }
        ProjectFormat::Auru => {
            restore_auru(provider, commit, snapshot, restore_root, file_name).await
        }
        ProjectFormat::Dawproject => {
            restore_dawproject(provider, commit, snapshot, restore_root, file_name).await
        }
        ProjectFormat::BitwigProject => restore_opaque_project(snapshot, restore_root, file_name),
    }
}

fn restore_opaque_project(
    snapshot: &ProjectSnapshot,
    restore_root: &Path,
    file_name: &str,
) -> Result<RestoreResult, String> {
    std::fs::create_dir_all(restore_root)
        .map_err(|error| format!("create restore folder: {error}"))?;
    let project_file = restore_root.join(file_name);
    snapshot
        .restore_to_path(&project_file)
        .map_err(|error| format!("restore project: {error}"))?;
    Ok(RestoreResult {
        project_file,
        files_written: 0,
        unavailable: 0,
        verified_files: 0,
        previous_copy: None,
    })
}

async fn restore_dawproject(
    provider: &dyn ProjectProvider,
    commit: &auru_pm::Commit,
    snapshot: &ProjectSnapshot,
    restore_root: &Path,
    file_name: &str,
) -> Result<RestoreResult, String> {
    let manifest_bytes = provider
        .get_blob(&commit.tree.samples)
        .await
        .map_err(|error| format!("fetch DAWproject media list: {error}"))?;
    let manifest: SampleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| format!("read DAWproject media list: {error}"))?;
    let resource_paths = auru_pm::dawproject::archive_resource_paths(snapshot)
        .map_err(|error| format!("read DAWproject resources: {error}"))?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    let requires_hydration = auru_pm::dawproject::requires_resource_hydration(snapshot)
        .map_err(|error| format!("read DAWproject resource storage: {error}"))?;
    let mut fetched = BTreeMap::new();
    for entry in manifest
        .entries
        .iter()
        .filter(|entry| resource_paths.contains(&entry.path))
    {
        if let Ok(bytes) = provider.get_blob(&entry.hash).await {
            fetched.insert(entry.path.clone(), bytes);
        }
    }
    let hydrated = auru_pm::dawproject::hydrate_embedded_assets(snapshot, &fetched)
        .map_err(|error| format!("hydrate DAWproject media: {error}"))?;

    std::fs::create_dir_all(restore_root)
        .map_err(|error| format!("create restore folder: {error}"))?;
    let project_file = restore_root.join(file_name);
    hydrated
        .restore_to_path(&project_file)
        .map_err(|error| format!("restore project: {error}"))?;
    Ok(RestoreResult {
        project_file,
        files_written: fetched.len(),
        unavailable: if requires_hydration {
            resource_paths.len().saturating_sub(fetched.len())
        } else {
            0
        },
        verified_files: 0,
        previous_copy: None,
    })
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
        write_verified_new(&target, &bytes)
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
        verified_files: 0,
        previous_copy: None,
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
    write_verified_new(&project_file, &stream.encode())
        .map_err(|error| format!("write restored FL project: {error}"))?;
    Ok(RestoreResult {
        project_file,
        files_written,
        unavailable,
        verified_files: 0,
        previous_copy: None,
    })
}

fn restore_root_for(
    destination: &Path,
    location: Option<&ProjectLocation>,
    fallback_project_path: &Path,
    commit_id: CommitId,
) -> PathBuf {
    let Some(relative) = location.and_then(safe_restore_location) else {
        return restore_root_candidate(destination, fallback_project_path, commit_id);
    };
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let project_path = relative
        .file_name()
        .map(PathBuf::from)
        .unwrap_or_else(|| fallback_project_path.to_path_buf());
    restore_root_candidate(&destination.join(parent), &project_path, commit_id)
}

/// The first restore path presented to the user before any collision policy is
/// applied. Safe to call on provider metadata because every component is
/// sanitised by the same path builder used by the restore itself.
pub fn restore_target(
    destination: &Path,
    requested_file_name: &str,
    format: ProjectFormat,
    location: Option<&ProjectLocation>,
    commit_id: CommitId,
) -> PathBuf {
    let requested_stem = Path::new(requested_file_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Restored Project");
    let file_name = safe_project_file_name(requested_stem, format);
    restore_root_for(destination, location, Path::new(&file_name), commit_id)
}

/// Turn an untrusted provider profile location into a path that can only live
/// beneath the restore root. Invalid paths deliberately fall back to the root
/// rather than making a remote catalogue entry impossible to recover.
fn safe_restore_location(location: &ProjectLocation) -> Option<PathBuf> {
    const MAX_COMPONENTS: usize = 32;
    let value = location.relative_path.trim();
    if value.is_empty() || value.starts_with(['/', '\\']) {
        return None;
    }

    let mut relative = PathBuf::new();
    for (index, component) in value.split('/').enumerate() {
        if index >= MAX_COMPONENTS || component.is_empty() || matches!(component, "." | "..") {
            return None;
        }
        let mut component = safe_file_component(component, "", 160);
        if component.is_empty() {
            return None;
        }
        if is_windows_reserved_name(&component) {
            component.insert(0, '_');
        }
        relative.push(component);
    }
    (!relative.as_os_str().is_empty()).then_some(relative)
}

fn restore_root_candidate(destination: &Path, project_path: &Path, commit_id: CommitId) -> PathBuf {
    let stem = project_path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("Project");
    let commit = commit_id.0.to_string();
    let commit = commit.strip_prefix("blake3:").unwrap_or(&commit);
    let name = format!("{stem} Restored {}", &commit[..8]);
    destination.join(name)
}

#[cfg(test)]
fn unique_restore_root(destination: &Path, project_path: &Path, commit_id: CommitId) -> PathBuf {
    let first = restore_root_candidate(destination, project_path, commit_id);
    unique_duplicate_root(&first)
}

fn unique_duplicate_root(first: &Path) -> PathBuf {
    if !first.exists() {
        return first.to_path_buf();
    }
    let destination = first.parent().unwrap_or_else(|| Path::new(""));
    let name = first
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Project Restored");

    (2..)
        .map(|copy| destination.join(format!("{name} ({copy})")))
        .find(|candidate| !candidate.exists())
        .expect("the restore copy number cannot be exhausted")
}

fn resolve_restore_collision(
    requested: &Path,
    collision: RestoreCollisionChoice,
) -> Result<PathBuf, String> {
    if !requested.exists() {
        return Ok(requested.to_path_buf());
    }
    match collision {
        RestoreCollisionChoice::AbortIfExists => Err(format!(
            "{} now exists. Choose whether to delete, overwrite, duplicate, or ignore it.",
            requested.display()
        )),
        RestoreCollisionChoice::Duplicate => Ok(unique_duplicate_root(requested)),
        RestoreCollisionChoice::Overwrite | RestoreCollisionChoice::DeleteAndReplace => {
            Ok(requested.to_path_buf())
        }
    }
}

fn create_staging_root(final_root: &Path) -> Result<PathBuf, String> {
    let parent = final_root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|error| format!("create restore parent: {error}"))?;
    let name = final_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Project Restored");
    for copy in 1_u32.. {
        let candidate = parent.join(format!(
            ".{name}.auru-restoring-{}-{copy}",
            std::process::id()
        ));
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("create restore staging folder: {error}")),
        }
    }
    unreachable!("the staging copy number cannot be exhausted")
}

fn hash_restore_tree(root: &Path) -> Result<Vec<(PathBuf, ContentHash)>, String> {
    fn visit(
        root: &Path,
        directory: &Path,
        hashes: &mut Vec<(PathBuf, ContentHash)>,
    ) -> Result<(), String> {
        let mut entries = fs::read_dir(directory)
            .map_err(|error| format!("read staged restore: {error}"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("read staged restore entry: {error}"))?;
        entries.sort_by_key(std::fs::DirEntry::path);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path)
                .map_err(|error| format!("inspect staged restore: {error}"))?;
            if metadata.file_type().is_symlink() {
                return Err(format!(
                    "refusing a symlink in staged restore: {}",
                    path.display()
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, hashes)?;
            } else if metadata.is_file() {
                let relative = path
                    .strip_prefix(root)
                    .map(Path::to_path_buf)
                    .map_err(|_| "staged restore escaped its root".to_owned())?;
                let hash = ContentHash::of_file(&path)
                    .map_err(|error| format!("BLAKE3 hash {}: {error}", path.display()))?;
                hashes.push((relative, hash));
            }
        }
        Ok(())
    }

    let mut hashes = Vec::new();
    visit(root, root, &mut hashes)?;
    Ok(hashes)
}

fn verify_restore_tree(
    root: &Path,
    expected_files: &[(PathBuf, ContentHash)],
) -> Result<(), String> {
    for (relative, expected) in expected_files {
        let path = root.join(relative);
        let actual = ContentHash::of_file(&path)
            .map_err(|error| format!("verify restored file {}: {error}", path.display()))?;
        if actual != *expected {
            return Err(format!(
                "restored file {} failed BLAKE3 verification: expected {expected}, read {actual}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn publish_verified_restore(
    staging: &Path,
    final_root: &Path,
    expected_files: &[(PathBuf, ContentHash)],
    collision: RestoreCollisionChoice,
) -> Result<Option<PathBuf>, String> {
    if !final_root.exists() {
        fs::rename(staging, final_root)
            .map_err(|error| format!("publish verified restore: {error}"))?;
        if let Err(error) = verify_restore_tree(final_root, expected_files) {
            let _ = fs::rename(final_root, staging);
            return Err(error);
        }
        return Ok(None);
    }

    if matches!(
        collision,
        RestoreCollisionChoice::AbortIfExists | RestoreCollisionChoice::Duplicate
    ) {
        return Err(format!(
            "{} appeared while the restore was being verified; nothing was overwritten",
            final_root.display()
        ));
    }

    let quarantine = unique_quarantine_path(final_root);
    fs::rename(final_root, &quarantine)
        .map_err(|error| format!("protect existing project before replacement: {error}"))?;
    if let Err(error) = fs::rename(staging, final_root) {
        let _ = fs::rename(&quarantine, final_root);
        return Err(format!("install verified restore: {error}"));
    }

    let installed = if collision == RestoreCollisionChoice::Overwrite {
        copy_missing_entries(&quarantine, final_root)
            .and_then(|()| verify_restore_tree(final_root, expected_files))
    } else {
        verify_restore_tree(final_root, expected_files)
    };
    if let Err(error) = installed {
        let rollback = rollback_replacement(final_root, staging, &quarantine);
        return Err(match rollback {
            Ok(()) => format!("{error}; the existing project was restored unchanged"),
            Err(rollback) => format!(
                "{error}; automatic rollback also failed: {rollback}. The previous copy remains at {}",
                quarantine.display()
            ),
        });
    }

    match remove_path(&quarantine) {
        Ok(()) => Ok(None),
        Err(_) => Ok(Some(quarantine)),
    }
}

fn unique_quarantine_path(final_root: &Path) -> PathBuf {
    let parent = final_root.parent().unwrap_or_else(|| Path::new(""));
    let name = final_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("Project Restored");
    (1_u32..)
        .map(|copy| {
            parent.join(format!(
                ".{name}.auru-previous-{}-{copy}",
                std::process::id()
            ))
        })
        .find(|candidate| !candidate.exists())
        .expect("the quarantine copy number cannot be exhausted")
}

fn copy_missing_entries(source: &Path, destination: &Path) -> Result<(), String> {
    for entry in fs::read_dir(source).map_err(|error| format!("read existing project: {error}"))? {
        let entry = entry.map_err(|error| format!("read existing project entry: {error}"))?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_path)
            .map_err(|error| format!("inspect existing project entry: {error}"))?;
        if destination_path.exists() {
            if metadata.is_dir() && destination_path.is_dir() {
                copy_missing_entries(&source_path, &destination_path)?;
            }
            continue;
        }
        if metadata.file_type().is_symlink() {
            return Err(format!(
                "cannot safely merge symlink {}; choose Keep both or Delete and replace instead",
                source_path.display()
            ));
        }
        if metadata.is_dir() {
            fs::create_dir(&destination_path)
                .map_err(|error| format!("preserve existing folder: {error}"))?;
            copy_missing_entries(&source_path, &destination_path)?;
        } else if metadata.is_file() {
            fs::copy(&source_path, &destination_path)
                .map_err(|error| format!("preserve existing file: {error}"))?;
            let expected = ContentHash::of_file(&source_path)
                .map_err(|error| format!("hash existing file: {error}"))?;
            let actual = ContentHash::of_file(&destination_path)
                .map_err(|error| format!("verify preserved file: {error}"))?;
            if actual != expected {
                return Err(format!(
                    "preserved file {} failed BLAKE3 verification",
                    destination_path.display()
                ));
            }
        }
    }
    Ok(())
}

fn rollback_replacement(
    final_root: &Path,
    staging: &Path,
    quarantine: &Path,
) -> Result<(), String> {
    fs::rename(final_root, staging)
        .map_err(|error| format!("move failed restore aside: {error}"))?;
    fs::rename(quarantine, final_root)
        .map_err(|error| format!("put existing project back: {error}"))
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

fn write_verified_new(path: &Path, bytes: &[u8]) -> Result<ContentHash, String> {
    let expected = ContentHash::of(bytes);
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("create {}: {error}", path.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(format!("write {}: {error}", path.display()));
    }
    drop(file);
    let actual = ContentHash::of_file(path)
        .map_err(|error| format!("verify {}: {error}", path.display()))?;
    if actual != expected {
        let _ = fs::remove_file(path);
        return Err(format!(
            "{} failed BLAKE3 verification: expected {expected}, read {actual}",
            path.display()
        ));
    }
    Ok(expected)
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
    fn an_opaque_bitwig_restore_should_write_the_exact_project_bytes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = b"BtWg0003000200ba\0opaque project data";
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::BitwigProject, source)
            .expect("snapshot");

        let restored = restore_opaque_project(&snapshot, temp.path(), "Song.bwproject")
            .expect("restore Bitwig project");

        assert_eq!(
            std::fs::read(restored.project_file).expect("restored bytes"),
            source
        );
    }

    #[test]
    fn a_local_backup_should_create_history_and_record_its_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        std::fs::write(&project, br#"{"version":8,"channels":[]}"#).expect("project");
        let destination = temp.path().join("Backups");
        let listing = local_listing(&destination);

        let BackupResult::Committed(BackupReceipt {
            history,
            verification,
            ..
        }) = back_up(
            listing.clone(),
            project.clone(),
            "Jake".to_owned(),
            None,
            true,
        )
        .expect("backup")
        else {
            panic!("first backup should commit");
        };

        assert_eq!(history.len(), 1);
        assert_eq!(verification, BackupVerification::Verified);
        let sidecar = Sidecar::load(&sidecar_path_for(&project)).expect("sidecar");
        assert_eq!(sidecar.primary.as_deref(), Some(listing.entry.id.as_str()));
        assert_eq!(sidecar.verified_head, Some(history[0].id));
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
    fn backup_and_recovery_should_preserve_the_nested_library_location() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp
            .path()
            .join("Music Production/Auru/Projects/Night Drive.auru");
        std::fs::create_dir_all(project.parent().expect("project parent")).expect("mkdir");
        std::fs::write(&project, br#"{"version":8,"channels":[]}"#).expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let location = ProjectLocation {
            relative_path: "Auru/Projects/Night Drive.auru".to_owned(),
        };
        let prepared = prepare_backup(project, Some(location.clone())).expect("prepare backup");
        let BackupResult::Committed(BackupReceipt { history, .. }) = back_up_prepared(
            listing.clone(),
            prepared,
            "Jake".to_owned(),
            None,
            false,
            BundlePolicy::default(),
        )
        .expect("backup") else {
            panic!("backup should commit");
        };
        let remote = remote_catalogue(listing.clone())
            .expect("catalogue")
            .projects
            .remove(0);
        let recovery_root = temp.path().join("Recovered Music Production");

        let restored = restore_remote(
            listing,
            remote.handle,
            remote.file_name,
            remote.metadata,
            remote.location,
            history[0].id,
            recovery_root.clone(),
        )
        .expect("restore");

        assert!(
            restored
                .project_file
                .starts_with(recovery_root.join("Auru/Projects")),
            "restored to {}",
            restored.project_file.display()
        );
        let sidecar =
            Sidecar::load(&sidecar_path_for(&restored.project_file)).expect("restored sidecar");
        assert_eq!(sidecar.location, Some(location));
        assert_eq!(sidecar.verified_head, Some(history[0].id));
    }

    #[test]
    fn saving_metadata_should_update_the_local_sidecar_and_provider_profile() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        std::fs::write(&project, br#"{"version":8,"channels":[]}"#).expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let BackupResult::Committed(_) = back_up(
            listing.clone(),
            project.clone(),
            "Jake".to_owned(),
            None,
            false,
        )
        .expect("initial backup") else {
            panic!("first backup should commit");
        };
        let sidecar_path = sidecar_path_for(&project);
        let sidecar = Sidecar::load(&sidecar_path).expect("enrolled sidecar");
        let backup_marker = std::fs::metadata(&sidecar_path)
            .and_then(|metadata| metadata.modified())
            .expect("backup marker");
        let handle = sidecar
            .provider_handles
            .get(&listing.entry.id)
            .expect("project handle")
            .clone();
        let metadata = ProjectMetadata {
            genre: Some("Drum & Bass".to_owned()),
            tags: vec!["work in progress".to_owned(), "collab".to_owned()],
        };
        let location = ProjectLocation {
            relative_path: "Auru/Projects/Song.auru".to_owned(),
        };

        save_project_metadata(
            Some((listing.clone(), handle)),
            Some(project.clone()),
            ProjectProfile {
                display_name: "Song".to_owned(),
                format: ProjectFormat::Auru,
                metadata: metadata.clone(),
                location: Some(location.clone()),
            },
        )
        .expect("save project metadata");

        let local = Sidecar::load(&sidecar_path).expect("updated sidecar");
        let saved_marker = std::fs::metadata(&sidecar_path)
            .and_then(|metadata| metadata.modified())
            .expect("preserved backup marker");
        let remote = remote_catalogue(listing)
            .expect("updated provider catalogue")
            .projects
            .remove(0);
        assert_eq!(
            (
                local.metadata,
                remote.metadata,
                local.location,
                remote.location,
                saved_marker
            ),
            (
                metadata.clone(),
                metadata,
                Some(location.clone()),
                Some(location),
                backup_marker
            )
        );
    }

    #[test]
    fn a_failed_verification_should_preserve_history_when_the_commit_cannot_be_re_read() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.dawproject");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/auru-pm/tests/fixtures/interchange/oracle-midi.dawproject"),
            &project,
        )
        .expect("project");
        let destination = temp.path().join("Backups");
        let listing = local_listing(&destination);
        let BackupResult::Committed(BackupReceipt { history, .. }) = back_up(
            listing.clone(),
            project.clone(),
            "Jake".to_owned(),
            None,
            false,
        )
        .expect("backup") else {
            panic!("backup should commit");
        };
        let sidecar = Sidecar::load(&sidecar_path_for(&project)).expect("sidecar");
        let handle = sidecar
            .provider_handles
            .get(&listing.entry.id)
            .expect("provider handle");
        let stored = FilesystemProvider::open(destination.join(".auru-pm/projects").join(handle))
            .expect("stored project");
        let commits = auru_pm::Cas::open(stored.root().join("commits")).expect("commit store");
        std::fs::remove_file(commits.path_for(&history[0].id.0)).expect("remove uploaded commit");
        let mut verification = BackupVerification::Failed("re-read commit: not found".to_owned());

        let retained = runtime()
            .expect("runtime")
            .block_on(load_history_after_backup(
                &stored,
                history.clone(),
                &mut verification,
            ))
            .expect("a committed upload with a verification warning remains successful");

        assert_eq!(retained.len(), 1);
        assert_eq!(retained[0].id, history[0].id);
        let BackupVerification::Failed(error) = verification else {
            panic!("history failure should remain a verification warning");
        };
        assert!(error.contains("re-read backup history"), "{error}");
    }

    #[test]
    fn a_prepared_backup_should_commit_the_revision_recorded_before_publication() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.dawproject");
        std::fs::copy(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../crates/auru-pm/tests/fixtures/interchange/oracle-midi.dawproject"),
            &project,
        )
        .expect("project");
        let prepared =
            prepare_backup(project.clone(), None).expect("prepare stable project revision");
        let prepared_revision = prepared.source_revision();
        assert!(prepared_revision.is_some());

        // Simulate another save arriving after the UI durably records the
        // prepared revision but before the coordinator publishes it.
        std::fs::write(&project, b"newer, incomplete save").expect("change working project");

        let BackupResult::Committed(receipt) = back_up_prepared(
            local_listing(&temp.path().join("Backups")),
            prepared,
            "Jake".to_owned(),
            None,
            false,
            BundlePolicy::default(),
        )
        .expect("commit the already prepared revision") else {
            panic!("prepared backup should commit");
        };

        assert!(!receipt.history.is_empty());
    }

    #[test]
    fn a_conflicted_backup_should_resume_with_one_choice_per_field() {
        let temp = tempfile::tempdir().expect("tempdir");
        let destination = temp.path().join("Backups");
        let listing = local_listing(&destination);
        let local = temp.path().join("Local.auru");
        let remote = temp.path().join("Remote.auru");
        let project = |tempo| {
            serde_json::to_vec(&serde_json::json!({
                "version": 8,
                "tempo": tempo,
                "channels": []
            }))
            .expect("project")
        };
        std::fs::write(&local, project(120)).expect("local project");
        let BackupResult::Committed(_) = back_up(
            listing.clone(),
            local.clone(),
            "Local".to_owned(),
            None,
            false,
        )
        .expect("initial backup") else {
            panic!("initial backup should commit");
        };
        std::fs::copy(&local, &remote).expect("remote working copy");
        std::fs::copy(sidecar_path_for(&local), sidecar_path_for(&remote)).expect("remote sidecar");

        std::fs::write(&remote, project(140)).expect("remote edit");
        let BackupResult::Committed(_) =
            back_up(listing.clone(), remote, "Remote".to_owned(), None, false)
                .expect("remote backup")
        else {
            panic!("remote backup should commit");
        };
        std::fs::write(&local, project(128)).expect("local edit");
        let BackupResult::NeedsResolution(conflict) =
            back_up(listing, local, "Local".to_owned(), None, false).expect("conflicted backup")
        else {
            panic!("divergent tempo edits should conflict");
        };
        assert_eq!(conflict.conflicts().len(), 1);

        let BackupResult::Committed(_) =
            resolve_backup(&conflict, vec![auru_pm::ConflictChoice::Remote])
                .expect("resolved backup")
        else {
            panic!("a complete set of choices should commit");
        };
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
            let BackupResult::Committed(receipt) = back_up(
                listing.clone(),
                project.clone(),
                "Jake".to_owned(),
                rule,
                true,
            )
            .expect("backup") else {
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
    fn an_incomplete_backup_should_keep_the_last_verified_version_and_skip_retention() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song.auru");
        let sample = temp.path().join("take.wav");
        fs::write(&sample, b"irreplaceable audio").expect("sample");
        let project_bytes = |version| {
            serde_json::to_vec(&serde_json::json!({
                "version": 8,
                "name": version,
                "channels": [{ "clips": [{
                    "data": { "Audio": { "file_path": sample.to_str().unwrap() } }
                }]}]
            }))
            .expect("project json")
        };
        fs::write(&project, project_bytes("complete")).expect("project");
        let listing = local_listing(&temp.path().join("Backups"));
        let rule = Some(RetentionRule::Latest { count: 1 });
        let BackupResult::Committed(first) = back_up(
            listing.clone(),
            project.clone(),
            "Jake".to_owned(),
            rule,
            true,
        )
        .expect("complete backup") else {
            panic!("complete backup should commit");
        };
        let verified = first.history[0].id;

        fs::remove_file(&sample).expect("simulate disconnected source");
        fs::write(&project, project_bytes("incomplete")).expect("changed project");
        let BackupResult::Committed(second) =
            back_up(listing, project.clone(), "Jake".to_owned(), rule, true)
                .expect("incomplete commit remains recoverable")
        else {
            panic!("incomplete backup should still preserve history");
        };

        assert!(matches!(
            second.verification,
            BackupVerification::Incomplete(ref unavailable) if unavailable.len() == 1
        ));
        assert_eq!(
            second.history.len(),
            2,
            "the verified version must not be pruned"
        );
        let sidecar = Sidecar::load(&sidecar_path_for(&project)).expect("sidecar");
        assert_eq!(sidecar.verified_head, Some(verified));
        assert_ne!(sidecar.local_head, sidecar.verified_head);
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
            back_up(listing.clone(), project, "Jake".to_owned(), None, false).expect("backup")
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
            remote.metadata,
            remote.location,
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
        assert_eq!(sidecar.verified_head, Some(history[0].id));
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
            false,
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
            verified_files: 4,
            previous_copy: None,
        };

        let error = require_complete_recovery(result).expect_err("partial restore");

        assert!(error.contains("was not enrolled as synchronized"));
        assert!(error.contains("2 referenced file(s) could not be restored"));
    }

    #[test]
    fn an_unverified_remote_restore_should_not_be_treated_as_synchronized() {
        let result = RestoreResult {
            project_file: PathBuf::from("/recover/Night Drive.als"),
            files_written: 0,
            unavailable: 0,
            verified_files: 0,
            previous_copy: None,
        };

        let error = require_complete_recovery(result).expect_err("unverified restore");

        assert!(error.contains("no output files could be hash-verified"));
        assert!(error.contains("was not enrolled as synchronized"));
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
            back_up(listing.clone(), project, "Jake".to_owned(), None, false).expect("backup")
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
            remote.metadata,
            remote.location,
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
        let BackupResult::Committed(BackupReceipt { history, .. }) = back_up(
            listing.clone(),
            project.clone(),
            "Jake".to_owned(),
            None,
            false,
        )
        .expect("backup") else {
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
        let BackupResult::Committed(BackupReceipt { history, .. }) = back_up(
            listing.clone(),
            project.clone(),
            "Jake".to_owned(),
            None,
            false,
        )
        .expect("backup") else {
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
    fn a_restore_location_should_recreate_only_safe_relative_parents() {
        let temp = tempfile::tempdir().expect("tempdir");
        let commit = CommitId(auru_pm::ContentHash::of(b"nested restore"));
        let location = ProjectLocation {
            relative_path: "Ableton/Projects/Night Drive Project".to_owned(),
        };

        let root = restore_root_for(
            temp.path(),
            Some(&location),
            Path::new("Night Drive.als"),
            commit,
        );

        assert_eq!(
            root.parent(),
            Some(temp.path().join("Ableton/Projects").as_path())
        );
        assert!(
            root.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("Night Drive Project Restored "))
        );
    }

    #[test]
    fn a_traversal_restore_location_should_fall_back_inside_the_selected_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let commit = CommitId(auru_pm::ContentHash::of(b"safe restore"));
        let location = ProjectLocation {
            relative_path: "../../outside".to_owned(),
        };

        let root = restore_root_for(
            temp.path(),
            Some(&location),
            Path::new("Night Drive.als"),
            commit,
        );

        assert_eq!(root.parent(), Some(temp.path()));
    }

    #[test]
    fn delete_and_replace_should_publish_only_after_the_staged_copy_is_verified() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("Song Restored");
        fs::create_dir(&final_root).expect("existing root");
        fs::write(final_root.join("old.txt"), b"old project").expect("existing project");
        let staging = create_staging_root(&final_root).expect("staging");
        write_verified_new(&staging.join("Song.auru"), b"verified restore").expect("write");
        let hashes = hash_restore_tree(&staging).expect("hash staging");

        publish_verified_restore(
            &staging,
            &final_root,
            &hashes,
            RestoreCollisionChoice::DeleteAndReplace,
        )
        .expect("replace");

        assert_eq!(
            fs::read(final_root.join("Song.auru")).expect("restored project"),
            b"verified restore"
        );
        assert!(!final_root.join("old.txt").exists());
    }

    #[test]
    fn overwrite_should_preserve_unrelated_files_and_replace_matching_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("Song Restored");
        fs::create_dir(&final_root).expect("existing root");
        fs::write(final_root.join("Song.auru"), b"old project").expect("existing project");
        fs::write(final_root.join("notes.txt"), b"keep me").expect("existing notes");
        let staging = create_staging_root(&final_root).expect("staging");
        write_verified_new(&staging.join("Song.auru"), b"verified restore").expect("write");
        let hashes = hash_restore_tree(&staging).expect("hash staging");

        publish_verified_restore(
            &staging,
            &final_root,
            &hashes,
            RestoreCollisionChoice::Overwrite,
        )
        .expect("overwrite");

        assert_eq!(
            fs::read(final_root.join("Song.auru")).expect("restored project"),
            b"verified restore"
        );
        assert_eq!(
            fs::read(final_root.join("notes.txt")).expect("preserved notes"),
            b"keep me"
        );
    }

    #[test]
    fn an_unconfirmed_collision_should_leave_the_existing_project_untouched() {
        let temp = tempfile::tempdir().expect("tempdir");
        let final_root = temp.path().join("Song Restored");
        fs::create_dir(&final_root).expect("existing root");
        fs::write(final_root.join("Song.auru"), b"working project").expect("existing project");

        resolve_restore_collision(&final_root, RestoreCollisionChoice::AbortIfExists)
            .expect_err("choice required");

        assert_eq!(
            fs::read(final_root.join("Song.auru")).expect("working project"),
            b"working project"
        );
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
