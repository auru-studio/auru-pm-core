//! The `.flp` container and its event stream.
//!
//! An FL Studio project is not markup. It is two chunks — `FLhd` holding a
//! handful of fixed fields, and `FLdt` holding a flat stream of events — where
//! each event is one identifier byte followed by a payload whose *length is
//! implied by the identifier's numeric range*:
//!
//! | id | payload |
//! |---|---|
//! | `0..64` | 1 byte |
//! | `64..128` | 2 bytes, little-endian |
//! | `128..192` | 4 bytes, little-endian |
//! | `192..=255` | a 7-bit varint length, then that many bytes |
//!
//! There is no length prefix on the stream and no way to skip an event without
//! understanding that table, so a single misread byte desynchronises
//! everything after it. That is why this module's contract is byte-exactness
//! rather than "close enough": [`Stream::decode`] followed by
//! [`Stream::encode`] reproduces the input exactly, and the tests hold it to
//! that against every identifier band.

use std::fmt;

use crate::error::{Error, Result};

/// `FLhd`, the header chunk.
const HEADER_MAGIC: &[u8; 4] = b"FLhd";
/// `FLdt`, the event-stream chunk.
const DATA_MAGIC: &[u8; 4] = b"FLdt";
/// The only header length FL has ever written: format, channels, ppq.
const HEADER_LEN: u32 = 6;

/// Upper bound on a project we will parse.
///
/// Generous on purpose — a real project sampled during design was 18 MB, and
/// a sample-heavy set can be larger still. The limit exists to stop a
/// malformed length field from turning into an enormous allocation, not to
/// second-guess how big someone's music is.
pub const MAX_FLP_BYTES: u64 = 512 * 1024 * 1024;

/// Upper bound on how many events we will decode.
///
/// A corrupt stream can otherwise loop out one-byte events for as long as the
/// buffer lasts.
pub const MAX_EVENTS: usize = 4_000_000;

/// The identifier of the event carrying the FL Studio version string.
///
/// Special because it is **always ASCII**, even in projects whose every other
/// text event is UTF-16 — a reader has to know the version before it can know
/// the encoding, so this one event cannot depend on it. Confirmed against real
/// projects written by FL 12.2 and FL 20.5.
pub const EVENT_VERSION: u8 = 199;

/// The first identifier whose payload is variable-length.
pub const FIRST_VARIABLE_EVENT: u8 = 192;

/// Fixed fields at the top of every `.flp`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    /// Project type. `0` is a normal song; other values are FL's internal
    /// score/pattern formats, which we read but do not claim to understand.
    pub format: i16,
    /// The channel-rack channel count FL recorded when saving.
    pub channels: u16,
    /// Pulses per quarter note — the tick resolution every position in the
    /// file is expressed in.
    pub ppq: u16,
}

/// One event, exactly as it appeared in the stream.
///
/// `payload` is the raw bytes with no interpretation whatsoever: text is still
/// encoded, numbers are still little-endian, plugin state is still opaque.
/// Interpretation belongs to the layers above, so that a value this module
/// cannot make sense of still survives a round trip untouched.
#[derive(Clone, PartialEq, Eq)]
pub struct Event {
    pub id: u8,
    pub payload: Vec<u8>,
}

impl Event {
    pub fn new(id: u8, payload: impl Into<Vec<u8>>) -> Self {
        Self {
            id,
            payload: payload.into(),
        }
    }

    /// How many payload bytes this identifier requires, or `None` when the
    /// identifier is variable-length and carries its own count.
    pub const fn fixed_len(id: u8) -> Option<usize> {
        match id {
            0..=63 => Some(1),
            64..=127 => Some(2),
            128..=191 => Some(4),
            _ => None,
        }
    }

    pub const fn is_variable(&self) -> bool {
        self.id >= FIRST_VARIABLE_EVENT
    }

    /// The payload read as a little-endian unsigned integer.
    ///
    /// `None` for variable-length events, whose payload is not a number.
    pub fn as_u32(&self) -> Option<u32> {
        if self.is_variable() {
            return None;
        }
        let mut bytes = [0u8; 4];
        bytes[..self.payload.len()].copy_from_slice(&self.payload);
        Some(u32::from_le_bytes(bytes))
    }
}

