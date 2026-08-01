//! Finding projects on disk, whatever DAW made them.
//!
//! The supported formats have incompatible ideas of what a project *is*,
//! and the difference cannot be papered over:
//!
//! - An **Ableton** project is a folder. Whatever is inside belongs to it, so
//!   the scan treats it as a leaf and never descends further.
//! - **Bitwig Studio**, **FL Studio**, **DAWproject**, and native **Auru**
//!   projects are standalone files. The directory containing one is not the
//!   project — an `.flp` examined during design sat in a downloads dump beside
//!   a thousand unrelated images.
//!
//! Callers should not have to know which they are holding, so this module is
//! the one place that does. Everything above it works in terms of
//! [`DiscoveredProject`]: something with a project file, a location, and a
//! format.

use std::fs;
use std::path::{Path, PathBuf};

use crate::ableton::{self, AbletonBundle};

use crate::error::Result;
use crate::flstudio;
use crate::project_format::ProjectFormat;

/// A project found on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DiscoveredProject {
    /// A Live Set inside its project folder.
    Ableton(AbletonBundle),
    /// A project whose file is the whole project.
    Standalone {
        project_file: PathBuf,
        format: ProjectFormat,
    },
}

impl DiscoveredProject {
    /// Recognise whatever `path` points at, or `Ok(None)` if it is not a
    /// project.
    ///
    /// Accepts a folder or a file, because both are things a person may
    /// reasonably drag in.
    pub fn detect(path: &Path) -> Result<Option<Self>> {
        if let Some(format) = standalone_format(path) {
            return Ok(Some(Self::Standalone {
                project_file: path.to_path_buf(),
                format,
            }));
        }
        Ok(AbletonBundle::detect(path)?.map(Self::Ableton))
    }

    /// The file a DAW would open.
    pub fn project_file(&self) -> &Path {
        match self {
            Self::Ableton(bundle) => bundle.live_set(),
            Self::Standalone { project_file, .. } => project_file,
        }
    }

    /// The folder the project lives in.
    ///
    /// For Ableton this is the project's own folder and nothing else is in it.
    /// For FL it is merely *where the file happens to be* — it may hold
    /// anything, including other projects, so it must never be treated as
    /// belonging to this project.
    pub fn directory(&self) -> &Path {
        match self {
            Self::Ableton(bundle) => bundle.root(),
            Self::Standalone { project_file, .. } => {
                project_file.parent().unwrap_or_else(|| Path::new("."))
            }
        }
    }

    /// Whether the containing folder belongs to this project alone.
    ///
    /// The distinction that stops Auru sweeping up someone's Downloads
    /// folder because a `.flp` was saved into it.
    pub const fn owns_its_directory(&self) -> bool {
        matches!(self, Self::Ableton(_))
    }

    pub const fn format(&self) -> ProjectFormat {
        match self {
            Self::Ableton(_) => ProjectFormat::AbletonLiveSet,
            Self::Standalone { format, .. } => *format,
        }
    }

    /// A stable identity for this project.
    ///
    /// Keyed on the **project file**, not the folder. A folder can hold
    /// several `.flp`s, and keying on the directory would make them collide —
    /// adding the second would silently replace the first.
    pub fn key(&self) -> String {
        self.project_file().display().to_string()
    }

    /// What to call this project in a list.
    pub fn name(&self) -> String {
        self.project_file()
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("Untitled")
            .to_owned()
    }
}

/// How far to look, and when to stop.
pub type ScanOptions = ableton::ScanOptions;

/// Every project under `root`, of any supported format.
///
/// Sorted by project file so two machines scanning the same drive agree on
/// the order, and so the list a person is asked to review does not reshuffle
/// between runs.
pub fn scan_for_projects(root: &Path, options: &ScanOptions) -> Vec<DiscoveredProject> {
    let mut found = Vec::new();
    let mut visited_directories = 0;
    scan_into(root, 0, options, &mut visited_directories, &mut found);
    found.sort_by(|left, right| left.project_file().cmp(right.project_file()));
    found.truncate(options.max_projects);
    found
}

