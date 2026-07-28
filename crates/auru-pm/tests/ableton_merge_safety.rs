//! Reconciling two people's changes to the same Live Set.
//!
//! Pulling in someone else's work and replaying your own on top is the normal
//! path, and it should stay quiet when it can. These tests pin the two places
//! that must not be quiet: a merge that is structurally clean but leaves the
//! project incoherent, and the guarantee that your own version survives the
//! attempt either way.

use std::path::Path;

use auru_pm::{
    AuthorIdentity, FilesystemProvider, IntegrityProblem, ProjectProvider, ProjectSnapshot,
    PushOutcome, Sidecar, push_with_freshness_check, sidecar_path_for, stashed_snapshot,
};
use tempfile::TempDir;

/// A Live Set with `body` inside `Tracks` and the given modulation counter.
fn live_set(next_pointee_id: u32, body: &str) -> Vec<u8> {
    let xml = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12">
  <LiveSet>
    <NextPointeeId Value="{next_pointee_id}" />
    <Tracks>{body}</Tracks>
  </LiveSet>
</Ableton>"#
    );
    ProjectSnapshot::from_source_bytes(auru_pm::ProjectFormat::AbletonLiveSet, xml.as_bytes())
        .expect("normalize Live Set")
        .as_bytes()
        .to_vec()
}

/// Two tracks, each optionally carrying one modulation target.
fn tracks(first: Option<u32>, second: Option<u32>) -> String {
    named_tracks("Bass", "Lead", first, second)
}

