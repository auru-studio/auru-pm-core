//! An FL Studio project all the way through: read, plan, commit, restore.
//!
//! The promise being tested is the one that matters to someone moving to a new
//! machine: the project opens, and its samples are there — even though the
//! paths it was saved with refer to a drive that no longer exists.

use std::collections::BTreeMap;

use auru_pm::ableton::PathAlias;
use auru_pm::flstudio::{self, Event, Header, Stream};
use auru_pm::{ProjectFormat, ProjectSnapshot};

const EVENT_VERSION: u8 = 199;
const EVENT_SAMPLE_PATH: u8 = 196;

fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// A project whose samples live on a Windows drive that is not this machine.
fn project(paths: &[&str]) -> Vec<u8> {
    let mut events = vec![
        Event::new(EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
        Event::new(156, 174_000u32.to_le_bytes()),
        Event::new(194, utf16("Night Drive")),
    ];
    for path in paths {
        events.push(Event::new(EVENT_SAMPLE_PATH, utf16(path)));
    }
    Stream {
        header: Header {
            format: 0,
            channels: 2,
            ppq: 96,
        },
        events,
    }
    .encode()
}

#[test]
fn a_project_should_be_restorable_onto_a_machine_that_never_had_its_samples() {
    let temp = tempfile::tempdir().expect("tempdir");

    // The "other machine": where the samples really are today.
    let packs = temp.path().join("packs");
    std::fs::create_dir_all(&packs).expect("mkdir");
    std::fs::write(packs.join("Kick.wav"), b"kick audio").expect("write");
    std::fs::write(packs.join("Snare.wav"), b"snare audio").expect("write");

    let source = project(&[r"D:\Soundpacks\Kick.wav", r"D:\Soundpacks\Snare.wav"]);
    let aliases = vec![PathAlias::new(r"D:\Soundpacks", &packs)];

    // --- what a backup would capture -------------------------------------
    let plan = flstudio::plan_bundle_assets(&source, &aliases).expect("plan");
    assert_eq!(plan.assets.len(), 2, "both samples found");
    assert!(plan.unresolved.is_empty());
    assert_eq!(plan.total_bytes(), 21);

    // --- commit ----------------------------------------------------------
    let snapshot =
        ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source).expect("snapshot");
    let stored_project = snapshot.as_bytes().to_vec();
    let stored_assets: BTreeMap<String, Vec<u8>> = plan
        .assets
        .iter()
        .map(|asset| {
            (
                asset.bundle_path.clone(),
                std::fs::read(&asset.source).expect("read asset"),
            )
        })
        .collect();

    // --- restore, somewhere else entirely --------------------------------
    let destination = temp.path().join("restored");
    std::fs::create_dir_all(&destination).expect("mkdir");
    let project_file = destination.join("Night Drive.flp");

    let restored = ProjectSnapshot::from_canonical_bytes(&stored_project).expect("fetch");
    let mut stream = Stream::decode(&restored.restore_bytes().expect("restore")).expect("decode");

    let captured: BTreeMap<String, String> = plan
        .assets
        .iter()
        .map(|asset| (asset.origin.clone(), asset.bundle_path.clone()))
        .collect();
    let report = flstudio::restore::repoint(&mut stream, &captured);

    assert_eq!(report.refs_repointed, 2);
    assert!(
        report.is_complete(),
        "nothing should be missing: {:?}",
        report.still_missing
    );

    for (relative, bytes) in &stored_assets {
        flstudio::restore::write_asset(&destination, relative, bytes).expect("write asset");
    }
    std::fs::write(&project_file, stream.encode()).expect("write project");

    // --- the project on the new machine ----------------------------------
    assert_eq!(
        std::fs::read(destination.join("Samples/Kick.wav")).expect("kick"),
        b"kick audio"
    );

    let reopened = std::fs::read(&project_file).expect("reopen");
    let meta = flstudio::read_metadata(&reopened).expect("metadata");
    assert_eq!(meta.tempo, Some(174.0), "the project itself is unchanged");
    assert_eq!(meta.title.as_deref(), Some("Night Drive"));

    // Every sample now sits beside the project rather than on a drive this
    // machine does not have — which is the whole point.
    let refs = flstudio::read_asset_refs(&reopened).expect("refs");
    assert_eq!(refs.len(), 2);
    for reference in &refs {
        assert_eq!(reference.class, flstudio::RefClass::ProjectRelative);
        assert!(
            destination
                .join(reference.recorded_path.replace('\\', "/"))
                .is_file(),
            "{} does not exist beside the project",
            reference.recorded_path
        );
    }
}

#[test]
fn a_sample_that_cannot_be_found_should_not_stop_the_backup() {
    // A project half of whose samples are gone is still worth backing up, and
    // the person needs to be told which half rather than refused outright.
    let temp = tempfile::tempdir().expect("tempdir");
    let packs = temp.path().join("packs");
    std::fs::create_dir_all(&packs).expect("mkdir");
    std::fs::write(packs.join("Kick.wav"), b"kick").expect("write");

    let source = project(&[r"D:\Soundpacks\Kick.wav", r"D:\Soundpacks\Gone.wav"]);
    let aliases = vec![PathAlias::new(r"D:\Soundpacks", &packs)];

    let plan = flstudio::plan_bundle_assets(&source, &aliases).expect("plan");
    assert_eq!(plan.assets.len(), 1);
    assert_eq!(plan.unresolved.len(), 1);
    assert_eq!(plan.unresolved[0].recorded_path, r"D:\Soundpacks\Gone.wav");

    // And the commit still succeeds.
    assert!(ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source).is_ok());
}

#[test]
fn restoring_twice_should_produce_the_same_project() {
    // Restore has to be repeatable: repointing an already-repointed project
    // must not mangle paths that are already relative.
    let captured = BTreeMap::from([(
        r"D:\Packs\Kick.wav".to_owned(),
        "Samples/Kick.wav".to_owned(),
    )]);

    let source = project(&[r"D:\Packs\Kick.wav"]);
    let mut once = Stream::decode(&source).expect("decode");
    flstudio::restore::repoint(&mut once, &captured);
    let after_one = once.encode();

    let mut twice = Stream::decode(&after_one).expect("decode");
    let report = flstudio::restore::repoint(&mut twice, &captured);

    assert_eq!(
        twice.encode(),
        after_one,
        "a second restore changed the project"
    );
    assert_eq!(
        report.refs_repointed, 0,
        "there was nothing left to repoint"
    );
    assert!(
        report.still_missing.is_empty(),
        "a sample already sitting beside the project is not missing: {:?}",
        report.still_missing
    );
}
