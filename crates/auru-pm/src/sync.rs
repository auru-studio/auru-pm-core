//! Push coordinator: freshness-checked commit, mirror fan-out, pending drain.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::Value;

use crate::ableton::BundlePolicy;
use crate::ableton::validate::IntegrityProblem;
use crate::canonical::compute_commit_id;
use crate::commit::{AuthorIdentity, Commit, CommitId, TreeRef};
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::merge::{ConflictResolution, ConflictedField, MergeOutcome, merge3, resolve_conflicts};
use crate::project_format::ProjectSnapshot;
use crate::project_info::ProjectInfo;
use crate::provider::{HeadAdvance, ProjectProvider};
use crate::sample_manifest::{SampleEntry, SampleManifest, plan_assets};
use crate::sidecar::{Sidecar, Stash};

/// Per-mirror push result.
pub struct MirrorResult {
    pub provider_id: String,
    pub ok: bool,
    pub error: Option<String>,
}

/// Outcome of [`push_with_freshness_check`].
pub enum PushOutcome {
    /// Commit landed on primary. Mirror results are best-effort; failures
    /// are already recorded in sidecar pending_pushes for later drain.
    Committed {
        commit_id: CommitId,
        was_merge: bool,
        mirror_results: Vec<MirrorResult>,
    },
    /// The remote advanced and the auto-merge found conflicts the user must
    /// resolve. `base` has all disjoint changes already applied.
    NeedsResolution {
        base: Value,
        conflicts: Vec<ConflictedField>,
    },
    /// The merge produced no field conflicts, but the result is not a project
    /// we are willing to hand back.
    ///
    /// This is the case a format-agnostic merge cannot see: both sides made
    /// edits that are individually fine and jointly incoherent — most often
    /// two people allocating the same modulation identity from Live's counter.
    /// Nothing has been committed. The user's own work is safe in
    /// [`crate::sidecar::Stash`] and reachable via [`stashed_snapshot`].
    NeedsReview {
        merged: Value,
        problems: Vec<IntegrityProblem>,
        stash: ContentHash,
    },
}

/// Caller-controlled project bundling behavior for one push.
#[derive(Clone, Debug, Default)]
pub struct PushOptions {
    pub bundle_policy: BundlePolicy,
    pub conflict_resolutions: Option<Vec<ConflictResolution>>,
    /// Opaque resources detached from a version-two project snapshot.
    ///
    /// Prefer [`Self::for_snapshot`] so hashes and bytes stay paired.
    snapshot_resources: Arc<BTreeMap<String, Vec<u8>>>,
}

impl PushOptions {
    /// Start push options with every detached resource held by `snapshot`.
    pub fn for_snapshot(snapshot: &ProjectSnapshot) -> Self {
        Self {
            snapshot_resources: snapshot.detached_resources_handle(),
            ..Self::default()
        }
    }
}

/// Re-read a committed backup through its provider and validate every object
/// needed to restore it.
pub async fn verify_commit_copy(provider: &dyn ProjectProvider, commit_id: CommitId) -> Result<()> {
    let commit = provider
        .get_commit(&commit_id)
        .await
        .map_err(|error| Error::Other(format!("re-read commit: {error}")))?;
    if commit.id != commit_id {
        return Err(Error::Other(format!(
            "re-read commit id mismatch: requested {}, received {}",
            commit_id.0, commit.id.0
        )));
    }
    let computed_id = compute_commit_id(&commit)
        .map_err(|error| Error::Other(format!("validate commit encoding: {error}")))?;
    if computed_id != commit_id {
        return Err(Error::Other(format!(
            "re-read commit is damaged: expected {}, computed {}",
            commit_id.0, computed_id.0
        )));
    }

    let snapshot_bytes = verify_blob(provider, commit.tree.snapshot, "snapshot").await?;
    let snapshot = ProjectSnapshot::from_canonical_bytes(&snapshot_bytes)
        .map_err(|error| Error::Other(format!("re-read snapshot is invalid: {error}")))?;

    let manifest_bytes = verify_blob(provider, commit.tree.samples, "asset manifest").await?;
    let manifest: SampleManifest = serde_json::from_slice(&manifest_bytes)
        .map_err(|error| Error::Other(format!("re-read asset manifest is invalid: {error}")))?;
    verify_snapshot_manifest(&snapshot, &snapshot_bytes, &manifest)?;

    if let Some(metadata) = commit.metadata {
        verify_blob(provider, metadata, "project metadata").await?;
    }

    for entry in manifest.entries {
        let label = format!("asset `{}`", entry.path);
        let bytes = verify_blob(provider, entry.hash, &label).await?;
        let actual_size = bytes.len() as u64;
        if actual_size != entry.size {
            return Err(Error::Other(format!(
                "re-read {label} has the wrong size: expected {} bytes, received {actual_size}",
                entry.size
            )));
        }
    }

    Ok(())
}

fn verify_snapshot_manifest(
    snapshot: &ProjectSnapshot,
    snapshot_bytes: &[u8],
    manifest: &SampleManifest,
) -> Result<()> {
    if snapshot.format() != crate::project_format::ProjectFormat::Dawproject
        || !crate::dawproject::requires_resource_hydration(snapshot)?
    {
        return Ok(());
    }

    let value: Value = serde_json::from_slice(snapshot_bytes)?;
    for resource in crate::dawproject::snapshot_resources_from_value(&value)? {
        let Some(entry) = manifest
            .entries
            .iter()
            .find(|entry| entry.path == resource.path)
        else {
            return Err(Error::Other(format!(
                "re-read asset manifest is missing DAWproject resource '{}'",
                resource.path
            )));
        };
        if entry.hash != resource.hash || entry.size != resource.size {
            return Err(Error::Other(format!(
                "re-read asset manifest does not match DAWproject resource '{}'",
                resource.path
            )));
        }
    }
    Ok(())
}

async fn verify_blob(
    provider: &dyn ProjectProvider,
    expected: ContentHash,
    label: &str,
) -> Result<Vec<u8>> {
    let bytes = provider
        .get_blob(&expected)
        .await
        .map_err(|error| Error::Other(format!("re-read {label}: {error}")))?;
    let actual = ContentHash::of(&bytes);
    if actual != expected {
        return Err(Error::Other(format!(
            "re-read {label} is damaged: expected {expected}, computed {actual}"
        )));
    }
    Ok(bytes)
}

/// Directory relative references resolve against.
///
/// The sidecar lives beside the project file — `sidecar_path_for` appends
/// `-pm.json` to the whole filename — so its parent is the project directory,
/// and for an Ableton project that directory is the project folder. Deriving
/// it here rather than taking it as an argument keeps the push API unchanged.
fn project_root_for(sidecar_path: &Path) -> Option<&Path> {
    sidecar_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
}