// Payloads are routinely kilobytes of plugin state; printing them in full
// turns any debug output into noise.
impl fmt::Debug for Event {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Event")
            .field("id", &self.id)
            .field("len", &self.payload.len())
            .finish()
    }
}

/// A whole `.flp`: its header and every event, in order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stream {
    pub header: Header,
    pub events: Vec<Event>,
}

impl Stream {
    /// Read a `.flp`.
    ///
    /// Rejects anything that is not recognisably one rather than guessing:
    /// a desynchronised parse of a truncated file would produce plausible
    /// nonsense, and committing that would be worse than refusing it.
    pub fn decode(source: &[u8]) -> Result<Self> {
        if source.len() as u64 > MAX_FLP_BYTES {
            return Err(Error::ProjectFormat(format!(
                "FL Studio project exceeds the {} MiB limit",
                MAX_FLP_BYTES / (1024 * 1024)
            )));
        }

        let mut reader = Reader::new(source);
        if reader.take(4)? != HEADER_MAGIC {
            return Err(Error::ProjectFormat(
                "not an FL Studio project: missing the FLhd header".to_owned(),
            ));
        }

        let header_len = reader.take_u32("FLhd length")?;
        if header_len != HEADER_LEN {
            return Err(Error::ProjectFormat(format!(
                "unsupported FLhd header length {header_len}, expected {HEADER_LEN}"
            )));
        }
        let header = Header {
            format: reader.take_u16("format")? as i16,
            channels: reader.take_u16("channel count")?,
            ppq: reader.take_u16("ppq")?,
        };

        if reader.take(4)? != DATA_MAGIC {
            return Err(Error::ProjectFormat(
                "FL Studio project has no FLdt event chunk".to_owned(),
            ));
        }
        let data_len = reader.take_u32("FLdt length")? as usize;
        let body = reader.take(data_len)?;

        Ok(Self {
            header,
            events: decode_events(body)?,
        })
    }

    /// Write a `.flp`.
    ///
    /// The inverse of [`Self::decode`] byte for byte, which is what lets a
    /// restored project be compared against the original rather than merely
    /// opened and eyeballed.
    pub fn encode(&self) -> Vec<u8> {
        let mut body = Vec::new();
        for event in &self.events {
            body.push(event.id);
            if event.is_variable() {
                write_varint(&mut body, event.payload.len());
            }
            body.extend_from_slice(&event.payload);
        }

        let mut out = Vec::with_capacity(body.len() + 22);
        out.extend_from_slice(HEADER_MAGIC);
        out.extend_from_slice(&HEADER_LEN.to_le_bytes());
        out.extend_from_slice(&self.header.format.to_le_bytes());
        out.extend_from_slice(&self.header.channels.to_le_bytes());
        out.extend_from_slice(&self.header.ppq.to_le_bytes());
        out.extend_from_slice(DATA_MAGIC);
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend_from_slice(&body);
        out
    }

    /// The FL Studio version string, if the project records one.
    pub fn version(&self) -> Option<String> {
        let event = self.events.iter().find(|event| event.id == EVENT_VERSION)?;
        Some(decode_ascii(&event.payload))
    }

    /// The major version number, used to pick the text encoding.
    pub fn major_version(&self) -> Option<u32> {
        self.version()?.split('.').next()?.parse().ok()
    }
}

/// Whether text events in a project of this version are UTF-16.
///
/// FL switched its text events from single-byte to UTF-16 at version 12.
/// Guessing wrong does not fail loudly — it yields mojibake, or a sample path
/// that no longer resolves — so the version is consulted rather than the bytes
/// sniffed. Unknown versions are treated as UTF-16, which is every FL release
/// since 2015.
pub fn uses_utf16(major_version: Option<u32>) -> bool {
    major_version.is_none_or(|major| major >= 12)
}

