//! Instrument and effect inventory.
//!
//! Devices hang off `Devices` elements in each track's device chain. A device
//! is either one of Live's own (the element tag *is* the device name — `Eq8`,
//! `Reverb`, `InstrumentGroupDevice`) or a `PluginDevice` wrapping a
//! third-party plugin described by `VstPluginInfo`, `Vst3PluginInfo`, or
//! `AuPluginInfo`.
//!
//! Detecting Live's own devices structurally, rather than against a hardcoded
//! device list, means a set saved by a newer Live version reports its new
//! devices correctly instead of silently dropping them.
//!
//! Identity is what matters downstream: a VST2 `UniqueId` or a VST3 TUID is
//! what [`crate::plugin_registry`] keys on to tell the user where to obtain a
//! plugin they do not have installed. Nothing here touches licensing — the
//! preset state stays in the set and reloads once the user installs and
//! authorizes the plugin themselves.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::project_format::XmlElement;

/// Plugin interface a device is loaded through.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginFormat {
    Vst2,
    Vst3,
    AudioUnit,
    /// Built into the DAW.
    Native,
    /// A hosted plugin whose binary could not be identified.
    Unknown,
}

impl PluginFormat {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Vst2 => "VST2",
            Self::Vst3 => "VST3",
            Self::AudioUnit => "AU",
            Self::Native => "Built-in",
            Self::Unknown => "Plugin",
        }
    }

    /// Whether the plugin is supplied by Ableton rather than a third party.
    pub const fn is_native(self) -> bool {
        matches!(self, Self::Native)
    }
}

/// Stable identity for a plugin, independent of its display name or install path.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginId {
    /// VST2 four-character code packed into a `u32`.
    Vst2 { unique_id: u32 },
    /// VST3 class id: four `u32` fields, written as `Fields.0`–`Fields.3`.
    Vst3 { tuid: [u32; 4] },
    /// Audio Unit. Live records no numeric identity we can rely on, so the
    /// name carries it.
    AudioUnit { name: String },
    /// A Live device, identified by its XML tag.
    Native { device: String },
    /// A plugin identified only by the file it was loaded from.
    ///
    /// FL Studio records no numeric identity for third-party plugins — every
    /// one of them reports the same internal name — so the binary's file name
    /// is all there is. Stored lowercase, and deliberately *without* its
    /// directory: two projects examined loaded the same Serum from
    /// `E:\VST\VST 64 bit` and `C:\Program Files\Common Files\VST2`, so a
    /// full path would identify the machine rather than the plugin.
    Vst2ByFile { file_name: String },
    /// A plugin bundled with FL Studio, identified by its own name.
    ///
    /// Kept apart from [`Self::Native`] so that a stock FL effect and a Live
    /// device that happen to share a name cannot resolve to each other.
    FlNative { device: String },
}

impl fmt::Display for PluginId {
    /// Registry lookup key. Stable across Live versions and host platforms.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Vst2 { unique_id } => write!(formatter, "vst2:{unique_id}"),
            Self::Vst3 { tuid } => write!(
                formatter,
                "vst3:{}-{}-{}-{}",
                tuid[0], tuid[1], tuid[2], tuid[3]
            ),
            Self::AudioUnit { name } => write!(formatter, "au:{name}"),
            Self::Native { device } => write!(formatter, "live:{device}"),
            Self::Vst2ByFile { file_name } => write!(formatter, "vst2file:{file_name}"),
            Self::FlNative { device } => write!(formatter, "fl:{device}"),
        }
    }
}

/// Error returned when a registry key is not a plugin identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsePluginIdError(String);

impl fmt::Display for ParsePluginIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "'{}' is not a plugin identity", self.0)
    }
}

impl std::error::Error for ParsePluginIdError {}

impl std::str::FromStr for PluginId {
    type Err = ParsePluginIdError;