/// Store the pre-merge working state and record it in the sidecar.
///
/// The blob is very often already in the CAS — these are the same bytes the
/// push is about to store — so this usually costs a hash and a small sidecar
/// write. Failing to stash is not fatal to the push, but it is reported,
/// because proceeding without a way back is worth knowing about.
async fn stash_snapshot(
    primary: &dyn ProjectProvider,
    sidecar_path: &Path,
    snapshot_bytes: &[u8],
    snapshot_resources: &BTreeMap<String, Vec<u8>>,
    base: Option<CommitId>,
    reason: &str,
) -> Result<ContentHash, String> {
    let snapshot = ProjectSnapshot::from_canonical_bytes(snapshot_bytes)
        .map_err(|error| format!("validate local snapshot before stashing: {error}"))?;
    let value: Value = serde_json::from_slice(snapshot.as_bytes())
        .map_err(|error| format!("read local snapshot before stashing: {error}"))?;
    let mut resources = BTreeMap::new();
    if snapshot.format() == crate::project_format::ProjectFormat::Dawproject
        && crate::dawproject::requires_resource_hydration(&snapshot)
            .map_err(|error| format!("inspect local snapshot before stashing: {error}"))?
    {
        for resource in crate::dawproject::snapshot_resources_from_value(&value)
            .map_err(|error| format!("read local DAWproject resources before stashing: {error}"))?
        {
            let bytes = snapshot_resources.get(&resource.path).ok_or_else(|| {
                format!(
                    "stash local DAWproject resource '{}': bytes are unavailable; build PushOptions with PushOptions::for_snapshot",
                    resource.path
                )
            })?;
            if ContentHash::of(bytes) != resource.hash || bytes.len() as u64 != resource.size {
                return Err(format!(
                    "stash local DAWproject resource '{}': bytes do not match the snapshot hash and size",
                    resource.path
                ));
            }
            primary
                .put_blob(&resource.hash, bytes)
                .await
                .map_err(|error| {
                    format!(
                        "stash local DAWproject resource '{}': {error}",
                        resource.path
                    )
                })?;
            resources.insert(resource.path, resource.hash);
        }
    }

    let hash = ContentHash::of(snapshot_bytes);
    primary
        .put_blob(&hash, snapshot_bytes)
        .await
        .map_err(|e| format!("stash local snapshot: {e}"))?;
    Sidecar::modify(sidecar_path, |sidecar| {
        sidecar.stash = Some(Stash {
            snapshot: hash,
            resources,
            created_at: now_epoch_secs(),
            base,
            reason: reason.to_owned(),
        });
    })
    .map_err(|e| format!("record stash in sidecar: {e}"))?;
    Ok(hash)
}

/// Fetch the snapshot held in the sidecar's stash, if there is one.
///
/// Lets a caller offer "put my version back" after a merge the user does not
/// want to keep. A version-two DAWproject stash is returned as a self-contained
/// version-one snapshot so the existing restore API can rebuild the complete
/// archive without separate hydration. Returns `Ok(None)` when nothing is
/// stashed.
pub async fn stashed_snapshot(
    provider: &dyn ProjectProvider,
    sidecar_path: &Path,
) -> Result<Option<Vec<u8>>, String> {
    let sidecar = Sidecar::load(sidecar_path).map_err(|e| format!("load sidecar: {e}"))?;
    let Some(stash) = sidecar.stash else {
        return Ok(None);
    };
    let snapshot_bytes = provider
        .get_blob(&stash.snapshot)
        .await
        .map_err(|e| format!("fetch stashed snapshot: {e}"))?;
    if stash.resources.is_empty() {
        return Ok(Some(snapshot_bytes));
    }

    let stored = ProjectSnapshot::from_canonical_bytes(&snapshot_bytes)
        .map_err(|error| format!("validate stashed snapshot: {error}"))?;
    if stored.format() != crate::project_format::ProjectFormat::Dawproject {
        return Err("only a DAWproject stash may declare detached resources".to_owned());
    }
    let mut value: Value = serde_json::from_slice(&snapshot_bytes)
        .map_err(|error| format!("parse stashed snapshot: {error}"))?;
    let resources = value
        .get_mut("resources")
        .and_then(Value::as_array_mut)
        .ok_or_else(|| "stashed DAWproject snapshot is missing its resources".to_owned())?;
    for resource in resources {
        let object = resource
            .as_object_mut()
            .ok_or_else(|| "stashed DAWproject resource is not an object".to_owned())?;
        let path = object
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| "stashed DAWproject resource is missing its path".to_owned())?
            .to_owned();
        let expected = stash
            .resources
            .get(&path)
            .ok_or_else(|| format!("stashed DAWproject resource '{path}' has no protected blob"))?;
        let recorded_hash: ContentHash = serde_json::from_value(
            object
                .get("hash")
                .cloned()
                .ok_or_else(|| format!("stashed DAWproject resource '{path}' has no hash"))?,
        )
        .map_err(|error| format!("read stashed DAWproject resource hash for '{path}': {error}"))?;
        if recorded_hash != *expected {
            return Err(format!(
                "stashed DAWproject resource '{path}' does not match its protected hash"
            ));
        }
        let recorded_size = object
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| format!("stashed DAWproject resource '{path}' has no recorded size"))?;
        let bytes = provider
            .get_blob(expected)
            .await
            .map_err(|error| format!("fetch stashed DAWproject resource '{path}': {error}"))?;
        if ContentHash::of(&bytes) != *expected || bytes.len() as u64 != recorded_size {
            return Err(format!(
                "stashed DAWproject resource '{path}' does not match its protected hash and size"
            ));
        }
        object.insert(
            "data".to_owned(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        );
        object.remove("hash");
        object.remove("size");
    }
    value["auru_pm_snapshot"] = Value::from(1);
    let encoded =
        serde_json::to_vec(&value).map_err(|error| format!("encode stashed snapshot: {error}"))?;
    let recovered = ProjectSnapshot::from_canonical_bytes(&encoded)
        .map_err(|error| format!("validate stashed snapshot: {error}"))?;
    recovered
        .restore_bytes()
        .map_err(|error| format!("validate stashed DAWproject resources: {error}"))?;
    Ok(Some(recovered.into_bytes()))
}

/// Forget the stashed pre-merge state.
///
/// For when a person has looked at a merge and decided to keep it. A landed
/// commit clears the stash on its own; this is for discarding one explicitly.
/// The blob itself stays in the CAS until garbage collection runs.
pub fn discard_stash(sidecar_path: &Path) -> Result<(), String> {
    Sidecar::modify(sidecar_path, |sidecar| sidecar.stash = None)
        .map(|_| ())
        .map_err(|e| format!("clear stash: {e}"))
}

/// Format-specific integrity problems in a merged snapshot.
///
/// Only Ableton Live Sets are checked today — they are the format whose
/// internal identity allocation a structural merge can silently corrupt. Every
/// other format returns empty, so nothing else changes behaviour.
fn integrity_problems(merged: &Value) -> Vec<IntegrityProblem> {
    crate::ableton::validate_snapshot_value(merged)
}

