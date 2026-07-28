//! Comparing two committed versions of the same Live Set.
//!
//! Exercises the diff through the public entry point the history UI uses,
//! against snapshots that went through a real commit round trip — so it covers
//! normalization and storage, not just the comparison in isolation.

use auru_pm::{
    AuthorIdentity, ChangeKind, ChangeTag, ChannelDiff, ChannelKind, FilesystemProvider,
    ProjectDiff, ProjectFormat, ProjectProvider, ProjectSnapshot, PushOutcome,
    push_with_freshness_check, sidecar_path_for, structured_diff, summarize_diff,
};
use tempfile::TempDir;

/// A Live Set with the given tracks and tempo.
fn live_set(tempo: &str, tracks: &str) -> Vec<u8> {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12.0.25">
  <LiveSet>
    <MainTrack><DeviceChain><Mixer>
      <Tempo><Manual Value="{tempo}" /></Tempo>
      <TimeSignature><Manual Value="201" /></TimeSignature>
    </Mixer></DeviceChain></MainTrack>
    <Tracks>{tracks}</Tracks>
  </LiveSet>
</Ableton>"#
    );
    ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, xml.as_bytes())
        .expect("normalize")
        .as_bytes()
        .to_vec()
}

fn track(id: &str, name: &str, body: &str) -> String {
    format!(
        r#"<MidiTrack Id="{id}">
             <Name><EffectiveName Value="{name}" /></Name>
             <DeviceChain>
               <Mixer>
                 <Volume><Manual Value="1" /></Volume>
                 <Pan><Manual Value="0" /></Pan>
                 <Speaker><Manual Value="true" /></Speaker>
               </Mixer>
               <DeviceChain><Devices><Eq8 /></Devices></DeviceChain>
             </DeviceChain>
             {body}
           </MidiTrack>"#
    )
}

fn clip(id: &str, start: u32, end: u32) -> String {
    format!(
        r#"<MidiClip Id="{id}"><CurrentStart Value="{start}" /><CurrentEnd Value="{end}" /><Name Value="" /></MidiClip>"#
    )
}

/// Commit both versions and diff them the way the history UI would: fetch each
/// snapshot back out of the provider, then compare.
async fn committed_diff(first: &[u8], second: &[u8]) -> ProjectDiff {
    let root = TempDir::new().expect("tempdir");
    let provider = FilesystemProvider::open(root.path().join("provider")).expect("provider");
    let sidecar = sidecar_path_for(&root.path().join("song.als"));
    let author = || AuthorIdentity {
        display_name: "Diff Test".to_owned(),
        provider_user_id: "diff-test".to_owned(),
        provider_id: "local".to_owned(),
        email: None,
    };

    let mut ids = Vec::new();
    for (snapshot, message) in [(first, "first"), (second, "second")] {
        let PushOutcome::Committed { commit_id, .. } = push_with_freshness_check(
            &provider,
            &provider.provider_id(),
            &[],
            &sidecar,
            snapshot,
            author(),
            message,
            "",
        )
        .await
        .expect("push") else {
            panic!("a linear push should land");
        };
        ids.push(commit_id);
    }

    let mut snapshots = Vec::new();
    for id in &ids {
        let commit = provider.get_commit(id).await.expect("get commit");
        let bytes = provider
            .get_blob(&commit.tree.snapshot)
            .await
            .expect("get snapshot");
        snapshots.push(serde_json::from_slice(&bytes).expect("parse snapshot"));
    }

    structured_diff(&snapshots[0], &snapshots[1])
}

fn card<'a>(diff: &'a ProjectDiff, name: &str) -> &'a ChannelDiff {
    diff.channels
        .iter()
        .find(|channel| channel.name == name)
        .unwrap_or_else(|| panic!("expected a card for {name}, got {:?}", diff.channels))
}

