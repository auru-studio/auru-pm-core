use serde_json::Value;
use std::collections::HashMap;

pub type DiffSummary = Vec<String>;

/// Compute a human-readable diff summary between two canonical project
/// snapshots.
///
/// Native Auru snapshots receive the detailed track/clip treatment below.
/// External DAW snapshots use format-aware XML/resource summaries.
pub fn summarize_diff(ancestor: &Value, current: &Value) -> DiffSummary {
    if is_external_snapshot(ancestor) || is_external_snapshot(current) {
        return summarize_external_diff(ancestor, current, true);
    }

    let mut items = Vec::new();

    // BPM
    let a_bpm = ancestor.get("bpm").and_then(Value::as_f64);
    let b_bpm = current.get("bpm").and_then(Value::as_f64);
    if a_bpm != b_bpm {
        match (a_bpm, b_bpm) {
            (Some(a), Some(b)) => items.push(format!("Tempo: {} → {}", fmt_f64(a), fmt_f64(b))),
            _ => items.push("Tempo changed".to_owned()),
        }
    }

    // Time signature
    let a_num = ancestor
        .get("time_sig_numerator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    let a_den = ancestor
        .get("time_sig_denominator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    let b_num = current
        .get("time_sig_numerator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    let b_den = current
        .get("time_sig_denominator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    if a_num != b_num || a_den != b_den {
        items.push(format!("Time signature: {a_num}/{a_den} → {b_num}/{b_den}"));
    }
    // Clip position formatting uses the *current* time signature — that's
    // what the user sees on the timeline when reading the diff.
    let time_sig = TimeSig {
        numerator: b_num.max(1) as u32,
        denominator: b_den.max(1) as u32,
    };

    // Project scale
    if ancestor.get("project_scale") != current.get("project_scale") {
        let label = |v: &Value| {
            v.get("project_scale")
                .and_then(|s| s.get("label").or_else(|| s.as_str().map(|_| s)))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned()
        };
        items.push(format!(
            "Key/Scale: {} → {}",
            label(ancestor),
            label(current)
        ));
    }

    // Channels
    diff_channels(
        &mut items,
        ancestor.get("channels"),
        current.get("channels"),
        time_sig,
    );

    // Automation
    if ancestor.get("automation") != current.get("automation") {
        items.push("Automation changed".to_owned());
    }

    // Markers
    diff_array_count(
        &mut items,
        ancestor.get("markers"),
        current.get("markers"),
        "marker",
    );

    // Notes
    if ancestor.get("notes") != current.get("notes") {
        items.push("Project notes updated".to_owned());
    }

    if items.is_empty() {
        items.push("No structural changes".to_owned());
    }

    items
}

fn fmt_f64(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        format!("{:.2}", v)
    }
}

/// Time signature snapshot used by position formatting in the clip diff.
#[derive(Clone, Copy)]
struct TimeSig {
    /// Beats per bar (in beats of the denominator unit).
    numerator: u32,
    /// Note value that gets one beat (2 = half, 4 = quarter, 8 = eighth…).
    denominator: u32,
}

fn diff_channels(items: &mut Vec<String>, a: Option<&Value>, b: Option<&Value>, time_sig: TimeSig) {
    let empty: Vec<Value> = Vec::new();
    let a_arr = a.and_then(Value::as_array).unwrap_or(&empty);
    let b_arr = b.and_then(Value::as_array).unwrap_or(&empty);

    let get_id = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_owned);
    let get_name = |v: &Value| {
        v.get("name")
            .and_then(Value::as_str)
            .unwrap_or("Untitled")
            .to_owned()
    };

    let a_map: HashMap<String, &Value> = a_arr
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();
    let b_map: HashMap<String, &Value> = b_arr
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();

    let added: Vec<String> = b_arr
        .iter()
        .filter(|v| {
            get_id(v)
                .map(|id| !a_map.contains_key(&id))
                .unwrap_or(false)
        })
        .map(&get_name)
        .collect();
    let removed: Vec<String> = a_arr
        .iter()
        .filter(|v| {
            get_id(v)
                .map(|id| !b_map.contains_key(&id))
                .unwrap_or(false)
        })
        .map(get_name)
        .collect();

    let mut renamed: Vec<String> = Vec::new();

    for (id, b_ch) in &b_map {
        if let Some(a_ch) = a_map.get(id) {
            let a_name = get_name(a_ch);
            let b_name = get_name(b_ch);
            if a_name != b_name {
                renamed.push(format!("\"{}\" → \"{}\"", a_name, b_name));
            }
            if a_ch != b_ch {
                diff_clips(items, &b_name, a_ch, b_ch, time_sig);
                diff_channel_mix(items, &b_name, a_ch, b_ch);
                diff_channel_fx(items, &b_name, a_ch, b_ch);
                diff_channel_instrument(items, &b_name, a_ch, b_ch);
            }
        }
    }

    match added.len() {
        0 => {}
        1 => items.push(format!("Channel added: \"{}\"", added[0])),
        n => items.push(format!("{n} channels added")),
    }
    match removed.len() {
        0 => {}
        1 => items.push(format!("Channel removed: \"{}\"", removed[0])),
        n => items.push(format!("{n} channels removed")),
    }
    for r in renamed.iter().take(3) {
        items.push(format!("Channel renamed: {r}"));
    }
}

/// Per-clip diff for a single channel. Surfaces:
///   • adds / removes (by clip id)
///   • rename, position move (bars.beats.subbeats), length change
///   • audio-clip BPM, warp algorithm, pitch shift
///   • loop toggle
///
/// `time_sig` controls how `position_beats` is rendered as
/// `bar.beat.subbeat` — uses the *current* (post-edit) time signature so
/// the diff matches what the user sees on the timeline.
fn diff_clips(
    items: &mut Vec<String>,
    channel_name: &str,
    a_ch: &Value,
    b_ch: &Value,
    time_sig: TimeSig,
) {
    let empty: Vec<Value> = Vec::new();
    let a_clips = a_ch
        .get("clips")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let b_clips = b_ch
        .get("clips")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let get_id = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_owned);
    let a_map: HashMap<String, &Value> = a_clips
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();
    let b_map: HashMap<String, &Value> = b_clips
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();

    for v in b_clips {
        let Some(id) = get_id(v) else { continue };
        if !a_map.contains_key(&id) {
            items.push(format!(
                "{}: clip added \"{}\" at {}",
                channel_name,
                clip_name(v),
                fmt_pos_beats(clip_position(v), time_sig),
            ));
        }
    }
    for v in a_clips {
        let Some(id) = get_id(v) else { continue };
        if !b_map.contains_key(&id) {
            items.push(format!(
                "{}: clip removed \"{}\"",
                channel_name,
                clip_name(v)
            ));
        }
    }

    for (id, b_clip) in &b_map {
        let Some(a_clip) = a_map.get(id) else {
            continue;
        };
        if a_clip == b_clip {
            continue;
        }
        let a_name = clip_name(a_clip);
        let b_name = clip_name(b_clip);
        let scope = format!("{} / \"{}\"", channel_name, b_name);

        if a_name != b_name {
            items.push(format!(
                "{}: clip renamed \"{}\" → \"{}\"",
                channel_name, a_name, b_name
            ));
        }

        let a_pos = a_clip.get("position_beats").and_then(Value::as_f64);
        let b_pos = b_clip.get("position_beats").and_then(Value::as_f64);
        if let Some((ap, bp)) = a_pos.zip(b_pos).filter(|(ap, bp)| ap != bp) {
            items.push(format!(
                "{}: moved {} → {}",
                scope,
                fmt_pos_beats(ap, time_sig),
                fmt_pos_beats(bp, time_sig)
            ));
        }

        let a_len = a_clip.get("length_beats").and_then(Value::as_f64);
        let b_len = b_clip.get("length_beats").and_then(Value::as_f64);
        if let Some((al, bl)) = a_len.zip(b_len).filter(|(al, bl)| al != bl) {
            items.push(format!(
                "{}: length {} → {}",
                scope,
                fmt_length_beats(al),
                fmt_length_beats(bl)
            ));
        }

        // Audio-clip-only fields live under `data.Audio.*` in the serialized
        // form (untagged enum produces a `{ "Audio": { … } }` wrapper).
        let a_bpm = audio_field(a_clip, "clip_bpm").and_then(Value::as_f64);
        let b_bpm = audio_field(b_clip, "clip_bpm").and_then(Value::as_f64);
        if let Some((ab, bb)) = a_bpm.zip(b_bpm).filter(|(ab, bb)| ab != bb) {
            items.push(format!(
                "{}: clip BPM {} → {}",
                scope,
                fmt_f64(ab),
                fmt_f64(bb)
            ));
        }
        let a_warp = audio_field(a_clip, "warp_algorithm").and_then(Value::as_str);
        let b_warp = audio_field(b_clip, "warp_algorithm").and_then(Value::as_str);
        if let Some((aw, bw)) = a_warp.zip(b_warp).filter(|(aw, bw)| aw != bw) {
            items.push(format!("{}: warp algorithm {} → {}", scope, aw, bw));
        }
        let a_pitch = audio_field(a_clip, "pitch_semitones").and_then(Value::as_f64);
        let b_pitch = audio_field(b_clip, "pitch_semitones").and_then(Value::as_f64);
        if let Some((ap, bp)) = a_pitch.zip(b_pitch).filter(|(ap, bp)| ap != bp) {
            items.push(format!(
                "{}: pitch {} → {} st",
                scope,
                fmt_f64(ap),
                fmt_f64(bp)
            ));
        }

        let a_loop = a_clip
            .get("looping")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let b_loop = b_clip
            .get("looping")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if a_loop != b_loop {
            items.push(format!(
                "{}: loop {}",
                scope,
                if b_loop { "enabled" } else { "disabled" }
            ));
        }
    }
}

