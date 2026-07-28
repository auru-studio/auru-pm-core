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

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use async_trait::async_trait;

use crate::canonical::{canonical_encoding, compute_commit_id};
use crate::cas::Cas;
use crate::commit::{Commit, CommitId, CommitSummary, HistoryRange};
use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::{
    AuthMethod, Capabilities, HeadAdvance, Member, PermSet, ProjectProvider, UserId,
};

/// Default cap for [`ProjectProvider::list_history`] when the caller
/// doesn't specify `limit`. Matches the practical history-panel page
/// size; deliberately small so a 10k-commit project doesn't fault in
/// every commit on first open.
const DEFAULT_HISTORY_LIMIT: u32 = 100;

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

    fn head_path(&self) -> PathBuf {
        self.root.join("HEAD")
    }

    fn read_head(&self) -> Result<Option<CommitId>> {
        match fs::read_to_string(self.head_path()) {
            Ok(s) => {
                let trimmed = s.trim();
                if trimmed.is_empty() {
                    Ok(None)
                } else {
                    let hash = ContentHash::from_str(trimmed)
                        .map_err(|e| Error::Other(format!("invalid HEAD contents: {e}")))?;
                    Ok(Some(CommitId(hash)))
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(Error::Io(e)),
        }
    }

    fn write_head(&self, id: Option<CommitId>) -> Result<()> {
        let body = match id {
            Some(c) => c.0.to_string(),
            None => String::new(),
        };
        let head = self.head_path();
        let tmp = head.with_extension("tmp");
        fs::write(&tmp, body.as_bytes())?;
        match fs::rename(&tmp, &head) {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                Err(Error::Io(e))
            }
        }
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
}

#[async_trait]
impl ProjectProvider for FilesystemProvider {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            members: false,
            permissions: false,
            branches: false,
            server_side_merge: false,
            auth_methods: vec![AuthMethod::None],
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
        let limit = range.limit.unwrap_or(DEFAULT_HISTORY_LIMIT) as usize;
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
            // Linear walk: M4 introduces merges, at which point this
            // gains a BFS fallback for the secondary parent.
            next = commit.parents.first().copied();
        }
        Ok(out)
    }

    async fn get_head(&self) -> Result<Option<CommitId>> {
        self.read_head()
    }

    async fn advance_head(&self, from: Option<CommitId>, to: CommitId) -> Result<HeadAdvance> {
        let current = self.read_head()?;
        if current != from {
            return Ok(HeadAdvance::Conflict { current });
        }
        self.write_head(Some(to))?;
        Ok(HeadAdvance::Advanced)
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
                samples: ContentHash::of(b"empty"),
            },
            author: author(),
            timestamp: 1_700_000_000,
            message: message.into(),
            description: String::new(),
            auru_version: "0.1.0".into(),
            format_version: 8,
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
