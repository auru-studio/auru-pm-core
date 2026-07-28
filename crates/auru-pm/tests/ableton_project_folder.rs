//! Committing an Ableton project folder and restoring it somewhere else.
//!
//! The whole point of the folder being the unit of version control: a project
//! that referenced a sample from a library elsewhere on the machine should,
//! after a round trip, open on a machine that has never seen that library.

use std::path::{Path, PathBuf};

use auru_pm::ableton::{self, BundlePolicy, PathAlias};
use auru_pm::{
    AuthorIdentity, FilesystemProvider, ProjectProvider, ProjectSnapshot, PushOutcome,
    push_with_freshness_check, sidecar_path_for,
};
use tempfile::TempDir;

/// A Live Set referencing one sample from outside its folder and one inside.
const LIVE_SET_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12.0.25">
  <LiveSet>
    <NextPointeeId Value="100" />
    <MainTrack>
      <DeviceChain><Mixer>
        <Tempo><Manual Value="175" /></Tempo>
        <TimeSignature><Manual Value="201" /></TimeSignature>
      </Mixer></DeviceChain>
    </MainTrack>
    <Tracks>
      <AudioTrack Id="1">
        <Name><EffectiveName Value="Break" /></Name>
        <SampleRef><FileRef>
          <RelativePathType Value="1" />
          <RelativePath Value="../library/SPLICE/break.wav" />
          <Path Value="E:/Music Production/library/SPLICE/break.wav" />
          <LivePackName Value="" />
          <OriginalFileSize Value="9" />
        </FileRef></SampleRef>
        <SampleRef><FileRef>
          <RelativePathType Value="3" />
          <RelativePath Value="Samples/Processed/loop.wav" />
          <Path Value="E:/Music Production/Song Project/Samples/Processed/loop.wav" />
          <LivePackName Value="" />
        </FileRef></SampleRef>
        <SampleRef><FileRef>
          <RelativePathType Value="5" />
          <RelativePath Value="Devices/Audio Effects/EQ Eight" />
          <Path Value="" />
          <LivePackName Value="Core Library" />
        </FileRef></SampleRef>
        <SampleRef><FileRef>
          <RelativePathType Value="0" />
          <RelativePath Value="" /><Path Value="" />
        </FileRef></SampleRef>
      </AudioTrack>
    </Tracks>
  </LiveSet>
</Ableton>"#;

fn touch(path: &Path, bytes: &[u8]) {
    std::fs::create_dir_all(path.parent().expect("has parent")).expect("create dirs");
    std::fs::write(path, bytes).expect("write");
}

fn author() -> AuthorIdentity {
    AuthorIdentity {
        display_name: "Folder Test".to_owned(),
        provider_user_id: "folder-test".to_owned(),
        provider_id: "local".to_owned(),
        email: None,
    }
}

/// A project folder plus a sample library beside it, as on a real machine.
struct Scenario {
    _temp: TempDir,
    project: PathBuf,
    live_set: PathBuf,
    provider: FilesystemProvider,
    policy: BundlePolicy,
}

fn scenario() -> Scenario {
    let temp = TempDir::new().expect("tempdir");
    let project = temp.path().join("Song Project");
    let live_set = project.join("Song.als");

    let gzipped = ProjectSnapshot::from_source_bytes(
        auru_pm::ProjectFormat::AbletonLiveSet,
        LIVE_SET_XML.as_bytes(),
    )
    .expect("normalize")
    .restore_bytes()
    .expect("gzip");
    touch(&live_set, &gzipped);
    touch(&project.join("Ableton Project Info/AProject.ico"), b"icon");
    touch(&project.join("Samples/Processed/loop.wav"), b"in-folder");
    touch(
        &project.join("Backup/Song [2026-01-01 000000].als"),
        b"autosave",
    );
    // The sample that lives outside the folder — the one that breaks when the
    // project moves.
    touch(&temp.path().join("library/SPLICE/break.wav"), b"the break");

    let provider =
        FilesystemProvider::open(temp.path().join("provider")).expect("filesystem provider");
    let policy = BundlePolicy {
        path_aliases: vec![PathAlias::new(
            "E:/Music Production/library",
            temp.path().join("library"),
        )],
        ..BundlePolicy::default()
    };

    Scenario {
        _temp: temp,
        project,
        live_set,
        provider,
        policy,
    }
}

