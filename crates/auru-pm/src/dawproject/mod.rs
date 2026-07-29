//! Semantic reading of open DAWproject archives.
//!
//! DAWproject's ZIP and XML codec lives in [`crate::project_format`]. This
//! module reads the normalized tree into the smaller, musical vocabulary used
//! by project rows, plugin checks, asset manifests, and version diffs.

use std::collections::BTreeMap;

use base64::Engine;

mod assets;
pub(crate) mod diff;
mod meta;
mod plugins;

pub use assets::{DawprojectAssetRef, DawprojectAssetSummary};
pub use meta::{
    DawprojectMetadata, DawprojectTrackCounts, DawprojectTrackKind, DawprojectTrackSummary,
};

use crate::error::{Error, Result};
use crate::project_format::{PortableSnapshot, ProjectFormat, ProjectSnapshot, XmlDocument};

/// Read project detail from a DAWproject snapshot.
pub fn read_metadata(snapshot: &ProjectSnapshot) -> Result<DawprojectMetadata> {
    let portable = portable(snapshot)?;
    Ok(meta::extract(&portable))
}

/// Collect every media file the project references.
pub fn read_asset_refs(snapshot: &ProjectSnapshot) -> Result<Vec<DawprojectAssetRef>> {
    let portable = portable(snapshot)?;
    Ok(assets::collect(&portable))
}

/// Collect the distinct instruments and effects the project loads.
pub fn read_plugins(snapshot: &ProjectSnapshot) -> Result<Vec<crate::PluginRef>> {
    let portable = portable(snapshot)?;
    Ok(plugins::collect(&portable.project.root))
}

/// Replace embedded archive resources with bytes fetched from the asset CAS.
///
/// Version-one canonical snapshots remain self-contained, so callers can use
/// this opportunistically and fall back to the inline copy when a remote blob
/// is unavailable.
pub fn hydrate_embedded_assets(
    snapshot: &ProjectSnapshot,
    assets: &BTreeMap<String, Vec<u8>>,
) -> Result<ProjectSnapshot> {
    let mut portable = portable(snapshot)?;
    for resource in &mut portable.resources {
        if let Some(bytes) = assets.get(&resource.id) {
            resource.data = base64::engine::general_purpose::STANDARD.encode(bytes);
        }
    }
    ProjectSnapshot::from_portable(portable)
}

pub(crate) struct SnapshotParts {
    pub project: XmlDocument,
    pub metadata: Option<XmlDocument>,
}

pub(crate) fn snapshot_parts_from_value(snapshot: &serde_json::Value) -> Option<SnapshotParts> {
    if serde_json::from_value::<ProjectFormat>(snapshot.get("format")?.clone()).ok()?
        != ProjectFormat::Dawproject
    {
        return None;
    }
    Some(SnapshotParts {
        project: serde_json::from_value(snapshot.get("project")?.clone()).ok()?,
        metadata: snapshot
            .get("metadata")
            .map(|metadata| serde_json::from_value(metadata.clone()))
            .transpose()
            .ok()?,
    })
}

pub(crate) fn metadata_from_value(snapshot: &serde_json::Value) -> Option<DawprojectMetadata> {
    let parts = snapshot_parts_from_value(snapshot)?;
    let asset_refs = assets::collect_from_value(&parts.project.root, snapshot);
    Some(meta::extract_parts(
        &parts.project.root,
        parts.metadata.as_ref().map(|document| &document.root),
        &asset_refs,
    ))
}

pub(crate) fn embedded_assets_from_value(
    snapshot: &serde_json::Value,
) -> Vec<assets::EmbeddedAsset> {
    let Some(parts) = snapshot_parts_from_value(snapshot) else {
        return Vec::new();
    };
    assets::embedded_from_value(&parts.project.root, snapshot)
}

