use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::diff::{
    ChangeKind, ChangeRow, ChangeTag, ChannelDiff, ChannelKind, ProjectDiff, TimeSig, fmt_f64,
    fmt_pos_beats,
};
use crate::project_format::{XmlContent, XmlElement};

const MAX_ROWS_PER_TRACK: usize = 24;

pub(crate) fn structured_diff(ancestor: &Value, current: &Value) -> Option<ProjectDiff> {
    let before = super::snapshot_parts_from_value(ancestor)?;
    let after = super::snapshot_parts_from_value(current)?;
    let before_assets = super::assets::collect_from_value(&before.project.root, ancestor);
    let after_assets = super::assets::collect_from_value(&after.project.root, current);
    let before_meta = super::meta::extract_parts(
        &before.project.root,
        before.metadata.as_ref().map(|document| &document.root),
        &before_assets,
    );
    let after_meta = super::meta::extract_parts(
        &after.project.root,
        after.metadata.as_ref().map(|document| &document.root),
        &after_assets,
    );
    let time_sig = after_meta.time_signature.map_or((4, 4), |signature| {
        (signature.numerator, signature.denominator)
    });

    let mut project_changes = project_changes(&before_meta, &after_meta);
    resource_changes(&mut project_changes, ancestor, current);
    let mut channels = track_diffs(&before.project.root, &after.project.root, time_sig);
    if let Some(plugins) = plugin_diff(&before_meta.plugins, &after_meta.plugins) {
        channels.push(plugins);
    }
    // The structured rows are intentionally selective, but DAWproject permits
    // vendor extensions anywhere in the XML. Keep one umbrella row whenever
    // the tree changed so an extension edit cannot disappear merely because a
    // recognized tempo, track, or clip edit happened in the same version.
    if before.project.root != after.project.root {
        project_changes.push("DAWproject project XML changed".to_owned());
    }
    if before.metadata.as_ref().map(|document| &document.root)
        != after.metadata.as_ref().map(|document| &document.root)
        && !project_changes.iter().any(|change| {
            ["Title", "Artist", "Producer", "Genre"]
                .iter()
                .any(|label| change.starts_with(label))
        })
    {
        project_changes.push("DAWproject metadata changed".to_owned());
    }

    Some(ProjectDiff {
        project_changes,
        time_sig,
        channels,
    })
}

fn project_changes(
    before: &super::DawprojectMetadata,
    after: &super::DawprojectMetadata,
) -> Vec<String> {
    let mut changes = Vec::new();
    if before.format_version != after.format_version {
        match (&before.format_version, &after.format_version) {
            (Some(left), Some(right)) => {
                changes.push(format!("DAWproject format: {left} → {right}"));
            }
            (None, Some(right)) => changes.push(format!("DAWproject format: {right}")),
            (Some(left), None) => {
                changes.push(format!("DAWproject format removed (was {left})"));
            }
            _ => {}
        }
    }
    match (before.tempo, after.tempo) {
        (Some(left), Some(right)) if (left - right).abs() > f64::EPSILON => {
            changes.push(format!("Tempo: {} → {}", fmt_f64(left), fmt_f64(right)));
        }
        (None, Some(right)) => changes.push(format!("Tempo: {}", fmt_f64(right))),
        (Some(left), None) => changes.push(format!("Tempo removed (was {})", fmt_f64(left))),
        _ => {}
    }
    if before.time_signature != after.time_signature {
        match (before.time_signature, after.time_signature) {
            (Some(left), Some(right)) => {
                changes.push(format!("Time signature: {left} → {right}"));
            }
            (None, Some(right)) => changes.push(format!("Time signature: {right}")),
            (Some(left), None) => changes.push(format!("Time signature removed (was {left})")),
            _ => {}
        }
    }
    for (label, left, right) in [
        ("Title", &before.title, &after.title),
        ("Artist", &before.artist, &after.artist),
        ("Producer", &before.producer, &after.producer),
        ("Genre", &before.genre, &after.genre),
    ] {
        if left != right {
            match (left, right) {
                (Some(left), Some(right)) => {
                    changes.push(format!("{label}: {left} → {right}"));
                }
                (None, Some(right)) => changes.push(format!("{label}: {right}")),
                (Some(left), None) => changes.push(format!("{label} removed (was {left})")),
                _ => {}
            }
        }
    }
    if before.application_name != after.application_name
        || before.application_version != after.application_version
    {
        if let (Some(left), Some(right)) = (before.application_label(), after.application_label()) {
            changes.push(format!("Saved with: {left} → {right}"));
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
        "Markers",
        before.marker_count,
        after.marker_count,
    );
    changes
}

fn push_count_change(changes: &mut Vec<String>, label: &str, before: usize, after: usize) {
    if before != after {
        changes.push(format!("{label}: {before} → {after}"));
    }
}

fn resource_changes(changes: &mut Vec<String>, before: &Value, after: &Value) {
    let (before, after) = (resource_index(before), resource_index(after));
    for path in after.keys() {
        match before.get(path) {
            None => changes.push(format!("Embedded resource added: {path}")),
            Some(old) if old != after.get(path).expect("indexed path") => {
                changes.push(format!("Embedded resource changed: {path}"));
            }
            _ => {}
        }
    }
    for path in before.keys().filter(|path| !after.contains_key(*path)) {
        changes.push(format!("Embedded resource removed: {path}"));
    }
}

fn resource_index(snapshot: &Value) -> BTreeMap<&str, &str> {
    super::assets::resource_values(snapshot).collect()
}

#[derive(Clone)]
struct TrackView<'a> {
    element: &'a XmlElement,
    clips: Vec<&'a XmlElement>,
}

