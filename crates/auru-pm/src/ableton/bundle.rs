//! The Ableton Project Folder as the unit of version control.
//!
//! A Live Set is never self-contained. The `.als` is one file in a folder Live
//! creates and maintains around it:
//!
//! ```text
//! dunno yet-1 Project/
//!     dunno yet.als              ← the set itself; becomes the snapshot blob
//!     Ableton Project Info/      ← folder marker and icon
//!     Samples/Processed/…        ← warped and reversed audio Live generated
//!     Samples/Recorded/…         ← audio recorded into the project
//!     Backup/…                   ← Live's own timestamped autosaves
//! ```
//!
//! and it reaches *outside* that folder for anything the user dragged in from
//! a sample library or their User Library. Committing only the `.als` — which
//! is all this crate could do before — produces a backup that restores to a
//! project full of missing media.
//!
//! So the folder is the commit unit, and anything referenced from outside it
//! is gathered in alongside. See [`crate::ableton::refs`] for how those
//! outside references are found and classified.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::sample_manifest::AssetKind;

/// Folder Live writes to mark a directory as a project.
pub const PROJECT_INFO_DIR: &str = "Ableton Project Info";

/// Where gathered outside assets are placed, mirroring the destination
/// Ableton's own "Collect All and Save" uses.
pub const IMPORTED_DIR: &str = "Samples/Imported";

/// Directory holding Live's autosaves.
const BACKUP_DIR: &str = "Backup";

/// Suffixes never committed: our own sidecar state and in-flight temporaries.
const ALWAYS_EXCLUDED_SUFFIXES: [&str; 3] = ["-pm.json", "-pm.json.tmp", ".tmp"];

/// Default ceiling on a single gathered file.
const DEFAULT_MAX_ASSET_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// What a commit of a project folder captures.
///
/// Defaults reflect the decision that a restored project should open and play
/// on a machine that has never seen the original, without carrying redundant
/// history: everything the project needs, plus the user's own content from
/// outside the folder, minus Live's autosaves.
#[derive(Clone, Debug)]
pub struct BundlePolicy {
    /// Live's own `Backup/*.als` autosaves.
    ///
    /// Off by default. Auru's history supersedes them, and they are large —
    /// in a real project the autosaves were 3.7 MB against a 1 MB set, so
    /// including them roughly quadruples every commit to store versions
    /// Auru already tracks better.
    pub include_backups: bool,
    /// `.asd` analysis sidecars. On by default: Live regenerates them, but
    /// regenerating discards hand-edited warp markers, and they are tiny.
    pub include_analysis_files: bool,
    /// `Ableton Project Info/` and platform folder metadata (`Desktop.ini`,
    /// `.DS_Store`).
    pub include_project_info: bool,
    /// Gather referenced files that live outside the project folder.
    pub vendor_external_assets: bool,
    /// Gather the user's own User Library presets and racks. Ableton's own
    /// "Collect All and Save" does not do this, which is a common way for a
    /// shared project to arrive broken.
    pub vendor_user_library: bool,
    /// Skip any single file larger than this.
    pub max_asset_bytes: u64,
    /// Prefix substitutions applied when a recorded absolute path does not
    /// exist locally — for reading a project saved on another machine or OS.
    /// See [`PathAlias`].
    pub path_aliases: Vec<PathAlias>,
}

impl Default for BundlePolicy {
    fn default() -> Self {
        Self {
            include_backups: false,
            include_analysis_files: true,
            include_project_info: true,
            vendor_external_assets: true,
            vendor_user_library: true,
            max_asset_bytes: DEFAULT_MAX_ASSET_BYTES,
            path_aliases: PathAlias::from_environment(),
        }
    }
}

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

    /// Read aliases from `AURU_ABLETON_PATH_ALIASES`.
    ///
    /// Format is `from=to`, separated by the platform path separator:
    ///
    /// ```text
    /// AURU_ABLETON_PATH_ALIASES='E:/Music Production=/mnt/ssd/Music Production'
    /// ```
    pub fn from_environment() -> Vec<Self> {
        let Ok(raw) = std::env::var("AURU_ABLETON_PATH_ALIASES") else {
            return Vec::new();
        };
        raw.split([';', ':'].as_ref())
            .filter_map(|entry| {
                // Split on the first `=`; a Windows prefix contains a colon,
                // so `=` is the only safe separator within an entry.
                let (from, to) = entry.split_once('=')?;
                let (from, to) = (from.trim(), to.trim());
                (!from.is_empty() && !to.is_empty()).then(|| Self::new(from, to))
            })
            .collect()
    }

    /// Apply this alias to `path`, if it matches.
    fn apply(&self, path: &str) -> Option<PathBuf> {
        let from = self.from.trim_end_matches(['/', '\\']);
        if path.len() < from.len() || !path[..from.len()].eq_ignore_ascii_case(from) {
            return None;
        }
        let rest = path[from.len()..].trim_start_matches(['/', '\\']);
        Some(self.to.join(to_native_relative(rest)))
    }
}