/// Put every DAWproject merge input into the v2 resource representation.
///
/// A merge can span commits written before and after resources were detached
/// from canonical JSON. Hashing legacy inline bytes before `merge3` prevents a
/// chosen v1 `data` field from landing beneath a v2 schema marker. Persisting
/// those bytes first also guarantees that whichever descriptor wins can be
/// resolved by `build_sample_manifest`.
async fn normalize_dawproject_for_merge(
    primary: &dyn ProjectProvider,
    snapshot: &mut Value,
) -> Result<(), String> {
    if crate::dawproject::snapshot_parts_from_value(snapshot).is_none() {
        return Ok(());
    }

    let parsed = crate::dawproject::snapshot_resources_from_value(snapshot)
        .map_err(|error| format!("read DAWproject merge resources: {error}"))?;
    let mut legacy_blobs = Vec::new();
    if !parsed.is_empty() {
        let resources = snapshot
            .get_mut("resources")
            .and_then(Value::as_array_mut)
            .ok_or_else(|| "DAWproject merge snapshot is missing its resources".to_owned())?;
        if resources.len() != parsed.len() {
            return Err("DAWproject merge resource count changed while normalizing".to_owned());
        }
        for (resource, parsed) in resources.iter_mut().zip(parsed) {
            let object = resource
                .as_object_mut()
                .ok_or_else(|| "DAWproject merge resource is not an object".to_owned())?;
            if object.get("id").and_then(Value::as_str) != Some(parsed.path.as_str()) {
                return Err("DAWproject merge resource order changed while normalizing".to_owned());
            }
            let Some(bytes) = parsed.inline_data else {
                continue;
            };
            let hash = ContentHash::of(&bytes);
            object.insert(
                "hash".to_owned(),
                serde_json::to_value(hash)
                    .map_err(|error| format!("encode DAWproject resource hash: {error}"))?,
            );
            object.insert("size".to_owned(), Value::from(bytes.len() as u64));
            object.remove("data");
            legacy_blobs.push((parsed.path, hash, bytes));
        }
    }
    snapshot["auru_pm_snapshot"] = Value::from(2);
    let normalized = serde_json::to_vec(snapshot)
        .map_err(|error| format!("encode normalized DAWproject merge snapshot: {error}"))?;
    ProjectSnapshot::from_canonical_bytes(&normalized)
        .map_err(|error| format!("validate normalized DAWproject merge snapshot: {error}"))?;

    for (path, hash, bytes) in legacy_blobs {
        primary
            .put_blob(&hash, &bytes)
            .await
            .map_err(|error| format!("store legacy DAWproject resource '{path}': {error}"))?;
    }
    Ok(())
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read every asset the project depends on from disk, store the bytes in the
/// CAS, and assemble the [`SampleManifest`] for the commit.
///
/// What counts as an asset depends on the project:
///
/// - Native Auru snapshots contribute the audio referenced by their clips.
/// - An Ableton Live Set inside a project folder contributes the whole folder
///   *plus* every file it references from outside — the sample library loop,
///   the User Library rack — gathered in so the commit restores to a project
///   that actually opens elsewhere. See [`crate::ableton::assets`].
/// - A loose `.als` with no project folder, and DAWproject, contribute
///   no filesystem assets: the first has no folder to walk, while the second's
///   embedded media is decoded directly from the canonical archive wrapper.
///
/// `project_root` is where relative references resolve from. It is derived
/// from the sidecar's location, which sits beside the project file.
///
/// Best-effort on missing files: an asset that can't be read is logged and
/// skipped rather than failing the whole commit. This keeps commits working
/// when a project references a sample that has since moved, and when a 3-way
/// merge pulls in a remote clip whose sample isn't on local disk (its blob
/// already lives in the CAS from the remote's own push).
///
/// Returns the manifest plus the `(hash, bytes)` of every asset actually
/// stored, so the mirror fan-out can replay the same blobs without touching
/// disk again.
async fn build_sample_manifest(
    primary: &dyn ProjectProvider,
    snapshot: &Value,
    project_root: Option<&Path>,
    policy: &BundlePolicy,
    detached_resources: &BTreeMap<String, Vec<u8>>,
) -> Result<(SampleManifest, Vec<(ContentHash, Vec<u8>)>), String> {
    let mut manifest = SampleManifest::new();
    let mut blobs: Vec<(ContentHash, Vec<u8>)> = Vec::new();

    for asset in plan_assets(snapshot, project_root, policy) {
        let bytes = match std::fs::read(&asset.source) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!(
                    "[pm] skipping unreadable asset '{}': {e}",
                    asset.source.display()
                );
                continue;
            }
        };
        let hash = ContentHash::of(&bytes);
        primary
            .put_blob(&hash, &bytes)
            .await
            .map_err(|e| format!("put asset blob '{}': {e}", asset.bundle_path))?;
        manifest.insert(SampleEntry {
            path: asset.bundle_path,
            hash,
            size: bytes.len() as u64,
            kind: asset.kind,
            origin: asset.origin,
        });
        blobs.push((hash, bytes));
    }

    // DAWproject v2 keeps opaque archive entries out of canonical JSON.
    // Locally-created resources arrive in `detached_resources`; resources
    // selected from a remote merge are already in the provider CAS.
    for resource in crate::dawproject::snapshot_resources_from_value(snapshot)
        .map_err(|error| format!("read DAWproject resources: {error}"))?
    {
        let matching_candidate = detached_resources
            .get(&resource.path)
            .filter(|bytes| {
                ContentHash::of(bytes) == resource.hash && bytes.len() as u64 == resource.size
            })
            .cloned()
            .or_else(|| {
                resource.inline_data.filter(|bytes| {
                    ContentHash::of(bytes) == resource.hash && bytes.len() as u64 == resource.size
                })
            });
        if let Some(bytes) = matching_candidate {
            primary
                .put_blob(&resource.hash, &bytes)
                .await
                .map_err(|e| format!("put embedded asset blob '{}': {e}", resource.path))?;
            blobs.push((resource.hash, bytes));
        } else {
            let bytes = primary
                .get_blob(&resource.hash)
                .await
                .map_err(|error| {
                    format!(
                        "DAWproject resource '{}' is detached from its snapshot and could not be read from the provider ({error}); build PushOptions with PushOptions::for_snapshot for a newly loaded project",
                        resource.path
                    )
                })?;
            let actual_hash = ContentHash::of(&bytes);
            if actual_hash != resource.hash || bytes.len() as u64 != resource.size {
                return Err(format!(
                    "provider copy of DAWproject resource '{}' does not match its snapshot hash and size",
                    resource.path
                ));
            }
            // Mirror fan-out needs every final resource, including one chosen
            // from the remote side of a merge that this machine never loaded.
            blobs.push((resource.hash, bytes));
        }
        manifest.insert(SampleEntry {
            path: resource.path,
            hash: resource.hash,
            size: resource.size,
            kind: resource.kind,
            origin: None,
        });
    }

    Ok((manifest, blobs))
}

/// Derive and store the commit's project summary, returning its blob hash.
///
/// `Ok(None)` when the project has no summary to give. A failure to *derive*
/// one is never fatal — a project we cannot fully read is still worth backing
/// up, and the client falls back to the snapshot. A failure to *store* one is
/// reported, because it would leave the commit pointing at a blob that is not
/// there.
async fn store_project_info(
    primary: &dyn ProjectProvider,
    snapshot_bytes: &[u8],
) -> Result<Option<ContentHash>, String> {
    let Ok(snapshot) = serde_json::from_slice::<Value>(snapshot_bytes) else {
        return Ok(None);
    };
    let Some(info) = ProjectInfo::from_snapshot(&snapshot) else {
        return Ok(None);
    };
    let Ok(bytes) = info.canonical_encoding() else {
        return Ok(None);
    };
    let hash = ContentHash::of(&bytes);
    primary
        .put_blob(&hash, &bytes)
        .await
        .map_err(|e| format!("put project info blob: {e}"))?;
    Ok(Some(hash))
}

/// Fetch the project summary a commit points at.
///
/// `Ok(None)` when the commit has no summary — a format this crate does not
/// summarize, or a commit written before summaries existed — in which case the
/// caller should read the snapshot instead. A summary written by a newer build
/// than this one is also reported as absent rather than half-understood.
pub async fn fetch_project_info(
    provider: &dyn ProjectProvider,
    commit: &Commit,
) -> Result<Option<ProjectInfo>, String> {
    let Some(hash) = commit.metadata else {
        return Ok(None);
    };
    let bytes = provider
        .get_blob(&hash)
        .await
        .map_err(|e| format!("fetch project info: {e}"))?;
    let info: ProjectInfo =
        serde_json::from_slice(&bytes).map_err(|e| format!("parse project info: {e}"))?;
    Ok(info.is_readable().then_some(info))
}

