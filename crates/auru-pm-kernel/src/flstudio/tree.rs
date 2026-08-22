//! The event stream as a tree the rest of the crate already knows how to
//! store, merge and diff.
//!
//! [`crate::project_format::PortableSnapshot`] carries an [`XmlDocument`], and
//! every format Auru supports normalises into one. That type is not really
//! about XML: it is a generic tree of `{ tag, id, attributes, children }`
//! where `id` exists solely to give the JSON array merge a stable identity.
//! An FL Studio event stream maps onto it directly, which means `.flp` support
//! costs nothing in the commit hash, the canonical encoding, the wire
//! protocol, or the golden fixtures — all of which generalising the snapshot
//! would have disturbed.
//!
//! Events are grouped under the cursors FL itself uses, so that identity
//! survives insertion: renaming channel 7 must not look like an edit to every
//! channel after it. Two grouping rules hold in every project examined:
//!
//! - event 64 opens a channel, and its value *is* the channel's identity
//! - event 65 opens a pattern, likewise
//!
//! Mixer inserts get no group. FL does not delimit them with a cursor event —
//! in a real project the entire mixer arrives as one 56 KB blob under event
//! 225 — so inventing a boundary would be guesswork that merge would then act
//! on. They stay in document order instead, which is coarser to merge but
//! never wrong. See [`crate::flstudio::events`] for the container itself.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;

use crate::error::{Error, Result};
use crate::project_format::{XmlContent, XmlDocument, XmlElement};

use super::events::{Event, Header, Stream, decode_ascii, decode_utf16, uses_utf16};

/// Root element tag.
const ROOT: &str = "FLProject";
/// Tag for a single event.
const EVENT: &str = "E";

/// Opens a channel; its payload is the channel's identity.
pub const EVENT_NEW_CHANNEL: u8 = 64;
/// Opens a pattern; its payload is the pattern's identity.
pub const EVENT_NEW_PATTERN: u8 = 65;

/// Events whose payload is text worth showing a person.
///
/// Restricted to the ones a user actually edits — names, paths, credits —
/// because a readable value is what makes a diff mean something. Everything
/// else stays opaque bytes; guessing that a payload is text when it is really
/// plugin state would produce a diff full of mojibake.
const TEXT_EVENTS: &[u8] = &[
    193, // pattern name
    194, // project title
    195, // comment
    196, // sample path
    197, // project URL
    199, // version (always ASCII — see events::EVENT_VERSION)
    200, // registration name
    201, // internal plugin name
    202, // project data folder
    203, // plugin display name
    204, // mixer insert name
    205, // arrangement marker name
    206, // genre
    207, // author
];

/// Convert a decoded `.flp` into the tree the snapshot carries.
pub(crate) fn to_document(stream: &Stream) -> XmlDocument {
    let utf16 = uses_utf16(stream.major_version());

    let mut root = XmlElement {
        tag: ROOT.to_owned(),
        id: None,
        attributes: BTreeMap::from([
            ("Format".to_owned(), stream.header.format.to_string()),
            ("Channels".to_owned(), stream.header.channels.to_string()),
            ("Ppq".to_owned(), stream.header.ppq.to_string()),
        ]),
        children: Vec::new(),
    };

    // Events before the first cursor belong to the project as a whole.
    let mut section = group("Preamble", None);
    for event in &stream.events {
        match event.id {
            EVENT_NEW_CHANNEL | EVENT_NEW_PATTERN => {
                root.children.push(XmlContent::Element(section));
                let tag = if event.id == EVENT_NEW_CHANNEL {
                    "Channel"
                } else {
                    "Pattern"
                };
                // The cursor's own value is the group's identity, and the
                // cursor event stays inside the group so flattening restores
                // the original order without needing to re-synthesise it.
                let identity = event.as_u32().unwrap_or_default().to_string();
                section = group(tag, Some(identity));
            }
            _ => {}
        }
        section
            .children
            .push(XmlContent::Element(encode_event(event, utf16)));
    }
    root.children.push(XmlContent::Element(section));

    XmlDocument { root }
}

/// Convert the tree back into a `.flp`.
///
/// Exactly inverse to [`to_document`]: the groups are flattened in order and
/// contribute nothing of their own, so what comes out is the event list that
/// went in.
pub(crate) fn from_document(document: &XmlDocument) -> Result<Stream> {
    let root = &document.root;
    if root.tag != ROOT {
        return Err(Error::ProjectFormat(format!(
            "expected an {ROOT} snapshot, found <{}>",
            root.tag
        )));
    }

    let header = Header {
        format: parse_attribute(root, "Format")?,
        channels: parse_attribute(root, "Channels")?,
        ppq: parse_attribute(root, "Ppq")?,
    };

    let mut events = Vec::new();
    for group in root.children.iter().filter_map(element) {
        for element in group.children.iter().filter_map(element) {
            events.push(decode_event(element)?);
        }
    }
    Ok(Stream { header, events })
}

