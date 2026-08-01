//! Bundled filesystem-backed [`ProjectProvider`].
//!
//! Backs a chosen folder as a "bare repo" — useful for Dropbox / iCloud
//! / NAS / USB-stick collab paths where there's no server, just shared
//! storage. Implements the exact same trait as the HTTP provider so
//! the rest of the app doesn't care which one it's talking to.
//!
//! Repo layout:
//! ```text
//! <root>/
//!   HEAD                                # `blake3:<hex>` of current commit, or empty
//!   objects/blake3/<aa>/<rest>          # blob CAS (snapshots, sample manifests, sample bytes)
//!   commits/blake3/<aa>/<rest>          # commit JSON, hashed identically
//! ```
//!
//! All I/O is synchronous inside `async fn` bodies. The trait is async
//! so HTTP providers can be properly async; the filesystem variant
//! pays no overhead for that — its futures resolve immediately.

use std::collections::HashSet;
use std::fs;
use std::fs::OpenOptions;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_trait::async_trait;
use fs2::FileExt;

use crate::canonical::{canonical_encoding, compute_commit_id};
use crate::cas::Cas;
use crate::commit::{Commit, CommitId, CommitSummary, HistoryRange};
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::{
    AuthMethod, Capabilities, HeadAdvance, Member, PermSet, ProjectProfile, ProjectProvider,
    ProviderProject, RetentionReport, RetentionRoots, RetentionRule, UserId,
};
use crate::sample_manifest::SampleManifest;

/// Default cap for [`ProjectProvider::list_history`] when the caller
/// doesn't specify `limit`. Matches the practical history-panel page
/// size; deliberately small so a 10k-commit project doesn't fault in
/// every commit on first open.
const DEFAULT_HISTORY_LIMIT: u32 = 100;
const PROJECT_PROFILE_FILE: &str = "project.json";
const HISTORY_FLOOR_FILE: &str = "HISTORY_FLOOR";
const REPOSITORY_LOCK_FILE: &str = "LOCK";
const RETENTION_GC_GRACE_SECS: u64 = 60 * 60;

#[derive(Clone, Debug)]
pub struct FilesystemProvider {
    root: PathBuf,
    blobs: Cas,
    commits: Cas,
}

