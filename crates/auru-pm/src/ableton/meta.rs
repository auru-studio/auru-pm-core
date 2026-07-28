//! Project detail read out of a Live Set: tempo, key, tracks, clips, plugins.
//!
//! Everything here is derived from the normalized XML tree, so it costs one
//! walk of an already-in-memory snapshot rather than a re-read of the source
//! file. Fields are `Option` wherever Live may legitimately omit them; a set
//! that cannot be fully understood still reports what it can rather than
//! failing the commit.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::ableton::plugins::{self, PluginRef};
use crate::ableton::refs::{self, AssetRef, RefClass};
use crate::project_format::XmlElement;

/// Tag alternatives for the main output track.
///
/// Live 12 renamed `MasterTrack` to `MainTrack`. Reading only one silently
/// yields no tempo for half the sets in existence.
const MAIN_TRACK_TAGS: [&str; 2] = ["MainTrack", "MasterTrack"];

/// Denominators addressable by Ableton's packed time-signature enum, indexed
/// by the enum's high component.
const PACKED_DENOMINATORS: [u32; 5] = [1, 2, 4, 8, 16];

/// Span of one packed time-signature denominator group.
const PACKED_STRIDE: u32 = 99;

/// Note names for [`KeyInfo::root_note`], which Live stores as a pitch class.
const NOTE_NAMES: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// Everything the project-detail view needs about a Live Set.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct AbletonMetadata {
    /// `Creator` attribute, eg `"Ableton Live 12.0.25"`.
    pub live_version: Option<String>,
    pub major_version: Option<String>,
    pub minor_version: Option<String>,
    pub schema_change_count: Option<u32>,
    pub tempo: Option<f64>,
    pub time_signature: Option<TimeSignature>,
    pub key: Option<KeyInfo>,
    pub tracks: TrackCounts,
    pub track_names: Vec<TrackSummary>,
    pub clip_count: usize,
    pub scene_count: usize,
    pub locator_count: usize,
    /// Beat position of the furthest clip end — the arrangement's extent.
    pub arrangement_end_beats: f64,
    /// Arrangement loop as `(start, length)` in beats, when enabled.
    pub loop_region: Option<(f64, f64)>,
    pub plugins: Vec<PluginRef>,
    pub assets: AssetSummary,
}

impl AbletonMetadata {
    /// Arrangement length in bars, when the time signature is known.
    pub fn arrangement_bars(&self) -> Option<f64> {
        let signature = self.time_signature?;
        let beats_per_bar = signature.beats_per_bar();
        (beats_per_bar > 0.0).then(|| self.arrangement_end_beats / beats_per_bar)
    }

    /// Third-party plugins the project needs, in listing order.
    pub fn third_party_plugins(&self) -> impl Iterator<Item = &PluginRef> {
        self.plugins.iter().filter(|plugin| plugin.is_third_party())
    }
}

/// A musical time signature as written, not as packed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TimeSignature {
    pub numerator: u32,
    pub denominator: u32,
}

impl TimeSignature {
    /// Quarter-note beats per bar — the unit Live measures clip positions in.
    pub fn beats_per_bar(self) -> f64 {
        if self.denominator == 0 {
            return 0.0;
        }
        f64::from(self.numerator) * 4.0 / f64::from(self.denominator)
    }
}

impl std::fmt::Display for TimeSignature {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.numerator, self.denominator)
    }
}

/// Live's project key, when the set declares one.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyInfo {
    /// Pitch class, 0 = C.
    pub root_note: u32,
    /// Scale name as Live writes it, eg `"Phrygian"`.
    pub scale_name: String,
    /// Whether "In Key" is engaged for the set.
    pub in_key: bool,
}

impl KeyInfo {
    /// Human-readable key, eg `"C Phrygian"`.
    pub fn label(&self) -> String {
        let note = NOTE_NAMES
            .get(self.root_note as usize % NOTE_NAMES.len())
            .copied()
            .unwrap_or("?");
        format!("{note} {}", self.scale_name)
    }
}

/// Track tally by kind.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrackCounts {
    pub midi: usize,
    pub audio: usize,
    pub group: usize,
    pub retn: usize,
}

impl TrackCounts {
    /// Every track in the set, including groups and returns.
    pub const fn total(&self) -> usize {
        self.midi + self.audio + self.group + self.retn
    }
}