    /// Parse the form [`Display`](fmt::Display) writes.
    ///
    /// This is how plugin registries key their entries: a stable, readable
    /// string that survives being written in a JSON file by hand.
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || ParsePluginIdError(value.to_owned());
        let (scheme, rest) = value.split_once(':').ok_or_else(invalid)?;
        match scheme {
            "vst2" => Ok(Self::Vst2 {
                unique_id: rest.parse().map_err(|_| invalid())?,
            }),
            "vst3" => {
                let mut tuid = [0_u32; 4];
                let mut fields = rest.split('-');
                for slot in &mut tuid {
                    *slot = fields
                        .next()
                        .ok_or_else(invalid)?
                        .parse()
                        .map_err(|_| invalid())?;
                }
                if fields.next().is_some() {
                    return Err(invalid());
                }
                Ok(Self::Vst3 { tuid })
            }
            "au" if !rest.is_empty() => Ok(Self::AudioUnit {
                name: rest.to_owned(),
            }),
            "live" if !rest.is_empty() => Ok(Self::Native {
                device: rest.to_owned(),
            }),
            "vst2file" if !rest.is_empty() => Ok(Self::Vst2ByFile {
                file_name: rest.to_ascii_lowercase(),
            }),
            "fl" if !rest.is_empty() => Ok(Self::FlNative {
                device: rest.to_owned(),
            }),
            _ => Err(invalid()),
        }
    }
}

/// One distinct plugin used by the set, with how many times it appears.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PluginRef {
    pub name: String,
    pub format: PluginFormat,
    pub id: PluginId,
    /// VST3 `DeviceType`: `1` instrument, `2` audio effect.
    pub device_type: Option<u32>,
    /// Install path recorded when the set was saved. Machine-specific — useful
    /// as a hint, never as something to resolve against.
    pub path: Option<String>,
    /// Number of instances across the set.
    pub instances: usize,
}

impl PluginRef {
    /// Whether this needs to be obtained separately to open the project.
    pub const fn is_third_party(&self) -> bool {
        !self.format.is_native()
    }
}

/// Collect distinct plugins, sorted by format then name for a stable listing.
pub(crate) fn collect(root: &XmlElement) -> Vec<PluginRef> {
    let mut found: BTreeMap<PluginId, PluginRef> = BTreeMap::new();

    for devices in root.descendants().filter(|node| node.tag == "Devices") {
        for device in devices.child_elements() {
            if let Some(plugin) = read_device(device) {
                found
                    .entry(plugin.id.clone())
                    .and_modify(|existing| existing.instances += 1)
                    .or_insert(plugin);
            }
        }
    }

    let mut plugins: Vec<PluginRef> = found.into_values().collect();
    plugins.sort_by(|left, right| {
        left.format
            .cmp(&right.format)
            .then_with(|| left.name.cmp(&right.name))
    });
    plugins
}

fn read_device(device: &XmlElement) -> Option<PluginRef> {
    match device.tag.as_str() {
        "PluginDevice" | "AuPluginDevice" => read_hosted_plugin(device),
        // Anything else under `Devices` is one of Live's own.
        tag => Some(PluginRef {
            name: tag.to_owned(),
            format: PluginFormat::Native,
            id: PluginId::Native {
                device: tag.to_owned(),
            },
            device_type: None,
            path: None,
            instances: 1,
        }),
    }
}

/// Read the plugin description nested inside a `PluginDevice`.
fn read_hosted_plugin(device: &XmlElement) -> Option<PluginRef> {
    // `PluginDesc` sits between the device and the format-specific info, but
    // the depth has shifted between Live versions — search rather than walk a
    // fixed path.
    device
        .descendants()
        .find_map(|node| match node.tag.as_str() {
            "VstPluginInfo" => read_vst2(node),
            "Vst3PluginInfo" => read_vst3(node),
            "AuPluginInfo" => read_audio_unit(node),
            _ => None,
        })
}

fn read_vst2(info: &XmlElement) -> Option<PluginRef> {
    let unique_id = info.child_value("UniqueId")?.parse::<u32>().ok()?;
    let name = info
        .child_value("PlugName")
        .filter(|name| !name.is_empty())
        .unwrap_or("Unknown VST2 plugin")
        .to_owned();
    Some(PluginRef {
        name,
        format: PluginFormat::Vst2,
        id: PluginId::Vst2 { unique_id },
        device_type: None,
        path: info
            .child_value("Path")
            .filter(|path| !path.is_empty())
            .map(str::to_owned),
        instances: 1,
    })
}

fn read_vst3(info: &XmlElement) -> Option<PluginRef> {
    let tuid = read_tuid(info)?;
    let name = info
        .child_value("Name")
        .filter(|name| !name.is_empty())
        .unwrap_or("Unknown VST3 plugin")
        .to_owned();
    Some(PluginRef {
        name,
        format: PluginFormat::Vst3,
        id: PluginId::Vst3 { tuid },
        device_type: info
            .child_value("DeviceType")
            .and_then(|value| value.parse::<u32>().ok()),
        path: info
            .child_value("Path")
            .filter(|path| !path.is_empty())
            .map(str::to_owned),
        instances: 1,
    })
}

