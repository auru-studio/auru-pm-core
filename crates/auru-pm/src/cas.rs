//! Content-addressed storage.
//!
//! Backs both the global client-side cache (`${app_data}/auru/objects/`
//! shared across all tracked projects so sample dedup is free) and the
//! bundled [`crate::filesystem::FilesystemProvider`]'s own blob store.
//! Layout: `<root>/blake3/<first2>/<rest>` — sharded on the first two
//! hex chars so a single directory never blows up.
//!
//! Writes go through a `<file>.tmp` + atomic rename so a process killed
//! mid-write never leaves a half-written blob in the store. Reads do
//! not re-verify the hash; the filename IS the hash and integrity is
//! enforced at write time. This matches git's loose-object behavior.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::ProjectProvider;
use crate::sample_manifest::SampleManifest;

/// Result of a [`Cas::gc`] run.
#[derive(Clone, Debug, Default)]
pub struct GcReport {
    pub freed_bytes: u64,
    pub freed_count: usize,
    pub kept_count: usize,
}

/// A filesystem-backed CAS rooted at a directory.
#[derive(Clone, Debug)]
pub struct Cas {
    root: PathBuf,
}

impl Cas {
    /// Open (creating if necessary) a CAS at `root`. The `blake3/`
    /// shard directory is created on the first `put`.
    pub fn open(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Path the blob with `hash` lives at — may or may not exist yet.
    pub fn path_for(&self, hash: &ContentHash) -> PathBuf {
        let hex = hash_to_hex(hash);
        let (shard, rest) = hex.split_at(2);
        self.root.join("blake3").join(shard).join(rest)
    }

    pub fn has(&self, hash: &ContentHash) -> bool {
        self.path_for(hash).exists()
    }

    /// Write `bytes` under `hash`. The caller is responsible for
    /// passing the correct hash — `put` verifies and returns
    /// [`Error::Other`] on mismatch so a corrupted hash can never be
    /// silently filed under the wrong name.
    pub fn put(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        let computed = ContentHash::of(bytes);
        if computed != *hash {
            return Err(Error::Other(format!(
                "hash mismatch: caller said {hash}, content hashed to {computed}"
            )));
        }
        let path = self.path_for(hash);
        if path.exists() {
            // CAS is content-addressed — same hash means same bytes.
            return Ok(());
        }
        let parent = path
            .parent()
            .ok_or_else(|| Error::Other("CAS path has no parent".into()))?;
        fs::create_dir_all(parent)?;

        // Atomic write: tmp file in the same directory, then rename.
        // Same-directory rename is atomic on every POSIX FS and on
        // NTFS / Windows since `fs::rename` was made to honor that.
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, bytes)?;
        match fs::rename(&tmp, &path) {
            Ok(()) => Ok(()),
            Err(e) => {
                // Best-effort cleanup; if it stays the next put for the
                // same hash will overwrite it.
                let _ = fs::remove_file(&tmp);
                Err(Error::Io(e))
            }
        }
    }

    pub fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let path = self.path_for(hash);
        fs::read(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(hash.to_string()),
            _ => Error::Io(e),
        })
    }

    /// Total bytes occupied by all blobs in this CAS (not counting directories
    /// or the `.tmp` temporaries from in-flight writes).
    pub fn disk_usage(&self) -> u64 {
        let blake3_dir = self.root.join("blake3");
        walk_bytes(&blake3_dir)
    }

    /// Enumerate every hash stored in this CAS by reading filenames. Skips
    /// files with unparseable names (e.g. leftover `.tmp` files).
    pub fn all_hashes(&self) -> Result<HashSet<ContentHash>> {
        let blake3_dir = self.root.join("blake3");
        if !blake3_dir.exists() {
            return Ok(HashSet::new());
        }
        let mut out = HashSet::new();
        for shard_entry in fs::read_dir(&blake3_dir)? {
            let shard = shard_entry?.path();
            if !shard.is_dir() {
                continue;
            }
            let shard_prefix = shard
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            for blob_entry in fs::read_dir(&shard)? {
                let blob = blob_entry?.path();
                if let Some(rest) = blob.file_name().and_then(|n| n.to_str()) {
                    if rest.ends_with(".tmp") {
                        continue;
                    }
                    let hex = format!("{shard_prefix}{rest}");
                    let full = format!("blake3:{hex}");
                    if let Ok(h) = full.parse::<ContentHash>() {
                        out.insert(h);
                    }
                }
            }
        }
        Ok(out)
    }

    /// Delete blobs that are not in `reachable` and were last written more
    /// than `grace_secs` seconds ago (protects in-progress uncommitted work).
    pub fn gc(&self, reachable: &HashSet<ContentHash>, grace_secs: u64) -> Result<GcReport> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut report = GcReport::default();
        let blake3_dir = self.root.join("blake3");
        if !blake3_dir.exists() {
            return Ok(report);
        }
        for shard_entry in fs::read_dir(&blake3_dir)? {
            let shard = shard_entry?.path();
            if !shard.is_dir() {
                continue;
            }
            let shard_prefix = shard
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            for blob_entry in fs::read_dir(&shard)? {
                let blob = blob_entry?.path();
                let rest = match blob.file_name().and_then(|n| n.to_str()) {
                    Some(r) if !r.ends_with(".tmp") => r.to_owned(),
                    _ => continue,
                };
                let hex = format!("{shard_prefix}{rest}");
                let full = format!("blake3:{hex}");
                let hash = match full.parse::<ContentHash>() {
                    Ok(h) => h,
                    Err(_) => continue,
                };
                if reachable.contains(&hash) {
                    report.kept_count += 1;
                    continue;
                }
                // Grace period: skip blobs written within the last `grace_secs`.
                let mtime_secs = blob
                    .metadata()
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if now.saturating_sub(mtime_secs) < grace_secs {
                    report.kept_count += 1;
                    continue;
                }
                let size = blob.metadata().map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(&blob).is_ok() {
                    report.freed_bytes += size;
                    report.freed_count += 1;
                }
            }
        }
        Ok(report)
    }
}

