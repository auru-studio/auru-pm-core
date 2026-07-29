//! Content-addressed storage.
//!
//! Backs both the global client-side cache (`${app_data}/auru/objects/`
//! shared across all tracked projects so sample dedup is free) and the
//! bundled [`crate::filesystem::FilesystemProvider`]'s own blob store.
//! Layout: `<root>/blake3/<first2>/<rest>` — sharded on the first two
//! hex chars so a single directory never blows up.
//!
//! Writes go through a `<file>.tmp` + atomic rename so a process killed
//! mid-write never leaves a half-written blob in the store.
//!
//! # Compression
//!
//! Blobs are stored compressed. A project snapshot is canonical JSON of an
//! XML tree and compresses roughly sevenfold — a real Live Set measures 7.0 MB
//! canonical against about 1 MB gzipped — and every commit stores a full fresh
//! copy, so this is the difference between history being cheap and being
//! unaffordable.
//!
//! **Hashes are always over the uncompressed bytes.** Compression is a storage
//! detail and must never reach content identity: gzip output is not guaranteed
//! stable across encoder versions or level changes, so hashing it would mean
//! the same project could hash two ways, silently breaking deduplication and
//! forking commit ids. Callers hand [`Cas::put`] the plaintext hash and get
//! plaintext back from [`Cas::get`]; nothing above this module is aware that
//! compression happened.
//!
//! Stored blobs carry an explicit frame rather than relying on sniffing the
//! payload, because blob *content* is frequently gzip already — Ableton's own
//! `.als` autosaves are gzipped XML — and a sniffing reader would cheerfully
//! decompress one of those and hand back the wrong bytes. Blobs written before
//! framing existed are stored raw and are still read correctly; see
//! [`Cas::get`].

use std::collections::HashSet;
use std::fs;
use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;

use crate::error::{Error, Result};
use crate::hash::ContentHash;
use crate::provider::{ProjectProvider, RetentionRoots};
use crate::sample_manifest::SampleManifest;

/// Marks a stored blob as framed. Chosen to be improbable as the opening
/// bytes of real content; a collision is still handled, see [`Cas::get`].
const FRAME_MAGIC: &[u8; 4] = b"AZB1";

/// Payload stored verbatim.
const CODEC_RAW: u8 = 0;
/// Payload stored gzip-compressed.
const CODEC_GZIP: u8 = 1;

const FRAME_HEADER_LEN: usize = FRAME_MAGIC.len() + 1;

/// Wrap `bytes` in a stored-blob frame, compressing when that actually helps.
///
/// Already-compressed content — audio, images, gzipped autosaves — does not
/// shrink, and storing a larger payload to say so would be perverse. Such
/// blobs are framed with [`CODEC_RAW`] and cost four bytes of header.
fn encode_frame(bytes: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(bytes.len() + FRAME_HEADER_LEN);
    frame.extend_from_slice(FRAME_MAGIC);

    match gzip(bytes) {
        Ok(compressed) if compressed.len() < bytes.len() => {
            frame.push(CODEC_GZIP);
            frame.extend_from_slice(&compressed);
        }
        _ => {
            frame.push(CODEC_RAW);
            frame.extend_from_slice(bytes);
        }
    }
    frame
}

fn gzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

fn gunzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    io::Read::read_to_end(&mut GzDecoder::new(bytes), &mut out)?;
    Ok(out)
}

/// Undo [`encode_frame`], or `None` if `stored` is not a frame we understand.
fn decode_frame(stored: &[u8]) -> Option<Vec<u8>> {
    if stored.len() < FRAME_HEADER_LEN || !stored.starts_with(FRAME_MAGIC) {
        return None;
    }
    let payload = &stored[FRAME_HEADER_LEN..];
    match stored[FRAME_MAGIC.len()] {
        CODEC_RAW => Some(payload.to_vec()),
        CODEC_GZIP => gunzip(payload).ok(),
        _ => None,
    }
}

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
    ///
    /// `bytes` and `hash` are both in plaintext terms. What lands on disk is
    /// a compressed frame; see the module docs.
    pub fn put(&self, hash: &ContentHash, bytes: &[u8]) -> Result<()> {
        let computed = ContentHash::of(bytes);
        if computed != *hash {
            return Err(Error::Other(format!(
                "hash mismatch: caller said {hash}, content hashed to {computed}"
            )));
        }
        let path = self.path_for(hash);
        if path.exists() {
            // CAS is content-addressed — same hash means same bytes. Checked
            // before compressing, so a blob is only ever compressed once.
            return Ok(());
        }
        let bytes = &encode_frame(bytes);
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

    /// Read the plaintext bytes stored under `hash`.
    ///
    /// Handles both framed blobs and the unframed ones written before
    /// compression existed. Disambiguation is by verifying the hash rather
    /// than by trusting the frame header: content that happens to begin with
    /// the frame magic would otherwise be mistaken for a frame. Whichever
    /// interpretation hashes to `hash` is the right one, and if neither does
    /// the blob is corrupt and says so instead of returning wrong bytes.
    pub fn get(&self, hash: &ContentHash) -> Result<Vec<u8>> {
        let path = self.path_for(hash);
        let stored = fs::read(&path).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => Error::NotFound(hash.to_string()),
            _ => Error::Io(e),
        })?;

        if let Some(decoded) = decode_frame(&stored) {
            if ContentHash::of(&decoded) == *hash {
                return Ok(decoded);
            }
        }
        // Written before framing, or content that merely looks framed.
        if ContentHash::of(&stored) == *hash {
            return Ok(stored);
        }
        Err(Error::Other(format!(
            "blob {hash} is corrupt: stored bytes do not hash to their name"
        )))
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
    collect_reachable_with_roots(provider, &RetentionRoots::default()).await
}

