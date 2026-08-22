//! Every operation the bindings expose, defined once.
//!
//! The shapes crossing the boundary are deliberately boring: `&[u8]` for
//! payloads a caller already holds as bytes — snapshots, blobs — and JSON
//! strings for structured results, which are small. A 7 MB Live Set snapshot
//! therefore crosses as bytes and is parsed here, never serialized to a string
//! on the way in.
//!
//! Errors are `String`. Both bindings turn them into a native exception, and
//! the kernel's error text already says what failed.

use auru_pm_kernel::{
    Commit, ContentHash, ProjectFormat, ProjectInfo, ProjectSnapshot, canonical_encoding,
    compute_commit_id, merge3_json_bytes, structured_diff, summarize_diff,
};
use serde_json::Value;

/// Wire protocol version this build speaks.
///
/// A client refuses to talk to a provider whose `/v1/health` reports anything
/// else.
pub fn protocol_version() -> &'static str {
    auru_pm_kernel::WIRE_VERSION
}

fn parse_json(bytes: &[u8], what: &str) -> Result<Value, String> {
    serde_json::from_slice(bytes).map_err(|error| format!("parse {what}: {error}"))
}

fn to_json<T: serde::Serialize>(value: &T) -> Result<String, String> {
    serde_json::to_string(value).map_err(|error| format!("encode result: {error}"))
}

// ── Commit identity ──────────────────────────────────────────────────────────

/// The canonical bytes whose BLAKE3 is a commit's id.
///
/// RFC 8785 over the commit with its `id` member removed. Exposed as well as
/// [`commit_id`] so a client can show, log, or diff exactly what it hashed when
/// a provider rejects a commit.
pub fn commit_canonical_encoding(commit_json: &str) -> Result<Vec<u8>, String> {
    let commit: Commit =
        serde_json::from_str(commit_json).map_err(|error| format!("parse commit: {error}"))?;
    canonical_encoding(&commit).map_err(|error| format!("canonicalize commit: {error}"))
}

/// Derive a commit's id from its content.
///
/// Providers recompute this and reject a mismatch, so a client must call it
/// rather than trusting an `id` it was handed.
pub fn commit_id(commit_json: &str) -> Result<String, String> {
    let commit: Commit =
        serde_json::from_str(commit_json).map_err(|error| format!("parse commit: {error}"))?;
    compute_commit_id(&commit)
        .map(|id| id.0.to_string())
        .map_err(|error| format!("canonicalize commit: {error}"))
}

// ── Content addressing ───────────────────────────────────────────────────────

/// BLAKE3 of `bytes`, in the canonical `blake3:<64 hex>` form.
pub fn content_hash(bytes: &[u8]) -> String {
    ContentHash::of(bytes).to_string()
}

/// Whether `bytes` hash to `expected`.
///
/// The check every client owes itself on a downloaded blob, before using it.
pub fn verify_blob(bytes: &[u8], expected: &str) -> Result<bool, String> {
    let expected: ContentHash = expected
        .parse()
        .map_err(|error| format!("parse hash: {error}"))?;
    Ok(ContentHash::of(bytes) == expected)
}

// ── Reading a project ────────────────────────────────────────────────────────

/// The few-kilobyte summary of what a snapshot *is*.
///
/// `None` when the snapshot is of a format this build cannot summarize; the
/// caller then falls back to reading the snapshot itself.
///
/// This is the call a dashboard should reach for first. A commit points at a
/// stored `ProjectInfo` blob precisely so a project list never has to fetch the
/// snapshot, which for a real Live Set is around 7 MB.
pub fn project_info_from_snapshot(snapshot: &[u8]) -> Result<Option<String>, String> {
    match ProjectInfo::from_snapshot_bytes(snapshot) {
        Some(info) => to_json(&info).map(Some),
        None => Ok(None),
    }
}

/// Parse an already-stored `ProjectInfo` blob.
pub fn project_info_from_blob(blob: &[u8]) -> Result<String, String> {
    let info: ProjectInfo =
        serde_json::from_slice(blob).map_err(|error| format!("parse project info: {error}"))?;
    to_json(&info)
}

/// Which format a project file is, from its name and leading bytes.
///
/// Returns the wire value — `ableton-live-set`, `fl-studio`, `dawproject`,
/// `auru`, `bitwig-project`.
pub fn detect_format(file_name: &str, source: &[u8]) -> Result<String, String> {
    let format = ProjectFormat::detect(std::path::Path::new(file_name), source)
        .map_err(|error| format!("detect format: {error}"))?;
    to_json(&format).map(|quoted| quoted.trim_matches('"').to_owned())
}

/// Normalize a project file into canonical snapshot bytes.
pub fn snapshot_from_source(format: &str, source: &[u8]) -> Result<Vec<u8>, String> {
    let format: ProjectFormat = serde_json::from_str(&format!("\"{format}\""))
        .map_err(|error| format!("unknown project format {format:?}: {error}"))?;
    let snapshot = ProjectSnapshot::from_source_bytes(format, source)
        .map_err(|error| format!("snapshot project: {error}"))?;
    Ok(snapshot.as_bytes().to_vec())
}

