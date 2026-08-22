//! Mapping paths recorded on one machine onto the machine reading them.
//!
//! Pure string and path rewriting, so it lives in the kernel: a browser
//! inspecting a Live Set needs to understand a `E:/Music Production/...`
//! reference just as much as the desktop does, even though it will never open
//! the file.

use std::path::PathBuf;

/// Rewrite rule mapping a path prefix recorded in a Live Set onto a local one.
///
/// Live records absolute paths from whichever machine saved the set — a
/// Windows volume such as `E:/Music Production/samples/…`. Opening that
/// project from a Linux or macOS host, the same drive is mounted elsewhere.
/// An alias bridges the two without the user re-linking every sample by hand.
///
/// Matching is case-insensitive on the prefix, because Windows paths are.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PathAlias {
    /// Prefix as written in the Live Set, eg `E:/Music Production`.
    pub from: String,
    /// Local directory it corresponds to.
    pub to: PathBuf,
}

impl PathAlias {
    pub fn new(from: impl Into<String>, to: impl Into<PathBuf>) -> Self {
        Self {
            from: from.into(),
            to: to.into(),
        }
    }

    /// Read aliases from `AURU_PATH_ALIASES`.
    ///
    /// Format is `from=to`; separate multiple mappings with `;`:
    ///
    /// ```text
    /// AURU_PATH_ALIASES='E:/Music Production=/mnt/ssd/Music Production'
    /// ```
    ///
    /// `AURU_ABLETON_PATH_ALIASES` remains a fallback for existing setups.
    /// The neutral name is used first because the same aliases resolve paths
    /// recorded by Ableton, FL Studio, and future DAW adapters.
    pub fn from_environment() -> Vec<Self> {
        let raw = std::env::var("AURU_PATH_ALIASES")
            .or_else(|_| std::env::var("AURU_ABLETON_PATH_ALIASES"));
        let Ok(raw) = raw else {
            return Vec::new();
        };
        parse_path_aliases(&raw)
    }

    /// Apply this alias to `path`, if it matches.
    pub fn apply(&self, path: &str) -> Option<PathBuf> {
        let from = self.from.trim_end_matches(['/', '\\']);
        if path.len() < from.len() || !path[..from.len()].eq_ignore_ascii_case(from) {
            return None;
        }
        let rest = path[from.len()..].trim_start_matches(['/', '\\']);
        Some(self.to.join(to_native_relative(rest)))
    }
}

fn parse_path_aliases(raw: &str) -> Vec<PathAlias> {
    // `:` was the original Unix separator, despite Windows source paths also
    // containing one. Preserve multi-entry values written in that form, but
    // never split a drive prefix before its mapping's `=`.
    let mut entries = Vec::new();
    for semicolon_entry in raw.split(';') {
        let mut start = 0;
        let mut saw_equals = false;
        for (index, character) in semicolon_entry.char_indices() {
            match character {
                '=' => saw_equals = true,
                ':' if saw_equals && semicolon_entry[index + 1..].contains('=') => {
                    entries.push(&semicolon_entry[start..index]);
                    start = index + 1;
                    saw_equals = false;
                }
                _ => {}
            }
        }
        entries.push(&semicolon_entry[start..]);
    }

    entries
        .into_iter()
        .filter_map(|entry| {
            // Split on the first `=`; a Windows prefix contains a colon,
            // so `=` is the only safe separator within an entry.
            let (from, to) = entry.split_once('=')?;
            let (from, to) = (from.trim(), to.trim());
            (!from.is_empty() && !to.is_empty()).then(|| PathAlias::new(from, to))
        })
        .collect()
}

/// Convert an Ableton-style relative path to a native one.
///
/// These always use `/`, even when written on Windows, and `..` segments are
/// resolved lexically by [`normalize`] afterwards.
pub fn to_native_relative(path: &str) -> PathBuf {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_alias_should_match_case_insensitively_and_join_the_remainder() {
        let alias = PathAlias::new("E:/Music Production", "/mnt/ssd/Music");
        assert_eq!(
            alias.apply("e:/MUSIC PRODUCTION/samples/a.wav"),
            Some(PathBuf::from("/mnt/ssd/Music/samples/a.wav"))
        );
        assert_eq!(alias.apply("F:/Other/a.wav"), None);
    }

    #[test]
    fn path_aliases_should_parse_from_the_environment_format() {
        // Parsed directly rather than through the env var, so the test does
        // not depend on process-global state.
        let aliases =
            parse_path_aliases("E:/Music Production=/mnt/ssd/Music Production;D:\\Packs=/packs");
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0].from, "E:/Music Production");
        assert_eq!(aliases[0].to, PathBuf::from("/mnt/ssd/Music Production"));
        assert_eq!(aliases[1].from, "D:\\Packs");
        assert_eq!(aliases[1].to, PathBuf::from("/packs"));
    }

    #[test]
    fn legacy_colon_separated_path_aliases_should_still_parse() {
        let aliases = parse_path_aliases("/old=/new:/another=/elsewhere");
        assert_eq!(aliases.len(), 2);
        assert_eq!(aliases[0], PathAlias::new("/old", "/new"));
        assert_eq!(aliases[1], PathAlias::new("/another", "/elsewhere"));
    }
}
