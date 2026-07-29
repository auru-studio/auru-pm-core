//! Report the instruments and effects a project needs, and where to get them.
//!
//! Uses the registry compiled into this build; no network, no licences, no
//! installation. Availability is a best-effort look at the conventional plugin
//! folders, extendable with `AURU_VST_PATHS`.
//!
//! ```text
//! cargo run -p auru-pm --example ableton_plugins -- "/path/to/Song.als"
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use auru_pm::plugin_registry::{self, PluginAvailability, PluginSearchPaths};
use auru_pm::{ProjectFormat, ProjectSnapshot, ableton};

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ableton_plugins <project.als>");
        return ExitCode::FAILURE;
    };

    let snapshot = match ProjectSnapshot::load(&path) {
        Ok(snapshot) if snapshot.format() == ProjectFormat::AbletonLiveSet => snapshot,
        Ok(snapshot) => {
            eprintln!("'{}' is a {} project", path.display(), snapshot.format());
            return ExitCode::FAILURE;
        }
        Err(error) => {
            eprintln!("could not read '{}': {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    let plugins = match ableton::read_plugins(&snapshot) {
        Ok(plugins) => plugins,
        Err(error) => {
            eprintln!("could not read plugins: {error}");
            return ExitCode::FAILURE;
        }
    };

    let search_paths = PluginSearchPaths::detect();
    let resolved = plugin_registry::resolve(&plugins, plugin_registry::bundled(), &search_paths);

    println!("== {} ==\n", path.display());

    let (third_party, from_live): (Vec<_>, Vec<_>) = resolved
        .iter()
        .partition(|plugin| plugin.availability != PluginAvailability::BundledWithDaw);

    if !third_party.is_empty() {
        println!("-- needs to be installed separately --");
        for plugin in &third_party {
            println!(
                "  {:<6} {:<24} ×{:<3} {}",
                plugin.format.label(),
                plugin.name,
                plugin.instances,
                plugin.availability.label()
            );
            if !plugin.vendor.is_empty() {
                println!("         by {}", plugin.vendor);
            }
            match plugin.link() {
                Some(link) => println!("         {link}"),
                None => println!("         not in the registry — search for it by name"),
            }
            if let Some(notes) = &plugin.notes {
                println!("         {notes}");
            }
        }
        println!();
    }

    if !from_live.is_empty() {
        println!("-- comes with your DAW --");
        for plugin in &from_live {
            println!("  {:<24} ×{}", plugin.name, plugin.instances);
        }
        println!();
    }

    let blocked = resolved
        .iter()
        .filter(|plugin| plugin.blocks_playback())
        .count();
    if blocked == 0 {
        println!("  everything this project needs is on this computer");
    } else {
        println!("  {blocked} plugin(s) are not on this computer.");
        println!(
            "  Your settings for them are saved inside the project. They come back\n  \
             exactly as you left them once the plugin is installed and authorized here."
        );
    }

    if search_paths.directories.is_empty() {
        println!("\n  (no plugin folders searched — set AURU_VST_PATHS to check availability)");
    }

    ExitCode::SUCCESS
}