#[tokio::test]
async fn two_committed_versions_should_diff_per_track() {
    // The change a person actually made: renamed one track, moved a clip in
    // it, and left the other track alone.
    let before = live_set(
        "175",
        &format!(
            "{}{}",
            track("1", "Reese", &clip("0", 0, 32)),
            track("2", "Drums", &clip("0", 0, 64))
        ),
    );
    let after = live_set(
        "175",
        &format!(
            "{}{}",
            track("1", "Screech", &clip("0", 64, 96)),
            track("2", "Drums", &clip("0", 0, 64))
        ),
    );

    let diff = committed_diff(&before, &after).await;

    assert_eq!(
        diff.channels.len(),
        1,
        "only the track that changed should get a card: {:?}",
        diff.channels
    );
    let changed = card(&diff, "Screech");
    assert_eq!(changed.status, ChangeKind::Modify);
    assert_eq!(changed.kind, ChannelKind::Midi);
    assert_eq!(changed.clips_modified, 1);

    let tags: Vec<ChangeTag> = changed.rows.iter().map(|row| row.tag).collect();
    assert!(tags.contains(&ChangeTag::Renamed), "{tags:?}");
    assert!(tags.contains(&ChangeTag::Moved), "{tags:?}");
}

#[tokio::test]
async fn a_tempo_change_should_surface_at_project_level() {
    let before = live_set("172", &track("1", "Reese", ""));
    let after = live_set("175", &track("1", "Reese", ""));

    let diff = committed_diff(&before, &after).await;

    assert!(
        diff.project_changes
            .iter()
            .any(|change| change == "Tempo: 172 → 175"),
        "{:?}",
        diff.project_changes
    );
    assert_eq!(diff.time_sig, (4, 4), "read from the set, not assumed");
    assert!(
        diff.channels.is_empty(),
        "a tempo change belongs to no single track"
    );
}

#[tokio::test]
async fn committing_the_same_project_twice_should_diff_to_nothing() {
    let project = live_set("175", &track("1", "Reese", &clip("0", 0, 32)));
    let diff = committed_diff(&project, &project).await;

    assert!(diff.project_changes.is_empty());
    assert!(diff.channels.is_empty());
    assert!(diff.is_empty(), "an unchanged project has no diff to show");
}

#[tokio::test]
async fn clip_counts_should_roll_up_across_tracks() {
    let before = live_set("175", &track("1", "Reese", &clip("0", 0, 32)));
    let after = live_set(
        "175",
        &format!(
            "{}{}",
            track(
                "1",
                "Reese",
                &format!("{}{}", clip("0", 0, 32), clip("1", 32, 64))
            ),
            track("2", "Lead", &clip("0", 0, 16))
        ),
    );

    let diff = committed_diff(&before, &after).await;

    assert_eq!(diff.total_clips_added(), 2, "one per track");
    assert_eq!(diff.total_clips_removed(), 0);
    assert_eq!(diff.channel_count(), 2);
}

#[tokio::test]
async fn the_text_summary_should_still_work_for_ableton() {
    // `summarize_diff` is the one-line history-row text. It stays on the
    // format-agnostic path, which must keep saying something true.
    let before = live_set("172", &track("1", "Reese", ""));
    let after = live_set("175", &track("1", "Reese", ""));
    let (before, after): (serde_json::Value, serde_json::Value) = (
        serde_json::from_slice(&before).expect("parse"),
        serde_json::from_slice(&after).expect("parse"),
    );

    let summary = summarize_diff(&before, &after);
    assert!(
        !summary.is_empty(),
        "a changed project must not summarize as unchanged"
    );
}

#[tokio::test]
async fn a_removed_track_should_be_reported_with_its_clips() {
    let before = live_set(
        "175",
        &format!(
            "{}{}",
            track("1", "Reese", &clip("0", 0, 32)),
            track(
                "2",
                "Lead",
                &format!("{}{}", clip("0", 0, 16), clip("1", 16, 32))
            )
        ),
    );
    let after = live_set("175", &track("1", "Reese", &clip("0", 0, 32)));

    let diff = committed_diff(&before, &after).await;

    let gone = card(&diff, "Lead");
    assert_eq!(gone.status, ChangeKind::Remove);
    assert_eq!(gone.clips_removed, 2);
}