/// An Ableton project folder on disk.
#[derive(Clone, Debug)]
pub struct AbletonBundle {
    root: PathBuf,
    live_set: PathBuf,
}

impl AbletonBundle {
    /// Resolve `path` — either the folder or the `.als` inside it — to a bundle.
    ///
    /// Returns `Ok(None)` for a `.als` that is not inside a project folder.
    /// That is a legitimate arrangement (a loose set on the Desktop), and it
    /// keeps working exactly as it did before: snapshot the one file, no
    /// folder semantics.
    pub fn detect(path: &Path) -> Result<Option<Self>> {
        if path.is_dir() {
            return Self::from_root(path);
        }
        if !is_live_set(path) {
            return Ok(None);
        }
        // A set inside a project folder: the parent is the root.
        let Some(parent) = path.parent() else {
            return Ok(None);
        };
        if !looks_like_project_root(parent) {
            return Ok(None);
        }
        Ok(Some(Self {
            root: parent.to_path_buf(),
            live_set: path.to_path_buf(),
        }))
    }

    fn from_root(root: &Path) -> Result<Option<Self>> {
        if !looks_like_project_root(root) {
            return Ok(None);
        }
        let live_set = choose_live_set(root)?;
        Ok(Some(Self {
            root: root.to_path_buf(),
            live_set,
        }))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn live_set(&self) -> &Path {
        &self.live_set
    }

    /// Every file in the folder that `policy` says to commit.
    ///
    /// The `.als` itself is excluded — its content is the snapshot blob, so
    /// including it here would store it twice.
    pub fn enumerate(&self, policy: &BundlePolicy) -> Result<Vec<BundleFile>> {
        let mut files = Vec::new();
        self.walk(&self.root, policy, &mut files)?;
        files.sort_by(|left, right| left.relative.cmp(&right.relative));
        Ok(files)
    }

    fn walk(&self, dir: &Path, policy: &BundlePolicy, out: &mut Vec<BundleFile>) -> Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            // Never follow symlinks: they can leave the folder entirely, and
            // a cycle would hang the walk.
            let metadata = fs::symlink_metadata(&path)?;
            if metadata.is_symlink() {
                continue;
            }
            if metadata.is_dir() {
                self.walk(&path, policy, out)?;
                continue;
            }
            let Some(relative) = self.relative_path(&path) else {
                continue;
            };
            let Some(kind) = classify(&relative, policy) else {
                continue;
            };
            if path == self.live_set {
                continue;
            }
            if metadata.len() > policy.max_asset_bytes {
                continue;
            }
            out.push(BundleFile {
                relative,
                absolute: path,
                size: metadata.len(),
                kind,
            });
        }
        Ok(())
    }

    /// Folder-relative path with `/` separators, matching how Ableton writes
    /// them and how they are stored in the manifest.
    fn relative_path(&self, path: &Path) -> Option<String> {
        let relative = path.strip_prefix(&self.root).ok()?;
        let mut parts = Vec::new();
        for component in relative.components() {
            match component {
                std::path::Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
                // Anything else would escape the folder; refuse it rather
                // than producing a path that writes outside on restore.
                _ => return None,
            }
        }
        (!parts.is_empty()).then(|| parts.join("/"))
    }

    /// Resolve a path recorded in the Live Set against this folder.
    ///
    /// Tries, in order: the project-relative path; the recorded absolute path;
    /// then each configured [`PathAlias`]. Returns the first that exists.
    pub fn resolve(
        &self,
        relative: &str,
        absolute: &str,
        policy: &BundlePolicy,
    ) -> Option<PathBuf> {
        if !relative.is_empty() && !has_windows_drive(relative) && !relative.starts_with('/') {
            let candidate = normalize(&self.root.join(to_native_relative(relative)));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !absolute.is_empty() {
            // A drive-qualified path is not a path on this OS. Handing
            // `E:/x` to `Path::new` on Unix yields a *relative* path named
            // `E:` that would resolve against the working directory, so it is
            // only ever routed through the aliases below.
            if !has_windows_drive(absolute) {
                let candidate = PathBuf::from(absolute);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            for alias in &policy.path_aliases {
                if let Some(candidate) = alias.apply(absolute) {
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    /// Whether `path` lies inside this project folder.
    pub fn contains(&self, path: &Path) -> bool {
        normalize(path).starts_with(normalize(&self.root))
    }
}

/// One committed file from inside the project folder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BundleFile {
    /// Folder-relative, `/`-separated.
    pub relative: String,
    pub absolute: PathBuf,
    pub size: u64,
    pub kind: AssetKind,
}

/// Whether a directory is an Ableton project folder.
/// How far to look, and how much to look at, when searching a folder for
/// projects.
///
/// Both limits exist because the folder a person points at is often the top of
/// a music drive: one real library measured 655 projects across 670 entries.
/// Walking that is fine; walking a home directory to unbounded depth is not.
#[derive(Clone, Debug)]
pub struct ScanOptions {
    /// Directory levels below the chosen folder to search.
    ///
    /// Projects normally sit one level down. The default allows for a couple
    /// of organising folders — `Ableton Projects/2026/Client Work/…` — without
    /// turning a mistaken pick of `/` into an exhaustive disk walk.
    pub max_depth: usize,
    /// Stop after this many projects, so a pathological tree still returns.
    pub max_projects: usize,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_projects: 5_000,
        }
    }
}

/// Find every Ableton project inside `root`.
///
/// Returns them sorted by path so the same folder always lists the same way.
/// `root` itself is included when it is a project.
///
/// Three rules keep the count honest:
///
/// - A project is not descended into. Its `Backup/` folder holds autosaves
///   that would otherwise each look like a project of their own — a real
///   library holds 1,941 `.als` files across 655 projects, so counting sets
///   rather than projects would overstate it threefold.
/// - `Backup/` is never treated as a project, even standalone: a project with
///   a single autosave would otherwise match the "one lone set" rule.
/// - Hidden folders and symlinks are skipped. Symlinks can point anywhere,
///   including into a cycle.
pub fn scan_for_projects(root: &Path, options: &ScanOptions) -> Vec<AbletonBundle> {
    let mut found = Vec::new();
    scan_into(root, 0, options, &mut found);
    found.sort_by(|left, right| left.root.cmp(&right.root));
    found
}

fn scan_into(dir: &Path, depth: usize, options: &ScanOptions, found: &mut Vec<AbletonBundle>) {
    if found.len() >= options.max_projects {
        return;
    }

    // A project is a leaf: whatever is inside belongs to it.
    if looks_like_project_root(dir) {
        if let Ok(Some(bundle)) = AbletonBundle::from_root(dir) {
            found.push(bundle);
        }
        return;
    }

    if depth >= options.max_depth {
        return;
    }

    let Ok(entries) = fs::read_dir(dir) else {
        // An unreadable folder is skipped rather than failing the scan — a
        // music drive routinely contains something the user cannot read.
        return;
    };

    let mut children: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| is_scannable_directory(path))
        .collect();
    children.sort();

    for child in children {
        scan_into(&child, depth + 1, options, found);
        if found.len() >= options.max_projects {
            return;
        }
    }
}

/// Whether the scan should look inside `path`.
fn is_scannable_directory(path: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return false;
    };
    if metadata.is_symlink() || !metadata.is_dir() {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    // `Backup` holds a project's own autosaves; treating it as a project
    // would double-count every project that has one.
    !name.starts_with('.') && !name.eq_ignore_ascii_case(BACKUP_DIR)
}

fn looks_like_project_root(dir: &Path) -> bool {
    if dir.join(PROJECT_INFO_DIR).is_dir() {
        return true;
    }
    // A folder Live has not yet marked, but which holds exactly one set.
    top_level_live_sets(dir).is_ok_and(|sets| sets.len() == 1)
}

fn is_live_set(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("als"))
}