fn clip_name(v: &Value) -> String {
    v.get("name")
        .and_then(Value::as_str)
        .unwrap_or("Untitled clip")
        .to_owned()
}

fn clip_position(v: &Value) -> f64 {
    v.get("position_beats")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn audio_field<'a>(v: &'a Value, field: &str) -> Option<&'a Value> {
    v.get("data")
        .and_then(|d| d.get("Audio"))
        .and_then(|a| a.get(field))
}

/// Format a position in beats as `bar.beat.subbeat` (1-indexed bar/beat,
/// 0-indexed sixteenth-note subbeat within the beat). `time_sig` controls
/// the bar length — `numerator` beats per bar of the `denominator` unit
/// (e.g. 6/8 → 6 eighth-notes per bar, which is 3 quarter-note beats).
fn fmt_pos_beats(beats: f64, time_sig: TimeSig) -> String {
    // Project clips store `position_beats` in *quarter-note* beats. Convert
    // the time-sig numerator (in `denominator` units) into quarter-note
    // beats so the math stays in a single unit.
    let bar_len_qbeats = time_sig.numerator as f64 * (4.0 / time_sig.denominator as f64);
    let bar_len_qbeats = if bar_len_qbeats <= 0.0 {
        4.0
    } else {
        bar_len_qbeats
    };
    let bar = (beats / bar_len_qbeats).floor() as i64 + 1;
    let pos_in_bar_qbeats = beats - (bar - 1) as f64 * bar_len_qbeats;
    // Beat within the bar, in the time-sig's denominator unit.
    let beat_unit_qbeats = 4.0 / time_sig.denominator as f64;
    let beat_in_bar = pos_in_bar_qbeats / beat_unit_qbeats;
    let beat = beat_in_bar.floor() as i64 + 1;
    let subbeat = (beat_in_bar.fract() * 4.0).round() as i64;
    let subbeat = subbeat.clamp(0, 3);
    format!("{bar}.{beat}.{subbeat}")
}