/// Build a Commit, compute its id, push blobs + commit to `provider`, and
/// advance HEAD. Returns `Err` if the CAS advance races (caller should retry).
///
/// The sample manifest is built + its data blobs stored by the caller via
/// [`build_sample_manifest`]; here we only persist the manifest blob itself.
// The arguments mirror commit fields and provider state; grouping them would
// obscure this internal one-shot construction without simplifying callers.
#[expect(clippy::too_many_arguments)]
async fn write_commit_to_primary(
    primary: &dyn ProjectProvider,
    snapshot_bytes: &[u8],
    manifest: &SampleManifest,
    parents: Vec<CommitId>,
    author: AuthorIdentity,
    message: &str,
    description: &str,
    from_head: Option<CommitId>,
) -> Result<Commit, String> {
    // Snapshot blob.
    let snapshot_hash = crate::hash::ContentHash::of(snapshot_bytes);
    primary
        .put_blob(&snapshot_hash, snapshot_bytes)
        .await
        .map_err(|e| format!("put snapshot blob: {e}"))?;

    // Sample manifest blob (data blobs already stored by the caller).
    let manifest_bytes = manifest
        .canonical_encoding()
        .map_err(|e| format!("encode sample manifest: {e}"))?;
    let samples_hash = crate::hash::ContentHash::of(&manifest_bytes);
    primary
        .put_blob(&samples_hash, &manifest_bytes)
        .await
        .map_err(|e| format!("put samples blob: {e}"))?;

    let tree = TreeRef {
        snapshot: snapshot_hash,
        samples: samples_hash,
    };

    // Summary blob: what this version *is*, small enough for a client to fetch
    // per project instead of pulling a multi-megabyte snapshot to show a
    // tempo. Absent for formats whose detail this crate does not read, which
    // keeps their commits byte-identical to before summaries existed.
    let metadata = store_project_info(primary, snapshot_bytes).await?;

    // Placeholder id so we can call compute_commit_id.
    let placeholder_id = CommitId(crate::hash::ContentHash::ZERO);
    let mut commit = Commit {
        id: placeholder_id,
        parents,
        tree,
        author,
        timestamp: now_epoch_secs(),
        message: message.to_owned(),
        description: description.to_owned(),
        auru_version: env!("CARGO_PKG_VERSION").to_owned(),
        format_version: snapshot_format_version(snapshot_bytes),
        metadata,
    };

    let real_id = compute_commit_id(&commit).map_err(|e| format!("compute commit id: {e}"))?;
    commit.id = real_id;

    primary
        .put_commit(&commit)
        .await
        .map_err(|e| format!("put commit: {e}"))?;

    match primary
        .advance_head(from_head, commit.id)
        .await
        .map_err(|e| format!("advance head: {e}"))?
    {
        HeadAdvance::Advanced => Ok(commit),
        HeadAdvance::Conflict { .. } => Err(
            "HEAD conflict: remote moved again while we were preparing; please retry".to_owned(),
        ),
    }
}

fn snapshot_format_version(snapshot_bytes: &[u8]) -> u32 {
    serde_json::from_slice::<Value>(snapshot_bytes)
        .ok()
        .and_then(|snapshot| {
            snapshot
                .get("version")
                .or_else(|| snapshot.get("format_version"))
                .or_else(|| snapshot.get("auru_pm_snapshot"))
                .and_then(Value::as_u64)
        })
        .and_then(|version| u32::try_from(version).ok())
        .unwrap_or(8)
}

/// Push snapshot + commit blobs to a mirror provider and try to advance its
/// HEAD. Mirror HEAD advance failures are tolerated — mirrors are eventually
/// consistent.
async fn push_to_mirror(
    mirror: &dyn ProjectProvider,
    commit: &Commit,
    snapshot_bytes: &[u8],
    manifest_bytes: &[u8],
    sample_blobs: &[(ContentHash, Vec<u8>)],
) -> Result<(), String> {
    mirror
        .put_blob(&commit.tree.snapshot, snapshot_bytes)
        .await
        .map_err(|e| format!("mirror put snapshot: {e}"))?;
    // Sample data blobs the manifest references — push before the manifest so
    // a mirror is never left advertising a sample it can't serve.
    for (hash, bytes) in sample_blobs {
        mirror
            .put_blob(hash, bytes)
            .await
            .map_err(|e| format!("mirror put sample: {e}"))?;
    }
    mirror
        .put_blob(&commit.tree.samples, manifest_bytes)
        .await
        .map_err(|e| format!("mirror put samples: {e}"))?;
    mirror
        .put_commit(commit)
        .await
        .map_err(|e| format!("mirror put commit: {e}"))?;
    // Best-effort HEAD advance; we don't CAS-loop on mirrors.
    let _ = mirror.advance_head(None, commit.id).await;
    Ok(())
}

/// Push the local snapshot to the primary provider, auto-merging if the
/// remote has advanced since our last pull.
///
/// Returns `PushOutcome::NeedsResolution` when the merge cannot be
/// completed automatically. Returns `Err(String)` only for hard failures
/// (I/O, serialization, repeated HEAD races).
///
/// This legacy byte-only entry point is suitable for native Auru, Ableton,
/// FL Studio, and self-contained DAWproject v1 snapshots. Newly loaded
/// DAWproject v2 snapshots detach their archive resources; push those through
/// [`push_with_options`] with [`PushOptions::for_snapshot`].
// This is a stable public coordinator API whose parameters represent distinct
// caller-owned resources. New optional behavior belongs in `PushOptions`.
#[expect(clippy::too_many_arguments)]
pub async fn push_with_freshness_check(
    primary: &dyn ProjectProvider,
    primary_id: &str,
    mirrors: &[(String, Arc<dyn ProjectProvider>)],
    sidecar_path: &Path,
    snapshot_bytes: &[u8],
    author: AuthorIdentity,
    message: &str,
    description: &str,
) -> Result<PushOutcome, String> {
    let options = PushOptions::default();
    push_with_options(
        primary,
        primary_id,
        mirrors,
        sidecar_path,
        snapshot_bytes,
        author,
        message,
        description,
        &options,
    )
    .await
}

/// Push with caller-controlled project bundling behavior.
#[expect(clippy::too_many_arguments)]
pub async fn push_with_options(
    primary: &dyn ProjectProvider,
    primary_id: &str,
    mirrors: &[(String, Arc<dyn ProjectProvider>)],
    sidecar_path: &Path,
    snapshot_bytes: &[u8],
    author: AuthorIdentity,
    message: &str,
    description: &str,
    options: &PushOptions,
) -> Result<PushOutcome, String> {
    push_with_optional_resolutions(
        primary,
        primary_id,
        mirrors,
        sidecar_path,
        snapshot_bytes,
        author,
        message,
        description,
        options.conflict_resolutions.as_deref(),
        &options.bundle_policy,
        &options.snapshot_resources,
    )
    .await
}

