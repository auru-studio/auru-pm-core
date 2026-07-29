//! Deciding which of a project's files to capture, and where they land.
//!
//! An FL project has no folder of its own, so there is nowhere a sample could
//! already be "inside". Every file a project depends on is somewhere else on
//! the machine, and capturing it is the only way it survives a move.
//!
//! Nothing here writes anything. Planning is deliberately separate from doing:
//! a plan can be shown to someone — *this is what will be uploaded, this is
//! what cannot be found* — before a single byte leaves the machine. That is
//! the same promise the watch-folder screen makes, and it only holds if
//! working out the cost does not have side effects.

use std::collections::BTreeSet;
use std::path::PathBuf;

use crate::ableton::PathAlias;
use crate::sample_manifest::AssetKind;

use super::refs::{self, AssetRef, RefClass};

/// Where captured samples are put inside a restored project.
///
/// A folder beside the `.flp` rather than a rename or a rewrite in place: FL's
/// own convention for a self-contained project, and the restoring machine is
/// the only place Auru is entitled to create anything.
pub const SAMPLES_DIR: &str = "Samples";

/// One file that will be captured.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedAsset {
    /// Where to read the bytes from now.
    pub source: PathBuf,
    /// Where it will live relative to the restored `.flp`, `/`-separated.
    /// Also the manifest key.
    pub bundle_path: String,
    pub kind: AssetKind,
    /// The path string the project recorded, so restore can find the events
    /// that need repointing at the captured copy.
    pub origin: String,
    /// Why this file is being captured.
    pub class: RefClass,
}

/// A file the project refers to that could not be found on this machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnresolvedAsset {
    pub recorded_path: String,
    pub class: RefClass,
    /// Why it could not be resolved, in words a person can act on.
    pub reason: String,
}

/// What a backup of this project would capture.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetPlan {
    /// Sorted by `bundle_path`, so a given project always plans identically
    /// and two machines agree on what a commit contains.
    pub assets: Vec<PlannedAsset>,
    pub unresolved: Vec<UnresolvedAsset>,
}

impl AssetPlan {
    pub fn total_bytes(&self) -> u64 {
        self.assets
            .iter()
            .filter_map(|asset| std::fs::metadata(&asset.source).ok())
            .map(|metadata| metadata.len())
            .sum()
    }

    /// Files at risk that were found, and so can still be rescued.
    ///
    /// Worth surfacing on its own: these are the ones where backing up now is
    /// the difference between keeping the audio and losing it.
    pub fn rescuable(&self) -> impl Iterator<Item = &PlannedAsset> {
        self.assets
            .iter()
            .filter(|asset| asset.class == RefClass::Fragile)
    }
}

/// Work out which files a backup would capture.
///
/// `aliases` translate paths written on another machine — a project saved on
/// Windows referring to `D:\Packs` resolves against a drive mounted here.
pub fn plan(refs: &[AssetRef], aliases: &[PathAlias]) -> AssetPlan {
    let mut plan = AssetPlan::default();
    let mut taken: BTreeSet<String> = BTreeSet::new();

    for reference in refs::distinct(refs) {
        if !reference.class.should_vendor() {
            if reference.class == RefClass::Missing {
                plan.unresolved.push(UnresolvedAsset {
                    recorded_path: reference.recorded_path.clone(),
                    class: reference.class,
                    reason: "the project records no path for this sample".to_owned(),
                });
            }
            continue;
        }

        let Some(source) = reference.local_path(aliases) else {
            plan.unresolved.push(UnresolvedAsset {
                recorded_path: reference.recorded_path.clone(),
                class: reference.class,
                reason: "this path is on another machine; set a path alias to find it".to_owned(),
            });
            continue;
        };
        if !source.is_file() {
            plan.unresolved.push(UnresolvedAsset {
                recorded_path: reference.recorded_path.clone(),
                class: reference.class,
                // The honest phrasing for a fragile ref: it is not that we
                // cannot find it, it is that it is already gone.
                reason: if reference.class == RefClass::Fragile {
                    "this was in temporary space and has already been deleted".to_owned()
                } else {
                    "no file at this path on this machine".to_owned()
                },
            });
            continue;
        }

        let bundle_path = unique_destination(reference.file_name(), &mut taken);
        plan.assets.push(PlannedAsset {
            source,
            bundle_path,
            kind: AssetKind::Sample,
            origin: reference.recorded_path.clone(),
            class: reference.class,
        });
    }

    plan.assets
        .sort_by(|left, right| left.bundle_path.cmp(&right.bundle_path));
    plan
}