fn track_diffs(
    before_root: &XmlElement,
    after_root: &XmlElement,
    time_sig: (u32, u32),
) -> Vec<ChannelDiff> {
    let before = tracks(before_root);
    let after = tracks(after_root);
    let time_sig = TimeSig {
        numerator: time_sig.0.max(1),
        denominator: time_sig.1.max(1),
    };
    let mut matches = BTreeMap::new();
    let mut matched_before = BTreeSet::new();

    // Bitwig's exporter can renumber every later XML id when a track is
    // inserted. Match the identities that survive that renumbering first,
    // then use an unchanged compatible id for tracks that were simply renamed.
    for (after_id, after_track) in &after {
        let candidates = before
            .iter()
            .filter(|(before_id, before_track)| {
                !matched_before.contains(*before_id)
                    && high_confidence_track_match(before_track, after_track)
            })
            .map(|(before_id, _)| before_id)
            .collect::<Vec<_>>();
        if let [before_id] = candidates.as_slice() {
            matches.insert(after_id.clone(), (*before_id).clone());
            matched_before.insert((*before_id).clone());
        }
    }
    for (after_id, after_track) in &after {
        if matches.contains_key(after_id) {
            continue;
        }
        let Some(before_track) = before.get(after_id) else {
            continue;
        };
        if !matched_before.contains(after_id)
            && super::meta::track_kind(before_track.element)
                == super::meta::track_kind(after_track.element)
        {
            matches.insert(after_id.clone(), after_id.clone());
            matched_before.insert(after_id.clone());
        }
    }

    let mut cards = Vec::new();
    for (id, track) in &after {
        match matches.get(id).and_then(|before_id| before.get(before_id)) {
            Some(previous) => {
                if let Some(card) = modified_track(previous, track, time_sig) {
                    cards.push(card);
                }
            }
            None => cards.push(ChannelDiff {
                name: track_name(track.element),
                kind: channel_kind(track.element),
                status: ChangeKind::Add,
                clips_added: track.clips.len(),
                clips_removed: 0,
                clips_modified: 0,
                rows: Vec::new(),
            }),
        }
    }
    for (id, track) in &before {
        if !matched_before.contains(id) {
            cards.push(ChannelDiff {
                name: track_name(track.element),
                kind: channel_kind(track.element),
                status: ChangeKind::Remove,
                clips_added: 0,
                clips_removed: track.clips.len(),
                clips_modified: 0,
                rows: Vec::new(),
            });
        }
    }
    cards
}