/// Commit the project folder and return the commit.
async fn commit(scenario: &Scenario) -> auru_pm::Commit {
    let snapshot = ProjectSnapshot::load(&scenario.live_set).expect("snapshot");
    let plan = ableton::plan_bundle_assets(&snapshot, &scenario.project, &scenario.policy)
        .expect("plan")
        .expect("project folder detected");

    // Store the planned assets, mirroring what the push path does with the
    // default policy — this test drives a custom policy for the path alias.
    let mut manifest = auru_pm::SampleManifest::new();
    for asset in &plan.assets {
        let bytes = std::fs::read(&asset.source).expect("read asset");
        let hash = auru_pm::ContentHash::of(&bytes);
        scenario
            .provider
            .put_blob(&hash, &bytes)
            .await
            .expect("put asset");
        manifest.insert(auru_pm::SampleEntry {
            path: asset.bundle_path.clone(),
            hash,
            size: bytes.len() as u64,
            kind: asset.kind,
            origin: asset.origin.clone(),
        });
    }

    let PushOutcome::Committed { commit_id, .. } = push_with_freshness_check(
        &scenario.provider,
        &scenario.provider.provider_id(),
        &[],
        &sidecar_path_for(&scenario.live_set),
        snapshot.as_bytes(),
        author(),
        "Folder commit",
        "",
    )
    .await
    .expect("push") else {
        panic!("first commit cannot conflict");
    };

    // Point the commit at the manifest built above.
    let manifest_bytes = manifest.canonical_encoding().expect("encode manifest");
    let manifest_hash = auru_pm::ContentHash::of(&manifest_bytes);
    scenario
        .provider
        .put_blob(&manifest_hash, &manifest_bytes)
        .await
        .expect("put manifest");

    let mut commit = scenario
        .provider
        .get_commit(&commit_id)
        .await
        .expect("get commit");
    commit.tree.samples = manifest_hash;
    commit
}

#[tokio::test]
async fn a_restored_project_should_find_the_sample_that_lived_outside_it() {
    // The headline case. Before this, restoring gave you a Live Set pointing
    // at `E:/Music Production/library/…` on a machine with no `E:` drive.
    let scenario = scenario();
    let commit = commit(&scenario).await;

    let destination = scenario.project.parent().expect("parent").join("Restored");
    let report = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("restore");

    let gathered = destination.join("Samples/Imported/break.wav");
    assert!(
        gathered.is_file(),
        "the outside sample should have travelled"
    );
    assert_eq!(std::fs::read(&gathered).expect("read"), b"the break");

    assert_eq!(
        report.rewrite.rewritten, 1,
        "its reference should be repointed"
    );
    assert!(
        report.rewrite.is_complete(),
        "nothing should still be missing: {:?}",
        report.rewrite.unresolved
    );
}

#[tokio::test]
async fn the_restored_live_set_should_point_inside_its_own_folder() {
    let scenario = scenario();
    let commit = commit(&scenario).await;
    let destination = scenario.project.parent().expect("parent").join("Restored");
    let report = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("restore");

    // Re-read the restored set the same way Live would: gunzip, parse, walk.
    let restored = ProjectSnapshot::load(&report.live_set).expect("load restored set");
    let refs = ableton::read_asset_refs(&restored).expect("read refs");

    let outside = refs
        .iter()
        .find(|asset| asset.file_name() == Some("break.wav"))
        .expect("the gathered sample is referenced");
    assert_eq!(outside.relative_path, "Samples/Imported/break.wav");
    assert_eq!(outside.class, ableton::RefClass::InFolder);
    assert!(
        outside
            .absolute_path
            .ends_with("Restored/Samples/Imported/break.wav"),
        "absolute path should address the restored folder: {}",
        outside.absolute_path
    );

    // And the reference resolves against the restored folder on this machine.
    let bundle = ableton::AbletonBundle::detect(&destination)
        .expect("detect")
        .expect("restored folder is a project");
    assert!(
        bundle
            .resolve(
                &outside.relative_path,
                &outside.absolute_path,
                &BundlePolicy::default()
            )
            .is_some(),
        "the restored project should find its own sample with no aliases"
    );
}

