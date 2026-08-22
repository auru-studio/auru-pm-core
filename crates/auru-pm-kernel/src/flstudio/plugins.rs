//! What instruments and effects an FL Studio project loads.
//!
//! FL identifies plugins quite differently from Ableton, and the difference
//! decides how they can be matched against a registry.
//!
//! Every plugin instance is a run of events observed in this order in real
//! projects:
//!
//! | event | meaning |
//! |---|---|
//! | 201 | internal identity — `""` for the built-in sampler, `Fruity Wrapper` for a hosted plugin, otherwise the stock plugin's own name |
//! | 212 | the instance itself |
//! | 203 | the name the user sees |
//! | 213 | opaque plugin state, which for a hosted plugin contains its file path |
//!
//! So an FL stock plugin has a stable textual identity, while a third-party
//! plugin has **no numeric identity at all** — only the path to the binary
//! that was loaded. Ableton records a VST3 class id; FL records
//! `E:\VST\VST 64 bit\Serum_x64.dll`. Matching therefore keys on the *file
//! name*, never the directory, because the directory says more about the
//! machine that saved the project than about the plugin.
//!
//! > The path is read out of event 213 and **never written back**. Its offsets
//! > within that blob are not modelled, and changing the length of a string
//! > inside plugin state would corrupt it. Identification is safe; rewriting
//! > is not.

use std::collections::BTreeMap;

use crate::ableton::{PluginFormat, PluginId, PluginRef};

use super::events::{Stream, decode_ascii, decode_utf16, uses_utf16};

/// Internal plugin identity.
pub const EVENT_INTERNAL_NAME: u8 = 201;
/// A plugin instance.
pub const EVENT_NEW_PLUGIN: u8 = 212;
/// The name shown to the user.
pub const EVENT_DISPLAY_NAME: u8 = 203;
/// Opaque plugin state.
pub const EVENT_PLUGIN_PARAMS: u8 = 213;

/// The internal name FL gives to any hosted third-party plugin.
///
/// Every VST in a project reports this, which is why it cannot be used as the
/// plugin's identity — the real one is inside the following state blob.
pub const WRAPPER_NAME: &str = "Fruity Wrapper";

/// Extensions that mark a plugin binary inside an opaque state blob.
const BINARY_EXTENSIONS: [&str; 4] = [".dll", ".vst3", ".so", ".component"];

/// Every plugin the project loads, deduplicated, with instance counts.
pub fn collect(stream: &Stream) -> Vec<PluginRef> {
    let utf16 = uses_utf16(stream.major_version());
    let mut found: BTreeMap<String, PluginRef> = BTreeMap::new();

    // Walk instances rather than scanning for names: a name event on its own
    // may be naming a channel rather than a plugin, and counting those would
    // inflate the inventory with things that are not plugins at all.
    let mut internal: Option<String> = None;
    for (index, event) in stream.events.iter().enumerate() {
        match event.id {
            EVENT_INTERNAL_NAME => {
                internal = Some(text(&event.payload, utf16));
            }
            EVENT_NEW_PLUGIN => {
                let Some(kind) = internal.take() else {
                    continue;
                };
                let plugin = describe(stream, index, &kind, utf16);
                found
                    .entry(plugin.id.to_string())
                    .and_modify(|existing| existing.instances += 1)
                    .or_insert(plugin);
            }
            _ => {}
        }
    }

    found.into_values().collect()
}

/// Build one plugin from the events following its instance marker.
fn describe(stream: &Stream, index: usize, internal: &str, utf16: bool) -> PluginRef {
    // The display name and state blob follow the instance, but not
    // immediately, and the run ends at the next instance.
    let mut display = None;
    let mut path = None;
    for event in stream.events[index + 1..].iter() {
        match event.id {
            EVENT_NEW_PLUGIN | EVENT_INTERNAL_NAME => break,
            EVENT_DISPLAY_NAME if display.is_none() => {
                display = Some(text(&event.payload, utf16));
            }
            EVENT_PLUGIN_PARAMS if path.is_none() => {
                path = binary_path(&event.payload);
            }
            _ => {}
        }
    }

    let hosted = internal == WRAPPER_NAME;
    let file_name = path.as_deref().map(file_name_of);

    let (id, format) = match (hosted, file_name) {
        // A hosted plugin whose binary we found: the file name is the only
        // identity FL gives us.
        (true, Some(file)) => (
            PluginId::Vst2ByFile {
                file_name: file.to_ascii_lowercase(),
            },
            format_of(file),
        ),
        // A wrapper we could not see inside. Fall back to the display name so
        // it is still reported rather than silently dropped.
        (true, None) => (
            PluginId::FlNative {
                device: display.clone().unwrap_or_else(|| WRAPPER_NAME.to_owned()),
            },
            PluginFormat::Unknown,
        ),
        // An FL stock plugin, or the built-in sampler when the name is empty.
        _ => (
            PluginId::FlNative {
                device: if internal.is_empty() {
                    "Sampler".to_owned()
                } else {
                    internal.to_owned()
                },
            },
            PluginFormat::Native,
        ),
    };

    // This inventory answers "what does this project need installed", so a
    // plugin is named for what it is, never for what the user called one
    // instance of it. Event 203 on a channel is the *channel's* name — taking
    // it would list an FL stock synth as "Kick" and a sampler as "T".
    let name = match &id {
        PluginId::Vst2ByFile { .. } => file_name
            .map(strip_extension)
            .unwrap_or_else(|| WRAPPER_NAME.to_owned()),
        // A wrapper whose binary could not be read has no better name to
        // offer, so the user's label is the most informative thing left.
        _ if hosted => display.unwrap_or_else(|| WRAPPER_NAME.to_owned()),
        _ if internal.is_empty() => "Sampler".to_owned(),
        _ => internal.to_owned(),
    };

    PluginRef {
        name,
        format,
        id,
        device_type: None,
        path,
        instances: 1,
    }
}

