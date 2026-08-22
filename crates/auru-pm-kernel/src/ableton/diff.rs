//! Comparing two versions of a Live Set.
//!
//! Before this, comparing two Ableton versions could only say "the project XML
//! changed" — true, and useless. What a person wants to know is which track
//! they touched, whether a clip moved, what device they added.
//!
//! The answers are expressed in the vocabulary the diff surface already
//! speaks ([`ChannelDiff`], [`ChangeRow`], [`ChangeTag`]) rather than a
//! parallel Ableton-shaped one, so nothing downstream needs to learn a second
//! way to describe a change.
//!
//! # What this reports, and what it does not
//!
//! Reported: tempo, time signature, key, Live version, scene and locator
//! counts, arrangement length; tracks added, removed and renamed; clips added,
//! removed, moved and resized; devices added, removed and reordered;
//! instrument changes; volume, pan and mute.
//!
//! Deliberately not reported yet: individual MIDI notes, automation envelope
//! shapes, warp markers, and plugin parameter values. A real project holds
//! thousands of parameter nodes, and listing them would bury the handful of
//! changes a person actually made. A device whose stored state changed is
//! reported once, as one row, rather than as its parameters.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::ableton::meta::{self, AbletonMetadata};
use crate::diff::{
    ChangeKind, ChangeRow, ChangeTag, ChannelDiff, ChannelKind, ProjectDiff, TimeSig, fmt_f64,
    fmt_pan, fmt_pos_beats, linear_to_db,
};
use crate::project_format::XmlElement;

/// Cap on rows shown for one track.
///
/// A track whose every clip moved would otherwise produce hundreds of rows and
/// drown the few that matter. Past this, the remainder is summarized.
const MAX_ROWS_PER_CHANNEL: usize = 24;

/// Compare two Ableton snapshots.
///
/// `None` when either side is not a Live Set we can read — including an
/// Ableton snapshot with no `LiveSet` element — so the caller falls back to
/// the format-agnostic summary rather than showing an empty comparison.
pub(crate) fn structured_diff(ancestor: &Value, current: &Value) -> Option<ProjectDiff> {
    let before = super::root_from_value(ancestor)?;
    let after = super::root_from_value(current)?;
    // Both sides must have something to compare; a bare `<Ableton/>` does not.
    before.child("LiveSet")?;
    after.child("LiveSet")?;

    let before_meta = meta::extract(&before);
    let after_meta = meta::extract(&after);
    let time_sig = after_meta
        .time_signature
        .map_or((4, 4), |sig| (sig.numerator, sig.denominator));

    Some(ProjectDiff {
        project_changes: project_changes(&before_meta, &after_meta),
        time_sig,
        channels: channel_diffs(&before, &after, time_sig),
    })
}

fn project_changes(before: &AbletonMetadata, after: &AbletonMetadata) -> Vec<String> {
    let mut changes = Vec::new();

    match (before.tempo, after.tempo) {
        (Some(a), Some(b)) if (a - b).abs() > f64::EPSILON => {
            changes.push(format!("Tempo: {} → {}", fmt_f64(a), fmt_f64(b)));
        }
        (None, Some(b)) => changes.push(format!("Tempo: {}", fmt_f64(b))),
        _ => {}
    }

    if before.time_signature != after.time_signature {
        if let (Some(a), Some(b)) = (before.time_signature, after.time_signature) {
            changes.push(format!("Time signature: {a} → {b}"));
        }
    }

    if before.key != after.key {
        match (&before.key, &after.key) {
            (Some(a), Some(b)) => changes.push(format!("Key: {} → {}", a.label(), b.label())),
            (None, Some(b)) => changes.push(format!("Key: {}", b.label())),
            (Some(a), None) => changes.push(format!("Key removed (was {})", a.label())),
            (None, None) => {}
        }
    }

    if before.live_version != after.live_version {
        if let (Some(a), Some(b)) = (&before.live_version, &after.live_version) {
            changes.push(format!("Saved with: {a} → {b}"));
        }
    }

    push_count_change(
        &mut changes,
        "Scenes",
        before.scene_count,
        after.scene_count,
    );
    push_count_change(
        &mut changes,
        "Locators",
        before.locator_count,
        after.locator_count,
    );

    if let (Some(a), Some(b)) = (before.arrangement_bars(), after.arrangement_bars()) {
        if (a - b).abs() >= 1.0 {
            changes.push(format!(
                "Arrangement: {} → {} bars",
                fmt_f64(a.round()),
                fmt_f64(b.round())
            ));
        }
    }

    changes
}

fn push_count_change(changes: &mut Vec<String>, label: &str, before: usize, after: usize) {
    if before != after {
        changes.push(format!("{label}: {before} → {after}"));
    }
}