impl FilesystemProvider {
    /// Open (creating if absent) a filesystem provider rooted at
    /// `root`. The `HEAD` file isn't materialized until the first
    /// `advance_head` — a freshly opened repo simply has no HEAD.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root: PathBuf = root.into();
        fs::create_dir_all(&root)?;
        let blobs = Cas::open(root.join("objects"))?;
        let commits = Cas::open(root.join("commits"))?;
        Ok(Self {
            root,
            blobs,
            commits,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Reference to the blob CAS, used for GC and disk-usage queries.
    pub fn blobs_cas(&self) -> &Cas {
        &self.blobs
    }

    /// Stable identifier for this provider instance, suitable for use
    /// as a key in `.auru` `known_providers` and the sidecar `remotes`
    /// map.
    pub fn provider_id(&self) -> String {
        format!("local-folder://{}", self.root.display())
    }

    /// List every project repository beneath an account-level projects root.
    ///
    /// A broken or half-created child is skipped so one damaged project does
    /// not hide every other recovery option on the drive.
    pub fn list_projects(root: &Path) -> Result<Vec<ProviderProject>> {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::Io(error)),
        };
        let mut projects = Vec::new();
        for entry in entries {
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let handle = entry.file_name().to_string_lossy().into_owned();
            let Ok(provider) = Self::open(entry.path()) else {
                continue;
            };
            let Ok(Some(head)) = provider.read_head() else {
                continue;
            };
            let Ok(commit) = provider.read_commit(&head) else {
                continue;
            };
            let profile = fs::read(provider.root.join(PROJECT_PROFILE_FILE))
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok());
            projects.push(ProviderProject {
                handle,
                head,
                profile,
                updated_at: commit.timestamp,
            });
        }
        projects.sort_by(|left, right| {
            right
                .updated_at
                .cmp(&left.updated_at)
                .then_with(|| left.handle.cmp(&right.handle))
        });
        Ok(projects)
    }

    fn head_path(&self) -> PathBuf {
        self.root.join("HEAD")
    }

    fn control_backup_path(path: &Path) -> PathBuf {
        path.with_extension("old")
    }

    fn read_control_file(path: &Path) -> Result<Option<String>> {
        Self::read_control_file_with(path, |path| fs::read_to_string(path))
    }

    fn read_control_file_with(
        path: &Path,
        mut read: impl FnMut(&Path) -> io::Result<String>,
    ) -> Result<Option<String>> {
        match read(path) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match read(&Self::control_backup_path(path)) {
                    Ok(value) => Ok(Some(value)),
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {
                        // The Windows replacement path briefly moves the
                        // primary to `.old`. A writer may have installed the
                        // new primary and removed `.old` between our reads, so
                        // check the primary once more before reporting absence.
                        match read(path) {
                            Ok(value) => Ok(Some(value)),
                            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                            Err(error) => Err(Error::Io(error)),
                        }
                    }
                    Err(error) => Err(Error::Io(error)),
                }
            }
            Err(error) => Err(Error::Io(error)),
        }
    }

    fn replace_control_file(path: &Path, body: &[u8]) -> Result<()> {
        let tmp = path.with_extension("tmp");
        let backup = Self::control_backup_path(path);
        fs::write(&tmp, body)?;
        match fs::rename(&tmp, path) {
            Ok(()) => {
                let _ = fs::remove_file(backup);
                Ok(())
            }
            Err(_) if path.exists() => {
                // Windows cannot rename over an existing file. Move the last
                // valid value aside first; readers fall back to `.old` if the
                // process stops before the replacement reaches its final name.
                let _ = fs::remove_file(&backup);
                if let Err(move_error) = fs::rename(path, &backup) {
                    let _ = fs::remove_file(&tmp);
                    return Err(Error::Io(move_error));
                }
                match fs::rename(&tmp, path) {
                    Ok(()) => {
                        let _ = fs::remove_file(&backup);
                        Ok(())
                    }
                    Err(replace_error) => {
                        let _ = fs::rename(&backup, path);
                        let _ = fs::remove_file(&tmp);
                        Err(Error::Io(replace_error))
                    }
                }
            }
            Err(error) => {
                let _ = fs::remove_file(&tmp);
                Err(Error::Io(error))
            }
        }
    }

    fn read_head(&self) -> Result<Option<CommitId>> {
        let Some(value) = Self::read_control_file(&self.head_path())? else {
            return Ok(None);
        };
        let trimmed = value.trim();
        if trimmed.is_empty() {
            Ok(None)
        } else {
            let hash = ContentHash::from_str(trimmed)
                .map_err(|error| Error::Other(format!("invalid HEAD contents: {error}")))?;
            Ok(Some(CommitId(hash)))
        }
    }

    fn write_head(&self, id: Option<CommitId>) -> Result<()> {
        let body = match id {
            Some(c) => c.0.to_string(),
            None => String::new(),
        };
        Self::replace_control_file(&self.head_path(), body.as_bytes())
    }

    fn read_commit(&self, id: &CommitId) -> Result<Commit> {
        let bytes = self.commits.get(&id.0)?;
        // The stored blob is the canonical encoding — `id` was stripped
        // before hashing so the commit's identity is a function of its
        // content. Re-inject the id we just looked up so deserialization
        // produces a full [`Commit`].
        let mut value: serde_json::Value = serde_json::from_slice(&bytes)?;
        if let serde_json::Value::Object(map) = &mut value {
            map.insert("id".to_string(), serde_json::to_value(id)?);
        }
        let commit: Commit = serde_json::from_value(value)?;
        Ok(commit)
    }

    fn write_project_profile(&self, profile: &ProjectProfile) -> Result<()> {
        let path = self.root.join(PROJECT_PROFILE_FILE);
        let mut profile = profile.clone();
        if profile.location.is_none() {
            profile.location = fs::read(&path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<ProjectProfile>(&bytes).ok())
                .and_then(|existing| existing.location);
        }
        let body = serde_json::to_vec_pretty(&profile)?;
        // Unlike commits, this small cache is reconstructible from HEAD when
        // absent or malformed. A direct overwrite also keeps the required
        // idempotent upsert semantics on Windows, where rename does not
        // replace an existing destination.
        fs::write(path, body)?;
        Ok(())
    }

    fn history_floor_path(&self) -> PathBuf {
        self.root.join(HISTORY_FLOOR_FILE)
    }

    fn acquire_repository_lock(&self) -> Result<fs::File> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.root.join(REPOSITORY_LOCK_FILE))?;
        FileExt::lock_exclusive(&file)?;
        Ok(file)
    }

    #[cfg(test)]
    fn history_floor_backup_path(&self) -> PathBuf {
        Self::control_backup_path(&self.history_floor_path())
    }

    fn read_history_floor(&self) -> Result<Option<CommitId>> {
        let path = self.history_floor_path();
        let Some(value) = Self::read_control_file(&path)? else {
            return Ok(None);
        };
        match ContentHash::from_str(value.trim()) {
            Ok(hash) => Ok(Some(CommitId(hash))),
            Err(error) => Err(Error::Other(format!("invalid history floor: {error}"))),
        }
    }

    fn write_history_floor(&self, id: CommitId) -> Result<()> {
        Self::replace_control_file(&self.history_floor_path(), id.0.to_string().as_bytes())
    }

    fn visible_commits(&self) -> Result<Vec<Commit>> {
        let floor = self.read_history_floor()?;
        let mut commits = Vec::new();
        let mut next = self.read_head()?;
        while let Some(id) = next {
            let commit = self.read_commit(&id)?;
            next = commit.parents.first().copied();
            commits.push(commit);
            if Some(id) == floor {
                break;
            }
        }
        Ok(commits)
    }

    fn retention_graph(
        &self,
        retained: &[Commit],
        floor: CommitId,
        protected: &RetentionRoots,
    ) -> Result<Vec<Commit>> {
        let mut graph = Vec::new();
        let mut seen = HashSet::new();
        let mut pending: Vec<CommitId> = retained
            .iter()
            .map(|commit| commit.id)
            .chain(protected.commits.iter().copied())
            .collect();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let commit = retained
                .iter()
                .find(|commit| commit.id == id)
                .cloned()
                .map_or_else(|| self.read_commit(&id), Ok)?;
            if id != floor {
                pending.extend(commit.parents.iter().copied());
            }
            graph.push(commit);
        }
        Ok(graph)
    }

    fn reachable_blobs(
        &self,
        commits: &[Commit],
        protected: &RetentionRoots,
    ) -> Result<HashSet<ContentHash>> {
        let mut reachable = HashSet::new();
        for commit in commits {
            reachable.insert(commit.tree.snapshot);
            reachable.insert(commit.tree.samples);
            if let Some(metadata) = commit.metadata {
                reachable.insert(metadata);
            }
            // Fail closed. If a retained manifest is missing or malformed, we
            // cannot know which sample objects it protects and must not run GC.
            let bytes = self.blobs.get(&commit.tree.samples)?;
            let manifest = serde_json::from_slice::<SampleManifest>(&bytes)?;
            reachable.extend(manifest.entries.iter().map(|entry| entry.hash));
        }
        reachable.extend(protected.blobs.iter().copied());
        Ok(reachable)
    }

    fn validate_commit_objects(&self, commit: &Commit) -> Result<()> {
        if !self.blobs.has(&commit.tree.snapshot) {
            return Err(Error::NotFound(commit.tree.snapshot.to_string()));
        }
        if let Some(metadata) = commit.metadata
            && !self.blobs.has(&metadata)
        {
            return Err(Error::NotFound(metadata.to_string()));
        }
        let manifest_bytes = self.blobs.get(&commit.tree.samples)?;
        let manifest = serde_json::from_slice::<SampleManifest>(&manifest_bytes)?;
        if let Some(missing) = manifest
            .entries
            .iter()
            .map(|entry| entry.hash)
            .find(|hash| !self.blobs.has(hash))
        {
            return Err(Error::NotFound(missing.to_string()));
        }
        Ok(())
    }

    async fn prune_history_with_grace(
        &self,
        rule: RetentionRule,
        protected: &RetentionRoots,
        grace_secs: u64,
    ) -> Result<RetentionReport> {
        let _repository_lock = self.acquire_repository_lock()?;
        let history = self.visible_commits()?;
        let retained_len = rule.retained_prefix_len(history.iter().map(|commit| commit.timestamp));
        let Some(floor) = retained_len
            .checked_sub(1)
            .and_then(|index| history.get(index))
            .map(|commit| commit.id)
        else {
            return Ok(RetentionReport::default());
        };
        let versions_removed = history.len().saturating_sub(retained_len);
        let retained = &history[..retained_len];
        let retained_graph = self.retention_graph(retained, floor, protected)?;
        let reachable_blobs = self.reachable_blobs(&retained_graph, protected)?;
        let reachable_commits: HashSet<ContentHash> =
            retained_graph.iter().map(|commit| commit.id.0).collect();

        // Publish the new boundary before reclaiming content. If collection is
        // interrupted after this atomic write, history is already truthful and
        // the next retention pass can safely finish deleting orphaned objects.
        self.write_history_floor(floor)?;
        let blob_report = self.blobs.gc(&reachable_blobs, grace_secs)?;
        let commit_report = self.commits.gc(&reachable_commits, grace_secs)?;

        Ok(RetentionReport {
            versions_removed: versions_removed as u64,
            objects_removed: (blob_report.freed_count + commit_report.freed_count) as u64,
            bytes_freed: blob_report.freed_bytes + commit_report.freed_bytes,
        })
    }
}