/// Retry a freshness-checked push with an explicit choice for every field
/// reported by the current three-way merge.
///
/// The provider head and conflict set are recomputed before applying choices,
/// so a remote change between displaying and confirming the resolver cannot
/// silently commit against stale data.
///
/// This legacy byte-only retry has the same DAWproject v2 limitation as
/// [`push_with_freshness_check`]. Set `conflict_resolutions` on
/// [`PushOptions`] created with [`PushOptions::for_snapshot`] and call
/// [`push_with_options`] when the snapshot has detached resources.
// This extends the stable coordinator API with explicit resolutions; see
// `push_with_freshness_check` for why these inputs remain separate.
#[expect(clippy::too_many_arguments)]
pub async fn push_with_conflict_resolutions(
    primary: &dyn ProjectProvider,
    primary_id: &str,
    mirrors: &[(String, Arc<dyn ProjectProvider>)],
    sidecar_path: &Path,
    snapshot_bytes: &[u8],
    author: AuthorIdentity,
    message: &str,
    description: &str,
    resolutions: &[ConflictResolution],
) -> Result<PushOutcome, String> {
    let options = PushOptions {
        conflict_resolutions: Some(resolutions.to_vec()),
        ..PushOptions::default()
    };
    push_with_options(
        primary,
        primary_id,
        mirrors,
        sidecar_path,
        snapshot_bytes,
        author,
        message,
        description,
        &options,
    )
    .await
}

// Private common implementation for the two intentionally wide public APIs.
#[expect(clippy::too_many_arguments)]
async fn push_with_optional_resolutions(
    primary: &dyn ProjectProvider,
    primary_id: &str,
    mirrors: &[(String, Arc<dyn ProjectProvider>)],
    sidecar_path: &Path,
    snapshot_bytes: &[u8],
    author: AuthorIdentity,
    message: &str,
    description: &str,
    resolutions: Option<&[ConflictResolution]>,
    bundle_policy: &BundlePolicy,
    snapshot_resources: &BTreeMap<String, Vec<u8>>,
) -> Result<PushOutcome, String> {
    let remote_head = primary
        .get_head()
        .await
        .map_err(|e| format!("get remote head: {e}"))?;

    let sidecar = Sidecar::load(sidecar_path).map_err(|e| format!("load sidecar: {e}"))?;
    let local_head = sidecar.local_head;

    // Resolve what snapshot to push, the parent list, and whether this is a
    // merge commit.
    let (final_snapshot_bytes, parents, was_merge, from_head) = if remote_head == local_head {
        // Fast-forward: no merge needed.
        let parents = local_head.map(|id| vec![id]).unwrap_or_default();
        (snapshot_bytes.to_vec(), parents, false, local_head)
    } else {
        match local_head {
            None => {
                // We have no history — push directly as a root commit.
                (snapshot_bytes.to_vec(), vec![], false, None)
            }
            Some(local_id) => {
                let remote_id = remote_head.ok_or("remote HEAD is None but differs from local")?;

                // The remote moved, so local work is about to be reconciled
                // with someone else's. Put the pre-merge state somewhere
                // recoverable first — this is the only operation here that can
                // leave a person worse off than before they started.
                stash_snapshot(
                    primary,
                    sidecar_path,
                    snapshot_bytes,
                    snapshot_resources,
                    local_head,
                    "before merging with the latest version",
                )
                .await?;

                // Fetch ancestor (our last known common point).
                let ancestor_commit = primary
                    .get_commit(&local_id)
                    .await
                    .map_err(|e| format!("get ancestor commit: {e}"))?;
                let ancestor_bytes = primary
                    .get_blob(&ancestor_commit.tree.snapshot)
                    .await
                    .map_err(|e| format!("get ancestor snapshot: {e}"))?;

                // Fetch remote tip.
                let remote_commit = primary
                    .get_commit(&remote_id)
                    .await
                    .map_err(|e| format!("get remote commit: {e}"))?;
                let remote_bytes = primary
                    .get_blob(&remote_commit.tree.snapshot)
                    .await
                    .map_err(|e| format!("get remote snapshot: {e}"))?;

                // Parse all three states.
                let mut ancestor_json: Value = serde_json::from_slice(&ancestor_bytes)
                    .map_err(|e| format!("parse ancestor snapshot: {e}"))?;
                let mut current_local: Value = serde_json::from_slice(snapshot_bytes)
                    .map_err(|e| format!("parse local snapshot: {e}"))?;
                let mut remote_json: Value = serde_json::from_slice(&remote_bytes)
                    .map_err(|e| format!("parse remote snapshot: {e}"))?;
                normalize_dawproject_for_merge(primary, &mut ancestor_json).await?;
                normalize_dawproject_for_merge(primary, &mut current_local).await?;
                normalize_dawproject_for_merge(primary, &mut remote_json).await?;

                match merge3(&ancestor_json, &current_local, &remote_json) {
                    MergeOutcome::Conflict { base, conflicts } => {
                        let Some(resolutions) = resolutions else {
                            return Ok(PushOutcome::NeedsResolution { base, conflicts });
                        };
                        if !conflict_resolutions_match(&conflicts, resolutions) {
                            return Ok(PushOutcome::NeedsResolution { base, conflicts });
                        }
                        let choices = conflicts
                            .iter()
                            .zip(resolutions)
                            .map(|(_, resolution)| resolution.choice)
                            .collect::<Vec<_>>();
                        let merged = resolve_conflicts(base, &conflicts, &choices)?;
                        // Resolving the fields a person was asked about can
                        // still leave the set incoherent overall.
                        let problems = integrity_problems(&merged);
                        if !problems.is_empty() {
                            let stash = ContentHash::of(snapshot_bytes);
                            return Ok(PushOutcome::NeedsReview {
                                merged,
                                problems,
                                stash,
                            });
                        }
                        let merged_bytes = serde_json::to_vec(&merged)
                            .map_err(|error| format!("serialize resolved snapshot: {error}"))?;
                        let parents = vec![local_id, remote_id];
                        (merged_bytes, parents, true, Some(remote_id))
                    }
                    MergeOutcome::Clean { merged } => {
                        // "No field conflicts" is not the same as "a project
                        // worth handing back". Check the merged result before
                        // calling it clean.
                        let problems = integrity_problems(&merged);
                        if !problems.is_empty() {
                            let stash = ContentHash::of(snapshot_bytes);
                            return Ok(PushOutcome::NeedsReview {
                                merged,
                                problems,
                                stash,
                            });
                        }
                        let merged_bytes = serde_json::to_vec(&merged)
                            .map_err(|e| format!("serialize merged snapshot: {e}"))?;
                        // Merge commit has two parents: local then remote.
                        let parents = vec![local_id, remote_id];
                        (merged_bytes, parents, true, Some(remote_id))
                    }
                }
            }
        }
    };

    // Resolve the asset set from the final snapshot, store each asset's bytes
    // in the CAS, and build the manifest that the commit will point at.
    let final_snapshot: Value = serde_json::from_slice(&final_snapshot_bytes)
        .map_err(|e| format!("parse final snapshot: {e}"))?;
    ProjectSnapshot::from_canonical_bytes(&final_snapshot_bytes)
        .map_err(|error| format!("validate final snapshot: {error}"))?;
    let (manifest, sample_blobs) = build_sample_manifest(
        primary,
        &final_snapshot,
        project_root_for(sidecar_path),
        bundle_policy,
        snapshot_resources,
    )
    .await?;

    // Write to primary (may return Err on CAS race).
    let commit = write_commit_to_primary(
        primary,
        &final_snapshot_bytes,
        &manifest,
        parents,
        author,
        message,
        description,
        from_head,
    )
    .await?;

    let commit_id = commit.id;

    // Manifest bytes for mirror pushes — same manifest the commit points at.
    let manifest_bytes = manifest
        .canonical_encoding()
        .map_err(|e| format!("encode mirror manifest: {e}"))?;

    // Fan-out to mirrors — collect results, record failures in pending_pushes.
    let mut mirror_results: Vec<MirrorResult> = Vec::with_capacity(mirrors.len());
    let mut failed_mirror_ids: Vec<String> = Vec::new();

    for (mirror_id, mirror_provider) in mirrors {
        let result = push_to_mirror(
            mirror_provider.as_ref(),
            &commit,
            &final_snapshot_bytes,
            &manifest_bytes,
            &sample_blobs,
        )
        .await;

        match result {
            Ok(()) => mirror_results.push(MirrorResult {
                provider_id: mirror_id.clone(),
                ok: true,
                error: None,
            }),
            Err(e) => {
                failed_mirror_ids.push(mirror_id.clone());
                mirror_results.push(MirrorResult {
                    provider_id: mirror_id.clone(),
                    ok: false,
                    error: Some(e),
                });
            }
        }
    }

    // Update sidecar: advance local_head, record remote state, queue failures.
    Sidecar::modify(sidecar_path, |s| {
        // The successful destination becomes the source of truth for history
        // and restore on this machine. Recording it here keeps every caller
        // honest; a UI should not have to duplicate coordinator bookkeeping.
        s.primary = Some(primary_id.to_owned());
        s.local_head = Some(commit_id);
        let state = s.remotes.entry(primary_id.to_owned()).or_default();
        state.remote_head = Some(commit_id);
        state.last_pulled = Some(now_epoch_secs());
        // Enqueue commit for any mirror that failed.
        if !failed_mirror_ids.is_empty() && !s.pending_pushes.contains(&commit_id) {
            s.pending_pushes.push(commit_id);
        }
        // The work is committed and reachable through history now, so the
        // pre-merge copy has nothing left to protect.
        s.stash = None;
    })
    .map_err(|e| format!("save sidecar: {e}"))?;

    Ok(PushOutcome::Committed {
        commit_id,
        was_merge,
        mirror_results,
    })
}

