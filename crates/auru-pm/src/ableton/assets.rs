//! Deciding what a project-folder commit captures.
//!
//! Two sources combine into one list:
//!
//! 1. every file inside the project folder that policy admits
//!    ([`AbletonBundle::enumerate`]), and
//! 2. every file the Live Set references from *outside* the folder, gathered
//!    in so the project is self-contained.
//!
//! The destination for gathered files is chosen here, at commit time, not on
//! restore. That matters: it makes restore a pure write-out with no filename
//! negotiation, so two machines restoring the same commit produce identical
//! folders, and re-restoring is naturally idempotent.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use crate::ableton::bundle::{AbletonBundle, BundlePolicy, IMPORTED_DIR};
use crate::ableton::refs::{self, RefClass};
use crate::project_format::XmlElement;
use crate::sample_manifest::AssetKind;

/// One file the commit will store.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PlannedAsset {
    /// Where to read the bytes from now.
    pub source: PathBuf,
    /// Where it lives inside the project folder, `/`-separated. Also the
    /// manifest key and the path restore writes to.
    pub bundle_path: String,
    pub kind: AssetKind,
    /// The path string the Live Set used, when this file came from outside
    /// the folder. [`crate::ableton::rewrite`] matches on it to repoint the
    /// reference at the gathered copy.
    pub origin: Option<String>,
}

impl PlannedAsset {
    /// Whether this file was gathered from outside the project folder.
    pub const fn is_vendored(&self) -> bool {
        self.origin.is_some()
    }
}

/// A reference that could not be located on this machine.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnresolvedAsset {
    /// The path as the Live Set recorded it.
    pub reference: String,
    pub class: RefClass,
}

/// What a commit will capture, plus what it could not find.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AssetPlan {
    /// Sorted by `bundle_path`, so a given project always plans identically.
    pub assets: Vec<PlannedAsset>,
    pub unresolved: Vec<UnresolvedAsset>,
}

impl AssetPlan {
    /// Files gathered from outside the project folder.
    pub fn vendored(&self) -> impl Iterator<Item = &PlannedAsset> {
        self.assets.iter().filter(|asset| asset.is_vendored())
    }

    pub fn total_bytes(&self) -> u64 {
        self.assets
            .iter()
            .filter_map(|asset| std::fs::metadata(&asset.source).ok())
            .map(|metadata| metadata.len())
            .sum()
    }
}

/// Plan the assets for a commit of `bundle`.
pub(crate) fn plan(bundle: &AbletonBundle, root: &XmlElement, policy: &BundlePolicy) -> AssetPlan {
    let mut assets: BTreeMap<String, PlannedAsset> = BTreeMap::new();
    let mut unresolved = Vec::new();

    // Everything already in the folder. If the folder cannot be enumerated,
    // retain that as a completeness problem instead of silently producing an
    // empty-but-apparently-valid manifest.
    let folder_files = match bundle.enumerate(policy) {
        Ok(files) => files,
        Err(error) => {
            unresolved.push(UnresolvedAsset {
                reference: format!("{} ({error})", bundle.root().display()),
                class: RefClass::InFolder,
            });
            Vec::new()
        }
    };
    let enumerated_sources = folder_files
        .iter()
        .map(|file| file.absolute.clone())
        .collect::<BTreeSet<_>>();
    for file in folder_files {
        assets.insert(
            file.relative.clone(),
            PlannedAsset {
                source: file.absolute,
                bundle_path: file.relative,
                kind: file.kind,
                origin: None,
            },
        );
    }

    // Then everything referenced from outside it. Collapse the many
    // occurrences of each file first — one loop can be referenced 25 times.
    let mut seen = BTreeSet::new();
    let mut outside = Vec::new();
    for asset in refs::collect(root) {
        if !should_gather(asset.class, policy) {
            continue;
        }
        if !seen.insert(asset.dedup_key().to_owned()) {
            continue;
        }
        outside.push(asset);
    }
    // Sort so collision suffixes are assigned deterministically.
    outside.sort_by(|left, right| left.dedup_key().cmp(right.dedup_key()));

    let mut taken: BTreeSet<String> = assets.keys().cloned().collect();
    for asset in outside {
        let Some(source) = bundle.resolve(&asset.relative_path, &asset.absolute_path, policy)
        else {
            unresolved.push(UnresolvedAsset {
                reference: asset.dedup_key().to_owned(),
                class: asset.class,
            });
            continue;
        };
        // A reference that escapes the folder textually but lands back inside
        // it needs no gathering.
        if bundle.contains(&source) {
            if !enumerated_sources.contains(&source) {
                // Symlinks and files above the configured size ceiling are not
                // enumerated. A reference to one must make the backup
                // incomplete rather than looking safely captured in-place.
                unresolved.push(UnresolvedAsset {
                    reference: asset.dedup_key().to_owned(),
                    class: RefClass::InFolder,
                });
            }
            continue;
        }
        let Some(file_name) = asset.file_name() else {
            continue;
        };
        let bundle_path = allocate_path(file_name, &mut taken);
        assets.insert(
            bundle_path.clone(),
            PlannedAsset {
                source,
                bundle_path,
                kind: kind_for(file_name),
                origin: Some(asset.dedup_key().to_owned()),
            },
        );
    }

    AssetPlan {
        assets: assets.into_values().collect(),
        unresolved,
    }
}