/// One card per track, matched by the `Id` the merge also matches on.
fn channel_diffs(
    before: &XmlElement,
    after: &XmlElement,
    time_sig: (u32, u32),
) -> Vec<ChannelDiff> {
    let time_sig = TimeSig {
        numerator: time_sig.0,
        denominator: time_sig.1,
    };
    let before_tracks = tracks(before);
    let after_tracks = tracks(after);

    let mut cards = Vec::new();

    // Current tracks first, in their own order — added and modified.
    for (key, track) in &after_tracks {
        match before_tracks.get(key) {
            Some(previous) => {
                if let Some(card) = modified_track(previous, track, time_sig) {
                    cards.push(card);
                }
            }
            None => cards.push(added_track(track)),
        }
    }
    // Then anything that is gone.
    for (key, track) in &before_tracks {
        if !after_tracks.contains_key(key) {
            cards.push(removed_track(track));
        }
    }

    cards
}

/// Tracks keyed by `Id`, which is unique among siblings.
fn tracks(root: &XmlElement) -> BTreeMap<String, &XmlElement> {
    let mut out = BTreeMap::new();
    let Some(list) = root.resolve("LiveSet/Tracks") else {
        return out;
    };
    for (index, track) in list.child_elements().enumerate() {
        if track_kind(&track.tag).is_none() {
            continue;
        }
        // Fall back to position for the rare set with an unlabelled track.
        let key = track.id.clone().unwrap_or_else(|| format!("@{index}"));
        out.insert(key, track);
    }
    out
}

fn track_kind(tag: &str) -> Option<ChannelKind> {
    match tag {
        "MidiTrack" => Some(ChannelKind::Midi),
        "AudioTrack" => Some(ChannelKind::Audio),
        // Groups and returns carry no clips of their own; they are structure.
        "GroupTrack" | "ReturnTrack" => Some(ChannelKind::Other),
        _ => None,
    }
}

fn track_name(track: &XmlElement) -> String {
    track
        .child("Name")
        .and_then(|name| {
            name.child_value("EffectiveName")
                .or_else(|| name.child_value("UserName"))
        })
        .filter(|name| !name.is_empty())
        .unwrap_or(&track.tag)
        .to_owned()
}

fn added_track(track: &XmlElement) -> ChannelDiff {
    let clips = clips(track);
    ChannelDiff {
        name: track_name(track),
        kind: channel_kind(track),
        status: ChangeKind::Add,
        clips_added: clips.len(),
        clips_removed: 0,
        clips_modified: 0,
        rows: Vec::new(),
    }
}

fn removed_track(track: &XmlElement) -> ChannelDiff {
    let clips = clips(track);
    ChannelDiff {
        name: track_name(track),
        kind: channel_kind(track),
        status: ChangeKind::Remove,
        clips_added: 0,
        clips_removed: clips.len(),
        clips_modified: 0,
        rows: Vec::new(),
    }
}

/// `None` when nothing about the track changed, so unchanged tracks produce no
/// card at all.
fn modified_track(
    before: &XmlElement,
    after: &XmlElement,
    time_sig: TimeSig,
) -> Option<ChannelDiff> {
    let mut rows = Vec::new();

    let before_name = track_name(before);
    let after_name = track_name(after);
    if before_name != after_name {
        rows.push(ChangeRow {
            tag: ChangeTag::Renamed,
            kind: ChangeKind::Modify,
            target: after_name.clone(),
            before: Some(before_name),
            after: Some(after_name.clone()),
        });
    }

    let (clips_added, clips_removed, clips_modified) =
        diff_clips(before, after, time_sig, &mut rows);
    diff_mixer(before, after, &mut rows);
    diff_devices(before, after, &mut rows);

    let changed = clips_added + clips_removed + clips_modified > 0 || !rows.is_empty();
    if !changed {
        return None;
    }

    truncate_rows(&mut rows);
    Some(ChannelDiff {
        name: after_name,
        kind: channel_kind(after),
        status: ChangeKind::Modify,
        clips_added,
        clips_removed,
        clips_modified,
        rows,
    })
}

/// A track hosting a third-party plugin reads as a plugin channel.
fn channel_kind(track: &XmlElement) -> ChannelKind {
    if devices(track)
        .iter()
        .any(|(_, device)| matches!(device.tag.as_str(), "PluginDevice" | "AuPluginDevice"))
    {
        return ChannelKind::Plugin;
    }
    track_kind(&track.tag).unwrap_or(ChannelKind::Other)
}

