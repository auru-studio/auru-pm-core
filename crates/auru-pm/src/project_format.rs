//! Canonical project-file adapters for project-management snapshots.
//!
//! Auru snapshots are already JSON and stay in their native shape. External
//! project files are normalized into deterministic JSON before they enter the
//! content-addressed store:
//!
//! - DAWproject is a ZIP containing `project.xml`, optional `metadata.xml`, and
//!   opaque media/plugin-state entries.
//! - Ableton Live Sets (`.als`) are gzip-compressed XML.
//!
//! XML is represented as an ordered tree. Attributes are sorted, insignificant
//! formatting whitespace is discarded, and element IDs are copied to a
//! top-level `id` field. That last detail lets the existing JSON three-way merge
//! match tracks, devices, and clips by identity instead of array position.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path};

use base64::Engine as _;
use flate2::Compression;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use quick_xml::events::{BytesCData, BytesDecl, BytesEnd, BytesStart, BytesText, Event};
use quick_xml::{Reader, Writer};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Error, Result};

const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
const MAX_XML_BYTES: u64 = 256 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 100_000;
const MAX_ARCHIVE_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Project-file formats that can be normalized for Auru project management.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectFormat {
    /// Native `.auru` JSON.
    Auru,
    /// Open DAWproject `.dawproject` ZIP container.
    Dawproject,
    /// Ableton Live Set `.als` gzip-compressed XML.
    AbletonLiveSet,
}

impl ProjectFormat {
    /// Detect a supported format from a path extension.
    pub fn from_path(path: &Path) -> Option<Self> {
        let extension = path.extension()?.to_str()?;
        if extension.eq_ignore_ascii_case("auru") {
            Some(Self::Auru)
        } else if extension.eq_ignore_ascii_case("dawproject") {
            Some(Self::Dawproject)
        } else if extension.eq_ignore_ascii_case("als") {
            Some(Self::AbletonLiveSet)
        } else {
            None
        }
    }

    /// Preferred extension without a leading dot.
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Auru => "auru",
            Self::Dawproject => "dawproject",
            Self::AbletonLiveSet => "als",
        }
    }

    fn detect(path: &Path, source: &[u8]) -> Result<Self> {
        if let Some(format) = Self::from_path(path) {
            return Ok(format);
        }

        let first_non_whitespace = source
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace());
        if source.starts_with(&[0x1f, 0x8b]) {
            Ok(Self::AbletonLiveSet)
        } else if source.starts_with(b"PK\x03\x04")
            || source.starts_with(b"PK\x05\x06")
            || source.starts_with(b"PK\x07\x08")
        {
            Ok(Self::Dawproject)
        } else if matches!(first_non_whitespace, Some(b'{') | Some(b'[')) {
            Ok(Self::Auru)
        } else {
            Err(Error::ProjectFormat(format!(
                "unsupported project file '{}'; expected .auru, .dawproject, or .als",
                path.display()
            )))
        }
    }
}

impl fmt::Display for ProjectFormat {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Auru => "Auru",
            Self::Dawproject => "DAWproject",
            Self::AbletonLiveSet => "Ableton Live Set",
        })
    }
}

/// A project normalized into canonical JSON bytes for the PM snapshot CAS.
///
/// Use [`Self::load`] for a project on disk, pass [`Self::as_bytes`] to the
/// existing push APIs, and use [`Self::restore_to_path`] when checking out an
/// old version.
///
/// ```
/// use auru_pm::{ProjectFormat, ProjectSnapshot};
///
/// let snapshot = ProjectSnapshot::from_source_bytes(
///     ProjectFormat::Auru,
///     br#"{ "version": 8, "channels": [] }"#,
/// )?;
/// assert_eq!(snapshot.format(), ProjectFormat::Auru);
/// assert!(snapshot.as_bytes().starts_with(b"{"));
/// # Ok::<(), auru_pm::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct ProjectSnapshot {
    format: ProjectFormat,
    canonical_bytes: Vec<u8>,
}

impl ProjectSnapshot {
    /// Read and normalize a supported project file.
    pub fn load(path: &Path) -> Result<Self> {
        let source = fs::read(path)?;
        let format = ProjectFormat::detect(path, &source)?;
        Self::from_source_bytes(format, &source)
    }