/// Whether a reference of this class should be gathered into the commit.
fn should_gather(class: RefClass, policy: &BundlePolicy) -> bool {
    match class {
        RefClass::External => policy.vendor_external_assets,
        RefClass::UserLibrary => policy.vendor_user_library,
        // Library content resolves from any comparable Live install and is
        // not ours to redistribute; in-folder files are already enumerated;
        // unresolvable references name no file at all.
        RefClass::Library | RefClass::InFolder | RefClass::Unresolvable => false,
    }
}

/// Choose a free destination under `Samples/Imported/`.
///
/// Two different files can share a basename — `break.wav` from two packs — so
/// a taken name gets a numeric suffix. Because the caller feeds references in
/// sorted order, the same project always allocates the same names.
fn allocate_path(file_name: &str, taken: &mut BTreeSet<String>) -> String {
    let (stem, extension) = match file_name.rsplit_once('.') {
        Some((stem, extension)) if !stem.is_empty() => (stem, format!(".{extension}")),
        _ => (file_name, String::new()),
    };
    let mut candidate = format!("{IMPORTED_DIR}/{file_name}");
    let mut suffix = 2;
    while taken.contains(&candidate) {
        candidate = format!("{IMPORTED_DIR}/{stem}-{suffix}{extension}");
        suffix += 1;
    }
    taken.insert(candidate.clone());
    candidate
}

