//! Every commit carrying a small summary of what its project is.
//!
//! The point is that a client can show a library of projects — tempo, key,
//! track count — without downloading a snapshot per project. A real Live Set
//! snapshot is around 7 MB; the summary is a couple of kilobytes.

use auru_pm::{
    AuthorIdentity, Cas, FilesystemProvider, ProjectProvider, ProjectSnapshot, PushOutcome,
    collect_reachable, fetch_project_info, push_with_freshness_check, sidecar_path_for,
};
use tempfile::TempDir;

const LIVE_SET_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12.0.25">
  <LiveSet>
    <MainTrack><DeviceChain><Mixer>
      <Tempo><Manual Value="175" /></Tempo>
      <TimeSignature><Manual Value="201" /></TimeSignature>
    </Mixer></DeviceChain></MainTrack>
    <ScaleInformation><RootNote Value="0" /><Name Value="Phrygian" /></ScaleInformation>
    <InKey Value="true" />
    <Tracks>
      <MidiTrack Id="1"><Name><EffectiveName Value="Reese" /></Name></MidiTrack>
      <AudioTrack Id="2"><Name><EffectiveName Value="Break" /></Name></AudioTrack>
      <ReturnTrack Id="3"><Name><EffectiveName Value="A-Reverb" /></Name></ReturnTrack>
    </Tracks>
  </LiveSet>
</Ableton>"#;

fn author() -> AuthorIdentity {
    AuthorIdentity {
        display_name: "Info Test".to_owned(),
        provider_user_id: "info-test".to_owned(),
        provider_id: "local".to_owned(),
        email: None,
    }
}

/// Commit `snapshot_bytes` under `file_name` and return the provider + commit.
async fn commit_project(
    file_name: &str,
    snapshot_bytes: &[u8],
) -> (TempDir, FilesystemProvider, auru_pm::Commit) {
    let root = TempDir::new().expect("tempdir");
    let provider = FilesystemProvider::open(root.path().join("provider")).expect("provider");
    let sidecar = sidecar_path_for(&root.path().join(file_name));

    let PushOutcome::Committed { commit_id, .. } = push_with_freshness_check(
        &provider,
        &provider.provider_id(),
        &[],
        &sidecar,
        snapshot_bytes,
        author(),
        "First version",
        "",
    )
    .await
    .expect("push") else {
        panic!("a first commit cannot conflict");
    };

    let commit = provider.get_commit(&commit_id).await.expect("get commit");
    (root, provider, commit)
}

fn live_set_snapshot() -> Vec<u8> {
    ProjectSnapshot::from_source_bytes(
        auru_pm::ProjectFormat::AbletonLiveSet,
        LIVE_SET_XML.as_bytes(),
    )
    .expect("normalize")
    .as_bytes()
    .to_vec()
}

#[tokio::test]
async fn an_ableton_commit_should_describe_its_project() {
    let (_root, provider, commit) = commit_project("song.als", &live_set_snapshot()).await;

    let info = fetch_project_info(&provider, &commit)
        .await
        .expect("fetch summary")
        .expect("an Ableton commit should carry a summary");

    assert_eq!(info.headline(), "175 BPM · 4/4 · C Phrygian");

    let ableton = info.ableton.as_ref().expect("ableton detail");
    assert_eq!(ableton.tracks.total(), 3);
    assert_eq!(ableton.tracks.midi, 1);
    assert_eq!(ableton.tracks.audio, 1);
    assert_eq!(ableton.tracks.retn, 1);
    assert_eq!(
        ableton.live_version.as_deref(),
        Some("Ableton Live 12.0.25")
    );
}