fn fmt_length_beats(beats: f64) -> String {
    if beats.fract() == 0.0 {
        format!("{} beats", beats as i64)
    } else {
        format!("{:.2} beats", beats)
    }
}

// ── Structured diff (for richer renderers) ───────────────────────────────────

/// Whether a row represents an addition, a removal, or a modification.
/// Drives icon/colour choice in the Save Version sidebar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Add,
    Remove,
    Modify,
}

/// Category of channel — used for the per-card AUDIO / MIDI / PLUGIN badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelKind {
    Audio,
    Midi,
    Plugin,
    Other,
}

/// Specific kind of clip-level (or channel-level) change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeTag {
    Added,
    Removed,
    Renamed,
    Moved,
    Length,
    Pitch,
    Warp,
    ClipBpm,
    Loop,
    PluginPreset,
    // Channel-level mix/routing changes
    Volume,
    Pan,
    Muted,
    Solo,
    // FX chain changes
    FxAdded,
    FxRemoved,
    FxReordered,
    // Instrument changes
    InstrumentChanged,
}

impl ChangeTag {
    pub fn label(self) -> &'static str {
        match self {
            ChangeTag::Added => "ADDED",
            ChangeTag::Removed => "REMOVED",
            ChangeTag::Renamed => "RENAMED",
            ChangeTag::Moved => "MOVED",
            ChangeTag::Length => "LENGTH",
            ChangeTag::Pitch => "PITCH",
            ChangeTag::Warp => "WARP ALGORITHM",
            ChangeTag::ClipBpm => "CLIP BPM",
            ChangeTag::Loop => "LOOP",
            ChangeTag::PluginPreset => "PRESET",
            ChangeTag::Volume => "VOLUME",
            ChangeTag::Pan => "PAN",
            ChangeTag::Muted => "MUTE",
            ChangeTag::Solo => "SOLO",
            ChangeTag::FxAdded => "FX ADDED",
            ChangeTag::FxRemoved => "FX REMOVED",
            ChangeTag::FxReordered => "FX ORDER",
            ChangeTag::InstrumentChanged => "INSTRUMENT",
        }
    }
}

/// One change row inside a channel card.
#[derive(Clone, Debug)]
pub struct ChangeRow {
    pub tag: ChangeTag,
    pub kind: ChangeKind,
    /// Clip or sub-target the change applies to (e.g. clip name).
    pub target: String,
    /// Previous value formatted for display, when applicable (omit for adds).
    pub before: Option<String>,
    /// New value formatted for display (omit only when meaningless, e.g.
    /// removals where `before` carries the data instead).
    pub after: Option<String>,
}

/// One channel card in the Save Version right panel.
#[derive(Clone, Debug)]
pub struct ChannelDiff {
    pub name: String,
    pub kind: ChannelKind,
    pub status: ChangeKind,
    pub clips_added: usize,
    pub clips_removed: usize,
    pub clips_modified: usize,
    pub rows: Vec<ChangeRow>,
}

/// Project-level structured diff. Returned by [`structured_diff`].
#[derive(Clone, Debug, Default)]
pub struct ProjectDiff {
    /// Project-wide changes that don't belong to any single channel
    /// (tempo, key/scale, automation, markers, notes).
    pub project_changes: Vec<String>,
    /// Time signature of the *current* snapshot — surfaced in the header
    /// summary line.
    pub time_sig: (u32, u32),
    /// Per-channel cards in arrival order.
    pub channels: Vec<ChannelDiff>,
}