/// Clips in one track, keyed within that track.
///
/// Clip `Id`s are only unique among siblings — in a real project several
/// tracks each hold clips numbered `0` and `1` — so a global key would pair
/// clips from unrelated tracks. The occurrence counter additionally guards
/// against a set that repeats an id between its session and arrangement lanes.
fn clips(track: &XmlElement) -> BTreeMap<String, &XmlElement> {
    let mut out = BTreeMap::new();
    let mut seen: BTreeMap<&str, usize> = BTreeMap::new();
    for clip in track
        .descendants()
        .filter(|node| matches!(node.tag.as_str(), "MidiClip" | "AudioClip"))
    {
        let id = clip.id.as_deref().unwrap_or("?");
        let occurrence = seen.entry(id).or_insert(0);
        out.insert(format!("{id}#{occurrence}"), clip);
        *occurrence += 1;
    }
    out
}

fn diff_clips(
    before: &XmlElement,
    after: &XmlElement,
    time_sig: TimeSig,
    rows: &mut Vec<ChangeRow>,
) -> (usize, usize, usize) {
    let before_clips = clips(before);
    let after_clips = clips(after);
    let (mut added, mut removed, mut modified) = (0, 0, 0);

    let mut unmatched_new: Vec<&XmlElement> = Vec::new();
    let mut unmatched_gone: Vec<&XmlElement> = Vec::new();

    for (key, clip) in &after_clips {
        match before_clips.get(key) {
            None => unmatched_new.push(clip),
            Some(previous) => {
                let mut clip_changed = false;
                if start(previous) != start(clip) {
                    clip_changed = true;
                    rows.push(ChangeRow {
                        tag: ChangeTag::Moved,
                        kind: ChangeKind::Modify,
                        target: clip_name(clip, time_sig),
                        before: Some(position(previous, time_sig)),
                        after: Some(position(clip, time_sig)),
                    });
                }
                if length(previous) != length(clip) {
                    clip_changed = true;
                    rows.push(ChangeRow {
                        tag: ChangeTag::Length,
                        kind: ChangeKind::Modify,
                        target: clip_name(clip, time_sig),
                        before: Some(beats(length(previous))),
                        after: Some(beats(length(clip))),
                    });
                }
                // Anything else inside the clip — notes, envelopes, warp
                // markers — counts as modified without being enumerated.
                if !clip_changed && *previous != *clip {
                    clip_changed = true;
                }
                if clip_changed {
                    modified += 1;
                }
            }
        }
    }

    for (key, clip) in &before_clips {
        if !after_clips.contains_key(key) {
            unmatched_gone.push(clip);
        }
    }

    // Live reissues a clip's `Id` when its contents are edited — two clips in
    // a real project kept their exact position and length across a save while
    // their ids went 11→13 and 10→12. Matching only by id would report each of
    // those as a deletion *and* an insertion, which reads as far more churn
    // than actually happened. A new clip sitting at exactly the same place and
    // length as one that vanished is the same clip, edited.
    unmatched_gone.retain(|gone| {
        let paired = unmatched_new
            .iter()
            .position(|new| start(new) == start(gone) && length(new) == length(gone));
        match paired {
            Some(index) => {
                unmatched_new.remove(index);
                modified += 1;
                false
            }
            None => true,
        }
    });

    for clip in unmatched_new {
        added += 1;
        rows.push(ChangeRow {
            tag: ChangeTag::Added,
            kind: ChangeKind::Add,
            target: clip_name(clip, time_sig),
            before: None,
            after: Some(position(clip, time_sig)),
        });
    }
    for clip in unmatched_gone {
        removed += 1;
        rows.push(ChangeRow {
            tag: ChangeTag::Removed,
            kind: ChangeKind::Remove,
            target: clip_name(clip, time_sig),
            before: Some(position(clip, time_sig)),
            after: None,
        });
    }

    (added, removed, modified)
}

fn start(clip: &XmlElement) -> Option<f64> {
    clip.child_value("CurrentStart")?.parse().ok()
}

fn end(clip: &XmlElement) -> Option<f64> {
    clip.child_value("CurrentEnd")?.parse().ok()
}

fn length(clip: &XmlElement) -> Option<f64> {
    Some(end(clip)? - start(clip)?)
}

fn position(clip: &XmlElement, time_sig: TimeSig) -> String {
    start(clip).map_or_else(|| "?".to_owned(), |beats| fmt_pos_beats(beats, time_sig))
}

fn beats(value: Option<f64>) -> String {
    value.map_or_else(
        || "?".to_owned(),
        |beats| format!("{} beats", fmt_f64(beats)),
    )
}

/// A clip's display name, falling back to where it sits.
///
/// MIDI clips are usually unnamed — every clip in a real project had an empty
/// `Name` — so "clip at 5.1.1" identifies it far better than an empty string.
fn clip_name(clip: &XmlElement, time_sig: TimeSig) -> String {
    clip.child_value("Name")
        .filter(|name| !name.is_empty())
        .map_or_else(
            || format!("clip at {}", position(clip, time_sig)),
            str::to_owned,
        )
}