/// A cursor that refuses to read past the end.
///
/// The chunk headers are a fixed sequence of small reads, and every one of
/// them is a place a truncated file could otherwise panic on a slice index.
struct Reader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .ok_or_else(|| Error::ProjectFormat("FL Studio chunk length overflows".to_owned()))?;
        if end > self.bytes.len() {
            return Err(Error::ProjectFormat(
                "FL Studio project ends mid-chunk".to_owned(),
            ));
        }
        let slice = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(slice)
    }

    fn take_u16(&mut self, what: &str) -> Result<u16> {
        let bytes = self.take(2).map_err(|_| {
            Error::ProjectFormat(format!("FL Studio project ends before its {what}"))
        })?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn take_u32(&mut self, what: &str) -> Result<u32> {
        let bytes = self.take(4).map_err(|_| {
            Error::ProjectFormat(format!("FL Studio project ends before its {what}"))
        })?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

fn decode_events(mut body: &[u8]) -> Result<Vec<Event>> {
    let mut events = Vec::new();
    while !body.is_empty() {
        if events.len() >= MAX_EVENTS {
            return Err(Error::ProjectFormat(format!(
                "FL Studio project has more than {MAX_EVENTS} events"
            )));
        }

        let id = body[0];
        body = &body[1..];

        let len = match Event::fixed_len(id) {
            Some(len) => len,
            None => {
                let (len, rest) = read_varint(body)?;
                body = rest;
                len
            }
        };

        if body.len() < len {
            return Err(Error::ProjectFormat(format!(
                "FL Studio event {id} claims {len} bytes but only {} remain",
                body.len()
            )));
        }
        events.push(Event::new(id, &body[..len]));
        body = &body[len..];
    }
    Ok(events)
}

/// Read FL's 7-bits-per-byte little-endian length prefix.
fn read_varint(bytes: &[u8]) -> Result<(usize, &[u8])> {
    let mut value: usize = 0;
    let mut shift = 0;
    for (index, byte) in bytes.iter().enumerate() {
        // Five groups of seven bits covers a 32-bit length; more than that is
        // a corrupt stream rather than a very large event.
        if shift > 28 {
            return Err(Error::ProjectFormat(
                "FL Studio event length is not a valid varint".to_owned(),
            ));
        }
        value |= usize::from(byte & 0x7f) << shift;
        if byte & 0x80 == 0 {
            return Ok((value, &bytes[index + 1..]));
        }
        shift += 7;
    }
    Err(Error::ProjectFormat(
        "FL Studio event length ran past the end of the file".to_owned(),
    ))
}

fn write_varint(out: &mut Vec<u8>, mut value: usize) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        // The high bit means "another byte follows".
        out.push(if value == 0 { byte } else { byte | 0x80 });
        if value == 0 {
            return;
        }
    }
}

/// Decode a single-byte-per-character payload, dropping the terminator.
pub fn decode_ascii(payload: &[u8]) -> String {
    let end = payload
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(payload.len());
    payload[..end].iter().map(|byte| *byte as char).collect()
}

