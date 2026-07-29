//! What an FL Studio project *is* — the detail a person recognises it by.

use serde::{Deserialize, Serialize};

use super::events::{Stream, decode_ascii, decode_utf16, uses_utf16};
use super::plugins;
use super::refs::{self, AssetRef, RefClass};

/// Tempo, stored as beats per minute multiplied by a thousand.
///
/// The multiplier is why this is an integer event: FL supports fractional
/// tempos, and 92.5 BPM is stored as `92500`.
pub const EVENT_TEMPO: u8 = 156;
const TEMPO_SCALE: f64 = 1000.0;

const EVENT_TIME_SIG_NUMERATOR: u8 = 17;
const EVENT_TIME_SIG_DENOMINATOR: u8 = 18;
const EVENT_VERSION_BUILD: u8 = 159;
const EVENT_PATTERN_NAME: u8 = 193;
const EVENT_TITLE: u8 = 194;
const EVENT_COMMENT: u8 = 195;
const EVENT_URL: u8 = 197;
const EVENT_INSERT_NAME: u8 = 204;
const EVENT_MARKER_NAME: u8 = 205;
const EVENT_GENRE: u8 = 206;
const EVENT_AUTHOR: u8 = 207;

/// How many of a project's files fall into each class.
///
/// Counts **distinct files**, not references: the same sample loaded onto
/// eight channels is one file to back up, and saying eight would misrepresent
/// what an upload is about to cost.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssetSummary {
    pub total: usize,
    pub project_relative: usize,
    pub external: usize,
    pub user_data: usize,
    /// Samples living in scratch space the system may delete at any time.
    pub fragile: usize,
    pub missing: usize,
}

impl AssetSummary {
    pub fn tally(refs: &[AssetRef]) -> Self {
        let mut summary = Self::default();
        for reference in refs::distinct(refs) {
            summary.total += 1;
            match reference.class {
                RefClass::ProjectRelative => summary.project_relative += 1,
                RefClass::External => summary.external += 1,
                RefClass::UserData => summary.user_data += 1,
                RefClass::Fragile => summary.fragile += 1,
                RefClass::Missing => summary.missing += 1,
            }
        }
        summary
    }

    /// Files that would be captured into a backup.
    pub const fn vendored(&self) -> usize {
        self.external + self.user_data + self.fragile
    }
}

/// Everything read from a project without opening its audio.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FlStudioMetadata {
    /// eg `20.5.0.1142`.
    pub version: Option<String>,
    pub build: Option<u32>,
    pub title: Option<String>,
    pub author: Option<String>,
    pub genre: Option<String>,
    pub url: Option<String>,
    pub comment: Option<String>,
    pub tempo: Option<f64>,
    pub time_signature: Option<(u32, u32)>,
    /// Tick resolution every position in the project is expressed in.
    pub ppq: u16,
    /// Channels in the rack, as the header records them.
    pub channels: u16,
    pub pattern_names: Vec<String>,
    /// Named mixer inserts, in the order they appear.
    pub insert_names: Vec<String>,
    /// Arrangement markers — the sections of the song.
    pub markers: Vec<String>,
    pub plugins: Vec<crate::ableton::PluginRef>,
    pub assets: AssetSummary,
}

impl FlStudioMetadata {
    /// A one-line description, for a list.
    pub fn headline(&self) -> String {
        let mut parts = Vec::new();
        if let Some(tempo) = self.tempo {
            parts.push(format!("{} BPM", format_tempo(tempo)));
        }
        if let Some((numerator, denominator)) = self.time_signature {
            parts.push(format!("{numerator}/{denominator}"));
        }
        parts.push(match self.channels {
            1 => "1 channel".to_owned(),
            channels => format!("{channels} channels"),
        });
        parts.join(" · ")
    }
}