/// [`collect_reachable`] plus client-owned roots outside visible history.
///
/// Callers running destructive GC should use this form when queued mirror
/// commits or a pre-merge stash may still depend on provider objects.
pub async fn collect_reachable_with_roots(
    provider: &dyn ProjectProvider,
    protected: &RetentionRoots,
) -> Result<HashSet<ContentHash>> {
    use crate::commit::HistoryRange;

    let mut visible_history = Vec::new();

    // `limit: None` means the provider's default page size, not unbounded.
    // Follow exclusive cursors until the provider returns an empty page.
    let mut before = None;
    let mut seen_pages = HashSet::new();
    loop {
        let history = provider
            .list_history(HistoryRange {
                limit: Some(100),
                before,
            })
            .await?;
        let Some(next_before) = history.last().map(|summary| summary.id) else {
            break;
        };

        for summary in &history {
            if !seen_pages.insert(summary.id) {
                return Err(Error::Other(format!(
                    "provider repeated commit {} while paging history",
                    summary.id.0
                )));
            }
        }
        visible_history.extend(history);
        before = Some(next_before);
    }

    let floor = visible_history.last().map(|summary| summary.id);
    let mut reachable: HashSet<ContentHash> = protected.blobs.iter().copied().collect();
    let mut pending: Vec<_> = visible_history
        .iter()
        .map(|summary| summary.id)
        .chain(protected.commits.iter().copied())
        .collect();
    let mut seen_commits = HashSet::new();
    while let Some(id) = pending.pop() {
        if !seen_commits.insert(id) {
            continue;
        }
        let commit = provider.get_commit(&id).await?;
        if Some(id) != floor {
            pending.extend(commit.parents.iter().copied());
        }

        reachable.insert(commit.tree.snapshot);
        reachable.insert(commit.tree.samples);
        if let Some(metadata) = commit.metadata {
            reachable.insert(metadata);
        }

        // Fail closed. This set authorizes deletion, so an unreadable manifest
        // means collection cannot safely continue.
        let manifest_bytes = provider.get_blob(&commit.tree.samples).await?;
        let manifest = serde_json::from_slice::<SampleManifest>(&manifest_bytes)?;
        reachable.extend(manifest.entries.iter().map(|entry| entry.hash));
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
    use crate::canonical::compute_commit_id;
    use crate::commit::{AuthorIdentity, Commit, CommitId, TreeRef};
    use crate::filesystem::FilesystemProvider;
    use crate::provider::{HeadAdvance, ProjectProvider};
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

    #[tokio::test]
    async fn reachability_should_page_through_more_than_the_provider_default() {
        let dir = TempDir::new().unwrap();
        let provider = FilesystemProvider::open(dir.path()).unwrap();
        let sample_manifest = serde_json::to_vec(&SampleManifest::default()).unwrap();
        let samples = ContentHash::of(&sample_manifest);
        provider.put_blob(&samples, &sample_manifest).await.unwrap();
        let mut parent = None;
        let mut oldest_snapshot = None;

        for index in 0..101 {
            let snapshot_bytes = format!("snapshot {index}").into_bytes();
            let snapshot = ContentHash::of(&snapshot_bytes);
            provider.put_blob(&snapshot, &snapshot_bytes).await.unwrap();
            oldest_snapshot.get_or_insert(snapshot);
            let mut commit = Commit {
                id: CommitId(ContentHash::ZERO),
                parents: parent.into_iter().collect(),
                tree: TreeRef { snapshot, samples },
                author: AuthorIdentity {
                    display_name: "Test".into(),
                    provider_user_id: "test".into(),
                    provider_id: "filesystem".into(),
                    email: None,
                },
                timestamp: index,
                message: format!("version {index}"),
                description: String::new(),
                auru_version: "test".into(),
                format_version: 8,
                metadata: None,
            };
            commit.id = compute_commit_id(&commit).unwrap();
            provider.put_commit(&commit).await.unwrap();
            assert_eq!(
                provider.advance_head(parent, commit.id).await.unwrap(),
                HeadAdvance::Advanced
            );
            parent = Some(commit.id);
        }

        let reachable = collect_reachable(&provider).await.unwrap();

        assert!(
            reachable.contains(&oldest_snapshot.unwrap()),
            "the oldest version lives on page two and must survive GC"
        );
    }

    /// Compressible filler roughly shaped like canonical snapshot JSON.
    fn snapshot_like(entries: usize) -> Vec<u8> {
        let mut json = String::from(r#"{"entries":["#);
        for index in 0..entries {
            json.push_str(&format!(
                r#"{{"tag":"MidiTrack","id":"{index}","attributes":{{"Value":"0"}}}},"#
            ));
        }
        json.push_str("null]}");
        json.into_bytes()
    }

    #[test]
    fn compressible_blobs_should_shrink_on_disk() {
        // The reason any of this exists: a snapshot is stored in full on
        // every commit, so the at-rest size is what makes history affordable.
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = snapshot_like(2_000);
        let hash = ContentHash::of(&payload);

        cas.put(&hash, &payload).unwrap();
        let on_disk = fs::metadata(cas.path_for(&hash)).unwrap().len();

        assert!(
            on_disk < payload.len() as u64 / 4,
            "expected snapshot-shaped JSON to compress well: {on_disk} vs {}",
            payload.len()
        );
        assert_eq!(
            cas.get(&hash).unwrap(),
            payload,
            "and still read back whole"
        );
    }

    #[test]
    fn hashes_should_stay_over_plaintext() {
        // Identity must not depend on the compressor. If it did, an encoder
        // change would refork every commit id and break deduplication.
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = snapshot_like(64);
        let hash = ContentHash::of(&payload);

        cas.put(&hash, &payload).unwrap();
        let stored = fs::read(cas.path_for(&hash)).unwrap();

        assert_ne!(stored, payload, "stored form should be compressed");
        assert_ne!(
            ContentHash::of(&stored),
            hash,
            "the stored bytes hash differently — which is exactly why the \
             plaintext hash is the one that names the blob"
        );
        assert!(cas.has(&hash));
    }

    #[test]
    fn already_compressed_content_should_not_be_inflated() {
        // Audio and gzipped autosaves do not shrink. Storing a larger payload
        // to record that fact would be worse than storing it plainly.
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        // Incompressible: distinct bytes with no structure to exploit.
        let payload: Vec<u8> = (0..8_192_u32)
            .flat_map(|index| index.wrapping_mul(2_654_435_761).to_le_bytes())
            .collect();
        let hash = ContentHash::of(&payload);

        cas.put(&hash, &payload).unwrap();
        let on_disk = fs::metadata(cas.path_for(&hash)).unwrap().len();

        assert!(
            on_disk <= payload.len() as u64 + FRAME_HEADER_LEN as u64,
            "incompressible content should cost at most the frame header"
        );
        assert_eq!(cas.get(&hash).unwrap(), payload);
    }

    #[test]
    fn gzip_content_should_survive_a_round_trip() {
        // The trap this frame format exists for. Ableton's own `Backup/*.als`
        // autosaves are gzipped XML, so a reader that sniffed the payload for
        // gzip magic would decompress the *content* and hand back the wrong
        // bytes entirely.
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = gzip(b"<Ableton><LiveSet /></Ableton>").unwrap();
        assert_eq!(&payload[..2], &[0x1f, 0x8b], "fixture really is gzip");

        let hash = ContentHash::of(&payload);
        cas.put(&hash, &payload).unwrap();

        assert_eq!(
            cas.get(&hash).unwrap(),
            payload,
            "a gzipped blob must come back gzipped, not decompressed"
        );
    }

    #[test]
    fn content_beginning_with_the_frame_magic_should_round_trip() {
        // Nothing stops real content starting with these four bytes. Reading
        // verifies the hash rather than trusting the header, so this resolves
        // to the right interpretation either way.
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let mut payload = FRAME_MAGIC.to_vec();
        payload.push(CODEC_GZIP);
        payload.extend_from_slice(b"not actually gzip");

        let hash = ContentHash::of(&payload);
        cas.put(&hash, &payload).unwrap();
        assert_eq!(cas.get(&hash).unwrap(), payload);
    }

    #[test]
    fn blobs_written_before_compression_should_still_be_readable() {
        // Existing stores hold unframed blobs. They must keep working without
        // a migration step.
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = b"written by an older version".to_vec();
        let hash = ContentHash::of(&payload);

        // Write it the way the previous implementation did: raw, no frame.
        let path = cas.path_for(&hash);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &payload).unwrap();

        assert!(cas.has(&hash));
        assert_eq!(cas.get(&hash).unwrap(), payload);
    }

    #[test]
    fn a_corrupt_blob_should_error_rather_than_return_wrong_bytes() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let payload = snapshot_like(16);
        let hash = ContentHash::of(&payload);
        cas.put(&hash, &payload).unwrap();

        // Truncate the stored frame behind the CAS's back.
        let path = cas.path_for(&hash);
        let stored = fs::read(&path).unwrap();
        fs::write(&path, &stored[..stored.len() / 2]).unwrap();

        assert!(
            cas.get(&hash).is_err(),
            "silently returning damaged project data is the one unacceptable outcome"
        );
    }

    #[test]
    fn an_empty_blob_should_round_trip() {
        let dir = TempDir::new().unwrap();
        let cas = Cas::open(dir.path()).unwrap();
        let hash = ContentHash::of(b"");
        cas.put(&hash, b"").unwrap();
        assert_eq!(cas.get(&hash).unwrap(), b"");
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