impl ProjectDiff {
    pub fn total_clips_added(&self) -> usize {
        self.channels.iter().map(|c| c.clips_added).sum()
    }
    pub fn total_clips_removed(&self) -> usize {
        self.channels.iter().map(|c| c.clips_removed).sum()
    }
    pub fn total_clips_modified(&self) -> usize {
        self.channels.iter().map(|c| c.clips_modified).sum()
    }
    pub fn channel_count(&self) -> usize {
        self.channels.len()
    }
    pub fn is_empty(&self) -> bool {
        self.project_changes.is_empty() && self.channels.is_empty()
    }
}

/// Compute a structured diff between two canonical project snapshots.
/// Mirrors [`summarize_diff`] but returns typed rows the Save Version dialog
/// can render as per-channel cards. External DAW changes are returned as
/// project-level rows because their native channel schemas are DAW-specific.
pub fn structured_diff(ancestor: &Value, current: &Value) -> ProjectDiff {
    if is_external_snapshot(ancestor) || is_external_snapshot(current) {
        return ProjectDiff {
            project_changes: summarize_external_diff(ancestor, current, false),
            time_sig: (4, 4),
            channels: Vec::new(),
        };
    }

    let mut out = ProjectDiff::default();

    // ── Project-level changes ───────────────────────────────────────
    let a_bpm = ancestor.get("bpm").and_then(Value::as_f64);
    let b_bpm = current.get("bpm").and_then(Value::as_f64);
    if a_bpm != b_bpm {
        match (a_bpm, b_bpm) {
            (Some(a), Some(b)) => {
                out.project_changes
                    .push(format!("Tempo: {} → {}", fmt_f64(a), fmt_f64(b)))
            }
            _ => out.project_changes.push("Tempo changed".to_owned()),
        }
    }
    let a_num = ancestor
        .get("time_sig_numerator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    let a_den = ancestor
        .get("time_sig_denominator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    let b_num = current
        .get("time_sig_numerator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    let b_den = current
        .get("time_sig_denominator")
        .and_then(Value::as_u64)
        .unwrap_or(4);
    if a_num != b_num || a_den != b_den {
        out.project_changes
            .push(format!("Time signature: {a_num}/{a_den} → {b_num}/{b_den}"));
    }
    out.time_sig = (b_num.max(1) as u32, b_den.max(1) as u32);

    if ancestor.get("project_scale") != current.get("project_scale") {
        let label = |v: &Value| {
            v.get("project_scale")
                .and_then(|s| s.get("label").or_else(|| s.as_str().map(|_| s)))
                .and_then(Value::as_str)
                .unwrap_or("?")
                .to_owned()
        };
        out.project_changes.push(format!(
            "Key/Scale: {} → {}",
            label(ancestor),
            label(current)
        ));
    }
    if ancestor.get("automation") != current.get("automation") {
        out.project_changes.push("Automation changed".to_owned());
    }
    if ancestor.get("notes") != current.get("notes") {
        out.project_changes.push("Project notes updated".to_owned());
    }

    let time_sig = TimeSig {
        numerator: out.time_sig.0,
        denominator: out.time_sig.1,
    };

    // ── Per-channel cards ───────────────────────────────────────────
    let empty: Vec<Value> = Vec::new();
    let a_channels = ancestor
        .get("channels")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let b_channels = current
        .get("channels")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let get_id = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_owned);
    let a_map: HashMap<String, &Value> = a_channels
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();
    let b_map: HashMap<String, &Value> = b_channels
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();

    // Added channels.
    for v in b_channels {
        let Some(id) = get_id(v) else { continue };
        if a_map.contains_key(&id) {
            continue;
        }
        let mut card = empty_channel_diff(v, ChangeKind::Add);
        // Every clip in the new channel counts as an addition.
        for clip in v
            .get("clips")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            card.clips_added += 1;
            card.rows.push(ChangeRow {
                tag: ChangeTag::Added,
                kind: ChangeKind::Add,
                target: clip_name(clip),
                before: None,
                after: Some(format!(
                    "at {}",
                    fmt_pos_beats(clip_position(clip), time_sig)
                )),
            });
        }
        out.channels.push(card);
    }

    // Removed channels.
    for v in a_channels {
        let Some(id) = get_id(v) else { continue };
        if b_map.contains_key(&id) {
            continue;
        }
        let mut card = empty_channel_diff(v, ChangeKind::Remove);
        for clip in v
            .get("clips")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            card.clips_removed += 1;
            card.rows.push(ChangeRow {
                tag: ChangeTag::Removed,
                kind: ChangeKind::Remove,
                target: clip_name(clip),
                before: None,
                after: Some("gone".into()),
            });
        }
        out.channels.push(card);
    }

    // Modified channels (matched by id, content differs).
    for (id, b_ch) in &b_map {
        let Some(a_ch) = a_map.get(id) else { continue };
        if a_ch == b_ch {
            continue;
        }
        let mut card = empty_channel_diff(b_ch, ChangeKind::Modify);
        // Channel rename surfaces as a row on the modified card.
        let a_name = channel_name(a_ch);
        let b_name = channel_name(b_ch);
        if a_name != b_name {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Renamed,
                kind: ChangeKind::Modify,
                target: a_name.clone(),
                before: None,
                after: Some(format!("\"{}\"", b_name)),
            });
        }
        push_clip_rows(&mut card, a_ch, b_ch, time_sig);
        push_mix_rows(&mut card, a_ch, b_ch);
        push_fx_rows(&mut card, a_ch, b_ch);
        push_instrument_rows(&mut card, a_ch, b_ch);
        if !card.rows.is_empty() {
            out.channels.push(card);
        }
    }

    out
}

