//! Canonical encoding of [`Commit`]s for content-addressed identity.
//!
//! Every commit's `id` is the blake3 of its canonical JSON encoding —
//! the same bytes a third-party provider implementation re-hashes to
//! verify the id matches. Determinism is the whole point: two clients
//! that build the same logical commit must produce byte-identical
//! encodings.
//!
//! We get determinism by round-tripping through [`serde_json::Value`],
//! explicitly sorting every object, and re-serializing. Explicit sorting is
//! required because Cargo feature unification can enable serde_json's
//! `preserve_order` feature through another workspace crate. The `id` field is
//! stripped before hashing so the commit's identity is a function of its
//! content, not of itself.

use crate::commit::{Commit, CommitId};
use crate::hash::ContentHash;

/// Encode a commit for hashing.
///
/// Produces the canonical byte sequence whose blake3 hash IS the commit
/// id. The `id` field on the input is ignored — it's the value we are
/// (re)computing.
pub fn canonical_encoding(commit: &Commit) -> Result<Vec<u8>, serde_json::Error> {
    let mut value = serde_json::to_value(commit)?;
    if let serde_json::Value::Object(map) = &mut value {
        map.remove("id");
    }
    sort_json_objects(&mut value);
    serde_json::to_vec(&value)
}

fn sort_json_objects(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for child in map.values_mut() {
                sort_json_objects(child);
            }
            map.sort_keys();
        }
        serde_json::Value::Array(values) => {
            for child in values {
                sort_json_objects(child);
            }
        }
        _ => {}
    }
}

/// Compute the canonical id of a commit. Returned id does NOT have to
/// equal `commit.id` — callers compare to verify integrity.
pub fn compute_commit_id(commit: &Commit) -> Result<CommitId, serde_json::Error> {
    let bytes = canonical_encoding(commit)?;
    Ok(CommitId(ContentHash::of(&bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commit::{AuthorIdentity, Commit, TreeRef};

    fn fixture() -> Commit {
        Commit {
            // Placeholder — the id is recomputed below.
            id: CommitId(ContentHash::ZERO),
            parents: vec![],
            tree: TreeRef {
                snapshot: ContentHash::of(b"snapshot"),
                samples: ContentHash::of(b"samples"),
            },
            author: AuthorIdentity {
                display_name: "Test User".into(),
                provider_user_id: "user-1".into(),
                provider_id: "local-folder".into(),
                email: None,
            },
            timestamp: 1_700_000_000,
            message: "first take".into(),
            description: String::new(),
            auru_version: "0.1.0".into(),
            format_version: 8,
            metadata: None,
        }
    }

    #[test]
    fn deterministic() {
        let mut a = fixture();
        let mut b = fixture();
        // The id field must not affect canonical encoding.
        a.id = CommitId(ContentHash::ZERO);
        b.id = CommitId(ContentHash::of(b"different"));
        let enc_a = canonical_encoding(&a).unwrap();
        let enc_b = canonical_encoding(&b).unwrap();
        assert_eq!(enc_a, enc_b);
    }

    #[test]
    fn changes_with_message() {
        let mut a = fixture();
        let mut b = fixture();
        a.message = "first take".into();
        b.message = "second take".into();
        assert_ne!(
            compute_commit_id(&a).unwrap(),
            compute_commit_id(&b).unwrap()
        );
    }

    #[test]
    fn id_field_is_stripped() {
        let commit = fixture();
        let bytes = canonical_encoding(&commit).unwrap();
        let as_str = std::str::from_utf8(&bytes).unwrap();
        assert!(!as_str.contains("\"id\""), "id leaked: {as_str}");
    }

    #[test]
    fn keys_sorted() {
        // Top-level keys appear in alphabetical order. Spot-check a few
        // that would otherwise depend on struct field declaration.
        let bytes = canonical_encoding(&fixture()).unwrap();
        let s = std::str::from_utf8(&bytes).unwrap();
        let pos_auru = s.find("\"auru_version\"").unwrap();
        let pos_author = s.find("\"author\"").unwrap();
        let pos_tree = s.find("\"tree\"").unwrap();
        assert!(pos_auru < pos_author);
        assert!(pos_author < pos_tree);
    }
}
