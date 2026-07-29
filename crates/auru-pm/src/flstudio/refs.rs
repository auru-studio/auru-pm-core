//! What files an FL Studio project depends on.
//!
//! FL records a sample as a single path string (event 196) with no
//! classification field — unlike Ableton, which states outright whether a
//! reference is project-relative, in the user library, or missing. The class
//! has to be inferred from the shape of the path, and it matters, because the
//! four classes need four different things done about them.
//!
//! Paths are Windows-shaped even when they are not absolute Windows paths, and
//! FL substitutes its own variables into them. Both are handled as *string*
//! work: `D:\Packs\Kick.wav` is not a path on this machine and must never be
//! handed to [`std::path`] before it has been translated.

use std::path::{Path, PathBuf};

use crate::ableton::PathAlias;

use super::events::{Stream, decode_ascii, decode_utf16, uses_utf16};

/// Event carrying a sample's path.
pub const EVENT_SAMPLE_PATH: u8 = 196;

/// FL's own path variables, as they appear in a saved project.
///
/// FL writes these rather than absolute paths so a project keeps working when
/// the user folder moves. They have to be expanded before a file can be found,
/// and put back when a path is rewritten, or the project stops being portable
/// in exactly the way the variable existed to guarantee.
pub const FL_DATA_VARIABLE: &str = "%FLStudioData%";
const USER_PROFILE_VARIABLE: &str = "%USERPROFILE%";

/// The marker of a path FL made while unpacking a zip.
///
/// Anything under here is scratch space the operating system is entitled to
/// delete. A project referencing it is already one reboot away from losing
/// that audio.
const TEMP_IMPORT_MARKER: &str = r"\Temp\Image-Line\";

/// What kind of reference this is, and therefore what to do about it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefClass {
    /// Relative to the project — travels with it already.
    ProjectRelative,
    /// Somewhere else on this machine. The common case, and the one that
    /// breaks when a project moves.
    External,
    /// Under FL's own data folder: the user's recordings and patches.
    UserData,
    /// A scratch path from a zip import.
    ///
    /// Called out separately rather than lumped in with [`Self::External`]
    /// because the honest thing to tell someone is not "this is elsewhere" but
    /// "this is already at risk, and a backup now is the only thing that will
    /// save it".
    Fragile,
    /// The project records no path at all.
    Missing,
}

impl RefClass {
    /// Whether the file should be captured into the backup.
    pub const fn should_vendor(self) -> bool {
        matches!(self, Self::External | Self::UserData | Self::Fragile)
    }

    /// Whether a person should be told about this without being asked.
    pub const fn needs_attention(self) -> bool {
        matches!(self, Self::Fragile | Self::Missing)
    }
}

/// One file the project refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRef {
    /// The path exactly as the project records it, variables and all.
    pub recorded_path: String,
    /// The path with FL's variables expanded, still in its original spelling.
    pub expanded_path: String,
    pub class: RefClass,
    /// Index of the event this came from, so a rewrite can find it again
    /// without re-deriving the position.
    pub event_index: usize,
}

impl AssetRef {
    /// What to deduplicate on.
    ///
    /// The same sample loaded onto eight channels is one file to back up, not
    /// eight — counting occurrences would tell someone they are about to
    /// upload eight times what they really are.
    pub fn dedup_key(&self) -> &str {
        &self.expanded_path
    }

    /// The file name alone.
    pub fn file_name(&self) -> &str {
        let path = self.expanded_path.as_str();
        match path.rfind(['/', '\\']) {
            Some(at) => &path[at + 1..],
            None => path,
        }
    }

    /// Where this file is on *this* machine, if it can be worked out.
    ///
    /// `aliases` map a prefix as written into a local directory, which is how
    /// a project saved on Windows resolves against a drive mounted elsewhere.
    /// Returns `None` when the path is not absolute or no alias applies —
    /// unresolvable is reported, never guessed at.
    pub fn local_path(&self, aliases: &[PathAlias]) -> Option<PathBuf> {
        let path = &self.expanded_path;
        for alias in aliases {
            // Compare with separators normalised so an alias written either
            // way matches a path written either way.
            let (candidate, prefix) = (
                normalize_separators(path),
                normalize_separators(&alias.from),
            );
            if let Some(rest) = candidate.strip_prefix(&prefix) {
                let rest = rest.trim_start_matches('/');
                return Some(alias.to.join(rest));
            }
        }

        // No alias: only a path already valid here can be used.
        if path.starts_with('/') {
            return Some(PathBuf::from(path));
        }
        None
    }
}