    /// Normalize source project bytes of a known format.
    pub fn from_source_bytes(format: ProjectFormat, source: &[u8]) -> Result<Self> {
        let value = match format {
            ProjectFormat::Auru => parse_auru(source)?,
            ProjectFormat::Dawproject => encode_dawproject(source)?,
            ProjectFormat::AbletonLiveSet => encode_ableton(source)?,
        };
        Ok(Self {
            format,
            canonical_bytes: canonical_json(value)?,
        })
    }

    /// Validate PM snapshot bytes fetched from the CAS.
    ///
    /// Native Auru JSON has no wrapper and is recognized as Auru. External
    /// snapshots carry an `auru_pm_snapshot` marker and their source format.
    pub fn from_canonical_bytes(bytes: &[u8]) -> Result<Self> {
        let value: Value = serde_json::from_slice(bytes)?;
        let format = portable_snapshot_format(&value)?.unwrap_or(ProjectFormat::Auru);
        if format != ProjectFormat::Auru {
            let snapshot: PortableSnapshot = serde_json::from_value(value.clone())?;
            snapshot.validate()?;
        }
        Ok(Self {
            format,
            canonical_bytes: canonical_json(value)?,
        })
    }

    /// Source format represented by this snapshot.
    pub const fn format(&self) -> ProjectFormat {
        self.format
    }

    /// Canonical JSON stored as the commit's snapshot blob.
    pub fn as_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    /// Consume the snapshot and return its canonical JSON bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.canonical_bytes
    }

    /// Deserialize the external-format wrapper backing this snapshot.
    ///
    /// `Ok(None)` for native Auru, which stores its JSON unwrapped. Readers in
    /// [`crate::ableton`] go through here rather than re-parsing the canonical
    /// bytes themselves.
    pub(crate) fn portable(&self) -> Result<Option<PortableSnapshot>> {
        if self.format == ProjectFormat::Auru {
            return Ok(None);
        }
        let snapshot: PortableSnapshot = serde_json::from_slice(&self.canonical_bytes)?;
        snapshot.validate()?;
        Ok(Some(snapshot))
    }

    /// Rebuild a snapshot from a wrapper whose tree has been edited.
    ///
    /// The one supported reason to edit a normalized tree is repointing an
    /// Ableton `FileRef` at a gathered copy of its file on restore; see
    /// [`crate::ableton::rewrite`]. Re-canonicalizing here means the result is
    /// indistinguishable from a snapshot of the rewritten project.
    pub(crate) fn from_portable(portable: PortableSnapshot) -> Result<Self> {
        portable.validate()?;
        let format = portable.format;
        let value = serde_json::to_value(portable)?;
        Ok(Self {
            format,
            canonical_bytes: canonical_json(value)?,
        })
    }

    /// Reconstruct source project bytes from this canonical snapshot.
    pub fn restore_bytes(&self) -> Result<Vec<u8>> {
        match self.format {
            ProjectFormat::Auru => Ok(self.canonical_bytes.clone()),
            ProjectFormat::Dawproject => {
                let snapshot: PortableSnapshot = serde_json::from_slice(&self.canonical_bytes)?;
                snapshot.validate()?;
                decode_dawproject(&snapshot)
            }
            ProjectFormat::AbletonLiveSet => {
                let snapshot: PortableSnapshot = serde_json::from_slice(&self.canonical_bytes)?;
                snapshot.validate()?;
                decode_ableton(&snapshot)
            }
        }
    }

    /// Reconstruct the source project file at `path`.
    pub fn restore_to_path(&self, path: &Path) -> Result<()> {
        if let Some(destination_format) = ProjectFormat::from_path(path) {
            if destination_format != self.format {
                return Err(Error::ProjectFormat(format!(
                    "cannot restore {} snapshot to '{}'; expected .{}",
                    self.format,
                    path.display(),
                    self.format.extension()
                )));
            }
        }
        fs::write(path, self.restore_bytes()?)?;
        Ok(())
    }
}

/// Read a project file and convert it to canonical PM snapshot JSON.
pub fn snapshot_project(path: &Path) -> Result<ProjectSnapshot> {
    ProjectSnapshot::load(path)
}

/// Reconstruct a project file from canonical PM snapshot JSON.
pub fn restore_project(snapshot_bytes: &[u8], path: &Path) -> Result<ProjectFormat> {
    let snapshot = ProjectSnapshot::from_canonical_bytes(snapshot_bytes)?;
    snapshot.restore_to_path(path)?;
    Ok(snapshot.format())
}