fn channel_name(v: &Value) -> String {
    v.get("name")
        .and_then(Value::as_str)
        .unwrap_or("Untitled")
        .to_owned()
}

fn detect_channel_kind(v: &Value) -> ChannelKind {
    if v.get("instrument").map(|i| !i.is_null()).unwrap_or(false) {
        return ChannelKind::Midi;
    }
    let clips = v.get("clips").and_then(Value::as_array);
    if let Some(arr) = clips {
        for c in arr {
            if let Some(data) = c.get("data") {
                if data.get("Midi").is_some() {
                    return ChannelKind::Midi;
                }
                if data.get("Audio").is_some() {
                    return ChannelKind::Audio;
                }
            }
        }
    }
    let has_fx = v
        .get("fx_chain")
        .and_then(Value::as_array)
        .map(|a| !a.is_empty())
        .unwrap_or(false);
    if has_fx {
        ChannelKind::Plugin
    } else {
        ChannelKind::Other
    }
}

fn empty_channel_diff(v: &Value, status: ChangeKind) -> ChannelDiff {
    ChannelDiff {
        name: channel_name(v),
        kind: detect_channel_kind(v),
        status,
        clips_added: 0,
        clips_removed: 0,
        clips_modified: 0,
        rows: Vec::new(),
    }
}

fn push_clip_rows(card: &mut ChannelDiff, a_ch: &Value, b_ch: &Value, time_sig: TimeSig) {
    let empty: Vec<Value> = Vec::new();
    let a_clips = a_ch
        .get("clips")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let b_clips = b_ch
        .get("clips")
        .and_then(Value::as_array)
        .unwrap_or(&empty);

    let get_id = |v: &Value| v.get("id").and_then(Value::as_str).map(str::to_owned);
    let a_map: HashMap<String, &Value> = a_clips
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();
    let b_map: HashMap<String, &Value> = b_clips
        .iter()
        .filter_map(|v| get_id(v).map(|id| (id, v)))
        .collect();

    // Added clips.
    for v in b_clips {
        let Some(id) = get_id(v) else { continue };
        if a_map.contains_key(&id) {
            continue;
        }
        card.clips_added += 1;
        card.rows.push(ChangeRow {
            tag: ChangeTag::Added,
            kind: ChangeKind::Add,
            target: clip_name(v),
            before: None,
            after: Some(format!("at {}", fmt_pos_beats(clip_position(v), time_sig))),
        });
    }
    // Removed clips.
    for v in a_clips {
        let Some(id) = get_id(v) else { continue };
        if b_map.contains_key(&id) {
            continue;
        }
        card.clips_removed += 1;
        card.rows.push(ChangeRow {
            tag: ChangeTag::Removed,
            kind: ChangeKind::Remove,
            target: clip_name(v),
            before: None,
            after: Some("gone".into()),
        });
    }
    // Modified clips.
    for (id, b_clip) in &b_map {
        let Some(a_clip) = a_map.get(id) else {
            continue;
        };
        if a_clip == b_clip {
            continue;
        }
        card.clips_modified += 1;
        let target = clip_name(b_clip);

        let a_name = clip_name(a_clip);
        if a_name != target {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Renamed,
                kind: ChangeKind::Modify,
                target: a_name,
                before: None,
                after: Some(format!("\"{}\"", target)),
            });
        }
        let a_pos = a_clip.get("position_beats").and_then(Value::as_f64);
        let b_pos = b_clip.get("position_beats").and_then(Value::as_f64);
        if let Some((ap, bp)) = a_pos.zip(b_pos).filter(|(ap, bp)| ap != bp) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Moved,
                kind: ChangeKind::Modify,
                target: target.clone(),
                before: Some(fmt_pos_beats(ap, time_sig)),
                after: Some(fmt_pos_beats(bp, time_sig)),
            });
        }
        let a_len = a_clip.get("length_beats").and_then(Value::as_f64);
        let b_len = b_clip.get("length_beats").and_then(Value::as_f64);
        if let Some((al, bl)) = a_len.zip(b_len).filter(|(al, bl)| al != bl) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Length,
                kind: ChangeKind::Modify,
                target: target.clone(),
                before: Some(fmt_length_beats(al)),
                after: Some(fmt_length_beats(bl)),
            });
        }
        let a_bpm = audio_field(a_clip, "clip_bpm").and_then(Value::as_f64);
        let b_bpm = audio_field(b_clip, "clip_bpm").and_then(Value::as_f64);
        if let Some((ab, bb)) = a_bpm.zip(b_bpm).filter(|(ab, bb)| ab != bb) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::ClipBpm,
                kind: ChangeKind::Modify,
                target: target.clone(),
                before: Some(fmt_f64(ab)),
                after: Some(fmt_f64(bb)),
            });
        }
        let a_warp = audio_field(a_clip, "warp_algorithm").and_then(Value::as_str);
        let b_warp = audio_field(b_clip, "warp_algorithm").and_then(Value::as_str);
        if let Some((aw, bw)) = a_warp.zip(b_warp).filter(|(aw, bw)| aw != bw) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Warp,
                kind: ChangeKind::Modify,
                target: target.clone(),
                before: Some(aw.to_owned()),
                after: Some(bw.to_owned()),
            });
        }
        let a_pitch = audio_field(a_clip, "pitch_semitones").and_then(Value::as_f64);
        let b_pitch = audio_field(b_clip, "pitch_semitones").and_then(Value::as_f64);
        if let Some((ap, bp)) = a_pitch.zip(b_pitch).filter(|(ap, bp)| ap != bp) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Pitch,
                kind: ChangeKind::Modify,
                target: target.clone(),
                before: Some(format!("{} st", fmt_f64(ap))),
                after: Some(format!("{} st", fmt_f64(bp))),
            });
        }
        let a_loop = a_clip
            .get("looping")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let b_loop = b_clip
            .get("looping")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if a_loop != b_loop {
            card.rows.push(ChangeRow {
                tag: ChangeTag::Loop,
                kind: ChangeKind::Modify,
                target: target.clone(),
                before: None,
                after: Some(if b_loop {
                    "enabled".into()
                } else {
                    "disabled".into()
                }),
            });
        }
    }
}