fn kind_for(file_name: &str) -> AssetKind {
    let extension = file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "adg" | "adv" | "alp" | "ams" | "agr" => AssetKind::Preset,
        "asd" => AssetKind::Analysis,
        _ => AssetKind::Sample,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::*;
    use crate::ableton::bundle::{PROJECT_INFO_DIR, PathAlias};
    use crate::ableton::test_support::parse_xml;

    fn touch(path: &Path, bytes: &[u8]) {
        fs::create_dir_all(path.parent().expect("has parent")).expect("create dirs");
        fs::write(path, bytes).expect("write");
    }

    fn file_ref(relative_path_type: u32, relative: &str, absolute: &str) -> String {
        format!(
            r#"<SampleRef><FileRef>
                <RelativePathType Value="{relative_path_type}" />
                <RelativePath Value="{relative}" />
                <Path Value="{absolute}" />
            </FileRef></SampleRef>"#
        )
    }

    fn live_set_xml(body: &str) -> XmlElement {
        parse_xml(&format!(
            "<Ableton><LiveSet><Tracks><AudioTrack Id=\"1\">{body}</AudioTrack></Tracks></LiveSet></Ableton>"
        ))
    }

    /// A project folder plus an outside sample library, as on a real machine.
    fn scenario(temp: &Path) -> (AbletonBundle, BundlePolicy) {
        let project = temp.join("dunno yet-1 Project");
        touch(&project.join("dunno yet.als"), b"set");
        touch(
            &project.join(PROJECT_INFO_DIR).join("AProject.ico"),
            b"icon",
        );
        touch(&project.join("Samples/Processed/loop.wav"), b"in-folder");
        touch(
            &project.join("Backup/dunno yet [2026-01-01 000000].als"),
            b"old",
        );
        touch(&temp.join("library/SPLICE/break.wav"), b"outside");
        touch(&temp.join("library/UserLibrary/rack.adg"), b"rack");

        let bundle = AbletonBundle::detect(&project)
            .expect("detect")
            .expect("is a bundle");
        let policy = BundlePolicy {
            path_aliases: vec![PathAlias::new("E:/lib", temp.join("library"))],
            ..BundlePolicy::default()
        };
        (bundle, policy)
    }

    fn paths(plan: &AssetPlan) -> Vec<&str> {
        plan.assets
            .iter()
            .map(|asset| asset.bundle_path.as_str())
            .collect()
    }

    #[test]
    fn external_samples_should_be_gathered_into_samples_imported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let root = live_set_xml(&file_ref(
            1,
            "../../library/SPLICE/break.wav",
            "E:/lib/SPLICE/break.wav",
        ));

        let plan = plan(&bundle, &root, &policy);
        assert!(paths(&plan).contains(&"Samples/Imported/break.wav"));

        let gathered = plan.vendored().next().expect("one gathered file");
        assert_eq!(
            gathered.origin.as_deref(),
            Some("../../library/SPLICE/break.wav")
        );
        assert_eq!(fs::read(&gathered.source).expect("read"), b"outside");
    }

    #[test]
    fn user_library_racks_should_be_gathered() {
        // Ableton's own Collect All and Save leaves these behind.
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let root = live_set_xml(&file_ref(
            6,
            "UserLibrary/rack.adg",
            "E:/lib/UserLibrary/rack.adg",
        ));

        let plan = plan(&bundle, &root, &policy);
        let gathered = plan.vendored().next().expect("rack gathered");
        assert_eq!(gathered.bundle_path, "Samples/Imported/rack.adg");
        assert_eq!(gathered.kind, AssetKind::Preset);
    }

    #[test]
    fn library_references_should_never_be_gathered() {
        // Core Library content resolves from any Live install, and
        // redistributing it is not ours to do.
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let root = live_set_xml(&file_ref(5, "Devices/Audio Effects/EQ Eight", ""));

        let plan = plan(&bundle, &root, &policy);
        assert_eq!(plan.vendored().count(), 0);
    }

    #[test]
    fn in_folder_references_should_not_be_duplicated_into_imported() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        // Escapes textually, but lands back inside the folder.
        let root = live_set_xml(&file_ref(
            1,
            "../dunno yet-1 Project/Samples/Processed/loop.wav",
            "",
        ));

        let plan = plan(&bundle, &root, &policy);
        assert_eq!(plan.vendored().count(), 0);
        assert!(paths(&plan).contains(&"Samples/Processed/loop.wav"));
    }

    #[test]
    fn folder_contents_should_be_planned_alongside_gathered_files() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let root = live_set_xml(&file_ref(
            1,
            "../../library/SPLICE/break.wav",
            "E:/lib/SPLICE/break.wav",
        ));

        let plan = plan(&bundle, &root, &policy);
        assert_eq!(
            paths(&plan),
            vec![
                "Ableton Project Info/AProject.ico",
                "Samples/Imported/break.wav",
                "Samples/Processed/loop.wav",
            ],
            "backups excluded by default; the .als itself is the snapshot"
        );
    }

    #[test]
    fn unreachable_references_should_be_reported_not_dropped() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let root = live_set_xml(&file_ref(
            1,
            "../../library/SPLICE/gone.wav",
            "E:/lib/SPLICE/gone.wav",
        ));

        let plan = plan(&bundle, &root, &policy);
        assert_eq!(plan.vendored().count(), 0);
        assert_eq!(plan.unresolved.len(), 1);
        assert_eq!(plan.unresolved[0].class, RefClass::External);
    }

    #[test]
    fn empty_references_should_not_be_reported_as_unresolved() {
        // The 14 empty FileRefs in a real project name no file at all.
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let root = live_set_xml(&file_ref(0, "", ""));

        let plan = plan(&bundle, &root, &policy);
        assert!(plan.unresolved.is_empty());
    }

    #[test]
    fn one_file_referenced_many_times_should_be_gathered_once() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        let reference = file_ref(
            1,
            "../../library/SPLICE/break.wav",
            "E:/lib/SPLICE/break.wav",
        );
        let root = live_set_xml(&format!("{reference}{reference}{reference}"));

        let plan = plan(&bundle, &root, &policy);
        assert_eq!(plan.vendored().count(), 1);
    }

    #[test]
    fn colliding_basenames_should_get_deterministic_suffixes() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, policy) = scenario(temp.path());
        touch(&temp.path().join("library/PackA/break.wav"), b"a");
        touch(&temp.path().join("library/PackB/break.wav"), b"b");
        // One level up from the project folder reaches `library/`.
        let root = live_set_xml(&format!(
            "{}{}",
            file_ref(1, "../library/PackA/break.wav", ""),
            file_ref(1, "../library/PackB/break.wav", "")
        ));

        let first = plan(&bundle, &root, &policy);
        let second = plan(&bundle, &root, &policy);

        let destination = |origin: &str| {
            first
                .vendored()
                .find(|asset| asset.origin.as_deref() == Some(origin))
                .map(|asset| asset.bundle_path.as_str())
                .expect("gathered")
        };
        // Both land under Imported with distinct names, allocated in sorted
        // reference order so the assignment never depends on walk order.
        assert_eq!(
            destination("../library/PackA/break.wav"),
            "Samples/Imported/break.wav"
        );
        assert_eq!(
            destination("../library/PackB/break.wav"),
            "Samples/Imported/break-2.wav"
        );
        // Planning twice must produce the same layout.
        assert_eq!(first, second);
    }

    #[test]
    fn vendoring_can_be_turned_off_entirely() {
        let temp = tempfile::tempdir().expect("tempdir");
        let (bundle, _) = scenario(temp.path());
        let policy = BundlePolicy {
            vendor_external_assets: false,
            vendor_user_library: false,
            path_aliases: vec![PathAlias::new("E:/lib", temp.path().join("library"))],
            ..BundlePolicy::default()
        };
        let root = live_set_xml(&file_ref(1, "../../library/SPLICE/break.wav", ""));

        let plan = plan(&bundle, &root, &policy);
        assert_eq!(plan.vendored().count(), 0);
        assert!(plan.unresolved.is_empty());
    }
}
