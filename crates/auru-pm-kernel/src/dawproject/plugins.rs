use std::collections::BTreeMap;

use crate::project_format::XmlElement;
use crate::{PluginFormat, PluginId, PluginRef};

pub(crate) fn collect(root: &XmlElement) -> Vec<PluginRef> {
    let mut found: BTreeMap<PluginId, PluginRef> = BTreeMap::new();
    let application = root
        .child("Application")
        .and_then(|element| element.attribute("name"))
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown-exporter");

    for devices in root
        .descendants()
        .filter(|element| element.tag == "Devices")
    {
        for device in devices
            .child_elements()
            .filter_map(|device| read_device(device, application))
        {
            found
                .entry(device.id.clone())
                .and_modify(|existing| existing.instances += 1)
                .or_insert(device);
        }
    }

    let mut plugins: Vec<_> = found.into_values().collect();
    plugins.sort_by(|left, right| {
        left.format
            .cmp(&right.format)
            .then_with(|| left.name.cmp(&right.name))
    });
    plugins
}

fn read_device(device: &XmlElement, application: &str) -> Option<PluginRef> {
    let name = device
        .attribute("deviceName")
        .or_else(|| device.attribute("name"))
        .filter(|name| !name.is_empty())
        .unwrap_or(&device.tag)
        .to_owned();
    let device_id = device
        .attribute("deviceID")
        .filter(|id| !id.is_empty())
        .unwrap_or(&name);

    let (format, id) = match device.tag.as_str() {
        "Vst2Plugin" => (
            PluginFormat::Vst2,
            device_id
                .parse()
                .map(|unique_id| PluginId::Vst2 { unique_id })
                .unwrap_or_else(|_| fallback_id(PluginFormat::Vst2, device_id)),
        ),
        "Vst3Plugin" => (
            PluginFormat::Vst3,
            parse_vst3_id(device_id)
                .map(|tuid| PluginId::Vst3 { tuid })
                .unwrap_or_else(|| fallback_id(PluginFormat::Vst3, device_id)),
        ),
        "ClapPlugin" => (
            PluginFormat::Clap,
            PluginId::Clap {
                plugin_id: device_id.to_owned(),
            },
        ),
        "AuPlugin" => (
            PluginFormat::AudioUnit,
            PluginId::AudioUnit {
                name: device_id.to_owned(),
            },
        ),
        "Device" | "BuiltinDevice" | "Equalizer" | "Compressor" | "NoiseGate" | "Limiter" => (
            PluginFormat::Native,
            PluginId::DawprojectBuiltin {
                application: application.to_owned(),
                device_id: device_id.to_owned(),
            },
        ),
        _ => return None,
    };

    let device_type = match device.attribute("deviceRole") {
        Some("instrument") => Some(1),
        Some("audioFX") | Some("noteFX") => Some(2),
        _ => None,
    };
    Some(PluginRef {
        name,
        format,
        id,
        device_type,
        // `State/@path` points inside the DAWproject archive. It is not an
        // installation hint and must not be tested against this filesystem.
        path: None,
        instances: 1,
    })
}

fn fallback_id(format: PluginFormat, device_id: &str) -> PluginId {
    PluginId::Dawproject {
        format,
        device_id: device_id.to_owned(),
    }
}

/// Convert DAWproject's canonical VST3 UUID text into the same four-word TUID
/// Ableton records. Removing hyphens preserves the 16 bytes in display order.
fn parse_vst3_id(id: &str) -> Option<[u32; 4]> {
    let compact: String = id.chars().filter(|character| *character != '-').collect();
    if compact.len() != 32 || !compact.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    let mut tuid = [0_u32; 4];
    for (index, slot) in tuid.iter_mut().enumerate() {
        let start = index * 8;
        *slot = u32::from_str_radix(&compact[start..start + 8], 16).ok()?;
    }
    Some(tuid)
}