/// Walk `dir` recursively and return the total bytes of all regular files.
fn walk_bytes(dir: &Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            total += walk_bytes(&path);
        } else if path.is_file() {
            total += path.metadata().map(|m| m.len()).unwrap_or(0);
        }
    }
    total
}

/// Walk the commit graph reachable from the provider's HEAD and collect every
/// blob hash that a live commit references: snapshot blobs, sample-manifest
/// blobs, and every individual sample blob listed in each manifest.
///
/// Blobs not returned by this function are candidates for GC.
pub async fn collect_reachable(provider: &dyn ProjectProvider) -> Result<HashSet<ContentHash>> {
    use crate::commit::HistoryRange;

    let mut reachable: HashSet<ContentHash> = HashSet::new();

    // Fetch the entire history (no limit — we need every commit).
    let history = provider
        .list_history(HistoryRange {
            limit: None,
            before: None,
        })
        .await?;

    for summary in &history {
        let commit = match provider.get_commit(&summary.id).await {
            Ok(c) => c,
            Err(_) => continue,
        };

        // Snapshot blob.
        reachable.insert(commit.tree.snapshot);

        // Sample-manifest blob.
        reachable.insert(commit.tree.samples);

        // Individual sample blobs listed in the manifest.
        if let Ok(manifest_bytes) = provider.get_blob(&commit.tree.samples).await {
            if let Ok(manifest) = serde_json::from_slice::<SampleManifest>(&manifest_bytes) {
                for entry in &manifest.entries {
                    reachable.insert(entry.hash);
                }
            }
        }
    }

    Ok(reachable)
}

fn hash_to_hex(hash: &ContentHash) -> String {
    let s = hash.to_string();
    // Strip the `blake3:` prefix that Display adds.
    s.strip_prefix("blake3:").map(|h| h.to_owned()).unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn put_then_get() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = b"hello world";
        let hash = ContentHash::of(payload);

        assert!(!cas.has(&hash));
        cas.put(&hash, payload).unwrap();
        assert!(cas.has(&hash));
        assert_eq!(cas.get(&hash).unwrap(), payload);
    }

    #[test]
    fn put_is_idempotent() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = b"twice";
        let hash = ContentHash::of(payload);
        cas.put(&hash, payload).unwrap();
        cas.put(&hash, payload).unwrap();
        assert_eq!(cas.get(&hash).unwrap(), payload);
    }

    #[test]
    fn put_rejects_hash_mismatch() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = b"genuine";
        let wrong = ContentHash::of(b"different");
        let err = cas.put(&wrong, payload).unwrap_err();
        match err {
            Error::Other(msg) => assert!(msg.contains("hash mismatch"), "{msg}"),
            other => panic!("expected Other, got {other:?}"),
        }
    }

    #[test]
    fn get_missing_is_not_found() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let missing = ContentHash::of(b"absent");
        match cas.get(&missing).unwrap_err() {
            Error::NotFound(_) => {}
            other => panic!("expected NotFound, got {other:?}"),
        }
    }

    #[test]
    fn sharded_layout() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let hash = ContentHash::of(b"shard test");
        cas.put(&hash, b"shard test").unwrap();
        // The on-disk path lives under blake3/<first 2 hex chars>/<rest>
        let path = cas.path_for(&hash);
        let rel = path.strip_prefix(dir.path()).unwrap();
        let mut comps = rel.components();
        assert_eq!(
            comps.next().unwrap().as_os_str().to_str().unwrap(),
            "blake3"
        );
        let shard = comps.next().unwrap().as_os_str().to_str().unwrap();
        assert_eq!(shard.len(), 2);
        let rest = comps.next().unwrap().as_os_str().to_str().unwrap();
        assert_eq!(rest.len(), 64 - 2);
    }
}
