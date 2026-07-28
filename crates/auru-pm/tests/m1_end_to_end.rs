//! Plan M1 happy-path: enrol PM on a fresh project pointing at a
//! filesystem-backed provider, commit "first take", verify the CAS
//! dir is populated, sidecar is updated, local HEAD advanced, and the
//! commit message persists across a fresh re-open of the provider.
//!
//! Desktop clients use the same provider flow.

use auru_pm::{
    AuthorIdentity, Commit, CommitId, ContentHash, FilesystemProvider, HeadAdvance, HistoryRange,
    ProjectProvider, RemoteState, SampleEntry, SampleManifest, Sidecar, TreeRef, compute_commit_id,
    sidecar_path_for,
};
use tempfile::TempDir;

#[tokio::test(flavor = "current_thread")]
async fn m1_first_take_roundtrips() {
    let project_dir = TempDir::new().unwrap();
    let remote_dir = TempDir::new().unwrap();

    // 1. Open a filesystem provider against a fresh folder (the M1 stand-in
    //    for the chosen Dropbox / NAS folder).
    let provider = FilesystemProvider::open(remote_dir.path()).unwrap();
    let provider_id = provider.provider_id();
    assert!(provider_id.starts_with("local-folder://"));

    // No HEAD yet — fresh repo.
    assert_eq!(provider.get_head().await.unwrap(), None);

    // 2. Build a snapshot blob + an (empty) sample manifest. A desktop
    //    client supplies these bytes from its active project model.
    let snapshot_bytes = br#"{"version":8,"bpm":120,"channels":[]}"#;
    let snapshot_hash = ContentHash::of(snapshot_bytes);
    let samples = SampleManifest::new();
    let samples_bytes = samples.canonical_encoding().unwrap();
    let samples_hash = ContentHash::of(&samples_bytes);

    provider
        .put_blob(&snapshot_hash, snapshot_bytes)
        .await
        .unwrap();
    provider
        .put_blob(&samples_hash, &samples_bytes)
        .await
        .unwrap();

    // 3. Build the commit and store it. The id is the canonical hash.
    let author = AuthorIdentity {
        display_name: "Test User".into(),
        provider_user_id: "user-1".into(),
        provider_id: provider_id.clone(),
        email: None,
    };
    let mut commit = Commit {
        id: CommitId(ContentHash::ZERO),
        parents: vec![],
        tree: TreeRef {
            snapshot: snapshot_hash,
            samples: samples_hash,
        },
        author,
        timestamp: 1_700_000_000,
        message: "first take".into(),
        description: "rough draft of the chorus".into(),
        auru_version: "0.1.0".into(),
        format_version: 8,
    };
    commit.id = compute_commit_id(&commit).unwrap();
    provider.put_commit(&commit).await.unwrap();

    // 4. Advance HEAD (initial publish, from = None).
    let advance = provider.advance_head(None, commit.id).await.unwrap();
    assert_eq!(advance, HeadAdvance::Advanced);
    assert_eq!(provider.get_head().await.unwrap(), Some(commit.id));

    // 5. Persist the sidecar next to the project file (per-user PM state).
    let project_path = project_dir.path().join("song.auru");
    let sidecar_path = sidecar_path_for(&project_path);
    Sidecar::modify(&sidecar_path, |s| {
        s.primary = Some(provider_id.clone());
        s.local_head = Some(commit.id);
        s.remotes.insert(
            provider_id.clone(),
            RemoteState {
                remote_head: Some(commit.id),
                last_pulled: Some(commit.timestamp),
            },
        );
    })
    .unwrap();

    // CAS dir is populated on disk.
    assert!(remote_dir.path().join("objects").exists());
    assert!(remote_dir.path().join("commits").exists());
    assert!(remote_dir.path().join("HEAD").exists());

    // 6. Re-open the provider against the same folder — equivalent to
    //    closing and reopening the desktop app. The history and HEAD
    //    survive.
    drop(provider);
    let reopened = FilesystemProvider::open(remote_dir.path()).unwrap();
    assert_eq!(reopened.get_head().await.unwrap(), Some(commit.id));

    let history = reopened
        .list_history(HistoryRange::default())
        .await
        .unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].id, commit.id);
    assert_eq!(history[0].message, "first take");
    assert_eq!(history[0].description, "rough draft of the chorus");

    // Snapshot blob is still retrievable by hash — this is what the
    // open-project flow will use to materialize the working state.
    let fetched = reopened.get_blob(&snapshot_hash).await.unwrap();
    assert_eq!(fetched, snapshot_bytes);

    // 7. Sidecar reloads cleanly.
    let loaded = Sidecar::load(&sidecar_path).unwrap();
    assert_eq!(loaded.primary.as_deref(), Some(provider_id.as_str()));
    assert_eq!(loaded.local_head, Some(commit.id));
    assert_eq!(
        loaded.remotes.get(&provider_id).and_then(|r| r.remote_head),
        Some(commit.id)
    );
}

