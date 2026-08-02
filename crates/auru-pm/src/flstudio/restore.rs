//! Putting an FL Studio project back on disk.
//!
//! This is the only place Auru writes a sample anywhere. On the way in,
//! captured files are read where they already live and nothing is moved; on
//! the way out they are materialised into a `Samples/` folder beside the
//! restored `.flp`, and the project's own paths are repointed at them.
//!
//! Repointing is what makes a restored project *work* rather than merely
//! exist. The original paths refer to a machine that may not be this one —
//! `D:\Soundpacks\...` on a laptop with no D drive — so a faithful restore of
//! the bytes would open with every sample missing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};

use super::assets::SAMPLES_DIR;
use super::events::Stream;
use super::refs::{self, RefClass};

/// What a restore did, and what it could not do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RestoreReport {
    /// The `.flp` to open.
    pub project_file: PathBuf,
    /// Where the samples went.
    pub samples_dir: Option<PathBuf>,
    pub files_written: usize,
    pub bytes_written: u64,
    /// Sample references repointed at the restored copies.
    pub refs_repointed: usize,
    /// References left pointing where they always did, because the commit
    /// carried no copy of them. The project will open with these missing, and
    /// the person restoring it needs to be told which.
    pub still_missing: Vec<String>,
}

impl RestoreReport {
    /// Whether the project will open with everything it needs.
    pub fn is_complete(&self) -> bool {
        self.still_missing.is_empty()
    }
}

/// Repoint a project's sample references at files beside it.
///
/// `captured` maps the path a project recorded to where that file now lives,
/// relative to the `.flp`. References with no entry are left exactly as they
/// were: a reference to a sample we do not have is still the best record of
/// what the project wanted, and blanking it would destroy the only clue.
pub fn repoint(stream: &mut Stream, captured: &BTreeMap<String, String>) -> RestoreReport {
    let utf16 = super::events::uses_utf16(stream.major_version());
    let references = refs::collect(stream);

    let mut report = RestoreReport::default();
    for reference in &references {
        match captured.get(&reference.recorded_path) {
            Some(destination) => {
                // Written relative, so the project stays portable: moving the
                // folder somewhere else keeps every sample resolvable.
                stream.events[reference.event_index].payload =
                    encode_path(&to_native_separators(destination), utf16);
                report.refs_repointed += 1;
            }
            // Nothing was named, so there is nothing anyone could go and find.
            None if reference.class == RefClass::Missing => {}
            // Already relative to the project, so it travels with it — either
            // it was always beside the `.flp`, or a previous restore put it
            // there. Reporting these would make a repeat restore claim its own
            // captured samples were missing.
            None if reference.class == RefClass::ProjectRelative => {}
            None => report.still_missing.push(reference.recorded_path.clone()),
        }
    }

    report.still_missing.sort();
    report.still_missing.dedup();
    report
}

/// Write `bytes` as a file under `root`, refusing anything that would escape.
///
/// `relative` comes out of a commit, which came out of a project file, so it
/// is untrusted: a crafted manifest entry must not be able to write outside
/// the folder the user chose to restore into.
pub fn write_asset(root: &Path, relative: &str, bytes: &[u8]) -> Result<PathBuf> {
    let destination = safe_join(root, relative)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    crate::verified_io::write_verified_new(&destination, bytes)?;
    Ok(destination)
}

/// Join `relative` onto `root`, or refuse.
fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    if relative.is_empty() {
        return Err(Error::ProjectFormat(
            "a captured file has no destination".to_owned(),
        ));
    }

    let mut destination = root.to_path_buf();
    for part in relative.split(['/', '\\']) {
        match part {
            // A leading `/` produces an empty first part; `.` is a no-op.
            "" | "." => continue,
            ".." => {
                return Err(Error::ProjectFormat(format!(
                    "refusing to restore '{relative}': it points outside the project"
                )));
            }
            part if part.contains(':') => {
                return Err(Error::ProjectFormat(format!(
                    "refusing to restore '{relative}': it names a drive"
                )));
            }
            part => destination.push(part),
        }
    }

    if destination == root {
        return Err(Error::ProjectFormat(format!(
            "refusing to restore '{relative}': it names no file"
        )));
    }
    Ok(destination)
}

/// Where the samples folder sits for a given project file.
pub fn samples_dir_for(project_file: &Path) -> PathBuf {
    project_file
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(SAMPLES_DIR)
}

fn to_native_separators(path: &str) -> String {
    // FL reads either, but writes backslashes; matching it keeps a restored
    // project textually indistinguishable from one FL saved itself.
    path.replace('/', "\\")
}

