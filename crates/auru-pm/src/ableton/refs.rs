//! `FileRef` harvesting and classification.
//!
//! Every external file an Ableton Live Set depends on — samples, `.adv`/`.adg`
//! presets, Core Library devices — is reached through a `FileRef` element:
//!
//! ```xml
//! <FileRef>
//!     <RelativePathType Value="1" />
//!     <RelativePath Value="../../samples/SPLICE/…/break.wav" />
//!     <Path Value="E:/Music Production/samples/SPLICE/…/break.wav" />
//!     <Type Value="1" />
//!     <LivePackName Value="" />
//!     <LivePackId Value="" />
//!     <OriginalFileSize Value="5907514" />
//!     <OriginalCrc Value="…" />
//! </FileRef>
//! ```
//!
//! `RelativePathType` is the authoritative classification signal — it says
//! where Live resolves the path from, which is exactly the distinction that
//! decides whether we vendor a file into the project folder or merely record
//! that it exists. Path-shape heuristics are only a fallback for sets that
//! predate the field or write it as `0`.

use crate::project_format::XmlElement;

/// How Live resolves a `FileRef`'s `RelativePath`.
///
/// Values observed across real Live 10–12 sets. Unknown discriminants are
/// preserved verbatim in [`Self::Other`] rather than coerced, so a set written
/// by a future Live version round-trips unchanged.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelativePathType {
    /// No usable path — both `RelativePath` and `Path` are empty.
    Missing,
    /// Relative to the project folder, and may escape it via `../`.
    ProjectRelative,
    /// Relative to, and contained by, the project folder.
    ProjectFolder,
    /// Relative to the Live installation / Core Library.
    Installation,
    /// Relative to the user's Ableton User Library.
    UserLibrary,
    /// A discriminant this version does not model.
    Other(u32),
}

/// `RelativePathType` for a path resolved inside the project folder. This is
/// what a gathered file's reference is rewritten to.
pub(crate) const PROJECT_FOLDER_PATH_TYPE: u32 = 3;

/// `RelativePathType` for content that ships with the Live installation.
pub(crate) const INSTALLATION_PATH_TYPE: u32 = 5;

impl RelativePathType {
    fn from_raw(value: u32) -> Self {
        match value {
            0 => Self::Missing,
            1 => Self::ProjectRelative,
            PROJECT_FOLDER_PATH_TYPE => Self::ProjectFolder,
            INSTALLATION_PATH_TYPE => Self::Installation,
            6 => Self::UserLibrary,
            other => Self::Other(other),
        }
    }
}

/// What we intend to do with a referenced file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RefClass {
    /// Already inside the project folder — commit it where it lies.
    InFolder,
    /// Outside the project folder. Vendored into `Samples/Imported/` so the
    /// project opens on a machine that has never seen the original path.
    External,
    /// The user's own Ableton User Library content — racks, presets. Vendored
    /// for the same reason; note Ableton's own "Collect All and Save" does not
    /// gather these, which is precisely why projects break when shared.
    UserLibrary,
    /// Ships with Live or an installed Live Pack. Recorded, never vendored:
    /// any machine with the same Live install resolves it, and redistributing
    /// Ableton's library content is not ours to do.
    Library,
    /// Neither a relative nor an absolute path — nothing to resolve.
    Unresolvable,
}

impl RefClass {
    /// Whether the referenced bytes should be pulled into the commit.
    pub const fn should_vendor(self) -> bool {
        matches!(self, Self::External | Self::UserLibrary)
    }
}

/// One `FileRef` occurrence, located so it can be rewritten later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetRef {
    /// Index path of child-element positions from the document root down to
    /// the `FileRef`. Stable for the lifetime of one snapshot and the address
    /// [`crate::ableton::rewrite`] writes back through.
    pub location: Vec<usize>,
    pub relative_path: String,
    pub absolute_path: String,
    pub relative_path_type: RelativePathType,
    /// Raw `RelativePathType` value, kept so rewriting can leave untouched
    /// refs bit-identical.
    pub relative_path_type_raw: u32,
    pub live_pack_name: Option<String>,
    pub live_pack_id: Option<String>,
    /// `OriginalFileSize`, when the set recorded a non-zero one.
    pub original_size: Option<u64>,
    /// `OriginalCrc`, when the set recorded a non-zero one.
    pub original_crc: Option<u32>,
    pub class: RefClass,
}

impl AssetRef {
    /// File name implied by the reference, preferring the relative path.
    ///
    /// Both fields use forward slashes even for Windows volumes (`E:/…`), so
    /// this splits on `/` and `\` directly rather than going through
    /// [`std::path`], which would treat a Windows path as one component on
    /// Unix.
    pub fn file_name(&self) -> Option<&str> {
        let source = if self.relative_path.is_empty() {
            &self.absolute_path
        } else {
            &self.relative_path
        };
        source
            .rsplit(['/', '\\'])
            .find(|segment| !segment.is_empty())
    }