#[async_trait]
impl ProjectProvider for FilesystemProvider {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            project_listing: true,
            members: false,
            permissions: false,
            branches: false,
            server_side_merge: false,
            auth_methods: vec![AuthMethod::None],
            // Not an HTTP provider — there is no transfer encoding to
            // negotiate. Blobs are still stored compressed by the CAS.
            compressed_uploads: false,
            history_retention: true,
            project_scoped_blobs: true,
        }
    }

    async fn put_blob(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        self.blobs.put(hash, bytes)
    }

    async fn has_blobs(&self, hashes: &[ContentHash]) -> Result<Vec<bool>> {
        Ok(hashes.iter().map(|h| self.blobs.has(h)).collect())
    }

    async fn get_blob(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        self.blobs.get(hash)
    }

    async fn put_commit(&self, commit: &Commit) -> Result<CommitId> {
        let computed = compute_commit_id(commit)?;
        if computed != commit.id {
            return Err(Error::Other(format!(
                "commit id mismatch: caller said {}, canonical encoding hashes to {}",
                commit.id.0, computed.0
            )));
        }
        let bytes = canonical_encoding(commit)?;
        self.commits.put(&commit.id.0, &bytes)?;
        Ok(commit.id)
    }

    async fn get_commit(&self, id: &CommitId) -> Result<Commit> {
        self.read_commit(id)
    }

    async fn list_history(&self, range: HistoryRange) -> Result<Vec<CommitSummary>> {
        let _repository_lock = self.acquire_repository_lock()?;
        let limit = range.limit.unwrap_or(DEFAULT_HISTORY_LIMIT) as usize;
        let floor = self.read_history_floor()?;
        let mut out = Vec::new();
        let mut started = range.before.is_none();
        let mut next = self.read_head()?;
        while let Some(id) = next {
            let commit = self.read_commit(&id)?;
            if started {
                out.push(CommitSummary::from(&commit));
                if out.len() >= limit {
                    break;
                }
            } else if Some(id) == range.before {
                // The cursor is exclusive — the named commit is not
                // included; start collecting from its parent.
                started = true;
            }
            if Some(id) == floor {
                break;
            }
            // Linear walk: M4 introduces merges, at which point this
            // gains a BFS fallback for the secondary parent.
            next = commit.parents.first().copied();
        }
        Ok(out)
    }

    async fn get_head(&self) -> Result<Option<CommitId>> {
        let _repository_lock = self.acquire_repository_lock()?;
        self.read_head()
    }

    async fn advance_head(&self, from: Option<CommitId>, to: CommitId) -> Result<HeadAdvance> {
        let _repository_lock = self.acquire_repository_lock()?;
        let current = self.read_head()?;
        if current != from {
            return Ok(HeadAdvance::Conflict { current });
        }
        // Once this repository has ever collected history, HEAD publication
        // and GC share the repository lock. Re-check every object while
        // holding it so a push that reused an old blob cannot publish a commit
        // after a concurrent retention pass removed that blob.
        if self.read_history_floor()?.is_some() {
            let commit = self.read_commit(&to)?;
            self.validate_commit_objects(&commit)?;
        }
        self.write_head(Some(to))?;
        Ok(HeadAdvance::Advanced)
    }

    async fn put_project_profile(&self, profile: &ProjectProfile) -> Result<()> {
        self.write_project_profile(profile)
    }

    async fn prune_history(
        &self,
        rule: RetentionRule,
        protected: &RetentionRoots,
    ) -> Result<RetentionReport> {
        self.prune_history_with_grace(rule, protected, RETENTION_GC_GRACE_SECS)
            .await
    }

    async fn list_members(&self) -> Result<Vec<Member>> {
        Err(Error::Unsupported("members"))
    }

    async fn permissions(&self, _user: &UserId) -> Result<PermSet> {
        Err(Error::Unsupported("permissions"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{AuthorIdentity, Commit, TreeRef};
    use tempfile::TempDir;

    const EMPTY_MANIFEST_JSON: &[u8] = br#"{"entries":[]}"#;

    fn rt() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn author() -> AuthorIdentity {
        AuthorIdentity {
            display_name: "Test User".into(),
            provider_user_id: "user-1".into(),
            provider_id: "local-folder".into(),
            email: None,
        }
    }

    /// Build a commit with the right canonical id. `parent` chains the
    /// commit to a prior one; pass `None` for the root.
    fn build_commit(message: &str, parent: Option<CommitId>) -> Commit {
        let mut commit = Commit {
            id: CommitId(ContentHash::ZERO),
            parents: parent.into_iter().collect(),
            tree: TreeRef {
                snapshot: ContentHash::of(message.as_bytes()),
                samples: ContentHash::of(EMPTY_MANIFEST_JSON),
            },
            author: author(),
            timestamp: 1_700_000_000,
            message: message.into(),
            description: String::new(),
            auru_version: "0.1.0".into(),
            format_version: 8,
            metadata: None,
        };
        commit.id = compute_commit_id(&commit).unwrap();
        commit
    }

    #[test]
    fn fresh_repo_has_no_head() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let head = rt().block_on(provider.get_head()).unwrap();
        assert_eq!(head, None);
    }

    #[test]
    fn an_interrupted_history_floor_replacement_should_keep_the_last_valid_floor() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let first = CommitId(ContentHash::of(b"first"));
        let second = CommitId(ContentHash::of(b"second"));
        provider.write_history_floor(first).unwrap();

        fs::rename(
            provider.history_floor_path(),
            provider.history_floor_backup_path(),
        )
        .unwrap();

        assert_eq!(provider.read_history_floor().unwrap(), Some(first));
        provider.write_history_floor(second).unwrap();
        assert_eq!(provider.read_history_floor().unwrap(), Some(second));
        assert!(!provider.history_floor_backup_path().exists());
    }

    #[test]
    fn a_control_reader_should_retry_after_a_replacement_interleaving() {
        let mut read_count = 0;
        let value = FilesystemProvider::read_control_file_with(Path::new("HEAD"), |_| {
            read_count += 1;
            match read_count {
                1 | 2 => Err(io::Error::from(io::ErrorKind::NotFound)),
                3 => Ok("new value".to_owned()),
                _ => panic!("unexpected extra read"),
            }
        })
        .unwrap();

        assert_eq!(value.as_deref(), Some("new value"));
        assert_eq!(read_count, 3);
    }

    #[test]
    fn blob_roundtrip() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let payload = b"abc def 123";
        let hash = ContentHash::of(payload);
        rt().block_on(async {
            provider.put_blob(&hash, payload).await.unwrap();
            assert_eq!(provider.has_blobs(&[hash]).await.unwrap(), vec![true]);
            assert_eq!(provider.get_blob(&hash).await.unwrap(), payload);
        });
    }

    #[test]
    fn put_commit_rejects_bad_id() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let mut commit = build_commit("hello", None);
        commit.id = CommitId(ContentHash::of(b"wrong"));
        let err = rt().block_on(provider.put_commit(&commit)).unwrap_err();
        match err {
            Error::Other(msg) => assert!(msg.contains("commit id mismatch"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn advance_head_cas() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let c1 = build_commit("first", None);
        let c2 = build_commit("second", Some(c1.id));

        rt().block_on(async {
            provider.put_commit(&c1).await.unwrap();
            // Initial publish: from = None.
            let r = provider.advance_head(None, c1.id).await.unwrap();
            assert_eq!(r, HeadAdvance::Advanced);
            assert_eq!(provider.get_head().await.unwrap(), Some(c1.id));

            // Stale `from` is rejected, current HEAD reported.
            let bad = CommitId(ContentHash::of(b"nope"));
            let r = provider.advance_head(Some(bad), c2.id).await.unwrap();
            assert_eq!(
                r,
                HeadAdvance::Conflict {
                    current: Some(c1.id)
                }
            );
            assert_eq!(provider.get_head().await.unwrap(), Some(c1.id));

            // Correct `from` advances.
            provider.put_commit(&c2).await.unwrap();
            let r = provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            assert_eq!(r, HeadAdvance::Advanced);
            assert_eq!(provider.get_head().await.unwrap(), Some(c2.id));
        });
    }

    #[test]
    fn list_history_walks_parents() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let c1 = build_commit("first", None);
        let c2 = build_commit("second", Some(c1.id));
        let c3 = build_commit("third", Some(c2.id));

        rt().block_on(async {
            provider.put_commit(&c1).await.unwrap();
            provider.put_commit(&c2).await.unwrap();
            provider.put_commit(&c3).await.unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            provider.advance_head(Some(c2.id), c3.id).await.unwrap();

            // No range → newest first, all entries.
            let h = provider
                .list_history(HistoryRange::default())
                .await
                .unwrap();
            let msgs: Vec<&str> = h.iter().map(|s| s.message.as_str()).collect();
            assert_eq!(msgs, vec!["third", "second", "first"]);

            // limit caps the page.
            let h = provider
                .list_history(HistoryRange {
                    limit: Some(2),
                    before: None,
                })
                .await
                .unwrap();
            assert_eq!(h.len(), 2);
            assert_eq!(h[0].message, "third");
            assert_eq!(h[1].message, "second");

            // `before` is exclusive — pagination cursor semantics.
            let h = provider
                .list_history(HistoryRange {
                    limit: None,
                    before: Some(c3.id),
                })
                .await
                .unwrap();
            let msgs: Vec<&str> = h.iter().map(|s| s.message.as_str()).collect();
            assert_eq!(msgs, vec!["second", "first"]);
        });
    }

    #[test]
    fn retention_should_keep_only_the_latest_versions_and_reclaim_the_rest() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let c1 = build_commit("first", None);
        let c2 = build_commit("second", Some(c1.id));
        let c3 = build_commit("third", Some(c2.id));

        rt().block_on(async {
            for commit in [&c1, &c2, &c3] {
                provider.put_commit(commit).await.unwrap();
                provider
                    .put_blob(&commit.tree.snapshot, commit.message.as_bytes())
                    .await
                    .unwrap();
            }
            provider
                .put_blob(&c1.tree.samples, EMPTY_MANIFEST_JSON)
                .await
                .unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            provider.advance_head(Some(c2.id), c3.id).await.unwrap();

            let report = provider
                .prune_history_with_grace(
                    RetentionRule::Latest { count: 2 },
                    &RetentionRoots::default(),
                    0,
                )
                .await
                .unwrap();

            let history = provider
                .list_history(HistoryRange {
                    limit: Some(10),
                    before: None,
                })
                .await
                .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|commit| commit.message.as_str())
                    .collect::<Vec<_>>(),
                vec!["third", "second"]
            );
            assert_eq!(report.versions_removed, 1);
            assert!(!provider.commits.has(&c1.id.0));
            assert!(!provider.blobs.has(&c1.tree.snapshot));
            assert!(provider.blobs.has(&c2.tree.snapshot));
            assert!(provider.blobs.has(&c3.tree.snapshot));
        });
    }

    #[test]
    fn retention_since_should_keep_a_connected_prefix_through_the_cutoff() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let mut c1 = build_commit("first", None);
        c1.timestamp = 1_600_000_000;
        c1.id = compute_commit_id(&c1).unwrap();
        let mut c2 = build_commit("second", Some(c1.id));
        c2.timestamp = 1_700_000_000;
        c2.id = compute_commit_id(&c2).unwrap();
        let mut c3 = build_commit("third", Some(c2.id));
        c3.timestamp = 1_800_000_000;
        c3.id = compute_commit_id(&c3).unwrap();

        rt().block_on(async {
            for commit in [&c1, &c2, &c3] {
                provider.put_commit(commit).await.unwrap();
            }
            provider
                .put_blob(&c1.tree.samples, EMPTY_MANIFEST_JSON)
                .await
                .unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            provider.advance_head(Some(c2.id), c3.id).await.unwrap();

            provider
                .prune_history_with_grace(
                    RetentionRule::Since {
                        timestamp: 1_650_000_000,
                    },
                    &RetentionRoots::default(),
                    0,
                )
                .await
                .unwrap();

            let history = provider
                .list_history(HistoryRange {
                    limit: Some(10),
                    before: None,
                })
                .await
                .unwrap();
            assert_eq!(
                history
                    .iter()
                    .map(|commit| commit.message.as_str())
                    .collect::<Vec<_>>(),
                vec!["third", "second"]
            );
        });
    }

    #[test]
    fn retention_should_fail_closed_when_a_kept_manifest_cannot_be_read() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let malformed = b"not a sample manifest";
        let malformed_hash = ContentHash::of(malformed);
        let mut c1 = build_commit("first", None);
        c1.tree.samples = malformed_hash;
        c1.id = compute_commit_id(&c1).unwrap();
        let mut c2 = build_commit("second", Some(c1.id));
        c2.tree.samples = malformed_hash;
        c2.id = compute_commit_id(&c2).unwrap();

        rt().block_on(async {
            provider.put_commit(&c1).await.unwrap();
            provider.put_commit(&c2).await.unwrap();
            provider.put_blob(&malformed_hash, malformed).await.unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();

            provider
                .prune_history_with_grace(
                    RetentionRule::Latest { count: 1 },
                    &RetentionRoots::default(),
                    0,
                )
                .await
                .expect_err("unknown sample reachability must stop collection");

            assert_eq!(
                provider
                    .list_history(HistoryRange {
                        limit: Some(10),
                        before: None,
                    })
                    .await
                    .unwrap()
                    .len(),
                2,
                "the history boundary must not move before reachability is known"
            );
        });
    }

    #[test]
    fn retention_should_preserve_protected_pending_commits_and_their_blobs() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let c1 = build_commit("first", None);
        let c2 = build_commit("second", Some(c1.id));
        let c3 = build_commit("third", Some(c2.id));

        rt().block_on(async {
            for commit in [&c1, &c2, &c3] {
                provider.put_commit(commit).await.unwrap();
                provider
                    .put_blob(&commit.tree.snapshot, commit.message.as_bytes())
                    .await
                    .unwrap();
            }
            provider
                .put_blob(&c1.tree.samples, EMPTY_MANIFEST_JSON)
                .await
                .unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            provider.advance_head(Some(c2.id), c3.id).await.unwrap();

            provider
                .prune_history_with_grace(
                    RetentionRule::Latest { count: 1 },
                    &RetentionRoots {
                        commits: vec![c1.id],
                        blobs: Vec::new(),
                    },
                    0,
                )
                .await
                .unwrap();

            assert!(provider.commits.has(&c1.id.0));
            assert!(provider.blobs.has(&c1.tree.snapshot));
            assert!(!provider.commits.has(&c2.id.0));
            assert!(!provider.blobs.has(&c2.tree.snapshot));
        });
    }

    #[test]
    fn retention_should_preserve_secondary_merge_ancestry_above_the_floor() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let c1 = build_commit("first", None);
        let c2 = build_commit("second", Some(c1.id));
        let branch = build_commit("branch", Some(c1.id));
        let mut merge = build_commit("merge", Some(c2.id));
        merge.parents.push(branch.id);
        merge.id = compute_commit_id(&merge).unwrap();
        let c4 = build_commit("fourth", Some(merge.id));

        rt().block_on(async {
            for commit in [&c1, &c2, &branch, &merge, &c4] {
                provider.put_commit(commit).await.unwrap();
                provider
                    .put_blob(&commit.tree.snapshot, commit.message.as_bytes())
                    .await
                    .unwrap();
            }
            provider
                .put_blob(&c1.tree.samples, EMPTY_MANIFEST_JSON)
                .await
                .unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            provider.advance_head(Some(c2.id), merge.id).await.unwrap();
            provider.advance_head(Some(merge.id), c4.id).await.unwrap();

            provider
                .prune_history_with_grace(
                    RetentionRule::Latest { count: 3 },
                    &RetentionRoots::default(),
                    0,
                )
                .await
                .unwrap();

            assert!(provider.commits.has(&branch.id.0));
            assert!(provider.blobs.has(&branch.tree.snapshot));
            assert!(
                provider.commits.has(&c1.id.0),
                "the secondary branch still reaches its base"
            );
        });
    }

    #[test]
    fn head_publication_after_retention_should_reject_a_collected_blob() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let c1 = build_commit("first", None);
        let c2 = build_commit("second", Some(c1.id));

        rt().block_on(async {
            for commit in [&c1, &c2] {
                provider.put_commit(commit).await.unwrap();
                provider
                    .put_blob(&commit.tree.snapshot, commit.message.as_bytes())
                    .await
                    .unwrap();
            }
            provider
                .put_blob(&c1.tree.samples, EMPTY_MANIFEST_JSON)
                .await
                .unwrap();
            provider.advance_head(None, c1.id).await.unwrap();
            provider.advance_head(Some(c1.id), c2.id).await.unwrap();
            provider
                .prune_history_with_grace(
                    RetentionRule::Latest { count: 1 },
                    &RetentionRoots::default(),
                    0,
                )
                .await
                .unwrap();
            assert!(!provider.blobs.has(&c1.tree.snapshot));

            let mut c3 = build_commit("third", Some(c2.id));
            c3.tree.snapshot = c1.tree.snapshot;
            c3.id = compute_commit_id(&c3).unwrap();
            provider.put_commit(&c3).await.unwrap();

            let error = provider
                .advance_head(Some(c2.id), c3.id)
                .await
                .expect_err("a commit cannot publish after GC removed a reused object");
            assert!(matches!(error, Error::NotFound(_)));
            assert_eq!(provider.get_head().await.unwrap(), Some(c2.id));
        });
    }

    #[test]
    fn an_account_projects_root_should_list_committed_projects() {
        let dir = TempDir::new().unwrap();
        let projects_root = dir.path().join("projects");
        let provider = FilesystemProvider::open(projects_root.join("night-drive")).unwrap();
        let commit = build_commit("first", None);
        let profile = ProjectProfile {
            display_name: "Night Drive".into(),
            format: crate::ProjectFormat::AbletonLiveSet,
            metadata: crate::ProjectMetadata::default(),
            location: Some(crate::ProjectLocation {
                relative_path: "Ableton/Projects/Night Drive Project".into(),
            }),
        };

        rt().block_on(async {
            provider.put_commit(&commit).await.unwrap();
            provider.put_project_profile(&profile).await.unwrap();
            provider
                .put_project_profile(&ProjectProfile {
                    display_name: "Night Drive (Renamed)".into(),
                    location: None,
                    ..profile.clone()
                })
                .await
                .unwrap();
            provider.advance_head(None, commit.id).await.unwrap();
        });
        // A half-created child has no HEAD and must not hide the valid one.
        fs::create_dir_all(projects_root.join("unfinished")).unwrap();

        let projects = FilesystemProvider::list_projects(&projects_root).unwrap();

        assert_eq!(projects.len(), 1);
        assert_eq!(projects[0].handle, "night-drive");
        assert_eq!(projects[0].head, commit.id);
        assert_eq!(
            projects[0]
                .profile
                .as_ref()
                .map(|profile| profile.display_name.as_str()),
            Some("Night Drive (Renamed)")
        );
        assert_eq!(
            projects[0]
                .profile
                .as_ref()
                .and_then(|profile| profile.location.as_ref())
                .map(|location| location.relative_path.as_str()),
            Some("Ableton/Projects/Night Drive Project")
        );
        assert_eq!(projects[0].updated_at, commit.timestamp);
    }

    #[test]
    fn capability_gated_methods_return_unsupported() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        rt().block_on(async {
            match provider.list_members().await {
                Err(Error::Unsupported("members")) => {}
                other => panic!("unexpected: {other:?}"),
            }
            match provider.permissions(&"x".to_string()).await {
                Err(Error::Unsupported("permissions")) => {}
                other => panic!("unexpected: {other:?}"),
            }
        });
    }

    #[test]
    fn put_commit_then_get_commit_roundtrip() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let commit = build_commit("only", None);
        rt().block_on(async {
            provider.put_commit(&commit).await.unwrap();
            let read = provider.get_commit(&commit.id).await.unwrap();
            assert_eq!(read.id, commit.id);
            assert_eq!(read.message, "only");
            assert_eq!(read.tree.snapshot, commit.tree.snapshot);
        });
    }
}
