//! Compare two Ableton Live Sets and print what changed.
//!
//! Handy against a project's own `Backup/` autosaves, which are real successive
//! versions of the same set.
//!
//! ```text
//! cargo run -p auru-pm --example ableton_diff -- "Backup/Song [older].als" "Song.als"
//! ```

use std::path::PathBuf;
use std::process::ExitCode;

use auru_pm::{ProjectFormat, ProjectSnapshot, structured_diff};

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let (Some(before_path), Some(after_path)) = (args.next(), args.next()) else {
        eprintln!("usage: ableton_diff <older.als> <newer.als>");
        return ExitCode::FAILURE;
    };

    let mut snapshots = Vec::new();
    for path in [&before_path, &after_path] {
        match ProjectSnapshot::load(path) {
            Ok(snapshot) if snapshot.format() == ProjectFormat::AbletonLiveSet => {
                let value: serde_json::Value =
                    serde_json::from_slice(snapshot.as_bytes()).expect("canonical JSON");
                snapshots.push(value);
            }
            Ok(snapshot) => {
                eprintln!("'{}' is a {} project", path.display(), snapshot.format());
                return ExitCode::FAILURE;
            }
            Err(error) => {
                eprintln!("could not read '{}': {error}", path.display());
                return ExitCode::FAILURE;
            }
        }
    }

    let diff = structured_diff(&snapshots[0], &snapshots[1]);

    println!(
        "{}  →  {}",
        before_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        after_path.file_name().unwrap_or_default().to_string_lossy()
    );
    println!("time signature {}/{}\n", diff.time_sig.0, diff.time_sig.1);

    if diff.is_empty() {
        println!("no changes");
        return ExitCode::SUCCESS;
    }

    if !diff.project_changes.is_empty() {
        println!("-- project --");
        for change in &diff.project_changes {
            println!("  {change}");
        }
        println!();
    }

    if !diff.channels.is_empty() {
        println!("-- tracks --");
        for channel in &diff.channels {
            let status = match channel.status {
                auru_pm::ChangeKind::Add => "added",
                auru_pm::ChangeKind::Remove => "removed",
                auru_pm::ChangeKind::Modify => "changed",
            };
            let kind = match channel.kind {
                auru_pm::ChannelKind::Audio => "AUDIO",
                auru_pm::ChannelKind::Midi => "MIDI",
                auru_pm::ChannelKind::Plugin => "PLUGIN",
                auru_pm::ChannelKind::Other => "OTHER",
            };
            println!("\n  [{kind}] {} ({status})", channel.name);
            if channel.clips_added + channel.clips_removed + channel.clips_modified > 0 {
                println!(
                    "      clips: +{} -{} ~{}",
                    channel.clips_added, channel.clips_removed, channel.clips_modified
                );
            }
            for row in &channel.rows {
                let detail = match (&row.before, &row.after) {
                    (Some(before), Some(after)) => format!("{before} → {after}"),
                    (None, Some(after)) => after.clone(),
                    (Some(before), None) => before.clone(),
                    (None, None) => String::new(),
                };
                println!("      {:<18} {:<28} {detail}", row.tag.label(), row.target);
            }
        }
        println!();
    }

    println!(
        "  {} track(s) · clips +{} -{} ~{}",
        diff.channel_count(),
        diff.total_clips_added(),
        diff.total_clips_removed(),
        diff.total_clips_modified()
    );

    ExitCode::SUCCESS
}
