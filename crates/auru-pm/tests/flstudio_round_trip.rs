//! FL Studio projects through the whole commit and restore path.
//!
//! The property under test is the one that makes `.flp` support trustworthy:
//! a project committed and restored comes back *byte for byte*, with a single
//! deliberate exception that is named rather than glossed over.

use auru_pm::flstudio::{Event, Header, Stream};
use auru_pm::{ProjectFormat, ProjectSnapshot};

/// The registration name, which a backup deliberately does not carry.
const EVENT_REG_NAME: u8 = 200;
const EVENT_VERSION: u8 = 199;
const EVENT_TEMPO: u8 = 156;
const EVENT_SAMPLE_PATH: u8 = 196;
const EVENT_NEW_CHANNEL: u8 = 64;
const EVENT_DISPLAY_NAME: u8 = 203;
const EVENT_PLUGIN_PARAMS: u8 = 213;

fn utf16(text: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// A project exercising every kind of payload a real one contains.
fn fixture_project() -> Vec<u8> {
    Stream {
        header: Header {
            format: 0,
            channels: 2,
            ppq: 96,
        },
        events: vec![
            Event::new(EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
            Event::new(EVENT_TEMPO, 174_500u32.to_le_bytes()),
            Event::new(17, [4]),
            Event::new(18, [4]),
            Event::new(194, utf16("Night Drive")),
            Event::new(EVENT_NEW_CHANNEL, 0u16.to_le_bytes()),
            Event::new(EVENT_DISPLAY_NAME, utf16("Kick")),
            Event::new(EVENT_SAMPLE_PATH, utf16(r"D:\Packs\Kick.wav")),
            Event::new(EVENT_NEW_CHANNEL, 1u16.to_le_bytes()),
            Event::new(EVENT_DISPLAY_NAME, utf16("Bass")),
            // Opaque plugin state, including bytes that are not valid text.
            Event::new(EVENT_PLUGIN_PARAMS, vec![0x00, 0xff, 0xfe, 0x80, 0x7f]),
        ],
    }
    .encode()
}

#[test]
fn a_project_should_be_recognised_by_its_contents_and_its_extension() {
    let source = fixture_project();
    assert_eq!(
        ProjectFormat::from_path(std::path::Path::new("Song.flp")),
        Some(ProjectFormat::FlStudio)
    );

    // And without the extension, from the bytes alone — projects get handed
    // around renamed.
    let snapshot = snapshot_project_bytes(&source, "Song");
    assert_eq!(snapshot.format(), ProjectFormat::FlStudio);
}

#[test]
fn commit_and_restore_should_return_the_original_bytes() {
    // No registration name in this project, so there is nothing to redact and
    // the result must be identical — the plain statement of the contract.
    let source = fixture_project();
    let snapshot =
        ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source).expect("snapshot");

    assert_eq!(
        snapshot.restore_bytes().expect("restore"),
        source,
        "a committed project must come back exactly as it went in"
    );
}

#[test]
fn a_snapshot_should_survive_the_trip_through_the_content_store() {
    // What actually happens on a push and pull: the canonical bytes are what
    // is stored, and the project is rebuilt from those alone.
    let source = fixture_project();
    let snapshot =
        ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source).expect("snapshot");

    let stored = snapshot.as_bytes().to_vec();
    let fetched = ProjectSnapshot::from_canonical_bytes(&stored).expect("from canonical");

    assert_eq!(fetched.format(), ProjectFormat::FlStudio);
    assert_eq!(fetched.restore_bytes().expect("restore"), source);
}

