use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::project_format::{PortableSnapshot, XmlElement};
use crate::sample_manifest::AssetKind;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DawprojectAssetRef {
    pub path: String,
    pub external: bool,
    pub embedded: bool,
    pub size: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DawprojectAssetSummary {
    pub referenced: usize,
    pub embedded: usize,
    pub external: usize,
    pub missing: usize,
    pub known_bytes: u64,
}

impl DawprojectAssetSummary {
    pub const fn total(&self) -> usize {
        self.referenced
    }

    pub(crate) fn from_refs(refs: &[DawprojectAssetRef]) -> Self {
        let mut summary = Self {
            referenced: refs.len(),
            ..Self::default()
        };
        for asset in refs {
            if asset.external {
                summary.external += 1;
            } else if asset.embedded {
                summary.embedded += 1;
            } else {
                summary.missing += 1;
            }
            summary.known_bytes += asset.size.unwrap_or(0);
        }
        summary
    }
}

pub(crate) fn collect(portable: &PortableSnapshot) -> Vec<DawprojectAssetRef> {
    let resources: BTreeMap<&str, Option<u64>> = portable
        .resources
        .iter()
        .map(|resource| {
            (
                resource.id.as_str(),
                resource.size.or_else(|| {
                    resource
                        .data
                        .as_deref()
                        .and_then(decoded_base64_size)
                        .map(|size| size as u64)
                }),
            )
        })
        .collect();
    collect_with_resources(&portable.project.root, &resources)
}

pub(crate) fn collect_from_value(
    root: &XmlElement,
    snapshot: &serde_json::Value,
) -> Vec<DawprojectAssetRef> {
    let resources = resource_sizes(snapshot).collect();
    collect_with_resources(root, &resources)
}

fn collect_with_resources(
    root: &XmlElement,
    resources: &BTreeMap<&str, Option<u64>>,
) -> Vec<DawprojectAssetRef> {
    let mut found: BTreeMap<String, DawprojectAssetRef> = BTreeMap::new();

    for file in root.descendants().filter(|element| element.tag == "File") {
        let Some(path) = nonempty_attribute(file, "path") else {
            continue;
        };
        let external = file
            .attribute("external")
            .is_some_and(|value| value.eq_ignore_ascii_case("true") || value == "1");
        let embedded = !external && resources.contains_key(path);
        let size = embedded
            .then(|| resources.get(path).copied().flatten())
            .flatten();
        found.entry(path.to_owned()).or_insert(DawprojectAssetRef {
            path: path.to_owned(),
            external,
            embedded,
            size,
        });
    }

    found.into_values().collect()
}

pub(crate) fn resource_values(snapshot: &serde_json::Value) -> impl Iterator<Item = (&str, &str)> {
    snapshot
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|resource| {
            Some((
                resource.get("id")?.as_str()?,
                resource
                    .get("hash")
                    .or_else(|| resource.get("data"))?
                    .as_str()?,
            ))
        })
}

fn resource_sizes(snapshot: &serde_json::Value) -> impl Iterator<Item = (&str, Option<u64>)> {
    snapshot
        .get("resources")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|resource| {
            let path = resource.get("id")?.as_str()?;
            let size = resource
                .get("size")
                .and_then(serde_json::Value::as_u64)
                .or_else(|| {
                    resource
                        .get("data")
                        .and_then(serde_json::Value::as_str)
                        .and_then(decoded_base64_size)
                        .map(|size| size as u64)
                });
            Some((path, size))
        })
}

/// Size of canonical padded base64 without allocating the decoded resource.
///
/// Snapshot resources are written by `STANDARD.encode`, so their length is a
/// multiple of four. Returning `None` for anything else keeps metadata reading
/// tolerant of a corrupt future snapshot while restore remains the authority
/// that rejects invalid base64.
fn decoded_base64_size(encoded: &str) -> Option<usize> {
    if encoded.len() % 4 != 0 {
        return None;
    }
    let padding = encoded
        .as_bytes()
        .iter()
        .rev()
        .take_while(|byte| **byte == b'=')
        .count();
    (padding <= 2).then(|| encoded.len() / 4 * 3 - padding)
}

fn nonempty_attribute<'a>(element: &'a XmlElement, name: &str) -> Option<&'a str> {
    element.attribute(name).filter(|value| !value.is_empty())
}

pub(crate) fn classify(path: &str) -> AssetKind {
    let extension = Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if matches!(
        extension.to_ascii_lowercase().as_str(),
        "wav" | "wave" | "aif" | "aiff" | "flac" | "mp3" | "ogg" | "m4a"
    ) {
        AssetKind::Sample
    } else {
        AssetKind::Other
    }
}
