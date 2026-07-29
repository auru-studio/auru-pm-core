//! Inspect and round-trip a real DAWproject file.
//!
//! Usage:
//! `cargo run -p auru-pm --example dawproject_inspect -- /path/to/song.dawproject`

use std::error::Error;
use std::path::PathBuf;

use auru_pm::{ProjectFormat, ProjectSnapshot, dawproject};

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("pass the path to a .dawproject file")?;
    let snapshot = ProjectSnapshot::load(&path)?;
    if snapshot.format() != ProjectFormat::Dawproject {
        return Err(format!("{} is a {}", path.display(), snapshot.format()).into());
    }

    let metadata = dawproject::read_metadata(&snapshot)?;
    let plugins = dawproject::read_plugins(&snapshot)?;
    let assets = dawproject::read_asset_refs(&snapshot)?;
    let application = metadata.application_label();

    println!("File: {}", path.display());
    println!(
        "Title: {}",
        metadata.title.as_deref().unwrap_or("not declared")
    );
    println!(
        "Application: {}",
        application.as_deref().unwrap_or("not declared")
    );
    println!(
        "Tempo: {}",
        metadata
            .tempo
            .map(|tempo| format!("{tempo} BPM"))
            .unwrap_or_else(|| "not declared".to_owned())
    );
    println!(
        "Time signature: {}",
        metadata
            .time_signature
            .map(|signature| signature.to_string())
            .unwrap_or_else(|| "not declared".to_owned())
    );
    println!(
        "Tracks: {} song, {} master; clips: {}; scenes: {}",
        metadata.tracks.total(),
        metadata.tracks.master,
        metadata.clip_count,
        metadata.scene_count
    );
    println!(
        "Media: {} referenced, {} embedded, {} external, {} missing",
        metadata.assets.referenced,
        metadata.assets.embedded,
        metadata.assets.external,
        metadata.assets.missing
    );
    println!("Plugins: {}", plugins.len());
    for plugin in &plugins {
        println!(
            "  {} · {} · {} · {} instance(s)",
            plugin.name,
            plugin.format.label(),
            plugin.id,
            plugin.instances
        );
    }
    for asset in assets
        .iter()
        .filter(|asset| asset.external || !asset.embedded)
    {
        println!(
            "  media: {} · {}",
            asset.path,
            if asset.external {
                "external"
            } else {
                "missing from archive"
            }
        );
    }

    let restored = snapshot.restore_bytes()?;
    let round_tripped = ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &restored)?;
    if round_tripped.as_bytes() != snapshot.as_bytes() {
        return Err("restored archive did not normalize back to the same snapshot".into());
    }
    println!("Round trip: canonical snapshot unchanged");
    Ok(())
}