fn diff_mixer(before: &XmlElement, after: &XmlElement, rows: &mut Vec<ChangeRow>) {
    let (Some(before_mixer), Some(after_mixer)) = (mixer(before), mixer(after)) else {
        return;
    };

    if let (Some(a), Some(b)) = (
        manual_f64(before_mixer, "Volume"),
        manual_f64(after_mixer, "Volume"),
    ) {
        if (a - b).abs() > f64::EPSILON {
            rows.push(ChangeRow {
                tag: ChangeTag::Volume,
                kind: ChangeKind::Modify,
                target: "Volume".to_owned(),
                before: Some(format!("{} dB", fmt_f64(linear_to_db(a)))),
                after: Some(format!("{} dB", fmt_f64(linear_to_db(b)))),
            });
        }
    }

    if let (Some(a), Some(b)) = (
        manual_f64(before_mixer, "Pan"),
        manual_f64(after_mixer, "Pan"),
    ) {
        if (a - b).abs() > f64::EPSILON {
            rows.push(ChangeRow {
                tag: ChangeTag::Pan,
                kind: ChangeKind::Modify,
                target: "Pan".to_owned(),
                before: Some(fmt_pan(a)),
                after: Some(fmt_pan(b)),
            });
        }
    }

    // Live stores audibility, not mute: `Speaker/Manual` is `true` when the
    // track *is* heard. Reading it as a mute flag would report every change
    // backwards.
    let audible = |mixer: &XmlElement| {
        mixer
            .child("Speaker")
            .and_then(|speaker| speaker.child_value("Manual"))
            .map(|value| value == "true")
    };
    if let (Some(a), Some(b)) = (audible(before_mixer), audible(after_mixer)) {
        if a != b {
            rows.push(ChangeRow {
                tag: ChangeTag::Muted,
                kind: ChangeKind::Modify,
                target: "Mute".to_owned(),
                before: Some(muted_label(!a)),
                after: Some(muted_label(!b)),
            });
        }
    }

    // `ChangeTag::Solo` is intentionally unmapped: a real Live 12 set carries
    // no track-level solo state, only `Mixer/SoloSink` and a set-wide
    // `SoloOrPflSavedValue`, and guessing which of those means "soloed" would
    // report changes that did not happen.
}

fn muted_label(muted: bool) -> String {
    if muted { "muted" } else { "unmuted" }.to_owned()
}

fn mixer(track: &XmlElement) -> Option<&XmlElement> {
    track.resolve("DeviceChain/Mixer")
}

fn manual_f64(mixer: &XmlElement, tag: &str) -> Option<f64> {
    mixer.child(tag)?.child_value("Manual")?.parse().ok()
}

/// Devices in a track's main chain, **in chain order**.
///
/// Order is not incidental here: a device chain is a signal chain, so the
/// sequence is part of what the track sounds like. This returns a `Vec` rather
/// than a map for exactly that reason — a sorted map would silently normalize
/// the order away and make reordering undetectable.
///
/// Live nests the chain twice (`DeviceChain/DeviceChain/Devices`), and device
/// `Id`s restart per chain, so entries are keyed by name plus an occurrence
/// counter the same way clips are.
fn devices(track: &XmlElement) -> Vec<(String, &XmlElement)> {
    let Some(list) = track.resolve("DeviceChain/DeviceChain/Devices") else {
        return Vec::new();
    };
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    list.child_elements()
        .map(|device| {
            let label = device_name(device);
            let occurrence = seen.entry(label.clone()).or_insert(0);
            let key = format!("{label}#{occurrence}");
            *occurrence += 1;
            (key, device)
        })
        .collect()
}

/// Look up a device by the key [`devices`] assigned it.
fn find_device<'a>(devices: &[(String, &'a XmlElement)], key: &str) -> Option<&'a XmlElement> {
    devices
        .iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, device)| *device)
}

/// What to call a device: its user-given name, else the plugin it hosts, else
/// the Live device's own name.
fn device_name(device: &XmlElement) -> String {
    if let Some(name) = device
        .child_value("UserName")
        .filter(|name| !name.is_empty())
    {
        return name.to_owned();
    }
    for node in device.descendants() {
        let hosted = match node.tag.as_str() {
            "VstPluginInfo" => node.child_value("PlugName"),
            "Vst3PluginInfo" | "AuPluginInfo" => node.child_value("Name"),
            _ => None,
        };
        if let Some(name) = hosted.filter(|name| !name.is_empty()) {
            return name.to_owned();
        }
    }
    device.tag.clone()
}