/// A destination that no other asset has claimed.
///
/// Two samples can share a file name and come from different folders, so the
/// name alone is not enough. Suffixes are allocated in plan order, which is
/// deterministic because the references are walked in document order.
fn unique_destination(file_name: &str, taken: &mut BTreeSet<String>) -> String {
    let file_name = sanitize(file_name);
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem.to_owned(), format!(".{extension}")),
        _ => (file_name.clone(), String::new()),
    };

    let mut candidate = format!("{SAMPLES_DIR}/{stem}{extension}");
    let mut suffix = 2;
    while !taken.insert(candidate.clone()) {
        candidate = format!("{SAMPLES_DIR}/{stem}-{suffix}{extension}");
        suffix += 1;
    }
    candidate
}

/// Make a recorded file name safe to write on this machine.
///
/// Names come from another operating system's file system and may contain
/// characters this one forbids, or path separators that would let a crafted
/// project write outside the destination entirely.
fn sanitize(file_name: &str) -> String {
    let cleaned: String = file_name
        .chars()
        .map(|character| match character {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            character if character.is_control() => '_',
            character => character,
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').to_owned();
    if cleaned.is_empty() {
        "sample".to_owned()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flstudio::events::{Event, Header, Stream};

    fn utf16(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    fn refs_for(paths: &[&str]) -> Vec<AssetRef> {
        let mut events = vec![Event::new(199, b"20.5.0.1142\0".to_vec())];
        events.extend(
            paths
                .iter()
                .map(|path| Event::new(refs::EVENT_SAMPLE_PATH, utf16(path))),
        );
        refs::collect(&Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events,
        })
    }

    #[test]
    fn a_found_sample_should_be_planned_into_the_samples_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("Kick.wav"), b"audio").expect("write");
        let aliases = vec![PathAlias::new(r"D:\Packs", temp.path())];

        let plan = plan(&refs_for(&[r"D:\Packs\Kick.wav"]), &aliases);
        assert_eq!(plan.assets.len(), 1);
        assert_eq!(plan.assets[0].bundle_path, "Samples/Kick.wav");
        assert_eq!(plan.assets[0].origin, r"D:\Packs\Kick.wav");
        assert!(plan.unresolved.is_empty());
    }

    #[test]
    fn two_samples_with_the_same_name_should_not_overwrite_each_other() {
        let temp = tempfile::tempdir().expect("tempdir");
        for folder in ["a", "b"] {
            std::fs::create_dir_all(temp.path().join(folder)).expect("mkdir");
            std::fs::write(temp.path().join(folder).join("Kick.wav"), b"audio").expect("write");
        }
        let aliases = vec![PathAlias::new(r"D:\Packs", temp.path())];

        let plan = plan(
            &refs_for(&[r"D:\Packs\a\Kick.wav", r"D:\Packs\b\Kick.wav"]),
            &aliases,
        );
        assert_eq!(plan.assets.len(), 2);
        let destinations: Vec<&str> = plan
            .assets
            .iter()
            .map(|asset| asset.bundle_path.as_str())
            .collect();
        assert_eq!(destinations, ["Samples/Kick-2.wav", "Samples/Kick.wav"]);
    }

    #[test]
    fn a_sample_on_another_machine_should_be_reported_not_skipped_silently() {
        let plan = plan(&refs_for(&[r"D:\Packs\Kick.wav"]), &[]);
        assert!(plan.assets.is_empty());
        assert_eq!(plan.unresolved.len(), 1);
        assert!(
            plan.unresolved[0].reason.contains("path alias"),
            "the message should say what would fix it: {}",
            plan.unresolved[0].reason
        );
    }

    #[test]
    fn a_fragile_sample_already_deleted_should_say_so_plainly() {
        // "No file at this path" would be technically true and useless. The
        // person needs to know the audio is gone, not that a lookup failed.
        let temp = tempfile::tempdir().expect("tempdir");
        let aliases = vec![PathAlias::new("C:", temp.path())];
        let plan = plan(
            &refs_for(&[r"C:\Temp\Image-Line\{GUID}\Zip\Cowbell.wav"]),
            &aliases,
        );
        assert_eq!(plan.unresolved.len(), 1);
        assert_eq!(plan.unresolved[0].class, RefClass::Fragile);
        assert!(
            plan.unresolved[0].reason.contains("already been deleted"),
            "{}",
            plan.unresolved[0].reason
        );
    }

    #[test]
    fn a_fragile_sample_that_still_exists_should_be_rescuable() {
        let temp = tempfile::tempdir().expect("tempdir");
        let scratch = temp.path().join(r"Temp/Image-Line/GUID/Zip");
        std::fs::create_dir_all(&scratch).expect("mkdir");
        std::fs::write(scratch.join("Cowbell.wav"), b"audio").expect("write");
        let aliases = vec![PathAlias::new("C:", temp.path())];

        let plan = plan(
            &refs_for(&[r"C:\Temp\Image-Line\GUID\Zip\Cowbell.wav"]),
            &aliases,
        );
        assert_eq!(plan.rescuable().count(), 1);
    }

    #[test]
    fn a_crafted_file_name_should_not_escape_the_samples_folder() {
        // The file name comes out of the project file, so it is untrusted
        // input. The property that matters is not how the name is spelled
        // afterwards but where it can land: joined onto a destination it must
        // stay inside it, whatever the project asked for.
        let root = std::path::Path::new("/restore/here");
        for hostile in [
            r"..\..\evil.wav",
            "../../evil.wav",
            "/etc/passwd",
            r"C:\Windows\System32\evil.dll",
            "..",
        ] {
            let mut taken = BTreeSet::new();
            let destination = unique_destination(hostile, &mut taken);

            let resolved = root.join(&destination);
            assert!(
                resolved.starts_with(root.join(SAMPLES_DIR)),
                "{hostile:?} planned to {destination:?}, which leaves the samples folder"
            );
            assert!(
                !destination[SAMPLES_DIR.len() + 1..].contains(['/', '\\']),
                "{hostile:?} planned to {destination:?}, which is more than one level deep"
            );
        }
    }

    #[test]
    fn a_name_that_sanitises_to_nothing_should_still_get_a_destination() {
        let mut taken = BTreeSet::new();
        assert_eq!(unique_destination("...", &mut taken), "Samples/sample");
    }

    #[test]
    fn a_plan_should_not_depend_on_the_order_references_were_read() {
        // Two machines planning the same project must agree, or they commit
        // different manifests for identical content.
        let temp = tempfile::tempdir().expect("tempdir");
        for name in ["Kick.wav", "Snare.wav"] {
            std::fs::write(temp.path().join(name), b"audio").expect("write");
        }
        let aliases = vec![PathAlias::new(r"D:\Packs", temp.path())];

        let one = plan(
            &refs_for(&[r"D:\Packs\Kick.wav", r"D:\Packs\Snare.wav"]),
            &aliases,
        );
        let other = plan(
            &refs_for(&[r"D:\Packs\Snare.wav", r"D:\Packs\Kick.wav"]),
            &aliases,
        );
        let paths = |plan: &AssetPlan| -> Vec<String> {
            plan.assets
                .iter()
                .map(|asset| asset.bundle_path.clone())
                .collect()
        };
        assert_eq!(paths(&one), paths(&other));
    }
}