/// Find the plugin binary's path inside an opaque state blob.
///
/// A scan rather than a parse: the layout of plugin state is not documented
/// and differs per plugin, but the path is stored as plain single-byte text.
/// Used only to identify the plugin, so a miss costs a less specific report
/// and nothing else.
fn binary_path(payload: &[u8]) -> Option<String> {
    let mut current = String::new();

    // Runs of printable single-byte characters; anything else ends the run.
    for byte in payload.iter().chain(std::iter::once(&0)) {
        if (0x20..0x7f).contains(byte) {
            current.push(*byte as char);
            continue;
        }
        if let Some(path) = path_within(&current) {
            return Some(path);
        }
        current.clear();
    }
    None
}

/// Extract a plugin path from a run of printable bytes.
///
/// The path is looked for *inside* the run rather than expected to be the
/// whole of it. Neighbouring binary fields routinely fall in the printable
/// range: in a real project the run reads
/// `E:\VST\VST 64 bit\Serum_x64.dll8`, where the trailing `8` is the first
/// byte of the following length. Requiring the run to end at the extension
/// finds nothing at all, and the failure is silent — the plugin is simply
/// reported by whatever the user named the channel.
fn path_within(run: &str) -> Option<String> {
    let lower = run.to_ascii_lowercase();
    let end = BINARY_EXTENSIONS
        .iter()
        .filter_map(|extension| lower.find(extension).map(|at| at + extension.len()))
        .min()?;

    let candidate = &run[start_of_path(&run[..end])..end];
    (candidate.len() >= 5).then(|| candidate.to_owned())
}

/// Where the path begins within a run that may have junk in front of it.
///
/// A drive letter is the reliable anchor on Windows, which is where FL keeps
/// plugins; without one the whole run is taken, since a leading fragment is a
/// cosmetic problem and the file name — the only part used as identity — is at
/// the other end.
fn start_of_path(head: &str) -> usize {
    let bytes = head.as_bytes();
    (0..bytes.len().saturating_sub(2))
        .rev()
        .find(|index| {
            bytes[*index].is_ascii_alphabetic()
                && bytes[index + 1] == b':'
                && matches!(bytes[index + 2], b'\\' | b'/')
        })
        .unwrap_or(0)
}

fn file_name_of(path: &str) -> &str {
    match path.rfind(['/', '\\']) {
        Some(at) => &path[at + 1..],
        None => path,
    }
}

fn strip_extension(file: &str) -> String {
    match file.rfind('.') {
        Some(at) => file[..at].to_owned(),
        None => file.to_owned(),
    }
}

fn format_of(file: &str) -> PluginFormat {
    let lower = file.to_ascii_lowercase();
    if lower.ends_with(".vst3") {
        PluginFormat::Vst3
    } else if lower.ends_with(".component") {
        PluginFormat::AudioUnit
    } else {
        PluginFormat::Vst2
    }
}