#[test]
fn round_trip_should_differ_only_in_the_redacted_reg_name() {
    // The single deliberate exception to byte-exactness. The registration name
    // identifies the FL licence holder and travels with every copy of a shared
    // project, so it is emptied on commit; FL writes a fresh one on save.
    let mut stream = Stream::decode(&fixture_project()).expect("decode");
    stream
        .events
        .insert(1, Event::new(EVENT_REG_NAME, utf16("ez:57h2vAv0@>=B>C;8")));
    let source = stream.encode();

    let restored = ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source)
        .expect("snapshot")
        .restore_bytes()
        .expect("restore");

    assert_ne!(restored, source, "the licence identity must not survive");

    let before = Stream::decode(&source).expect("decode before");
    let after = Stream::decode(&restored).expect("decode after");
    assert_eq!(before.header, after.header);
    assert_eq!(
        before.events.len(),
        after.events.len(),
        "redaction empties an event, it does not remove one"
    );

    for (before, after) in before.events.iter().zip(&after.events) {
        assert_eq!(before.id, after.id);
        if before.id == EVENT_REG_NAME {
            assert!(
                after.payload.iter().all(|byte| *byte == 0),
                "the registration name should be blank, found {:?}",
                after.payload
            );
        } else {
            assert_eq!(
                before.payload, after.payload,
                "event {} changed, but only {EVENT_REG_NAME} may",
                before.id
            );
        }
    }
}

#[test]
fn restoring_to_the_wrong_extension_should_be_refused() {
    // Writing a `.flp` where an `.als` is expected produces a file no DAW can
    // open, and the mistake would only surface when someone tried to work.
    let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &fixture_project())
        .expect("snapshot");
    let temp = tempfile::tempdir().expect("tempdir");
    assert!(
        snapshot
            .restore_to_path(&temp.path().join("Song.als"))
            .is_err()
    );
    assert!(
        snapshot
            .restore_to_path(&temp.path().join("Song.flp"))
            .is_ok()
    );
}

#[test]
fn a_truncated_project_should_be_refused_rather_than_committed() {
    // A desynchronised parse yields plausible nonsense; committing that would
    // store a corrupt project under a hash that claims to be the real one.
    let mut source = fixture_project();
    source.truncate(source.len() / 2);
    assert!(ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &source).is_err());
}

/// Snapshot bytes whose format has to be inferred from content.
fn snapshot_project_bytes(source: &[u8], stem: &str) -> ProjectSnapshot {
    let temp = tempfile::tempdir().expect("tempdir");
    let path = temp.path().join(stem);
    std::fs::write(&path, source).expect("write");
    auru_pm::snapshot_project(&path).expect("snapshot")
}

#[test]
fn fl_plugins_should_resolve_against_the_registry() {
    // The question the detail page asks: of everything this project loads,
    // what would fail to open here, and where does the user get it?
    use auru_pm::ableton::{PluginFormat, PluginId};
    use auru_pm::plugin_registry::{self, PluginSearchPaths};

    let plugins = vec![
        auru_pm::ableton::PluginRef {
            name: "Serum_x64".to_owned(),
            format: PluginFormat::Vst2,
            id: PluginId::Vst2ByFile {
                file_name: "serum_x64.dll".to_owned(),
            },
            device_type: None,
            path: None,
            instances: 18,
        },
        auru_pm::ableton::PluginRef {
            name: "Fruity Limiter".to_owned(),
            format: PluginFormat::Native,
            id: PluginId::FlNative {
                device: "Fruity Limiter".to_owned(),
            },
            device_type: None,
            path: None,
            instances: 4,
        },
    ];

    let resolved = plugin_registry::resolve(
        &plugins,
        plugin_registry::bundled(),
        &PluginSearchPaths::default(),
    );

    let serum = &resolved[0];
    assert_eq!(
        serum.name, "Serum",
        "FL identifies a plugin by its file; the registry has to meet it there"
    );
    assert_eq!(serum.vendor, "Xfer Records");
    assert_eq!(serum.instances, 18);
    assert!(
        serum.link().is_some(),
        "a plugin the user may not have needs somewhere to get it"
    );

    // FL's own effects ship with the DAW, so they are never something the
    // person has to go and obtain.
    assert!(!resolved[1].blocks_playback());
}
