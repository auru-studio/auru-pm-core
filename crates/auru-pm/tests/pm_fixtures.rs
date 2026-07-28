use std::fs;
use std::path::PathBuf;

use auru_pm::{Commit, Sidecar, compute_commit_id};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/pm")
        .join(name)
}

#[test]
fn commit_fixture_retains_canonical_identity() {
    let bytes = fs::read(fixture("oracle-commit.json")).unwrap();
    let commit: Commit = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(compute_commit_id(&commit).unwrap(), commit.id);
}

#[test]
fn sidecar_fixture_retains_sync_state() {
    let sidecar = Sidecar::load(&fixture("oracle-sidecar.auru-pm.json")).unwrap();
    let primary = sidecar.primary.as_deref().unwrap();
    assert_eq!(primary, "local-folder://fixture-provider");
    assert_eq!(sidecar.local_head, sidecar.remotes[primary].remote_head);
    assert_eq!(sidecar.pending_pushes.len(), 1);
}

#[test]
fn pm_fixtures_never_persist_credentials() {
    for name in ["oracle-commit.json", "oracle-sidecar.auru-pm.json"] {
        let body = fs::read_to_string(fixture(name)).unwrap().to_lowercase();
        assert!(!body.contains("token"), "{name} contains token material");
        assert!(
            !body.contains("credential"),
            "{name} contains credential material"
        );
    }
}