/// Wrapper the external formats normalize into.
///
/// Visible to the crate so the format-specific readers in [`crate::ableton`]
/// can walk — and, on restore, rewrite — the XML tree without re-deriving it
/// from raw JSON.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct PortableSnapshot {
    pub(crate) auru_pm_snapshot: u32,
    pub(crate) format: ProjectFormat,
    pub(crate) project: XmlDocument,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) metadata: Option<XmlDocument>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) resources: Vec<ArchiveResource>,
}

impl PortableSnapshot {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.auru_pm_snapshot != SNAPSHOT_SCHEMA_VERSION {
            return Err(Error::ProjectFormat(format!(
                "unsupported external snapshot schema {}; expected {}",
                self.auru_pm_snapshot, SNAPSHOT_SCHEMA_VERSION
            )));
        }
        if self.format == ProjectFormat::Auru {
            return Err(Error::ProjectFormat(
                "external snapshot wrapper cannot declare native Auru format".to_owned(),
            ));
        }
        self.project.validate()?;
        if let Some(metadata) = &self.metadata {
            metadata.validate()?;
        }
        let mut paths = BTreeSet::new();
        for resource in &self.resources {
            validate_archive_path(&resource.id)?;
            if !paths.insert(resource.id.as_str()) {
                return Err(Error::ProjectFormat(format!(
                    "duplicate DAWproject archive entry '{}'",
                    resource.id
                )));
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct ArchiveResource {
    /// Archive path doubles as the stable array identity for three-way merge.
    pub(crate) id: String,
    pub(crate) data: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct XmlDocument {
    pub(crate) root: XmlElement,
}

impl XmlDocument {
    pub(crate) fn parse(xml: &[u8], label: &str) -> Result<Self> {
        if xml.len() as u64 > MAX_XML_BYTES {
            return Err(Error::ProjectFormat(format!(
                "{label} exceeds the {} MiB XML limit",
                MAX_XML_BYTES / (1024 * 1024)
            )));
        }

        let mut reader = Reader::from_reader(xml);
        reader.config_mut().check_end_names = true;
        let mut buffer = Vec::new();
        let mut stack: Vec<XmlElement> = Vec::new();
        let mut root: Option<XmlElement> = None;

        loop {
            match reader.read_event_into(&mut buffer) {
                Ok(Event::Start(event)) => {
                    stack.push(XmlElement::from_start(&reader, &event)?);
                }
                Ok(Event::Empty(event)) => {
                    let element = XmlElement::from_start(&reader, &event)?;
                    append_element(&mut stack, &mut root, element)?;
                }
                Ok(Event::End(_)) => {
                    let element = stack.pop().ok_or_else(|| {
                        Error::ProjectFormat(format!("{label} has an unmatched closing tag"))
                    })?;
                    append_element(&mut stack, &mut root, element)?;
                }
                Ok(Event::Text(event)) => {
                    let text = event
                        .unescape()
                        .map_err(|error| xml_error(label, error))?
                        .into_owned();
                    if !text.trim().is_empty() {
                        append_content(&mut stack, XmlContent::Text { text }, label)?;
                    }
                }
                Ok(Event::CData(event)) => {
                    let cdata = reader
                        .decoder()
                        .decode(&event)
                        .map_err(|error| xml_error(label, error))?
                        .into_owned();
                    append_content(&mut stack, XmlContent::Cdata { cdata }, label)?;
                }
                Ok(Event::Comment(event)) => {
                    let comment = reader
                        .decoder()
                        .decode(&event)
                        .map_err(|error| xml_error(label, error))?
                        .into_owned();
                    if !stack.is_empty() {
                        append_content(&mut stack, XmlContent::Comment { comment }, label)?;
                    }
                }
                Ok(Event::Decl(_) | Event::PI(_) | Event::DocType(_)) => {}
                Ok(Event::Eof) => break,
                Err(error) => return Err(xml_error(label, error)),
            }
            buffer.clear();
        }

        if !stack.is_empty() {
            return Err(Error::ProjectFormat(format!(
                "{label} ended before all XML elements were closed"
            )));
        }
        let document = Self {
            root: root.ok_or_else(|| {
                Error::ProjectFormat(format!("{label} does not contain a root XML element"))
            })?,
        };
        document.validate()?;
        Ok(document)
    }

    fn to_xml(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut writer = Writer::new(Vec::new());
        writer
            .write_event(Event::Decl(BytesDecl::new("1.0", Some("UTF-8"), None)))
            .map_err(|error| xml_error("snapshot XML", error))?;
        self.root.write(&mut writer)?;
        Ok(writer.into_inner())
    }

    fn validate(&self) -> Result<()> {
        self.root.validate()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct XmlElement {
    pub(crate) tag: String,
    /// Duplicated from an `id`/`Id` attribute to give JSON array merge a
    /// stable identity. XML reconstruction uses the original attribute map.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) id: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) attributes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(crate) children: Vec<XmlContent>,
}

impl XmlElement {
    fn from_start(reader: &Reader<&[u8]>, event: &BytesStart<'_>) -> Result<Self> {
        let tag = reader
            .decoder()
            .decode(event.name().as_ref())
            .map_err(|error| xml_error("XML element name", error))?
            .into_owned();
        let mut attributes = BTreeMap::new();
        for attribute in event.attributes() {
            let attribute = attribute.map_err(|error| xml_error("XML attribute", error))?;
            let key = reader
                .decoder()
                .decode(attribute.key.as_ref())
                .map_err(|error| xml_error("XML attribute name", error))?
                .into_owned();
            let value = attribute
                .decode_and_unescape_value(reader.decoder())
                .map_err(|error| xml_error("XML attribute value", error))?
                .into_owned();
            if attributes.insert(key.clone(), value).is_some() {
                return Err(Error::ProjectFormat(format!(
                    "XML element '{tag}' contains duplicate attribute '{key}'"
                )));
            }
        }
        let id = attributes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case("id"))
            .map(|(_, value)| value.clone());
        Ok(Self {
            tag,
            id,
            attributes,
            children: Vec::new(),
        })
    }

    fn write(&self, writer: &mut Writer<Vec<u8>>) -> Result<()> {
        let mut start = BytesStart::new(self.tag.as_str());
        for (key, value) in &self.attributes {
            start.push_attribute((key.as_str(), value.as_str()));
        }

        if self.children.is_empty() {
            writer
                .write_event(Event::Empty(start))
                .map_err(|error| xml_error("snapshot XML", error))?;
            return Ok(());
        }

        writer
            .write_event(Event::Start(start))
            .map_err(|error| xml_error("snapshot XML", error))?;
        for child in &self.children {
            match child {
                XmlContent::Element(element) => element.write(writer)?,
                XmlContent::Text { text } => writer
                    .write_event(Event::Text(BytesText::new(text)))
                    .map_err(|error| xml_error("snapshot XML text", error))?,
                XmlContent::Cdata { cdata } => {
                    for section in BytesCData::escaped(cdata) {
                        writer
                            .write_event(Event::CData(section))
                            .map_err(|error| xml_error("snapshot XML CDATA", error))?;
                    }
                }
                XmlContent::Comment { comment } => writer
                    .write_event(Event::Comment(BytesText::new(comment)))
                    .map_err(|error| xml_error("snapshot XML comment", error))?,
            }
        }
        writer
            .write_event(Event::End(BytesEnd::new(self.tag.as_str())))
            .map_err(|error| xml_error("snapshot XML", error))?;
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        validate_xml_name(&self.tag)?;
        for key in self.attributes.keys() {
            validate_xml_name(key)?;
        }
        if let Some(id) = &self.id {
            let attribute_id = self
                .attributes
                .iter()
                .find(|(key, _)| key.eq_ignore_ascii_case("id"))
                .map(|(_, value)| value);
            if attribute_id != Some(id) {
                return Err(Error::ProjectFormat(format!(
                    "XML element '{}' has inconsistent merge identity",
                    self.tag
                )));
            }
        }
        for child in &self.children {
            if let XmlContent::Element(element) = child {
                element.validate()?;
            }
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub(crate) enum XmlContent {
    Element(XmlElement),
    Text { text: String },
    Cdata { cdata: String },
    Comment { comment: String },
}

/// Navigation over the normalized tree.
///
/// Ableton encodes nearly every scalar as `<Tag Value="…" />`, so
/// [`Self::child_value`] covers most reads. Tag alternatives matter because
/// Live 12 renamed `MasterTrack` to `MainTrack` — see [`Self::child_any`].
impl XmlElement {
    /// Value of `key`, matched case-sensitively.
    pub(crate) fn attribute(&self, key: &str) -> Option<&str> {
        self.attributes.get(key).map(String::as_str)
    }

    /// Direct child elements in document order, skipping text and comments.
    pub(crate) fn child_elements(&self) -> impl Iterator<Item = &Self> {
        self.children.iter().filter_map(|child| match child {
            XmlContent::Element(element) => Some(element),
            _ => None,
        })
    }

    /// First direct child element named `tag`.
    pub(crate) fn child(&self, tag: &str) -> Option<&Self> {
        self.child_elements().find(|element| element.tag == tag)
    }

    /// First direct child element matching any of `tags`, preferring earlier
    /// entries. Use for tags Ableton has renamed across Live versions.
    pub(crate) fn child_any(&self, tags: &[&str]) -> Option<&Self> {
        tags.iter().find_map(|tag| self.child(tag))
    }

    /// The `Value` attribute of the direct child named `tag` — Ableton's
    /// near-universal scalar shape.
    pub(crate) fn child_value(&self, tag: &str) -> Option<&str> {
        self.child(tag)?.attribute("Value")
    }

    /// Resolve a `/`-separated chain of direct child tags.
    pub(crate) fn resolve(&self, path: &str) -> Option<&Self> {
        path.split('/')
            .try_fold(self, |element, tag| element.child(tag))
    }

    /// Direct child elements in document order, mutably.
    pub(crate) fn child_elements_mut(&mut self) -> impl Iterator<Item = &mut Self> {
        self.children.iter_mut().filter_map(|child| match child {
            XmlContent::Element(element) => Some(element),
            _ => None,
        })
    }

    /// Set the `Value` attribute of the direct child named `tag`, adding the
    /// child if the set does not already have one.
    ///
    /// Writing through this rather than by hand keeps the duplicated `id`
    /// field consistent with the attribute map — see [`Self::from_start`] for
    /// why that duplication exists.
    pub(crate) fn set_child_value(&mut self, tag: &str, value: impl Into<String>) {
        let value = value.into();
        if let Some(child) = self.children.iter_mut().find_map(|child| match child {
            XmlContent::Element(element) if element.tag == tag => Some(element),
            _ => None,
        }) {
            child.attributes.insert("Value".to_owned(), value);
            return;
        }
        let mut created = Self {
            tag: tag.to_owned(),
            id: None,
            attributes: BTreeMap::new(),
            children: Vec::new(),
        };
        created.attributes.insert("Value".to_owned(), value);
        self.children.push(XmlContent::Element(created));
    }

    /// Depth-first traversal including `self`.
    pub(crate) fn descendants(&self) -> Descendants<'_> {
        Descendants { stack: vec![self] }
    }
}

/// Depth-first iterator produced by [`XmlElement::descendants`].
pub(crate) struct Descendants<'a> {
    stack: Vec<&'a XmlElement>,
}

impl<'a> Iterator for Descendants<'a> {
    type Item = &'a XmlElement;

    fn next(&mut self) -> Option<Self::Item> {
        let element = self.stack.pop()?;
        // Push in reverse so siblings pop back in document order.
        self.stack.extend(
            element
                .child_elements()
                .collect::<Vec<_>>()
                .into_iter()
                .rev(),
        );
        Some(element)
    }
}

fn parse_auru(source: &[u8]) -> Result<Value> {
    serde_json::from_slice(source).map_err(Error::from)
}

fn encode_ableton(source: &[u8]) -> Result<Value> {
    let xml = if source.starts_with(&[0x1f, 0x8b]) {
        read_limited(
            GzDecoder::new(Cursor::new(source)),
            MAX_XML_BYTES,
            "Ableton Live Set XML",
        )?
    } else {
        if source.len() as u64 > MAX_XML_BYTES {
            return Err(Error::ProjectFormat(format!(
                "Ableton Live Set XML exceeds the {} MiB limit",
                MAX_XML_BYTES / (1024 * 1024)
            )));
        }
        source.to_vec()
    };
    let snapshot = PortableSnapshot {
        auru_pm_snapshot: SNAPSHOT_SCHEMA_VERSION,
        format: ProjectFormat::AbletonLiveSet,
        project: XmlDocument::parse(&xml, "Ableton Live Set")?,
        metadata: None,
        resources: Vec::new(),
    };
    serde_json::to_value(snapshot).map_err(Error::from)
}

fn decode_ableton(snapshot: &PortableSnapshot) -> Result<Vec<u8>> {
    if snapshot.format != ProjectFormat::AbletonLiveSet {
        return Err(Error::ProjectFormat(format!(
            "expected Ableton snapshot, found {}",
            snapshot.format
        )));
    }
    let xml = snapshot.project.to_xml()?;
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&xml)?;
    encoder.finish().map_err(Error::from)
}

fn encode_dawproject(source: &[u8]) -> Result<Value> {
    let reader = Cursor::new(source);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|error| Error::ProjectFormat(format!("invalid DAWproject ZIP: {error}")))?;
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(Error::ProjectFormat(format!(
            "DAWproject contains too many archive entries ({} > {MAX_ARCHIVE_ENTRIES})",
            archive.len()
        )));
    }

    let mut project = None;
    let mut metadata = None;
    let mut resources = Vec::new();
    let mut paths = BTreeSet::new();

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| Error::ProjectFormat(format!("invalid ZIP entry: {error}")))?;
        if entry.is_dir() {
            continue;
        }
        let path = entry.name().to_owned();
        validate_archive_path(&path)?;
        if !paths.insert(path.clone()) {
            return Err(Error::ProjectFormat(format!(
                "duplicate DAWproject archive entry '{path}'"
            )));
        }

        let limit = if path == "project.xml" || path == "metadata.xml" {
            MAX_XML_BYTES
        } else {
            MAX_ARCHIVE_ENTRY_BYTES
        };
        if entry.size() > limit {
            return Err(Error::ProjectFormat(format!(
                "DAWproject entry '{path}' exceeds the {} MiB limit",
                limit / (1024 * 1024)
            )));
        }
        let bytes = read_limited(&mut entry, limit, &format!("DAWproject entry '{path}'"))?;

        match path.as_str() {
            "project.xml" => {
                project = Some(XmlDocument::parse(&bytes, "DAWproject project.xml")?);
            }
            "metadata.xml" => {
                metadata = Some(XmlDocument::parse(&bytes, "DAWproject metadata.xml")?);
            }
            _ => resources.push(ArchiveResource {
                id: path,
                data: base64::engine::general_purpose::STANDARD.encode(bytes),
            }),
        }
    }

    resources.sort_by(|left, right| left.id.cmp(&right.id));
    let snapshot = PortableSnapshot {
        auru_pm_snapshot: SNAPSHOT_SCHEMA_VERSION,
        format: ProjectFormat::Dawproject,
        project: project.ok_or_else(|| {
            Error::ProjectFormat("DAWproject archive is missing project.xml".to_owned())
        })?,
        metadata,
        resources,
    };
    serde_json::to_value(snapshot).map_err(Error::from)
}

fn decode_dawproject(snapshot: &PortableSnapshot) -> Result<Vec<u8>> {
    if snapshot.format != ProjectFormat::Dawproject {
        return Err(Error::ProjectFormat(format!(
            "expected DAWproject snapshot, found {}",
            snapshot.format
        )));
    }

    let cursor = Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);