fn high_confidence_track_match(before: &TrackView<'_>, after: &TrackView<'_>) -> bool {
    let kind = super::meta::track_kind(before.element);
    if kind != super::meta::track_kind(after.element) {
        return false;
    }
    if kind == super::DawprojectTrackKind::Master {
        return true;
    }
    let before_name = track_name(before.element);
    let after_name = track_name(after.element);
    if before_name != "Untitled track" && before_name == after_name {
        return true;
    }
    let before_devices = device_identity(before.element);
    !before_devices.is_empty() && before_devices == device_identity(after.element)
}

fn tracks(root: &XmlElement) -> BTreeMap<String, TrackView<'_>> {
    let Some(structure) = root.child("Structure") else {
        return BTreeMap::new();
    };
    structure
        .descendants()
        .filter(|element| element.tag == "Track")
        .enumerate()
        .map(|(index, track)| {
            let key = track.id.clone().unwrap_or_else(|| format!("@{index}"));
            let clips = track
                .id
                .as_deref()
                .map(|id| clips_for_track(root, id))
                .unwrap_or_default();
            (
                key,
                TrackView {
                    element: track,
                    clips,
                },
            )
        })
        .collect()
}

fn clips_for_track<'a>(root: &'a XmlElement, track_id: &str) -> Vec<&'a XmlElement> {
    let Some(arrangement) = root.child("Arrangement") else {
        return Vec::new();
    };
    arrangement
        .descendants()
        .filter(|lane| lane.tag == "Lanes" && lane.attribute("track") == Some(track_id))
        .flat_map(XmlElement::child_elements)
        .filter(|element| element.tag == "Clips")
        .flat_map(XmlElement::child_elements)
        .filter(|element| element.tag == "Clip")
        .collect()
}

fn modified_track(
    before: &TrackView<'_>,
    after: &TrackView<'_>,
    time_sig: TimeSig,
) -> Option<ChannelDiff> {
    let mut rows = Vec::new();
    let before_name = track_name(before.element);
    let after_name = track_name(after.element);
    if before_name != after_name {
        rows.push(ChangeRow {
            tag: ChangeTag::Renamed,
            kind: ChangeKind::Modify,
            target: after_name.clone(),
            before: Some(before_name),
            after: Some(after_name.clone()),
        });
    }
    mix_rows(&mut rows, before.element, after.element);
    device_rows(&mut rows, before.element, after.element);
    let (added, removed, modified) = clip_rows(&mut rows, &before.clips, &after.clips, time_sig);

    if rows.is_empty() && added == 0 && removed == 0 && modified == 0 {
        return None;
    }
    rows.truncate(MAX_ROWS_PER_TRACK);
    Some(ChannelDiff {
        name: after_name,
        kind: channel_kind(after.element),
        status: ChangeKind::Modify,
        clips_added: added,
        clips_removed: removed,
        clips_modified: modified,
        rows,
    })
}

fn mix_rows(rows: &mut Vec<ChangeRow>, before: &XmlElement, after: &XmlElement) {
    for (tag, element) in [
        (ChangeTag::Volume, "Volume"),
        (ChangeTag::Pan, "Pan"),
        (ChangeTag::Muted, "Mute"),
    ] {
        let before = channel_parameter(before, element);
        let after = channel_parameter(after, element);
        if before != after {
            rows.push(ChangeRow {
                tag,
                kind: ChangeKind::Modify,
                target: element.to_owned(),
                before: before.map(str::to_owned),
                after: after.map(str::to_owned),
            });
        }
    }
}

fn channel_parameter<'a>(track: &'a XmlElement, tag: &str) -> Option<&'a str> {
    track.child("Channel")?.child(tag)?.attribute("value")
}

fn device_rows(rows: &mut Vec<ChangeRow>, before: &XmlElement, after: &XmlElement) {
    let (before, after) = (devices(before), devices(after));
    for (id, device) in &after {
        match before.get(id) {
            None => rows.push(ChangeRow {
                tag: ChangeTag::FxAdded,
                kind: ChangeKind::Add,
                target: device_name(device),
                before: None,
                after: None,
            }),
            Some(previous) if !elements_equal_ignoring_generated_ids(previous, device) => rows
                .push(ChangeRow {
                    tag: ChangeTag::PluginPreset,
                    kind: ChangeKind::Modify,
                    target: device_name(device),
                    before: None,
                    after: Some("device state changed".to_owned()),
                }),
            _ => {}
        }
    }
    for (id, device) in before {
        if !after.contains_key(&id) {
            rows.push(ChangeRow {
                tag: ChangeTag::FxRemoved,
                kind: ChangeKind::Remove,
                target: device_name(device),
                before: None,
                after: None,
            });
        }
    }
}