fn diff_devices(before: &XmlElement, after: &XmlElement, rows: &mut Vec<ChangeRow>) {
    let before_devices = devices(before);
    let after_devices = devices(after);
    let key_of = |entry: &(String, &XmlElement)| entry.0.clone();
    let before_keys: Vec<String> = before_devices.iter().map(key_of).collect();
    let after_keys: Vec<String> = after_devices.iter().map(key_of).collect();

    for (key, device) in &after_devices {
        if !before_keys.contains(key) {
            rows.push(ChangeRow {
                tag: ChangeTag::FxAdded,
                kind: ChangeKind::Add,
                target: device_name(device),
                before: None,
                after: Some(device_name(device)),
            });
        }
    }
    for (key, device) in &before_devices {
        if !after_keys.contains(key) {
            rows.push(ChangeRow {
                tag: ChangeTag::FxRemoved,
                kind: ChangeKind::Remove,
                target: device_name(device),
                before: Some(device_name(device)),
                after: None,
            });
        }
    }

    // Same devices, different order — a real change, because chain order is
    // signal order and therefore sound. Comparing the sequences directly is
    // what makes this visible; a set comparison would call it unchanged.
    if before_keys != after_keys {
        let mut sorted_before = before_keys.clone();
        let mut sorted_after = after_keys.clone();
        sorted_before.sort();
        sorted_after.sort();
        if sorted_before == sorted_after {
            rows.push(ChangeRow {
                tag: ChangeTag::FxReordered,
                kind: ChangeKind::Modify,
                target: "Device chain".to_owned(),
                before: Some(join_chain(&before_keys)),
                after: Some(join_chain(&after_keys)),
            });
        }
    }

    // The instrument is the first device in the chain; swapping it changes
    // what the track sounds like more than anything else on this list.
    let instrument =
        |devices: &[(String, &XmlElement)]| devices.first().map(|(_, device)| device_name(device));
    let before_instrument = instrument(&before_devices);
    let after_instrument = instrument(&after_devices);
    if before_instrument != after_instrument {
        if let (Some(a), Some(b)) = (&before_instrument, &after_instrument) {
            rows.push(ChangeRow {
                tag: ChangeTag::InstrumentChanged,
                kind: ChangeKind::Modify,
                target: b.clone(),
                before: Some(a.clone()),
                after: Some(b.clone()),
            });
        }
    }

    // A device whose stored state moved — a preset tweak, a knob turn — is one
    // row. Enumerating its parameters would bury everything else: a single
    // real project holds over three thousand of them.
    for (key, device) in &after_devices {
        let Some(previous) = find_device(&before_devices, key) else {
            continue;
        };
        if previous != *device {
            rows.push(ChangeRow {
                tag: ChangeTag::PluginPreset,
                kind: ChangeKind::Modify,
                target: device_name(device),
                before: None,
                after: Some("settings changed".to_owned()),
            });
        }
    }
}

/// Render a chain as `"EQ Eight → Reverb"`, dropping the occurrence suffixes.
fn join_chain(keys: &[String]) -> String {
    keys.iter()
        .map(|key| key.rsplit_once('#').map_or(key.as_str(), |(name, _)| name))
        .collect::<Vec<_>>()
        .join(" → ")
}