    /// Whether this reference resolves to nothing on any machine.
    pub const fn is_unresolvable(&self) -> bool {
        matches!(self.class, RefClass::Unresolvable)
    }

    /// Key identifying the underlying file across its many occurrences.
    ///
    /// A single sample is referenced once per clip that uses it — 25 times for
    /// one loop in a real set — so counts and manifests must collapse on this
    /// rather than on occurrences.
    pub fn dedup_key(&self) -> &str {
        if self.relative_path.is_empty() {
            &self.absolute_path
        } else {
            &self.relative_path
        }
    }
}

/// Collect every `FileRef` in document order.
pub(crate) fn collect(root: &XmlElement) -> Vec<AssetRef> {
    let mut refs = Vec::new();
    walk(root, &mut Vec::new(), &mut refs);
    refs
}

fn walk(element: &XmlElement, location: &mut Vec<usize>, out: &mut Vec<AssetRef>) {
    if element.tag == "FileRef" {
        out.push(read_file_ref(element, location.clone()));
        // A FileRef never nests another; stop descending.
        return;
    }
    for (index, child) in element.child_elements().enumerate() {
        location.push(index);
        walk(child, location, out);
        location.pop();
    }
}

fn read_file_ref(element: &XmlElement, location: Vec<usize>) -> AssetRef {
    let relative_path = read_relative_path(element);
    let absolute_path = element.child_value("Path").unwrap_or_default().to_owned();
    let relative_path_type_raw = element
        .child_value("RelativePathType")
        .and_then(|value| value.parse::<u32>().ok())
        .unwrap_or(0);
    let relative_path_type = RelativePathType::from_raw(relative_path_type_raw);
    let live_pack_name = non_empty(element.child_value("LivePackName"));
    let live_pack_id = non_empty(element.child_value("LivePackId"));

    let class = classify(
        relative_path_type,
        &relative_path,
        &absolute_path,
        live_pack_name.is_some(),
    );

    AssetRef {
        location,
        relative_path,
        absolute_path,
        relative_path_type,
        relative_path_type_raw,
        live_pack_name,
        live_pack_id,
        original_size: read_positive(element, "OriginalFileSize"),
        original_crc: read_positive(element, "OriginalCrc").map(|value| value as u32),
        class,
    }
}

/// Read `RelativePath`, tolerating both encodings Ableton has shipped.
///
/// Live 9 and earlier nested a list of `<RelativePathElement Dir="…" />`
/// children; Live 10+ writes a flat `Value` attribute.
fn read_relative_path(element: &XmlElement) -> String {
    let Some(node) = element.child("RelativePath") else {
        return String::new();
    };
    if let Some(value) = node.attribute("Value") {
        return value.to_owned();
    }
    let segments: Vec<&str> = node
        .descendants()
        .filter(|descendant| descendant.tag == "RelativePathElement")
        .filter_map(|descendant| descendant.attribute("Dir"))
        .filter(|dir| !dir.is_empty())
        .collect();
    segments.join("/")
}

fn classify(
    relative_path_type: RelativePathType,
    relative_path: &str,
    absolute_path: &str,
    has_live_pack: bool,
) -> RefClass {
    if relative_path.is_empty() && absolute_path.is_empty() {
        return RefClass::Unresolvable;
    }
    match relative_path_type {
        RelativePathType::Installation => RefClass::Library,
        RelativePathType::UserLibrary => RefClass::UserLibrary,
        RelativePathType::ProjectFolder => RefClass::InFolder,
        RelativePathType::ProjectRelative => {
            // Type 1 permits `../` — an escaping path leaves the folder and
            // has to be vendored; a contained one is already in place.
            if escapes_project_folder(relative_path) {
                RefClass::External
            } else {
                RefClass::InFolder
            }
        }
        // `Missing` with a path present, or a discriminant we do not model:
        // fall back to shape. A Live Pack name is a strong library signal.
        RelativePathType::Missing | RelativePathType::Other(_) => {
            if has_live_pack || looks_like_library_path(absolute_path) {
                RefClass::Library
            } else if relative_path.is_empty() || escapes_project_folder(relative_path) {
                RefClass::External
            } else {
                RefClass::InFolder
            }
        }
    }
}

/// Whether an absolute path points into a Live installation's library.
///
/// Only consulted when `RelativePathType` gives no answer. Real sets carry
/// device presets whose absolute path was recorded on whichever machine the
/// preset came from — a Live 11 Core Library path from another user's Mac, for
/// instance. Those are library content wherever they came from: they resolve
/// from any comparable Live install, and vendoring them would both fail (the
/// path does not exist here) and redistribute Ableton's own content.
///
/// Erring toward `Library` is the safe direction — it only means "do not
/// vendor", and the reference still resolves normally when the file is present.
fn looks_like_library_path(absolute_path: &str) -> bool {
    absolute_path.contains("Core Library") || absolute_path.contains("/Live Packs/")
}