fn devices(track: &XmlElement) -> BTreeMap<String, &XmlElement> {
    let mut occurrences = BTreeMap::<String, usize>::new();
    track
        .resolve("Channel/Devices")
        .into_iter()
        .flat_map(XmlElement::child_elements)
        .map(|device| {
            let identity = device
                .attribute("deviceID")
                .or_else(|| device.attribute("name"))
                .filter(|value| !value.is_empty())
                .unwrap_or(&device.tag);
            let base = format!("{}:{identity}", device.tag);
            let occurrence = occurrences.entry(base.clone()).or_default();
            let key = format!("{base}#{occurrence}");
            *occurrence += 1;
            (key, device)
        })
        .collect()
}

fn device_identity(track: &XmlElement) -> Vec<String> {
    devices(track).into_keys().collect()
}

fn clip_rows(
    rows: &mut Vec<ChangeRow>,
    before: &[&XmlElement],
    after: &[&XmlElement],
    time_sig: TimeSig,
) -> (usize, usize, usize) {
    let (before, after) = (clip_index(before), clip_index(after));
    let mut added = 0;
    let mut removed = 0;
    let mut modified = 0;

    for (id, clip) in &after {
        let Some(previous) = before.get(id) else {
            added += 1;
            rows.push(ChangeRow {
                tag: ChangeTag::Added,
                kind: ChangeKind::Add,
                target: clip_name(clip),
                before: None,
                after: clip
                    .attribute("time")
                    .and_then(|value| value.parse().ok())
                    .map(|beat| fmt_pos_beats(beat, time_sig)),
            });
            continue;
        };
        if elements_equal_ignoring_generated_ids(previous, clip) {
            continue;
        }
        modified += 1;
        let previous_name = clip_name(previous);
        let name = clip_name(clip);
        if previous_name != name {
            rows.push(ChangeRow {
                tag: ChangeTag::Renamed,
                kind: ChangeKind::Modify,
                target: name.clone(),
                before: Some(previous_name),
                after: Some(name.clone()),
            });
        }
        attribute_row(
            rows,
            ChangeTag::Moved,
            &name,
            previous,
            clip,
            "time",
            |value| {
                value
                    .parse()
                    .ok()
                    .map(|beat| fmt_pos_beats(beat, time_sig))
                    .unwrap_or_else(|| value.to_owned())
            },
        );
        attribute_row(
            rows,
            ChangeTag::Length,
            &name,
            previous,
            clip,
            "duration",
            |value| format!("{value} beats"),
        );
        if !contents_equal_ignoring_generated_ids(&previous.children, &clip.children) {
            rows.push(ChangeRow {
                tag: ChangeTag::Content,
                kind: ChangeKind::Modify,
                target: name,
                before: None,
                after: Some("notes, audio, or automation changed".to_owned()),
            });
        }
    }
    for (id, clip) in before {
        if !after.contains_key(&id) {
            removed += 1;
            rows.push(ChangeRow {
                tag: ChangeTag::Removed,
                kind: ChangeKind::Remove,
                target: clip_name(clip),
                before: None,
                after: None,
            });
        }
    }
    (added, removed, modified)
}

fn elements_equal_ignoring_generated_ids(left: &XmlElement, right: &XmlElement) -> bool {
    left.tag == right.tag
        && left
            .attributes
            .iter()
            .filter(|(name, _)| name.as_str() != "id")
            .eq(right
                .attributes
                .iter()
                .filter(|(name, _)| name.as_str() != "id"))
        && contents_equal_ignoring_generated_ids(&left.children, &right.children)
}