fn encode_path(path: &str, utf16: bool) -> Vec<u8> {
    let mut bytes = Vec::new();
    if utf16 {
        for unit in path.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
    } else {
        bytes.extend(path.chars().map(|character| character as u8));
        bytes.push(0);
    }
    bytes
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
                .map(|path| Event::new(refs::EVENT_SAMPLE_PATH, utf16(path))),
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
    fn a_captured_sample_should_be_repointed_beside_the_project() {
        let mut stream = project(&[r"D:\Soundpacks\Kick.wav"]);
        let captured = BTreeMap::from([(
            r"D:\Soundpacks\Kick.wav".to_owned(),
            "Samples/Kick.wav".to_owned(),
        )]);

        let report = repoint(&mut stream, &captured);
        assert_eq!(report.refs_repointed, 1);
        assert!(report.is_complete());

        let after = refs::collect(&stream);
        assert_eq!(
            after[0].recorded_path, r"Samples\Kick.wav",
            "relative, so the project stays portable if the folder moves"
        );
        assert_eq!(after[0].class, RefClass::ProjectRelative);
    }

    #[test]
    fn a_sample_the_backup_never_had_should_be_left_alone_and_reported() {
        // Blanking it would destroy the only record of what the project
        // wanted, which is exactly what someone needs in order to go and find
        // the file themselves.
        let mut stream = project(&[r"D:\Soundpacks\Kick.wav"]);
        let report = repoint(&mut stream, &BTreeMap::new());

        assert_eq!(report.refs_repointed, 0);
        assert!(!report.is_complete());
        assert_eq!(report.still_missing, [r"D:\Soundpacks\Kick.wav"]);
        assert_eq!(
            refs::collect(&stream)[0].recorded_path,
            r"D:\Soundpacks\Kick.wav"
        );
    }

    #[test]
    fn a_reference_with_no_path_should_not_be_called_missing() {
        // The project never named a file, so there is nothing the person
        // restoring could go and find. Listing it would be noise.
        let mut stream = project(&[""]);
        let report = repoint(&mut stream, &BTreeMap::new());
        assert!(report.still_missing.is_empty());
        assert!(report.is_complete());
    }

    #[test]
    fn a_sample_already_beside_the_project_should_not_be_called_missing() {
        // What a second restore sees: the paths are already relative because
        // the first one repointed them. Reporting these would have a repeat
        // restore claim its own captured samples had gone.
        let mut stream = project(&[r"Samples\Kick.wav"]);
        let report = repoint(&mut stream, &BTreeMap::new());
        assert!(report.is_complete(), "{:?}", report.still_missing);
    }

    #[test]
    fn the_same_missing_sample_on_many_channels_should_be_reported_once() {
        let mut stream = project(&[
            r"D:\Packs\Kick.wav",
            r"D:\Packs\Kick.wav",
            r"D:\Packs\Snare.wav",
        ]);
        let report = repoint(&mut stream, &BTreeMap::new());
        assert_eq!(report.still_missing.len(), 2);
    }

    #[test]
    fn repointing_should_leave_the_project_otherwise_byte_identical() {
        // The restore must change the sample paths and nothing else.
        let mut stream = project(&[r"D:\Packs\Kick.wav"]);
        stream
            .events
            .push(Event::new(213, vec![0x00, 0xff, 0xfe, 0x7f]));
        let before = stream.clone();

        repoint(
            &mut stream,
            &BTreeMap::from([(
                r"D:\Packs\Kick.wav".to_owned(),
                "Samples/Kick.wav".to_owned(),
            )]),
        );

        assert_eq!(stream.header, before.header);
        assert_eq!(stream.events.len(), before.events.len());
        for (index, (after, before)) in stream.events.iter().zip(&before.events).enumerate() {
            assert_eq!(after.id, before.id);
            if after.id != refs::EVENT_SAMPLE_PATH {
                assert_eq!(after.payload, before.payload, "event {index} changed");
            }
        }
    }

    #[test]
    fn a_destination_that_escapes_the_project_should_be_refused() {
        // A commit's manifest came from a project file and is untrusted; a
        // crafted entry must not write outside where the user chose.
        let temp = tempfile::tempdir().expect("tempdir");
        for hostile in [
            "../escaped.wav",
            "Samples/../../escaped.wav",
            r"..\escaped.wav",
            "C:/Windows/evil.dll",
            "",
        ] {
            assert!(
                write_asset(temp.path(), hostile, b"x").is_err(),
                "{hostile:?} should have been refused"
            );
        }
    }

    #[test]
    fn an_ordinary_destination_should_be_written_with_its_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let written = write_asset(temp.path(), "Samples/Kick.wav", b"audio").expect("write");
        assert_eq!(std::fs::read(&written).expect("read"), b"audio");
        assert!(written.starts_with(temp.path()));
    }

    #[test]
    fn a_leading_slash_should_not_make_a_destination_absolute() {
        let temp = tempfile::tempdir().expect("tempdir");
        let written = write_asset(temp.path(), "/Samples/Kick.wav", b"audio").expect("write");
        assert!(
            written.starts_with(temp.path()),
            "{} escaped {}",
            written.display(),
            temp.path().display()
        );
    }

    #[test]
    fn the_samples_folder_should_sit_beside_the_project_file() {
        assert_eq!(
            samples_dir_for(Path::new("/music/restored/Song.flp")),
            PathBuf::from("/music/restored/Samples")
        );
    }
}