fn top_level_live_sets(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut sets = Vec::new();
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_file() && is_live_set(&path) {
            sets.push(path);
        }
    }
    sets.sort();
    Ok(sets)
}

/// Pick the project's set from a folder that holds more than one.
///
/// Live names the folder after the set (`dunno yet.als` → `dunno yet Project`,
/// or `dunno yet-1 Project` when disambiguated), so the set whose stem is a
/// prefix of the folder name is the right one. Autosaves live in `Backup/` and
/// are never top-level, so they cannot be picked by accident.
fn choose_live_set(root: &Path) -> Result<PathBuf> {
    let sets = top_level_live_sets(root)?;
    match sets.len() {
        0 => Err(Error::ProjectFormat(format!(
            "'{}' contains no Ableton Live Set",
            root.display()
        ))),
        1 => Ok(sets.into_iter().next().expect("length checked")),
        _ => {
            let folder = root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            sets.iter()
                .find(|set| {
                    set.file_stem()
                        .and_then(|stem| stem.to_str())
                        .is_some_and(|stem| folder.starts_with(stem))
                })
                .cloned()
                .ok_or_else(|| {
                    Error::ProjectFormat(format!(
                        "'{}' contains {} Live Sets and none matches the folder name; \
                         open the .als directly to choose one",
                        root.display(),
                        sets.len()
                    ))
                })
        }
    }
}