    archive
        .start_file("project.xml", options)
        .map_err(|error| Error::ProjectFormat(format!("create DAWproject ZIP: {error}")))?;
    archive.write_all(&snapshot.project.to_xml()?)?;

    if let Some(metadata) = &snapshot.metadata {
        archive
            .start_file("metadata.xml", options)
            .map_err(|error| Error::ProjectFormat(format!("create DAWproject ZIP: {error}")))?;
        archive.write_all(&metadata.to_xml()?)?;
    }

    for resource in &snapshot.resources {
        validate_archive_path(&resource.id)?;
        let data = base64::engine::general_purpose::STANDARD
            .decode(&resource.data)
            .map_err(|error| {
                Error::ProjectFormat(format!(
                    "invalid base64 for DAWproject entry '{}': {error}",
                    resource.id
                ))
            })?;
        if data.len() as u64 > MAX_ARCHIVE_ENTRY_BYTES {
            return Err(Error::ProjectFormat(format!(
                "DAWproject entry '{}' exceeds the {} MiB limit",
                resource.id,
                MAX_ARCHIVE_ENTRY_BYTES / (1024 * 1024)
            )));
        }
        archive
            .start_file(&resource.id, options)
            .map_err(|error| Error::ProjectFormat(format!("create DAWproject ZIP: {error}")))?;
        archive.write_all(&data)?;
    }

