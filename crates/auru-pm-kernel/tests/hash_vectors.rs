//! Conformance vectors for the content-hash function.
//!
//! Commit ids are the BLAKE3 of canonical bytes, so an SDK that implements
//! BLAKE3 itself — rather than binding to this crate — has to get it exactly
//! right, and a subtle error would surface as a provider rejecting a commit
//! rather than as anything legible.
//!
//! The lengths here are the ones where BLAKE3's structure changes: the 64-byte
//! compression block, the 1024-byte chunk, and the points where the Merkle tree
//! gains a level. A port that handles a short string correctly and nothing else
//! passes no test worth having.
//!
//! Regenerate after an intentional change with:
//!
//! ```sh
//! UPDATE_SPEC_VECTORS=1 cargo test -p auru-pm-kernel --test hash_vectors
//! ```

use std::path::PathBuf;

use auru_pm_kernel::ContentHash;
use serde_json::json;

/// The input pattern from BLAKE3's own test suite: byte `i` is `i % 251`.
///
/// Described by length rather than written out, so the vector file stays
/// readable and a 100 kB case costs one line.
fn input(length: usize) -> Vec<u8> {
    (0..length).map(|index| (index % 251) as u8).collect()
}

const LENGTHS: &[usize] = &[
    0, 1, 2, 3, 31, 32, 63, 64, 65, 127, 128, 129, 255, 256, 257, 511, 512, 513, 1023, 1024, 1025,
    2047, 2048, 2049, 3072, 3073, 4095, 4096, 4097, 8192, 16385, 31744, 100_000,
];

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../spec/vectors/content-hash.json")
}

fn document() -> serde_json::Value {
    json!({
        "hash": "blake3, rendered as `blake3:<64 lowercase hex>`",
        "input": concat!(
            "Byte i of an input of length n is `i % 251`, the pattern BLAKE3's own test ",
            "suite uses. Lengths cover the 64-byte block, the 1024-byte chunk, and each ",
            "point where the Merkle tree gains a level.",
        ),
        "cases": LENGTHS
            .iter()
            .map(|&length| {
                json!({
                    "length": length,
                    "hash": ContentHash::of(&input(length)).to_string(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

#[test]
fn published_hash_vectors_are_current() {
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
             UPDATE_SPEC_VECTORS=1 cargo test -p auru-pm-kernel --test hash_vectors",
            path.display()
        )
    });

    assert_eq!(
        published,
        generated,
        "\n{} is stale; the content-hash function changed",
        path.display()
    );
}

#[test]
fn the_lengths_cover_every_structural_boundary() {
    // A port that only handles inputs inside one chunk would pass a careless
    // list, so assert the interesting sizes are actually present.
    for boundary in [64_usize, 1024, 1025, 2048, 4096] {
        assert!(
            LENGTHS.contains(&boundary),
            "missing the {boundary}-byte boundary"
        );
    }
    assert!(LENGTHS.iter().any(|&length| length > 65_536));
}