fn portable(snapshot: &ProjectSnapshot) -> Result<PortableSnapshot> {
    if snapshot.format() != ProjectFormat::Dawproject {
        return Err(Error::ProjectFormat(format!(
            "expected a DAWproject snapshot, found {}",
            snapshot.format()
        )));
    }
    snapshot.portable()?.ok_or_else(|| {
        Error::ProjectFormat("DAWproject snapshot is missing its format wrapper".to_owned())
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Read, Write};

    use super::*;
    use crate::{PluginFormat, PluginId, ProjectInfo};

    fn archive(project: &str, metadata: Option<&str>, resources: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        writer.start_file("project.xml", options).expect("project");
        writer.write_all(project.as_bytes()).expect("project XML");
        if let Some(metadata) = metadata {
            writer
                .start_file("metadata.xml", options)
                .expect("metadata");
            writer.write_all(metadata.as_bytes()).expect("metadata XML");
        }
        for (path, bytes) in resources {
            writer.start_file(*path, options).expect("resource");
            writer.write_all(bytes).expect("resource bytes");
        }
        writer.finish().expect("finish").into_inner()
    }

    fn full_snapshot() -> ProjectSnapshot {
        let project = r#"
            <Project version="1.0">
              <Application name="Bitwig Studio" version="5.2"/>
              <Transport>
                <Tempo unit="bpm" value="127.5" id="tempo"/>
                <TimeSignature numerator="7" denominator="8" id="signature"/>
              </Transport>
              <Structure>
                <Track contentType="notes" id="synth" name="Synth">
                  <Channel role="regular" id="synth-channel">
                    <Devices>
                      <Vst2Plugin deviceID="1483109208" deviceName="Serum" deviceRole="instrument" id="vst2"/>
                      <Vst3Plugin deviceID="56535453-6572-756d-7353-6572756d0000" deviceName="Serum 2" deviceRole="instrument" id="vst3"/>
                      <ClapPlugin deviceID="org.surge-synth-team.surge-xt" deviceName="Surge XT" deviceRole="instrument" id="clap"/>
                      <AuPlugin deviceID="aufx.delay.vendor" deviceName="Delay" deviceRole="audioFX" id="au"/>
                      <BuiltinDevice deviceID="bitwig.eq" deviceName="EQ+" deviceRole="audioFX" id="builtin"/>
                    </Devices>
                  </Channel>
                </Track>
                <Track contentType="audio" id="audio" name="Vocals">
                  <Channel role="regular" id="audio-channel"/>
                </Track>
                <Track contentType="audio notes" id="master" name="Master">
                  <Channel role="master" id="master-channel"/>
                </Track>
              </Structure>
              <Arrangement id="arrangement">
                <Lanes timeUnit="beats" id="lanes">
                  <Lanes track="synth" id="synth-lane">
                    <Clips id="clips">
                      <Clip time="4" duration="8" name="Verse"/>
                    </Clips>
                  </Lanes>
                  <Lanes track="audio" id="audio-lane">
                    <Clips id="audio-clips">
                      <Clip time="0" duration="16" name="Vocal">
                        <Audio channels="2" duration="16" sampleRate="48000" id="recording">
                          <File path="audio/vocal.wav"/>
                        </Audio>
                      </Clip>
                    </Clips>
                  </Lanes>
                </Lanes>
              </Arrangement>
              <Scenes><Scene id="scene" name="A"><Lanes/></Scene></Scenes>
            </Project>
        "#;
        let metadata = r#"
            <MetaData>
              <Title>Real Song</Title>
              <Artist>Actual Artist</Artist>
              <Producer>Careful Producer</Producer>
              <Genre>Electronic</Genre>
            </MetaData>
        "#;
        let bytes = archive(
            project,
            Some(metadata),
            &[("audio/vocal.wav", b"RIFF-real-audio")],
        );
        ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &bytes).expect("snapshot")
    }

    #[test]
    fn metadata_should_describe_the_project_without_inventing_a_key() {
        let metadata = read_metadata(&full_snapshot()).expect("metadata");

        assert_eq!(metadata.title.as_deref(), Some("Real Song"));
        assert_eq!(metadata.artist.as_deref(), Some("Actual Artist"));
        assert_eq!(metadata.application_name.as_deref(), Some("Bitwig Studio"));
        assert_eq!(metadata.application_version.as_deref(), Some("5.2"));
        assert_eq!(metadata.tempo, Some(127.5));
        assert_eq!(
            metadata.time_signature,
            Some(crate::TimeSignature {
                numerator: 7,
                denominator: 8,
            })
        );
        assert_eq!(metadata.tracks.total(), 2);
        assert_eq!(metadata.tracks.notes, 1);
        assert_eq!(metadata.tracks.audio, 1);
        assert_eq!(metadata.tracks.master, 1);
        assert_eq!(metadata.clip_count, 2);
        assert_eq!(metadata.scene_count, 1);
        assert_eq!(metadata.arrangement_end_beats, 16.0);
        assert_eq!(metadata.assets.embedded, 1);
        assert_eq!(metadata.assets.known_bytes, 15);
    }

    #[test]
    fn project_info_should_include_dawproject_detail() {
        let snapshot = full_snapshot();
        let info = ProjectInfo::from_snapshot_bytes(snapshot.as_bytes()).expect("summary");

        assert_eq!(info.format, ProjectFormat::Dawproject);
        assert!(info.dawproject.is_some());
        assert_eq!(info.headline(), "127.50 BPM · 7/8");
    }

    #[test]
    fn plugins_should_keep_the_standard_identity_from_the_schema() {
        let plugins = read_plugins(&full_snapshot()).expect("plugins");

        assert_eq!(plugins.len(), 5);
        assert!(plugins.iter().any(|plugin| {
            plugin.format == PluginFormat::Vst2
                && plugin.id
                    == PluginId::Vst2 {
                        unique_id: 1_483_109_208,
                    }
        }));
        assert!(plugins.iter().any(|plugin| {
            plugin.format == PluginFormat::Vst3
                && plugin.id
                    == PluginId::Vst3 {
                        tuid: [0x5653_5453, 0x6572_756d, 0x7353_6572, 0x756d_0000],
                    }
        }));
        assert!(plugins.iter().any(|plugin| {
            plugin.format == PluginFormat::Clap
                && plugin.id
                    == PluginId::Clap {
                        plugin_id: "org.surge-synth-team.surge-xt".to_owned(),
                    }
        }));
        assert!(plugins.iter().any(|plugin| {
            plugin.format == PluginFormat::AudioUnit
                && plugin.id
                    == PluginId::AudioUnit {
                        name: "aufx.delay.vendor".to_owned(),
                    }
        }));
        assert!(plugins.iter().any(|plugin| {
            plugin.format == PluginFormat::Native
                && plugin.id
                    == PluginId::DawprojectBuiltin {
                        application: "Bitwig Studio".to_owned(),
                        device_id: "bitwig.eq".to_owned(),
                    }
        }));
    }

    #[test]
    fn embedded_media_should_be_available_as_individual_assets() {
        let snapshot = full_snapshot();
        let value = serde_json::from_slice(snapshot.as_bytes()).expect("canonical JSON");
        let assets = embedded_assets_from_value(&value);

        assert_eq!(assets.len(), 1);
        assert_eq!(assets[0].path, "audio/vocal.wav");
        assert_eq!(assets[0].data, b"RIFF-real-audio");
    }

    #[test]
    fn embedded_media_should_hydrate_from_individual_asset_blobs() {
        let snapshot = full_snapshot();
        let hydrated = hydrate_embedded_assets(
            &snapshot,
            &BTreeMap::from([(
                "audio/vocal.wav".to_owned(),
                b"audio-fetched-from-cas".to_vec(),
            )]),
        )
        .expect("hydrate");
        let restored = hydrated.restore_bytes().expect("restore");
        let mut archive = zip::ZipArchive::new(Cursor::new(restored)).expect("ZIP");
        let mut audio = Vec::new();
        archive
            .by_name("audio/vocal.wav")
            .expect("embedded audio")
            .read_to_end(&mut audio)
            .expect("read embedded audio");

        assert_eq!(audio, b"audio-fetched-from-cas");
    }

    #[test]
    fn media_inventory_should_distinguish_embedded_external_and_missing_files() {
        let project = r#"
            <Project version="1.0">
              <Application name="Test" version="1"/>
              <Arrangement><Lanes><Clips>
                <Clip time="0"><Audio channels="2" duration="1" sampleRate="48000">
                  <File path="audio/inside.wav"/>
                </Audio></Clip>
                <Clip time="1"><Audio channels="2" duration="1" sampleRate="48000">
                  <File path="/library/outside.wav" external="1"/>
                </Audio></Clip>
                <Clip time="2"><Audio channels="2" duration="1" sampleRate="48000">
                  <File path="audio/missing.wav"/>
                </Audio></Clip>
              </Clips></Lanes></Arrangement>
            </Project>
        "#;
        let source = archive(project, None, &[("audio/inside.wav", b"embedded bytes")]);
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &source)
            .expect("snapshot");
        let refs = read_asset_refs(&snapshot).expect("asset refs");

        assert_eq!(refs.len(), 3);
        assert!(
            refs.iter()
                .any(|asset| asset.path == "audio/inside.wav" && asset.embedded)
        );
        assert!(
            refs.iter()
                .any(|asset| asset.path == "/library/outside.wav" && asset.external)
        );
        assert!(refs.iter().any(|asset| {
            asset.path == "audio/missing.wav" && !asset.external && !asset.embedded
        }));
    }

    #[test]
    fn arrangement_extent_should_only_count_top_level_beat_clips() {
        let project = r#"
            <Project version="1.0">
              <Application name="Test" version="1"/>
              <Arrangement>
                <Lanes timeUnit="beats">
                  <Lanes track="beat-track">
                    <Clips>
                      <Clip time="4" duration="8">
                        <Clips>
                          <Clip time="100" duration="100"/>
                        </Clips>
                      </Clip>
                    </Clips>
                  </Lanes>
                  <Lanes track="seconds-track" timeUnit="seconds">
                    <Clips>
                      <Clip time="100" duration="100"/>
                    </Clips>
                  </Lanes>
                </Lanes>
              </Arrangement>
            </Project>
        "#;
        let source = archive(project, None, &[]);
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &source)
            .expect("snapshot");

        assert_eq!(
            read_metadata(&snapshot)
                .expect("metadata")
                .arrangement_end_beats,
            12.0
        );
    }
}