/// Volume / pan / mute / solo changes for a single channel.
fn diff_channel_mix(items: &mut Vec<String>, channel_name: &str, a_ch: &Value, b_ch: &Value) {
    let a_vol = a_ch.get("volume").and_then(Value::as_f64);
    let b_vol = b_ch.get("volume").and_then(Value::as_f64);
    if let Some((av, bv)) = a_vol.zip(b_vol).filter(|(av, bv)| av != bv) {
        items.push(format!(
            "{channel_name}: volume {} → {} dB",
            fmt_f64(linear_to_db(av)),
            fmt_f64(linear_to_db(bv))
        ));
    }

    let a_pan = a_ch.get("pan").and_then(Value::as_f64);
    let b_pan = b_ch.get("pan").and_then(Value::as_f64);
    if let Some((ap, bp)) = a_pan.zip(b_pan).filter(|(ap, bp)| ap != bp) {
        items.push(format!(
            "{channel_name}: pan {} → {}",
            fmt_pan(ap),
            fmt_pan(bp)
        ));
    }

    let a_mute = a_ch.get("muted").and_then(Value::as_bool);
    let b_mute = b_ch.get("muted").and_then(Value::as_bool);
    if let Some((am, bm)) = a_mute.zip(b_mute).filter(|(am, bm)| am != bm) {
        if bm {
            items.push(format!("{channel_name}: muted"));
        } else if am {
            items.push(format!("{channel_name}: unmuted"));
        }
    }

    let a_solo = a_ch.get("solo").and_then(Value::as_bool);
    let b_solo = b_ch.get("solo").and_then(Value::as_bool);
    if let Some((as_, bs)) = a_solo.zip(b_solo).filter(|(as_, bs)| as_ != bs) {
        if bs {
            items.push(format!("{channel_name}: soloed"));
        } else if as_ {
            items.push(format!("{channel_name}: solo cleared"));
        }
    }
}

/// FX-chain changes for a single channel (adds, removes, reorders).
fn diff_channel_fx(items: &mut Vec<String>, channel_name: &str, a_ch: &Value, b_ch: &Value) {
    let empty = vec![];
    let a_fx = a_ch
        .get("fx_chain")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let b_fx = b_ch
        .get("fx_chain")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if a_fx == b_fx {
        return;
    }

    let plugin_name = |v: &Value| {
        v.get("plugin")
            .and_then(|p| p.get("name").or_else(|| p.get("id")))
            .and_then(Value::as_str)
            .unwrap_or("Unknown plugin")
            .to_owned()
    };

    let a_names: Vec<String> = a_fx.iter().map(&plugin_name).collect();
    let b_names: Vec<String> = b_fx.iter().map(plugin_name).collect();

    for name in &b_names {
        if !a_names.contains(name) {
            items.push(format!("{channel_name}: FX added — {name}"));
        }
    }
    for name in &a_names {
        if !b_names.contains(name) {
            items.push(format!("{channel_name}: FX removed — {name}"));
        }
    }
    // Reorder: same set of plugins but different order.
    let mut a_sorted = a_names.clone();
    let mut b_sorted = b_names.clone();
    a_sorted.sort();
    b_sorted.sort();
    if a_sorted == b_sorted && a_names != b_names {
        items.push(format!("{channel_name}: FX chain reordered"));
    }
}