#[tokio::test]
async fn the_summary_should_not_grow_with_the_size_of_the_project() {
    // The whole justification. A real Live Set's bulk is device and parameter
    // state — one project measured 7 MB of canonical JSON across ~100,000
    // elements — while what a person wants to see about it is a handful of
    // numbers. The summary has to stay flat as that bulk grows, or fetching it
    // is no better than fetching the snapshot.
    let plain = live_set_snapshot();

    // The same project, plus a device chain full of parameter state.
    let parameters: String = (0..2_000)
        .map(|index| {
            format!(
                r#"<PluginFloatParameter Id="{index}">
                     <ParameterName Value="Macro {index}" />
                     <ParameterValue><Manual Value="0.5" /></ParameterValue>
                   </PluginFloatParameter>"#
            )
        })
        .collect();
    let heavy_xml = LIVE_SET_XML.replace(
        r#"<MidiTrack Id="1"><Name><EffectiveName Value="Reese" /></Name></MidiTrack>"#,
        &format!(
            r#"<MidiTrack Id="1"><Name><EffectiveName Value="Reese" /></Name>
                 <DeviceChain><DeviceChain><Devices>
                   <PluginDevice>{parameters}</PluginDevice>
                 </Devices></DeviceChain></DeviceChain>
               </MidiTrack>"#
        ),
    );
    let heavy = ProjectSnapshot::from_source_bytes(
        auru_pm::ProjectFormat::AbletonLiveSet,
        heavy_xml.as_bytes(),
    )
    .expect("normalize")
    .as_bytes()
    .to_vec();

    let (_a, provider_a, plain_commit) = commit_project("plain.als", &plain).await;
    let (_b, provider_b, heavy_commit) = commit_project("heavy.als", &heavy).await;

    let plain_summary = provider_a
        .get_blob(&plain_commit.metadata.expect("summary"))
        .await
        .expect("fetch");
    let heavy_summary = provider_b
        .get_blob(&heavy_commit.metadata.expect("summary"))
        .await
        .expect("fetch");

    assert!(
        heavy.len() > plain.len() * 100,
        "the heavy project should genuinely be much larger: {} vs {}",
        heavy.len(),
        plain.len()
    );
    assert!(
        heavy_summary.len() < plain_summary.len() * 2,
        "a 100x larger project produced a {}-byte summary against {} — \
         the summary must stay flat",
        heavy_summary.len(),
        plain_summary.len()
    );
    assert!(
        heavy_summary.len() * 100 < heavy.len(),
        "and be negligible next to the snapshot it describes"
    );
}

#[tokio::test]
async fn a_native_project_should_commit_exactly_as_before() {
    // Native Auru has no summary to give, so its commits must be untouched by
    // this feature — same fields, same id.
    let (_root, provider, commit) =
        commit_project("song.auru", br#"{"bpm":174,"channels":[],"version":8}"#).await;

    assert!(
        commit.metadata.is_none(),
        "a format we do not summarize must not gain a field"
    );
    assert!(
        fetch_project_info(&provider, &commit)
            .await
            .expect("fetch")
            .is_none(),
        "and reading it back should say so rather than inventing one"
    );
}

#[tokio::test]
async fn the_summary_blob_should_survive_garbage_collection() {
    // Collection walks history for reachable blobs. Missing the summary would
    // quietly strip every project's tempo and key on the next sweep.
    let (_root, provider, commit) = commit_project("song.als", &live_set_snapshot()).await;
    let hash = commit.metadata.expect("summary present");

    let reachable = collect_reachable(&provider).await.expect("collect");
    assert!(
        reachable.contains(&hash),
        "the summary blob must be reachable from its commit"
    );

    // And a real sweep with no grace period leaves it in place.
    let cas = Cas::open(provider.root().join("objects")).expect("open cas");
    cas.gc(&reachable, 0).expect("gc");
    assert!(
        cas.has(&hash),
        "collection must not remove a blob a commit points at"
    );
}

#[tokio::test]
async fn an_unreadable_summary_should_be_reported_as_absent() {
    // A commit written by a newer build may carry a summary whose shape this
    // one does not know. Falling back to the snapshot beats rendering fields
    // we have half understood.
    let (_root, provider, mut commit) = commit_project("song.als", &live_set_snapshot()).await;

    let future = serde_json::json!({
        "schema": auru_pm::PROJECT_INFO_SCHEMA + 1,
        "format": "ableton-live-set"
    });
    let bytes = serde_json::to_vec(&future).expect("encode");
    let hash = auru_pm::ContentHash::of(&bytes);
    provider.put_blob(&hash, &bytes).await.expect("put");
    commit.metadata = Some(hash);

    assert!(
        fetch_project_info(&provider, &commit)
            .await
            .expect("fetch should not error")
            .is_none()
    );
}

#[tokio::test]
async fn the_same_project_should_reuse_one_summary_blob() {
    // Content addressing means an unchanged project does not accumulate a new
    // summary per commit.
    let snapshot = live_set_snapshot();
    let (root, provider, first) = commit_project("song.als", &snapshot).await;

    // A second commit of the same project, differing only in its message.
    let sidecar = sidecar_path_for(&root.path().join("song.als"));
    let PushOutcome::Committed { commit_id, .. } = push_with_freshness_check(
        &provider,
        &provider.provider_id(),
        &[],
        &sidecar,
        &snapshot,
        author(),
        "Second version",
        "",
    )
    .await
    .expect("push") else {
        panic!("a fast-forward push should land");
    };

    let second = provider.get_commit(&commit_id).await.expect("get commit");
    assert_ne!(first.id, second.id, "distinct commits");
    assert_eq!(
        first.metadata, second.metadata,
        "an unchanged project should point at the same summary blob"
    );
}