/// One walk finding every supported format.
///
/// Deliberately a single pass. Walking twice — once for folders, once for
/// files — doubles the directory reads on a tree that is mostly sample packs,
/// and on a real music drive that was the difference between a scan taking a
/// tenth of a second and taking a second.
fn scan_into(
    dir: &Path,
    depth: usize,
    options: &ScanOptions,
    visited_directories: &mut usize,
    found: &mut Vec<DiscoveredProject>,
) {
    if found.len() >= options.max_projects || *visited_directories >= options.max_directories {
        return;
    }
    *visited_directories += 1;

    let Ok(entries) = fs::read_dir(dir) else {
        // A music drive routinely holds something the user cannot read; that
        // is not a reason to abandon the whole scan.
        return;
    };
    let entries = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();

    // An Ableton project folder is a leaf: whatever is inside belongs to it,
    // including any `.flp` someone bounced into it. Reuse this directory read
    // for detection and traversal.
    if let Ok(Some(bundle)) = AbletonBundle::from_root_entries(dir, &entries) {
        found.push(DiscoveredProject::Ableton(bundle));
        return;
    }

    if depth >= options.max_depth {
        return;
    }

    let mut files = Vec::new();
    let mut directories = Vec::new();
    for path in entries {
        if let Some(format) = standalone_format(&path) {
            files.push((path, format));
        } else if is_scannable(&path, options) {
            directories.push(path);
        }
    }
    files.sort_by(|(left, _), (right, _)| left.cmp(right));
    directories.sort();

    found.extend(
        files
            .into_iter()
            .map(|(project_file, format)| DiscoveredProject::Standalone {
                project_file,
                format,
            }),
    );
    for child in directories {
        scan_into(&child, depth + 1, options, visited_directories, found);
        if found.len() >= options.max_projects || *visited_directories >= options.max_directories {
            return;
        }
    }
}

/// Whether the scan should descend into `path`.
fn is_scannable(path: &Path, options: &ScanOptions) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    // Symlinks are not followed: a link pointing at an ancestor would make the
    // walk run forever.
    if metadata.is_symlink() || !metadata.is_dir() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.starts_with('.') {
        return false;
    }
    // `Backup` and Bitwig's `auto-backups` hold a DAW's own autosaves. They are
    // versions of a project already listed, and offering them as projects in
    // their own right would bury the real one among its own history.
    // An Ableton project folder is not excluded here: `scan_into` recognises
    // it on the way down and stops there, which costs one check per directory
    // instead of two.
    !name.eq_ignore_ascii_case("Backup")
        && !name.eq_ignore_ascii_case("auto-backups")
        && !options
            .excluded_directory_names
            .iter()
            .any(|excluded| name.eq_ignore_ascii_case(excluded))
}

fn standalone_format(path: &Path) -> Option<ProjectFormat> {
    let format = ProjectFormat::from_path(path)?;
    path.is_file()
        .then_some(format)
        .filter(|format| *format != ProjectFormat::AbletonLiveSet)
}