/// Per-track summary for the detail listing.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrackSummary {
    /// The XML `Id` attribute — also the identity the three-way merge uses.
    pub id: Option<String>,
    pub name: String,
    pub kind: TrackKind,
    pub clip_count: usize,
    pub device_count: usize,
}

/// Kind of track, from its XML tag.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum TrackKind {
    Midi,
    Audio,
    Return,
    Group,
    #[default]
    Other,
}

impl TrackKind {
    fn from_tag(tag: &str) -> Option<Self> {
        match tag {
            "MidiTrack" => Some(Self::Midi),
            "AudioTrack" => Some(Self::Audio),
            "ReturnTrack" => Some(Self::Return),
            "GroupTrack" => Some(Self::Group),
            _ => None,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Midi => "MIDI",
            Self::Audio => "Audio",
            Self::Return => "Return",
            Self::Group => "Group",
            Self::Other => "Other",
        }
    }
}

/// Referenced-file tally, split by whether the file travels with the project.
///
/// Counts **distinct files**, not references. A single loop is referenced once
/// per clip that uses it — 25 times in the project this was built against — so
/// counting occurrences would tell a user they have 40 files to gather when
/// they really have five.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetSummary {
    pub in_folder: usize,
    pub external: usize,
    pub user_library: usize,
    pub library: usize,
    pub unresolvable: usize,
    /// Sum of `OriginalFileSize` over distinct files that recorded one.
    pub known_bytes: u64,
}

impl AssetSummary {
    /// Distinct files that must be vendored for the project to open elsewhere.
    pub const fn vendorable(&self) -> usize {
        self.external + self.user_library
    }

    /// Distinct files referenced, of every class.
    pub const fn total(&self) -> usize {
        self.in_folder + self.external + self.user_library + self.library + self.unresolvable
    }

    fn tally(refs: &[AssetRef]) -> Self {
        let mut summary = Self::default();
        let mut seen: BTreeSet<&str> = BTreeSet::new();
        let mut unresolvable_seen = false;

        for asset in refs {
            if asset.is_unresolvable() {
                // Every unresolvable ref has an empty key, so they would all
                // collapse into one another. They are not a file at all —
                // report their presence once.
                if !std::mem::replace(&mut unresolvable_seen, true) {
                    summary.unresolvable += 1;
                }
                continue;
            }
            if !seen.insert(asset.dedup_key()) {
                continue;
            }
            match asset.class {
                RefClass::InFolder => summary.in_folder += 1,
                RefClass::External => summary.external += 1,
                RefClass::UserLibrary => summary.user_library += 1,
                RefClass::Library => summary.library += 1,
                RefClass::Unresolvable => unreachable!("handled above"),
            }
            summary.known_bytes += asset.original_size.unwrap_or(0);
        }
        summary
    }
}

/// Read every supported detail from a Live Set's root `Ableton` element.
pub(crate) fn extract(root: &XmlElement) -> AbletonMetadata {
    let live_set = root.child("LiveSet");
    let asset_refs = refs::collect(root);

    let mut metadata = AbletonMetadata {
        live_version: root.attribute("Creator").map(str::to_owned),
        major_version: root.attribute("MajorVersion").map(str::to_owned),
        minor_version: root.attribute("MinorVersion").map(str::to_owned),
        schema_change_count: root
            .attribute("SchemaChangeCount")
            .and_then(|value| value.parse().ok()),
        plugins: plugins::collect(root),
        assets: AssetSummary::tally(&asset_refs),
        ..AbletonMetadata::default()
    };

    let Some(live_set) = live_set else {
        return metadata;
    };

    metadata.tempo = read_main_mixer(live_set)
        .and_then(|mixer| mixer.resolve("Tempo/Manual"))
        .and_then(|manual| manual.attribute("Value"))
        .and_then(|value| value.parse().ok());
    metadata.time_signature = read_time_signature(live_set);
    metadata.key = read_key(live_set);
    metadata.scene_count = live_set
        .child("Scenes")
        .map_or(0, |scenes| scenes.child_elements().count());
    metadata.locator_count = count_locators(live_set);
    metadata.loop_region = read_loop_region(live_set);

    for track in live_set
        .child("Tracks")
        .into_iter()
        .flat_map(XmlElement::child_elements)
    {
        let Some(kind) = TrackKind::from_tag(&track.tag) else {
            continue;
        };
        let summary = read_track(track, kind);
        match kind {
            TrackKind::Midi => metadata.tracks.midi += 1,
            TrackKind::Audio => metadata.tracks.audio += 1,
            TrackKind::Return => metadata.tracks.retn += 1,
            TrackKind::Group => metadata.tracks.group += 1,
            TrackKind::Other => {}
        }
        metadata.clip_count += summary.clip_count;
        metadata.track_names.push(summary);
    }

    metadata.arrangement_end_beats = read_arrangement_end(live_set);
    metadata
}