/// Instrument-plugin change for a single channel.
fn diff_channel_instrument(
    items: &mut Vec<String>,
    channel_name: &str,
    a_ch: &Value,
    b_ch: &Value,
) {
    let inst_name = |v: &Value| {
        v.get("instrument")
            .filter(|i| !i.is_null())
            .and_then(|i| i.get("plugin"))
            .and_then(|p| p.get("name").or_else(|| p.get("id")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let a_inst = inst_name(a_ch);
    let b_inst = inst_name(b_ch);
    if a_inst != b_inst {
        match (a_inst, b_inst) {
            (Some(a), Some(b)) => items.push(format!("{channel_name}: instrument {a} → {b}")),
            (None, Some(b)) => items.push(format!("{channel_name}: instrument set — {b}")),
            (Some(a), None) => items.push(format!("{channel_name}: instrument removed — {a}")),
            (None, None) => {}
        }
    }
}

/// Convert a linear gain value to dB (−inf for 0).
fn linear_to_db(linear: f64) -> f64 {
    if linear <= 0.0 {
        return -f64::INFINITY;
    }
    20.0 * linear.log10()
}

/// Format a pan value in [−1, 1] as a user-readable string.
fn fmt_pan(pan: f64) -> String {
    if pan.abs() < 0.005 {
        "C".to_owned()
    } else if pan < 0.0 {
        format!("{}L", fmt_f64(-pan * 100.0))
    } else {
        format!("{}R", fmt_f64(pan * 100.0))
    }
}

/// Emit structured volume / pan / mute / solo rows onto a channel card.
fn push_mix_rows(card: &mut ChannelDiff, a_ch: &Value, b_ch: &Value) {
    let a_vol = a_ch.get("volume").and_then(Value::as_f64);
    let b_vol = b_ch.get("volume").and_then(Value::as_f64);
    if let Some((av, bv)) = a_vol.zip(b_vol).filter(|(av, bv)| av != bv) {
        card.rows.push(ChangeRow {
            tag: ChangeTag::Volume,
            kind: ChangeKind::Modify,
            target: card.name.clone(),
            before: Some(format!("{} dB", fmt_f64(linear_to_db(av)))),
            after: Some(format!("{} dB", fmt_f64(linear_to_db(bv)))),
        });
    }

    let a_pan = a_ch.get("pan").and_then(Value::as_f64);
    let b_pan = b_ch.get("pan").and_then(Value::as_f64);
    if let Some((ap, bp)) = a_pan.zip(b_pan).filter(|(ap, bp)| ap != bp) {
        card.rows.push(ChangeRow {
            tag: ChangeTag::Pan,
            kind: ChangeKind::Modify,
            target: card.name.clone(),
            before: Some(fmt_pan(ap)),
            after: Some(fmt_pan(bp)),
        });
    }

    let a_mute = a_ch.get("muted").and_then(Value::as_bool);
    let b_mute = b_ch.get("muted").and_then(Value::as_bool);
    if a_mute != b_mute {
        card.rows.push(ChangeRow {
            tag: ChangeTag::Muted,
            kind: ChangeKind::Modify,
            target: card.name.clone(),
            before: a_mute.map(|v| if v { "muted" } else { "active" }.to_owned()),
            after: b_mute.map(|v| if v { "muted" } else { "active" }.to_owned()),
        });
    }

    let a_solo = a_ch.get("solo").and_then(Value::as_bool);
    let b_solo = b_ch.get("solo").and_then(Value::as_bool);
    if a_solo != b_solo {
        card.rows.push(ChangeRow {
            tag: ChangeTag::Solo,
            kind: ChangeKind::Modify,
            target: card.name.clone(),
            before: a_solo.map(|v| if v { "soloed" } else { "off" }.to_owned()),
            after: b_solo.map(|v| if v { "soloed" } else { "off" }.to_owned()),
        });
    }
}

/// Emit structured FX-chain rows onto a channel card.
fn push_fx_rows(card: &mut ChannelDiff, a_ch: &Value, b_ch: &Value) {
    let empty = vec![];
    let a_fx = a_ch
        .get("fx_chain")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    let b_fx = b_ch
        .get("fx_chain")
        .and_then(Value::as_array)
        .unwrap_or(&empty);
    if a_fx == b_fx {
        return;
    }

    let plugin_name = |v: &Value| {
        v.get("plugin")
            .and_then(|p| p.get("name").or_else(|| p.get("id")))
            .and_then(Value::as_str)
            .unwrap_or("Unknown plugin")
            .to_owned()
    };

    let a_names: Vec<String> = a_fx.iter().map(&plugin_name).collect();
    let b_names: Vec<String> = b_fx.iter().map(plugin_name).collect();

    for name in &b_names {
        if !a_names.contains(name) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::FxAdded,
                kind: ChangeKind::Add,
                target: name.clone(),
                before: None,
                after: None,
            });
        }
    }
    for name in &a_names {
        if !b_names.contains(name) {
            card.rows.push(ChangeRow {
                tag: ChangeTag::FxRemoved,
                kind: ChangeKind::Remove,
                target: name.clone(),
                before: None,
                after: None,
            });
        }
    }
    let mut a_sorted = a_names.clone();
    let mut b_sorted = b_names.clone();
    a_sorted.sort();
    b_sorted.sort();
    if a_sorted == b_sorted && a_names != b_names {
        card.rows.push(ChangeRow {
            tag: ChangeTag::FxReordered,
            kind: ChangeKind::Modify,
            target: card.name.clone(),
            before: None,
            after: None,
        });
    }
}

