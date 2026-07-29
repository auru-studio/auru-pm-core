//! Verify a directory of real projects without committing proprietary files.
//!
//! Usage:
//! `cargo run -p auru-pm --example corpus_roundtrip -- /path/to/projects`

use std::collections::BTreeMap;
use std::error::Error;
use std::io::{Cursor, Read};
use std::path::PathBuf;

use auru_pm::{DiscoveredProject, ProjectFormat, ProjectSnapshot, ScanOptions, discovery};

fn main() -> Result<(), Box<dyn Error>> {
    let inputs = std::env::args_os().skip(1).map(PathBuf::from);
    let mut projects = BTreeMap::new();
    for input in inputs {
        if input.is_dir() {
            for project in discovery::scan_for_projects(&input, &ScanOptions::default()) {
                projects.insert(project.project_file().to_path_buf(), project.format());
            }
        } else if let Some(project) = DiscoveredProject::detect(&input)? {
            projects.insert(project.project_file().to_path_buf(), project.format());
        } else {
            return Err(format!("'{}' is not a supported project", input.display()).into());
        }
    }
    if projects.is_empty() {
        return Err("no supported projects were found".into());
    }

    for (path, format) in &projects {
        let snapshot = ProjectSnapshot::load(path)?;
        let restored = snapshot.restore_bytes()?;
        let round_trip = ProjectSnapshot::from_source_bytes(*format, &restored)?;
        if snapshot.as_bytes() != round_trip.as_bytes() {
            return Err(format!("'{}' changed after round-trip", path.display()).into());
        }
        if *format == ProjectFormat::Dawproject {
            verify_dawproject_hydration(&snapshot, &restored)?;
        }
        println!("ok  {}  {}", format, path.display());
    }
    println!("verified {} project(s)", projects.len());
    Ok(())
}

fn verify_dawproject_hydration(
    snapshot: &ProjectSnapshot,
    restored: &[u8],
) -> Result<(), Box<dyn Error>> {
    let paths = auru_pm::dawproject::archive_resource_paths(snapshot)?;
    if paths.is_empty() {
        return Ok(());
    }
    let fetched_snapshot = ProjectSnapshot::from_canonical_bytes(snapshot.as_bytes())?;
    if fetched_snapshot.restore_bytes().is_ok() {
        return Err("a fetched DAWproject v2 snapshot unexpectedly retained resource bytes".into());
    }

    let mut archive = zip::ZipArchive::new(Cursor::new(restored))?;
    let mut resources = BTreeMap::new();
    for path in paths {
        let mut bytes = Vec::new();
        archive.by_name(&path)?.read_to_end(&mut bytes)?;
        resources.insert(path, bytes);
    }
    let hydrated = auru_pm::dawproject::hydrate_embedded_assets(&fetched_snapshot, &resources)?;
    let hydrated_source = hydrated.restore_bytes()?;
    let hydrated_round_trip =
        ProjectSnapshot::from_source_bytes(ProjectFormat::Dawproject, &hydrated_source)?;
    if hydrated_round_trip.as_bytes() != snapshot.as_bytes() {
        return Err("DAWproject provider hydration changed the canonical snapshot".into());
    }
    Ok(())
}
