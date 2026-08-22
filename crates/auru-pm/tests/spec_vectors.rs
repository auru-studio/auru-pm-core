//! Conformance vectors for the `auru-pm-v1` canonical commit encoding.
//!
//! Two jobs, one corpus:
//!
//! 1. **Migration proof.** The canonical rule moved from a house rule
//!    (`serde_json` plus recursively sorted keys) to RFC 8785 (JCS). That
//!    re-derives every commit id, so the switch was only safe if it changed no
//!    bytes. These tests hold that evidence rather than leaving it in a commit
//!    message.
//! 2. **Published contract.** The same corpus is written to
//!    `spec/vectors/commit-encoding.json`, which every non-Rust SDK verifies
//!    against. A vector that passes here and fails in TypeScript is exactly the
//!    failure the spec exists to catch.
//!
//! Regenerate after an intentional change with:
//!
//! ```sh
//! UPDATE_SPEC_VECTORS=1 cargo test -p auru-pm --test spec_vectors
//! ```

use std::path::PathBuf;

use auru_pm::{
    AuthorIdentity, Commit, CommitId, ContentHash, TreeRef, canonical_encoding, compute_commit_id,
};
use serde_json::{Value, json};

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/vectors/commit-encoding.json")
}

/// The pre-JCS rule, kept only so the migration stays provable.
fn legacy_encoding(commit: &Commit) -> Vec<u8> {
    fn sort_json_objects(value: &mut Value) {
        match value {
            Value::Object(map) => {
                for child in map.values_mut() {
                    sort_json_objects(child);
                }
                map.sort_keys();
            }
            Value::Array(values) => {
                for child in values {
                    sort_json_objects(child);
                }
            }
            _ => {}
        }
    }
    let mut value = serde_json::to_value(commit).expect("to_value");
    if let Value::Object(map) = &mut value {
        map.remove("id");
    }
    sort_json_objects(&mut value);
    serde_json::to_vec(&value).expect("to_vec")
}