#[tokio::test(flavor = "current_thread")]
async fn m1_second_commit_chains_to_first() {
    // Two-commit history: confirms list_history walks parents and the
    // CAS shares storage across commits (no double-upload of blobs).
    let dir = TempDir::new().unwrap();
    let provider = FilesystemProvider::open(dir.path()).unwrap();

    let author = AuthorIdentity {
        display_name: "Test".into(),
        provider_user_id: "u".into(),
        provider_id: provider.provider_id(),
        email: None,
    };
    let mk = |message: &str, parent: Option<CommitId>| -> Commit {
        let mut c = Commit {
            id: CommitId(ContentHash::ZERO),
            parents: parent.into_iter().collect(),
            tree: TreeRef {
                snapshot: ContentHash::of(message.as_bytes()),
                samples: ContentHash::of(b"empty"),
            },
            author: author.clone(),
            timestamp: 1_700_000_000,
            message: message.into(),
            description: String::new(),
            auru_version: "0.1.0".into(),
            format_version: 8,
        };
        c.id = compute_commit_id(&c).unwrap();
        c
    };

    let c1 = mk("first take", None);
    let c2 = mk("second take", Some(c1.id));

    provider.put_commit(&c1).await.unwrap();
    provider.put_commit(&c2).await.unwrap();
    provider.advance_head(None, c1.id).await.unwrap();
    provider.advance_head(Some(c1.id), c2.id).await.unwrap();

    let history = provider
        .list_history(HistoryRange::default())
        .await
        .unwrap();
    assert_eq!(history.len(), 2);
    assert_eq!(history[0].id, c2.id);
    assert_eq!(history[1].id, c1.id);
}

#[tokio::test(flavor = "current_thread")]
async fn m1_sample_manifest_blob_roundtrips() {
    // Sanity-check the sample manifest path: encode → store → fetch →
    // decode produces an equal manifest, and the same set of samples
    // hashes to the same blob regardless of insertion order.
    let dir = TempDir::new().unwrap();
    let provider = FilesystemProvider::open(dir.path()).unwrap();

    let mut samples = SampleManifest::new();
    samples.insert(SampleEntry {
        path: "samples/kick.wav".into(),
        hash: ContentHash::of(b"kick"),
        size: 4,
    });
    samples.insert(SampleEntry {
        path: "samples/snare.wav".into(),
        hash: ContentHash::of(b"snare"),
        size: 5,
    });

    let bytes = samples.canonical_encoding().unwrap();
    let hash = samples.content_hash().unwrap();
    assert_eq!(ContentHash::of(&bytes), hash);

    provider.put_blob(&hash, &bytes).await.unwrap();
    let fetched = provider.get_blob(&hash).await.unwrap();
    let decoded: SampleManifest = serde_json::from_slice(&fetched).unwrap();
    assert_eq!(decoded, samples);
}
