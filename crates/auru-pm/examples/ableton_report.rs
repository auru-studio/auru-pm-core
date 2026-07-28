//! Dump what Auru understands about an Ableton Live Set.
//!
//! Reads a `.als`, prints its project detail, plugin inventory, and every file
//! it references — classified by whether that file would travel with the
//! project. Useful for eyeballing a real set against the extraction rules
//! without committing anything.
//!
//! ```text
//! cargo run -p auru-pm --example ableton_report -- "/path/to/Song.als"
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use auru_pm::ableton::{self, RefClass};
use auru_pm::{ProjectFormat, ProjectSnapshot};

fn main() -> ExitCode {
    let Some(path) = std::env::args_os().nth(1).map(PathBuf::from) else {
        eprintln!("usage: ableton_report <project.als>");
        return ExitCode::FAILURE;
    };

    let snapshot = match ProjectSnapshot::load(&path) {
        Ok(snapshot) => snapshot,
        Err(error) => {
            eprintln!("could not read '{}': {error}", path.display());
            return ExitCode::FAILURE;
        }
    };

    if snapshot.format() != ProjectFormat::AbletonLiveSet {
        eprintln!(
            "'{}' is a {} project, not an Ableton Live Set",
            path.display(),
            snapshot.format()
        );
        return ExitCode::FAILURE;
    }

    let metadata = match ableton::read_metadata(&snapshot) {
        Ok(metadata) => metadata,
        Err(error) => {
            eprintln!("could not read project detail: {error}");
            return ExitCode::FAILURE;
        }
    };

    println!("== {} ==", path.display());
    println!(
        "snapshot   {} KiB canonical JSON",
        snapshot.as_bytes().len() / 1024
    );
    println!(
        "made with  {}",
        metadata.live_version.as_deref().unwrap_or("unknown")
    );
    println!(
        "tempo      {}",
        metadata
            .tempo
            .map_or_else(|| "unknown".to_owned(), |tempo| format!("{tempo} BPM"))
    );
    println!(
        "time sig   {}",
        metadata
            .time_signature
            .map_or_else(|| "unknown".to_owned(), |sig| sig.to_string())
    );
    println!(
        "key        {}",
        metadata.key.as_ref().map_or_else(
            || "none".to_owned(),
            |key| {
                let suffix = if key.in_key { " · in key" } else { "" };
                format!("{}{suffix}", key.label())
            }
        )
    );
    let tracks = metadata.tracks;
    println!(
        "tracks     {} total · {} MIDI · {} audio · {} group · {} return",
        tracks.total(),
        tracks.midi,
        tracks.audio,
        tracks.group,
        tracks.retn
    );
    println!(
        "clips      {} · arrangement {}",
        metadata.clip_count,
        metadata.arrangement_bars().map_or_else(
            || format!("{} beats", metadata.arrangement_end_beats),
            |bars| format!("{bars} bars"),
        )
    );
    println!(
        "scenes     {} · locators {}",
        metadata.scene_count, metadata.locator_count
    );
    if let Some((start, length)) = metadata.loop_region {
        println!("loop       {start} → {} beats", start + length);
    }

    println!("\n-- plugins --");
    for plugin in &metadata.plugins {
        let kind = match plugin.device_type {
            Some(1) => " (instrument)",
            Some(2) => " (effect)",
            _ => "",
        };
        println!(
            "  {:<5} {:<28} ×{:<3} {}{kind}",
            plugin.format.label(),
            plugin.name,
            plugin.instances,
            plugin.id
        );
    }
    let third_party = metadata.third_party_plugins().count();
    println!(
        "  {} distinct · {third_party} third-party, needed to open this project",
        metadata.plugins.len()
    );

    println!("\n-- referenced files --");
    let refs = match ableton::read_asset_refs(&snapshot) {
        Ok(refs) => refs,
        Err(error) => {
            eprintln!("could not read file references: {error}");
            return ExitCode::FAILURE;
        }
    };

    // Collapse the many occurrences of each file into one line.
    let mut distinct: Vec<(&str, RefClass, usize, Option<u64>)> = Vec::new();
    for asset in &refs {
        let key = asset.dedup_key();
        match distinct.iter_mut().find(|(seen, ..)| *seen == key) {
            Some((_, _, count, _)) => *count += 1,
            None => distinct.push((key, asset.class, 1, asset.original_size)),
        }
    }

    for (path, class, count, size) in &distinct {
        let label = match class {
            RefClass::InFolder => "in-folder",
            RefClass::External => "EXTERNAL",
            RefClass::UserLibrary => "USER LIB",
            RefClass::Library => "library",
            RefClass::Unresolvable => "unresolved",
        };
        let size = size.map_or_else(String::new, |bytes| format!(" · {} KiB", bytes / 1024));
        let shown = if path.is_empty() { "(no path)" } else { path };
        println!("  [{label:^10}] ×{count:<3}{size}\n              {shown}");
    }

    let assets = metadata.assets;
    println!(
        "\n  {} occurrences · {} distinct",
        refs.len(),
        distinct.len()
    );
    println!(
        "  {} in-folder · {} external · {} user-library · {} library · {} unresolvable",
        assets.in_folder, assets.external, assets.user_library, assets.library, assets.unresolvable
    );
    println!(
        "  {} file(s) would be vendored into the project folder to make it portable",
        assets.vendorable()
    );

    ExitCode::SUCCESS
}