/// Emit a structured instrument-change row onto a channel card.
fn push_instrument_rows(card: &mut ChannelDiff, a_ch: &Value, b_ch: &Value) {
    let inst_name = |v: &Value| {
        v.get("instrument")
            .filter(|i| !i.is_null())
            .and_then(|i| i.get("plugin"))
            .and_then(|p| p.get("name").or_else(|| p.get("id")))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    let a_inst = inst_name(a_ch);
    let b_inst = inst_name(b_ch);
    if a_inst == b_inst {
        return;
    }
    card.rows.push(ChangeRow {
        tag: ChangeTag::InstrumentChanged,
        kind: ChangeKind::Modify,
        target: card.name.clone(),
        before: a_inst,
        after: b_inst,
    });
}

fn diff_array_count(items: &mut Vec<String>, a: Option<&Value>, b: Option<&Value>, noun: &str) {
    let a_len = a.and_then(Value::as_array).map(|v| v.len()).unwrap_or(0);
    let b_len = b.and_then(Value::as_array).map(|v| v.len()).unwrap_or(0);
    if a_len != b_len {
        let diff = b_len as isize - a_len as isize;
        if diff > 0 {
            items.push(format!("{diff} {noun}(s) added"));
        } else {
            items.push(format!("{} {noun}(s) removed", diff.unsigned_abs()));
        }
    } else if a != b {
        items.push(format!("{noun}(s) changed"));
    }
}

fn is_external_snapshot(snapshot: &Value) -> bool {
    snapshot.get("auru_pm_snapshot").is_some()
}

fn summarize_external_diff(
    ancestor: &Value,
    current: &Value,
    include_no_changes: bool,
) -> DiffSummary {
    let mut items = Vec::new();
    let ancestor_format = external_format_label(ancestor);
    let current_format = external_format_label(current);

    if ancestor_format != current_format {
        match (ancestor_format, current_format) {
            (Some(before), Some(after)) => {
                items.push(format!("Project format: {before} → {after}"));
            }
            (None, Some(after)) => items.push(format!("{after} project added")),
            (Some(before), None) => items.push(format!("{before} project removed")),
            (None, None) => {}
        }
    }

    if ancestor.get("project") != current.get("project") {
        let format = current_format.or(ancestor_format).unwrap_or("External DAW");
        items.push(format!("{format} project XML changed"));
    }
    if ancestor.get("metadata") != current.get("metadata") {
        items.push("DAWproject metadata changed".to_owned());
    }

    diff_external_resources(&mut items, ancestor, current);

    if include_no_changes && items.is_empty() {
        items.push("No structural changes".to_owned());
    }
    items
}

fn external_format_label(snapshot: &Value) -> Option<&'static str> {
    match snapshot.get("format").and_then(Value::as_str) {
        Some("dawproject") => Some("DAWproject"),
        Some("ableton-live-set") => Some("Ableton Live Set"),
        Some(_) => Some("External DAW"),
        None => None,
    }
}

fn diff_external_resources(items: &mut Vec<String>, ancestor: &Value, current: &Value) {
    let ancestor_resources = external_resources(ancestor);
    let current_resources = external_resources(current);

    for id in current_resources.keys() {
        if !ancestor_resources.contains_key(id) {
            items.push(format!("Embedded resource added: {id}"));
        }
    }
    for id in ancestor_resources.keys() {
        if !current_resources.contains_key(id) {
            items.push(format!("Embedded resource removed: {id}"));
        }
    }
    for (id, resource) in &current_resources {
        if ancestor_resources
            .get(id)
            .is_some_and(|ancestor| *ancestor != *resource)
        {
            items.push(format!("Embedded resource changed: {id}"));
        }
    }
    items.sort();
}

fn external_resources(snapshot: &Value) -> HashMap<&str, &Value> {
    snapshot
        .get("resources")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|resource| {
            resource
                .get("id")
                .and_then(Value::as_str)
                .map(|id| (id, resource))
        })
        .collect()
}

#[cfg(test)]
mod external_snapshot_tests {
    use super::*;

    #[test]
    fn external_diff_should_report_xml_and_resource_changes() {
        let ancestor = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "dawproject",
            "project": {"root": {"tag": "Project"}},
            "resources": [{"id": "audio/kick.wav", "data": "old"}]
        });
        let current = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "dawproject",
            "project": {"root": {"tag": "Project", "attributes": {"version": "1.0"}}},
            "resources": [
                {"id": "audio/kick.wav", "data": "new"},
                {"id": "audio/snare.wav", "data": "added"}
            ]
        });

        let summary = summarize_diff(&ancestor, &current);
        assert!(
            summary
                .iter()
                .any(|item| item.contains("project XML changed"))
        );
        assert!(
            summary
                .iter()
                .any(|item| item == "Embedded resource changed: audio/kick.wav")
        );
        assert!(
            summary
                .iter()
                .any(|item| item == "Embedded resource added: audio/snare.wav")
        );
    }

    #[test]
    fn structured_external_diff_should_use_project_level_changes() {
        let ancestor = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "ableton-live-set",
            "project": {"root": {"tag": "Ableton"}}
        });
        let current = serde_json::json!({
            "auru_pm_snapshot": 1,
            "format": "ableton-live-set",
            "project": {"root": {"tag": "Ableton", "children": [{"text": "changed"}]}}
        });

        let diff = structured_diff(&ancestor, &current);
        assert!(diff.channels.is_empty());
        assert_eq!(
            diff.project_changes,
            vec!["Ableton Live Set project XML changed"]
        );
    }
}