fn named_tracks(
    first_name: &str,
    second_name: &str,
    first: Option<u32>,
    second: Option<u32>,
) -> String {
    let target = |id: Option<u32>| match id {
        Some(id) => format!(r#"<ModulationTarget Id="{id}" />"#),
        None => String::new(),
    };
    format!(
        r#"<MidiTrack Id="1"><Name><EffectiveName Value="{first_name}" /></Name>{}</MidiTrack>
           <MidiTrack Id="2"><Name><EffectiveName Value="{second_name}" /></Name>{}</MidiTrack>"#,
        target(first),
        target(second)
    )
}

fn author(name: &str) -> AuthorIdentity {
    AuthorIdentity {
        display_name: name.to_owned(),
        provider_user_id: name.to_owned(),
        provider_id: "local".to_owned(),
        email: None,
    }
}

/// Push `snapshot` as `who`, from a sidecar at `sidecar_path`.
async fn push(
    provider: &FilesystemProvider,
    sidecar_path: &Path,
    snapshot: &[u8],
    who: &str,
    message: &str,
) -> PushOutcome {
    let provider_id = provider.provider_id();
    push_with_freshness_check(
        provider,
        &provider_id,
        &[],
        sidecar_path,
        snapshot,
        author(who),
        message,
        "",
    )
    .await
    .expect("push should not error")
}

/// Set up a shared project two people have both edited from a common
/// ancestor, with the collaborator's work already pushed.
///
/// Returns the temp dir, the provider, our sidecar path, and our local
/// snapshot — which is now behind the remote.
async fn diverged(
    ours: &str,
    theirs: &str,
    next_pointee_id: u32,
) -> (TempDir, FilesystemProvider, std::path::PathBuf, Vec<u8>) {
    let root = TempDir::new().expect("temporary root");
    let provider =
        FilesystemProvider::open(root.path().join("provider")).expect("filesystem provider");

    // A shared starting point, pushed by both people's sidecars so each
    // believes it is up to date.
    let ancestor = live_set(next_pointee_id, &tracks(None, None));
    let our_sidecar = sidecar_path_for(&root.path().join("song.als"));
    let their_sidecar = sidecar_path_for(&root.path().join("their-song.als"));

    let PushOutcome::Committed { commit_id, .. } =
        push(&provider, &our_sidecar, &ancestor, "us", "start").await
    else {
        panic!("the first commit cannot conflict");
    };
    Sidecar::modify(&their_sidecar, |sidecar| {
        sidecar.local_head = Some(commit_id);
    })
    .expect("seed collaborator sidecar");

    // The collaborator pushes first, so the remote has moved on.
    let PushOutcome::Committed { .. } = push(
        &provider,
        &their_sidecar,
        theirs.as_bytes(),
        "them",
        "their change",
    )
    .await
    else {
        panic!("collaborator push should land");
    };

    (root, provider, our_sidecar, ours.as_bytes().to_vec())
}

#[tokio::test]
async fn diverged_edits_to_different_tracks_should_reconcile_quietly() {
    // The ordinary case: each person renamed a different track and neither
    // touched the modulation counter. This must stay silent — a version
    // control layer that interrupts for ordinary work is not worth using.
    let ours = live_set(100, &named_tracks("Reese", "Lead", None, None));
    let theirs = live_set(100, &named_tracks("Bass", "Screech", None, None));
    let (_root, provider, sidecar, ours) = diverged(
        &String::from_utf8(ours).expect("utf-8"),
        &String::from_utf8(theirs).expect("utf-8"),
        100,
    )
    .await;

    let outcome = push(&provider, &sidecar, &ours, "us", "our change").await;
    match outcome {
        PushOutcome::Committed { was_merge, .. } => assert!(was_merge, "should be a merge commit"),
        PushOutcome::NeedsResolution { .. } => panic!("disjoint edits should not conflict"),
        PushOutcome::NeedsReview { problems, .. } => {
            panic!("disjoint edits should not need review: {problems:?}")
        }
    }

    // Nothing left to recover from — the work is in history.
    assert!(
        Sidecar::load(&sidecar)
            .expect("load sidecar")
            .stash
            .is_none(),
        "a landed merge should clear the stash"
    );
}

#[tokio::test]
async fn two_people_allocating_the_same_modulation_id_should_need_review() {
    // Both started from the same ancestor, both added a modulation, so both
    // took identity 100 and both advanced the counter to 101. Every field
    // edit is individually consistent — the structural merge finds no
    // conflict — but the result has one identity claimed twice.
    let ours = live_set(101, &tracks(Some(100), None));
    let theirs = live_set(101, &tracks(None, Some(100)));
    let (_root, provider, sidecar, ours) = diverged(
        &String::from_utf8(ours).expect("utf-8"),
        &String::from_utf8(theirs).expect("utf-8"),
        100,
    )
    .await;

    let outcome = push(&provider, &sidecar, &ours, "us", "our change").await;
    let PushOutcome::NeedsReview { problems, .. } = outcome else {
        panic!("a duplicated modulation identity must not commit silently");
    };
    assert!(
        problems.iter().any(|problem| matches!(
            problem,
            IntegrityProblem::DuplicateModulationId { id: 100, .. }
        )),
        "expected the duplicated identity to be named: {problems:?}"
    );
}

#[tokio::test]
async fn a_merge_that_needs_review_should_leave_history_untouched() {
    let ours = live_set(101, &tracks(Some(100), None));
    let theirs = live_set(101, &tracks(None, Some(100)));
    let (_root, provider, sidecar, ours) = diverged(
        &String::from_utf8(ours).expect("utf-8"),
        &String::from_utf8(theirs).expect("utf-8"),
        100,
    )
    .await;

    let head_before = provider.get_head().await.expect("head before");
    let outcome = push(&provider, &sidecar, &ours, "us", "our change").await;
    assert!(matches!(outcome, PushOutcome::NeedsReview { .. }));

    assert_eq!(
        provider.get_head().await.expect("head after"),
        head_before,
        "nothing should be committed while the merge is unresolved"
    );
}

#[tokio::test]
async fn our_own_version_should_be_recoverable_after_a_merge_attempt() {
    // The point of stashing: whatever the merge did, the work as it stood
    // before it can be put back in full.
    let ours = live_set(101, &tracks(Some(100), None));
    let theirs = live_set(101, &tracks(None, Some(100)));
    let (_root, provider, sidecar, ours) = diverged(
        &String::from_utf8(ours).expect("utf-8"),
        &String::from_utf8(theirs).expect("utf-8"),
        100,
    )
    .await;

    let outcome = push(&provider, &sidecar, &ours, "us", "our change").await;
    assert!(matches!(outcome, PushOutcome::NeedsReview { .. }));

    let recovered = stashed_snapshot(&provider, &sidecar)
        .await
        .expect("read stash")
        .expect("a merge attempt should leave a stash");
    assert_eq!(
        recovered, ours,
        "the stash must reproduce our pre-merge snapshot exactly"
    );

    // And it restores to a Live Set that still opens.
    let snapshot = ProjectSnapshot::from_canonical_bytes(&recovered).expect("valid snapshot");
    assert_eq!(snapshot.format(), auru_pm::ProjectFormat::AbletonLiveSet);
    assert!(
        snapshot
            .restore_bytes()
            .expect("restore")
            .starts_with(&[0x1f, 0x8b]),
        "restores to a gzipped .als"
    );
}

#[tokio::test]
async fn a_fast_forward_push_should_not_stash() {
    // No divergence, no merge, nothing at risk — so no stash to clean up.
    let root = TempDir::new().expect("temporary root");
    let provider =
        FilesystemProvider::open(root.path().join("provider")).expect("filesystem provider");
    let sidecar = sidecar_path_for(&root.path().join("song.als"));

    push(
        &provider,
        &sidecar,
        &live_set(100, &tracks(None, None)),
        "us",
        "start",
    )
    .await;
    push(
        &provider,
        &sidecar,
        &live_set(101, &tracks(Some(100), None)),
        "us",
        "add modulation",
    )
    .await;

    assert!(
        Sidecar::load(&sidecar)
            .expect("load sidecar")
            .stash
            .is_none(),
        "a push with nothing to reconcile should not stash"
    );
}

#[tokio::test]
async fn native_projects_should_be_unaffected_by_the_ableton_check() {
    // The integrity gate is Ableton-specific; native projects must merge
    // exactly as they did before it existed.
    let root = TempDir::new().expect("temporary root");
    let provider =
        FilesystemProvider::open(root.path().join("provider")).expect("filesystem provider");
    let ours = sidecar_path_for(&root.path().join("song.auru"));
    let theirs = sidecar_path_for(&root.path().join("their-song.auru"));

    let ancestor = br#"{"bpm":120,"channels":[],"version":8}"#;
    let PushOutcome::Committed { commit_id, .. } =
        push(&provider, &ours, ancestor, "us", "start").await
    else {
        panic!("first commit cannot conflict");
    };
    Sidecar::modify(&theirs, |sidecar| sidecar.local_head = Some(commit_id))
        .expect("seed collaborator");

    push(
        &provider,
        &theirs,
        br#"{"bpm":120,"channels":[],"tempo_note":"theirs","version":8}"#,
        "them",
        "their change",
    )
    .await;

    let outcome = push(
        &provider,
        &ours,
        br#"{"bpm":174,"channels":[],"version":8}"#,
        "us",
        "our change",
    )
    .await;
    assert!(
        matches!(
            outcome,
            PushOutcome::Committed {
                was_merge: true,
                ..
            }
        ),
        "a native three-way merge should still land"
    );
}
