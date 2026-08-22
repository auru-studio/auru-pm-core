//! Comparing two versions of an FL Studio project.
//!
//! Mapped onto the vocabulary the UI already speaks — [`ProjectDiff`],
//! [`ChannelDiff`], [`ChangeRow`] — rather than inventing a parallel one, so
//! a version history reads the same whichever DAW made the project.
//!
//! What FL offers to compare is not what Ableton offers. There are no tracks,
//! so a "channel" here is a **channel-rack channel**; there are no clips on a
//! timeline to diff, so pattern and mixer changes carry the detail instead.
//! Deliberately *not* attempted: note-level pattern data and plugin state.
//! A single project sampled during design held 18 MB of Serum parameters, and
//! a diff that reported thousands of changed floats would bury the one thing
//! the person actually did.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::diff::{ChangeKind, ChangeRow, ChangeTag, ChannelDiff, ChannelKind, ProjectDiff};

use super::events::Stream;
use super::meta::{self, FlStudioMetadata};
use super::tree;

/// Cap on rows shown for one channel.
///
/// A diff is read by a person deciding whether to keep a version; past a
/// couple of dozen lines it stops being read at all.
const MAX_ROWS_PER_CHANNEL: usize = 24;

/// Compare two FL Studio snapshots.
///
/// `None` when either side is not an FL project we can read, so the caller
/// falls back to the format-agnostic summary rather than showing an empty
/// comparison — which would claim the two versions are identical.
pub(crate) fn structured_diff(ancestor: &Value, current: &Value) -> Option<ProjectDiff> {
    let before = stream_from_value(ancestor)?;
    let after = stream_from_value(current)?;

    let before_meta = meta::extract(&before);
    let after_meta = meta::extract(&after);
    let time_sig = after_meta.time_signature.unwrap_or((4, 4));

    Some(ProjectDiff {
        project_changes: project_changes(&before_meta, &after_meta),
        time_sig,
        channels: channel_diffs(&before_meta, &after_meta),
    })
}

fn stream_from_value(snapshot: &Value) -> Option<Stream> {
    let format: crate::project_format::ProjectFormat =
        serde_json::from_value(snapshot.get("format")?.clone()).ok()?;
    if format != crate::project_format::ProjectFormat::FlStudio {
        return None;
    }
    let document = serde_json::from_value(snapshot.get("project")?.clone()).ok()?;
    tree::from_document(&document).ok()
}

fn project_changes(before: &FlStudioMetadata, after: &FlStudioMetadata) -> Vec<String> {
    let mut changes = Vec::new();

    match (before.tempo, after.tempo) {
        (Some(a), Some(b)) if (a - b).abs() > f64::EPSILON => {
            changes.push(format!("Tempo: {} → {}", tempo(a), tempo(b)));
        }
        (None, Some(b)) => changes.push(format!("Tempo: {}", tempo(b))),
        _ => {}
    }

    if before.time_signature != after.time_signature
        && let (Some((an, ad)), Some((bn, bd))) = (before.time_signature, after.time_signature)
    {
        changes.push(format!("Time signature: {an}/{ad} → {bn}/{bd}"));
    }

    for (label, before, after) in [
        ("Title", &before.title, &after.title),
        ("Author", &before.author, &after.author),
        ("Genre", &before.genre, &after.genre),
    ] {
        if before != after {
            match (before, after) {
                (Some(a), Some(b)) => changes.push(format!("{label}: {a} → {b}")),
                (None, Some(b)) => changes.push(format!("{label}: {b}")),
                (Some(a), None) => changes.push(format!("{label} removed (was {a})")),
                (None, None) => {}
            }
        }
    }

    if before.channels != after.channels {
        changes.push(format!(
            "Channels: {} → {}",
            before.channels, after.channels
        ));
    }

    // Arrangement markers are how a person navigates their own song, so a
    // section appearing or disappearing is a headline change, not a detail.
    describe_set_change(&mut changes, "Section", &before.markers, &after.markers);

    if before.version != after.version
        && let (Some(a), Some(b)) = (&before.version, &after.version)
    {
        changes.push(format!("Saved by FL {a} → {b}"));
    }

    changes
}