fn group(tag: &str, identity: Option<String>) -> XmlElement {
    let mut attributes = BTreeMap::new();
    if let Some(identity) = &identity {
        attributes.insert("Id".to_owned(), identity.clone());
    }
    XmlElement {
        tag: tag.to_owned(),
        id: identity,
        attributes,
        children: Vec::new(),
    }
}

fn element(content: &XmlContent) -> Option<&XmlElement> {
    match content {
        XmlContent::Element(element) => Some(element),
        _ => None,
    }
}

/// Render one event.
///
/// Text is preferred where it is readable, but only after checking that
/// re-encoding the decoded string reproduces the original bytes exactly. A
/// payload that merely *looks* like text — one carrying an embedded NUL, or a
/// lone surrogate — silently loses data when decoded, so it falls back to
/// base64 rather than being trusted. Round-tripping is therefore a property of
/// the encoder, not something the tests have to hope for.
fn encode_event(event: &Event, utf16: bool) -> XmlElement {
    let mut attributes = BTreeMap::from([("T".to_owned(), event.id.to_string())]);

    if !event.is_variable() {
        // Fixed-width numbers: the width is implied by the identifier band, so
        // only the value needs recording.
        attributes.insert(
            "V".to_owned(),
            event.as_u32().unwrap_or_default().to_string(),
        );
    } else if let Some((text, encoding)) = readable_text(event, utf16) {
        attributes.insert("V".to_owned(), text);
        attributes.insert("Enc".to_owned(), encoding.to_owned());
    } else if !event.payload.is_empty() {
        attributes.insert("B".to_owned(), BASE64.encode(&event.payload));
    }

    XmlElement {
        tag: EVENT.to_owned(),
        id: None,
        attributes,
        children: Vec::new(),
    }
}

/// The decoded text of an event, if decoding it is lossless.
fn readable_text(event: &Event, utf16: bool) -> Option<(String, &'static str)> {
    if !TEXT_EVENTS.contains(&event.id) {
        return None;
    }
    // The version event predates knowing the encoding, so it is always ASCII.
    let ascii = event.id == super::events::EVENT_VERSION || !utf16;
    let (text, encoding) = if ascii {
        (decode_ascii(&event.payload), "ascii")
    } else {
        (decode_utf16(&event.payload), "utf16")
    };

    (encode_text(&text, encoding) == event.payload).then_some((text, encoding))
}

/// Re-encode text the way FL wrote it, terminator included.
fn encode_text(text: &str, encoding: &str) -> Vec<u8> {
    let mut bytes = Vec::new();
    if encoding == "ascii" {
        bytes.extend(text.chars().map(|character| character as u8));
        bytes.push(0);
    } else {
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    }
    bytes
}

fn decode_event(element: &XmlElement) -> Result<Event> {
    if element.tag != EVENT {
        return Err(Error::ProjectFormat(format!(
            "unexpected <{}> in an FL Studio snapshot",
            element.tag
        )));
    }
    let id: u8 = parse_attribute(element, "T")?;

    if let Some(width) = Event::fixed_len(id) {
        let value: u32 = parse_attribute(element, "V")?;
        return Ok(Event::new(id, &value.to_le_bytes()[..width]));
    }

    if let Some(encoding) = element.attributes.get("Enc") {
        let text = element.attributes.get("V").map_or("", String::as_str);
        return Ok(Event::new(id, encode_text(text, encoding)));
    }

    match element.attributes.get("B") {
        Some(encoded) => {
            let payload = BASE64.decode(encoded).map_err(|error| {
                Error::ProjectFormat(format!("FL Studio event {id} has invalid payload: {error}"))
            })?;
            Ok(Event::new(id, payload))
        }
        None => Ok(Event::new(id, Vec::new())),
    }
}

