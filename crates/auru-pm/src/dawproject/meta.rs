use serde::{Deserialize, Serialize};

use crate::project_format::{PortableSnapshot, XmlContent, XmlElement};
use crate::{PluginRef, TimeSignature};

use super::DawprojectAssetSummary;

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct DawprojectMetadata {
    pub format_version: Option<String>,
    pub application_name: Option<String>,
    pub application_version: Option<String>,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub original_artist: Option<String>,
    pub composer: Option<String>,
    pub songwriter: Option<String>,
    pub producer: Option<String>,
    pub arranger: Option<String>,
    pub year: Option<String>,
    pub genre: Option<String>,
    pub copyright: Option<String>,
    pub website: Option<String>,
    pub comment: Option<String>,
    pub tempo: Option<f64>,
    pub time_signature: Option<TimeSignature>,
    pub tracks: DawprojectTrackCounts,
    pub track_names: Vec<DawprojectTrackSummary>,
    pub clip_count: usize,
    pub scene_count: usize,
    pub marker_count: usize,
    pub arrangement_end_beats: f64,
    pub plugins: Vec<PluginRef>,
    pub assets: DawprojectAssetSummary,
}

impl DawprojectMetadata {
    /// Exporting application and version as one human-readable label.
    pub fn application_label(&self) -> Option<String> {
        let label = [
            self.application_name.as_deref(),
            self.application_version.as_deref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" ");
        (!label.is_empty()).then_some(label)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DawprojectTrackCounts {
    pub notes: usize,
    pub audio: usize,
    pub hybrid: usize,
    pub group: usize,
    pub effect: usize,
    pub submix: usize,
    pub other: usize,
    pub master: usize,
}

impl DawprojectTrackCounts {
    pub const fn total(&self) -> usize {
        self.notes + self.audio + self.hybrid + self.group + self.effect + self.submix + self.other
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DawprojectTrackKind {
    Notes,
    Audio,
    Hybrid,
    Group,
    Effect,
    Submix,
    Master,
    #[default]
    Other,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DawprojectTrackSummary {
    pub id: Option<String>,
    pub name: String,
    pub kind: DawprojectTrackKind,
}

pub(crate) fn extract(portable: &PortableSnapshot) -> DawprojectMetadata {
    let root = &portable.project.root;
    let asset_refs = super::assets::collect(portable);
    extract_parts(
        root,
        portable.metadata.as_ref().map(|document| &document.root),
        &asset_refs,
    )
}

pub(crate) fn extract_parts(
    root: &XmlElement,
    document_metadata: Option<&XmlElement>,
    asset_refs: &[super::DawprojectAssetRef],
) -> DawprojectMetadata {
    let application = root.child("Application");
    let mut metadata = DawprojectMetadata {
        format_version: string_attribute(root, "version"),
        application_name: application.and_then(|node| string_attribute(node, "name")),
        application_version: application.and_then(|node| string_attribute(node, "version")),
        tempo: root
            .resolve("Transport/Tempo")
            .and_then(|node| number_attribute(node, "value")),
        time_signature: read_time_signature(root),
        clip_count: root.descendants().filter(|node| node.tag == "Clip").count(),
        scene_count: root
            .child("Scenes")
            .map(|scenes| {
                scenes
                    .child_elements()
                    .filter(|node| node.tag == "Scene")
                    .count()
            })
            .unwrap_or(0),
        marker_count: root
            .descendants()
            .filter(|node| node.tag == "Marker")
            .count(),
        arrangement_end_beats: arrangement_end(root),
        plugins: super::plugins::collect(root),
        assets: DawprojectAssetSummary::from_refs(asset_refs),
        ..DawprojectMetadata::default()
    };
    read_tracks(root, &mut metadata);
    read_document_metadata(document_metadata, &mut metadata);
    metadata
}

fn read_time_signature(root: &XmlElement) -> Option<TimeSignature> {
    let signature = root.resolve("Transport/TimeSignature")?;
    Some(TimeSignature {
        numerator: signature.attribute("numerator")?.parse().ok()?,
        denominator: signature.attribute("denominator")?.parse().ok()?,
    })
}

fn read_tracks(root: &XmlElement, metadata: &mut DawprojectMetadata) {
    let Some(structure) = root.child("Structure") else {
        return;
    };
    for track in structure
        .descendants()
        .filter(|element| element.tag == "Track")
    {
        let kind = track_kind(track);
        match kind {
            DawprojectTrackKind::Notes => metadata.tracks.notes += 1,
            DawprojectTrackKind::Audio => metadata.tracks.audio += 1,
            DawprojectTrackKind::Hybrid => metadata.tracks.hybrid += 1,
            DawprojectTrackKind::Group => metadata.tracks.group += 1,
            DawprojectTrackKind::Effect => metadata.tracks.effect += 1,
            DawprojectTrackKind::Submix => metadata.tracks.submix += 1,
            DawprojectTrackKind::Master => metadata.tracks.master += 1,
            DawprojectTrackKind::Other => metadata.tracks.other += 1,
        }
        metadata.track_names.push(DawprojectTrackSummary {
            id: track.id.clone(),
            name: track
                .attribute("name")
                .filter(|name| !name.is_empty())
                .unwrap_or("Untitled track")
                .to_owned(),
            kind,
        });
    }
}

pub(crate) fn track_kind(track: &XmlElement) -> DawprojectTrackKind {
    let role = track
        .child("Channel")
        .and_then(|channel| channel.attribute("role"));
    match role {
        Some("master") => return DawprojectTrackKind::Master,
        Some("effect") => return DawprojectTrackKind::Effect,
        Some("submix") => return DawprojectTrackKind::Submix,
        _ => {}
    }
    if track.child_elements().any(|element| element.tag == "Track") {
        return DawprojectTrackKind::Group;
    }
    let content = track.attribute("contentType").unwrap_or_default();
    let audio = content.split_ascii_whitespace().any(|kind| kind == "audio");
    let notes = content.split_ascii_whitespace().any(|kind| kind == "notes");
    match (audio, notes) {
        (true, true) => DawprojectTrackKind::Hybrid,
        (true, false) => DawprojectTrackKind::Audio,
        (false, true) => DawprojectTrackKind::Notes,
        (false, false) => DawprojectTrackKind::Other,
    }
}

fn read_document_metadata(root: Option<&XmlElement>, target: &mut DawprojectMetadata) {
    let Some(root) = root else {
        return;
    };
    target.title = child_text(root, "Title");
    target.artist = child_text(root, "Artist");
    target.album = child_text(root, "Album");
    target.original_artist = child_text(root, "OriginalArtist");
    target.composer = child_text(root, "Composer");
    target.songwriter = child_text(root, "Songwriter");
    target.producer = child_text(root, "Producer");
    target.arranger = child_text(root, "Arranger");
    target.year = child_text(root, "Year");
    target.genre = child_text(root, "Genre");
    target.copyright = child_text(root, "Copyright");
    target.website = child_text(root, "Website");
    target.comment = child_text(root, "Comment");
}

fn child_text(root: &XmlElement, tag: &str) -> Option<String> {
    let child = root.child(tag)?;
    let mut value = String::new();
    for content in &child.children {
        match content {
            XmlContent::Text { text } => value.push_str(text),
            XmlContent::Cdata { cdata } => value.push_str(cdata),
            _ => {}
        }
    }
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn arrangement_end(root: &XmlElement) -> f64 {
    let Some(lanes) = root.resolve("Arrangement/Lanes") else {
        return 0.0;
    };
    arrangement_lanes_end(lanes, None)
}

/// Furthest end of a top-level arrangement clip measured in beats.
///
/// DAWproject permits seconds and beats in one document, and an audio clip can
/// contain nested clips whose times are local to that clip. Only the clips
/// directly carried by a track lane describe the song's arrangement extent.
fn arrangement_lanes_end(lanes: &XmlElement, inherited_unit: Option<&str>) -> f64 {
    let unit = lanes.attribute("timeUnit").or(inherited_unit);
    let own_end = if lanes.attribute("track").is_some() && unit == Some("beats") {
        lanes
            .child_elements()
            .filter(|element| element.tag == "Clips")
            .flat_map(XmlElement::child_elements)
            .filter(|element| element.tag == "Clip")
            .filter_map(|clip| {
                Some((
                    number_attribute(clip, "time")?,
                    number_attribute(clip, "duration")?,
                ))
            })
            .map(|(time, duration)| time + duration)
            .fold(0.0, f64::max)
    } else {
        0.0
    };
    lanes
        .child_elements()
        .filter(|element| element.tag == "Lanes")
        .map(|child| arrangement_lanes_end(child, unit))
        .fold(own_end, f64::max)
}

fn string_attribute(element: &XmlElement, name: &str) -> Option<String> {
    element
        .attribute(name)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn number_attribute(element: &XmlElement, name: &str) -> Option<f64> {
    element.attribute(name)?.parse().ok()
}