fn base() -> Commit {
    Commit {
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

/// Commits a real client could produce, chosen to sit on every edge where two
/// JSON serializers are likely to disagree.
fn corpus() -> Vec<(&'static str, Commit)> {
    let mut cases = Vec::new();

    cases.push(("baseline", base()));

    let mut merge = base();
    merge.parents = vec![
        CommitId(ContentHash::of(b"left")),
        CommitId(ContentHash::of(b"right")),
    ];
    cases.push(("merge commit", merge));

    let mut root = base();
    root.parents = vec![];
    root.message = "initial version".into();
    cases.push(("root commit", root));

    let mut with_metadata = base();
    with_metadata.metadata = Some(ContentHash::of(b"project info"));
    cases.push(("metadata present", with_metadata));

    let mut with_email = base();
    with_email.author.email = Some("user@example.com".into());
    cases.push(("author email present", with_email));

    // Free text is the only place a user can put arbitrary bytes, so it carries
    // all of the escaping risk.
    let mut unicode = base();
    unicode.message = "renamed Café → Café (NFC vs NFD)".into();
    unicode.description = "日本語のテキスト, ελληνικά, עברית".into();
    cases.push(("non-ascii text", unicode));

    let mut astral = base();
    astral.message = "🎛️ mixdown 🥁 v2 𝄞".into();
    astral.author.display_name = "𝕁𝕒𝕜𝕖".into();
    cases.push(("astral plane (surrogate pairs)", astral));

    let mut escapes = base();
    escapes.message = "quote \" backslash \\ slash / tab \t newline \n".into();
    escapes.description = "carriage \r backspace \u{8} formfeed \u{c}".into();
    cases.push(("json escapes", escapes));

    let mut controls = base();
    controls.message = "null \u{0} unit \u{1f} del \u{7f} nbsp \u{a0}".into();
    cases.push(("control characters", controls));

    let mut empty = base();
    empty.message = String::new();
    empty.description = String::new();
    empty.auru_version = String::new();
    empty.author.display_name = String::new();
    cases.push(("empty strings", empty));

    let mut zeros = base();
    zeros.timestamp = 0;
    zeros.format_version = 0;
    cases.push(("zero numbers", zeros));

    let mut negative = base();
    negative.timestamp = -1_700_000_000;
    cases.push(("negative timestamp", negative));

    let mut big = base();
    big.timestamp = 253_402_300_799; // year 9999
    big.format_version = u32::MAX;
    cases.push(("far-future timestamp, max format_version", big));

    let mut safe_int = base();
    safe_int.timestamp = 9_007_199_254_740_991; // 2^53 - 1, the documented bound
    cases.push(("largest safe integer", safe_int));

    cases
}

/// Each case as it appears in the published vector file.
fn rendered_cases() -> Vec<Value> {
    corpus()
        .into_iter()
        .map(|(name, mut commit)| {
            let id = compute_commit_id(&commit).expect("compute id");
            commit.id = id;
            let canonical = canonical_encoding(&commit).expect("encode");
            json!({
                "name": name,
                "commit": serde_json::to_value(&commit).expect("to_value"),
                "canonical": String::from_utf8(canonical).expect("utf-8"),
                "id": id.0.to_string(),
            })
        })
        .collect()
}

fn document() -> Value {
    json!({
        "rule": "RFC 8785 (JCS) over the commit object with the `id` member removed",
        "hash": "blake3, rendered as `blake3:<64 lowercase hex>`",
        "integer_bound": "every integer must lie within +/- (2^53 - 1)",
        "note": concat!(
            "An implementation is conformant when, for every case, canonicalizing ",
            "`commit` reproduces `canonical` byte-for-byte and hashing those bytes ",
            "reproduces `id`.",
        ),
        "cases": rendered_cases(),
    })
}

#[test]
fn jcs_matches_the_legacy_rule_for_every_realistic_commit() {
    let mut divergent = Vec::new();
    for (name, commit) in corpus() {
        let legacy = legacy_encoding(&commit);
        let jcs = canonical_encoding(&commit).expect("encode");
        if legacy != jcs {
            divergent.push(format!(
                "{name}\n  legacy: {}\n  jcs:    {}",
                String::from_utf8_lossy(&legacy),
                String::from_utf8_lossy(&jcs)
            ));
        }
    }
    assert!(
        divergent.is_empty(),
        "the JCS migration changed the canonical bytes for {} case(s):\n{}",
        divergent.len(),
        divergent.join("\n")
    );
}

#[test]
fn commit_ids_survived_the_migration() {
    for (name, commit) in corpus() {
        let legacy_id = ContentHash::of(&legacy_encoding(&commit));
        let jcs_id = compute_commit_id(&commit).expect("compute id").0;
        assert_eq!(legacy_id, jcs_id, "id changed for case: {name}");
    }
}

/// Records where the two rules part company, so the spec can state the bound
/// rather than leave it to be discovered by an implementer.
#[test]
fn integers_beyond_2_53_are_outside_the_contract() {
    let mut beyond = base();
    beyond.timestamp = i64::MAX;
    let legacy = String::from_utf8(legacy_encoding(&beyond)).unwrap();
    let jcs = String::from_utf8(canonical_encoding(&beyond).unwrap()).unwrap();
    assert!(legacy.contains("9223372036854775807"));
    assert!(
        jcs.contains("9223372036854776000"),
        "JCS is expected to round i64::MAX through binary64; got {jcs}"
    );
}

#[test]
fn every_case_hashes_to_its_recorded_id() {
    for case in rendered_cases() {
        let canonical = case["canonical"].as_str().expect("canonical");
        let recorded = case["id"].as_str().expect("id");
        assert_eq!(
            ContentHash::of(canonical.as_bytes()).to_string(),
            recorded,
            "case {} does not hash to its recorded id",
            case["name"]
        );
    }
}

#[test]
fn published_vectors_are_current() {
    let path = vectors_path();
    let generated = format!(
        "{}\n",
        serde_json::to_string_pretty(&document()).expect("serialize")
    );

    if std::env::var_os("UPDATE_SPEC_VECTORS").is_some() {
        std::fs::create_dir_all(path.parent().expect("parent")).expect("create spec/vectors");
        std::fs::write(&path, &generated).expect("write vectors");
        return;
    }

    let published = std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!(
            "{} is missing ({error}). Regenerate with \
             UPDATE_SPEC_VECTORS=1 cargo test -p auru-pm --test spec_vectors",
            path.display()
        )
    });

    assert_eq!(
        published,
        generated,
        "\n{} is stale. If the canonical encoding changed on purpose, every \
         existing commit id changes with it — confirm that is intended, then \
         regenerate with UPDATE_SPEC_VECTORS=1 cargo test -p auru-pm --test spec_vectors\n",
        path.display()
    );
}