/// The main output track's mixer, under whichever tag this Live version used.
fn read_main_mixer(live_set: &XmlElement) -> Option<&XmlElement> {
    live_set
        .child_any(&MAIN_TRACK_TAGS)?
        .resolve("DeviceChain/Mixer")
}

/// Read the set's time signature, preferring the explicit encoding.
///
/// Live writes signatures two ways. Clips and newer structures carry explicit
/// `Numerator`/`Denominator` children; the main mixer stores a single packed
/// enum. Explicit wins whenever it is present because it needs no decoding and
/// cannot be misread.
fn read_time_signature(live_set: &XmlElement) -> Option<TimeSignature> {
    let mixer = read_main_mixer(live_set)?;
    let signature = mixer.child("TimeSignature")?;

    if let Some(explicit) = read_explicit_time_signature(signature) {
        return Some(explicit);
    }

    let packed = signature.child_value("Manual")?.parse::<u32>().ok()?;
    decode_packed_time_signature(packed)
}

/// Find an explicit `Numerator`/`Denominator` pair anywhere under `node`.
fn read_explicit_time_signature(node: &XmlElement) -> Option<TimeSignature> {
    node.descendants().find_map(|descendant| {
        let numerator = descendant.child_value("Numerator")?.parse().ok()?;
        let denominator = descendant.child_value("Denominator")?.parse().ok()?;
        Some(TimeSignature {
            numerator,
            denominator,
        })
    })
}

/// Decode Ableton's packed time-signature enum.
///
/// The value is `99 * denominator_index + (numerator - 1)`, where the index
/// selects from `[1, 2, 4, 8, 16]` and the numerator runs 1..=99. So `201`
/// decodes as index 2 (denominator 4) and numerator 4 — 4/4.
pub fn decode_packed_time_signature(packed: u32) -> Option<TimeSignature> {
    let index = (packed / PACKED_STRIDE) as usize;
    let numerator = packed % PACKED_STRIDE + 1;
    Some(TimeSignature {
        numerator,
        denominator: *PACKED_DENOMINATORS.get(index)?,
    })
}

fn read_key(live_set: &XmlElement) -> Option<KeyInfo> {
    let scale = live_set.child("ScaleInformation")?;
    let scale_name = scale.child_value("Name")?.to_owned();
    if scale_name.is_empty() {
        return None;
    }
    Some(KeyInfo {
        root_note: scale
            .child_value("RootNote")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0),
        scale_name,
        in_key: live_set.child_value("InKey") == Some("true"),
    })
}

fn read_loop_region(live_set: &XmlElement) -> Option<(f64, f64)> {
    let transport = live_set.child("Transport")?;
    if transport.child_value("LoopOn") != Some("true") {
        return None;
    }
    let start = transport.child_value("LoopStart")?.parse().ok()?;
    let length = transport.child_value("LoopLength")?.parse().ok()?;
    Some((start, length))
}

fn read_track(track: &XmlElement, kind: TrackKind) -> TrackSummary {
    let name = track
        .child("Name")
        .and_then(|node| {
            node.child_value("EffectiveName")
                .or_else(|| node.child_value("UserName"))
        })
        .unwrap_or_default()
        .to_owned();

    TrackSummary {
        id: track.id.clone(),
        name,
        kind,
        clip_count: count_clips(track),
        device_count: track
            .descendants()
            .filter(|node| node.tag == "Devices")
            .map(|devices| devices.child_elements().count())
            .sum(),
    }
}

/// Count arrangement locators.
///
/// Live nests these one level deeper than the outer element suggests —
/// `LiveSet/Locators/Locators/Locator` — so counting direct children of the
/// outer `Locators` reports the wrapper itself, or zero, rather than the
/// locators. Searching descendants for the leaf tag is correct either way.
fn count_locators(live_set: &XmlElement) -> usize {
    live_set.child("Locators").map_or(0, |locators| {
        locators
            .descendants()
            .filter(|node| node.tag == "Locator")
            .count()
    })
}