/// Read a project's detail.
pub fn extract(stream: &Stream) -> FlStudioMetadata {
    let utf16 = uses_utf16(stream.major_version());
    let text = |payload: &[u8]| {
        if utf16 {
            decode_utf16(payload)
        } else {
            decode_ascii(payload)
        }
    };

    let mut meta = FlStudioMetadata {
        version: stream.version(),
        ppq: stream.header.ppq,
        channels: stream.header.channels,
        plugins: plugins::collect(stream),
        assets: AssetSummary::tally(&refs::collect(stream)),
        ..FlStudioMetadata::default()
    };

    let mut numerator = None;
    let mut denominator = None;
    for event in &stream.events {
        match event.id {
            EVENT_TEMPO => {
                meta.tempo = event.as_u32().map(|raw| f64::from(raw) / TEMPO_SCALE);
            }
            EVENT_TIME_SIG_NUMERATOR => numerator = event.as_u32(),
            EVENT_TIME_SIG_DENOMINATOR => denominator = event.as_u32(),
            EVENT_VERSION_BUILD => meta.build = event.as_u32(),
            EVENT_TITLE => meta.title = non_empty(text(&event.payload)),
            EVENT_AUTHOR => meta.author = non_empty(text(&event.payload)),
            EVENT_GENRE => meta.genre = non_empty(text(&event.payload)),
            EVENT_URL => meta.url = non_empty(text(&event.payload)),
            EVENT_COMMENT => meta.comment = non_empty(text(&event.payload)),
            EVENT_PATTERN_NAME => push_named(&mut meta.pattern_names, text(&event.payload)),
            EVENT_INSERT_NAME => push_named(&mut meta.insert_names, text(&event.payload)),
            EVENT_MARKER_NAME => push_named(&mut meta.markers, text(&event.payload)),
            _ => {}
        }
    }

    // Both parts or neither: half a time signature is not one, and reporting
    // "4/" would look like a bug rather than missing data.
    meta.time_signature = numerator.zip(denominator).filter(|(_, d)| *d != 0);
    meta
}