/// Note names added and removed between two ordered lists.
fn describe_set_change(
    changes: &mut Vec<String>,
    label: &str,
    before: &[String],
    after: &[String],
) {
    let before_set: std::collections::BTreeSet<&String> = before.iter().collect();
    let after_set: std::collections::BTreeSet<&String> = after.iter().collect();

    for added in after_set.difference(&before_set) {
        changes.push(format!("{label} added: {added}"));
    }
    for removed in before_set.difference(&after_set) {
        changes.push(format!("{label} removed: {removed}"));
    }
}

/// One card per mixer insert that changed, plus one for the plugin inventory.
///
/// FL's channel rack has no per-channel history we can read without decoding
/// pattern data, so what is offered instead is the two things a person
/// actually changes between saves: the mixer, and what is loaded.
fn channel_diffs(before: &FlStudioMetadata, after: &FlStudioMetadata) -> Vec<ChannelDiff> {
    let mut channels = Vec::new();

    if let Some(mixer) = mixer_diff(before, after) {
        channels.push(mixer);
    }
    if let Some(plugins) = plugin_diff(before, after) {
        channels.push(plugins);
    }
    if let Some(patterns) = pattern_diff(before, after) {
        channels.push(patterns);
    }
    channels
}

fn mixer_diff(before: &FlStudioMetadata, after: &FlStudioMetadata) -> Option<ChannelDiff> {
    let rows = named_rows(&before.insert_names, &after.insert_names);
    (!rows.is_empty()).then(|| ChannelDiff {
        name: "Mixer".to_owned(),
        kind: ChannelKind::Audio,
        status: ChangeKind::Modify,
        clips_added: 0,
        clips_removed: 0,
        clips_modified: 0,
        rows,
    })
}

fn pattern_diff(before: &FlStudioMetadata, after: &FlStudioMetadata) -> Option<ChannelDiff> {
    let rows = named_rows(&before.pattern_names, &after.pattern_names);
    if rows.is_empty() {
        return None;
    }
    let added = rows
        .iter()
        .filter(|row| row.kind == ChangeKind::Add)
        .count();
    let removed = rows.len() - added;
    Some(ChannelDiff {
        name: "Patterns".to_owned(),
        kind: ChannelKind::Midi,
        status: ChangeKind::Modify,
        clips_added: added,
        clips_removed: removed,
        clips_modified: 0,
        rows,
    })
}

fn plugin_diff(before: &FlStudioMetadata, after: &FlStudioMetadata) -> Option<ChannelDiff> {
    let index = |meta: &FlStudioMetadata| -> BTreeMap<String, (String, usize)> {
        meta.plugins
            .iter()
            .map(|plugin| {
                (
                    plugin.id.to_string(),
                    (plugin.name.clone(), plugin.instances),
                )
            })
            .collect()
    };
    let (before, after) = (index(before), index(after));

    let mut rows = Vec::new();
    for (id, (name, count)) in &after {
        match before.get(id) {
            None => rows.push(ChangeRow {
                tag: ChangeTag::FxAdded,
                kind: ChangeKind::Add,
                target: name.clone(),
                before: None,
                after: Some(instances(*count)),
            }),
            // A plugin used more or less often is a real change — someone
            // added a Serum to four more channels — and cheap to say.
            Some((_, was)) if was != count => rows.push(ChangeRow {
                tag: ChangeTag::InstrumentChanged,
                kind: ChangeKind::Modify,
                target: name.clone(),
                before: Some(instances(*was)),
                after: Some(instances(*count)),
            }),
            Some(_) => {}
        }
    }
    for (id, (name, count)) in &before {
        if !after.contains_key(id) {
            rows.push(ChangeRow {
                tag: ChangeTag::FxRemoved,
                kind: ChangeKind::Remove,
                target: name.clone(),
                before: Some(instances(*count)),
                after: None,
            });
        }
    }

    if rows.is_empty() {
        return None;
    }
    rows.truncate(MAX_ROWS_PER_CHANNEL);
    Some(ChannelDiff {
        name: "Plugins".to_owned(),
        kind: ChannelKind::Plugin,
        status: ChangeKind::Modify,
        clips_added: 0,
        clips_removed: 0,
        clips_modified: 0,
        rows,
    })
}