/// Decide what a folder-relative path is, or `None` to exclude it.
fn classify(relative: &str, policy: &BundlePolicy) -> Option<AssetKind> {
    if ALWAYS_EXCLUDED_SUFFIXES
        .iter()
        .any(|suffix| relative.ends_with(suffix))
    {
        return None;
    }

    let is_backup = relative
        .split('/')
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case(BACKUP_DIR));
    if is_backup {
        return policy.include_backups.then_some(AssetKind::Backup);
    }

    let name = relative.rsplit('/').next().unwrap_or(relative);
    let is_project_info = relative.starts_with(PROJECT_INFO_DIR)
        || name.eq_ignore_ascii_case("Desktop.ini")
        || name.eq_ignore_ascii_case(".DS_Store");
    if is_project_info {
        return policy
            .include_project_info
            .then_some(AssetKind::ProjectInfo);
    }

    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "asd" => policy.include_analysis_files.then_some(AssetKind::Analysis),
        "adg" | "adv" | "alp" | "ams" | "agr" => Some(AssetKind::Preset),
        "wav" | "aif" | "aiff" | "flac" | "mp3" | "ogg" | "m4a" => Some(AssetKind::Sample),
        _ => Some(AssetKind::Other),
    }
}

/// Convert an Ableton-style relative path to a native one.
///
/// These always use `/`, even when written on Windows, and `..` segments are
/// resolved lexically by [`normalize`] afterwards.
fn to_native_relative(path: &str) -> PathBuf {
    path.split(['/', '\\'])
        .filter(|segment| !segment.is_empty())
        .collect()
}