fn count_clips(node: &XmlElement) -> usize {
    node.descendants().filter(|node| is_clip(&node.tag)).count()
}

fn is_clip(tag: &str) -> bool {
    matches!(tag, "MidiClip" | "AudioClip")
}

/// Furthest clip end in the set, in beats.
fn read_arrangement_end(live_set: &XmlElement) -> f64 {
    live_set
        .descendants()
        .filter(|node| is_clip(&node.tag))
        .filter_map(|clip| clip.child_value("CurrentEnd"))
        .filter_map(|value| value.parse::<f64>().ok())
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ableton::test_support::parse_xml;

    /// A Live Set skeleton with a configurable main-track tag.
    fn live_set(main_track_tag: &str, body: &str) -> XmlElement {
        parse_xml(&format!(
            r#"<Ableton MajorVersion="5" MinorVersion="12.0_12049"
                       SchemaChangeCount="12" Creator="Ableton Live 12.0.25">
                <LiveSet>
                    <{main_track_tag}>
                        <DeviceChain><Mixer>
                            <Tempo><Manual Value="175" /></Tempo>
                            <TimeSignature><Manual Value="201" /></TimeSignature>
                        </Mixer></DeviceChain>
                    </{main_track_tag}>
                    {body}
                </LiveSet>
            </Ableton>"#
        ))
    }

    #[test]
    fn packed_time_signature_should_decode_201_as_four_four() {
        // The value carried by the real project.
        assert_eq!(
            decode_packed_time_signature(201),
            Some(TimeSignature {
                numerator: 4,
                denominator: 4
            })
        );
    }

    #[test]
    fn packed_time_signature_should_decode_across_denominator_groups() {
        // index 0 -> /1, index 2 -> /4, index 3 -> /8.
        assert_eq!(
            decode_packed_time_signature(0),
            Some(TimeSignature {
                numerator: 1,
                denominator: 1
            })
        );
        assert_eq!(
            decode_packed_time_signature(procedural_packed(2, 3)),
            Some(TimeSignature {
                numerator: 3,
                denominator: 4
            })
        );
        assert_eq!(
            decode_packed_time_signature(procedural_packed(3, 7)),
            Some(TimeSignature {
                numerator: 7,
                denominator: 8
            })
        );
    }

    fn procedural_packed(denominator_index: u32, numerator: u32) -> u32 {
        denominator_index * PACKED_STRIDE + numerator - 1
    }

    #[test]
    fn packed_time_signature_should_reject_out_of_range_denominators() {
        assert_eq!(decode_packed_time_signature(99 * 5), None);
    }

    #[test]
    fn tempo_should_read_from_main_track_on_live_12() {
        let metadata = extract(&live_set("MainTrack", ""));
        assert_eq!(metadata.tempo, Some(175.0));
        assert_eq!(
            metadata.live_version.as_deref(),
            Some("Ableton Live 12.0.25")
        );
        assert_eq!(metadata.schema_change_count, Some(12));
    }

    #[test]
    fn tempo_should_read_from_master_track_on_live_11() {
        // Live 11 and earlier name the same track `MasterTrack`.
        let metadata = extract(&live_set("MasterTrack", ""));
        assert_eq!(metadata.tempo, Some(175.0));
    }

    #[test]
    fn packed_time_signature_should_be_read_from_the_main_mixer() {
        let metadata = extract(&live_set("MainTrack", ""));
        assert_eq!(
            metadata.time_signature,
            Some(TimeSignature {
                numerator: 4,
                denominator: 4
            })
        );
    }

    #[test]
    fn explicit_numerator_should_win_over_packed_enum() {
        // Packed says 4/4; the explicit pair says 7/8 and must take priority.
        let root = parse_xml(
            r#"<Ableton><LiveSet><MainTrack><DeviceChain><Mixer>
                <Tempo><Manual Value="120" /></Tempo>
                <TimeSignature>
                    <Manual Value="201" />
                    <RemoteableTimeSignature>
                        <Numerator Value="7" />
                        <Denominator Value="8" />
                    </RemoteableTimeSignature>
                </TimeSignature>
            </Mixer></DeviceChain></MainTrack></LiveSet></Ableton>"#,
        );
        assert_eq!(
            extract(&root).time_signature,
            Some(TimeSignature {
                numerator: 7,
                denominator: 8
            })
        );
    }

    #[test]
    fn beats_per_bar_should_account_for_the_denominator() {
        assert_eq!(
            TimeSignature {
                numerator: 4,
                denominator: 4
            }
            .beats_per_bar(),
            4.0
        );
        assert_eq!(
            TimeSignature {
                numerator: 7,
                denominator: 8
            }
            .beats_per_bar(),
            3.5
        );
    }

    #[test]
    fn key_should_read_root_note_and_scale() {
        let root = live_set(
            "MainTrack",
            r#"<ScaleInformation><RootNote Value="0" /><Name Value="Phrygian" /></ScaleInformation>
               <InKey Value="true" />"#,
        );
        let key = extract(&root).key.expect("key present");
        assert_eq!(key.label(), "C Phrygian");
        assert!(key.in_key);
    }

    #[test]
    fn key_should_be_absent_when_scale_name_is_empty() {
        let root = live_set(
            "MainTrack",
            r#"<ScaleInformation><RootNote Value="3" /><Name Value="" /></ScaleInformation>"#,
        );
        assert!(extract(&root).key.is_none());
    }

    #[test]
    fn tracks_should_be_counted_by_kind() {
        let root = live_set(
            "MainTrack",
            r#"<Tracks>
                <MidiTrack Id="1"><Name><EffectiveName Value="Bass" /></Name></MidiTrack>
                <MidiTrack Id="2"><Name><EffectiveName Value="Lead" /></Name></MidiTrack>
                <AudioTrack Id="3"><Name><EffectiveName Value="Break" /></Name></AudioTrack>
                <GroupTrack Id="4"><Name><EffectiveName Value="Drums" /></Name></GroupTrack>
                <ReturnTrack Id="5"><Name><EffectiveName Value="A-Reverb" /></Name></ReturnTrack>
            </Tracks>"#,
        );
        let metadata = extract(&root);
        assert_eq!(metadata.tracks.midi, 2);
        assert_eq!(metadata.tracks.audio, 1);
        assert_eq!(metadata.tracks.group, 1);
        assert_eq!(metadata.tracks.retn, 1);
        assert_eq!(metadata.tracks.total(), 5);
        assert_eq!(metadata.track_names[0].name, "Bass");
        assert_eq!(metadata.track_names[0].id.as_deref(), Some("1"));
        assert_eq!(metadata.track_names[4].kind, TrackKind::Return);
    }

    #[test]
    fn clips_and_arrangement_extent_should_be_measured() {
        let root = live_set(
            "MainTrack",
            r#"<Tracks><MidiTrack Id="1">
                <Name><EffectiveName Value="Bass" /></Name>
                <MidiClip><CurrentStart Value="0" /><CurrentEnd Value="64" /></MidiClip>
                <MidiClip><CurrentStart Value="64" /><CurrentEnd Value="352" /></MidiClip>
            </MidiTrack></Tracks>"#,
        );
        let metadata = extract(&root);
        assert_eq!(metadata.clip_count, 2);
        assert_eq!(metadata.arrangement_end_beats, 352.0);
        // 352 beats at 4/4 is 88 bars — matches the real project.
        assert_eq!(metadata.arrangement_bars(), Some(88.0));
    }

    #[test]
    fn scenes_and_locators_should_be_counted() {
        let root = live_set(
            "MainTrack",
            r#"<Scenes><Scene Id="1" /><Scene Id="2" /><Scene Id="3" /></Scenes>
               <Locators><Locator Id="1" /></Locators>"#,
        );
        let metadata = extract(&root);
        assert_eq!(metadata.scene_count, 3);
        assert_eq!(metadata.locator_count, 1);
    }

    #[test]
    fn locator_count_should_see_through_the_nested_wrapper() {
        // Live writes `LiveSet/Locators/Locators/Locator`; counting direct
        // children of the outer element would report the wrapper, not the
        // locators.
        let root = live_set(
            "MainTrack",
            r#"<Locators><Locators>
                <Locator Id="1" /><Locator Id="2" />
            </Locators></Locators>"#,
        );
        assert_eq!(extract(&root).locator_count, 2);
    }

    #[test]
    fn an_empty_locator_wrapper_should_count_as_zero() {
        // The shape the real project ships: `<Locators><Locators /></Locators>`.
        let root = live_set("MainTrack", "<Locators><Locators /></Locators>");
        assert_eq!(extract(&root).locator_count, 0);
    }

    #[test]
    fn devices_should_be_counted_through_the_doubly_nested_device_chain() {
        // Live nests `Track/DeviceChain/DeviceChain/Devices`.
        let root = live_set(
            "MainTrack",
            r#"<Tracks><MidiTrack Id="1">
                <Name><EffectiveName Value="Bass" /></Name>
                <DeviceChain><Mixer /><DeviceChain><Devices>
                    <Eq8 /><Reverb />
                </Devices></DeviceChain></DeviceChain>
            </MidiTrack></Tracks>"#,
        );
        let metadata = extract(&root);
        assert_eq!(metadata.track_names[0].device_count, 2);
        assert_eq!(metadata.plugins.len(), 2);
    }

    #[test]
    fn loop_region_should_be_read_only_when_enabled() {
        let enabled = live_set(
            "MainTrack",
            r#"<Transport><LoopOn Value="true" /><LoopStart Value="64" />
               <LoopLength Value="256" /></Transport>"#,
        );
        assert_eq!(extract(&enabled).loop_region, Some((64.0, 256.0)));

        let disabled = live_set(
            "MainTrack",
            r#"<Transport><LoopOn Value="false" /><LoopStart Value="64" />
               <LoopLength Value="256" /></Transport>"#,
        );
        assert!(extract(&disabled).loop_region.is_none());
    }

    #[test]
    fn a_set_without_a_live_set_element_should_still_report_version() {
        // Degraded input must yield partial detail, never a panic.
        let root = parse_xml(r#"<Ableton Creator="Ableton Live 12.0.25" />"#);
        let metadata = extract(&root);
        assert_eq!(
            metadata.live_version.as_deref(),
            Some("Ableton Live 12.0.25")
        );
        assert_eq!(metadata.tracks.total(), 0);
        assert!(metadata.tempo.is_none());
        assert!(metadata.arrangement_bars().is_none());
    }

    #[test]
    fn assets_should_be_summarized_by_class() {
        let root = live_set(
            "MainTrack",
            r#"<Tracks><AudioTrack Id="1">
                <SampleRef><FileRef>
                    <RelativePathType Value="1" />
                    <RelativePath Value="../../samples/break.wav" />
                    <Path Value="E:/samples/break.wav" />
                    <OriginalFileSize Value="5907514" />
                </FileRef></SampleRef>
                <SampleRef><FileRef>
                    <RelativePathType Value="5" />
                    <RelativePath Value="Devices/Audio Effects/EQ Eight" />
                    <Path Value="" />
                </FileRef></SampleRef>
            </AudioTrack></Tracks>"#,
        );
        let assets = extract(&root).assets;
        assert_eq!(assets.external, 1);
        assert_eq!(assets.library, 1);
        assert_eq!(assets.vendorable(), 1);
        assert_eq!(assets.known_bytes, 5_907_514);
    }

    #[test]
    fn assets_should_count_distinct_files_not_references() {
        // One loop used by three clips is one file to gather, not three.
        let file_ref = r#"<SampleRef><FileRef>
            <RelativePathType Value="1" />
            <RelativePath Value="../../samples/break.wav" />
            <Path Value="E:/samples/break.wav" />
            <OriginalFileSize Value="5907514" />
        </FileRef></SampleRef>"#;
        let root = live_set(
            "MainTrack",
            &format!(
                r#"<Tracks><AudioTrack Id="1">{file_ref}{file_ref}{file_ref}</AudioTrack></Tracks>"#
            ),
        );
        let assets = extract(&root).assets;
        assert_eq!(assets.external, 1);
        assert_eq!(assets.total(), 1);
        // Size is counted once, not tripled.
        assert_eq!(assets.known_bytes, 5_907_514);
    }

    #[test]
    fn many_empty_refs_should_summarize_as_one_unresolvable_entry() {
        // The real project carries 14 of these; they are not 14 files.
        let empty = r#"<SampleRef><FileRef>
            <RelativePathType Value="0" />
            <RelativePath Value="" /><Path Value="" />
        </FileRef></SampleRef>"#;
        let root = live_set(
            "MainTrack",
            &format!(r#"<Tracks><AudioTrack Id="1">{empty}{empty}{empty}</AudioTrack></Tracks>"#),
        );
        let assets = extract(&root).assets;
        assert_eq!(assets.unresolvable, 1);
        assert_eq!(assets.vendorable(), 0);
    }
}