fn contents_equal_ignoring_generated_ids(left: &[XmlContent], right: &[XmlContent]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| match (left, right) {
                (XmlContent::Element(left), XmlContent::Element(right)) => {
                    elements_equal_ignoring_generated_ids(left, right)
                }
                (XmlContent::Text { text: left }, XmlContent::Text { text: right }) => {
                    left == right
                }
                (XmlContent::Cdata { cdata: left }, XmlContent::Cdata { cdata: right }) => {
                    left == right
                }
                (XmlContent::Comment { comment: left }, XmlContent::Comment { comment: right }) => {
                    left == right
                }
                _ => false,
            })
}

fn clip_index<'a>(clips: &[&'a XmlElement]) -> BTreeMap<String, &'a XmlElement> {
    clips
        .iter()
        .enumerate()
        .map(|(index, clip)| {
            let key = clip
                .attribute("reference")
                .map(str::to_owned)
                .or_else(|| clip.id.clone())
                .unwrap_or_else(|| format!("@{index}"));
            (key, *clip)
        })
        .collect()
}

fn attribute_row(
    rows: &mut Vec<ChangeRow>,
    tag: ChangeTag,
    target: &str,
    before: &XmlElement,
    after: &XmlElement,
    attribute: &str,
    format: impl Fn(&str) -> String,
) {
    let (before, after) = (before.attribute(attribute), after.attribute(attribute));
    if before != after {
        rows.push(ChangeRow {
            tag,
            kind: ChangeKind::Modify,
            target: target.to_owned(),
            before: before.map(&format),
            after: after.map(format),
        });
    }
}

fn plugin_diff(before: &[crate::PluginRef], after: &[crate::PluginRef]) -> Option<ChannelDiff> {
    let (before, after) = (plugin_index(before), plugin_index(after));
    let mut rows = Vec::new();
    for (id, plugin) in &after {
        match before.get(id) {
            None => rows.push(ChangeRow {
                tag: ChangeTag::FxAdded,
                kind: ChangeKind::Add,
                target: plugin.name.clone(),
                before: None,
                after: Some(instances(plugin.instances)),
            }),
            Some(previous) if previous.instances != plugin.instances => rows.push(ChangeRow {
                tag: ChangeTag::InstrumentChanged,
                kind: ChangeKind::Modify,
                target: plugin.name.clone(),
                before: Some(instances(previous.instances)),
                after: Some(instances(plugin.instances)),
            }),
            _ => {}
        }
    }
    for (id, plugin) in before {
        if !after.contains_key(&id) {
            rows.push(ChangeRow {
                tag: ChangeTag::FxRemoved,
                kind: ChangeKind::Remove,
                target: plugin.name.clone(),
                before: Some(instances(plugin.instances)),
                after: None,
            });
        }
    }
    (!rows.is_empty()).then(|| ChannelDiff {
        name: "Plugins".to_owned(),
        kind: ChannelKind::Plugin,
        status: ChangeKind::Modify,
        clips_added: 0,
        clips_removed: 0,
        clips_modified: 0,
        rows,
    })
}

fn plugin_index(plugins: &[crate::PluginRef]) -> BTreeMap<String, &crate::PluginRef> {
    plugins
        .iter()
        .map(|plugin| (plugin.id.to_string(), plugin))
        .collect()
}

fn instances(count: usize) -> String {
    if count == 1 {
        "1 instance".to_owned()
    } else {
        format!("{count} instances")
    }
}

fn track_name(track: &XmlElement) -> String {
    track
        .attribute("name")
        .filter(|name| !name.is_empty())
        .unwrap_or("Untitled track")
        .to_owned()
}

fn channel_kind(track: &XmlElement) -> ChannelKind {
    match super::meta::track_kind(track) {
        super::DawprojectTrackKind::Notes => ChannelKind::Midi,
        super::DawprojectTrackKind::Audio => ChannelKind::Audio,
        super::DawprojectTrackKind::Hybrid => ChannelKind::Other,
        _ => ChannelKind::Other,
    }
}

fn clip_name(clip: &XmlElement) -> String {
    clip.attribute("name")
        .filter(|name| !name.is_empty())
        .unwrap_or("Untitled clip")
        .to_owned()
}