/// Every file the project refers to, in the order the events appear.
pub fn collect(stream: &Stream) -> Vec<AssetRef> {
    let utf16 = uses_utf16(stream.major_version());
    stream
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.id == EVENT_SAMPLE_PATH)
        .map(|(event_index, event)| {
            let recorded_path = if utf16 {
                decode_utf16(&event.payload)
            } else {
                decode_ascii(&event.payload)
            };
            let expanded_path = expand_variables(&recorded_path);
            AssetRef {
                class: classify(&recorded_path, &expanded_path),
                recorded_path,
                expanded_path,
                event_index,
            }
        })
        .collect()
}

/// Distinct files, keyed on where they point.
pub fn distinct(refs: &[AssetRef]) -> Vec<&AssetRef> {
    let mut seen = std::collections::BTreeSet::new();
    refs.iter()
        .filter(|reference| seen.insert(reference.dedup_key().to_owned()))
        .collect()
}

fn classify(recorded: &str, expanded: &str) -> RefClass {
    if recorded.trim().is_empty() {
        return RefClass::Missing;
    }
    if expanded.contains(TEMP_IMPORT_MARKER) {
        return RefClass::Fragile;
    }
    if recorded.starts_with(FL_DATA_VARIABLE) {
        return RefClass::UserData;
    }
    if is_absolute(expanded) {
        return RefClass::External;
    }
    RefClass::ProjectRelative
}

/// Whether a path is absolute in the spelling FL uses.
///
/// Deliberately not [`Path::is_absolute`]: `D:\Packs\Kick.wav` is absolute in
/// the project and relative to Unix, and asking the local platform would get
/// that backwards on every machine that is not the one the project was made
/// on.
fn is_absolute(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.starts_with(b"/") || bytes.starts_with(b"\\\\") {
        return true;
    }
    // A drive letter, a colon, then a separator.
    matches!(bytes, [drive, b':', b'/' | b'\\', ..] if drive.is_ascii_alphabetic())
}

/// Substitute FL's variables, leaving unknown ones untouched.
///
/// An unrecognised variable is left as written rather than blanked: a path we
/// cannot expand is still worth showing someone, and mangling it would only
/// make the report harder to act on.
fn expand_variables(path: &str) -> String {
    let mut expanded = path.to_owned();
    if let Some(rest) = path.strip_prefix(USER_PROFILE_VARIABLE)
        && let Some(home) = home_directory()
    {
        expanded = format!("{}{rest}", home.display());
    }
    if let Some(rest) = path.strip_prefix(FL_DATA_VARIABLE)
        && let Some(data) = fl_data_directory()
    {
        expanded = format!("{}{rest}", data.display());
    }
    expanded
}

fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// FL's user data folder for this platform.
///
/// Where FL keeps recordings, rendered stems and user patches — the things
/// `%FLStudioData%` points at.
fn fl_data_directory() -> Option<PathBuf> {
    // The same layout on every platform FL ships for — it is a Documents
    // folder rather than an OS-specific application-data location.
    Some(
        home_directory()?
            .join("Documents")
            .join("Image-Line")
            .join("FL Studio"),
    )
}