/// Fold `.` and `..` lexically.
///
/// Not [`std::fs::canonicalize`]: that requires the path to exist, and this
/// runs on candidates that may not.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `path` starts with a Windows drive qualifier such as `E:/`.
fn has_windows_drive(path: &str) -> bool {
    let mut chars = path.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.next() == Some(':')
        && matches!(chars.next(), Some('/' | '\\'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("has parent")).expect("create dirs");
        fs::write(path, bytes).expect("write file");
    }

    /// Build a project folder shaped like the real one, at `root`.
    fn project_folder(root: &Path) -> PathBuf {
        let live_set = root.join("dunno yet.als");
        touch(&live_set, b"set");
        touch(&root.join(PROJECT_INFO_DIR).join("AProject.ico"), b"icon");
        touch(&root.join("Desktop.ini"), b"[.ShellClassInfo]");
        touch(
            &root.join("Backup/dunno yet [2026-07-28 151730].als"),
            b"old",
        );
        touch(&root.join("Samples/Processed/Reverse/loop.wav"), b"audio");
        touch(&root.join("Samples/Processed/Reverse/loop.wav.asd"), b"asd");
        touch(&root.join("dunno yet.als-pm.json"), b"{}");
        live_set
    }

    fn names(files: &[BundleFile]) -> Vec<&str> {
        files.iter().map(|file| file.relative.as_str()).collect()
    }

    #[test]
    fn bundle_detection_should_accept_the_folder_or_the_live_set() {
        let temp = tempfile::tempdir().expect("tempdir");
        let live_set = project_folder(temp.path());

        let from_folder = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");
        let from_set = AbletonBundle::detect(&live_set)
            .expect("detect")
            .expect("is a bundle");

        assert_eq!(from_folder.root(), temp.path());
        assert_eq!(from_folder.live_set(), live_set);
        assert_eq!(from_set.root(), temp.path());
        assert_eq!(from_set.live_set(), live_set);
    }

    #[test]
    fn a_loose_live_set_should_not_be_a_bundle() {
        // A set on the Desktop with no project folder keeps working as a
        // single file, exactly as before folder support existed.
        let temp = tempfile::tempdir().expect("tempdir");
        let loose = temp.path().join("sketch.als");
        touch(&loose, b"set");
        touch(&temp.path().join("other.als"), b"set");
        touch(&temp.path().join("notes.txt"), b"hi");

        assert!(
            AbletonBundle::detect(&loose)
                .expect("detect")
                .is_none_or(|bundle| bundle.live_set() == loose),
            "two sets and no project marker is not one project"
        );
    }

    #[test]
    fn backups_should_be_excluded_by_default() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");

        let files = bundle
            .enumerate(&BundlePolicy::default())
            .expect("enumerate");
        assert!(
            !names(&files).iter().any(|name| name.starts_with("Backup/")),
            "autosaves are superseded by Auru's own history: {:?}",
            names(&files)
        );
    }

    #[test]
    fn backups_should_be_included_when_the_policy_asks() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");

        let policy = BundlePolicy {
            include_backups: true,
            ..BundlePolicy::default()
        };
        let files = bundle.enumerate(&policy).expect("enumerate");
        let backup = files
            .iter()
            .find(|file| file.relative.starts_with("Backup/"))
            .expect("backup present");
        assert_eq!(backup.kind, AssetKind::Backup);
    }

    #[test]
    fn enumeration_should_classify_and_exclude_correctly() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");
        let files = bundle
            .enumerate(&BundlePolicy::default())
            .expect("enumerate");

        assert_eq!(
            names(&files),
            vec![
                "Ableton Project Info/AProject.ico",
                "Desktop.ini",
                "Samples/Processed/Reverse/loop.wav",
                "Samples/Processed/Reverse/loop.wav.asd",
            ]
        );
        let kind = |name: &str| {
            files
                .iter()
                .find(|file| file.relative == name)
                .expect("present")
                .kind
        };
        assert_eq!(
            kind("Samples/Processed/Reverse/loop.wav"),
            AssetKind::Sample
        );
        assert_eq!(
            kind("Samples/Processed/Reverse/loop.wav.asd"),
            AssetKind::Analysis
        );
        assert_eq!(kind("Desktop.ini"), AssetKind::ProjectInfo);
    }

    #[test]
    fn the_live_set_and_sidecar_should_never_be_enumerated() {
        // The set is the snapshot blob; committing it here would store it
        // twice. The sidecar lives inside the folder and changes on every
        // write, so including it would make the manifest never stabilize.
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");
        let files = bundle
            .enumerate(&BundlePolicy::default())
            .expect("enumerate");

        assert!(!names(&files).contains(&"dunno yet.als"));
        assert!(!names(&files).contains(&"dunno yet.als-pm.json"));
    }

    #[test]
    fn analysis_files_should_be_excluded_when_the_policy_says_so() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");

        let policy = BundlePolicy {
            include_analysis_files: false,
            ..BundlePolicy::default()
        };
        let files = bundle.enumerate(&policy).expect("enumerate");
        assert!(!names(&files).iter().any(|name| name.ends_with(".asd")));
    }

    #[test]
    fn resolve_should_find_a_project_relative_sample() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");

        let found = bundle
            .resolve(
                "Samples/Processed/Reverse/loop.wav",
                "E:/nowhere/loop.wav",
                &BundlePolicy::default(),
            )
            .expect("resolved");
        assert!(found.is_file());
    }

    #[test]
    fn resolve_should_bridge_a_windows_path_through_an_alias() {
        // The real case: a set saved on Windows referencing `E:/Music
        // Production/samples/…`, opened where that volume is mounted at
        // `/mnt/ssd/Music Production`.
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("proj");
        project_folder(&project);
        let library = temp.path().join("mnt/ssd/Music Production/samples");
        touch(&library.join("SPLICE/break.wav"), b"audio");

        let bundle = AbletonBundle::detect(&project)
            .expect("detect")
            .expect("is a bundle");
        let policy = BundlePolicy {
            path_aliases: vec![PathAlias::new(
                "E:/Music Production",
                temp.path().join("mnt/ssd/Music Production"),
            )],
            ..BundlePolicy::default()
        };

        let found = bundle
            .resolve(
                "../../samples/SPLICE/break.wav",
                "E:/Music Production/samples/SPLICE/break.wav",
                &policy,
            )
            .expect("resolved through alias");
        assert_eq!(fs::read(&found).expect("read"), b"audio");
    }

    #[test]
    fn resolve_should_never_treat_a_drive_path_as_relative_to_the_cwd() {
        // `Path::new("E:/x")` on Unix is a *relative* path named `E:`. If a
        // directory called `E:` happened to exist beside us, resolving it
        // would silently pick up the wrong file.
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("proj");
        project_folder(&project);
        touch(&project.join("E:/Music/break.wav"), b"wrong file");

        let bundle = AbletonBundle::detect(&project)
            .expect("detect")
            .expect("is a bundle");
        assert!(
            bundle
                .resolve("", "E:/Music/break.wav", &BundlePolicy::default())
                .is_none(),
            "a drive-qualified path must only resolve through an alias"
        );
    }

    #[test]
    fn resolve_should_return_none_for_an_unreachable_reference() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");
        assert!(
            bundle
                .resolve("Samples/gone.wav", "", &BundlePolicy::default())
                .is_none()
        );
    }

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
    fn contains_should_distinguish_inside_from_outside() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(temp.path());
        let bundle = AbletonBundle::detect(temp.path())
            .expect("detect")
            .expect("is a bundle");

        assert!(bundle.contains(&temp.path().join("Samples/a.wav")));
        assert!(!bundle.contains(Path::new("/elsewhere/a.wav")));
    }

    #[test]
    fn symlinks_should_never_be_committed() {
        // A symlink can point anywhere, including outside the folder or into
        // a cycle. Refusing them keeps the walk bounded and the commit honest.
        #[cfg(unix)]
        {
            let temp = tempfile::tempdir().expect("tempdir");
            project_folder(temp.path());
            let outside = temp.path().parent().expect("parent").join("outside.wav");
            let _ = fs::write(&outside, b"x");
            let _ = std::os::unix::fs::symlink(&outside, temp.path().join("Samples/link.wav"));

            let bundle = AbletonBundle::detect(temp.path())
                .expect("detect")
                .expect("is a bundle");
            let files = bundle
                .enumerate(&BundlePolicy::default())
                .expect("enumerate");
            assert!(!names(&files).contains(&"Samples/link.wav"));
        }
    }

    #[test]
    fn a_folder_with_several_sets_should_pick_the_one_named_after_it() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("dunno yet-1 Project");
        touch(&root.join("dunno yet.als"), b"set");
        touch(&root.join("scratch.als"), b"set");
        touch(&root.join(PROJECT_INFO_DIR).join("AProject.ico"), b"icon");

        let bundle = AbletonBundle::detect(&root)
            .expect("detect")
            .expect("is a bundle");
        assert_eq!(bundle.live_set(), root.join("dunno yet.als"));
    }

    #[test]
    fn scanning_should_find_every_project_in_a_library() {
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("Ableton Projects");
        for name in ["110 riddim Project", "ayy Project", "dunno yet-1 Project"] {
            project_folder(&library.join(name));
        }

        let found = scan_for_projects(&library, &ScanOptions::default());
        let names: Vec<&str> = found
            .iter()
            .map(|bundle| {
                bundle
                    .root()
                    .file_name()
                    .and_then(|name| name.to_str())
                    .expect("named")
            })
            .collect();
        assert_eq!(
            names,
            vec!["110 riddim Project", "ayy Project", "dunno yet-1 Project"],
            "sorted, so the same folder always lists the same way"
        );
    }

    #[test]
    fn a_projects_own_backups_should_not_count_as_projects() {
        // The mistake that would triple the count: a real library holds 1,941
        // `.als` files across 655 projects, nearly all of the difference being
        // autosaves inside `Backup/`.
        let temp = tempfile::tempdir().expect("tempdir");
        let library = temp.path().join("Projects");
        project_folder(&library.join("Song Project"));

        let found = scan_for_projects(&library, &ScanOptions::default());
        assert_eq!(found.len(), 1, "one project, not one per autosave");
        assert!(found[0].root().ends_with("Song Project"));
    }

    #[test]
    fn a_lone_backup_folder_should_never_look_like_a_project() {
        // A `Backup` holding a single autosave matches the "one lone set"
        // rule that catches unmarked projects, so it needs excluding by name.
        let temp = tempfile::tempdir().expect("tempdir");
        let backup = temp.path().join("Backup");
        touch(&backup.join("Song [2026-01-01 000000].als"), b"autosave");

        assert!(scan_for_projects(temp.path(), &ScanOptions::default()).is_empty());
    }

    #[test]
    fn scanning_should_reach_projects_filed_under_subfolders() {
        // People organise. `Projects/2026/Client Work/Song Project` is normal.
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(&temp.path().join("2026/Client Work/Song Project"));

        let found = scan_for_projects(temp.path(), &ScanOptions::default());
        assert_eq!(found.len(), 1);
    }

    #[test]
    fn scanning_should_stop_at_the_depth_limit() {
        // The guard against a mistaken pick of a home directory turning into
        // an exhaustive disk walk.
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(&temp.path().join("a/b/c/d/e/Song Project"));

        let shallow = ScanOptions {
            max_depth: 2,
            ..ScanOptions::default()
        };
        assert!(scan_for_projects(temp.path(), &shallow).is_empty());

        let deep = ScanOptions {
            max_depth: 12,
            ..ScanOptions::default()
        };
        assert_eq!(scan_for_projects(temp.path(), &deep).len(), 1);
    }

    #[test]
    fn scanning_should_honour_the_project_ceiling() {
        let temp = tempfile::tempdir().expect("tempdir");
        for index in 0..5 {
            project_folder(&temp.path().join(format!("Song {index} Project")));
        }

        let capped = ScanOptions {
            max_projects: 3,
            ..ScanOptions::default()
        };
        assert_eq!(scan_for_projects(temp.path(), &capped).len(), 3);
    }

    #[test]
    fn pointing_the_scan_at_one_project_should_find_that_project() {
        // Someone may pick the project folder itself rather than the library.
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("Song Project");
        project_folder(&project);

        let found = scan_for_projects(&project, &ScanOptions::default());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].root(), project);
    }

    #[test]
    fn hidden_folders_and_symlinks_should_be_skipped() {
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(&temp.path().join(".Trash/Deleted Project"));

        #[cfg(unix)]
        {
            let real = temp.path().parent().expect("parent").join("elsewhere");
            project_folder(&real.join("Linked Project"));
            let _ = std::os::unix::fs::symlink(&real, temp.path().join("link"));
        }

        assert!(
            scan_for_projects(temp.path(), &ScanOptions::default()).is_empty(),
            "neither a hidden folder nor a symlink should be followed"
        );
    }

    #[test]
    fn an_unreadable_folder_should_not_abort_the_scan() {
        // A music drive routinely holds something the user cannot read; one
        // such folder must not cost them the rest of their library.
        let temp = tempfile::tempdir().expect("tempdir");
        project_folder(&temp.path().join("Song Project"));
        let blocked = temp.path().join("locked");
        fs::create_dir_all(&blocked).expect("create");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000));
        }

        let found = scan_for_projects(temp.path(), &ScanOptions::default());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let _ = fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755));
        }

        assert_eq!(found.len(), 1, "the readable project is still found");
    }

    #[test]
    fn path_aliases_should_parse_from_the_environment_format() {
        // Parsed directly rather than through the env var, so the test does
        // not depend on process-global state.
        let alias = PathAlias::new("E:/Music Production", "/mnt/ssd/Music Production");
        assert_eq!(alias.from, "E:/Music Production");
        assert_eq!(alias.to, PathBuf::from("/mnt/ssd/Music Production"));
    }
}