fn conflict_resolutions_match(
    current: &[ConflictedField],
    resolutions: &[ConflictResolution],
) -> bool {
    current.len() == resolutions.len()
        && current
            .iter()
            .zip(resolutions)
            .all(|(current, resolution)| current == &resolution.conflict)
}

/// Retry pending pushes to all listed providers.
///
/// Loads the sidecar, iterates `pending_pushes`, tries to fetch each commit
/// from the first reachable provider and push it to every provider in
/// `providers`. Removes successfully delivered entries from the pending list
/// and returns the count drained.
pub async fn drain_pending_pushes(
    providers: &BTreeMap<String, Arc<dyn ProjectProvider>>,
    sidecar_path: &Path,
) -> usize {
    let sidecar = match Sidecar::load(sidecar_path) {
        Ok(s) => s,
        Err(_) => return 0,
    };

    if sidecar.pending_pushes.is_empty() || providers.is_empty() {
        return 0;
    }

    let provider_list: Vec<(&String, &Arc<dyn ProjectProvider>)> = providers.iter().collect();
    let mut drained = 0;
    let mut still_pending: Vec<CommitId> = Vec::new();

    for commit_id in &sidecar.pending_pushes {
        // Try to fetch the commit + all its blobs from any available provider.
        let mut commit_opt: Option<Commit> = None;
        let mut snapshot_opt: Option<Vec<u8>> = None;
        let mut manifest_opt: Option<Vec<u8>> = None;
        let mut sample_blobs: Vec<(ContentHash, Vec<u8>)> = Vec::new();

        for (_pid, provider) in &provider_list {
            let Ok(commit) = provider.get_commit(commit_id).await else {
                continue;
            };
            let Ok(snap) = provider.get_blob(&commit.tree.snapshot).await else {
                continue;
            };
            let Ok(man) = provider.get_blob(&commit.tree.samples).await else {
                continue;
            };
            // Pull every sample blob the manifest references so we can replay
            // them to providers that are missing them. Bail to the next
            // provider if any is unavailable here.
            let manifest: SampleManifest = match serde_json::from_slice(&man) {
                Ok(m) => m,
                Err(_) => continue,
            };
            let mut blobs = Vec::with_capacity(manifest.entries.len());
            let mut complete = true;
            for entry in &manifest.entries {
                match provider.get_blob(&entry.hash).await {
                    Ok(bytes) => blobs.push((entry.hash, bytes)),
                    Err(_) => {
                        complete = false;
                        break;
                    }
                }
            }
            if !complete {
                continue;
            }
            commit_opt = Some(commit);
            snapshot_opt = Some(snap);
            manifest_opt = Some(man);
            sample_blobs = blobs;
            break;
        }

        let (commit, snapshot_bytes, manifest_bytes) =
            match (commit_opt, snapshot_opt, manifest_opt) {
                (Some(c), Some(s), Some(m)) => (c, s, m),
                _ => {
                    // Cannot fetch from any provider — keep pending.
                    still_pending.push(*commit_id);
                    continue;
                }
            };

        // Push to all providers that don't already have it. Failure on any
        // individual provider keeps the commit in the pending list.
        let mut all_ok = true;
        for (_pid, provider) in &provider_list {
            // Skip providers that already acknowledged this commit.
            if let Ok(present) = provider.has_blobs(&[commit.tree.snapshot]).await {
                if present.first().copied().unwrap_or(false) {
                    // Blobs already there; still try the commit + head advance.
                } else if provider
                    .put_blob(&commit.tree.snapshot, &snapshot_bytes)
                    .await
                    .is_err()
                {
                    all_ok = false;
                    continue;
                }
            }

            // Sample data blobs first, then the manifest that points at them.
            let mut sample_ok = true;
            for (hash, bytes) in &sample_blobs {
                if provider.put_blob(hash, bytes).await.is_err() {
                    sample_ok = false;
                    break;
                }
            }
            if !sample_ok {
                all_ok = false;
                continue;
            }

            if provider
                .put_blob(&commit.tree.samples, &manifest_bytes)
                .await
                .is_err()
            {
                all_ok = false;
                continue;
            }

            if provider.put_commit(&commit).await.is_err() {
                all_ok = false;
                continue;
            }

            // Best-effort HEAD advance — tolerate conflicts on mirrors.
            let _ = provider.advance_head(None, *commit_id).await;
        }

        if all_ok {
            drained += 1;
        } else {
            still_pending.push(*commit_id);
        }
    }

    // Persist updated pending list only when something changed.
    if drained > 0 {
        let _ = Sidecar::modify(sidecar_path, |s| {
            s.pending_pushes = still_pending.clone();
        });
    }

    drained
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;
    use crate::filesystem::FilesystemProvider;
    use crate::{ProjectFormat, ProjectSnapshot};
    use tempfile::TempDir;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn dawproject_with_resource(bytes: &[u8]) -> ProjectSnapshot {
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("project.xml", options).unwrap();
        archive
            .write_all(br#"<Project version="1.0"><Structure/></Project>"#)
            .unwrap();
        archive.start_file("audio/take.wav", options).unwrap();
        archive.write_all(bytes).unwrap();
        let source = archive.finish().unwrap().into_inner();
        ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &source).unwrap()
    }

    fn dawproject_v1_value(bytes: &[u8]) -> Value {
        let snapshot = dawproject_with_resource(bytes);
        let mut value: Value = serde_json::from_slice(snapshot.as_bytes()).unwrap();
        value["auru_pm_snapshot"] = Value::from(1);
        let resource = value["resources"][0].as_object_mut().unwrap();
        resource.insert(
            "data".into(),
            Value::String(base64::engine::general_purpose::STANDARD.encode(bytes)),
        );
        resource.remove("hash");
        resource.remove("size");
        value
    }

    #[test]
    fn explicit_verification_should_detect_a_missing_uploaded_snapshot() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();
        let provider_id = provider.provider_id();
        let sidecar = dir.path().join("song.dawproject-pm.json");
        let source = std::fs::read(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/interchange/oracle-midi.dawproject"),
        )
        .unwrap();
        let snapshot =
            ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &source).unwrap();
        let outcome = rt()
            .block_on(push_with_freshness_check(
                &provider,
                &provider_id,
                &[],
                &sidecar,
                snapshot.as_bytes(),
                AuthorIdentity {
                    display_name: "Jake".to_owned(),
                    provider_user_id: "jake".to_owned(),
                    provider_id: provider_id.clone(),
                    email: None,
                },
                "Backed up changes",
                "",
            ))
            .unwrap();
        let PushOutcome::Committed { commit_id, .. } = outcome else {
            panic!("backup should commit");
        };
        let commit = rt().block_on(provider.get_commit(&commit_id)).unwrap();
        std::fs::remove_file(provider.blobs_cas().path_for(&commit.tree.snapshot)).unwrap();

        let error = rt()
            .block_on(verify_commit_copy(&provider, commit_id))
            .expect_err("verification must re-read the uploaded snapshot");

        assert!(error.to_string().contains("snapshot"), "{error}");
        assert!(error.to_string().contains("not found"), "{error}");
    }

    #[test]
    fn manifest_captures_referenced_samples_into_cas() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();

        // Two real sample files on disk, referenced by two audio clips
        // (one of them twice — must collapse to a single manifest entry).
        let kick = dir.path().join("kick.wav");
        let snare = dir.path().join("snare.wav");
        std::fs::write(&kick, b"kick-bytes").unwrap();
        std::fs::write(&snare, b"snare-bytes-longer").unwrap();

        let snapshot = serde_json::json!({
            "channels": [
                { "clips": [
                    { "data": { "Audio": { "file_path": kick.to_str().unwrap() } } },
                    { "data": { "Audio": { "file_path": snare.to_str().unwrap() } } },
                ]},
                { "clips": [
                    { "data": { "Audio": { "file_path": kick.to_str().unwrap() } } },
                    { "data": { "Midi": { "notes": [] } } },
                ]},
            ]
        });

        let (manifest, blobs) = rt()
            .block_on(build_sample_manifest(
                &provider,
                &snapshot,
                None,
                &BundlePolicy::default(),
                &BTreeMap::new(),
            ))
            .unwrap();

        // Deduped to two entries, sorted by path.
        assert_eq!(manifest.entries.len(), 2);
        assert_eq!(blobs.len(), 2);

        // Each entry's hash + size matches the bytes, and the blob landed
        // in the CAS so it's fetchable by hash.
        for (file, bytes) in [
            (&kick, b"kick-bytes".to_vec()),
            (&snare, b"snare-bytes-longer".to_vec()),
        ] {
            let entry = manifest
                .entries
                .iter()
                .find(|e| e.path == file.to_str().unwrap())
                .expect("entry present");
            assert_eq!(entry.hash, ContentHash::of(&bytes));
            assert_eq!(entry.size, bytes.len() as u64);
            let stored = rt().block_on(provider.get_blob(&entry.hash)).unwrap();
            assert_eq!(stored, bytes);
        }
    }

    #[test]
    fn missing_sample_is_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();

        let present = dir.path().join("present.wav");
        std::fs::write(&present, b"here").unwrap();
        let absent = dir.path().join("gone.wav");

        let snapshot = serde_json::json!({
            "channels": [{ "clips": [
                { "data": { "Audio": { "file_path": present.to_str().unwrap() } } },
                { "data": { "Audio": { "file_path": absent.to_str().unwrap() } } },
            ]}]
        });

        let (manifest, blobs) = rt()
            .block_on(build_sample_manifest(
                &provider,
                &snapshot,
                None,
                &BundlePolicy::default(),
                &BTreeMap::new(),
            ))
            .unwrap();

        // The unreadable sample is dropped; the commit still gets a manifest
        // listing the sample that does exist.
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(blobs.len(), 1);
        assert_eq!(manifest.entries[0].path, present.to_str().unwrap());
    }

    #[test]
    fn dawproject_embedded_media_is_stored_as_an_individual_asset() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();
        let mut archive = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default();
        archive.start_file("project.xml", options).unwrap();
        archive
            .write_all(
                br#"<Project version="1.0">
                    <Application name="Test" version="1"/>
                    <Arrangement><Lanes><Clips>
                      <Clip time="0" duration="1">
                        <Audio channels="2" duration="1" sampleRate="48000">
                          <File path="audio/take.wav"/>
                        </Audio>
                      </Clip>
                    </Clips></Lanes></Arrangement>
                  </Project>"#,
            )
            .unwrap();
        archive.start_file("audio/take.wav", options).unwrap();
        archive.write_all(b"embedded-audio").unwrap();
        let source = archive.finish().unwrap().into_inner();
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &source)
            .expect("normalize DAWproject");
        let value = serde_json::from_slice(snapshot.as_bytes()).expect("canonical JSON");

        let (manifest, blobs) = rt()
            .block_on(build_sample_manifest(
                &provider,
                &value,
                None,
                &BundlePolicy::default(),
                &snapshot.detached_resources_handle(),
            ))
            .expect("manifest");

        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(manifest.entries[0].path, "audio/take.wav");
        assert_eq!(manifest.entries[0].hash, ContentHash::of(b"embedded-audio"));
        assert_eq!(
            blobs,
            vec![(
                ContentHash::of(b"embedded-audio"),
                b"embedded-audio".to_vec()
            )]
        );
        assert_eq!(
            rt().block_on(provider.get_blob(&manifest.entries[0].hash))
                .expect("stored asset"),
            b"embedded-audio"
        );
    }

    #[test]
    fn remote_dawproject_resource_should_win_when_the_local_path_has_other_bytes() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();
        let local = dawproject_with_resource(b"local version");
        let remote = dawproject_with_resource(b"remote version");
        let remote_value: Value = serde_json::from_slice(remote.as_bytes()).unwrap();
        let remote_resource =
            crate::dawproject::snapshot_resources_from_value(&remote_value).unwrap()[0].hash;
        rt().block_on(provider.put_blob(&remote_resource, b"remote version"))
            .unwrap();

        let (manifest, blobs) = rt()
            .block_on(build_sample_manifest(
                &provider,
                &remote_value,
                None,
                &BundlePolicy::default(),
                &local.detached_resources_handle(),
            ))
            .expect("remote CAS bytes should satisfy the final descriptor");

        assert_eq!(manifest.entries[0].hash, remote_resource);
        assert_eq!(blobs, vec![(remote_resource, b"remote version".to_vec())]);
    }

    #[test]
    fn v1_inline_resource_should_win_when_detached_bytes_at_its_path_do_not_match() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();
        let local = dawproject_with_resource(b"local version");
        let remote_value = dawproject_v1_value(b"remote inline version");

        let (manifest, blobs) = rt()
            .block_on(build_sample_manifest(
                &provider,
                &remote_value,
                None,
                &BundlePolicy::default(),
                &local.detached_resources_handle(),
            ))
            .expect("v1 inline bytes should remain a complete fallback");

        let expected_hash = ContentHash::of(b"remote inline version");
        assert_eq!(manifest.entries[0].hash, expected_hash);
        assert_eq!(
            blobs,
            vec![(expected_hash, b"remote inline version".to_vec())]
        );
    }

    #[test]
    fn mixed_v1_v2_resource_conflict_should_resolve_to_one_valid_v2_descriptor() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();
        let mut ancestor = dawproject_v1_value(b"ancestor version");
        let local_snapshot = dawproject_with_resource(b"local version");
        let mut local: Value = serde_json::from_slice(local_snapshot.as_bytes()).unwrap();
        let mut remote = dawproject_v1_value(b"remote version");

        rt().block_on(async {
            normalize_dawproject_for_merge(&provider, &mut ancestor)
                .await
                .unwrap();
            normalize_dawproject_for_merge(&provider, &mut local)
                .await
                .unwrap();
            normalize_dawproject_for_merge(&provider, &mut remote)
                .await
                .unwrap();
        });
        assert_eq!(ancestor["auru_pm_snapshot"], 2);
        assert_eq!(local["auru_pm_snapshot"], 2);
        assert_eq!(remote["auru_pm_snapshot"], 2);
        assert!(remote["resources"][0].get("data").is_none());

        let MergeOutcome::Conflict { base, conflicts } = merge3(&ancestor, &local, &remote) else {
            panic!("both sides changed the same resource");
        };
        let choices = vec![crate::merge::ConflictChoice::Remote; conflicts.len()];
        let resolved = resolve_conflicts(base, &conflicts, &choices).unwrap();
        let bytes = serde_json::to_vec(&resolved).unwrap();
        ProjectSnapshot::from_canonical_bytes(&bytes).expect("resolved v2 snapshot is valid");
        let (manifest, blobs) = rt()
            .block_on(build_sample_manifest(
                &provider,
                &resolved,
                None,
                &BundlePolicy::default(),
                &local_snapshot.detached_resources_handle(),
            ))
            .expect("remote legacy bytes were retained in CAS");

        let remote_hash = ContentHash::of(b"remote version");
        assert_eq!(manifest.entries[0].hash, remote_hash);
        assert_eq!(blobs, vec![(remote_hash, b"remote version".to_vec())]);
    }

    #[test]
    fn verification_should_require_every_detached_dawproject_resource_in_the_manifest() {
        let snapshot = dawproject_with_resource(b"provider asset");
        let fetched = ProjectSnapshot::from_canonical_bytes(snapshot.as_bytes()).unwrap();
        let error = verify_snapshot_manifest(&fetched, snapshot.as_bytes(), &SampleManifest::new())
            .expect_err("detached resource must be represented by the manifest");

        assert!(error.to_string().contains("audio/take.wav"), "{error}");
    }

    #[test]
    fn dawproject_stash_should_restore_detached_resources_after_reopening_provider() {
        let dir = TempDir::new().unwrap();
        let provider_path = dir.path().join("cas");
        let sidecar = dir.path().join("song.dawproject-pm.json");
        let snapshot = dawproject_with_resource(b"stashed audio");
        let provider = FilesystemProvider::open(&provider_path).unwrap();
        rt().block_on(stash_snapshot(
            &provider,
            &sidecar,
            snapshot.as_bytes(),
            &snapshot.detached_resources_handle(),
            None,
            "test recovery",
        ))
        .expect("stash");
        drop(provider);

        let reopened = FilesystemProvider::open(provider_path).unwrap();
        let recovered = rt()
            .block_on(stashed_snapshot(&reopened, &sidecar))
            .expect("read stash")
            .expect("stash exists");
        let recovered = ProjectSnapshot::from_canonical_bytes(&recovered).unwrap();
        let restored = recovered.restore_bytes().expect("restore complete archive");
        let mut archive = zip::ZipArchive::new(Cursor::new(restored)).unwrap();
        let mut resource = Vec::new();
        std::io::Read::read_to_end(
            &mut archive.by_name("audio/take.wav").unwrap(),
            &mut resource,
        )
        .unwrap();

        assert_eq!(resource, b"stashed audio");
        let restored = archive.into_inner().into_inner();
        let renormalized = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &restored)
            .expect("re-normalize restored stash");
        assert_eq!(renormalized.as_bytes(), snapshot.as_bytes());
    }

    #[test]
    fn resolved_push_commits_explicit_choices_against_current_remote_head() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path().join("cas")).unwrap();
        let provider_id = provider.provider_id();
        let local_sidecar = dir.path().join("local.sidecar.json");
        let remote_sidecar = dir.path().join("remote.sidecar.json");
        let author = |name: &str| AuthorIdentity {
            display_name: name.to_owned(),
            provider_user_id: name.to_lowercase(),
            provider_id: provider_id.clone(),
            email: None,
        };
        let ancestor = serde_json::to_vec(&serde_json::json!({
            "tempo": 120,
            "remote_label": "ancestor"
        }))
        .unwrap();
        let initial = rt()
            .block_on(push_with_freshness_check(
                &provider,
                &provider_id,
                &[],
                &local_sidecar,
                &ancestor,
                author("Initial"),
                "Initial",
                "",
            ))
            .unwrap();
        let PushOutcome::Committed { commit_id, .. } = initial else {
            panic!("initial push should commit");
        };
        Sidecar::modify(&remote_sidecar, |sidecar| {
            sidecar.local_head = Some(commit_id);
            sidecar.primary = Some(provider_id.clone());
        })
        .unwrap();

        let remote = serde_json::to_vec(&serde_json::json!({
            "tempo": 140,
            "remote_label": "theirs"
        }))
        .unwrap();
        rt().block_on(push_with_freshness_check(
            &provider,
            &provider_id,
            &[],
            &remote_sidecar,
            &remote,
            author("Remote"),
            "Remote edit",
            "",
        ))
        .unwrap();

        let local = serde_json::to_vec(&serde_json::json!({
            "tempo": 128,
            "remote_label": "ancestor"
        }))
        .unwrap();
        let conflict = rt()
            .block_on(push_with_freshness_check(
                &provider,
                &provider_id,
                &[],
                &local_sidecar,
                &local,
                author("Local"),
                "Local edit",
                "",
            ))
            .unwrap();
        let PushOutcome::NeedsResolution { conflicts, .. } = conflict else {
            panic!("divergent tempo should require resolution");
        };
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].path, "tempo");

        let resolutions = vec![ConflictResolution {
            conflict: conflicts[0].clone(),
            choice: crate::merge::ConflictChoice::Local,
        }];
        let options = PushOptions {
            conflict_resolutions: Some(resolutions),
            ..PushOptions::default()
        };
        let resolved = rt()
            .block_on(push_with_options(
                &provider,
                &provider_id,
                &[],
                &local_sidecar,
                &local,
                author("Local"),
                "Resolved edit",
                "",
                &options,
            ))
            .unwrap();
        let PushOutcome::Committed {
            commit_id,
            was_merge,
            ..
        } = resolved
        else {
            panic!("complete resolutions should commit");
        };
        assert!(was_merge);
        let commit = rt().block_on(provider.get_commit(&commit_id)).unwrap();
        let bytes = rt()
            .block_on(provider.get_blob(&commit.tree.snapshot))
            .unwrap();
        let snapshot: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(snapshot["tempo"], 128);
        assert_eq!(snapshot["remote_label"], "theirs");
    }

    #[test]
    fn conflict_resolution_should_reject_changed_values_at_the_same_path() {
        let displayed = ConflictedField {
            path: "tempo".to_owned(),
            ancestor: Some(serde_json::json!(120)),
            local: Some(serde_json::json!(128)),
            remote: Some(serde_json::json!(140)),
        };
        let current = ConflictedField {
            remote: Some(serde_json::json!(150)),
            ..displayed.clone()
        };
        let resolutions = [ConflictResolution {
            conflict: displayed,
            choice: crate::merge::ConflictChoice::Remote,
        }];

        assert!(!conflict_resolutions_match(&[current], &resolutions));
    }
}