/// Read a project's headline detail, whatever format it is.
pub fn read_headline(project: &DiscoveredProject) -> Option<String> {
    match project.format() {
        ProjectFormat::FlStudio => {
            let source = fs::read(project.project_file()).ok()?;
            Some(flstudio::read_metadata(&source).ok()?.headline())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A minimal `.flp` on disk.
    fn write_flp(path: &Path) {
        use crate::flstudio::{Event, Header, Stream};
        let bytes = Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events: vec![
                Event::new(199, b"20.5.0.1142\0".to_vec()),
                Event::new(156, 174_000u32.to_le_bytes()),
            ],
        }
        .encode();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, bytes).expect("write");
    }

    fn write_bitwig_project(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(path, b"BtWg0003000200ba\0project").expect("write");
    }

    /// A minimal Ableton project folder.
    fn write_ableton_project(root: &Path) {
        fs::create_dir_all(root.join("Ableton Project Info")).expect("mkdir");
        let xml = br#"<?xml version="1.0" encoding="UTF-8"?><Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12.0.25"><LiveSet></LiveSet></Ableton>"#;
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, xml).expect("gz");
        fs::write(root.join("Song.als"), encoder.finish().expect("finish")).expect("write");
    }

    #[test]
    fn a_flp_should_be_recognised_and_should_not_claim_its_folder() {
        // The heart of it: a `.flp` in a downloads folder is a project, but
        // the downloads folder is not part of it.
        let temp = tempfile::tempdir().expect("tempdir");
        let project_file = temp.path().join("Downloads").join("Doom.flp");
        write_flp(&project_file);

        let found = DiscoveredProject::detect(&project_file)
            .expect("detect")
            .expect("a project");
        assert_eq!(found.format(), ProjectFormat::FlStudio);
        assert_eq!(found.project_file(), project_file);
        assert!(
            !found.owns_its_directory(),
            "the containing folder is not the project"
        );
        assert_eq!(found.name(), "Doom");
    }

    #[test]
    fn an_ableton_project_should_still_own_its_folder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_ableton_project(&root);

        let found = DiscoveredProject::detect(&root)
            .expect("detect")
            .expect("a project");
        assert_eq!(found.format(), ProjectFormat::AbletonLiveSet);
        assert!(found.owns_its_directory());
        assert_eq!(found.directory(), root);
    }

    #[test]
    fn two_projects_in_one_folder_should_not_collide() {
        // Keying identity on the folder would make adding the second silently
        // replace the first — the folder is not the project.
        let temp = tempfile::tempdir().expect("tempdir");
        write_flp(&temp.path().join("One.flp"));
        write_flp(&temp.path().join("Two.flp"));

        let found = scan_for_projects(temp.path(), &ScanOptions::default());
        assert_eq!(found.len(), 2);
        assert_ne!(found[0].key(), found[1].key());
    }

    #[test]
    fn a_scan_should_find_both_formats_together() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_ableton_project(&temp.path().join("Live Song Project"));
        write_flp(&temp.path().join("FL Song.flp"));

        let formats: Vec<ProjectFormat> = scan_for_projects(temp.path(), &ScanOptions::default())
            .iter()
            .map(DiscoveredProject::format)
            .collect();
        assert_eq!(formats.len(), 2);
        assert!(formats.contains(&ProjectFormat::AbletonLiveSet));
        assert!(formats.contains(&ProjectFormat::FlStudio));
    }

    #[test]
    fn a_scan_should_offer_dawproject_and_native_projects() {
        let temp = tempfile::tempdir().expect("tempdir");
        fs::write(temp.path().join("Open Exchange.dawproject"), b"fixture").expect("write");
        fs::write(temp.path().join("Native.auru"), b"{}").expect("write");

        let formats: Vec<ProjectFormat> = scan_for_projects(temp.path(), &ScanOptions::default())
            .iter()
            .map(DiscoveredProject::format)
            .collect();

        assert_eq!(formats.len(), 2);
        assert!(formats.contains(&ProjectFormat::Dawproject));
        assert!(formats.contains(&ProjectFormat::Auru));
    }

    #[test]
    fn a_flp_inside_an_ableton_project_should_not_be_listed_separately() {
        // It belongs to that project's folder. Listing it on its own would
        // show the same work twice under two names.
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("Song Project");
        write_ableton_project(&root);
        write_flp(&root.join("Bounce.flp"));

        let found = scan_for_projects(temp.path(), &ScanOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].format(), ProjectFormat::AbletonLiveSet);
    }

    #[test]
    fn autosaves_should_not_be_offered_as_projects() {
        // FL writes its own backups into `Backup/`. Each is a version of a
        // project already in the list, and showing them would bury the real
        // one among its own history.
        let temp = tempfile::tempdir().expect("tempdir");
        write_flp(&temp.path().join("Song.flp"));
        write_flp(&temp.path().join("Backup").join("Song_autosave.flp"));

        let found = scan_for_projects(temp.path(), &ScanOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name(), "Song");
    }

    #[test]
    fn a_scan_should_find_bitwig_projects_nested_beneath_a_library_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project_file = temp
            .path()
            .join("Bitwig Projects")
            .join("Album")
            .join("Song")
            .join("Song.bwproject");
        write_bitwig_project(&project_file);

        let found = scan_for_projects(temp.path(), &ScanOptions::default());

        assert_eq!(found[0].project_file(), project_file);
    }

    #[test]
    fn bitwig_auto_backups_should_not_be_offered_as_projects() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_bitwig_project(&temp.path().join("Song").join("Song.bwproject"));
        write_bitwig_project(
            &temp
                .path()
                .join("Song")
                .join("auto-backups")
                .join("Song")
                .join("Song [2026-08-01 120000].bwproject"),
        );

        let found = scan_for_projects(temp.path(), &ScanOptions::default());

        assert_eq!(found.len(), 1);
    }

    #[test]
    fn a_scan_should_be_bounded_so_a_mistaken_pick_still_returns() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..5 {
            write_flp(&temp.path().join(format!("Song {index}.flp")));
        }
        let options = ScanOptions {
            max_projects: 3,
            ..ScanOptions::default()
        };
        assert_eq!(scan_for_projects(temp.path(), &options).len(), 3);
    }

    #[test]
    fn conventional_sample_pack_folders_should_be_pruned() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_flp(&temp.path().join("Projects").join("Song.flp"));
        write_flp(
            &temp
                .path()
                .join("Sample Packs")
                .join("Deep")
                .join("Pack Song.flp"),
        );

        let found = scan_for_projects(temp.path(), &ScanOptions::default());

        assert_eq!(found.len(), 1);
        assert_eq!(found[0].name(), "Song");
    }

    #[test]
    fn a_directory_budget_should_stop_in_stable_path_order() {
        let temp = tempfile::tempdir().expect("tempdir");
        write_flp(&temp.path().join("A").join("First.flp"));
        write_flp(&temp.path().join("B").join("Second.flp"));
        let options = ScanOptions {
            max_directories: 2,
            excluded_directory_names: Vec::new(),
            ..ScanOptions::default()
        };

        let names = || {
            scan_for_projects(temp.path(), &options)
                .into_iter()
                .map(|project| project.name())
                .collect::<Vec<_>>()
        };
        assert_eq!(names(), ["First"]);
        assert_eq!(names(), names());
    }

    #[test]
    fn something_that_is_not_a_project_should_be_neither_format() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("holiday.jpg");
        fs::write(&path, b"not a project").expect("write");
        assert_eq!(DiscoveredProject::detect(&path).expect("detect"), None);
    }

    #[test]
    fn a_scan_should_be_ordered_the_same_way_every_time() {
        // The review screen must not reshuffle between runs while someone is
        // ticking boxes on it.
        let temp = tempfile::tempdir().expect("tempdir");
        for name in ["Zebra.flp", "Apple.flp", "Mango.flp"] {
            write_flp(&temp.path().join(name));
        }
        let names = || -> Vec<String> {
            scan_for_projects(temp.path(), &ScanOptions::default())
                .iter()
                .map(DiscoveredProject::name)
                .collect()
        };
        assert_eq!(names(), ["Apple", "Mango", "Zebra"]);
        assert_eq!(names(), names());
    }

    #[test]
    fn a_headline_should_be_readable_for_an_fl_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("Song.flp");
        write_flp(&path);

        let found = DiscoveredProject::detect(&path)
            .expect("detect")
            .expect("a project");
        assert_eq!(
            read_headline(&found).as_deref(),
            Some("174 BPM · 1 channel")
        );
    }
}