fn device_name(device: &XmlElement) -> String {
    device
        .attribute("deviceName")
        .or_else(|| device.attribute("name"))
        .filter(|name| !name.is_empty())
        .unwrap_or(&device.tag)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{ChangeTag, ChannelKind};
    use crate::project_format::{ArchiveResource, PortableSnapshot, ProjectFormat, XmlDocument};

    fn snapshot(xml: &str) -> Value {
        snapshot_with_resources(xml, Vec::new())
    }

    fn snapshot_with_resources(xml: &str, resources: Vec<ArchiveResource>) -> Value {
        serde_json::to_value(PortableSnapshot {
            auru_pm_snapshot: 1,
            format: ProjectFormat::Dawproject,
            project: XmlDocument::parse(xml.as_bytes(), "test project").expect("XML"),
            metadata: None,
            resources,
        })
        .expect("snapshot")
    }

    #[test]
    fn dawproject_diff_should_report_musical_and_per_track_changes() {
        let before = snapshot(
            r#"<Project version="1.0">
                <Application name="Bitwig Studio" version="5.1"/>
                <Transport>
                  <Tempo unit="bpm" value="120" id="tempo"/>
                  <TimeSignature numerator="4" denominator="4" id="signature"/>
                </Transport>
                <Structure>
                  <Track contentType="notes" id="lead" name="Lead">
                    <Channel role="regular" id="lead-channel">
                      <Volume unit="linear" value="0.5" id="volume"/>
                    </Channel>
                  </Track>
                </Structure>
                <Arrangement id="arrangement"><Lanes timeUnit="beats" id="lanes">
                  <Lanes track="lead" id="lead-lane"><Clips id="lead-clips">
                    <Clip time="4" duration="4" name="Verse"><Notes/></Clip>
                  </Clips></Lanes>
                </Lanes></Arrangement>
              </Project>"#,
        );
        let after = snapshot(
            r#"<Project version="1.0">
                <Application name="Bitwig Studio" version="5.2"/>
                <Transport>
                  <Tempo unit="bpm" value="128" id="tempo"/>
                  <TimeSignature numerator="3" denominator="4" id="signature"/>
                </Transport>
                <Structure>
                  <Track contentType="notes" id="lead" name="Main Lead">
                    <Channel role="regular" id="lead-channel">
                      <Volume unit="linear" value="0.75" id="volume"/>
                    </Channel>
                  </Track>
                  <Track contentType="audio" id="drums" name="Drums">
                    <Channel role="regular" id="drum-channel"/>
                  </Track>
                </Structure>
                <Arrangement id="arrangement"><Lanes timeUnit="beats" id="lanes">
                  <Lanes track="lead" id="lead-lane"><Clips id="lead-clips">
                    <Clip time="8" duration="6" name="Verse"><Notes>
                      <Note time="0" duration="1" channel="0" key="60" vel="1"/>
                    </Notes></Clip>
                    <Clip time="16" duration="2" name="Outro"/>
                  </Clips></Lanes>
                </Lanes></Arrangement>
              </Project>"#,
        );

        let diff = structured_diff(&before, &after).expect("structured diff");

        assert_eq!(diff.time_sig, (3, 4));
        assert!(
            diff.project_changes
                .iter()
                .any(|change| change == "Tempo: 120 → 128")
        );
        assert!(
            diff.project_changes
                .iter()
                .any(|change| change == "Time signature: 4/4 → 3/4")
        );
        let lead = diff
            .channels
            .iter()
            .find(|channel| channel.name == "Main Lead")
            .expect("lead track");
        assert_eq!(lead.kind, ChannelKind::Midi);
        assert_eq!(lead.clips_added, 1);
        assert_eq!(lead.clips_modified, 1);
        assert!(lead.rows.iter().any(|row| row.tag == ChangeTag::Renamed));
        assert!(lead.rows.iter().any(|row| row.tag == ChangeTag::Moved));
        assert!(lead.rows.iter().any(|row| row.tag == ChangeTag::Length));
        assert!(lead.rows.iter().any(|row| row.tag == ChangeTag::Volume));
        assert!(lead.rows.iter().any(|row| row.tag == ChangeTag::Content));

        let drums = diff
            .channels
            .iter()
            .find(|channel| channel.name == "Drums")
            .expect("added drums");
        assert_eq!(drums.kind, ChannelKind::Audio);
        assert_eq!(drums.status, crate::diff::ChangeKind::Add);
    }

    #[test]
    fn dawproject_diff_should_keep_resource_changes_and_unknown_xml_visible() {
        let before = snapshot_with_resources(
            r#"<Project version="1.0">
                <Application name="Test" version="1"/>
                <Transport><Tempo unit="bpm" value="120"/></Transport>
                <Scenes/>
              </Project>"#,
            vec![ArchiveResource {
                id: "audio/take.wav".to_owned(),
                data: "b2xk".to_owned(),
            }],
        );
        let after = snapshot_with_resources(
            r#"<Project version="1.0">
                <Application name="Test" version="1"/>
                <Transport><Tempo unit="bpm" value="128"/></Transport>
                <Scenes custom="new"/>
              </Project>"#,
            vec![ArchiveResource {
                id: "audio/take.wav".to_owned(),
                data: "bmV3".to_owned(),
            }],
        );

        let diff = structured_diff(&before, &after).expect("structured diff");

        assert!(
            diff.project_changes
                .iter()
                .any(|change| change == "Embedded resource changed: audio/take.wav")
        );
        assert!(
            diff.project_changes
                .iter()
                .any(|change| change == "Tempo: 120 → 128")
        );
        assert!(
            diff.project_changes
                .iter()
                .any(|change| change == "DAWproject project XML changed"),
            "an extension field must not disappear just because the known metadata stayed equal"
        );
    }

    #[test]
    fn bitwig_id_reissue_should_not_turn_shifted_tracks_into_edits() {
        let before = snapshot(
            r#"<Project version="1.0">
                <Application name="Bitwig Studio" version="5.3.13"/>
                <Structure>
                  <Track contentType="notes" id="id2" name="Synth">
                    <Channel role="regular"/>
                  </Track>
                  <Track contentType="audio" id="id23" name="FX 1">
                    <Channel role="effect"/>
                  </Track>
                  <Track contentType="audio notes" id="id28" name="Master">
                    <Channel role="master"/>
                  </Track>
                </Structure>
                <Arrangement><Lanes timeUnit="beats">
                  <Lanes track="id2"><Clips>
                    <Clip time="0" duration="4"><Notes id="id49">
                      <Note time="0" duration="1" channel="0" key="60" vel="1"/>
                    </Notes></Clip>
                  </Clips></Lanes>
                </Lanes></Arrangement>
              </Project>"#,
        );
        let after = snapshot(
            r#"<Project version="1.0">
                <Application name="Bitwig Studio" version="5.3.13"/>
                <Structure>
                  <Track contentType="notes" id="id2" name="Synth">
                    <Channel role="regular"/>
                  </Track>
                  <Track contentType="audio" id="id23" name="GrannyBass Loop 3">
                    <Channel role="regular"/>
                  </Track>
                  <Track contentType="audio" id="id28" name="FX 1">
                    <Channel role="effect"/>
                  </Track>
                  <Track contentType="audio notes" id="id33" name="Master">
                    <Channel role="master"/>
                  </Track>
                </Structure>
                <Arrangement><Lanes timeUnit="beats">
                  <Lanes track="id2"><Clips>
                    <Clip time="0" duration="4"><Notes id="id57">
                      <Note time="0" duration="1" channel="0" key="60" vel="1"/>
                    </Notes></Clip>
                  </Clips></Lanes>
                </Lanes></Arrangement>
              </Project>"#,
        );

        let diff = structured_diff(&before, &after).expect("structured diff");

        assert_eq!(diff.channels.len(), 1);
        assert_eq!(diff.channels[0].name, "GrannyBass Loop 3");
        assert_eq!(diff.channels[0].status, ChangeKind::Add);
    }

    #[test]
    fn an_unchanged_dawproject_should_have_an_empty_structured_diff() {
        let snapshot =
            snapshot(r#"<Project version="1.0"><Application name="Test" version="1"/></Project>"#);
        assert!(
            structured_diff(&snapshot, &snapshot)
                .expect("structured diff")
                .is_empty()
        );
    }
}