    let cursor = archive
        .finish()
        .map_err(|error| Error::ProjectFormat(format!("finalize DAWproject ZIP: {error}")))?;
    Ok(cursor.into_inner())
}

fn portable_snapshot_format(value: &Value) -> Result<Option<ProjectFormat>> {
    let Some(marker) = value.get("auru_pm_snapshot") else {
        return Ok(None);
    };
    let marker = marker.as_u64().ok_or_else(|| {
        Error::ProjectFormat("auru_pm_snapshot marker must be an integer".to_owned())
    })?;
    if marker != u64::from(SNAPSHOT_SCHEMA_VERSION) {
        return Err(Error::ProjectFormat(format!(
            "unsupported external snapshot schema {marker}; expected {SNAPSHOT_SCHEMA_VERSION}"
        )));
    }
    let format = value
        .get("format")
        .ok_or_else(|| Error::ProjectFormat("external snapshot is missing format".to_owned()))?;
    serde_json::from_value(format.clone())
        .map(Some)
        .map_err(Error::from)
}

fn canonical_json(mut value: Value) -> Result<Vec<u8>> {
    sort_json_objects(&mut value);
    serde_json::to_vec(&value).map_err(Error::from)
}

fn sort_json_objects(value: &mut Value) {
    match value {
        Value::Object(object) => {
            for child in object.values_mut() {
                sort_json_objects(child);
            }
            object.sort_keys();
        }
        Value::Array(values) => {
            for child in values {
                sort_json_objects(child);
            }
        }
        _ => {}
    }
}