#[tokio::test]
async fn core_library_and_empty_references_should_survive_untouched() {
    let scenario = scenario();
    let commit = commit(&scenario).await;
    let destination = scenario.project.parent().expect("parent").join("Restored");
    let report = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("restore");

    assert_eq!(
        report.rewrite.left_to_live, 1,
        "EQ Eight resolves from Live"
    );
    assert_eq!(report.rewrite.empty, 1, "the empty reference is not a file");

    let restored = ProjectSnapshot::load(&report.live_set).expect("load restored set");
    let refs = ableton::read_asset_refs(&restored).expect("read refs");
    assert!(
        refs.iter()
            .any(|asset| asset.relative_path == "Devices/Audio Effects/EQ Eight"),
        "Core Library reference should be unchanged"
    );
}

#[tokio::test]
async fn the_restored_project_should_keep_its_musical_detail() {
    let scenario = scenario();
    let commit = commit(&scenario).await;
    let destination = scenario.project.parent().expect("parent").join("Restored");
    let report = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("restore");

    let restored = ProjectSnapshot::load(&report.live_set).expect("load restored set");
    let metadata = ableton::read_metadata(&restored).expect("metadata");

    assert_eq!(metadata.tempo, Some(175.0));
    assert_eq!(
        metadata.time_signature.map(|sig| sig.to_string()),
        Some("4/4".to_owned())
    );
    assert_eq!(metadata.tracks.audio, 1);
    assert_eq!(
        metadata.live_version.as_deref(),
        Some("Ableton Live 12.0.25")
    );
}

#[tokio::test]
async fn in_folder_files_should_be_restored_and_backups_left_out() {
    let scenario = scenario();
    let commit = commit(&scenario).await;
    let destination = scenario.project.parent().expect("parent").join("Restored");
    let report = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("restore");

    assert!(destination.join("Samples/Processed/loop.wav").is_file());
    assert!(
        destination
            .join("Ableton Project Info/AProject.ico")
            .is_file()
    );
    assert!(
        !destination.join("Backup").exists(),
        "autosaves are excluded by default — Auru's history supersedes them"
    );
    assert!(report.unavailable.is_empty());
    assert!(report.files_written >= 3);
}

#[tokio::test]
async fn restoring_twice_should_produce_the_same_folder() {
    // Restoring over a previous restore is how you move back to an earlier
    // version, so it has to be safe to repeat.
    //
    // Restore reads the commit, never the folder, so the second pass does the
    // same work as the first rather than skipping it — that is what makes the
    // result a pure function of the commit, and why two machines restoring the
    // same version get identical folders. (Rewriting an *already rewritten*
    // tree is a no-op; that is covered in `ableton::rewrite`.)
    let scenario = scenario();
    let commit = commit(&scenario).await;
    let destination = scenario.project.parent().expect("parent").join("Restored");

    let first = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("first restore");
    let first_bytes = std::fs::read(&first.live_set).expect("read set");
    let first_sample =
        std::fs::read(destination.join("Samples/Imported/break.wav")).expect("read sample");

    let second = ableton::restore_bundle(&scenario.provider, &commit, &destination, "Song.als")
        .await
        .expect("second restore should not be refused");

    assert_eq!(
        second.rewrite, first.rewrite,
        "the same commit should always rewrite the same way"
    );
    assert_eq!(
        std::fs::read(&second.live_set).expect("read set"),
        first_bytes,
        "and produce a byte-identical Live Set"
    );
    assert_eq!(
        std::fs::read(destination.join("Samples/Imported/break.wav")).expect("read sample"),
        first_sample,
        "without duplicating or corrupting the gathered sample"
    );
    assert!(
        !destination.join("Samples/Imported/break-2.wav").exists(),
        "and without gathering a second copy alongside it"
    );
}

#[tokio::test]
async fn restoring_into_someone_elses_folder_should_be_refused() {
    let scenario = scenario();
    let commit = commit(&scenario).await;

    let occupied = scenario.project.parent().expect("parent").join("Documents");
    touch(&occupied.join("taxes.pdf"), b"important");

    let error = ableton::restore_bundle(&scenario.provider, &commit, &occupied, "Song.als")
        .await
        .expect_err("restoring over unrelated files must be refused");
    assert!(
        error.to_string().contains("already contains other files"),
        "the refusal should explain itself: {error}"
    );
    assert!(
        occupied.join("taxes.pdf").is_file(),
        "and must not have touched anything"
    );
}