/// Keep a card readable, saying plainly what was left out.
fn truncate_rows(rows: &mut Vec<ChangeRow>) {
    if rows.len() <= MAX_ROWS_PER_CHANNEL {
        return;
    }
    let hidden = rows.len() - MAX_ROWS_PER_CHANNEL;
    rows.truncate(MAX_ROWS_PER_CHANNEL);
    rows.push(ChangeRow {
        tag: ChangeTag::Moved,
        kind: ChangeKind::Modify,
        target: format!(
            "and {hidden} more change{}",
            if hidden == 1 { "" } else { "s" }
        ),
        before: None,
        after: None,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProjectFormat;
    use crate::project_format::ProjectSnapshot;

    /// Build a canonical snapshot value from Live Set body XML.
    fn snapshot(body: &str) -> Value {
        let xml = format!(
            r#"<Ableton MajorVersion="5" Creator="Ableton Live 12.0.25"><LiveSet>
                 <MainTrack><DeviceChain><Mixer>
                   <Tempo><Manual Value="175" /></Tempo>
                   <TimeSignature><Manual Value="201" /></TimeSignature>
                 </Mixer></DeviceChain></MainTrack>
                 {body}
               </LiveSet></Ableton>"#
        );
        let snapshot =
            ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, xml.as_bytes())
                .expect("normalize");
        serde_json::from_slice(snapshot.as_bytes()).expect("parse canonical")
    }

    /// A MIDI track with optional clips and devices.
    fn track(id: &str, name: &str, clips: &str, devices: &str) -> String {
        format!(
            r#"<MidiTrack Id="{id}">
                 <Name><EffectiveName Value="{name}" /></Name>
                 <DeviceChain>
                   <Mixer>
                     <Volume><Manual Value="1" /></Volume>
                     <Pan><Manual Value="0" /></Pan>
                     <Speaker><Manual Value="true" /></Speaker>
                   </Mixer>
                   <DeviceChain><Devices>{devices}</Devices></DeviceChain>
                 </DeviceChain>
                 {clips}
               </MidiTrack>"#
        )
    }

    fn clip(id: &str, start: u32, end: u32) -> String {
        format!(
            r#"<MidiClip Id="{id}"><CurrentStart Value="{start}" /><CurrentEnd Value="{end}" /><Name Value="" /></MidiClip>"#
        )
    }

    fn tracks_xml(body: &str) -> String {
        format!("<Tracks>{body}</Tracks>")
    }

    fn diff(before: &str, after: &str) -> ProjectDiff {
        structured_diff(&snapshot(before), &snapshot(after)).expect("an Ableton diff")
    }

    fn card<'a>(diff: &'a ProjectDiff, name: &str) -> &'a ChannelDiff {
        diff.channels
            .iter()
            .find(|channel| channel.name == name)
            .unwrap_or_else(|| panic!("a card for {name}: {:?}", diff.channels))
    }

    fn tags(card: &ChannelDiff) -> Vec<ChangeTag> {
        card.rows.iter().map(|row| row.tag).collect()
    }

    #[test]
    fn an_unchanged_project_should_report_nothing() {
        let project = tracks_xml(&track("1", "Reese", &clip("0", 0, 32), ""));
        let diff = diff(&project, &project);
        assert!(diff.project_changes.is_empty());
        assert!(
            diff.channels.is_empty(),
            "unchanged tracks produce no cards"
        );
    }

    #[test]
    fn a_renamed_track_should_be_reported_once() {
        let before = tracks_xml(&track("1", "Reese", "", ""));
        let after = tracks_xml(&track("1", "Screech", "", ""));
        let diff = diff(&before, &after);

        let card = card(&diff, "Screech");
        assert_eq!(card.status, ChangeKind::Modify);
        assert_eq!(tags(card), vec![ChangeTag::Renamed]);
        assert_eq!(card.rows[0].before.as_deref(), Some("Reese"));
        assert_eq!(card.rows[0].after.as_deref(), Some("Screech"));
    }

    #[test]
    fn added_and_removed_tracks_should_be_reported() {
        let before = tracks_xml(&track("1", "Reese", "", ""));
        let after = tracks_xml(&format!(
            "{}{}",
            track("1", "Reese", "", ""),
            track("2", "Lead", &clip("0", 0, 16), "")
        ));
        let forward = diff(&before, &after);
        assert_eq!(card(&forward, "Lead").status, ChangeKind::Add);
        assert_eq!(card(&forward, "Lead").clips_added, 1);

        let reversed = diff(&after, &before);
        assert_eq!(card(&reversed, "Lead").status, ChangeKind::Remove);
        assert_eq!(card(&reversed, "Lead").clips_removed, 1);
    }

    #[test]
    fn clips_should_be_matched_within_their_own_track() {
        // Two tracks each holding clips 0 and 1 — the shape a real project
        // has. A global key would pair track 1's clip with track 2's and
        // report changes on both that never happened.
        let before = tracks_xml(&format!(
            "{}{}",
            track("1", "Reese", &clip("0", 0, 32), ""),
            track("2", "Lead", &clip("0", 64, 96), "")
        ));
        let after = tracks_xml(&format!(
            "{}{}",
            track("1", "Reese", &clip("0", 0, 32), ""),
            track("2", "Lead", &clip("0", 128, 160), "")
        ));
        let diff = diff(&before, &after);

        assert_eq!(
            diff.channels.len(),
            1,
            "only Lead changed: {:?}",
            diff.channels
        );
        assert_eq!(tags(card(&diff, "Lead")), vec![ChangeTag::Moved]);
    }

    #[test]
    fn a_moved_clip_should_report_bar_positions() {
        let before = tracks_xml(&track("1", "Reese", &clip("0", 0, 32), ""));
        let after = tracks_xml(&track("1", "Reese", &clip("0", 64, 96), ""));
        let card = diff(&before, &after).channels.remove(0);

        assert_eq!(card.clips_modified, 1);
        let row = &card.rows[0];
        assert_eq!(row.tag, ChangeTag::Moved);
        // Beat 64 at 4/4 is bar 17. Positions use the crate-wide
        // `bar.beat.subbeat` form, whose subbeat is 0-based.
        assert_eq!(row.before.as_deref(), Some("1.1.0"));
        assert_eq!(row.after.as_deref(), Some("17.1.0"));
        assert_eq!(
            row.target, "clip at 17.1.0",
            "unnamed clips say where they are"
        );
    }

    #[test]
    fn a_resized_clip_should_report_its_length() {
        let before = tracks_xml(&track("1", "Reese", &clip("0", 0, 32), ""));
        let after = tracks_xml(&track("1", "Reese", &clip("0", 0, 64), ""));
        let card = diff(&before, &after).channels.remove(0);

        assert_eq!(tags(&card), vec![ChangeTag::Length]);
        assert_eq!(card.rows[0].before.as_deref(), Some("32 beats"));
        assert_eq!(card.rows[0].after.as_deref(), Some("64 beats"));
    }

    #[test]
    fn devices_added_and_removed_should_be_named() {
        let before = tracks_xml(&track("1", "Reese", "", "<Eq8 />"));
        let after = tracks_xml(&track("1", "Reese", "", "<Eq8 /><Reverb />"));
        let forward = diff(&before, &after);

        let added = card(&forward, "Reese");
        assert_eq!(tags(added), vec![ChangeTag::FxAdded]);
        assert_eq!(added.rows[0].target, "Reverb");

        let reversed = diff(&after, &before);
        assert_eq!(tags(card(&reversed, "Reese")), vec![ChangeTag::FxRemoved]);
    }

    #[test]
    fn reordering_a_device_chain_should_be_reported() {
        // Chain order is signal order, so this changes the sound even though
        // nothing was added or removed.
        let before = tracks_xml(&track("1", "Reese", "", "<Eq8 /><Reverb />"));
        let after = tracks_xml(&track("1", "Reese", "", "<Reverb /><Eq8 />"));
        let card = diff(&before, &after).channels.remove(0);
        assert!(
            tags(&card).contains(&ChangeTag::FxReordered),
            "{:?}",
            tags(&card)
        );
    }

    #[test]
    fn a_hosted_plugin_should_be_named_by_its_plugin() {
        let plugin = r#"<PluginDevice><PluginDesc><Vst3PluginInfo>
            <Name Value="Serum 2" />
            <Uid><Fields.0 Value="1" /><Fields.1 Value="2" />
                 <Fields.2 Value="3" /><Fields.3 Value="4" /></Uid>
        </Vst3PluginInfo></PluginDesc></PluginDevice>"#;
        let before = tracks_xml(&track("1", "Reese", "", ""));
        let after = tracks_xml(&track("1", "Reese", "", plugin));
        let diff = diff(&before, &after);

        let card = card(&diff, "Reese");
        assert!(card.rows.iter().any(|row| row.target == "Serum 2"));
        assert_eq!(card.kind, ChannelKind::Plugin, "a track hosting a plugin");
    }

    #[test]
    fn mute_should_be_read_as_audibility_not_inverted() {
        // Live stores `Speaker/Manual = true` for a track that IS heard.
        // Reading it as a mute flag reports every change backwards.
        let audible = tracks_xml(&track("1", "Reese", "", ""));
        let muted = audible.replace(
            r#"<Speaker><Manual Value="true" /></Speaker>"#,
            r#"<Speaker><Manual Value="false" /></Speaker>"#,
        );
        assert_ne!(audible, muted, "the fixture really did change");

        let card = diff(&audible, &muted).channels.remove(0);
        let row = card
            .rows
            .iter()
            .find(|row| row.tag == ChangeTag::Muted)
            .expect("a mute row");
        assert_eq!(row.before.as_deref(), Some("unmuted"));
        assert_eq!(row.after.as_deref(), Some("muted"));
    }

    #[test]
    fn volume_and_pan_changes_should_be_reported_in_musical_units() {
        let before = tracks_xml(&track("1", "Reese", "", ""));
        let after = before
            .replace(
                r#"<Volume><Manual Value="1" /></Volume>"#,
                r#"<Volume><Manual Value="0.5" /></Volume>"#,
            )
            .replace(
                r#"<Pan><Manual Value="0" /></Pan>"#,
                r#"<Pan><Manual Value="-0.5" /></Pan>"#,
            );
        let card = diff(&before, &after).channels.remove(0);

        let volume = card
            .rows
            .iter()
            .find(|row| row.tag == ChangeTag::Volume)
            .expect("a volume row");
        assert!(
            volume.after.as_deref().is_some_and(|v| v.contains("dB")),
            "volume should read in dB, not as a linear gain: {volume:?}"
        );

        let pan = card
            .rows
            .iter()
            .find(|row| row.tag == ChangeTag::Pan)
            .expect("a pan row");
        assert!(pan.after.is_some());
    }

    #[test]
    fn project_level_changes_should_read_as_sentences() {
        let before = tracks_xml("");
        let after = tracks_xml("");
        let after_snapshot = snapshot(&after);
        let mut before_snapshot = snapshot(&before);

        // Retune the ancestor so the current side reads as a tempo change.
        let json = before_snapshot
            .to_string()
            .replace(r#""Value":"175""#, r#""Value":"172""#);
        before_snapshot = serde_json::from_str(&json).expect("reparse");

        let diff = structured_diff(&before_snapshot, &after_snapshot).expect("diff");
        assert!(
            diff.project_changes
                .iter()
                .any(|change| change == "Tempo: 172 → 175"),
            "{:?}",
            diff.project_changes
        );
        assert_eq!(diff.time_sig, (4, 4));
    }

    #[test]
    fn a_snapshot_without_a_live_set_should_not_be_diffed_here() {
        // The format-agnostic summary handles this; returning an empty diff
        // instead would silently claim the projects are identical.
        let bare = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "ableton-live-set",
            "project": {"root": {"tag": "Ableton"}}
        });
        assert!(structured_diff(&bare, &bare).is_none());
    }

    #[test]
    fn a_non_ableton_snapshot_should_not_be_diffed_here() {
        let dawproject = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "dawproject",
            "project": {"root": {"tag": "Project"}}
        });
        assert!(structured_diff(&dawproject, &dawproject).is_none());
    }

    #[test]
    fn a_clip_whose_id_was_reissued_should_read_as_edited_not_replaced() {
        // Live reassigns a clip's Id when its contents are edited. Two clips
        // in a real project did exactly this across one save, keeping their
        // positions and lengths while their ids went 11→13 and 10→12.
        let before = tracks_xml(&track(
            "1",
            "Reese",
            &format!("{}{}", clip("11", 256, 288), clip("10", 288, 320)),
            "",
        ));
        let after = tracks_xml(&track(
            "1",
            "Reese",
            &format!("{}{}", clip("13", 256, 288), clip("12", 288, 320)),
            "",
        ));
        let card = diff(&before, &after).channels.remove(0);

        assert_eq!(card.clips_modified, 2, "edited, not replaced");
        assert_eq!(card.clips_added, 0, "no phantom insertions");
        assert_eq!(card.clips_removed, 0, "no phantom deletions");
        assert!(
            card.rows.is_empty(),
            "nothing to say beyond the counts: {:?}",
            card.rows
        );
    }

    #[test]
    fn a_genuinely_new_clip_should_still_read_as_added() {
        // The pairing above must not swallow real insertions.
        let before = tracks_xml(&track("1", "Reese", &clip("0", 0, 32), ""));
        let after = tracks_xml(&track(
            "1",
            "Reese",
            &format!("{}{}", clip("0", 0, 32), clip("1", 64, 96)),
            "",
        ));
        let card = diff(&before, &after).channels.remove(0);

        assert_eq!(card.clips_added, 1);
        assert_eq!(card.clips_removed, 0);
        assert_eq!(tags(&card), vec![ChangeTag::Added]);
    }

    #[test]
    fn a_reissued_clip_that_also_moved_should_read_as_replaced() {
        // Only an exact position-and-length match is treated as the same
        // clip. Anything else is genuinely a different arrangement, and
        // guessing harder would start inventing history.
        let before = tracks_xml(&track("1", "Reese", &clip("11", 256, 288), ""));
        let after = tracks_xml(&track("1", "Reese", &clip("13", 320, 352), ""));
        let card = diff(&before, &after).channels.remove(0);

        assert_eq!(card.clips_added, 1);
        assert_eq!(card.clips_removed, 1);
    }

    #[test]
    fn a_track_with_many_changes_should_be_summarized_not_dumped() {
        // A person needs to see that a track changed a lot, not scroll a
        // hundred rows to find out.
        let many_before: String = (0..40)
            .map(|i| clip(&i.to_string(), i * 4, i * 4 + 4))
            .collect();
        let many_after: String = (0..40)
            .map(|i| clip(&i.to_string(), i * 8, i * 8 + 4))
            .collect();
        let before = tracks_xml(&track("1", "Reese", &many_before, ""));
        let after = tracks_xml(&track("1", "Reese", &many_after, ""));

        let card = diff(&before, &after).channels.remove(0);
        assert_eq!(card.rows.len(), MAX_ROWS_PER_CHANNEL + 1);
        assert!(
            card.rows
                .last()
                .expect("summary row")
                .target
                .contains("more change"),
            "the last row should say what was left out"
        );
        // The counts still reflect everything, not just what is shown.
        assert_eq!(card.clips_modified, 39, "clip 0 did not move");
    }
}