fn parse_attribute<T: std::str::FromStr>(element: &XmlElement, name: &str) -> Result<T> {
    let raw = element
        .attributes
        .get(name)
        .ok_or_else(|| Error::ProjectFormat(format!("<{}> is missing its {name}", element.tag)))?;
    raw.parse().map_err(|_| {
        Error::ProjectFormat(format!("<{}> has an unreadable {name}: {raw}", element.tag))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flstudio::events::EVENT_VERSION;

    fn stream(events: Vec<Event>) -> Stream {
        Stream {
            header: Header {
                format: 0,
                channels: 2,
                ppq: 96,
            },
            events,
        }
    }

    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    #[test]
    fn the_tree_should_round_trip_back_to_the_same_events() {
        let original = stream(vec![
            Event::new(EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
            Event::new(156, 92_000u32.to_le_bytes()),
            Event::new(EVENT_NEW_CHANNEL, 0u16.to_le_bytes()),
            Event::new(203, utf16("Snare")),
            Event::new(213, vec![0xde, 0xad, 0xbe, 0xef]),
            Event::new(EVENT_NEW_CHANNEL, 1u16.to_le_bytes()),
            Event::new(203, utf16("Kick")),
        ]);

        let rebuilt = from_document(&to_document(&original)).expect("from tree");
        assert_eq!(rebuilt, original);
        // And through the bytes, which is what actually reaches disk.
        assert_eq!(rebuilt.encode(), original.encode());
    }

    #[test]
    fn channels_should_become_groups_keyed_on_their_own_number() {
        let document = to_document(&stream(vec![
            Event::new(EVENT_VERSION, b"20.0.0.0\0".to_vec()),
            Event::new(EVENT_NEW_CHANNEL, 7u16.to_le_bytes()),
            Event::new(203, utf16("Snare")),
            Event::new(EVENT_NEW_PATTERN, 3u16.to_le_bytes()),
            Event::new(193, utf16("Drums")),
        ]));

        let groups: Vec<(&str, Option<&str>)> = document
            .root
            .children
            .iter()
            .filter_map(element)
            .map(|group| (group.tag.as_str(), group.id.as_deref()))
            .collect();
        assert_eq!(
            groups,
            [
                ("Preamble", None),
                ("Channel", Some("7")),
                ("Pattern", Some("3")),
            ]
        );
    }

    #[test]
    fn inserting_a_channel_should_not_disturb_the_identity_of_the_others() {
        // The whole reason grouping keys on the cursor's value rather than a
        // running count: with a counter, adding a channel renumbers every one
        // after it and the merge sees the entire project as rewritten.
        let before = to_document(&stream(vec![
            Event::new(EVENT_NEW_CHANNEL, 0u16.to_le_bytes()),
            Event::new(EVENT_NEW_CHANNEL, 5u16.to_le_bytes()),
        ]));
        let after = to_document(&stream(vec![
            Event::new(EVENT_NEW_CHANNEL, 0u16.to_le_bytes()),
            Event::new(EVENT_NEW_CHANNEL, 3u16.to_le_bytes()),
            Event::new(EVENT_NEW_CHANNEL, 5u16.to_le_bytes()),
        ]));

        let ids = |document: &XmlDocument| -> Vec<String> {
            document
                .root
                .children
                .iter()
                .filter_map(element)
                .filter_map(|group| group.id.clone())
                .collect()
        };
        assert_eq!(ids(&before), ["0", "5"]);
        assert_eq!(ids(&after), ["0", "3", "5"]);
    }

    #[test]
    fn readable_text_should_appear_as_text_not_base64() {
        let document = to_document(&stream(vec![Event::new(196, utf16("D:\\Packs\\Kick.wav"))]));
        let event = &document.root.children[0];
        let XmlContent::Element(preamble) = event else {
            panic!("expected a group");
        };
        let XmlContent::Element(event) = &preamble.children[0] else {
            panic!("expected an event");
        };
        assert_eq!(
            event.attributes.get("V").map(String::as_str),
            Some("D:\\Packs\\Kick.wav")
        );
        assert_eq!(
            event.attributes.get("Enc").map(String::as_str),
            Some("utf16")
        );
        assert!(!event.attributes.contains_key("B"), "should not be opaque");
    }

    #[test]
    fn a_text_event_that_cannot_be_decoded_losslessly_should_stay_opaque() {
        // An embedded NUL truncates on decode. Storing the truncated string
        // would silently drop the rest of the payload, so such an event has to
        // fall back to bytes — checked by re-encoding, not by guesswork.
        let mut payload = utf16("first");
        payload.extend_from_slice(&utf16("second"));

        let document = to_document(&stream(vec![Event::new(196, payload.clone())]));
        let XmlContent::Element(preamble) = &document.root.children[0] else {
            panic!("expected a group");
        };
        let XmlContent::Element(event) = &preamble.children[0] else {
            panic!("expected an event");
        };
        assert!(
            event.attributes.contains_key("B"),
            "a lossy decode must fall back to base64"
        );

        let rebuilt = from_document(&document).expect("from tree");
        assert_eq!(rebuilt.events[0].payload, payload, "nothing may be lost");
    }

    #[test]
    fn the_version_event_should_stay_ascii_in_a_utf16_project() {
        let original = stream(vec![
            Event::new(EVENT_VERSION, b"20.5.0.1142\0".to_vec()),
            Event::new(203, utf16("Serum")),
        ]);
        let document = to_document(&original);
        let XmlContent::Element(preamble) = &document.root.children[0] else {
            panic!("expected a group");
        };
        let encodings: Vec<Option<&str>> = preamble
            .children
            .iter()
            .filter_map(element)
            .map(|event| event.attributes.get("Enc").map(String::as_str))
            .collect();
        assert_eq!(encodings, [Some("ascii"), Some("utf16")]);
        assert_eq!(from_document(&document).expect("round trip"), original);
    }

    #[test]
    fn an_older_project_should_use_single_byte_text() {
        let original = stream(vec![
            Event::new(EVENT_VERSION, b"11.0.0.0\0".to_vec()),
            Event::new(203, b"Sytrus\0".to_vec()),
        ]);
        assert_eq!(
            from_document(&to_document(&original)).expect("round trip"),
            original
        );
    }

    #[test]
    fn a_foreign_tree_should_be_refused_rather_than_misread() {
        let document = XmlDocument {
            root: XmlElement {
                tag: "Ableton".to_owned(),
                id: None,
                attributes: BTreeMap::new(),
                children: Vec::new(),
            },
        };
        assert!(from_document(&document).is_err());
    }
}