fn append_element(
    stack: &mut [XmlElement],
    root: &mut Option<XmlElement>,
    element: XmlElement,
) -> Result<()> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(XmlContent::Element(element));
    } else if root.replace(element).is_some() {
        return Err(Error::ProjectFormat(
            "XML document contains more than one root element".to_owned(),
        ));
    }
    Ok(())
}

fn append_content(stack: &mut [XmlElement], content: XmlContent, label: &str) -> Result<()> {
    let parent = stack.last_mut().ok_or_else(|| {
        Error::ProjectFormat(format!("{label} contains text outside its root element"))
    })?;
    parent.children.push(content);
    Ok(())
}

fn read_limited(reader: impl Read, limit: u64, label: &str) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(Error::ProjectFormat(format!(
            "{label} exceeds the {} MiB limit",
            limit / (1024 * 1024)
        )));
    }
    Ok(bytes)
}

fn validate_archive_path(path: &str) -> Result<()> {
    if path.is_empty() || path.contains('\\') {
        return Err(Error::ProjectFormat(format!(
            "unsafe DAWproject archive path '{path}'"
        )));
    }
    let valid = Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)));
    if !valid {
        return Err(Error::ProjectFormat(format!(
            "unsafe DAWproject archive path '{path}'"
        )));
    }
    Ok(())
}