/// Assemble the four-part VST3 class id.
///
/// ```xml
/// <Uid>
///     <Fields.0 Value="1448297816" />
///     <Fields.1 Value="1718833267" />
///     <Fields.2 Value="1701999981" />
///     <Fields.3 Value="540147712" />
/// </Uid>
/// ```
///
/// Read by field name rather than child position — a set that omits or
/// reorders a field yields `None` instead of a silently wrong identity that
/// would mislead a registry lookup.
fn read_tuid(info: &XmlElement) -> Option<[u32; 4]> {
    let uid = info.child("Uid")?;
    let mut tuid = [0_u32; 4];
    for (index, slot) in tuid.iter_mut().enumerate() {
        *slot = uid
            .child_value(&format!("Fields.{index}"))?
            .parse::<u32>()
            .ok()?;
    }
    Some(tuid)
}

fn read_audio_unit(info: &XmlElement) -> Option<PluginRef> {
    let name = info
        .child_value("Name")
        .or_else(|| info.child_value("PlugName"))
        .filter(|name| !name.is_empty())?
        .to_owned();
    Some(PluginRef {
        name: name.clone(),
        format: PluginFormat::AudioUnit,
        id: PluginId::AudioUnit { name },
        device_type: info
            .child_value("DeviceType")
            .and_then(|value| value.parse::<u32>().ok()),
        path: None,
        instances: 1,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ableton::test_support::parse_xml;

    fn vst2(name: &str, unique_id: u32, path: &str) -> String {
        format!(
            r#"<PluginDevice><PluginDesc><VstPluginInfo>
                <Path Value="{path}" />
                <PlugName Value="{name}" />
                <UniqueId Value="{unique_id}" />
                <VstVersion Value="2400" />
            </VstPluginInfo></PluginDesc></PluginDevice>"#
        )
    }

    fn vst3(name: &str, tuid: [u32; 4], device_type: u32) -> String {
        format!(
            r#"<PluginDevice><PluginDesc><Vst3PluginInfo>
                <Name Value="{name}" />
                <DeviceType Value="{device_type}" />
                <Uid>
                    <Fields.0 Value="{}" />
                    <Fields.1 Value="{}" />
                    <Fields.2 Value="{}" />
                    <Fields.3 Value="{}" />
                </Uid>
            </Vst3PluginInfo></PluginDesc></PluginDevice>"#,
            tuid[0], tuid[1], tuid[2], tuid[3]
        )
    }

    fn in_devices(body: &str) -> XmlElement {
        parse_xml(&format!("<Root><Devices>{body}</Devices></Root>"))
    }

    #[test]
    fn vst2_plugin_should_extract_unique_id_and_name() {
        // Identities taken from a real Live 12 set.
        let root = in_devices(&vst2("Serum_x64", 1_483_109_208, "E:/VSTs/Serum_x64.dll"));
        let plugins = collect(&root);
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "Serum_x64");
        assert_eq!(plugins[0].format, PluginFormat::Vst2);
        assert_eq!(
            plugins[0].id,
            PluginId::Vst2 {
                unique_id: 1_483_109_208
            }
        );
        assert_eq!(plugins[0].path.as_deref(), Some("E:/VSTs/Serum_x64.dll"));
        assert!(plugins[0].is_third_party());
    }

    #[test]
    fn vst3_uid_should_assemble_from_four_fields() {
        let tuid = [1_448_297_816, 1_718_833_267, 1_701_999_981, 540_147_712];
        let root = in_devices(&vst3("Serum 2", tuid, 1));
        let plugins = collect(&root);
        assert_eq!(plugins[0].id, PluginId::Vst3 { tuid });
        assert_eq!(plugins[0].device_type, Some(1));
        assert_eq!(
            plugins[0].id.to_string(),
            "vst3:1448297816-1718833267-1701999981-540147712"
        );
    }

    #[test]
    fn vst3_with_incomplete_uid_should_be_skipped() {
        // A partial id would produce a confidently wrong registry lookup.
        let root = in_devices(
            r#"<PluginDevice><PluginDesc><Vst3PluginInfo>
                <Name Value="Broken" />
                <Uid><Fields.0 Value="1" /><Fields.1 Value="2" /></Uid>
            </Vst3PluginInfo></PluginDesc></PluginDevice>"#,
        );
        assert!(collect(&root).is_empty());
    }

    #[test]
    fn native_devices_should_be_identified_by_tag() {
        let root = in_devices("<Eq8 /><Reverb /><InstrumentGroupDevice />");
        let plugins = collect(&root);
        let names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["Eq8", "InstrumentGroupDevice", "Reverb"]);
        assert!(plugins.iter().all(|p| p.format == PluginFormat::Native));
        assert!(plugins.iter().all(|p| !p.is_third_party()));
    }

    #[test]
    fn unknown_native_devices_should_still_be_reported() {
        // A device from a newer Live than this build knows about.
        let root = in_devices("<SomeFutureDevice />");
        let plugins = collect(&root);
        assert_eq!(plugins[0].name, "SomeFutureDevice");
        assert_eq!(
            plugins[0].id,
            PluginId::Native {
                device: "SomeFutureDevice".to_owned()
            }
        );
    }

    #[test]
    fn repeated_plugins_should_collapse_and_count_instances() {
        // The real set loads Serum 8 times and EQ Eight 13 times.
        let serum = vst2("Serum_x64", 1_483_109_208, "E:/Serum.dll");
        let root = in_devices(&format!("{serum}{serum}{serum}<Eq8 /><Eq8 />"));
        let plugins = collect(&root);
        assert_eq!(plugins.len(), 2);

        let serum = plugins
            .iter()
            .find(|p| p.format == PluginFormat::Vst2)
            .expect("serum present");
        assert_eq!(serum.instances, 3);

        let eq = plugins
            .iter()
            .find(|p| p.name == "Eq8")
            .expect("eq8 present");
        assert_eq!(eq.instances, 2);
    }

    #[test]
    fn plugins_should_be_collected_across_every_device_chain() {
        let root = parse_xml(&format!(
            "<Root><MidiTrack><Devices>{}</Devices></MidiTrack>\
             <AudioTrack><Devices><Eq8 /></Devices></AudioTrack></Root>",
            vst3("Serum 2", [1, 2, 3, 4], 1)
        ));
        let plugins = collect(&root);
        assert_eq!(plugins.len(), 2);
    }

    #[test]
    fn distinct_vst3_plugins_should_not_merge_on_name() {
        // Serum 2 and Serum 2 FX differ only by TUID and DeviceType.
        let instrument = vst3("Serum 2", [1_448_297_816, 1_718_833_267, 1, 2], 1);
        let effect = vst3("Serum 2 FX", [1_448_297_816, 1_718_833_523, 1, 3], 2);
        let root = in_devices(&format!("{instrument}{effect}"));
        let plugins = collect(&root);
        assert_eq!(plugins.len(), 2);
        assert_eq!(plugins[0].device_type, Some(1));
        assert_eq!(plugins[1].device_type, Some(2));
    }

    #[test]
    fn every_identity_should_survive_its_registry_key() {
        // The key is what a registry file is written and maintained by, so a
        // round trip has to be exact — a misparsed id means a plugin the user
        // owns is reported as unrecognised.
        for id in [
            PluginId::Vst2 {
                unique_id: 1_483_109_208,
            },
            PluginId::Vst3 {
                tuid: [1_448_297_816, 1_718_833_267, 1_701_999_981, 540_147_712],
            },
            PluginId::AudioUnit {
                name: "Some AU".to_owned(),
            },
            PluginId::Native {
                device: "Eq8".to_owned(),
            },
        ] {
            let key = id.to_string();
            assert_eq!(
                key.parse::<PluginId>().expect("round trip"),
                id,
                "key {key} did not parse back"
            );
        }
    }

    #[test]
    fn a_malformed_registry_key_should_be_rejected() {
        // Better to report an unusable registry line than to silently key an
        // entry under an identity nothing will ever match.
        for bad in [
            "",
            "vst2:",
            "vst2:not-a-number",
            "vst3:1-2-3",
            "vst3:1-2-3-4-5",
            "au:",
            "live:",
            "something:else",
            "no-scheme",
        ] {
            assert!(
                bad.parse::<PluginId>().is_err(),
                "{bad:?} should not parse as a plugin identity"
            );
        }
    }

    #[test]
    fn plugin_id_should_render_a_stable_registry_key() {
        assert_eq!(
            PluginId::Vst2 {
                unique_id: 1_483_109_208
            }
            .to_string(),
            "vst2:1483109208"
        );
        assert_eq!(
            PluginId::Native {
                device: "Eq8".to_owned()
            }
            .to_string(),
            "live:Eq8"
        );
    }
}