fn normalize_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// Whether `path` exists, for reporting what can still be captured.
pub fn exists(path: &Path) -> bool {
    path.is_file()
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

    fn project(paths: &[&str]) -> Stream {
        let mut events = vec![Event::new(199, b"20.5.0.1142\0".to_vec())];
        events.extend(
            paths
                .iter()
                .map(|path| Event::new(EVENT_SAMPLE_PATH, utf16(path))),
        );
        Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events,
        }
    }

    #[test]
    fn a_windows_path_should_be_external_even_on_unix() {
        // The trap: asking the local platform whether `D:\...` is absolute
        // says no on every machine that is not Windows, which would file every
        // external sample as project-relative and silently skip backing it up.
        let refs = collect(&project(&[r"D:\Soundpacks\Phenom Drums\Kick.wav"]));
        assert_eq!(refs[0].class, RefClass::External);
        assert!(refs[0].class.should_vendor());
        assert_eq!(refs[0].file_name(), "Kick.wav");
    }

    #[test]
    fn a_zip_import_scratch_path_should_be_called_fragile() {
        // Taken verbatim from a real project. The operating system may delete
        // this at any time, so "elsewhere on your machine" would understate it.
        let refs = collect(&project(&[
            r"%USERPROFILE%\AppData\Local\Temp\Image-Line\{152CA619-2D16-412E-8E59-989887A6795C}\Zip\Cowbell.wav",
        ]));
        assert_eq!(refs[0].class, RefClass::Fragile);
        assert!(refs[0].class.needs_attention());
        assert!(refs[0].class.should_vendor(), "capture it while it exists");
    }

    #[test]
    fn the_fl_data_variable_should_be_recognised_and_expanded() {
        let refs = collect(&project(&[r"%FLStudioData%\Patches\Recorded\Chords.wav"]));
        assert_eq!(refs[0].class, RefClass::UserData);
        assert_eq!(
            refs[0].recorded_path, r"%FLStudioData%\Patches\Recorded\Chords.wav",
            "the original spelling is kept so a rewrite can restore it"
        );
        assert!(
            !refs[0].expanded_path.starts_with('%'),
            "but the expanded form is a real location: {}",
            refs[0].expanded_path
        );
    }

    #[test]
    fn an_empty_path_should_be_missing_not_relative() {
        let refs = collect(&project(&[""]));
        assert_eq!(refs[0].class, RefClass::Missing);
        assert!(
            !refs[0].class.should_vendor(),
            "there is nothing to capture"
        );
        assert!(refs[0].class.needs_attention());
    }

    #[test]
    fn a_relative_path_travels_with_the_project() {
        let refs = collect(&project(&[r"Samples\Kick.wav"]));
        assert_eq!(refs[0].class, RefClass::ProjectRelative);
        assert!(!refs[0].class.should_vendor());
    }

    #[test]
    fn the_same_sample_on_many_channels_should_count_once() {
        // Counting occurrences would tell someone they are uploading eight
        // files when they are uploading one.
        let refs = collect(&project(&[
            r"D:\Packs\Kick.wav",
            r"D:\Packs\Kick.wav",
            r"D:\Packs\Snare.wav",
        ]));
        assert_eq!(refs.len(), 3);
        assert_eq!(distinct(&refs).len(), 2);
    }

    #[test]
    fn an_alias_should_resolve_a_windows_path_to_a_local_one() {
        // How a project saved on Windows is read on this machine.
        let refs = collect(&project(&[r"D:\Soundpacks\Phenom\Kick.wav"]));
        let aliases = vec![PathAlias::new(r"D:\Soundpacks", "/mnt/ssd/packs")];
        assert_eq!(
            refs[0].local_path(&aliases),
            Some(PathBuf::from("/mnt/ssd/packs/Phenom/Kick.wav"))
        );
    }

    #[test]
    fn an_alias_should_match_whichever_way_the_separators_lean() {
        let refs = collect(&project(&[r"D:\Soundpacks\Kick.wav"]));
        let forward = vec![PathAlias::new("D:/Soundpacks", "/mnt/packs")];
        assert_eq!(
            refs[0].local_path(&forward),
            Some(PathBuf::from("/mnt/packs/Kick.wav")),
            "a project spells paths with backslashes; an alias may not"
        );
    }

    #[test]
    fn an_unresolvable_path_should_report_nothing_rather_than_guess() {
        let refs = collect(&project(&[r"D:\Soundpacks\Kick.wav"]));
        assert_eq!(refs[0].local_path(&[]), None);
    }

    #[test]
    fn an_older_project_should_have_its_paths_read_as_single_byte_text() {
        let stream = Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events: vec![
                Event::new(199, b"11.0.0.0\0".to_vec()),
                Event::new(EVENT_SAMPLE_PATH, b"D:\\Packs\\Kick.wav\0".to_vec()),
            ],
        };
        let refs = collect(&stream);
        assert_eq!(refs[0].recorded_path, r"D:\Packs\Kick.wav");
        assert_eq!(refs[0].class, RefClass::External);
    }
}