fn non_empty(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

fn push_named(into: &mut Vec<String>, value: String) {
    if !value.trim().is_empty() {
        into.push(value);
    }
}

/// Tempo without a trailing `.0`, since most projects are whole numbers.
fn format_tempo(tempo: f64) -> String {
    if (tempo.fract()).abs() < f64::EPSILON {
        format!("{tempo:.0}")
    } else {
        format!("{tempo}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flstudio::events::{Event, Header};

    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    fn project(channels: u16, events: Vec<Event>) -> Stream {
        let mut all = vec![Event::new(199, b"20.5.0.1142\0".to_vec())];
        all.extend(events);
        Stream {
            header: Header {
                format: 0,
                channels,
                ppq: 96,
            },
            events: all,
        }
    }

    #[test]
    fn tempo_should_be_divided_by_a_thousand() {
        // Both real projects store it this way: 92000 and 150000.
        let meta = extract(&project(
            2,
            vec![Event::new(EVENT_TEMPO, 92_000u32.to_le_bytes())],
        ));
        assert_eq!(meta.tempo, Some(92.0));
    }

    #[test]
    fn a_fractional_tempo_should_survive() {
        // The thousand-fold scale exists precisely so this is representable;
        // rounding it to an integer would quietly change someone's project.
        let meta = extract(&project(
            1,
            vec![Event::new(EVENT_TEMPO, 174_500u32.to_le_bytes())],
        ));
        assert_eq!(meta.tempo, Some(174.5));
        assert!(
            meta.headline().starts_with("174.5 BPM"),
            "{}",
            meta.headline()
        );
    }

    #[test]
    fn a_whole_tempo_should_not_be_shown_with_a_decimal_point() {
        let meta = extract(&project(
            1,
            vec![Event::new(EVENT_TEMPO, 150_000u32.to_le_bytes())],
        ));
        assert!(
            meta.headline().starts_with("150 BPM"),
            "{}",
            meta.headline()
        );
    }

    #[test]
    fn half_a_time_signature_should_be_reported_as_none() {
        let meta = extract(&project(1, vec![Event::new(EVENT_TIME_SIG_NUMERATOR, [4])]));
        assert_eq!(meta.time_signature, None, "'4/' is not a time signature");

        let both = extract(&project(
            1,
            vec![
                Event::new(EVENT_TIME_SIG_NUMERATOR, [3]),
                Event::new(EVENT_TIME_SIG_DENOMINATOR, [4]),
            ],
        ));
        assert_eq!(both.time_signature, Some((3, 4)));
    }

    #[test]
    fn a_zero_denominator_should_not_become_a_time_signature() {
        let meta = extract(&project(
            1,
            vec![
                Event::new(EVENT_TIME_SIG_NUMERATOR, [4]),
                Event::new(EVENT_TIME_SIG_DENOMINATOR, [0]),
            ],
        ));
        assert_eq!(meta.time_signature, None);
    }

    #[test]
    fn credits_and_names_should_be_read() {
        let meta = extract(&project(
            40,
            vec![
                Event::new(EVENT_TITLE, utf16("Cymatics - Hentai Dolly")),
                Event::new(EVENT_AUTHOR, utf16("Cymatics")),
                Event::new(EVENT_GENRE, utf16("Dubstep")),
                Event::new(EVENT_URL, utf16("cymatics.fm")),
                Event::new(EVENT_MARKER_NAME, utf16("Intro")),
                Event::new(EVENT_MARKER_NAME, utf16("Drop 1. 1")),
                Event::new(EVENT_INSERT_NAME, utf16("Master")),
                Event::new(EVENT_PATTERN_NAME, utf16("Drums")),
            ],
        ));
        assert_eq!(meta.title.as_deref(), Some("Cymatics - Hentai Dolly"));
        assert_eq!(meta.author.as_deref(), Some("Cymatics"));
        assert_eq!(meta.genre.as_deref(), Some("Dubstep"));
        assert_eq!(meta.markers, ["Intro", "Drop 1. 1"]);
        assert_eq!(meta.insert_names, ["Master"]);
        assert_eq!(meta.pattern_names, ["Drums"]);
    }

    #[test]
    fn a_blank_field_should_be_absent_rather_than_an_empty_string() {
        // So the UI can decide not to show a row at all, instead of showing a
        // label with nothing after it.
        let meta = extract(&project(1, vec![Event::new(EVENT_TITLE, utf16(""))]));
        assert_eq!(meta.title, None);
    }

    #[test]
    fn assets_should_be_counted_as_distinct_files() {
        let meta = extract(&project(
            3,
            vec![
                Event::new(refs::EVENT_SAMPLE_PATH, utf16(r"D:\Packs\Kick.wav")),
                Event::new(refs::EVENT_SAMPLE_PATH, utf16(r"D:\Packs\Kick.wav")),
                Event::new(
                    refs::EVENT_SAMPLE_PATH,
                    utf16(r"%FLStudioData%\Patches\Chords.wav"),
                ),
            ],
        ));
        assert_eq!(meta.assets.total, 2, "the same sample twice is one file");
        assert_eq!(meta.assets.external, 1);
        assert_eq!(meta.assets.user_data, 1);
        assert_eq!(meta.assets.vendored(), 2);
    }

    #[test]
    fn the_headline_should_read_as_a_sentence_a_musician_recognises() {
        let meta = extract(&project(
            40,
            vec![
                Event::new(EVENT_TEMPO, 150_000u32.to_le_bytes()),
                Event::new(EVENT_TIME_SIG_NUMERATOR, [4]),
                Event::new(EVENT_TIME_SIG_DENOMINATOR, [4]),
            ],
        ));
        assert_eq!(meta.headline(), "150 BPM · 4/4 · 40 channels");
    }

    #[test]
    fn a_project_with_nothing_recorded_should_still_describe_itself() {
        let meta = extract(&project(1, vec![]));
        assert_eq!(meta.headline(), "1 channel");
    }
}
