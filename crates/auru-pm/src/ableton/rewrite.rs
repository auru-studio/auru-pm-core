//! Repointing a Live Set at the copies of its files that travelled with it.
//!
//! When a project is committed, files it referenced from outside its folder
//! are gathered into `Samples/Imported/` ([`crate::ableton::assets`]). The set
//! itself still points at wherever those files were on the machine that saved
//! it — `E:/Music Production/samples/…`, `C:/Users/…/User Library/…`. On any
//! other machine those paths mean nothing, and Live opens the project with the
//! media missing.
//!
//! So on restore the references are rewritten to point inside the folder. This
//! is the same thing Ableton's own "Collect All and Save" does, and it means
//! the restored `.als` is deliberately *not* byte-identical to the original —
//! it is the same project, correctly addressed for where it now lives.
//!
//! Only files that were actually gathered are touched. Core Library devices,
//! Live Pack content, references already inside the folder, and the empty
//! references a real set carries by the dozen are all left exactly as they
//! were.

use std::collections::BTreeMap;
use std::path::Path;

use crate::project_format::XmlElement;

/// Where each gathered file ended up, keyed by the path the set referenced it
/// by. Built from the commit's asset manifest.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct VendorPlan {
    placements: BTreeMap<String, String>,
}

impl VendorPlan {
    /// Build a plan from `(original reference, path inside the folder)` pairs.
    pub fn new(placements: impl IntoIterator<Item = (String, String)>) -> Self {
        Self {
            placements: placements.into_iter().collect(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.placements.is_empty()
    }

    fn destination_for(&self, reference: &str) -> Option<&str> {
        self.placements.get(reference).map(String::as_str)
    }
}

/// What rewriting did, and what it deliberately left alone.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RewriteReport {
    /// References repointed at a gathered copy.
    pub rewritten: usize,
    /// References already pointing inside the project folder.
    pub already_in_folder: usize,
    /// Core Library and Live Pack references, which resolve from the user's
    /// own Live installation.
    pub left_to_live: usize,
    /// References naming no file at all.
    pub empty: usize,
    /// References to files that were not gathered and are not library
    /// content — they will still be missing when the project opens.
    pub unresolved: Vec<String>,
}

impl RewriteReport {
    /// Whether the restored project will open with everything it needs.
    pub fn is_complete(&self) -> bool {
        self.unresolved.is_empty()
    }
}

/// Point every gathered reference in `root` at its copy inside `project_root`.
///
/// Idempotent over its own output: a rewritten reference no longer matches
/// anything in the plan, and is recognised by its shape as already in-folder,
/// so applying this twice to one tree changes nothing the second time.
///
/// Note that restoring the same commit twice does *not* hit that path — a
/// restore starts from the committed snapshot, which still holds the original
/// reference, so it performs the same rewrite again and produces the same
/// bytes. That is what keeps a restore a pure function of its commit.
pub(crate) fn rewrite_file_refs(
    root: &mut XmlElement,
    plan: &VendorPlan,
    project_root: &Path,
) -> RewriteReport {
    let mut report = RewriteReport::default();
    visit_file_refs(root, &mut |file_ref| {
        rewrite_one(file_ref, plan, project_root, &mut report);
    });
    report
}

/// Depth-first walk invoking `visit` on every `FileRef`.
fn visit_file_refs(element: &mut XmlElement, visit: &mut impl FnMut(&mut XmlElement)) {
    if element.tag == "FileRef" {
        visit(element);
        // A FileRef never nests another.
        return;
    }
    for child in element.child_elements_mut() {
        visit_file_refs(child, visit);
    }
}

fn rewrite_one(
    file_ref: &mut XmlElement,
    plan: &VendorPlan,
    project_root: &Path,
    report: &mut RewriteReport,
) {
    let relative = file_ref.child_value("RelativePath").unwrap_or_default();
    let absolute = file_ref.child_value("Path").unwrap_or_default();
    let path_type = file_ref
        .child_value("RelativePathType")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let live_pack = file_ref
        .child_value("LivePackName")
        .filter(|name| !name.is_empty())
        .is_some();

    // Names no file — a real project carries a dozen or more of these.
    if relative.is_empty() && absolute.is_empty() {
        report.empty += 1;
        return;
    }

    // Ships with Live. Resolves from the user's own installation, and
    // redistributing it is not ours to do.
    if path_type == super::refs::INSTALLATION_PATH_TYPE || live_pack {
        report.left_to_live += 1;
        return;
    }

    let reference = if relative.is_empty() {
        absolute
    } else {
        relative
    };

    if let Some(destination) = plan.destination_for(reference) {
        let destination = destination.to_owned();
        write_reference(file_ref, &destination, project_root);
        report.rewritten += 1;
        return;
    }

    // Already inside the folder, either from the start or from a previous
    // restore. The second case is what makes this idempotent.
    if path_type == super::refs::PROJECT_FOLDER_PATH_TYPE && !relative.is_empty() {
        report.already_in_folder += 1;
        return;
    }

    report.unresolved.push(reference.to_owned());
}

/// Point one reference at `destination`, a folder-relative `/`-separated path.
fn write_reference(file_ref: &mut XmlElement, destination: &str, project_root: &Path) {
    file_ref.set_child_value(
        "RelativePathType",
        super::refs::PROJECT_FOLDER_PATH_TYPE.to_string(),
    );
    // Always forward slashes, on every platform — Ableton's own convention,
    // and what makes the restored set portable in turn.
    file_ref.set_child_value("RelativePath", destination);
    file_ref.set_child_value("Path", absolute_path_for(project_root, destination));
    // `OriginalFileSize` and `OriginalCrc` describe the content, which
    // gathering did not change, so they are left alone.
}

/// The host-absolute form of a folder-relative path, in Ableton's slash style.
fn absolute_path_for(project_root: &Path, destination: &str) -> String {
    let mut path = project_root.to_string_lossy().replace('\\', "/");
    if !path.ends_with('/') {
        path.push('/');
    }
    path.push_str(destination);
    path
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ableton::test_support::parse_xml;

    fn file_ref(path_type: u32, relative: &str, absolute: &str) -> String {
        format!(
            r#"<SampleRef><FileRef>
                <RelativePathType Value="{path_type}" />
                <RelativePath Value="{relative}" />
                <Path Value="{absolute}" />
                <Type Value="1" />
                <LivePackName Value="" />
                <OriginalFileSize Value="5907514" />
                <OriginalCrc Value="42" />
            </FileRef></SampleRef>"#
        )
    }

    fn live_set(body: &str) -> XmlElement {
        parse_xml(&format!(
            "<Ableton><LiveSet><Tracks><AudioTrack Id=\"1\">{body}</AudioTrack></Tracks></LiveSet></Ableton>"
        ))
    }

    fn first_file_ref(root: &XmlElement) -> &XmlElement {
        root.descendants()
            .find(|node| node.tag == "FileRef")
            .expect("a FileRef")
    }

    fn splice_plan() -> VendorPlan {
        VendorPlan::new([(
            "../../samples/SPLICE/break.wav".to_owned(),
            "Samples/Imported/break.wav".to_owned(),
        )])
    }

    #[test]
    fn a_gathered_reference_should_point_inside_the_project_folder() {
        let mut root = live_set(&file_ref(
            1,
            "../../samples/SPLICE/break.wav",
            "E:/Music Production/samples/SPLICE/break.wav",
        ));
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/home/jake/Song"));

        assert_eq!(report.rewritten, 1);
        assert!(report.is_complete());

        let rewritten = first_file_ref(&root);
        assert_eq!(rewritten.child_value("RelativePathType"), Some("3"));
        assert_eq!(
            rewritten.child_value("RelativePath"),
            Some("Samples/Imported/break.wav")
        );
        assert_eq!(
            rewritten.child_value("Path"),
            Some("/home/jake/Song/Samples/Imported/break.wav")
        );
    }

    #[test]
    fn rewriting_should_preserve_the_content_fingerprint() {
        // Size and CRC describe the file, which gathering did not change.
        // Clearing them would lose Live's own integrity check.
        let mut root = live_set(&file_ref(1, "../../samples/SPLICE/break.wav", ""));
        rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        let rewritten = first_file_ref(&root);
        assert_eq!(rewritten.child_value("OriginalFileSize"), Some("5907514"));
        assert_eq!(rewritten.child_value("OriginalCrc"), Some("42"));
    }

    #[test]
    fn restoring_twice_should_not_change_anything_the_second_time() {
        let mut root = live_set(&file_ref(1, "../../samples/SPLICE/break.wav", ""));
        let plan = splice_plan();

        let first = rewrite_file_refs(&mut root, &plan, Path::new("/p"));
        let after_first = format!("{root:?}");
        let second = rewrite_file_refs(&mut root, &plan, Path::new("/p"));

        assert_eq!(first.rewritten, 1);
        assert_eq!(second.rewritten, 0, "nothing left to repoint");
        assert_eq!(second.already_in_folder, 1);
        assert_eq!(format!("{root:?}"), after_first, "the tree is unchanged");
    }

    #[test]
    fn core_library_references_should_be_left_to_live() {
        let mut root = live_set(&file_ref(5, "Devices/Audio Effects/EQ Eight", ""));
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        assert_eq!(report.left_to_live, 1);
        assert_eq!(report.rewritten, 0);
        assert_eq!(
            first_file_ref(&root).child_value("RelativePath"),
            Some("Devices/Audio Effects/EQ Eight"),
            "untouched"
        );
    }

    #[test]
    fn live_pack_references_should_be_left_to_live() {
        let mut root = parse_xml(
            r#"<Ableton><LiveSet><FileRef>
                <RelativePathType Value="1" />
                <RelativePath Value="Samples/pack.wav" />
                <Path Value="" />
                <LivePackName Value="Core Library" />
            </FileRef></LiveSet></Ableton>"#,
        );
        let report = rewrite_file_refs(&mut root, &VendorPlan::default(), Path::new("/p"));
        assert_eq!(report.left_to_live, 1);
    }

    #[test]
    fn empty_references_should_be_left_alone() {
        // A real Live 12 set carries 14 of these. They must never be counted
        // as missing, and never rewritten into something pointing nowhere.
        let mut root = live_set(&format!("{}{}", file_ref(0, "", ""), file_ref(0, "", "")));
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        assert_eq!(report.empty, 2);
        assert!(report.is_complete());
        assert_eq!(first_file_ref(&root).child_value("RelativePath"), Some(""));
    }

    #[test]
    fn references_already_in_the_folder_should_be_left_alone() {
        let mut root = live_set(&file_ref(3, "Samples/Processed/loop.wav", "/old/loop.wav"));
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        assert_eq!(report.already_in_folder, 1);
        assert_eq!(report.rewritten, 0);
        assert_eq!(
            first_file_ref(&root).child_value("Path"),
            Some("/old/loop.wav"),
            "an in-folder reference is not repointed"
        );
    }

    #[test]
    fn a_file_that_was_never_gathered_should_be_reported_as_still_missing() {
        // Honest reporting matters more than a clean-looking result: the user
        // needs to know this one will open with media missing.
        let mut root = live_set(&file_ref(
            1,
            "../../samples/gone.wav",
            "E:/samples/gone.wav",
        ));
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        assert_eq!(report.rewritten, 0);
        assert!(!report.is_complete());
        assert_eq!(report.unresolved, vec!["../../samples/gone.wav"]);
    }

    #[test]
    fn a_reference_keyed_by_absolute_path_should_be_matched() {
        // References with no relative path are keyed by their absolute one,
        // matching how the manifest recorded them.
        let mut root = live_set(&file_ref(0, "", "C:/Users/jake/rack.adg"));
        let plan = VendorPlan::new([(
            "C:/Users/jake/rack.adg".to_owned(),
            "Samples/Imported/rack.adg".to_owned(),
        )]);
        let report = rewrite_file_refs(&mut root, &plan, Path::new("/p"));

        assert_eq!(report.rewritten, 1);
        assert_eq!(
            first_file_ref(&root).child_value("RelativePath"),
            Some("Samples/Imported/rack.adg")
        );
    }

    #[test]
    fn every_occurrence_of_one_file_should_be_rewritten() {
        // A single loop is referenced once per clip that uses it — 25 times in
        // a real project. Missing any one leaves that clip broken.
        let reference = file_ref(1, "../../samples/SPLICE/break.wav", "");
        let mut root = live_set(&reference.repeat(4));
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        assert_eq!(report.rewritten, 4);
        for node in root.descendants().filter(|node| node.tag == "FileRef") {
            assert_eq!(
                node.child_value("RelativePath"),
                Some("Samples/Imported/break.wav")
            );
        }
    }

    #[test]
    fn windows_project_roots_should_be_written_in_ableton_slash_style() {
        let mut root = live_set(&file_ref(1, "../../samples/SPLICE/break.wav", ""));
        rewrite_file_refs(
            &mut root,
            &splice_plan(),
            Path::new(r"C:\Users\jake\Music\Song"),
        );
        assert_eq!(
            first_file_ref(&root).child_value("Path"),
            Some("C:/Users/jake/Music/Song/Samples/Imported/break.wav")
        );
    }

    #[test]
    fn a_reference_missing_its_path_type_should_still_be_rewritten() {
        // Older sets omit fields this one adds back.
        let mut root = parse_xml(
            r#"<Ableton><LiveSet><FileRef>
                <RelativePath Value="../../samples/SPLICE/break.wav" />
            </FileRef></LiveSet></Ableton>"#,
        );
        let report = rewrite_file_refs(&mut root, &splice_plan(), Path::new("/p"));

        assert_eq!(report.rewritten, 1);
        let rewritten = first_file_ref(&root);
        assert_eq!(rewritten.child_value("RelativePathType"), Some("3"));
        assert_eq!(
            rewritten.child_value("Path"),
            Some("/p/Samples/Imported/break.wav"),
            "the absolute path is added even though the set had none"
        );
    }
}