/// Decode a UTF-16LE payload, dropping the terminator.
///
/// Lone surrogates are replaced rather than rejected: a mangled project name
/// is not a reason to refuse to back up someone's music.
pub fn decode_utf16(payload: &[u8]) -> String {
    let units: Vec<u16> = payload
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|unit| *unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a `.flp` from events, so tests describe intent rather than bytes.
    pub(crate) fn flp(events: &[Event]) -> Vec<u8> {
        Stream {
            header: Header {
                format: 0,
                channels: 2,
                ppq: 96,
            },
            events: events.to_vec(),
        }
        .encode()
    }

    #[test]
    fn round_trip_should_be_byte_exact_across_every_id_band() {
        // The four bands are the whole parsing contract: get one boundary
        // wrong and every event after it is read at the wrong offset.
        let source = flp(&[
            Event::new(0, [1]),                    // 1-byte band
            Event::new(63, [255]),                 // last of the 1-byte band
            Event::new(64, [2, 0]),                // 2-byte band
            Event::new(127, [255, 255]),           // last of the 2-byte band
            Event::new(128, [3, 0, 0, 0]),         // 4-byte band
            Event::new(191, [255, 255, 255, 255]), // last of the 4-byte band
            Event::new(192, b"variable".to_vec()), // first variable
            Event::new(255, Vec::new()),           // empty variable
        ]);

        let decoded = Stream::decode(&source).expect("decode");
        assert_eq!(decoded.encode(), source);
        assert_eq!(decoded.events.len(), 8);
    }

    #[test]
    fn a_payload_longer_than_one_varint_byte_should_round_trip() {
        // 127 is the largest length a single varint byte can express, so 128
        // is where a wrong continuation bit first shows up.
        for len in [126usize, 127, 128, 129, 16_383, 16_384] {
            let source = flp(&[Event::new(200, vec![7u8; len])]);
            let decoded = Stream::decode(&source).expect("decode");
            assert_eq!(decoded.events[0].payload.len(), len, "at length {len}");
            assert_eq!(decoded.encode(), source, "at length {len}");
        }
    }

    #[test]
    fn the_header_should_survive_unchanged() {
        let stream = Stream {
            header: Header {
                format: 0,
                channels: 40,
                ppq: 96,
            },
            events: vec![Event::new(EVENT_VERSION, b"20.5.0.1142\0".to_vec())],
        };
        let decoded = Stream::decode(&stream.encode()).expect("decode");
        assert_eq!(decoded.header, stream.header);
        assert_eq!(decoded.version().as_deref(), Some("20.5.0.1142"));
        assert_eq!(decoded.major_version(), Some(20));
    }

    #[test]
    fn the_version_event_should_be_read_as_ascii_not_utf16() {
        // The trap: a reader cannot know the text encoding until it has read
        // the version, so this one event is always single-byte. Decoding it as
        // UTF-16 yields CJK mojibake instead of a version number, and the
        // failure is silent.
        let payload = b"20.5.0.1142\0".to_vec();
        assert_eq!(decode_ascii(&payload), "20.5.0.1142");
        assert_ne!(decode_utf16(&payload), "20.5.0.1142");
    }

    #[test]
    fn text_encoding_should_follow_the_major_version() {
        assert!(!uses_utf16(Some(11)), "FL 11 and earlier wrote single-byte");
        assert!(uses_utf16(Some(12)));
        assert!(uses_utf16(Some(20)));
        assert!(uses_utf16(None), "an unknown version is a modern one");
    }

    #[test]
    fn text_decoding_should_stop_at_the_terminator() {
        let mut payload = Vec::new();
        for unit in "Snare".encode_utf16() {
            payload.extend_from_slice(&unit.to_le_bytes());
        }
        payload.extend_from_slice(&[0, 0]);
        payload.extend_from_slice(&[65, 0]); // junk past the terminator
        assert_eq!(decode_utf16(&payload), "Snare");
    }

    #[test]
    fn numbers_should_read_little_endian_at_their_declared_width() {
        assert_eq!(Event::new(0, [7]).as_u32(), Some(7));
        assert_eq!(Event::new(64, [0x10, 0x27]).as_u32(), Some(10_000));
        // Tempo, as FL stores it: beats per minute times a thousand.
        assert_eq!(
            Event::new(156, 150_000u32.to_le_bytes()).as_u32(),
            Some(150_000)
        );
        assert_eq!(Event::new(200, b"text".to_vec()).as_u32(), None);
    }

    #[test]
    fn a_file_that_is_not_a_project_should_be_refused_not_guessed_at() {
        // Reading on past a bad magic would desynchronise and produce
        // plausible-looking nonsense, which is worse than a clear failure.
        assert!(Stream::decode(b"").is_err());
        assert!(Stream::decode(b"PK\x03\x04 this is a zip").is_err());
        assert!(Stream::decode(b"FLhd").is_err(), "header without a length");
    }

    #[test]
    fn a_truncated_event_should_be_refused() {
        let mut source = flp(&[Event::new(200, vec![1u8; 64])]);
        source.truncate(source.len() - 10);
        assert!(Stream::decode(&source).is_err());
    }

    #[test]
    fn a_never_ending_varint_should_not_hang_or_overflow() {
        // Every byte having its continuation bit set is the shape a corrupt
        // length takes; it must fail rather than shift forever.
        let mut source = flp(&[]);
        source.extend_from_slice(&[200]);
        source.extend_from_slice(&[0xff; 12]);
        let len = source.len();
        // Fix up the FLdt length so only the varint itself is malformed.
        let body_len = (len - 22) as u32;
        source[18..22].copy_from_slice(&body_len.to_le_bytes());
        assert!(Stream::decode(&source).is_err());
    }
}
