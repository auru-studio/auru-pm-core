use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// 32-byte blake3 content hash.
///
/// Display, [`FromStr`], and serde all use the canonical `blake3:<hex>`
/// form (64 hex chars, lowercase). The JSON form is identical across
/// providers — never raw bytes — so HTTP wire payloads and the
/// on-disk sidecar share the same notation.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
pub struct ContentHash([u8; 32]);

impl ContentHash {
    pub const ZERO: Self = ContentHash([0u8; 32]);

    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        ContentHash(bytes)
    }

    /// Hash `data` with blake3 and return the result.
    pub fn of(data: &[u8]) -> Self {
        ContentHash(*blake3::hash(data).as_bytes())
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

impl fmt::Display for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("blake3:")?;
        for b in &self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for ContentHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Same string as Display so debug printing matches what users see.
        fmt::Display::fmt(self, f)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ParseHashError {
    #[error("hash must be prefixed `blake3:`")]
    BadPrefix,
    #[error("hash hex section must be 64 chars, got {0}")]
    BadLength(usize),
    #[error("non-hex character in hash")]
    BadHex,
}

impl FromStr for ContentHash {
    type Err = ParseHashError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let hex = s.strip_prefix("blake3:").ok_or(ParseHashError::BadPrefix)?;
        if hex.len() != 64 {
            return Err(ParseHashError::BadLength(hex.len()));
        }
        let mut out = [0u8; 32];
        for (i, pair) in hex.as_bytes().chunks(2).enumerate() {
            let s = std::str::from_utf8(pair).map_err(|_| ParseHashError::BadHex)?;
            out[i] = u8::from_str_radix(s, 16).map_err(|_| ParseHashError::BadHex)?;
        }
        Ok(ContentHash(out))
    }
}

impl Serialize for ContentHash {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ContentHash {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        ContentHash::from_str(&s).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_deterministic() {
        let a = ContentHash::of(b"hello");
        let b = ContentHash::of(b"hello");
        assert_eq!(a, b);
        assert_ne!(a, ContentHash::of(b"world"));
    }

    #[test]
    fn display_parse_roundtrip() {
        let h = ContentHash::of(b"auru-pm");
        let s = h.to_string();
        assert!(s.starts_with("blake3:"));
        assert_eq!(s.len(), "blake3:".len() + 64);
        let parsed: ContentHash = s.parse().expect("roundtrip parse");
        assert_eq!(h, parsed);
    }

    #[test]
    fn rejects_bad_prefix() {
        let err: ParseHashError = "sha256:deadbeef".parse::<ContentHash>().unwrap_err();
        assert_eq!(err, ParseHashError::BadPrefix);
    }

    #[test]
    fn rejects_short_hex() {
        let err: ParseHashError = "blake3:abcd".parse::<ContentHash>().unwrap_err();
        assert!(matches!(err, ParseHashError::BadLength(4)));
    }

    #[test]
    fn rejects_non_hex() {
        let mut bad = String::from("blake3:");
        bad.push_str(&"z".repeat(64));
        let err: ParseHashError = bad.parse::<ContentHash>().unwrap_err();
        assert_eq!(err, ParseHashError::BadHex);
    }

    #[test]
    fn json_uses_canonical_form() {
        let h = ContentHash::of(b"x");
        let json = serde_json::to_string(&h).unwrap();
        // Quoted canonical form.
        assert_eq!(json, format!("\"{h}\""));
        let back: ContentHash = serde_json::from_str(&json).unwrap();
        assert_eq!(h, back);
    }
}
