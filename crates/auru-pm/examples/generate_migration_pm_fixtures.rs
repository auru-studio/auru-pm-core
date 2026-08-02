use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use auru_pm::{
    AuthorIdentity, Commit, CommitId, ContentHash, RemoteState, Sidecar, TreeRef, compute_commit_id,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let output = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pm");
    fs::create_dir_all(&output)?;

    let commit = fixture_commit()?;
    fs::write(
        output.join("oracle-commit.json"),
        serde_json::to_vec_pretty(&commit)?,
    )?;

    let provider_id = "local-folder://fixture-provider".to_owned();
    let sidecar = Sidecar {
        location: None,
        metadata: auru_pm::ProjectMetadata::default(),
        primary: Some(provider_id.clone()),
        provider_handles: BTreeMap::new(),
        local_head: Some(commit.id),
        verified_head: Some(commit.id),
        remotes: BTreeMap::from([(
            provider_id,
            RemoteState {
                remote_head: Some(commit.id),
                last_pulled: Some(1_750_000_000),
            },
        )]),
        pending_pushes: vec![CommitId(ContentHash::of(b"pending-oracle-commit"))],
        // No stash: the fixture represents a settled project, and a stash
        // only exists between starting a merge and resolving it.
        stash: None,
    };
    sidecar.save(&output.join("oracle-sidecar.auru-pm.json"))?;
    Ok(())
}

fn fixture_commit() -> Result<Commit, serde_json::Error> {
    let mut commit = Commit {
        id: CommitId(ContentHash::ZERO),
        parents: vec![],
        tree: TreeRef {
            snapshot: ContentHash::of(b"oracle-project-snapshot"),
            samples: ContentHash::of(b"oracle-sample-manifest"),
        },
        author: AuthorIdentity {
            display_name: "Oracle Author".to_owned(),
            provider_user_id: "oracle-user".to_owned(),
            provider_id: "local-folder://fixture-provider".to_owned(),
            email: None,
        },
        timestamp: 1_750_000_000,
        message: "Freeze migration fixture".to_owned(),
        description: "Deterministic PM state for egui-to-GPUI parity.".to_owned(),
        auru_version: "egui-oracle-v1".to_owned(),
        format_version: 8,
        // The fixture predates project summaries and must keep the commit id
        // it was generated with — see `pm_fixtures`.
        metadata: None,
    };
    commit.id = compute_commit_id(&commit)?;
    Ok(commit)
}