fn text(payload: &[u8], utf16: bool) -> String {
    if utf16 {
        decode_utf16(payload)
    } else {
        decode_ascii(payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flstudio::events::{Event, Header};

    fn utf16_bytes(text: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        bytes.extend_from_slice(&[0, 0]);
        bytes
    }

    /// A plugin state blob with a binary path buried in it, as FL writes them.
    fn params_with(path: &str) -> Vec<u8> {
        let mut payload = vec![0x01, 0x00, 0x00, 0x00, 0x2a, 0xff];
        payload.extend_from_slice(path.as_bytes());
        payload.extend_from_slice(&[0x00, 0x00, 0x99, 0x88]);
        payload
    }

    fn project(events: Vec<Event>) -> Stream {
        let mut all = vec![Event::new(199, b"20.5.0.1142\0".to_vec())];
        all.extend(events);
        Stream {
            header: Header {
                format: 0,
                channels: 1,
                ppq: 96,
            },
            events: all,
        }
    }

    #[test]
    fn a_hosted_vst_should_be_identified_by_its_binary_not_the_wrapper() {
        // Every third-party plugin reports "Fruity Wrapper" as its internal
        // name, so using that as the identity would collapse a project's
        // entire plugin list into one entry.
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Bass 2")),
            Event::new(
                EVENT_PLUGIN_PARAMS,
                params_with(r"E:\VST\VST 64 bit\Serum_x64.dll"),
            ),
        ]));

        assert_eq!(plugins.len(), 1);
        assert_eq!(
            plugins[0].id,
            PluginId::Vst2ByFile {
                file_name: "serum_x64.dll".to_owned()
            }
        );
        assert_eq!(plugins[0].name, "Serum_x64", "named for what it is");
        assert_eq!(plugins[0].format, PluginFormat::Vst2);
    }

    #[test]
    fn the_same_plugin_on_two_machines_should_be_one_entry() {
        // The directory differs between the two real projects examined; only
        // the file name is stable, so only the file name may be the identity.
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(
                EVENT_PLUGIN_PARAMS,
                params_with(r"E:\VST\VST 64 bit\Serum_x64.dll"),
            ),
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(
                EVENT_PLUGIN_PARAMS,
                params_with(r"C:\Program Files\Common Files\VST2\Serum_x64.dll"),
            ),
        ]));

        assert_eq!(plugins.len(), 1, "one plugin, two install locations");
        assert_eq!(plugins[0].instances, 2);
    }

    #[test]
    fn a_stock_plugin_should_be_identified_by_its_own_name() {
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes("Fruity Limiter")),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Limiter")),
        ]));
        assert_eq!(
            plugins[0].id,
            PluginId::FlNative {
                device: "Fruity Limiter".to_owned()
            }
        );
        assert_eq!(
            plugins[0].name, "Fruity Limiter",
            "named for the plugin, not for what this instance was called"
        );
        assert_eq!(plugins[0].format, PluginFormat::Native);
    }

    #[test]
    fn an_empty_internal_name_should_mean_the_built_in_sampler() {
        // Observed on an audio channel in a real project: internal name empty,
        // display name "Sampler".
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes("")),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Sampler")),
        ]));
        assert_eq!(
            plugins[0].id,
            PluginId::FlNative {
                device: "Sampler".to_owned()
            }
        );
        assert_eq!(plugins[0].name, "Sampler");
    }

    #[test]
    fn a_channel_label_should_not_become_the_name_of_the_plugin_on_it() {
        // A real project put 3x Osc on channels called "T", "H" and "E"; the
        // inventory has to say "3x Osc", because it answers what the user
        // needs installed, not what they named a channel.
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes("3x Osc")),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("T")),
        ]));
        assert_eq!(plugins[0].name, "3x Osc");
    }

    #[test]
    fn a_channel_name_on_its_own_should_not_become_a_plugin() {
        // Event 203 names channels as well as plugins. Scanning for names
        // instead of walking instances would report every channel in the rack
        // as an instrument the user might be missing.
        let plugins = collect(&project(vec![
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Kick")),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Snare")),
        ]));
        assert!(plugins.is_empty());
    }

    #[test]
    fn a_vst3_binary_should_be_reported_as_vst3() {
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_PLUGIN_PARAMS, params_with(r"C:\VST3\Serum2.vst3")),
        ]));
        assert_eq!(plugins[0].format, PluginFormat::Vst3);
    }

    #[test]
    fn a_wrapper_we_cannot_see_inside_should_still_be_reported() {
        // Better an entry named by whatever the user called it than a plugin
        // silently missing from the list of what this project needs.
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Some Synth")),
        ]));
        assert_eq!(plugins.len(), 1);
        assert_eq!(plugins[0].name, "Some Synth");
    }

    #[test]
    fn a_path_followed_by_printable_binary_should_still_be_found() {
        // Taken byte for byte from a real project. The `8` after `.dll` is the
        // first byte of the next length field, and it is printable — so the
        // run of printable characters does not end at the extension. Requiring
        // it to found nothing, and the failure was silent: the plugin was
        // reported under whatever the user had named the channel.
        let mut params =
            b"\x00\x00\x00\x00\x00Serum7\x00\x00\x00\x1f\x00\x00\x00\x00\x00\x00\x00".to_vec();
        params.extend_from_slice(br"E:\VST\VST 64 bit\Serum_x64.dll");
        params.extend_from_slice(b"8\x00\x00\x00\x0c\x00\x00\x00\x00\x00\x00\x00Xfer ");

        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Serum")),
            Event::new(EVENT_PLUGIN_PARAMS, params),
        ]));

        assert_eq!(
            plugins[0].id,
            PluginId::Vst2ByFile {
                file_name: "serum_x64.dll".to_owned()
            }
        );
        assert_eq!(
            plugins[0].path.as_deref(),
            Some(r"E:\VST\VST 64 bit\Serum_x64.dll"),
            "the leading junk must be trimmed at the drive letter"
        );
    }

    #[test]
    fn state_that_merely_contains_words_should_not_look_like_a_path() {
        let plugins = collect(&project(vec![
            Event::new(EVENT_INTERNAL_NAME, utf16_bytes(WRAPPER_NAME)),
            Event::new(EVENT_NEW_PLUGIN, vec![0; 52]),
            Event::new(EVENT_DISPLAY_NAME, utf16_bytes("Mystery")),
            Event::new(EVENT_PLUGIN_PARAMS, b"preset name: warm pad".to_vec()),
        ]));
        assert_eq!(
            plugins[0].name, "Mystery",
            "no binary, so no false identity"
        );
    }
}
