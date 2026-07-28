use std::path::Path;

use auru_pm::{
    AuthorIdentity, FilesystemProvider, ProjectFormat, ProjectProvider, ProjectSnapshot,
    PushOutcome, push_with_freshness_check, restore_project, sidecar_path_for, snapshot_project,
};
use tempfile::TempDir;

const DAWPROJECT_FIXTURE: &[u8] = include_bytes!("fixtures/interchange/oracle-midi.dawproject");
const ABLETON_XML: &[u8] = br#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12">
  <LiveSet>
    <Tracks>
      <AudioTrack Id="12"><Name><EffectiveName Value="Vocals"/></Name></AudioTrack>
    </Tracks>
  </LiveSet>
</Ableton>"#;

fn author() -> AuthorIdentity {
    AuthorIdentity {
        display_name: "Format Test".to_owned(),
        provider_user_id: "format-test".to_owned(),
        provider_id: "local".to_owned(),
        email: None,
    }
}

async fn assert_pm_round_trip(format: ProjectFormat, source: &[u8], file_name: &str) {
    let root = TempDir::new().expect("temporary project root");
    let project_path = root.path().join(file_name);
    std::fs::write(&project_path, source).expect("write source project");

    let snapshot = snapshot_project(&project_path).expect("normalize source project");
    assert_eq!(snapshot.format(), format);

    let provider =
        FilesystemProvider::open(root.path().join("provider")).expect("filesystem provider");
    let provider_id = provider.provider_id();
    let outcome = push_with_freshness_check(
        &provider,
        &provider_id,
        &[],
        &sidecar_path_for(&project_path),
        snapshot.as_bytes(),
        author(),
        "External project",
        "",
    )
    .await
    .expect("commit external project");
    let PushOutcome::Committed { commit_id, .. } = outcome else {
        panic!("root project commit should not conflict");
    };

    let commit = provider
        .get_commit(&commit_id)
        .await
        .expect("fetch external project commit");
    assert_eq!(commit.format_version, 1);
    let stored = provider
        .get_blob(&commit.tree.snapshot)
        .await
        .expect("fetch external project snapshot");

    let restore_path = root.path().join(format!("restored.{}", format.extension()));
    let restored_format =
        restore_project(&stored, &restore_path).expect("restore external project file");
    assert_eq!(restored_format, format);
    assert_semantically_equal(&snapshot, &restore_path);
}

fn assert_semantically_equal(expected: &ProjectSnapshot, restored_path: &Path) {
    let restored = snapshot_project(restored_path).expect("normalize restored project");
    assert_eq!(restored.as_bytes(), expected.as_bytes());
}

#[tokio::test]
async fn dawproject_should_survive_commit_fetch_and_restore() {
    assert_pm_round_trip(
        ProjectFormat::Dawproject,
        DAWPROJECT_FIXTURE,
        "song.dawproject",
    )
    .await;
}

#[tokio::test]
async fn ableton_live_set_should_survive_commit_fetch_and_restore() {
    let source = ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, ABLETON_XML)
        .expect("normalize Ableton XML")
        .restore_bytes()
        .expect("create gzip Ableton fixture");
    assert_pm_round_trip(ProjectFormat::AbletonLiveSet, &source, "song.als").await;
}