/// Whether a project-relative path leaves the project folder.
///
/// Resolved by counting segments rather than with [`std::path`], because these
/// strings are Ableton's own forward-slash form regardless of host OS.
fn escapes_project_folder(relative_path: &str) -> bool {
    if relative_path.is_empty() {
        return false;
    }
    // An absolute or drive-qualified path is not folder-relative at all.
    if relative_path.starts_with('/') || has_windows_drive(relative_path) {
        return true;
    }
    let mut depth: i32 = 0;
    for segment in relative_path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            _ => depth += 1,
        }
    }
    false
}

/// Whether `path` starts with a Windows drive qualifier such as `E:/`.
pub(crate) fn has_windows_drive(path: &str) -> bool {
    let mut chars = path.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && matches!(chars.next(), Some(':'))
        && matches!(chars.next(), Some('/' | '\\'))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value.filter(|value| !value.is_empty()).map(str::to_owned)
}

/// Read a child scalar, treating Ableton's `0` placeholder as absent.
fn read_positive(element: &XmlElement, tag: &str) -> Option<u64> {
    element
        .child_value(tag)
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ableton::test_support::parse_xml;

    fn file_ref(relative_path_type: u32, relative_path: &str, path: &str) -> String {
        format!(
            r#"<FileRef>
                <RelativePathType Value="{relative_path_type}" />
                <RelativePath Value="{relative_path}" />
                <Path Value="{path}" />
                <Type Value="1" />
                <LivePackName Value="" />
                <LivePackId Value="" />
                <OriginalFileSize Value="0" />
                <OriginalCrc Value="0" />
            </FileRef>"#
        )
    }

    fn classify_one(relative_path_type: u32, relative_path: &str, path: &str) -> RefClass {
        let xml = format!(
            "<Root>{}</Root>",
            file_ref(relative_path_type, relative_path, path)
        );
        let root = parse_xml(&xml);
        collect(&root)[0].class
    }

    #[test]
    fn relative_path_type_should_drive_classification() {
        // The five discriminants seen in a real Live 12 set.
        assert_eq!(classify_one(0, "", ""), RefClass::Unresolvable);
        assert_eq!(
            classify_one(1, "../../samples/break.wav", "E:/samples/break.wav"),
            RefClass::External
        );
        assert_eq!(
            classify_one(3, "Samples/Processed/break.wav", "/p/Samples/break.wav"),
            RefClass::InFolder
        );
        assert_eq!(
            classify_one(5, "Devices/Audio Effects/EQ Eight", "/l/EQ Eight"),
            RefClass::Library
        );
        assert_eq!(
            classify_one(6, "Presets/Instruments/rack.adg", "C:/u/rack.adg"),
            RefClass::UserLibrary
        );
    }

    #[test]
    fn project_relative_type_should_split_on_whether_it_escapes() {
        // Type 1 covers both cases; only the escaping form needs vendoring.
        assert_eq!(
            classify_one(1, "Samples/Recorded/take.wav", ""),
            RefClass::InFolder
        );
        assert_eq!(classify_one(1, "../outside.wav", ""), RefClass::External);
        // `a/../b` nets out inside the folder despite containing `..`.
        assert_eq!(classify_one(1, "a/../b.wav", ""), RefClass::InFolder);
    }

    #[test]
    fn windows_drive_paths_should_count_as_escaping() {
        assert!(escapes_project_folder("E:/Music/break.wav"));
        assert!(has_windows_drive("C:/Users/jake"));
        assert!(has_windows_drive(r"C:\Users\jake"));
        assert!(!has_windows_drive("Samples/break.wav"));
        // A colon later in the string is not a drive qualifier.
        assert!(!has_windows_drive("Samples/my:file.wav"));
    }

    #[test]
    fn only_vendorable_classes_should_be_collected() {
        assert!(RefClass::External.should_vendor());
        assert!(RefClass::UserLibrary.should_vendor());
        assert!(!RefClass::Library.should_vendor());
        assert!(!RefClass::InFolder.should_vendor());
        assert!(!RefClass::Unresolvable.should_vendor());
    }

    #[test]
    fn empty_file_refs_should_be_collected_as_unresolvable() {
        // A real set carried 14 of these; they must never fail a commit.
        let xml = format!(
            "<Root>{}{}</Root>",
            file_ref(0, "", ""),
            file_ref(0, "", "")
        );
        let refs = collect(&parse_xml(&xml));
        assert_eq!(refs.len(), 2);
        assert!(refs.iter().all(AssetRef::is_unresolvable));
        assert!(refs.iter().all(|asset| asset.file_name().is_none()));
    }

    #[test]
    fn file_name_should_split_on_ableton_forward_slashes() {
        let xml = format!(
            "<Root>{}</Root>",
            file_ref(
                1,
                "../../samples/ENEI break.wav",
                "E:/samples/ENEI break.wav"
            )
        );
        let refs = collect(&parse_xml(&xml));
        assert_eq!(refs[0].file_name(), Some("ENEI break.wav"));
    }

    #[test]
    fn file_name_should_fall_back_to_absolute_path() {
        let xml = format!("<Root>{}</Root>", file_ref(0, "", "C:/Presets/rack.adg"));
        let refs = collect(&parse_xml(&xml));
        assert_eq!(refs[0].file_name(), Some("rack.adg"));
    }

    #[test]
    fn zero_size_and_crc_should_read_as_absent() {
        let xml = format!("<Root>{}</Root>", file_ref(3, "Samples/a.wav", ""));
        let refs = collect(&parse_xml(&xml));
        assert_eq!(refs[0].original_size, None);
        assert_eq!(refs[0].original_crc, None);
    }

    #[test]
    fn original_size_should_be_read_when_present() {
        let xml = r#"<Root><FileRef>
            <RelativePathType Value="1" />
            <RelativePath Value="../a.wav" />
            <Path Value="E:/a.wav" />
            <OriginalFileSize Value="5907514" />
            <OriginalCrc Value="42" />
        </FileRef></Root>"#;
        let refs = collect(&parse_xml(xml));
        assert_eq!(refs[0].original_size, Some(5_907_514));
        assert_eq!(refs[0].original_crc, Some(42));
    }

    #[test]
    fn stale_core_library_paths_should_classify_as_library_not_external() {
        // A Live 11 Core Library preset path recorded on someone else's Mac.
        // It cannot be vendored from here and is not ours to redistribute.
        assert_eq!(
            classify_one(
                0,
                "",
                "/Users/nsh/Library/Application Support/Ableton/Live 11 Core Library/Devices/Audio Effects/Simple Delay/Dotted Eighth Note.adv"
            ),
            RefClass::Library
        );
    }

    #[test]
    fn a_bare_absolute_path_should_still_classify_as_external() {
        // No library marker — treat it as something to gather, and let the
        // read attempt decide whether it is actually reachable.
        assert_eq!(
            classify_one(0, "", "/Reverb Default.adv"),
            RefClass::External
        );
    }

    #[test]
    fn dedup_key_should_collapse_repeated_references_to_one_file() {
        let xml = format!(
            "<Root>{}{}</Root>",
            file_ref(1, "../a.wav", "E:/a.wav"),
            file_ref(1, "../a.wav", "E:/a.wav")
        );
        let refs = collect(&parse_xml(&xml));
        assert_eq!(refs[0].dedup_key(), refs[1].dedup_key());
        assert_eq!(refs[0].dedup_key(), "../a.wav");
    }

    #[test]
    fn live_pack_name_should_classify_unknown_types_as_library() {
        let xml = r#"<Root><FileRef>
            <RelativePathType Value="99" />
            <RelativePath Value="Devices/Audio Effects/EQ Eight" />
            <Path Value="" />
            <LivePackName Value="Core Library" />
        </FileRef></Root>"#;
        let refs = collect(&parse_xml(xml));
        assert_eq!(refs[0].class, RefClass::Library);
        assert_eq!(refs[0].relative_path_type, RelativePathType::Other(99));
        assert_eq!(refs[0].live_pack_name.as_deref(), Some("Core Library"));
    }

    #[test]
    fn legacy_nested_relative_path_should_be_joined() {
        // Live 9 and earlier wrote path segments as child elements.
        let xml = r#"<Root><FileRef>
            <RelativePathType Value="3" />
            <RelativePath>
                <RelativePathElement Dir="Samples" />
                <RelativePathElement Dir="Imported" />
            </RelativePath>
            <Path Value="/p/Samples/Imported/a.wav" />
        </FileRef></Root>"#;
        let refs = collect(&parse_xml(xml));
        assert_eq!(refs[0].relative_path, "Samples/Imported");
    }

    #[test]
    fn locations_should_address_each_ref_in_document_order() {
        let xml = format!(
            "<Root><A>{}</A><B><C>{}</C></B></Root>",
            file_ref(3, "Samples/first.wav", ""),
            file_ref(3, "Samples/second.wav", "")
        );
        let refs = collect(&parse_xml(&xml));
        assert_eq!(refs[0].location, vec![0, 0]);
        assert_eq!(refs[1].location, vec![1, 0, 0]);
        assert_eq!(refs[0].file_name(), Some("first.wav"));
        assert_eq!(refs[1].file_name(), Some("second.wav"));
    }
}