/// Rebuild the original project file from canonical snapshot bytes.
///
/// Returns the bytes rather than writing them: a browser has nowhere to write,
/// and in Node the caller already knows where the file belongs.
pub fn restore_from_snapshot(canonical: &[u8]) -> Result<Vec<u8>, String> {
    ProjectSnapshot::from_canonical_bytes(canonical)
        .map_err(|error| format!("read snapshot: {error}"))?
        .restore_bytes()
        .map_err(|error| format!("restore project: {error}"))
}

// ── Comparing versions ───────────────────────────────────────────────────────

/// Per-channel structured diff between two canonical snapshots.
pub fn diff_snapshots(before: &[u8], after: &[u8]) -> Result<String, String> {
    let before = parse_json(before, "the earlier snapshot")?;
    let after = parse_json(after, "the later snapshot")?;
    to_json(&structured_diff(&before, &after))
}

/// One-line-per-change summary between two canonical snapshots.
pub fn summarize_snapshots(before: &[u8], after: &[u8]) -> Result<String, String> {
    let before = parse_json(before, "the earlier snapshot")?;
    let after = parse_json(after, "the later snapshot")?;
    to_json(&summarize_diff(&before, &after))
}

/// Three-way merge of canonical snapshots.
///
/// The result is a tagged union: `{"outcome":"clean","merged":…}` or
/// `{"outcome":"conflict","base":…,"conflicts":[…]}`.
pub fn merge_snapshots(ancestor: &[u8], local: &[u8], remote: &[u8]) -> Result<String, String> {
    let outcome = merge3_json_bytes(ancestor, local, remote)
        .map_err(|error| format!("parse snapshots for merge: {error}"))?;
    to_json(&outcome)
}

#[cfg(test)]
mod tests {
    use super::*;

    const COMMIT: &str = r#"{
        "id": "blake3:0000000000000000000000000000000000000000000000000000000000000000",
        "parents": [],
        "tree": {
            "snapshot": "blake3:2d168e1ff20930e41ae0c8e9b0bfc97bba39b1afb272f6bf9b52bf30716e96a5",
            "samples": "blake3:5f21e8a045666d350bf53517da2ada8c5bfb5e280c7e5f3f7c3c86e4d9a0b9cc"
        },
        "author": {
            "display_name": "Test User",
            "provider_user_id": "user-1",
            "provider_id": "local-folder"
        },
        "timestamp": 1700000000,
        "message": "first take",
        "description": "",
        "auru_version": "0.1.0",
        "format_version": 8
    }"#;

    #[test]
    fn commit_id_matches_the_published_vector() {
        // The same case as `spec/vectors/commit-encoding.json`, so a binding
        // that drifts from the spec fails here rather than at a provider.
        assert_eq!(
            commit_id(COMMIT).unwrap(),
            "blake3:8189ac12713762dcb92c060a1db950b532c06dc10ac9a83d05344a67f25906e7"
        );
    }

    #[test]
    fn canonical_encoding_strips_the_id_and_sorts() {
        let bytes = commit_canonical_encoding(COMMIT).unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(!text.contains("\"id\""), "id leaked: {text}");
        assert!(text.starts_with("{\"auru_version\""));
    }

    #[test]
    fn blob_verification_rejects_the_wrong_bytes() {
        let hash = content_hash(b"audio");
        assert!(verify_blob(b"audio", &hash).unwrap());
        assert!(!verify_blob(b"different", &hash).unwrap());
        assert!(verify_blob(b"audio", "not-a-hash").is_err());
    }

    #[test]
    fn a_snapshot_round_trips_through_the_surface() {
        let source = br#"{"version":8,"channels":[]}"#;
        let canonical = snapshot_from_source("auru", source).unwrap();
        assert_eq!(detect_format("song.auru", source).unwrap(), "auru");
        let restored = restore_from_snapshot(&canonical).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&restored).unwrap(),
            serde_json::from_slice::<Value>(source).unwrap()
        );
    }

    #[test]
    fn an_unknown_format_is_refused_by_name() {
        let error = snapshot_from_source("logic-pro", b"{}").unwrap_err();
        assert!(error.contains("unknown project format"), "{error}");
    }

    #[test]
    fn merging_reports_a_clean_outcome_as_a_tagged_union() {
        let base = br#"{"version":8,"tempo":120}"#;
        let ours = br#"{"version":8,"tempo":128}"#;
        let theirs = br#"{"version":8,"tempo":120}"#;
        let merged: Value =
            serde_json::from_str(&merge_snapshots(base, ours, theirs).unwrap()).unwrap();
        assert_eq!(merged["outcome"], "clean");
        assert_eq!(merged["merged"]["tempo"], 128);
    }

    #[test]
    fn merging_reports_conflicts_with_their_paths() {
        let base = br#"{"version":8,"tempo":120}"#;
        let ours = br#"{"version":8,"tempo":128}"#;
        let theirs = br#"{"version":8,"tempo":140}"#;
        let merged: Value =
            serde_json::from_str(&merge_snapshots(base, ours, theirs).unwrap()).unwrap();
        assert_eq!(merged["outcome"], "conflict");
        assert_eq!(merged["conflicts"][0]["path"], "tempo");
    }
}