fn validate_xml_name(name: &str) -> Result<()> {
    let mut characters = name.chars();
    let valid_start = characters
        .next()
        .is_some_and(|character| character.is_alphabetic() || matches!(character, '_' | ':'));
    let valid_rest = characters
        .all(|character| character.is_alphanumeric() || matches!(character, '_' | ':' | '-' | '.'));
    if !valid_start || !valid_rest {
        return Err(Error::ProjectFormat(format!(
            "invalid XML name '{name}' in project snapshot"
        )));
    }
    Ok(())
}

fn xml_error(label: &str, error: impl fmt::Display) -> Error {
    Error::ProjectFormat(format!("{label}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAWPROJECT_FIXTURE: &[u8] =
        include_bytes!("../tests/fixtures/interchange/oracle-midi.dawproject");

    const ABLETON_XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<Ableton MajorVersion="5" MinorVersion="12.0_12049" Creator="Ableton Live 12">
  <LiveSet>
    <Tracks>
      <MidiTrack Id="7"><Name><EffectiveName Value="Bass &amp; Lead"/></Name></MidiTrack>
    </Tracks>
  </LiveSet>
</Ableton>"#;

    #[test]
    fn path_detection_should_be_case_insensitive() {
        assert_eq!(
            ProjectFormat::from_path(Path::new("song.DAWPROJECT")),
            Some(ProjectFormat::Dawproject)
        );
        assert_eq!(
            ProjectFormat::from_path(Path::new("song.ALS")),
            Some(ProjectFormat::AbletonLiveSet)
        );
        assert_eq!(ProjectFormat::from_path(Path::new("song.wav")), None);
    }

    #[test]
    fn auru_snapshot_should_canonicalize_object_keys() {
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Auru, br#"{"z":1,"a":2}"#)
            .expect("valid Auru JSON");
        assert_eq!(snapshot.as_bytes(), br#"{"a":2,"z":1}"#);
    }

    #[test]
    fn ableton_snapshot_should_round_trip_semantic_xml() {
        let snapshot = ProjectSnapshot::from_source_bytes(
            ProjectFormat::AbletonLiveSet,
            ABLETON_XML.as_bytes(),
        )
        .expect("valid Ableton XML");
        let restored = snapshot.restore_bytes().expect("Ableton restore");
        let round_trip =
            ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, &restored)
                .expect("restored Ableton set should parse");
        assert_eq!(snapshot.as_bytes(), round_trip.as_bytes());
    }

    #[test]
    fn ableton_snapshot_should_ignore_formatting_only_whitespace() {
        let compact = ABLETON_XML.lines().map(str::trim).collect::<String>();
        let pretty = ProjectSnapshot::from_source_bytes(
            ProjectFormat::AbletonLiveSet,
            ABLETON_XML.as_bytes(),
        )
        .expect("pretty Ableton XML");
        let compact =
            ProjectSnapshot::from_source_bytes(ProjectFormat::AbletonLiveSet, compact.as_bytes())
                .expect("compact Ableton XML");
        assert_eq!(pretty.as_bytes(), compact.as_bytes());
    }

    #[test]
    fn dawproject_fixture_should_round_trip_to_the_same_snapshot() {
        let snapshot =
            ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, DAWPROJECT_FIXTURE)
                .expect("valid DAWproject fixture");
        let restored = snapshot.restore_bytes().expect("DAWproject restore");
        let round_trip = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &restored)
            .expect("restored DAWproject should parse");
        assert_eq!(snapshot.as_bytes(), round_trip.as_bytes());
    }

    #[test]
    fn dawproject_snapshot_should_preserve_opaque_resources() {
        let cursor = Cursor::new(Vec::new());
        let mut archive = zip::ZipWriter::new(cursor);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        archive
            .start_file("project.xml", options)
            .expect("project entry");
        archive
            .write_all(br#"<Project version="1.0"><Structure/></Project>"#)
            .expect("project XML");
        archive
            .start_file("audio/kick.wav", options)
            .expect("audio entry");
        archive.write_all(b"RIFF-test-audio").expect("audio bytes");
        let source = archive.finish().expect("finish ZIP").into_inner();

        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &source)
            .expect("valid DAWproject");
        let restored = snapshot.restore_bytes().expect("restore DAWproject");
        let mut restored =
            zip::ZipArchive::new(Cursor::new(restored)).expect("restored ZIP should open");
        let mut audio = Vec::new();
        restored
            .by_name("audio/kick.wav")
            .expect("restored audio entry")
            .read_to_end(&mut audio)
            .expect("restored audio bytes");
        assert_eq!(audio, b"RIFF-test-audio");
    }

    #[test]
    fn xml_ids_should_be_available_to_identity_based_merge() {
        let snapshot = ProjectSnapshot::from_source_bytes(
            ProjectFormat::AbletonLiveSet,
            ABLETON_XML.as_bytes(),
        )
        .expect("valid Ableton XML");
        let json: Value = serde_json::from_slice(snapshot.as_bytes()).expect("snapshot JSON");
        let encoded = serde_json::to_string(&json).expect("JSON string");
        assert!(encoded.contains(r#""id":"7""#));
        assert!(encoded.contains(r#""Id":"7""#));
    }

    #[test]
    fn mismatched_restore_extension_should_be_rejected() {
        let snapshot = ProjectSnapshot::from_source_bytes(ProjectFormat::Auru, br#"{"version":8}"#)
            .expect("valid Auru JSON");
        let error = snapshot
            .restore_to_path(Path::new("song.als"))
            .expect_err("mismatched extension must fail");
        assert!(error.to_string().contains("cannot restore Auru snapshot"));
    }

    #[test]
    fn archive_paths_should_reject_parent_traversal() {
        let error = validate_archive_path("../audio/kick.wav")
            .expect_err("parent traversal must be rejected");
        assert!(error.to_string().contains("unsafe DAWproject archive path"));
    }
}