/// Rows for names that appeared or disappeared from an ordered list.
///
/// Compared as a multiset rather than positionally: FL writes these in file
/// order, and one insertion near the top would otherwise report every name
/// after it as renamed.
fn named_rows(before: &[String], after: &[String]) -> Vec<ChangeRow> {
    let mut counts: BTreeMap<&str, i64> = BTreeMap::new();
    for name in before {
        *counts.entry(name.as_str()).or_default() -= 1;
    }
    for name in after {
        *counts.entry(name.as_str()).or_default() += 1;
    }

    let mut rows = Vec::new();
    for (name, delta) in counts {
        if delta > 0 {
            rows.push(ChangeRow {
                tag: ChangeTag::Added,
                kind: ChangeKind::Add,
                target: name.to_owned(),
                before: None,
                after: None,
            });
        } else if delta < 0 {
            rows.push(ChangeRow {
                tag: ChangeTag::Removed,
                kind: ChangeKind::Remove,
                target: name.to_owned(),
                before: None,
                after: None,
            });
        }
    }
    rows.truncate(MAX_ROWS_PER_CHANNEL);
    rows
}

fn instances(count: usize) -> String {
    if count == 1 {
        "1 instance".to_owned()
    } else {
        format!("{count} instances")
    }
}

fn tempo(value: f64) -> String {
    if value.fract().abs() < f64::EPSILON {
        format!("{value:.0}")
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flstudio::events::{Event, Header};
    use crate::{ProjectFormat, ProjectSnapshot};

    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    /// A snapshot value, as a commit would store it.
    fn snapshot(events: Vec<Event>) -> Value {
        let mut all = vec![Event::new(199, b"20.5.0.1142\0".to_vec())];
        all.extend(events);
        let bytes = Stream {
            header: Header {
                format: 0,
                channels: 2,
                ppq: 96,
            },
            events: all,
        }
        .encode();

        let snapshot =
            ProjectSnapshot::from_source_bytes(ProjectFormat::FlStudio, &bytes).expect("snapshot");
        serde_json::from_slice(snapshot.as_bytes()).expect("value")
    }

    #[test]
    fn a_tempo_change_should_be_reported_at_project_level() {
        let before = snapshot(vec![Event::new(156, 92_000u32.to_le_bytes())]);
        let after = snapshot(vec![Event::new(156, 174_000u32.to_le_bytes())]);

        let diff = structured_diff(&before, &after).expect("a diff");
        assert_eq!(diff.project_changes, ["Tempo: 92 → 174"]);
    }

    #[test]
    fn an_unchanged_project_should_report_nothing() {
        // An empty diff must mean "nothing changed", so it has to be reachable
        // — otherwise every version looks edited.
        let same = snapshot(vec![Event::new(156, 92_000u32.to_le_bytes())]);
        let diff = structured_diff(&same, &same).expect("a diff");
        assert!(diff.project_changes.is_empty());
        assert!(diff.channels.is_empty());
    }

    #[test]
    fn a_new_arrangement_section_should_be_called_out() {
        // How someone navigates their own song; adding a drop is a headline
        // change rather than a detail.
        let before = snapshot(vec![Event::new(205, utf16("Intro"))]);
        let after = snapshot(vec![
            Event::new(205, utf16("Intro")),
            Event::new(205, utf16("Drop 1")),
        ]);

        let diff = structured_diff(&before, &after).expect("a diff");
        assert!(
            diff.project_changes
                .contains(&"Section added: Drop 1".to_owned()),
            "{:?}",
            diff.project_changes
        );
    }

    #[test]
    fn a_renamed_mixer_insert_should_show_as_one_add_and_one_remove() {
        let before = snapshot(vec![Event::new(204, utf16("Bass"))]);
        let after = snapshot(vec![Event::new(204, utf16("Sub Bass"))]);

        let diff = structured_diff(&before, &after).expect("a diff");
        let mixer = diff
            .channels
            .iter()
            .find(|channel| channel.name == "Mixer")
            .expect("a mixer card");
        assert_eq!(mixer.rows.len(), 2);
        assert!(mixer.rows.iter().any(|row| row.target == "Sub Bass"));
        assert!(mixer.rows.iter().any(|row| row.target == "Bass"));
    }

    #[test]
    fn inserting_a_name_near_the_top_should_not_report_everything_after_it() {
        // The failure a positional comparison would produce: one added pattern
        // reported as every pattern after it having been renamed.
        let before = snapshot(vec![
            Event::new(193, utf16("Drums")),
            Event::new(193, utf16("Bass")),
            Event::new(193, utf16("Melody")),
        ]);
        let after = snapshot(vec![
            Event::new(193, utf16("Intro")),
            Event::new(193, utf16("Drums")),
            Event::new(193, utf16("Bass")),
            Event::new(193, utf16("Melody")),
        ]);

        let diff = structured_diff(&before, &after).expect("a diff");
        let patterns = diff
            .channels
            .iter()
            .find(|channel| channel.name == "Patterns")
            .expect("a patterns card");
        assert_eq!(patterns.rows.len(), 1, "{:?}", patterns.rows);
        assert_eq!(patterns.rows[0].target, "Intro");
        assert_eq!(patterns.clips_added, 1);
    }

    #[test]
    fn a_plugin_used_on_more_channels_should_be_reported() {
        let plugin = |name: &str| vec![Event::new(201, utf16(name)), Event::new(212, vec![0; 52])];
        let before = snapshot(plugin("Maximus"));
        let mut twice = plugin("Maximus");
        twice.extend(plugin("Maximus"));
        let after = snapshot(twice);

        let diff = structured_diff(&before, &after).expect("a diff");
        let plugins = diff
            .channels
            .iter()
            .find(|channel| channel.name == "Plugins")
            .expect("a plugins card");
        assert_eq!(plugins.rows[0].target, "Maximus");
        assert_eq!(plugins.rows[0].before.as_deref(), Some("1 instance"));
        assert_eq!(plugins.rows[0].after.as_deref(), Some("2 instances"));
    }

    #[test]
    fn a_newly_added_plugin_should_be_reported_as_added() {
        let before = snapshot(vec![]);
        let after = snapshot(vec![
            Event::new(201, utf16("Fruity Limiter")),
            Event::new(212, vec![0; 52]),
        ]);

        let diff = structured_diff(&before, &after).expect("a diff");
        let plugins = diff
            .channels
            .iter()
            .find(|channel| channel.name == "Plugins")
            .expect("a plugins card");
        assert_eq!(plugins.rows[0].kind, ChangeKind::Add);
        assert_eq!(plugins.rows[0].target, "Fruity Limiter");
    }

    #[test]
    fn a_snapshot_that_is_not_an_fl_project_should_fall_through() {
        // Returning an empty diff instead would claim two different Live Sets
        // were identical.
        let value = serde_json::json!({ "format": "ableton-live-set", "project": {} });
        assert!(structured_diff(&value, &value).is_none());
    }

    #[test]
    fn the_time_signature_should_come_from_the_newer_version() {
        let before = snapshot(vec![Event::new(17, [4]), Event::new(18, [4])]);
        let after = snapshot(vec![Event::new(17, [3]), Event::new(18, [4])]);

        let diff = structured_diff(&before, &after).expect("a diff");
        assert_eq!(diff.time_sig, (3, 4));
        assert!(
            diff.project_changes
                .contains(&"Time signature: 4/4 → 3/4".to_owned()),
            "{:?}",
            diff.project_changes
        );
    }
}
