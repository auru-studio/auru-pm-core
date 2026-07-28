//! Push coordinator: freshness-checked commit, mirror fan-out, pending drain.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

use crate::canonical::compute_commit_id;
use crate::commit::{AuthorIdentity, Commit, CommitId, TreeRef};
use crate::hash::ContentHash;
use crate::merge::{ConflictResolution, ConflictedField, MergeOutcome, merge3, resolve_conflicts};
use crate::provider::{HeadAdvance, ProjectProvider};
use crate::sample_manifest::{SampleEntry, SampleManifest, sample_paths_in_snapshot};
use crate::sidecar::Sidecar;

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
}

fn now_epoch_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Read every sample file referenced by `snapshot` from disk, store its bytes
/// in the CAS, and assemble the [`SampleManifest`] for the commit.
///
/// Best-effort on missing files: a sample whose path can't be read is logged
/// and skipped rather than failing the whole commit. This keeps commits
/// working when a project references a sample that has since moved, and when a
/// 3-way merge pulls in a remote clip whose sample isn't on local disk (its
/// blob already lives in the CAS from the remote's own push).
///
/// Returns the manifest plus the `(hash, bytes)` of every sample actually
/// stored, so the mirror fan-out can replay the same blobs without touching
/// disk again.
async fn build_sample_manifest(
    primary: &dyn ProjectProvider,
    snapshot: &Value,
) -> Result<(SampleManifest, Vec<(ContentHash, Vec<u8>)>), String> {
    let mut manifest = SampleManifest::new();
    let mut blobs: Vec<(ContentHash, Vec<u8>)> = Vec::new();

    for path in sample_paths_in_snapshot(snapshot) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) => {
                eprintln!("[pm] skipping unreadable sample '{path}': {e}");
                continue;
            }
        };
        let hash = ContentHash::of(&bytes);
        primary
            .put_blob(&hash, &bytes)
            .await
            .map_err(|e| format!("put sample blob '{path}': {e}"))?;
        manifest.insert(SampleEntry {
            path,
            hash,
            size: bytes.len() as u64,
        });
        blobs.push((hash, bytes));
    }

    Ok((manifest, blobs))
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
// This is a stable public coordinator API whose parameters represent distinct
// caller-owned resources; keep it source-compatible until a versioned options
// type can be introduced.
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
    push_with_optional_resolutions(
        primary,
        primary_id,
        mirrors,
        sidecar_path,
        snapshot_bytes,
        author,
        message,
        description,
        None,
    )
    .await
}

/// Retry a freshness-checked push with an explicit choice for every field
/// reported by the current three-way merge.
///
/// The provider head and conflict set are recomputed before applying choices,
/// so a remote change between displaying and confirming the resolver cannot
/// silently commit against stale data.
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
    push_with_optional_resolutions(
        primary,
        primary_id,
        mirrors,
        sidecar_path,
        snapshot_bytes,
        author,
        message,
        description,
        Some(resolutions),
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
                let ancestor_json: Value = serde_json::from_slice(&ancestor_bytes)
                    .map_err(|e| format!("parse ancestor snapshot: {e}"))?;
                let current_local: Value = serde_json::from_slice(snapshot_bytes)
                    .map_err(|e| format!("parse local snapshot: {e}"))?;
                let remote_json: Value = serde_json::from_slice(&remote_bytes)
                    .map_err(|e| format!("parse remote snapshot: {e}"))?;

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
                        let merged_bytes = serde_json::to_vec(&merged)
                            .map_err(|error| format!("serialize resolved snapshot: {error}"))?;
                        let parents = vec![local_id, remote_id];
                        (merged_bytes, parents, true, Some(remote_id))
                    }
                    MergeOutcome::Clean { merged } => {
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

    // Resolve the sample set from the final snapshot, store each sample's
    // bytes in the CAS, and build the manifest that the commit will point at.
    let final_snapshot: Value = serde_json::from_slice(&final_snapshot_bytes)
        .map_err(|e| format!("parse final snapshot: {e}"))?;
    let (manifest, sample_blobs) = build_sample_manifest(primary, &final_snapshot).await?;

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
        s.local_head = Some(commit_id);
        let state = s.remotes.entry(primary_id.to_owned()).or_default();
        state.remote_head = Some(commit_id);
        state.last_pulled = Some(now_epoch_secs());
        // Enqueue commit for any mirror that failed.
        if !failed_mirror_ids.is_empty() && !s.pending_pushes.contains(&commit_id) {
            s.pending_pushes.push(commit_id);
        }
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
    use super::*;
    use crate::filesystem::FilesystemProvider;
    use tempfile::TempDir;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
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
            .block_on(build_sample_manifest(&provider, &snapshot))
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
            .block_on(build_sample_manifest(&provider, &snapshot))
            .unwrap();

        // The unreadable sample is dropped; the commit still gets a manifest
        // listing the sample that does exist.
        assert_eq!(manifest.entries.len(), 1);
        assert_eq!(blobs.len(), 1);
        assert_eq!(manifest.entries[0].path, present.to_str().unwrap());
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
        let resolved = rt()
            .block_on(push_with_conflict_resolutions(
                &provider,
                &provider_id,
                &[],
                &local_sidecar,
                &local,
                author("Local"),
                "Resolved edit",
                "",
                &resolutions,
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
